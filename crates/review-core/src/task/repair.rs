//! Bounded current-Snapshot repair context and independent per-claim decisions.
use super::{execution::TaskInvocationV1, require, review::*};
use crate::{ChangeAttestationV1, SubjectV1, is_digest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TASK_REPAIR_CONTEXT_V1: &str = "af/TaskRepairContext@1";
pub const TASK_FIX_VERIFICATION_V1: &str = "af/TaskFixVerification@1";
pub const TASK_FIX_RECEIPT_V1: &str = "af/TaskFixReceipt@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRepairClaimV1 {
    pub original_view_id: String,
    pub current_view_id: String,
    pub title: String,
    pub body: String,
    pub remedy: String,
    pub attestation_id: String,
    pub attestation: ChangeAttestationV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRepairContextV1 {
    pub invocation: TaskInvocationV1,
    pub continuation_id: String,
    pub continuation: VerificationContinuationV1,
    pub subject: SubjectV1,
    pub previous_snapshot_id: String,
    pub claims: BTreeMap<String, TaskRepairClaimV1>,
}
impl TaskRepairContextV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        self.continuation.validate()?;
        self.subject.validate()?;
        require(
            is_digest(&self.continuation_id)
                && is_digest(&self.previous_snapshot_id)
                && self.subject.head_snapshot_id == self.continuation.current_snapshot_id
                && self.invocation.plan_id == self.continuation.plan_id
                && self.claims.len() <= 64
                && self.claims.keys().eq(self.continuation.claims.keys()),
            "Repair context requires bounded exact continuation authority",
        )?;
        for (finding, claim) in &self.claims {
            claim.attestation.validate()?;
            require(
                is_digest(&claim.original_view_id)
                    && is_digest(&claim.attestation_id)
                    && self.continuation.claims.get(finding) == Some(&claim.current_view_id)
                    && claim.attestation.finding_id == *finding
                    && claim.attestation.expected_finding_view_id == claim.current_view_id
                    && claim.attestation.subject_id == self.continuation.current_subject_id,
                "Repair context changed a preserved Finding or current attestation",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFixDecisionV1 {
    pub expected_view_id: String,
    pub attestation_id: String,
    pub outcome: VerificationOutcomeV1,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFixVerificationV1 {
    pub continuation_id: String,
    pub subject_id: String,
    pub claims: BTreeMap<String, TaskFixDecisionV1>,
}
impl TaskFixVerificationV1 {
    pub fn validate_context(&self, context: &TaskRepairContextV1) -> Result<(), String> {
        context.validate()?;
        require(
            self.continuation_id == context.continuation_id
                && self.subject_id == context.continuation.current_subject_id
                && self.claims.keys().eq(context.claims.keys()),
            "Fix verification must account for every original Finding on current S2",
        )?;
        for (finding, decision) in &self.claims {
            let claim = &context.claims[finding];
            require(
                decision.expected_view_id == claim.current_view_id
                    && decision.attestation_id == claim.attestation_id
                    && !decision.reason.trim().is_empty()
                    && decision.reason.chars().count() <= 65536,
                "Fix verification contains stale or unreasoned per-Finding evidence",
            )?;
        }
        Ok(())
    }
}

/// Inconclusive receipts record the absence of a verifier; they never impersonate an Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFixReceiptV1 {
    pub invocation: TaskInvocationV1,
    pub finding_id: String,
    pub continuation_id: String,
    pub subject_id: String,
    pub decision: TaskFixDecisionV1,
    pub verifier_output_id: Option<String>,
}
impl TaskFixReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        require(
            !self.finding_id.trim().is_empty()
                && [
                    &self.continuation_id,
                    &self.subject_id,
                    &self.decision.expected_view_id,
                    &self.decision.attestation_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                && self.verifier_output_id.as_deref().is_none_or(is_digest)
                && (self.verifier_output_id.is_some()
                    || self.decision.outcome == VerificationOutcomeV1::Inconclusive)
                && !self.decision.reason.trim().is_empty()
                && self.decision.reason.chars().count() <= 65536,
            "Fix receipt requires exact current evidence; an absent verifier is inconclusive",
        )
    }
}

/// Minimal declared repair input, containing claim content rather than inaccessible IDs only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewClaimsV1 {
    pub round_report_id: String,
    pub snapshot_id: String,
    pub claims: BTreeMap<String, TaskReviewClaimV1>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewClaimV1 {
    pub view_id: String,
    pub title: String,
    pub body: String,
    pub remedy: String,
}
pub const TASK_REVIEW_CLAIMS_V1: &str = "af/TaskReviewClaims@1";
