//! Finish from common Store evidence, including a crash after the canonical Round conclusion.
//! This path never dispatches the scheduler or a Worker and can read a closed Review Round.

use super::*;
use review_core::task::report::*;
use review_core::task::{TaskAcceptanceV1, TaskExecutionV1};
use review_graph::{NodeOutcome, RunReport, SuppressionReason};

/// A durable Round conclusion, separate from finishing its Campaign's Task. These public
/// facts aid presentation; only the Store can authorize a successor Round from exact evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedReviewRoundConclusion {
    pub task_revision_id: String,
    pub plan_id: String,
    pub task_report_id: String,
    pub canonical_report_event_id: String,
    pub verdict: crate::RunVerdict,
    pub resources_failed: bool,
    pub can_continue: bool,
    result: TaskResultV1,
}

impl LegacyReviewTaskHost<'_, '_> {
    /// Compatibility entry point for callers that are finishing this Task. Heavy callers
    /// publish a Round conclusion first and separately choose continuation or finalization.
    pub fn assemble_recorded_result(&self, cas: &Cas) -> Result<TaskResultV1, String> {
        let conclusion = self.publish_recorded_round_conclusion(cas)?;
        if conclusion.can_continue {
            return Err(
                "Heavy Review has another permitted Round; publish the Round conclusion and continue the Task before assembling its final result".into(),
            );
        }
        let result = conclusion.result;
        *self.result.lock().expect("Review result") =
            Some((conclusion.task_report_id, result.clone()));
        Ok(result)
    }

    /// Record or recover this Round's canonical conclusion without authorizing Task finish.
    /// Reopen after a closed Round reads the exact prior report and never dispatches work.
    pub fn publish_recorded_round_conclusion(
        &self,
        cas: &Cas,
    ) -> Result<RecordedReviewRoundConclusion, String> {
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
        let canonical = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .replay(&self.domain.run_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|event| {
                event.event_type == EventType::RunReportV6
                    && event.causation_id.as_ref() == Some(&self.domain.authority.round_event_id)
                    && event.payload["task_accounting"]["task_report_id"].as_str()
                        == Some(report_id)
            })
            .ok_or("Review conclusion has no exact durable canonical report")?;
        let canonical_report: review_core::RunReportPayloadV6 =
            serde_json::from_value(canonical.payload).map_err(|e| e.to_string())?;
        canonical_report.validate()?;
        if canonical_report.task_accounting.task_revision_id != state.revision_id
            || canonical_report.task_accounting.plan_id != self.plan_id
        {
            return Err("Canonical Review conclusion changed its Task revision or plan".into());
        }
        let can_continue = !resources_failed
            && self.compiler.mode()? == review_config::captured_review::ReviewMode::Heavy
            && verdict == crate::RunVerdict::Fail(crate::Verdict::NotConverged)
            && self.domain.authority.round < self.captured.loaded.convergence().max_rounds;
        let acceptance = if !complete || !missing.is_empty() || resources_failed {
            TaskAcceptanceV1::Inconclusive
        } else if verdict == crate::RunVerdict::Pass {
            TaskAcceptanceV1::Satisfied
        } else {
            TaskAcceptanceV1::Unsatisfied
        };
        let result = TaskResultV1 {
            task_revision_id: state.revision_id.clone(),
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
        Ok(RecordedReviewRoundConclusion {
            task_revision_id: state.revision_id,
            plan_id: self.plan_id.clone(),
            task_report_id: report_id.clone(),
            canonical_report_event_id: canonical.event_id,
            verdict,
            resources_failed,
            can_continue,
            result,
        })
    }
}
