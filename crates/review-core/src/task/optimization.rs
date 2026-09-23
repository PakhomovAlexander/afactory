//! Typed, source-addressed inputs and deterministic outputs for project economics.
//!
//! These contracts deliberately keep provider-native counters, AF chargeable accounting,
//! elapsed spans and optional estimates separate. A missing measurement is represented as
//! unknown; it is never normalized to zero.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::usage::{DecimalU64, DecimalU128, TaskTokenUsageV3};
use super::{is_name, require};
use crate::is_digest;

pub const OPTIMIZATION_HISTORY_V1: &str = "af/OptimizationHistory@1";
pub const OPTIMIZATION_ECONOMICS_V1: &str = "af/OptimizationEconomics@1";
pub const OPTIMIZATION_REPORT_V1: &str = "af/OptimizationReport@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementStatusV1 {
    Exact,
    Estimated,
    LowerBound,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCompletenessV1 {
    Complete,
    Partial,
    Unavailable,
}

/// An immutable prefix/range receipt. Adapters join only receipts with the same exact source
/// identity; a prose path or session title is not correlation authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationSourceReceiptV1 {
    pub receipt_id: String,
    pub adapter: String,
    pub adapter_version: String,
    pub project_id: String,
    pub source_id: String,
    pub execution_id: String,
    pub byte_start: DecimalU64,
    pub byte_end: DecimalU64,
    pub prefix_digest: String,
    pub cutoff_unix_ms: DecimalU64,
    pub redaction_version: String,
    pub completeness: SourceCompletenessV1,
}

impl OptimizationSourceReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.receipt_id)
                && is_digest(&self.project_id)
                && is_digest(&self.prefix_digest)
                && is_name(&self.adapter)
                && is_name(&self.adapter_version)
                && is_name(&self.redaction_version)
                && !self.source_id.is_empty()
                && self.source_id.len() <= 512
                && !self.source_id.chars().any(char::is_control)
                && !self.execution_id.is_empty()
                && self.execution_id.len() <= 256
                && self.byte_start <= self.byte_end,
            "Optimization source receipt requires exact identity, a bounded range and cutoff",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationAttributionV1 {
    pub project_id: String,
    pub case_family: String,
    pub execution_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub outer_execution_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub task_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub attempt_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub pipeline: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub node: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub worker: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub effort: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub configuration_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub environment_id: Option<String>,
}

impl OptimizationAttributionV1 {
    pub fn validate(&self) -> Result<(), String> {
        let bounded = |value: &str| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        };
        require(
            is_digest(&self.project_id)
                && bounded(&self.case_family)
                && bounded(&self.execution_id)
                && [
                    self.outer_execution_id.as_deref(),
                    self.task_id.as_deref(),
                    self.attempt_id.as_deref(),
                    self.pipeline.as_deref(),
                    self.node.as_deref(),
                    self.worker.as_deref(),
                    self.model.as_deref(),
                    self.effort.as_deref(),
                    self.configuration_id.as_deref(),
                    self.environment_id.as_deref(),
                ]
                .into_iter()
                .flatten()
                .all(bounded),
            "Optimization attribution requires bounded exact project/execution labels",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationTokenObservationV1 {
    /// Cumulative snapshots sharing this key are joined by component-wise maximum, never sum.
    pub cumulative_key: String,
    pub usage: TaskTokenUsageV3,
    pub status: MeasurementStatusV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub context_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub retrieval_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub repeated_context_tokens: Option<DecimalU128>,
    pub outer_session: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationSpanKindV1 {
    EndToEnd,
    Active,
    Check,
    DependencyPreparation,
    Queue,
    ApprovalWaiting,
    UserWaiting,
    Retrieval,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationSpanV1 {
    pub span_id: String,
    pub kind: OptimizationSpanKindV1,
    pub start_unix_ms: DecimalU64,
    pub end_unix_ms: DecimalU64,
    pub clock: String,
    pub status: MeasurementStatusV1,
}

impl OptimizationSpanV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.span_id)
                && self.start_unix_ms <= self.end_unix_ms
                && is_name(&self.clock),
            "Optimization span requires exact identity and a nonnegative observed interval",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheResultV1 {
    Hit,
    Miss,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTemperatureV1 {
    Cold,
    Warm,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationCacheObservationV1 {
    pub kind: String,
    pub eligible: bool,
    pub result: CacheResultV1,
    pub temperature: CacheTemperatureV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub invalidation_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub bytes_reused: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub tokens_reused: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub lookup_ms: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub warmup_ms: Option<DecimalU64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationOutcomeV1 {
    Verified,
    Failed,
    Incomplete,
    Cancelled,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationOutcomeObservationV1 {
    pub outcome: OptimizationOutcomeV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub requirements_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub verifier_id: Option<String>,
    pub retries: u32,
    pub repairs: u32,
    pub later_defects: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationObservationV1 {
    pub observation_id: String,
    pub source_receipt_id: String,
    pub attribution: OptimizationAttributionV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub tokens: Option<OptimizationTokenObservationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<OptimizationSpanV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<OptimizationCacheObservationV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub outcome: Option<OptimizationOutcomeObservationV1>,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub missing_fields: BTreeSet<String>,
}

impl OptimizationObservationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.observation_id) && is_digest(&self.source_receipt_id),
            "Optimization observation requires exact source identity",
        )?;
        self.attribution.validate()?;
        for span in &self.spans {
            span.validate()?;
        }
        if let Some(tokens) = &self.tokens {
            require(
                !tokens.cumulative_key.is_empty() && tokens.cumulative_key.len() <= 256,
                "Cumulative token key is missing or too large",
            )?;
        }
        if let Some(outcome) = &self.outcome {
            require(
                outcome.requirements_id.as_deref().is_none_or(is_digest)
                    && outcome.verifier_id.as_deref().is_none_or(is_digest),
                "Outcome authority ids must be digests",
            )?;
        }
        for cache in &self.caches {
            require(
                !cache.kind.is_empty()
                    && cache.kind.len() <= 256
                    && cache
                        .invalidation_id
                        .as_deref()
                        .is_none_or(|id| !id.is_empty() && id.len() <= 512),
                "Cache kind or invalidation identity is invalid",
            )?;
        }
        require(
            self.missing_fields.iter().all(|field| is_name(field)),
            "Missing-field names must be bounded identifiers",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationHistoryV1 {
    pub schema: String,
    pub project_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub previous_capture_id: Option<String>,
    pub cutoff_unix_ms: DecimalU64,
    pub receipts: Vec<OptimizationSourceReceiptV1>,
    pub observations: Vec<OptimizationObservationV1>,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub gaps: BTreeSet<String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub exposed_case_families: BTreeSet<String>,
}

impl OptimizationHistoryV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-history/1"
                && is_digest(&self.project_id)
                && self.previous_capture_id.as_deref().is_none_or(is_digest)
                && self.receipts.len() <= 200
                && self.observations.len() <= 100_000,
            "Optimization history requires bounded, exact project capture identity",
        )?;
        let mut receipts = BTreeSet::new();
        for receipt in &self.receipts {
            receipt.validate()?;
            require(
                receipt.project_id == self.project_id && receipts.insert(&receipt.receipt_id),
                "Optimization history has a foreign or duplicate source receipt",
            )?;
        }
        let mut observations = BTreeSet::new();
        for observation in &self.observations {
            observation.validate()?;
            require(
                receipts.contains(&observation.source_receipt_id)
                    && observation.attribution.project_id == self.project_id
                    && observations.insert(&observation.observation_id),
                "Optimization history has a duplicate or unreceipted observation",
            )?;
        }
        require(
            self.gaps.len() <= 1024
                && self.gaps.iter().all(|gap| is_name(gap))
                && self.exposed_case_families.len() <= 100_000
                && self
                    .exposed_case_families
                    .iter()
                    .all(|family| !family.is_empty() && family.len() <= 256),
            "Optimization capture coverage metadata exceeds bounds",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationEconomicsRowV1 {
    pub execution_id: String,
    pub case_family: String,
    pub af_usage: TaskTokenUsageV3,
    pub outer_session_usage: TaskTokenUsageV3,
    pub active_ms: DecimalU128,
    pub elapsed_ms: DecimalU128,
    pub summed_work_ms: DecimalU128,
    /// Union duration per observed span kind. These values are not additive across kinds.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub span_ms: BTreeMap<OptimizationSpanKindV1, DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub context_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub retrieval_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub repeated_context_tokens: Option<DecimalU128>,
    pub outcome: OptimizationOutcomeV1,
    pub occurrences: u32,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub missing_fields: BTreeSet<String>,
}

/// Deterministic cache evidence. Optional totals remain unknown when any contributing
/// observation omitted that measurement; counts never upgrade missing timing or reuse data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationCacheEconomicsV1 {
    pub eligible: u64,
    pub ineligible: u64,
    pub hits: u64,
    pub misses: u64,
    pub unknown_results: u64,
    pub cold: u64,
    pub warm: u64,
    pub unknown_temperature: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub bytes_reused: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub tokens_reused: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub lookup_ms: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub warmup_ms: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub invalidation_ids: BTreeSet<String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub missing_fields: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationEconomicsV1 {
    pub schema: String,
    pub project_id: String,
    pub capture_ids: Vec<String>,
    pub cutoff_unix_ms: DecimalU64,
    pub rows: Vec<OptimizationEconomicsRowV1>,
    pub af_usage: TaskTokenUsageV3,
    pub outer_session_usage: TaskTokenUsageV3,
    pub active_ms: DecimalU128,
    pub elapsed_ms: DecimalU128,
    pub summed_work_ms: DecimalU128,
    /// Project-wide interval union per kind on the recorded clock. `summed_work_ms` remains a
    /// distinct diagnostic and callers must not add these possibly overlapping values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub span_ms: BTreeMap<OptimizationSpanKindV1, DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub context_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub retrieval_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub repeated_context_tokens: Option<DecimalU128>,
    pub verified: u32,
    pub failed_or_incomplete: u32,
    pub repeated_failures: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cache_economics: BTreeMap<String, OptimizationCacheEconomicsV1>,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub missing_fields: BTreeSet<String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "super::unique_set"
    )]
    pub exposed_case_families: BTreeSet<String>,
}

impl OptimizationEconomicsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-economics/1"
                && is_digest(&self.project_id)
                && !self.capture_ids.is_empty()
                && self.capture_ids.iter().all(|id| is_digest(id))
                && self.capture_ids.iter().collect::<BTreeSet<_>>().len() == self.capture_ids.len(),
            "Optimization economics requires a unique exact capture chain",
        )?;
        require(
            self.missing_fields.iter().all(|field| is_name(field))
                && self
                    .exposed_case_families
                    .iter()
                    .all(|family| !family.is_empty() && family.len() <= 256)
                && self.rows.iter().all(|row| {
                    !row.execution_id.is_empty()
                        && row.execution_id.len() <= 256
                        && !row.case_family.is_empty()
                        && row.case_family.len() <= 256
                        && row.missing_fields.iter().all(|field| is_name(field))
                })
                && self.cache_economics.iter().all(|(kind, cache)| {
                    let observations = cache.eligible.checked_add(cache.ineligible);
                    let results = cache
                        .hits
                        .checked_add(cache.misses)
                        .and_then(|value| value.checked_add(cache.unknown_results));
                    let temperatures = cache
                        .cold
                        .checked_add(cache.warm)
                        .and_then(|value| value.checked_add(cache.unknown_temperature));
                    !kind.is_empty()
                        && kind.len() <= 256
                        && observations.is_some_and(|count| {
                            count > 0 && results == Some(count) && temperatures == Some(count)
                        })
                        && cache.invalidation_ids.iter().all(|id| {
                            !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
                        })
                        && cache.missing_fields.iter().all(|field| is_name(field))
                        && [
                            ("bytes_reused", cache.bytes_reused.is_some()),
                            ("tokens_reused", cache.tokens_reused.is_some()),
                            ("lookup_time", cache.lookup_ms.is_some()),
                            ("warmup_time", cache.warmup_ms.is_some()),
                        ]
                        .into_iter()
                        .all(|(field, present)| present != cache.missing_fields.contains(field))
                }),
            "Optimization economics row or coverage metadata is invalid",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationReportStatusV1 {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationReportV1 {
    pub schema: String,
    pub economics_id: String,
    pub project_id: String,
    pub status: OptimizationReportStatusV1,
    pub summary: String,
    pub highlights: Vec<String>,
    pub missing_measurements: Vec<String>,
}

impl OptimizationReportV1 {
    pub fn validate(&self) -> Result<(), String> {
        let bounded = |value: &str| {
            !value.trim().is_empty() && value.len() <= 16_384 && !value.contains('\0')
        };
        require(
            self.schema == "af.optimization-report/1"
                && is_digest(&self.economics_id)
                && is_digest(&self.project_id)
                && bounded(&self.summary)
                && self.highlights.len() <= 128
                && self.highlights.iter().all(|value| bounded(value))
                && self.missing_measurements.len() <= 1024
                && self.missing_measurements.iter().all(|value| bounded(value)),
            "Optimization report is malformed",
        )
    }

    pub fn render_markdown(&self) -> Result<String, String> {
        self.validate()?;
        let escape = |value: &str| value.replace('\r', " ").replace('#', "\\#");
        let mut output = format!(
            "# Project economics\n\n{}\n\nStatus: `{:?}`\n\n\
             Live paid demonstrations and adoption observations are still pending.\n",
            escape(&self.summary),
            self.status
        );
        if !self.highlights.is_empty() {
            output.push_str("\n## Observations\n");
            for value in &self.highlights {
                output.push_str(&format!("\n- {}\n", escape(value)));
            }
        }
        if !self.missing_measurements.is_empty() {
            output.push_str("\n## Unknown measurements\n");
            for value in &self.missing_measurements {
                output.push_str(&format!("\n- {}\n", escape(value)));
            }
        }
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationStrategyV1 {
    Light,
    Heavy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationPolicyV1 {
    pub schema: String,
    pub project_id: String,
    pub strategy: OptimizationStrategyV1,
    pub max_sessions: u32,
    pub max_raw_bytes: DecimalU64,
    pub max_record_bytes: DecimalU64,
    pub max_normalized_bytes: DecimalU64,
}

impl OptimizationPolicyV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-policy/1"
                && is_digest(&self.project_id)
                && (1..=200).contains(&self.max_sessions)
                && (1..=268_435_456).contains(&self.max_raw_bytes.get())
                && (1..=1_048_576).contains(&self.max_record_bytes.get())
                && (1..=16_777_216).contains(&self.max_normalized_bytes.get()),
            "Optimization policy exceeds M1 capture bounds",
        )
    }
}
