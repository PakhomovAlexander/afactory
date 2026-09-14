//! Bounded retry guidance, distinct from terminal diagnostics or a prior model transcript.
use serde::{Deserialize, Serialize};

pub const TASK_RETRY_FEEDBACK_V1: &str = "af/TaskRetryFeedback@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFeedbackCodeV1 {
    InvalidOutputContract,
    ProcessFailure,
    ProviderFailure,
    ContextRejected,
    OutputAdmissionRejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetryFeedbackV1 {
    pub attempt_id: String,
    pub contract_id: String,
    pub code: TaskFeedbackCodeV1,
}
impl TaskRetryFeedbackV1 {
    pub fn validate(&self) -> Result<(), String> {
        super::require(
            self.attempt_id.len() == 26
                && self.attempt_id.bytes().all(|c| c.is_ascii_alphanumeric())
                && crate::is_digest(&self.contract_id),
            "Retry feedback needs an exact Attempt and Worker contract",
        )
    }
}
