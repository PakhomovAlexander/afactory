//! Durable scheduler observations; these do not authorize outputs or change Task acceptance.

use review_core::task::report::*;

use super::*;

pub(super) fn references(cas: &Cas, id: &str) -> Result<Vec<String>, StoreError> {
    let report: TaskRunReportV1 = payload(cas, id, TASK_RUN_REPORT_V1)?;
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
        let report: TaskRunReportV1 = payload(cas, id, TASK_RUN_REPORT_V1)?;
        report.validate().map_err(conflict)?;
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
        let report: TaskRunReportV1 = payload(cas, id, TASK_RUN_REPORT_V1)?;
        report.validate().map_err(conflict)?;
        let execution = self
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task report has no execution"))?;
        if report.task_revision_id != self.revision_id
            || Some(&report.plan_id) != self.plan_id.as_ref()
            || report.through_sequence != self.next_sequence
            || !report
                .nodes
                .iter()
                .map(|n| &n.node)
                .eq(execution.graph.order.iter())
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
