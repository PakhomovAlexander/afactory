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

/// Length of a Warm Workspace identity: the lowercase hex prefix of a domain-separated digest.
pub const WARM_WORKSPACE_ID_HEX_LEN: usize = 32;

/// Whether `value` is a Warm Workspace identity: exactly [`WARM_WORKSPACE_ID_HEX_LEN`] lowercase hex digits.
pub fn is_warm_workspace_id(value: &str) -> bool {
    value.len() == WARM_WORKSPACE_ID_HEX_LEN
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
