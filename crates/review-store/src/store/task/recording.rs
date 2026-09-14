//! Additive recovery of an expired publication pause. The original deadline permanently
//! fences every ordinary invocation, reservation and operation; only the already-published
//! outputs at the failed-report prefix receive this recording capability.
use super::*;
use task::report::{TaskFailureClassV1, TaskNodeOutcomeV1};

#[derive(Debug, Clone)]
pub(super) struct RecordingRecovery {
    pub(super) revision_id: String,
    pub(super) plan_id: String,
    pub(super) report_id: String,
    pub(super) outputs: BTreeMap<String, String>,
}

impl TaskProjection {
    pub(super) fn recording_recovery_refs(&self, cas: &Cas) -> Result<Vec<String>, StoreError> {
        let Some(recovery) = &self.recording_recovery else {
            return Ok(Vec::new());
        };
        let mut refs = report::references(cas, &recovery.report_id)?;
        for id in recovery.outputs.values() {
            envelope(cas, id, task::execution::TASK_OUTPUT_V1)?;
            refs.push(id.clone());
        }
        Ok(refs)
    }

    pub fn has_recording_recovery(&self) -> bool {
        self.recording_recovery.as_ref().is_some_and(|r| {
            r.revision_id == self.revision_id && Some(&r.plan_id) == self.plan_id.as_ref()
        })
    }

    pub(super) fn validate_recording_resume(
        &self,
        cas: &Cas,
        revision_id: &str,
        plan_id: &str,
        report_id: &str,
        time: u64,
    ) -> Result<RecordingRecovery, StoreError> {
        if !self.admitted
            || self.phase
                != (TaskPhaseV1::Waiting {
                    reason: TaskWaitingReasonV1::NeedsHuman,
                })
            || self.resume_phase != Some(TaskPhaseV1::Running {})
            || time < self.revision.limits.deadline_unix_ms
            || self.revision_id != revision_id
            || self.plan_id.as_deref() != Some(plan_id)
            || self.run_reports.last().map(String::as_str) != Some(report_id)
        {
            return Err(conflict(
                "Recording recovery requires the exact expired admitted publication pause",
            ));
        }
        let (report, phase) = read_task_run_report(cas, report_id)?;
        let recorded = self
            .recording_report
            .as_ref()
            .filter(|r| {
                r.revision_id == revision_id && r.plan_id == plan_id && r.report_id == report_id
            })
            .ok_or_else(|| {
                conflict("Recording recovery has no exact failed-report output prefix")
            })?;
        let execution = self
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Recording recovery has no execution"))?;
        if phase.is_some()
            || execution.active_review_integration().is_some()
            || report.task_revision_id != revision_id
            || report.plan_id != plan_id
            || !execution.pending_attempts().is_empty()
            || !report.nodes.iter().any(|node| {
                matches!(
                    node.outcome,
                    TaskNodeOutcomeV1::Failed {
                        class: TaskFailureClassV1::DomainPublication,
                        ..
                    }
                ) && execution.outputs.get(&node.node).is_some_and(|(id, _)| {
                    recorded.outputs.get(&node.node) == Some(id)
                        && execution
                            .reusable_output(&node.node)
                            .is_some_and(|(selected, _)| selected == *id)
                })
            })
        {
            return Err(conflict(
                "Recording recovery needs an already-published selected output at the failed report",
            ));
        }
        // Capture at RunReported, not at the later pause: intervening pre-deadline
        // publication cannot expand the outputs authorized by this failed-report prefix.
        Ok(recorded.clone())
    }

    pub(super) fn check_recorded_output(
        &self,
        cas: &Cas,
        output_id: &str,
    ) -> Result<(), StoreError> {
        let recovery = self
            .recording_recovery
            .as_ref()
            .filter(|_| self.has_recording_recovery())
            .ok_or_else(|| conflict("Task has no exact recording recovery capability"))?;
        // Retain the report's identity on cached and fresh projections alike.
        let (report, phase) = read_task_run_report(cas, &recovery.report_id)?;
        if phase.is_some()
            || report.task_revision_id != recovery.revision_id
            || report.plan_id != recovery.plan_id
        {
            return Err(conflict("Recording recovery report changed identity"));
        }
        let execution = self
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Recording recovery has no execution"))?;
        let (node, _) = recovery
            .outputs
            .iter()
            .find(|(_, id)| id.as_str() == output_id)
            .ok_or_else(|| conflict("Output was not published at the recording recovery prefix"))?;
        if execution.outputs.get(node).map(|(id, _)| id.as_str()) != Some(output_id) {
            return Err(conflict("Recording recovery output changed identity"));
        }
        Ok(())
    }
}

impl EventStore {
    /// This additive transition is available only after the immutable execution deadline.
    /// It cannot resume an ordinary pause or authorize any new work, including pure work.
    pub fn resume_task_for_recording(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let plan_id = state
            .plan_id
            .as_deref()
            .ok_or_else(|| conflict("Task has no plan"))?;
        let report_id = state
            .run_reports
            .last()
            .ok_or_else(|| conflict("Task has no failed publication report"))?;
        let time = now()?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        self.authorized_plan(cas, &state, plan_id, authority)?;
        state.check_plan_decision(cas, time)?;
        if let Some(decision) = state.decisions.get(plan_id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        state.validate_recording_resume(cas, &state.revision_id, plan_id, report_id, time)?;
        if let Some(round) = review_round::ReviewRoundFence::for_state(cas, &state)? {
            round.validate(&self.conn)?;
        }
        // The prefix comparison, decision/lease expiry and Round fence are repeated by
        // the append transaction. A callback cannot substitute another plan or report.
        self.append_task_transition_with_owned_prefix(
            cas,
            lease.task_id(),
            TaskTransitionV1 {
                writer: lease.writer.clone(),
                epoch: lease.epoch,
                now_unix_ms: now()?,
                change: TaskChangeV1::RecordingResumed {
                    task_revision_id: state.revision_id.clone(),
                    plan_id: plan_id.into(),
                    report_id: report_id.clone(),
                },
            },
            Some((state.next_sequence, None)),
        )
    }

    pub(super) fn checked_recorded_output(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        output_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(TaskProjection, ExecutionPlanV1), StoreError> {
        let (state, plan) = self.checked_task_recording(cas, lease, authority)?;
        state.check_recorded_output(cas, output_id)?;
        Ok((state, plan))
    }

    /// Replay only the invocation of an output pinned by RecordingResumed. This neither
    /// creates an invocation nor dispatches its operator; ordinary dispatch stays expired.
    pub fn check_task_recorded_invocation(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        invocation_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<ExecutionPlanV1, StoreError> {
        let (state, plan) = self.checked_task_recording(cas, lease, authority)?;
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?;
        let (output_id, _) = execution
            .outputs
            .values()
            .find(|(_, out)| out.invocation_id == invocation_id)
            .ok_or_else(|| conflict("Invocation has no pinned published output"))?;
        state.check_recorded_output(cas, output_id)?;
        Ok(plan)
    }
}
