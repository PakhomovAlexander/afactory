//! Typed compatibility receipts preserve canonical Review artifacts beside Task output ports.
//! They carry evidence identities, never dispatch or selected-result authority.

use super::require;
use crate::{ProposalRefusalReasonV1, ReviewerResultContract, is_digest};
use serde::{Deserialize, Serialize};

pub const TASK_REVIEW_RESULT_METADATA_V1: &str = "af/TaskReviewResultMetadata@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReviewProposalV1 {
    None {},
    Prepared { candidate_artifact_id: String },
    Refused { reason: ProposalRefusalReasonV1 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewResultMetadataV1 {
    pub result_contract: ReviewerResultContract,
    /// Canonical flat Reviewer Result, retained without replacing its historical identity.
    pub result_artifact_id: String,
    pub provenance_artifact_id: String,
    /// A sealed candidate or explicit refusal. Canonical publication still requires the
    /// common Task's selected, published output and exact Round authority.
    pub proposal: TaskReviewProposalV1,
}

impl TaskReviewResultMetadataV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        let mut ids = vec![
            self.result_artifact_id.as_str(),
            self.provenance_artifact_id.as_str(),
        ];
        if let TaskReviewProposalV1::Prepared {
            candidate_artifact_id,
        } = &self.proposal
        {
            ids.push(candidate_artifact_id.as_str());
        }
        ids
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.artifact_refs().into_iter().all(is_digest),
            "Review metadata requires exact artifact identities",
        )
    }
}
