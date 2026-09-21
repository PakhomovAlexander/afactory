//! Measured host-runtime evidence retained beside common Task Attempts.
//!
//! These observations describe AF-owned boundaries only. In particular, making dependency
//! bytes available to a sandbox is preparation evidence, not a claim that a compiler or model
//! provider used its own cache.

use serde::{Deserialize, Serialize};

use super::{is_name, present_option, require};
use crate::is_digest;

pub const TASK_RUNTIME_EVIDENCE_V1: &str = "af/TaskRuntimeEvidence@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRuntimeSpanKindV1 {
    Check,
    DependencyPreparation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRuntimeSpanV1 {
    pub span_id: String,
    pub kind: TaskRuntimeSpanKindV1,
    pub label: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
}

impl TaskRuntimeSpanV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.span_id)
                && is_name(&self.label)
                && self.started_unix_ms > 0
                && self.started_unix_ms <= crate::json::SAFE_INTEGER_MAX as u64
                && self.elapsed_ms <= crate::json::SAFE_INTEGER_MAX as u64,
            "Task runtime span requires exact identity and bounded host timing",
        )
    }
}

/// Dependency bytes AF prepared for a sandbox. It records availability and host time only,
/// never a tool or provider cache result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCacheObservationV1 {
    pub observation_id: String,
    pub kind: String,
    pub eligible: bool,
    pub source_digest: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub toolchain_id: Option<String>,
    pub bytes_available: u64,
    pub lookup_ms: u64,
    pub materialization_ms: u64,
}

impl TaskCacheObservationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.observation_id)
                && is_name(&self.kind)
                && is_digest(&self.source_digest)
                && self.toolchain_id.as_deref().is_none_or(is_digest)
                && self.bytes_available <= crate::json::SAFE_INTEGER_MAX as u64
                && self.lookup_ms <= crate::json::SAFE_INTEGER_MAX as u64
                && self.materialization_ms <= crate::json::SAFE_INTEGER_MAX as u64,
            "Task cache evidence requires bounded identity and measurements",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRuntimeEvidenceV1 {
    pub task_id: String,
    pub attempt_id: String,
    pub node: String,
    pub context_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<TaskRuntimeSpanV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<TaskCacheObservationV1>,
}

impl TaskRuntimeEvidenceV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            !self.task_id.is_empty()
                && self.task_id.len() <= 256
                && !self.attempt_id.is_empty()
                && self.attempt_id.len() <= 256
                && !self.node.is_empty()
                && self.node.len() <= 256
                && !self.node.chars().any(char::is_control)
                && is_digest(&self.context_id)
                && self.spans.len() <= 10_000
                && self.caches.len() <= 1_000,
            "Task runtime evidence requires bounded Task, Attempt, node and context identity",
        )?;
        for span in &self.spans {
            span.validate()?;
        }
        for cache in &self.caches {
            cache.validate()?;
        }
        require(
            !self.spans.is_empty() || !self.caches.is_empty(),
            "Task runtime evidence cannot be empty",
        )
    }
}
