//! Captured post-Round Integration data. Only the protected Store transition activates work.
use super::{is_name, require, safe_number};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const TASK_REVIEW_CHECK_SEQUENCE_POLICY_V1: &str = "af/TaskReviewCheckSequencePolicy@1";
pub const TASK_REVIEW_INTEGRATION_PHASE_V1: &str = "af/TaskReviewIntegrationPhase@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewCheckSequencePolicyV1 {
    pub authority_policy_id: String,
    pub pipeline_policy_id: String,
    /// The pipeline artifact declares the Gate binding in this generation; no separate
    /// binding artifact is invented. Compilation rederives the exact binding from it.
    pub gate_execution_policy_id: String,
    pub ordered_check_names: Vec<String>,
    pub check_timeout_ms: u64,
}
impl TaskReviewCheckSequencePolicyV1 {
    pub fn artifact_refs(&self) -> Vec<String> {
        BTreeSet::from([
            self.authority_policy_id.clone(),
            self.pipeline_policy_id.clone(),
        ])
        .into_iter()
        .collect()
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.artifact_refs().iter().all(|id| crate::is_digest(id))
                && self.gate_execution_policy_id == self.pipeline_policy_id,
            "Check sequence requires exact authority and captured Gate policy",
        )?;
        let mut names = BTreeSet::new();
        require(
            !self.ordered_check_names.is_empty()
                && self.ordered_check_names.len() <= 63
                && self
                    .ordered_check_names
                    .iter()
                    .all(|n| !n.trim().is_empty() && n.len() <= 256 && names.insert(n))
                && self.check_timeout_ms > 0
                && safe_number(self.check_timeout_ms),
            "Check sequence requires unique bounded names and a positive timeout",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReviewIntegrationSelectionV1 {
    Empty {},
    Conflict {
        conflict: crate::IntegrationConflictPayloadV1,
    },
    Prepared {
        integration_plan_id: String,
        derived_snapshot_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewIntegrationPhaseV1 {
    pub task_id: String,
    pub task_revision_id: String,
    pub plan_id: String,
    pub round_id: String,
    pub closing_report_event_id: String,
    pub selection: TaskReviewIntegrationSelectionV1,
}
impl TaskReviewIntegrationPhaseV1 {
    pub fn artifact_refs(&self) -> Vec<String> {
        let mut refs = vec![
            self.task_revision_id.clone(),
            self.plan_id.clone(),
            self.round_id.clone(),
        ];
        match &self.selection {
            TaskReviewIntegrationSelectionV1::Empty {} => {}
            TaskReviewIntegrationSelectionV1::Conflict { conflict } => {
                refs.push(conflict.base_snapshot_id.clone());
                refs.extend(conflict.proposal_ids.iter().cloned());
            }
            TaskReviewIntegrationSelectionV1::Prepared {
                integration_plan_id,
                derived_snapshot_id,
            } => {
                refs.extend([integration_plan_id.clone(), derived_snapshot_id.clone()]);
            }
        }
        refs.sort();
        refs.dedup();
        refs
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.task_id)
                && self.artifact_refs().iter().all(|id| crate::is_digest(id))
                && event_id(&self.closing_report_event_id),
            "Integration phase requires exact Task, plan and closing report",
        )?;
        if let TaskReviewIntegrationSelectionV1::Conflict { conflict } = &self.selection {
            conflict.validate()?;
        }
        Ok(())
    }
}

pub(super) fn event_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}
