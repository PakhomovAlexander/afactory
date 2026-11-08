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

/// The durable conclusion of one Review Round, bound to the Task that ran it.
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
        self.validate_outcomes()?;
        self.validate_execution(!matches!(self.verdict, RunVerdictV3::Incomplete { .. }))
    }

    /// Node outcomes, blocked Gates and the verdict must describe one consistent conclusion.
    fn validate_outcomes(&self) -> Result<(), String> {
        if self.outcomes.is_empty() {
            return Err("a run report must contain at least one node outcome".into());
        }
        let mut nodes = std::collections::BTreeSet::new();
        for outcome in &self.outcomes {
            if outcome.node.trim().is_empty() {
                return Err("a run report contains an empty node id".into());
            }
            if !nodes.insert(outcome.node.as_str()) {
                return Err(format!(
                    "a run report contains duplicate outcome for node `{}`",
                    outcome.node
                ));
            }
            match &outcome.outcome {
                RunNodeOutcomeV2::Completed { output_artifacts } => {
                    let unique: std::collections::BTreeSet<&str> =
                        output_artifacts.iter().map(String::as_str).collect();
                    if unique.len() != output_artifacts.len() {
                        return Err(format!(
                            "completed node `{}` contains duplicate output artifacts",
                            outcome.node
                        ));
                    }
                    if let Some(artifact) = output_artifacts.iter().find(|id| !crate::is_digest(id))
                    {
                        return Err(format!(
                            "completed node `{}` contains invalid artifact id `{artifact}`",
                            outcome.node
                        ));
                    }
                }
                RunNodeOutcomeV2::Failed { error } if error.trim().is_empty() => {
                    return Err(format!("failed node `{}` has an empty error", outcome.node));
                }
                _ => {}
            }
        }
        if let Some(gate) = self
            .blocked_gates
            .iter()
            .find(|gate| gate.trim().is_empty())
        {
            return Err(format!("a run report contains empty blocked gate `{gate}`"));
        }
        let blocked: std::collections::BTreeSet<&str> =
            self.blocked_gates.iter().map(String::as_str).collect();
        if blocked.len() != self.blocked_gates.len() {
            return Err("a run report contains duplicate blocked gates".into());
        }
        if let Some(gate) = blocked.iter().find(|gate| !nodes.contains(**gate)) {
            return Err(format!(
                "blocked gate `{gate}` has no corresponding node outcome"
            ));
        }
        let unresolved: std::collections::BTreeSet<&str> = self
            .outcomes
            .iter()
            .filter_map(|outcome| match &outcome.outcome {
                RunNodeOutcomeV2::Completed { .. } => None,
                RunNodeOutcomeV2::Failed { .. } | RunNodeOutcomeV2::Suppressed { .. } => {
                    Some(outcome.node.as_str())
                }
            })
            .collect();
        match &self.verdict {
            // Only an exhausted budget may conclude with unresolved nodes. Every other terminal
            // verdict, including an authority failure, must have resolved each node.
            RunVerdictV3::Pass
            | RunVerdictV3::Fail {
                reason: RunFailureReasonV3::NotConverged | RunFailureReasonV3::AuthorityUnavailable,
            } if !unresolved.is_empty() => {
                Err("a terminal pass/fail report cannot contain failed or suppressed nodes".into())
            }
            RunVerdictV3::Pass if !self.blocked_gates.is_empty() => {
                Err("a passing report cannot contain blocked gates".into())
            }
            RunVerdictV3::Incomplete { missing_nodes } => {
                if missing_nodes.is_empty() {
                    return Err("an incomplete report must name at least one missing node".into());
                }
                if let Some(missing) = missing_nodes
                    .iter()
                    .find(|missing| missing.reason.trim().is_empty())
                {
                    return Err(format!(
                        "missing node `{}` has an empty reason",
                        missing.node
                    ));
                }
                let missing: std::collections::BTreeSet<&str> = missing_nodes
                    .iter()
                    .map(|missing| missing.node.as_str())
                    .collect();
                if missing.len() != missing_nodes.len() {
                    return Err("an incomplete report contains duplicate missing nodes".into());
                }
                if missing != unresolved {
                    return Err(
                        "an incomplete report's missing nodes must match failed and suppressed outcomes"
                            .into(),
                    );
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Every recorded Gate execution fact must belong to a reported node. A `complete` report
    /// also carries the evidence its execution variant promises. An incomplete one keeps the
    /// captured variant even when no Gate started or only part of its facts exist; Store checks
    /// coverage against the plan, outcomes and durable execution receipts.
    fn validate_execution(&self, complete: bool) -> Result<(), String> {
        let bindings = self.execution.bindings();
        if complete
            && !matches!(self.execution, RunReportExecutionV6::Unbound {})
            && bindings.is_empty()
        {
            return Err("RunReport@6 must contain at least one resolved execution binding".into());
        }
        let outcome_nodes: std::collections::BTreeSet<&str> = self
            .outcomes
            .iter()
            .map(|outcome| outcome.node.as_str())
            .collect();
        let mut binding_nodes = std::collections::BTreeSet::new();
        for binding in bindings {
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
        let RunReportExecutionV6::Cached {
            cache_snapshots,
            cache_failures,
            ..
        } = &self.execution
        else {
            return Ok(());
        };
        if complete && cache_snapshots.is_empty() && cache_failures.is_empty() {
            return Err("RunReport@6 must contain Cache Snapshot or failure evidence".into());
        }
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
        Ok(())
    }
}
