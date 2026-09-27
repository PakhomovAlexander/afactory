//! Warm Task checks (ADR-0123). A code policy's `[warm]` table lets a Task check reuse
//! machine-local build directories across Attempts and Tasks of one project — Cargo's target
//! directory and Cargo's home — and bind an administrator-approved Cache Snapshot, under
//! `trusted_local` only.
//!
//! Each directory is keyed by the project's repository identity and by the toolchain the check
//! would run: the Snapshot's toolchain declaration, `rustc -vV`, `cargo -vV`, the host triple and
//! the check's fixed environment, including the kernel's rustup home. The directories of one
//! toolchain key share one byte bound, enforced before, during and after every check, and each
//! is removed rather than repaired whenever it is over its bound, cannot be fully inspected or is
//! otherwise suspect. They sit outside every sandbox: no Worker, seal, candidate capture or
//! delivery ever reads them. Whether a check ran warm or cold is evidence beside its `Check`
//! span, never part of its result.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use review_check::{CheckDefinition, CheckExecution, CheckRunner};
use review_config::CacheKindSpec;
use review_core::task::present_option;
use review_core::task::runtime::{TaskCacheObservationV1, TaskRuntimeRustupHomeV1};
use review_sandbox::{
    CacheError, CacheErrorKind, CacheKind, CacheSource, Ensured, TaskBuildCacheKeyLock,
    TaskBuildCacheLock, WarmDirectory, default_task_build_cache_root, directory_bytes,
    lock_task_build_cache, lock_task_build_cache_key, materialize_cache_into_runtime,
};
use review_source_git::Manifest;
use review_store::Cas;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `max_bytes` when a policy leaves it out.
pub const DEFAULT_WARM_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// The largest bound a policy may declare.
pub const MAX_WARM_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// The reason a check that grew its warm directories past the bound fails with.
pub const WARM_CACHE_BOUND_EXCEEDED: &str = "warm_cache_bound_exceeded";
/// The check result reason when a warm directory the check used is suspect once it ended: a
/// link, a special file, a forbidden name such as `credentials.toml`, or a root that is no
/// longer a private real directory. The directories are removed and the check fails.
pub const WARM_CACHE_SUSPECT: &str = "warm_cache_suspect";

/// Why a check's warm directories are removed after it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Excess {
    /// The key's directories were above the shared bound, or could not be fully counted, during
    /// the check or once it ended; the bytes are what was counted.
    Bound(u64),
    /// A directory the check used is suspect once it ended; the reason names what was found.
    Suspect(String),
}
/// The reason every declared kind of a check that never started records when the check
/// Attempt's deadline ran out before or during preparation.
pub(crate) const DEADLINE_EXHAUSTED: &str = "deadline_exhausted";

const TOOLCHAIN_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const MONITOR_POLL: Duration = Duration::from_millis(50);

/// The closed vocabulary of Warm Check Cache directories. Distinct from a Gate's
/// `build_caches` (ADR-0108), which clone a Round-scoped capture into a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmBuildCacheKind {
    /// Cargo's build directory, bound as `CARGO_TARGET_DIR`.
    CargoTarget,
    /// Cargo's registry and git caches, bound as `CARGO_HOME`. It never holds a credential.
    CargoHome,
}

impl WarmBuildCacheKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CargoTarget => "cargo_target",
            Self::CargoHome => "cargo_home",
        }
    }

    /// The variable the check receives the directory in.
    pub const fn variable(self) -> &'static str {
        match self {
            Self::CargoTarget => "CARGO_TARGET_DIR",
            Self::CargoHome => "CARGO_HOME",
        }
    }

    /// Names that make the directory suspect when present at its root: Cargo reads a registry
    /// token from either, and the kernel never lets one persist between checks.
    const fn forbidden(self) -> &'static [&'static str] {
        match self {
            Self::CargoTarget => &[],
            Self::CargoHome => &["credentials.toml", "credentials"],
        }
    }
}

/// The optional `[warm]` table of `af.code-task-policy/1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeWarmPolicy {
    /// Build directories a check may reuse: `cargo_target` and `cargo_home`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_cache: Vec<WarmBuildCacheKind>,
    /// Cache Snapshot kinds resolved through the machine's cache policy, as a Gate's
    /// `[gate] caches` are (ADR-0036). `cargo` takes precedence over a `cargo_home` directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<CacheKindSpec>,
    /// One bound shared by every warm directory of one toolchain key.
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

    /// A declared `cargo` Cache Snapshot binds `CARGO_HOME`, so a `cargo_home` directory is
    /// never bound beside it.
    fn supersedes(&self, kind: WarmBuildCacheKind) -> bool {
        kind == WarmBuildCacheKind::CargoHome && self.caches.contains(&CacheKindSpec::Cargo)
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

/// The rustup home a warm check and its toolchain probe receive. With a check's fresh `HOME`
/// a rustup proxy would otherwise install the pinned toolchain into that `HOME` before
/// answering, on every check. The kernel only passes the path on: it never creates, bounds,
/// writes or removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustupHome {
    pub(crate) path: Option<String>,
    pub(crate) source: TaskRuntimeRustupHomeV1,
}

impl RustupHome {
    /// The kernel process's own `RUSTUP_HOME`, else `$HOME/.rustup` of its `HOME` when that
    /// directory exists, else unset with the reason.
    pub(crate) fn of_kernel() -> Self {
        Self::resolve(std::env::var_os("RUSTUP_HOME"), std::env::var_os("HOME"))
    }

    pub(crate) fn resolve(rustup_home: Option<OsString>, home: Option<OsString>) -> Self {
        if let Some(path) = rustup_home
            .filter(|value| !value.is_empty())
            .and_then(|value| std::path::absolute(PathBuf::from(value)).ok())
            .and_then(|path| path.into_os_string().into_string().ok())
        {
            return Self {
                path: Some(path),
                source: TaskRuntimeRustupHomeV1::KernelEnvironment,
            };
        }
        let Some(home) = home
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
        else {
            return Self {
                path: None,
                source: TaskRuntimeRustupHomeV1::UnsetNoHome,
            };
        };
        match home.join(".rustup") {
            path if path.is_dir() => Self {
                path: path.into_os_string().into_string().ok(),
                source: TaskRuntimeRustupHomeV1::KernelHome,
            },
            _ => Self {
                path: None,
                source: TaskRuntimeRustupHomeV1::UnsetNotInstalled,
            },
        }
    }

    /// The variables a warm check and its probe receive: `RUSTUP_HOME` when one was found, and
    /// always `RUSTUP_AUTO_INSTALL=0`, so a toolchain the machine lacks is a probe failure and
    /// never a download.
    pub(crate) fn environment(&self) -> Vec<(String, String)> {
        self.path
            .iter()
            .map(|path| ("RUSTUP_HOME".to_string(), path.clone()))
            .chain([("RUSTUP_AUTO_INSTALL".to_string(), "0".to_string())])
            .collect()
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
/// toolchain file, a `PATH` or a rustup home that selects another compiler selects another key.
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
            fixed("RUSTUP_HOME"),
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
    /// `CARGO_TARGET_DIR` first, then a `cargo_home` directory's `CARGO_HOME`, then any Cache
    /// Snapshot's variables.
    pub(crate) environment: Vec<(String, String)>,
    /// The exclusive lock over the whole toolchain key, held from preparation to the end of the
    /// removal step so that checks of one project and toolchain never overlap.
    pub(crate) key_lock: Option<TaskBuildCacheKeyLock>,
    /// The locked warm directories the check builds into; an absent kind runs cold.
    pub(crate) directories: Vec<(WarmBuildCacheKind, WarmDirectory)>,
    /// Exactly one per declared kind, build directories first, in declared order.
    pub(crate) observations: Vec<Observation>,
    /// A declared Cache Snapshot that could not be materialized: the check does not run, exactly
    /// as a Gate does not dispatch its command.
    pub(crate) refusal: Option<String>,
}

/// One held directory between validation and measurement.
struct Held {
    kind: WarmBuildCacheKind,
    directory: WarmDirectory,
    toolchain: String,
    started: Instant,
    lookup_ms: u64,
    materialization_ms: u64,
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

    /// Lock, validate and then measure the build directories, and materialize any Cache
    /// Snapshot into `runtime`. `environment` is the check's own environment, used for the one
    /// toolchain probe. `budget` is the check Attempt's remaining wall time: the toolchain probe
    /// and every lock wait are bounded by what is left of it, so preparation can never outlive
    /// the reservation it prepares for.
    pub(crate) fn prepare(
        &mut self,
        cas: &Cas,
        environment: &[(String, String)],
        workdir: &Path,
        runtime: &Path,
        cancellation: Option<&AtomicBool>,
        budget: Duration,
    ) -> Result<PreparedCheck, String> {
        let preparing = Instant::now();
        let left = || budget.saturating_sub(preparing.elapsed());
        let mut prepared = PreparedCheck {
            environment: Vec::new(),
            key_lock: None,
            directories: Vec::new(),
            observations: Vec::new(),
            refusal: None,
        };
        let mut settled = BTreeMap::new();
        let mut held = Vec::new();
        // Locks are taken in one fixed kind order, so two checks never hold one kind each while
        // waiting for the other's.
        let mut kinds = self.policy.build_cache.clone();
        kinds.sort();
        for kind in kinds {
            let label = kind.as_str();
            let started = Instant::now();
            if self.policy.supersedes(kind) {
                let resolved = self.toolchain.clone().and_then(Result::ok);
                settled.insert(
                    kind,
                    self.ineligible(cas, label, "superseded", resolved.as_deref(), 0, started)?,
                );
                continue;
            }
            let toolchain = self
                .toolchain
                .get_or_insert_with(|| {
                    toolchain_identity(
                        workdir,
                        environment,
                        &self.declaration,
                        cancellation,
                        left().min(TOOLCHAIN_PROBE_TIMEOUT),
                    )
                })
                .clone();
            let toolchain = match toolchain {
                Ok(id) => id,
                Err(detail) => {
                    eprintln!("warm check cache diagnostic: {detail}");
                    settled.insert(
                        kind,
                        self.ineligible(cas, label, "toolchain_unresolved", None, 0, started)?,
                    );
                    continue;
                }
            };
            let root = self
                .host
                .root
                .clone()
                .map_or_else(default_task_build_cache_root, Ok);
            if prepared.key_lock.is_none() {
                // The whole toolchain key first: a check holds every kind's bound together, so
                // two checks declaring different kinds cannot each fill the shared bound.
                match root.clone().and_then(|root| {
                    lock_task_build_cache_key(
                        &root,
                        &self.project,
                        &hex(&toolchain),
                        self.host.lock_wait.min(left()),
                    )
                }) {
                    Ok(Some(lock)) => prepared.key_lock = Some(lock),
                    Ok(None) => {
                        settled.insert(
                            kind,
                            self.ineligible(cas, label, "busy", Some(&toolchain), 0, started)?,
                        );
                        continue;
                    }
                    Err(detail) => {
                        eprintln!("warm check cache diagnostic: {detail}");
                        settled.insert(
                            kind,
                            self.ineligible(
                                cas,
                                label,
                                "unavailable",
                                Some(&toolchain),
                                0,
                                started,
                            )?,
                        );
                        continue;
                    }
                }
            }
            let lock = root.and_then(|root| {
                lock_task_build_cache(
                    &root,
                    &self.project,
                    &hex(&toolchain),
                    label,
                    self.host.lock_wait.min(left()),
                )
            });
            let directory = match lock {
                Ok(TaskBuildCacheLock::Held(directory)) => directory.forbidding(kind.forbidden()),
                Ok(TaskBuildCacheLock::Busy) => {
                    settled.insert(
                        kind,
                        self.ineligible(cas, label, "busy", Some(&toolchain), 0, started)?,
                    );
                    continue;
                }
                Err(detail) => {
                    eprintln!("warm check cache diagnostic: {detail}");
                    settled.insert(
                        kind,
                        self.ineligible(cas, label, "unavailable", Some(&toolchain), 0, started)?,
                    );
                    continue;
                }
            };
            let lookup_ms = started.elapsed().as_millis() as u64;
            let materializing = Instant::now();
            match directory.ensure() {
                Ok(Ensured::Discarded(reason)) => {
                    // Removed and recreated empty: whatever it held is gone, so the measurement
                    // below finds a cold directory, never the discarded one's bytes.
                    eprintln!("warm check cache diagnostic: `{label}` discarded: {reason}");
                }
                Ok(Ensured::Reused | Ensured::Created) => {}
                Err(detail) => {
                    eprintln!("warm check cache diagnostic: {detail}");
                    settled.insert(
                        kind,
                        self.ineligible(cas, label, "unavailable", Some(&toolchain), 0, started)?,
                    );
                    continue;
                }
            }
            held.push(Held {
                kind,
                directory,
                toolchain,
                started,
                lookup_ms,
                materialization_ms: materializing.elapsed().as_millis() as u64,
            });
        }
        // Measured only after validation. The kinds of one toolchain key share the bound; one
        // that cannot be fully counted is above it.
        let mut measured = Vec::with_capacity(held.len());
        let mut over = false;
        let mut total = 0_u64;
        for held in held {
            let bytes = match held.directory.bytes() {
                Ok(bytes) => bytes,
                Err(uninspectable) => {
                    eprintln!("warm check cache diagnostic: {uninspectable}");
                    over = true;
                    uninspectable.counted
                }
            };
            total = total.saturating_add(bytes);
            measured.push((held, bytes));
        }
        over |= total > self.policy.max_bytes();
        for (held, bytes) in measured {
            let label = held.kind.as_str();
            if over {
                // Over the bound before the check: removed, never trimmed, and this check runs
                // cold for every kind in its private runtime directory.
                held.directory.remove()?;
                settled.insert(
                    held.kind,
                    self.ineligible(
                        cas,
                        label,
                        "bound_exceeded",
                        Some(&held.toolchain),
                        bytes,
                        held.started,
                    )?,
                );
                continue;
            }
            settled.insert(
                held.kind,
                Observation {
                    kind: label.into(),
                    eligible: true,
                    source_digest: self.identity(cas, label, Some(&held.toolchain), None)?,
                    toolchain_id: Some(held.toolchain),
                    bytes_available: bytes,
                    lookup_ms: held.lookup_ms,
                    materialization_ms: held.materialization_ms,
                    evicted_bytes: None,
                },
            );
            prepared.directories.push((held.kind, held.directory));
        }
        for kind in &self.policy.build_cache {
            prepared.observations.push(
                settled
                    .remove(kind)
                    .ok_or("Warm check left a declared kind unobserved")?,
            );
        }
        let bound = |kind: WarmBuildCacheKind| {
            prepared
                .directories
                .iter()
                .find(|(held, _)| *held == kind)
                .map(|(_, directory)| directory.path().display().to_string())
        };
        prepared.environment.push((
            WarmBuildCacheKind::CargoTarget.variable().into(),
            bound(WarmBuildCacheKind::CargoTarget)
                .unwrap_or_else(|| runtime.join("target").display().to_string()),
        ));
        if let Some(home) = bound(WarmBuildCacheKind::CargoHome) {
            prepared
                .environment
                .push((WarmBuildCacheKind::CargoHome.variable().into(), home));
        }
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
            // The check will not run, so it must not keep another check off a directory, and no
            // kind it prepared may read as used.
            prepared.directories.clear();
            prepared.key_lock = None;
            prepared.observations =
                self.not_started(cas, prepared.observations, "cache_refused")?;
        }
        Ok(prepared)
    }

    /// The observations of a check skipped before preparation: every declared kind, ineligible
    /// with `reason`, so the check keeps one observation per kind although it never started.
    pub(crate) fn skipped(&self, cas: &Cas, reason: &str) -> Result<Vec<Observation>, String> {
        let started = Instant::now();
        let toolchain = self.toolchain.clone().and_then(Result::ok);
        let mut observations = Vec::new();
        for kind in &self.policy.build_cache {
            observations.push(self.ineligible(
                cas,
                kind.as_str(),
                reason,
                toolchain.as_deref(),
                0,
                started,
            )?);
        }
        for kind in &self.policy.caches {
            observations.push(self.ineligible(
                cas,
                cache_kind(*kind).name(),
                reason,
                None,
                0,
                started,
            )?);
        }
        Ok(observations)
    }

    /// A check that was prepared but never started used nothing it prepared: each eligible
    /// observation becomes ineligible with `reason`, keeping what it measured. An observation
    /// that already names its reason keeps it.
    pub(crate) fn not_started(
        &self,
        cas: &Cas,
        observations: Vec<Observation>,
        reason: &str,
    ) -> Result<Vec<Observation>, String> {
        observations
            .into_iter()
            .map(|observation| {
                if !observation.eligible {
                    return Ok(observation);
                }
                Ok(Observation {
                    kind: format!("{}:{reason}", observation.kind),
                    eligible: false,
                    source_digest: self.identity(
                        cas,
                        &observation.kind,
                        observation.toolchain_id.as_deref(),
                        Some(reason),
                    )?,
                    ..observation
                })
            })
            .collect()
    }

    /// Run one check while a monitor samples its warm directories. Above the shared bound the
    /// monitor ends the check's process group through the runner's supervised cancellation.
    /// However fast the check was, the directories are measured again once it has ended and
    /// before its result is accepted, so a check that wrote past the bound between two samples
    /// is caught too. A directory the count cannot fully inspect is above the bound. The byte
    /// count that crossed it is returned so the caller fails the check and removes the
    /// directories.
    pub(crate) fn run_monitored(
        &self,
        runner: CheckRunner<'_>,
        definition: &CheckDefinition,
        directories: &[(WarmBuildCacheKind, WarmDirectory)],
        cancellation: Option<&AtomicBool>,
    ) -> (CheckExecution, Option<Excess>) {
        let stop = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        let exceeded: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);
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
                        if let Some(bytes) = over_bound(directories, max) {
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
        let sampled = *exceeded.lock().expect("warm bound monitor");
        // Once the check ended, however fast: the whole key is measured again, and every
        // directory the check used is judged as `ensure` would judge it before the next check
        // — a credential written into a Cargo home, a link, a swapped root, all make it suspect.
        let excess = sampled
            .or_else(|| over_bound(directories, max))
            .map(Excess::Bound)
            .or_else(|| {
                directories.iter().find_map(|(kind, directory)| {
                    directory
                        .inspect()
                        .map(|reason| Excess::Suspect(format!("{}: {reason}", kind.as_str())))
                })
            });
        (execution, excess)
    }

    /// After the check: when [`Self::run_monitored`] found the directories above the shared
    /// bound, during the check or once it ended, they are all removed while their locks are
    /// still held, so the next check runs cold. Each kind's removed bytes are returned for the
    /// caller to record on that kind's one observation as `evicted_bytes`; an empty list means
    /// nothing was over the bound.
    pub(crate) fn finish(
        &self,
        directories: Vec<(WarmBuildCacheKind, WarmDirectory)>,
        excess: Option<Excess>,
    ) -> Result<Vec<(WarmBuildCacheKind, u64)>, String> {
        if excess.is_none() {
            return Ok(Vec::new());
        }
        let mut evicted = Vec::with_capacity(directories.len());
        for (kind, directory) in directories {
            let bytes = directory
                .bytes()
                .unwrap_or_else(|uninspectable| uninspectable.counted);
            directory.remove()?;
            drop(directory);
            evicted.push((kind, bytes));
        }
        Ok(evicted)
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

/// The directories' combined bytes when they are above `max`, or when any of them cannot be
/// fully counted: what the kernel cannot see it cannot bound.
/// Whether the toolchain key the directories belong to is above `max`. The whole key directory
/// is counted — every kind under it, held by this check or not — because the bound is shared
/// by the key, and each held root must still be a real directory: a root swapped for a link or
/// a file is uninspectable, and an uninspectable key is above every bound.
fn over_bound(directories: &[(WarmBuildCacheKind, WarmDirectory)], max: u64) -> Option<u64> {
    let (_, first) = directories.first()?;
    let mut uninspectable = false;
    for (_, directory) in directories {
        match std::fs::symlink_metadata(directory.path()) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => uninspectable = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => uninspectable = true,
        }
    }
    let key = first.path().parent().unwrap_or_else(|| first.path());
    let total = match directory_bytes(key) {
        Ok(bytes) => bytes,
        Err(error) => {
            uninspectable = true;
            error.counted
        }
    };
    (uninspectable || total > max).then_some(total)
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
        let both: CodeWarmPolicy =
            toml::from_str("build_cache = [\"cargo_target\", \"cargo_home\"]").unwrap();
        both.validate().unwrap();
        assert_eq!(
            both.build_cache,
            [
                WarmBuildCacheKind::CargoTarget,
                WarmBuildCacheKind::CargoHome
            ]
        );
        assert_eq!(policy.max_bytes(), DEFAULT_WARM_MAX_BYTES);
        assert_eq!(
            serde_json::to_value(&policy).unwrap(),
            json!({"build_cache": ["cargo_target"]})
        );
        for refused in [
            "build_cache = []",
            "build_cache = [\"cargo_target\", \"cargo_target\"]",
            "build_cache = [\"cargo_home\", \"cargo_home\"]",
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
            build_cache: vec![WarmBuildCacheKind::CargoTarget],
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
        assert_eq!(held.directories.len(), 1);
        assert_eq!(held.observations[0].kind, "cargo_target");
        assert!(held.observations[0].eligible);
        assert_eq!(held.observations[0].bytes_available, 0, "empty is cold");

        let mut second = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let busy = prepare(&fixture, &mut second);
        assert!(busy.directories.is_empty());
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
        assert_eq!(prepare(&fixture, &mut third).directories.len(), 1);
    }

    #[test]
    fn a_directory_over_its_bound_is_removed_before_the_check_and_the_check_runs_cold() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let roomy = warm(None);
        let mut session = WarmSession::new(&host, &roomy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let PreparedCheck {
            directories,
            key_lock,
            ..
        } = prepared;
        let directory = directories.into_iter().next().unwrap();
        std::fs::write(directory.1.path().join("out"), [7_u8; 64]).unwrap();
        let path = directory.1.path().to_path_buf();
        assert!(
            session.finish(vec![directory], None).unwrap().is_empty(),
            "under the bound the directory is kept"
        );
        // The key stays locked until the check's removal step has ended; release it here.
        drop(key_lock);

        let tight = warm(Some(1));
        let mut session = WarmSession::new(&host, &tight, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        assert!(prepared.directories.is_empty());
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
        let path = prepared.directories[0].1.path().to_path_buf();
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
        let (execution, exceeded) =
            session.run_monitored(runner, &check, &prepared.directories, None);
        assert!(started.elapsed() < Duration::from_secs(50), "ended early");
        assert_eq!(exceeded, Some(Excess::Bound(8192)));
        assert_ne!(execution.result.status, review_check::CheckStatus::Passed);
        let removed = session.finish(prepared.directories, exceeded).unwrap();
        assert_eq!(
            removed,
            [(WarmBuildCacheKind::CargoTarget, 8192)],
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
        assert!(prepared.directories.is_empty());
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

    fn run(
        fixture: &Fixture,
        session: &WarmSession<'_>,
        prepared: &PreparedCheck,
        script: &str,
    ) -> (CheckExecution, Option<Excess>) {
        let runner = prepared.environment.iter().fold(
            CheckRunner::new(&fixture.cas, fixture.workdir.path())
                .with_timeout(Duration::from_secs(120)),
            |runner, (key, value)| runner.with_env(key.clone(), value.clone()),
        );
        let check = CheckDefinition::new(
            "grow",
            review_core::Command::new(
                "/bin/sh",
                vec![
                    review_core::Arg::literal("-c"),
                    review_core::Arg::literal(script),
                ],
            ),
        );
        session.run_monitored(runner, &check, &prepared.directories, None)
    }

    #[test]
    fn a_fast_check_that_writes_past_the_bound_is_caught_after_it_ends() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(Some(4096));
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let path = prepared.directories[0].1.path().to_path_buf();
        let started = Instant::now();
        let (execution, exceeded) = run(
            &fixture,
            &session,
            &prepared,
            "head -c 8192 /dev/zero > \"$CARGO_TARGET_DIR/big\"",
        );
        assert!(
            started.elapsed() < SAMPLE_INTERVAL,
            "the check ended before the monitor's first sample"
        );
        assert_eq!(
            execution.result.status,
            review_check::CheckStatus::Passed,
            "the command itself succeeded"
        );
        assert_eq!(
            exceeded,
            Some(Excess::Bound(8192)),
            "measured once it ended"
        );
        assert_eq!(
            session.finish(prepared.directories, exceeded).unwrap(),
            [(WarmBuildCacheKind::CargoTarget, 8192)]
        );
        assert!(!path.exists());
    }

    #[test]
    fn an_unreadable_subtree_a_check_leaves_behind_is_above_every_bound() {
        // Running as root reads through mode 000, so the fixture cannot be built.
        let probe = tempfile::tempdir().unwrap();
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(probe.path()).is_ok();
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if readable {
            return;
        }
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(Some(1024 * 1024));
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let path = prepared.directories[0].1.path().to_path_buf();
        let (_, exceeded) = run(
            &fixture,
            &session,
            &prepared,
            "mkdir \"$CARGO_TARGET_DIR/hidden\" \
             && head -c 2097152 /dev/zero > \"$CARGO_TARGET_DIR/hidden/big\" \
             && chmod 000 \"$CARGO_TARGET_DIR/hidden\"",
        );
        assert!(
            exceeded.is_some(),
            "an uninspectable directory is never under its bound"
        );
        let evicted = session.finish(prepared.directories, exceeded).unwrap();
        assert_eq!(evicted.len(), 1);
        assert!(
            !path.exists(),
            "removed under the lock, unreadable child and all"
        );
    }

    #[test]
    fn a_discarded_directory_is_reported_cold_never_with_its_old_bytes() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(None);
        let populate = |session: &mut WarmSession<'_>| {
            let prepared = prepare(&fixture, session);
            let (kind, directory) = prepared.directories.into_iter().next().unwrap();
            std::fs::write(directory.path().join("build.bin"), [1_u8; 512]).unwrap();
            let path = directory.path().to_path_buf();
            assert!(
                session
                    .finish(vec![(kind, directory)], None)
                    .unwrap()
                    .is_empty()
            );
            path
        };
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let path = populate(&mut session);
        let warm = prepare(&fixture, &mut session);
        assert_eq!(
            warm.observations[0].bytes_available, 512,
            "reused while private"
        );
        drop(warm);

        // A same-user widened directory.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let widened = prepare(&fixture, &mut session);
        assert_eq!(widened.observations[0].kind, "cargo_target");
        assert!(widened.observations[0].eligible);
        assert_eq!(
            widened.observations[0].bytes_available, 0,
            "recreated: a cold check"
        );
        assert!(!path.join("build.bin").exists());
        drop(widened);

        // A directory holding a link.
        let path = populate(&mut session);
        std::os::unix::fs::symlink("/etc/hosts", path.join("link")).unwrap();
        let linked = prepare(&fixture, &mut session);
        assert!(linked.observations[0].eligible);
        assert_eq!(linked.observations[0].bytes_available, 0);
        assert!(std::fs::symlink_metadata(path.join("link")).is_err());
    }

    fn both(max_bytes: Option<u64>, caches: Vec<CacheKindSpec>) -> CodeWarmPolicy {
        CodeWarmPolicy {
            build_cache: vec![
                WarmBuildCacheKind::CargoTarget,
                WarmBuildCacheKind::CargoHome,
            ],
            caches,
            max_bytes,
        }
    }

    #[test]
    fn cargo_home_is_a_second_keyed_directory_bound_as_cargo_home() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = both(None, vec![]);
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let kinds: Vec<_> = prepared
            .observations
            .iter()
            .map(|o| o.kind.as_str())
            .collect();
        assert_eq!(kinds, ["cargo_target", "cargo_home"]);
        assert_eq!(
            prepared.observations[0].toolchain_id, prepared.observations[1].toolchain_id,
            "one toolchain key"
        );
        assert_ne!(
            prepared.observations[0].source_digest,
            prepared.observations[1].source_digest
        );
        let target = prepared.directories[0].1.path();
        let home = prepared.directories[1].1.path();
        assert_eq!(target.parent(), home.parent());
        assert!(home.ends_with("cargo_home"));
        assert_eq!(
            prepared.environment,
            [
                ("CARGO_TARGET_DIR".to_string(), target.display().to_string()),
                ("CARGO_HOME".to_string(), home.display().to_string()),
            ]
        );
        std::fs::write(
            home.join("credentials.toml"),
            b"[registry]\ntoken = \"x\"\n",
        )
        .unwrap();
        std::fs::create_dir(home.join("registry")).unwrap();
        std::fs::write(home.join("registry/index"), [0_u8; 32]).unwrap();
        let home = home.to_path_buf();
        assert!(
            session
                .finish(prepared.directories, None)
                .unwrap()
                .is_empty()
        );
        drop(prepared.key_lock);

        let again = prepare(&fixture, &mut session);
        assert_eq!(again.observations[1].kind, "cargo_home");
        assert_eq!(
            again.observations[1].bytes_available, 0,
            "a credential makes the directory suspect: removed, never reused"
        );
        assert!(!home.join("credentials.toml").exists());
        assert!(!home.join("registry").exists());
    }

    #[test]
    fn the_kinds_of_one_toolchain_share_one_bound() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let roomy = both(None, vec![]);
        let mut session = WarmSession::new(&host, &roomy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        for (_, directory) in &prepared.directories {
            std::fs::write(directory.path().join("half"), [0_u8; 3000]).unwrap();
        }
        let paths: Vec<_> = prepared
            .directories
            .iter()
            .map(|(_, directory)| directory.path().to_path_buf())
            .collect();
        drop(prepared);

        // Each directory alone is under 4096; together they are not.
        let tight = both(Some(4096), vec![]);
        let mut session = WarmSession::new(&host, &tight, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        assert!(prepared.directories.is_empty());
        let observed: Vec<_> = prepared
            .observations
            .iter()
            .map(|o| (o.kind.as_str(), o.eligible, o.bytes_available))
            .collect();
        assert_eq!(
            observed,
            [
                ("cargo_target:bound_exceeded", false, 3000),
                ("cargo_home:bound_exceeded", false, 3000),
            ]
        );
        assert!(paths.iter().all(|path| !path.exists()));
        assert_eq!(
            prepared.environment.len(),
            1,
            "no CARGO_HOME for a cold home"
        );
    }

    fn cargo_cache(fixture: &Fixture) -> Arc<CacheSourceResolver> {
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
        Arc::new(move |_| Ok(cache.clone()))
    }

    #[test]
    fn a_cargo_cache_snapshot_supersedes_the_cargo_home_directory() {
        let fixture = fixture();
        let host = host(&fixture, Some(cargo_cache(&fixture)));
        let policy = both(None, vec![CacheKindSpec::Cargo]);
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let kinds: Vec<_> = prepared
            .observations
            .iter()
            .map(|o| (o.kind.as_str(), o.eligible))
            .collect();
        assert_eq!(
            kinds,
            [
                ("cargo_target", true),
                ("cargo_home:superseded", false),
                ("cargo", true),
            ]
        );
        assert_eq!(prepared.directories.len(), 1);
        let homes: Vec<_> = prepared
            .environment
            .iter()
            .filter(|(key, _)| key == "CARGO_HOME")
            .collect();
        assert_eq!(
            homes.len(),
            1,
            "never bound twice: {:?}",
            prepared.environment
        );
        assert_eq!(
            homes[0].1,
            fixture
                .runtime
                .path()
                .join(".af-cache/cargo")
                .display()
                .to_string(),
            "CARGO_HOME is the Cache Snapshot"
        );
    }

    #[test]
    fn a_check_that_never_starts_observes_every_declared_kind() {
        let fixture = fixture();
        let policy = both(None, vec![CacheKindSpec::Cargo]);
        let unresolved = host(&fixture, None);
        let mut session = WarmSession::new(&unresolved, &policy, "repo", declaration(b"x"));
        let refused = prepare(&fixture, &mut session);
        assert!(refused.refusal.is_some());
        assert!(
            refused.directories.is_empty(),
            "a refused check holds no lock"
        );
        let kinds: Vec<_> = refused
            .observations
            .iter()
            .map(|o| (o.kind.as_str(), o.eligible))
            .collect();
        assert_eq!(
            kinds,
            [
                ("cargo_target:cache_refused", false),
                ("cargo_home:superseded", false),
                ("cargo:unavailable", false),
            ]
        );

        let skipped = session.skipped(&fixture.cas, DEADLINE_EXHAUSTED).unwrap();
        let kinds: Vec<_> = skipped.iter().map(|o| o.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "cargo_target:deadline_exhausted",
                "cargo_home:deadline_exhausted",
                "cargo:deadline_exhausted",
            ]
        );
        assert!(
            skipped
                .iter()
                .all(|o| !o.eligible && o.bytes_available == 0)
        );
    }

    #[test]
    fn the_rustup_home_is_the_kernels_or_its_homes_or_unset_with_a_reason() {
        let home = tempfile::tempdir().unwrap();
        let resolved = RustupHome::resolve(
            Some("/opt/rustup".into()),
            Some(home.path().as_os_str().into()),
        );
        assert_eq!(resolved.path.as_deref(), Some("/opt/rustup"));
        assert_eq!(resolved.source, TaskRuntimeRustupHomeV1::KernelEnvironment);
        assert_eq!(
            resolved.environment(),
            [
                ("RUSTUP_HOME".to_string(), "/opt/rustup".to_string()),
                ("RUSTUP_AUTO_INSTALL".to_string(), "0".to_string()),
            ]
        );
        let missing = RustupHome::resolve(Some("".into()), Some(home.path().as_os_str().into()));
        assert_eq!(missing.source, TaskRuntimeRustupHomeV1::UnsetNotInstalled);
        assert_eq!(
            missing.environment(),
            [("RUSTUP_AUTO_INSTALL".to_string(), "0".to_string())],
            "never a download, even without a rustup home"
        );
        std::fs::create_dir(home.path().join(".rustup")).unwrap();
        let installed = RustupHome::resolve(None, Some(home.path().as_os_str().into()));
        assert_eq!(installed.source, TaskRuntimeRustupHomeV1::KernelHome);
        assert_eq!(
            installed.path,
            Some(home.path().join(".rustup").display().to_string())
        );
        for home in [None, Some("".into()), Some("relative".into())] {
            assert_eq!(
                RustupHome::resolve(None, home).source,
                TaskRuntimeRustupHomeV1::UnsetNoHome
            );
        }
    }

    #[test]
    fn the_rustup_home_is_part_of_the_toolchain_key() {
        let bin = tempfile::tempdir().unwrap();
        let (workdir, environment) = toolchain(bin.path(), "rustc 1.88.0 (stub)");
        let key = |environment: &[(String, String)]| {
            toolchain_identity(
                workdir.path(),
                environment,
                &declaration(b"x"),
                None,
                Duration::from_secs(30),
            )
            .unwrap()
        };
        let unset = key(&environment);
        let mut bound = environment.clone();
        bound.push(("RUSTUP_HOME".into(), "/opt/rustup".into()));
        let first = key(&bound);
        assert_ne!(first, unset);
        bound.last_mut().unwrap().1 = "/opt/other".into();
        assert_ne!(key(&bound), first);
        bound.push(("RUSTUP_AUTO_INSTALL".into(), "0".into()));
        let mut same = environment.clone();
        same.push(("RUSTUP_HOME".into(), "/opt/other".into()));
        assert_eq!(
            key(&bound),
            key(&same),
            "only the fixed environment is keyed"
        );
    }

    fn only_home(max_bytes: Option<u64>) -> CodeWarmPolicy {
        CodeWarmPolicy {
            build_cache: vec![WarmBuildCacheKind::CargoHome],
            caches: vec![],
            max_bytes,
        }
    }

    #[test]
    fn a_credential_written_during_a_check_makes_its_cargo_home_suspect_and_gone() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = both(None, vec![]);
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        assert_eq!(prepared.directories.len(), 2);
        let paths: Vec<PathBuf> = prepared
            .directories
            .iter()
            .map(|(_, directory)| directory.path().to_path_buf())
            .collect();
        let (execution, excess) = run(
            &fixture,
            &session,
            &prepared,
            "printf 'token' > \"$CARGO_HOME/credentials.toml\"",
        );
        assert_eq!(execution.result.status, review_check::CheckStatus::Passed);
        match &excess {
            Some(Excess::Suspect(reason)) => assert!(reason.contains("cargo_home"), "{reason}"),
            other => panic!("a credential is suspect, got {other:?}"),
        }
        let evicted = session.finish(prepared.directories, excess).unwrap();
        assert_eq!(evicted.len(), 2, "every directory of the key goes");
        for path in paths {
            assert!(!path.exists(), "{} removed under the lock", path.display());
        }
    }

    #[test]
    fn a_root_swapped_for_a_link_during_a_check_is_above_every_bound_and_removed() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(Some(1024 * 1024));
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let path = prepared.directories[0].1.path().to_path_buf();
        let outside = fixture.root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let (_, excess) = run(
            &fixture,
            &session,
            &prepared,
            &format!(
                "rm -rf \"$CARGO_TARGET_DIR\" && ln -s {} \"$CARGO_TARGET_DIR\" \
                 && head -c 4096 /dev/zero > \"$CARGO_TARGET_DIR/big\"",
                outside.display()
            ),
        );
        assert!(
            matches!(excess, Some(Excess::Bound(_))),
            "a link root cannot be counted: {excess:?}"
        );
        session.finish(prepared.directories, excess).unwrap();
        assert!(
            std::fs::symlink_metadata(&path).is_err(),
            "the link at the root is removed under the lock"
        );
        assert!(
            outside.join("big").exists(),
            "the link's target is never followed"
        );
    }

    #[test]
    fn checks_of_one_toolchain_key_are_serialized_whatever_kinds_they_declare() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let target_only = warm(None);
        let home_only = only_home(None);
        let mut first = WarmSession::new(&host, &target_only, "repo", declaration(b"x"));
        let held = prepare(&fixture, &mut first);
        assert_eq!(held.directories.len(), 1);
        assert!(held.key_lock.is_some(), "the key is held with the kind");
        let mut second = WarmSession::new(&host, &home_only, "repo", declaration(b"x"));
        let busy = prepare(&fixture, &mut second);
        assert!(busy.directories.is_empty());
        assert_eq!(busy.observations[0].kind, "cargo_home:busy");
        drop(held);
        let mut third = WarmSession::new(&host, &home_only, "repo", declaration(b"x"));
        let free = prepare(&fixture, &mut third);
        assert_eq!(free.directories.len(), 1, "released with the first check");
        assert_eq!(free.observations[0].kind, "cargo_home");
    }
}
