//! Worker warm layers, package P3: the Warm Workspace.
//!
//! Each node of a Campaign owns one stable template root under the machine's cache directory.
//! When the head advances, the kernel re-bases that template by applying the tree diff to a
//! copy-on-write clone of the previous template and verifying that the result's manifest
//! digest equals the new head's Tree Digest; any other outcome falls back to a full
//! materialization and records why. Per-Attempt sandboxes remain fresh clones of the template,
//! so sibling isolation is unchanged. The workspace is identified by an opaque, domain-separated
//! identity, never by a host path, so nothing machine-local enters a durable record.

use serde::{Deserialize, Serialize};

/// Length of a workspace identity: the lowercase hex prefix of a domain-separated digest.
pub const WORKSPACE_ID_HEX_LEN: usize = 32;

/// Whether `value` is a workspace identity: exactly [`WORKSPACE_ID_HEX_LEN`] lowercase hex digits.
pub fn is_workspace_id(value: &str) -> bool {
    value.len() == WORKSPACE_ID_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// How the node's template came to hold the Round's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceBasisV1 {
    /// The head was materialized from the CAS in full; nothing was carried.
    Full,
    /// The previous template received the tree diff and verified against the head's Tree
    /// Digest.
    Rebased,
    /// The previous template already held the head's exact tree; nothing was materialized.
    Reused,
}

impl WorkspaceBasisV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Rebased => "rebased",
            Self::Reused => "reused",
        }
    }

    /// Whether the previous template contributed to this Round's workspace.
    pub const fn carried(self) -> bool {
        !matches!(self, Self::Full)
    }
}

/// Why a Round's workspace was materialized in full instead of re-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFallbackReasonV1 {
    /// No verified template existed at the root: the node's first Round, or an earlier
    /// preparation that ended before its verification marker was written.
    NoVerifiedTemplate,
    /// The root held a marker that contradicts its manifest or names a missing tree.
    TemplateCorrupt,
    /// The tree diff could not be applied to the clone of the previous template.
    ApplyFailed,
    /// The re-based tree's manifest digest differed from the head's Tree Digest.
    DigestMismatch,
    /// The root held a marker the Campaign log never recorded: a preparation that ended after
    /// its swap but before its `WorkspaceRebased@1` became durable, or a marker written by
    /// hand. Machine-local state supplies no lineage, so the head is rebuilt.
    UnrecordedPreparation,
}

/// Payload of `WorkspaceRebased@1`, appended once per node per kernel run before the node's
/// Warm Set is recorded: which head the template held, which head it holds now, on what basis,
/// the digest the result was verified against, and how many entries the preparation touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRebasedPayloadV1 {
    pub node: String,
    pub workspace_id: String,
    /// The head Snapshot the previous verified template held, when one existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_id: Option<String>,
    pub to_snapshot_id: String,
    pub basis: WorkspaceBasisV1,
    /// Present exactly when `basis` is `full`: why the template was not re-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<WorkspaceFallbackReasonV1>,
    /// The Tree Digest the resulting template was verified to hold: the head's own digest.
    pub verified_digest: String,
    /// Entries written or removed: every entry for a full materialization, the diff's path set
    /// for a rebase, zero for a reused template.
    pub entries_touched: u64,
    /// Host-observed time the preparation took, whatever its basis. Preparation happens before
    /// the Round's first Attempt is reserved, so no Attempt wall clock includes it.
    pub preparation_ms: u64,
}

impl WorkspaceRebasedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() {
            return Err("WorkspaceRebased@1 has an empty node".into());
        }
        if !is_workspace_id(&self.workspace_id) {
            return Err("WorkspaceRebased@1 has an invalid workspace identity".into());
        }
        if !crate::is_digest(&self.to_snapshot_id) || !crate::is_digest(&self.verified_digest) {
            return Err("WorkspaceRebased@1 has an invalid head Snapshot ID or digest".into());
        }
        if !self
            .from_snapshot_id
            .as_deref()
            .is_none_or(crate::is_digest)
        {
            return Err("WorkspaceRebased@1 has an invalid previous Snapshot ID".into());
        }
        match self.basis {
            WorkspaceBasisV1::Full => {
                if self.fallback.is_none() {
                    return Err("WorkspaceRebased@1 full materialization records no reason".into());
                }
            }
            WorkspaceBasisV1::Rebased => {
                if self.fallback.is_some() || self.from_snapshot_id.is_none() {
                    return Err(
                        "WorkspaceRebased@1 rebase needs a previous head and no fallback".into(),
                    );
                }
            }
            WorkspaceBasisV1::Reused => {
                if self.fallback.is_some()
                    || self.from_snapshot_id.is_none()
                    || self.entries_touched != 0
                {
                    return Err(
                        "WorkspaceRebased@1 reuse touches nothing and needs a previous head".into(),
                    );
                }
            }
        }
        if self.entries_touched > crate::json::SAFE_INTEGER_MAX as u64
            || self.preparation_ms > crate::json::SAFE_INTEGER_MAX as u64
        {
            return Err("WorkspaceRebased@1 counts exceed the JSON safe-integer bound".into());
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

    fn rebased() -> WorkspaceRebasedPayloadV1 {
        WorkspaceRebasedPayloadV1 {
            node: "correctness".into(),
            workspace_id: "a".repeat(32),
            from_snapshot_id: Some(digest('1')),
            to_snapshot_id: digest('2'),
            basis: WorkspaceBasisV1::Rebased,
            fallback: None,
            verified_digest: digest('3'),
            entries_touched: 4,
            preparation_ms: 12,
        }
    }

    #[test]
    fn a_workspace_identity_is_thirty_two_lowercase_hex_digits() {
        assert!(is_workspace_id(&"0".repeat(32)));
        assert!(is_workspace_id(&"f".repeat(32)));
        assert!(!is_workspace_id(&"F".repeat(32)));
        assert!(!is_workspace_id(&"a".repeat(31)));
        assert!(!is_workspace_id(&"g".repeat(32)));
        assert!(!is_workspace_id(""));
    }

    #[test]
    fn a_full_materialization_records_why_and_a_rebase_names_its_previous_head() {
        rebased().validate().unwrap();
        let full = WorkspaceRebasedPayloadV1 {
            basis: WorkspaceBasisV1::Full,
            fallback: Some(WorkspaceFallbackReasonV1::DigestMismatch),
            ..rebased()
        };
        full.validate().unwrap();
        let first = WorkspaceRebasedPayloadV1 {
            from_snapshot_id: None,
            fallback: Some(WorkspaceFallbackReasonV1::NoVerifiedTemplate),
            ..full.clone()
        };
        first.validate().unwrap();
        let silent = WorkspaceRebasedPayloadV1 {
            fallback: None,
            ..full
        };
        assert!(silent.validate().is_err(), "a fallback records its reason");
        let orphan = WorkspaceRebasedPayloadV1 {
            from_snapshot_id: None,
            ..rebased()
        };
        assert!(
            orphan.validate().is_err(),
            "a rebase names what it re-based"
        );
        let excused = WorkspaceRebasedPayloadV1 {
            fallback: Some(WorkspaceFallbackReasonV1::ApplyFailed),
            ..rebased()
        };
        assert!(excused.validate().is_err());
    }

    #[test]
    fn a_reused_template_touches_nothing() {
        let reused = WorkspaceRebasedPayloadV1 {
            basis: WorkspaceBasisV1::Reused,
            entries_touched: 0,
            ..rebased()
        };
        reused.validate().unwrap();
        assert!(reused.basis.carried());
        assert!(!WorkspaceBasisV1::Full.carried());
        let touched = WorkspaceRebasedPayloadV1 {
            entries_touched: 1,
            ..reused
        };
        assert!(touched.validate().is_err());
        let bad_id = WorkspaceRebasedPayloadV1 {
            workspace_id: "/tmp/workspace".into(),
            ..rebased()
        };
        assert!(
            bad_id.validate().is_err(),
            "a host path is never an identity"
        );
    }
}
