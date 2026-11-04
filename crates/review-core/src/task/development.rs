//! A Worker's requested local development capabilities. Host paths and credentials are never
//! package authority: a trusted machine binding must resolve every requested tool.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{is_name, require};

pub const DEVELOPMENT_FILE: &str = "development.toml";
pub const DEVELOPMENT_SCHEMA: &str = "af.worker-development/3";
pub const COMMAND_EVIDENCE_SCHEMA: &str = "af.worker-command-evidence/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDevelopmentV3 {
    pub schema: String,
    /// Symbolic tool name, resolved only through machine authority.
    pub harness: String,
    pub binding_identity: String,
    /// Tool name -> exact identity of the installed credential-free tool closure.
    pub tools: BTreeMap<String, String>,
    pub environment: BTreeMap<String, String>,
    #[serde(deserialize_with = "super::unique_set")]
    pub writable_roots: BTreeSet<DevelopmentRootV1>,
    pub state: PrivateStatePolicyV1,
    pub cache: PrivateStatePolicyV1,
    pub knowledge: PrivateStatePolicyV1,
    pub dependencies: DependencyPolicyV1,
    pub network: NetworkPolicyV1,
    pub limits: DevelopmentLimitsV1,
    /// Empty means no cross-Attempt state. Each root has at most one policy.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub caches: BTreeMap<String, DevelopmentCachePolicyV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentRootV1 {
    Source,
    State,
    Scratch,
    Build,
    Cache,
    Knowledge,
}
impl DevelopmentRootV1 {
    pub fn name(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::State => "state",
            Self::Scratch => "scratch",
            Self::Build => "build",
            Self::Cache => "cache",
            Self::Knowledge => "knowledge",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateStatePolicyV1 {
    AttemptPrivate,
    Disabled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyPolicyV1 {
    /// Available in the admitted tool closure, source or approved immutable Cache Snapshot;
    /// a miss never authorizes acquisition.
    AvailableOnly,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicyV1 {
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentLimitsV1 {
    pub wall_ms: u64,
    pub output_bytes: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cpu_millicores: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub memory_bytes: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub disk_bytes: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub processes: Option<u64>,
}

/// Values are either literals or one symbolic path plus safe relative components. No shell
/// interpolation, host path, second variable, alternate path alphabet or parent traversal.
pub fn symbolic_value(value: &str) -> Result<Option<(&str, &str)>, String> {
    require(
        value.len() <= 4096 && !value.chars().any(char::is_control),
        "Invalid development environment value",
    )?;
    if let Some(rest) = value.strip_prefix("${") {
        let (root, suffix) = rest.split_once('}').ok_or("Unclosed development path")?;
        require(
            [
                "source",
                "state",
                "scratch",
                "build",
                "cache",
                "knowledge",
                "dependencies",
            ]
            .contains(&root),
            "Unknown development path",
        )?;
        let relative = suffix.strip_prefix('/').unwrap_or(suffix);
        require(
            suffix.is_empty() || suffix.starts_with('/'),
            "Development path needs a separator",
        )?;
        require(
            relative.is_empty()
                || relative.split('/').all(|part| {
                    !part.is_empty()
                        && part != "."
                        && part != ".."
                        && !part.contains(['\\', '$', '{', '}'])
                }),
            "Unsafe development path",
        )?;
        Ok(Some((root, relative)))
    } else {
        require(
            !value.contains(['$', '\\']) && !value.starts_with('/'),
            "Development values cannot name host paths or expansions",
        )?;
        Ok(None)
    }
}

pub fn reserved_variable(key: &str) -> bool {
    matches!(
        key,
        "HOME"
            | "PWD"
            | "OLDPWD"
            | "USER"
            | "LOGNAME"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "PROMPT_COMMAND"
            | "DOCKER_HOST"
            | "DOCKER_CONTEXT"
            | "DOCKER_CONFIG"
            | "CONTAINER_HOST"
            | "KUBECONFIG"
            | "PATH"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "SHELL"
            | "ENV"
            | "BASH_ENV"
            | "ZDOTDIR"
            | "CDPATH"
            | "GIT_CONFIG"
            | "GIT_CONFIG_GLOBAL"
            | "GIT_CONFIG_SYSTEM"
    ) || ["AF_", "XDG_", "LD_", "DYLD_", "SSH_", "CODEX_", "CLAUDE_"]
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

impl WorkerDevelopmentV3 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == DEVELOPMENT_SCHEMA,
            "Unsupported Worker development schema",
        )?;
        require(
            crate::is_digest(&self.binding_identity),
            "Development requires an exact machine binding identity",
        )?;
        require(
            is_name(&self.harness) && self.tools.contains_key(&self.harness),
            "Development harness must name a pinned tool",
        )?;
        require(
            !self.tools.is_empty()
                && self.tools.len() <= 32
                && self
                    .tools
                    .iter()
                    .all(|(name, identity)| is_name(name) && crate::is_digest(identity)),
            "Development tools need bounded exact identities",
        )?;
        require(
            self.environment.len() <= 64,
            "Too many development variables",
        )?;
        for (key, value) in &self.environment {
            require(
                !reserved_variable(key)
                    && !key.is_empty()
                    && key.len() <= 128
                    && key.bytes().enumerate().all(|(i, byte)| {
                        byte.is_ascii_alphabetic() || byte == b'_' || i > 0 && byte.is_ascii_digit()
                    }),
                "Reserved or invalid development variable",
            )?;
            if let Some((root, _)) = symbolic_value(value)? {
                let disabled = match root {
                    "state" => self.state == PrivateStatePolicyV1::Disabled,
                    "cache" => self.cache == PrivateStatePolicyV1::Disabled,
                    "knowledge" => self.knowledge == PrivateStatePolicyV1::Disabled,
                    _ => false,
                };
                require(!disabled, "Development variable names disabled state")?;
            }
        }
        for (root, policy) in [
            (DevelopmentRootV1::State, self.state),
            (DevelopmentRootV1::Cache, self.cache),
            (DevelopmentRootV1::Knowledge, self.knowledge),
        ] {
            require(
                policy != PrivateStatePolicyV1::Disabled || !self.writable_roots.contains(&root),
                "Disabled development state cannot be writable",
            )?;
        }
        require(
            self.caches.len() <= 3,
            "At most three development cache roots",
        )?;
        let mut destinations = BTreeSet::new();
        let mut ports = BTreeSet::new();
        for (name, cache) in &self.caches {
            require(is_name(name), "Invalid development cache name")?;
            cache.validate()?;
            require(
                destinations.insert(cache.destination),
                "Overlapping development cache roots",
            )?;
            require(
                self.writable_roots.contains(&cache.destination),
                "Cache clone requires a writable private root",
            )?;
            for port in cache.input.iter().chain(&cache.output) {
                require(
                    ports.insert(port),
                    "Development cache ports must be distinct",
                )?;
            }
        }
        require(
            self.limits.wall_ms > 0
                && self.limits.wall_ms <= 14_400_000
                && self.limits.output_bytes > 0
                && self.limits.output_bytes <= 1024 * 1024,
            "Development wall/output bound is invalid",
        )?;
        require(
            self.limits.cpu_millicores.is_some()
                && self.limits.memory_bytes.is_some()
                && self.limits.disk_bytes.is_some()
                && self.limits.processes.is_some(),
            "Worker development requires CPU, memory, aggregate disk and process bounds",
        )?;
        require(
            [
                self.limits.cpu_millicores,
                self.limits.memory_bytes,
                self.limits.disk_bytes,
                self.limits.processes,
            ]
            .into_iter()
            .flatten()
            .all(|bound| bound > 0 && super::safe_number(bound)),
            "Development quotas must be positive exact integers",
        )?;
        require(
            self.limits.disk_bytes.is_some_and(|bytes| {
                (4096..=8 * 1024 * 1024 * 1024).contains(&bytes) && bytes % 4096 == 0
            }),
            "Aggregate disk bound must be a 4096-byte multiple from 4096 through 8589934592",
        )
    }

    /// The admitted tmpfs-volume backend enforces one aggregate storage ceiling. Linux
    /// tmpfs rounds sizes to pages; only exact page multiples are admitted.
    pub fn validate_container(&self) -> Result<(), String> {
        self.validate()
    }
}

/// Machine-owned, credential-free container binding. A package never supplies these paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDevelopmentBindingV3 {
    pub schema: String,
    pub storage: WorkerStorageBackendV1,
    pub runtime: String,
    pub image: String,
    /// Trusted image utility used only to hash installed executables, never a project build.
    pub checksum_program: String,
    /// Trusted POSIX shell for image-only mount-layout admission, never project code.
    pub probe_shell: String,
    pub tools: BTreeMap<String, DevelopmentToolV1>,
    pub limits: DevelopmentLimitsV1,
    /// Machine-approved immutable dependency Manifests in this Task's CAS. Acquisition is
    /// available-only: an operator must import credential-free bytes separately.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependency_sources: BTreeMap<String, String>,
    /// No declaration means the machine grants no cross-Attempt cache capability.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache_limits: Option<crate::BuildCacheLimitsV1>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStorageBackendV1 {
    /// Machine authorization to create a private local-driver tmpfs volume, never a host bind.
    TmpfsVolume,
}

/// Private CheckRuntime recovery intent, persisted before creating daemon-owned resources.
/// A Worker never sees this file or supplies its runtime/name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerStorageLeaseV1 {
    pub schema: String,
    pub runtime: String,
    pub name: String,
}
impl WorkerStorageLeaseV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.worker-storage/1"
                && canonical_program(&self.runtime)
                && self.name.starts_with("af-storage-")
                && self.name.len() > "af-storage-".len()
                && self.name.len() <= 128
                && self
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "Unsupported Worker storage recovery intent",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentToolV1 {
    pub program: String,
    pub identity: String,
}
impl WorkerDevelopmentBindingV3 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.worker-development-binding/3",
            "Unsupported Worker development binding schema",
        )?;
        require(
            canonical_program(&self.runtime),
            "Development runtime must be a canonical absolute machine path",
        )?;
        if let Some(limits) = &self.cache_limits {
            limits.validate()?;
        }
        require(
            self.dependency_sources.len() <= 32
                && self
                    .dependency_sources
                    .iter()
                    .all(|(name, id)| is_name(name) && crate::is_digest(id)),
            "Invalid approved dependency sources",
        )?;
        let (repository, digest) = self
            .image
            .split_once('@')
            .ok_or("Development image must be digest-pinned")?;
        require(
            !repository.is_empty()
                && !repository.starts_with('-')
                && !repository.chars().any(char::is_whitespace)
                && crate::is_digest(digest),
            "Development image must be digest-pinned",
        )?;
        require(
            image_program(&self.checksum_program)
                && image_program(&self.probe_shell)
                && !self.tools.is_empty()
                && self.tools.len() <= 32
                && self.tools.iter().all(|(name, tool)| {
                    is_name(name)
                        && image_program(&tool.program)
                        && crate::is_digest(&tool.identity)
                }),
            "Invalid machine development tools",
        )?;
        require(
            self.limits.wall_ms > 0
                && self.limits.wall_ms <= 14_400_000
                && self.limits.output_bytes > 0
                && self.limits.output_bytes <= 1024 * 1024
                && self.limits.disk_bytes.is_some_and(|bytes| {
                    (4096..=8 * 1024 * 1024 * 1024).contains(&bytes) && bytes % 4096 == 0
                }),
            "Invalid or unsupported machine development limits",
        )?;
        require(
            [
                self.limits.cpu_millicores,
                self.limits.memory_bytes,
                self.limits.processes,
            ]
            .into_iter()
            .all(|bound| bound.is_some_and(|bound| bound > 0 && super::safe_number(bound))),
            "Machine development binding requires CPU, memory and process ceilings",
        )
    }

    pub fn admit(&self, request: &WorkerDevelopmentV3) -> Result<(), String> {
        self.validate()?;
        request.validate_container()?;
        require(
            request.tools.iter().all(|(name, identity)| {
                self.tools
                    .get(name)
                    .is_some_and(|tool| &tool.identity == identity)
            }),
            "Worker development requests an unbound tool or changed identity",
        )?;
        require(
            request.limits.wall_ms <= self.limits.wall_ms
                && request.limits.output_bytes <= self.limits.output_bytes,
            "Worker development widens machine wall/output authority",
        )?;
        for (requested, allowed) in [
            (request.limits.cpu_millicores, self.limits.cpu_millicores),
            (request.limits.memory_bytes, self.limits.memory_bytes),
            (request.limits.disk_bytes, self.limits.disk_bytes),
            (request.limits.processes, self.limits.processes),
        ] {
            require(
                requested
                    .is_some_and(|requested| allowed.is_some_and(|allowed| requested <= allowed)),
                "Worker development needs bounded CPU, memory, aggregate disk and processes within machine policy",
            )?;
        }
        for cache in request.caches.values() {
            let ceiling = self
                .cache_limits
                .as_ref()
                .ok_or("Machine does not authorize development cache persistence")?;
            require(
                cache.limits.max_bytes <= ceiling.max_bytes
                    && cache.limits.max_entries <= ceiling.max_entries
                    && cache.limits.max_depth <= ceiling.max_depth
                    && cache.limits.max_path_bytes <= ceiling.max_path_bytes,
                "Development cache widens machine limits",
            )?;
            if let Some(source) = &cache.dependency_source {
                require(
                    self.dependency_sources.contains_key(source),
                    "Unapproved dependency acquisition source",
                )?;
            }
        }
        Ok(())
    }
}
fn image_program(path: &str) -> bool {
    let dynamic = [
        "/work",
        "/base",
        "/proc",
        "/sys",
        "/dev",
        "/tmp",
        "/run",
        "/input",
        "/disk",
        "/af/source",
        "/af/state",
        "/af/scratch",
        "/af/build",
        "/af/cache",
        "/af/knowledge",
        "/af/package",
    ];
    !dynamic
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
        && canonical_program(path)
}

fn canonical_program(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && path[1..].split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.chars().any(char::is_control)
                && !part.contains('\\')
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCommandPurposeV1 {
    ToolProbe,
    Harness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCommandEvidenceV1 {
    pub schema: String,
    pub purpose: WorkerCommandPurposeV1,
    pub attempt_id: String,
    pub context_id: String,
    pub source_snapshot_id: String,
    pub input_tree_digest: String,
    pub input_manifest_id: String,
    #[serde(deserialize_with = "nullable")]
    pub final_tree_digest: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub final_manifest_id: Option<String>,
    pub experiment: bool,
    pub command: Vec<String>,
    pub cwd: String,
    pub environment_id: String,
    pub binding_id: String,
    pub tool_probe_output_id: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    #[serde(deserialize_with = "nullable")]
    pub exit_code: Option<i32>,
    #[serde(deserialize_with = "nullable")]
    pub failure: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub resources: Option<CommandResourcesV1>,
    pub stdout_id: String,
    pub stderr_id: String,
    /// Exact bounded bytes returned by the command transport; absent when it returned failure.
    #[serde(deserialize_with = "nullable")]
    pub returned_output_id: Option<String>,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
    pub output_cap: u64,
    pub redacted: bool,
    #[serde(deserialize_with = "super::unique_set")]
    pub omissions: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResourcesV1 {
    pub user_ms: u64,
    pub system_ms: u64,
    #[serde(deserialize_with = "nullable")]
    pub max_rss_bytes: Option<u64>,
}
impl WorkerCommandEvidenceV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == COMMAND_EVIDENCE_SCHEMA
                && self.attempt_id.len() == 26
                && self
                    .attempt_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric()),
            "Invalid Worker command evidence schema or Attempt",
        )?;
        require(
            [
                &self.context_id,
                &self.source_snapshot_id,
                &self.input_tree_digest,
                &self.input_manifest_id,
                &self.environment_id,
                &self.binding_id,
                &self.tool_probe_output_id,
                &self.stdout_id,
                &self.stderr_id,
            ]
            .into_iter()
            .all(|id| crate::is_digest(id))
                && self
                    .final_tree_digest
                    .as_ref()
                    .is_none_or(|id| crate::is_digest(id))
                && self
                    .final_manifest_id
                    .as_ref()
                    .is_none_or(|id| crate::is_digest(id))
                && self
                    .returned_output_id
                    .as_ref()
                    .is_none_or(|id| crate::is_digest(id)),
            "Worker command evidence needs exact identities",
        )?;
        require(
            self.cwd == "/work"
                && !self.command.is_empty()
                && self.command.len() <= 1024
                && self
                    .command
                    .iter()
                    .all(|value| value.len() <= 4096 && !value.contains('\0')),
            "Invalid Worker command evidence command/cwd",
        )?;
        require(
            self.output_cap > 0
                && self.output_cap <= 1024 * 1024
                && self.started_unix_ms > 0
                && super::safe_number(self.started_unix_ms)
                && super::safe_number(self.elapsed_ms),
            "Invalid Worker command evidence bounds",
        )?;
        require(
            super::safe_number(self.stdout_total_bytes)
                && super::safe_number(self.stderr_total_bytes)
                && self.resources.as_ref().is_none_or(|resource| {
                    super::safe_number(resource.user_ms)
                        && super::safe_number(resource.system_ms)
                        && resource.max_rss_bytes.is_none_or(super::safe_number)
                }),
            "Worker command resource counters must be exact integers",
        )?;
        require(
            self.failure.is_some() || self.exit_code == Some(0),
            "Worker command success needs an observed zero exit",
        )?;
        require(
            self.returned_output_id.is_none()
                || self.failure.is_none()
                    && self.exit_code == Some(0)
                    && self.stdout_total_bytes <= self.output_cap
                    && self.stderr_total_bytes <= self.output_cap,
            "Failed or truncated Worker command cannot return output",
        )?;
        require(
            self.returned_output_id
                .as_ref()
                .is_none_or(|id| id == &self.stdout_id)
                && (!self.redacted || self.returned_output_id.is_none()),
            "Worker command output must identify the exact unredacted stdout",
        )?;
        require(
            self.purpose != WorkerCommandPurposeV1::ToolProbe || self.returned_output_id.is_none(),
            "Tool identity probes cannot claim a Worker reply",
        )?;
        require(
            self.final_tree_digest.is_some() == self.final_manifest_id.is_some()
                && (self.final_tree_digest.is_some()
                    || self.omissions.contains("final_tree_unavailable")),
            "Worker command final tree omission must be explicit",
        )?;
        require(
            [
                "internal_commands_unobserved",
                "internal_model_output_unobserved",
                "resources_are_runtime_client_only",
            ]
            .into_iter()
            .all(|marker| self.omissions.contains(marker)),
            "Worker command evidence must name unavailable internal observations",
        )?;
        require(
            self.omissions.iter().all(|value| {
                [
                    "internal_commands_unobserved",
                    "internal_model_output_unobserved",
                    "resources_are_runtime_client_only",
                    "final_tree_unavailable",
                ]
                .contains(&value.as_str())
            }),
            "Unknown Worker command evidence omission",
        )
    }
}

// With deserialize_with and no default, omission is an error while an explicit null remains
// an honest unavailable observation. This keeps the required nullable JSON fields strict.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

pub const DEVELOPMENT_WARM_PORT: &str = "development_warm";

/// A typed cache request, never a host path. Reuse and persistence are explicit graph ports;
/// a retry sees the invocation's original selection, not its failed sibling's directories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentCachePolicyV1 {
    pub role: DevelopmentCacheRoleV1,
    pub destination: DevelopmentRootV1,
    /// Canonical private subdirectory; empty selects the whole build/cache root.
    pub path: String,
    pub selection: DevelopmentCacheSelectionV1,
    pub persistence: DevelopmentCachePersistenceV1,
    /// Immutable layers are retained and reclaimed with the owning Task; mutable clones
    /// remain Attempt-private. A Worker cannot request a machine-wide or immortal cache.
    pub lifecycle: DevelopmentCacheLifecycleV1,
    pub limits: crate::BuildCacheLimitsV1,
    /// Bounded source-file content identities, e.g. dependency locks and tool configuration.
    #[serde(deserialize_with = "super::unique_set")]
    pub fingerprints: BTreeSet<String>,
    /// Explicit flags, profile, features and other tool-independent compatibility keys.
    pub keys: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub input: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub output: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub dependency_source: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentCacheRoleV1 {
    Dependency,
    Build,
    Continuation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentCacheSelectionV1 {
    Compatible,
    ExactSource,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentCachePersistenceV1 {
    Discard,
    AdmittedOutput,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentCacheLifecycleV1 {
    Task,
}
impl DevelopmentCachePolicyV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.limits.validate()?;
        require(
            self.path.len() <= 4096
                && (self.path.is_empty()
                    || self.path.split('/').all(|part| {
                        !part.is_empty()
                            && part != "."
                            && part != ".."
                            && !part.contains(['\\', '$', '{', '}'])
                            && !part.chars().any(char::is_control)
                    })),
            "Unsafe private cache subdirectory",
        )?;
        require(
            matches!(
                self.destination,
                DevelopmentRootV1::Build | DevelopmentRootV1::Cache | DevelopmentRootV1::State
            ),
            "Only build, cache and explicit continuation state may be carried",
        )?;
        require(
            self.fingerprints.len() <= 64
                && self.fingerprints.iter().all(|path| {
                    crate::is_valid_repo_path(path)
                        && crate::encode_path(&crate::decode_path(path)) == *path
                        && !path.contains('\\')
                        && path.len() <= 4096
                }),
            "Invalid bounded cache fingerprints",
        )?;
        require(
            self.keys.len() <= 32
                && self.keys.iter().all(|(key, value)| {
                    is_name(key) && value.len() <= 256 && !value.chars().any(char::is_control)
                }),
            "Invalid cache compatibility keys",
        )?;
        require(
            self.input.iter().chain(&self.output).all(|port| {
                is_name(port) && !["source", DEVELOPMENT_WARM_PORT].contains(&port.as_str())
            }),
            "Invalid cache graph port",
        )?;
        require(
            (self.persistence == DevelopmentCachePersistenceV1::AdmittedOutput)
                == self.output.is_some(),
            "Persistence requires exactly one admitted output port",
        )?;
        match self.role {
            DevelopmentCacheRoleV1::Dependency => require(
                self.dependency_source.as_deref().is_some_and(is_name)
                    && self.input.is_none()
                    && self.output.is_none()
                    && self.destination == DevelopmentRootV1::Cache
                    && self.limits.max_bytes <= crate::cache::MAX_CACHE_BYTES_V1
                    && self.limits.max_entries <= crate::cache::MAX_CACHE_ENTRIES_V1,
                "Dependency Cache Snapshots require machine acquisition authority and cannot be Worker outputs",
            )?,
            DevelopmentCacheRoleV1::Build => require(
                self.dependency_source.is_none() && self.destination == DevelopmentRootV1::Build,
                "Build Caches use only a private build destination",
            )?,
            DevelopmentCacheRoleV1::Continuation => require(
                self.dependency_source.is_none()
                    && self.destination == DevelopmentRootV1::State
                    && self.selection == DevelopmentCacheSelectionV1::ExactSource
                    && !self.path.is_empty()
                    && !["home", "config"].contains(&self.path.split('/').next().unwrap_or("")),
                "Continuation state requires exact source and same-lineage admission",
            )?,
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentCacheProvenanceV1 {
    pub run_id: String,
    pub slot: String,
    pub name: String,
    pub role: DevelopmentCacheRoleV1,
    pub compatibility_id: String,
    pub input_tree_digest: String,
    pub output_tree_digest: String,
}
impl DevelopmentCacheProvenanceV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            !self.run_id.is_empty()
                && self.run_id.len() <= 256
                && self.slot.len() <= 1024
                && !self.slot.is_empty()
                && is_name(&self.name)
                && self.role != DevelopmentCacheRoleV1::Dependency
                && crate::is_digest(&self.compatibility_id)
                && crate::is_digest(&self.input_tree_digest)
                && crate::is_digest(&self.output_tree_digest),
            "Invalid Worker cache provenance",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum DevelopmentCacheSelectionRecordV1 {
    Selected {
        artifact_id: String,
        manifest_id: String,
        compatibility_id: String,
    },
    Omitted {
        reason: DevelopmentCacheMissV1,
        compatibility_id: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentCacheMissV1 {
    NoInput,
    Incompatible,
    WrongLineage,
    Missing,
    Corrupt,
    OverBound,
}
impl DevelopmentCacheSelectionRecordV1 {
    pub fn validate(&self) -> Result<(), String> {
        let ids: Vec<&str> = match self {
            Self::Selected {
                artifact_id,
                manifest_id,
                compatibility_id,
            } => vec![artifact_id, manifest_id, compatibility_id],
            Self::Omitted {
                compatibility_id, ..
            } => vec![compatibility_id],
        };
        require(
            ids.into_iter().all(crate::is_digest),
            "Cache selection requires exact identities",
        )
    }
    pub fn compatibility_id(&self) -> &str {
        match self {
            Self::Selected {
                compatibility_id, ..
            }
            | Self::Omitted {
                compatibility_id, ..
            } => compatibility_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCacheCaptureV1 {
    pub schema: String,
    pub attempt_id: String,
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub artifact_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub omission: Option<WorkerCacheCaptureOmissionV1>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCacheCaptureOmissionV1 {
    CommandFailed,
    CleanupUnconfirmed,
    Cancelled,
    Deadline,
    CaptureRefused,
}
impl WorkerCacheCaptureV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.worker-cache-capture/1"
                && self.attempt_id.len() == 26
                && self
                    .attempt_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric())
                && is_name(&self.name)
                && (self.artifact_id.is_some() != self.omission.is_some())
                && self.artifact_id.as_deref().is_none_or(crate::is_digest),
            "Invalid Worker cache capture outcome",
        )
    }
}
