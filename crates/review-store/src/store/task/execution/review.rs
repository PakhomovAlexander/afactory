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
            |row| row.get(0),
        )?;
        if run_id != self.review_run_id
            || u64::try_from(first).ok() != Some(self.review_sequence)
            || sequence != self.task_sequence
            || now()? >= self.valid_until
            || event.event_type != EventType::TaskReviewResultSelectedV1
            || event.payload != self.event.payload
            || event.artifact_refs != self.event.artifact_refs
            || event.node_id != self.event.node_id
            || event.attempt_id != self.event.attempt_id
            || event.causation_id != self.event.causation_id
            || event.correlation_id.is_some()
            || event.legacy_import
        {
            return Err(conflict(
                "Task Review publication lost its exact execution/lease comparison",
            ));
        }
        Ok(())
    }
}

impl EventStore {
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
        let (state, plan) = self.checked_task_dispatch(cas, lease, authority)?;
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
        if run_id != &task_run_id(lease.task_id())? {
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
            || selected.is_none_or(|a| {
                a.node != *node_id || a.state != review_attempt::AttemptState::Selected
            })
            || !matches!(&recorded.settlement, Some(TaskExecutionRecordV1::Settled {
                result: TaskAttemptResultV1::Succeeded { output_id: selected }, ..
            }) if selected == output_id)
        {
            return Err(conflict(
                "Review selection differs from the common Task's selected output",
            ));
        }
        let (out, input) =
            Self::validate_task_output(cas, &state, &plan, output_id, Some(attempt_id), authority)?;
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
        let metadata_envelope_id =
            metadata.ok_or_else(|| conflict("Missing typed Review metadata"))?;
        let metadata: TaskReviewResultMetadataV1 =
            payload(cas, metadata_envelope_id, TASK_REVIEW_RESULT_METADATA_V1)?;
        metadata.validate().map_err(conflict)?;
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
            task_id: lease.task_id().into(),
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
            serde_json::to_value(selection)?,
        )
        .node(&context.review_node)
        .attempt(attempt_id)
        .caused_by(&context.round_event_id)
        .referencing(refs.into_iter().collect());
        // Recheck after domain validation, including approval revocation and CAS integrity.
        let (fresh, _) = self.checked_task_dispatch(cas, lease, authority)?;
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
        let permit = WritePermit::for_checked_selection(
            &fresh,
            &plan,
            &context.campaign_id,
            self.len(&context.campaign_id)?,
            event.clone(),
        )?;
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
        |row| row.get(0),
    )?;
    let prior: u64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND causation_id = ?2 AND node_id = ?3
         AND type IN ('TaskReviewResultSelected@1', 'AttemptDispatched@1', 'AttemptAdmitted@1', 'NodeOutputReceipt@1')",
        params![run_id, round_id, context.review_node],
        |row| row.get(0),
    )?;
    if invocation != 1 || prior != 0 {
        return Err(conflict(
            "Task Review selection needs its exact invocation and no prior selection",
        ));
    }
    Ok(())
}

/// A Task result's side metadata fixes its Proposal disposition before common selection.
/// Legacy admissions retain their original guard; they have no Task metadata to reinterpret.
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
        return Ok(());
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
