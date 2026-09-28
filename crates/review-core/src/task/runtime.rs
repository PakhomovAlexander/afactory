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
/// never a tool or provider cache result. A Warm Check Cache observation that was not eligible
/// names its reason after the kind, as in `cargo_target:busy` (ADR-0123).
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
    /// Bytes a warm layer removed once this check ended above its byte bound (ADR-0123): the
    /// same observation records availability before the check and eviction after it, so one
    /// declared kind yields exactly one observation per check. Absent when nothing was evicted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub evicted_bytes: Option<u64>,
    /// Why the eviction happened: `bound_exceeded` or `suspect`. Present exactly when
    /// `evicted_bytes` is.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub evicted_reason: Option<String>,
    /// Which byte bound of the Warm Check Cache acted on this directory (ADR-0127, amending
    /// ADR-0123): `max_bytes` evicted it before or after a check whose result stands, and
    /// `hard_max_bytes` ended the running check, which failed. Absent when no bound acted, and
    /// never beside a `suspect` eviction.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub bound: Option<TaskCacheBoundV1>,
}

/// The two byte bounds of a Warm Check Cache toolchain key (ADR-0127).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCacheBoundV1 {
    /// The eviction bound: a key above it is removed before the next check, never mid-check.
    MaxBytes,
    /// The only bound that ends a running check, with `warm_cache_bound_exceeded`.
    HardMaxBytes,
}

impl TaskCacheBoundV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxBytes => "max_bytes",
            Self::HardMaxBytes => "hard_max_bytes",
        }
    }
}

impl TaskCacheObservationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.observation_id)
                && is_cache_kind(&self.kind)
                && is_digest(&self.source_digest)
                && self.toolchain_id.as_deref().is_none_or(is_digest)
                && self.bytes_available <= crate::json::SAFE_INTEGER_MAX as u64
                && self.lookup_ms <= crate::json::SAFE_INTEGER_MAX as u64
                && self.materialization_ms <= crate::json::SAFE_INTEGER_MAX as u64
                && self
                    .evicted_bytes
                    .is_none_or(|bytes| bytes <= crate::json::SAFE_INTEGER_MAX as u64)
                && self.evicted_bytes.is_some() == self.evicted_reason.is_some()
                && self
                    .evicted_reason
                    .as_deref()
                    .is_none_or(|reason| matches!(reason, "bound_exceeded" | "suspect"))
                && (self.bound.is_none()
                    || self.evicted_reason.as_deref() == Some("bound_exceeded")
                    || self.kind.ends_with(":bound_exceeded")),
            "Task cache evidence requires bounded identity and measurements",
        )
    }
}

/// A cache kind, optionally followed by one `:reason` naming why it was not eligible.
pub fn is_cache_kind(value: &str) -> bool {
    match value.split_once(':') {
        Some((kind, reason)) => is_name(kind) && is_name(reason),
        None => is_name(value),
    }
}

/// How a warm check ended, as its `CheckResult@1` status says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRuntimeCheckOutcomeV1 {
    Passed,
    Failed,
    NotRun,
}

/// Where a warm check's `RUSTUP_HOME` came from (ADR-0123): the kernel's own `RUSTUP_HOME`, the
/// kernel `HOME`'s `.rustup`, or why it was left unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRuntimeRustupHomeV1 {
    KernelEnvironment,
    KernelHome,
    /// The kernel had neither `RUSTUP_HOME` nor an absolute `HOME`.
    UnsetNoHome,
    /// The kernel `HOME` has no `.rustup` directory.
    UnsetNotInstalled,
}

/// The check one warm evidence group belongs to (ADR-0123). It names the check and its outcome
/// whether or not the check started, so a check skipped before its command ran keeps its name
/// and its cache observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRuntimeCheckV1 {
    pub name: String,
    pub outcome: TaskRuntimeCheckOutcomeV1,
    pub rustup_home: TaskRuntimeRustupHomeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRuntimeEvidenceV1 {
    pub task_id: String,
    pub attempt_id: String,
    pub node: String,
    pub context_id: String,
    /// Present only on a warm check's group; absent, the record is byte-identical to one written
    /// before warm checks existed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub check: Option<TaskRuntimeCheckV1>,
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
        if let Some(check) = &self.check {
            // A named group is one check's: its only span is that check's, and a check that
            // never started still states what every declared kind was.
            require(
                is_name(&check.name)
                    && !self.caches.is_empty()
                    && self.spans.len() <= 1
                    && self.spans.iter().all(|span| {
                        span.kind == TaskRuntimeSpanKindV1::Check && span.label == check.name
                    }),
                "Task runtime check evidence names one check and its cache observations",
            )?;
        }
        require(
            !self.spans.is_empty() || !self.caches.is_empty(),
            "Task runtime evidence cannot be empty",
        )
    }
}
