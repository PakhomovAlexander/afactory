//! Additive Task lifecycle events in the common RunEvent log. Actor strings and writer
//! epochs describe recorded authority; only the trusted Store entry points can grant it.

use serde::{Deserialize, Serialize};

use super::{TaskWaitingReasonV1, is_name, require, safe_number};
use crate::is_digest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskChangeV1 {
    #[serde(skip)]
    ReviewIntegrationSelected {
        phase_id: String,
    },
    #[serde(skip)]
    ReviewIntegrationFinished {
        phase_id: String,
        report_id: String,
        integration_committed_event_id: Option<String>,
    },
    /// Internal normalized form. Only TaskTransition@2 can carry Review handoff authority.
    #[serde(skip)]
    ReviewContinued {
        handoff_id: String,
    },
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
    LeaseReleased {},
    RevisionRecorded {
        revision_id: String,
    },
    /// Atomic source-revision and replanning barrier. No execution approval carries over.
    SourceRefreshed {
        revision_id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        plan_id: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        waiting: Option<TaskWaitingReasonV1>,
    },
    PlanProposed {
        plan_id: String,
    },
    /// One atomic barrier retains planning charges and proposes the exact generated plan.
    PlanningCompleted {
        bootstrap_plan_id: String,
        proposal_id: String,
        revision_id: String,
        plan_id: String,
    },
    PlanDecided {
        decision_id: String,
        valid_until_unix_ms: u64,
    },
    ApprovalRevoked {
        decision_id: String,
        reason: String,
        /// Historical records lacked a retained proof. New trusted writes always include it.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        revocation_id: Option<String>,
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
    RunReported {
        report_id: String,
    },
    DeliveryRecorded {
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
            TaskChangeV1::ReviewIntegrationSelected { .. }
            | TaskChangeV1::ReviewIntegrationFinished { .. } => {
                Err("Review Integration requires TaskTransition@3".into())
            }
            TaskChangeV1::ReviewContinued { .. } => {
                Err("Review continuation requires TaskTransition@2".into())
            }
            TaskChangeV1::SourceRefreshed {
                plan_id, waiting, ..
            } => require(
                plan_id.is_some() != waiting.is_some()
                    && *waiting != Some(TaskWaitingReasonV1::NeedsPlanReview),
                "Source refresh needs a plan or an unresolved reason; approval waiting is derived from the plan",
            ),
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
            TaskChangeV1::ReviewIntegrationSelected { phase_id } => vec![phase_id],
            TaskChangeV1::ReviewIntegrationFinished {
                phase_id,
                report_id,
                ..
            } => vec![phase_id, report_id],
            TaskChangeV1::ReviewContinued { handoff_id } => vec![handoff_id],
            TaskChangeV1::SourceRefreshed {
                revision_id,
                plan_id,
                ..
            } => {
                let mut refs = vec![revision_id.as_str()];
                refs.extend(plan_id.as_deref());
                refs
            }
            TaskChangeV1::PlanningCompleted {
                bootstrap_plan_id,
                proposal_id,
                revision_id,
                plan_id,
            } => vec![bootstrap_plan_id, proposal_id, revision_id, plan_id],
            TaskChangeV1::Opened { revision_id, .. }
            | TaskChangeV1::RevisionRecorded { revision_id } => vec![revision_id],
            TaskChangeV1::PlanProposed { plan_id } | TaskChangeV1::PlanAdmitted { plan_id } => {
                vec![plan_id]
            }
            TaskChangeV1::PlanDecided { decision_id, .. } => vec![decision_id],
            TaskChangeV1::ApprovalRevoked {
                decision_id,
                revocation_id,
                ..
            } => std::iter::once(decision_id.as_str())
                .chain(revocation_id.as_deref())
                .collect(),
            TaskChangeV1::Finished { result_id } => vec![result_id],
            TaskChangeV1::RunReported { report_id } => vec![report_id],
            TaskChangeV1::ExecutionRecorded { record_id }
            | TaskChangeV1::DeliveryRecorded { record_id } => vec![record_id],
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskChangeV2 {
    ReviewContinued { handoff_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTransitionV2 {
    pub writer: String,
    pub epoch: u64,
    pub now_unix_ms: u64,
    pub change: TaskChangeV2,
}
impl TaskTransitionV2 {
    pub fn validate(&self) -> Result<(), String> {
        let TaskChangeV2::ReviewContinued { handoff_id } = &self.change;
        require(is_digest(handoff_id), "Invalid Review handoff identity")?;
        TaskTransitionV1 {
            writer: self.writer.clone(),
            epoch: self.epoch,
            now_unix_ms: self.now_unix_ms,
            change: TaskChangeV1::Resumed {},
        }
        .validate()
    }
    pub fn from_continuation(value: &TaskTransitionV1) -> Option<Self> {
        let TaskChangeV1::ReviewContinued { handoff_id } = &value.change else {
            return None;
        };
        Some(Self {
            writer: value.writer.clone(),
            epoch: value.epoch,
            now_unix_ms: value.now_unix_ms,
            change: TaskChangeV2::ReviewContinued {
                handoff_id: handoff_id.clone(),
            },
        })
    }
    pub fn into_transition(self) -> TaskTransitionV1 {
        let TaskChangeV2::ReviewContinued { handoff_id } = self.change;
        TaskTransitionV1 {
            writer: self.writer,
            epoch: self.epoch,
            now_unix_ms: self.now_unix_ms,
            change: TaskChangeV1::ReviewContinued { handoff_id },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskChangeV3 {
    ReviewIntegrationSelected {
        phase_id: String,
    },
    ReviewIntegrationFinished {
        phase_id: String,
        report_id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        integration_committed_event_id: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTransitionV3 {
    pub writer: String,
    pub epoch: u64,
    pub now_unix_ms: u64,
    pub change: TaskChangeV3,
}
impl TaskTransitionV3 {
    pub fn validate(&self) -> Result<(), String> {
        let phase = match &self.change {
            TaskChangeV3::ReviewIntegrationSelected { phase_id } => phase_id,
            TaskChangeV3::ReviewIntegrationFinished {
                phase_id,
                report_id,
                integration_committed_event_id,
            } => {
                require(
                    is_digest(report_id)
                        && integration_committed_event_id
                            .as_deref()
                            .is_none_or(super::review_integration::event_id),
                    "Invalid Integration finalization evidence",
                )?;
                phase_id
            }
        };
        require(is_digest(phase), "Invalid Integration phase identity")?;
        TaskTransitionV1 {
            writer: self.writer.clone(),
            epoch: self.epoch,
            now_unix_ms: self.now_unix_ms,
            change: TaskChangeV1::Resumed {},
        }
        .validate()
    }
    pub fn from_integration(value: &TaskTransitionV1) -> Option<Self> {
        let change = match &value.change {
            TaskChangeV1::ReviewIntegrationSelected { phase_id } => {
                TaskChangeV3::ReviewIntegrationSelected {
                    phase_id: phase_id.clone(),
                }
            }
            TaskChangeV1::ReviewIntegrationFinished {
                phase_id,
                report_id,
                integration_committed_event_id,
            } => TaskChangeV3::ReviewIntegrationFinished {
                phase_id: phase_id.clone(),
                report_id: report_id.clone(),
                integration_committed_event_id: integration_committed_event_id.clone(),
            },
            _ => return None,
        };
        Some(Self {
            writer: value.writer.clone(),
            epoch: value.epoch,
            now_unix_ms: value.now_unix_ms,
            change,
        })
    }
    pub fn into_transition(self) -> TaskTransitionV1 {
        let change = match self.change {
            TaskChangeV3::ReviewIntegrationSelected { phase_id } => {
                TaskChangeV1::ReviewIntegrationSelected { phase_id }
            }
            TaskChangeV3::ReviewIntegrationFinished {
                phase_id,
                report_id,
                integration_committed_event_id,
            } => TaskChangeV1::ReviewIntegrationFinished {
                phase_id,
                report_id,
                integration_committed_event_id,
            },
        };
        TaskTransitionV1 {
            writer: self.writer,
            epoch: self.epoch,
            now_unix_ms: self.now_unix_ms,
            change,
        }
    }
}
