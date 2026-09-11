//! Additive Task lifecycle events in the common RunEvent log. Actor strings and writer
//! epochs describe recorded authority; only the trusted Store entry points can grant it.

use serde::{Deserialize, Serialize};

use super::{TaskWaitingReasonV1, is_name, require, safe_number};
use crate::is_digest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskChangeV1 {
    Opened {
        revision_id: String,
        lease_until_unix_ms: u64,
    },
    LeaseTaken {
        lease_until_unix_ms: u64,
    },
    LeaseRenewed {
        lease_until_unix_ms: u64,
    },
    RevisionRecorded {
        revision_id: String,
    },
    PlanProposed {
        plan_id: String,
    },
    PlanDecided {
        decision_id: String,
        valid_until_unix_ms: u64,
    },
    ApprovalRevoked {
        decision_id: String,
        reason: String,
    },
    PlanAdmitted {
        plan_id: String,
    },
    Waiting {
        reason: TaskWaitingReasonV1,
    },
    Resumed {},
    Finished {
        result_id: String,
    },
    ExecutionRecorded {
        record_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTransitionV1 {
    pub writer: String,
    pub epoch: u64,
    pub now_unix_ms: u64,
    pub change: TaskChangeV1,
}

impl TaskTransitionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.writer)
                && self.epoch > 0
                && safe_number(self.epoch)
                && self.now_unix_ms > 0
                && safe_number(self.now_unix_ms),
            "Task transition needs a writer, epoch and bounded policy time",
        )?;
        for id in self.artifact_refs() {
            require(is_digest(id), "Invalid Task transition artifact ID")?;
        }
        match &self.change {
            TaskChangeV1::Opened {
                lease_until_unix_ms,
                ..
            }
            | TaskChangeV1::LeaseTaken {
                lease_until_unix_ms,
            }
            | TaskChangeV1::LeaseRenewed {
                lease_until_unix_ms,
            } => require(
                safe_number(*lease_until_unix_ms) && *lease_until_unix_ms > self.now_unix_ms,
                "Task lease must expire after its recorded acquisition time",
            ),
            TaskChangeV1::PlanDecided {
                valid_until_unix_ms,
                ..
            } => require(
                safe_number(*valid_until_unix_ms) && *valid_until_unix_ms > self.now_unix_ms,
                "Developer authorization must be current when recorded",
            ),
            TaskChangeV1::ApprovalRevoked { reason, .. } => require(
                !reason.trim().is_empty() && reason.chars().count() <= 65536,
                "Revocation needs a bounded reason",
            ),
            _ => Ok(()),
        }
    }

    pub fn artifact_refs(&self) -> Vec<&str> {
        match &self.change {
            TaskChangeV1::Opened { revision_id, .. }
            | TaskChangeV1::RevisionRecorded { revision_id } => vec![revision_id],
            TaskChangeV1::PlanProposed { plan_id } | TaskChangeV1::PlanAdmitted { plan_id } => {
                vec![plan_id]
            }
            TaskChangeV1::PlanDecided { decision_id, .. }
            | TaskChangeV1::ApprovalRevoked { decision_id, .. } => vec![decision_id],
            TaskChangeV1::Finished { result_id } => vec![result_id],
            TaskChangeV1::ExecutionRecorded { record_id } => vec![record_id],
            _ => Vec::new(),
        }
    }
}
