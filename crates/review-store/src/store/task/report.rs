//! Durable scheduler observations; these do not authorize outputs or change Task acceptance.

use review_core::task::report::*;

use super::*;

/// A scheduler observation of one run. Only an Integration phase report carries a phase.
pub fn read_task_run_report(cas: &Cas, id: &str) -> Result<TaskRunReportV1, StoreError> {
    let frame = cas
        .get_artifact(id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    if frame.artifact_type != TASK_RUN_REPORT_V2 {
        return Err(conflict("Expected a typed Task run report"));
    }
    let report: TaskRunReportV1 = serde_json::from_value(frame.payload)?;
    report.validate().map_err(conflict)?;
    Ok(report)
}

/// A canonical Round report: every compiled node of the original graph, never a phase.
pub(super) fn round_report(cas: &Cas, id: &str) -> Result<TaskRunReportV1, StoreError> {
    let report = read_task_run_report(cas, id)?;
    if report.phase_id.is_some() {
        return Err(conflict(
            "Expected a Round Task run report, not an Integration phase report",
        ));
    }
    Ok(report)
}

/// A report for one activated post-Round Integration phase node.
pub(super) fn phase_report(cas: &Cas, id: &str) -> Result<TaskRunReportV1, StoreError> {
    let report = read_task_run_report(cas, id)?;
    if report.phase_id.is_none() {
        return Err(conflict("Expected an Integration phase Task run report"));
    }
    Ok(report)
}

pub(super) fn references(cas: &Cas, id: &str) -> Result<Vec<String>, StoreError> {
    let report = read_task_run_report(cas, id)?;
    report.validate().map_err(conflict)?;
    for entry in &report.nodes {
        match &entry.outcome {
            TaskNodeOutcomeV1::Completed { output_id } => {
                envelope(cas, output_id, task::execution::TASK_OUTPUT_V1)?;
            }
            TaskNodeOutcomeV1::Failed { diagnostic_id, .. } => {
                let diagnostic: TaskDiagnosticV1 = payload(cas, diagnostic_id, TASK_DIAGNOSTIC_V1)?;
                diagnostic.validate().map_err(conflict)?;
            }
            TaskNodeOutcomeV1::Suppressed { .. } => {}
        }
    }
    Ok(std::iter::once(id.to_string())
        .chain(report.references().into_iter().map(str::to_owned))
        .collect())
}

impl TaskProjection {
    pub fn waiting_for_domain_publication(&self, cas: &Cas) -> Result<bool, StoreError> {
        if self.phase
            != (TaskPhaseV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsHuman,
            })
        {
            return Ok(false);
        }
        let Some(id) = self.run_reports.last() else {
            return Ok(false);
        };
        let report = read_task_run_report(cas, id)?;
        Ok(report.task_revision_id == self.revision_id
            && Some(&report.plan_id) == self.plan_id.as_ref()
            && report.nodes.iter().any(|node| {
                matches!(
                    node.outcome,
                    TaskNodeOutcomeV1::Failed {
                        class: TaskFailureClassV1::DomainPublication,
                        ..
                    }
                )
            }))
    }

    pub(super) fn apply_run_report(&mut self, cas: &Cas, id: &str) -> Result<(), StoreError> {
        let report = read_task_run_report(cas, id)?;
        let execution = self
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task report has no execution"))?;
        if report.task_revision_id != self.revision_id
            || Some(&report.plan_id) != self.plan_id.as_ref()
            || report.through_sequence != self.next_sequence
            || match &report.phase_id {
                None => {
                    execution.active_review_integration().is_some()
                        || !report
                            .nodes
                            .iter()
                            .map(|n| &n.node)
                            .eq(execution.graph.order.iter())
                }
                Some(phase) => execution.active_review_integration().is_none_or(|p| {
                    p.phase_id() != phase
                        || p.finished()
                        || report.nodes.len() != 1
                        || report.nodes[0].node != p.node()
                }),
            }
        {
            return Err(conflict(
                "Task run report differs from its current revision, plan, sequence or complete node order",
            ));
        }
        for entry in &report.nodes {
            if let TaskNodeOutcomeV1::Completed { output_id } = &entry.outcome
                && execution.outputs.get(&entry.node).map(|(id, _)| id) != Some(output_id)
            {
                return Err(conflict(
                    "Task report completion has no exact published output",
                ));
            }
        }
        self.recording_report =
            report
                .phase_id
                .is_none()
                .then(|| super::recording::RecordingRecovery {
                    revision_id: report.task_revision_id.clone(),
                    plan_id: report.plan_id.clone(),
                    report_id: id.to_string(),
                    outputs: execution
                        .outputs
                        .iter()
                        .map(|(node, (id, _))| (node.clone(), id.clone()))
                        .collect(),
                });
        self.run_reports.push(id.to_string());
        Ok(())
    }
}

impl EventStore {
    pub fn record_task_run_report(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        report_id: &str,
    ) -> Result<(), StoreError> {
        self.task_change(
            cas,
            lease,
            TaskChangeV1::RunReported {
                report_id: report_id.to_string(),
            },
            now()?,
        )
        .map(|_| ())
    }
}
