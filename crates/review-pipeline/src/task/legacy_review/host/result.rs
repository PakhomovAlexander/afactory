//! Finish from common Store evidence, including a crash after the canonical Round conclusion.
//! This path never dispatches the scheduler or a Worker and can read a closed Review Round.

use super::*;
use review_core::task::report::*;
use review_core::task::{TaskAcceptanceV1, TaskExecutionV1};
use review_graph::{NodeOutcome, RunReport, SuppressionReason};

impl LegacyReviewTaskHost<'_, '_> {
    pub fn assemble_recorded_result(&self, cas: &Cas) -> Result<TaskResultV1, String> {
        let state = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .task_projection(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or("Review Task has no recorded state")?;
        if state.revision != self.task || state.plan_id.as_deref() != Some(&self.plan_id) {
            return Err("Review Task changed before conclusion publication".into());
        }
        let execution = state
            .execution
            .as_ref()
            .ok_or("Review Task has no execution")?;
        if !execution.pending_attempts().is_empty() {
            return Err("Review Task cannot conclude with outstanding Attempts".into());
        }
        let report_id = state
            .run_reports
            .last()
            .ok_or("Review Task has no durable scheduler report")?;
        let artifact = cas.get_artifact(report_id).map_err(|e| e.to_string())?;
        if artifact.artifact_type != TASK_RUN_REPORT_V1 {
            return Err("Review Task report has another type".into());
        }
        let report: TaskRunReportV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        report.validate()?;
        if report.task_revision_id != state.revision_id || report.plan_id != self.plan_id {
            return Err("Review Task scheduler report belongs to another revision or plan".into());
        }
        let mut outcomes = BTreeMap::new();
        let mut resources_failed = execution.budget.breached();
        for entry in &report.nodes {
            let outcome = match &entry.outcome {
                TaskNodeOutcomeV1::Completed { output_id } => {
                    let output: TaskOutputV1 = serde_json::from_value(
                        cas.get_artifact(output_id)
                            .map_err(|e| e.to_string())?
                            .payload,
                    )
                    .map_err(|e| e.to_string())?;
                    if execution.outputs.get(&entry.node)
                        != Some(&(output_id.clone(), output.clone()))
                    {
                        return Err(
                            "Review scheduler report no longer names the selected output".into(),
                        );
                    }
                    NodeOutcome::Completed {
                        outputs: output
                            .outputs
                            .into_iter()
                            .map(|(port, value)| (port, value.artifact_ids))
                            .collect(),
                    }
                }
                TaskNodeOutcomeV1::Failed {
                    diagnostic_id,
                    class,
                } => {
                    if *class == TaskFailureClassV1::DomainPublication {
                        return Err(
                            "Review must recover domain publication before finalization".into()
                        );
                    }
                    let value = cas.get_artifact(diagnostic_id).map_err(|e| e.to_string())?;
                    if value.artifact_type != TASK_DIAGNOSTIC_V1 {
                        return Err("Review failure has another diagnostic type".into());
                    }
                    let diagnostic: TaskDiagnosticV1 =
                        serde_json::from_value(value.payload).map_err(|e| e.to_string())?;
                    diagnostic.validate()?;
                    resources_failed |= *class == TaskFailureClassV1::Resources;
                    NodeOutcome::Failed {
                        error: diagnostic.message,
                        class: (*class == TaskFailureClassV1::Resources)
                            .then_some(review_graph::NodeFailureClass::RunBudgetExhausted),
                    }
                }
                TaskNodeOutcomeV1::Suppressed { reason } => NodeOutcome::Suppressed {
                    reason: match reason {
                        TaskSuppressionV1::BranchNotSelected => {
                            SuppressionReason::BranchNotSelected
                        }
                        TaskSuppressionV1::GateBlocked => SuppressionReason::GateBlocked,
                        TaskSuppressionV1::UpstreamMissing => SuppressionReason::UpstreamMissing,
                    },
                },
            };
            outcomes.insert(entry.node.clone(), outcome);
        }
        if outcomes.keys().ne(execution.graph.nodes.keys()) {
            return Err("Review report omits a compiled Task node".into());
        }
        let complete = outcomes
            .values()
            .all(|outcome| matches!(outcome, NodeOutcome::Completed { .. }));
        let mut original = RunReport {
            outcomes: vec![],
            blocked_gates: Default::default(),
        };
        for name in &self.captured.loaded.planned().order {
            let mapping = &self.captured.compilation.nodes[name];
            let outcome = outcomes[&mapping.task_node].clone();
            let outcome = match outcome {
                NodeOutcome::Completed { outputs } => {
                    let raw = mapping
                        .outputs
                        .iter()
                        .map(|(port, original)| {
                            let ids = outputs.get(port).map(Vec::as_slice).unwrap_or_default();
                            Ok((
                                original.review_port.clone(),
                                ids.iter()
                                    .map(|id| original.codec.restore(cas, id))
                                    .collect::<Result<Vec<_>, String>>()?,
                            ))
                        })
                        .collect::<Result<ArtifactMap, String>>()?;
                    if self.captured.loaded.planned().nodes[name].kind == NodeKind::Gate {
                        let id = &raw.values().next().ok_or("Gate has no decision")?[0];
                        let decision: review_check::GateDecision =
                            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                                .map_err(|e| e.to_string())?;
                        if !decision.passed() {
                            original.blocked_gates.insert(name.clone());
                        }
                    }
                    NodeOutcome::Completed { outputs: raw }
                }
                other => other,
            };
            original.outcomes.push((name.clone(), outcome));
        }
        let outputs: Ports = execution
            .graph
            .outputs
            .iter()
            .filter_map(|(name, address)| {
                execution
                    .outputs
                    .get(&address.node)
                    .and_then(|(_, output)| output.outputs.get(&address.port))
                    .map(|port| (name.clone(), port.clone()))
            })
            .collect();
        let evidence = execution
            .graph
            .coverage
            .values()
            .filter_map(|address| {
                execution
                    .outputs
                    .get(&address.node)
                    .and_then(|(_, output)| output.outputs.get(&address.port))
            })
            .flat_map(|port| port.artifact_ids.iter().cloned())
            .collect();
        let missing: std::collections::BTreeSet<_> = self
            .task
            .acceptance
            .keys()
            .filter(|name| !outputs.contains_key(*name))
            .cloned()
            .collect();
        let (verdict, exhausted_at_publication) = self.domain.publish_task_report(
            &original,
            self.compiler
                .mode()?
                .convergence(self.captured.loaded.convergence()),
            report_id,
            &self.lease,
            &self.authority(),
        )?;
        // Publication may observe late charges after the projection read above. Its committed
        // exhausted verdict must also fence Task acceptance and execution classification.
        resources_failed |= exhausted_at_publication;
        let acceptance = if !complete || !missing.is_empty() || resources_failed {
            TaskAcceptanceV1::Inconclusive
        } else if verdict == crate::RunVerdict::Pass {
            TaskAcceptanceV1::Satisfied
        } else {
            TaskAcceptanceV1::Unsatisfied
        };
        let result = TaskResultV1 {
            task_revision_id: state.revision_id,
            execution: if resources_failed {
                TaskExecutionV1::Exhausted
            } else if complete {
                TaskExecutionV1::Completed
            } else {
                TaskExecutionV1::Incomplete
            },
            acceptance,
            domain_conclusion: match acceptance {
                TaskAcceptanceV1::Satisfied => "review_passed",
                TaskAcceptanceV1::Unsatisfied => "review_changes_requested",
                TaskAcceptanceV1::Inconclusive => "review_incomplete",
            }
            .into(),
            outputs,
            evidence,
            missing_obligations: missing,
        };
        result.validate()?;
        *self.result.lock().expect("Review result") = Some((report_id.clone(), result.clone()));
        Ok(result)
    }
}
