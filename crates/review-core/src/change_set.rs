//! The immutable, content-addressed description of one diff Subject.

use std::collections::BTreeSet;

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
            canonical_patch_base64: STANDARD.encode(canonical_patch),
            git_version: git_version.into(),
            diff_policy_version: diff_policy_version.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn canonical_patch(&self) -> Result<Vec<u8>, String> {
        let decoded = STANDARD
            .decode(&self.canonical_patch_base64)
            .map_err(|error| format!("ChangeSet@1 canonical patch is not base64: {error}"))?;
        if STANDARD.encode(&decoded) != self.canonical_patch_base64 {
            return Err("ChangeSet@1 canonical patch is not canonically encoded".into());
        }
        Ok(decoded)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.base_snapshot_id)
            || !crate::is_digest(&self.head_snapshot_id)
        {
            return Err("ChangeSet@1 has an invalid Base or head Snapshot ID".into());
        }
        if self.git_version.trim().is_empty() || self.diff_policy_version.trim().is_empty() {
            return Err("ChangeSet@1 requires Git build and diff-policy versions".into());
        }
        if self
            .changed_paths
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
            || self.changed_paths.iter().any(|path| !valid_path(path))
        {
            return Err("ChangeSet@1 changed paths must be sorted, unique, and relative".into());
        }
        if self.renames.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("ChangeSet@1 renames must be sorted and unique".into());
        }
        let paths: BTreeSet<&str> = self.changed_paths.iter().map(String::as_str).collect();
        for rename in &self.renames {
            if rename.similarity > 100
                || rename.old_path == rename.new_path
                || !valid_path(&rename.old_path)
                || !valid_path(&rename.new_path)
                || !paths.contains(rename.old_path.as_str())
                || !paths.contains(rename.new_path.as_str())
            {
                return Err(
                    "ChangeSet@1 rename paths must be changed, distinct, relative, and at most 100% similar"
                        .into(),
                );
            }
        }
        self.canonical_patch().map(|_| ())
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."))
}
