//! Current-Snapshot check/evaluation receipts. Negative evidence is a typed result, distinct
//! from a missing result or exhausted execution. Assembly preserves all exact input receipts.

use super::pipeline::ReceiptOutcomeV1;
use super::{is_name, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TASK_CHECK_RECEIPT_V1: &str = "af/TaskCheckReceipt@1";
pub const TASK_EVALUATION_V1: &str = "af/TaskEvaluation@1";
pub const VERIFICATION_RESULT_V1: &str = "af/VerificationResult@1";
pub const REVIEWED_IMPLEMENTATION_V1: &str = "af/ReviewedImplementation@1";

/// Retains the exact acceptance invocation, including the child Review and exposed checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedImplementationV1 {
    pub invocation: super::execution::TaskInvocationV1,
    pub snapshot_id: String,
    pub policy_id: String,
    pub outcome: ReceiptOutcomeV1,
}
impl ReviewedImplementationV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        require(
            is_digest(&self.snapshot_id) && is_digest(&self.policy_id),
            "Reviewed implementation needs exact Snapshot and Review policy",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckReceiptV1 {
    pub plan_id: String,
    pub snapshot_id: String,
    pub policy_id: String,
    pub outcome: ReceiptOutcomeV1,
    pub checks: BTreeMap<String, String>,
}
impl TaskCheckReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [&self.plan_id, &self.snapshot_id, &self.policy_id]
                .into_iter()
                .all(|id| is_digest(id))
                && !self.checks.is_empty()
                && self
                    .checks
                    .iter()
                    .all(|(name, id)| is_name(name) && is_digest(id)),
            "Check receipt requires exact plan, Snapshot, policy and named results",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEvaluationV1 {
    pub outcome: ReceiptOutcomeV1,
    pub reason: String,
}
impl TaskEvaluationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            !self.reason.trim().is_empty() && self.reason.chars().count() <= 65536,
            "Evaluation requires bounded nonempty evidence reasoning",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationResultV1 {
    pub plan_id: String,
    pub snapshot_id: String,
    pub policy_id: String,
    pub outcome: ReceiptOutcomeV1,
    pub check_receipt_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub evaluation_id: Option<String>,
}
impl VerificationResultV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.plan_id,
                &self.snapshot_id,
                &self.policy_id,
                &self.check_receipt_id,
            ]
            .into_iter()
            .all(|id| is_digest(id))
                && self.evaluation_id.as_deref().is_none_or(is_digest)
                && (self.outcome != ReceiptOutcomeV1::Passed || self.evaluation_id.is_some()),
            "Verification requires exact current evidence; positive acceptance needs an evaluator",
        )
    }
}
