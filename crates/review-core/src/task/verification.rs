//! Current-Snapshot check/evaluation receipts. Negative evidence is a typed result, distinct
//! from a missing result or exhausted execution. Assembly preserves all exact input receipts.

use super::pipeline::ReceiptOutcomeV1;
use super::{is_name, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TASK_CHECK_RECEIPT_V2: &str = "af/TaskCheckReceipt@2";
/// The receipt version before ADR-0255. This release neither writes nor reads it: a port that
/// declares it is refused by name, and so is a recorded receipt of it.
pub const RETIRED_TASK_CHECK_RECEIPT_V1: &str = "af/TaskCheckReceipt@1";
pub const TASK_EVALUATION_V1: &str = "af/TaskEvaluation@1";
pub const VERIFICATION_RESULT_V1: &str = "af/VerificationResult@1";
pub const REPAIR_ALLOWED_IMPLEMENTATION_V1: &str = "af/RepairAllowedImplementation@1";
pub const REVIEWED_IMPLEMENTATION_V1: &str = "af/ReviewedImplementation@1";

/// Refuse the retired receipt version by name (ADR-0145, ADR-0255); any other type passes.
pub fn refuse_retired_receipt(artifact_type: &str) -> Result<(), String> {
    if artifact_type == RETIRED_TASK_CHECK_RECEIPT_V1 {
        return Err(format!(
            "{RETIRED_TASK_CHECK_RECEIPT_V1} is the check receipt before ADR-0255, which this \
             release neither writes nor reads; declare {TASK_CHECK_RECEIPT_V2} instead and \
             re-pin the package with `af catalog digest`"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationReviewScopeV1 {
    CompleteReview,
    TargetedFixes,
}

/// Retains the exact acceptance invocation, including the child Review and exposed checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedImplementationV1 {
    pub invocation: super::execution::TaskInvocationV1,
    pub scope: ImplementationReviewScopeV1,
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

/// What a check node recorded for one plan on one Snapshot (ADR-0255): each check's result, the
/// checks the kernel stopped at their wall, and the Task's acceptance items when its Task file
/// stated them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckReceiptV2 {
    pub plan_id: String,
    pub snapshot_id: String,
    pub policy_id: String,
    pub outcome: ReceiptOutcomeV1,
    pub checks: BTreeMap<String, String>,
    /// Each check the kernel stopped, by name, with why: absent for a check that ended on its
    /// own or never started, so a receipt without one has no `stopped` at all.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "nonempty_stops"
    )]
    pub stopped: BTreeMap<String, TaskCheckStopV2>,
    /// The Task's `af/TaskAcceptance@1` items the checks ran under; absent for a Task file that
    /// states none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub acceptance: Option<TaskReceiptAcceptanceV2>,
}
impl TaskCheckReceiptV2 {
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
        )?;
        for (name, stop) in &self.stopped {
            require(
                self.checks.contains_key(name),
                "Check receipt stops a check it has no result for",
            )?;
            stop.validate()?;
        }
        if let Some(acceptance) = &self.acceptance {
            acceptance.validate()?;
        }
        Ok(())
    }
}

/// `stopped` is absent or names a check: an explicit empty map would be a second spelling of
/// "no check was stopped".
fn nonempty_stops<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, TaskCheckStopV2>, D::Error> {
    let stops = BTreeMap::<String, TaskCheckStopV2>::deserialize(deserializer)?;
    if stops.is_empty() {
        return Err(serde::de::Error::custom(
            "`stopped` is absent or names a stopped check",
        ));
    }
    Ok(stops)
}

/// Why the kernel stopped a check that had started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCheckStopCauseV2 {
    /// The check ran until its wall and was killed there: the per-check wall, the check
    /// Attempt's wall, or a warm phase deadline, whichever bound the process.
    Wall,
}

/// A check the kernel stopped: the cause, the wall it ran under and how long it ran, both in
/// milliseconds as the host measured them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckStopV2 {
    pub cause: TaskCheckStopCauseV2,
    pub wall_ms: u64,
    pub elapsed_ms: u64,
}
impl TaskCheckStopV2 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.wall_ms > 0
                && super::safe_number(self.wall_ms)
                && super::safe_number(self.elapsed_ms),
            "A stopped check names a positive wall and its elapsed time",
        )
    }
}

/// The acceptance a receipt carries: the `af/TaskAcceptance@1` record and its items, verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReceiptAcceptanceV2 {
    pub record_id: String,
    pub items: Vec<super::acceptance::TaskAcceptanceItemV1>,
}
impl TaskReceiptAcceptanceV2 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.record_id),
            "Receipt acceptance names its exact record",
        )?;
        super::acceptance::validate_items(&self.items)
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
