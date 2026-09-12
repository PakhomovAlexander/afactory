//! Typed compatibility receipts preserve canonical Review artifacts beside Task output ports.
//! They carry evidence identities, never dispatch or selected-result authority.

use super::require;
use crate::{ProposalRefusalReasonV1, ReviewerResultContract, is_digest};
use serde::{Deserialize, Serialize};

pub const TASK_REVIEW_RESULT_METADATA_V1: &str = "af/TaskReviewResultMetadata@1";
pub const TASK_REVIEW_CONTEXT_V1: &str = "af/TaskReviewContext@1";

/// Exact context captured by the compatibility host after the common Attempt reservation.
/// The host must recompute the declared inputs and rendered bytes during context admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewContextV1 {
    pub campaign_id: String,
    pub round_event_id: String,
    pub invocation_event_id: String,
    pub review_node: String,
    pub subject_id: String,
    pub campaign_manifest_id: String,
    pub task_invocation_id: String,
    pub attempt_id: String,
    pub reviewer_inputs_id: String,
    pub rendered_input_id: String,
    pub context_manifest_id: String,
}

impl TaskReviewContextV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        vec![
            &self.subject_id,
            &self.campaign_manifest_id,
            &self.task_invocation_id,
            &self.reviewer_inputs_id,
            &self.rendered_input_id,
            &self.context_manifest_id,
        ]
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            !self.campaign_id.is_empty() && self.campaign_id.chars().count() <= 256,
            "Review context needs a bounded Campaign identity",
        )?;
        require(
            !self.review_node.is_empty() && self.review_node.chars().count() <= 256,
            "Review context needs a bounded node identity",
        )?;
        require(
            self.attempt_id.len() == 26
                && self
                    .attempt_id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "Review context needs the actual Attempt identity",
        )?;
        require(
            [&self.round_event_id, &self.invocation_event_id]
                .into_iter()
                .all(|id| {
                    id.len() == 26
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                }),
            "Review context needs exact Round and invocation event identities",
        )?;
        require(
            self.artifact_refs().into_iter().all(is_digest),
            "Review context needs exact input identities",
        )
    }
}

/// A Review projection of a selected and published common Task output. Only the Store's
/// trusted Task publication entry point can append this event; its JSON grants no authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewResultSelectedV1 {
    pub task_id: String,
    pub task_revision_id: String,
    pub plan_id: String,
    pub task_node: String,
    pub invocation_id: String,
    pub output_id: String,
    pub context_id: String,
    pub result_envelope_id: String,
    pub metadata_envelope_id: String,
    pub result_artifact_id: String,
    pub provenance_artifact_id: String,
}

impl TaskReviewResultSelectedV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        vec![
            &self.task_revision_id,
            &self.plan_id,
            &self.invocation_id,
            &self.output_id,
            &self.context_id,
            &self.result_envelope_id,
            &self.metadata_envelope_id,
            &self.result_artifact_id,
            &self.provenance_artifact_id,
        ]
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            super::is_name(&self.task_id) && self.task_node.split('.').all(super::is_name),
            "Review selection needs an exact Task and qualified node",
        )?;
        require(
            self.artifact_refs().into_iter().all(is_digest),
            "Review selection needs exact execution and result identities",
        )
    }
}

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
