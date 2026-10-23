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

pub const WORKER_REPLY_REASK_V1: &str = "af/WorkerReplyReask@1";

/// What the kernel tells a model Worker whose typed reply did not parse when it asks it once more
/// within the same Attempt (ADR-0199): the parse error with its line and column, the offending
/// excerpt of the reply, and the CAS digest of the whole reply the Attempt keeps. It travels as
/// one more feedback value of the re-sent request; it is never derived from a transcript, and it
/// grants nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerReplyReaskV1 {
    /// Always [`WORKER_REPLY_REASK_V1`].
    pub schema: String,
    /// What to do: return the reply once more, as `reply_format` says.
    pub instruction: String,
    /// The parser's or the reply contract's own message.
    pub error: String,
    /// The 1-based line and column the parser stopped at, when it was a parse error.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub line: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub column: Option<u64>,
    /// The reply around the place it failed, at most 512 characters.
    pub excerpt: String,
    pub raw_reply_id: String,
}

impl WorkerReplyReaskV1 {
    pub fn validate(&self) -> Result<(), String> {
        super::require(
            self.schema == WORKER_REPLY_REASK_V1
                && !self.instruction.trim().is_empty()
                && self.instruction.chars().count() <= 1024
                && !self.error.trim().is_empty()
                && self.error.chars().count() <= 2048
                && self.excerpt.chars().count() <= 512
                && self
                    .line
                    .is_none_or(|line| line > 0 && super::safe_number(line))
                && self.column.is_none_or(super::safe_number)
                && crate::is_digest(&self.raw_reply_id),
            "A reply re-ask needs its bounded error, excerpt and the reply it kept",
        )
    }
}
