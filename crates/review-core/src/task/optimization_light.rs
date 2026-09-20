//! Contracts for the bounded light optimizer.
//!
//! These contracts describe executable, installed recipes and the economics decision made from
//! a protected M2 comparison. They deliberately do not grant effects or make a delivery claim.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::optimization_experiment::ComparisonConclusionV1;
use super::usage::DecimalU64;
use super::{is_name, is_package_name, require};
use crate::is_digest;

pub const OPTIMIZATION_RECIPE_CATALOG_V1: &str = "af/OptimizationRecipeCatalog@1";
pub const OPTIMIZATION_PROFILE_V1: &str = "af/OptimizationProfile@1";
pub const OPTIMIZATION_DIAGNOSTIC_V1: &str = "af/OptimizationDiagnostic@1";
pub const OPTIMIZATION_PROPOSAL_V1: &str = "af/OptimizationProposal@1";
pub const OPTIMIZATION_RESULT_V1: &str = "af/OptimizationResult@1";
pub const OPTIMIZATION_ADOPTION_RECEIPT_V1: &str = "af/OptimizationAdoptionReceipt@1";
pub const OPTIMIZATION_ADOPTION_OBSERVATION_V1: &str = "af/OptimizationAdoptionObservation@1";
pub const OPTIMIZATION_ADOPTION_TASK_EVIDENCE_V1: &str = "af/OptimizationAdoptionTaskEvidence@1";
pub const OPTIMIZATION_WRITABLE_CONFIGURATION_V1: &str = "af/OptimizationWritableConfiguration@1";
pub const OPTIMIZATION_EXECUTION_CONFIGURATION_V1: &str = "af/OptimizationExecutionConfiguration@1";

fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value.contains('\0')
        && !value
            .chars()
            .any(|character| character.is_control() && character != '\n')
}

fn safe_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('/')
        && !value.contains('\\')
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".." | ".git"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationRecipeCapabilityV1 {
    Context,
    ProviderPromptCache,
    SandboxCache,
    ArtifactReuse,
    RetryFeedback,
    WorkerBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationRecipeSupportV1 {
    Installed,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationRecipeV1 {
    pub recipe_id: String,
    pub capability: OptimizationRecipeCapabilityV1,
    pub support: OptimizationRecipeSupportV1,
    pub applicability: BTreeSet<String>,
    pub required_observations: BTreeSet<String>,
    pub writable_effects: BTreeSet<String>,
    pub validation: BTreeSet<String>,
    pub invalidation: BTreeSet<String>,
    pub payoff_basis: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub upstream_work: Option<String>,
}

impl OptimizationRecipeV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.recipe_id)
                && !self.applicability.is_empty()
                && !self.required_observations.is_empty()
                && !self.validation.is_empty()
                && !self.invalidation.is_empty()
                && self
                    .applicability
                    .iter()
                    .chain(self.required_observations.iter())
                    .chain(self.writable_effects.iter())
                    .chain(self.validation.iter())
                    .chain(self.invalidation.iter())
                    .all(|value| is_name(value))
                && bounded(&self.payoff_basis, 4096)
                && match self.support {
                    OptimizationRecipeSupportV1::Installed => self.upstream_work.is_none(),
                    OptimizationRecipeSupportV1::Unsupported => self
                        .upstream_work
                        .as_deref()
                        .is_some_and(|value| bounded(value, 4096)),
                },
            "Optimization recipe must declare executable support, evidence, effects, validation, invalidation and payoff",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationRecipeCatalogV1 {
    pub schema: String,
    pub catalog_version: String,
    pub recipes: Vec<OptimizationRecipeV1>,
}

impl OptimizationRecipeCatalogV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-recipe-catalog/1"
                && is_digest(&self.catalog_version)
                && !self.recipes.is_empty()
                && self.recipes.len() <= 64,
            "Optimization recipe catalog requires exact bounded installed declarations",
        )?;
        let mut ids = BTreeSet::new();
        for recipe in &self.recipes {
            recipe.validate()?;
            require(
                ids.insert(&recipe.recipe_id),
                "Duplicate optimization recipe",
            )?;
        }
        Ok(())
    }

    pub fn recipe(&self, id: &str) -> Option<&OptimizationRecipeV1> {
        self.recipes.iter().find(|recipe| recipe.recipe_id == id)
    }
}

/// Data-only view supplied to diagnosis/proposal Workers. It has no SourceTree port and carries
/// aggregate development evidence only; case bodies and holdout labels are absent by contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationProfileV1 {
    pub schema: String,
    pub project_id: String,
    pub history_id: String,
    pub economics_id: String,
    pub recipe_catalog_id: String,
    pub development_set_id: String,
    pub holdout_set_id: String,
    pub development_families: BTreeSet<String>,
    pub eligible_recipe_ids: BTreeSet<String>,
    pub incomplete_observations: BTreeSet<String>,
    /// Captured project policy, not a Worker estimate. Zero means that no comparable future
    /// workload was authorized and therefore economics cannot offer adoption.
    pub comparable_future_runs: DecimalU64,
}

impl OptimizationProfileV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-profile/1"
                && [
                    &self.project_id,
                    &self.history_id,
                    &self.economics_id,
                    &self.recipe_catalog_id,
                    &self.development_set_id,
                    &self.holdout_set_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                && self.development_set_id != self.holdout_set_id
                && self.development_families.len() <= 100_000
                && self
                    .development_families
                    .iter()
                    .all(|family| bounded(family, 256))
                && self.eligible_recipe_ids.iter().all(|id| is_name(id))
                && self.incomplete_observations.iter().all(|id| is_name(id)),
            "Optimization profile must bind aggregate development evidence and sealed sets",
        )
    }
}

/// Bounded text configuration visible to the author Workers. This is deliberately separate
/// from SourceTree: only paths already granted by captured project policy are represented, and
/// protected case/oracle bytes cannot be smuggled through this view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationWritableFileV1 {
    pub content_id: String,
    pub text: String,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationWritableConfigurationV1 {
    pub schema: String,
    pub source_snapshot_id: String,
    pub allowed_paths: BTreeSet<String>,
    pub files: BTreeMap<String, OptimizationWritableFileV1>,
    pub omitted_paths: BTreeSet<String>,
    pub total_bytes: DecimalU64,
}

impl OptimizationWritableConfigurationV1 {
    pub fn validate(&self) -> Result<(), String> {
        let measured = self
            .files
            .values()
            .try_fold(0_u64, |total, file| {
                total.checked_add(file.text.len() as u64)
            })
            .ok_or("Writable configuration byte count overflow")?;
        require(
            self.schema == "af.optimization-writable-configuration/1"
                && is_digest(&self.source_snapshot_id)
                && !self.allowed_paths.is_empty()
                && self.allowed_paths.len() <= 128
                && self.allowed_paths.iter().all(|path| safe_path(path))
                && self.files.len() <= 128
                && measured <= 4 * 1024 * 1024
                && measured == self.total_bytes.get()
                && self.files.iter().all(|(path, file)| {
                    safe_path(path)
                        && is_digest(&file.content_id)
                        && file.text.len() <= 1024 * 1024
                        && !file.text.contains('\0')
                })
                && self.omitted_paths.len() <= 1024
                && self.omitted_paths.iter().all(|path| safe_path(path))
                && self.files.keys().all(|path| {
                    self.allowed_paths.iter().any(|root| {
                        path == root
                            || path
                                .strip_prefix(root)
                                .is_some_and(|rest| rest.starts_with('/'))
                    })
                })
                && self
                    .files
                    .keys()
                    .all(|path| !self.omitted_paths.contains(path)),
            "Writable optimization configuration must be a bounded policy-selected text view",
        )
    }
}

/// Exact package instructions delivered to one experimental arm. The fixed experimental Worker
/// remains plan-bound; this typed input is the installed context hook that makes a measured
/// package-instruction edit part of the separately approved invocation closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationExecutionConfigurationV1 {
    pub schema: String,
    pub recipe_id: String,
    pub original_package_id: String,
    pub original_package_digest: String,
    pub source_snapshot_id: String,
    pub candidate_snapshot_id: String,
    pub repin_id: String,
    pub package: String,
    pub package_digest: String,
    pub instructions_id: String,
    pub instructions: String,
}

impl OptimizationExecutionConfigurationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-execution-configuration/1"
                && is_name(&self.recipe_id)
                && is_digest(&self.original_package_id)
                && is_digest(&self.original_package_digest)
                && is_digest(&self.source_snapshot_id)
                && is_digest(&self.candidate_snapshot_id)
                && self.source_snapshot_id != self.candidate_snapshot_id
                && is_digest(&self.repin_id)
                && is_package_name(&self.package)
                && is_digest(&self.package_digest)
                && self.original_package_digest != self.package_digest
                && is_digest(&self.instructions_id)
                && bounded(&self.instructions, 256 * 1024),
            "Experimental execution configuration must bind bounded captured package instructions",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationDiagnosticV1 {
    pub schema: String,
    pub profile_id: String,
    pub selected_recipe_id: String,
    pub avoidable_cost: String,
    pub evidence: BTreeSet<String>,
    pub expected_comparable_workload: DecimalU64,
}

impl OptimizationDiagnosticV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-diagnostic/1"
                && is_digest(&self.profile_id)
                && is_name(&self.selected_recipe_id)
                && bounded(&self.avoidable_cost, 16_384)
                && !self.evidence.is_empty()
                && self.evidence.len() <= 128
                && self.evidence.iter().all(|value| bounded(value, 4096)),
            "Optimization diagnostic must select one evidence-backed bounded recipe",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationCandidateEditV1 {
    pub text: String,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationExpectedEconomicsV1 {
    pub comparable_future_runs: DecimalU64,
    pub gross_token_savings_per_run: DecimalU64,
    pub gross_time_savings_ms_per_run: DecimalU64,
    pub recurring_tokens_per_run: DecimalU64,
    pub recurring_time_ms_per_run: DecimalU64,
    pub maximum_validation_tokens: DecimalU64,
    pub maximum_validation_time_ms: DecimalU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationCandidateBindingV1 {
    pub package: String,
    pub provider_kind: String,
    pub model: String,
    pub effort: String,
}

impl OptimizationCandidateBindingV1 {
    fn validate(&self) -> Result<(), String> {
        require(
            self.package.split('/').all(is_name)
                && is_name(&self.provider_kind)
                && bounded(&self.model, 256)
                && is_name(&self.effort),
            "Candidate binding must name one predeclared compatible package/model/effort",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationProposalV1 {
    pub schema: String,
    pub profile_id: String,
    pub diagnostic_id: String,
    pub recipe_catalog_id: String,
    pub recipe_id: String,
    pub hypothesis: String,
    pub edits: BTreeMap<String, OptimizationCandidateEditV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub candidate_binding: Option<OptimizationCandidateBindingV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub objective_exception: Option<String>,
    pub expected: OptimizationExpectedEconomicsV1,
}

impl OptimizationProposalV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-proposal/1"
                && [
                    &self.profile_id,
                    &self.diagnostic_id,
                    &self.recipe_catalog_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                && is_name(&self.recipe_id)
                && bounded(&self.hypothesis, 16_384)
                && !self.edits.is_empty()
                && self.edits.len() <= 64
                && self.edits.iter().all(|(path, edit)| {
                    safe_path(path) && edit.text.len() <= 1024 * 1024 && !edit.text.contains('\0')
                }),
            "Optimization proposal must contain one bounded executable candidate",
        )?;
        if let Some(binding) = &self.candidate_binding {
            binding.validate()?;
        }
        require(
            self.objective_exception
                .as_deref()
                .is_none_or(|value| matches!(value, "correctness" | "latency")),
            "Objective exception must explicitly name correctness or latency",
        )?;
        Ok(())
    }

    pub fn validate_against(
        &self,
        profile_id: &str,
        diagnostic_id: &str,
        catalog: &OptimizationRecipeCatalogV1,
    ) -> Result<(), String> {
        self.validate()?;
        catalog.validate()?;
        let recipe = catalog
            .recipe(&self.recipe_id)
            .ok_or("Proposal selected an unregistered recipe")?;
        require(
            self.profile_id == profile_id
                && self.diagnostic_id == diagnostic_id
                && recipe.support == OptimizationRecipeSupportV1::Installed
                && !(recipe.capability == OptimizationRecipeCapabilityV1::WorkerBinding
                    && self.candidate_binding.is_none())
                && (recipe.capability == OptimizationRecipeCapabilityV1::WorkerBinding
                    || self.candidate_binding.is_none()),
            "Proposal changed its diagnosis or selected an unsupported/unbound recipe",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationCostV1 {
    pub tokens: u64,
    pub time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationRealizedEconomicsV1 {
    pub gross_token_savings_per_run: i64,
    pub gross_time_savings_ms_per_run: i64,
    pub recurring_tokens_per_run: DecimalU64,
    pub recurring_time_ms_per_run: DecimalU64,
    pub one_off_tokens: DecimalU64,
    pub one_off_time_ms: DecimalU64,
    /// Number of matched arm executions represented by each per-run value.
    pub normalization_units: DecimalU64,
    /// False when native billing, Attempt timestamps, or exact normalization is unavailable.
    pub accounting_complete: bool,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub missing_measurements: BTreeSet<String>,
    /// Distinguishes the non-overlapping host elapsed projection from summed Worker time.
    pub one_off_time_basis: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub token_break_even_runs: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub time_break_even_runs: Option<DecimalU64>,
}

impl OptimizationRealizedEconomicsV1 {
    pub fn calculate(
        baseline: OptimizationCostV1,
        candidate: OptimizationCostV1,
        recurring: OptimizationCostV1,
        one_off: OptimizationCostV1,
    ) -> Result<Self, String> {
        Self::calculate_normalized(baseline, candidate, recurring, one_off, 1)
    }

    pub fn calculate_normalized(
        baseline: OptimizationCostV1,
        candidate: OptimizationCostV1,
        recurring: OptimizationCostV1,
        one_off: OptimizationCostV1,
        normalization_units: u64,
    ) -> Result<Self, String> {
        require(
            normalization_units > 0,
            "Optimization economics need matched trial units",
        )?;
        let token_net = baseline.tokens as i128 - candidate.tokens as i128;
        let time_net = baseline.time_ms as i128 - candidate.time_ms as i128;
        let divisor = i128::from(normalization_units);
        require(
            token_net % divisor == 0 && time_net % divisor == 0,
            "Optimization per-run economics are not exactly representable",
        )?;
        let token_net = token_net / divisor;
        let time_net = time_net / divisor;
        let token_net = i64::try_from(token_net)
            .map_err(|_| "Optimization token delta exceeds its exact signed wire domain")?;
        let time_net = i64::try_from(time_net)
            .map_err(|_| "Optimization time delta exceeds its exact signed wire domain")?;
        let token_net_after_recurring = i128::from(token_net) - recurring.tokens as i128;
        let time_net_after_recurring = i128::from(time_net) - recurring.time_ms as i128;
        let break_even = |cost: u64, saving: i128| {
            (saving > 0).then(|| DecimalU64::from(u128::from(cost).div_ceil(saving as u128) as u64))
        };
        Ok(Self {
            gross_token_savings_per_run: token_net,
            gross_time_savings_ms_per_run: time_net,
            recurring_tokens_per_run: recurring.tokens.into(),
            recurring_time_ms_per_run: recurring.time_ms.into(),
            one_off_tokens: one_off.tokens.into(),
            one_off_time_ms: one_off.time_ms.into(),
            normalization_units: normalization_units.into(),
            accounting_complete: true,
            missing_measurements: BTreeSet::new(),
            one_off_time_basis: "attempt_interval_union".into(),
            token_break_even_runs: break_even(one_off.tokens, token_net_after_recurring),
            time_break_even_runs: break_even(one_off.time_ms, time_net_after_recurring),
        })
    }

    pub fn pays_back(&self, future_runs: u64) -> bool {
        self.accounting_complete
            && self.missing_measurements.is_empty()
            && self.one_off_time_basis == "attempt_interval_union"
            && self
                .token_break_even_runs
                .is_some_and(|runs| runs.get() <= future_runs)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationResultConclusionV1 {
    Validated,
    Rejected,
    Inconclusive,
    RecommendationOnly,
    NoChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationResultV1 {
    pub schema: String,
    pub source_snapshot_id: String,
    pub candidate_snapshot_id: String,
    pub profile_id: String,
    pub proposal_id: String,
    pub comparison_id: String,
    pub evaluation_id: String,
    pub verification_id: String,
    pub conclusion: OptimizationResultConclusionV1,
    pub experiment_conclusion: ComparisonConclusionV1,
    pub economics: OptimizationRealizedEconomicsV1,
    pub expected_comparable_workload: DecimalU64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub objective_exception: Option<String>,
    pub adoption_offered: bool,
    pub live_demonstrations: String,
}

impl OptimizationResultV1 {
    pub fn validate(&self) -> Result<(), String> {
        let validated = self.conclusion == OptimizationResultConclusionV1::Validated;
        require(
            self.schema == "af.optimization-result/1"
                && [
                    &self.source_snapshot_id,
                    &self.candidate_snapshot_id,
                    &self.profile_id,
                    &self.proposal_id,
                    &self.comparison_id,
                    &self.evaluation_id,
                    &self.verification_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                && self.source_snapshot_id != self.candidate_snapshot_id
                && self.live_demonstrations == "pending"
                && self.economics.normalization_units.get() > 0
                && self.economics.one_off_time_basis == "attempt_interval_union"
                && self
                    .objective_exception
                    .as_deref()
                    .is_none_or(|value| matches!(value, "correctness" | "latency"))
                && (self.adoption_offered
                    == (validated
                        && self.experiment_conclusion == ComparisonConclusionV1::Accepted
                        && self.expected_comparable_workload.get() > 0
                        && self.economics.accounting_complete
                        && self.economics.missing_measurements.is_empty()
                        && (self
                            .economics
                            .pays_back(self.expected_comparable_workload.get())
                            || self.objective_exception.is_some()))),
            "Optimization result may offer adoption only for verified evidence with a reachable break-even",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationAdoptionReceiptV1 {
    pub schema: String,
    pub task_id: String,
    pub result_id: String,
    pub source_snapshot_id: String,
    pub delivered_snapshot_id: String,
    pub delivery_record_id: String,
    pub delivered_tree_id: String,
}

impl OptimizationAdoptionReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-adoption-receipt/1"
                && is_name(&self.task_id)
                && [
                    &self.result_id,
                    &self.source_snapshot_id,
                    &self.delivered_snapshot_id,
                    &self.delivery_record_id,
                    &self.delivered_tree_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                && self.source_snapshot_id != self.delivered_snapshot_id,
            "Adoption receipt must bind the exact verified local delivery",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdoptionEquivalenceV1 {
    Equivalent,
    Edited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationAdoptionObservationV1 {
    pub schema: String,
    pub adoption_receipt_id: String,
    pub commit_snapshot_id: String,
    pub commit_tree_id: String,
    pub equivalence: AdoptionEquivalenceV1,
    pub observed_unix_ms: DecimalU64,
    pub workload_id: String,
    pub model_id: String,
    pub engine_id: String,
    pub environment_id: String,
    pub causal_claim: bool,
}

impl OptimizationAdoptionObservationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.optimization-adoption-observation/1"
                && [
                    &self.adoption_receipt_id,
                    &self.commit_snapshot_id,
                    &self.commit_tree_id,
                    &self.workload_id,
                    &self.model_id,
                    &self.engine_id,
                    &self.environment_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                && !self.causal_claim,
            "Adoption follow-up is identity-bound observational evidence, never causal proof",
        )
    }
}

/// Immutable common-Task evidence linked to a later adoption observation. The owner-supplied
/// observation labels remain attestations; these fields retain the identities actually recorded
/// by the later Task without claiming the optimization caused its outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationAdoptionTaskEvidenceV1 {
    pub schema: String,
    pub adoption_receipt_id: String,
    pub commit_snapshot_id: String,
    pub observed_task_id: String,
    pub observed_task_revision_id: String,
    pub observed_task_result_id: String,
    pub observed_plan_id: String,
    pub outcome: String,
    pub attempt_ids: Vec<String>,
    pub unsuccessful_attempt_ids: Vec<String>,
    pub usage_ids: Vec<String>,
    pub runtime_evidence_ids: Vec<String>,
    pub binding_ids: Vec<String>,
    pub engine_id: String,
    pub environment_id: String,
    pub missing_fields: BTreeSet<String>,
    pub causal_claim: bool,
}

impl OptimizationAdoptionTaskEvidenceV1 {
    pub fn validate(&self) -> Result<(), String> {
        let digests = [
            &self.adoption_receipt_id,
            &self.commit_snapshot_id,
            &self.observed_task_revision_id,
            &self.observed_task_result_id,
            &self.observed_plan_id,
            &self.engine_id,
            &self.environment_id,
        ]
        .into_iter()
        .chain(self.usage_ids.iter())
        .chain(self.runtime_evidence_ids.iter())
        .chain(self.binding_ids.iter());
        require(
            self.schema == "af.optimization-adoption-task-evidence/1"
                && !self.observed_task_id.trim().is_empty()
                && self.observed_task_id.len() <= 256
                && bounded(&self.outcome, 256)
                && digests.into_iter().all(|id| is_digest(id))
                && self.attempt_ids.len() <= 10_000
                && self.unsuccessful_attempt_ids.len() <= self.attempt_ids.len()
                && self.usage_ids.len() <= self.attempt_ids.len()
                && self.runtime_evidence_ids.len() <= 10_000
                && self.binding_ids.len() <= 1_000
                && self.missing_fields.len() <= 1_000
                && self
                    .attempt_ids
                    .iter()
                    .chain(self.unsuccessful_attempt_ids.iter())
                    .all(|id| !id.trim().is_empty() && id.len() <= 256)
                && self.missing_fields.iter().all(|field| is_name(field))
                && !self.causal_claim,
            "Adoption Task evidence requires exact immutable receipts and cannot make a causal claim",
        )?;
        require(
            self.attempt_ids.windows(2).all(|pair| pair[0] < pair[1])
                && self
                    .unsuccessful_attempt_ids
                    .windows(2)
                    .all(|pair| pair[0] < pair[1])
                && self.usage_ids.windows(2).all(|pair| pair[0] < pair[1])
                && self
                    .runtime_evidence_ids
                    .windows(2)
                    .all(|pair| pair[0] < pair[1])
                && self.binding_ids.windows(2).all(|pair| pair[0] < pair[1])
                && self
                    .unsuccessful_attempt_ids
                    .iter()
                    .all(|id| self.attempt_ids.binary_search(id).is_ok()),
            "Adoption Task evidence identity lists must be unique, sorted and internally consistent",
        )
    }
}
