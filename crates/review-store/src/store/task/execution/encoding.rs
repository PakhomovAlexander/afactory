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
    let envelope = cas
        .get_artifact(id)
        .map_err(|error| StoreError::Artifact(error.to_string()))?;
    let kind = envelope.artifact_type.as_str();
    if !matches!(
        kind,
        TASK_EXECUTION_RECORD_V1
            | TASK_EXECUTION_RECORD_V3
            | TASK_EXECUTION_RECORD_V4
            | TASK_EXECUTION_RECORD_V5
    ) {
        return Err(conflict("Unsupported Task execution record version"));
    }
    let record = match kind {
        TASK_EXECUTION_RECORD_V1 => {
            let record: TaskExecutionRecordV1 = serde_json::from_value(envelope.payload.clone())?;
            record.validate().map_err(conflict)?;
            record
        }
        TASK_EXECUTION_RECORD_V3 => {
            let record: TaskExecutionRecordV3 = serde_json::from_value(envelope.payload.clone())?;
            record.validate().map_err(conflict)?;
            record.into_record()
        }
        TASK_EXECUTION_RECORD_V4 => {
            let record: TaskExecutionRecordV4 = serde_json::from_value(envelope.payload.clone())?;
            record.validate().map_err(conflict)?;
            record.into_record()
        }
        TASK_EXECUTION_RECORD_V5 => {
            let record: TaskExecutionRecordV5 = serde_json::from_value(envelope.payload.clone())?;
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
    if let Some(experiment) = TaskExecutionRecordV5::from_experiment(record) {
        experiment.validate().map_err(conflict)?;
        Ok((TASK_EXECUTION_RECORD_V5, serde_json::to_value(experiment)?))
    } else if let Some(owned) = TaskExecutionRecordV4::from_owned(record) {
        owned.validate().map_err(conflict)?;
        Ok((TASK_EXECUTION_RECORD_V4, serde_json::to_value(owned)?))
    } else if let Some(accounting) = TaskExecutionRecordV3::from_accounting(record) {
        accounting.validate().map_err(conflict)?;
        Ok((TASK_EXECUTION_RECORD_V3, serde_json::to_value(accounting)?))
    } else {
        record.validate().map_err(conflict)?;
        Ok((TASK_EXECUTION_RECORD_V1, serde_json::to_value(record)?))
    }
}
