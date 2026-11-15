//! Deterministic extraction and atomic persistence; no transport or Task dispatch.
use super::*;

/// Reconcile one bounded page. The old serialized checkpoint and the current checked source
/// are both compare-and-swap fences. Competing reconcilers cannot overwrite each other's page.
pub fn reconcile(
    store: &EventStore,
    cas: &Cas,
    subscription_id: &str,
    observed_unix_ms: u64,
) -> Result<bool, StoreError> {
    reconcile_with_run_facts(
        store,
        cas,
        subscription_id,
        observed_unix_ms,
        &[],
        || Ok(()),
    )
}

pub fn reconcile_with_run_facts(
    store: &EventStore,
    cas: &Cas,
    subscription_id: &str,
    observed_unix_ms: u64,
    facts: &[RecordedRunFacts],
    recorder_unchanged: impl Fn() -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    reconcile_with_run_facts_observed(
        store,
        cas,
        subscription_id,
        observed_unix_ms,
        facts,
        recorder_unchanged,
        |_| Ok(()),
    )
}

/// As above, with read/commit observation seams for deterministic concurrent writers.
pub fn reconcile_with_run_facts_observed(
    store: &EventStore,
    cas: &Cas,
    subscription_id: &str,
    observed_unix_ms: u64,
    facts: &[RecordedRunFacts],
    recorder_unchanged: impl Fn() -> Result<(), StoreError>,
    observe: impl Fn(ReconcilePoint) -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    reconcile_observed(
        store,
        cas,
        subscription_id,
        observed_unix_ms,
        facts,
        recorder_unchanged,
        observe,
        None,
    )
}

/// Live driver clock: capture before inspection, then sample after checked source capture.
/// Clock races yield no page; only fresh serialized validation may authorize a bounded deferral.
pub fn reconcile_live_with_run_facts_observed(
    store: &EventStore,
    cas: &Cas,
    subscription_id: &str,
    clock: impl Fn() -> u64,
    facts: &[RecordedRunFacts],
    recorder_unchanged: impl Fn() -> Result<(), StoreError>,
    observe: impl Fn(ReconcilePoint) -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    reconcile_observed(
        store,
        cas,
        subscription_id,
        clock(),
        facts,
        recorder_unchanged,
        observe,
        Some(&clock),
    )
}

#[allow(clippy::too_many_arguments)]
fn reconcile_observed(
    store: &EventStore,
    cas: &Cas,
    subscription_id: &str,
    observed_unix_ms: u64,
    facts: &[RecordedRunFacts],
    recorder_unchanged: impl Fn() -> Result<(), StoreError>,
    observe: impl Fn(ReconcilePoint) -> Result<(), StoreError>,
    clock: Option<&dyn Fn() -> u64>,
) -> Result<bool, StoreError> {
    observe(ReconcilePoint::ClockCaptured)?;
    let retained = store.inspect_wake(subscription_id)?;
    if !retained.source_available {
        return Err(error(
            "Wake source was collected; retained envelopes are historical only",
        ));
    }
    let subscription = &retained.registration.subscription;
    let result = (|| {
        let mut prior_clock = None;
        let cursor = if let Some(text) = &retained.checkpoint {
            let prior: WakeCheckpointV1 = serde_json::from_str(text)?;
            prior.validate().map_err(error)?;
            prior_clock = Some(prior.observed_unix_ms);
            // Changed authenticated cancellation facts require checked replay. Each rebuilt
            // page persists a bounded continuation rather than trusting stale semantics.
            if prior.run_facts_digest == facts_digest(facts)? {
                Some(restore_checkpoint_with_run_facts(
                    store,
                    cas,
                    subscription,
                    text.as_bytes(),
                    facts,
                )?)
            } else {
                None
            }
        } else {
            None
        };
        let page = project_page(
            store,
            cas,
            subscription,
            cursor.as_ref(),
            observed_unix_ms,
            facts,
            None,
            &observe,
            clock,
        )?;
        if clock.is_some() && prior_clock.is_some_and(|at| at > page.checkpoint.observed_unix_ms) {
            return Err(error("Invalid retained wake checkpoint clock"));
        }
        if clock.is_some() && prior_clock.is_some_and(|at| at > observed_unix_ms) {
            return Err(StoreError::WakeReconcileChanged);
        }
        let envelopes = page
            .observations
            .into_iter()
            .map(|o| {
                let candidate = envelope(subscription, o).map_err(error)?;
                if let Some(old) = retained
                    .envelopes
                    .iter()
                    .find(|e| e.delivery_id == candidate.delivery_id)
                {
                    let mut a = old.observation.clone();
                    a.observed_unix_ms = candidate.observation.observed_unix_ms;
                    a.checked_prefix = candidate.observation.checked_prefix.clone();
                    if a != candidate.observation {
                        return Err(error("Retained wake semantics changed"));
                    }
                    Ok(old.clone())
                } else {
                    Ok(candidate)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let checkpoint = serde_json::to_string(&page.checkpoint)?;
        observe(ReconcilePoint::BeforeCommit)?;
        store.commit_wake_batch_with(
            cas,
            subscription_id,
            retained.checkpoint.as_deref(),
            &checkpoint,
            page.through.sequence,
            &page.source,
            &envelopes,
            &recorder_unchanged,
        )?;
        Ok(page.caught_up)
    })();
    match result {
        Err(StoreError::WakeReconcileChanged) => {
            // The raw logical-change signal can originate during restore, inner/outer
            // observation, recorder revalidation or page CAS. None bypasses this decision.
            store.validate_wake_reconcile_with(cas, subscription, || {
                if let Some(clock) = clock {
                    validate_current_clock(store, cas, subscription, clock())?;
                }
                match recorder_unchanged() {
                    Ok(()) | Err(StoreError::WakeReconcileChanged) => Ok(()),
                    Err(error) => Err(error),
                }
            })?;
            Err(StoreError::WakeReconcileDeferred)
        }
        other => other,
    }
}

// Called under validate_wake_reconcile_with's writer transaction, after canonical source and
// retained-checkpoint validation. A valid writer never masks a concurrent impossible timestamp.
fn validate_current_clock(
    store: &EventStore,
    cas: &Cas,
    subscription: &WakeSubscriptionV1,
    observed_unix_ms: u64,
) -> Result<(), StoreError> {
    if observed_unix_ms == 0
        || observed_unix_ms > af_contracts::json::SAFE_INTEGER_MAX as u64
        || observed_unix_ms >= subscription.expires_unix_ms
    {
        return Err(error("Invalid current wake observation clock"));
    }
    let retained = store.inspect_wake(&subscription.subscription_id)?;
    if let Some(text) = retained.checkpoint {
        let checkpoint: WakeCheckpointV1 = serde_json::from_str(&text)?;
        if checkpoint.observed_unix_ms > observed_unix_ms {
            return Err(error("Invalid current wake checkpoint clock"));
        }
    }
    let current = store
        .task_projection(cas, &subscription.task.task_id)?
        .ok_or_else(|| error("Wake Task disappeared"))?;
    let mut from = 0;
    while from < current.next_sequence {
        let events = store.replay_window(
            &subscription.task.task_run_id,
            from,
            (current.next_sequence - from).min(4096),
        )?;
        if events.is_empty() {
            return Err(error("Current wake history gap"));
        }
        let mut bytes = 0usize;
        for event in &events {
            bytes += serde_json::to_vec(event)?.len();
            if bytes > 16 * 1024 * 1024 {
                return Err(error("Current wake history page exceeds byte limit"));
            }
            if policy_time(event)? > observed_unix_ms {
                return Err(error("Wake observation precedes current checked history"));
            }
        }
        from += events.len() as u64;
    }
    Ok(())
}
