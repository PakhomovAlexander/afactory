//! The immutable, content-addressed description of one diff Subject.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

/// One rename Git selected under the recorded diff policy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRenameV1 {
    pub old_path: String,
    pub new_path: String,
    pub similarity: u8,
}

/// The payload of a `review.kernel/ChangeSet@1` artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetV1 {
    pub base_snapshot_id: String,
    pub head_snapshot_id: String,
    pub changed_paths: Vec<String>,
    pub renames: Vec<PathRenameV1>,
    /// Git returned a complete add/delete path set but skipped exhaustive rename linkage at the
    /// fixed policy limit. Scope remains authoritative; consumers must not treat an empty rename
    /// map as proof that no rename occurred.
    #[serde(default, skip_serializing_if = "is_false")]
    pub rename_detection_truncated: bool,
    /// Exact patch bytes. Base64 keeps arbitrary text and binary patches lossless in JSON.
    pub canonical_patch_base64: String,
    pub git_version: String,
    pub diff_policy_version: String,
}

impl ChangeSetV1 {
    pub fn new(
        base_snapshot_id: impl Into<String>,
        head_snapshot_id: impl Into<String>,
        mut changed_paths: Vec<String>,
        mut renames: Vec<PathRenameV1>,
        canonical_patch: &[u8],
        git_version: impl Into<String>,
        diff_policy_version: impl Into<String>,
    ) -> Result<Self, String> {
        changed_paths.sort();
        changed_paths.dedup();
        renames.sort();
        renames.dedup();
        let value = Self {
            base_snapshot_id: base_snapshot_id.into(),
            head_snapshot_id: head_snapshot_id.into(),
            changed_paths,
            renames,
            rename_detection_truncated: false,
            canonical_patch_base64: STANDARD.encode(canonical_patch),
            git_version: git_version.into(),
            diff_policy_version: diff_policy_version.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn with_rename_detection_truncated(mut self, truncated: bool) -> Self {
        self.rename_detection_truncated = truncated;
        self
    }

    pub fn canonical_patch(&self) -> Result<Vec<u8>, String> {
        STANDARD
            .decode(&self.canonical_patch_base64)
            .map_err(|error| {
                format!("ChangeSet@1 canonical patch is not canonical base64: {error}")
            })
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_scope_shape()?;
        validate_canonical_base64(&self.canonical_patch_base64)
    }

    /// Validate the identity and path data needed to derive Report Scope without decoding the
    /// potentially large inline patch. Full artifact admission still uses [`Self::validate`].
    pub fn validate_scope_shape(&self) -> Result<(), String> {
        if !crate::is_digest(&self.base_snapshot_id) || !crate::is_digest(&self.head_snapshot_id) {
            return Err("ChangeSet@1 has an invalid Base or head Snapshot ID".into());
        }
        if self.git_version.trim().is_empty() || self.diff_policy_version.trim().is_empty() {
            return Err("ChangeSet@1 requires Git build and diff-policy versions".into());
        }
        if self.changed_paths.windows(2).any(|pair| pair[0] >= pair[1])
            || self.changed_paths.iter().any(|path| !valid_path(path))
        {
            return Err("ChangeSet@1 changed paths must be sorted, unique, and relative".into());
        }
        if self.renames.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("ChangeSet@1 renames must be sorted and unique".into());
        }
        for rename in &self.renames {
            if rename.similarity > 100
                || rename.old_path == rename.new_path
                || !valid_path(&rename.old_path)
                || !valid_path(&rename.new_path)
                || self.changed_paths.binary_search(&rename.old_path).is_err()
                || self.changed_paths.binary_search(&rename.new_path).is_err()
            {
                return Err(
                    "ChangeSet@1 rename paths must be changed, distinct, relative, and at most 100% similar"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

fn validate_canonical_base64(encoded: &str) -> Result<(), String> {
    let bytes = encoded.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("ChangeSet@1 canonical patch is not canonical base64: invalid length".into());
    }
    let padding = if bytes.ends_with(b"==") {
        2
    } else if bytes.ends_with(b"=") {
        1
    } else {
        0
    };
    let data_len = bytes.len().saturating_sub(padding);
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    if bytes[..data_len].iter().any(|byte| value(*byte).is_none())
        || bytes[data_len..].iter().any(|byte| *byte != b'=')
    {
        return Err("ChangeSet@1 canonical patch is not canonical base64: invalid alphabet".into());
    }
    if padding > 0 && data_len == 0 {
        return Err("ChangeSet@1 canonical patch is not canonical base64: invalid padding".into());
    }
    let trailing = data_len
        .checked_sub(1)
        .and_then(|index| value(bytes[index]))
        .unwrap_or(0);
    if (padding == 1 && trailing & 0b11 != 0) || (padding == 2 && trailing & 0b1111 != 0) {
        return Err(
            "ChangeSet@1 canonical patch is not canonical base64: non-zero trailing bits".into(),
        );
    }
    Ok(())
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn valid_path(path: &str) -> bool {
    crate::is_valid_repo_path(path)
}

#[cfg(test)]
mod tests {
    use super::validate_canonical_base64;
    use base64::{Engine, engine::general_purpose::STANDARD};

    #[test]
    fn canonical_base64_is_checked_without_decoding_the_patch() {
        for valid in ["", "Zg==", "Zm8=", "Zm9v"] {
            assert!(validate_canonical_base64(valid).is_ok(), "{valid}");
        }
        for invalid in ["Z", "Zh==", "Zm9=", "Zm=v", "Zm9v="] {
            assert!(validate_canonical_base64(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn allocation_free_validation_matches_the_canonical_engine() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for sample in 0..20_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let len = (state as usize) % 96;
            let mut candidate = String::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                candidate.push(char::from((state >> 32) as u8 & 0x7f));
            }
            assert_eq!(
                validate_canonical_base64(&candidate).is_ok(),
                STANDARD.decode(&candidate).is_ok(),
                "validator drift on generated sample {sample}: {candidate:?}"
            );
        }
    }
}
