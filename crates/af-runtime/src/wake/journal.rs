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
    let retained = store.inspect_wake(subscription_id)?;
    if !retained.source_available {
        return Err(error(
            "Wake source was collected; retained envelopes are historical only",
        ));
    }
    let subscription = &retained.registration.subscription;
    let cursor = if let Some(text) = &retained.checkpoint {
        let prior: WakeCheckpointV1 = serde_json::from_str(text)?;
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
    let page = project_with_run_facts(
        store,
        cas,
        subscription,
        cursor.as_ref(),
        observed_unix_ms,
        facts,
    )?;
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
    store.commit_wake_batch_with(
        cas,
        subscription_id,
        retained.checkpoint.as_deref(),
        &checkpoint,
        page.through.sequence,
        &page.source,
        &envelopes,
        recorder_unchanged,
    )?;
    Ok(page.caught_up)
}
