//! Captured Review Round succession retains one Task and its original accounting authority.
use super::*;
use review_core::task::event::TaskTransitionV2;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::task::review_handoff::*;
use review_graph::task::CompiledTask;

pub fn read_task_transition(event: &RunEvent) -> Result<TaskTransitionV1, StoreError> {
    match event.event_type {
        EventType::TaskTransitionV4 => {
            let value: task::event::TaskTransitionV4 =
                serde_json::from_value(event.payload.clone())?;
            value.validate().map_err(conflict)?;
            Ok(value.into_transition())
        }
        EventType::TaskTransitionV1 => {
            let value: TaskTransitionV1 = serde_json::from_value(event.payload.clone())?;
            value.validate().map_err(conflict)?;
            Ok(value)
        }
        EventType::TaskTransitionV3 => {
            let value: task::event::TaskTransitionV3 =
                serde_json::from_value(event.payload.clone())?;
            value.validate().map_err(conflict)?;
            Ok(value.into_transition())
        }
        EventType::TaskTransitionV2 => {
            let value: TaskTransitionV2 = serde_json::from_value(event.payload.clone())?;
            value.validate().map_err(conflict)?;
            Ok(value.into_transition())
        }
        _ => Err(conflict("Expected a typed Task transition")),
    }
}
pub(super) fn encode_transition(
    value: &TaskTransitionV1,
) -> Result<(EventType, serde_json::Value), StoreError> {
    if let Some(value) = task::event::TaskTransitionV4::from_recording(value) {
        value.validate().map_err(conflict)?;
        Ok((EventType::TaskTransitionV4, serde_json::to_value(value)?))
    } else if let Some(value) = task::event::TaskTransitionV3::from_integration(value) {
        value.validate().map_err(conflict)?;
        Ok((EventType::TaskTransitionV3, serde_json::to_value(value)?))
    } else if let Some(value) = TaskTransitionV2::from_continuation(value) {
        value.validate().map_err(conflict)?;
        Ok((EventType::TaskTransitionV2, serde_json::to_value(value)?))
    } else {
        value.validate().map_err(conflict)?;
        Ok((EventType::TaskTransitionV1, serde_json::to_value(value)?))
    }
}
fn producer(task_id: &str) -> Result<review_core::Producer, StoreError> {
    Ok(review_core::Producer::KernelOperation {
        run_id: task_run_id(task_id)?,
        node_id: None,
        operation_id: "review-continue@1".into(),
    })
}
pub fn capture_task_review_handoff(
    cas: &Cas,
    value: &TaskReviewHandoffV1,
) -> Result<String, StoreError> {
    let (kind, raw) = if let Some(v2) = TaskReviewHandoffV2::from_integrated(value) {
        v2.validate().map_err(conflict)?;
        (TASK_REVIEW_HANDOFF_V2, serde_json::to_value(v2)?)
    } else {
        value.validate().map_err(conflict)?;
        (TASK_REVIEW_HANDOFF_V1, serde_json::to_value(value)?)
    };
    Ok(cas
        .put_artifact(
            kind,
            producer(&value.task_id)?,
            value
                .artifact_refs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            None,
            raw,
        )
        .map_err(|e| StoreError::Artifact(e.to_string()))?
        .0)
}
pub fn read_task_review_handoff(cas: &Cas, id: &str) -> Result<TaskReviewHandoffV1, StoreError> {
    let frame = cas
        .get_artifact(id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    let value = match frame.artifact_type.as_str() {
        TASK_REVIEW_HANDOFF_V1 => {
            let v: TaskReviewHandoffV1 = serde_json::from_value(frame.payload)?;
            v.validate().map_err(conflict)?;
            v
        }
        TASK_REVIEW_HANDOFF_V2 => {
            let v: TaskReviewHandoffV2 = serde_json::from_value(frame.payload)?;
            v.validate().map_err(conflict)?;
            v.into_handoff()
        }
        _ => return Err(conflict("Expected versioned Review handoff")),
    };
    if frame.input_artifacts != value.artifact_refs()
        || frame.subject_snapshot_id.is_some()
        || frame.producer != producer(&value.task_id)?
    {
        return Err(conflict(
            "Review handoff changed its exact producer or references",
        ));
    }
    for id in value.artifact_refs() {
        cas.verify(id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
    }
    Ok(value)
}

fn round(
    cas: &Cas,
    revision: &TaskRevisionV1,
    id: &str,
) -> Result<LegacyReviewRoundV1, StoreError> {
    super::review_round::ReviewRoundFence::capture(cas, revision)?
        .ok_or_else(|| conflict("Review handoff lacks captured Round authority"))?;
    let input = revision
        .inputs
        .values()
        .find(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1)
        .expect("checked Round");
    if input.artifact_ids != [id] {
        return Err(conflict("Review handoff changed its exact Round root"));
    }
    payload(cas, id, LEGACY_REVIEW_ROUND_V1)
}

pub(super) fn validate_revisions(
    cas: &Cas,
    value: &TaskReviewHandoffV1,
) -> Result<
    (
        TaskRevisionV1,
        TaskRevisionV1,
        LegacyReviewRoundV1,
        LegacyReviewRoundV1,
    ),
    StoreError,
> {
    let previous = revision(cas, &value.predecessor_revision_id)?;
    let next = revision(cas, &value.successor_revision_id)?;
    let old = round(cas, &previous, &value.predecessor_round_id)?;
    let new = round(cas, &next, &value.successor_round_id)?;
    let mut expected = previous.clone();
    expected.revision = expected
        .revision
        .checked_add(1)
        .ok_or_else(|| conflict("Task revision overflow"))?;
    expected.previous_revision_id = Some(value.predecessor_revision_id.clone());
    expected.inputs = next.inputs.clone();
    if value.task_id != previous.task_id
        || expected != next
        || previous.inputs.keys().ne(next.inputs.keys())
        || previous.inputs.iter().any(|(name, input)| {
            input.artifact_type != next.inputs[name].artifact_type
                || input.cardinality != next.inputs[name].cardinality
        })
        || old.campaign_id != new.campaign_id
        || old.campaign_manifest_id != new.campaign_manifest_id
    {
        return Err(conflict(
            "Review handoff must retain its Task, Campaign, captured policy, contracts and original limits",
        ));
    }
    match value.evidence {
        TaskReviewHandoffEvidenceV1::ClosedRound { .. }
        | TaskReviewHandoffEvidenceV1::IntegratedRound { .. }
            if old.round.checked_add(1) == Some(new.round) && new.epoch == 1 => {}
        TaskReviewHandoffEvidenceV1::SupersededInput { .. }
            if old.round == new.round && old.epoch.checked_add(1) == Some(new.epoch) => {}
        _ => {
            return Err(conflict(
                "Review handoff requires one exact numeric Round or input epoch step",
            ));
        }
    }
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&old.campaign_manifest_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?,
    )?;
    manifest.validate().map_err(conflict)?;
    if new.round > manifest.convergence.max_rounds {
        return Err(conflict(
            "Review handoff exceeds captured Campaign Round limit",
        ));
    }
    Ok((previous, next, old, new))
}

/// Historical checks do not require the successor to remain current: later handoffs may
/// supersede it. The write permit separately fences the successor as the current open Round.
pub(super) fn validate_evidence(
    store: &EventStore,
    cas: &Cas,
    value: &TaskReviewHandoffV1,
) -> Result<(), StoreError> {
    let (_, _, old, new) = validate_revisions(cas, value)?;
    let events = store.replay(&old.campaign_id)?;
    let old_event = events
        .iter()
        .find(|e| e.event_id == old.round_event_id)
        .ok_or_else(|| conflict("Review predecessor Round is absent"))?;
    let new_event = events
        .iter()
        .find(|e| e.event_id == new.round_event_id)
        .ok_or_else(|| conflict("Review successor Round is absent"))?;
    for (event, round) in [(old_event, &old), (new_event, &new)] {
        if event.event_type != EventType::RoundStartedV1 {
            return Err(conflict("Review handoff names a non-Round event"));
        }
        let payload: review_core::RoundStartedPayloadV1 =
            serde_json::from_value(event.payload.clone())?;
        if payload.round != round.round
            || payload.epoch != round.epoch
            || payload.subject_id != round.subject_id
            || payload.campaign_manifest_id != round.campaign_manifest_id
        {
            return Err(conflict("Review handoff changed canonical Round authority"));
        }
    }
    if new_event.sequence <= old_event.sequence
        || events.iter().any(|event| {
            event.event_type == EventType::RoundStartedV1
                && old_event.sequence < event.sequence
                && event.sequence < new_event.sequence
        })
    {
        return Err(conflict("Review handoff skipped a canonical Round"));
    }
    let evidence = events
        .iter()
        .find(|event| event.event_id == value.evidence_event_id())
        .ok_or_else(|| conflict("Review handoff evidence is absent"))?;
    let old_started: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(old_event.payload.clone())?;
    let new_started: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(new_event.payload.clone())?;
    if evidence.sequence <= old_event.sequence
        || evidence.sequence >= new_event.sequence
        || evidence.causation_id.as_deref()
            != if matches!(
                value.evidence,
                TaskReviewHandoffEvidenceV1::IntegratedRound { .. }
            ) {
                None
            } else {
                Some(&old.round_event_id)
            }
    {
        return Err(conflict(
            "Review handoff evidence is outside its exact predecessor epoch",
        ));
    }
    match &value.evidence {
        TaskReviewHandoffEvidenceV1::IntegratedRound { .. } => {
            super::review_integration::validate_handoff(
                store,
                cas,
                value,
                evidence,
                &new,
                &new_started,
            )?;
        }
        TaskReviewHandoffEvidenceV1::ClosedRound { .. } => {
            if evidence.event_type != EventType::RunReportV6 {
                return Err(conflict(
                    "Review continuation requires its canonical Task-backed conclusion",
                ));
            }
            let report: review_core::RunReportPayloadV6 =
                serde_json::from_value(evidence.payload.clone())?;
            report.validate().map_err(conflict)?;
            if report.task_accounting.task_id != value.task_id
                || report.task_accounting.task_revision_id != value.predecessor_revision_id
                || report.task_accounting.plan_id != value.predecessor_plan_id
                || report.verdict
                    != (review_core::RunVerdictV3::Fail {
                        reason: review_core::RunFailureReasonV3::NotConverged,
                    })
            {
                return Err(conflict(
                    "Review continuation requires the exact predecessor NotConverged conclusion",
                ));
            }
            let task_report: task::report::TaskRunReportV1 = payload(
                cas,
                &report.task_accounting.task_report_id,
                task::report::TASK_RUN_REPORT_V1,
            )?;
            task_report.validate().map_err(conflict)?;
            if task_report.task_revision_id != value.predecessor_revision_id
                || task_report.plan_id != value.predecessor_plan_id
            {
                return Err(conflict(
                    "Review conclusion changed its common execution report",
                ));
            }
            let (_, demands) =
                selected_prior_sets(cas, &value.predecessor_plan_id, &old, &task_report)?;
            // prior_finding_set_id is the legacy raw PriorFindings view, which may include
            // intervening dispositions. The captured compiler resolves the canonical FindingSet
            // separately. DemandSet is an envelope ID, including a common-only companion port.
            if new_started.prior_demand_set_id != demands {
                return Err(conflict(
                    "Review successor dropped or changed the predecessor's selected DemandSet",
                ));
            }
        }
        TaskReviewHandoffEvidenceV1::SupersededInput { .. } => {
            if evidence.event_type != EventType::RoundInputSupersededV1
                || new_event.causation_id.as_deref() != Some(&old.round_event_id)
            {
                return Err(conflict(
                    "Review epoch continuation requires exact supersession evidence",
                ));
            }
            let change: review_core::RoundInputSupersededPayloadV1 =
                serde_json::from_value(evidence.payload.clone())?;
            // Compare checked arithmetic above before any frozen validator's old_epoch + 1.
            if change.round != old.round
                || change.old_epoch != old.epoch
                || change.new_epoch != new.epoch
                || change.old_subject_id != old.subject_id
                || change.replacement_subject_id != new.subject_id
                || change.campaign_manifest_id != old.campaign_manifest_id
            {
                return Err(conflict(
                    "Review handoff contradicts its input supersession",
                ));
            }
            if old_started.prior_finding_set_id != new_started.prior_finding_set_id
                || old_started.prior_demand_set_id != new_started.prior_demand_set_id
            {
                return Err(conflict(
                    "Review input supersession changed its original prior FindingSet or DemandSet",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn selected_prior_sets(
    cas: &Cas,
    predecessor_plan_id: &str,
    round: &LegacyReviewRoundV1,
    report: &task::report::TaskRunReportV1,
) -> Result<(String, String), StoreError> {
    let plan: ExecutionPlanV1 = payload(cas, predecessor_plan_id, task::EXECUTION_PLAN_V1)?;
    let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
    let mut sets = BTreeMap::<String, BTreeSet<String>>::new();
    for (node, definition) in &graph.nodes {
        if !matches!(
            definition.operator,
            review_graph::task::CompiledOperator::ReviewDomain {
                operation: review_graph::task::ReviewOperation::Ledger,
                ..
            }
        ) {
            continue;
        }
        let Some(task::report::TaskNodeOutcomeV1::Completed { output_id }) = report
            .nodes
            .iter()
            .find(|entry| entry.node == *node)
            .map(|entry| &entry.outcome)
        else {
            continue;
        };
        let output = execution::output(cas, output_id)?;
        let invocation = execution::invocation(cas, &output.invocation_id)?;
        if invocation.node != *node || invocation.plan_id != predecessor_plan_id {
            return Err(conflict(
                "Review Ledger output changed its original invocation",
            ));
        }
        for port in output.outputs.values().filter(|port| {
            matches!(
                port.artifact_type.as_str(),
                review_core::contract::FINDING_SET_V1 | review_core::contract::DEMAND_SET_V1
            )
        }) {
            let [id] = port.artifact_ids.as_slice() else {
                return Err(conflict("Review Ledger prior set is ambiguous"));
            };
            let artifact = envelope(cas, id, &port.artifact_type)?;
            if port.cardinality != review_core::PortCardinality::One
                || port.snapshot_id.as_ref() != Some(&round.head_snapshot_id)
                || artifact.subject_snapshot_id != port.snapshot_id
            {
                return Err(conflict("Review Ledger prior set changed its Snapshot"));
            }
            sets.entry(port.artifact_type.clone())
                .or_default()
                .insert(id.clone());
        }
    }
    let one = |kind: &str| -> Result<String, StoreError> {
        let ids = sets
            .get(kind)
            .ok_or_else(|| conflict("Review continuation lacks a selected Ledger prior set"))?;
        if ids.len() != 1 {
            return Err(conflict(
                "Review continuation has ambiguous authoritative Ledger prior sets",
            ));
        }
        Ok(ids.first().expect("one prior set").clone())
    };
    Ok((
        one(review_core::contract::FINDING_SET_V1)?,
        one(review_core::contract::DEMAND_SET_V1)?,
    ))
}

pub(super) fn validate_cached(
    store: &EventStore,
    cas: &Cas,
    state: &TaskProjection,
) -> Result<(), StoreError> {
    for (id, recorded) in &state.review_handoffs {
        if read_task_review_handoff(cas, id)? != *recorded {
            return Err(conflict("Cached Review handoff changed identity"));
        }
        validate_evidence(store, cas, recorded)?;
    }
    Ok(())
}

impl TaskProjection {
    pub(super) fn apply_review_handoff(
        &mut self,
        cas: &Cas,
        id: &str,
        time: u64,
    ) -> Result<(), StoreError> {
        let handoff = read_task_review_handoff(cas, id)?;
        let (previous, next, old, new) = validate_revisions(cas, &handoff)?;
        if self.task_id != handoff.task_id
            || self.revision_id != handoff.predecessor_revision_id
            || self.revision != previous
            || self.plan_id.as_ref() != Some(&handoff.predecessor_plan_id)
            || !self.admitted
            || self.phase != (TaskPhaseV1::Running {})
        {
            return Err(conflict(
                "Review continuation is not its admitted running predecessor",
            ));
        }
        self.check_plan_decision(cas, time)?;
        if let TaskReviewHandoffEvidenceV1::IntegratedRound {
            phase_id,
            integration_committed_event_id,
            ..
        } = &handoff.evidence
        {
            let active = self
                .execution
                .as_ref()
                .and_then(|e| e.active_review_integration())
                .ok_or_else(|| conflict("Integrated handoff lacks its active phase"))?;
            if active.phase_id() != phase_id
                || active.integration_committed_event_id() != Some(integration_committed_event_id)
                || !active.finished()
            {
                return Err(conflict("Integrated handoff lacks its sealed exact commit"));
            }
        }
        if self
            .execution
            .as_ref()
            .is_some_and(|e| !e.pending_attempts().is_empty())
        {
            return Err(conflict("Review handoff has pending common Attempts"));
        }
        let mut candidate = self.clone();
        candidate.revision_id = handoff.successor_revision_id.clone();
        candidate.revision = next;
        candidate.plan_id = Some(handoff.successor_plan_id.clone());
        let plan = plan(cas, &handoff.successor_plan_id, &candidate)?;
        let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
        if graph.inputs != candidate.revision.inputs
            || graph.order != graph.scheduler_plan().map_err(conflict)?.order
        {
            return Err(conflict(
                "Review successor graph has stale inputs or noncanonical ordering",
            ));
        }
        if let Some(execution) = &mut candidate.execution {
            if old.round == new.round && execution.graph.token_scopes != graph.token_scopes {
                return Err(conflict(
                    "Review input epoch changed its captured aggregate scopes",
                ));
            }
            execution.budget.invalidate_plan(time).map_err(conflict)?;
            graph
                .budget(execution.budget.remaining_limits())
                .map_err(conflict)?;
            execution
                .budget
                .install_graph_with_owned_templates(
                    graph.execution_allowances().map_err(conflict)?,
                    graph
                        .calls
                        .iter()
                        .map(|(n, c)| (n.clone(), c.max_attempts))
                        .collect(),
                    graph.token_scopes.clone(),
                    execution::owned::templates(&graph),
                    time,
                    plan.preparation.is_some(),
                )
                .map_err(conflict)?;
            execution.graph = graph;
            execution.active_review_integration = None;
            execution.invocations.clear();
            execution.outputs.clear();
        }
        candidate.admitted = false;
        candidate.resume_phase = None;
        candidate.planning = None;
        candidate.phase = if plan.requires_developer_approval() {
            TaskPhaseV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsPlanReview,
            }
        } else {
            TaskPhaseV1::Ready {}
        };
        candidate.review_handoffs.push((id.into(), handoff));
        *self = candidate;
        Ok(())
    }
}

impl EventStore {
    pub fn continue_task_review(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        handoff_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Review Task"))?;
        let time = now()?;
        let transition = TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::ReviewContinued {
                handoff_id: handoff_id.into(),
            },
        };
        state.check_lease(&transition)?;
        let handoff = read_task_review_handoff(cas, handoff_id)?;
        let (previous, next, _, new) = validate_revisions(cas, &handoff)?;
        if lease.task_id() != handoff.task_id {
            return Err(conflict("Review handoff belongs to another Task"));
        }
        validate_evidence(self, cas, &handoff)?;
        let prefix = self.len(&new.campaign_id)?;
        let mut prior = state.clone();
        prior.revision = previous.clone();
        prior.revision_id = handoff.predecessor_revision_id.clone();
        prior.plan_id = Some(handoff.predecessor_plan_id.clone());
        // The current compiler is the successor compiler. The dedicated callback re-derives
        // the predecessor with its own captured compiler instead of reinterpreting its roots.
        let old_plan = plan(cas, &handoff.predecessor_plan_id, &prior)?;
        prior.check_plan_decision(cas, time)?;
        if let Some(decision) = prior.decisions.get(&handoff.predecessor_plan_id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        let mut candidate = state.clone();
        candidate.revision = next.clone();
        candidate.revision_id = handoff.successor_revision_id.clone();
        candidate.plan_id = Some(handoff.successor_plan_id.clone());
        let next_plan =
            self.authorized_plan(cas, &candidate, &handoff.successor_plan_id, authority)?;
        authority
            .validate_review_continuation(cas, &previous, &next, &old_plan, &next_plan, &handoff)
            .map_err(conflict)?;
        let fresh = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Review Task disappeared"))?;
        if fresh.next_sequence != state.next_sequence || self.len(&new.campaign_id)? != prefix {
            return Err(conflict(
                "Task or Review changed during continuation validation",
            ));
        }
        fresh.check_lease(&TaskTransitionV1 {
            now_unix_ms: now()?,
            ..transition.clone()
        })?;
        if let Some(decision) = prior.decisions.get(&handoff.predecessor_plan_id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        super::review_round::ReviewRoundFence::capture(cas, &next)?
            .expect("checked Review")
            .validate(&self.conn)?;
        if state
            .review_handoffs
            .iter()
            .any(|(id, value)| id == handoff_id && value == &handoff)
            && state.revision_id == handoff.successor_revision_id
            && state.plan_id.as_ref() == Some(&handoff.successor_plan_id)
        {
            return self
                .replay(&task_run_id(lease.task_id())?)?
                .into_iter()
                .find(|event| {
                    read_task_transition(event).is_ok_and(|value| value.change == transition.change)
                })
                .ok_or_else(|| conflict("Recorded handoff lost its transition"));
        }
        self.append_task_transition_with_owned_prefix(
            cas,
            lease.task_id(),
            transition,
            Some((state.next_sequence, Some((new.campaign_id, prefix)))),
        )
    }
}
