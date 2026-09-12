//! Typed compatibility receipts preserve canonical Review artifacts beside Task output ports.
//! They carry evidence identities, never dispatch or selected-result authority.

use super::require;
use crate::{ProposalRefusalReasonV1, ReviewerResultContract, is_digest};
use serde::{Deserialize, Serialize};

pub const TASK_REVIEW_RESULT_METADATA_V1: &str = "af/TaskReviewResultMetadata@1";
pub const TASK_REVIEW_CONTEXT_V1: &str = "af/TaskReviewContext@1";
pub const LEGACY_REVIEW_ROUND_V1: &str = "af/LegacyReviewRound@1";
pub const LEGACY_REVIEW_GATE_OUTCOME_V1: &str = "af/LegacyReviewGateOutcome@1";
pub const TASK_REVIEW_GATE_FACTS_V1: &str = "af/TaskReviewGateFacts@1";
pub const TASK_REVIEW_ATTEMPT_PROVENANCE_V1: &str = "af/TaskReviewAttemptProvenance@1";

/// Transport observations bound to a real common Attempt; settlement remains charge authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewAttemptProvenanceV1 {
    pub context_id: String,
    pub task_invocation_id: String,
    pub attempt_id: String,
    pub review_node: String,
    pub result_artifact_id: String,
    pub mutations_artifact_id: String,
    pub raw_artifact_id: String,
    pub charged_tokens: super::usage::DecimalU64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub usage_id: Option<String>,
}

impl TaskReviewAttemptProvenanceV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        let mut refs = vec![
            self.context_id.as_str(),
            &self.task_invocation_id,
            &self.result_artifact_id,
            &self.mutations_artifact_id,
            &self.raw_artifact_id,
        ];
        refs.extend(self.usage_id.as_deref());
        refs
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.artifact_refs().into_iter().all(crate::is_digest)
                && event_id(&self.attempt_id)
                && !self.review_node.trim().is_empty()
                && self.review_node.chars().count() <= 256,
            "Task Review provenance requires exact captured identities",
        )
    }
}

/// Gate setup observations retained by the common settlement, including failed Attempts.
/// Machine-local paths and credentials never enter these portable facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewGateFactsV1 {
    pub round_event_id: String,
    pub review_node: String,
    pub attempt_id: String,
    pub cache_failures: Vec<crate::RunCacheFailureV5>,
}
impl TaskReviewGateFactsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            event_id(&self.round_event_id)
                && event_id(&self.attempt_id)
                && !self.review_node.trim().is_empty()
                && self.review_node.chars().count() <= 256,
            "Review Gate facts require their exact Round, node and Attempt",
        )?;
        // Cargo is currently the one supported cache kind.
        require(
            self.cache_failures.len() <= 1,
            "Duplicate Review Gate cache failure",
        )?;
        for failure in &self.cache_failures {
            failure.validate()?;
            require(
                failure.node == self.review_node,
                "Review Gate fact names another node",
            )?;
        }
        Ok(())
    }
}

/// Input binding for the installed legacy frontend. Unlike TaskReviewRound@1 (a completed
/// canonical reduction), this names one exact Campaign Round awaiting execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyReviewRoundV1 {
    pub campaign_id: String,
    pub round_event_id: String,
    pub campaign_manifest_id: String,
    pub subject_id: String,
    pub head_snapshot_id: String,
    pub round: u32,
    pub epoch: u32,
}

impl LegacyReviewRoundV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        vec![
            &self.campaign_manifest_id,
            &self.subject_id,
            &self.head_snapshot_id,
        ]
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            !self.campaign_id.is_empty() && self.campaign_id.chars().count() <= 256,
            "Legacy Review Round needs a bounded Campaign identity",
        )?;
        require(
            event_id(&self.round_event_id) && self.round > 0 && self.epoch > 0,
            "Legacy Review Round needs an exact event, Round and epoch",
        )?;
        require(
            self.artifact_refs().into_iter().all(is_digest),
            "Legacy Review Round needs exact captured authority and Subject identities",
        )
    }
}

/// A branch receipt beside the unchanged raw GateDecision@1. Setup failure produces no
/// receipt. The host validates `outcome` against that exact decision's executed result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyReviewGateOutcomeV1 {
    pub round_event_id: String,
    pub review_node: String,
    pub gate_decision_id: String,
    pub outcome: super::pipeline::ReceiptOutcomeV1,
}

impl LegacyReviewGateOutcomeV1 {
    pub fn validate(&self) -> Result<(), String> {
        use super::pipeline::ReceiptOutcomeV1;
        require(
            event_id(&self.round_event_id)
                && !self.review_node.is_empty()
                && self.review_node.chars().count() <= 256
                && is_digest(&self.gate_decision_id),
            "Legacy Review Gate outcome needs exact Round, node and decision identities",
        )?;
        require(
            matches!(
                self.outcome,
                ReceiptOutcomeV1::Passed | ReceiptOutcomeV1::Failed
            ),
            "Only an executed Gate decision supplies a Review branch outcome",
        )
    }
}

fn event_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

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
