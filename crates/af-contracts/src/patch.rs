//! `PatchProposal@1` — an atomic proposed change set.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
        if !crate::is_digest(&self.base_snapshot_id)
            || !crate::is_digest(&self.patch_artifact_id)
            || self.evidence_ids.iter().any(|id| !crate::is_digest(id))
        {
            return Err("proposal contains an invalid artifact ID");
        }
        if self.description.trim().is_empty() {
            return Err("proposal has an empty description");
        }
        if self
            .finding_refs
            .iter()
            .any(|claim| claim.id.trim().is_empty())
        {
            return Err("proposal contains an empty claim ID");
        }
        let mut paths = BTreeSet::new();
        if self
            .paths
            .iter()
            .any(|path| !crate::is_valid_repo_path(path) || !paths.insert(path))
        {
            return Err("proposal paths are invalid or repeated");
        }
        Ok(())
    }
}

/// Durable pre-reduction state for one selected Attempt's verified declaration. Report indexes
/// are deliberately unresolved here; only the canonical Ledger barrier can replace them with
/// typed Report IDs and publish a `PatchProposal@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalCandidateV1 {
    pub base_snapshot_id: String,
    pub patch_artifact_id: String,
    /// Complete sealed sandbox Manifest whose diff from Base was proved equal to the patch.
    pub derived_manifest_artifact_id: String,
    pub result_artifact_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub report_indexes: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finding_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    pub paths: Vec<String>,
    pub description: String,
    #[serde(default)]
    pub auto_apply_nominated: bool,
}

impl ProposalCandidateV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::is_digest(&self.base_snapshot_id)
            || !crate::is_digest(&self.patch_artifact_id)
            || !crate::is_digest(&self.derived_manifest_artifact_id)
            || !crate::is_digest(&self.result_artifact_id)
            || self.finding_ids.iter().any(|id| !crate::is_digest(id))
            || self.evidence_ids.iter().any(|id| !crate::is_digest(id))
        {
            return Err("proposal candidate contains an invalid artifact ID");
        }
        if self.report_indexes.is_empty() && self.finding_ids.is_empty() {
            return Err("proposal candidate names no claim");
        }
        if self.description.trim().is_empty() {
            return Err("proposal candidate has an empty description");
        }
        let mut indexes = BTreeSet::new();
        if self
            .report_indexes
            .iter()
            .any(|index| !indexes.insert(index))
        {
            return Err("proposal candidate repeats a Report index");
        }
        let mut finding_ids = BTreeSet::new();
        if self.finding_ids.iter().any(|id| !finding_ids.insert(id)) {
            return Err("proposal candidate repeats a Finding ID");
        }
        let mut evidence_ids = BTreeSet::new();
        if self.evidence_ids.iter().any(|id| !evidence_ids.insert(id)) {
            return Err("proposal candidate repeats an Evidence ID");
        }
        let mut paths = BTreeSet::new();
        if self.paths.is_empty()
            || self
                .paths
                .iter()
                .any(|path| !crate::is_valid_repo_path(path) || !paths.insert(path))
        {
            return Err("proposal candidate paths are empty, invalid, or repeated");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalRefusalReasonV1 {
    MalformedDeclaration,
    InvalidClaim,
    InvalidEvidence,
    InvalidPath,
    EmptyMutation,
    PatchMismatch,
    PathMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalPreparedPayloadV1 {
    pub candidate_artifact_id: String,
    pub result_artifact_id: String,
}

impl ProposalPreparedPayloadV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::is_digest(&self.candidate_artifact_id)
            || !crate::is_digest(&self.result_artifact_id)
        {
            return Err("prepared Proposal contains an invalid artifact ID");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalRefusedPayloadV1 {
    pub reason: ProposalRefusalReasonV1,
    pub result_artifact_id: String,
}

impl ProposalRefusedPayloadV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::is_digest(&self.result_artifact_id) {
            return Err("refused Proposal contains an invalid result artifact ID");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalAcceptedPayloadV1 {
    /// Semantic, domain-separated ID of the `PatchProposal@1` envelope.
    pub proposal_id: String,
    /// CAS record holding that envelope.
    pub proposal_artifact_id: String,
    /// Durable prepared declaration finalized by this event; used for idempotent replay.
    pub candidate_artifact_id: String,
}

impl ProposalAcceptedPayloadV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::is_digest(&self.proposal_id)
            || !crate::is_digest(&self.proposal_artifact_id)
            || !crate::is_digest(&self.candidate_artifact_id)
        {
            return Err("accepted Proposal contains an invalid artifact ID");
        }
        Ok(())
    }
}
