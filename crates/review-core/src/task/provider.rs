//! Provider capability admission is a charged Task operation, separate from business inputs.
use super::pipeline::ReceiptOutcomeV1;
use super::plan::WorkerExecutionV1;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const TASK_PROVIDER_ADMISSION_V1: &str = "af/TaskProviderAdmission@1";
pub const TASK_PROVIDER_ADMISSION_V2: &str = "af/TaskProviderAdmission@2";
pub const TASK_PROVIDER_PROBE_POLICY_V1: &str = "af/TaskProviderProbePolicy@1";

/// The fixed capability request and bounded `OK` acknowledgement protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskProviderProbeProtocolV1 {
    #[serde(rename = "af.provider-ok/1")]
    OkV1,
}

/// Independent authority for one installed Provider Attempt, never a business policy copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProviderProbePolicyV1 {
    pub authority_policy_id: String,
    pub execution: WorkerExecutionV1,
    pub credential_mode: crate::BrokerCredentialModeV1,
    pub probe_protocol: TaskProviderProbeProtocolV1,
    pub operations: Vec<crate::BrokerOperationPolicyV1>,
}

impl TaskProviderProbePolicyV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        vec![&self.authority_policy_id]
    }

    pub fn validate(&self) -> Result<(), String> {
        super::require(
            crate::is_digest(&self.authority_policy_id)
                && matches!(self.execution, WorkerExecutionV1::Model { .. })
                && self.credential_mode == crate::BrokerCredentialModeV1::Brokered
                && !self.operations.is_empty(),
            "Provider probe requires exact Model identity and explicit Brokered authority",
        )?;
        self.execution.validate()?;
        let mut names = BTreeSet::new();
        for operation in &self.operations {
            operation.validate()?;
            super::require(
                names.insert(&operation.name),
                "Provider probe has duplicate operations",
            )?;
        }
        crate::broker_authority_usage(&self.operations)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProviderAdmissionV2 {
    pub plan_id: String,
    #[serde(deserialize_with = "super::unique_set")]
    pub bindings: BTreeSet<String>,
    pub execution: WorkerExecutionV1,
    pub probe_policy_id: String,
    pub outcome: ReceiptOutcomeV1,
}

impl TaskProviderAdmissionV2 {
    pub fn validate(&self) -> Result<(), String> {
        TaskProviderAdmissionV1 {
            plan_id: self.plan_id.clone(),
            bindings: self.bindings.clone(),
            execution: self.execution.clone(),
            invocation_policy_id: self.probe_policy_id.clone(),
            outcome: self.outcome,
        }
        .validate()
    }
}

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
