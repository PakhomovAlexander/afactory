//! Installed report-only optimizer domain. The M1 operator is a pure fold of captured history.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::optimization::*;
use review_core::task::optimization_experiment::*;
use review_core::task::optimization_light::OptimizationProfileV1 as OptimizationDevelopmentProfileV1;
use review_core::task::optimization_light::*;
use review_core::task::pipeline::*;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::{
    ArtifactInputV1, TaskAcceptanceV1, TaskExecutionV1, TaskResultV1, TaskRevisionV1,
};
use review_core::{PortCardinality, Producer};
use review_graph::task::{CompiledOperator, CompiledTask, OperatorSignature};
use review_graph::{NodeOutcome, RunReport};
use review_store::store::task::TaskProjection;
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::{Cas, optimization::project_economics};

use super::host::TaskDomain;
use super::optimization_configuration::{self as configuration};
use super::source::invocation_producer;
use super::{TaskExperimentInputs, TaskOperatorHost, TaskWorkOutput, envelope};

fn port(artifact_type: &str) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: artifact_type.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        covers: BTreeSet::new(),
    }
}

pub fn optimization_signatures(
    policy_id: &str,
) -> Result<BTreeMap<String, OperatorSignature>, String> {
    if !review_core::is_digest(policy_id) {
        return Err("Optimization policy has no exact identity".into());
    }
    let mut signatures = BTreeMap::from([
        (
            "operator/optimization-project".into(),
            OperatorSignature {
                contract: PipelineContractV1 {
                    inputs: BTreeMap::from([("history".into(), port(OPTIMIZATION_HISTORY_V1))]),
                    outputs: BTreeMap::from([
                        ("economics".into(), port(OPTIMIZATION_ECONOMICS_V1)),
                        ("report".into(), port(OPTIMIZATION_REPORT_V1)),
                    ]),
                },
                effects: BTreeSet::new(),
                evidence: BTreeMap::from([("report".into(), BTreeSet::from([policy_id.into()]))]),
                retains: BTreeMap::from([
                    ("economics".into(), BTreeSet::from(["history".into()])),
                    ("report".into(), BTreeSet::from(["history".into()])),
                ]),
                roles: BTreeSet::new(),
                worker_input_type: None,
                worker_output_type: None,
                attempt: None,
                outcome_port: None,
            },
        ),
        (
            "operator/optimization-profile".into(),
            OperatorSignature {
                contract: PipelineContractV1 {
                    inputs: BTreeMap::from([
                        ("history".into(), port(OPTIMIZATION_HISTORY_V1)),
                        ("source".into(), port("af/SourceTree@1")),
                    ]),
                    outputs: BTreeMap::from([
                        (
                            "configuration".into(),
                            port(OPTIMIZATION_WRITABLE_CONFIGURATION_V1),
                        ),
                        ("economics".into(), port(OPTIMIZATION_ECONOMICS_V1)),
                        ("profile".into(), port(OPTIMIZATION_PROFILE_V1)),
                        ("recipes".into(), port(OPTIMIZATION_RECIPE_CATALOG_V1)),
                    ]),
                },
                effects: BTreeSet::new(),
                evidence: BTreeMap::new(),
                retains: BTreeMap::from([
                    ("configuration".into(), BTreeSet::from(["source".into()])),
                    ("economics".into(), BTreeSet::from(["history".into()])),
                    (
                        "profile".into(),
                        BTreeSet::from(["history".into(), "source".into()]),
                    ),
                    ("recipes".into(), BTreeSet::new()),
                ]),
                roles: BTreeSet::new(),
                worker_input_type: None,
                worker_output_type: None,
                attempt: None,
                outcome_port: None,
            },
        ),
        (
            "operator/optimization-experiment".into(),
            OperatorSignature {
                contract: PipelineContractV1 {
                    inputs: BTreeMap::from([
                        ("history".into(), port(OPTIMIZATION_HISTORY_V1)),
                        ("requirements".into(), port("af/Requirements@1")),
                        ("source".into(), port("af/SourceTree@1")),
                    ]),
                    outputs: BTreeMap::from([(
                        "comparison".into(),
                        port(EXPERIMENT_COMPARISON_V1),
                    )]),
                },
                effects: BTreeSet::new(),
                evidence: BTreeMap::from([(
                    "comparison".into(),
                    BTreeSet::from([policy_id.into()]),
                )]),
                retains: BTreeMap::from([(
                    "comparison".into(),
                    BTreeSet::from(["history".into(), "requirements".into(), "source".into()]),
                )]),
                roles: BTreeSet::new(),
                worker_input_type: None,
                worker_output_type: None,
                attempt: None,
                outcome_port: None,
            },
        ),
    ]);
    let mut source = port("af/SourceTree@1");
    source.affinity = PortAffinityV1::Unbound {};
    for (name, inputs, outputs) in [
        (
            "operator/optimization-prepare",
            BTreeMap::from([
                ("source".into(), source.clone()),
                ("requirements".into(), port("af/Requirements@1")),
            ]),
            BTreeMap::from([
                ("configuration".into(), port(configuration::CONFIGURATION)),
                (
                    "candidate".into(),
                    PipelinePortV1 {
                        affinity: PortAffinityV1::DerivedFrom {
                            input: "source".into(),
                        },
                        ..source.clone()
                    },
                ),
            ]),
        ),
        (
            "operator/optimization-finalize",
            BTreeMap::from([
                ("source".into(), source.clone()),
                ("candidate".into(), source.clone()),
                ("configuration".into(), port(configuration::CONFIGURATION)),
                ("comparison".into(), port(EXPERIMENT_COMPARISON_V1)),
                ("evaluation".into(), port(OPTIMIZATION_EVALUATION_V1)),
                ("profile".into(), port(OPTIMIZATION_PROFILE_V1)),
                ("diagnostic".into(), port(OPTIMIZATION_DIAGNOSTIC_V1)),
                ("proposal".into(), port(OPTIMIZATION_PROPOSAL_V1)),
            ]),
            BTreeMap::from([
                (
                    "verification".into(),
                    PipelinePortV1 {
                        affinity: PortAffinityV1::SameAs {
                            input: "candidate".into(),
                        },
                        ..port(OPTIMIZATION_VERIFICATION_V1)
                    },
                ),
                (
                    "snapshot".into(),
                    PipelinePortV1 {
                        affinity: PortAffinityV1::SameAs {
                            input: "candidate".into(),
                        },
                        ..source.clone()
                    },
                ),
                ("result".into(), port(OPTIMIZATION_RESULT_V1)),
            ]),
        ),
    ] {
        let inputs: BTreeMap<String, PipelinePortV1> = inputs;
        let outputs: BTreeMap<String, PipelinePortV1> = outputs;
        let retains = outputs
            .keys()
            .map(|output| (output.clone(), inputs.keys().cloned().collect()))
            .collect();
        let evidence = if name.ends_with("finalize") {
            BTreeMap::from([("verification".into(), BTreeSet::from([policy_id.into()]))])
        } else {
            BTreeMap::new()
        };
        signatures.insert(
            name.into(),
            OperatorSignature {
                contract: PipelineContractV1 { inputs, outputs },
                effects: BTreeSet::new(),
                evidence,
                retains,
                roles: BTreeSet::new(),
                worker_input_type: None,
                worker_output_type: None,
                attempt: None,
                outcome_port: None,
            },
        );
    }
    // A prepared configuration is optional only for the existing non-deliverable checkpoint.
    let mut config = port(configuration::CONFIGURATION);
    config.optional = true;
    signatures
        .get_mut("operator/optimization-experiment")
        .unwrap()
        .contract
        .inputs
        .insert("configuration".into(), config);
    let prepare = signatures
        .get_mut("operator/optimization-prepare")
        .expect("installed prepare signature");
    for (name, ty) in [
        ("profile", OPTIMIZATION_PROFILE_V1),
        ("recipes", OPTIMIZATION_RECIPE_CATALOG_V1),
        ("diagnostic", OPTIMIZATION_DIAGNOSTIC_V1),
        ("proposal", OPTIMIZATION_PROPOSAL_V1),
    ] {
        let mut input = port(ty);
        input.optional = true;
        prepare.contract.inputs.insert(name.into(), input);
    }
    let finalize = signatures
        .get_mut("operator/optimization-finalize")
        .expect("installed finalize signature");
    for name in ["profile", "diagnostic", "proposal"] {
        finalize
            .contract
            .inputs
            .get_mut(name)
            .expect("installed light finalize input")
            .optional = true;
    }
    finalize
        .contract
        .outputs
        .get_mut("result")
        .expect("installed light finalize output")
        .optional = true;
    Ok(signatures)
}

fn input_id<'a>(input: &'a TaskInvocationV1, port_name: &str, ty: &str) -> Result<&'a str, String> {
    let port = input
        .inputs
        .get(port_name)
        .ok_or_else(|| format!("Optimization operation lacks {port_name}"))?;
    port.validate()?;
    if port.artifact_type != ty
        || port.cardinality != PortCardinality::One
        || port.snapshot_id.is_some()
    {
        return Err(format!(
            "Optimization input {port_name} changed its contract"
        ));
    }
    Ok(&port.artifact_ids[0])
}

fn read<T: serde::de::DeserializeOwned>(cas: &Cas, id: &str, ty: &str) -> Result<T, String> {
    let artifact = envelope(cas, id)?;
    if artifact.artifact_type != ty
        || (artifact.subject_snapshot_id.is_some() && ty != OPTIMIZATION_VERIFICATION_V1)
    {
        return Err(format!("Expected data-only {ty}"));
    }
    serde_json::from_value(artifact.payload).map_err(|error| error.to_string())
}

fn history_chain(cas: &Cas, head: &str) -> Result<Vec<(String, OptimizationHistoryV1)>, String> {
    let mut reversed = Vec::new();
    let mut current = Some(head.to_owned());
    let mut seen = BTreeSet::new();
    while let Some(id) = current {
        if reversed.len() >= 10_000 || !seen.insert(id.clone()) {
            return Err("Optimization history chain is cyclic or exceeds retention bounds".into());
        }
        let history: OptimizationHistoryV1 = read(cas, &id, OPTIMIZATION_HISTORY_V1)?;
        history.validate()?;
        current = history.previous_capture_id.clone();
        reversed.push((id, history));
    }
    reversed.reverse();
    Ok(reversed)
}

fn report(economics_id: &str, economics: &OptimizationEconomicsV1) -> OptimizationReportV1 {
    let mut highlights = vec![format!(
        "AF charged {} tokens across {} execution records; outer-session charged usage is {} and is not added to that total.",
        economics.af_usage.chargeable_tokens.get(),
        economics.rows.len(),
        economics.outer_session_usage.chargeable_tokens.get()
    )];
    highlights.push(format!(
        "Elapsed {} ms, active {} ms, and summed work {} ms are reported separately because spans may overlap.",
        economics.elapsed_ms.get(), economics.active_ms.get(), economics.summed_work_ms.get()
    ));
    if let (Some(context), Some(retrieval), Some(repeated)) = (
        economics.context_tokens,
        economics.retrieval_tokens,
        economics.repeated_context_tokens,
    ) {
        highlights.push(format!(
            "Observed context volume is {} tokens, including {} retrieved and {} repeated-context tokens.",
            context.get(), retrieval.get(), repeated.get()
        ));
    }
    if !economics.cache_economics.is_empty() {
        let (hits, misses, unknown) = economics.cache_economics.values().fold(
            (0u64, 0u64, 0u64),
            |(hits, misses, unknown), cache| {
                (
                    hits.saturating_add(cache.hits),
                    misses.saturating_add(cache.misses),
                    unknown.saturating_add(cache.unknown_results),
                )
            },
        );
        highlights.push(format!(
            "Observed caches recorded {hits} hits, {misses} misses, and {unknown} unknown results; reuse and timing totals remain unknown when any contributing receipt omitted them."
        ));
    }
    if economics.repeated_failures > 0 {
        highlights.push(format!(
            "{} repeated failure occurrences remain visible after source-range deduplication.",
            economics.repeated_failures
        ));
    }
    OptimizationReportV1 {
        schema: "af.optimization-report/1".into(),
        economics_id: economics_id.into(),
        project_id: economics.project_id.clone(),
        status: if economics.missing_fields.is_empty() {
            OptimizationReportStatusV1::Complete
        } else {
            OptimizationReportStatusV1::Partial
        },
        summary: format!(
            "{} verified and {} failed or incomplete executions were reconstructed from {} retained captures.",
            economics.verified,
            economics.failed_or_incomplete,
            economics.capture_ids.len()
        ),
        highlights,
        missing_measurements: economics.missing_fields.iter().cloned().collect(),
    }
}

fn light_recipe_catalog() -> Result<OptimizationRecipeCatalogV1, String> {
    let installed = |recipe_id: &str,
                     capability: OptimizationRecipeCapabilityV1,
                     applicability: &[&str],
                     observations: &[&str],
                     effects: &[&str],
                     validation: &[&str],
                     invalidation: &[&str],
                     payoff: &str| OptimizationRecipeV1 {
        recipe_id: recipe_id.into(),
        capability,
        support: OptimizationRecipeSupportV1::Installed,
        applicability: applicability.iter().map(|value| (*value).into()).collect(),
        required_observations: observations.iter().map(|value| (*value).into()).collect(),
        writable_effects: effects.iter().map(|value| (*value).into()).collect(),
        validation: validation.iter().map(|value| (*value).into()).collect(),
        invalidation: invalidation.iter().map(|value| (*value).into()).collect(),
        payoff_basis: payoff.into(),
        upstream_work: None,
    };
    let unsupported = |recipe_id: &str, capability: OptimizationRecipeCapabilityV1, work: &str| {
        OptimizationRecipeV1 {
            recipe_id: recipe_id.into(),
            capability,
            support: OptimizationRecipeSupportV1::Unsupported,
            applicability: BTreeSet::from(["provider_binding_present".into()]),
            required_observations: BTreeSet::from(["native_provider_receipt".into()]),
            writable_effects: BTreeSet::new(),
            validation: BTreeSet::from(["matched_protected_trials".into()]),
            invalidation: BTreeSet::from(["provider_model_policy".into()]),
            payoff_basis: "No savings claim is available until the missing installed hook exists."
                .into(),
            upstream_work: Some(work.into()),
        }
    };
    let recipes = vec![
        installed(
            "context_retrieval_dedup",
            OptimizationRecipeCapabilityV1::Context,
            &["repeated_context_observed"],
            &["context_tokens", "retrieval_tokens"],
            &["project_configuration"],
            &["matched_protected_trials", "required_acceptance"],
            &["source_policy_worker_package"],
            "Gross removed context/retrieval tokens per verified outcome, less recurring setup.",
        ),
        unsupported(
            "provider_prompt_prefix",
            OptimizationRecipeCapabilityV1::ProviderPromptCache,
            "The installed Worker runner exposes provider/model/effort but no explicit prompt-cache control; add a provider-admitted setting and native hit receipt before enabling this recipe.",
        ),
        installed(
            "sandbox_dependency_cache",
            OptimizationRecipeCapabilityV1::SandboxCache,
            &["cargo_dependency_work"],
            &["cache_measurements"],
            &["sandbox_local_cache"],
            &["cold_warm_matched_trials", "required_acceptance"],
            &["source_toolchain_policy"],
            "Measured warm dependency preparation time less population, lookup, copy and optimizer setup cost.",
        ),
        unsupported(
            "deterministic_artifact_reuse",
            OptimizationRecipeCapabilityV1::ArtifactReuse,
            "A verified-current receipt is not an execution consumer. Install a reachable CAS reuse operation with fresh authority and mandatory verification before enabling this recipe.",
        ),
        unsupported(
            "targeted_retry_feedback",
            OptimizationRecipeCapabilityV1::RetryFeedback,
            "The common runtime persists admitted retry feedback, but light preparation has no recipe hook that changes the compatible Worker retry input; install that binding before enabling this recipe.",
        ),
        unsupported(
            "worker_binding_tuning",
            OptimizationRecipeCapabilityV1::WorkerBinding,
            "Predeclare compatible baseline/candidate Worker presets and bind each experiment arm to its exact provider/model/effort before enabling this recipe.",
        ),
    ];
    let catalog_version = review_store::content_id(
        &serde_json::to_value(&recipes).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let catalog = OptimizationRecipeCatalogV1 {
        schema: "af.optimization-recipe-catalog/1".into(),
        catalog_version,
        recipes,
    };
    catalog.validate()?;
    Ok(catalog)
}

fn interval_union_ms(mut intervals: Vec<(u64, u64)>) -> Result<u64, String> {
    intervals.sort_unstable();
    let mut total = 0u64;
    let mut current: Option<(u64, u64)> = None;
    for (start, end) in intervals {
        if end < start {
            return Err("Optimizer Attempt timestamps run backwards".into());
        }
        match current {
            None => current = Some((start, end)),
            Some((left, right)) if start <= right => current = Some((left, right.max(end))),
            Some((left, right)) => {
                total = total
                    .checked_add(right - left)
                    .ok_or("Optimizer elapsed interval union overflow")?;
                current = Some((start, end));
            }
        }
    }
    if let Some((left, right)) = current {
        total = total
            .checked_add(right - left)
            .ok_or("Optimizer elapsed interval union overflow")?;
    }
    Ok(total)
}

fn comparison_normalization_units(comparison: &ExperimentComparisonV1) -> Result<u64, String> {
    let baseline = comparison
        .trials
        .iter()
        .filter(|trial| trial.arm == ExperimentArmV1::Baseline)
        .count();
    let candidate = comparison
        .trials
        .iter()
        .filter(|trial| trial.arm == ExperimentArmV1::Candidate)
        .count();
    if baseline == 0 || baseline != candidate {
        return Err("Optimization economics require matched arm units".into());
    }
    u64::try_from(baseline).map_err(|_| "Optimization trial count overflow".into())
}

fn artifact_port(
    cas: &Cas,
    input: &TaskInvocationV1,
    ty: &str,
    value: &impl serde::Serialize,
    refs: Vec<String>,
) -> Result<ArtifactInputV1, String> {
    let id = cas
        .put_artifact(
            ty,
            invocation_producer(cas, input, None)?,
            refs,
            None,
            serde_json::to_value(value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
        .0;
    Ok(ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        snapshot_id: None,
    })
}

pub struct OptimizationTaskDomain {
    graph: CompiledTask,
}

impl OptimizationTaskDomain {
    pub fn captured(policy_id: &str, graph: CompiledTask) -> Result<Self, String> {
        let installed = optimization_signatures(policy_id)?;
        let mut projectors = 0usize;
        for node in graph.nodes.values() {
            if let CompiledOperator::Primitive {
                operator,
                signature,
            } = &node.operator
            {
                if !matches!(operator, TaskOperatorV1::OptimizationProject {}) {
                    return Err(
                        "Report-only Optimization plans cannot dispatch Workers or other domain operations"
                            .into(),
                    );
                }
                projectors += 1;
                if installed
                    .get(signature)
                    .is_none_or(|expected| expected.contract != node.contract)
                {
                    return Err("Optimization operator differs from its installed contract".into());
                }
            }
        }
        if projectors != 1 {
            return Err("Report-only Optimization requires exactly one installed projector".into());
        }
        Ok(Self { graph })
    }

    fn operator(&self, input: &TaskInvocationV1) -> Result<&TaskOperatorV1, String> {
        match &self
            .graph
            .nodes
            .get(&input.node)
            .ok_or("Unknown Optimization operator")?
            .operator
        {
            CompiledOperator::Primitive { operator, .. } => Ok(operator),
            _ => Err("Optimization node is not a primitive operator".into()),
        }
    }

    fn outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        if !matches!(
            self.operator(input)?,
            TaskOperatorV1::OptimizationProject {}
        ) {
            return Err("Unsupported Optimization operator".into());
        }
        let history_id = input_id(input, "history", OPTIMIZATION_HISTORY_V1)?;
        let economics = project_economics(&history_chain(cas, history_id)?)?;
        let economics_port = artifact_port(
            cas,
            input,
            OPTIMIZATION_ECONOMICS_V1,
            &economics,
            economics.capture_ids.clone(),
        )?;
        let economics_id = economics_port.artifact_ids[0].clone();
        let report = report(&economics_id, &economics);
        report.validate()?;
        let report_port = artifact_port(
            cas,
            input,
            OPTIMIZATION_REPORT_V1,
            &report,
            vec![economics_id, history_id.into()],
        )?;
        Ok(BTreeMap::from([
            ("economics".into(), economics_port),
            ("report".into(), report_port),
        ]))
    }

    fn assess(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        let report = result.outputs.get("report");
        let economics = result.outputs.get("economics");
        let outputs_complete = report
            .is_some_and(|port| port.artifact_type == OPTIMIZATION_REPORT_V1)
            && economics.is_some_and(|port| port.artifact_type == OPTIMIZATION_ECONOMICS_V1);
        let required_source_gap = match report {
            Some(port) if port.artifact_ids.len() == 1 => {
                let report: OptimizationReportV1 =
                    read(cas, &port.artifact_ids[0], OPTIMIZATION_REPORT_V1)?;
                report.validate()?;
                report.missing_measurements.iter().any(|missing| {
                    missing.starts_with("unavailable_") || missing.starts_with("partial_")
                })
            }
            _ => true,
        };
        let accepted = outputs_complete && !required_source_gap;
        result.acceptance = if accepted {
            TaskAcceptanceV1::Satisfied
        } else {
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = if accepted {
            "report_ready"
        } else {
            "history_incomplete"
        }
        .into();
        result.missing_obligations = if accepted {
            BTreeSet::new()
        } else {
            task.acceptance.keys().cloned().collect()
        };
        result.evidence = report
            .into_iter()
            .flat_map(|port| port.artifact_ids.iter().cloned())
            .collect();
        Ok(())
    }
}

impl TaskOperatorHost for OptimizationTaskDomain {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        if !feedback.is_empty() {
            return Err(
                "Deterministic Optimization projection does not consume retry feedback".into(),
            );
        }
        self.operator(input)?;
        cas.put_artifact(
            "af/OptimizationContext@1",
            invocation_producer(cas, input, None)?,
            input
                .inputs
                .values()
                .flat_map(|port| port.artifact_ids.iter().cloned())
                .collect(),
            None,
            serde_json::json!({"invocation":input}),
        )
        .map(|(id, _)| id)
        .map_err(|error| error.to_string())
    }

    fn execute_controlled(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }
        let output = self.execute(cas, input, attempt);
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }
        output
    }

    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        TaskWorkOutput {
            usage_observation: None,
            usage: None,
            outputs: self.outputs(cas, input),
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        }
    }
}

impl TaskDomain for OptimizationTaskDomain {
    fn assemble_result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let execution = state
            .execution
            .as_ref()
            .ok_or("Optimization Task has no execution")?;
        let get = |address: &review_graph::task::Address| {
            execution
                .outputs
                .get(&address.node)
                .and_then(|(_, output)| output.outputs.get(&address.port))
        };
        let mut result = TaskResultV1 {
            task_revision_id: state.revision_id.clone(),
            execution: if report
                .outcomes
                .iter()
                .any(|(_, outcome)| matches!(outcome, NodeOutcome::Failed { .. }))
            {
                TaskExecutionV1::Exhausted
            } else {
                TaskExecutionV1::Completed
            },
            acceptance: TaskAcceptanceV1::Inconclusive,
            domain_conclusion: "report_incomplete".into(),
            outputs: self
                .graph
                .outputs
                .iter()
                .filter_map(|(name, address)| get(address).map(|port| (name.clone(), port.clone())))
                .collect(),
            evidence: BTreeSet::new(),
            missing_obligations: BTreeSet::new(),
        };
        self.assess(cas, &state.revision, &mut result)?;
        result.validate()?;
        Ok(result)
    }

    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        context_id: &str,
    ) -> Result<(), String> {
        if self.prepare_context(cas, input, feedback)? != context_id {
            return Err("Optimization context changed its captured invocation".into());
        }
        Ok(())
    }

    fn validate_output(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let expected = self.outputs(cas, input)?;
        if output.outputs != expected {
            return Err("Optimization output changed its deterministic captured projection".into());
        }
        Ok(())
    }

    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let mut expected = result.clone();
        self.assess(cas, task, &mut expected)?;
        if expected != *result {
            return Err("Optimization result changed its evidence-derived acceptance".into());
        }
        Ok(())
    }
}

/// Installed M2 candidate coordinator. It prepares exact bounded children from captured
/// Pipeline slots and folds only Store-selected outputs and common Attempt accounting. It does
/// not produce delivery authority: independent finalization remains a separate obligation.
pub struct OptimizationCandidateTaskDomain {
    graph: CompiledTask,
    plan: ExecutionPlanV1,
    parent: String,
    baseline_slot: String,
    candidate_slot: String,
    baseline_definition: review_graph::task::CompiledNode,
    candidate_definition: review_graph::task::CompiledNode,
    baseline_attempt: review_graph::task::OperatorAttemptCost,
    candidate_attempt: review_graph::task::OperatorAttemptCost,
    baseline_effort: String,
    candidate_effort: String,
}

impl OptimizationCandidateTaskDomain {
    fn attach_exact_light_result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        let Some(proposal_port) = result.outputs.get("proposal") else {
            return Ok(());
        };
        if !["comparison", "verification", "result"]
            .into_iter()
            .all(|name| result.outputs.contains_key(name))
        {
            // A refusal before the experiment/finalizer remains an ordinary inconclusive Task
            // result and must not manufacture an economics decision from absent measurements.
            return Ok(());
        }
        let proposal_id = proposal_port
            .artifact_ids
            .first()
            .ok_or("Light result has no proposal identity")?;
        let proposal: OptimizationProposalV1 = read(cas, proposal_id, OPTIMIZATION_PROPOSAL_V1)?;
        proposal.validate()?;
        let comparison_id = result
            .outputs
            .get("comparison")
            .and_then(|port| port.artifact_ids.first())
            .ok_or("Light result has no comparison identity")?;
        let comparison: ExperimentComparisonV1 =
            read(cas, comparison_id, EXPERIMENT_COMPARISON_V1)?;
        comparison.validate()?;
        let verification_id = result
            .outputs
            .get("verification")
            .and_then(|port| port.artifact_ids.first())
            .ok_or("Light result has no verification identity")?;
        let verification: OptimizationVerificationV1 =
            read(cas, verification_id, OPTIMIZATION_VERIFICATION_V1)?;
        verification.validate()?;
        let provisional_result_id = result
            .outputs
            .get("result")
            .and_then(|port| port.artifact_ids.first())
            .cloned()
            .ok_or("Light result has no preliminary economics identity")?;
        let provisional: OptimizationResultV1 =
            read(cas, &provisional_result_id, OPTIMIZATION_RESULT_V1)?;
        provisional.validate()?;
        let verification_envelope = cas
            .get_artifact(verification_id)
            .map_err(|error| error.to_string())?;
        let configuration_id = verification_envelope
            .input_artifacts
            .iter()
            .find(|id| {
                cas.get_optional_artifact(id).is_ok_and(|artifact| {
                    artifact.is_some_and(|artifact| {
                        artifact.artifact_type == configuration::CONFIGURATION
                    })
                })
            })
            .ok_or("Light verification does not retain its captured configuration")?;
        let configuration = configuration::read_configuration(cas, configuration_id)?;
        let execution = state
            .execution
            .as_ref()
            .ok_or("Light result has no common-runtime accounting")?;
        let attempts = execution.attempt_accounting();
        let one_off_tokens = attempts.iter().try_fold(0u128, |total, attempt| {
            total
                .checked_add(attempt.charged_tokens)
                .ok_or("Light optimizer charge overflow")
        })?;
        let one_off_tokens = u64::try_from(one_off_tokens)
            .map_err(|_| "Light optimizer charge exceeds its wire domain")?;
        let mut missing_measurements = BTreeSet::new();
        let mut intervals = Vec::new();
        for attempt in &attempts {
            if !attempt.started {
                continue;
            }
            let native = if attempt.reservation.node.ends_with("_baseline") {
                self.baseline_effort != "command"
            } else if attempt.reservation.node.ends_with("_candidate") {
                self.candidate_effort != "command"
            } else {
                attempt.reservation.tokens > 0
            };
            if !attempt
                .billing_complete(cas, native)
                .map_err(|error| error.to_string())?
            {
                missing_measurements.insert("incomplete_attempt_billing".into());
            }
            match (attempt.started_unix_ms, attempt.settled_unix_ms) {
                (Some(start), Some(end)) if end >= start => intervals.push((start, end)),
                _ => {
                    missing_measurements.insert("attempt_wall_time".into());
                }
            }
        }
        let one_off_time_ms = interval_union_ms(intervals)?;
        let normalization_units = comparison_normalization_units(&comparison)?;
        let policy = configuration.light_economics.as_ref();
        if policy.is_none() {
            missing_measurements.insert("captured_light_economics_policy".into());
        }
        let recurring = policy.map_or(
            OptimizationCostV1 {
                tokens: 0,
                time_ms: 0,
            },
            |policy| OptimizationCostV1 {
                tokens: policy.recurring_tokens_per_run,
                time_ms: policy.recurring_time_ms_per_run,
            },
        );
        let mut economics = match OptimizationRealizedEconomicsV1::calculate_normalized(
            OptimizationCostV1 {
                tokens: comparison.baseline_tokens,
                time_ms: comparison.baseline_elapsed_ms,
            },
            OptimizationCostV1 {
                tokens: comparison.candidate_tokens,
                time_ms: comparison.candidate_elapsed_ms,
            },
            recurring.clone(),
            OptimizationCostV1 {
                tokens: one_off_tokens,
                time_ms: one_off_time_ms,
            },
            normalization_units,
        ) {
            Ok(value) => value,
            Err(_) => {
                missing_measurements.insert("exact_per_run_normalization".into());
                OptimizationRealizedEconomicsV1::calculate(
                    OptimizationCostV1 {
                        tokens: 0,
                        time_ms: 0,
                    },
                    OptimizationCostV1 {
                        tokens: 0,
                        time_ms: 0,
                    },
                    recurring.clone(),
                    OptimizationCostV1 {
                        tokens: one_off_tokens,
                        time_ms: one_off_time_ms,
                    },
                )?
            }
        };
        economics.accounting_complete = missing_measurements.is_empty();
        economics.missing_measurements = missing_measurements;
        let accepted = verification.conclusion == ComparisonConclusionV1::Accepted
            && comparison.conclusion == ComparisonConclusionV1::Accepted
            && verification.protected_checks_passed;
        let within_declared_validation = policy.is_some_and(|policy| {
            one_off_tokens <= policy.maximum_one_off_tokens
                && one_off_time_ms <= policy.maximum_one_off_time_ms
        });
        let objective_exception = policy.and_then(|policy| policy.objective_exception.clone());
        let expected_comparable_workload = policy.map_or(0, |policy| policy.comparable_future_runs);
        let adoption_offered = accepted
            && within_declared_validation
            && economics.accounting_complete
            && (economics.pays_back(expected_comparable_workload) || objective_exception.is_some());
        let exact = OptimizationResultV1 {
            schema: "af.optimization-result/1".into(),
            source_snapshot_id: verification.source_snapshot_id.clone(),
            candidate_snapshot_id: verification.candidate_snapshot_id.clone(),
            profile_id: proposal.profile_id.clone(),
            proposal_id: proposal_id.clone(),
            comparison_id: comparison_id.clone(),
            evaluation_id: verification.evaluation_id.clone(),
            verification_id: verification_id.clone(),
            conclusion: if adoption_offered {
                OptimizationResultConclusionV1::Validated
            } else if accepted {
                OptimizationResultConclusionV1::RecommendationOnly
            } else {
                OptimizationResultConclusionV1::Rejected
            },
            experiment_conclusion: comparison.conclusion,
            economics,
            expected_comparable_workload: expected_comparable_workload.into(),
            objective_exception,
            adoption_offered,
        };
        exact.validate()?;
        let id = cas
            .put_artifact(
                OPTIMIZATION_RESULT_V1,
                Producer::KernelOperation {
                    run_id: review_store::store::task::task_run_id(&state.task_id)
                        .map_err(|error| error.to_string())?,
                    node_id: None,
                    operation_id: "optimization-exact-economics-v1".into(),
                },
                vec![
                    provisional_result_id,
                    proposal_id.clone(),
                    comparison_id.clone(),
                    verification.evaluation_id,
                    verification_id.clone(),
                ],
                None,
                serde_json::to_value(exact).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?
            .0;
        result.outputs.insert(
            "result".into(),
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: OPTIMIZATION_RESULT_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: None,
            },
        );
        Ok(())
    }

    fn assess_candidate(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        let accepted = if let Some(port) = result.outputs.get("verification") {
            if port.artifact_type != OPTIMIZATION_VERIFICATION_V1 || port.artifact_ids.len() != 1 {
                return Err("Invalid optimizer verification output".into());
            }
            let value: OptimizationVerificationV1 =
                read(cas, &port.artifact_ids[0], OPTIMIZATION_VERIFICATION_V1)?;
            value.validate()?;
            if task
                .inputs
                .get("source")
                .and_then(|p| p.snapshot_id.as_ref())
                != Some(&value.source_snapshot_id)
                || result
                    .outputs
                    .get("snapshot")
                    .and_then(|p| p.snapshot_id.as_ref())
                    != Some(&value.candidate_snapshot_id)
                || task
                    .inputs
                    .get("requirements")
                    .and_then(|p| p.artifact_ids.first())
                    != Some(&value.requirements_id)
            {
                return Err(
                    "Optimizer verification belongs to another source, candidate or Requirements"
                        .into(),
                );
            }
            result.evidence.insert(port.artifact_ids[0].clone());
            let economics_admits = if result.outputs.contains_key("proposal") {
                let result_port = result
                    .outputs
                    .get("result")
                    .ok_or("Light optimizer omitted its exact economics result")?;
                if result_port.artifact_type != OPTIMIZATION_RESULT_V1
                    || result_port.artifact_ids.len() != 1
                {
                    return Err("Invalid light optimizer result output".into());
                }
                let economics: OptimizationResultV1 =
                    read(cas, &result_port.artifact_ids[0], OPTIMIZATION_RESULT_V1)?;
                economics.validate()?;
                if economics.source_snapshot_id != value.source_snapshot_id
                    || economics.candidate_snapshot_id != value.candidate_snapshot_id
                    || economics.verification_id != port.artifact_ids[0]
                    || economics.comparison_id != value.comparison_id
                    || economics.evaluation_id != value.evaluation_id
                {
                    return Err("Light economics result changed its verified experiment".into());
                }
                economics.adoption_offered
                    && economics.conclusion == OptimizationResultConclusionV1::Validated
            } else {
                true
            };
            value.deliverable
                && economics_admits
                && value.conclusion == ComparisonConclusionV1::Accepted
                && result.execution == TaskExecutionV1::Completed
        } else {
            false
        };
        result.acceptance = if accepted {
            TaskAcceptanceV1::Satisfied
        } else {
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = if accepted {
            "candidate_verified"
        } else if result.outputs.contains_key("verification") {
            "candidate_not_verified"
        } else {
            "comparison_ready_finalization_missing"
        }
        .into();
        result.missing_obligations = if accepted {
            BTreeSet::new()
        } else {
            task.acceptance.keys().cloned().collect()
        };
        Ok(())
    }

    fn pure_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let operator = match &self
            .graph
            .nodes
            .get(&input.node)
            .ok_or("Unknown optimizer node")?
            .operator
        {
            CompiledOperator::Primitive { operator, .. } => operator,
            _ => return Err("Optimizer operation is not primitive".into()),
        };
        let retained: Vec<String> = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.clone())
            .collect();
        match operator {
            TaskOperatorV1::OptimizationProfile {} => {
                let history_id = input_id(input, "history", OPTIMIZATION_HISTORY_V1)?;
                let source = super::source::source_input(
                    cas,
                    input.inputs.get("source").ok_or("Missing source")?,
                )?;
                let history = history_chain(cas, history_id)?;
                let economics = project_economics(&history)?;
                let economics_port = artifact_port(
                    cas,
                    input,
                    OPTIMIZATION_ECONOMICS_V1,
                    &economics,
                    economics.capture_ids.clone(),
                )?;
                let economics_id = economics_port.artifact_ids[0].clone();
                let writable = configuration::writable_configuration_view(cas, &source)?;
                let writable_port = artifact_port(
                    cas,
                    input,
                    OPTIMIZATION_WRITABLE_CONFIGURATION_V1,
                    &writable,
                    std::iter::once(source.clone())
                        .chain(writable.files.values().map(|file| file.content_id.clone()))
                        .collect(),
                )?;
                let catalog = light_recipe_catalog()?;
                let catalog_version = cas
                    .put_json(
                        &serde_json::to_value(&catalog.recipes)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                if catalog_version != catalog.catalog_version {
                    return Err("Installed light recipe catalog identity changed".into());
                }
                let recipes_port = artifact_port(
                    cas,
                    input,
                    OPTIMIZATION_RECIPE_CATALOG_V1,
                    &catalog,
                    vec![catalog_version],
                )?;
                let recipe_catalog_id = recipes_port.artifact_ids[0].clone();
                let (declared_development, holdout) =
                    configuration::configuration_case_families(cas, &source)?;
                let observed = history
                    .iter()
                    .flat_map(|(_, capture)| capture.observations.iter())
                    .map(|observation| observation.attribution.case_family.clone())
                    .collect::<BTreeSet<_>>();
                let development_families = observed
                    .difference(&holdout)
                    .cloned()
                    .chain(declared_development)
                    .collect::<BTreeSet<_>>();
                let development_set_id = review_store::content_id(&serde_json::json!({
                    "domain":"af/optimization-development-set/1",
                    "history":history_id,
                    "families":development_families,
                }))
                .map_err(|error| error.to_string())?;
                let holdout_set_id = review_store::content_id(&serde_json::json!({
                    "domain":"af/optimization-holdout-set/1",
                    "source":source,
                    "families":holdout,
                }))
                .map_err(|error| error.to_string())?;
                let has_repeated_context = economics
                    .repeated_context_tokens
                    .is_some_and(|value| value.get() > 0);
                let has_cache = !economics.cache_economics.is_empty();
                let mut available_observations = BTreeSet::new();
                if economics.context_tokens.is_some() {
                    available_observations.insert("context_tokens".into());
                }
                if economics.retrieval_tokens.is_some() {
                    available_observations.insert("retrieval_tokens".into());
                }
                if has_cache {
                    available_observations.insert("cache_measurements".into());
                }
                let candidate_recipe_ids = [
                    has_repeated_context.then_some("context_retrieval_dedup"),
                    has_cache.then_some("sandbox_dependency_cache"),
                ]
                .into_iter()
                .flatten()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
                let eligible_recipe_ids = catalog
                    .recipes
                    .iter()
                    .filter(|recipe| {
                        recipe.support == OptimizationRecipeSupportV1::Installed
                            && candidate_recipe_ids.contains(&recipe.recipe_id)
                            && recipe
                                .required_observations
                                .is_subset(&available_observations)
                    })
                    .map(|recipe| recipe.recipe_id.clone())
                    .collect();
                let profile = OptimizationDevelopmentProfileV1 {
                    schema: "af.optimization-profile/1".into(),
                    project_id: economics.project_id.clone(),
                    history_id: history_id.into(),
                    economics_id: economics_id.clone(),
                    recipe_catalog_id: recipe_catalog_id.clone(),
                    development_set_id,
                    holdout_set_id,
                    development_families,
                    eligible_recipe_ids,
                    incomplete_observations: economics.missing_fields.clone(),
                    comparable_future_runs: configuration::configuration_light_economics(
                        cas, &source,
                    )?
                    .map_or(0, |policy| policy.comparable_future_runs)
                    .into(),
                };
                profile.validate()?;
                let profile_port = artifact_port(
                    cas,
                    input,
                    OPTIMIZATION_PROFILE_V1,
                    &profile,
                    vec![history_id.into(), source, economics_id, recipe_catalog_id],
                )?;
                Ok(BTreeMap::from([
                    ("configuration".into(), writable_port),
                    ("economics".into(), economics_port),
                    ("profile".into(), profile_port),
                    ("recipes".into(), recipes_port),
                ]))
            }
            TaskOperatorV1::OptimizationPrepare {} => {
                let source = super::source::source_input(
                    cas,
                    input.inputs.get("source").ok_or("Missing source")?,
                )?;
                let requirements = input_id(input, "requirements", "af/Requirements@1")?;
                let producer = invocation_producer(cas, input, None)?;
                let optional = ["profile", "recipes", "diagnostic", "proposal"]
                    .into_iter()
                    .filter_map(|name| input.inputs.get(name).map(|port| (name, port)))
                    .collect::<BTreeMap<_, _>>();
                let proposal = if optional.is_empty() {
                    None
                } else {
                    if optional.len() != 4 {
                        return Err("Light candidate preparation requires profile, recipes, diagnostic and proposal together".into());
                    }
                    let profile_id = input_id(input, "profile", OPTIMIZATION_PROFILE_V1)?;
                    let profile: OptimizationDevelopmentProfileV1 =
                        read(cas, profile_id, OPTIMIZATION_PROFILE_V1)?;
                    profile.validate()?;
                    let recipes_id = input_id(input, "recipes", OPTIMIZATION_RECIPE_CATALOG_V1)?;
                    let recipes: OptimizationRecipeCatalogV1 =
                        read(cas, recipes_id, OPTIMIZATION_RECIPE_CATALOG_V1)?;
                    recipes.validate()?;
                    let diagnostic_id = input_id(input, "diagnostic", OPTIMIZATION_DIAGNOSTIC_V1)?;
                    let diagnostic: OptimizationDiagnosticV1 =
                        read(cas, diagnostic_id, OPTIMIZATION_DIAGNOSTIC_V1)?;
                    diagnostic.validate()?;
                    let proposal_id = input_id(input, "proposal", OPTIMIZATION_PROPOSAL_V1)?;
                    let proposal: OptimizationProposalV1 =
                        read(cas, proposal_id, OPTIMIZATION_PROPOSAL_V1)?;
                    proposal.validate_against(profile_id, diagnostic_id, &recipes)?;
                    if profile.recipe_catalog_id != recipes_id
                        || diagnostic.profile_id != profile_id
                        || diagnostic.selected_recipe_id != proposal.recipe_id
                        || !profile.eligible_recipe_ids.contains(&proposal.recipe_id)
                    {
                        return Err(
                            "Light proposal differs from its eligible development-only diagnosis"
                                .into(),
                        );
                    }
                    Some((
                        profile,
                        diagnostic,
                        recipes
                            .recipe(&proposal.recipe_id)
                            .cloned()
                            .ok_or("Light proposal selected an unregistered recipe")?,
                        proposal,
                    ))
                };
                let value = if let Some((profile, diagnostic, recipe, proposal)) = proposal {
                    let candidate_binding = self
                        .plan
                        .bindings
                        .get(&self.candidate_slot)
                        .ok_or("Light candidate slot has no exact captured binding")?;
                    configuration::prepare_light_configuration_with_proposal(
                        cas,
                        &source,
                        requirements,
                        &self.plan.engine_id,
                        &self.plan.authority.policy_id,
                        &self.plan.task_revision_id,
                        &candidate_binding.package_artifact_id,
                        &candidate_binding.package_digest,
                        producer.clone(),
                        &profile,
                        &diagnostic,
                        &recipe,
                        &proposal,
                    )?
                } else {
                    configuration::prepare_configuration_with_proposal(
                        cas,
                        &source,
                        requirements,
                        &self.plan.engine_id,
                        &self.plan.authority.policy_id,
                        &self.plan.task_revision_id,
                        producer.clone(),
                        None,
                    )?
                };
                let mut refs = vec![
                    source.clone(),
                    value.candidate_snapshot_id.clone(),
                    requirements.into(),
                    value.harness_id.clone(),
                    value.repin_id.clone(),
                    value.engine_id.clone(),
                    value.baseline_fixture_id.clone(),
                    value.candidate_fixture_id.clone(),
                ];
                refs.extend(value.materialization_id.iter().cloned());
                refs.extend(value.case_inputs.values().cloned());
                refs.extend(value.candidate_execution_configuration_id.iter().cloned());
                refs.extend(retained.clone());
                let config = artifact_port(cas, input, configuration::CONFIGURATION, &value, refs)?;
                let candidate = configuration::source_port(
                    cas,
                    &value.candidate_snapshot_id,
                    producer,
                    retained,
                )?;
                Ok(BTreeMap::from([
                    ("configuration".into(), config),
                    ("candidate".into(), candidate),
                ]))
            }
            TaskOperatorV1::OptimizationFinalize {} => {
                let config_id = input_id(input, "configuration", configuration::CONFIGURATION)?;
                let config = configuration::read_configuration(cas, config_id)?;
                let comparison_id = input_id(input, "comparison", EXPERIMENT_COMPARISON_V1)?;
                let comparison: ExperimentComparisonV1 =
                    read(cas, comparison_id, EXPERIMENT_COMPARISON_V1)?;
                comparison.validate()?;
                let evaluation_id = input_id(input, "evaluation", OPTIMIZATION_EVALUATION_V1)?;
                let evaluation: OptimizationEvaluationV1 =
                    read(cas, evaluation_id, OPTIMIZATION_EVALUATION_V1)?;
                evaluation.validate()?;
                let light_input_count = ["profile", "diagnostic", "proposal"]
                    .into_iter()
                    .filter(|name| input.inputs.contains_key(*name))
                    .count();
                if light_input_count != 0 && light_input_count != 3 {
                    return Err(
                        "Light finalization requires profile, diagnostic and proposal together"
                            .into(),
                    );
                }
                if light_input_count == 0 {
                    let source = super::source::source_input(cas, &input.inputs["source"])?;
                    let candidate = super::source::source_input(cas, &input.inputs["candidate"])?;
                    if config.source_snapshot_id != source
                        || config.candidate_snapshot_id != candidate
                        || evaluation.task_revision_id != self.plan.task_revision_id
                        || evaluation.requirements_id != config.requirements_id
                        || evaluation.candidate_snapshot_id != candidate
                        || evaluation.specification_id != comparison.specification_id
                        || evaluation.prepared_id != comparison.prepared_id
                        || evaluation.comparison_id != comparison_id
                    {
                        return Err(
                            "Finalization evidence changed the captured candidate or experiment"
                                .into(),
                        );
                    }
                    let accepted = comparison.conclusion == ComparisonConclusionV1::Accepted
                        && evaluation.conclusion == ComparisonConclusionV1::Accepted;
                    let verification = OptimizationVerificationV1 {
                        schema: "af.optimization-verification/1".into(),
                        profile: review_core::task::optimization_experiment::OptimizationProfileV1::Candidate,
                        source_snapshot_id: source.clone(),
                        candidate_snapshot_id: candidate.clone(),
                        requirements_id: config.requirements_id.clone(),
                        harness_id: config.harness_id.clone(),
                        comparison_id: comparison_id.into(),
                        evaluation_id: evaluation_id.into(),
                        package_repin_id: config.repin_id.clone(),
                        conclusion: if accepted {
                            ComparisonConclusionV1::Accepted
                        } else {
                            ComparisonConclusionV1::Inconclusive
                        },
                        deliverable: accepted,
                        protected_checks_passed: accepted,
                    };
                    verification.validate()?;
                    let mut producer = invocation_producer(cas, input, None)?;
                    if let Producer::KernelOperation { operation_id, .. } = &mut producer {
                        *operation_id = "optimization-final-verification-v1".into();
                    }
                    let mut refs = vec![
                        source.clone(),
                        candidate.clone(),
                        config.requirements_id,
                        config.harness_id,
                        comparison_id.into(),
                        evaluation_id.into(),
                        config.repin_id,
                        config_id.into(),
                    ];
                    refs.extend(retained.clone());
                    let verification_id = cas
                        .put_artifact(
                            OPTIMIZATION_VERIFICATION_V1,
                            producer.clone(),
                            refs,
                            Some(candidate.clone()),
                            serde_json::to_value(verification).map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| e.to_string())?
                        .0;
                    return Ok(BTreeMap::from([
                        (
                            "verification".into(),
                            ArtifactInputV1 {
                                artifact_ids: vec![verification_id],
                                artifact_type: OPTIMIZATION_VERIFICATION_V1.into(),
                                cardinality: PortCardinality::One,
                                snapshot_id: Some(candidate.clone()),
                            },
                        ),
                        (
                            "snapshot".into(),
                            configuration::source_port(cas, &candidate, producer, retained)?,
                        ),
                    ]));
                }
                let profile_id = input_id(input, "profile", OPTIMIZATION_PROFILE_V1)?;
                let profile: OptimizationDevelopmentProfileV1 =
                    read(cas, profile_id, OPTIMIZATION_PROFILE_V1)?;
                profile.validate()?;
                let diagnostic_id = input_id(input, "diagnostic", OPTIMIZATION_DIAGNOSTIC_V1)?;
                let diagnostic: OptimizationDiagnosticV1 =
                    read(cas, diagnostic_id, OPTIMIZATION_DIAGNOSTIC_V1)?;
                diagnostic.validate()?;
                let proposal_id = input_id(input, "proposal", OPTIMIZATION_PROPOSAL_V1)?;
                let proposal: OptimizationProposalV1 =
                    read(cas, proposal_id, OPTIMIZATION_PROPOSAL_V1)?;
                proposal.validate()?;
                let source = super::source::source_input(cas, &input.inputs["source"])?;
                let candidate = super::source::source_input(cas, &input.inputs["candidate"])?;
                if config.source_snapshot_id != source
                    || config.candidate_snapshot_id != candidate
                    || evaluation.task_revision_id != self.plan.task_revision_id
                    || evaluation.requirements_id != config.requirements_id
                    || evaluation.candidate_snapshot_id != candidate
                    || evaluation.specification_id != comparison.specification_id
                    || evaluation.prepared_id != comparison.prepared_id
                    || evaluation.comparison_id != comparison_id
                    || diagnostic.profile_id != profile_id
                    || proposal.profile_id != profile_id
                    || proposal.diagnostic_id != diagnostic_id
                    || proposal.expected.comparable_future_runs
                        != diagnostic.expected_comparable_workload
                {
                    return Err(
                        "Finalization evidence changed the captured candidate or experiment".into(),
                    );
                }
                let accepted = comparison.conclusion == ComparisonConclusionV1::Accepted
                    && evaluation.conclusion == ComparisonConclusionV1::Accepted;
                let light_policy = config.light_economics.as_ref();
                let recurring = light_policy.map_or(
                    OptimizationCostV1 {
                        tokens: 0,
                        time_ms: 0,
                    },
                    |policy| OptimizationCostV1 {
                        tokens: policy.recurring_tokens_per_run,
                        time_ms: policy.recurring_time_ms_per_run,
                    },
                );
                let normalization_units = comparison_normalization_units(&comparison)?;
                let provisional_one_off = OptimizationCostV1 {
                    tokens: comparison
                        .baseline_tokens
                        .checked_add(comparison.candidate_tokens)
                        .ok_or("Provisional optimizer token overflow")?,
                    time_ms: comparison
                        .baseline_elapsed_ms
                        .checked_add(comparison.candidate_elapsed_ms)
                        .ok_or("Provisional optimizer time overflow")?,
                };
                let mut provisional_economics =
                    match OptimizationRealizedEconomicsV1::calculate_normalized(
                        OptimizationCostV1 {
                            tokens: comparison.baseline_tokens,
                            time_ms: comparison.baseline_elapsed_ms,
                        },
                        OptimizationCostV1 {
                            tokens: comparison.candidate_tokens,
                            time_ms: comparison.candidate_elapsed_ms,
                        },
                        recurring.clone(),
                        provisional_one_off.clone(),
                        normalization_units,
                    ) {
                        Ok(value) => value,
                        Err(_) => {
                            let mut value = OptimizationRealizedEconomicsV1::calculate(
                                OptimizationCostV1 {
                                    tokens: 0,
                                    time_ms: 0,
                                },
                                OptimizationCostV1 {
                                    tokens: 0,
                                    time_ms: 0,
                                },
                                recurring,
                                provisional_one_off,
                            )?;
                            value.accounting_complete = false;
                            value
                                .missing_measurements
                                .insert("exact_per_run_normalization".into());
                            value
                        }
                    };
                if provisional_economics.normalization_units.get() != normalization_units {
                    provisional_economics.normalization_units = normalization_units.into();
                }
                // The final DAG node does not yet have the complete Task ledger. Exact
                // economics refinement after all Attempts settle is the only producer allowed
                // to offer adoption.
                let adoption_offered = false;
                let verification = OptimizationVerificationV1 {
                    schema: "af.optimization-verification/1".into(),
                    profile:
                        review_core::task::optimization_experiment::OptimizationProfileV1::Candidate,
                    source_snapshot_id: source.clone(),
                    candidate_snapshot_id: candidate.clone(),
                    requirements_id: config.requirements_id.clone(),
                    harness_id: config.harness_id.clone(),
                    comparison_id: comparison_id.into(),
                    evaluation_id: evaluation_id.into(),
                    package_repin_id: config.repin_id.clone(),
                    conclusion: if accepted {
                        ComparisonConclusionV1::Accepted
                    } else {
                        ComparisonConclusionV1::Inconclusive
                    },
                    // This receipt proves candidate verification. The exact post-settlement
                    // OptimizationResult remains the separate economics delivery gate.
                    deliverable: accepted,
                    protected_checks_passed: accepted,
                };
                verification.validate()?;
                let mut producer = invocation_producer(cas, input, None)?;
                if let Producer::KernelOperation { operation_id, .. } = &mut producer {
                    *operation_id = "optimization-final-verification-v1".into();
                }
                let mut refs = vec![
                    source.clone(),
                    candidate.clone(),
                    config.requirements_id,
                    config.harness_id,
                    comparison_id.into(),
                    evaluation_id.into(),
                    config.repin_id,
                    config_id.into(),
                ];
                refs.extend(retained.clone());
                let verification_id = cas
                    .put_artifact(
                        OPTIMIZATION_VERIFICATION_V1,
                        producer.clone(),
                        refs,
                        Some(candidate.clone()),
                        serde_json::to_value(verification).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?
                    .0;
                let optimization_result = OptimizationResultV1 {
                    schema: "af.optimization-result/1".into(),
                    source_snapshot_id: source.clone(),
                    candidate_snapshot_id: candidate.clone(),
                    profile_id: profile_id.into(),
                    proposal_id: proposal_id.into(),
                    comparison_id: comparison_id.into(),
                    evaluation_id: evaluation_id.into(),
                    verification_id: verification_id.clone(),
                    conclusion: if adoption_offered {
                        OptimizationResultConclusionV1::Validated
                    } else if accepted {
                        OptimizationResultConclusionV1::RecommendationOnly
                    } else {
                        OptimizationResultConclusionV1::Rejected
                    },
                    experiment_conclusion: comparison.conclusion,
                    economics: provisional_economics,
                    expected_comparable_workload: light_policy
                        .map_or(0, |policy| policy.comparable_future_runs)
                        .into(),
                    objective_exception: light_policy
                        .and_then(|policy| policy.objective_exception.clone()),
                    adoption_offered,
                };
                optimization_result.validate()?;
                let result = artifact_port(
                    cas,
                    input,
                    OPTIMIZATION_RESULT_V1,
                    &optimization_result,
                    vec![
                        source.clone(),
                        candidate.clone(),
                        profile_id.into(),
                        proposal_id.into(),
                        comparison_id.into(),
                        evaluation_id.into(),
                        verification_id.clone(),
                    ],
                )?;
                Ok(BTreeMap::from([
                    (
                        "verification".into(),
                        ArtifactInputV1 {
                            artifact_ids: vec![verification_id],
                            artifact_type: OPTIMIZATION_VERIFICATION_V1.into(),
                            cardinality: PortCardinality::One,
                            snapshot_id: Some(candidate.clone()),
                        },
                    ),
                    (
                        "snapshot".into(),
                        configuration::source_port(cas, &candidate, producer, retained)?,
                    ),
                    ("result".into(), result),
                ]))
            }
            _ => Err("Optimizer operation is not a deterministic configuration step".into()),
        }
    }

    pub fn captured(
        graph: CompiledTask,
        plan: ExecutionPlanV1,
        compiler: &review_config::task::catalog::TaskPlanCompiler,
    ) -> Result<Self, String> {
        let experiments = graph
            .nodes
            .iter()
            .filter_map(|(name, node)| match &node.operator {
                CompiledOperator::Primitive {
                    operator:
                        TaskOperatorV1::OptimizationExperiment {
                            baseline_slot,
                            candidate_slot,
                        },
                    ..
                } => Some((name.clone(), baseline_slot.clone(), candidate_slot.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(parent, baseline_local, candidate_local)] = experiments.as_slice() else {
            return Err(
                "Candidate Optimization requires exactly one installed experiment coordinator"
                    .into(),
            );
        };
        let scope = parent
            .split_once(".nodes.")
            .map(|(scope, _)| scope)
            .ok_or("Optimization coordinator has no compiled Pipeline scope")?;
        let qualify = |slot: &str| {
            if graph.slots.contains_key(slot) {
                slot.to_owned()
            } else {
                format!("{scope}.slots.{slot}")
            }
        };
        let baseline_slot = qualify(baseline_local);
        let candidate_slot = qualify(candidate_local);
        let definition = |slot: &str| -> Result<
            (
                review_graph::task::CompiledNode,
                review_graph::task::OperatorAttemptCost,
                String,
            ),
            String,
        > {
            let worker_name = &graph
                .slots
                .get(slot)
                .ok_or("Optimization experiment lost a captured Worker slot")?
                .worker;
            let worker = compiler
                .worker(worker_name)
                .ok_or("Optimization experiment Worker package is absent")?;
            let attempt = worker
                .signature
                .attempt
                .clone()
                .filter(|attempt| attempt.wall_ms > 0)
                .ok_or("Candidate Optimization requires an exact bounded Worker Attempt")?;
            if !worker.signature.effects.is_empty() {
                return Err(
                    "Measured optimization Workers must be read-only and effect-free".into(),
                );
            }
            let effort = match &plan
                .bindings
                .get(slot)
                .ok_or("Optimization experiment Worker has no effective binding")?
                .execution
            {
                review_core::task::plan::WorkerExecutionV1::Command {} => "command".into(),
                review_core::task::plan::WorkerExecutionV1::Model { effort, .. } => effort.clone(),
            };
            Ok((
                review_graph::task::CompiledNode {
                    operator: CompiledOperator::Primitive {
                        // Experimental measurements are protected verification operations.  The
                        // captured command package supplies the fixture implementation, but its
                        // exit status only becomes trial evidence under the common Verify path and
                        // its separately reserved verification allowance.
                        operator: TaskOperatorV1::Verify { slot: slot.into() },
                        signature: format!("worker/{worker_name}"),
                    },
                    contract: worker.signature.contract.clone(),
                    inputs: BTreeMap::new(),
                    conditions: vec![],
                },
                attempt,
                effort,
            ))
        };
        let (baseline_definition, baseline_attempt, baseline_effort) = definition(&baseline_slot)?;
        let (candidate_definition, candidate_attempt, candidate_effort) =
            definition(&candidate_slot)?;
        if baseline_definition.contract != candidate_definition.contract {
            return Err(
                "Baseline and candidate Workers must expose the same measured contract".into(),
            );
        }
        if !graph.experimental_slots.contains_key(parent) {
            return Err("Candidate Optimization plan has no protected experimental slot".into());
        }
        Ok(Self {
            graph,
            plan,
            parent: parent.clone(),
            baseline_slot,
            candidate_slot,
            baseline_definition,
            candidate_definition,
            baseline_attempt,
            candidate_attempt,
            baseline_effort,
            candidate_effort,
        })
    }

    fn one<'a>(
        input: &'a TaskInvocationV1,
        name: &str,
        ty: &str,
    ) -> Result<&'a ArtifactInputV1, String> {
        let value = input
            .inputs
            .get(name)
            .ok_or_else(|| format!("Optimization experiment lacks {name}"))?;
        value.validate()?;
        if value.artifact_type != ty || value.artifact_ids.len() != 1 {
            return Err(format!(
                "Optimization experiment {name} changed its captured contract"
            ));
        }
        Ok(value)
    }

    fn selected_output<'a>(
        cas: &Cas,
        fact: &'a review_store::store::task::execution::experiment::ExperimentChildEvidence,
    ) -> Result<&'a str, String> {
        let selected = fact
            .selected_output_id
            .as_deref()
            .ok_or("Experiment child has no selected common-runtime output")?;
        if fact.published_output_id.as_deref() != Some(selected) {
            return Err("Experiment child selection and publication differ".into());
        }
        let output: TaskOutputV1 =
            read(cas, selected, review_core::task::execution::TASK_OUTPUT_V1)?;
        if output.outputs.is_empty() {
            return Err("Experiment child selected an empty output".into());
        }
        Ok(selected)
    }
}

impl TaskOperatorHost for OptimizationCandidateTaskDomain {
    fn execute_controlled(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }
        let output = self.execute(cas, input, attempt);
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }
        output
    }

    fn prepare_experiment(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        writer_epoch: u64,
    ) -> Result<TaskExperimentInputs, String> {
        if parent.node != self.parent || !review_core::is_digest(&parent.plan_id) {
            return Err("Optimization preparation belongs to another coordinator".into());
        }
        let history = Self::one(parent, "history", OPTIMIZATION_HISTORY_V1)?;
        let requirements = Self::one(parent, "requirements", "af/Requirements@1")?;
        let source = Self::one(parent, "source", "af/SourceTree@1")?;
        let source_snapshot = super::source::source_input(cas, source)?;
        let configuration = parent
            .inputs
            .get("configuration")
            .map(|p| configuration::read_configuration(cas, &p.artifact_ids[0]))
            .transpose()?;
        if configuration.as_ref().is_some_and(|c| {
            source.snapshot_id.as_ref() != Some(&c.source_snapshot_id)
                || c.requirements_id != requirements.artifact_ids[0]
                || c.task_revision_id != self.plan.task_revision_id
        }) {
            return Err("Prepared configuration differs from the captured experiment".into());
        }
        let history_value: OptimizationHistoryV1 =
            read(cas, &history.artifact_ids[0], OPTIMIZATION_HISTORY_V1)?;
        history_value.validate()?;
        let slot_id = self.graph.experimental_slots[&self.parent].slot_id.clone();
        let slot: ExperimentalSlotV2 = read(cas, &slot_id, EXPERIMENTAL_SLOT_V2)?;
        let baseline_binding = self
            .plan
            .bindings
            .get(&self.baseline_slot)
            .ok_or("Baseline binding is absent")?;
        let candidate_binding = self
            .plan
            .bindings
            .get(&self.candidate_slot)
            .ok_or("Candidate binding is absent")?;
        let baseline_package = self.graph.slots[&self.baseline_slot].worker.clone();
        let candidate_package = self.graph.slots[&self.candidate_slot].worker.clone();
        let configuration = configuration.as_ref();
        let baseline_execution_authority = baseline_binding.package_artifact_id.clone();
        let candidate_execution_authority = match configuration
            .and_then(|value| value.candidate_execution_configuration_id.as_ref())
        {
            Some(configuration_id) => {
                let execution: OptimizationExecutionConfigurationV1 = read(
                    cas,
                    configuration_id,
                    OPTIMIZATION_EXECUTION_CONFIGURATION_V1,
                )?;
                execution.validate()?;
                let captured =
                    configuration.ok_or("Candidate package derivation lacks configuration")?;
                if execution.original_package_id != candidate_binding.package_artifact_id
                    || execution.original_package_digest != candidate_binding.package_digest
                    || execution.source_snapshot_id != captured.source_snapshot_id
                    || execution.candidate_snapshot_id != captured.candidate_snapshot_id
                    || execution.repin_id != captured.repin_id
                    || execution.package != candidate_package
                {
                    return Err(
                        "Candidate package derivation differs from source, repin or captured binding"
                            .into(),
                    );
                }
                review_config::task::catalog::TaskPlanCompiler::derive_worker_instructions_package(
                    cas,
                    &candidate_binding.package_artifact_id,
                    &candidate_binding.package_digest,
                    &execution.package_digest,
                    &execution.instructions_id,
                    &execution.instructions,
                    invocation_producer(cas, parent, None)?,
                    vec![
                        configuration_id.clone(),
                        captured.source_snapshot_id.clone(),
                        captured.candidate_snapshot_id.clone(),
                        captured.repin_id.clone(),
                    ],
                )?
            }
            None => candidate_binding.package_artifact_id.clone(),
        };
        // The standalone `--experiment` checkpoint has no candidate Snapshot and remains a
        // non-deliverable deterministic smoke comparison. Deliverable candidate Pipelines always
        // provide the captured policy below.
        let fallback_experiment = configuration::ConfigurationExperimentPolicy {
            recipe: ComparisonRecipeV1::DeterministicCorrection,
            uncertainty_rule: ComparisonUncertaintyRuleV1::Deterministic,
            repetitions: 1,
            minimum_families: 1,
            token_increase_ceiling_bps: 0,
            cases: vec![configuration::ConfigurationExperimentCase {
                family: "standalone_experiment".into(),
                input_path: String::new(),
                membership: "holdout".into(),
            }],
        };
        let experiment_policy = configuration
            .map(|configuration| &configuration.experiment)
            .unwrap_or(&fallback_experiment);
        let family_id = |family: &str| {
            review_store::content_id(&serde_json::json!(["optimization-family-v1", family]))
                .map_err(|error| error.to_string())
        };
        let exposed_family_ids = history_value
            .exposed_case_families
            .iter()
            .map(|family| family_id(family))
            .collect::<Result<BTreeSet<_>, String>>()?;
        let profile_id = review_store::content_id(&serde_json::json!([
            slot.policy_id,
            configuration.map(|configuration| &configuration.harness_id),
            experiment_policy
        ]))
        .map_err(|error| error.to_string())?;
        let cases = experiment_policy
            .cases
            .iter()
            .map(|declared| {
                let family_id = family_id(&declared.family)?;
                let case_id = review_store::content_id(&serde_json::json!([
                    profile_id,
                    family_id,
                    declared.membership,
                    source.artifact_ids[0],
                    requirements.artifact_ids[0]
                ]))
                .map_err(|error| error.to_string())?;
                Ok(ExperimentCaseV1 {
                    case_id,
                    family_id,
                    membership: declared.membership.clone(),
                    source_snapshot_id: source_snapshot.clone(),
                    requirements_id: requirements.artifact_ids[0].clone(),
                    compatibility_id: slot.protected_oracle_id.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let set_id = |membership: &str| {
            review_store::content_id(&serde_json::json!({
                "membership": membership,
                "cases": cases.iter().filter(|case| case.membership == membership)
                    .map(|case| &case.case_id).collect::<Vec<_>>()
            }))
            .map_err(|error| error.to_string())
        };
        let specification = ExperimentSpecificationV1 {
            schema: "af.experiment-specification/1".into(),
            slot_id: slot_id.clone(),
            policy_id: slot.policy_id.clone(),
            profile_id,
            development_set_id: set_id("development")?,
            holdout_set_id: set_id("holdout")?,
            protected_oracle_id: slot.protected_oracle_id.clone(),
            baseline_authority_id: baseline_execution_authority.clone(),
            candidate_authority_id: candidate_execution_authority.clone(),
            baseline_package: baseline_package.clone(),
            candidate_package: candidate_package.clone(),
            recipe: experiment_policy.recipe,
            uncertainty_rule: experiment_policy.uncertainty_rule,
            repetitions: experiment_policy.repetitions,
            minimum_families: experiment_policy.minimum_families,
            token_increase_ceiling_bps: experiment_policy.token_increase_ceiling_bps,
            exposed_family_ids,
            cases: cases.clone(),
        };
        specification.validate()?;
        let specification_id = cas
            .put_artifact(
                EXPERIMENT_SPECIFICATION_V1,
                invocation_producer(cas, parent, None)?,
                vec![
                    slot_id.clone(),
                    history.artifact_ids[0].clone(),
                    source.artifact_ids[0].clone(),
                    requirements.artifact_ids[0].clone(),
                ],
                None,
                serde_json::to_value(&specification).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?
            .0;
        let mut children = Vec::new();
        let mut planned = BTreeMap::new();
        let mut candidate_definition = self.candidate_definition.clone();
        if candidate_execution_authority != candidate_binding.package_artifact_id {
            let CompiledOperator::Primitive { signature, .. } = &mut candidate_definition.operator
            else {
                return Err("Candidate derivation requires a primitive Worker definition".into());
            };
            *signature = format!("worker-derived/{candidate_execution_authority}");
        }
        for (case_index, case) in cases.iter().enumerate() {
            for repetition in 1..=specification.repetitions {
                for (label, arm, package, binding, definition, attempt, effort) in [
                    (
                        "baseline",
                        ExperimentArmV1::Baseline,
                        &baseline_package,
                        baseline_binding,
                        &self.baseline_definition,
                        &self.baseline_attempt,
                        &self.baseline_effort,
                    ),
                    (
                        "candidate",
                        ExperimentArmV1::Candidate,
                        &candidate_package,
                        candidate_binding,
                        &candidate_definition,
                        &self.candidate_attempt,
                        &self.candidate_effort,
                    ),
                ] {
                    let node = format!(
                        "{}.case{}_rep{}_{}",
                        self.parent, case_index, repetition, label
                    );
                    if configuration.is_some() && !definition.contract.inputs.contains_key("case") {
                        return Err(
                            "Configured experimental Workers must consume the exact case input"
                                .into(),
                        );
                    }
                    let inputs = definition
                        .contract
                        .inputs
                        .keys()
                        .map(|name| {
                            if name == "case" {
                                let config = configuration
                                    .ok_or("Case input requires captured configuration")?;
                                let family = &experiment_policy.cases[case_index].family;
                                let id = config
                                    .case_inputs
                                    .get(family)
                                    .ok_or("Missing captured case artifact")?;
                                return Ok((
                                    name.clone(),
                                    ArtifactInputV1 {
                                        artifact_ids: vec![id.clone()],
                                        artifact_type: "af/OptimizationCase@1".into(),
                                        cardinality: PortCardinality::One,
                                        snapshot_id: None,
                                    },
                                ));
                            }
                            if name == "fixture" {
                                let config = configuration
                                    .ok_or("Measured Worker requires a prepared configuration")?;
                                let snapshot = if arm == ExperimentArmV1::Baseline {
                                    &config.baseline_fixture_id
                                } else {
                                    &config.candidate_fixture_id
                                };
                                return configuration::source_port(
                                    cas,
                                    snapshot,
                                    invocation_producer(cas, parent, None)?,
                                    parent
                                        .inputs
                                        .values()
                                        .flat_map(|p| p.artifact_ids.clone())
                                        .collect(),
                                )
                                .map(|value| (name.clone(), value));
                            }
                            parent
                                .inputs
                                .get(name)
                                .cloned()
                                .map(|value| (name.clone(), value))
                                .ok_or_else(|| {
                                    format!("Measured Worker requires unavailable input {name}")
                                })
                        })
                        .collect::<Result<BTreeMap<_, _>, _>>()?;
                    let invocation = TaskInvocationV1 {
                        plan_id: parent.plan_id.clone(),
                        node: node.clone(),
                        inputs,
                    };
                    invocation.validate()?;
                    let invocation_id = cas
                        .put_artifact(
                            review_core::task::execution::TASK_INVOCATION_V1,
                            invocation_producer(cas, parent, None)?,
                            invocation
                                .inputs
                                .values()
                                .flat_map(|port| port.artifact_ids.iter().cloned())
                                .collect(),
                            None,
                            serde_json::to_value(&invocation).map_err(|error| error.to_string())?,
                        )
                        .map_err(|error| error.to_string())?
                        .0;
                    let worker_slot = if arm == ExperimentArmV1::Baseline {
                        &self.baseline_slot
                    } else {
                        &self.candidate_slot
                    };
                    let max_attempts = self.graph.slots[worker_slot].max_attempts;
                    let tokens = attempt.tokens.max(1);
                    let allowance = review_attempt::task_budget::NodeAllowance {
                        tokens_per_attempt: tokens,
                        wall_ms_per_attempt: attempt.wall_ms,
                        max_attempts,
                        verification_attempts: max_attempts,
                    };
                    planned.insert(
                        node.clone(),
                        review_graph::task::ExperimentPlannedChildV1 {
                            definition: definition.clone(),
                            invocation,
                            allowance: allowance.clone(),
                        },
                    );
                    children.push(ExperimentChildClosureV1 {
                        node,
                        arm,
                        case_id: case.case_id.clone(),
                        repetition,
                        task_kind: "optimize".into(),
                        package: package.clone(),
                        worker_package_id: if arm == ExperimentArmV1::Candidate {
                            candidate_execution_authority.clone()
                        } else {
                            binding.package_artifact_id.clone()
                        },
                        effort: effort.clone(),
                        effects: BTreeSet::new(),
                        source_snapshot_id: source_snapshot.clone(),
                        requirements_id: requirements.artifact_ids[0].clone(),
                        authority_id: if arm == ExperimentArmV1::Baseline {
                            baseline_execution_authority.clone()
                        } else {
                            candidate_execution_authority.clone()
                        },
                        invocation_id,
                        allowance: ExperimentAllowanceV1 {
                            tokens,
                            attempts: max_attempts,
                            wall_ms: attempt.wall_ms,
                        },
                    });
                }
            }
        }
        let child_plan = review_graph::task::ExperimentExecutionPlanV1 {
            schema: "af.experiment-execution-plan/1".into(),
            parent_node: self.parent.clone(),
            children: planned,
        };
        let child_plan_id = cas
            .put_artifact(
                review_graph::task::EXPERIMENT_EXECUTION_PLAN_V1,
                invocation_producer(cas, parent, None)?,
                children
                    .iter()
                    .map(|child| child.invocation_id.clone())
                    .collect(),
                None,
                serde_json::to_value(&child_plan).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?
            .0;
        let prepared = ExperimentPreparedV1 {
            schema: "af.experiment-prepared/1".into(),
            task_revision_id: self.plan.task_revision_id.clone(),
            outer_plan_id: parent.plan_id.clone(),
            slot_id,
            specification_id,
            compiled_child_plan_id: child_plan_id,
            policy_id: slot.policy_id,
            spent_accounting_prefix_id: self.plan.task_revision_id.clone(),
            writer_epoch,
            children,
        };
        prepared.validate()?;
        let prepared_id = cas
            .put_artifact(
                EXPERIMENT_PREPARED_V1,
                invocation_producer(cas, parent, None)?,
                vec![
                    prepared.task_revision_id.clone(),
                    prepared.outer_plan_id.clone(),
                    prepared.slot_id.clone(),
                    prepared.specification_id.clone(),
                    prepared.compiled_child_plan_id.clone(),
                ],
                None,
                serde_json::to_value(&prepared).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?
            .0;
        Ok(TaskExperimentInputs { prepared_id })
    }

    fn complete_experiment(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        experiment: &review_store::store::task::execution::experiment::RegisteredTaskExperiment,
        facts: &[review_store::store::task::execution::experiment::ExperimentChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let specification: ExperimentSpecificationV1 = read(
            cas,
            &experiment.prepared.specification_id,
            EXPERIMENT_SPECIFICATION_V1,
        )?;
        let cases = specification
            .cases
            .iter()
            .map(|case| (&case.case_id, case))
            .collect::<BTreeMap<_, _>>();
        let mut refs = vec![
            experiment.prepared_id.clone(),
            experiment.prepared.specification_id.clone(),
        ];
        let mut trials = Vec::new();
        for fact in facts {
            if fact.selected_output_id.is_some() || fact.published_output_id.is_some() {
                refs.push(Self::selected_output(cas, fact)?.into());
            }
            let planned = experiment
                .child_plan
                .children
                .get(&fact.closure.node)
                .ok_or("Measured trial has no registered executable node")?;
            if !matches!(
                planned.definition.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::Verify { .. } | TaskOperatorV1::FixVerify { .. },
                    ..
                }
            ) || planned.allowance.verification_attempts == 0
            {
                return Err(
                    "Measured trial result requires a protected registered verifier slot".into(),
                );
            }
            let case = cases
                .get(&fact.closure.case_id)
                .ok_or("Registered child names an unknown case")?;
            let succeeded = fact.selected_output_id.is_some()
                && fact.published_output_id == fact.selected_output_id
                && fact.attempts.iter().any(|attempt| {
                    matches!(
                        attempt.result,
                        Some(review_core::task::execution::TaskAttemptResultV1::Succeeded { .. })
                    )
                });
            let billing_complete = !fact.attempts.is_empty()
                && fact
                    .attempts
                    .iter()
                    .map(|attempt| {
                        attempt
                            .billing_complete(cas, fact.closure.effort != "command")
                            .map_err(|e| e.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .all(|complete| complete);
            let charged = fact.attempts.iter().try_fold(0u64, |total, attempt| {
                total
                    .checked_add(
                        u64::try_from(attempt.charged_tokens)
                            .map_err(|_| "Experiment charge exceeds u64")?,
                    )
                    .ok_or("Experiment charge overflow")
            })?;
            let measured =
                review_store::store::task::execution::experiment::experiment_measured_costs(
                    cas,
                    &fact.attempts,
                )
                .map_err(|error| error.to_string())?;
            trials.push(ExperimentTrialV1 {
                invocation_id: fact.closure.invocation_id.clone(),
                case_id: fact.closure.case_id.clone(),
                family_id: case.family_id.clone(),
                arm: fact.closure.arm,
                repetition: fact.closure.repetition,
                compatibility_id: case.compatibility_id.clone(),
                verified: succeeded,
                protected_checks_passed: succeeded,
                billing_complete,
                charged_tokens: charged,
                elapsed_ms: measured.elapsed_ms,
                preparation_ms: measured.preparation_ms,
                cache_population_ms: measured.cache_population_ms,
                cache_lookup_ms: measured.cache_lookup_ms,
                cache_copy_ms: measured.cache_copy_ms,
                missing_measurements: measured.missing_measurements,
                intervals: measured.intervals,
            });
        }
        let comparison = compare_experiment(
            &experiment.prepared.specification_id,
            &experiment.prepared_id,
            &specification,
            trials,
        )?;
        comparison.validate()?;
        let mut producer = invocation_producer(cas, parent, None)?;
        if let Producer::KernelOperation { operation_id, .. } = &mut producer {
            *operation_id = "optimization-comparison-v1".into();
        }
        let comparison_id = cas
            .put_artifact(
                EXPERIMENT_COMPARISON_V1,
                producer,
                refs,
                None,
                serde_json::to_value(comparison).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?
            .0;
        Ok(BTreeMap::from([(
            "comparison".into(),
            ArtifactInputV1 {
                artifact_ids: vec![comparison_id],
                artifact_type: EXPERIMENT_COMPARISON_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: None,
            },
        )]))
    }

    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        if !feedback.is_empty() {
            return Err("Pure optimizer steps have no retry context".into());
        }
        cas.put_artifact(
            "af/OptimizationContext@1",
            invocation_producer(cas, input, None)?,
            input
                .inputs
                .values()
                .flat_map(|p| p.artifact_ids.clone())
                .collect(),
            None,
            serde_json::json!({"invocation":input}),
        )
        .map(|(id, _)| id)
        .map_err(|e| e.to_string())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        TaskWorkOutput {
            outputs: self.pure_outputs(cas, input),
            usage_observation: None,
            usage: None,
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        }
    }
}

impl TaskDomain for OptimizationCandidateTaskDomain {
    fn validate_experiment_preparation(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        prepared: &ExperimentPreparedV1,
    ) -> Result<(), String> {
        prepared.validate()?;
        let child_plan: review_graph::task::ExperimentExecutionPlanV1 = read(
            cas,
            &prepared.compiled_child_plan_id,
            review_graph::task::EXPERIMENT_EXECUTION_PLAN_V1,
        )?;
        let child_outer_plan = child_plan
            .children
            .values()
            .next()
            .map(|child| &child.invocation.plan_id);
        if prepared.task_revision_id != plan.task_revision_id
            || child_outer_plan != Some(&prepared.outer_plan_id)
            || child_plan.parent_node != self.parent
            || self.graph.experimental_slots[&self.parent].slot_id != prepared.slot_id
        {
            return Err("Candidate experiment differs from its captured Task, plan or slot".into());
        }
        Ok(())
    }

    fn assemble_result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let execution = state
            .execution
            .as_ref()
            .ok_or("Optimization Task has no execution")?;
        let outputs: BTreeMap<String, ArtifactInputV1> = self
            .graph
            .outputs
            .iter()
            .filter_map(|(name, address)| {
                execution
                    .outputs
                    .get(&address.node)
                    .and_then(|(_, output)| output.outputs.get(&address.port))
                    .map(|port| (name.clone(), port.clone()))
            })
            .collect();
        let mut result = TaskResultV1 {
            task_revision_id: state.revision_id.clone(),
            execution: if report.outcomes.iter().any(|(node, o)| {
                self.graph.nodes.contains_key(node) && matches!(o, NodeOutcome::Failed { .. })
            }) {
                TaskExecutionV1::Exhausted
            } else {
                TaskExecutionV1::Completed
            },
            acceptance: TaskAcceptanceV1::Inconclusive,
            domain_conclusion: "comparison_ready_finalization_missing".into(),
            evidence: self
                .graph
                .coverage
                .values()
                .filter_map(|a| {
                    execution
                        .outputs
                        .get(&a.node)
                        .and_then(|(_, o)| o.outputs.get(&a.port))
                })
                .flat_map(|p| p.artifact_ids.clone())
                .collect(),
            outputs,
            missing_obligations: state.revision.acceptance.keys().cloned().collect(),
        };
        self.attach_exact_light_result(cas, state, &mut result)?;
        self.assess_candidate(cas, &state.revision, &mut result)?;
        if result.acceptance == TaskAcceptanceV1::Satisfied {
            review_store::store::task::validate_optimization_delivery(cas, state, &result)
                .map_err(|e| e.to_string())?;
        }
        result.validate()?;
        Ok(result)
    }

    fn validate_context(
        &self,
        _: &Cas,
        input: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        if input.node == self.parent {
            Err("Optimization coordinator has no Attempt context".into())
        } else {
            Ok(())
        }
    }

    fn validate_output(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        if input.node != self.parent {
            if self.graph.nodes.get(&input.node).is_some_and(|node| {
                matches!(
                    node.operator,
                    CompiledOperator::Primitive {
                        operator: TaskOperatorV1::OptimizationProfile {}
                            | TaskOperatorV1::OptimizationPrepare {}
                            | TaskOperatorV1::OptimizationFinalize {},
                        ..
                    }
                )
            }) && self.pure_outputs(cas, input)? != output.outputs
            {
                return Err(
                    "Optimizer output differs from deterministic captured configuration".into(),
                );
            }
            return Ok(());
        }
        let port = output
            .outputs
            .get("comparison")
            .ok_or("Optimization coordinator omitted comparison")?;
        if port.artifact_type != EXPERIMENT_COMPARISON_V1 || port.artifact_ids.len() != 1 {
            return Err("Optimization coordinator changed its comparison contract".into());
        }
        let comparison: ExperimentComparisonV1 =
            read(cas, &port.artifact_ids[0], EXPERIMENT_COMPARISON_V1)?;
        comparison.validate()
    }

    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let mut expected = result.clone();
        self.assess_candidate(cas, task, &mut expected)?;
        if expected != *result {
            return Err("Optimization acceptance changed its exact verification evidence".into());
        }
        Ok(())
    }
}
