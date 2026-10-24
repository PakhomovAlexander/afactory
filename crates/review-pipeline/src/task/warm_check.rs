//! Warm Task checks (ADR-0131). A code policy's `[warm]` table lets a Task check reuse
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
use review_core::task::check_layer::{
    TaskLayerEvictionV1, TaskLayerEvictorV1, TaskLayerHolderV1, TaskLayerLockV1, TaskLayerPhaseV1,
    TaskLayerSourcesV1, WARM_HOLDER_V1, WarmEvictionV1, WarmHolderV1,
};
use review_core::task::present_option;
use review_core::task::runtime::{
    TaskCacheBoundV1, TaskCacheObservationV1, TaskRuntimeRustupHomeV1,
};
use review_sandbox::{
    CacheError, CacheErrorKind, CacheKind, CacheSource, Ensured, TaskBuildCacheKeyLock,
    TaskBuildCacheLock, WarmDirectory, clone_warm_kind, default_task_build_cache_root,
    lock_task_build_cache_key_waiting, materialize_cache_into_runtime, read_key_eviction,
    read_key_holder,
};
use review_source_git::Manifest;
use review_store::Cas;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `max_bytes` when a policy leaves it out.
pub const DEFAULT_WARM_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// The largest bound a policy may declare, for `max_bytes` and `hard_max_bytes` alike: the disk
/// safety ADR-0131 set is not widened by the second bound (ADR-0135).
pub const MAX_WARM_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// The reason a check that grew its warm directories past `hard_max_bytes` fails with.
pub const WARM_CACHE_BOUND_EXCEEDED: &str = "warm_cache_bound_exceeded";
/// The check result reason when a warm directory the check used is suspect once it ended: a
/// link, a special file, a forbidden name such as `credentials.toml`, or a root that is no
/// longer a private real directory. The directories are removed and the check fails.
pub const WARM_CACHE_SUSPECT: &str = "warm_cache_suspect";

/// Why a check's warm directories are removed after it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Excess {
    /// The key's directories were above `max_bytes` once the check ended but never above
    /// `hard_max_bytes`: they are evicted before the next check and the check's own result
    /// stands (ADR-0135). The bytes are what was counted.
    Evict(u64),
    /// The key's directories were above `hard_max_bytes`, or could not be fully counted, during
    /// the check or once it ended; the check fails. The bytes are what was counted.
    Bound(u64),
    /// A directory the check used is suspect once it ended; the reason names what was found.
    Suspect(String),
}
/// The reason every declared kind of a check that never started records when the check
/// Attempt's deadline ran out before or during preparation.
pub(crate) const DEADLINE_EXHAUSTED: &str = "deadline_exhausted";

mod sources;

/// The offline check's npm cache rules (ADR-0201), for callers that fill or inspect a cache.
pub use review_check::NpmPackagePin as NpmPin;
pub use review_check::offline;
pub(crate) use sources::{PackageIndex, SourcePlan};

/// The build deadline this repository declares, and the one the documentation recommends
/// (ADR-0201). A policy that leaves `[warm] build_wall_ms` out keeps one deadline for the
/// whole check: a default would lengthen every warm check Attempt and refuse Task files whose
/// verification reserve was sized for `check_wall_ms` alone.
pub const RECOMMENDED_BUILD_WALL_MS: u64 = 2_400_000;
/// The longest build phase a policy may declare.
pub const MAX_BUILD_WALL_MS: u64 = 3_600_000;

const TOOLCHAIN_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a check waiting for a held layer says so (ADR-0201).
const WAIT_NOTICE: Duration = Duration::from_secs(60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const MONITOR_POLL: Duration = Duration::from_millis(50);
/// How often the monitor reads a check's report for the end of its build phase.
const PHASE_POLL: Duration = Duration::from_secs(1);

/// The closed vocabulary of Warm Check Cache directories. Distinct from a Gate's
/// `build_caches` (ADR-0108), which clone a Round-scoped capture into a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmBuildCacheKind {
    /// Cargo's build directory, bound as `CARGO_TARGET_DIR`.
    CargoTarget,
    /// Cargo's registry and git caches, bound as `CARGO_HOME`. It never holds a credential.
    CargoHome,
    /// npm's content-addressed cache, bound as `npm_config_cache`: what an offline check
    /// (ADR-0201) installs its pinned packages from without network.
    NpmCache,
}

impl WarmBuildCacheKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CargoTarget => "cargo_target",
            Self::CargoHome => "cargo_home",
            Self::NpmCache => "npm_cache",
        }
    }

    /// The variable the check receives the directory in.
    pub const fn variable(self) -> &'static str {
        match self {
            Self::CargoTarget => "CARGO_TARGET_DIR",
            Self::CargoHome => "CARGO_HOME",
            Self::NpmCache => "npm_config_cache",
        }
    }

    /// Names that make the directory suspect when present at its root: Cargo reads a registry
    /// token from either, and the kernel never lets one persist between checks.
    const fn forbidden(self) -> &'static [&'static str] {
        match self {
            Self::CargoTarget | Self::NpmCache => &[],
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
    /// The eviction bound shared by every warm directory of one toolchain key: a key above it is
    /// removed before a check and after one, whose result stands.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub max_bytes: Option<u64>,
    /// The only bound that ends a running check (ADR-0135): twice `max_bytes` when absent, never
    /// more than [`MAX_WARM_MAX_BYTES`] and never less than `max_bytes`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub hard_max_bytes: Option<u64>,
    /// When another check holds the layer, build in a private copy of it instead of waiting
    /// (ADR-0201). Absent or `false`, a check waits for the layer until its deadline.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub clone_when_busy: Option<bool>,
    /// The deadline of a check's build phase, until it reports its `build` step (ADR-0201);
    /// `check_wall_ms` then governs the tests. Absent, the whole check has `check_wall_ms`, as
    /// before; see [`RECOMMENDED_BUILD_WALL_MS`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub build_wall_ms: Option<u64>,
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
        if let Some(hard) = self.hard_max_bytes
            && (hard < self.max_bytes() || hard > MAX_WARM_MAX_BYTES)
        {
            return Err(format!(
                "Code policy [warm] hard_max_bytes must be between max_bytes ({}) and {MAX_WARM_MAX_BYTES}",
                self.max_bytes()
            ));
        }
        if self
            .build_wall_ms
            .is_some_and(|ms| ms == 0 || ms > MAX_BUILD_WALL_MS)
        {
            return Err(format!(
                "Code policy [warm] build_wall_ms must be between 1 and {MAX_BUILD_WALL_MS}"
            ));
        }
        Ok(())
    }

    /// The build phase's own deadline, when the policy declares one.
    pub fn build_wall_ms(&self) -> Option<u64> {
        self.build_wall_ms
    }

    pub fn clone_when_busy(&self) -> bool {
        self.clone_when_busy.unwrap_or(false)
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes.unwrap_or(DEFAULT_WARM_MAX_BYTES)
    }

    /// The bound that ends a running check: the declared value, else twice `max_bytes`, at most
    /// [`MAX_WARM_MAX_BYTES`].
    pub fn hard_max_bytes(&self) -> u64 {
        self.hard_max_bytes
            .unwrap_or_else(|| self.max_bytes().saturating_mul(2).min(MAX_WARM_MAX_BYTES))
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
    /// How long a check waits for a held layer; `None`, the default, waits until the check's
    /// deadline (ADR-0201).
    pub(crate) lock_wait: Option<Duration>,
    /// How often a waiting check says whom it waits for.
    pub(crate) wait_notice: Duration,
    pub(crate) resolver: Option<Arc<CacheSourceResolver>>,
}

impl Default for WarmCheckHost {
    fn default() -> Self {
        Self {
            root: None,
            lock_wait: None,
            // A test makes the minute shorter so a wait it provokes says so in seconds.
            wait_notice: std::env::var("AF_TEST_WARM_WAIT_NOTICE_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map_or(WAIT_NOTICE, Duration::from_millis),
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

/// The fixed token a check's runtime path becomes in every hashed environment value, so a key
/// never depends on the per-check directory a value happens to name.
pub const RUNTIME_TOKEN: &str = "<af-check-runtime>";

/// What a toolchain key hashes besides the version probes and the declaration: the check's
/// runtime directory, replaced by [`RUNTIME_TOKEN`] wherever a hashed value names it, and the
/// verified content digest of a prepared native toolchain, which a private copy's path cannot
/// stand for.
#[derive(Debug, Clone, Copy, Default)]
pub struct ToolchainContext<'a> {
    pub runtime: Option<&'a Path>,
    pub native_digest: Option<&'a str>,
}

impl ToolchainContext<'_> {
    /// `value` with every occurrence of the runtime path, as given and as resolved, replaced
    /// by [`RUNTIME_TOKEN`]; the longer spelling first, so one is never half-replaced by the
    /// other.
    pub fn normalize(&self, value: &str) -> String {
        let Some(runtime) = self.runtime else {
            return value.to_string();
        };
        let mut spellings = vec![runtime.display().to_string()];
        if let Ok(resolved) = runtime.canonicalize() {
            spellings.push(resolved.display().to_string());
        }
        spellings.retain(|spelling| !spelling.is_empty());
        spellings.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        spellings.dedup();
        let mut value = value.to_string();
        for spelling in spellings {
            value = value.replace(&spelling, RUNTIME_TOKEN);
        }
        value
    }

    /// The identity a session memoizes one resolved key under: every environment value, with
    /// the runtime path replaced, and the native toolchain digest. Two checks whose environments
    /// differ only in their runtime directory share one entry; any other difference is another.
    fn memo(&self, environment: &[(String, String)]) -> String {
        let normalized: Vec<(&str, String)> = environment
            .iter()
            .map(|(name, value)| (name.as_str(), self.normalize(value)))
            .collect();
        serde_json::to_string(&(normalized, self.native_digest)).unwrap_or_default()
    }
}

/// Resolve the toolchain identity a check would build with. Both version commands run in the
/// check's working directory, under exactly the check's environment and a 30-second bound, so a
/// toolchain file, a `PATH` or a rustup home that selects another compiler selects another key.
/// The hashed environment values name the check's runtime directory only as [`RUNTIME_TOKEN`],
/// and a prepared native toolchain is keyed by its verified digest (domain
/// `af.task-build-cache.toolchain/2`), so no per-check or per-Attempt path changes the key.
pub fn toolchain_identity(
    workdir: &Path,
    environment: &[(String, String)],
    declaration: &ToolchainDeclaration,
    context: ToolchainContext<'_>,
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
        context
            .normalize(
                environment
                    .iter()
                    .rev()
                    .find(|(name, _)| name == key)
                    .map_or("", |(_, value)| value.as_str()),
            )
            .into_bytes()
    };
    Ok(key_digest(
        "af.task-build-cache.toolchain/2",
        &[
            declaration.path.as_deref().unwrap_or("none").as_bytes(),
            &declaration.bytes,
            &rustc,
            &cargo,
            host.as_bytes(),
            &fixed("PATH"),
            &fixed("LC_ALL"),
            &fixed("TZ"),
            &fixed("RUSTUP_HOME"),
            context.native_digest.unwrap_or("none").as_bytes(),
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
    let span = crate::telemetry::process(program, "toolchain_probe");
    let _probing = span.enter();
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
    crate::telemetry::process_capture(&span, &captured, None);
    match captured.status {
        Ok(status) if status.success() && !captured.stdout.is_empty() => Ok(captured.stdout),
        Ok(status) => Err(format!("{program} -vV exited with {status}")),
        Err(error) => Err(format!("{program} -vV did not complete: {error}")),
    }
}

pub(crate) fn key_digest(domain: &str, fields: &[&[u8]]) -> String {
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

/// Whether a check ran warm, for `af.check.warm`: `none` without a warm layer, `hit` when a
/// bound warm directory held bytes for it, `miss` otherwise.
pub(crate) fn warm_result(declared: bool, prepared: Option<&PreparedCheck>) -> &'static str {
    if !declared {
        return "none";
    }
    let hit = prepared.is_some_and(|prepared| {
        prepared.observations.iter().any(|observation| {
            observation.eligible
                && observation.discarded.is_none()
                && observation.bytes_available > 0
        })
    });
    if hit { "hit" } else { "miss" }
}

/// End an `af.warm.restore` span: the toolchain key, whether the check got a warm directory,
/// how many kinds it declared and the bytes available to it.
pub(crate) fn record_restore(span: &tracing::Span, prepared: &Result<PreparedCheck, String>) {
    match prepared {
        Ok(prepared) => crate::telemetry::warm_end(
            span,
            prepared
                .observations
                .iter()
                .find_map(|observation| observation.toolchain_id.as_deref()),
            warm_result(true, Some(prepared)) == "hit",
            prepared.observations.len(),
            prepared
                .observations
                .iter()
                .filter(|observation| observation.eligible)
                .map(|observation| observation.bytes_available)
                .fold(0_u64, u64::saturating_add),
            prepared.refusal.as_deref(),
        ),
        Err(reason) => crate::telemetry::warm_end(span, None, false, 0, 0, Some(reason)),
    }
}

/// End an `af.warm.save` span: what the check left in its warm directories, and the bytes the
/// bounds evicted after it.
pub(crate) fn record_save(
    span: &tracing::Span,
    observations: &[Observation],
    saved: &Result<Vec<(String, u64)>, String>,
) {
    let key = observations
        .iter()
        .find_map(|observation| observation.toolchain_id.as_deref());
    let hit = observations.iter().any(|observation| {
        observation.eligible && observation.discarded.is_none() && observation.bytes_available > 0
    });
    match saved {
        Ok(evicted) => crate::telemetry::warm_end(
            span,
            key,
            hit,
            observations.len(),
            evicted
                .iter()
                .map(|(_, bytes)| *bytes)
                .fold(0_u64, u64::saturating_add),
            None,
        ),
        Err(reason) => crate::telemetry::warm_end(span, key, hit, 0, 0, Some(reason)),
    }
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
    /// `bound_exceeded` or `suspect`, present exactly when `evicted_bytes` is.
    pub(crate) evicted_reason: Option<String>,
    /// Which byte bound acted on the directory, when one did (ADR-0135).
    pub(crate) bound: Option<TaskCacheBoundV1>,
    /// Why the directory was discarded and recreated empty before this check, when it was: the
    /// check then ran against an empty directory, which is warm again afterwards.
    pub(crate) discarded: Option<String>,
}

impl Observation {
    /// Give the observation its identity: the exact check and Attempt it belongs to.
    pub(crate) fn record(
        self,
        cas: &Cas,
        attempt: [&str; 4],
    ) -> Result<TaskCacheObservationV1, String> {
        let mut identity = json!([
            attempt,
            "warm_check",
            self.kind,
            self.eligible,
            self.source_digest,
            self.toolchain_id,
            self.bytes_available,
            self.lookup_ms,
            self.materialization_ms,
            self.evicted_bytes,
            self.evicted_reason
        ]);
        // The bound joins the identity only when one acted, so an observation no bound touched
        // keeps the identity ADR-0131 gave it.
        if let (Some(bound), Some(fields)) = (self.bound, identity.as_array_mut()) {
            fields.push(json!(bound.as_str()));
        }
        let observation_id = cas.put_json(&identity).map_err(|e| e.to_string())?;
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
            evicted_reason: self.evicted_reason,
            bound: self.bound,
        };
        observation.validate()?;
        Ok(observation)
    }
}

/// What a check learned of its layer while it was prepared (ADR-0201), for its
/// `af/TaskCheckLayer@1` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LayerFacts {
    /// `<project>/<toolchain>`, once the toolchain resolved.
    pub(crate) key: Option<String>,
    pub(crate) lock: TaskLayerLockV1,
    pub(crate) waited_ms: u64,
    /// The holder the check met, when it met one that named itself.
    pub(crate) holder: Option<TaskLayerHolderV1>,
    /// What the content rule did with the Snapshot's source groups.
    pub(crate) sources: TaskLayerSourcesV1,
}

impl LayerFacts {
    pub(crate) fn unresolved() -> Self {
        Self {
            key: None,
            lock: TaskLayerLockV1::Unresolved,
            waited_ms: 0,
            holder: None,
            sources: TaskLayerSourcesV1 { stale: 0, fresh: 0 },
        }
    }
}

/// The holder record beside a key, when it is one this release reads.
fn holder_of(root: &Path, project: &str, toolchain: &str) -> Option<WarmHolderV1> {
    read_key_holder(root, project, toolchain)
        .and_then(|bytes| serde_json::from_slice::<WarmHolderV1>(&bytes).ok())
        .filter(|holder| holder.validate().is_ok())
}

/// This process, as the holder record a waiting check reads.
fn this_holder() -> Vec<u8> {
    let holder = WarmHolderV1 {
        schema: WARM_HOLDER_V1.into(),
        pid: std::process::id(),
        host: review_process::host::host_name().unwrap_or_else(|| "unknown host".into()),
        since_unix_ms: unix_ms(),
        command: "af task run".into(),
    };
    serde_json::to_vec(&holder).unwrap_or_default()
}

pub(crate) fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The line a check waiting for a held layer prints every minute.
fn wait_notice(
    key: &str,
    holder: Option<&WarmHolderV1>,
    waited: Duration,
    left: Duration,
) -> String {
    let held_by = match holder {
        Some(holder) => format!(
            "{} {} since {} on {}",
            holder.command,
            holder.pid,
            utc_time(holder.since_unix_ms),
            holder.host
        ),
        None => "another check that left no holder record".into(),
    };
    format!(
        "waiting for warm layer {key} held by {held_by}; waited {} s, waiting up to {} s more \
         (the check deadline)",
        waited.as_secs(),
        left.as_secs()
    )
}

/// `2026-10-23 12:00:00 UTC`.
pub(crate) fn utc_time(unix_ms: u64) -> String {
    let seconds = unix_ms / 1000;
    let days = (seconds / 86_400) as i64;
    let of_day = seconds % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        of_day / 3_600,
        of_day / 60 % 60,
        of_day % 60
    )
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
    /// Private copies of a busy layer's directories, below the check's runtime directory, when
    /// `[warm] clone_when_busy` is on (ADR-0201).
    pub(crate) clones: Vec<(WarmBuildCacheKind, PathBuf)>,
    /// Exactly one per declared kind, build directories first, in declared order.
    pub(crate) observations: Vec<Observation>,
    /// A declared Cache Snapshot that could not be materialized: the check does not run, exactly
    /// as a Gate does not dispatch its command.
    pub(crate) refusal: Option<String>,
    pub(crate) layer: LayerFacts,
}

impl PreparedCheck {
    /// Order sources against the actual held target, not an assumed monotonic wall clock, and
    /// by content (ADR-0201): under the key's lock, a source group whose digest the key's
    /// sources record holds gets its recorded stamp back, so Cargo keeps what it built from that
    /// content; every other group is stamped now, newer than everything in the target, and is
    /// rebuilt. The record is removed before any file is stamped and written again before the
    /// check starts. If the target is future-dated, bind a cold private target for this check
    /// and keep the original locks and byte monitoring; Cargo home stays warm.
    pub(crate) fn refresh_source_mtimes(
        &mut self,
        sandbox: &review_sandbox::Sandbox,
        runtime: &Path,
        cancellation: Option<&AtomicBool>,
        budget: Duration,
    ) -> std::io::Result<usize> {
        let started = Instant::now();
        let target = self
            .directories
            .iter()
            .find(|(kind, _)| *kind == WarmBuildCacheKind::CargoTarget)
            .map(|(_, directory)| directory);
        let mut after = target
            .map(|directory| directory.latest_mtime(cancellation, budget))
            .transpose()?
            .flatten();
        let mut keyed = target.is_some();
        if target.is_none()
            && let Some((_, clone)) = self
                .clones
                .iter()
                .find(|(kind, _)| *kind == WarmBuildCacheKind::CargoTarget)
        {
            // A private copy keeps its artifacts' times: sources are ordered after them too.
            after = latest_mtime_below(clone, cancellation, budget)?;
        }
        if after.is_some_and(|latest| latest > std::time::SystemTime::now()) {
            let kind = WarmBuildCacheKind::CargoTarget;
            let target = self
                .environment
                .iter_mut()
                .find(|(name, _)| name == kind.variable())
                .ok_or_else(|| std::io::Error::other("held target has no environment binding"))?;
            target.1 = runtime.join("target").display().to_string();
            let observation = self
                .observations
                .iter_mut()
                .find(|observation| observation.kind == kind.as_str())
                .ok_or_else(|| std::io::Error::other("held target has no observation"))?;
            observation.kind = "cargo_target:future_mtime".into();
            observation.eligible = false;
            observation.bytes_available = 0;
            after = None;
            keyed = false;
        }
        let manifest = sandbox.baseline();
        let key = self.key_lock.as_ref().filter(|_| keyed);
        let plan = match key {
            Some(key) => {
                // Gone before any file is stamped: if anything below fails, the key has no
                // record, and the next check rebuilds every group instead of trusting one.
                let record = key.read_sources();
                key.forget_sources().map_err(std::io::Error::other)?;
                SourcePlan::decide(manifest, record.as_deref(), std::time::SystemTime::now())
            }
            None => SourcePlan::all_stale(manifest),
        };
        let index = PackageIndex::new(manifest);
        let refreshed = sandbox.refresh_source_mtimes_keyed(
            after,
            &|path| plan.kept_time(&index, path),
            cancellation,
            budget.saturating_sub(started.elapsed()),
        )?;
        if let Some(key) = key {
            match plan.record(refreshed.stamp) {
                Some(record) => {
                    if let Err(detail) = serde_json::to_vec(&record)
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| key.write_sources(&bytes))
                    {
                        eprintln!("warm check cache diagnostic: {detail}");
                    }
                }
                None => eprintln!(
                    "warm check cache diagnostic: the Snapshot has more source groups than a \
                     sources record holds; every group is rebuilt next time"
                ),
            }
        }
        self.layer.sources = plan.counts();
        Ok(refreshed.stamped + refreshed.kept)
    }

    /// Whether the layer held a build when the check started: a bound or copied directory with
    /// bytes.
    pub(crate) fn started_warm(&self) -> bool {
        self.observations.iter().any(|observation| {
            observation.discarded.is_none()
                && observation.bytes_available > 0
                && (observation.eligible || observation.kind.ends_with(":cloned"))
        })
    }
}

/// The newest modification time below `root`, links not followed; `None` when it is empty.
fn latest_mtime_below(
    root: &Path,
    cancellation: Option<&AtomicBool>,
    budget: Duration,
) -> std::io::Result<Option<std::time::SystemTime>> {
    let started = Instant::now();
    let mut latest = None;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "source freshness refresh cancelled",
            ));
        }
        if started.elapsed() >= budget {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "source freshness refresh exceeded check deadline",
            ));
        }
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = entry.path().symlink_metadata()?;
            let modified = metadata.modified()?;
            latest = Some(latest.map_or(modified, |old: std::time::SystemTime| old.max(modified)));
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(latest)
}

/// The warm layer of one check Attempt. The toolchain is resolved once per distinct hashed
/// environment: a second check with the same environment (its runtime directory aside) reuses
/// the first one's key, and a check with another environment resolves its own.
pub(crate) struct WarmSession<'a> {
    host: &'a WarmCheckHost,
    policy: &'a CodeWarmPolicy,
    project: String,
    declaration: ToolchainDeclaration,
    toolchains: BTreeMap<String, Result<String, String>>,
    /// The key the latest preparation resolved, which a check skipped before preparing names.
    latest: Option<String>,
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
            toolchains: BTreeMap::new(),
            latest: None,
        }
    }

    /// Lock, validate and then measure the build directories, and materialize any Cache
    /// Snapshot into `runtime`. `environment` is the check's own environment, used for the
    /// toolchain probe; `native_digest` is the verified digest of a native toolchain prepared for
    /// this check. `budget` is the check Attempt's remaining wall time: the toolchain probe and
    /// every lock wait are bounded by what is left of it, so preparation can never outlive the
    /// reservation it prepares for.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare(
        &mut self,
        cas: &Cas,
        environment: &[(String, String)],
        workdir: &Path,
        runtime: &Path,
        native_digest: Option<&str>,
        cancellation: Option<&AtomicBool>,
        budget: Duration,
    ) -> Result<PreparedCheck, String> {
        let preparing = Instant::now();
        let left = || budget.saturating_sub(preparing.elapsed());
        let mut prepared = PreparedCheck {
            environment: Vec::new(),
            key_lock: None,
            directories: Vec::new(),
            clones: Vec::new(),
            observations: Vec::new(),
            refusal: None,
            layer: LayerFacts::unresolved(),
        };
        let mut settled = BTreeMap::new();
        let mut kinds = self.policy.build_cache.clone();
        kinds.sort();
        // A kind a declared Cache Snapshot supersedes binds nothing, but it is still a kind
        // below the key: the key is locked, measured and, over its bound, emptied all the same,
        // and its observation carries the resolved toolchain like every other kind's.
        let reason_for = |kind: WarmBuildCacheKind, otherwise: &'static str| {
            if self.policy.supersedes(kind) {
                "superseded"
            } else {
                otherwise
            }
        };
        if !kinds.is_empty() {
            let started = Instant::now();
            let context = ToolchainContext {
                runtime: Some(runtime),
                native_digest,
            };
            let toolchain = self
                .toolchains
                .entry(context.memo(environment))
                .or_insert_with(|| {
                    toolchain_identity(
                        workdir,
                        environment,
                        &self.declaration,
                        context,
                        cancellation,
                        left().min(TOOLCHAIN_PROBE_TIMEOUT),
                    )
                })
                .clone();
            if let Ok(resolved) = &toolchain {
                self.latest = Some(resolved.clone());
            }
            match toolchain {
                Err(detail) => {
                    eprintln!("warm check cache diagnostic: {detail}");
                    for kind in &kinds {
                        settled.insert(
                            *kind,
                            self.ineligible(
                                cas,
                                kind.as_str(),
                                reason_for(*kind, "toolchain_unresolved"),
                                None,
                                0,
                                started,
                            )?,
                        );
                    }
                }
                Ok(toolchain) => {
                    // The whole toolchain key first: a check holds every kind's bound together,
                    // so two checks declaring different kinds cannot each fill the shared bound,
                    // and a kind another check left over the bound is evicted before this one
                    // binds anything.
                    let toolchain_hex = hex(&toolchain);
                    let root = self
                        .host
                        .root
                        .clone()
                        .map_or_else(default_task_build_cache_root, Ok);
                    let layer_key = format!("{}/{toolchain_hex}", self.project);
                    prepared.layer.key = Some(layer_key.clone());
                    prepared.layer.lock = TaskLayerLockV1::Unavailable;
                    // A held layer is waited for until the check's deadline, saying every minute
                    // whom it waits for; with `clone_when_busy` it is not waited for at all.
                    let clone = self.policy.clone_when_busy();
                    let waiting = Instant::now();
                    let mut met: Option<Option<WarmHolderV1>> = None;
                    let key = root.clone().and_then(|root| {
                        // A key that does not exist yet is new bytes on the machine: the
                        // Storage Budget evicts what it must before it is made (ADR-0144).
                        let path = root.join(&self.project).join(&toolchain_hex);
                        if std::fs::symlink_metadata(&path).is_err() {
                            crate::storage::before_new_warm_key();
                        }
                        let wait = if clone {
                            Duration::ZERO
                        } else {
                            self.host.lock_wait.unwrap_or(Duration::MAX).min(left())
                        };
                        let mut noticed = Duration::ZERO;
                        lock_task_build_cache_key_waiting(
                            &root,
                            &self.project,
                            &toolchain_hex,
                            wait,
                            &mut |waited| {
                                if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                                    return false;
                                }
                                let holder = met.get_or_insert_with(|| {
                                    holder_of(&root, &self.project, &toolchain_hex)
                                });
                                if waited >= noticed + self.host.wait_notice {
                                    noticed = waited;
                                    eprintln!(
                                        "{}",
                                        wait_notice(&layer_key, holder.as_ref(), waited, left())
                                    );
                                }
                                true
                            },
                        )
                    });
                    let waited_ms = waiting.elapsed().as_millis() as u64;
                    prepared.layer.waited_ms = waited_ms;
                    match key {
                        Ok(None) => {
                            let holder = met.flatten().or_else(|| {
                                root.as_ref()
                                    .ok()
                                    .and_then(|root| holder_of(root, &self.project, &toolchain_hex))
                            });
                            prepared.layer.holder = holder.map(|holder| TaskLayerHolderV1 {
                                pid: holder.pid,
                                host: holder.host,
                                since_unix_ms: holder.since_unix_ms,
                            });
                            prepared.layer.lock = TaskLayerLockV1::Busy;
                            for kind in &kinds {
                                let cloned = match (&root, clone && !self.policy.supersedes(*kind))
                                {
                                    (Ok(root), true) => {
                                        let destination =
                                            runtime.join(format!("{}-clone", kind.as_str()));
                                        let copying = Instant::now();
                                        match clone_warm_kind(
                                            root,
                                            &self.project,
                                            &toolchain_hex,
                                            kind.as_str(),
                                            &destination,
                                            &|| {
                                                !left().is_zero()
                                                    && !cancellation.is_some_and(|flag| {
                                                        flag.load(Ordering::Acquire)
                                                    })
                                            },
                                        ) {
                                            Ok(bytes) => Some((destination, bytes, copying)),
                                            Err(detail) => {
                                                eprintln!("warm check cache diagnostic: {detail}");
                                                None
                                            }
                                        }
                                    }
                                    _ => None,
                                };
                                let observation = match cloned {
                                    Some((destination, bytes, copying)) => {
                                        // A private copy: warm with what the holder had built,
                                        // without its sources record, so every source group is
                                        // stamped now and the copy is never the layer itself.
                                        prepared.layer.lock = TaskLayerLockV1::Cloned;
                                        prepared.clones.push((*kind, destination));
                                        let mut observation = self.ineligible(
                                            cas,
                                            kind.as_str(),
                                            "cloned",
                                            Some(&toolchain),
                                            bytes,
                                            started,
                                        )?;
                                        observation.materialization_ms =
                                            copying.elapsed().as_millis() as u64;
                                        observation
                                    }
                                    None => self.ineligible(
                                        cas,
                                        kind.as_str(),
                                        reason_for(*kind, "busy"),
                                        Some(&toolchain),
                                        0,
                                        started,
                                    )?,
                                };
                                settled.insert(*kind, observation);
                            }
                        }
                        Err(detail) => {
                            eprintln!("warm check cache diagnostic: {detail}");
                            for kind in &kinds {
                                settled.insert(
                                    *kind,
                                    self.ineligible(
                                        cas,
                                        kind.as_str(),
                                        reason_for(*kind, "unavailable"),
                                        Some(&toolchain),
                                        0,
                                        started,
                                    )?,
                                );
                            }
                        }
                        Ok(Some(key)) => {
                            prepared.layer.lock = if met.is_some() {
                                TaskLayerLockV1::Waited
                            } else {
                                TaskLayerLockV1::Held
                            };
                            prepared.layer.holder = met.flatten().map(|holder| TaskLayerHolderV1 {
                                pid: holder.pid,
                                host: holder.host,
                                since_unix_ms: holder.since_unix_ms,
                            });
                            if let Err(detail) = key.announce(&this_holder()) {
                                eprintln!("warm check cache diagnostic: {detail}");
                            }
                            let over = match key.bytes() {
                                Ok(bytes) => bytes > self.policy.max_bytes(),
                                Err(uninspectable) => {
                                    eprintln!("warm check cache diagnostic: {uninspectable}");
                                    true
                                }
                            };
                            let removed = if over {
                                // Over the bound, or uninspectable, before the check: every
                                // entry below the key is removed, never trimmed, and this check
                                // runs cold in its private runtime directory.
                                Some(key.remove_kinds()?)
                            } else {
                                None
                            };
                            for kind in &kinds {
                                let label = kind.as_str();
                                if self.policy.supersedes(*kind) {
                                    settled.insert(
                                        *kind,
                                        self.ineligible(
                                            cas,
                                            label,
                                            "superseded",
                                            Some(&toolchain),
                                            0,
                                            started,
                                        )?,
                                    );
                                    continue;
                                }
                                if let Some(removed) = &removed {
                                    let bytes = removed
                                        .iter()
                                        .find(|(name, _)| name == label)
                                        .map_or(0, |(_, bytes)| *bytes);
                                    settled.insert(
                                        *kind,
                                        self.ineligible(
                                            cas,
                                            label,
                                            "bound_exceeded",
                                            Some(&toolchain),
                                            bytes,
                                            started,
                                        )?,
                                    );
                                    continue;
                                }
                                let started = Instant::now();
                                let directory = match key.lock_kind(
                                    label,
                                    self.host.lock_wait.unwrap_or(Duration::MAX).min(left()),
                                ) {
                                    Ok(TaskBuildCacheLock::Held(directory)) => {
                                        directory.forbidding(kind.forbidden())
                                    }
                                    Ok(TaskBuildCacheLock::Busy) => {
                                        settled.insert(
                                            *kind,
                                            self.ineligible(
                                                cas,
                                                label,
                                                "busy",
                                                Some(&toolchain),
                                                0,
                                                started,
                                            )?,
                                        );
                                        continue;
                                    }
                                    Err(detail) => {
                                        eprintln!("warm check cache diagnostic: {detail}");
                                        settled.insert(
                                            *kind,
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
                                };
                                let lookup_ms = started.elapsed().as_millis() as u64;
                                let materializing = Instant::now();
                                let discarded = match directory.ensure() {
                                    Ok(Ensured::Discarded(reason)) => {
                                        // Removed and recreated empty: whatever it held is gone,
                                        // so the measurement below finds a cold directory, never
                                        // the discarded one's bytes; the observation keeps why.
                                        eprintln!(
                                            "warm check cache diagnostic: `{label}` discarded: {reason}"
                                        );
                                        Some(reason)
                                    }
                                    Ok(Ensured::Reused | Ensured::Created) => None,
                                    Err(detail) => {
                                        eprintln!("warm check cache diagnostic: {detail}");
                                        settled.insert(
                                            *kind,
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
                                };
                                // Measured only after validation.
                                let bytes = match directory.bytes() {
                                    Ok(bytes) => bytes,
                                    Err(uninspectable) => {
                                        eprintln!("warm check cache diagnostic: {uninspectable}");
                                        directory.remove()?;
                                        settled.insert(
                                            *kind,
                                            self.ineligible(
                                                cas,
                                                label,
                                                "bound_exceeded",
                                                Some(&toolchain),
                                                uninspectable.counted,
                                                started,
                                            )?,
                                        );
                                        continue;
                                    }
                                };
                                settled.insert(
                                    *kind,
                                    Observation {
                                        kind: label.into(),
                                        eligible: true,
                                        source_digest: self.identity(
                                            cas,
                                            label,
                                            Some(&toolchain),
                                        )?,
                                        toolchain_id: Some(toolchain.clone()),
                                        bytes_available: bytes,
                                        lookup_ms,
                                        materialization_ms: materializing.elapsed().as_millis()
                                            as u64,
                                        evicted_bytes: None,
                                        evicted_reason: None,
                                        bound: None,
                                        discarded,
                                    },
                                );
                                prepared.directories.push((*kind, directory));
                            }
                            prepared.key_lock = Some(key);
                        }
                    }
                }
            }
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
                .or_else(|| {
                    prepared
                        .clones
                        .iter()
                        .find(|(cloned, _)| *cloned == kind)
                        .map(|(_, path)| path.display().to_string())
                })
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
        if self
            .policy
            .build_cache
            .contains(&WarmBuildCacheKind::NpmCache)
        {
            // A layer the check does not hold leaves it a private, empty npm cache: an offline
            // check then fills it once, while the network is still allowed.
            prepared.environment.push((
                WarmBuildCacheKind::NpmCache.variable().into(),
                bound(WarmBuildCacheKind::NpmCache)
                    .unwrap_or_else(|| runtime.join("npm-cache").display().to_string()),
            ));
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
                        evicted_reason: None,
                        bound: None,
                        discarded: None,
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
            prepared.clones.clear();
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
        let toolchain = self.latest.clone();
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
                    )?,
                    ..observation
                })
            })
            .collect()
    }

    /// Run one check while a monitor watches it (ADR-0131, ADR-0201).
    ///
    /// When the check holds its key, the monitor samples the key's bytes: above `hard_max_bytes`
    /// it ends the check's process group through the runner's supervised cancellation. However
    /// fast the check was, the directories are measured again once it has ended and before its
    /// result is accepted, so a check that wrote past the hard bound between two samples is
    /// caught too. A directory the count cannot fully inspect is above every bound. The byte
    /// count that crossed the hard bound is returned as [`Excess::Bound`] so the caller fails
    /// the check and removes the directories; a key that ended above `max_bytes` only is
    /// [`Excess::Evict`]: removed before the next check, while this check's result stands. A
    /// key that left its name while the check held it was evicted under it: the monitor names
    /// the phase, the cause and the time.
    ///
    /// Every warm check has two phases: the build, until it reports a `build` step through
    /// `report` (`$AF_CHECK_REPORT`), bounded by `deadlines.build`; then the tests, bounded by
    /// `deadlines.tests` from the build's end. A check that has reported steps but no build step
    /// once its build deadline passed is still building and is ended there; one that reports no
    /// step at all is bounded only by the runner's timeout, both deadlines together.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_monitored(
        &self,
        runner: CheckRunner<'_>,
        definition: &CheckDefinition,
        held: Option<(
            &TaskBuildCacheKeyLock,
            &[(WarmBuildCacheKind, WarmDirectory)],
        )>,
        report: &Path,
        deadlines: PhaseDeadlines,
        cancellation: Option<&AtomicBool>,
    ) -> (CheckExecution, Option<Excess>, PhaseRecord) {
        let stop = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        let exceeded: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);
        let phases = std::sync::Mutex::new(PhaseRecord::default());
        let max = self.policy.max_bytes();
        let hard = self.policy.hard_max_bytes();
        let start_unix_ms = unix_ms();
        let execution = std::thread::scope(|scope| {
            scope.spawn(|| {
                let began = Instant::now();
                let mut sampled = Instant::now();
                let mut polled: Option<Instant> = None;
                let mut reported_any = false;
                while !done.load(Ordering::Acquire) {
                    if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                        stop.store(true, Ordering::Release);
                    }
                    let elapsed = began.elapsed();
                    let mut record = phases.lock().expect("warm phase monitor");
                    if record.build_end.is_none()
                        && polled.is_none_or(|polled| polled.elapsed() >= PHASE_POLL)
                    {
                        polled = Some(Instant::now());
                        let (any, build) = reported_steps(report);
                        reported_any |= any;
                        if build {
                            record.build_end = Some(elapsed);
                        }
                    }
                    let ended = match (record.build_end, deadlines.build) {
                        (None, Some(build)) if reported_any && elapsed >= build => {
                            Some(TaskLayerPhaseV1::Build)
                        }
                        (Some(end), Some(_)) if elapsed >= end + deadlines.tests => {
                            Some(TaskLayerPhaseV1::Tests)
                        }
                        _ => None,
                    };
                    if let Some(phase) = ended {
                        record.deadline = Some(phase);
                        stop.store(true, Ordering::Release);
                        return;
                    }
                    drop(record);
                    if let Some((key, directories)) = held
                        && sampled.elapsed() >= SAMPLE_INTERVAL
                    {
                        sampled = Instant::now();
                        let mut record = phases.lock().expect("warm phase monitor");
                        if record.evicted.is_none() && !key.at_its_name() {
                            record.evicted =
                                Some(self.eviction_cause(key, record.phase(), start_unix_ms));
                        }
                        drop(record);
                        if let Some(bytes) = over_bound(key, directories, hard) {
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
        let mut record = phases.into_inner().expect("warm phase monitor");
        record.elapsed = Duration::from_millis(execution.elapsed_ms);
        if record.build_end.is_none() && reported_steps(report).1 {
            // A build step reported after the last poll still ends the build phase, at the end.
            record.build_end = Some(record.elapsed);
        }
        let Some((key, directories)) = held else {
            return (execution, None, record);
        };
        if record.evicted.is_none() && !key.at_its_name() {
            record.evicted = Some(self.eviction_cause(key, record.phase(), start_unix_ms));
        }
        // An offline check's npm reinstalls its pinned packages from the cache into `_npx`,
        // with links in it; the kernel removes that install tree before the cache is judged.
        for (kind, directory) in directories {
            if *kind == WarmBuildCacheKind::NpmCache
                && let Err(detail) = directory.remove_child("_npx")
            {
                eprintln!("warm check cache diagnostic: {detail}");
            }
        }
        let sampled = *exceeded.lock().expect("warm bound monitor");
        // Once the check ended, however fast: the whole key is measured again, and every
        // directory the check used is judged as `ensure` would judge it before the next check
        // — a credential written into a Cargo home, a link, a swapped root, all make it suspect.
        // Suspicion first: a replaced, linked or grown lock, a displaced or widened key or a
        // suspect directory is the cause even when the count is over the bound as well, so the
        // durable evidence names what happened rather than how many bytes it took.
        let excess = key
            .inspect()
            .map(|reason| Excess::Suspect(format!("toolchain key: {reason}")))
            .or_else(|| {
                directories.iter().find_map(|(kind, directory)| {
                    directory
                        .inspect()
                        .map(|reason| Excess::Suspect(format!("{}: {reason}", kind.as_str())))
                })
            })
            .or_else(|| {
                if let Some(bytes) = sampled {
                    return Some(Excess::Bound(bytes));
                }
                let (bytes, uninspectable) = measure(key, directories);
                if uninspectable || bytes > hard {
                    Some(Excess::Bound(bytes))
                } else if bytes > max {
                    Some(Excess::Evict(bytes))
                } else {
                    None
                }
            });
        (execution, excess, record)
    }

    /// Who evicted a key that left its name while a check held it: the Storage Budget's sweep
    /// when its eviction record is from after the check started, anything else otherwise.
    fn eviction_cause(
        &self,
        key: &TaskBuildCacheKeyLock,
        phase: TaskLayerPhaseV1,
        start_unix_ms: u64,
    ) -> TaskLayerEvictionV1 {
        let sweep = key
            .path()
            .parent()
            .and_then(Path::parent)
            .zip(key.path().file_name().and_then(|name| name.to_str()))
            .and_then(|(root, toolchain)| read_key_eviction(root, &self.project, toolchain))
            .and_then(|bytes| serde_json::from_slice::<WarmEvictionV1>(&bytes).ok())
            .filter(|eviction| eviction.validate().is_ok() && eviction.at_unix_ms >= start_unix_ms);
        match sweep {
            Some(eviction) => TaskLayerEvictionV1 {
                phase,
                by: TaskLayerEvictorV1::StorageSweep,
                at_unix_ms: eviction.at_unix_ms,
            },
            None => TaskLayerEvictionV1 {
                phase,
                by: TaskLayerEvictorV1::AnotherProcess,
                at_unix_ms: unix_ms(),
            },
        }
    }

    /// After the check: when [`Self::run_monitored`] found the key above either bound or a
    /// used directory suspect, every kind directory below the key — held by this check or left
    /// by another — is removed while the key lock is still held, so the next check runs cold.
    /// Each kind's removed bytes are returned for the caller to record on that kind's one
    /// observation as `evicted_bytes`; an empty list means nothing was removed. The key lock
    /// and the kind locks are released when this returns.
    pub(crate) fn finish(
        &self,
        key: Option<TaskBuildCacheKeyLock>,
        directories: Vec<(WarmBuildCacheKind, WarmDirectory)>,
        excess: Option<Excess>,
    ) -> Result<Vec<(String, u64)>, String> {
        let Some(key) = key else {
            return Ok(Vec::new());
        };
        if excess.is_none() {
            // The key's allocated bytes, recorded while it is still locked, so the Storage
            // Budget's sweep need not walk an unchanged target again (ADR-0144).
            match review_sandbox::storage::allocated_bytes(key.path()) {
                Ok(bytes) => {
                    if let Err(detail) = key.record_size(bytes) {
                        eprintln!("warm check cache diagnostic: {detail}");
                    }
                }
                Err(error) => eprintln!("warm check cache diagnostic: measuring the key: {error}"),
            }
            return Ok(Vec::new());
        }
        let removed = key.remove_kinds()?;
        drop(directories);
        drop(key);
        Ok(removed)
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
            source_digest: self.identity(cas, kind, toolchain_id)?,
            toolchain_id: toolchain_id.map(String::from),
            bytes_available: bytes,
            lookup_ms: started.elapsed().as_millis() as u64,
            materialization_ms: 0,
            evicted_bytes: None,
            evicted_reason: None,
            // Before a check only the eviction bound acts: a key above `max_bytes`, or one the
            // count cannot fully inspect, is removed and the check runs cold.
            bound: (reason == "bound_exceeded").then_some(TaskCacheBoundV1::MaxBytes),
            discarded: None,
        })
    }

    /// The path-free identity of the directory an observation is about: the project key, the
    /// toolchain and the kind, never a host path and never the lookup's outcome, so the
    /// observations of one directory join on it whether the check found it warm, busy or gone.
    fn identity(
        &self,
        cas: &Cas,
        kind: &str,
        toolchain_id: Option<&str>,
    ) -> Result<String, String> {
        cas.put_json(&json!({
            "schema": "af.task-build-cache/1",
            "project": self.project,
            "toolchain_id": toolchain_id,
            "kind": kind,
        }))
        .map_err(|e| e.to_string())
    }
}

/// The deadlines of a warm check's two phases (ADR-0201).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhaseDeadlines {
    /// `[warm] build_wall_ms`, from the check's start; absent, no phase has a deadline of its
    /// own and the runner's timeout, `check_wall_ms`, bounds the whole check.
    pub(crate) build: Option<Duration>,
    /// `check_wall_ms` (or the per-check wall), from the build's end.
    pub(crate) tests: Duration,
}

/// What the monitor saw of a warm check's phases and its layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PhaseRecord {
    /// When the check reported its `build` step, from its start.
    pub(crate) build_end: Option<Duration>,
    /// The phase whose deadline ended the check.
    pub(crate) deadline: Option<TaskLayerPhaseV1>,
    /// The layer, evicted under the check while it ran.
    pub(crate) evicted: Option<TaskLayerEvictionV1>,
    pub(crate) elapsed: Duration,
}

impl PhaseRecord {
    fn phase(&self) -> TaskLayerPhaseV1 {
        if self.build_end.is_some() {
            TaskLayerPhaseV1::Tests
        } else {
            TaskLayerPhaseV1::Build
        }
    }

    /// The reason a check whose phase deadline ended it records.
    pub(crate) fn deadline_reason(&self, deadlines: PhaseDeadlines) -> Option<String> {
        match self.deadline? {
            TaskLayerPhaseV1::Build => Some(format!(
                "the build phase ran past [warm] build_wall_ms ({} ms) before the check \
                 reported its build step; the check was ended",
                deadlines.build.unwrap_or_default().as_millis()
            )),
            TaskLayerPhaseV1::Tests => Some(format!(
                "the tests ran past check_wall_ms ({} ms) after the build step ended at {} ms; \
                 the check was ended",
                deadlines.tests.as_millis(),
                self.build_end.unwrap_or_default().as_millis()
            )),
            TaskLayerPhaseV1::After => None,
        }
    }
}

/// Whether the check's report names any step yet, and whether one of them is a `build` step.
/// A report that does not parse yet (a line still being written) names nothing.
fn reported_steps(report: &Path) -> (bool, bool) {
    use review_core::task::check_report::{
        CheckStageV1, MAX_CHECK_REPORT_BYTES, parse_check_report,
    };
    let Ok(metadata) = std::fs::symlink_metadata(report) else {
        return (false, false);
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_CHECK_REPORT_BYTES {
        return (false, false);
    }
    let Ok(bytes) = std::fs::read(report) else {
        return (false, false);
    };
    // Only whole lines: ci-step.py appends one line per step.
    let whole = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&bytes[..0], |end| &bytes[..=end]);
    match parse_check_report(whole) {
        Ok((steps, _)) => (
            !steps.is_empty(),
            steps.iter().any(|step| step.stage == CheckStageV1::Build),
        ),
        Err(_) => (false, false),
    }
}

/// The directories' combined bytes when they are above `max`, or when any of them cannot be
/// fully counted: what the kernel cannot see it cannot bound.
fn over_bound(
    key: &TaskBuildCacheKeyLock,
    directories: &[(WarmBuildCacheKind, WarmDirectory)],
    max: u64,
) -> Option<u64> {
    let (total, uninspectable) = measure(key, directories);
    (uninspectable || total > max).then_some(total)
}

/// The toolchain key's bytes and whether it could not be fully counted. The whole key directory
/// is counted — every kind under it, held by this check or not — because the bounds are shared
/// by the key, and each held root must still be a real directory: a root swapped for a link or a
/// file is uninspectable, and an uninspectable key is above every bound.
fn measure(
    key: &TaskBuildCacheKeyLock,
    directories: &[(WarmBuildCacheKind, WarmDirectory)],
) -> (u64, bool) {
    let mut uninspectable = directories
        .iter()
        .any(|(_, directory)| directory.is_swapped());
    let total = match key.bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            uninspectable = true;
            error.counted
        }
    };
    (total, uninspectable)
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
            ToolchainContext::default(),
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
                ToolchainContext::default(),
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
                ToolchainContext::default(),
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
                ToolchainContext::default(),
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
                ToolchainContext::default(),
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
                ToolchainContext::default(),
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
                ToolchainContext::default(),
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
                ToolchainContext::default(),
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
                ToolchainContext::default(),
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
        assert_eq!(
            bound.hard_max_bytes(),
            MAX_WARM_MAX_BYTES,
            "twice the bound, at most the hard maximum"
        );
    }

    #[test]
    fn the_hard_bound_defaults_to_twice_the_eviction_bound_and_never_below_it() {
        let default: CodeWarmPolicy = toml::from_str("build_cache = [\"cargo_target\"]").unwrap();
        default.validate().unwrap();
        assert_eq!(default.hard_max_bytes(), 2 * DEFAULT_WARM_MAX_BYTES);
        let small: CodeWarmPolicy =
            toml::from_str("build_cache = [\"cargo_target\"]\nmax_bytes = 4096").unwrap();
        small.validate().unwrap();
        assert_eq!(small.hard_max_bytes(), 8192);
        let declared: CodeWarmPolicy = toml::from_str(
            "build_cache = [\"cargo_target\"]\nmax_bytes = 4096\nhard_max_bytes = 4096",
        )
        .unwrap();
        declared.validate().unwrap();
        assert_eq!(declared.hard_max_bytes(), 4096);
        assert_eq!(
            serde_json::to_value(&declared).unwrap(),
            json!({"build_cache": ["cargo_target"], "max_bytes": 4096, "hard_max_bytes": 4096}),
            "captured as declared"
        );
        for refused in [
            "build_cache = [\"cargo_target\"]\nmax_bytes = 4096\nhard_max_bytes = 4095",
            "build_cache = [\"cargo_target\"]\nhard_max_bytes = 4096",
            "build_cache = [\"cargo_target\"]\nhard_max_bytes = 34359738369",
            "build_cache = [\"cargo_target\"]\nmax_bytes = 1\nhard_max_bytes = 0",
        ] {
            let policy: CodeWarmPolicy = toml::from_str(refused).unwrap();
            let error = policy.validate().unwrap_err();
            assert!(error.contains("hard_max_bytes"), "{refused}: {error}");
        }
    }

    struct Fixture {
        _bin: tempfile::TempDir,
        root: tempfile::TempDir,
        workdir: tempfile::TempDir,
        runtime: tempfile::TempDir,
        cas: Cas,
        environment: Vec<(String, String)>,
    }

    /// Deadlines no unit test reaches.
    const NO_PHASES: PhaseDeadlines = PhaseDeadlines {
        build: None,
        tests: Duration::from_secs(3_600),
    };

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
            lock_wait: Some(Duration::from_millis(200)),
            wait_notice: Duration::from_millis(100),
            resolver,
        }
    }

    fn warm(max_bytes: Option<u64>) -> CodeWarmPolicy {
        CodeWarmPolicy {
            build_cache: vec![WarmBuildCacheKind::CargoTarget],
            caches: vec![],
            max_bytes,
            // One bound for both, as ADR-0131 had it; the split has its own tests.
            hard_max_bytes: max_bytes,
            clone_when_busy: None,
            build_wall_ms: None,
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
            busy.observations[0].source_digest, held.observations[0].source_digest,
            "one directory has one identity whether the check found it warm or busy"
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
            session
                .finish(key_lock, vec![directory], None)
                .unwrap()
                .is_empty(),
            "under the bound the directory is kept"
        );

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
        let (execution, exceeded, _) = session.run_monitored(
            runner,
            &check,
            Some((
                prepared
                    .key_lock
                    .as_ref()
                    .expect("a prepared check holds its key"),
                &prepared.directories,
            )),
            &fixture.runtime.path().join("check-report.jsonl"),
            NO_PHASES,
            None,
        );
        assert!(started.elapsed() < Duration::from_secs(50), "ended early");
        assert_eq!(exceeded, Some(Excess::Bound(8192)));
        assert_ne!(execution.result.status, review_check::CheckStatus::Passed);
        let removed = session
            .finish(prepared.key_lock, prepared.directories, exceeded)
            .unwrap();
        assert_eq!(
            removed,
            [("cargo_target".to_string(), 8192)],
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
            hard_max_bytes: None,
            clone_when_busy: None,
            build_wall_ms: None,
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
        run_with(fixture, session, prepared, script, &[])
    }

    fn run_with(
        fixture: &Fixture,
        session: &WarmSession<'_>,
        prepared: &PreparedCheck,
        script: &str,
        extra: &[(&str, String)],
    ) -> (CheckExecution, Option<Excess>) {
        let runner = prepared.environment.iter().fold(
            CheckRunner::new(&fixture.cas, fixture.workdir.path())
                .with_timeout(Duration::from_secs(120)),
            |runner, (key, value)| runner.with_env(key.clone(), value.clone()),
        );
        let runner = extra.iter().fold(runner, |runner, (key, value)| {
            runner.with_env(*key, value.clone())
        });
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
        let (execution, excess, _) = session.run_monitored(
            runner,
            &check,
            Some((
                prepared
                    .key_lock
                    .as_ref()
                    .expect("a prepared check holds its key"),
                &prepared.directories,
            )),
            &fixture.runtime.path().join("check-report.jsonl"),
            NO_PHASES,
            None,
        );
        (execution, excess)
    }

    #[test]
    fn a_check_above_only_the_eviction_bound_stands_and_its_directory_is_evicted() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = CodeWarmPolicy {
            hard_max_bytes: Some(65_536),
            clone_when_busy: None,
            build_wall_ms: None,
            ..warm(Some(4096))
        };
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let path = prepared.directories[0].1.path().to_path_buf();
        let (execution, exceeded) = run(
            &fixture,
            &session,
            &prepared,
            "head -c 8192 /dev/zero > \"$CARGO_TARGET_DIR/big\"; sleep 6",
        );
        assert_eq!(
            execution.result.status,
            review_check::CheckStatus::Passed,
            "the monitor sampled 8192 bytes, under the hard bound, and let it run"
        );
        assert_eq!(exceeded, Some(Excess::Evict(8192)));
        assert_eq!(
            session
                .finish(prepared.key_lock, prepared.directories, exceeded)
                .unwrap(),
            [("cargo_target".to_string(), 8192)],
            "evicted after the check, under the lock"
        );
        assert!(!path.exists(), "gone before the next check");
        let next = prepare(&fixture, &mut session);
        assert_eq!(next.observations[0].kind, "cargo_target");
        assert_eq!(
            next.observations[0].bytes_available, 0,
            "the next check is cold"
        );
        assert_eq!(next.observations[0].bound, None);
    }

    #[test]
    fn only_the_hard_bound_ends_a_running_check() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = CodeWarmPolicy {
            hard_max_bytes: Some(16_384),
            clone_when_busy: None,
            build_wall_ms: None,
            ..warm(Some(4096))
        };
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let started = Instant::now();
        let (execution, exceeded) = run(
            &fixture,
            &session,
            &prepared,
            "head -c 32768 /dev/zero > \"$CARGO_TARGET_DIR/big\"; sleep 60",
        );
        assert!(started.elapsed() < Duration::from_secs(50), "ended early");
        assert_eq!(exceeded, Some(Excess::Bound(32_768)));
        assert_ne!(execution.result.status, review_check::CheckStatus::Passed);
        session
            .finish(prepared.key_lock, prepared.directories, exceeded)
            .unwrap();
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
            session
                .finish(prepared.key_lock, prepared.directories, exceeded)
                .unwrap(),
            [("cargo_target".to_string(), 8192)]
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
        let evicted = session
            .finish(prepared.key_lock, prepared.directories, exceeded)
            .unwrap();
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
            let PreparedCheck {
                directories,
                key_lock,
                ..
            } = prepared;
            let (kind, directory) = directories.into_iter().next().unwrap();
            std::fs::write(directory.path().join("build.bin"), [1_u8; 512]).unwrap();
            let path = directory.path().to_path_buf();
            assert!(
                session
                    .finish(key_lock, vec![(kind, directory)], None)
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
            // One bound for both, as ADR-0131 had it; the split has its own tests.
            hard_max_bytes: max_bytes,
            clone_when_busy: None,
            build_wall_ms: None,
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
                .finish(prepared.key_lock, prepared.directories, None)
                .unwrap()
                .is_empty()
        );

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
                ToolchainContext::default(),
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
            // One bound for both, as ADR-0131 had it; the split has its own tests.
            hard_max_bytes: max_bytes,
            clone_when_busy: None,
            build_wall_ms: None,
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
        let evicted = session
            .finish(prepared.key_lock, prepared.directories, excess)
            .unwrap();
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
            matches!(&excess, Some(Excess::Suspect(reason)) if reason.contains("link")),
            "a root swapped for a link is suspect before it is a byte count: {excess:?}"
        );
        session
            .finish(prepared.key_lock, prepared.directories, excess)
            .unwrap();
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

    #[test]
    fn a_kind_another_policy_left_over_the_bound_is_evicted_before_the_next_check_binds() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let home_only = only_home(None);
        let mut first = WarmSession::new(&host, &home_only, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut first);
        let home = prepared.directories[0].1.path().to_path_buf();
        std::fs::write(home.join("registry"), [0_u8; 4096]).unwrap();
        assert!(
            first
                .finish(prepared.key_lock, prepared.directories, None)
                .unwrap()
                .is_empty()
        );

        let tight_target = warm(Some(1024));
        let mut second = WarmSession::new(&host, &tight_target, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut second);
        assert!(
            prepared.directories.is_empty(),
            "the key was over its shared bound before this check bound anything"
        );
        assert_eq!(prepared.observations[0].kind, "cargo_target:bound_exceeded");
        assert!(!home.exists(), "the other policy's kind went with the key");
        assert!(prepared.key_lock.is_some());
    }

    #[test]
    fn a_superseded_only_policy_still_locks_the_key_and_evicts_an_over_bound_one() {
        let fixture = fixture();
        let plain = host(&fixture, None);
        let home_only = only_home(None);
        let mut first = WarmSession::new(&plain, &home_only, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut first);
        let home = prepared.directories[0].1.path().to_path_buf();
        std::fs::write(home.join("registry"), [0_u8; 4096]).unwrap();
        assert!(
            first
                .finish(prepared.key_lock, prepared.directories, None)
                .unwrap()
                .is_empty()
        );

        let with_cache = host(&fixture, Some(cargo_cache(&fixture)));
        let superseded = CodeWarmPolicy {
            build_cache: vec![WarmBuildCacheKind::CargoHome],
            caches: vec![CacheKindSpec::Cargo],
            max_bytes: Some(1024),
            hard_max_bytes: Some(1024),
            clone_when_busy: None,
            build_wall_ms: None,
        };
        let mut second = WarmSession::new(&with_cache, &superseded, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut second);
        assert!(
            prepared.key_lock.is_some(),
            "the key is locked although nothing is bound"
        );
        assert!(prepared.directories.is_empty());
        assert_eq!(prepared.observations[0].kind, "cargo_home:superseded");
        assert!(
            prepared.observations[0].toolchain_id.is_some(),
            "a superseded kind still carries the resolved toolchain"
        );
        assert!(
            !home.exists(),
            "the over-bound key was emptied under its lock"
        );
    }

    #[test]
    fn a_check_that_removes_its_own_warm_root_is_suspect_with_nothing_left_to_evict() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(None);
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let (execution, excess) = run(
            &fixture,
            &session,
            &prepared,
            "rm -rf \"$CARGO_TARGET_DIR\"",
        );
        assert_eq!(execution.result.status, review_check::CheckStatus::Passed);
        assert!(
            matches!(&excess, Some(Excess::Suspect(reason)) if reason.contains("gone")),
            "{excess:?}"
        );
        let evicted = session
            .finish(prepared.key_lock, prepared.directories, excess)
            .unwrap();
        assert!(
            evicted.is_empty(),
            "nothing was left to remove, and the check still fails"
        );
    }

    #[test]
    fn a_check_that_replaces_the_held_key_lock_with_an_oversized_file_fails_and_loses_it() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(Some(4096));
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let key_path = prepared.key_lock.as_ref().unwrap().path().to_path_buf();
        let (_, excess) = run(
            &fixture,
            &session,
            &prepared,
            "k=\"$(dirname \"$CARGO_TARGET_DIR\")\"; rm \"$k/warm.lock\"; head -c 8192 /dev/zero > \"$k/warm.lock\"",
        );
        assert!(
            matches!(&excess, Some(Excess::Suspect(reason)) if reason.contains("lock")),
            "a replaced lock is suspect before it is a byte count: {excess:?}"
        );
        session
            .finish(prepared.key_lock, prepared.directories, excess)
            .unwrap();
        assert!(
            !key_path.join("warm.lock").exists(),
            "the impostor went with the key; the kernel's inode was already unlinked"
        );
    }

    #[test]
    fn a_check_that_widens_the_toolchain_key_is_suspect_after_it_ends() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(None);
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        let (_, excess) = run(
            &fixture,
            &session,
            &prepared,
            "chmod 0777 \"$(dirname \"$CARGO_TARGET_DIR\")\"",
        );
        assert!(
            matches!(&excess, Some(Excess::Suspect(reason)) if reason.contains("mode")),
            "{excess:?}"
        );
        let path = prepared.directories[0].1.path().to_path_buf();
        session
            .finish(prepared.key_lock, prepared.directories, excess)
            .unwrap();
        assert!(!path.exists(), "emptied under the lock");
    }

    #[test]
    fn a_superseded_only_policy_is_still_measured_after_the_check() {
        let fixture = fixture();
        let with_cache = host(&fixture, Some(cargo_cache(&fixture)));
        let superseded = CodeWarmPolicy {
            build_cache: vec![WarmBuildCacheKind::CargoHome],
            caches: vec![CacheKindSpec::Cargo],
            max_bytes: Some(4096),
            hard_max_bytes: Some(4096),
            clone_when_busy: None,
            build_wall_ms: None,
        };
        let mut session = WarmSession::new(&with_cache, &superseded, "repo", declaration(b"x"));
        let prepared = prepare(&fixture, &mut session);
        assert!(prepared.directories.is_empty());
        let key_path = prepared.key_lock.as_ref().unwrap().path().to_path_buf();
        let (_, excess) = run_with(
            &fixture,
            &session,
            &prepared,
            "head -c 8192 /dev/zero > \"$WARM_KEY/junk\"",
            &[("WARM_KEY", key_path.display().to_string())],
        );
        assert!(
            matches!(excess, Some(Excess::Bound(bytes)) if bytes >= 8192),
            "the key is measured although nothing was bound: {excess:?}"
        );
        session
            .finish(prepared.key_lock, prepared.directories, excess)
            .unwrap();
        assert!(!key_path.join("junk").exists(), "emptied under the lock");
    }

    /// The held check and a waiting one: the waiter holds the layer once the holder released
    /// it, says it waited, and names the holder its record named (ADR-0201).
    #[test]
    fn two_checks_in_one_layer_serialize_and_the_second_names_whom_it_waited_for() {
        let fixture = fixture();
        let mut host = host(&fixture, None);
        host.lock_wait = None;
        let policy = warm(None);
        let mut first = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let held = prepare(&fixture, &mut first);
        assert_eq!(held.layer.lock, TaskLayerLockV1::Held);
        let key = held.layer.key.clone().unwrap();
        let (project, toolchain) = key.split_once('/').unwrap();
        let recorded = holder_of(
            &fixture.root.path().join("task-build-cache"),
            project,
            toolchain,
        )
        .unwrap();
        assert_eq!(recorded.pid, std::process::id());
        assert_eq!(recorded.command, "af task run");
        let released = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            drop(held);
            Instant::now()
        });
        let mut second = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let waited = prepare(&fixture, &mut second);
        let acquired = Instant::now();
        let released = released.join().unwrap();
        assert!(
            acquired >= released,
            "the second check never overlapped the first"
        );
        assert_eq!(waited.layer.lock, TaskLayerLockV1::Waited);
        assert!(waited.layer.waited_ms >= 500, "{}", waited.layer.waited_ms);
        assert_eq!(waited.layer.key.as_deref(), Some(key.as_str()));
        assert_eq!(
            waited.layer.holder.as_ref().map(|holder| holder.pid),
            Some(std::process::id())
        );
        assert_eq!(waited.directories.len(), 1, "it holds the layer itself now");
        assert!(waited.observations[0].eligible);
        assert!(
            wait_notice(
                &key,
                Some(&recorded),
                Duration::from_secs(60),
                Duration::from_secs(5)
            )
            .starts_with(&format!(
                "waiting for warm layer {key} held by af task run {} since ",
                std::process::id()
            ))
        );
    }

    /// With `clone_when_busy`, a check that finds the layer held builds in a private copy of
    /// it, with the holder's bytes and modification times, and never touches the layer.
    #[test]
    fn a_busy_layer_is_cloned_into_the_check_runtime_when_clone_when_busy_is_on() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = CodeWarmPolicy {
            clone_when_busy: Some(true),
            ..warm(None)
        };
        let mut first = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let held = prepare(&fixture, &mut first);
        let layer = held.directories[0].1.path().to_path_buf();
        std::fs::create_dir_all(layer.join("debug")).unwrap();
        std::fs::write(layer.join("debug/libx.rlib"), vec![7_u8; 4096]).unwrap();
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(layer.join("debug/libx.rlib"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let mut second = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let cloned = prepare(&fixture, &mut second);
        assert_eq!(cloned.layer.lock, TaskLayerLockV1::Cloned);
        assert!(cloned.key_lock.is_none() && cloned.directories.is_empty());
        let copy = fixture.runtime.path().join("cargo_target-clone");
        assert_eq!(target(&cloned), copy.display().to_string());
        assert_eq!(cloned.observations[0].kind, "cargo_target:cloned");
        assert_eq!(cloned.observations[0].bytes_available, 4096);
        assert!(cloned.started_warm());
        let copied = copy.join("debug/libx.rlib");
        assert_eq!(std::fs::read(&copied).unwrap(), vec![7_u8; 4096]);
        assert_eq!(std::fs::metadata(&copied).unwrap().modified().unwrap(), old);
        assert!(
            layer.join("debug/libx.rlib").exists(),
            "the layer is untouched"
        );
        drop(held);
    }

    fn step_line(label: &str, stage: &str) -> String {
        format!(
            "{{\"schema\":\"af.check-report/1\",\"record\":\"step\",\"label\":\"{label}\",\
             \"stage\":\"{stage}\",\"exit_code\":0,\"command\":[\"true\"]}}"
        )
    }

    fn phased(
        fixture: &Fixture,
        script: &str,
        deadlines: PhaseDeadlines,
    ) -> (CheckExecution, PhaseRecord) {
        let host = host(fixture, None);
        let policy = warm(None);
        let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
        let prepared = prepare(fixture, &mut session);
        let report = fixture.runtime.path().join("check-report.jsonl");
        let _ = std::fs::remove_file(&report);
        let runner = prepared.environment.iter().fold(
            CheckRunner::new(&fixture.cas, fixture.workdir.path())
                .with_timeout(Duration::from_secs(120))
                .with_env("AF_CHECK_REPORT", report.display().to_string()),
            |runner, (key, value)| runner.with_env(key.clone(), value.clone()),
        );
        let check = CheckDefinition::new(
            "phased",
            review_core::Command::new(
                "/bin/sh",
                vec![
                    review_core::Arg::literal("-c"),
                    review_core::Arg::literal(script),
                ],
            ),
        );
        let (execution, _, record) = session.run_monitored(
            runner,
            &check,
            Some((prepared.key_lock.as_ref().unwrap(), &prepared.directories)),
            &report,
            deadlines,
            None,
        );
        session
            .finish(prepared.key_lock, prepared.directories, None)
            .unwrap();
        (execution, record)
    }

    /// The build phase ends at the build step; the tests' deadline counts from there, so a
    /// slow build never starves them, and a build past its own deadline is ended as such.
    #[test]
    fn build_and_test_phases_have_their_own_deadlines() {
        let fixture = fixture();
        let short = PhaseDeadlines {
            build: Some(Duration::from_millis(1500)),
            tests: Duration::from_millis(1500),
        };
        let started = Instant::now();
        let (execution, record) = phased(
            &fixture,
            &format!(
                "echo '{}' >> \"$AF_CHECK_REPORT\"; sleep 60",
                step_line("lint", "lint")
            ),
            short,
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "ended at its build deadline"
        );
        assert_eq!(record.deadline, Some(TaskLayerPhaseV1::Build));
        assert_eq!(record.build_end, None);
        assert_ne!(execution.result.status, review_check::CheckStatus::Passed);
        assert!(
            record
                .deadline_reason(short)
                .unwrap()
                .starts_with("the build phase ran past [warm] build_wall_ms (1500 ms)")
        );

        // A build that takes longer than the tests' deadline still leaves the tests theirs.
        let (execution, record) = phased(
            &fixture,
            &format!(
                "sleep 2; echo '{}' >> \"$AF_CHECK_REPORT\"; sleep 1; echo done",
                step_line("test-build", "build")
            ),
            PhaseDeadlines {
                build: Some(Duration::from_secs(10)),
                tests: Duration::from_secs(5),
            },
        );
        assert_eq!(execution.result.status, review_check::CheckStatus::Passed);
        assert_eq!(record.deadline, None);
        let build = record
            .build_end
            .expect("the build step ended the build phase");
        assert!(build >= Duration::from_secs(2), "{build:?}");
        assert!(record.elapsed >= build);

        let (_, record) = phased(
            &fixture,
            &format!(
                "echo '{}' >> \"$AF_CHECK_REPORT\"; sleep 60",
                step_line("test-build", "build")
            ),
            short,
        );
        assert_eq!(record.deadline, Some(TaskLayerPhaseV1::Tests));
        assert!(record.build_end.is_some());
        assert!(
            record
                .deadline_reason(short)
                .unwrap()
                .starts_with("the tests ran past check_wall_ms (1500 ms) after the build step")
        );

        // A check that reports no step has no phase deadline: only its own wall bounds it.
        let (execution, record) = phased(&fixture, "sleep 2; echo done", short);
        assert_eq!(execution.result.status, review_check::CheckStatus::Passed);
        assert_eq!(record.deadline, None);
        assert_eq!(record.build_end, None);

        // Without a declared build_wall_ms the phases are recorded but neither has a deadline.
        let (execution, record) = phased(
            &fixture,
            &format!(
                "echo '{}' >> \"$AF_CHECK_REPORT\"; sleep 3; echo done",
                step_line("test-build", "build")
            ),
            PhaseDeadlines {
                build: None,
                tests: Duration::from_millis(500),
            },
        );
        assert_eq!(execution.result.status, review_check::CheckStatus::Passed);
        assert_eq!(record.deadline, None);
        assert!(record.build_end.is_some());
    }

    /// A layer the Storage Budget evicted under a running check names the sweep and the time;
    /// one removed without an eviction record names another process.
    #[test]
    fn a_layer_evicted_during_the_build_names_its_cause() {
        let fixture = fixture();
        let host = host(&fixture, None);
        let policy = warm(None);
        for sweep in [true, false] {
            let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"x"));
            let prepared = prepare(&fixture, &mut session);
            let key = prepared.key_lock.as_ref().unwrap().path().to_path_buf();
            let root = fixture.root.path().join("task-build-cache");
            let project = key
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let toolchain = key.file_name().unwrap().to_str().unwrap().to_string();
            let evictor = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                std::fs::remove_dir_all(&key).unwrap();
                if sweep {
                    let record = WarmEvictionV1 {
                        schema: review_core::task::check_layer::WARM_EVICTION_V1.into(),
                        by: review_core::task::check_layer::WarmEvictorV1::StorageSweep,
                        at_unix_ms: unix_ms(),
                        bytes: 0,
                    };
                    review_sandbox::record_key_eviction(
                        &root,
                        &project,
                        &toolchain,
                        &serde_json::to_vec(&record).unwrap(),
                    )
                    .unwrap();
                }
            });
            let runner = prepared.environment.iter().fold(
                CheckRunner::new(&fixture.cas, fixture.workdir.path())
                    .with_timeout(Duration::from_secs(120)),
                |runner, (key, value)| runner.with_env(key.clone(), value.clone()),
            );
            let check = CheckDefinition::new(
                "evicted",
                review_core::Command::new(
                    "/bin/sh",
                    vec![
                        review_core::Arg::literal("-c"),
                        review_core::Arg::literal("sleep 6"),
                    ],
                ),
            );
            let (_, excess, record) = session.run_monitored(
                runner,
                &check,
                Some((prepared.key_lock.as_ref().unwrap(), &prepared.directories)),
                &fixture.runtime.path().join("none.jsonl"),
                NO_PHASES,
                None,
            );
            evictor.join().unwrap();
            let evicted = record
                .evicted
                .expect("the monitor saw the key leave its name");
            assert_eq!(evicted.phase, TaskLayerPhaseV1::Build);
            assert_eq!(
                evicted.by,
                if sweep {
                    TaskLayerEvictorV1::StorageSweep
                } else {
                    TaskLayerEvictorV1::AnotherProcess
                }
            );
            assert!(evicted.sentence("t").starts_with(if sweep {
                "layer evicted during build by storage sweep at t"
            } else {
                "layer evicted during build by another process at t"
            }));
            assert!(matches!(excess, Some(Excess::Suspect(_))));
            let _ = session.finish(prepared.key_lock, prepared.directories, excess);
        }
    }

    /// Issue #266: one warm layer builds Snapshot A, where crate `y` calls `x::old`, then
    /// Snapshot B, where `x` renamed it to `x::new` and `y` calls that, from a materialization
    /// that left every source with one old modification time. With the content rule, B builds
    /// and runs B's code, `z`, unchanged, stays fresh, and returning to A runs A's code again.
    /// Without it (the control) Cargo takes A's `x` as fresh for B.
    #[test]
    fn a_warm_layer_builds_snapshot_b_after_a_with_only_the_changed_crates_stale() {
        use review_sandbox::{Mode, Sandbox};
        use review_source_git::{Entry, EntryKind, Manifest};
        let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        // Keep this small Cargo build in the checkout's own target, as every gate does.
        let target_root = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target"))
            .join("tmp");
        std::fs::create_dir_all(&target_root).unwrap();
        let temp = tempfile::tempdir_in(target_root).unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let rustup = RustupHome::of_kernel();
        let mut environment = vec![
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_default(),
            ),
            ("LC_ALL".into(), "C".into()),
            ("TZ".into(), "UTC".into()),
            ("HOME".into(), home.display().to_string()),
            ("CARGO_INCREMENTAL".into(), "0".into()),
        ];
        environment.extend(rustup.environment());
        let snapshot = |x: &str, y: &str| {
            let files = [
                (
                    "Cargo.toml",
                    "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
                     [dependencies]\ny = { path = \"y\" }\nz = { path = \"z\" }\n[workspace]\n",
                ),
                (
                    "Cargo.lock",
                    "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\
                     dependencies = [\n \"y\",\n \"z\",\n]\n\n[[package]]\nname = \"x\"\n\
                     version = \"0.1.0\"\n\n[[package]]\nname = \"y\"\nversion = \"0.1.0\"\n\
                     dependencies = [\n \"x\",\n]\n\n[[package]]\nname = \"z\"\n\
                     version = \"0.1.0\"\n",
                ),
                (
                    "src/main.rs",
                    "fn main() { print!(\"{}{}\", y::value(), z::value()); }\n",
                ),
                (
                    "x/Cargo.toml",
                    "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                ("x/src/lib.rs", x),
                (
                    "y/Cargo.toml",
                    "[package]\nname = \"y\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
                     [dependencies]\nx = { path = \"../x\" }\n",
                ),
                ("y/src/lib.rs", y),
                (
                    "z/Cargo.toml",
                    "[package]\nname = \"z\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                ("z/src/lib.rs", "pub fn value() -> &'static str { \"z\" }\n"),
            ];
            Manifest::new(
                files
                    .into_iter()
                    .map(|(path, text)| Entry {
                        path: path.into(),
                        kind: EntryKind::File,
                        content: cas.put(text.as_bytes()).unwrap(),
                        size: text.len() as u64,
                    })
                    .collect(),
            )
            .unwrap()
        };
        let a = snapshot(
            "pub fn old() -> &'static str { \"A\" }\n",
            "pub fn value() -> &'static str { x::old() }\n",
        );
        let b = snapshot(
            "pub fn new() -> &'static str { \"B\" }\n",
            "pub fn value() -> &'static str { x::new() }\n",
        );
        let host = WarmCheckHost {
            root: Some(temp.path().join("task-build-cache")),
            ..WarmCheckHost::default()
        };
        let policy = warm(None);
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        // Materialize, give every file the same old time a reflinked CAS blob can carry, take
        // the layer, apply the content rule and build. `content_rule = false` is the control.
        let build = |manifest: &Manifest, content_rule: bool| {
            let sandbox = Sandbox::materialize(manifest, &cas, Mode::ReadOnly).unwrap();
            for entry in &manifest.entries {
                std::fs::File::open(sandbox.root().join(&entry.path))
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(old))
                    .unwrap();
            }
            let runtime = tempfile::tempdir_in(temp.path()).unwrap();
            let mut session = WarmSession::new(&host, &policy, "repo", declaration(b"none"));
            let mut prepared = session
                .prepare(
                    &cas,
                    &environment,
                    sandbox.root(),
                    runtime.path(),
                    None,
                    None,
                    Duration::from_secs(120),
                )
                .unwrap();
            assert_eq!(
                prepared.layer.lock,
                TaskLayerLockV1::Held,
                "{:?}",
                prepared.observations
            );
            if content_rule {
                prepared
                    .refresh_source_mtimes(&sandbox, runtime.path(), None, Duration::from_secs(30))
                    .unwrap();
            }
            let output = std::process::Command::new("cargo")
                .args(["build", "--offline", "--locked", "--message-format=json"])
                .current_dir(sandbox.root())
                .env_clear()
                .envs(environment.iter().map(|(key, value)| (key, value)))
                .envs(prepared.environment.iter().map(|(key, value)| (key, value)))
                .env("CARGO_HOME", temp.path().join("cargo-home"))
                .output()
                .unwrap();
            let messages = String::from_utf8_lossy(&output.stdout).into_owned();
            let printed = if output.status.success() {
                let app =
                    std::process::Command::new(Path::new(target(&prepared)).join("debug/app"))
                        .output()
                        .unwrap();
                String::from_utf8(app.stdout).unwrap()
            } else {
                format!("build failed: {}", String::from_utf8_lossy(&output.stderr))
            };
            let fresh = |package: &str| {
                messages.lines().any(|line| {
                    serde_json::from_str::<serde_json::Value>(line).is_ok_and(|message| {
                        message["reason"] == "compiler-artifact"
                            && message["target"]["name"] == package
                            && message["fresh"] == true
                    })
                })
            };
            let freshness = (fresh("x"), fresh("y"), fresh("z"));
            let sources = prepared.layer.sources;
            session
                .finish(prepared.key_lock, prepared.directories, None)
                .unwrap();
            (printed, freshness, sources)
        };

        let (printed, _, sources) = build(&a, true);
        assert_eq!(printed, "Az");
        assert_eq!(
            sources,
            TaskLayerSourcesV1 { stale: 4, fresh: 0 },
            "no record: all cold"
        );

        let (printed, (x, y, z), sources) = build(&b, true);
        assert_eq!(printed, "Bz", "B builds from B's sources");
        assert_eq!(sources, TaskLayerSourcesV1 { stale: 2, fresh: 2 });
        assert!(!x && !y, "the changed crates were rebuilt");
        assert!(z, "the unchanged crate kept its build");

        let (printed, (x, y, z), _) = build(&a, true);
        assert_eq!(printed, "Az", "A again, from A's sources");
        assert!(!x && !y && z);

        // The control: without the content rule the same layer links A's `x` into B.
        let (printed, _, _) = build(&b, false);
        assert_ne!(printed, "Bz", "the stale link #266 recorded");
    }
}
