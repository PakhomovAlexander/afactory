//! The Project and its Workspaces (ADR-0151): the events a Project Store's log holds.
//!
//! A Project is the durable identity of one repository, derived from the `repository_id` every
//! Snapshot origin already carries (its sorted root commits). A Workspace is one registered
//! checkout of it. The log names a Workspace by an opaque ID and a label only: checkout, Store,
//! bindings and Task-file paths are machine-local and live in `workspaces.toml` beside the Store,
//! never in a record. The identities are derived by the `af` binary; these types fix their
//! spelling and refuse anything that could carry a host path.

use serde::{Deserialize, Serialize};

use crate::task::usage::DecimalU64;

/// Hex digits of a Workspace ID after its `w-` prefix.
pub const PROJECT_WORKSPACE_ID_HEX_LEN: usize = 16;

/// Upper bound on the `[project] name` a registration records.
pub const MAX_PROJECT_NAME_BYTES: usize = 256;

/// Upper bound on the `min_af` text a registration records.
pub const MAX_MIN_AF_BYTES: usize = 64;

/// Upper bound on a `repository_id`: 64 root commits of a SHA-256 repository.
pub const MAX_REPOSITORY_ID_BYTES: usize = 64 * 65;

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether `value` is a Project ID: `sha256:` and 64 lowercase hex digits.
pub fn is_project_id(value: &str) -> bool {
    crate::is_digest(value)
}

/// Whether `value` is a Workspace ID: `w-` and 16 lowercase hex digits.
pub fn is_project_workspace_id(value: &str) -> bool {
    value
        .strip_prefix("w-")
        .is_some_and(|hex| hex.len() == PROJECT_WORKSPACE_ID_HEX_LEN && is_lower_hex(hex))
}

/// Whether `value` is a Workspace label: the safe-name rule of Task IDs and pipeline names.
pub fn is_workspace_label(value: &str) -> bool {
    crate::task::is_name(value)
}

/// Whether `value` is a `repository_id`: sorted, distinct root commit IDs (SHA-1 or SHA-256),
/// joined by `,`.
pub fn is_repository_id(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_REPOSITORY_ID_BYTES {
        return false;
    }
    let roots: Vec<&str> = value.split(',').collect();
    roots
        .iter()
        .all(|root| matches!(root.len(), 40 | 64) && is_lower_hex(root))
        && roots.windows(2).all(|pair| pair[0] < pair[1])
}

/// How a checkout became a Workspace of its Project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOriginV1 {
    /// An existing checkout, registered lazily or by `af project workspace add`.
    Adopted,
    /// A worktree `af project workspace new` made.
    Created,
    /// The worktree `af task deliver` made, labelled by the Task ID.
    Delivered,
}

impl WorkspaceOriginV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::Created => "created",
            Self::Delivered => "delivered",
        }
    }
}

/// Payload of `ProjectRegistered@1`: the first event of a Project Store's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRegisteredPayloadV1 {
    pub project_id: String,
    pub repository_id: String,
    /// `[project] name` of the registering checkout's `.af/af.toml`, when that file parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `[project] min_af` of the same file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_af: Option<String>,
    pub registered_unix_ms: DecimalU64,
}

impl ProjectRegisteredPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_project_id(&self.project_id) {
            return Err("ProjectRegistered@1 has an invalid Project ID".into());
        }
        if !is_repository_id(&self.repository_id) {
            return Err("ProjectRegistered@1 has an invalid repository_id".into());
        }
        if let Some(name) = &self.name
            && (name.trim().is_empty()
                || name.len() > MAX_PROJECT_NAME_BYTES
                || name.chars().any(char::is_control))
        {
            return Err("ProjectRegistered@1 has an invalid project name".into());
        }
        if let Some(min_af) = &self.min_af
            && (min_af.is_empty()
                || min_af.len() > MAX_MIN_AF_BYTES
                || !min_af
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+')))
        {
            return Err("ProjectRegistered@1 has an invalid min_af".into());
        }
        Ok(())
    }
}

/// Payload of `WorkspaceRegistered@1`. Never a path: the checkout, its Store, bindings and Task
/// files are named only in the machine-local `workspaces.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRegisteredPayloadV1 {
    pub workspace_id: String,
    pub label: String,
    pub origin: WorkspaceOriginV1,
    pub registered_unix_ms: DecimalU64,
}

impl WorkspaceRegisteredPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_project_workspace_id(&self.workspace_id) {
            return Err("WorkspaceRegistered@1 has an invalid Workspace ID".into());
        }
        if !is_workspace_label(&self.label) {
            return Err("WorkspaceRegistered@1 has an invalid label".into());
        }
        Ok(())
    }
}

/// Payload of `WorkspaceRemoved@1`: the Workspace is no longer registered. Its checkout and its
/// Store are left where they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRemovedPayloadV1 {
    pub workspace_id: String,
    pub removed_unix_ms: DecimalU64,
}

impl WorkspaceRemovedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_project_workspace_id(&self.workspace_id) {
            return Err("WorkspaceRemoved@1 has an invalid Workspace ID".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_A: &str = "1111111111111111111111111111111111111111";
    const ROOT_B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn identities_have_one_spelling() {
        assert!(is_project_workspace_id("w-0123456789abcdef"));
        assert!(!is_project_workspace_id("w-0123456789ABCDEF"));
        assert!(!is_project_workspace_id("0123456789abcdef"));
        assert!(!is_project_workspace_id("w-0123"));
        assert!(is_repository_id(ROOT_A));
        assert!(is_repository_id(&format!("{ROOT_A},{ROOT_B}")));
        assert!(!is_repository_id(&format!("{ROOT_B},{ROOT_A}")), "unsorted");
        assert!(!is_repository_id(&format!("{ROOT_A},{ROOT_A}")), "repeated");
        assert!(!is_repository_id("/home/me/repo"));
        assert!(is_workspace_label("research"));
        assert!(!is_workspace_label("-research"));
        assert!(!is_workspace_label("a/b"));
    }

    #[test]
    fn payloads_refuse_paths_and_foreign_identities() {
        let registered = ProjectRegisteredPayloadV1 {
            project_id: format!("sha256:{}", "a".repeat(64)),
            repository_id: ROOT_A.into(),
            name: Some("afactory".into()),
            min_af: Some("0.12".into()),
            registered_unix_ms: 1.into(),
        };
        registered.validate().unwrap();
        for broken in [
            ProjectRegisteredPayloadV1 {
                project_id: "sha256:short".into(),
                ..registered.clone()
            },
            ProjectRegisteredPayloadV1 {
                repository_id: "/srv/repo".into(),
                ..registered.clone()
            },
            ProjectRegisteredPayloadV1 {
                name: Some(" ".into()),
                ..registered.clone()
            },
            ProjectRegisteredPayloadV1 {
                min_af: Some("0.1 /tmp".into()),
                ..registered.clone()
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
        let workspace = WorkspaceRegisteredPayloadV1 {
            workspace_id: "w-0123456789abcdef".into(),
            label: "research".into(),
            origin: WorkspaceOriginV1::Created,
            registered_unix_ms: 2.into(),
        };
        workspace.validate().unwrap();
        assert!(
            WorkspaceRegisteredPayloadV1 {
                label: "/home/me/afactory-wt-research".into(),
                ..workspace.clone()
            }
            .validate()
            .is_err()
        );
        let removed = WorkspaceRemovedPayloadV1 {
            workspace_id: "w-0123456789abcdef".into(),
            removed_unix_ms: 3.into(),
        };
        removed.validate().unwrap();
        assert!(
            WorkspaceRemovedPayloadV1 {
                workspace_id: "research".into(),
                ..removed
            }
            .validate()
            .is_err()
        );
        let text = serde_json::to_value(&workspace).unwrap();
        assert_eq!(text["registered_unix_ms"], "2");
        assert_eq!(text["origin"], "created");
    }
}
