//! `SourceSnapshot@1` — identification of source content, never of a branch.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vcs {
    Git,
}

/// The atomic read boundary that admitted a dirty tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirtyBoundary {
    /// An atomic filesystem snapshot.
    FilesystemSnapshot,
    /// Monitoring started before the first read; two complete manifests and the index digest
    /// matched with no intervening or overflowed change event.
    Revalidated,
}

/// How content was admitted.
///
/// A best-effort copy is deliberately not a variant. It may serve as untrusted diagnostic input,
/// but it can never produce a target eligible for convergence or automatic integration, so it
/// must not be expressible as a capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Capture {
    Committed {
        tree_id: String,
    },
    SyntheticWorktree {
        tree_id: String,
        boundary: DirtyBoundary,
        /// Capture attempts consumed before admission. Bounded — exhausting the bound fails
        /// closed rather than admitting a torn tree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempts: Option<u32>,
    },
    /// Produced by integrating a validated patch. Never a mutation of the parent.
    Derived {
        tree_id: String,
        parent_snapshot_id: String,
        integration_batch_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submodule {
    pub path: String,
    pub revision: String,
    /// Implicit recursion is disabled during capture, so inclusion is always an explicit
    /// policy outcome rather than a side effect of the host's configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included: Option<bool>,
}

/// The payload of a `review.kernel/SourceSnapshot@1` artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshot {
    pub repository_id: String,
    pub vcs: Vcs,
    pub capture: Capture,
    /// Digest over the canonical ordered manifest. Equal content yields an equal digest
    /// regardless of host or repository configuration — that is the property the sanitized
    /// capture path exists to guarantee.
    pub content_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_snapshot_id: Option<String>,
    /// The upstream revision this content corresponds to, when one exists. Provenance, not
    /// identity: a synthetic worktree capture has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_manifest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submodules: Vec<Submodule>,
}

impl SourceSnapshot {
    /// Whether this snapshot was produced by integrating a patch rather than captured from
    /// source. Every variant here is admissible for convergence by construction — a best-effort
    /// copy is not expressible as a [`Capture`], which is the point.
    pub fn is_derived(&self) -> bool {
        matches!(self.capture, Capture::Derived { .. })
    }
}
