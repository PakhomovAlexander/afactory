//! Recording-only owned lifecycle upgrade; all earlier wire generations stay frozen.

use super::TaskExecutionRecordV1;
use serde::{Deserialize, Serialize};

pub const TASK_EXECUTION_RECORD_V4: &str = "af/TaskExecutionRecord@4";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskExecutionRecordV4 {
    OwnedChildrenRegistered {
        child_set_id: String,
    },
    OwnedChildPublished {
        child_set_id: String,
        output_id: String,
        attempt_id: String,
    },
    OwnedChildrenCompleted {
        child_set_id: String,
        output_id: String,
    },
}

impl TaskExecutionRecordV4 {
    pub fn from_owned(record: &TaskExecutionRecordV1) -> Option<Self> {
        match record {
            TaskExecutionRecordV1::OwnedChildrenRegistered { child_set_id } => {
                Some(Self::OwnedChildrenRegistered {
                    child_set_id: child_set_id.clone(),
                })
            }
            TaskExecutionRecordV1::OwnedChildPublished {
                child_set_id,
                output_id,
                attempt_id,
            } => Some(Self::OwnedChildPublished {
                child_set_id: child_set_id.clone(),
                output_id: output_id.clone(),
                attempt_id: attempt_id.clone(),
            }),
            TaskExecutionRecordV1::OwnedChildrenCompleted {
                child_set_id,
                output_id,
            } => Some(Self::OwnedChildrenCompleted {
                child_set_id: child_set_id.clone(),
                output_id: output_id.clone(),
            }),
            _ => None,
        }
    }

    pub fn into_record(self) -> TaskExecutionRecordV1 {
        match self {
            Self::OwnedChildrenRegistered { child_set_id } => {
                TaskExecutionRecordV1::OwnedChildrenRegistered { child_set_id }
            }
            Self::OwnedChildPublished {
                child_set_id,
                output_id,
                attempt_id,
            } => TaskExecutionRecordV1::OwnedChildPublished {
                child_set_id,
                output_id,
                attempt_id,
            },
            Self::OwnedChildrenCompleted {
                child_set_id,
                output_id,
            } => TaskExecutionRecordV1::OwnedChildrenCompleted {
                child_set_id,
                output_id,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.clone().into_record().validate_fields()
    }
}
