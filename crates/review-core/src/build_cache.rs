//! Worker warm layers, package P2: `BuildCache@1`, the explicitly unsafe capture of a Gate's
//! candidate-built output.
//!
//! A Build Cache is NOT a Cache Snapshot. A Cache Snapshot is an administrator-approved,
//! credential-free dependency cache resolved through machine policy. A Build Cache was produced
//! by candidate code running under the trusted-local policy, which can read anything the
//! operator can. It therefore carries no administrator approval and no credential-free
//! guarantee: it is admitted only under `trusted_local` and refused under the safe policy. Its
//! capture layout is closed: regular files only, descriptor-relative no-follow traversal, entry,
//! depth, path and byte limits, two fixed modes, extended attributes and ACLs stripped, and
//! producer plus head provenance recorded.

use serde::{Deserialize, Serialize};

use crate::cache::CREDENTIAL_COMPONENTS;
use crate::warm::is_monotonic_id;

/// Kernel ceiling for the bytes one Build Cache may carry. Policy may lower it, never raise it.
pub const MAX_BUILD_CACHE_BYTES_V1: u64 = 8 * 1024 * 1024 * 1024;
/// Default byte bound when a Gate declares a build cache without naming one.
pub const DEFAULT_BUILD_CACHE_BYTES_V1: u64 = 2 * 1024 * 1024 * 1024;
/// Kernel ceiling for filesystem entries (directories and files) traversed during capture.
pub const MAX_BUILD_CACHE_ENTRIES_V1: u64 = 500_000;
/// Default entry bound when a Gate declares a build cache without naming one.
pub const DEFAULT_BUILD_CACHE_ENTRIES_V1: u64 = 200_000;
/// Fixed directory depth limit of the closed capture layout.
pub const BUILD_CACHE_MAX_DEPTH_V1: u32 = 64;
/// Fixed portable path length limit of the closed capture layout.
pub const BUILD_CACHE_MAX_PATH_BYTES_V1: u64 = 4096;

/// The closed vocabulary of build cache kinds. A kind names behavior the kernel understands:
/// the sandbox-local directory it clones and the single environment variable that points a
/// build tool at it. The registry-only `cargo` Cache Snapshot keeps its existing meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCacheKindV1 {
    /// A Cargo target directory: `CARGO_TARGET_DIR` points at the sandbox-local clone.
    CargoTarget,
}

impl BuildCacheKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CargoTarget => "cargo_target",
        }
    }

    /// The one environment variable this kind sets, pointing at the sandbox-local clone.
    pub const fn environment_variable(self) -> &'static str {
        match self {
            Self::CargoTarget => "CARGO_TARGET_DIR",
        }
    }
}

impl std::fmt::Display for BuildCacheKindV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The limits one capture applied, recorded with the capture so a reader knows what bounded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildCacheLimitsV1 {
    pub max_bytes: u64,
    /// Filesystem entries traversed, directories included.
    pub max_entries: u64,
    pub max_depth: u32,
    pub max_path_bytes: u64,
}

impl BuildCacheLimitsV1 {
    /// The kernel defaults, with the fixed depth and path bounds of the closed layout.
    pub const fn default_v1() -> Self {
        Self {
            max_bytes: DEFAULT_BUILD_CACHE_BYTES_V1,
            max_entries: DEFAULT_BUILD_CACHE_ENTRIES_V1,
            max_depth: BUILD_CACHE_MAX_DEPTH_V1,
            max_path_bytes: BUILD_CACHE_MAX_PATH_BYTES_V1,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_bytes == 0
            || self.max_bytes > MAX_BUILD_CACHE_BYTES_V1
            || self.max_entries == 0
            || self.max_entries > MAX_BUILD_CACHE_ENTRIES_V1
            || self.max_depth == 0
            || self.max_depth > BUILD_CACHE_MAX_DEPTH_V1
            || self.max_path_bytes == 0
            || self.max_path_bytes > BUILD_CACHE_MAX_PATH_BYTES_V1
        {
            return Err(format!(
                "build cache limits must be positive and bounded by {MAX_BUILD_CACHE_BYTES_V1} bytes, {MAX_BUILD_CACHE_ENTRIES_V1} entries, depth {BUILD_CACHE_MAX_DEPTH_V1} and {BUILD_CACHE_MAX_PATH_BYTES_V1} path bytes"
            ));
        }
        Ok(())
    }
}

impl Default for BuildCacheLimitsV1 {
    fn default() -> Self {
        Self::default_v1()
    }
}

/// Validate one relative path of the closed build cache layout: portable UTF-8, canonical and
/// relative, bounded in length and depth, and never credential-shaped. Directories and files
/// share the rule; the kind decides nothing further today because a Cargo target directory is
/// free-form below its root.
pub fn validate_build_cache_path_v1(
    kind: BuildCacheKindV1,
    path: &[u8],
    limits: &BuildCacheLimitsV1,
) -> Result<(), String> {
    let text = std::str::from_utf8(path)
        .map_err(|_| format!("{kind} build cache paths must be portable UTF-8"))?;
    if !crate::is_valid_repo_path(text) || text.contains('\\') {
        return Err(format!(
            "{kind} build cache contains a non-portable relative path"
        ));
    }
    if path.len() as u64 > limits.max_path_bytes {
        return Err(format!(
            "{kind} build cache path exceeds its {} byte limit",
            limits.max_path_bytes
        ));
    }
    let components: Vec<&str> = text.split('/').collect();
    if components.len() as u64 > u64::from(limits.max_depth) {
        return Err(format!(
            "{kind} build cache tree exceeds its {} directory depth limit",
            limits.max_depth
        ));
    }
    if components.iter().any(|component| {
        CREDENTIAL_COMPONENTS
            .iter()
            .any(|credential| component.as_bytes().eq_ignore_ascii_case(credential))
    }) {
        return Err(format!(
            "{kind} build cache contains a credential-shaped path"
        ));
    }
    Ok(())
}

/// The trust a Build Cache carries. There is exactly one value: it was built by candidate code.
/// The field exists so the artifact says so itself and can never be mistaken for a Cache
/// Snapshot by a reader that only sees the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCacheTrustV1 {
    CandidateBuilt,
}

/// The payload of a `review.kernel/BuildCache@1` artifact.
///
/// `manifest_id` names a `Manifest` of regular files (kind `file` or `executable`, never a
/// symlink) whose content objects live in the CAS, so the cache can be re-materialized on a
/// resumed Round exactly as any other warm layer. `content_digest` is that manifest's Tree
/// Digest. `entries` counts the manifest's files; `limits.max_entries` bounded the traversal
/// that produced them, directories included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildCacheV1 {
    pub kind: BuildCacheKindV1,
    pub trust: BuildCacheTrustV1,
    /// The Gate node whose check Attempt built the cache.
    pub gate_node: String,
    /// The common Task Attempt the Gate ran under, when the Task runtime executed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_attempt_id: Option<String>,
    /// The head Snapshot the Gate built.
    pub head_snapshot_id: String,
    pub manifest_id: String,
    pub content_digest: String,
    pub entries: u64,
    pub bytes: u64,
    pub limits: BuildCacheLimitsV1,
}

impl BuildCacheV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.gate_node.trim().is_empty() {
            return Err("BuildCache@1 has an empty Gate node".into());
        }
        if !self.gate_attempt_id.as_deref().is_none_or(is_monotonic_id) {
            return Err("BuildCache@1 has an invalid Gate Attempt ID".into());
        }
        if !crate::is_digest(&self.head_snapshot_id)
            || !crate::is_digest(&self.manifest_id)
            || !crate::is_digest(&self.content_digest)
        {
            return Err("BuildCache@1 has an invalid head, manifest or content identity".into());
        }
        self.limits.validate()?;
        if self.entries == 0 || self.entries > self.limits.max_entries {
            return Err("BuildCache@1 entry count is empty or exceeds its limit".into());
        }
        if self.bytes > self.limits.max_bytes {
            return Err("BuildCache@1 byte count exceeds its limit".into());
        }
        Ok(())
    }
}

/// Why a declared build cache was not captured. Recorded on the Gate; the Gate verdict is
/// unaffected, and the Workers that declared the kind run without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCacheRefusalReasonV1 {
    /// A symlink, FIFO, socket, device or other non-regular entry, or a credential-shaped path.
    UnsafeContent,
    /// The tree exceeded an entry, depth, path or byte limit.
    LimitExceeded,
    /// The Gate left no cache directory or an empty one.
    SourceUnavailable,
    /// The tree changed during capture or the capture could not be completed.
    CaptureFailed,
}

/// Payload of `BuildCacheCaptured@1`, appended by the Gate after its checks passed: exactly one
/// of the captured artifact or a refusal reason, with the head and the limits that applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildCacheCapturedPayloadV1 {
    pub gate_node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_attempt_id: Option<String>,
    pub head_snapshot_id: String,
    pub kind: BuildCacheKindV1,
    pub limits: BuildCacheLimitsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_cache_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<BuildCacheRefusalReasonV1>,
    pub entries: u64,
    pub bytes: u64,
}

impl BuildCacheCapturedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.gate_node.trim().is_empty() {
            return Err("BuildCacheCaptured@1 has an empty Gate node".into());
        }
        if !self.gate_attempt_id.as_deref().is_none_or(is_monotonic_id) {
            return Err("BuildCacheCaptured@1 has an invalid Gate Attempt ID".into());
        }
        if !crate::is_digest(&self.head_snapshot_id) {
            return Err("BuildCacheCaptured@1 has an invalid head Snapshot ID".into());
        }
        self.limits.validate()?;
        match (&self.build_cache_artifact_id, self.refused) {
            (Some(id), None) if crate::is_digest(id) => {
                if self.entries == 0 {
                    return Err("BuildCacheCaptured@1 captured an empty build cache".into());
                }
            }
            (None, Some(_)) => {}
            _ => {
                return Err(
                    "BuildCacheCaptured@1 must name exactly one of a Build Cache artifact or a refusal"
                        .into(),
                );
            }
        }
        if self.entries > crate::json::SAFE_INTEGER_MAX as u64
            || self.bytes > crate::json::SAFE_INTEGER_MAX as u64
        {
            return Err("BuildCacheCaptured@1 counts exceed the JSON safe-integer bound".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    #[test]
    fn the_closed_layout_refuses_escaping_deep_long_and_credential_shaped_paths() {
        let limits = BuildCacheLimitsV1 {
            max_depth: 3,
            max_path_bytes: 32,
            ..BuildCacheLimitsV1::default_v1()
        };
        let kind = BuildCacheKindV1::CargoTarget;
        validate_build_cache_path_v1(kind, b"debug/deps/libfoo.rlib", &limits).unwrap();
        for refused in [
            &b"../escape"[..],
            b"/absolute",
            b"debug//deps",
            b"a/b/c/d",
            b"debug/deps/an-extremely-long-file-name.rlib",
            b"debug/credentials.toml",
            b"debug\\deps",
            b"a\xffb",
        ] {
            assert!(
                validate_build_cache_path_v1(kind, refused, &limits).is_err(),
                "{refused:?} was admitted"
            );
        }
    }

    #[test]
    fn limits_stay_below_the_kernel_ceilings() {
        BuildCacheLimitsV1::default_v1().validate().unwrap();
        let over = BuildCacheLimitsV1 {
            max_bytes: MAX_BUILD_CACHE_BYTES_V1 + 1,
            ..BuildCacheLimitsV1::default_v1()
        };
        assert!(over.validate().is_err());
        let deep = BuildCacheLimitsV1 {
            max_depth: BUILD_CACHE_MAX_DEPTH_V1 + 1,
            ..BuildCacheLimitsV1::default_v1()
        };
        assert!(deep.validate().is_err());
    }

    #[test]
    fn a_build_cache_names_its_producer_head_and_bounds() {
        let cache = BuildCacheV1 {
            kind: BuildCacheKindV1::CargoTarget,
            trust: BuildCacheTrustV1::CandidateBuilt,
            gate_node: "gate".into(),
            gate_attempt_id: Some("a".repeat(26)),
            head_snapshot_id: digest('1'),
            manifest_id: digest('2'),
            content_digest: digest('3'),
            entries: 2,
            bytes: 10,
            limits: BuildCacheLimitsV1::default_v1(),
        };
        cache.validate().unwrap();
        assert!(
            BuildCacheV1 {
                entries: 0,
                ..cache.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            BuildCacheV1 {
                bytes: MAX_BUILD_CACHE_BYTES_V1,
                ..cache.clone()
            }
            .validate()
            .is_err(),
            "bytes above the applied limit are refused"
        );
        assert!(
            BuildCacheV1 {
                gate_attempt_id: Some("short".into()),
                ..cache
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn a_capture_record_is_either_an_artifact_or_a_refusal() {
        let captured = BuildCacheCapturedPayloadV1 {
            gate_node: "gate".into(),
            gate_attempt_id: None,
            head_snapshot_id: digest('1'),
            kind: BuildCacheKindV1::CargoTarget,
            limits: BuildCacheLimitsV1::default_v1(),
            build_cache_artifact_id: Some(digest('4')),
            refused: None,
            entries: 3,
            bytes: 12,
        };
        captured.validate().unwrap();
        let refused = BuildCacheCapturedPayloadV1 {
            build_cache_artifact_id: None,
            refused: Some(BuildCacheRefusalReasonV1::UnsafeContent),
            entries: 0,
            bytes: 0,
            ..captured.clone()
        };
        refused.validate().unwrap();
        let both = BuildCacheCapturedPayloadV1 {
            refused: Some(BuildCacheRefusalReasonV1::LimitExceeded),
            ..captured.clone()
        };
        assert!(both.validate().is_err());
        let neither = BuildCacheCapturedPayloadV1 {
            build_cache_artifact_id: None,
            ..captured
        };
        assert!(neither.validate().is_err());
    }
}
