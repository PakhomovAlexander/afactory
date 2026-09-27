//! Warm Task checks (ADR-0123). A code policy's `[warm]` table lets a Task check reuse one
//! machine-local build directory across Attempts and Tasks of one project, and bind an
//! administrator-approved Cache Snapshot, under `trusted_local` only.
//!
//! The directory is keyed by the project's repository identity and by the toolchain the check
//! would run: the Snapshot's toolchain declaration, `rustc -vV`, `cargo -vV`, the host triple and
//! the check's fixed environment. It is bounded in bytes before, during and after every check,
//! and removed rather than repaired whenever it is over its bound or otherwise suspect. It sits
//! outside every sandbox: no Worker, seal, candidate capture or delivery ever reads it. Whether a
//! check ran warm or cold is evidence beside its `Check` span, never part of its result.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use review_check::{CheckDefinition, CheckExecution, CheckRunner};
use review_config::{BuildCacheKindSpec, CacheKindSpec};
use review_core::task::present_option;
use review_core::task::runtime::TaskCacheObservationV1;
use review_sandbox::{
    CacheError, CacheErrorKind, CacheKind, CacheSource, TaskBuildCacheLock, WarmDirectory,
    default_task_build_cache_root, directory_bytes, lock_task_build_cache,
    materialize_cache_into_runtime,
};
use review_source_git::Manifest;
use review_store::Cas;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `max_bytes` when a policy leaves it out.
pub const DEFAULT_WARM_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// The largest bound a policy may declare.
pub const MAX_WARM_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// The reason a check that grew its warm directory past the bound fails with.
pub const WARM_CACHE_BOUND_EXCEEDED: &str = "warm_cache_bound_exceeded";

const TOOLCHAIN_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const MONITOR_POLL: Duration = Duration::from_millis(50);

/// The optional `[warm]` table of `af.code-task-policy/1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeWarmPolicy {
    /// Build directories a check may reuse; `cargo_target` is the only kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_cache: Vec<BuildCacheKindSpec>,
    /// Cache Snapshot kinds resolved through the machine's cache policy, as a Gate's
    /// `[gate] caches` are (ADR-0036).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<CacheKindSpec>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub max_bytes: Option<u64>,
}

impl CodeWarmPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.build_cache.is_empty() && self.caches.is_empty() {
            return Err("Code policy [warm] declares neither build_cache nor caches".into());
        }
        for (index, kind) in self.build_cache.iter().enumerate() {
            if self.build_cache[..index].contains(kind) {
                return Err(format!(
                    "Code policy [warm] declares build cache `{}` more than once",
                    kind.as_str()
                ));
            }
        }
        for (index, kind) in self.caches.iter().enumerate() {
            if self.caches[..index].contains(kind) {
                return Err(format!(
                    "Code policy [warm] declares cache `{}` more than once",
                    cache_kind(*kind).name()
                ));
            }
        }
        if self
            .max_bytes
            .is_some_and(|bytes| bytes == 0 || bytes > MAX_WARM_MAX_BYTES)
        {
            return Err(format!(
                "Code policy [warm] max_bytes must be between 1 and {MAX_WARM_MAX_BYTES}"
            ));
        }
        Ok(())
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes.unwrap_or(DEFAULT_WARM_MAX_BYTES)
    }
}

fn cache_kind(kind: CacheKindSpec) -> CacheKind {
    match kind {
        CacheKindSpec::Cargo => CacheKind::Cargo,
    }
}

pub(crate) type CacheSourceResolver =
    dyn Fn(CacheKind) -> Result<CacheSource, CacheError> + Send + Sync;

/// Machine-local wiring of the warm layer. None of it is Task authority: the policy says what a
/// check may use, and this says where the machine keeps it.
#[derive(Clone)]
pub(crate) struct WarmCheckHost {
    pub(crate) root: Option<PathBuf>,
    pub(crate) lock_wait: Duration,
    pub(crate) resolver: Option<Arc<CacheSourceResolver>>,
}

impl Default for WarmCheckHost {
    fn default() -> Self {
        Self {
            root: None,
            lock_wait: LOCK_WAIT,
            resolver: None,
        }
    }
}

/// The toolchain declaration a Snapshot carries at its root, as the key reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainDeclaration {
    pub path: Option<String>,
    pub bytes: Vec<u8>,
}

impl ToolchainDeclaration {
    /// `rust-toolchain.toml`, else `rust-toolchain`, else the literal `none`.
    pub fn from_manifest(cas: &Cas, manifest: &Manifest) -> Result<Self, String> {
        for path in ["rust-toolchain.toml", "rust-toolchain"] {
            if let Some(entry) = manifest.get(path) {
                return Ok(Self {
                    path: Some(path.into()),
                    bytes: cas.get(&entry.content).map_err(|e| e.to_string())?,
                });
            }
        }
        Ok(Self {
            path: None,
            bytes: b"none".to_vec(),
        })
    }
}

/// The directory component of a project: a domain-separated digest of the Store's repository
/// identity, so no repository-controlled text becomes a path.
pub fn project_key(repository_id: &str) -> String {
    hex(&key_digest(
        "af.task-build-cache.project/1",
        &[repository_id.as_bytes()],
    ))
}

/// Resolve the toolchain identity a check would build with. Both version commands run in the
/// check's working directory, under exactly the check's environment and a 30-second bound, so a
/// toolchain file or a `PATH` that selects another compiler selects another key.
pub fn toolchain_identity(
    workdir: &Path,
    environment: &[(String, String)],
    declaration: &ToolchainDeclaration,
    cancellation: Option<&AtomicBool>,
    bound: Duration,
) -> Result<String, String> {
    let rustc = version(workdir, environment, "rustc", cancellation, bound)?;
    let cargo = version(workdir, environment, "cargo", cancellation, bound)?;
    let host = String::from_utf8_lossy(&rustc)
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::trim).map(String::from))
        .filter(|host| !host.is_empty())
        .ok_or("rustc -vV reported no host triple")?;
    let fixed = |key: &str| {
        environment
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map_or("", |(_, value)| value.as_str())
            .as_bytes()
    };
    Ok(key_digest(
        "af.task-build-cache.toolchain/1",
        &[
            declaration.path.as_deref().unwrap_or("none").as_bytes(),
            &declaration.bytes,
            &rustc,
            &cargo,
            host.as_bytes(),
            fixed("PATH"),
            fixed("LC_ALL"),
            fixed("TZ"),
        ],
    ))
}

fn version(
    workdir: &Path,
    environment: &[(String, String)],
    program: &str,
    cancellation: Option<&AtomicBool>,
    bound: Duration,
) -> Result<Vec<u8>, String> {
    if bound.is_zero() {
        return Err(format!(
            "{program} -vV had no time left before the check deadline"
        ));
    }
    let mut command = std::process::Command::new(program);
    command
        .arg("-vV")
        .current_dir(workdir)
        .env_clear()
        .envs(environment.iter().map(|(key, value)| (key, value)));
    let captured = match cancellation {
        Some(flag) => review_process::run_supervised_captured_cancellable_with_policy(
            &mut command,
            None,
            bound,
            review_process::ExitPolicy::KillProcessGroup,
            flag,
        ),
        None => review_process::run_supervised_captured_with_policy(
            &mut command,
            None,
            bound,
            review_process::ExitPolicy::KillProcessGroup,
        ),
    };
    match captured.status {
        Ok(status) if status.success() && !captured.stdout.is_empty() => Ok(captured.stdout),
        Ok(status) => Err(format!("{program} -vV exited with {status}")),
        Err(error) => Err(format!("{program} -vV did not complete: {error}")),
    }
}

fn key_digest(domain: &str, fields: &[&[u8]]) -> String {
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(0);
    for field in fields {
        bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    review_store::canonical::blob_content_id(&bytes)
}

fn hex(digest: &str) -> String {
    digest.strip_prefix("sha256:").unwrap_or(digest).to_string()
}

/// One measured observation before it receives its content identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observation {
    pub(crate) kind: String,
    pub(crate) eligible: bool,
    pub(crate) source_digest: String,
    pub(crate) toolchain_id: Option<String>,
    pub(crate) bytes_available: u64,
    pub(crate) lookup_ms: u64,
    pub(crate) materialization_ms: u64,
    /// Bytes removed after the check when the directory ended above its bound; one
    /// observation per kind carries both the availability before and the eviction after.
    pub(crate) evicted_bytes: Option<u64>,
}

impl Observation {
    /// Give the observation its identity: the exact check and Attempt it belongs to.
    pub(crate) fn record(
        self,
        cas: &Cas,
        attempt: [&str; 4],
    ) -> Result<TaskCacheObservationV1, String> {
        let observation_id = cas
            .put_json(&json!([
                attempt,
                "warm_check",
                self.kind,
                self.eligible,
                self.source_digest,
                self.toolchain_id,
                self.bytes_available,
                self.lookup_ms,
                self.materialization_ms,
                self.evicted_bytes
            ]))
            .map_err(|e| e.to_string())?;
        let observation = TaskCacheObservationV1 {
            observation_id,
            kind: self.kind,
            eligible: self.eligible,
            source_digest: self.source_digest,
            toolchain_id: self.toolchain_id,
            bytes_available: self.bytes_available,
            lookup_ms: self.lookup_ms,
            materialization_ms: self.materialization_ms,
            evicted_bytes: self.evicted_bytes,
        };
        observation.validate()?;
        Ok(observation)
    }
}

/// What one check receives from its warm layer.
pub(crate) struct PreparedCheck {
    /// `CARGO_TARGET_DIR` first, then any Cache Snapshot's variables.
    pub(crate) environment: Vec<(String, String)>,
    /// The locked warm directory the check builds into; `None` runs cold.
    pub(crate) directory: Option<WarmDirectory>,
    pub(crate) observations: Vec<Observation>,
    /// A declared Cache Snapshot that could not be materialized: the check does not run, exactly
    /// as a Gate does not dispatch its command.
    pub(crate) refusal: Option<String>,
}

/// The warm layer of one check Attempt. The toolchain is resolved once, before the first check.
pub(crate) struct WarmSession<'a> {
    host: &'a WarmCheckHost,
    policy: &'a CodeWarmPolicy,
    project: String,
    declaration: ToolchainDeclaration,
    toolchain: Option<Result<String, String>>,
}

impl<'a> WarmSession<'a> {
    pub(crate) fn new(
        host: &'a WarmCheckHost,
        policy: &'a CodeWarmPolicy,
        repository_id: &str,
        declaration: ToolchainDeclaration,
    ) -> Self {
        Self {
            host,
            policy,
            project: project_key(repository_id),
            declaration,
            toolchain: None,
        }
    }

    /// Lock and measure the build directory and materialize any Cache Snapshot into `runtime`.
    /// `environment` is the check's own environment, used for the one toolchain probe. `budget`
    /// is the check Attempt's remaining wall time: the toolchain probe and the lock wait are
    /// each bounded by it, so preparation can never outlive the reservation it prepares for.
    pub(crate) fn prepare(
        &mut self,
        cas: &Cas,
        environment: &[(String, String)],
        workdir: &Path,
        runtime: &Path,
        cancellation: Option<&AtomicBool>,
        budget: Duration,
    ) -> Result<PreparedCheck, String> {
        let mut prepared = PreparedCheck {
            environment: Vec::new(),
            directory: None,
            observations: Vec::new(),
            refusal: None,
        };
        let mut target = runtime.join("target");
        for kind in &self.policy.build_cache {
            let label = kind.as_str();
            let started = Instant::now();
            let toolchain = self
                .toolchain
                .get_or_insert_with(|| {
                    toolchain_identity(
                        workdir,
                        environment,
                        &self.declaration,
                        cancellation,
                        budget.min(TOOLCHAIN_PROBE_TIMEOUT),
                    )
                })
                .clone();
            let toolchain = match toolchain {
                Ok(id) => id,
                Err(detail) => {
                    eprintln!("warm check cache diagnostic: {detail}");
                    prepared.observations.push(self.ineligible(
                        cas,
                        label,
                        "toolchain_unresolved",
                        None,
                        0,
                        started,
                    )?);
                    continue;
                }
            };
            let lock = self
                .host
                .root
                .clone()
                .map_or_else(default_task_build_cache_root, Ok)
                .and_then(|root| {
                    lock_task_build_cache(
                        &root,
                        &self.project,
                        &hex(&toolchain),
                        label,
                        self.host.lock_wait.min(budget),
                    )
                });
            let directory = match lock {
                Ok(TaskBuildCacheLock::Held(directory)) => directory,
                Ok(TaskBuildCacheLock::Busy) => {
                    prepared.observations.push(self.ineligible(
                        cas,
                        label,
                        "busy",
                        Some(&toolchain),
                        0,
                        started,
                    )?);
                    continue;
                }
                Err(detail) => {
                    eprintln!("warm check cache diagnostic: {detail}");
                    prepared.observations.push(self.ineligible(
                        cas,
                        label,
                        "unavailable",
                        Some(&toolchain),
                        0,
                        started,
                    )?);
                    continue;
                }
            };
            let bytes = directory.bytes();
            if bytes > self.policy.max_bytes() {
                // Over its bound before the check: removed, never trimmed, and this check runs
                // cold in its private runtime directory.
                directory.remove()?;
                prepared.observations.push(self.ineligible(
                    cas,
                    label,
                    "bound_exceeded",
                    Some(&toolchain),
                    bytes,
                    started,
                )?);
                continue;
            }
            let lookup_ms = started.elapsed().as_millis() as u64;
            let materializing = Instant::now();
            if let Err(detail) = directory.ensure() {
                eprintln!("warm check cache diagnostic: {detail}");
                prepared.observations.push(self.ineligible(
                    cas,
                    label,
                    "unavailable",
                    Some(&toolchain),
                    0,
                    started,
                )?);
                continue;
            }
            prepared.observations.push(Observation {
                kind: label.into(),
                eligible: true,
                source_digest: self.identity(cas, label, Some(&toolchain), None)?,
                toolchain_id: Some(toolchain),
                bytes_available: bytes,
                lookup_ms,
                materialization_ms: materializing.elapsed().as_millis() as u64,
                evicted_bytes: None,
            });
            target = directory.path().to_path_buf();
            prepared.directory = Some(directory);
        }
        prepared
            .environment
            .push(("CARGO_TARGET_DIR".into(), target.display().to_string()));
        for kind in &self.policy.caches {
            let kind = cache_kind(*kind);
            let started = Instant::now();
            let resolved = match &self.host.resolver {
                Some(resolver) => resolver(kind),
                None => Err(CacheError::new(
                    CacheErrorKind::PolicyUnavailable,
                    "no machine-local cache resolver is installed",
                )),
            };
            match resolved.and_then(|source| materialize_cache_into_runtime(&source, runtime, cas))
            {
                Ok(snapshot) => {
                    prepared.environment.extend(kind.environment(runtime).local);
                    prepared.observations.push(Observation {
                        kind: kind.name().into(),
                        eligible: true,
                        source_digest: snapshot.source_digest,
                        toolchain_id: None,
                        bytes_available: snapshot.bytes,
                        lookup_ms: snapshot.lookup_ms,
                        materialization_ms: snapshot.materialization_ms,
                        evicted_bytes: None,
                    });
                }
                Err(error) => {
                    eprintln!(
                        "cache diagnostic for Task check: {}",
                        error.operator_detail()
                    );
                    prepared.refusal = Some(format!(
                        "Task check requested `{}` cache: {error}",
                        kind.name()
                    ));
                    prepared.observations.push(self.ineligible(
                        cas,
                        kind.name(),
                        "unavailable",
                        None,
                        0,
                        started,
                    )?);
                }
            }
        }
        if prepared.refusal.is_some() {
            // The check will not run, so it must not keep another check off the directory.
            prepared.directory = None;
        }
        Ok(prepared)
    }

    /// Run one check while a monitor samples its warm directory. Above the bound the monitor
    /// ends the check's process group through the runner's supervised cancellation; the byte
    /// count it saw is returned so the caller records the failure and removes the directory.
    pub(crate) fn run_monitored(
        &self,
        runner: CheckRunner<'_>,
        definition: &CheckDefinition,
        directory: &WarmDirectory,
        cancellation: Option<&AtomicBool>,
    ) -> (CheckExecution, Option<u64>) {
        let stop = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        let exceeded = std::sync::Mutex::new(None);
        let max = self.policy.max_bytes();
        let execution = std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut sampled = Instant::now();
                while !done.load(Ordering::Acquire) {
                    if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                        stop.store(true, Ordering::Release);
                    }
                    if sampled.elapsed() >= SAMPLE_INTERVAL {
                        sampled = Instant::now();
                        let bytes = directory_bytes(directory.path());
                        if bytes > max {
                            *exceeded.lock().expect("warm bound monitor") = Some(bytes);
                            stop.store(true, Ordering::Release);
                            return;
                        }
                    }
                    std::thread::sleep(MONITOR_POLL);
                }
            });
            let execution = runner
                .with_cancellation(Some(&stop))
                .run_observed(definition);
            done.store(true, Ordering::Release);
            execution
        });
        let exceeded = *exceeded.lock().expect("warm bound monitor");
        (execution, exceeded)
    }

    /// After the check: a directory the monitor stopped, or one that ended above the bound, is
    /// removed while its lock is still held, so the next check runs cold. The bytes it held are
    /// returned for the caller to record on the kind's one observation as `evicted_bytes`.
    pub(crate) fn finish(
        &self,
        directory: Option<WarmDirectory>,
        exceeded: Option<u64>,
    ) -> Result<Option<u64>, String> {
        let Some(directory) = directory else {
            return Ok(None);
        };
        let bytes = exceeded.unwrap_or_else(|| directory.bytes());
        if exceeded.is_none() && bytes <= self.policy.max_bytes() {
            return Ok(None);
        }
        directory.remove()?;
        drop(directory);
        Ok(Some(bytes))
    }

    fn ineligible(
        &self,
        cas: &Cas,
        kind: &str,
        reason: &str,
        toolchain_id: Option<&str>,
        bytes: u64,
        started: Instant,
    ) -> Result<Observation, String> {
        Ok(Observation {
            kind: format!("{kind}:{reason}"),
            eligible: false,
            source_digest: self.identity(cas, kind, toolchain_id, Some(reason))?,
            toolchain_id: toolchain_id.map(String::from),
            bytes_available: bytes,
            lookup_ms: started.elapsed().as_millis() as u64,
            materialization_ms: 0,
            evicted_bytes: None,
        })
    }

    /// The path-free identity of the directory an observation is about: the project key, the
    /// toolchain and the kind, never a host path.
    fn identity(
        &self,
        cas: &Cas,
        kind: &str,
        toolchain_id: Option<&str>,
        reason: Option<&str>,
    ) -> Result<String, String> {
        cas.put_json(&json!({
            "schema": "af.task-build-cache/1",
            "project": self.project,
            "toolchain_id": toolchain_id,
            "kind": kind,
            "reason": reason,
        }))
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn stub(directory: &Path, program: &str, text: &str) {
        let path = directory.join(program);
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{text}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn toolchain(bin: &Path, rustc: &str) -> (tempfile::TempDir, Vec<(String, String)>) {
        stub(
            bin,
            "rustc",
            &format!("{rustc}\nhost: aarch64-apple-darwin"),
        );
        stub(bin, "cargo", "cargo 1.88.0 (873a06493 2025-05-10)");
        let workdir = tempfile::tempdir().unwrap();
        let environment = vec![
            ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
            ("LC_ALL".into(), "C".into()),
            ("TZ".into(), "UTC".into()),
            ("HOME".into(), workdir.path().display().to_string()),
        ];
        (workdir, environment)
    }

    fn declaration(bytes: &[u8]) -> ToolchainDeclaration {
        ToolchainDeclaration {
            path: Some("rust-toolchain.toml".into()),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn the_toolchain_key_follows_declaration_compiler_and_environment() {
        let bin = tempfile::tempdir().unwrap();
        let (workdir, environment) = toolchain(bin.path(), "rustc 1.88.0 (6b00bc388 2025-06-23)");
        let pinned = declaration(b"[toolchain]\nchannel = \"1.88.0\"\n");
        let first = toolchain_identity(
            workdir.path(),
            &environment,
            &pinned,
            None,
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(review_core::is_digest(&first));
        assert_eq!(
            toolchain_identity(
                workdir.path(),
                &environment,
                &pinned,
                None,
                Duration::from_secs(30)
            )
            .unwrap(),
            first,
            "the same toolchain is the same key"
        );
        let changed = declaration(b"[toolchain]\nchannel = \"1.89.0\"\n");
        assert_ne!(
            toolchain_identity(
                workdir.path(),
                &environment,
                &changed,
                None,
                Duration::from_secs(30)
            )
            .unwrap(),
            first,
            "a changed rust-toolchain.toml is a new key"
        );
        let absent = ToolchainDeclaration {
            path: None,
            bytes: b"none".to_vec(),
        };
        assert_ne!(
            toolchain_identity(
                workdir.path(),
                &environment,
                &absent,
                None,
                Duration::from_secs(30)
            )
            .unwrap(),
            first
        );
        let mut zone = environment.clone();
        zone[2].1 = "Europe/Berlin".into();
        assert_ne!(
            toolchain_identity(
                workdir.path(),
                &zone,
                &pinned,
                None,
                Duration::from_secs(30)
            )
            .unwrap(),
            first,
            "the check's fixed environment is part of the key"
        );
        stub(
            bin.path(),
            "rustc",
            "rustc 1.89.0 (29483883e 2025-08-04)\nhost: aarch64-apple-darwin",
        );
        assert_ne!(
            toolchain_identity(
                workdir.path(),
                &environment,
                &pinned,
                None,
                Duration::from_secs(30)
            )
            .unwrap(),
            first,
            "a rustc reporting another version is a new key"
        );
    }

    #[test]
    fn an_unresolvable_toolchain_is_an_error_not_a_key() {
        let bin = tempfile::tempdir().unwrap();
        let (workdir, mut environment) = toolchain(bin.path(), "rustc 1.88.0");
        stub(bin.path(), "rustc", "rustc 1.88.0");
        let pinned = declaration(b"x");
        assert!(
            toolchain_identity(
                workdir.path(),
                &environment,
                &pinned,
                None,
                Duration::from_secs(30)
            )
            .unwrap_err()
            .contains("host triple")
        );
        environment[0].1 = "/nonexistent".into();
        assert!(
            toolchain_identity(
                workdir.path(),
                &environment,
                &pinned,
                None,
                Duration::from_secs(30)
            )
            .is_err()
        );
        stub(bin.path(), "rustc", "rustc 1.88.0\nhost: x");
        std::fs::write(bin.path().join("cargo"), "#!/bin/sh\nexit 3\n").unwrap();
        environment[0].1 = format!("{}:/usr/bin:/bin", bin.path().display());
        assert!(
            toolchain_identity(
                workdir.path(),
                &environment,
                &pinned,
                None,
                Duration::from_secs(30)
            )
            .unwrap_err()
            .contains("cargo -vV")
        );
    }

    #[test]
    fn warm_policy_bounds_and_kinds_are_closed() {
        let policy: CodeWarmPolicy = toml::from_str("build_cache = [\"cargo_target\"]").unwrap();
        policy.validate().unwrap();
        assert_eq!(policy.max_bytes(), DEFAULT_WARM_MAX_BYTES);
        assert_eq!(
            serde_json::to_value(&policy).unwrap(),
            json!({"build_cache": ["cargo_target"]})
        );
        for refused in [
            "build_cache = []",
            "build_cache = [\"cargo_target\", \"cargo_target\"]",
            "caches = [\"cargo\", \"cargo\"]",
            "build_cache = [\"cargo_target\"]\nmax_bytes = 0",
            "build_cache = [\"cargo_target\"]\nmax_bytes = 34359738369",
        ] {
            let policy: CodeWarmPolicy = toml::from_str(refused).unwrap();
            assert!(policy.validate().is_err(), "{refused}");
        }
        for unknown in [
            "build_cache = [\"target\"]",
            "caches = [\"npm\"]",
            "build_cache = [\"cargo_target\"]\npath = \"/tmp\"",
        ] {
            assert!(
                toml::from_str::<CodeWarmPolicy>(unknown).is_err(),
                "{unknown}"
            );
        }
        let bound: CodeWarmPolicy =
            toml::from_str("build_cache = [\"cargo_target\"]\nmax_bytes = 34359738368").unwrap();
        bound.validate().unwrap();
        assert_eq!(bound.max_bytes(), MAX_WARM_MAX_BYTES);
    }

    struct Fixture {
        _bin: tempfile::TempDir,
        root: tempfile::TempDir,
        workdir: tempfile::TempDir,
        runtime: tempfile::TempDir,
        cas: Cas,
        environment: Vec<(String, String)>,
    }

    fn fixture() -> Fixture {
        let bin = tempfile::tempdir().unwrap();
        let (workdir, environment) = toolchain(bin.path(), "rustc 1.88.0 (stub)");
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path().join("cas")).unwrap();
        Fixture {
            _bin: bin,
            root,
            workdir,
            runtime: tempfile::tempdir().unwrap(),
            cas,
            environment,
        }
    }

    fn host(fixture: &Fixture, resolver: Option<Arc<CacheSourceResolver>>) -> WarmCheckHost {
        WarmCheckHost {
            root: Some(fixture.root.path().join("task-build-cache")),
            lock_wait: Duration::from_millis(200),
            resolver,
        }
    }

    fn warm(max_bytes: Option<u64>) -> CodeWarmPolicy {
        CodeWarmPolicy {
            build_cache: vec![BuildCacheKindSpec::CargoTarget],
            caches: vec![],
            max_bytes,
        }
    }

    fn prepare(fixture: &Fixture, session: &mut WarmSession<'_>) -> PreparedCheck {
        session
            .prepare(
                &fixture.cas,
                &fixture.environment,
                fixture.workdir.path(),
                fixture.runtime.path(),
                None,
                Duration::from_secs(120),
            )
            .unwrap()
    }

    fn target(prepared: &PreparedCheck) -> &str {
        &prepared.environment[0].1
    }

    #[test]
    fn a_held_directory_runs_the_next_check_cold_and_says_it_was_busy() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(None);
        let mut first = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let held = prepare(&fixture, &mut first);
        assert!(held.directory.is_some());
        assert_eq!(held.observations[0].kind, "cargo_target");
        assert!(held.observations[0].eligible);
        assert_eq!(held.observations[0].bytes_available, 0, "empty is cold");

        let mut second = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let busy = prepare(&fixture, &mut second);
        assert!(busy.directory.is_none());
        assert_eq!(busy.observations.len(), 1);
        assert_eq!(busy.observations[0].kind, "cargo_target:busy");
        assert!(!busy.observations[0].eligible);
        assert_eq!(
            busy.observations[0].toolchain_id,
            held.observations[0].toolchain_id
        );
        assert_eq!(
            target(&busy),
            fixture.runtime.path().join("target").display().to_string(),
            "a busy check builds in its private runtime directory"
        );
        drop(held);
        let mut third = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        assert!(prepare(&fixture, &mut third).directory.is_some());
    }

    #[test]
    fn a_directory_over_its_bound_is_removed_before_the_check_and_the_check_runs_cold() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let roomy = warm(None);
        let mut session = WarmSession::new(&host, &roomy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let directory = prepared.directory.unwrap();
        std::fs::write(directory.path().join("out"), [7_u8; 64]).unwrap();
        let path = directory.path().to_path_buf();
        assert!(
            session.finish(Some(directory), None).unwrap().is_none(),
            "under the bound the directory is kept"
        );

        let tight = warm(Some(1));
        let mut session = WarmSession::new(&host, &tight, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        assert!(prepared.directory.is_none());
        assert_eq!(prepared.observations[0].kind, "cargo_target:bound_exceeded");
        assert_eq!(prepared.observations[0].bytes_available, 64);
        assert!(!path.exists(), "removed, never trimmed");
        assert_eq!(
            target(&prepared),
            fixture.runtime.path().join("target").display().to_string()
        );
    }

    #[test]
    fn a_check_that_writes_past_the_bound_is_ended_and_its_directory_removed() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(Some(4096));
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let directory = prepared.directory.as_ref().unwrap();
        let path = directory.path().to_path_buf();
        let runner = CheckRunner::new(&fixture.cas, fixture.workdir.path())
            .with_timeout(Duration::from_secs(120))
            .with_env("CARGO_TARGET_DIR", target(&prepared));
        let check = CheckDefinition::new(
            "grow",
            review_core::Command::new(
                "/bin/sh",
                vec![
                    review_core::Arg::literal("-c"),
                    review_core::Arg::literal(
                        "head -c 8192 /dev/zero > \"$CARGO_TARGET_DIR/big\"; sleep 60",
                    ),
                ],
            ),
        );
        let started = Instant::now();
        let (execution, exceeded) = session.run_monitored(runner, &check, directory, None);
        assert!(started.elapsed() < Duration::from_secs(50), "ended early");
        assert_eq!(exceeded, Some(8192));
        assert_ne!(execution.result.status, review_check::CheckStatus::Passed);
        let removed = session.finish(prepared.directory, exceeded).unwrap();
        assert_eq!(
            removed,
            Some(8192),
            "the evicted bytes land on the kind's one observation"
        );
        assert!(!path.exists(), "removed before the lock was released");
    }

    #[test]
    fn an_unresolvable_toolchain_runs_cold_with_its_reason() {
        let mut fixture = fixture();
        fixture.environment[0].1 = "/nonexistent".into();
        let host = host(&fixture, None);
        let policy = warm(None);
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        assert!(prepared.directory.is_none());
        assert_eq!(
            prepared.observations[0].kind,
            "cargo_target:toolchain_unresolved"
        );
        assert_eq!(prepared.observations[0].toolchain_id, None);
        assert!(!fixture.root.path().join("task-build-cache").exists());
    }

    #[test]
    fn a_cache_snapshot_is_materialized_into_the_runtime_directory_only() {
        let fixture = fixture();
        let source = fixture.root.path().join("cargo-home");
        std::fs::create_dir_all(source.join("registry/cache/index")).unwrap();
        std::fs::write(source.join("registry/cache/index/a.crate"), b"crate").unwrap();
        let cache = CacheSource {
            kind: CacheKind::Cargo,
            source,
            limits: review_sandbox::CacheLimits {
                max_bytes: 1024,
                max_files: 16,
                max_copy_bytes: 1024,
            },
        };
        let resolver: Arc<CacheSourceResolver> = Arc::new(move |_| Ok(cache.clone()));
        let host = host(&fixture, Some(resolver));
        let policy = CodeWarmPolicy {
            build_cache: vec![],
            caches: vec![CacheKindSpec::Cargo],
            max_bytes: None,
        };
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        assert_eq!(prepared.refusal, None);
        let home = fixture.runtime.path().join(".af-cache/cargo");
        assert_eq!(
            prepared.environment[1..],
            [
                ("CARGO_HOME".to_string(), home.display().to_string()),
                ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
            ]
        );
        assert_eq!(
            std::fs::read(home.join("registry/cache/index/a.crate")).unwrap(),
            b"crate"
        );
        assert!(
            std::fs::read_dir(fixture.workdir.path())
                .unwrap()
                .next()
                .is_none(),
            "nothing enters the source tree"
        );
        assert_eq!(prepared.observations[0].kind, "cargo");
        assert_eq!(prepared.observations[0].bytes_available, 5);

        let unresolved = WarmCheckHost {
            resolver: None,
            ..host.clone()
        };
        let runtime = tempfile::tempdir().unwrap();
        let prepared = WarmSession::new(&unresolved, &policy, "repo", declaration(b"x"))
            .prepare(
                &fixture.cas,
                &fixture.environment,
                fixture.workdir.path(),
                runtime.path(),
                None,
                Duration::from_secs(120),
            )
            .unwrap();
        assert!(
            prepared
                .refusal
                .unwrap()
                .contains("Task check requested `cargo` cache")
        );
        assert_eq!(prepared.observations[0].kind, "cargo:unavailable");
    }

    #[test]
    fn the_project_key_is_an_opaque_path_component() {
        let key = project_key("../../etc,abc");
        assert_eq!(key.len(), 64);
        assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(key, project_key("another"));
    }
}
