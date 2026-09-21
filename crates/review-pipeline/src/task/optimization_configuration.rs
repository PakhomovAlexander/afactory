//! Deterministic, captured-source preparation for the candidate optimization Pipeline.
//! Proposals supply text edits only. Protected policy and oracle bytes come from S0.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use review_core::task::ArtifactInputV1;
use review_core::task::optimization_experiment::*;
use review_core::task::optimization_light::{
    OPTIMIZATION_EXECUTION_CONFIGURATION_V1, OptimizationDiagnosticV1,
    OptimizationExecutionConfigurationV1, OptimizationProfileV1, OptimizationProposalV1,
    OptimizationRecipeCapabilityV1, OptimizationRecipeV1, OptimizationWritableConfigurationV1,
    OptimizationWritableFileV1,
};
use review_core::{PortCardinality, Producer};
use review_source_git::task::{SourceTree, capture_snapshot, read_snapshot};
use review_source_git::{Entry, EntryKind, Manifest, decode_path, encode_path};
use review_store::Cas;
use serde::{Deserialize, Serialize};

use super::optimization_producers::{
    HarnessFixtureRequest, finalize_configuration, materialize_harness_fixture,
};

pub const CONFIGURATION: &str = "af/OptimizationConfiguration@1";
pub const POLICY_PATH: &str = ".af/optimization-policy.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateEdit {
    pub text: String,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateProposal {
    pub schema: String,
    pub edits: BTreeMap<String, CandidateEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationPolicy {
    pub schema: String,
    pub writable_paths: BTreeSet<String>,
    pub checks: BTreeMap<String, String>,
    /// An explicitly named mutable harness uses two derived fixed-fixture Snapshots.
    pub harness_path: Option<String>,
    /// The complete comparison design is project authority captured before any arm runs.
    pub experiment: ConfigurationExperimentPolicy,
    /// Predeclared economics authority for a light run. Worker output may describe expected
    /// economics, but only these captured limits can authorize delivery.
    #[serde(default)]
    pub light_economics: Option<LightEconomicsPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightEconomicsPolicy {
    pub comparable_future_runs: u64,
    pub recurring_tokens_per_run: u64,
    pub recurring_time_ms_per_run: u64,
    pub maximum_one_off_tokens: u64,
    pub maximum_one_off_time_ms: u64,
    #[serde(default)]
    pub objective_exception: Option<String>,
}

fn validate_light_economics(policy: &LightEconomicsPolicy) -> Result<(), String> {
    if policy.comparable_future_runs == 0
        || policy
            .objective_exception
            .as_deref()
            .is_some_and(|value| !matches!(value, "correctness" | "latency"))
    {
        return Err("Light economics policy has invalid workload or objective exception".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationExperimentCase {
    pub family: String,
    pub membership: String,
    pub input_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationExperimentPolicy {
    pub recipe: ComparisonRecipeV1,
    pub uncertainty_rule: ComparisonUncertaintyRuleV1,
    pub repetitions: u32,
    pub minimum_families: u32,
    pub token_increase_ceiling_bps: u32,
    pub cases: Vec<ConfigurationExperimentCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub schema: String,
    pub task_revision_id: String,
    pub source_snapshot_id: String,
    pub candidate_snapshot_id: String,
    pub baseline_fixture_id: String,
    pub candidate_fixture_id: String,
    pub requirements_id: String,
    pub harness_id: String,
    pub repin_id: String,
    pub materialization_id: Option<String>,
    pub engine_id: String,
    pub experiment: ConfigurationExperimentPolicy,
    pub case_inputs: BTreeMap<String, String>,
    /// Read-only compatibility for configurations produced by the rejected data-input design.
    /// New configurations never attach instructions as an arm business input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_execution_configuration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_execution_configuration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_cache_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_recipe_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_invalidation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_economics: Option<LightEconomicsPolicy>,
}

fn beneath(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'))
}

fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|p| !matches!(p, "" | "." | ".." | ".git"))
}

pub fn read_configuration(cas: &Cas, id: &str) -> Result<Configuration, String> {
    let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
    if envelope.artifact_type != CONFIGURATION {
        return Err("Expected installed OptimizationConfiguration".into());
    }
    let value: Configuration =
        serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
    if value.schema != "af.optimization-configuration/1"
        || [
            &value.task_revision_id,
            &value.source_snapshot_id,
            &value.candidate_snapshot_id,
            &value.baseline_fixture_id,
            &value.candidate_fixture_id,
            &value.requirements_id,
            &value.harness_id,
            &value.repin_id,
            &value.engine_id,
        ]
        .iter()
        .any(|id| !review_core::is_digest(id))
    {
        return Err("Optimization configuration has invalid captured identities".into());
    }
    validate_experiment_policy(&value.experiment)?;
    if let Some(policy) = &value.light_economics {
        validate_light_economics(policy)?;
    }
    if value.case_inputs.keys().cloned().collect::<BTreeSet<_>>()
        != value
            .experiment
            .cases
            .iter()
            .map(|case| case.family.clone())
            .collect::<BTreeSet<_>>()
        || value
            .case_inputs
            .values()
            .any(|id| !review_core::is_digest(id))
        || value
            .sandbox_cache_kind
            .as_deref()
            .is_some_and(|kind| kind != "cargo")
        || value
            .materialization_id
            .as_ref()
            .is_some_and(|id| !review_core::is_digest(id))
        || value
            .light_recipe_id
            .as_ref()
            .is_some_and(|id| !review_core::task::is_name(id))
        || value
            .light_invalidation_id
            .as_ref()
            .is_some_and(|id| !review_core::is_digest(id))
        || value.light_recipe_id.is_some() != value.light_invalidation_id.is_some()
        || value
            .baseline_execution_configuration_id
            .iter()
            .chain(value.candidate_execution_configuration_id.iter())
            .any(|id| !review_core::is_digest(id))
    {
        return Err("Optimization configuration has invalid captured case inputs".into());
    }
    Ok(value)
}

fn validate_experiment_policy(policy: &ConfigurationExperimentPolicy) -> Result<(), String> {
    let mut families = BTreeSet::new();
    let holdout_families = policy
        .cases
        .iter()
        .filter(|case| case.membership == "holdout")
        .map(|case| case.family.as_str())
        .collect::<BTreeSet<_>>();
    if policy.cases.is_empty()
        || policy.cases.len() > 2_048
        || policy.repetitions == 0
        || policy.repetitions > 256
        || policy.minimum_families == 0
        || policy.token_increase_ceiling_bps > 100_000
        || !matches!(
            (policy.recipe, policy.uncertainty_rule),
            (
                ComparisonRecipeV1::DeterministicCorrection,
                ComparisonUncertaintyRuleV1::Deterministic
            ) | (
                ComparisonRecipeV1::TokensPerVerifiedOutcome | ComparisonRecipeV1::Latency,
                ComparisonUncertaintyRuleV1::RepetitionDispersion
            )
        )
        || policy.cases.iter().any(|case| {
            !review_core::task::is_name(&case.family)
                || !safe(&case.input_path)
                || !matches!(case.membership.as_str(), "development" | "holdout")
                || !families.insert(case.family.clone())
        })
        || holdout_families.is_empty()
        || holdout_families.len() < policy.minimum_families as usize
    {
        return Err("Optimization experiment policy has invalid frozen cases or bounds".into());
    }
    Ok(())
}

/// Read only frozen case membership for the data-only light profile. Case bytes and paths do not
/// cross this boundary, so diagnosis/proposal Workers cannot inspect protected bodies.
pub fn configuration_case_families(
    cas: &Cas,
    source: &str,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let (_, manifest) = read_snapshot(cas, source)?;
    let entry = manifest
        .entries
        .iter()
        .find(|entry| decode_path(&entry.path) == POLICY_PATH.as_bytes())
        .ok_or("Light optimization requires captured .af/optimization-policy.json")?;
    if entry.kind != EntryKind::File {
        return Err("Optimization policy must be a regular captured file".into());
    }
    let policy: ConfigurationPolicy = serde_json::from_slice(
        &cas.get_bounded(&entry.content, 1024 * 1024)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    validate_experiment_policy(&policy.experiment)?;
    let mut development = BTreeSet::new();
    let mut holdout = BTreeSet::new();
    for case in policy.experiment.cases {
        if case.membership == "development" {
            development.insert(case.family);
        } else {
            holdout.insert(case.family);
        }
    }
    Ok((development, holdout))
}

/// Read only the predeclared economics limits. This does not expose source or protected case
/// bytes to author Workers.
pub fn configuration_light_economics(
    cas: &Cas,
    source: &str,
) -> Result<Option<LightEconomicsPolicy>, String> {
    let (_, manifest) = read_snapshot(cas, source)?;
    let entry = manifest
        .entries
        .iter()
        .find(|entry| decode_path(&entry.path) == POLICY_PATH.as_bytes())
        .ok_or("Light optimization requires captured .af/optimization-policy.json")?;
    let policy: ConfigurationPolicy = serde_json::from_slice(
        &cas.get_bounded(&entry.content, 1024 * 1024)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if let Some(economics) = &policy.light_economics {
        validate_light_economics(economics)?;
    }
    Ok(policy.light_economics)
}

/// Capture only project-authorized editable text for diagnosis/proposal Workers. Protected case
/// bodies, checks and policy bytes are absent even if a malformed policy tries to overlap them;
/// the same overlap is rejected again by candidate preparation.
pub fn writable_configuration_view(
    cas: &Cas,
    source: &str,
) -> Result<OptimizationWritableConfigurationV1, String> {
    let (_, manifest) = read_snapshot(cas, source)?;
    let policy_entry = manifest
        .entries
        .iter()
        .find(|entry| decode_path(&entry.path) == POLICY_PATH.as_bytes())
        .ok_or("Light optimization requires captured .af/optimization-policy.json")?;
    if policy_entry.kind != EntryKind::File {
        return Err("Optimization policy must be a regular captured file".into());
    }
    let policy: ConfigurationPolicy = serde_json::from_slice(
        &cas.get_bounded(&policy_entry.content, 1024 * 1024)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if policy.schema != "af.optimization-configuration-policy/1"
        || policy.writable_paths.is_empty()
        || policy.writable_paths.len() > 128
        || !policy.writable_paths.iter().all(|path| safe(path))
    {
        return Err("Invalid bounded writable optimization policy".into());
    }
    let mut protected = policy
        .checks
        .values()
        .chain(policy.experiment.cases.iter().map(|case| &case.input_path))
        .cloned()
        .collect::<BTreeSet<_>>();
    protected.insert(POLICY_PATH.into());
    if policy.writable_paths.iter().any(|writable| {
        protected
            .iter()
            .any(|path| beneath(path, writable) || beneath(writable, path))
    }) {
        return Err("Writable optimization paths overlap protected policy, checks or cases".into());
    }
    let mut files = BTreeMap::new();
    let mut omitted_paths = BTreeSet::new();
    let mut total = 0_u64;
    for entry in &manifest.entries {
        let path = String::from_utf8(decode_path(&entry.path))
            .map_err(|_| "Writable optimization paths must be UTF-8")?;
        if !policy
            .writable_paths
            .iter()
            .any(|root| beneath(&path, root))
        {
            continue;
        }
        if !matches!(entry.kind, EntryKind::File | EntryKind::Executable) {
            omitted_paths.insert(path);
            continue;
        }
        if entry.size > 1024 * 1024 || files.len() == 128 {
            omitted_paths.insert(path);
            continue;
        }
        let data = cas
            .get_bounded(&entry.content, 1024 * 1024)
            .map_err(|error| error.to_string())?;
        let text = match String::from_utf8(data) {
            Ok(text) if !text.contains('\0') => text,
            _ => {
                omitted_paths.insert(path);
                continue;
            }
        };
        let next = total
            .checked_add(text.len() as u64)
            .ok_or("Writable configuration byte count overflow")?;
        if next > 4 * 1024 * 1024 {
            omitted_paths.insert(path);
            continue;
        }
        total = next;
        files.insert(
            path,
            OptimizationWritableFileV1 {
                content_id: entry.content.clone(),
                text,
                executable: entry.kind == EntryKind::Executable,
            },
        );
    }
    let value = OptimizationWritableConfigurationV1 {
        schema: "af.optimization-writable-configuration/1".into(),
        source_snapshot_id: source.into(),
        allowed_paths: policy.writable_paths,
        files,
        omitted_paths,
        total_bytes: total.into(),
    };
    value.validate()?;
    Ok(value)
}

pub fn source_port(
    cas: &Cas,
    snapshot: &str,
    producer: Producer,
    mut refs: Vec<String>,
) -> Result<ArtifactInputV1, String> {
    read_snapshot(cas, snapshot)?;
    refs.push(snapshot.into());
    let id = cas
        .put_artifact(
            "af/SourceTree@1",
            producer,
            refs,
            Some(snapshot.into()),
            serde_json::to_value(SourceTree {
                snapshot_id: snapshot.into(),
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    Ok(ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: "af/SourceTree@1".into(),
        cardinality: PortCardinality::One,
        snapshot_id: Some(snapshot.into()),
    })
}

/// Prepare one candidate with a conservative oracle closure: all captured files outside the
/// explicitly writable roots are protected, including policy, check code and tool configuration.
#[allow(clippy::too_many_arguments)]
pub fn prepare_configuration_with_proposal(
    cas: &Cas,
    source: &str,
    requirements: &str,
    engine: &str,
    environment: &str,
    task_revision: &str,
    producer: Producer,
    proposal_override: Option<CandidateProposal>,
) -> Result<Configuration, String> {
    let (snapshot, manifest) = read_snapshot(cas, source)?;
    let policy_entry = manifest
        .entries
        .iter()
        .find(|e| decode_path(&e.path) == POLICY_PATH.as_bytes())
        .ok_or("Candidate optimization requires captured .af/optimization-policy.json")?;
    if policy_entry.kind != EntryKind::File {
        return Err("Optimization policy must be a regular captured file".into());
    }
    let policy: ConfigurationPolicy = serde_json::from_slice(
        &cas.get_bounded(&policy_entry.content, 1024 * 1024)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if policy.schema != "af.optimization-configuration-policy/1"
        || policy.writable_paths.is_empty()
        || policy.checks.is_empty()
        || !policy.writable_paths.iter().all(|p| safe(p))
        || !policy
            .checks
            .iter()
            .all(|(name, p)| review_core::task::is_name(name) && safe(p))
        || policy
            .writable_paths
            .iter()
            .any(|p| beneath(POLICY_PATH, p))
    {
        return Err("Invalid protected optimization policy".into());
    }
    validate_experiment_policy(&policy.experiment)?;
    if let Some(economics) = &policy.light_economics {
        validate_light_economics(economics)?;
    }
    let mut protected = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for entry in &manifest.entries {
        let path = String::from_utf8(decode_path(&entry.path))
            .map_err(|_| "Non-UTF8 optimization path")?;
        if !safe(&path) || entry.kind == EntryKind::Symlink {
            return Err("Optimization configuration refuses path aliases and symlinks".into());
        }
        if !policy
            .writable_paths
            .iter()
            .any(|root| beneath(&path, root))
        {
            protected.insert(path.clone());
        }
        paths.insert(path);
    }
    if !policy
        .checks
        .values()
        .all(|p| protected.contains(p) && paths.contains(p))
    {
        return Err(
            "Every protected check must be present outside candidate-writable paths".into(),
        );
    }
    let mut case_inputs = BTreeMap::new();
    let mut distinct_inputs = BTreeSet::new();
    for case in &policy.experiment.cases {
        if !protected.contains(&case.input_path) {
            return Err("Experimental case inputs must be protected captured files".into());
        }
        let entry = manifest
            .entries
            .iter()
            .find(|entry| decode_path(&entry.path) == case.input_path.as_bytes())
            .ok_or("Missing captured experimental case input")?;
        if entry.kind != EntryKind::File {
            return Err("Case input must be a regular file".into());
        }
        let data: serde_json::Value = serde_json::from_slice(
            &cas.get_bounded(&entry.content, 1024 * 1024)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let identity = review_store::content_id(&data).map_err(|e| e.to_string())?;
        if !distinct_inputs.insert(identity.clone()) {
            return Err("Different case families cannot relabel identical captured inputs".into());
        }
        let id = cas
            .put_artifact(
                "af/OptimizationCase@1",
                producer.clone(),
                vec![source.into(), entry.content.clone()],
                None,
                serde_json::json!({"schema":"af.optimization-case/1", "family":case.family,
                "membership":case.membership, "input_path":case.input_path,
                "input_identity":identity, "data":data}),
            )
            .map_err(|e| e.to_string())?
            .0;
        case_inputs.insert(case.family.clone(), id);
    }
    let harness = OptimizationHarnessV1 {
        schema: "af.optimization-harness/1".into(),
        oracle_id: policy_entry.content.clone(),
        checks: policy.checks.into_keys().collect(),
        transitive_dependencies: protected,
        candidate_writable_paths: policy.writable_paths,
        cache_read_scopes: BTreeSet::new(),
        cache_write_scopes: BTreeSet::new(),
    };
    harness.validate()?;
    let harness_id = cas
        .put_artifact(
            OPTIMIZATION_HARNESS_V1,
            producer.clone(),
            vec![source.into(), policy_entry.content.clone()],
            None,
            serde_json::to_value(&harness).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    let requirement = cas.get_artifact(requirements).map_err(|e| e.to_string())?;
    if requirement.artifact_type != "af/Requirements@1" {
        return Err("Candidate preparation requires captured Requirements".into());
    }
    let proposal: CandidateProposal = match proposal_override {
        Some(proposal) => proposal,
        None => serde_json::from_value(
            requirement
                .payload
                .pointer("/specification/candidate")
                .cloned()
                .ok_or("Requirements have no concrete candidate edits")?,
        )
        .map_err(|e| e.to_string())?,
    };
    if proposal.schema != "af.optimization-candidate/1"
        || proposal.edits.is_empty()
        || proposal.edits.len() > 64
        || proposal.edits.values().map(|e| e.text.len()).sum::<usize>() > 4 * 1024 * 1024
    {
        return Err("Candidate edits exceed the declared text configuration bounds".into());
    }
    let mut entries = manifest.entries.clone();
    for (path, edit) in proposal.edits {
        if !safe(&path)
            || !harness
                .candidate_writable_paths
                .iter()
                .any(|root| beneath(&path, root))
        {
            return Err("Candidate edit lies outside captured writable paths".into());
        }
        let content = cas.put(edit.text.as_bytes()).map_err(|e| e.to_string())?;
        let entry = Entry {
            path: encode_path(path.as_bytes()),
            kind: if edit.executable {
                EntryKind::Executable
            } else {
                EntryKind::File
            },
            content,
            size: edit.text.len() as u64,
        };
        entries.retain(|old| decode_path(&old.path) != path.as_bytes());
        entries.push(entry);
    }
    let candidate_manifest = Manifest::new(entries).map_err(|e| e.to_string())?;
    let proposed = capture_snapshot(cas, &candidate_manifest, &snapshot.origin_id, Some(source))?;
    let finalized = finalize_configuration(
        cas,
        source,
        &proposed,
        &harness_id,
        engine,
        producer.clone(),
    )?;
    let (baseline_fixture_id, candidate_fixture_id, materialization_id) =
        if let Some(path) = policy.harness_path {
            let id = materialize_harness_fixture(
                cas,
                &HarnessFixtureRequest {
                    fixture_snapshot_id: source,
                    product_source_id: source,
                    configuration_source_id: source,
                    product_candidate_id: &finalized.snapshot_id,
                    harness_path: &path,
                    requirements_id: requirements,
                    harness_id: &harness_id,
                    engine_id: engine,
                    environment_id: environment,
                },
                producer,
            )?;
            let envelope = cas.get_artifact(&id).map_err(|e| e.to_string())?;
            let receipt: HarnessMaterializationV1 =
                serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
            (
                receipt.baseline_derived_snapshot_id,
                receipt.candidate_derived_snapshot_id,
                Some(id),
            )
        } else {
            (source.into(), finalized.snapshot_id.clone(), None)
        };
    Ok(Configuration {
        schema: "af.optimization-configuration/1".into(),
        task_revision_id: task_revision.into(),
        source_snapshot_id: source.into(),
        candidate_snapshot_id: finalized.snapshot_id,
        baseline_fixture_id,
        candidate_fixture_id,
        requirements_id: requirements.into(),
        harness_id,
        repin_id: finalized.repin_id,
        materialization_id,
        engine_id: engine.into(),
        experiment: policy.experiment,
        case_inputs,
        baseline_execution_configuration_id: None,
        candidate_execution_configuration_id: None,
        sandbox_cache_kind: None,
        light_recipe_id: None,
        light_invalidation_id: None,
        light_economics: policy.light_economics,
    })
}

/// Admit a generated light proposal only when its selected installed recipe constrains the
/// concrete edit and the protected experiment that will measure it. The resulting invalidation
/// identity travels with the candidate configuration and changes with source, engine, policy or
/// the recipe's declared invalidation dimensions.
#[allow(clippy::too_many_arguments)]
pub fn prepare_light_configuration_with_proposal(
    cas: &Cas,
    source: &str,
    requirements: &str,
    engine: &str,
    environment: &str,
    task_revision: &str,
    candidate_package_id: &str,
    candidate_package_digest: &str,
    producer: Producer,
    profile: &OptimizationProfileV1,
    diagnostic: &OptimizationDiagnosticV1,
    recipe: &OptimizationRecipeV1,
    proposal: &OptimizationProposalV1,
) -> Result<Configuration, String> {
    if diagnostic.selected_recipe_id != recipe.recipe_id
        || proposal.recipe_id != recipe.recipe_id
        || !profile.eligible_recipe_ids.contains(&recipe.recipe_id)
        || proposal.expected.comparable_future_runs != diagnostic.expected_comparable_workload
        || proposal.candidate_binding.is_some()
    {
        return Err(
            "Light proposal changed its selected recipe, workload or executable binding".into(),
        );
    }
    let expected_shape = match recipe.capability {
        OptimizationRecipeCapabilityV1::Context => (
            BTreeSet::from(["context_tokens".into(), "retrieval_tokens".into()]),
            BTreeSet::from(["project_configuration".into()]),
            BTreeSet::from([
                "matched_protected_trials".into(),
                "required_acceptance".into(),
            ]),
            BTreeSet::from(["source_policy_worker_package".into()]),
        ),
        OptimizationRecipeCapabilityV1::ArtifactReuse => (
            BTreeSet::from(["artifact_identity".into(), "authority_identity".into()]),
            BTreeSet::from(["project_configuration".into()]),
            BTreeSet::from([
                "fresh_integrity_check".into(),
                "required_verification".into(),
            ]),
            BTreeSet::from(["source_authority_toolchain_policy".into()]),
        ),
        OptimizationRecipeCapabilityV1::SandboxCache => (
            BTreeSet::from(["cache_measurements".into()]),
            BTreeSet::from(["sandbox_local_cache".into()]),
            BTreeSet::from([
                "cold_warm_matched_trials".into(),
                "required_acceptance".into(),
            ]),
            BTreeSet::from(["source_toolchain_policy".into()]),
        ),
        _ => return Err("Selected light recipe has no installed preparation hook".into()),
    };
    if recipe.required_observations != expected_shape.0
        || recipe.writable_effects != expected_shape.1
        || recipe.validation != expected_shape.2
        || recipe.invalidation != expected_shape.3
    {
        return Err("Installed light recipe declaration differs from its preparation hook".into());
    }
    let allowed = |path: &str| match recipe.capability {
        OptimizationRecipeCapabilityV1::Context => {
            path.starts_with(".af/") && path.ends_with("/instructions.md")
        }
        OptimizationRecipeCapabilityV1::ArtifactReuse => path.starts_with(".af/artifact-reuse/"),
        OptimizationRecipeCapabilityV1::SandboxCache => path.starts_with(".af/cache/"),
        _ => false,
    };
    if !proposal.edits.keys().all(|path| allowed(path)) {
        return Err("Light proposal edit lies outside the selected recipe effect roots".into());
    }
    let mut edits = proposal.edits.clone();
    if recipe.capability == OptimizationRecipeCapabilityV1::ArtifactReuse {
        let path = ".af/artifact-reuse/receipt.json";
        if edits.len() != 1 {
            return Err("Artifact reuse requires one exact receipt request".into());
        }
        let request = edits
            .get(path)
            .ok_or("Artifact reuse requires its exact receipt path")?;
        if request.executable {
            return Err("Artifact reuse requires its non-executable receipt path".into());
        }
        let request_value: serde_json::Value =
            serde_json::from_str(&request.text).map_err(|_| "Invalid artifact reuse request")?;
        if request_value
            != serde_json::json!({"schema":"af.artifact-reuse-request/1","reuse":"source_snapshot"})
        {
            return Err(
                "Artifact reuse request may select only the current captured source".into(),
            );
        }
        cas.verify(source).map_err(|error| error.to_string())?;
        let reuse_identity = review_store::content_id(&serde_json::json!([
            "af/artifact-reuse-receipt/1",
            source,
            engine,
            environment,
            recipe.recipe_id,
            recipe.invalidation,
        ]))
        .map_err(|error| error.to_string())?;
        let receipt = serde_json::json!({
            "schema":"af.artifact-reuse-receipt/1",
            "artifact_id":source,
            "engine_id":engine,
            "authority_policy_id":environment,
            "reuse_identity":reuse_identity,
            "integrity":"verified_from_cas",
            "verification":"required"
        });
        edits.get_mut(path).expect("exact request").text =
            serde_json::to_string(&receipt).map_err(|error| error.to_string())? + "\n";
    }
    let sandbox_cache_kind = if recipe.capability == OptimizationRecipeCapabilityV1::SandboxCache {
        let path = ".af/cache/cargo.json";
        if edits.len() != 1 {
            return Err("Sandbox cache recipe requires one admitted cache selection".into());
        }
        let request = edits
            .get(path)
            .ok_or("Sandbox cache recipe requires .af/cache/cargo.json")?;
        if request.executable
            || serde_json::from_str::<serde_json::Value>(&request.text)
                .map_err(|_| "Invalid sandbox cache selection")?
                != serde_json::json!({"schema":"af.sandbox-cache-selection/1","kind":"cargo"})
        {
            return Err("Sandbox cache recipe may select only the admitted Cargo snapshot".into());
        }
        Some("cargo".to_owned())
    } else {
        None
    };
    let candidate = CandidateProposal {
        schema: "af.optimization-candidate/1".into(),
        edits: edits
            .iter()
            .map(|(path, edit)| {
                (
                    path.clone(),
                    CandidateEdit {
                        text: edit.text.clone(),
                        executable: edit.executable,
                    },
                )
            })
            .collect(),
    };
    let mut configuration = prepare_configuration_with_proposal(
        cas,
        source,
        requirements,
        engine,
        environment,
        task_revision,
        producer.clone(),
        Some(candidate),
    )?;
    if recipe.capability == OptimizationRecipeCapabilityV1::SandboxCache
        && (!matches!(configuration.experiment.recipe, ComparisonRecipeV1::Latency)
            || !matches!(
                configuration.experiment.uncertainty_rule,
                ComparisonUncertaintyRuleV1::RepetitionDispersion
            )
            || configuration.experiment.repetitions < 2)
    {
        return Err(
            "Sandbox cache recipe requires repeated cold/warm latency comparison authority".into(),
        );
    }
    if recipe.capability == OptimizationRecipeCapabilityV1::Context {
        let envelope = cas
            .get_artifact(&configuration.repin_id)
            .map_err(|error| error.to_string())?;
        let repin: OptimizationPackageRepinV1 =
            serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
        repin.validate()?;
        if repin.entailed.is_empty() {
            return Err(
                "Context recipe did not repin any executable Worker or Pipeline package".into(),
            );
        }
        if repin.entailed.len() != 1 {
            return Err("Context recipe must change exactly one executable Worker package".into());
        }
        let (package, change) = repin.entailed.iter().next().expect("one repin");
        let (_, baseline_manifest) = read_snapshot(cas, source)?;
        let (_, candidate_manifest) = read_snapshot(cas, &configuration.candidate_snapshot_id)?;
        let catalog_entry = baseline_manifest
            .entries
            .iter()
            .find(|entry| decode_path(&entry.path) == b".af/task-catalog.toml")
            .ok_or("Context recipe source has no captured Task catalog")?;
        let catalog: toml::Value = toml::from_str(
            std::str::from_utf8(
                &cas.get_bounded(&catalog_entry.content, 4 * 1024 * 1024)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let root = catalog
            .get("packages")
            .and_then(|value| value.get(package.as_str()))
            .and_then(|value| value.get("path"))
            .and_then(toml::Value::as_str)
            .ok_or("Context recipe package has no captured local root")?;
        let instructions_path = format!("{root}/instructions.md");
        if edits.len() != 1 || !edits.contains_key(&instructions_path) {
            return Err(
                "Context recipe currently supports one exact captured instructions.md edit".into(),
            );
        }
        let read_instructions = |manifest: &Manifest| -> Result<(String, String), String> {
            let entry = manifest
                .entries
                .iter()
                .find(|entry| decode_path(&entry.path) == instructions_path.as_bytes())
                .ok_or("Context recipe package has no captured instructions.md")?;
            if entry.kind != EntryKind::File || entry.size > 256 * 1024 {
                return Err("Context recipe instructions exceed their text bound".into());
            }
            let text = String::from_utf8(
                cas.get_bounded(&entry.content, 256 * 1024)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|_| "Context recipe instructions must be UTF-8")?;
            Ok((entry.content.clone(), text))
        };
        let (baseline_instructions_id, baseline_instructions) =
            read_instructions(&baseline_manifest)?;
        let (candidate_instructions_id, candidate_instructions) =
            read_instructions(&candidate_manifest)?;
        if baseline_instructions_id == candidate_instructions_id {
            return Err("Context recipe did not change executable instruction bytes".into());
        }
        let publish_execution =
            |instructions_id: &str, instructions: String| -> Result<String, String> {
                let value = OptimizationExecutionConfigurationV1 {
                    schema: "af.optimization-execution-configuration/1".into(),
                    recipe_id: recipe.recipe_id.clone(),
                    original_package_id: candidate_package_id.into(),
                    original_package_digest: change.before.clone(),
                    source_snapshot_id: source.into(),
                    candidate_snapshot_id: configuration.candidate_snapshot_id.clone(),
                    repin_id: configuration.repin_id.clone(),
                    package: package.clone(),
                    package_digest: change.after.clone(),
                    instructions_id: instructions_id.into(),
                    instructions,
                };
                value.validate()?;
                cas.put_artifact(
                    OPTIMIZATION_EXECUTION_CONFIGURATION_V1,
                    producer.clone(),
                    vec![
                        source.into(),
                        configuration.candidate_snapshot_id.clone(),
                        configuration.repin_id.clone(),
                        candidate_package_id.into(),
                        instructions_id.into(),
                    ],
                    None,
                    serde_json::to_value(value).map_err(|error| error.to_string())?,
                )
                .map(|(id, _)| id)
                .map_err(|error| error.to_string())
            };
        if candidate_package_digest != change.before
            || review_store::canonical::blob_content_id(baseline_instructions.as_bytes())
                != baseline_instructions_id
        {
            return Err("Context recipe changed its captured candidate-slot package".into());
        }
        configuration.candidate_execution_configuration_id = Some(publish_execution(
            &candidate_instructions_id,
            candidate_instructions,
        )?);
    }
    let invalidation_id = review_store::content_id(&serde_json::json!([
        "af/optimization-recipe-invalidation/1",
        source,
        engine,
        environment,
        recipe.recipe_id,
        recipe.invalidation,
    ]))
    .map_err(|error| error.to_string())?;
    configuration.light_recipe_id = Some(recipe.recipe_id.clone());
    configuration.light_invalidation_id = Some(invalidation_id);
    configuration.sandbox_cache_kind = sandbox_cache_kind;
    Ok(configuration)
}

/// The historical product source and explicitly derived measurement fixture are separate
/// declared inputs. Only this installed environment substitutes the checked fixture for cwd.
type CacheSourceResolver = dyn Fn(review_sandbox::CacheKind) -> Result<review_sandbox::CacheSource, review_sandbox::CacheError>
    + Send
    + Sync;

pub struct OptimizationEnvironment {
    pub source: super::source::SnapshotTaskEnvironment,
    policy_id: String,
    cache_source_resolver: Option<Arc<CacheSourceResolver>>,
    prepared_caches: Mutex<BTreeMap<String, PreparedOptimizationCache>>,
}

#[derive(Debug, Clone)]
struct PreparedOptimizationCache {
    snapshot: Option<review_sandbox::CacheSnapshot>,
    started_unix_ms: u64,
    source_digest: String,
    invalidation_id: String,
    toolchain_id: Option<String>,
}

impl OptimizationEnvironment {
    pub fn new(source: super::source::SnapshotTaskEnvironment, policy_id: String) -> Self {
        Self {
            source,
            policy_id,
            cache_source_resolver: None,
            prepared_caches: Mutex::new(BTreeMap::new()),
        }
    }

    fn captured_toolchain(cas: &Cas, manifest: &Manifest) -> Result<Option<String>, String> {
        let declared_toolchain = manifest
            .entries
            .iter()
            .filter_map(|entry| {
                let path = decode_path(&entry.path);
                matches!(path.as_slice(), b"rust-toolchain" | b"rust-toolchain.toml").then(|| {
                    (
                        String::from_utf8_lossy(&path).into_owned(),
                        entry.content.clone(),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        (!declared_toolchain.is_empty())
            .then(|| {
                cas.put_json(&serde_json::json!({
                    "schema":"af.cargo-toolchain-identity/1",
                    "declarations":declared_toolchain,
                }))
                .map_err(|error| error.to_string())
            })
            .transpose()
    }

    fn captured_cache_selection(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
    ) -> Result<Option<(String, String, Option<String>)>, String> {
        let Some(source) = input.inputs.get("source") else {
            return Ok(None);
        };
        let (snapshot_id, _, manifest) = super::source::source_snapshot(cas, source)?;
        let selection = manifest
            .entries
            .iter()
            .find(|entry| decode_path(&entry.path) == b".af/cache/cargo.json");
        let Some(selection) = selection else {
            return Ok(None);
        };
        if selection.kind != EntryKind::File || selection.size > 4096 {
            return Err("Cargo cache selection must be one bounded regular file".into());
        }
        let bytes = cas
            .get_bounded(&selection.content, 4096)
            .map_err(|error| error.to_string())?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Cargo cache selection")?;
        if value != serde_json::json!({"schema":"af.sandbox-cache-selection/1","kind":"cargo"}) {
            return Err("Source may select only the admitted Cargo cache contract".into());
        }
        let toolchain_id = Self::captured_toolchain(cas, &manifest)?;
        Ok(Some((snapshot_id, selection.content.clone(), toolchain_id)))
    }

    pub fn with_cache_source_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(
                review_sandbox::CacheKind,
            ) -> Result<review_sandbox::CacheSource, review_sandbox::CacheError>
            + Send
            + Sync
            + 'static,
    {
        self.cache_source_resolver = Some(Arc::new(resolver));
        self
    }
}

impl super::host::TaskEnvironment for OptimizationEnvironment {
    fn kernel_outputs(
        &self,
        signature: &review_graph::task::OperatorSignature,
    ) -> BTreeSet<String> {
        self.source.kernel_outputs(signature)
    }

    fn validate_outputs(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        signature: &review_graph::task::OperatorSignature,
        output: &review_core::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        self.source.validate_outputs(cas, input, signature, output)
    }

    fn materialize(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        signature: &review_graph::task::OperatorSignature,
    ) -> Result<review_sandbox::Sandbox, String> {
        self.materialize_worker(cas, input, signature, true)
    }

    fn materialize_worker(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        signature: &review_graph::task::OperatorSignature,
        command_worker: bool,
    ) -> Result<review_sandbox::Sandbox, String> {
        if !input.inputs.contains_key("source") {
            if !signature.effects.is_empty() {
                return Err("Data-only optimizer Workers cannot declare sandbox effects".into());
            }
            let manifest = Manifest::new(vec![]).map_err(|error| error.to_string())?;
            let sandbox = review_sandbox::Sandbox::materialize(
                &manifest,
                cas,
                review_sandbox::Mode::ReadOnly,
            )
            .map_err(|error| error.to_string())?;
            review_sandbox::admit(self.source.policy, &sandbox)
                .map_err(|error| error.to_string())?;
            return Ok(sandbox);
        }
        let mut materialized = input.clone();
        if let Some(fixture) = input.inputs.get("fixture") {
            let config = read_configuration(
                cas,
                &input
                    .inputs
                    .get("configuration")
                    .ok_or("A measurement fixture needs its protected configuration")?
                    .artifact_ids[0],
            )?;
            let expected = if input.node.ends_with("baseline") {
                &config.baseline_fixture_id
            } else if input.node.ends_with("candidate") {
                &config.candidate_fixture_id
            } else {
                return Err("Fixture materialization belongs to an experimental arm".into());
            };
            if super::source::source_input(cas, fixture)? != *expected
                || super::source::source_input(cas, &input.inputs["source"])?
                    != config.source_snapshot_id
            {
                return Err("Experimental fixture changed captured source identities".into());
            }
            materialized.inputs.insert("source".into(), fixture.clone());
        }
        let cache_trial = input
            .inputs
            .get("configuration")
            .map(|port| read_configuration(cas, &port.artifact_ids[0]))
            .transpose()?
            .is_some_and(|configuration| {
                configuration.sandbox_cache_kind.as_deref() == Some("cargo")
            });
        let mut selection = self.captured_cache_selection(cas, &materialized)?;
        let cache_arm = input.node.ends_with("_baseline") || input.node.ends_with("_candidate");
        if cache_trial && !cache_arm {
            // The independent evaluator reads the candidate source but does not participate in
            // the measured cold/warm intervention. Ordinary later Tasks have no protected
            // optimization configuration input and therefore consume the delivered selection.
            selection = None;
        }
        if cache_trial && input.node.ends_with("_baseline") {
            selection = None;
        }
        if selection.is_some() && (!command_worker || signature.effects.contains("write-source")) {
            // The selection grants no ambient environment authority. Unsupported Worker kinds
            // run unchanged; a later ordinary non-writing command check may consume the cache.
            selection = None;
        }
        let sandbox = if selection.is_some() {
            self.source
                .materialize_preparation(cas, &materialized, signature)?
        } else {
            self.source.materialize(cas, &materialized, signature)?
        };
        if cache_trial && cache_arm {
            if let Some(configuration_port) = input.inputs.get("configuration") {
                let configuration = read_configuration(cas, &configuration_port.artifact_ids[0])?;
                if configuration.sandbox_cache_kind.as_deref() == Some("cargo") {
                    let started_unix_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_millis() as u64;
                    if input.node.ends_with("_baseline") {
                        let (_, _, manifest) = super::source::source_snapshot(
                            cas,
                            materialized
                                .inputs
                                .get("source")
                                .ok_or("Cache arm lacks source")?,
                        )?;
                        let toolchain_id = Self::captured_toolchain(cas, &manifest)?;
                        self.prepared_caches
                            .lock()
                            .expect("optimization caches")
                            .insert(
                                input.node.clone(),
                                PreparedOptimizationCache {
                                    snapshot: None,
                                    started_unix_ms,
                                    source_digest: configuration.source_snapshot_id.clone(),
                                    invalidation_id: configuration
                                        .light_invalidation_id
                                        .clone()
                                        .ok_or("Cache recipe has no invalidation identity")?,
                                    toolchain_id,
                                },
                            );
                        return Ok(sandbox);
                    }
                }
            }
        }
        if let Some((source_snapshot_id, selection_id, toolchain_id)) = selection {
            let resolver = self
                .cache_source_resolver
                .as_ref()
                .ok_or("Cargo cache selection has no administrator-approved cache mapping")?;
            let source =
                resolver(review_sandbox::CacheKind::Cargo).map_err(|error| error.to_string())?;
            let snapshot = review_sandbox::materialize_cache(&source, &sandbox, cas)
                .map_err(|error| error.to_string())?;
            let invalidation_id = review_store::content_id(&serde_json::json!([
                "af/cargo-cache-invalidation/1",
                source_snapshot_id,
                selection_id,
                toolchain_id,
                self.policy_id,
                snapshot.source_digest,
            ]))
            .map_err(|error| error.to_string())?;
            self.prepared_caches
                .lock()
                .expect("optimization caches")
                .insert(
                    input.node.clone(),
                    PreparedOptimizationCache {
                        started_unix_ms: snapshot.started_unix_ms,
                        source_digest: snapshot.source_digest.clone(),
                        snapshot: Some(snapshot),
                        invalidation_id,
                        toolchain_id,
                    },
                );
        }
        Ok(sandbox)
    }

    fn command_environment(
        &self,
        input: &review_core::task::execution::TaskInvocationV1,
        sandbox: &review_sandbox::Sandbox,
    ) -> Result<Vec<(String, String)>, String> {
        if self
            .prepared_caches
            .lock()
            .expect("optimization caches")
            .get(&input.node)
            .is_some_and(|prepared| prepared.snapshot.is_some())
        {
            Ok(review_sandbox::CacheKind::Cargo
                .environment(sandbox.root())
                .local)
        } else {
            Ok(Vec::new())
        }
    }

    fn finish(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        signature: &review_graph::task::OperatorSignature,
        attempt: &review_store::store::task::execution::PreparedTaskAttempt,
        sandbox: review_sandbox::Sandbox,
        outputs: BTreeMap<String, ArtifactInputV1>,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        if !input.inputs.contains_key("source") {
            if !sandbox
                .seal()
                .map_err(|error| error.to_string())?
                .unchanged()
            {
                return Err("Data-only optimizer Worker mutated its empty sandbox".into());
            }
            return Ok(outputs);
        }
        if input.inputs.contains_key("fixture") && signature.effects.contains("write-source") {
            return Err("Experimental checks cannot mutate measurement fixtures".into());
        }
        if self
            .prepared_caches
            .lock()
            .expect("optimization caches")
            .get(&input.node)
            .is_some_and(|prepared| prepared.snapshot.is_some())
        {
            review_sandbox::remove_materialized_caches(&sandbox)?;
        }
        self.source
            .finish(cas, input, signature, attempt, sandbox, outputs)
    }

    fn runtime_evidence(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        attempt: &review_store::store::task::execution::PreparedTaskAttempt,
    ) -> Result<Option<String>, String> {
        use review_core::task::runtime::*;
        let Some(prepared) = self
            .prepared_caches
            .lock()
            .expect("optimization caches")
            .remove(&input.node)
        else {
            return Ok(None);
        };
        let lookup_ms = prepared
            .snapshot
            .as_ref()
            .map_or(0, |value| value.lookup_ms);
        let materialization_ms = prepared
            .snapshot
            .as_ref()
            .map_or(0, |value| value.materialization_ms);
        let elapsed = lookup_ms.saturating_add(materialization_ms);
        let span_id = cas
            .put_json(&serde_json::json!([
                attempt.task_id(),
                attempt.id(),
                "dependency_preparation",
                prepared.started_unix_ms,
                elapsed,
            ]))
            .map_err(|error| error.to_string())?;
        let observation_id = cas
            .put_json(&serde_json::json!([
                attempt.task_id(),
                attempt.id(),
                prepared.invalidation_id.clone(),
                prepared.source_digest.clone(),
                lookup_ms,
                materialization_ms,
            ]))
            .map_err(|error| error.to_string())?;
        let evidence = TaskRuntimeEvidenceV1 {
            task_id: attempt.task_id().into(),
            attempt_id: attempt.id().into(),
            node: input.node.clone(),
            context_id: attempt.context_id().into(),
            spans: prepared
                .snapshot
                .is_some()
                .then_some(TaskRuntimeSpanV1 {
                    span_id: span_id.clone(),
                    kind: TaskRuntimeSpanKindV1::DependencyPreparation,
                    label: "cargo".into(),
                    started_unix_ms: prepared.started_unix_ms.max(1),
                    elapsed_ms: elapsed,
                })
                .into_iter()
                .collect(),
            caches: vec![TaskCacheObservationV1 {
                observation_id,
                kind: "cargo".into(),
                eligible: prepared.snapshot.is_some(),
                source_digest: prepared.source_digest.clone(),
                toolchain_id: prepared.toolchain_id.clone(),
                bytes_available: prepared.snapshot.as_ref().map_or(0, |value| value.bytes),
                lookup_ms,
                materialization_ms,
            }],
        };
        evidence.validate()?;
        cas.put_artifact(
            TASK_RUNTIME_EVIDENCE_V1,
            super::source::invocation_producer(cas, input, Some(attempt))?,
            vec![
                attempt.context_id().into(),
                span_id,
                prepared.invalidation_id.clone(),
                prepared.source_digest.clone(),
                prepared
                    .toolchain_id
                    .unwrap_or(prepared.invalidation_id.clone()),
            ],
            None,
            serde_json::to_value(evidence).map_err(|error| error.to_string())?,
        )
        .map(|(id, _)| Some(id))
        .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::host::TaskEnvironment;
    use review_core::task::execution::TaskInvocationV1;
    use review_core::task::pipeline::PipelineContractV1;
    use review_graph::task::OperatorSignature;
    use review_sandbox::{CacheKind, CacheLimits, CacheSource, Policy};
    use review_source_git::task::SOURCE_TREE_V1;
    use std::path::PathBuf;

    fn producer() -> Producer {
        Producer::KernelOperation {
            run_id: "ordinary-cache-test".into(),
            node_id: None,
            operation_id: "capture@1".into(),
        }
    }

    fn source_port(cas: &Cas, files: &[(&str, &[u8])]) -> ArtifactInputV1 {
        let origin = cas
            .put_json(&serde_json::json!({"origin":"fixture"}))
            .unwrap();
        let entries = files
            .iter()
            .map(|(path, bytes)| Entry {
                path: (*path).into(),
                kind: EntryKind::File,
                content: cas.put(bytes).unwrap(),
                size: bytes.len() as u64,
            })
            .collect();
        let snapshot_id =
            capture_snapshot(cas, &Manifest::new(entries).unwrap(), &origin, None).unwrap();
        let artifact_id = cas
            .put_artifact(
                SOURCE_TREE_V1,
                producer(),
                vec![snapshot_id.clone()],
                Some(snapshot_id.clone()),
                serde_json::to_value(SourceTree {
                    snapshot_id: snapshot_id.clone(),
                })
                .unwrap(),
            )
            .unwrap()
            .0;
        ArtifactInputV1 {
            artifact_ids: vec![artifact_id],
            artifact_type: SOURCE_TREE_V1.into(),
            cardinality: PortCardinality::One,
            snapshot_id: Some(snapshot_id),
        }
    }

    fn invocation(cas: &Cas, source: ArtifactInputV1, node: &str) -> TaskInvocationV1 {
        TaskInvocationV1 {
            plan_id: cas
                .put_json(&serde_json::json!({"plan":"fixture"}))
                .unwrap(),
            node: node.into(),
            inputs: BTreeMap::from([("source".into(), source)]),
        }
    }

    fn signature(effects: &[&str]) -> OperatorSignature {
        OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::new(),
                outputs: BTreeMap::new(),
            },
            effects: effects.iter().map(|value| (*value).into()).collect(),
            evidence: BTreeMap::new(),
            retains: BTreeMap::new(),
            roles: BTreeSet::new(),
            worker_input_type: None,
            worker_output_type: None,
            outcome_port: None,
            attempt: None,
        }
    }

    fn cache_source(path: PathBuf) -> CacheSource {
        CacheSource {
            kind: CacheKind::Cargo,
            source: path,
            limits: CacheLimits {
                max_bytes: 1024 * 1024,
                max_files: 100,
                max_copy_bytes: 1024 * 1024,
            },
        }
    }

    fn selected_source(cas: &Cas, toolchain: &[u8], extra: &[u8]) -> ArtifactInputV1 {
        source_port(
            cas,
            &[
                (
                    ".af/cache/cargo.json",
                    br#"{"schema":"af.sandbox-cache-selection/1","kind":"cargo"}"#,
                ),
                ("rust-toolchain.toml", toolchain),
                ("src.txt", extra),
            ],
        )
    }

    fn prepare(
        cas: &Cas,
        source: ArtifactInputV1,
        policy_id: &str,
        cache: &std::path::Path,
    ) -> PreparedOptimizationCache {
        let cache = cache_source(cache.to_path_buf());
        let environment = OptimizationEnvironment::new(
            super::super::source::SnapshotTaskEnvironment {
                policy: Policy::trusted_local(),
            },
            policy_id.into(),
        )
        .with_cache_source_resolver(move |_| Ok(cache.clone()));
        let input = invocation(cas, source, "root.nodes.ordinary_check");
        let sandbox = environment
            .materialize_worker(
                cas,
                &input,
                &signature(&["read-source", "execute-checks"]),
                true,
            )
            .unwrap();
        let vars = environment.command_environment(&input, &sandbox).unwrap();
        assert!(vars.contains(&("CARGO_NET_OFFLINE".into(), "true".into())));
        assert!(
            sandbox
                .root()
                .join(".af-cache/cargo/registry/cache/example.crate")
                .is_file()
        );
        environment
            .prepared_caches
            .lock()
            .unwrap()
            .get(&input.node)
            .unwrap()
            .clone()
    }

    #[test]
    fn ordinary_command_tasks_consume_only_the_exact_admitted_cache_selection() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let cache = directory.path().join("cache");
        let cached = cache.join("registry/cache/example.crate");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"captured crate").unwrap();
        let policy = cas.put_json(&serde_json::json!({"policy":"one"})).unwrap();
        let selected = selected_source(&cas, b"[toolchain]\nchannel='1.88.0'\n", b"one");
        let prepared = prepare(&cas, selected.clone(), &policy, &cache);
        assert!(prepared.snapshot.is_some());
        assert!(prepared.toolchain_id.is_some());
        assert_ne!(prepared.source_digest, prepared.invalidation_id);

        let environment = OptimizationEnvironment::new(
            super::super::source::SnapshotTaskEnvironment {
                policy: Policy::trusted_local(),
            },
            policy,
        );
        let input = invocation(&cas, selected, "root.nodes.ordinary_check");
        assert!(
            environment
                .materialize_worker(&cas, &input, &signature(&["read-source"]), true)
                .err()
                .unwrap()
                .contains("administrator-approved")
        );
        let sandbox = environment
            .materialize_worker(&cas, &input, &signature(&["read-source"]), false)
            .unwrap();
        assert!(
            environment
                .command_environment(&input, &sandbox)
                .unwrap()
                .is_empty()
        );

        let invalid = source_port(
            &cas,
            &[(
                ".af/cache/cargo.json",
                br#"{"schema":"af.sandbox-cache-selection/1","kind":"host"}"#,
            )],
        );
        let invalid = invocation(&cas, invalid, "root.nodes.invalid");
        assert!(
            environment
                .materialize_worker(&cas, &invalid, &signature(&["read-source"]), true)
                .err()
                .unwrap()
                .contains("only the admitted Cargo cache contract")
        );

        let absent = invocation(
            &cas,
            source_port(&cas, &[("src.txt", b"no selection")]),
            "root.nodes.absent",
        );
        let sandbox = environment
            .materialize_worker(&cas, &absent, &signature(&["read-source"]), true)
            .unwrap();
        assert!(
            environment
                .command_environment(&absent, &sandbox)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cache_identity_changes_with_source_toolchain_policy_and_cache_content() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let cache = directory.path().join("cache");
        let cached = cache.join("registry/cache/example.crate");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"first cache").unwrap();
        let p1 = cas.put_json(&serde_json::json!({"policy":"one"})).unwrap();
        let p2 = cas.put_json(&serde_json::json!({"policy":"two"})).unwrap();
        let s1 = selected_source(&cas, b"channel='1.88.0'\n", b"one");
        let s2 = selected_source(&cas, b"channel='1.88.0'\n", b"two");
        let s3 = selected_source(&cas, b"channel='1.89.0'\n", b"one");
        let first = prepare(&cas, s1.clone(), &p1, &cache);
        let changed_source = prepare(&cas, s2, &p1, &cache);
        let changed_toolchain = prepare(&cas, s3, &p1, &cache);
        let changed_policy = prepare(&cas, s1.clone(), &p2, &cache);
        std::fs::write(&cached, b"second cache").unwrap();
        let changed_cache = prepare(&cas, s1, &p1, &cache);
        for changed in [
            &changed_source,
            &changed_toolchain,
            &changed_policy,
            &changed_cache,
        ] {
            assert_ne!(first.invalidation_id, changed.invalidation_id);
        }
        assert_ne!(first.toolchain_id, changed_toolchain.toolchain_id);
        assert_ne!(first.source_digest, changed_cache.source_digest);
    }
}
