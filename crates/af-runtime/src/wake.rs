//! Pure, read-only wake extraction. No journal, dispatch, host API or canonical event writes.
use af_contracts::{
    EventType, RunEvent,
    task::{self, TaskPhaseV1, TaskResultV1, event::TaskChangeV1, wake::*},
};
use af_store::{
    Cas, EventStore, content_id,
    store::{
        StoreError,
        task::{self as stored, TaskProjection},
    },
};
use serde_json::json;
pub mod journal;

/// A candidate extraction, requiring current-history revalidation and transactional persistence
/// by the future reconciler. The cursor means examined history, never successful delivery.
pub struct WakeProjection {
    pub source: TaskProjection,
    pub checkpoint: WakeCheckpoint,
    pub caught_up: bool,
    pub observations: Vec<WakeObservationV1>,
    pub through: WakeBoundaryV1,
}
/// Opaque checked continuation. Only this projector constructs it; durable readers must use
/// `restore_checkpoint`, which reconstructs and compares its exact checked prefix.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WakeCheckpoint {
    schema: String,
    configuration_digest: String,
    run_facts_digest: String,
    through: WakeBoundaryV1,
    prior_semantic: Option<(u64, String, WakeStateV1)>,
    lease_boundary: WakeBoundaryV1,
    observations: u64,
    observed_unix_ms: u64,
    prefix_digest: String,
    clock_observed: bool,
}

/// Restore same-release control state, never a caller-authored lifecycle summary. Rebuild in
/// bounded pages and compare all bytes, including the rolling source fingerprint. No legacy
/// reader or unchecked deserialization supplies continuation authority.
pub fn restore_checkpoint(
    store: &EventStore,
    cas: &Cas,
    subscription: &WakeSubscriptionV1,
    bytes: &[u8],
) -> Result<WakeCheckpoint, StoreError> {
    restore_checkpoint_with_run_facts(store, cas, subscription, bytes, &[])
}

pub fn restore_checkpoint_with_run_facts(
    store: &EventStore,
    cas: &Cas,
    subscription: &WakeSubscriptionV1,
    bytes: &[u8],
    facts: &[RecordedRunFacts],
) -> Result<WakeCheckpoint, StoreError> {
    if bytes.len() > MAX_WAKE_BYTES {
        return Err(error("Wake checkpoint too large"));
    }
    let wire: WakeCheckpointV1 = serde_json::from_slice(bytes)?;
    wire.validate().map_err(error)?;
    let expected = WakeCheckpoint {
        schema: wire.schema,
        configuration_digest: wire.configuration_digest,
        run_facts_digest: wire.run_facts_digest,
        through: wire.through,
        prior_semantic: wire.prior_semantic,
        lease_boundary: wire.lease_boundary,
        observations: wire.observations,
        observed_unix_ms: wire.observed_unix_ms,
        prefix_digest: wire.prefix_digest,
        clock_observed: wire.clock_observed,
    };
    if expected.schema != "af/task-wake-checkpoint@1"
        || store
            .latest_run_sequence(&subscription.task.task_run_id)?
            .is_none_or(|last| expected.through.sequence > last)
    {
        return Err(error("Invalid wake checkpoint boundary"));
    }
    if expected.configuration_digest != subscription.configuration_digest
        || expected.run_facts_digest != facts_digest(facts)?
    {
        return Err(error("Foreign wake checkpoint configuration"));
    }
    if store.wake_checkpoint_is_retained(bytes_to_str(bytes)?)? {
        return Ok(expected);
    }
    let mut cursor = None;
    loop {
        let page = project_page(
            store,
            cas,
            subscription,
            cursor.as_ref(),
            expected.observed_unix_ms,
            facts,
            Some((expected.through.sequence, expected.clock_observed)),
        )?;
        if page.through.sequence == expected.through.sequence {
            if page.checkpoint != expected {
                return Err(error("Wake checkpoint provenance mismatch"));
            }
            return Ok(page.checkpoint);
        }
        cursor = Some(page.checkpoint);
    }
}

fn bytes_to_str(bytes: &[u8]) -> Result<&str, StoreError> {
    std::str::from_utf8(bytes).map_err(|e| error(e.to_string()))
}
fn error(message: impl Into<String>) -> StoreError {
    StoreError::Conflict(message.into())
}
fn boundary(event: &RunEvent) -> WakeBoundaryV1 {
    WakeBoundaryV1 {
        sequence: event.sequence,
        event_id: event.event_id.clone(),
    }
}
fn policy_time(event: &RunEvent) -> Result<u64, StoreError> {
    if stored::is_execution_started(event) {
        Ok(stored::read_execution_started(event)?.now_unix_ms)
    } else if stored::is_provider_event(event) {
        Ok(stored::read_provider_event(event)?.now_unix_ms)
    } else if stored::gate_retry::is_gate_retried(event) {
        Ok(stored::gate_retry::read_gate_retried(event)?.now_unix_ms)
    } else {
        Ok(stored::read_task_transition(event)?.now_unix_ms)
    }
}
fn semantics(
    cas: &Cas,
    state: &TaskProjection,
    at: u64,
    facts: &[RecordedRunFacts],
) -> Result<WakeStateV1, StoreError> {
    let result = if let TaskPhaseV1::Finished { result_id } = &state.phase {
        let envelope = cas
            .get_artifact(result_id)
            .map_err(|e| error(e.to_string()))?;
        if envelope.artifact_type != task::TASK_RESULT_V1 {
            return Err(error("Wrong completion result type"));
        }
        let result: TaskResultV1 = serde_json::from_value(envelope.payload)?;
        result.validate().map_err(error)?;
        Some(result)
    } else {
        None
    };
    let cancelled = facts
        .iter()
        .filter_map(|f| f.cancellation.as_ref())
        .find(|c| {
            c.epoch == state.lease_epoch()
                && c.writer == state.lease_writer()
                && c.requested_unix_ms <= at
        })
        .map(|c| c.epoch);
    crate::task_lifecycle::semantic_state(cas, state, result.as_ref(), at, cancelled).map_err(error)
}
fn kind(state: &WakeStateV1) -> Option<WakeKindV1> {
    use af_contracts::task::state::TaskStatePhaseV1::*;
    match state.phase {
        Waiting => Some(WakeKindV1::AttentionRequired),
        Abandoned => Some(WakeKindV1::ExecutionStopped),
        Finished => Some(WakeKindV1::Completed),
        _ => None,
    }
}

/// Original typed run/cancellation records supplied by the trusted recorder, never OS guesses.
/// Projection validates their exact Task/run/process/writer/epoch joins against checked history.
#[derive(serde::Serialize)]
pub struct RecordedRunFacts {
    pub process: task::agent_loop::TaskRunProcessV1,
    pub cancellation: Option<task::agent_loop::TaskCancelRequestV1>,
}

fn facts_digest(facts: &[RecordedRunFacts]) -> Result<String, StoreError> {
    if facts.len() > 4096 {
        return Err(error("Too many recorded run facts"));
    }
    content_id(&json!({"domain":"af/wake-run-facts/1","facts":facts}))
        .map_err(|e| error(e.to_string()))
}

pub fn project(
    store: &EventStore,
    cas: &Cas,
    subscription: &WakeSubscriptionV1,
    cursor: Option<&WakeCheckpoint>,
    observed_unix_ms: u64,
) -> Result<WakeProjection, StoreError> {
    project_with_run_facts(store, cas, subscription, cursor, observed_unix_ms, &[])
}

pub fn project_with_run_facts(
    store: &EventStore,
    cas: &Cas,
    subscription: &WakeSubscriptionV1,
    cursor: Option<&WakeCheckpoint>,
    observed_unix_ms: u64,
    facts: &[RecordedRunFacts],
) -> Result<WakeProjection, StoreError> {
    if facts.len() > 4096 {
        return Err(error("Too many recorded run facts"));
    }
    project_page(
        store,
        cas,
        subscription,
        cursor,
        observed_unix_ms,
        facts,
        None,
    )
}

fn project_page(
    store: &EventStore,
    cas: &Cas,
    subscription: &WakeSubscriptionV1,
    cursor: Option<&WakeCheckpoint>,
    observed_unix_ms: u64,
    facts: &[RecordedRunFacts],
    stop_at: Option<(u64, bool)>,
) -> Result<WakeProjection, StoreError> {
    let database = store.database_observation()?;
    let ((result, runs), reads) = cas.track_reads(|| {
        store.track_replays(|| {
            project_inner(
                store,
                cas,
                subscription,
                cursor,
                observed_unix_ms,
                facts,
                stop_at,
            )
        })
    });
    let result = result?;
    let current = store
        .task_projection(cas, &subscription.task.task_id)?
        .ok_or_else(|| error("Wake Task disappeared"))?;
    if !store.task_observation_unchanged_with(cas, &current, &runs, || {
        for id in &reads {
            cas.verify(id).map_err(|e| error(e.to_string()))?;
        }
        Ok(())
    })? || !store.database_observation_unchanged(&database)?
    {
        return Err(error("Wake source changed; revalidate before committing"));
    }
    Ok(result)
}

/// Replay all eligible boundaries in a bounded checked history, including downtime. The supplied
/// Store incarnation is opaque configuration evidence, not allocated or activated here. This API
/// accepts no process-liveness guesses or caller-classified lifecycle summaries.
fn project_inner(
    store: &EventStore,
    cas: &Cas,
    subscription: &WakeSubscriptionV1,
    cursor: Option<&WakeCheckpoint>,
    observed_unix_ms: u64,
    facts: &[RecordedRunFacts],
    stop_at: Option<(u64, bool)>,
) -> Result<WakeProjection, StoreError> {
    subscription.validate().map_err(error)?;
    if subscription.configuration_digest != configuration_digest(subscription).map_err(error)? {
        return Err(error("Wake configuration digest mismatch"));
    }
    if observed_unix_ms > subscription.expires_unix_ms {
        return Err(error("Wake subscription has expired"));
    }
    if observed_unix_ms == 0 || observed_unix_ms > af_contracts::json::SAFE_INTEGER_MAX as u64 {
        return Err(error("Invalid wake observation time"));
    }
    let task = &subscription.task;
    if stored::task_run_id(&task.task_id)? != task.task_run_id {
        return Err(error("Wake Task run identity mismatch"));
    }
    let first = store
        .replay_window(&task.task_run_id, 0, 1)?
        .pop()
        .ok_or_else(|| error("Unknown wake Task"))?;
    if task.opening_event_id != first.event_id {
        return Err(error("Wake opening identity mismatch"));
    }
    let final_state = store
        .task_projection(cas, &task.task_id)?
        .ok_or_else(|| error("Unknown wake Task"))?;
    let last = final_state.next_sequence - 1;
    let scan_last = stop_at.map_or(last, |(sequence, _)| sequence).min(last);
    let run_facts_digest = facts_digest(facts)?;
    if cursor.is_some_and(|c| {
        c.schema != "af/task-wake-checkpoint@1"
            || c.configuration_digest != subscription.configuration_digest
            || c.run_facts_digest != run_facts_digest
            || c.through.sequence > scan_last
            || c.observed_unix_ms > observed_unix_ms
    }) {
        return Err(error("Invalid wake checkpoint scope or clock"));
    }
    // Revalidate exact source bytes in fixed-size pages; event IDs alone are position-derived.
    // This does no lifecycle extraction and never buffers the complete prefix. Canonical Task
    // validation below retains its own authority and CAS-corruption checks.
    let mut prefix_digest = content_id(&json!({"domain":"af/wake-prefix/1","task":task}))
        .map_err(|e| error(e.to_string()))?;
    if let Some(c) = cursor {
        if store.wake_checkpoint_is_retained(&serde_json::to_string(c)?)? {
            prefix_digest = c.prefix_digest.clone();
        } else {
            let mut from = 0;
            while from <= c.through.sequence {
                let page = store.replay_window(
                    &task.task_run_id,
                    from,
                    (c.through.sequence + 1 - from).min(4096),
                )?;
                if page.is_empty() {
                    return Err(error("Wake checkpoint beyond source"));
                }
                for e in &page {
                    prefix_digest = prefix_hash(&prefix_digest, e)?;
                }
                from += page.len() as u64;
                if from == c.through.sequence + 1 && boundary(page.last().unwrap()) != c.through {
                    return Err(error("Wake checkpoint boundary mismatch"));
                }
            }
            if prefix_digest != c.prefix_digest {
                return Err(error("Wake checkpoint source changed"));
            }
        }
    }
    let from = cursor.map_or(0, |c| c.through.sequence + 1);
    let rows = (scan_last + 1).saturating_sub(from).min(4096);
    let events = if rows == 0 {
        Vec::new()
    } else {
        store.replay_window(&task.task_run_id, from, rows)?
    };
    // Run facts join canonical history, independent of the extraction page.
    let mut checked_writers = std::collections::BTreeSet::new();
    let mut checked_epochs = std::collections::BTreeSet::new();
    if !facts.is_empty() {
        let mut from = 0;
        while from <= last {
            let events = store.replay_window(&task.task_run_id, from, 4096)?;
            if events.is_empty() {
                return Err(error("Recorded wake fact history gap"));
            }
            let mut bytes = 0usize;
            for event in &events {
                bytes += serde_json::to_vec(event)?.len();
                if bytes > 16 * 1024 * 1024 {
                    return Err(error("Recorded wake fact page exceeds byte limit"));
                }
                if let Ok(t) = stored::read_task_transition(event)
                    && matches!(
                        t.change,
                        TaskChangeV1::Opened { .. } | TaskChangeV1::LeaseTaken { .. }
                    )
                {
                    for fact in facts {
                        if t.writer == fact.process.writer {
                            checked_writers.insert(t.writer.clone());
                        }
                        if fact
                            .cancellation
                            .as_ref()
                            .is_some_and(|c| c.writer == t.writer && c.epoch == t.epoch)
                        {
                            checked_epochs.insert((t.writer.clone(), t.epoch));
                        }
                    }
                }
            }
            from = events
                .last()
                .ok_or_else(|| error("Missing fact page"))?
                .sequence
                + 1;
        }
    }
    let mut run_ids = std::collections::BTreeSet::new();
    for fact in facts {
        fact.process.validate().map_err(error)?;
        if fact.process.task_id != task.task_id || !run_ids.insert(fact.process.run) {
            return Err(error("Recorded wake run identity mismatch or duplicate"));
        }
        let writer_exists = checked_writers.contains(&fact.process.writer);
        if !writer_exists {
            return Err(error("Recorded wake writer absent from checked history"));
        }
        if let Some(cancel) = &fact.cancellation {
            cancel.validate().map_err(error)?;
            let process = &fact.process;
            if cancel.task_id != process.task_id
                || cancel.run != process.run
                || cancel.pid != process.pid
                || cancel.host != process.host
                || cancel.writer != process.writer
                || cancel.requested_unix_ms < process.started_unix_ms
                || !checked_epochs.contains(&(cancel.writer.clone(), cancel.epoch))
            {
                return Err(error(
                    "Recorded wake cancellation does not bind its checked run/lease",
                ));
            }
        }
    }
    let mut observations = Vec::new();
    let mut previous = cursor
        .map(|c| {
            store
                .task_projection_at_sequence(cas, &task.task_id, c.through.sequence)
                .and_then(|s| s.ok_or_else(|| error("Missing checkpoint prefix")))
        })
        .transpose()?;
    let mut prior_semantic = cursor.and_then(|c| c.prior_semantic.clone());
    let mut episode = previous.as_ref().map_or(1, |s| s.lifecycle_episode());
    let mut lease_boundary = cursor.map_or_else(|| boundary(&first), |c| c.lease_boundary.clone());
    let mut last_boundary = cursor.map_or_else(|| boundary(&first), |c| c.through.clone());
    for event in &events {
        prefix_digest = prefix_hash(&prefix_digest, event)?;
        let at = policy_time(event)?;
        if at > observed_unix_ms {
            return Err(error("Wake observation precedes checked history"));
        }
        // Expiry can become observable between recorded events. The persisted deadline is its
        // authority; the later event only supplies an explicit observation clock.
        if let Some(state) = &previous {
            let deadline = state.lease_until_unix_ms();
            if deadline > 0 && deadline <= at {
                let semantic = semantics(cas, state, deadline, facts)?;
                emit(
                    &mut observations,
                    &mut prior_semantic,
                    task,
                    state,
                    semantic,
                    episode,
                    &lease_boundary,
                    &last_boundary,
                    WakeEvidenceV1::LeaseExpired {
                        epoch: state.lease_epoch(),
                        deadline_unix_ms: deadline,
                    },
                    at,
                )?;
            }
        }
        let state = store.advance_task_projection(cas, &task.task_id, previous.take(), event)?;
        if state.next_sequence != event.sequence + 1 {
            return Err(error("Invalid checked wake prefix"));
        }
        episode = state.lifecycle_episode();
        if event.event_type == EventType::TaskTransitionV6 {
            match stored::read_task_transition(event)?.change {
                TaskChangeV1::Opened { .. }
                | TaskChangeV1::LeaseTaken { .. }
                | TaskChangeV1::LeaseRenewed { .. }
                | TaskChangeV1::LeaseReleased {} => lease_boundary = boundary(event),
                _ => {}
            }
        }
        let semantic = semantics(cas, &state, at, facts)?;
        emit(
            &mut observations,
            &mut prior_semantic,
            task,
            &state,
            semantic,
            episode,
            &boundary(event),
            &boundary(event),
            WakeEvidenceV1::History {},
            at,
        )?;
        last_boundary = boundary(event);
        previous = Some(state);
    }
    let state = previous.ok_or_else(|| error("Empty wake history"))?;
    let caught_up = last_boundary.sequence == last;
    // A page boundary is not an observation of lease expiry. Observe the explicit clock only
    // when caught up, otherwise a later checked renewal in the next page could supersede it.
    let clock_observed = stop_at.map_or(caught_up, |(_, observe)| {
        observe && last_boundary.sequence == scan_last
    });
    if clock_observed {
        let semantic = semantics(cas, &state, observed_unix_ms, facts)?;
        emit(
            &mut observations,
            &mut prior_semantic,
            task,
            &state,
            semantic,
            episode,
            &lease_boundary,
            &last_boundary,
            WakeEvidenceV1::LeaseExpired {
                epoch: state.lease_epoch(),
                deadline_unix_ms: state.lease_until_unix_ms(),
            },
            observed_unix_ms,
        )?;
    }
    let count = cursor.map_or(0, |c| c.observations) + observations.len() as u64;
    if count > subscription.max_observations {
        return Err(error("Wake observation allowance exhausted"));
    }
    if !store.task_observation_unchanged(cas, &final_state)? {
        return Err(error("Wake source changed; revalidate before committing"));
    }
    let checkpoint = WakeCheckpoint {
        schema: "af/task-wake-checkpoint@1".into(),
        configuration_digest: subscription.configuration_digest.clone(),
        run_facts_digest: facts_digest(facts)?,
        through: last_boundary.clone(),
        prior_semantic,
        lease_boundary,
        observations: count,
        observed_unix_ms,
        prefix_digest,
        clock_observed,
    };
    Ok(WakeProjection {
        source: final_state,
        observations,
        through: last_boundary,
        checkpoint,
        caught_up,
    })
}

fn prefix_hash(prior: &str, event: &RunEvent) -> Result<String, StoreError> {
    content_id(&json!({"prior":prior,"event":event})).map_err(|e| error(e.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut Vec<WakeObservationV1>,
    previous: &mut Option<(u64, String, WakeStateV1)>,
    task: &WakeTaskV1,
    state: &TaskProjection,
    semantic: WakeStateV1,
    episode: u64,
    causal: &WakeBoundaryV1,
    prefix: &WakeBoundaryV1,
    evidence: WakeEvidenceV1,
    at: u64,
) -> Result<(), StoreError> {
    // A maintenance lease may render Running in the CLI, but it neither resumes execution
    // nor reopens a wake boundary. Keep the previous boundary until an authorized restart
    // changes the episode (or a recorded phase/revision supplies a real lifecycle transition).
    if matches!(state.phase, TaskPhaseV1::Running {})
        && !state.execution_epochs.contains(&state.lease_epoch())
        && previous.as_ref().is_some_and(|(ep, revision, s)| {
            *ep == episode && revision == &state.revision_id && kind(s).is_some()
        })
    {
        return Ok(());
    }
    // Accounting or cleanup may refine a cause but cannot create another lifecycle boundary.
    let changed = previous.as_ref().is_none_or(|(ep, revision, s)| {
        *ep != episode
            || revision != &state.revision_id
            || s.phase != semantic.phase
            || s.waiting_reason != semantic.waiting_reason
    });
    *previous = Some((episode, state.revision_id.clone(), semantic.clone()));
    let Some(kind) = kind(&semantic).filter(|_| changed) else {
        return Ok(());
    };
    let result_id = match &state.phase {
        TaskPhaseV1::Finished { result_id } => Some(result_id.clone()),
        _ => None,
    };
    let observation_id = content_id(&json!({"domain":"af/wake-observation/1","task":task,"causal":causal,"revision":state.revision_id,"episode":episode,"kind":kind,"evidence":evidence})).map_err(|e| error(e.to_string()))?;
    let observation = WakeObservationV1 {
        schema: WAKE_OBSERVATION_V1.into(),
        task: task.clone(),
        observation_id,
        causal: causal.clone(),
        checked_prefix: prefix.clone(),
        revision_id: state.revision_id.clone(),
        episode,
        kind,
        state: semantic,
        evidence,
        observed_unix_ms: at,
        result_id,
    };
    observation.validate().map_err(error)?;
    out.push(observation);
    Ok(())
}

pub use af_store::store::wake::{
    configuration_digest, decode_envelope, envelope, same_registration,
};
