//! Host observations of one scheduler run. These records explain failures (including failures
//! before an Attempt exists); they never substitute for acceptance or selected output receipts.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{is_name, require, safe_number};
use crate::is_digest;

pub const TASK_RUN_REPORT_V1: &str = "af/TaskRunReport@1";
pub const TASK_DIAGNOSTIC_V1: &str = "af/TaskDiagnostic@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDiagnosticV1 {
    pub message: String,
    pub truncated: bool,
}

impl TaskDiagnosticV1 {
    pub fn capture(message: &str) -> Self {
        Self {
            message: message.chars().take(65_536).collect(),
            truncated: message.chars().nth(65_536).is_some(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.message.chars().count() <= 65_536,
            "Task diagnostic exceeds its bound",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailureClassV1 {
    Execution,
    Resources,
    DomainPublication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSuppressionV1 {
    BranchNotSelected,
    GateBlocked,
    UpstreamMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskNodeOutcomeV1 {
    Completed {
        output_id: String,
    },
    Failed {
        diagnostic_id: String,
        class: TaskFailureClassV1,
    },
    Suppressed {
        reason: TaskSuppressionV1,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskNodeReportV1 {
    pub node: String,
    pub outcome: TaskNodeOutcomeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunReportV1 {
    pub task_revision_id: String,
    pub plan_id: String,
    /// Next Task event sequence observed before publishing this report.
    pub through_sequence: u64,
    /// Every compiled node in plan order, including nodes that never received an Attempt.
    pub nodes: Vec<TaskNodeReportV1>,
}

impl TaskRunReportV1 {
    pub fn references(&self) -> Vec<&str> {
        let mut refs = vec![self.task_revision_id.as_str(), self.plan_id.as_str()];
        for entry in &self.nodes {
            match &entry.outcome {
                TaskNodeOutcomeV1::Completed { output_id } => refs.push(output_id),
                TaskNodeOutcomeV1::Failed { diagnostic_id, .. } => refs.push(diagnostic_id),
                TaskNodeOutcomeV1::Suppressed { .. } => {}
            }
        }
        refs
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.references().into_iter().all(is_digest),
            "Invalid Task run report reference",
        )?;
        require(
            self.through_sequence > 0 && safe_number(self.through_sequence),
            "Invalid Task run report sequence",
        )?;
        require(
            !self.nodes.is_empty() && self.nodes.len() <= 4096,
            "Task run report needs a bounded node list",
        )?;
        let mut names = BTreeSet::new();
        for entry in &self.nodes {
            require(
                entry.node.len() <= 4096
                    && entry.node.split('.').all(is_name)
                    && names.insert(&entry.node),
                "Invalid or duplicate Task report node",
            )?;
        }
        Ok(())
    }
}

/// A report for the exact activated post-Round sequence. Frozen ordinary report membership
/// remains every node in the original Round graph; this generation has one phase node.
pub const TASK_RUN_REPORT_V2: &str = "af/TaskRunReport@2";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunReportV2 {
    pub task_revision_id: String,
    pub plan_id: String,
    pub through_sequence: u64,
    pub phase_id: String,
    pub nodes: Vec<TaskNodeReportV1>,
}
impl TaskRunReportV2 {
    pub fn as_report(&self) -> TaskRunReportV1 {
        TaskRunReportV1 {
            task_revision_id: self.task_revision_id.clone(),
            plan_id: self.plan_id.clone(),
            through_sequence: self.through_sequence,
            nodes: self.nodes.clone(),
        }
    }
    pub fn references(&self) -> Vec<&str> {
        let mut refs = vec![
            self.task_revision_id.as_str(),
            self.plan_id.as_str(),
            self.phase_id.as_str(),
        ];
        for n in &self.nodes {
            match &n.outcome {
                TaskNodeOutcomeV1::Completed { output_id } => refs.push(output_id),
                TaskNodeOutcomeV1::Failed { diagnostic_id, .. } => refs.push(diagnostic_id),
                TaskNodeOutcomeV1::Suppressed { .. } => {}
            }
        }
        refs
    }
    pub fn validate(&self) -> Result<(), String> {
        self.as_report().validate()?;
        require(
            is_digest(&self.phase_id)
                && self.nodes.len() == 1
                && !matches!(self.nodes[0].outcome, TaskNodeOutcomeV1::Suppressed { .. }),
            "Integration report requires its exact phase and one executed or failed node",
        )
    }
}
