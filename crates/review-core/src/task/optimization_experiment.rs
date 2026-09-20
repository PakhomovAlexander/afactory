//! Versioned authority and evidence for bounded optimization experiments.
//!
//! These values deliberately do not extend `TaskOwnedChildSetV1`: an owned-child set is data,
//! while an experimental closure needs its own exact developer authority before registration.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{is_name, is_package_name, require, safe_number};
use crate::is_digest;

pub const EXPERIMENTAL_SLOT_V1: &str = "af/ExperimentalSlot@1";
pub const EXPERIMENTAL_SLOT_V2: &str = "af/ExperimentalSlot@2";
pub const OPTIMIZATION_HARNESS_V1: &str = "af/OptimizationHarness@1";
pub const EXPERIMENT_SPECIFICATION_V1: &str = "af/ExperimentSpecification@1";
pub const EXPERIMENT_PREPARED_V1: &str = "af/ExperimentPrepared@1";
pub const EXPERIMENT_PLAN_DECISION_V1: &str = "af/ExperimentPlanDecision@1";
pub const EXPERIMENT_COMPARISON_V1: &str = "af/ExperimentComparison@1";
pub const EXPERIMENT_TRIAL_RESULT_V1: &str = "af/ExperimentTrialResult@1";
pub const OPTIMIZATION_VERIFICATION_V1: &str = "af/OptimizationVerification@1";
pub const OPTIMIZATION_EVALUATION_V1: &str = "af/OptimizationEvaluation@1";
pub const OPTIMIZATION_PACKAGE_REPIN_V1: &str = "af/OptimizationPackageRepin@1";
pub const HARNESS_MATERIALIZATION_V1: &str = "af/HarnessMaterialization@1";

fn digests(values: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    values.into_iter().all(|value| is_digest(value.as_ref()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentAllowanceV1 {
    pub tokens: u64,
    pub attempts: u32,
    pub wall_ms: u64,
}

impl ExperimentAllowanceV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.tokens > 0
                && self.attempts > 0
                && self.wall_ms > 0
                && safe_number(self.tokens)
                && safe_number(self.wall_ms),
            "Experimental allowance must be positive and in the safe integer domain",
        )
    }

    fn contains(&self, child: &Self) -> bool {
        child.tokens <= self.tokens
            && child.attempts <= self.attempts
            && child.wall_ms <= self.wall_ms
    }
}

/// Immutable outer-plan authority for preparing, but not dispatching, generated children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentalSlotV1 {
    pub schema: String,
    pub slot: String,
    pub outer_plan_id: String,
    pub policy_id: String,
    pub protected_oracle_id: String,
    pub allowed_task_kinds: BTreeSet<String>,
    pub allowed_packages: BTreeSet<String>,
    pub allowed_worker_package_ids: BTreeSet<String>,
    pub allowed_efforts: BTreeSet<String>,
    pub allowed_effects: BTreeSet<String>,
    pub max_children: u32,
    pub max_depth: u32,
    pub max_concurrency: u32,
    pub max_development_candidates: u32,
    pub allowance: ExperimentAllowanceV1,
}

impl ExperimentalSlotV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.allowance.validate()?;
        require(
            self.schema == "af.experimental-slot/1"
                && is_name(&self.slot)
                && digests([
                    &self.outer_plan_id,
                    &self.policy_id,
                    &self.protected_oracle_id,
                ])
                && !self.allowed_task_kinds.is_empty()
                && self.allowed_task_kinds.iter().all(|value| is_name(value))
                && !self.allowed_packages.is_empty()
                && self
                    .allowed_packages
                    .iter()
                    .all(|value| is_package_name(value))
                && !self.allowed_worker_package_ids.is_empty()
                && self
                    .allowed_worker_package_ids
                    .iter()
                    .all(|value| is_digest(value))
                && !self.allowed_efforts.is_empty()
                && self.allowed_efforts.iter().all(|value| is_name(value))
                && self.allowed_effects.iter().all(|value| is_name(value))
                && (1..=4096).contains(&self.max_children)
                && (1..=8).contains(&self.max_depth)
                && (1..=256).contains(&self.max_concurrency)
                && (1..=3).contains(&self.max_development_candidates)
                && self.max_concurrency <= self.max_children,
            "Experimental slot needs exact bounded preparation authority",
        )
    }
}

/// Cycle-free slot generation. `outer_plan_binding_id` is a compiler domain digest over the
/// Task revision, policy and logical slot before artifact IDs are assigned. The Store separately
/// proves that this exact slot artifact is embedded in the current outer compiled graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentalSlotV2 {
    pub schema: String,
    pub slot: String,
    pub outer_plan_binding_id: String,
    pub policy_id: String,
    pub protected_oracle_id: String,
    pub allowed_task_kinds: BTreeSet<String>,
    pub allowed_packages: BTreeSet<String>,
    pub allowed_worker_package_ids: BTreeSet<String>,
    pub allowed_efforts: BTreeSet<String>,
    pub allowed_effects: BTreeSet<String>,
    pub max_children: u32,
    pub max_depth: u32,
    pub max_concurrency: u32,
    pub max_development_candidates: u32,
    pub allowance: ExperimentAllowanceV1,
}

impl ExperimentalSlotV2 {
    pub fn validate(&self) -> Result<(), String> {
        let compatibility = ExperimentalSlotV1 {
            schema: "af.experimental-slot/1".into(),
            slot: self.slot.clone(),
            outer_plan_id: self.outer_plan_binding_id.clone(),
            policy_id: self.policy_id.clone(),
            protected_oracle_id: self.protected_oracle_id.clone(),
            allowed_task_kinds: self.allowed_task_kinds.clone(),
            allowed_packages: self.allowed_packages.clone(),
            allowed_worker_package_ids: self.allowed_worker_package_ids.clone(),
            allowed_efforts: self.allowed_efforts.clone(),
            allowed_effects: self.allowed_effects.clone(),
            max_children: self.max_children,
            max_depth: self.max_depth,
            max_concurrency: self.max_concurrency,
            max_development_candidates: self.max_development_candidates,
            allowance: self.allowance.clone(),
        };
        require(
            self.schema == "af.experimental-slot/2",
            "Experimental slot uses the wrong cycle-free generation",
        )?;
        compatibility.validate()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn validate_experiment_registration_v2(
    slot_id: &str,
    slot: &ExperimentalSlotV2,
    specification_id: &str,
    specification: &ExperimentSpecificationV1,
    prepared_id: &str,
    prepared: &ExperimentPreparedV1,
    decision: &ExperimentPlanDecisionV1,
    now_unix_ms: u64,
    remaining: &ExperimentAllowanceV1,
) -> Result<(), String> {
    slot.validate()?;
    let compatibility = ExperimentalSlotV1 {
        schema: "af.experimental-slot/1".into(),
        slot: slot.slot.clone(),
        // V2 binds the actual outer plan through ExperimentPrepared and Store graph membership.
        outer_plan_id: prepared.outer_plan_id.clone(),
        policy_id: slot.policy_id.clone(),
        protected_oracle_id: slot.protected_oracle_id.clone(),
        allowed_task_kinds: slot.allowed_task_kinds.clone(),
        allowed_packages: slot.allowed_packages.clone(),
        allowed_worker_package_ids: slot.allowed_worker_package_ids.clone(),
        allowed_efforts: slot.allowed_efforts.clone(),
        allowed_effects: slot.allowed_effects.clone(),
        max_children: slot.max_children,
        max_depth: slot.max_depth,
        max_concurrency: slot.max_concurrency,
        max_development_candidates: slot.max_development_candidates,
        allowance: slot.allowance.clone(),
    };
    validate_experiment_registration(
        slot_id,
        &compatibility,
        specification_id,
        specification,
        prepared_id,
        prepared,
        decision,
        now_unix_ms,
        remaining,
    )
}

/// Candidate-writable bytes are disjoint from every transitive oracle dependency and cache
/// widening is explicitly impossible under this captured contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationHarnessV1 {
    pub schema: String,
    pub oracle_id: String,
    pub checks: BTreeSet<String>,
    pub transitive_dependencies: BTreeSet<String>,
    pub candidate_writable_paths: BTreeSet<String>,
    pub cache_read_scopes: BTreeSet<String>,
    pub cache_write_scopes: BTreeSet<String>,
}

impl OptimizationHarnessV1 {
    pub fn validate(&self) -> Result<(), String> {
        let path = |value: &String| {
            !value.is_empty()
                && value.len() <= 1024
                && !value.starts_with('/')
                && !value.contains('\\')
                && !value
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
                && !value.chars().any(char::is_control)
        };
        let overlaps = |left: &str, right: &str| {
            left == right
                || left
                    .strip_prefix(right)
                    .is_some_and(|suffix| suffix.starts_with('/'))
                || right
                    .strip_prefix(left)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        };
        require(
            self.schema == "af.optimization-harness/1"
                && is_digest(&self.oracle_id)
                && !self.checks.is_empty()
                && self.checks.iter().all(|value| is_name(value))
                && !self.transitive_dependencies.is_empty()
                && self.transitive_dependencies.iter().all(path)
                && self.candidate_writable_paths.iter().all(path)
                && !self.transitive_dependencies.iter().any(|protected| {
                    self.candidate_writable_paths
                        .iter()
                        .any(|writable| overlaps(protected, writable))
                })
                && self.cache_read_scopes.iter().all(path)
                && self.cache_write_scopes.iter().all(path)
                && self.cache_write_scopes.iter().all(|write| {
                    self.cache_read_scopes.iter().any(|read| {
                        write == read
                            || write
                                .strip_prefix(read)
                                .is_some_and(|suffix| suffix.starts_with('/'))
                    })
                }),
            "Protected harness must keep checks, transitive oracle bytes and cache authority outside candidate control",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePinChangeV1 {
    pub before: String,
    pub after: String,
}

/// Trusted finalizer receipt. Exactly the entailed package entries may move; engine and
/// protected policy pins are repeated here so a consumer can reject a repin after verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationPackageRepinV1 {
    pub schema: String,
    pub source_snapshot_id: String,
    pub candidate_snapshot_id: String,
    pub before_lock_id: String,
    pub after_lock_id: String,
    pub engine_release_id: String,
    pub entailed: BTreeMap<String, PackagePinChangeV1>,
    pub protected_pins: BTreeMap<String, String>,
}

impl OptimizationPackageRepinV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-package-repin/1"
                && digests([
                    &self.source_snapshot_id,
                    &self.candidate_snapshot_id,
                    &self.before_lock_id,
                    &self.after_lock_id,
                    &self.engine_release_id,
                ])
                && self.source_snapshot_id != self.candidate_snapshot_id
                && (self.entailed.is_empty() == (self.before_lock_id == self.after_lock_id))
                && self.entailed.keys().all(|name| is_package_name(name))
                && self.entailed.values().all(|change| {
                    is_digest(&change.before)
                        && is_digest(&change.after)
                        && change.before != change.after
                })
                && self.protected_pins.iter().all(|(name, id)| {
                    is_package_name(name) && is_digest(id) && !self.entailed.contains_key(name)
                }),
            "Trusted repin must change only entailed package pins and retain protected authority",
        )
    }
}

/// Deterministic correction never relabels a modified tree as historical source. Each arm has
/// a distinct derived Snapshot while the product source and Requirements remain fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessMaterializationV1 {
    pub schema: String,
    pub fixture_constructor_id: String,
    pub product_source_snapshot_id: String,
    pub requirements_id: String,
    pub protected_oracle_id: String,
    pub baseline_harness_id: String,
    pub candidate_harness_id: String,
    pub baseline_derived_snapshot_id: String,
    pub candidate_derived_snapshot_id: String,
    pub environment_id: String,
}

impl HarnessMaterializationV1 {
    pub fn validate(&self) -> Result<(), String> {
        let ids = [
            &self.fixture_constructor_id,
            &self.product_source_snapshot_id,
            &self.requirements_id,
            &self.protected_oracle_id,
            &self.baseline_harness_id,
            &self.candidate_harness_id,
            &self.baseline_derived_snapshot_id,
            &self.candidate_derived_snapshot_id,
            &self.environment_id,
        ];
        require(
            self.schema == "af.harness-materialization/1"
                && digests(ids)
                && self.baseline_harness_id != self.candidate_harness_id
                && self.baseline_derived_snapshot_id != self.candidate_derived_snapshot_id
                && self.baseline_derived_snapshot_id != self.product_source_snapshot_id
                && self.candidate_derived_snapshot_id != self.product_source_snapshot_id,
            "Harness correction needs distinct derived arm Snapshots and fixed source identity",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentArmV1 {
    Baseline,
    Candidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonRecipeV1 {
    DeterministicCorrection,
    TokensPerVerifiedOutcome,
    Latency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonUncertaintyRuleV1 {
    Deterministic,
    RepetitionDispersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentCaseV1 {
    pub case_id: String,
    pub family_id: String,
    pub membership: String,
    pub source_snapshot_id: String,
    pub requirements_id: String,
    pub compatibility_id: String,
}

impl ExperimentCaseV1 {
    fn validate(&self) -> Result<(), String> {
        require(
            digests([
                &self.case_id,
                &self.family_id,
                &self.source_snapshot_id,
                &self.requirements_id,
                &self.compatibility_id,
            ]) && matches!(self.membership.as_str(), "development" | "holdout"),
            "Experiment case needs frozen family, source, Requirements and compatibility identity",
        )
    }
}

/// Fixed before results: source/Requirements are common while arm authority is separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentSpecificationV1 {
    pub schema: String,
    pub slot_id: String,
    pub policy_id: String,
    pub profile_id: String,
    pub development_set_id: String,
    pub holdout_set_id: String,
    pub protected_oracle_id: String,
    pub baseline_authority_id: String,
    pub candidate_authority_id: String,
    pub baseline_package: String,
    pub candidate_package: String,
    pub recipe: ComparisonRecipeV1,
    pub uncertainty_rule: ComparisonUncertaintyRuleV1,
    pub repetitions: u32,
    pub minimum_families: u32,
    pub token_increase_ceiling_bps: u32,
    /// Families exposed to diagnosis or an earlier report before this specification was sealed.
    /// A confirmatory holdout may never draw from this set.
    pub exposed_family_ids: BTreeSet<String>,
    pub cases: Vec<ExperimentCaseV1>,
}

impl ExperimentSpecificationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.experiment-specification/1"
                && digests([
                    &self.slot_id,
                    &self.policy_id,
                    &self.profile_id,
                    &self.development_set_id,
                    &self.holdout_set_id,
                    &self.protected_oracle_id,
                    &self.baseline_authority_id,
                    &self.candidate_authority_id,
                ])
                && self.baseline_authority_id != self.candidate_authority_id
                && is_package_name(&self.baseline_package)
                && is_package_name(&self.candidate_package)
                && self.repetitions > 0
                && self.minimum_families > 0
                && self.token_increase_ceiling_bps <= 100_000
                && matches!(
                    (self.recipe, self.uncertainty_rule),
                    (
                        ComparisonRecipeV1::DeterministicCorrection,
                        ComparisonUncertaintyRuleV1::Deterministic
                    ) | (
                        ComparisonRecipeV1::TokensPerVerifiedOutcome | ComparisonRecipeV1::Latency,
                        ComparisonUncertaintyRuleV1::RepetitionDispersion
                    )
                )
                && digests(&self.exposed_family_ids)
                && !self.cases.is_empty(),
            "Experiment specification must freeze separate arm authority, sets and comparison rule",
        )?;
        let mut cases = BTreeSet::new();
        for case in &self.cases {
            case.validate()?;
            require(cases.insert(&case.case_id), "Duplicate experiment case")?;
        }
        require(
            self.cases.iter().any(|case| case.membership == "holdout"),
            "Experiment needs a pre-frozen holdout",
        )?;
        require(
            !self.cases.iter().any(|case| {
                case.membership == "holdout" && self.exposed_family_ids.contains(&case.family_id)
            }),
            "An exposed family cannot be reused as a confirmatory holdout",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentChildClosureV1 {
    pub node: String,
    pub arm: ExperimentArmV1,
    pub case_id: String,
    pub repetition: u32,
    pub task_kind: String,
    pub package: String,
    pub worker_package_id: String,
    pub effort: String,
    pub effects: BTreeSet<String>,
    pub source_snapshot_id: String,
    pub requirements_id: String,
    pub authority_id: String,
    pub invocation_id: String,
    pub allowance: ExperimentAllowanceV1,
}

impl ExperimentChildClosureV1 {
    fn validate(&self) -> Result<(), String> {
        self.allowance.validate()?;
        require(
            self.node.split('.').all(is_name)
                && self.repetition > 0
                && is_name(&self.task_kind)
                && is_package_name(&self.package)
                && is_name(&self.effort)
                && self.effects.iter().all(|value| is_name(value))
                && digests([
                    &self.case_id,
                    &self.worker_package_id,
                    &self.source_snapshot_id,
                    &self.requirements_id,
                    &self.authority_id,
                    &self.invocation_id,
                ]),
            "Experimental child closure must bind effective execution and exact inputs",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentPreparedV1 {
    pub schema: String,
    pub task_revision_id: String,
    pub outer_plan_id: String,
    pub slot_id: String,
    pub specification_id: String,
    pub compiled_child_plan_id: String,
    pub policy_id: String,
    pub spent_accounting_prefix_id: String,
    pub writer_epoch: u64,
    pub children: Vec<ExperimentChildClosureV1>,
}

impl ExperimentPreparedV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.experiment-prepared/1"
                && self.writer_epoch > 0
                && digests([
                    &self.task_revision_id,
                    &self.outer_plan_id,
                    &self.slot_id,
                    &self.specification_id,
                    &self.compiled_child_plan_id,
                    &self.policy_id,
                    &self.spent_accounting_prefix_id,
                ])
                && !self.children.is_empty(),
            "Prepared experiment requires exact outer/child authority and accounting prefix",
        )?;
        let mut nodes = BTreeSet::new();
        let mut invocations = BTreeSet::new();
        for child in &self.children {
            child.validate()?;
            require(
                nodes.insert(&child.node) && invocations.insert(&child.invocation_id),
                "Prepared experiment repeats a node or invocation",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentDecisionKindV1 {
    Approved,
    Rejected,
}

/// The signature is authenticated by the host; this payload binds what was signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentPlanDecisionV1 {
    pub schema: String,
    pub prepared_id: String,
    pub task_revision_id: String,
    pub outer_plan_id: String,
    pub slot_id: String,
    pub specification_id: String,
    pub compiled_child_plan_id: String,
    pub policy_id: String,
    pub developer: String,
    pub authorization_id: String,
    pub key_policy_id: String,
    pub signature_id: String,
    pub decision: ExperimentDecisionKindV1,
    pub expires_unix_ms: u64,
    pub reason: String,
}

impl ExperimentPlanDecisionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.experiment-plan-decision/1"
                && safe_number(self.expires_unix_ms)
                && self.expires_unix_ms > 0
                && digests([
                    &self.prepared_id,
                    &self.task_revision_id,
                    &self.outer_plan_id,
                    &self.slot_id,
                    &self.specification_id,
                    &self.compiled_child_plan_id,
                    &self.policy_id,
                    &self.authorization_id,
                    &self.key_policy_id,
                    &self.signature_id,
                ])
                && !self.developer.trim().is_empty()
                && !self.reason.trim().is_empty(),
            "Experiment decision must bind the full prepared closure and signed authority",
        )
    }

    pub fn approves(&self, prepared_id: &str, prepared: &ExperimentPreparedV1, now: u64) -> bool {
        self.decision == ExperimentDecisionKindV1::Approved
            && now < self.expires_unix_ms
            && self.prepared_id == prepared_id
            && self.task_revision_id == prepared.task_revision_id
            && self.outer_plan_id == prepared.outer_plan_id
            && self.slot_id == prepared.slot_id
            && self.specification_id == prepared.specification_id
            && self.compiled_child_plan_id == prepared.compiled_child_plan_id
            && self.policy_id == prepared.policy_id
    }
}

/// Pure half of the Store admission barrier. The Store additionally authenticates the
/// signature/revocation, checks its lease/current prefix, and reserves these children atomically.
#[allow(clippy::too_many_arguments)]
pub fn validate_experiment_registration(
    slot_id: &str,
    slot: &ExperimentalSlotV1,
    specification_id: &str,
    specification: &ExperimentSpecificationV1,
    prepared_id: &str,
    prepared: &ExperimentPreparedV1,
    decision: &ExperimentPlanDecisionV1,
    now_unix_ms: u64,
    remaining: &ExperimentAllowanceV1,
) -> Result<(), String> {
    slot.validate()?;
    specification.validate()?;
    prepared.validate()?;
    decision.validate()?;
    remaining.validate()?;
    require(
        specification.slot_id == slot_id
            && specification.policy_id == slot.policy_id
            && specification.protected_oracle_id == slot.protected_oracle_id
            && prepared.slot_id == slot_id
            && prepared.outer_plan_id == slot.outer_plan_id
            && prepared.policy_id == slot.policy_id
            && prepared.specification_id == specification_id
            && decision.approves(prepared_id, prepared, now_unix_ms),
        "Experimental authority is stale, rejected, expired or belongs to another parent/slot",
    )?;
    require(
        prepared.children.len() <= slot.max_children as usize,
        "Experimental child count exceeds captured slot",
    )?;
    let cases: BTreeMap<_, _> = specification
        .cases
        .iter()
        .map(|case| (&case.case_id, case))
        .collect();
    let mut total = ExperimentAllowanceV1 {
        tokens: 0,
        attempts: 0,
        wall_ms: 0,
    };
    let mut expected = BTreeSet::new();
    for child in &prepared.children {
        let case = cases
            .get(&child.case_id)
            .ok_or("Experimental child names an unfrozen case")?;
        let expected_authority = match child.arm {
            ExperimentArmV1::Baseline => &specification.baseline_authority_id,
            ExperimentArmV1::Candidate => &specification.candidate_authority_id,
        };
        let expected_package = match child.arm {
            ExperimentArmV1::Baseline => &specification.baseline_package,
            ExperimentArmV1::Candidate => &specification.candidate_package,
        };
        // A candidate may select one derived package artifact as its separately approved arm
        // authority. The Store must additionally prove that artifact is the constrained
        // instruction-only derivation of an allowed captured package. Other children retain the
        // original exact membership rule.
        let allowed_worker = slot
            .allowed_worker_package_ids
            .contains(&child.worker_package_id)
            || (child.arm == ExperimentArmV1::Candidate
                && child.worker_package_id == specification.candidate_authority_id);
        require(
            slot.allowed_task_kinds.contains(&child.task_kind)
                && slot.allowed_packages.contains(&child.package)
                && allowed_worker
                && slot.allowed_efforts.contains(&child.effort)
                && child.effects.is_subset(&slot.allowed_effects)
                && child.node.split('.').count() <= slot.max_depth as usize
                && &child.authority_id == expected_authority
                && &child.package == expected_package
                && child.source_snapshot_id == case.source_snapshot_id
                && child.requirements_id == case.requirements_id
                && child.repetition <= specification.repetitions
                && expected.insert((child.case_id.clone(), child.arm, child.repetition)),
            "Experimental child changed package/Worker/effects/case authority or repeats work",
        )?;
        total.tokens = total
            .tokens
            .checked_add(child.allowance.tokens)
            .ok_or("Experimental token bound overflow")?;
        total.attempts = total
            .attempts
            .checked_add(child.allowance.attempts)
            .ok_or("Experimental Attempt bound overflow")?;
        total.wall_ms = total
            .wall_ms
            .checked_add(child.allowance.wall_ms)
            .ok_or("Experimental wall-time bound overflow")?;
    }
    for case in &specification.cases {
        for repetition in 1..=specification.repetitions {
            for arm in [ExperimentArmV1::Baseline, ExperimentArmV1::Candidate] {
                require(
                    expected.contains(&(case.case_id.clone(), arm, repetition)),
                    "Prepared closure omits a declared arm or repetition",
                )?;
            }
        }
    }
    require(
        slot.allowance.contains(&total) && remaining.contains(&total),
        "Experimental closure exceeds original parent or remaining allowance",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentTrialV1 {
    pub invocation_id: String,
    pub case_id: String,
    pub family_id: String,
    pub arm: ExperimentArmV1,
    pub repetition: u32,
    pub compatibility_id: String,
    pub verified: bool,
    pub protected_checks_passed: bool,
    pub billing_complete: bool,
    pub charged_tokens: u64,
    pub elapsed_ms: u64,
    pub preparation_ms: u64,
    pub cache_population_ms: u64,
    pub cache_lookup_ms: u64,
    pub cache_copy_ms: u64,
    /// A zero phase total is a measured zero only when its name is absent here. Runtime adapters
    /// list unsupported phases explicitly so missing cache evidence cannot appear as savings.
    pub missing_measurements: BTreeSet<String>,
    /// Monotonic intervals relative to the beginning of this invocation. Named phase totals
    /// above must equal their interval unions. Overall elapsed is the union of every interval,
    /// so preparation/cache work already inside execution is never added twice.
    pub intervals: Vec<ExperimentMeasurementIntervalV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentIntervalKindV1 {
    Execution,
    Preparation,
    CachePopulation,
    CacheLookup,
    CacheCopy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentMeasurementIntervalV1 {
    pub kind: ExperimentIntervalKindV1,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Protected verifier output for one registered arm invocation. Accounting and billing are
/// deliberately absent: the reducer derives them from the common Attempt ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentTrialResultV1 {
    pub schema: String,
    pub invocation_id: String,
    pub case_id: String,
    pub family_id: String,
    pub arm: ExperimentArmV1,
    pub repetition: u32,
    pub compatibility_id: String,
    pub verified: bool,
    pub protected_checks_passed: bool,
    pub intervals: Vec<ExperimentMeasurementIntervalV1>,
}

impl ExperimentTrialResultV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.experiment-trial-result/1"
                && digests([
                    &self.invocation_id,
                    &self.case_id,
                    &self.family_id,
                    &self.compatibility_id,
                ])
                && self.repetition > 0
                && !self.intervals.is_empty(),
            "Experiment trial result needs exact registered identity and measurements",
        )?;
        interval_union_ms(&self.intervals)?;
        Ok(())
    }

    pub fn into_trial(
        self,
        charged_tokens: u64,
        billing_complete: bool,
    ) -> Result<ExperimentTrialV1, String> {
        self.validate()?;
        let measured = |kind| {
            interval_union_ms(
                self.intervals
                    .iter()
                    .filter(|interval| interval.kind == kind),
            )
        };
        let elapsed_ms = measured(ExperimentIntervalKindV1::Execution)?;
        let preparation_ms = measured(ExperimentIntervalKindV1::Preparation)?;
        let cache_population_ms = measured(ExperimentIntervalKindV1::CachePopulation)?;
        let cache_lookup_ms = measured(ExperimentIntervalKindV1::CacheLookup)?;
        let cache_copy_ms = measured(ExperimentIntervalKindV1::CacheCopy)?;
        let trial = ExperimentTrialV1 {
            invocation_id: self.invocation_id,
            case_id: self.case_id,
            family_id: self.family_id,
            arm: self.arm,
            repetition: self.repetition,
            compatibility_id: self.compatibility_id,
            verified: self.verified,
            protected_checks_passed: self.protected_checks_passed,
            billing_complete,
            charged_tokens,
            elapsed_ms,
            preparation_ms,
            cache_population_ms,
            cache_lookup_ms,
            cache_copy_ms,
            missing_measurements: BTreeSet::new(),
            intervals: self.intervals,
        };
        trial.validate()?;
        Ok(trial)
    }
}

fn interval_union_ms<'a>(
    intervals: impl IntoIterator<Item = &'a ExperimentMeasurementIntervalV1>,
) -> Result<u64, String> {
    let mut spans = intervals
        .into_iter()
        .map(|interval| {
            require(
                interval.start_ms < interval.end_ms
                    && safe_number(interval.start_ms)
                    && safe_number(interval.end_ms),
                "Experiment interval must be a positive safe monotonic range",
            )?;
            Ok((interval.start_ms, interval.end_ms))
        })
        .collect::<Result<Vec<_>, String>>()?;
    spans.sort_unstable();
    let mut total = 0u64;
    let mut current: Option<(u64, u64)> = None;
    for (start, end) in spans {
        match current {
            Some((old_start, old_end)) if start <= old_end => {
                current = Some((old_start, old_end.max(end)));
            }
            Some((old_start, old_end)) => {
                total = total
                    .checked_add(old_end - old_start)
                    .ok_or("Experiment interval union overflow")?;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((start, end)) = current {
        total = total
            .checked_add(end - start)
            .ok_or("Experiment interval union overflow")?;
    }
    Ok(total)
}

impl ExperimentTrialV1 {
    fn validate(&self) -> Result<(), String> {
        require(
            digests([
                &self.invocation_id,
                &self.case_id,
                &self.family_id,
                &self.compatibility_id,
            ]) && self.repetition > 0
                && safe_number(self.charged_tokens)
                && [
                    self.elapsed_ms,
                    self.preparation_ms,
                    self.cache_population_ms,
                    self.cache_lookup_ms,
                    self.cache_copy_ms,
                ]
                .into_iter()
                .all(safe_number)
                && self.missing_measurements.iter().all(|name| {
                    matches!(
                        name.as_str(),
                        "execution"
                            | "preparation"
                            | "cache_population"
                            | "cache_lookup"
                            | "cache_copy"
                            | "cache_toolchain_identity"
                    )
                })
                && !self.intervals.is_empty(),
            "Experiment trial needs exact identity and bounded measured costs",
        )?;
        let declared = [
            (ExperimentIntervalKindV1::Execution, self.elapsed_ms),
            (ExperimentIntervalKindV1::Preparation, self.preparation_ms),
            (
                ExperimentIntervalKindV1::CachePopulation,
                self.cache_population_ms,
            ),
            (ExperimentIntervalKindV1::CacheLookup, self.cache_lookup_ms),
            (ExperimentIntervalKindV1::CacheCopy, self.cache_copy_ms),
        ];
        for (kind, expected) in declared {
            let name = match kind {
                ExperimentIntervalKindV1::Execution => "execution",
                ExperimentIntervalKindV1::Preparation => "preparation",
                ExperimentIntervalKindV1::CachePopulation => "cache_population",
                ExperimentIntervalKindV1::CacheLookup => "cache_lookup",
                ExperimentIntervalKindV1::CacheCopy => "cache_copy",
            };
            let measured = interval_union_ms(
                self.intervals
                    .iter()
                    .filter(|interval| interval.kind == kind),
            )?;
            require(
                measured == expected
                    && (!self.missing_measurements.contains(name) || expected == 0),
                "Experiment phase total differs from its measured interval union",
            )?;
        }
        Ok(())
    }

    fn total_elapsed_ms(&self) -> Result<u64, String> {
        interval_union_ms(&self.intervals)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonConclusionV1 {
    Accepted,
    Rejected,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentComparisonV1 {
    pub schema: String,
    pub specification_id: String,
    pub prepared_id: String,
    pub conclusion: ComparisonConclusionV1,
    pub reason: String,
    pub baseline_verified: u32,
    pub candidate_verified: u32,
    pub baseline_tokens: u64,
    pub candidate_tokens: u64,
    pub baseline_elapsed_ms: u64,
    pub candidate_elapsed_ms: u64,
    pub trials: Vec<ExperimentTrialV1>,
}

#[derive(Default)]
struct ComparisonTotals {
    baseline_tokens: u64,
    candidate_tokens: u64,
    baseline_elapsed_ms: u64,
    candidate_elapsed_ms: u64,
    baseline_verified: u32,
    candidate_verified: u32,
}

fn comparison_totals<'a>(
    trials: impl IntoIterator<Item = &'a ExperimentTrialV1>,
) -> Result<ComparisonTotals, String> {
    let mut totals = ComparisonTotals::default();
    for trial in trials {
        let elapsed = trial.total_elapsed_ms()?;
        let (tokens, time, verified) = match trial.arm {
            ExperimentArmV1::Baseline => (
                &mut totals.baseline_tokens,
                &mut totals.baseline_elapsed_ms,
                &mut totals.baseline_verified,
            ),
            ExperimentArmV1::Candidate => (
                &mut totals.candidate_tokens,
                &mut totals.candidate_elapsed_ms,
                &mut totals.candidate_verified,
            ),
        };
        *tokens = tokens
            .checked_add(trial.charged_tokens)
            .ok_or("Experiment family token total overflow")?;
        *time = time
            .checked_add(elapsed)
            .ok_or("Experiment family elapsed total overflow")?;
        *verified = verified
            .checked_add(u32::from(trial.verified))
            .ok_or("Experiment family verified total overflow")?;
    }
    Ok(totals)
}

/// Signed basis-point improvement in cost per verified outcome. A missing denominator is not a
/// zero cost: it is an undefined comparison and therefore cannot support a measured claim.
fn normalized_improvement_bps(
    baseline_cost: u64,
    candidate_cost: u64,
    baseline_verified: u32,
    candidate_verified: u32,
) -> Option<i128> {
    if baseline_cost == 0 || baseline_verified == 0 || candidate_verified == 0 {
        return None;
    }
    let baseline = i128::from(baseline_cost) * i128::from(candidate_verified);
    let candidate = i128::from(candidate_cost) * i128::from(baseline_verified);
    Some((baseline - candidate) * 10_000 / baseline)
}

impl ExperimentComparisonV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.experiment-comparison/1"
                && digests([&self.specification_id, &self.prepared_id])
                && is_name(&self.reason)
                && !self.trials.is_empty(),
            "Comparison needs exact experiment identity and explicit conclusion",
        )?;
        let mut invocations = BTreeSet::new();
        for trial in &self.trials {
            trial.validate()?;
            require(
                invocations.insert(&trial.invocation_id),
                "An invocation may be charged only once",
            )?;
        }
        Ok(())
    }
}

pub fn compare_experiment(
    specification_id: &str,
    prepared_id: &str,
    specification: &ExperimentSpecificationV1,
    trials: Vec<ExperimentTrialV1>,
) -> Result<ExperimentComparisonV1, String> {
    specification.validate()?;
    require(!trials.is_empty(), "Comparison has no trials")?;
    let cases: BTreeMap<_, _> = specification
        .cases
        .iter()
        .map(|case| (&case.case_id, case))
        .collect();
    let mut keys = BTreeSet::new();
    let mut invocations = BTreeSet::new();
    let mut baseline_verified = 0u32;
    let mut candidate_verified = 0u32;
    let mut baseline_tokens = 0u64;
    let mut candidate_tokens = 0u64;
    let mut baseline_elapsed_ms = 0u64;
    let mut candidate_elapsed_ms = 0u64;
    let mut billing_complete = true;
    let mut checks_pass = true;
    for trial in &trials {
        trial.validate()?;
        let case = cases
            .get(&trial.case_id)
            .ok_or("Trial names an unfrozen case")?;
        require(
            trial.family_id == case.family_id
                && trial.compatibility_id == case.compatibility_id
                && trial.repetition <= specification.repetitions
                && keys.insert((trial.case_id.clone(), trial.arm, trial.repetition))
                && invocations.insert(trial.invocation_id.clone()),
            "Trial changed case compatibility or duplicates a charge",
        )?;
        let full_elapsed = trial.total_elapsed_ms()?;
        match trial.arm {
            ExperimentArmV1::Baseline => {
                baseline_verified += u32::from(trial.verified);
                baseline_tokens = baseline_tokens
                    .checked_add(trial.charged_tokens)
                    .ok_or("Baseline token cost overflow")?;
                baseline_elapsed_ms = baseline_elapsed_ms
                    .checked_add(full_elapsed)
                    .ok_or("Baseline elapsed cost overflow")?;
            }
            ExperimentArmV1::Candidate => {
                candidate_verified += u32::from(trial.verified);
                candidate_tokens = candidate_tokens
                    .checked_add(trial.charged_tokens)
                    .ok_or("Candidate token cost overflow")?;
                candidate_elapsed_ms = candidate_elapsed_ms
                    .checked_add(full_elapsed)
                    .ok_or("Candidate elapsed cost overflow")?;
                checks_pass &= trial.protected_checks_passed;
            }
        }
        billing_complete &= trial.billing_complete;
    }
    for case in &specification.cases {
        for repetition in 1..=specification.repetitions {
            for arm in [ExperimentArmV1::Baseline, ExperimentArmV1::Candidate] {
                require(
                    keys.contains(&(case.case_id.clone(), arm, repetition)),
                    "Comparison lacks a matched arm or repetition",
                )?;
            }
        }
    }
    let holdout = trials
        .iter()
        .filter(|trial| cases[&trial.case_id].membership == "holdout")
        .collect::<Vec<_>>();
    let families = holdout
        .iter()
        .map(|trial| &trial.family_id)
        .collect::<BTreeSet<_>>()
        .len();
    let broad_recipe = specification.recipe != ComparisonRecipeV1::DeterministicCorrection;
    // Quality is required on both sets. Development evidence is allowed to select the one light
    // candidate, but it is not disposable after selection: a candidate that regresses a
    // development family cannot be rescued by a clean holdout aggregate.
    let family_quality = trials
        .iter()
        .map(|trial| (cases[&trial.case_id].membership.as_str(), &trial.family_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .all(|(membership, family)| {
            let baseline = trials
                .iter()
                .filter(|trial| {
                    cases[&trial.case_id].membership == membership
                        && trial.family_id == *family
                        && trial.arm == ExperimentArmV1::Baseline
                        && trial.verified
                })
                .count();
            let candidate = trials
                .iter()
                .filter(|trial| {
                    cases[&trial.case_id].membership == membership
                        && trial.family_id == *family
                        && trial.arm == ExperimentArmV1::Candidate
                        && trial.verified
                        && trial.protected_checks_passed
                })
                .count();
            candidate >= baseline
        });
    let mut objective_met = true;
    let mut observed_effects = Vec::new();
    if broad_recipe {
        for family in holdout
            .iter()
            .map(|trial| &trial.family_id)
            .collect::<BTreeSet<_>>()
        {
            for repetition in 1..=specification.repetitions {
                let totals =
                    comparison_totals(holdout.iter().copied().filter(|trial| {
                        trial.family_id == *family && trial.repetition == repetition
                    }))?;
                let (effect, constraint_met) = match specification.recipe {
                    ComparisonRecipeV1::TokensPerVerifiedOutcome => {
                        let effect = normalized_improvement_bps(
                            totals.baseline_tokens,
                            totals.candidate_tokens,
                            totals.baseline_verified,
                            totals.candidate_verified,
                        );
                        let latency_ok = u128::from(totals.candidate_elapsed_ms)
                            * u128::from(totals.baseline_verified)
                            <= u128::from(totals.baseline_elapsed_ms)
                                * u128::from(totals.candidate_verified);
                        (effect, latency_ok)
                    }
                    ComparisonRecipeV1::Latency => {
                        let effect = normalized_improvement_bps(
                            totals.baseline_elapsed_ms,
                            totals.candidate_elapsed_ms,
                            totals.baseline_verified,
                            totals.candidate_verified,
                        );
                        let token_ceiling = u128::from(totals.baseline_tokens)
                            * u128::from(totals.candidate_verified)
                            * u128::from(10_000 + specification.token_increase_ceiling_bps);
                        let candidate_tokens = u128::from(totals.candidate_tokens)
                            * u128::from(totals.baseline_verified)
                            * 10_000;
                        (effect, candidate_tokens <= token_ceiling)
                    }
                    ComparisonRecipeV1::DeterministicCorrection => unreachable!(),
                };
                objective_met &= constraint_met && effect.is_some_and(|value| value > 0);
                if let Some(effect) = effect {
                    observed_effects.push(effect);
                }
            }
        }
    }
    // The captured repetition/family minimum is necessary but not sufficient. Require the
    // smallest observed improvement to exceed the complete observed spread. This deterministic
    // lower margin rejects a broad claim supported only by a noisy point estimate.
    let measurements_complete = trials
        .iter()
        .all(|trial| trial.missing_measurements.is_empty());
    let sufficient_uncertainty = !broad_recipe
        || (specification.uncertainty_rule == ComparisonUncertaintyRuleV1::RepetitionDispersion
            && specification.repetitions >= 2
            && specification.minimum_families >= 2
            && families >= specification.minimum_families as usize
            && observed_effects.len()
                == families.saturating_mul(specification.repetitions as usize)
            && observed_effects
                .iter()
                .min()
                .zip(observed_effects.iter().max())
                .is_some_and(|(minimum, maximum)| *minimum > 0 && *minimum >= *maximum - *minimum));
    let (conclusion, reason) = if broad_recipe && !measurements_complete {
        (
            ComparisonConclusionV1::Inconclusive,
            "unsupported_measurements",
        )
    } else if !billing_complete {
        (ComparisonConclusionV1::Inconclusive, "partial_billing")
    } else if candidate_verified == 0
        || (baseline_verified == 0
            && specification.recipe != ComparisonRecipeV1::DeterministicCorrection)
    {
        (
            ComparisonConclusionV1::Inconclusive,
            "zero_verified_outcomes",
        )
    } else if !checks_pass || candidate_verified < baseline_verified || !family_quality {
        (ComparisonConclusionV1::Rejected, "negative_quality")
    } else if broad_recipe && !objective_met {
        match specification.recipe {
            ComparisonRecipeV1::TokensPerVerifiedOutcome => {
                (ComparisonConclusionV1::Rejected, "token_objective_not_met")
            }
            ComparisonRecipeV1::Latency => (
                ComparisonConclusionV1::Rejected,
                "latency_or_token_ceiling_not_met",
            ),
            ComparisonRecipeV1::DeterministicCorrection => unreachable!(),
        }
    } else if !sufficient_uncertainty {
        (ComparisonConclusionV1::Inconclusive, "insufficient_samples")
    } else {
        match specification.recipe {
            ComparisonRecipeV1::DeterministicCorrection => {
                let baseline_failed = trials
                    .iter()
                    .filter(|trial| trial.arm == ExperimentArmV1::Baseline)
                    .any(|trial| !trial.verified);
                if baseline_failed
                    && trials
                        .iter()
                        .filter(|trial| trial.arm == ExperimentArmV1::Candidate)
                        .all(|trial| trial.verified && trial.protected_checks_passed)
                {
                    (ComparisonConclusionV1::Accepted, "correction_verified")
                } else {
                    (
                        ComparisonConclusionV1::Inconclusive,
                        "correction_not_reproduced",
                    )
                }
            }
            ComparisonRecipeV1::TokensPerVerifiedOutcome => {
                if objective_met {
                    (ComparisonConclusionV1::Accepted, "token_objective_met")
                } else {
                    (ComparisonConclusionV1::Rejected, "token_objective_not_met")
                }
            }
            ComparisonRecipeV1::Latency => {
                if objective_met {
                    (ComparisonConclusionV1::Accepted, "latency_objective_met")
                } else {
                    (
                        ComparisonConclusionV1::Rejected,
                        "latency_or_token_ceiling_not_met",
                    )
                }
            }
        }
    };
    let comparison = ExperimentComparisonV1 {
        schema: "af.experiment-comparison/1".into(),
        specification_id: specification_id.into(),
        prepared_id: prepared_id.into(),
        conclusion,
        reason: reason.into(),
        baseline_verified,
        candidate_verified,
        baseline_tokens,
        candidate_tokens,
        baseline_elapsed_ms,
        candidate_elapsed_ms,
        trials,
    };
    comparison.validate()?;
    Ok(comparison)
}

/// Independent evaluator output. Delivery accepts it only when its envelope is produced by an
/// Attempt from the current Task and consumes the exact candidate, Requirements and comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationEvaluationV1 {
    pub schema: String,
    pub task_revision_id: String,
    pub requirements_id: String,
    pub candidate_snapshot_id: String,
    pub specification_id: String,
    pub prepared_id: String,
    pub comparison_id: String,
    pub conclusion: ComparisonConclusionV1,
    pub reason: String,
}

impl OptimizationEvaluationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-evaluation/1"
                && digests([
                    &self.task_revision_id,
                    &self.requirements_id,
                    &self.candidate_snapshot_id,
                    &self.specification_id,
                    &self.prepared_id,
                    &self.comparison_id,
                ])
                && is_name(&self.reason),
            "Optimization evaluation must bind the current Task and exact experiment evidence",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationProfileV1 {
    Analysis,
    Candidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationVerificationV1 {
    pub schema: String,
    pub profile: OptimizationProfileV1,
    pub source_snapshot_id: String,
    pub candidate_snapshot_id: String,
    pub requirements_id: String,
    pub harness_id: String,
    pub comparison_id: String,
    pub evaluation_id: String,
    pub package_repin_id: String,
    pub conclusion: ComparisonConclusionV1,
    pub deliverable: bool,
    pub protected_checks_passed: bool,
}

impl OptimizationVerificationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-verification/1"
                && digests([
                    &self.source_snapshot_id,
                    &self.candidate_snapshot_id,
                    &self.requirements_id,
                    &self.harness_id,
                    &self.comparison_id,
                    &self.evaluation_id,
                    &self.package_repin_id,
                ])
                && self.source_snapshot_id != self.candidate_snapshot_id
                && (!self.deliverable
                    || (self.profile == OptimizationProfileV1::Candidate
                        && self.conclusion == ComparisonConclusionV1::Accepted
                        && self.protected_checks_passed)),
            "Only a positive candidate profile with protected evidence is deliverable",
        )
    }
}
