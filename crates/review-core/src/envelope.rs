//! The artifact envelope every kernel record carries.

use serde::{Deserialize, Serialize};

/// An immutable artifact record over a content-addressed payload.
///
/// `content_id` hashes the canonical payload bytes; `artifact_id` hashes the envelope under a
/// different domain separator. That split is what lets identical content be stored once while
/// two provenance records stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEnvelope {
    /// Contract type URI, e.g. `review.kernel/FindingReport@1`.
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub artifact_id: String,
    pub content_id: String,
    pub producer: Producer,
    /// Exact artifact IDs consumed to produce this record. No node consumes ambient inputs.
    pub input_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_snapshot_id: Option<String>,
    pub payload: serde_json::Value,
}

/// Who produced an artifact.
///
/// Executed output is an [`Producer::Attempt`] and carries its Attempt ID. Deterministic
/// kernel-derived output is a [`Producer::KernelOperation`] with a stable `operation_id`, so
/// re-executing the same canonical operation reproduces the same `artifact_id` — retry attempt
/// IDs stay in events and receipts, deliberately outside this envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Producer {
    Attempt {
        run_id: String,
        node_id: String,
        attempt_id: String,
    },
    KernelOperation {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        operation_id: String,
    },
}

impl Producer {
    pub fn run_id(&self) -> &str {
        match self {
            Producer::Attempt { run_id, .. } | Producer::KernelOperation { run_id, .. } => run_id,
        }
    }

    /// Whether this producer is replayable to the same identity by re-execution.
    pub fn is_deterministic(&self) -> bool {
        matches!(self, Producer::KernelOperation { .. })
    }
}
