//! Additive Task contracts. These types do not change frozen review artifacts or events.
//!
//! Validation here establishes shape and local invariants. Compilation establishes graph
//! compatibility; the Store establishes identity, authority and legal transitions.

pub mod broker;
pub mod delivery;
pub mod document;
pub mod event;
pub mod execution;
pub mod feedback;
pub mod owned_children;
pub mod pipeline;
pub mod plan;
pub mod planning;
pub mod provider;
pub mod repair;
pub mod report;
pub mod review;
pub mod review_compat;
pub mod source;
pub mod usage;
pub mod verification;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{PortCardinality, is_artifact_type, is_digest};

pub const TASK_REVISION_V1: &str = "af/TaskRevision@1";
pub const TASK_RESULT_V1: &str = "af/TaskResult@1";
pub const PIPELINE_V1: &str = "af/Pipeline@1";
pub const EXECUTION_PLAN_V1: &str = "af/ExecutionPlan@1";
pub const PLAN_DECISION_V1: &str = "af/PlanDecision@1";
pub const REVIEW_HISTORY_V1: &str = "af/ReviewHistory@1";
pub const VERIFICATION_CONTINUATION_V1: &str = "af/VerificationContinuation@1";
pub const REPAIR_ASSESSMENT_V1: &str = "af/RepairAssessment@1";

pub(super) fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

pub fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphanumeric() || (i > 0 && matches!(b, b'_' | b'-')))
}

pub fn is_package_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.split('/').all(is_name)
}

pub(super) fn safe_number(value: u64) -> bool {
    value <= crate::json::SAFE_INTEGER_MAX as u64
}

/// Optional properties allow omission, not JSON null. Keep serde admission aligned with
/// the wire schema before the value is canonicalized and receives an identity.
pub fn present_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

pub(super) fn unique_set<'de, D, T>(deserializer: D) -> Result<BTreeSet<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Ord,
{
    let items = Vec::<T>::deserialize(deserializer)?;
    let mut set = BTreeSet::new();
    for item in items {
        if set.last().is_some_and(|previous| previous >= &item) {
            return Err(serde::de::Error::custom(
                "set entries must be sorted and unique",
            ));
        }
        set.insert(item);
    }
    Ok(set)
}

pub(super) fn unique_set_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, BTreeSet<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Set(#[serde(deserialize_with = "unique_set")] BTreeSet<String>);
    BTreeMap::<String, Set>::deserialize(deserializer)
        .map(|m| m.into_iter().map(|(k, v)| (k, v.0)).collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactInputV1 {
    pub artifact_ids: Vec<String>,
    pub artifact_type: String,
    pub cardinality: PortCardinality,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub snapshot_id: Option<String>,
}

impl ArtifactInputV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.artifact_ids.iter().all(|id| is_digest(id))
                && self.artifact_ids.iter().collect::<BTreeSet<_>>().len()
                    == self.artifact_ids.len()
                && (self.cardinality == PortCardinality::Many || self.artifact_ids.len() == 1)
                && is_artifact_type(&self.artifact_type)
                && self.snapshot_id.as_deref().is_none_or(is_digest),
            "Task input requires an exact artifact ID, versioned type and valid Snapshot ID",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthorityV1 {
    pub policy_id: String,
    #[serde(deserialize_with = "unique_set")]
    pub allowed_effects: BTreeSet<String>,
    #[serde(deserialize_with = "unique_set")]
    pub data_destinations: BTreeSet<String>,
}

impl TaskAuthorityV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.policy_id),
            "Task policy must be an exact artifact ID",
        )?;
        require(
            self.allowed_effects.iter().all(|s| is_name(s))
                && self.data_destinations.iter().all(|s| is_name(s)),
            "Task effects and destinations must name trusted policy entries",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReserveV1 {
    pub tokens: u64,
    pub attempts: u32,
    pub wall_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLimitsV1 {
    pub tokens: u64,
    pub max_attempts: u32,
    pub deadline_unix_ms: u64,
    pub verification: VerificationReserveV1,
}

impl TaskLimitsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            safe_number(self.tokens)
                && self.max_attempts > 0
                && self.deadline_unix_ms > 0
                && safe_number(self.deadline_unix_ms)
                && safe_number(self.verification.wall_ms)
                && self.verification.tokens <= self.tokens
                && self.verification.attempts <= self.max_attempts,
            "Task limits must be safe integers and include the protected verification reserve",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskFactV1 {
    Boolean(bool),
    Text(String),
    Integer(i64),
}

impl TaskFactV1 {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Integer(n) => require(
                (crate::json::SAFE_INTEGER_MIN..=crate::json::SAFE_INTEGER_MAX).contains(n),
                "Unsafe Task fact integer",
            ),
            Self::Text(s) => require(s.chars().count() <= 4096, "Task fact text is too large"),
            Self::Boolean(_) => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredOutputV1 {
    pub artifact_type: String,
    pub cardinality: PortCardinality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProvenanceV1 {
    pub adapter_id: String,
    pub input_artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineFallbackV1 {
    Refuse,
    Select,
    Generate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineChoiceV1 {
    pub name: String,
    pub fallback: PipelineFallbackV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceObligationV1 {
    pub evidence_type: String,
    pub verifier_policy: String,
}

impl AcceptanceObligationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_artifact_type(&self.evidence_type) && is_digest(&self.verifier_policy),
            "Acceptance needs a versioned evidence type and exact trusted verifier policy",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRevisionV1 {
    pub task_id: String,
    pub revision: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub previous_revision_id: Option<String>,
    pub kind: String,
    pub goal: String,
    pub inputs: BTreeMap<String, ArtifactInputV1>,
    pub required_outputs: BTreeMap<String, RequiredOutputV1>,
    pub acceptance: BTreeMap<String, AcceptanceObligationV1>,
    pub provenance: TaskProvenanceV1,
    pub authority: TaskAuthorityV1,
    pub limits: TaskLimitsV1,
    pub strategy: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub pipeline: Option<PipelineChoiceV1>,
    pub facts: BTreeMap<String, TaskFactV1>,
}

impl TaskRevisionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.task_id) && is_package_name(&self.kind),
            "Invalid Task identity or kind",
        )?;
        require(self.revision > 0, "Task revision starts at one")?;
        require(
            if self.revision == 1 {
                self.previous_revision_id.is_none()
            } else {
                self.previous_revision_id.as_deref().is_some_and(is_digest)
            },
            "Task revision must retain its exact predecessor",
        )?;
        require(
            !self.goal.trim().is_empty() && self.goal.chars().count() <= 65536,
            "Task goal must be present and bounded",
        )?;
        require(is_name(&self.strategy), "Invalid Task strategy")?;
        require(
            !self.acceptance.is_empty(),
            "Task must declare acceptance obligations",
        )?;
        require(
            !self.required_outputs.is_empty()
                && self
                    .required_outputs
                    .iter()
                    .all(|(name, output)| is_name(name) && is_artifact_type(&output.artifact_type)),
            "Task must declare typed required outputs",
        )?;
        require(
            is_digest(&self.provenance.adapter_id)
                && self
                    .provenance
                    .input_artifact_ids
                    .iter()
                    .all(|id| is_digest(id))
                && self
                    .provenance
                    .input_artifact_ids
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    == self.provenance.input_artifact_ids.len(),
            "Task provenance requires an exact adapter and unique input identities",
        )?;
        self.authority.validate()?;
        self.limits.validate()?;
        for (name, input) in &self.inputs {
            require(is_name(name), "Invalid Task input name")?;
            input.validate()?;
        }
        for (name, obligation) in &self.acceptance {
            require(is_name(name), "Invalid acceptance obligation name")?;
            obligation.validate()?;
        }
        if let Some(choice) = &self.pipeline {
            require(is_package_name(&choice.name), "Invalid Pipeline choice")?;
        }
        for (name, value) in &self.facts {
            require(is_name(name), "Invalid Task fact name")?;
            value.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskExecutionV1 {
    Completed,
    Incomplete,
    Blocked,
    Exhausted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAcceptanceV1 {
    Satisfied,
    Unsatisfied,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskWaitingReasonV1 {
    NeedsResources,
    NeedsInput,
    NeedsHuman,
    NeedsPlanReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskPhaseV1 {
    Submitted {},
    Resolving {},
    Planning {},
    Ready {},
    Running {},
    Verifying {},
    Waiting { reason: TaskWaitingReasonV1 },
    Finished { result_id: String },
}

impl TaskPhaseV1 {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Finished { result_id } => require(
                is_digest(result_id),
                "Finished Task requires an exact result artifact",
            ),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResultV1 {
    pub task_revision_id: String,
    pub execution: TaskExecutionV1,
    pub acceptance: TaskAcceptanceV1,
    pub domain_conclusion: String,
    pub outputs: BTreeMap<String, ArtifactInputV1>,
    #[serde(deserialize_with = "unique_set")]
    pub evidence: BTreeSet<String>,
    #[serde(deserialize_with = "unique_set")]
    pub missing_obligations: BTreeSet<String>,
}

impl TaskResultV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.task_revision_id),
            "Task result needs its exact revision",
        )?;
        require(
            !self.domain_conclusion.trim().is_empty(),
            "Task result needs a domain conclusion",
        )?;
        if self.acceptance == TaskAcceptanceV1::Satisfied {
            require(
                self.execution == TaskExecutionV1::Completed
                    && self.missing_obligations.is_empty()
                    && !self.evidence.is_empty()
                    && !self.outputs.is_empty(),
                "Satisfied acceptance requires completed execution and evidence with no missing obligations",
            )?;
        }
        require(
            self.evidence.iter().all(|id| is_digest(id)),
            "Invalid Task evidence ID",
        )?;
        require(
            self.missing_obligations.iter().all(|s| is_name(s)),
            "Invalid missing obligation name",
        )?;
        for (name, value) in &self.outputs {
            require(is_name(name), "Invalid Task output name")?;
            value.validate()?;
            require(
                !value.artifact_ids.is_empty(),
                "A present Task result output must contain an artifact",
            )?;
        }
        Ok(())
    }
}
