//! `PatchProposal@1` — an atomic proposed change set.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimRefKind {
    Finding,
    Report,
}

/// A claim this patch covers. A same-attempt proposal cannot know the canonical Finding ID
/// assigned after reduction, so a Report ID from the same selected attempt is accepted and
/// mapped deterministically at the later ledger barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRef {
    pub kind: ClaimRefKind,
    pub id: String,
}

/// The payload of a `review.kernel/PatchProposal@1` artifact.
///
/// Atomic by construction: there is no per-hunk selection, because a partially applied fix is a
/// change nobody reviewed. A proposal never resolves a Finding on its own — only positive
/// derived-snapshot verification does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchProposal {
    pub base_snapshot_id: String,
    pub patch_artifact_id: String,
    pub finding_refs: Vec<ClaimRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    /// Declared path set. Validation requires it to equal the paths the patch actually changes.
    pub paths: Vec<String>,
    pub description: String,
    /// A request, not a grant: the Reviewer Binding's patch policy decides. A reviewer cannot
    /// infer eligibility from its own severity or confidence.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_apply_nominated: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl PatchProposal {
    /// Structural preconditions checkable without a repository: a proposal must name at least
    /// one claim and one path. The full validation order — patch parses, changes only its
    /// declared paths, satisfies protected-path rules, references resolve through the selected
    /// FindingSet — needs the store and the source adapter.
    pub fn check_shape(&self) -> Result<(), &'static str> {
        if self.finding_refs.is_empty() {
            return Err("proposal names no claim: a patch that fixes nothing cannot be verified");
        }
        if self.paths.is_empty() {
            return Err("proposal declares no paths");
        }
        Ok(())
    }
}
