//! Generic Task invocation and Attempt records. Domain outputs remain separately typed;
//! an execution failure is never coerced into a negative verification receipt.

use super::{ArtifactInputV1, is_name, require, safe_number};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TASK_INVOCATION_V1: &str = "af/TaskInvocation@1";
pub const TASK_OUTPUT_V1: &str = "af/TaskOutput@1";
pub const TASK_EXECUTION_RECORD_V1: &str = "af/TaskExecutionRecord@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInvocationV1 {
    pub plan_id: String,
    pub node: String,
    /// Only present values are recorded; optional missing ports do not get invented artifacts.
    pub inputs: BTreeMap<String, ArtifactInputV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOutputV1 {
    pub invocation_id: String,
    pub outputs: BTreeMap<String, ArtifactInputV1>,
}

fn ports(values: &BTreeMap<String, ArtifactInputV1>) -> Result<(), String> {
    for (name, value) in values {
        require(is_name(name), "Invalid Task invocation port")?;
        value.validate()?;
    }
    Ok(())
}

impl TaskInvocationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.plan_id) && self.node.split('.').all(is_name),
            "Task invocation needs exact plan and qualified node",
        )?;
        ports(&self.inputs)
    }
}

impl TaskOutputV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.invocation_id),
            "Task output needs its exact invocation",
        )?;
        ports(&self.outputs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskAttemptResultV1 {
    Succeeded {
        output_id: String,
    },
    Failed {
        diagnostic_id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        feedback_id: Option<String>,
    },
    Abandoned {
        diagnostic_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskExecutionRecordV1 {
    Invocation {
        invocation_id: String,
    },
    Prepared {
        invocation_id: String,
        attempt_id: String,
        reservation_id: String,
        reserved_tokens: u64,
        deadline_unix_ms: u64,
        context_id: String,
        /// Admitted retry feedback, never diagnostic prose or in-memory transcript state.
        feedback_ids: Vec<String>,
    },
    /// Reserves the real Attempt identity before rendering its exact context.
    Reserved {
        invocation_id: String,
        attempt_id: String,
        reservation_id: String,
        reserved_tokens: u64,
        deadline_unix_ms: u64,
        feedback_ids: Vec<String>,
    },
    ContextBound {
        attempt_id: String,
        context_id: String,
    },
    Started {
        attempt_id: String,
    },
    Released {
        attempt_id: String,
        reason: String,
    },
    Settled {
        attempt_id: String,
        charged_tokens: u64,
        result: TaskAttemptResultV1,
        raw_artifact_ids: Vec<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        usage_id: Option<String>,
    },
    Published {
        output_id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        attempt_id: Option<String>,
    },
    UsageObserved {
        attempt_id: String,
        charged_tokens: u64,
        usage_id: String,
        raw_artifact_ids: Vec<String>,
    },
}

impl TaskExecutionRecordV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        let mut refs = Vec::new();
        match self {
            Self::Invocation { invocation_id } => refs.push(invocation_id.as_str()),
            Self::Prepared {
                invocation_id,
                context_id,
                feedback_ids,
                ..
            } => {
                refs.extend([invocation_id.as_str(), context_id.as_str()]);
                refs.extend(feedback_ids.iter().map(String::as_str));
            }
            Self::Reserved {
                invocation_id,
                feedback_ids,
                ..
            } => {
                refs.push(invocation_id.as_str());
                refs.extend(feedback_ids.iter().map(String::as_str));
            }
            Self::ContextBound { context_id, .. } => refs.push(context_id.as_str()),
            Self::Settled {
                result,
                raw_artifact_ids,
                usage_id,
                ..
            } => {
                refs.extend(raw_artifact_ids.iter().map(String::as_str));
                refs.extend(usage_id.iter().map(String::as_str));
                match result {
                    TaskAttemptResultV1::Succeeded { output_id } => refs.push(output_id),
                    TaskAttemptResultV1::Failed {
                        diagnostic_id,
                        feedback_id,
                    } => {
                        refs.push(diagnostic_id);
                        refs.extend(feedback_id.iter().map(String::as_str));
                    }
                    TaskAttemptResultV1::Abandoned { diagnostic_id } => refs.push(diagnostic_id),
                }
            }
            Self::Published { output_id, .. } => refs.push(output_id),
            Self::UsageObserved {
                usage_id,
                raw_artifact_ids,
                ..
            } => {
                refs.push(usage_id);
                refs.extend(raw_artifact_ids.iter().map(String::as_str));
            }
            _ => (),
        }
        refs
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.artifact_refs().iter().all(|id| is_digest(id)),
            "Invalid Task execution artifact reference",
        )?;
        let attempt = match self {
            Self::Invocation { .. } => None,
            Self::Prepared {
                attempt_id,
                reservation_id,
                reserved_tokens,
                deadline_unix_ms,
                feedback_ids,
                ..
            }
            | Self::Reserved {
                attempt_id,
                reservation_id,
                reserved_tokens,
                deadline_unix_ms,
                feedback_ids,
                ..
            } => {
                require(
                    reservation_id
                        .strip_prefix("reservation:")
                        .is_some_and(|number| {
                            !number.is_empty()
                                && number.len() <= 20
                                && number.bytes().all(|b| b.is_ascii_digit())
                        })
                        && safe_number(*reserved_tokens)
                        && *deadline_unix_ms > 0
                        && safe_number(*deadline_unix_ms),
                    "Invalid prepared Task reservation",
                )?;
                require(
                    feedback_ids.len() <= 16
                        && feedback_ids
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == feedback_ids.len(),
                    "Invalid Task retry feedback selection",
                )?;
                Some(attempt_id)
            }
            Self::Started { attempt_id } | Self::ContextBound { attempt_id, .. } => {
                Some(attempt_id)
            }
            Self::Released { attempt_id, reason } => {
                require(
                    !reason.trim().is_empty() && reason.chars().count() <= 65536,
                    "Task release needs bounded diagnostics",
                )?;
                Some(attempt_id)
            }
            Self::Settled {
                attempt_id,
                charged_tokens,
                raw_artifact_ids,
                ..
            }
            | Self::UsageObserved {
                attempt_id,
                charged_tokens,
                raw_artifact_ids,
                ..
            } => {
                require(
                    safe_number(*charged_tokens),
                    "Task charge exceeds safe integer bound",
                )?;
                require(
                    raw_artifact_ids.len() <= 64
                        && raw_artifact_ids
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == raw_artifact_ids.len(),
                    "Invalid raw Task evidence selection",
                )?;
                Some(attempt_id)
            }
            Self::Published { attempt_id, .. } => attempt_id.as_ref(),
        };
        if let Some(attempt) = attempt {
            require(
                attempt.len() == 26 && attempt.bytes().all(|b| b.is_ascii_alphanumeric()),
                "Invalid Task Attempt identity",
            )?;
        }
        Ok(())
    }
}
