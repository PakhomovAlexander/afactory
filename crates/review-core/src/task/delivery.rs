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
