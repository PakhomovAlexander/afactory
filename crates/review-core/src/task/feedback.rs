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
    CompilerRejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCompilerFeedbackV1 {
    pub proposal_id: String,
    pub proposal: super::planning::PipelineProposalV1,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetryFeedbackV1 {
    pub attempt_id: String,
    pub contract_id: String,
    pub code: TaskFeedbackCodeV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub compiler: Option<TaskCompilerFeedbackV1>,
}
impl TaskRetryFeedbackV1 {
    pub fn validate(&self) -> Result<(), String> {
        super::require(
            self.attempt_id.len() == 26
                && self.attempt_id.bytes().all(|c| c.is_ascii_alphanumeric())
                && crate::is_digest(&self.contract_id),
            "Retry feedback needs an exact Attempt and Worker contract",
        )?;
        super::require(
            (self.code == TaskFeedbackCodeV1::CompilerRejected) == self.compiler.is_some(),
            "Compiler rejection requires typed compiler feedback",
        )?;
        if let Some(compiler) = &self.compiler {
            compiler.proposal.validate()?;
            super::require(
                crate::is_digest(&compiler.proposal_id)
                    && !compiler.diagnostics.is_empty()
                    && compiler.diagnostics.len() <= 4
                    && compiler
                        .diagnostics
                        .iter()
                        .all(|s| !s.trim().is_empty() && s.len() <= 8192),
                "Compiler feedback needs a proposal identity and bounded diagnostics",
            )?;
        }
        Ok(())
    }
}
