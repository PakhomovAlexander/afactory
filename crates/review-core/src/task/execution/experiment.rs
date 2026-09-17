//! Experimental child lifecycle. Earlier execution generations remain byte-for-byte readable.

use super::TaskExecutionRecordV1;
use serde::{Deserialize, Serialize};

pub const TASK_EXECUTION_RECORD_V5: &str = "af/TaskExecutionRecord@5";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskExecutionRecordV5 {
    ExperimentPrepared {
        prepared_id: String,
    },
    ExperimentPlanDecided {
        prepared_id: String,
        decision_id: String,
    },
    ExperimentChildrenRegistered {
        prepared_id: String,
        decision_id: String,
        child_plan_id: String,
    },
}

impl TaskExecutionRecordV5 {
    pub fn from_experiment(value: &TaskExecutionRecordV1) -> Option<Self> {
        Some(match value {
            TaskExecutionRecordV1::ExperimentPrepared { prepared_id } => Self::ExperimentPrepared {
                prepared_id: prepared_id.clone(),
            },
            TaskExecutionRecordV1::ExperimentPlanDecided {
                prepared_id,
                decision_id,
            } => Self::ExperimentPlanDecided {
                prepared_id: prepared_id.clone(),
                decision_id: decision_id.clone(),
            },
            TaskExecutionRecordV1::ExperimentChildrenRegistered {
                prepared_id,
                decision_id,
                child_plan_id,
            } => Self::ExperimentChildrenRegistered {
                prepared_id: prepared_id.clone(),
                decision_id: decision_id.clone(),
                child_plan_id: child_plan_id.clone(),
            },
            _ => return None,
        })
    }

    pub fn into_record(self) -> TaskExecutionRecordV1 {
        match self {
            Self::ExperimentPrepared { prepared_id } => {
                TaskExecutionRecordV1::ExperimentPrepared { prepared_id }
            }
            Self::ExperimentPlanDecided {
                prepared_id,
                decision_id,
            } => TaskExecutionRecordV1::ExperimentPlanDecided {
                prepared_id,
                decision_id,
            },
            Self::ExperimentChildrenRegistered {
                prepared_id,
                decision_id,
                child_plan_id,
            } => TaskExecutionRecordV1::ExperimentChildrenRegistered {
                prepared_id,
                decision_id,
                child_plan_id,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let value = self.clone().into_record();
        if value.artifact_refs().into_iter().all(crate::is_digest) {
            Ok(())
        } else {
            Err("Experimental transition needs exact artifact identities".into())
        }
    }
}
