//! Task lifecycle in the common event log. The public JSON contracts are data; the Rust
//! authority interface is implemented only by the trusted host, outside every Worker sandbox.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
use review_core::task::plan::{
    ExecutionPlanV1, GeneratedOriginV1, PlanDecisionKindV1, PlanDecisionV1,
};
use review_core::task::{
    self, TaskAcceptanceV1, TaskPhaseV1, TaskResultV1, TaskRevisionV1, TaskWaitingReasonV1,
};
use review_core::{ArtifactEnvelope, EventType, RunEvent};
use serde::de::DeserializeOwned;
use serde_json::json;

use super::{EventStore, NewEvent, StoreError};
use crate::{Cas, content_id, validate_envelope};

#[cfg(test)]
mod tests;

mod delivery;
pub mod execution;
mod lease;
pub mod planning;
mod recording;
mod report;
pub use report::read_task_run_report;
pub mod review_handoff;
pub mod review_integration;
mod review_round;
pub mod review_round_publication;
pub use review_handoff::read_task_transition;
mod source;

fn conflict(message: impl Into<String>) -> StoreError {
    StoreError::Conflict(message.into())
}
fn now() -> Result<u64, StoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|n| u64::try_from(n.as_millis()).ok())
        .ok_or_else(|| conflict("Host clock is unavailable"))
}

pub fn task_run_id(task_id: &str) -> Result<String, StoreError> {
    if !task::is_name(task_id) {
        return Err(conflict("Invalid Task ID"));
    }
    content_id(&json!({"namespace":"af/task-log/1", "task_id":task_id}))
        .map(|id| format!("task:{id}"))
        .map_err(|e| conflict(e.to_string()))
}

/// A capability returned only after a successful lease transaction. It cannot be deserialized
/// from Worker output. The Store still checks its owner and epoch on every mutation.
#[derive(Debug, Clone)]
pub struct TaskLease {
    task_id: String,
    writer: String,
    epoch: u64,
}

impl TaskLease {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl TaskProjection {
    pub fn lease_until_unix_ms(&self) -> u64 {
        self.lease_until
    }
}

#[derive(Debug, Clone)]
pub struct DeveloperGrant {
    pub developer: String,
    pub authorization_id: String,
    pub valid_until_unix_ms: u64,
}

/// Trusted application boundary, never constructed from a Task, Pipeline, or Worker result.
/// The implementation authenticates the developer through the host and resolves exact captured
/// package authority. Possession of actor strings or serialized PlanDecision does not implement
/// this interface. Every execution adapter must use this same boundary on resume and dispatch.
pub trait TaskAuthority: Sync {
    fn validate_review_integration_selection(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _phase: &task::review_integration::TaskReviewIntegrationPhaseV1,
        _evidence: &review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        Err("Captured Review Integration is not configured".into())
    }
    #[allow(clippy::too_many_arguments)]
    fn validate_review_integration_completion(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _phase: &task::review_integration::TaskReviewIntegrationPhaseV1,
        _report: &task::report::TaskRunReportV2,
        _events: &[NewEvent],
        _evidence: &review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        Err("Captured Review Integration completion is not configured".into())
    }

    /// Recompile both captured Review plans and the successor roots. Runs under the caller's
    /// Store mutex; implementations must be pure and must not re-lock SharedEventStore.
    #[allow(clippy::too_many_arguments)]
    fn validate_review_continuation(
        &self,
        _cas: &Cas,
        _previous: &TaskRevisionV1,
        _next: &TaskRevisionV1,
        _previous_plan: &ExecutionPlanV1,
        _next_plan: &ExecutionPlanV1,
        _handoff: &task::review_handoff::TaskReviewHandoffV1,
    ) -> Result<(), String> {
        Err("Captured Review continuation is not configured".into())
    }
    fn validate_owned_children(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _parent: &task::execution::TaskInvocationV1,
        _children: &task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        Err("Owned Task child admission is not configured".into())
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_owned_completion(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _parent: &task::execution::TaskInvocationV1,
        _children: &task::owned_children::TaskOwnedChildSetV1,
        _facts: &[execution::owned::TaskOwnedChildEvidence],
        _output: &task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        Err("Owned Task completion is not configured".into())
    }
    /// Re-derive named Broker operations from the exact captured Worker policy. Serialized
    /// handle/binding data alone cannot authorize connector access.
    fn validate_broker_binding(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _binding: &review_core::task::broker::TaskBrokerBindingV1,
    ) -> Result<(), String> {
        Err("Task Broker authority admission is not configured".into())
    }
    /// Validate the compiled graph, bindings and exact dependency closure, returning the
    /// generated origins derived from trusted package provenance, including nested Pipelines.
    fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String>;
    /// Trusted retry eligibility over durable failures of this exact invocation. Attempt
    /// count and token capacity alone do not authorize retrying every failure class.
    fn validate_retry(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _invocation: &review_core::task::execution::TaskInvocationV1,
        _previous: &BTreeMap<String, review_core::task::execution::TaskAttemptResultV1>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn authorize_decision(
        &self,
        task: &TaskRevisionV1,
        plan_id: &str,
        decision: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String>;
    fn authorization_current(&self, decision: &PlanDecisionV1) -> Result<(), String>;
    /// Recompute only admitted root input constructors at the preparation/execution barrier.
    fn validate_planning_inputs(
        &self,
        _cas: &Cas,
        _previous: &TaskRevisionV1,
        _next: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
    ) -> Result<(), String> {
        Err("Planning input normalization is not configured".into())
    }
    /// Match the exact rendered context to captured schemas, instructions, invocation and
    /// admitted retry feedback before any Attempt reservation can become executable.
    fn validate_context(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _invocation: &review_core::task::execution::TaskInvocationV1,
        _feedback_ids: &[String],
        _context_id: &str,
    ) -> Result<(), String> {
        Err("Task context admission is not configured".into())
    }
    /// Adapters whose prompt includes Attempt authority validate the exact Store reservation.
    /// This runs after pure capture and before the context can become executable.
    fn validate_context_for_attempt(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        invocation: &review_core::task::execution::TaskInvocationV1,
        attempt: &execution::ReservedTaskAttempt,
        context_id: &str,
    ) -> Result<(), String> {
        self.validate_context(
            cas,
            task,
            plan,
            invocation,
            attempt.feedback_ids(),
            context_id,
        )
    }
    /// Recompute acceptance from exact durable output/verification receipts, not result prose.
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String>;
    /// Domain checks supplement the Store's exact graph-port/provenance checks. The host
    /// verifies typed Worker schemas, seal ancestry and retained verifier receipts here.
    fn validate_output(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _invocation: &review_core::task::execution::TaskInvocationV1,
        _output: &review_core::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        Err("Task output domain admission is not configured".into())
    }
}

#[derive(Debug, Clone)]
struct Decision {
    artifact_id: String,
    value: PlanDecisionV1,
    valid_until: u64,
    revoked: bool,
    event: RunEvent,
}

#[derive(Debug, Clone)]
pub struct TaskProjection {
    pub task_id: String,
    pub revision_id: String,
    pub revision: TaskRevisionV1,
    pub plan_id: Option<String>,
    pub phase: TaskPhaseV1,
    pub admitted: bool,
    pub next_sequence: u64,
    writer: String,
    epoch: u64,
    lease_until: u64,
    last_time: u64,
    resume_phase: Option<TaskPhaseV1>,
    recording_recovery: Option<recording::RecordingRecovery>,
    recording_report: Option<recording::RecordingRecovery>,
    decisions: BTreeMap<String, Decision>,
    // Exact reference closure of the parsed prefix, including superseded execution evidence.
    // This memo saves parsing only: every object is verified again on the next access.
    artifact_refs: BTreeSet<String>,
    pub execution: Option<execution::TaskExecutionProjection>,
    pub planning: Option<planning::TaskPlanningProof>,
    pub deliveries: Vec<(String, task::delivery::TaskDeliveryRecordV1)>,
    pub run_reports: Vec<String>,
    pub review_handoffs: Vec<(String, task::review_handoff::TaskReviewHandoffV1)>,
}

/// Created after replay/transition validation; the shared transaction then compares sequence
/// before append. A racing lease takeover or another writer invalidates the complete batch.
pub(super) struct WritePermit {
    run_id: String,
    first: u64,
    payloads: Vec<serde_json::Value>,
    event_type: EventType,
    valid_until: Option<u64>,
    review_round: Option<review_round::ReviewRoundFence>,
    review_prefix: Option<(String, u64)>,
}

impl WritePermit {
    pub(super) fn validate(
        &self,
        connection: &rusqlite::Connection,
        run_id: &str,
        first: i64,
        events: &[NewEvent],
    ) -> Result<(), StoreError> {
        if self.run_id != run_id
            || u64::try_from(first).ok() != Some(self.first)
            || events.len() != self.payloads.len()
            || events
                .iter()
                .zip(&self.payloads)
                .any(|(e, p)| e.event_type != self.event_type || &e.payload != p)
            || self
                .valid_until
                .is_some_and(|until| now().map_or(true, |time| time >= until))
        {
            return Err(conflict("Task write lost its sequence/lease comparison"));
        }
        if let Some((run_id, expected)) = &self.review_prefix {
            let actual: u64 = connection.query_row(
                "SELECT COALESCE(MAX(sequence)+1,0) FROM events WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            )?;
            if actual != *expected {
                return Err(conflict(
                    "Owned completion lost its canonical Review prefix",
                ));
            }
        }
        if let Some(round) = &self.review_round {
            round.validate(connection)?;
        }
        Ok(())
    }
}

fn envelope(cas: &Cas, id: &str, expected: &str) -> Result<ArtifactEnvelope, StoreError> {
    let envelope = cas
        .get_artifact(id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    if envelope.artifact_id != id || envelope.artifact_type != expected {
        return Err(conflict(format!("Expected exact {expected} artifact {id}")));
    }
    Ok(envelope)
}

fn payload<T: DeserializeOwned>(cas: &Cas, id: &str, expected: &str) -> Result<T, StoreError> {
    Ok(serde_json::from_value(
        envelope(cas, id, expected)?.payload,
    )?)
}

fn revision(cas: &Cas, id: &str) -> Result<TaskRevisionV1, StoreError> {
    let revision: TaskRevisionV1 = payload(cas, id, task::TASK_REVISION_V1)?;
    revision.validate().map_err(conflict)?;
    Ok(revision)
}

fn plan(cas: &Cas, id: &str, state: &TaskProjection) -> Result<ExecutionPlanV1, StoreError> {
    let plan: ExecutionPlanV1 = payload(cas, id, task::EXECUTION_PLAN_V1)?;
    plan.validate().map_err(conflict)?;
    if plan.task_revision_id != state.revision_id
        || plan.authority != state.revision.authority
        || plan.limits != state.revision.limits
        || plan.inputs != state.revision.inputs
        || (plan.preparation.is_none()
            && !plan.acceptance.keys().eq(state.revision.acceptance.keys()))
    {
        return Err(conflict(
            "Plan does not bind the exact Task revision, inputs, authority and limits",
        ));
    }
    Ok(plan)
}

fn validate_input_refs(
    cas: &Cas,
    input: &task::ArtifactInputV1,
    refs: &mut BTreeSet<String>,
) -> Result<(), StoreError> {
    input.validate().map_err(conflict)?;
    for id in &input.artifact_ids {
        let value = envelope(cas, id, &input.artifact_type)?;
        if value.subject_snapshot_id != input.snapshot_id {
            return Err(conflict(
                "Task port Snapshot identity contradicts its artifact envelope",
            ));
        }
        refs.insert(id.clone());
        refs.extend(value.input_artifacts);
    }
    refs.extend(input.snapshot_id.iter().cloned());
    Ok(())
}

fn references(
    cas: &Cas,
    change: &TaskChangeV1,
    state: Option<&TaskProjection>,
) -> Result<Vec<String>, StoreError> {
    let mut refs = BTreeSet::new();
    if let TaskChangeV1::ApprovalRevoked {
        revocation_id: Some(id),
        ..
    } = change
    {
        let value: PlanDecisionV1 = payload(cas, id, task::PLAN_DECISION_V1)?;
        value.validate().map_err(conflict)?;
        refs.extend([
            id.clone(),
            value.task_revision_id,
            value.plan_id,
            value.policy_id,
            value.authorization_id,
        ]);
    }
    match change {
        TaskChangeV1::RecordingResumed { report_id, .. } => {
            refs.extend(report::references(cas, report_id)?);
            let state = state.ok_or_else(|| conflict("Recording recovery precedes Task"))?;
            let execution = state
                .execution
                .as_ref()
                .ok_or_else(|| conflict("Recording recovery has no execution"))?;
            for (id, _) in execution.outputs.values() {
                envelope(cas, id, task::execution::TASK_OUTPUT_V1)?;
                refs.insert(id.clone());
            }
        }
        TaskChangeV1::ReviewIntegrationSelected { phase_id }
        | TaskChangeV1::ReviewIntegrationFinished { phase_id, .. } => {
            let phase = review_integration::read_task_review_integration(cas, phase_id)?;
            refs.insert(phase_id.clone());
            refs.extend(phase.artifact_refs());
            if let TaskChangeV1::ReviewIntegrationFinished { report_id, .. } = change {
                refs.extend(report::references(cas, report_id)?);
            }
        }
        TaskChangeV1::ReviewContinued { handoff_id } => {
            let handoff = review_handoff::read_task_review_handoff(cas, handoff_id)?;
            refs.insert(handoff_id.clone());
            refs.extend(handoff.artifact_refs().into_iter().map(str::to_owned));
            let mut next = state
                .ok_or_else(|| conflict("Review handoff precedes Task"))?
                .clone();
            next.revision = revision(cas, &handoff.successor_revision_id)?;
            next.revision_id = handoff.successor_revision_id.clone();
            refs.extend(references(
                cas,
                &TaskChangeV1::RevisionRecorded {
                    revision_id: handoff.successor_revision_id.clone(),
                },
                None,
            )?);
            refs.extend(references(
                cas,
                &TaskChangeV1::PlanProposed {
                    plan_id: handoff.successor_plan_id.clone(),
                },
                Some(&next),
            )?);
        }
        TaskChangeV1::SourceRefreshed {
            revision_id,
            plan_id,
            ..
        } => {
            let mut next = state
                .ok_or_else(|| conflict("Source refresh precedes Task"))?
                .clone();
            next.revision = revision(cas, revision_id)?;
            next.revision_id = revision_id.clone();
            refs.extend(references(
                cas,
                &TaskChangeV1::RevisionRecorded {
                    revision_id: revision_id.clone(),
                },
                None,
            )?);
            if let Some(plan_id) = plan_id {
                refs.extend(references(
                    cas,
                    &TaskChangeV1::PlanProposed {
                        plan_id: plan_id.clone(),
                    },
                    Some(&next),
                )?);
            }
        }

        TaskChangeV1::PlanningCompleted {
            bootstrap_plan_id,
            proposal_id,
            revision_id,
            plan_id,
        } => {
            let mut next = state
                .ok_or_else(|| conflict("Planning precedes Task"))?
                .clone();
            next.revision = revision(cas, revision_id)?;
            next.revision_id = revision_id.clone();
            refs.extend(references(
                cas,
                &TaskChangeV1::RevisionRecorded {
                    revision_id: revision_id.clone(),
                },
                None,
            )?);
            refs.extend(references(
                cas,
                &TaskChangeV1::PlanProposed {
                    plan_id: plan_id.clone(),
                },
                Some(&next),
            )?);
            refs.extend([bootstrap_plan_id.clone(), proposal_id.clone()]);
        }
        TaskChangeV1::DeliveryRecorded { record_id } => {
            let value: task::delivery::TaskDeliveryRecordV1 =
                payload(cas, record_id, task::delivery::TASK_DELIVERY_RECORD_V1)?;
            value.validate().map_err(conflict)?;
            refs.insert(record_id.clone());
            refs.extend(value.references().into_iter().map(str::to_owned));
        }
        TaskChangeV1::ExecutionRecorded { record_id } => {
            refs.extend(execution::references(cas, record_id)?);
        }
        TaskChangeV1::RunReported { report_id } => {
            refs.extend(report::references(cas, report_id)?);
        }
        TaskChangeV1::Opened { revision_id, .. }
        | TaskChangeV1::RevisionRecorded { revision_id } => {
            let value = revision(cas, revision_id)?;
            refs.insert(revision_id.clone());
            refs.insert(value.authority.policy_id);
            refs.insert(value.provenance.adapter_id);
            refs.extend(value.provenance.input_artifact_ids);
            refs.extend(value.previous_revision_id);
            refs.extend(value.acceptance.values().map(|o| o.verifier_policy.clone()));
            for input in value.inputs.values() {
                validate_input_refs(cas, input, &mut refs)?;
            }
        }
        TaskChangeV1::PlanProposed { plan_id } | TaskChangeV1::PlanAdmitted { plan_id } => {
            let value = plan(
                cas,
                plan_id,
                state.ok_or_else(|| conflict("Plan precedes Task"))?,
            )?;
            refs.extend([
                plan_id.clone(),
                value.task_revision_id,
                value.engine_id,
                value.pipeline_id,
                value.compiled_graph_id,
                value.authority.policy_id,
            ]);
            for dep in value.dependencies.values() {
                let raw = cas
                    .get_json(&dep.artifact_id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))?;
                let dep_envelope: ArtifactEnvelope = serde_json::from_value(raw)?;
                validate_envelope(&dep_envelope).map_err(conflict)?;
                if dep_envelope.artifact_id != dep.artifact_id
                    || dep_envelope.content_id != dep.content_digest
                {
                    return Err(conflict("Plan dependency content disagrees with its lock"));
                }
                refs.insert(dep.artifact_id.clone());
                refs.extend(dep_envelope.input_artifacts);
            }
            for binding in value.bindings.values() {
                refs.extend([
                    binding.package_artifact_id.clone(),
                    binding.invocation_policy_id.clone(),
                ]);
            }
            for origin in value.generated_origins {
                refs.extend([
                    origin.pipeline_id,
                    origin.proposal_id,
                    origin.bootstrap_plan_id,
                ]);
            }
            for input in value.inputs.values() {
                validate_input_refs(cas, input, &mut refs)?;
            }
        }
        TaskChangeV1::PlanDecided { decision_id, .. }
        | TaskChangeV1::ApprovalRevoked { decision_id, .. } => {
            let value: PlanDecisionV1 = payload(cas, decision_id, task::PLAN_DECISION_V1)?;
            value.validate().map_err(conflict)?;
            refs.extend([
                decision_id.clone(),
                value.task_revision_id,
                value.plan_id,
                value.policy_id,
                value.authorization_id,
            ]);
        }
        TaskChangeV1::Finished { result_id } => {
            refs.extend(envelope(cas, result_id, task::TASK_RESULT_V1)?.input_artifacts);
            let result: TaskResultV1 = payload(cas, result_id, task::TASK_RESULT_V1)?;
            result.validate().map_err(conflict)?;
            refs.extend([result_id.clone(), result.task_revision_id]);
            refs.extend(result.evidence);
            for output in result.outputs.values() {
                validate_input_refs(cas, output, &mut refs)?;
            }
        }
        _ => (),
    }
    Ok(refs.into_iter().collect())
}

impl TaskProjection {
    pub fn plan_decision(&self, plan_id: &str) -> Option<PlanDecisionKindV1> {
        self.decisions.get(plan_id).map(|d| d.value.decision)
    }
    fn check_lease(&self, transition: &TaskTransitionV1) -> Result<(), StoreError> {
        if transition.writer != self.writer
            || transition.epoch != self.epoch
            || transition.now_unix_ms >= self.lease_until
            || transition.now_unix_ms < self.last_time
        {
            return Err(conflict("Task writer lease is expired or fenced"));
        }
        Ok(())
    }

    fn apply(
        &mut self,
        cas: &Cas,
        event: &RunEvent,
        transition: &TaskTransitionV1,
    ) -> Result<(), StoreError> {
        if event.sequence != self.next_sequence {
            return Err(conflict("Task event sequence has a gap or duplicate"));
        }
        let accounting_after_finish = match &transition.change {
            TaskChangeV1::LeaseTaken { .. }
            | TaskChangeV1::LeaseRenewed { .. }
            | TaskChangeV1::LeaseReleased {}
            | TaskChangeV1::DeliveryRecorded { .. }
            | TaskChangeV1::SourceRefreshed { .. } => true,
            TaskChangeV1::ExecutionRecorded { record_id } => matches!(
                execution::read_execution_record(cas, record_id)?.record,
                review_core::task::execution::TaskExecutionRecordV1::UsageObserved { .. }
            ),
            _ => false,
        };
        if transition.now_unix_ms < self.last_time
            || (matches!(self.phase, TaskPhaseV1::Finished { .. }) && !accounting_after_finish)
        {
            return Err(conflict(
                "Task is finished or its policy clock moved backwards",
            ));
        }
        if let TaskChangeV1::LeaseTaken {
            lease_until_unix_ms,
        } = transition.change
        {
            if transition.now_unix_ms < self.lease_until
                || self.epoch.checked_add(1) != Some(transition.epoch)
            {
                return Err(conflict(
                    "Task lease takeover is premature or has a stale epoch",
                ));
            }
            self.writer = transition.writer.clone();
            self.epoch = transition.epoch;
            self.lease_until = lease_until_unix_ms;
        } else {
            self.check_lease(transition)?;
            match &transition.change {
                TaskChangeV1::ReviewIntegrationSelected { .. }
                | TaskChangeV1::ReviewIntegrationFinished { .. } => {
                    self.apply_review_integration(cas, &transition.change, transition.now_unix_ms)?;
                }
                TaskChangeV1::ReviewContinued { handoff_id } => {
                    self.apply_review_handoff(cas, handoff_id, transition.now_unix_ms)?;
                }
                TaskChangeV1::SourceRefreshed {
                    revision_id,
                    plan_id,
                    waiting,
                } => {
                    self.apply_source_refreshed(
                        cas,
                        revision_id,
                        plan_id.as_deref(),
                        *waiting,
                        transition.now_unix_ms,
                    )?;
                }
                TaskChangeV1::DeliveryRecorded { record_id } => {
                    self.apply_delivery(cas, record_id)?;
                }
                TaskChangeV1::ExecutionRecorded { record_id } => {
                    self.apply_execution(cas, record_id, transition.now_unix_ms)?;
                }
                TaskChangeV1::RunReported { report_id } => {
                    self.apply_run_report(cas, report_id)?;
                }
                TaskChangeV1::Opened { .. } | TaskChangeV1::LeaseTaken { .. } => {
                    return Err(conflict("Task already exists"));
                }
                TaskChangeV1::LeaseRenewed {
                    lease_until_unix_ms,
                } => {
                    if *lease_until_unix_ms <= self.lease_until {
                        return Err(conflict("Task lease renewal must advance expiry"));
                    }
                    self.lease_until = *lease_until_unix_ms;
                }
                TaskChangeV1::LeaseReleased {} => {
                    if self
                        .execution
                        .as_ref()
                        .is_some_and(|e| !e.pending_attempts().is_empty())
                    {
                        return Err(conflict(
                            "Cannot release a Task lease with pending Attempts",
                        ));
                    }
                    self.lease_until = transition.now_unix_ms;
                }
                TaskChangeV1::RevisionRecorded { revision_id } => {
                    if self.admitted || self.execution.is_some() {
                        return Err(conflict(
                            "An executing Task needs a recorded replan barrier",
                        ));
                    }
                    let next = revision(cas, revision_id)?;
                    if next.task_id != self.task_id
                        || next.previous_revision_id.as_ref() != Some(&self.revision_id)
                        || self.revision.revision.checked_add(1) != Some(next.revision)
                        || next.limits.tokens > self.revision.limits.tokens
                        || next.limits.max_attempts > self.revision.limits.max_attempts
                        || next.limits.deadline_unix_ms > self.revision.limits.deadline_unix_ms
                    {
                        return Err(conflict(
                            "Task revision must retain its predecessor and remaining resource bounds",
                        ));
                    }
                    self.revision_id = revision_id.clone();
                    self.revision = next;
                    self.plan_id = None;
                    self.phase = TaskPhaseV1::Submitted {};
                    self.resume_phase = None;
                }
                TaskChangeV1::PlanProposed { plan_id } => {
                    if self.admitted || self.execution.is_some() {
                        return Err(conflict("Cannot replace an executing Task plan"));
                    }
                    let plan = plan(cas, plan_id, self)?;
                    self.phase = if plan.requires_developer_approval() {
                        TaskPhaseV1::Waiting {
                            reason: TaskWaitingReasonV1::NeedsPlanReview,
                        }
                    } else {
                        TaskPhaseV1::Ready {}
                    };
                    self.plan_id = Some(plan_id.clone());
                    self.resume_phase = None;
                }
                TaskChangeV1::PlanningCompleted {
                    bootstrap_plan_id,
                    proposal_id,
                    revision_id,
                    plan_id,
                } => {
                    self.apply_planning_completed(
                        cas,
                        bootstrap_plan_id,
                        proposal_id,
                        revision_id,
                        plan_id,
                        transition.now_unix_ms,
                    )?;
                }
                TaskChangeV1::PlanDecided {
                    decision_id,
                    valid_until_unix_ms,
                } => {
                    let decision: PlanDecisionV1 =
                        payload(cas, decision_id, task::PLAN_DECISION_V1)?;
                    decision.validate().map_err(conflict)?;
                    if self.admitted
                        || self.plan_id.as_ref() != Some(&decision.plan_id)
                        || decision.task_revision_id != self.revision_id
                        || decision.policy_id != self.revision.authority.policy_id
                        || self.decisions.contains_key(&decision.plan_id)
                    {
                        return Err(conflict(
                            "Developer decision is stale, duplicated or mismatched",
                        ));
                    }
                    self.decisions.insert(
                        decision.plan_id.clone(),
                        Decision {
                            artifact_id: decision_id.clone(),
                            value: decision,
                            valid_until: *valid_until_unix_ms,
                            revoked: false,
                            event: event.clone(),
                        },
                    );
                }
                TaskChangeV1::ApprovalRevoked {
                    decision_id,
                    reason,
                    revocation_id,
                } => {
                    let decision = self
                        .decisions
                        .values_mut()
                        .find(|d| &d.artifact_id == decision_id)
                        .ok_or_else(|| conflict("Unknown Task approval"))?;
                    if let Some(id) = revocation_id {
                        let revocation: PlanDecisionV1 = payload(cas, id, task::PLAN_DECISION_V1)?;
                        revocation.validate().map_err(conflict)?;
                        if revocation.decision != PlanDecisionKindV1::Rejected
                            || revocation.task_revision_id != self.revision_id
                            || revocation.plan_id != decision.value.plan_id
                            || revocation.policy_id != self.revision.authority.policy_id
                            || revocation.reason != *reason
                        {
                            return Err(conflict(
                                "Revocation proof changed its exact plan, Task, authority or reason",
                            ));
                        }
                    }
                    decision.revoked = true;
                    if self.plan_id.as_ref() == Some(&decision.value.plan_id) {
                        self.admitted = false;
                        self.phase = TaskPhaseV1::Waiting {
                            reason: TaskWaitingReasonV1::NeedsPlanReview,
                        };
                        self.resume_phase = None;
                    }
                }
                TaskChangeV1::PlanAdmitted { plan_id } => {
                    if !matches!(
                        self.phase,
                        TaskPhaseV1::Ready {}
                            | TaskPhaseV1::Waiting {
                                reason: TaskWaitingReasonV1::NeedsPlanReview
                            }
                    ) {
                        return Err(conflict(
                            "Task plan admission cannot bypass another pause or restart running work",
                        ));
                    }
                    if self.plan_id.as_ref() != Some(plan_id) {
                        return Err(conflict("Task plan admission is stale"));
                    }
                    self.check_approval(cas, transition.now_unix_ms)?;
                    self.admitted = true;
                    self.phase = TaskPhaseV1::Running {};
                    self.resume_phase = None;
                }
                TaskChangeV1::Waiting { reason } => {
                    if self.resume_phase.is_some() {
                        return Err(conflict("Task is already waiting"));
                    }
                    if *reason == TaskWaitingReasonV1::NeedsPlanReview {
                        return Err(conflict(
                            "Plan waiting is derived from a persisted proposal",
                        ));
                    }
                    self.resume_phase = Some(self.phase.clone());
                    self.phase = TaskPhaseV1::Waiting { reason: *reason };
                }
                TaskChangeV1::Resumed {} => {
                    let prior = self
                        .resume_phase
                        .take()
                        .ok_or_else(|| conflict("Task has no resumable pause"))?;
                    if self.admitted {
                        self.check_approval(cas, transition.now_unix_ms)?;
                    }
                    self.phase = prior;
                }
                TaskChangeV1::RecordingResumed {
                    task_revision_id,
                    plan_id,
                    report_id,
                } => {
                    let recovery = self.validate_recording_resume(
                        cas,
                        task_revision_id,
                        plan_id,
                        report_id,
                        transition.now_unix_ms,
                    )?;
                    self.check_plan_decision(cas, transition.now_unix_ms)?;
                    self.recording_recovery = Some(recovery);
                    self.resume_phase = None;
                    self.phase = TaskPhaseV1::Running {};
                }
                TaskChangeV1::Finished { result_id } => {
                    if self
                        .execution
                        .as_ref()
                        .is_some_and(|execution| !execution.pending_attempts().is_empty())
                    {
                        return Err(conflict(
                            "Task must settle or release every pending Attempt before finishing",
                        ));
                    }
                    let result: TaskResultV1 = payload(cas, result_id, task::TASK_RESULT_V1)?;
                    result.validate().map_err(conflict)?;
                    if result.acceptance == TaskAcceptanceV1::Satisfied
                        && matches!(self.phase, TaskPhaseV1::Waiting { .. })
                    {
                        return Err(conflict("A waiting Task cannot claim satisfied acceptance"));
                    }
                    if result.acceptance == TaskAcceptanceV1::Satisfied
                        && self.has_recording_recovery()
                    {
                        return Err(conflict(
                            "A recording-only Task cannot claim satisfied acceptance",
                        ));
                    }
                    if let Some(execution) = &self.execution {
                        if execution.budget.breached()
                            && (result.execution != task::TaskExecutionV1::Exhausted
                                || result.acceptance == TaskAcceptanceV1::Satisfied)
                        {
                            return Err(conflict(
                                "Task result predates its committed resource exhaustion",
                            ));
                        }
                        let expected_outputs: BTreeMap<_, _> = execution
                            .graph
                            .outputs
                            .iter()
                            .filter_map(|(name, address)| {
                                execution
                                    .outputs
                                    .get(&address.node)
                                    .and_then(|(_, receipt)| receipt.outputs.get(&address.port))
                                    .map(|value| (name.clone(), value.clone()))
                            })
                            .collect();
                        let mut expected_evidence: BTreeSet<_> = execution
                            .graph
                            .coverage
                            .values()
                            .filter_map(|address| {
                                execution
                                    .outputs
                                    .get(&address.node)
                                    .and_then(|(_, receipt)| receipt.outputs.get(&address.port))
                            })
                            .flat_map(|port| port.artifact_ids.iter().cloned())
                            .collect();
                        if let Some(integration) = execution.active_review_integration() {
                            if !integration.finished() {
                                return Err(conflict(
                                    "Task must seal its activated Integration before finishing",
                                ));
                            }
                            expected_evidence.insert(integration.phase_id().to_string());
                            if let Some(report_id) = integration.report_id() {
                                expected_evidence.insert(report_id.to_string());
                            }
                            if let Some((output_id, _)) = execution.outputs.get(integration.node())
                            {
                                expected_evidence.insert(output_id.clone());
                            }
                        } else if execution.graph.review_integration.is_some()
                            && result.acceptance == TaskAcceptanceV1::Satisfied
                        {
                            return Err(conflict(
                                "Task acceptance requires the captured Integration disposition",
                            ));
                        }
                        if result.outputs != expected_outputs
                            || result.evidence != expected_evidence
                        {
                            return Err(conflict(
                                "Task result does not retain its exact admitted public outputs and evidence",
                            ));
                        }
                    }
                    if result.task_revision_id != self.revision_id {
                        return Err(conflict("Task result is stale"));
                    }
                    if result.acceptance == TaskAcceptanceV1::Satisfied {
                        let current = plan(
                            cas,
                            self.plan_id
                                .as_deref()
                                .ok_or_else(|| conflict("Task has no plan"))?,
                            self,
                        )?;
                        if current.preparation.is_some() {
                            return Err(conflict(
                                "Planning cannot satisfy business Task acceptance",
                            ));
                        }
                        if !self.admitted {
                            return Err(conflict("Unadmitted Task cannot satisfy acceptance"));
                        }
                        for (name, required) in &self.revision.required_outputs {
                            let output = result.outputs.get(name).ok_or_else(|| {
                                conflict(format!("Task result is missing {name}"))
                            })?;
                            if output.artifact_type != required.artifact_type
                                || output.cardinality != required.cardinality
                                || output.artifact_ids.is_empty()
                            {
                                return Err(conflict(format!(
                                    "Task result has incompatible output {name}"
                                )));
                            }
                        }
                    }
                    self.phase = TaskPhaseV1::Finished {
                        result_id: result_id.clone(),
                    };
                }
            }
        }
        self.last_time = transition.now_unix_ms;
        self.next_sequence = event.sequence + 1;
        Ok(())
    }

    fn check_approval(&self, cas: &Cas, time: u64) -> Result<(), StoreError> {
        let plan = self.check_plan_decision(cas, time)?;
        if time >= plan.limits.deadline_unix_ms {
            return Err(conflict("Task plan deadline expired"));
        }
        Ok(())
    }

    // Recording a conclusion still requires a current developer decision, but cannot grant
    // another execution effect by extending the plan deadline.
    fn check_plan_decision(&self, cas: &Cas, time: u64) -> Result<ExecutionPlanV1, StoreError> {
        let id = self
            .plan_id
            .as_ref()
            .ok_or_else(|| conflict("Task has no plan"))?;
        let plan = plan(cas, id, self)?;
        if let Some(decision) = self.decisions.get(id) {
            if decision.revoked
                || time >= decision.valid_until
                || !decision.value.approves(id, &plan)
            {
                return Err(conflict(
                    "Task plan approval is rejected, revoked, expired or stale",
                ));
            }
        } else if plan.requires_developer_approval() {
            return Err(conflict("Generated Task plan needs developer review"));
        }
        Ok(plan)
    }
}

impl EventStore {
    /// Enumerate only validated common Task streams; Review run IDs are not Task labels.
    pub fn task_ids(&self, cas: &Cas) -> Result<Vec<String>, StoreError> {
        self.map_tasks(cas, |task| task.task_id)
    }

    /// Project each Task once, in label order, and retain only the caller's mapped value.
    /// Every stream still passes full replay and fresh artifact validation. A listing can
    /// consume that checked projection without retaining all Tasks or projecting them again.
    pub fn map_tasks<T>(
        &self,
        cas: &Cas,
        mut map: impl FnMut(TaskProjection) -> T,
    ) -> Result<Vec<T>, StoreError> {
        let mut ids = BTreeSet::new();
        for run_id in self.run_ids()? {
            if !run_id.starts_with("task:") {
                continue;
            }
            let events = self.replay(&run_id)?;
            let first = events
                .first()
                .ok_or_else(|| conflict("Empty Task stream"))?;
            let transition: TaskTransitionV1 = serde_json::from_value(first.payload.clone())
                .map_err(|e| conflict(e.to_string()))?;
            let TaskChangeV1::Opened { revision_id, .. } = transition.change else {
                return Err(conflict("Task stream does not begin with Opened"));
            };
            let task = revision(cas, &revision_id)?;
            if task_run_id(&task.task_id)? != run_id {
                return Err(conflict("Task stream identity differs from its revision"));
            }
            ids.insert(task.task_id);
        }
        ids.into_iter()
            .map(|id| {
                let task = self
                    .task_projection(cas, &id)?
                    .ok_or_else(|| conflict("Task disappeared"))?;
                Ok(map(task))
            })
            .collect()
    }

    pub fn task_projection(
        &self,
        cas: &Cas,
        task_id: &str,
    ) -> Result<Option<TaskProjection>, StoreError> {
        let mut state = self
            .task_cache
            .borrow()
            .as_ref()
            .filter(|state| state.task_id == task_id)
            .cloned();
        let mut verified = BTreeSet::new();
        // Cached prefix is only a parse memo. Revalidate current revision/plan bytes on every
        // access; a removed or corrupted active artifact must never inherit cached authority.
        if let Some(state) = &state {
            review_handoff::validate_cached(self, cas, state)?;
            review_integration::validate_cached(self, cas, state)?;
            execution::broker::validate_cached(cas, state)?;
            execution::owned::validate_cached(cas, state)?;
            if state.planning.is_some() {
                state.planning_proof(cas)?;
            }
            let mut active_refs: BTreeSet<String> = references(
                cas,
                &TaskChangeV1::RevisionRecorded {
                    revision_id: state.revision_id.clone(),
                },
                Some(state),
            )?
            .into_iter()
            .collect();
            if revision(cas, &state.revision_id)? != state.revision {
                return Err(conflict("Cached Task revision changed identity"));
            }
            if let Some(id) = &state.plan_id {
                plan(cas, id, state)?;
                active_refs.extend(references(
                    cas,
                    &TaskChangeV1::PlanProposed {
                        plan_id: id.clone(),
                    },
                    Some(state),
                )?);
                if let Some(decision) = state.decisions.get(id) {
                    let current: PlanDecisionV1 =
                        payload(cas, &decision.artifact_id, task::PLAN_DECISION_V1)?;
                    if current != decision.value {
                        return Err(conflict("Cached approval changed identity"));
                    }
                    active_refs.extend(references(
                        cas,
                        &TaskChangeV1::PlanDecided {
                            decision_id: decision.artifact_id.clone(),
                            valid_until_unix_ms: decision.valid_until,
                        },
                        Some(state),
                    )?);
                }
            }
            if let TaskPhaseV1::Finished { result_id } = &state.phase {
                active_refs.extend(references(
                    cas,
                    &TaskChangeV1::Finished {
                        result_id: result_id.clone(),
                    },
                    Some(state),
                )?);
            }
            for (id, recorded) in &state.deliveries {
                let value: task::delivery::TaskDeliveryRecordV1 =
                    payload(cas, id, task::delivery::TASK_DELIVERY_RECORD_V1)?;
                if &value != recorded {
                    return Err(conflict("Cached Task delivery changed identity"));
                }
                active_refs.extend(references(
                    cas,
                    &TaskChangeV1::DeliveryRecorded {
                        record_id: id.clone(),
                    },
                    Some(state),
                )?);
            }
            active_refs.extend(state.recording_recovery_refs(cas)?);
            active_refs.extend(state.artifact_refs.iter().cloned());
            for id in active_refs {
                cas.verify(&id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))?;
                verified.insert(id);
            }
        }
        let first = state.as_ref().map_or(0, |state| state.next_sequence);
        for event in self.replay_from(&task_run_id(task_id)?, first)? {
            if event.event_type == EventType::TaskBrokerTransitionV1 {
                let state = state
                    .as_mut()
                    .ok_or_else(|| conflict("Task Broker evidence precedes genesis"))?;
                execution::broker::apply_event(cas, state, &event)?;
                verified.extend(event.artifact_refs.iter().cloned());
                continue;
            }
            if !matches!(
                event.event_type,
                EventType::TaskTransitionV1
                    | EventType::TaskTransitionV2
                    | EventType::TaskTransitionV3
                    | EventType::TaskTransitionV4
            ) {
                return Err(conflict("Task log contains a foreign event"));
            }
            let transition = read_task_transition(&event)?;
            if let TaskChangeV1::ReviewContinued { handoff_id } = &transition.change {
                review_handoff::validate_evidence(
                    self,
                    cas,
                    &review_handoff::read_task_review_handoff(cas, handoff_id)?,
                )?;
            }
            let expected = references(cas, &transition.change, state.as_ref())?;
            if expected != event.artifact_refs {
                return Err(conflict(
                    "Task event references disagree with its typed payload",
                ));
            }
            for id in &expected {
                if verified.insert(id.clone()) {
                    cas.verify(id)
                        .map_err(|e| StoreError::Artifact(e.to_string()))?;
                }
            }
            if let Some(state) = &mut state {
                state.apply(cas, &event, &transition)?;
            } else if let TaskChangeV1::Opened {
                revision_id,
                lease_until_unix_ms,
            } = &transition.change
            {
                let revision = revision(cas, revision_id)?;
                if revision.task_id != task_id
                    || revision.revision != 1
                    || event.sequence != 0
                    || transition.epoch != 1
                {
                    return Err(conflict("Invalid Task genesis"));
                }
                state = Some(TaskProjection {
                    task_id: task_id.into(),
                    revision_id: revision_id.clone(),
                    revision,
                    plan_id: None,
                    phase: TaskPhaseV1::Submitted {},
                    admitted: false,
                    next_sequence: 1,
                    writer: transition.writer,
                    epoch: 1,
                    lease_until: *lease_until_unix_ms,
                    last_time: transition.now_unix_ms,
                    resume_phase: None,
                    recording_recovery: None,
                    recording_report: None,
                    decisions: BTreeMap::new(),
                    artifact_refs: BTreeSet::new(),
                    execution: None,
                    planning: None,
                    deliveries: Vec::new(),
                    run_reports: Vec::new(),
                    review_handoffs: Vec::new(),
                });
            } else {
                return Err(conflict("Task transition precedes genesis"));
            }
        }
        if let Some(state) = &state
            && state.next_sequence != first
        {
            review_integration::validate_cached(self, cas, state)?;
        }
        if let Some(state) = &mut state {
            state.artifact_refs = verified;
        }
        *self.task_cache.borrow_mut() = state.clone();
        Ok(state)
    }

    fn append_task_transition(
        &mut self,
        cas: &Cas,
        task_id: &str,
        transition: TaskTransitionV1,
    ) -> Result<RunEvent, StoreError> {
        self.append_task_transition_with_owned_prefix(cas, task_id, transition, None)
    }

    fn append_task_transition_with_owned_prefix(
        &mut self,
        cas: &Cas,
        task_id: &str,
        transition: TaskTransitionV1,
        owned_prefix: Option<(u64, Option<(String, u64)>)>,
    ) -> Result<RunEvent, StoreError> {
        let (event_type, value) = review_handoff::encode_transition(&transition)?;
        let state = self.task_projection(cas, task_id)?;
        let first = state.as_ref().map_or(0, |s| s.next_sequence);
        if owned_prefix
            .as_ref()
            .is_some_and(|(expected, _)| *expected != first)
        {
            return Err(conflict("Owned completion lost its Task prefix"));
        }
        let owned_record = if let TaskChangeV1::ExecutionRecorded { record_id } = &transition.change
        {
            execution::owned::is_owned_record(
                &execution::read_execution_record(cas, record_id)?.record,
            )
        } else {
            false
        };
        let valid_until = if owned_record
            || matches!(
                transition.change,
                TaskChangeV1::ReviewContinued { .. } | TaskChangeV1::RecordingResumed { .. }
            ) {
            state.as_ref().map(|state| {
                state.lease_until.min(
                    state
                        .plan_id
                        .as_ref()
                        .and_then(|id| state.decisions.get(id))
                        .map_or(u64::MAX, |decision| decision.valid_until),
                )
            })
        } else {
            None
        };
        let refs = references(cas, &transition.change, state.as_ref())?;
        let run_id = task_run_id(task_id)?;
        let event = NewEvent::new(event_type, value.clone()).referencing(refs);
        let review_round = review_round::fence_for_transition(cas, &transition, state.as_ref())?;
        if let Some(mut state) = state {
            state.apply(
                cas,
                &RunEvent {
                    run_id: run_id.clone(),
                    event_id: super::derive_event_id(&run_id, first as i64),
                    sequence: first,
                    event_type: event.event_type,
                    occurred_at: event.occurred_at.clone(),
                    node_id: None,
                    attempt_id: None,
                    causation_id: None,
                    correlation_id: None,
                    artifact_refs: event.artifact_refs.clone(),
                    payload: value.clone(),
                },
                &transition,
            )?;
        } else {
            let TaskChangeV1::Opened { revision_id, .. } = &transition.change else {
                return Err(conflict("Task has not been opened"));
            };
            let initial = revision(cas, revision_id)?;
            if initial.task_id != task_id || initial.revision != 1 || transition.epoch != 1 {
                return Err(conflict("Invalid Task genesis"));
            }
        }
        let permit = WritePermit {
            run_id: run_id.clone(),
            first,
            payloads: vec![value],
            event_type,
            valid_until,
            review_round,
            review_prefix: owned_prefix.and_then(|(_, prefix)| prefix),
        };
        self.append_batch_inner(&run_id, cas, &[event], Some(&permit), None)?
            .pop()
            .ok_or_else(|| conflict("Task transition appended no event"))
    }

    pub fn open_task(
        &mut self,
        cas: &Cas,
        revision_id: &str,
        writer: &str,
        lease_ms: u64,
    ) -> Result<TaskLease, StoreError> {
        let revision = revision(cas, revision_id)?;
        let time = now()?;
        let until = time
            .checked_add(lease_ms)
            .ok_or_else(|| conflict("Task lease overflow"))?;
        self.append_task_transition(
            cas,
            &revision.task_id,
            TaskTransitionV1 {
                writer: writer.into(),
                epoch: 1,
                now_unix_ms: time,
                change: TaskChangeV1::Opened {
                    revision_id: revision_id.into(),
                    lease_until_unix_ms: until,
                },
            },
        )?;
        Ok(TaskLease {
            task_id: revision.task_id,
            writer: writer.into(),
            epoch: 1,
        })
    }

    pub fn take_task_lease(
        &mut self,
        cas: &Cas,
        task_id: &str,
        writer: &str,
        lease_ms: u64,
    ) -> Result<TaskLease, StoreError> {
        let state = self
            .task_projection(cas, task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let epoch = state
            .epoch
            .checked_add(1)
            .ok_or_else(|| conflict("Task epoch overflow"))?;
        let time = now()?;
        self.append_task_transition(
            cas,
            task_id,
            TaskTransitionV1 {
                writer: writer.into(),
                epoch,
                now_unix_ms: time,
                change: TaskChangeV1::LeaseTaken {
                    lease_until_unix_ms: time
                        .checked_add(lease_ms)
                        .ok_or_else(|| conflict("Task lease overflow"))?,
                },
            },
        )?;
        Ok(TaskLease {
            task_id: task_id.into(),
            writer: writer.into(),
            epoch,
        })
    }

    /// Check the exact existing writer capability without renewing or granting dispatch.
    pub fn check_task_lease_current(
        &self,
        cas: &Cas,
        lease: &TaskLease,
    ) -> Result<u64, StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now()?,
            change: TaskChangeV1::Resumed {},
        })?;
        Ok(state.lease_until_unix_ms())
    }

    pub fn renew_task_lease(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        lease_ms: u64,
    ) -> Result<RunEvent, StoreError> {
        let time = now()?;
        self.task_change(
            cas,
            lease,
            TaskChangeV1::LeaseRenewed {
                lease_until_unix_ms: time
                    .checked_add(lease_ms)
                    .ok_or_else(|| conflict("Task lease overflow"))?,
            },
            time,
        )
    }

    pub fn release_task_lease(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
    ) -> Result<RunEvent, StoreError> {
        self.task_change(cas, lease, TaskChangeV1::LeaseReleased {}, now()?)
    }

    fn task_change(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        change: TaskChangeV1,
        time: u64,
    ) -> Result<RunEvent, StoreError> {
        self.append_task_transition(
            cas,
            &lease.task_id,
            TaskTransitionV1 {
                writer: lease.writer.clone(),
                epoch: lease.epoch,
                now_unix_ms: time,
                change,
            },
        )
    }

    fn authorized_plan(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<ExecutionPlanV1, StoreError> {
        let value = plan(cas, id, state)?;
        let expected = authority
            .validate_plan(cas, &state.revision, &value)
            .map_err(conflict)?;
        if expected != value.generated_origins {
            return Err(conflict(
                "Plan omitted or changed generated dependency provenance",
            ));
        }
        Ok(value)
    }

    pub fn propose_task_plan(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        plan_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        self.authorized_plan(cas, &state, plan_id, authority)?;
        self.task_change(
            cas,
            lease,
            TaskChangeV1::PlanProposed {
                plan_id: plan_id.into(),
            },
            now()?,
        )
    }

    pub fn decide_task_plan(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        plan_id: &str,
        decision: PlanDecisionKindV1,
        reason: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        self.authorized_plan(cas, &state, plan_id, authority)?;
        if state.plan_id.as_deref() != Some(plan_id) {
            return Err(conflict("Developer decision names a stale proposal"));
        }
        let grant = authority
            .authorize_decision(&state.revision, plan_id, decision)
            .map_err(conflict)?;
        let time = now()?;
        let value = PlanDecisionV1 {
            task_revision_id: state.revision_id.clone(),
            plan_id: plan_id.into(),
            policy_id: state.revision.authority.policy_id.clone(),
            developer: grant.developer,
            authorization_id: grant.authorization_id,
            decision,
            reason: reason.into(),
        };
        value.validate().map_err(conflict)?;
        authority.authorization_current(&value).map_err(conflict)?;
        if let Some(old) = state.decisions.get(plan_id) {
            state.check_lease(&TaskTransitionV1 {
                writer: lease.writer.clone(),
                epoch: lease.epoch,
                now_unix_ms: time,
                change: TaskChangeV1::Resumed {},
            })?;
            if old.value == value && !old.revoked && time < old.valid_until {
                return Ok(old.event.clone());
            }
            return Err(conflict(
                "Plan already has a different or expired developer decision",
            ));
        }
        let (decision_id, _) = cas
            .put_artifact(
                task::PLAN_DECISION_V1,
                review_core::Producer::KernelOperation {
                    run_id: task_run_id(&state.task_id)?,
                    node_id: None,
                    operation_id: "task-plan-decision@1".into(),
                },
                vec![
                    value.task_revision_id.clone(),
                    value.plan_id.clone(),
                    value.policy_id.clone(),
                    value.authorization_id.clone(),
                ],
                None,
                serde_json::to_value(value)?,
            )
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        self.task_change(
            cas,
            lease,
            TaskChangeV1::PlanDecided {
                decision_id,
                valid_until_unix_ms: grant.valid_until_unix_ms,
            },
            time,
        )
    }

    pub fn admit_task_plan(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let id = state
            .plan_id
            .as_ref()
            .ok_or_else(|| conflict("Task has no proposal"))?;
        self.authorized_plan(cas, &state, id, authority)?;
        state.check_approval(cas, now()?)?;
        if let Some(decision) = state.decisions.get(id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        self.task_change(
            cas,
            lease,
            TaskChangeV1::PlanAdmitted {
                plan_id: id.clone(),
            },
            now()?,
        )
    }

    pub fn finish_task(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        result_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let result = payload(cas, result_id, task::TASK_RESULT_V1)?;
        authority
            .validate_result(cas, &state.revision, &result)
            .map_err(conflict)?;
        self.task_change(
            cas,
            lease,
            TaskChangeV1::Finished {
                result_id: result_id.into(),
            },
            now()?,
        )
    }

    pub fn revise_task(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        revision_id: &str,
    ) -> Result<RunEvent, StoreError> {
        self.task_change(
            cas,
            lease,
            TaskChangeV1::RevisionRecorded {
                revision_id: revision_id.into(),
            },
            now()?,
        )
    }

    pub fn wait_task(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        reason: TaskWaitingReasonV1,
    ) -> Result<RunEvent, StoreError> {
        self.task_change(cas, lease, TaskChangeV1::Waiting { reason }, now()?)
    }

    pub fn resume_task(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        if state.admitted {
            self.current_task_plan(cas, &state, authority, now()?)?;
        }
        self.task_change(cas, lease, TaskChangeV1::Resumed {}, now()?)
    }

    pub fn revoke_task_approval(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        plan_id: &str,
        reason: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let decision = state
            .decisions
            .get(plan_id)
            .ok_or_else(|| conflict("Unknown Task approval"))?;
        // The host authenticates revocation too. Knowing a recorded decision ID is not authority.
        let grant = authority
            .authorize_decision(&state.revision, plan_id, PlanDecisionKindV1::Rejected)
            .map_err(conflict)?;
        if now()? >= grant.valid_until_unix_ms {
            return Err(conflict("Developer revocation authorization has expired"));
        }
        let revocation = PlanDecisionV1 {
            task_revision_id: state.revision_id.clone(),
            plan_id: plan_id.into(),
            policy_id: state.revision.authority.policy_id.clone(),
            developer: grant.developer,
            authorization_id: grant.authorization_id,
            decision: PlanDecisionKindV1::Rejected,
            reason: reason.into(),
        };
        revocation.validate().map_err(conflict)?;
        authority
            .authorization_current(&revocation)
            .map_err(conflict)?;
        let (revocation_id, _) = cas
            .put_artifact(
                task::PLAN_DECISION_V1,
                review_core::Producer::KernelOperation {
                    run_id: task_run_id(&state.task_id)?,
                    node_id: None,
                    operation_id: "task-plan-revocation@1".into(),
                },
                vec![
                    revocation.task_revision_id.clone(),
                    revocation.plan_id.clone(),
                    revocation.policy_id.clone(),
                    revocation.authorization_id.clone(),
                ],
                None,
                serde_json::to_value(revocation)?,
            )
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        self.task_change(
            cas,
            lease,
            TaskChangeV1::ApprovalRevoked {
                decision_id: decision.artifact_id.clone(),
                reason: reason.into(),
                revocation_id: Some(revocation_id),
            },
            now()?,
        )
    }

    fn current_task_plan(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        authority: &dyn TaskAuthority,
        time: u64,
    ) -> Result<ExecutionPlanV1, StoreError> {
        let id = state
            .plan_id
            .as_ref()
            .ok_or_else(|| conflict("Task has no proposal"))?;
        let plan = self.authorized_plan(cas, state, id, authority)?;
        state.check_approval(cas, time)?;
        if let Some(decision) = state.decisions.get(id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        Ok(plan)
    }

    /// Shared admission guard for Provider preparation and every initial/retry dispatch.
    /// A runtime must perform this under the same owner lock as its durable Attempt append;
    /// merely possessing a previously returned plan does not grant future dispatch authority.
    pub fn check_task_dispatch(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<ExecutionPlanV1, StoreError> {
        self.checked_task_dispatch(cas, lease, authority)
            .map(|(_, plan)| plan)
    }

    /// Reuse one freshly validated projection within a Store operation. Publication still
    /// re-reads authority and the log through append_task_transition, including after any
    /// domain callback; this does not create a reusable dispatch capability.
    fn checked_task_dispatch(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<(TaskProjection, ExecutionPlanV1), StoreError> {
        self.checked_task_current(cas, lease, authority, true)
    }

    /// Current recording authority does not permit another execution effect. It retains
    /// writer, exact admitted plan, developer approval and Review Round fences at expiry.
    pub fn check_current_task_plan_for_recording(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<ExecutionPlanV1, StoreError> {
        self.checked_task_recording(cas, lease, authority)
            .map(|(_, plan)| plan)
    }

    fn checked_task_recording(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<(TaskProjection, ExecutionPlanV1), StoreError> {
        self.checked_task_current(cas, lease, authority, false)
    }

    fn checked_task_current(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
        dispatching: bool,
    ) -> Result<(TaskProjection, ExecutionPlanV1), StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let time = now()?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        if !state.admitted || state.phase != (TaskPhaseV1::Running {}) {
            return Err(conflict("Task is not admitted and running"));
        }
        let plan = if dispatching {
            self.current_task_plan(cas, &state, authority, time)?
        } else {
            let id = state
                .plan_id
                .as_deref()
                .ok_or_else(|| conflict("Task has no plan"))?;
            let plan = self.authorized_plan(cas, &state, id, authority)?;
            state.check_plan_decision(cas, time)?;
            if let Some(decision) = state.decisions.get(id) {
                authority
                    .authorization_current(&decision.value)
                    .map_err(conflict)?;
            }
            plan
        };
        if let Some(round) = review_round::ReviewRoundFence::for_state(cas, &state)? {
            round.validate(&self.conn)?;
        }
        Ok((state, plan))
    }
}
