//! Review lineage and repair evidence are explicit Task inputs, never ambient history.

use super::{TaskAcceptanceV1, TaskExecutionV1, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use super::review_context::*;

pub const TASK_REVIEW_SUBJECT_V1: &str = "af/TaskReviewSubject@1";
pub const TASK_REVIEW_ROUND_V1: &str = "af/TaskReviewRound@1";

/// A Review invocation binds the immutable Subject separately from the execution plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewSubjectV1 {
    pub subject_id: String,
    /// Declared Worker context contains the actual scope, not inaccessible CAS pointers alone.
    pub subject: crate::SubjectV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub change_set: Option<crate::ChangeSetV1>,
    pub snapshot_id: String,
    pub prior_history_id: String,
    pub round: u32,
}
impl TaskReviewSubjectV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.subject.validate()?;
        require(
            self.subject.head_snapshot_id == self.snapshot_id,
            "Review Subject has a stale Snapshot",
        )?;
        match (&self.subject.kind, &self.change_set) {
            (crate::SubjectKind::WholeTree, None) => (),
            (crate::SubjectKind::Diff, Some(changes)) => {
                changes.validate()?;
                require(
                    changes.head_snapshot_id == self.snapshot_id
                        && self.subject.base_snapshot_id.as_ref()
                            == Some(&changes.base_snapshot_id)
                        && !changes.changed_paths.is_empty(),
                    "Review Diff requires exact nonempty scope",
                )?;
            }
            _ => return Err("Review Subject must retain its exact Change Set".into()),
        }
        require(
            [&self.subject_id, &self.snapshot_id, &self.prior_history_id]
                .into_iter()
                .all(|id| is_digest(id))
                && (1..=16).contains(&self.round),
            "Review Subject requires bounded Round and exact Subject, Snapshot and history",
        )
    }
}

/// A complete Round exposes canonical views. An incomplete gather retains selected raw
/// results for inspection but never creates a partial Ledger or claims a closed Round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewRoundV1 {
    pub invocation: super::execution::TaskInvocationV1,
    pub policy_id: String,
    pub subject_id: String,
    pub snapshot_id: String,
    pub round: u32,
    pub outcome: super::pipeline::ReceiptOutcomeV1,
    pub conclusion: ReviewConclusionV1,
    pub selected_results: BTreeMap<String, String>,
    #[serde(deserialize_with = "super::unique_set")]
    pub missing_reviewers: std::collections::BTreeSet<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub finding_set_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub demand_set_id: Option<String>,
}
impl TaskReviewRoundV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        require(
            [&self.policy_id, &self.subject_id, &self.snapshot_id]
                .into_iter()
                .all(|id| is_digest(id))
                && (1..=16).contains(&self.round)
                && self.selected_results.len() <= 64
                && self.missing_reviewers.len() <= 64
                && self
                    .selected_results
                    .iter()
                    .all(|(name, id)| super::is_name(name) && is_digest(id))
                && self
                    .missing_reviewers
                    .iter()
                    .all(|name| super::is_name(name) && !self.selected_results.contains_key(name))
                && self.finding_set_id.as_deref().is_none_or(is_digest)
                && self.demand_set_id.as_deref().is_none_or(is_digest),
            "Review Round requires exact bounded current evidence",
        )?;
        let complete = self.conclusion != ReviewConclusionV1::Incomplete;
        require(
            complete == (self.finding_set_id.is_some() && self.demand_set_id.is_some())
                && (complete || (self.finding_set_id.is_none() && self.demand_set_id.is_none()))
                && (!complete
                    || (self.missing_reviewers.is_empty() && !self.selected_results.is_empty()))
                && self.outcome
                    == match self.conclusion {
                        ReviewConclusionV1::Pass => super::pipeline::ReceiptOutcomeV1::Passed,
                        ReviewConclusionV1::ChangesRequested
                        | ReviewConclusionV1::ConvergenceExhausted => {
                            super::pipeline::ReceiptOutcomeV1::Failed
                        }
                        ReviewConclusionV1::Incomplete => {
                            super::pipeline::ReceiptOutcomeV1::Inconclusive
                        }
                    },
            "Incomplete Review cannot expose authoritative sets or a passing outcome",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewHistoryV1 {
    Empty {},
    Recorded {
        lineage_id: String,
        subject_id: String,
        finding_set_id: String,
        demand_set_id: String,
        round_report_id: String,
    },
}

impl ReviewHistoryV1 {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Empty {} => Ok(()),
            Self::Recorded {
                lineage_id,
                subject_id,
                finding_set_id,
                demand_set_id,
                round_report_id,
            } => require(
                [
                    lineage_id,
                    subject_id,
                    finding_set_id,
                    demand_set_id,
                    round_report_id,
                ]
                .into_iter()
                .all(|id| is_digest(id)),
                "Review history requires exact recorded lineage and views",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationContinuationV1 {
    pub task_revision_id: String,
    pub plan_id: String,
    pub prior_history_id: String,
    pub previous_subject_id: String,
    pub current_subject_id: String,
    pub current_snapshot_id: String,
    pub policy_id: String,
    /// Exact original claim view for each Finding being rechecked.
    pub claims: BTreeMap<String, String>,
}

impl VerificationContinuationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.task_revision_id,
                &self.plan_id,
                &self.prior_history_id,
                &self.previous_subject_id,
                &self.current_subject_id,
                &self.current_snapshot_id,
                &self.policy_id,
            ]
            .into_iter()
            .all(|id| is_digest(id)),
            "Verification continuation requires exact Task, plan, history and Subject authority",
        )?;
        require(
            self.previous_subject_id != self.current_subject_id,
            "Repair continuation requires a new Subject",
        )?;
        require(
            !self.claims.is_empty()
                && self
                    .claims
                    .iter()
                    .all(|(finding, view)| !finding.trim().is_empty() && is_digest(view)),
            "Continuation must retain the claims being verified",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcomeV1 {
    Positive,
    Negative,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairScopeV1 {
    TargetedFixes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimVerificationV1 {
    pub expected_view_id: String,
    pub attestation_id: String,
    pub receipt_id: String,
    pub outcome: VerificationOutcomeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairAssessmentV1 {
    pub continuation_id: String,
    pub current_subject_id: String,
    pub current_snapshot_id: String,
    pub check_receipt_id: String,
    /// Targeted verification cannot claim a complete review of the repaired Snapshot.
    pub scope: RepairScopeV1,
    pub claims: BTreeMap<String, ClaimVerificationV1>,
}

impl RepairAssessmentV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.continuation_id,
                &self.current_subject_id,
                &self.current_snapshot_id,
                &self.check_receipt_id,
            ]
            .into_iter()
            .all(|id| is_digest(id)),
            "Repair assessment requires current authority and Gates",
        )?;
        require(
            !self.claims.is_empty()
                && self.claims.iter().all(|(finding, claim)| {
                    !finding.trim().is_empty()
                        && [
                            &claim.expected_view_id,
                            &claim.attestation_id,
                            &claim.receipt_id,
                        ]
                        .into_iter()
                        .all(|id| is_digest(id))
                }),
            "Repair assessment requires typed per-Finding receipts",
        )
    }

    /// Reference matching only; Store admission must also validate receipt authority,
    /// Snapshot affinity, verifier independence and successful current checks.
    pub fn validate_continuation(
        &self,
        id: &str,
        continuation: &VerificationContinuationV1,
    ) -> Result<(), String> {
        self.validate()?;
        continuation.validate()?;
        require(
            self.continuation_id == id
                && self.current_subject_id == continuation.current_subject_id
                && self.current_snapshot_id == continuation.current_snapshot_id,
            "Repair assessment is stale or belongs to another continuation",
        )?;
        require(
            self.claims.keys().eq(continuation.claims.keys())
                && self.claims.iter().all(|(finding, claim)| {
                    continuation.claims.get(finding) == Some(&claim.expected_view_id)
                }),
            "Repair assessment must account for every original claim view",
        )
    }
}

/// Domain exit precedence remains separate from generic Task acceptance. Operational and
/// usage errors occur outside this recorded review outcome contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewConclusionV1 {
    Pass,
    ChangesRequested,
    ConvergenceExhausted,
    Incomplete,
}

impl ReviewConclusionV1 {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::ChangesRequested | Self::ConvergenceExhausted => 3,
            Self::Incomplete => 4,
        }
    }

    pub fn validate_result(
        self,
        execution: TaskExecutionV1,
        acceptance: TaskAcceptanceV1,
        required_nodes_complete: bool,
    ) -> Result<(), String> {
        // The adapter derives this fact from the recorded review receipts. Task acceptance
        // cannot distinguish a complete finding-bearing review from missing reviewer output.
        require(
            required_nodes_complete != (self == Self::Incomplete),
            "Missing required review output must retain the Incomplete conclusion",
        )?;
        match self {
            Self::Pass => require(
                execution == TaskExecutionV1::Completed
                    && acceptance == TaskAcceptanceV1::Satisfied,
                "Pass requires a complete accepted review",
            ),
            Self::ChangesRequested => require(
                execution == TaskExecutionV1::Completed,
                "Changes requested requires a complete review",
            ),
            Self::ConvergenceExhausted => require(
                matches!(
                    execution,
                    TaskExecutionV1::Completed | TaskExecutionV1::Exhausted
                ) && (execution == TaskExecutionV1::Completed
                    || acceptance != TaskAcceptanceV1::Satisfied),
                "Satisfied acceptance requires completed Task execution even when review convergence is exhausted",
            ),
            Self::Incomplete => require(
                execution != TaskExecutionV1::Completed
                    && acceptance != TaskAcceptanceV1::Satisfied,
                "Missing required review output cannot complete acceptance",
            ),
        }
    }
}
