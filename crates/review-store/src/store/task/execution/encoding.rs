//! One closed Task execution record encoding; there is no permissive fallback.

use super::super::conflict;
use crate::{Cas, StoreError};
use review_core::{ArtifactEnvelope, task::execution::*};

pub struct DecodedTaskExecutionRecord {
    pub envelope: ArtifactEnvelope,
    pub record: TaskExecutionRecordV1,
}

pub fn read_execution_record(
    cas: &Cas,
    id: &str,
) -> Result<DecodedTaskExecutionRecord, StoreError> {
    let envelope = cas
        .get_artifact(id)
        .map_err(|error| StoreError::Artifact(error.to_string()))?;
    if envelope.artifact_type != TASK_EXECUTION_RECORD_V5 {
        return Err(conflict("Expected a typed Task execution record"));
    }
    let record: TaskExecutionRecordV1 = serde_json::from_value(envelope.payload.clone())?;
    record.validate().map_err(conflict)?;
    Ok(DecodedTaskExecutionRecord { envelope, record })
}

pub(super) fn encode_record(
    record: &TaskExecutionRecordV1,
) -> Result<(&'static str, serde_json::Value), StoreError> {
    record.validate().map_err(conflict)?;
    Ok((TASK_EXECUTION_RECORD_V5, serde_json::to_value(record)?))
}
