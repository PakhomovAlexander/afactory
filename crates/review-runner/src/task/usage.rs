//! One exact usage representation for Worker and Provider admission results.

use crate::TokenUsage;
use review_core::{
    ArtifactEnvelope, Producer,
    task::usage::{
        DecimalU64, TASK_TOKEN_USAGE_V1, TASK_TOKEN_USAGE_V2, TaskTokenUsageV1, TaskTokenUsageV2,
    },
};
use review_store::{Cas, validate_envelope};

impl From<&TokenUsage> for TaskTokenUsageV1 {
    fn from(value: &TokenUsage) -> Self {
        Self {
            input_tokens: value.input_tokens.map(Into::into),
            output_tokens: value.output_tokens.map(Into::into),
            cache_read_tokens: value.cache_read_tokens.map(Into::into),
            cache_write_tokens: value.cache_write_tokens.map(Into::into),
            reasoning_tokens: value.reasoning_tokens.map(Into::into),
            chargeable_tokens: value.chargeable_tokens.into(),
        }
    }
}

impl From<TaskTokenUsageV1> for TokenUsage {
    fn from(value: TaskTokenUsageV1) -> Self {
        Self {
            input_tokens: value.input_tokens.map(DecimalU64::get),
            output_tokens: value.output_tokens.map(DecimalU64::get),
            cache_read_tokens: value.cache_read_tokens.map(DecimalU64::get),
            cache_write_tokens: value.cache_write_tokens.map(DecimalU64::get),
            reasoning_tokens: value.reasoning_tokens.map(DecimalU64::get),
            chargeable_tokens: value.chargeable_tokens.get(),
        }
    }
}

pub fn persist_task_usage(
    cas: &Cas,
    producer: Producer,
    context_id: &str,
    usage: &TokenUsage,
) -> Result<String, String> {
    cas.put_artifact(
        TASK_TOKEN_USAGE_V1,
        producer,
        vec![context_id.into()],
        None,
        serde_json::to_value(TaskTokenUsageV1::from(usage)).map_err(|error| error.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|error| error.to_string())
}

pub fn read_task_usage(cas: &Cas, id: &str) -> Result<TokenUsage, String> {
    let value = cas.get_json(id).map_err(|error| error.to_string())?;
    if value.get("type").is_some() {
        let envelope: ArtifactEnvelope =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        validate_envelope(&envelope)?;
        if envelope.artifact_id != id || envelope.artifact_type != TASK_TOKEN_USAGE_V1 {
            return Err("Expected an exact TaskTokenUsage@1 artifact".into());
        }
        serde_json::from_value::<TaskTokenUsageV1>(envelope.payload)
            .map(Into::into)
            .map_err(|error| error.to_string())
    } else {
        // Released Task usage blobs used this closed numeric structure without an envelope.
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
}

impl From<&TokenUsage> for TaskTokenUsageV2 {
    fn from(value: &TokenUsage) -> Self {
        TaskTokenUsageV1::from(value).into()
    }
}

pub fn persist_task_usage_exact(
    cas: &Cas,
    producer: Producer,
    context_id: &str,
    usage: &TaskTokenUsageV2,
) -> Result<String, String> {
    cas.put_artifact(
        TASK_TOKEN_USAGE_V2,
        producer,
        vec![context_id.into()],
        None,
        serde_json::to_value(usage).map_err(|error| error.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|error| error.to_string())
}

pub fn read_task_usage_exact(cas: &Cas, id: &str) -> Result<TaskTokenUsageV2, String> {
    let value = cas.get_json(id).map_err(|error| error.to_string())?;
    if value.get("type").is_some() {
        let envelope: ArtifactEnvelope =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        validate_envelope(&envelope)?;
        if envelope.artifact_id != id {
            return Err("Task usage artifact identity differs from its reference".into());
        }
        match envelope.artifact_type.as_str() {
            TASK_TOKEN_USAGE_V1 => serde_json::from_value::<TaskTokenUsageV1>(envelope.payload)
                .map(Into::into)
                .map_err(|error| error.to_string()),
            TASK_TOKEN_USAGE_V2 => {
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())
            }
            _ => Err("Unsupported Task usage artifact version".into()),
        }
    } else {
        let usage: TokenUsage = serde_json::from_value(value).map_err(|error| error.to_string())?;
        Ok(TaskTokenUsageV2::from(&usage))
    }
}
