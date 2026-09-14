//! Exact captured Review Round succession. These data grant no execution authority.
use super::{is_name, require};
use serde::{Deserialize, Serialize};

pub const TASK_REVIEW_HANDOFF_V1: &str = "af/TaskReviewHandoff@1";
pub const TASK_REVIEW_HANDOFF_V2: &str = "af/TaskReviewHandoff@2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReviewHandoffEvidenceV1 {
    /// Only Handoff@2 carries this internal normalized variant.
    #[serde(skip)]
    IntegratedRound {
        report_event_id: String,
        phase_id: String,
        integration_committed_event_id: String,
    },
    ClosedRound {
        report_event_id: String,
    },
    SupersededInput {
        superseded_event_id: String,
    },
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
    pub fn artifact_refs(&self) -> Vec<&str> {
        let mut refs: Vec<&str> = vec![
            &self.predecessor_revision_id,
            &self.predecessor_plan_id,
            &self.successor_revision_id,
            &self.successor_plan_id,
            &self.predecessor_round_id,
            &self.successor_round_id,
        ];
        if let TaskReviewHandoffEvidenceV1::IntegratedRound { phase_id, .. } = &self.evidence {
            refs.push(phase_id);
        }
        refs
    }
    pub fn evidence_event_id(&self) -> &str {
        match &self.evidence {
            TaskReviewHandoffEvidenceV1::IntegratedRound {
                integration_committed_event_id,
                ..
            } => integration_committed_event_id,
            TaskReviewHandoffEvidenceV1::ClosedRound { report_event_id } => report_event_id,
            TaskReviewHandoffEvidenceV1::SupersededInput {
                superseded_event_id,
            } => superseded_event_id,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            !matches!(
                self.evidence,
                TaskReviewHandoffEvidenceV1::IntegratedRound { .. }
            ),
            "Integrated continuation requires TaskReviewHandoff@2",
        )?;
        self.validate_identity()
    }
    fn validate_identity(&self) -> Result<(), String> {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReviewHandoffEvidenceV2 {
    IntegratedRound {
        report_event_id: String,
        phase_id: String,
        integration_committed_event_id: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewHandoffV2 {
    pub task_id: String,
    pub predecessor_revision_id: String,
    pub predecessor_plan_id: String,
    pub successor_revision_id: String,
    pub successor_plan_id: String,
    pub predecessor_round_id: String,
    pub successor_round_id: String,
    pub evidence: TaskReviewHandoffEvidenceV2,
}
impl TaskReviewHandoffV2 {
    pub fn from_integrated(value: &TaskReviewHandoffV1) -> Option<Self> {
        let TaskReviewHandoffEvidenceV1::IntegratedRound {
            report_event_id,
            phase_id,
            integration_committed_event_id,
        } = &value.evidence
        else {
            return None;
        };
        Some(Self {
            task_id: value.task_id.clone(),
            predecessor_revision_id: value.predecessor_revision_id.clone(),
            predecessor_plan_id: value.predecessor_plan_id.clone(),
            successor_revision_id: value.successor_revision_id.clone(),
            successor_plan_id: value.successor_plan_id.clone(),
            predecessor_round_id: value.predecessor_round_id.clone(),
            successor_round_id: value.successor_round_id.clone(),
            evidence: TaskReviewHandoffEvidenceV2::IntegratedRound {
                report_event_id: report_event_id.clone(),
                phase_id: phase_id.clone(),
                integration_committed_event_id: integration_committed_event_id.clone(),
            },
        })
    }
    pub fn into_handoff(self) -> TaskReviewHandoffV1 {
        let TaskReviewHandoffEvidenceV2::IntegratedRound {
            report_event_id,
            phase_id,
            integration_committed_event_id,
        } = self.evidence;
        TaskReviewHandoffV1 {
            task_id: self.task_id,
            predecessor_revision_id: self.predecessor_revision_id,
            predecessor_plan_id: self.predecessor_plan_id,
            successor_revision_id: self.successor_revision_id,
            successor_plan_id: self.successor_plan_id,
            predecessor_round_id: self.predecessor_round_id,
            successor_round_id: self.successor_round_id,
            evidence: TaskReviewHandoffEvidenceV1::IntegratedRound {
                report_event_id,
                phase_id,
                integration_committed_event_id,
            },
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let TaskReviewHandoffEvidenceV2::IntegratedRound {
            report_event_id, ..
        } = &self.evidence;
        require(
            super::review_integration::event_id(report_event_id),
            "Integrated handoff requires its exact passing report",
        )?;
        self.clone().into_handoff().validate_identity()
    }
}
