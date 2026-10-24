//! Additive native usage evidence; it records facts but never grants execution authority.
use super::*;
use review_core::task::usage::{TASK_USAGE_OBSERVATION_V1, TaskUsageObservationV1};

pub fn capture_task_usage_observation(
    cas: &Cas,
    producer: review_core::Producer,
    context_id: &str,
    observation: &TaskUsageObservationV1,
) -> Result<String, StoreError> {
    observation.validate().map_err(conflict)?;
    if !matches!(producer, review_core::Producer::Attempt { .. }) {
        return Err(conflict(
            "Task usage observation requires its Attempt producer",
        ));
    }
    cas.put_artifact(
        TASK_USAGE_OBSERVATION_V1,
        producer,
        vec![context_id.into()],
        None,
        serde_json::to_value(observation)?,
    )
    .map(|(id, _)| id)
    .map_err(|error| StoreError::Artifact(error.to_string()))
}

/// What a record with a native usage observation charges, by what it settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ObservedCharge {
    /// A `UsageObserved` record: the usage seen so far, never below the counters it reports.
    Observation,
    /// A settlement the Attempt's own answer or failure ended: incomplete reported counters
    /// keep the reservation floor.
    Settlement { unknown_usage: bool },
    /// An abandoned Attempt's settlement: charged what was observed, never its reservation
    /// (ADR-0203).
    Abandoned,
}

pub(super) fn validate(
    cas: &Cas,
    task_id: &str,
    attempt_id: &str,
    attempt: &RecordedAttempt,
    charged_tokens: u128,
    raw_artifact_ids: &[String],
    charge: ObservedCharge,
) -> Result<(), StoreError> {
    let mut seen = false;
    for id in raw_artifact_ids {
        let Some(envelope) = cas
            .get_optional_artifact(id)
            .map_err(|error| StoreError::Artifact(error.to_string()))?
        else {
            continue;
        };
        if envelope.artifact_type != TASK_USAGE_OBSERVATION_V1 {
            continue;
        }
        if seen {
            return Err(conflict("Duplicate Task usage observations in one record"));
        }
        seen = true;
        let observation: TaskUsageObservationV1 = serde_json::from_value(envelope.payload)?;
        observation.validate().map_err(conflict)?;
        if envelope.producer
            != (review_core::Producer::Attempt {
                run_id: task_run_id(task_id)?,
                node_id: attempt.reservation.node.clone(),
                attempt_id: attempt_id.into(),
            })
            || envelope.input_artifacts != attempt.context_id.iter().cloned().collect::<Vec<_>>()
            || envelope.subject_snapshot_id.is_some()
        {
            return Err(conflict(
                "Task usage observation differs from its admitted Attempt/context",
            ));
        }
        // An incomplete observation that reported no counter at all is no usage report: such
        // an Attempt settles as unknown usage at zero (ADR-0143). A settlement of one that
        // reported any counter keeps the reservation floor, unless the Attempt was abandoned:
        // that is charged what was observed (ADR-0203). An observation is no settlement.
        let floor = match charge {
            ObservedCharge::Settlement { unknown_usage } => {
                !(unknown_usage && observation.reported_usage.is_none())
            }
            ObservedCharge::Observation | ObservedCharge::Abandoned => false,
        };
        if charged_tokens
            < observation
                .reported_usage
                .as_ref()
                .map_or(0, |u| u.chargeable_tokens.get())
            || (!observation.charge_complete
                && floor
                && charged_tokens < u128::from(attempt.reservation.tokens))
        {
            return Err(conflict(
                "Task charge is below its incomplete native observation floor",
            ));
        }
    }
    Ok(())
}
