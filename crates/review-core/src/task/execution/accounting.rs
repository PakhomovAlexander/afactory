//! Accounting upgrades preserve exact counters. Legacy records keep their bytes and bounds.

use super::{TaskAttemptResultV1, TaskExecutionRecordV1};
use crate::task::usage::{DecimalU64, DecimalU128};
use serde::{Deserialize, Serialize};

pub const TASK_EXECUTION_RECORD_V2: &str = "af/TaskExecutionRecord@2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskExecutionRecordV2 {
    Settled {
        attempt_id: String,
        charged_tokens: DecimalU64,
        result: TaskAttemptResultV1,
        raw_artifact_ids: Vec<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::task::present_option"
        )]
        usage_id: Option<String>,
    },
    UsageObserved {
        attempt_id: String,
        charged_tokens: DecimalU64,
        usage_id: String,
        raw_artifact_ids: Vec<String>,
    },
}

impl TaskExecutionRecordV2 {
    /// Returns None for non-accounting records or charges outside the frozen u64 domain.
    pub fn from_accounting(record: &TaskExecutionRecordV1) -> Option<Self> {
        match record {
            TaskExecutionRecordV1::Settled {
                attempt_id,
                charged_tokens,
                result,
                raw_artifact_ids,
                usage_id,
            } => Some(Self::Settled {
                attempt_id: attempt_id.clone(),
                charged_tokens: u64::try_from(*charged_tokens).ok()?.into(),
                result: result.clone(),
                raw_artifact_ids: raw_artifact_ids.clone(),
                usage_id: usage_id.clone(),
            }),
            TaskExecutionRecordV1::UsageObserved {
                attempt_id,
                charged_tokens,
                usage_id,
                raw_artifact_ids,
            } => Some(Self::UsageObserved {
                attempt_id: attempt_id.clone(),
                charged_tokens: u64::try_from(*charged_tokens).ok()?.into(),
                usage_id: usage_id.clone(),
                raw_artifact_ids: raw_artifact_ids.clone(),
            }),
            _ => None,
        }
    }

    /// Internal lifecycle operations share the same record shape; this is not a v1 encoding.
    pub fn into_record(self) -> TaskExecutionRecordV1 {
        match self {
            Self::Settled {
                attempt_id,
                charged_tokens,
                result,
                raw_artifact_ids,
                usage_id,
            } => TaskExecutionRecordV1::Settled {
                attempt_id,
                charged_tokens: u128::from(charged_tokens.get()),
                result,
                raw_artifact_ids,
                usage_id,
            },
            Self::UsageObserved {
                attempt_id,
                charged_tokens,
                usage_id,
                raw_artifact_ids,
            } => TaskExecutionRecordV1::UsageObserved {
                attempt_id,
                charged_tokens: u128::from(charged_tokens.get()),
                usage_id,
                raw_artifact_ids,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.clone().into_record().validate_fields()
    }
}

pub const TASK_EXECUTION_RECORD_V3: &str = "af/TaskExecutionRecord@3";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskExecutionRecordV3 {
    Settled {
        attempt_id: String,
        charged_tokens: DecimalU128,
        result: TaskAttemptResultV1,
        raw_artifact_ids: Vec<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::task::present_option"
        )]
        usage_id: Option<String>,
    },
    UsageObserved {
        attempt_id: String,
        charged_tokens: DecimalU128,
        usage_id: String,
        raw_artifact_ids: Vec<String>,
    },
}

impl TaskExecutionRecordV3 {
    pub fn from_accounting(record: &TaskExecutionRecordV1) -> Option<Self> {
        match record {
            TaskExecutionRecordV1::Settled {
                attempt_id,
                charged_tokens,
                result,
                raw_artifact_ids,
                usage_id,
            } => Some(Self::Settled {
                attempt_id: attempt_id.clone(),
                charged_tokens: (*charged_tokens).into(),
                result: result.clone(),
                raw_artifact_ids: raw_artifact_ids.clone(),
                usage_id: usage_id.clone(),
            }),
            TaskExecutionRecordV1::UsageObserved {
                attempt_id,
                charged_tokens,
                usage_id,
                raw_artifact_ids,
            } => Some(Self::UsageObserved {
                attempt_id: attempt_id.clone(),
                charged_tokens: (*charged_tokens).into(),
                usage_id: usage_id.clone(),
                raw_artifact_ids: raw_artifact_ids.clone(),
            }),
            _ => None,
        }
    }

    /// Internal lifecycle operations share the same record shape; this is not a v1 encoding.
    pub fn into_record(self) -> TaskExecutionRecordV1 {
        match self {
            Self::Settled {
                attempt_id,
                charged_tokens,
                result,
                raw_artifact_ids,
                usage_id,
            } => TaskExecutionRecordV1::Settled {
                attempt_id,
                charged_tokens: charged_tokens.get(),
                result,
                raw_artifact_ids,
                usage_id,
            },
            Self::UsageObserved {
                attempt_id,
                charged_tokens,
                usage_id,
                raw_artifact_ids,
            } => TaskExecutionRecordV1::UsageObserved {
                attempt_id,
                charged_tokens: charged_tokens.get(),
                usage_id,
                raw_artifact_ids,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.clone().into_record().validate_fields()
    }
}
