//! One closed Task execution record encoding; there is no permissive fallback.

use super::super::{artifact_read_error, conflict};
use crate::{Cas, StoreError};
use af_contracts::{ArtifactEnvelope, task::execution::*};
use serde::Deserialize as _;

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
        .map_err(|error| artifact_read_error(id, error))?;
    if envelope.artifact_type != TASK_EXECUTION_RECORD_V6 {
        return Err(conflict("Expected a typed Task execution record"));
    }
    let record: TaskExecutionRecordV1 = TaskExecutionRecordV1::deserialize(&envelope.payload)?;
    record.validate().map_err(conflict)?;
    Ok(DecodedTaskExecutionRecord { envelope, record })
}

pub(super) fn encode_record(
    record: &TaskExecutionRecordV1,
) -> Result<(&'static str, serde_json::Value), StoreError> {
    record.validate().map_err(conflict)?;
    Ok((TASK_EXECUTION_RECORD_V6, serde_json::to_value(record)?))
}
