use review_core::task::report::*;
use review_graph::{NodeOutcome, SuppressionReason};

use super::*;

impl TaskRuntime<'_, '_> {
    pub(super) fn record_run_report(&self, report: &RunReport) -> Result<(), String> {
        let state = self.projection()?;
        let execution = state
            .execution
            .as_ref()
            .ok_or("Task has no execution to report")?;
        let producer = Producer::KernelOperation {
            run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
            node_id: None,
            operation_id: "task-run-report@1".into(),
        };
        let mut nodes = Vec::new();
        for (node, outcome) in &report.outcomes {
            let outcome = match outcome {
                NodeOutcome::Completed { .. } => TaskNodeOutcomeV1::Completed {
                    output_id: execution
                        .outputs
                        .get(node)
                        .ok_or("Completed node has no published Task output")?
                        .0
                        .clone(),
                },
                NodeOutcome::Failed { error, class } => {
                    let diagnostic_id = self
                        .cas
                        .put_artifact(
                            TASK_DIAGNOSTIC_V1,
                            producer.clone(),
                            vec![],
                            None,
                            serde_json::to_value(TaskDiagnosticV1::capture(error))
                                .map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| e.to_string())?
                        .0;
                    TaskNodeOutcomeV1::Failed {
                        diagnostic_id,
                        class: if self
                            .publication_failures
                            .lock()
                            .expect("Task publication failures")
                            .contains(node)
                        {
                            TaskFailureClassV1::DomainPublication
                        } else if *class == Some(NodeFailureClass::RunBudgetExhausted) {
                            TaskFailureClassV1::Resources
                        } else {
                            TaskFailureClassV1::Execution
                        },
                    }
                }
                NodeOutcome::Suppressed { reason } => TaskNodeOutcomeV1::Suppressed {
                    reason: match reason {
                        SuppressionReason::BranchNotSelected => {
                            TaskSuppressionV1::BranchNotSelected
                        }
                        SuppressionReason::GateBlocked => TaskSuppressionV1::GateBlocked,
                        SuppressionReason::UpstreamMissing => TaskSuppressionV1::UpstreamMissing,
                    },
                },
            };
            nodes.push(TaskNodeReportV1 {
                node: node.clone(),
                outcome,
            });
        }
        let value = TaskRunReportV1 {
            task_revision_id: state.revision_id,
            plan_id: self.plan_id.clone(),
            through_sequence: state.next_sequence,
            nodes,
        };
        value.validate()?;
        let id = self
            .cas
            .put_artifact(
                TASK_RUN_REPORT_V1,
                producer,
                value
                    .references()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                None,
                serde_json::to_value(&value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        self.store
            .lock()
            .expect("Task Store")
            .record_task_run_report(self.cas, &self.lease, &id)
            .map_err(|e| e.to_string())
    }
}
