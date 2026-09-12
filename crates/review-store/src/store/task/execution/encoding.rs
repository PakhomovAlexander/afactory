//! Select the declared wire version before decoding; there is no permissive fallback.

use super::super::conflict;
use crate::{Cas, StoreError};
use review_core::{ArtifactEnvelope, task::execution::*};

pub struct DecodedTaskExecutionRecord {
    pub envelope: ArtifactEnvelope,
    /// The in-memory lifecycle shape. Only the envelope declares its persisted encoding.
    pub record: TaskExecutionRecordV1,
}

pub fn read_execution_record(
    cas: &Cas,
    id: &str,
) -> Result<DecodedTaskExecutionRecord, StoreError> {
    let raw = cas
        .get_json(id)
        .map_err(|error| StoreError::Artifact(error.to_string()))?;
    let envelope: ArtifactEnvelope = serde_json::from_value(raw)?;
    crate::validate_envelope(&envelope).map_err(conflict)?;
    if envelope.artifact_id != id {
        return Err(conflict(
            "Task execution envelope differs from its artifact identity",
        ));
    }
    let kind = envelope.artifact_type.as_str();
    if !matches!(kind, TASK_EXECUTION_RECORD_V1 | TASK_EXECUTION_RECORD_V2) {
        return Err(conflict("Unsupported Task execution record version"));
    }
    let record = match kind {
        TASK_EXECUTION_RECORD_V1 => {
            let record: TaskExecutionRecordV1 = serde_json::from_value(envelope.payload.clone())?;
            record.validate().map_err(conflict)?;
            record
        }
        TASK_EXECUTION_RECORD_V2 => {
            let record: TaskExecutionRecordV2 = serde_json::from_value(envelope.payload.clone())?;
            record.validate().map_err(conflict)?;
            record.into_record()
        }
        _ => unreachable!(),
    };
    Ok(DecodedTaskExecutionRecord { envelope, record })
}

pub(super) fn encode_record(
    record: &TaskExecutionRecordV1,
) -> Result<(&'static str, serde_json::Value), StoreError> {
    if let Some(accounting) = TaskExecutionRecordV2::from_accounting(record) {
        accounting.validate().map_err(conflict)?;
        Ok((TASK_EXECUTION_RECORD_V2, serde_json::to_value(accounting)?))
    } else {
        record.validate().map_err(conflict)?;
        Ok((TASK_EXECUTION_RECORD_V1, serde_json::to_value(record)?))
    }
}
