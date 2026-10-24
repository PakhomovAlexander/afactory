//! Explicit local delivery is a post-result operation, never a Worker or Pipeline effect.
use serde::{Deserialize, Serialize};

pub const TASK_DELIVERY_RECORD_V1: &str = "af/TaskDeliveryRecord@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDeliveryStatusV1 {
    Prepared,
    Delivered,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDeliveryRecordV1 {
    pub task_id: String,
    pub result_id: String,
    pub source_snapshot_id: String,
    pub derived_snapshot_id: String,
    pub target_id: String,
    pub receipt_id: String,
    pub status: TaskDeliveryStatusV1,
}

impl TaskDeliveryRecordV1 {
    pub fn validate(&self) -> Result<(), String> {
        super::require(
            super::is_name(&self.task_id) && self.references().into_iter().all(crate::is_digest),
            "Delivery needs exact Task, result, source, output, target and receipt identities",
        )
    }
    pub fn references(&self) -> [&str; 5] {
        [
            &self.result_id,
            &self.source_snapshot_id,
            &self.derived_snapshot_id,
            &self.target_id,
            &self.receipt_id,
        ]
    }
}

pub const TASK_DELIVERY_PROOF_V1: &str = "af/TaskDeliveryProof@1";

/// Why a verification Task's delivery places another Task's candidate (ADR-0205): the
/// verification Task, of kind `verification`, verified read-only a Snapshot it bound from the
/// candidate Task's recorded output, and delivery places that candidate on the candidate Task's
/// committed source. Every delivery record of such a delivery references this proof, so the
/// record names both Tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDeliveryProofV1 {
    pub verification_task_id: String,
    pub verification_result_id: String,
    pub candidate_task_id: String,
    pub candidate_result_id: String,
    pub candidate_snapshot_id: String,
    pub candidate_source_snapshot_id: String,
}

impl TaskDeliveryProofV1 {
    pub fn validate(&self) -> Result<(), String> {
        super::require(
            super::is_name(&self.verification_task_id)
                && super::is_name(&self.candidate_task_id)
                && self.verification_task_id != self.candidate_task_id
                && self.references().into_iter().all(crate::is_digest),
            "A delivery proof names two distinct Tasks, their exact results and the candidate's \
             Snapshot and source",
        )
    }
    pub fn references(&self) -> [&str; 4] {
        [
            &self.verification_result_id,
            &self.candidate_result_id,
            &self.candidate_snapshot_id,
            &self.candidate_source_snapshot_id,
        ]
    }
}
