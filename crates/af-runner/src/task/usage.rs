//! One exact usage representation for Worker and Provider admission results.

use af_contracts::{
    ArtifactEnvelope, Producer,
    task::usage::{TASK_TOKEN_USAGE_V3, TaskTokenUsageV3},
};
use af_store::{Cas, validate_envelope};

/// Native protocol counters are per-event u64 numbers. Absence is distinct from a present
/// invalid value; only the provider's declared optional fields may interpret absence as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCounter {
    Absent,
    Value(u64),
    Invalid,
}
impl NativeCounter {
    pub fn read(value: &serde_json::Value, key: &str) -> Self {
        match value.get(key) {
            None => Self::Absent,
            Some(value) => value.as_u64().map_or(Self::Invalid, Self::Value),
        }
    }
    pub fn value(self) -> Option<u64> {
        match self {
            Self::Value(n) => Some(n),
            _ => None,
        }
    }
    pub fn optional_zero(self) -> Option<u64> {
        match self {
            Self::Absent => Some(0),
            Self::Value(n) => Some(n),
            Self::Invalid => None,
        }
    }
}

/// Every usage artifact is `af/TaskTokenUsage@3`, whatever the width of its counters.
pub fn persist_task_usage_exact(
    cas: &Cas,
    producer: Producer,
    context_id: &str,
    usage: &TaskTokenUsageV3,
) -> Result<String, String> {
    cas.put_artifact(
        TASK_TOKEN_USAGE_V3,
        producer,
        vec![context_id.into()],
        None,
        serde_json::to_value(usage).map_err(|error| error.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|error| error.to_string())
}

pub fn read_task_usage_exact(cas: &Cas, id: &str) -> Result<TaskTokenUsageV3, String> {
    let value = cas.get_json(id).map_err(|error| error.to_string())?;
    let envelope: ArtifactEnvelope =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    validate_envelope(&envelope)?;
    if envelope.artifact_id != id {
        return Err("Task usage artifact identity differs from its reference".into());
    }
    if envelope.artifact_type != TASK_TOKEN_USAGE_V3 {
        return Err("Unsupported Task usage artifact version".into());
    }
    serde_json::from_value(envelope.payload).map_err(|error| error.to_string())
}
