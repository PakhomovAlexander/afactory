//! A captured, activated post-Round sequence reuses the common one-node lifecycle.
use super::*;
use review_core::task::report::*;
use review_graph::{NodeKind, NodeOutcome};
use review_store::store::task::review_integration::RegisteredTaskReviewIntegration;

impl<'store, 'host> TaskRuntime<'store, 'host> {
    /// Reopen only this registered phase after its passing Round closed. Ordinary Task
    /// construction keeps the open-Round fence and cannot use this authority for another node.
    pub fn with_review_integration(
        store: SharedEventStore<'store>,
        cas: &'store Cas,
        lease: TaskLease,
        authority: &'host dyn TaskAuthority,
        host: &'host dyn TaskOperatorHost,
        phase: &RegisteredTaskReviewIntegration,
    ) -> Result<Self, String> {
        let (plan, projection) = {
            let locked = store.lock().expect("Task Store");
            let plan = locked
                .check_task_review_integration_current(cas, &lease, phase, authority, false)
                .map_err(|e| e.to_string())?;
            let projection = locked
                .task_projection(cas, lease.task_id())
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Task")?;
            (plan, projection)
        };
        Self::from_captured(
            store,
            cas,
            lease,
            authority,
            host,
            plan,
            projection,
            Some(phase.clone()),
        )
    }

    pub(super) fn check_node_authority(&self, node: &str, dispatching: bool) -> Result<(), String> {
        let locked = self.store.lock().expect("Task Store");
        if let Some(phase) = &self.integration {
            if node != phase.node() {
                return Err("Integration runtime cannot dispatch another node".into());
            }
            locked.check_task_review_integration_current(
                self.cas,
                &self.lease,
                phase,
                self.authority,
                dispatching,
            )
        } else if dispatching {
            locked.check_task_dispatch(self.cas, &self.lease, self.authority)
        } else {
            locked.check_current_task_plan_for_recording(self.cas, &self.lease, self.authority)
        }
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Execute exactly the captured sequence with the original Attempt allowance. Returned
    /// report @2 never replaces the original Round report @1 or publishes a canonical commit.
    pub fn execute_review_integration(
        &self,
        phase: &RegisteredTaskReviewIntegration,
    ) -> Result<(String, RunReport), String> {
        if self.integration.as_ref().map(|p| p.phase_id()) != Some(phase.phase_id()) {
            return Err("Runtime was not opened for this Integration phase".into());
        }
        self.check_node_authority(phase.node(), false)?;
        let active = self
            .store
            .lock()
            .expect("Task Store")
            .registered_task_review_integration(self.cas, self.lease.task_id())
            .map_err(|e| e.to_string())?
            .ok_or("Integration phase disappeared")?;
        if !active.requires_checks() {
            return Err("Empty or conflicting Integration has no check Attempt".into());
        }
        if let Some(id) = active.report_id() {
            return Ok((id.into(), self.restore_phase_report(id)?));
        }
        // A crash after report publication but before canonical finish also reuses that exact report.
        for id in self.projection()?.run_reports.iter().rev() {
            let value = envelope(self.cas, id)?;
            if value.artifact_type == TASK_RUN_REPORT_V2 {
                let report: TaskRunReportV2 =
                    serde_json::from_value(value.payload).map_err(|e| e.to_string())?;
                if report.phase_id == phase.phase_id() {
                    return Ok((id.clone(), self.restore_phase_report(id)?));
                }
            }
        }
        let definition = self.resolve_node(phase.node())?.definition;
        let node = Node::new(phase.node(), NodeKind::Task)
            .accepting_contracts(
                definition
                    .contract
                    .inputs
                    .iter()
                    .map(|(n, p)| owned::port(n, p))
                    .collect(),
            )
            .emitting_contracts(
                definition
                    .contract
                    .outputs
                    .iter()
                    .map(|(n, p)| owned::port(n, p))
                    .collect(),
            );
        let inputs = artifact_map(&phase.inputs().map_err(|e| e.to_string())?);
        let outcome = lease::with_heartbeat_controlled(
            &self.store,
            self.cas,
            &self.lease,
            self.cancellation,
            || {
                self.record_invocation(&node, &inputs)?;
                let outputs = self.run(&node, &inputs)?;
                self.record_outputs(&node, &outputs)?;
                Ok(outputs)
            },
        );
        let outcome = match outcome {
            Ok(outputs) => NodeOutcome::Completed { outputs },
            Err(error) => {
                let class = self.failure_class(&node.id).or_else(|| {
                    self.store
                        .lock()
                        .expect("Task Store")
                        .task_review_integration_resource_refusal(
                            self.cas,
                            &self.lease,
                            phase,
                            self.authority,
                        )
                        .ok()
                        .flatten()
                        .map(|_| NodeFailureClass::RunBudgetExhausted)
                });
                NodeOutcome::Failed { class, error }
            }
        };
        let report = RunReport {
            outcomes: vec![(node.id, outcome)],
            blocked_gates: BTreeSet::new(),
        };
        let id = self.capture_run_report(&report, Some(phase.phase_id()))?;
        Ok((id, report))
    }

    /// Record a later resource observation without rewriting selected Check output or its
    /// original Completed report. Only Store-proven Task deadline/budget loss admits this.
    pub(crate) fn record_integration_resource_refusal(
        &self,
        phase: &RegisteredTaskReviewIntegration,
    ) -> Result<Option<String>, String> {
        let reason = self
            .store
            .lock()
            .expect("Task Store")
            .task_review_integration_resource_refusal(self.cas, &self.lease, phase, self.authority)
            .map_err(|e| e.to_string())?;
        let Some(reason) = reason else {
            return Ok(None);
        };
        for id in self.projection()?.run_reports.iter().rev() {
            let value = envelope(self.cas, id)?;
            if value.artifact_type != TASK_RUN_REPORT_V2 {
                continue;
            }
            let report: TaskRunReportV2 =
                serde_json::from_value(value.payload).map_err(|e| e.to_string())?;
            if report.phase_id == phase.phase_id()
                && matches!(
                    report.nodes[0].outcome,
                    TaskNodeOutcomeV1::Failed {
                        class: TaskFailureClassV1::Resources,
                        ..
                    }
                )
            {
                return Ok(Some(id.clone()));
            }
        }
        let report = RunReport {
            outcomes: vec![(
                phase.node().into(),
                NodeOutcome::Failed {
                    error: format!(
                        "Integration promotion refused after resource authority was lost: {reason}"
                    ),
                    class: Some(NodeFailureClass::RunBudgetExhausted),
                },
            )],
            blocked_gates: BTreeSet::new(),
        };
        self.capture_run_report(&report, Some(phase.phase_id()))
            .map(Some)
    }

    fn restore_phase_report(&self, id: &str) -> Result<RunReport, String> {
        let value = envelope(self.cas, id)?;
        if value.artifact_type != TASK_RUN_REPORT_V2 {
            return Err("Integration report has another type".into());
        }
        let report: TaskRunReportV2 =
            serde_json::from_value(value.payload).map_err(|e| e.to_string())?;
        report.validate()?;
        let entry = &report.nodes[0];
        let outcome = match &entry.outcome {
            TaskNodeOutcomeV1::Completed { output_id } => {
                let output: TaskOutputV1 =
                    serde_json::from_value(envelope(self.cas, output_id)?.payload)
                        .map_err(|e| e.to_string())?;
                NodeOutcome::Completed {
                    outputs: artifact_map(&output.outputs),
                }
            }
            TaskNodeOutcomeV1::Failed {
                diagnostic_id,
                class,
            } => {
                let diagnostic: TaskDiagnosticV1 =
                    serde_json::from_value(envelope(self.cas, diagnostic_id)?.payload)
                        .map_err(|e| e.to_string())?;
                NodeOutcome::Failed {
                    error: diagnostic.message,
                    class: (*class == TaskFailureClassV1::Resources)
                        .then_some(NodeFailureClass::RunBudgetExhausted),
                }
            }
            TaskNodeOutcomeV1::Suppressed { .. } => {
                return Err("Integration report cannot suppress its sequence".into());
            }
        };
        Ok(RunReport {
            outcomes: vec![(entry.node.clone(), outcome)],
            blocked_gates: BTreeSet::new(),
        })
    }
}
