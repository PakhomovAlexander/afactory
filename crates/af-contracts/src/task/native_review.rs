//! Native proposal preparation and promotion. These receipts never name a Campaign log.
use super::{execution::TaskInvocationV1, pipeline::ReceiptOutcomeV1, require};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const TASK_REVIEW_PROPOSAL_V1: &str = "af/TaskReviewProposal@1";
pub const TASK_REVIEW_INTEGRATION_V1: &str = "af/TaskReviewIntegration@1";
pub const TASK_REVIEW_COMMIT_V1: &str = "af/TaskReviewCommit@1";

/// A declaration beside ReviewerResult@2. The kernel captures the candidate itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewProposalV1 {
    pub result_id: String,
    pub paths: Vec<String>,
    pub report_indices: Vec<u32>,
    pub evidence_ids: Vec<String>,
    pub auto_apply_nominated: bool,
}
impl TaskReviewProposalV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            crate::is_digest(&self.result_id)
                && !self.paths.is_empty()
                && self.paths.len() <= 4096
                && self.paths.iter().all(|p| crate::is_valid_repo_path(p))
                && self.paths.windows(2).all(|p| p[0] < p[1])
                && !self.report_indices.is_empty()
                && self.report_indices.len() <= 4096
                && self.report_indices.windows(2).all(|p| p[0] < p[1])
                && self.evidence_ids.len() <= 64
                && self.evidence_ids.iter().all(|id| crate::is_digest(id))
                && self.evidence_ids.iter().collect::<BTreeSet<_>>().len()
                    == self.evidence_ids.len(),
            "Proposal requires complete sorted paths, unique report indices and durable evidence",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewSelectedProposalV1 {
    pub result_id: String,
    pub declaration_id: String,
    pub candidate_id: String,
    /// Original canonical claim to the exact view this Proposal repairs.
    pub claims: BTreeMap<String, String>,
}
impl TaskReviewSelectedProposalV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [&self.result_id, &self.declaration_id, &self.candidate_id]
                .into_iter()
                .all(|id| crate::is_digest(id))
                && !self.claims.is_empty()
                && self.claims.len() <= 4096
                && self
                    .claims
                    .iter()
                    .all(|(id, view)| crate::is_digest(id) && crate::is_digest(view)),
            "Selected Proposal requires exact result, sealed candidate and original claim views",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReviewIntegrationSelection {
    Empty {},
    Conflict {
        paths: Vec<String>,
        reason: String,
    },
    Prepared {
        snapshot_id: String,
        manifest_id: String,
        proposals: BTreeMap<String, TaskReviewSelectedProposalV1>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewIntegrationV1 {
    pub invocation: TaskInvocationV1,
    pub policy_id: String,
    pub review_id: String,
    pub base_snapshot_id: String,
    pub outcome: ReceiptOutcomeV1,
    pub selection: TaskReviewIntegrationSelection,
}
impl TaskReviewIntegrationV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        require(
            [&self.policy_id, &self.review_id, &self.base_snapshot_id]
                .into_iter()
                .all(|id| crate::is_digest(id)),
            "Integration requires exact native Round and source authority",
        )?;
        match &self.selection {
            TaskReviewIntegrationSelection::Empty {} => require(
                self.outcome == ReceiptOutcomeV1::Inconclusive,
                "Empty Integration has no promotion authority",
            ),
            TaskReviewIntegrationSelection::Conflict { paths, reason } => require(
                self.outcome == ReceiptOutcomeV1::Failed
                    && !paths.is_empty()
                    && paths.len() <= 4096
                    && paths.iter().all(|p| crate::is_valid_repo_path(p))
                    && paths.windows(2).all(|p| p[0] < p[1])
                    && !reason.trim().is_empty()
                    && reason.len() <= 4096,
                "Integration conflict requires complete exact paths and no promotion",
            ),
            TaskReviewIntegrationSelection::Prepared {
                snapshot_id,
                manifest_id,
                proposals,
            } => {
                require(
                    self.outcome == ReceiptOutcomeV1::Passed
                        && crate::is_digest(snapshot_id)
                        && crate::is_digest(manifest_id)
                        && snapshot_id != &self.base_snapshot_id
                        && !proposals.is_empty()
                        && proposals.len() <= 64
                        && proposals.values().map(|p| p.claims.len()).sum::<usize>() <= 4096
                        && proposals.keys().all(|s| super::is_name(s)),
                    "Prepared Integration requires a derived source and bounded selections",
                )?;
                for proposal in proposals.values() {
                    proposal.validate()?;
                }
                Ok(())
            }
        }
    }
}

/// Publication of this one receipt and its output is the promotion boundary. Claims remain
/// pending until a later complete discovery Round verifies the new Subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewCommitV1 {
    pub invocation: TaskInvocationV1,
    pub integration_id: String,
    pub snapshot_id: String,
    pub prior_history_id: String,
    pub pending_claims: BTreeMap<String, String>,
    pub outcome: ReceiptOutcomeV1,
}
impl TaskReviewCommitV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        require(
            [
                &self.integration_id,
                &self.snapshot_id,
                &self.prior_history_id,
            ]
            .into_iter()
            .all(|id| crate::is_digest(id))
                && self.pending_claims.len() <= 4096
                && self
                    .pending_claims
                    .iter()
                    .all(|(id, view)| crate::is_digest(id) && crate::is_digest(view))
                && (self.outcome != ReceiptOutcomeV1::Passed || !self.pending_claims.is_empty()),
            "Integration commit requires exact checked source, history and pending original claims",
        )
    }
}
