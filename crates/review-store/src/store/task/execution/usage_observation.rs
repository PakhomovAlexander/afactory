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

pub(super) fn validate(
    cas: &Cas,
    task_id: &str,
    attempt_id: &str,
    attempt: &RecordedAttempt,
    charged_tokens: u128,
    raw_artifact_ids: &[String],
) -> Result<(), StoreError> {
    let mut seen = false;
    for id in raw_artifact_ids {
        let bytes = cas
            .get(id)
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some(TASK_USAGE_OBSERVATION_V1)
        {
            continue;
        }
        if seen {
            return Err(conflict("Duplicate Task usage observations in one record"));
        }
        seen = true;
        let envelope = cas
            .get_artifact(id)
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
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
        if charged_tokens
            < observation
                .reported_usage
                .as_ref()
                .map_or(0, |u| u.chargeable_tokens.get())
            || (!observation.charge_complete
                && charged_tokens < u128::from(attempt.reservation.tokens))
        {
            return Err(conflict(
                "Task charge is below its incomplete native observation floor",
            ));
        }
    }
    Ok(())
}
