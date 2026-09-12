//! Exact captured Review Round succession. These data grant no execution authority.
use super::{is_name, require};
use serde::{Deserialize, Serialize};

pub const TASK_REVIEW_HANDOFF_V1: &str = "af/TaskReviewHandoff@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReviewHandoffEvidenceV1 {
    ClosedRound { report_event_id: String },
    SupersededInput { superseded_event_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewHandoffV1 {
    pub task_id: String,
    pub predecessor_revision_id: String,
    pub predecessor_plan_id: String,
    pub successor_revision_id: String,
    pub successor_plan_id: String,
    pub predecessor_round_id: String,
    pub successor_round_id: String,
    pub evidence: TaskReviewHandoffEvidenceV1,
}
impl TaskReviewHandoffV1 {
    pub fn artifact_refs(&self) -> [&str; 6] {
        [
            &self.predecessor_revision_id,
            &self.predecessor_plan_id,
            &self.successor_revision_id,
            &self.successor_plan_id,
            &self.predecessor_round_id,
            &self.successor_round_id,
        ]
    }
    pub fn evidence_event_id(&self) -> &str {
        match &self.evidence {
            TaskReviewHandoffEvidenceV1::ClosedRound { report_event_id } => report_event_id,
            TaskReviewHandoffEvidenceV1::SupersededInput {
                superseded_event_id,
            } => superseded_event_id,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let event = self.evidence_event_id();
        require(
            is_name(&self.task_id)
                && self.artifact_refs().into_iter().all(crate::is_digest)
                && event.len() == 26
                && event
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && self.predecessor_revision_id != self.successor_revision_id
                && self.predecessor_plan_id != self.successor_plan_id
                && self.predecessor_round_id != self.successor_round_id,
            "Review handoff requires distinct exact predecessor and successor identities",
        )
    }
}
