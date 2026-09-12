//! Task-backed canonical conclusions retain an exact accounting prefix and wide lifetime charge.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewAccountingV1 {
    pub task_id: String,
    pub task_revision_id: String,
    pub plan_id: String,
    pub task_report_id: String,
    pub through_sequence: u64,
}
impl TaskReviewAccountingV1 {
    pub fn artifact_refs(&self) -> [&str; 3] {
        [&self.task_revision_id, &self.plan_id, &self.task_report_id]
    }
    pub fn validate(&self) -> Result<(), String> {
        if !crate::task::is_name(&self.task_id)
            || !self.artifact_refs().into_iter().all(crate::is_digest)
            || self.through_sequence > crate::json::SAFE_INTEGER_MAX as u64
        {
            return Err("Task Review accounting requires an exact Task and log prefix".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunReportExecutionV6 {
    Unbound {},
    Bound {
        execution_bindings: Vec<RunExecutionBindingV4>,
    },
    Cached {
        execution_bindings: Vec<RunExecutionBindingV4>,
        cache_snapshots: Vec<RunCacheSnapshotV5>,
        cache_failures: Vec<RunCacheFailureV5>,
    },
}
impl RunReportExecutionV6 {
    pub fn bindings(&self) -> &[RunExecutionBindingV4] {
        match self {
            Self::Unbound {} => &[],
            Self::Bound { execution_bindings }
            | Self::Cached {
                execution_bindings, ..
            } => execution_bindings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReportPayloadV6 {
    pub outcomes: Vec<RunNodeReportV2>,
    pub blocked_gates: Vec<String>,
    pub verdict: RunVerdictV3,
    pub spent_tokens: crate::task::usage::DecimalU128,
    pub task_accounting: TaskReviewAccountingV1,
    pub execution: RunReportExecutionV6,
}
impl RunReportPayloadV6 {
    pub fn validate(&self) -> Result<(), String> {
        self.task_accounting.validate()?;
        if matches!(&self.verdict, RunVerdictV3::Incomplete { .. }) {
            RunReportPayloadV3 {
                outcomes: self.outcomes.clone(),
                blocked_gates: self.blocked_gates.clone(),
                verdict: self.verdict.clone(),
                spent_tokens: None,
            }
            .validate()?;
            return self.validate_incomplete_execution();
        }
        match &self.execution {
            RunReportExecutionV6::Unbound {} => RunReportPayloadV3 {
                outcomes: self.outcomes.clone(),
                blocked_gates: self.blocked_gates.clone(),
                verdict: self.verdict.clone(),
                spent_tokens: None,
            }
            .validate(),
            RunReportExecutionV6::Bound { execution_bindings } => RunReportPayloadV4 {
                outcomes: self.outcomes.clone(),
                blocked_gates: self.blocked_gates.clone(),
                verdict: self.verdict.clone(),
                spent_tokens: None,
                execution_bindings: execution_bindings.clone(),
            }
            .validate(),
            RunReportExecutionV6::Cached {
                execution_bindings,
                cache_snapshots,
                cache_failures,
            } => RunReportPayloadV5 {
                outcomes: self.outcomes.clone(),
                blocked_gates: self.blocked_gates.clone(),
                verdict: self.verdict.clone(),
                spent_tokens: None,
                execution_bindings: execution_bindings.clone(),
                cache_snapshots: cache_snapshots.clone(),
                cache_failures: cache_failures.clone(),
            }
            .validate(),
        }
    }

    fn validate_incomplete_execution(&self) -> Result<(), String> {
        // An unstarted Gate has no binding or cache observation to report. Keep the
        // captured execution variant and validate every fact that was recorded; Store
        // checks coverage against the plan, outcomes and durable execution receipts.
        let outcome_nodes: std::collections::BTreeSet<&str> = self
            .outcomes
            .iter()
            .map(|outcome| outcome.node.as_str())
            .collect();
        let mut binding_nodes = std::collections::BTreeSet::new();
        for binding in self.execution.bindings() {
            binding.validate()?;
            if !binding_nodes.insert(binding.node.as_str()) {
                return Err("RunReport@6 contains a duplicate binding node".into());
            }
            if !outcome_nodes.contains(binding.node.as_str()) {
                return Err(format!(
                    "RunReport@6 binding node `{}` has no corresponding outcome",
                    binding.node
                ));
            }
        }
        if let RunReportExecutionV6::Cached {
            cache_snapshots,
            cache_failures,
            ..
        } = &self.execution
        {
            let mut identities = std::collections::BTreeSet::new();
            for snapshot in cache_snapshots {
                snapshot.validate()?;
                if !binding_nodes.contains(snapshot.node.as_str()) {
                    return Err(format!(
                        "RunReport@6 Cache Snapshot node `{}` has no execution binding",
                        snapshot.node
                    ));
                }
                if !identities.insert((snapshot.node.as_str(), snapshot.kind)) {
                    return Err("RunReport@6 contains a duplicate Cache Snapshot identity".into());
                }
            }
            for failure in cache_failures {
                failure.validate()?;
                if !binding_nodes.contains(failure.node.as_str()) {
                    return Err(format!(
                        "RunReport@6 Cache failure node `{}` has no execution binding",
                        failure.node
                    ));
                }
                if !identities.insert((failure.node.as_str(), failure.kind)) {
                    return Err("RunReport@6 contains a duplicate Cache result identity".into());
                }
                if !self.outcomes.iter().any(|outcome| {
                    outcome.node == failure.node
                        && matches!(&outcome.outcome, RunNodeOutcomeV2::Failed { .. })
                }) {
                    return Err(format!(
                        "RunReport@6 Cache failure node `{}` does not have a failed outcome",
                        failure.node
                    ));
                }
            }
        }
        Ok(())
    }
}
