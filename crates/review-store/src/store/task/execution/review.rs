//! A canonical Review selection projects the common Task ledger; it never owns an Attempt.

use super::*;
use review_core::task::review_compat::*;
use rusqlite::{OptionalExtension, params};

/// The Task prefix and Review prefix are compared under the same SQLite writer lock.
pub(in crate::store) struct WritePermit {
    task_run_id: String,
    task_sequence: u64,
    review_run_id: String,
    review_sequence: u64,
    valid_until: u64,
    event: NewEvent,
}

impl WritePermit {
    pub(in crate::store::task) fn for_checked_recording(
        state: &TaskProjection,
        plan: &ExecutionPlanV1,
        review_run_id: &str,
        review_sequence: u64,
        event: NewEvent,
    ) -> Result<Self, StoreError> {
        let mut permit =
            Self::for_checked_selection(state, plan, review_run_id, review_sequence, event)?;
        permit.valid_until = state.lease_until.min(
            state
                .plan_id
                .as_ref()
                .and_then(|id| state.decisions.get(id))
                .map_or(u64::MAX, |decision| decision.valid_until),
        );
        Ok(permit)
    }

    pub(in crate::store::task) fn for_checked_selection(
        state: &TaskProjection,
        plan: &ExecutionPlanV1,
        review_run_id: &str,
        review_sequence: u64,
        event: NewEvent,
    ) -> Result<Self, StoreError> {
        let approval_until = state
            .plan_id
            .as_ref()
            .and_then(|id| state.decisions.get(id))
            .map_or(u64::MAX, |decision| decision.valid_until);
        Ok(Self {
            task_run_id: task_run_id(&state.revision.task_id)?,
            task_sequence: state.next_sequence,
            review_run_id: review_run_id.into(),
            review_sequence,
            valid_until: state
                .lease_until
                .min(plan.limits.deadline_unix_ms)
                .min(approval_until),
            event,
        })
    }
    pub(in crate::store) fn validate(
        &self,
        tx: &rusqlite::Transaction<'_>,
        run_id: &str,
        first: i64,
        events: &[NewEvent],
    ) -> Result<(), StoreError> {
        let [event] = events else {
            return Err(conflict(
                "Task Review publication requires one exact selection",
            ));
        };
        let sequence: u64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence) + 1, 0) FROM events WHERE run_id = ?1",
            [&self.task_run_id],
            |row| u64_column(row, 0),
        )?;
        if run_id != self.review_run_id
            || u64::try_from(first).ok() != Some(self.review_sequence)
            || sequence != self.task_sequence
            || now()? >= self.valid_until
            || event.event_type != self.event.event_type
            || event.payload != self.event.payload
            || event.artifact_refs != self.event.artifact_refs
            || event.node_id != self.event.node_id
            || event.attempt_id != self.event.attempt_id
            || event.causation_id != self.event.causation_id
            || event.correlation_id != self.event.correlation_id
        {
            return Err(conflict(
                "Task Review publication lost its exact execution/lease comparison",
            ));
        }
        Ok(())
    }
}

impl EventStore {
    /// Recording-only authority: current writer, plan decision and Review Round. The expired
    /// execution deadline continues to fence dispatch, but cannot erase its final accounting.
    fn checked_task_review_conclusion(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<(TaskProjection, ExecutionPlanV1), StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Review Task"))?;
        let time = now()?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        if !state.admitted || state.phase != (TaskPhaseV1::Running {}) {
            return Err(conflict("Review Task is not admitted and running"));
        }
        let id = state
            .plan_id
            .as_ref()
            .ok_or_else(|| conflict("Review Task has no plan"))?;
        let plan = self.authorized_plan(cas, &state, id, authority)?;
        state.check_plan_decision(cas, time)?;
        if let Some(decision) = state.decisions.get(id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        if let Some(round) =
            super::super::review_round::ReviewRoundFence::capture(cas, &state.revision)?
        {
            round.validate(&self.conn)?;
        }
        Ok((state, plan))
    }

    /// Bind a canonical conclusion to the current Task writer and exact scheduler report.
    /// Both log prefixes are compared again inside the append transaction.
    pub fn publish_task_review_report(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        report_id: &str,
        event: NewEvent,
        authority: &dyn TaskAuthority,
    ) -> Result<Vec<review_core::RunEvent>, StoreError> {
        use review_core::task::report::{
            TASK_RUN_REPORT_V1, TaskFailureClassV1, TaskNodeOutcomeV1, TaskRunReportV1,
        };
        let (state, plan) = self.checked_task_review_conclusion(cas, lease, authority)?;
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Review Task has no execution"))?;
        let report: TaskRunReportV1 = payload(cas, report_id, TASK_RUN_REPORT_V1)?;
        report.validate().map_err(conflict)?;
        if !execution.pending_attempts().is_empty()
            || state.run_reports.last().map(String::as_str) != Some(report_id)
            || report.task_revision_id != state.revision_id
            || Some(&report.plan_id) != state.plan_id.as_ref()
            || report.nodes.iter().any(|node| {
                matches!(
                    node.outcome,
                    TaskNodeOutcomeV1::Failed {
                        class: TaskFailureClassV1::DomainPublication,
                        ..
                    }
                )
            })
        {
            return Err(conflict(
                "Review conclusion requires settled execution and recovered domain publication",
            ));
        }
        let input = state
            .revision
            .inputs
            .values()
            .find(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1)
            .ok_or_else(|| conflict("Review conclusion has no captured Round"))?;
        let round: LegacyReviewRoundV1 =
            payload(cas, &input.artifact_ids[0], LEGACY_REVIEW_ROUND_V1)?;
        if !event.event_type.is_run_report()
            || event.node_id.is_some()
            || event.attempt_id.is_some()
            || event.causation_id.as_deref() != Some(&round.round_event_id)
            || event.correlation_id.as_deref() != Some(&round.subject_id)
            || !event.artifact_refs.iter().any(|id| id == report_id)
        {
            return Err(conflict(
                "Task Review conclusion changed its captured Round or report",
            ));
        }
        let conclusion: review_core::RunReportPayloadV6 =
            serde_json::from_value(event.payload.clone())?;
        conclusion.validate().map_err(conflict)?;
        if execution.budget.breached()
            && matches!(conclusion.verdict, review_core::RunVerdictV3::Pass)
        {
            return Err(conflict(
                "An exhausted Review Task cannot publish a passing conclusion",
            ));
        }
        let accounting = &conclusion.task_accounting;
        if accounting.task_id != state.task_id
            || accounting.task_revision_id != state.revision_id
            || Some(&accounting.plan_id) != state.plan_id.as_ref()
            || accounting.task_report_id != report_id
            || accounting.through_sequence.checked_add(1) != Some(state.next_sequence)
            || conclusion.spent_tokens.get() != execution.budget.committed_tokens()
            || !accounting
                .artifact_refs()
                .iter()
                .all(|id| event.artifact_refs.iter().any(|reference| reference == id))
        {
            return Err(conflict(
                "Review conclusion changed its exact Task accounting prefix",
            ));
        }
        validate_report_gate_failures(cas, &state, &round, &conclusion)?;
        let (fresh, _) = self.checked_task_review_conclusion(cas, lease, authority)?;
        if fresh.next_sequence != state.next_sequence {
            return Err(conflict("Task changed during conclusion validation"));
        }
        let mut permit = WritePermit::for_checked_selection(
            &fresh,
            &plan,
            &round.campaign_id,
            self.len(&round.campaign_id)?,
            event.clone(),
        )?;
        // Conclusion records completed work and may explain an expired execution deadline.
        // It still requires the current writer lease and any still-current plan approval.
        permit.valid_until = fresh.lease_until.min(
            fresh
                .plan_id
                .as_ref()
                .and_then(|id| fresh.decisions.get(id))
                .map_or(u64::MAX, |decision| decision.valid_until),
        );
        self.append_batch_inner(&round.campaign_id, cas, &[event], None, Some(&permit))
    }

    /// Publish the canonical Review identity of an already selected and published Task output.
    /// Routing comes from that Attempt's admitted context, never from caller-supplied Round IDs.
    /// An exact replay is idempotent. There are no synthetic legacy Attempt lifecycle events.
    pub fn publish_task_review_result(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        output_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        self.publish_task_review_result_inner(cas, lease, output_id, None, false, authority)
    }

    /// Recover only the selected output pinned by TaskTransition@4. Frozen ordinary
    /// selection still requires its dispatch deadline; this adds no execution authority.
    pub fn publish_task_recorded_review_result(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        output_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        self.publish_task_review_result_inner(cas, lease, output_id, None, true, authority)
    }

    pub fn publish_task_owned_review_result(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        children: &owned::RegisteredTaskChildren,
        output_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        self.publish_task_review_result_inner(
            cas,
            lease,
            output_id,
            Some(children),
            false,
            authority,
        )
    }

    fn publish_task_review_result_inner(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        output_id: &str,
        children: Option<&owned::RegisteredTaskChildren>,
        recording: bool,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, plan) = if recording {
            self.checked_recorded_output(cas, lease, output_id, authority)?
        } else {
            self.checked_task_current(cas, lease, authority, children.is_none())?
        };
        if let Some(children) = children {
            if children.task_id() != lease.task_id() {
                return Err(conflict("Owned Review capability belongs to another Task"));
            }
            state
                .execution
                .as_ref()
                .expect("checked execution")
                .check_owned_publication(children, output_id)?;
        }
        let (context, event) = selected_review_event(cas, &state, output_id)?;
        let attempt_id = event.attempt_id.as_deref().expect("derived Attempt");
        Self::validate_task_output(cas, &state, &plan, output_id, Some(attempt_id), authority)?;
        // Recheck after domain validation, including approval revocation and CAS integrity.
        let (fresh, _) = if recording {
            self.checked_recorded_output(cas, lease, output_id, authority)?
        } else {
            self.checked_task_current(cas, lease, authority, children.is_none())?
        };
        if fresh.next_sequence != state.next_sequence {
            return Err(conflict(
                "Task changed during Review publication validation",
            ));
        }
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM events WHERE run_id = ?1 AND causation_id = ?2
             AND node_id = ?3 AND attempt_id = ?4 AND type = 'TaskReviewResultSelected@1'",
                params![
                    context.campaign_id,
                    context.round_event_id,
                    context.review_node,
                    attempt_id
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            return if serde_json::from_str::<serde_json::Value>(&existing)? == event.payload {
                Ok(())
            } else {
                Err(conflict("Conflicting Task Review selection replay"))
            };
        }
        if children.is_some() {
            fresh
                .execution
                .as_ref()
                .expect("checked execution")
                .check_owned_open(
                    event
                        .node_id
                        .as_deref()
                        .and_then(|_| {
                            event
                                .payload
                                .get("task_node")
                                .and_then(serde_json::Value::as_str)
                        })
                        .ok_or_else(|| conflict("Owned selection lacks its Task node"))?,
                )?;
        }
        let mut permit = WritePermit::for_checked_selection(
            &fresh,
            &plan,
            &context.campaign_id,
            self.len(&context.campaign_id)?,
            event.clone(),
        )?;
        if children.is_some() || recording {
            permit.valid_until = fresh.lease_until.min(
                fresh
                    .plan_id
                    .as_ref()
                    .and_then(|id| fresh.decisions.get(id))
                    .map_or(u64::MAX, |decision| decision.valid_until),
            );
        }
        self.append_batch_inner(&context.campaign_id, cas, &[event], None, Some(&permit))?;
        Ok(())
    }
}

/// Canonical Round checks run inside the same transaction as the Task prefix comparison.
pub(in crate::store) fn validate_selection(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    round_id: &str,
    active: &review_core::RoundStartedPayloadV1,
    event: &NewEvent,
) -> Result<(), StoreError> {
    let selected: TaskReviewResultSelectedV1 = serde_json::from_value(event.payload.clone())?;
    let context: TaskReviewContextV1 = payload(cas, &selected.context_id, TASK_REVIEW_CONTEXT_V1)?;
    context.validate().map_err(conflict)?;
    if context.campaign_id != run_id
        || context.round_event_id != round_id
        || event.node_id.as_deref() != Some(context.review_node.as_str())
        || event.attempt_id.as_deref() != Some(context.attempt_id.as_str())
        || context.subject_id != active.subject_id
        || context.campaign_manifest_id != active.campaign_manifest_id
        || context.task_invocation_id != selected.invocation_id
    {
        return Err(conflict(
            "Selected Task Review context differs from canonical Round authority",
        ));
    }
    let invocation: u64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND causation_id = ?2 AND node_id = ?3
         AND event_id = ?4 AND type = 'NodeInvocation@1'",
        params![
            run_id,
            round_id,
            context.review_node,
            context.invocation_event_id
        ],
        |row| u64_column(row, 0),
    )?;
    let prior: u64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND causation_id = ?2 AND node_id = ?3
         AND type IN ('TaskReviewResultSelected@1', 'NodeOutputReceipt@1')",
        params![run_id, round_id, context.review_node],
        |row| u64_column(row, 0),
    )?;
    if invocation != 1 || prior != 0 {
        return Err(conflict(
            "Task Review selection needs its exact invocation and no prior selection",
        ));
    }
    Ok(())
}

fn validate_attempt_provenance(
    cas: &Cas,
    metadata: &TaskReviewResultMetadataV1,
    context: &TaskReviewContextV1,
    context_id: &str,
    producer: &review_core::Producer,
    reserved_tokens: u64,
    committed_tokens: u128,
) -> Result<(), StoreError> {
    let value = cas
        .get_json(&metadata.provenance_artifact_id)
        .map_err(|e| conflict(e.to_string()))?;
    if value.get("type").is_none() {
        return validate_legacy_provenance(cas, &value, metadata, context, committed_tokens);
    }
    let frame = cas
        .get_artifact(&metadata.provenance_artifact_id)
        .map_err(|e| conflict(e.to_string()))?;
    let legacy = frame.artifact_type == TASK_REVIEW_ATTEMPT_PROVENANCE_V1;
    let provenance: TaskReviewAttemptProvenanceV2 = match frame.artifact_type.as_str() {
        TASK_REVIEW_ATTEMPT_PROVENANCE_V1 => {
            let value: TaskReviewAttemptProvenanceV1 =
                serde_json::from_value(frame.payload.clone())?;
            value.validate().map_err(conflict)?;
            value.into()
        }
        TASK_REVIEW_ATTEMPT_PROVENANCE_V2 => serde_json::from_value(frame.payload.clone())?,
        _ => return Err(conflict("Unsupported Task Review provenance version")),
    };
    provenance.validate().map_err(conflict)?;
    if provenance.charged_tokens.get() > committed_tokens {
        return Err(conflict(
            "Review provenance exceeds its committed Attempt charge",
        ));
    }
    let subject: review_core::SubjectV1 = serde_json::from_value(
        cas.get_json(&context.subject_id)
            .map_err(|e| conflict(e.to_string()))?,
    )?;
    if &frame.producer != producer
        || provenance.context_id != context_id
        || provenance.task_invocation_id != context.task_invocation_id
        || provenance.attempt_id != context.attempt_id
        || provenance.review_node != context.review_node
        || provenance.result_artifact_id != metadata.result_artifact_id
        || frame.subject_snapshot_id.as_deref() != Some(&subject.head_snapshot_id)
        || frame.input_artifacts != provenance.artifact_refs()
    {
        return Err(conflict(
            "Task Review provenance changed its selected Attempt, context or result",
        ));
    }
    for reference in provenance.artifact_refs() {
        cas.verify(reference).map_err(|e| conflict(e.to_string()))?;
    }
    if let Some(id) = provenance.usage_id {
        use review_core::task::usage::*;
        let usage = cas.get_artifact(&id).map_err(|e| conflict(e.to_string()))?;
        let value: TaskTokenUsageV3 = match usage.artifact_type.as_str() {
            TASK_TOKEN_USAGE_V1 if legacy => {
                serde_json::from_value::<TaskTokenUsageV1>(usage.payload.clone())?.into()
            }
            TASK_TOKEN_USAGE_V2 if !legacy => {
                serde_json::from_value::<TaskTokenUsageV2>(usage.payload.clone())?.into()
            }
            TASK_TOKEN_USAGE_V3 if !legacy => serde_json::from_value(usage.payload.clone())?,
            _ => {
                return Err(conflict(
                    "Task Review provenance has another usage generation",
                ));
            }
        };
        if &usage.producer != producer
            || usage.input_artifacts != [context_id]
            || usage.subject_snapshot_id.is_some()
            || value.chargeable_tokens != provenance.charged_tokens
        {
            return Err(conflict(
                "Task Review provenance changed its reported usage",
            ));
        }
    } else if provenance.charged_tokens.get() != u128::from(reserved_tokens) {
        return Err(conflict(
            "Unknown Review usage must retain its full reservation",
        ));
    }
    Ok(())
}

/// A Task result's side metadata fixes its Proposal disposition before common selection.
pub(in crate::store) fn validate_proposal(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    round_id: &str,
    event: &NewEvent,
) -> Result<(), StoreError> {
    let selected: Option<String> = tx
        .query_row(
            "SELECT payload FROM events WHERE run_id = ?1 AND causation_id = ?2
         AND node_id = ?3 AND attempt_id = ?4 AND type = 'TaskReviewResultSelected@1'",
            params![run_id, round_id, event.node_id, event.attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(selected) = selected else {
        return Err(conflict("Proposal has no Task Review selection"));
    };
    let selected: TaskReviewResultSelectedV1 = serde_json::from_str(&selected)?;
    let metadata: TaskReviewResultMetadataV1 = payload(
        cas,
        &selected.metadata_envelope_id,
        TASK_REVIEW_RESULT_METADATA_V1,
    )?;
    metadata.validate().map_err(conflict)?;
    let matches = match event.event_type {
        EventType::ProposalPreparedV1 => {
            let proposed: review_core::ProposalPreparedPayloadV1 =
                serde_json::from_value(event.payload.clone())?;
            matches!(metadata.proposal, TaskReviewProposalV1::Prepared { candidate_artifact_id }
                if candidate_artifact_id == proposed.candidate_artifact_id)
        }
        EventType::ProposalRefusedV1 => {
            let refused: review_core::ProposalRefusedPayloadV1 =
                serde_json::from_value(event.payload.clone())?;
            matches!(metadata.proposal, TaskReviewProposalV1::Refused { reason } if reason == refused.reason)
        }
        _ => false,
    };
    if metadata.result_artifact_id != selected.result_artifact_id || !matches {
        return Err(conflict(
            "Canonical Proposal differs from the selected Task's disposition",
        ));
    }
    Ok(())
}

fn validate_legacy_provenance(
    cas: &Cas,
    value: &serde_json::Value,
    metadata: &TaskReviewResultMetadataV1,
    context: &TaskReviewContextV1,
    committed_tokens: u128,
) -> Result<(), StoreError> {
    let fields = [
        "node",
        "attempt",
        "result_artifact",
        "cost_tokens",
        "usage",
        "context_manifest",
        "raw",
        "sandbox_mutations",
    ];
    let object = value
        .as_object()
        .ok_or_else(|| conflict("Expected historical Review provenance object"))?;
    if object.len() != fields.len()
        || fields.iter().any(|key| !object.contains_key(*key))
        || value["node"] != context.review_node
        || value["attempt"] != context.attempt_id
        || value["result_artifact"] != metadata.result_artifact_id
        || value["context_manifest"]
            != cas
                .get_json(&context.context_manifest_id)
                .map_err(|e| conflict(e.to_string()))?
    {
        return Err(conflict(
            "Historical Review provenance changed its Attempt, result or context",
        ));
    }
    let usage = value["usage"]
        .as_object()
        .ok_or_else(|| conflict("Historical Review provenance has no usage"))?;
    let safe_counter = |value: &serde_json::Value| {
        value
            .as_u64()
            .filter(|value| *value <= review_core::json::SAFE_INTEGER_MAX as u64)
    };
    let charge = safe_counter(&value["cost_tokens"])
        .ok_or_else(|| conflict("Historical Review charge is not exact"))?;
    if u128::from(charge) > committed_tokens
        || safe_counter(&value["usage"]["chargeable_tokens"]) != Some(charge)
        || usage.iter().any(|(name, value)| {
            !matches!(
                name.as_str(),
                "chargeable_tokens"
                    | "input_tokens"
                    | "output_tokens"
                    | "cache_read_tokens"
                    | "cache_write_tokens"
                    | "reasoning_tokens"
            ) || safe_counter(value).is_none()
        })
    {
        return Err(conflict(
            "Historical Review provenance changed its exact usage",
        ));
    }
    for value in [&value["raw"], &value["sandbox_mutations"]["artifact"]] {
        let id = value
            .as_str()
            .filter(|id| review_core::is_digest(id))
            .ok_or_else(|| {
                conflict("Historical Review provenance has an invalid observation reference")
            })?;
        cas.verify(id).map_err(|e| conflict(e.to_string()))?;
    }
    Ok(())
}

fn validate_report_gate_failures(
    cas: &Cas,
    state: &TaskProjection,
    round: &LegacyReviewRoundV1,
    report: &review_core::RunReportPayloadV6,
) -> Result<(), StoreError> {
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Review Task has no execution"))?;
    let mut plans = BTreeMap::<String, (LegacyReviewRoundV1, CompiledTask)>::new();
    let mut failures = BTreeMap::new();
    for attempt in execution.attempt_accounting() {
        let attempt_id = &attempt.attempt_id;
        let Some(TaskExecutionRecordV1::Settled {
            raw_artifact_ids, ..
        }) = &execution.attempts[attempt_id].settlement
        else {
            continue;
        };
        if !plans.contains_key(&attempt.plan_id) {
            let plan: ExecutionPlanV1 = payload(cas, &attempt.plan_id, task::EXECUTION_PLAN_V1)?;
            let Some(input) = plan
                .inputs
                .values()
                .find(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1)
            else {
                // Earlier preparation work has no Review Gate authority.
                continue;
            };
            let captured: LegacyReviewRoundV1 =
                payload(cas, &input.artifact_ids[0], LEGACY_REVIEW_ROUND_V1)?;
            captured.validate().map_err(conflict)?;
            let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
            plans.insert(attempt.plan_id.clone(), (captured, graph));
        }
        let (captured, graph) = &plans[&attempt.plan_id];
        if captured.round_event_id != round.round_event_id {
            continue;
        }
        let node = execution.resolve_attempt_node(&attempt, graph)?;
        let CompiledOperator::ReviewDomain {
            review_node,
            operation: review_graph::task::ReviewOperation::Gate,
        } = &node.definition.operator
        else {
            continue;
        };
        for id in raw_artifact_ids {
            let observed = cas
                .get_artifact(id)
                .map_err(|error| StoreError::Artifact(error.to_string()))?;
            if observed.artifact_type == review_core::task::runtime::TASK_RUNTIME_EVIDENCE_V1 {
                let evidence: review_core::task::runtime::TaskRuntimeEvidenceV1 =
                    serde_json::from_value(observed.payload)?;
                evidence.validate().map_err(conflict)?;
                let context_id = execution.attempts[attempt_id]
                    .context_id
                    .as_ref()
                    .ok_or_else(|| conflict("Review Gate has no admitted context"))?;
                if evidence.task_id != state.task_id
                    || evidence.attempt_id != *attempt_id
                    || evidence.node != attempt.reservation.node
                    || evidence.context_id != *context_id
                    || observed.producer
                        != (review_core::Producer::Attempt {
                            run_id: task_run_id(&state.task_id)?,
                            node_id: attempt.reservation.node.clone(),
                            attempt_id: attempt_id.clone(),
                        })
                    || observed.subject_snapshot_id.as_deref() != Some(&round.head_snapshot_id)
                {
                    return Err(conflict(
                        "Review Gate runtime evidence changed its settled Attempt authority",
                    ));
                }
                continue;
            }
            let frame = envelope(cas, id, TASK_REVIEW_GATE_FACTS_V1)?;
            let facts: TaskReviewGateFactsV1 = serde_json::from_value(frame.payload)?;
            facts.validate().map_err(conflict)?;
            let context_id = execution.attempts[attempt_id]
                .context_id
                .as_ref()
                .ok_or_else(|| conflict("Review Gate has no admitted context"))?;
            if facts.round_event_id != round.round_event_id
                || facts.review_node != *review_node
                || facts.attempt_id != *attempt_id
                || frame.producer
                    != (review_core::Producer::Attempt {
                        run_id: task_run_id(&state.task_id)?,
                        node_id: attempt.reservation.node.clone(),
                        attempt_id: attempt_id.clone(),
                    })
                || frame.subject_snapshot_id.as_deref() != Some(&round.head_snapshot_id)
                || frame.input_artifacts != [context_id.clone()]
            {
                return Err(conflict(
                    "Review Gate failure facts changed their settled Attempt authority",
                ));
            }
            cas.verify(context_id)
                .map_err(|e| conflict(e.to_string()))?;
            for failure in facts.cache_failures {
                let key = (failure.node.clone(), failure.kind);
                if failures
                    .insert(key, failure.clone())
                    .is_some_and(|previous| previous != failure)
                {
                    return Err(conflict(
                        "Review Gate has contradictory settled cache failures",
                    ));
                }
            }
        }
    }
    let recorded: BTreeMap<_, _> = match &report.execution {
        review_core::RunReportExecutionV6::Cached { cache_failures, .. } => cache_failures
            .iter()
            .map(|failure| ((failure.node.clone(), failure.kind), failure.clone()))
            .collect(),
        _ => BTreeMap::new(),
    };
    if recorded != failures {
        return Err(conflict(
            "Review conclusion differs from its settled Gate cache failures",
        ));
    }
    Ok(())
}

/// Derive exact canonical identity from admitted common facts without re-entering a host.
pub(super) fn selected_review_event(
    cas: &Cas,
    state: &TaskProjection,
    output_id: &str,
) -> Result<(TaskReviewContextV1, NewEvent), StoreError> {
    let wrapper = envelope(cas, output_id, TASK_OUTPUT_V1)?;
    let review_core::Producer::Attempt {
        run_id,
        node_id,
        attempt_id,
    } = &wrapper.producer
    else {
        return Err(conflict(
            "Review selection needs a paid common Task Attempt",
        ));
    };
    if run_id != &task_run_id(state.task_id.as_str())? {
        return Err(conflict("Review selection belongs to another Task"));
    }
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Task has no execution"))?;
    let recorded = execution
        .attempts
        .get(attempt_id)
        .ok_or_else(|| conflict("Unknown Task Review Attempt"))?;
    let (published_id, _) = execution
        .outputs
        .get(node_id)
        .ok_or_else(|| conflict("Task Review output is not published"))?;
    let selected = execution.ledger.attempt(&AttemptId(attempt_id.clone()));
    if published_id != output_id
        || selected
            .is_none_or(|a| a.node != *node_id || a.state != review_attempt::AttemptState::Selected)
        || !matches!(&recorded.settlement, Some(TaskExecutionRecordV1::Settled {
                result: TaskAttemptResultV1::Succeeded { output_id: selected }, ..
            }) if selected == output_id)
    {
        return Err(conflict(
            "Review selection differs from the common Task's selected output",
        ));
    }
    let out = output(cas, output_id)?;
    let input = execution.verify_output(cas, &out)?;
    verify_attempt_producer(
        cas,
        &state.task_id,
        &input.node,
        attempt_id,
        output_id,
        &out,
    )?;
    let context_id = recorded
        .context_id
        .as_ref()
        .ok_or_else(|| conflict("Task Review Attempt has no context"))?;
    let context: TaskReviewContextV1 = payload(cas, context_id, TASK_REVIEW_CONTEXT_V1)?;
    context.validate().map_err(conflict)?;
    if context.attempt_id != *attempt_id
        || context.task_invocation_id != out.invocation_id
        || recorded.invocation_id != out.invocation_id
        || recorded.plan_id != input.plan_id
    {
        return Err(conflict(
            "Review context belongs to another common invocation or Attempt",
        ));
    }
    // These two host-declared ports are the flat business result and its side metadata.
    // They cannot be selected from a larger, ambiguous set of Worker outputs.
    let mut result = None;
    let mut metadata = None;
    for port in out.outputs.values() {
        let [id] = port.artifact_ids.as_slice() else {
            return Err(conflict(
                "Task Review output must have singular typed ports",
            ));
        };
        if port.cardinality != review_core::PortCardinality::One {
            return Err(conflict(
                "Task Review output needs one result and one metadata port",
            ));
        }
        if port.artifact_type == TASK_REVIEW_RESULT_METADATA_V1 {
            if metadata.replace(id).is_some() {
                return Err(conflict("Duplicate Review metadata port"));
            }
        } else if review_core::ReviewerResultContract::parse_artifact_type(&port.artifact_type)
            .is_some()
        {
            if result.replace((id, &port.artifact_type)).is_some() {
                return Err(conflict("Duplicate Review result port"));
            }
        } else {
            return Err(conflict("Undeclared compatibility Review output type"));
        }
    }
    let (result_envelope_id, result_type) =
        result.ok_or_else(|| conflict("Missing typed Review result"))?;
    let metadata_envelope_id = metadata.ok_or_else(|| conflict("Missing typed Review metadata"))?;
    let metadata: TaskReviewResultMetadataV1 =
        payload(cas, metadata_envelope_id, TASK_REVIEW_RESULT_METADATA_V1)?;
    metadata.validate().map_err(conflict)?;
    validate_attempt_provenance(
        cas,
        &metadata,
        &context,
        context_id,
        &review_core::Producer::Attempt {
            run_id: run_id.clone(),
            node_id: node_id.clone(),
            attempt_id: attempt_id.clone(),
        },
        recorded.reservation.tokens,
        selected.expect("selected Attempt checked above").charged,
    )?;
    let result = envelope(cas, result_envelope_id, result_type)?;
    if result_type != metadata.result_contract.artifact_type()
        || content_id(&result.payload).map_err(|e| conflict(e.to_string()))?
            != metadata.result_artifact_id
        || cas
            .get_json(&metadata.result_artifact_id)
            .map_err(|e| conflict(e.to_string()))?
            != result.payload
    {
        return Err(conflict(
            "Review side metadata contradicts the exact flat result",
        ));
    }
    match metadata.result_contract {
        review_core::ReviewerResultContract::V1 => {
            review_core::validate_reviewer_result(&result.payload)
        }
        review_core::ReviewerResultContract::V2 => {
            review_core::validate_reviewer_result_v2(&result.payload)
        }
    }
    .map_err(conflict)?;
    let selection = TaskReviewResultSelectedV1 {
        task_id: state.task_id.as_str().into(),
        task_revision_id: state.revision_id.clone(),
        plan_id: input.plan_id,
        task_node: input.node,
        invocation_id: out.invocation_id,
        output_id: output_id.into(),
        context_id: context_id.clone(),
        result_envelope_id: result_envelope_id.clone(),
        metadata_envelope_id: metadata_envelope_id.clone(),
        result_artifact_id: metadata.result_artifact_id.clone(),
        provenance_artifact_id: metadata.provenance_artifact_id.clone(),
    };
    selection.validate().map_err(conflict)?;
    let refs: BTreeSet<_> = selection
        .artifact_refs()
        .into_iter()
        .chain(context.artifact_refs())
        .chain(metadata.artifact_refs())
        .map(str::to_owned)
        .collect();
    // Exact replay also re-establishes side-input integrity; it does not reach the new
    // event publication barrier below, which would otherwise perform these checks.
    for id in &refs {
        cas.verify(id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
    }
    let event = NewEvent::new(
        EventType::TaskReviewResultSelectedV1,
        serde_json::to_value(&selection)?,
    )
    .node(&context.review_node)
    .attempt(attempt_id)
    .caused_by(&context.round_event_id)
    .referencing(refs.into_iter().collect());
    Ok((context, event))
}
