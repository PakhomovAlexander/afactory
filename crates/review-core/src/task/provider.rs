//! Provider capability admission is a charged Task operation, separate from business inputs.
use super::pipeline::ReceiptOutcomeV1;
use super::plan::WorkerExecutionV1;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const TASK_PROVIDER_ADMISSION_V1: &str = "af/TaskProviderAdmission@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProviderAdmissionV1 {
    pub plan_id: String,
    #[serde(deserialize_with = "super::unique_set")]
    pub bindings: BTreeSet<String>,
    pub execution: WorkerExecutionV1,
    pub invocation_policy_id: String,
    pub outcome: ReceiptOutcomeV1,
}
impl TaskProviderAdmissionV1 {
    pub fn validate(&self) -> Result<(), String> {
        super::require(
            crate::is_digest(&self.plan_id)
                && crate::is_digest(&self.invocation_policy_id)
                && !self.bindings.is_empty()
                && self.bindings.len() <= 64
                && self
                    .bindings
                    .iter()
                    .all(|slot| slot.len() <= 1024 && slot.split('.').all(super::is_name))
                && matches!(self.execution, WorkerExecutionV1::Model { .. })
                && self.outcome == ReceiptOutcomeV1::Passed,
            "Provider admission requires exact Model bindings and a successful bounded capability probe",
        )?;
        self.execution.validate()
    }
}
