//! Exact installed Review plans over captured authority. Capture may write missing wrappers;
//! validation only reads existing artifacts and rederives the complete plan.
use super::*;
use review_config::captured_review::ReviewMode;
use review_core::task::plan::{
    EffectiveWorkerBindingV1, ExecutionPlanV1, GeneratedOriginV1, PlanDependencyV1,
    WorkerExecutionV1,
};
use review_core::task::{
    AcceptanceObligationV1, PipelineChoiceV1, PipelineFallbackV1, RequiredOutputV1,
    TaskAuthorityV1, TaskProvenanceV1, TaskRevisionV1,
};
use review_graph::task::{Address, CompiledOperator, OperatorAttemptCost, ReviewOperation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const REVIEW_TASK_POLICY_V1: &str = "af/LegacyReviewTaskPolicy@1";
pub const REVIEW_DEPENDENCY_V1: &str = "af/LegacyReviewDependency@1";
pub const REVIEW_INVOCATION_POLICY_V1: &str = "af/LegacyReviewInvocationPolicy@1";
const ROOT: &str = "af/legacy-review";

/// Trusted host choices captured once for the Task, including all later numeric Rounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPlanSettings {
    pub mode: String,
    pub resources: ReviewResourcePolicy,
    pub outputs: BTreeMap<String, Address>,
    /// Original Review node IDs; these are not Task-qualified slot names.
    pub executions: BTreeMap<String, WorkerExecutionV1>,
    pub provider_admission: OperatorAttemptCost,
    pub allowed_effects: BTreeSet<String>,
}
impl ReviewPlanSettings {
    fn mode(&self) -> Result<ReviewMode, String> {
        match self.mode.as_str() {
            "light" => Ok(ReviewMode::Light),
            "heavy" => Ok(ReviewMode::Heavy),
            _ => Err("Unknown captured Review mode".into()),
        }
    }
    fn validate(&self) -> Result<(), String> {
        self.mode()?;
        self.resources.validate()?;
        if self.outputs.is_empty()
            || self.provider_admission.tokens == 0
            || self.provider_admission.wall_ms == 0
            || [
                self.provider_admission.tokens,
                self.provider_admission.wall_ms,
            ]
            .iter()
            .any(|v| *v > review_core::json::SAFE_INTEGER_MAX as u64)
            || !self
                .allowed_effects
                .iter()
                .all(|name| review_core::task::is_name(name))
        {
            return Err(
                "Review plan needs explicit outputs, bounded Provider admission and named effects"
                    .into(),
            );
        }
        for execution in self.executions.values() {
            execution.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewTaskPolicy {
    engine_id: String,
    campaign_manifest_id: String,
    settings: ReviewPlanSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewDependency {
    name: String,
    campaign_manifest_id: String,
    pipeline_id: String,
    lock_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    review_node: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    original_package_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    original_package_digest: Option<String>,
    files: BTreeMap<String, String>,
}
impl ReviewDependency {
    fn refs(&self) -> Vec<String> {
        let mut refs = BTreeSet::from([
            self.campaign_manifest_id.clone(),
            self.pipeline_id.clone(),
            self.lock_id.clone(),
        ]);
        refs.extend(self.original_package_id.clone());
        refs.extend(self.files.values().cloned());
        refs.into_iter().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewInvocationPolicy {
    task_policy_id: String,
    dependency_id: String,
    review_node: String,
    /// Exact typed values are rederived from the dependency's original captured files.
    runner_digest: String,
    execution_policy_digest: String,
    execution: WorkerExecutionV1,
    result_type: String,
    timeout_ms: u64,
}

pub struct LegacyReviewPlanCompiler {
    round: CapturedLegacyReviewRound,
    policy_id: String,
    policy: ReviewTaskPolicy,
}

impl LegacyReviewPlanCompiler {
    /// Called only by the trusted host while preparing the original Task policy.
    pub fn capture(
        cas: &Cas,
        round: CapturedLegacyReviewRound,
        engine_id: String,
        settings: ReviewPlanSettings,
    ) -> Result<Self, String> {
        settings.validate()?;
        cas.verify(&engine_id).map_err(|e| e.to_string())?;
        let policy = ReviewTaskPolicy {
            engine_id,
            campaign_manifest_id: round.binding().campaign_manifest_id,
            settings,
        };
        let envelope = capture_or_read(
            cas,
            REVIEW_TASK_POLICY_V1,
            "policy",
            vec![
                policy.engine_id.clone(),
                policy.campaign_manifest_id.clone(),
            ],
            &policy,
            None,
        )?;
        Self::reopen(cas, round, &policy.engine_id, &envelope.artifact_id)
    }

    /// The caller obtains this exact policy ID from its trusted Task authority. Reading a
    /// serialized plan or a Worker result alone cannot construct a host execution capability.
    pub fn reopen(
        cas: &Cas,
        round: CapturedLegacyReviewRound,
        engine_id: &str,
        policy_id: &str,
    ) -> Result<Self, String> {
        let envelope = cas.get_artifact(policy_id).map_err(|e| e.to_string())?;
        let policy: ReviewTaskPolicy =
            serde_json::from_value(envelope.payload.clone()).map_err(|e| e.to_string())?;
        policy.settings.validate()?;
        if policy.engine_id != engine_id
            || policy.campaign_manifest_id != round.binding().campaign_manifest_id
        {
            return Err(
                "Review Task policy differs from the installed engine or captured Campaign".into(),
            );
        }
        capture_or_read(
            cas,
            REVIEW_TASK_POLICY_V1,
            "policy",
            vec![
                policy.engine_id.clone(),
                policy.campaign_manifest_id.clone(),
            ],
            &policy,
            Some(policy_id),
        )?;
        let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
            cas.get_json(&policy.campaign_manifest_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let loaded = review_config::captured_review::load_captured_review(
            cas,
            &manifest,
            policy.settings.mode()?,
        )?;
        let workers: BTreeSet<_> = loaded
            .planned()
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Reviewer | NodeKind::Scatter))
            .map(|(name, _)| name)
            .collect();
        if workers != policy.settings.executions.keys().collect() {
            return Err("Review executions differ from captured Worker nodes".into());
        }
        validate_executions(&loaded, &policy.settings.executions)?;
        Ok(Self {
            round,
            policy_id: policy_id.into(),
            policy,
        })
    }
    pub fn policy_id(&self) -> &str {
        &self.policy_id
    }
    pub fn round(&self) -> &CapturedLegacyReviewRound {
        &self.round
    }

    pub fn prepare_revision(
        &self,
        cas: &Cas,
        task_id: &str,
        limits: TaskLimitsV1,
    ) -> Result<TaskRevisionV1, String> {
        let inputs = self.round.capture_inputs(cas)?;
        let compiled = self.compile_graph(cas, inputs.clone(), limits.clone())?;
        let (required_outputs, acceptance) = self.contract(&compiled.compilation);
        let revision = TaskRevisionV1 {
            task_id: task_id.into(),
            revision: 1,
            previous_revision_id: None,
            kind: "review/legacy".into(),
            goal: "Review the captured Subject through the configured Review Pipeline".into(),
            inputs,
            required_outputs,
            acceptance,
            provenance: TaskProvenanceV1 {
                adapter_id: self.policy.engine_id.clone(),
                input_artifact_ids: vec![self.policy.campaign_manifest_id.clone()],
            },
            authority: self.authority(),
            limits,
            strategy: self.policy.settings.mode.clone(),
            pipeline: Some(PipelineChoiceV1 {
                name: ROOT.into(),
                fallback: PipelineFallbackV1::Refuse,
            }),
            facts: BTreeMap::new(),
        };
        revision.validate()?;
        Ok(revision)
    }

    fn authority(&self) -> TaskAuthorityV1 {
        TaskAuthorityV1 {
            policy_id: self.policy_id.clone(),
            allowed_effects: self.policy.settings.allowed_effects.clone(),
            data_destinations: self
                .policy
                .settings
                .executions
                .values()
                .filter_map(|execution| match execution {
                    WorkerExecutionV1::Model { provider, .. } => Some(provider.clone()),
                    _ => None,
                })
                .collect(),
        }
    }

    fn compile_graph(
        &self,
        cas: &Cas,
        inputs: BTreeMap<String, ArtifactInputV1>,
        limits: TaskLimitsV1,
    ) -> Result<CapturedReviewCompilation, String> {
        let mut captured = self.round.compile_existing(
            cas,
            self.policy.settings.mode()?,
            &self.policy.settings.resources,
            ReviewCompilationRequest {
                limits,
                inputs,
                outputs: self.policy.settings.outputs.clone(),
            },
        )?;
        let compiled = &mut captured.compilation;
        // Every reviewer receipt and executed Gate/Scatter is an explicit acceptance input,
        // including nodes that the original Ledger did not consume.
        let mut evidence = BTreeMap::new();
        for mapping in compiled.nodes.values() {
            let node = &compiled.graph.nodes[&mapping.task_node];
            let port = match &node.operator {
                CompiledOperator::ReviewDomain {
                    operation: ReviewOperation::Reviewer { .. },
                    ..
                } => Some("metadata"),
                CompiledOperator::ReviewDomain {
                    operation: ReviewOperation::Gate,
                    ..
                } => Some("outcome"),
                CompiledOperator::ReviewDomain {
                    operation: ReviewOperation::Scatter { .. },
                    ..
                } => Some("o0"),
                _ => None,
            };
            if let Some(port) = port {
                let name = format!(
                    "af_evidence_{}",
                    mapping.task_node.rsplit('.').next().unwrap()
                );
                if compiled.graph.outputs.contains_key(&name) {
                    return Err("Public Review output collides with installed evidence port".into());
                }
                evidence.insert(
                    name,
                    Address {
                        node: mapping.task_node.clone(),
                        port: port.into(),
                    },
                );
            }
        }
        compiled.graph.outputs.extend(evidence);
        compiled.graph.coverage = compiled.graph.outputs.clone();
        let root = compiled
            .graph
            .calls
            .get_mut("root")
            .ok_or("Missing Review root call")?;
        root.outputs = compiled.graph.outputs.clone();
        root.coverage = compiled.graph.coverage.clone();
        for (name, address) in &compiled.graph.outputs {
            let mut port =
                compiled.graph.nodes[&address.node].contract.outputs[&address.port].clone();
            if matches!(
                port.affinity,
                review_core::task::pipeline::PortAffinityV1::SameAs { .. }
            ) {
                port.affinity = review_core::task::pipeline::PortAffinityV1::SameAs {
                    input: "head".into(),
                };
            }
            port.covers.insert(name.clone());
            compiled.contract.outputs.insert(name.clone(), port);
        }
        compiled.contract.validate()?;
        Ok(captured)
    }

    fn contract(
        &self,
        compiled: &LegacyReviewCompilation,
    ) -> (
        BTreeMap<String, RequiredOutputV1>,
        BTreeMap<String, AcceptanceObligationV1>,
    ) {
        let outputs = compiled
            .contract
            .outputs
            .iter()
            .map(|(name, port)| {
                (
                    name.clone(),
                    RequiredOutputV1 {
                        artifact_type: port.artifact_type.clone(),
                        cardinality: port.cardinality,
                    },
                )
            })
            .collect();
        let acceptance = compiled
            .contract
            .outputs
            .iter()
            .map(|(name, port)| {
                (
                    name.clone(),
                    AcceptanceObligationV1 {
                        evidence_type: port.artifact_type.clone(),
                        verifier_policy: self.policy_id.clone(),
                    },
                )
            })
            .collect();
        (outputs, acceptance)
    }

    pub fn compile(
        &self,
        cas: &Cas,
        revision_id: &str,
    ) -> Result<(ExecutionPlanV1, CapturedReviewCompilation), String> {
        self.compile_inner(cas, revision_id, None)
    }

    fn compile_inner(
        &self,
        cas: &Cas,
        revision_id: &str,
        recorded: Option<&ExecutionPlanV1>,
    ) -> Result<(ExecutionPlanV1, CapturedReviewCompilation), String> {
        // Check policy bytes on every admission/resume, even when the compiler stays in memory.
        capture_or_read(
            cas,
            REVIEW_TASK_POLICY_V1,
            "policy",
            vec![
                self.policy.engine_id.clone(),
                self.policy.campaign_manifest_id.clone(),
            ],
            &self.policy,
            Some(&self.policy_id),
        )?;
        let revision = cas.get_artifact(revision_id).map_err(|e| e.to_string())?;
        if revision.artifact_type != review_core::task::TASK_REVISION_V1 {
            return Err("Expected captured Task revision".into());
        }
        let task: TaskRevisionV1 =
            serde_json::from_value(revision.payload).map_err(|e| e.to_string())?;
        task.validate()?;
        let mut captured = self.compile_graph(cas, task.inputs.clone(), task.limits.clone())?;
        let (outputs, acceptance) = self.contract(&captured.compilation);
        if task.kind != "review/legacy"
            || task.authority != self.authority()
            || task.required_outputs != outputs
            || task.acceptance != acceptance
            || task.strategy != self.policy.settings.mode
            || task.provenance.adapter_id != self.policy.engine_id
            || task.provenance.input_artifact_ids != [self.policy.campaign_manifest_id.clone()]
            || task.pipeline
                != Some(PipelineChoiceV1 {
                    name: ROOT.into(),
                    fallback: PipelineFallbackV1::Refuse,
                })
            || !task.facts.is_empty()
        {
            return Err("Task differs from captured Review policy, contract or provenance".into());
        }
        let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
            cas.get_json(&self.policy.campaign_manifest_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut dependencies = BTreeMap::new();
        let root = ReviewDependency {
            name: ROOT.into(),
            campaign_manifest_id: self.policy.campaign_manifest_id.clone(),
            pipeline_id: manifest.pipeline.artifact_id.clone(),
            lock_id: manifest.reviewer_lock.artifact_id.clone(),
            review_node: None,
            original_package_id: None,
            original_package_digest: None,
            files: BTreeMap::new(),
        };
        let root = dependency(cas, &root, recorded)?;
        let pipeline_id = root.artifact_id.clone();
        dependencies.insert(ROOT.into(), root);
        let mut bindings = BTreeMap::new();
        for (review_node, mapping) in &captured.compilation.nodes {
            let graph = &captured.compilation.graph;
            let slot = match &graph.nodes[&mapping.task_node].operator {
                CompiledOperator::ReviewDomain {
                    operation:
                        ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
                    ..
                } => slot,
                _ => continue,
            };
            let original = manifest
                .reviewers
                .iter()
                .find(|package| &package.node == review_node);
            let files = if let Some(original) = original {
                let package: review_core::ReviewerPackageV1 = serde_json::from_value(
                    cas.get_json(&original.package_artifact_id)
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                package.files
            } else {
                BTreeMap::from([(
                    manifest.pipeline.path.clone(),
                    manifest.pipeline.artifact_id.clone(),
                )])
            };
            let package = ReviewDependency {
                name: graph.slots[slot].worker.clone(),
                campaign_manifest_id: self.policy.campaign_manifest_id.clone(),
                pipeline_id: manifest.pipeline.artifact_id.clone(),
                lock_id: manifest.reviewer_lock.artifact_id.clone(),
                review_node: Some(review_node.clone()),
                original_package_id: original.map(|p| p.package_artifact_id.clone()),
                original_package_digest: original.map(|p| p.digest.clone()),
                files,
            };
            let package = dependency(cas, &package, recorded)?;
            let policy = ReviewInvocationPolicy {
                task_policy_id: self.policy_id.clone(),
                dependency_id: package.artifact_id.clone(),
                review_node: review_node.clone(),
                runner_digest: review_store::content_id(
                    &serde_json::to_value(&captured.loaded.reviewers()[review_node])
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?,
                execution_policy_digest: review_store::content_id(
                    &serde_json::to_value(captured.loaded.reviewer_execution().get(review_node))
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?,
                execution: self.policy.settings.executions[review_node].clone(),
                result_type: graph.slots[slot].output_type.clone(),
                timeout_ms: graph.allowances[&mapping.task_node].wall_ms_per_attempt,
            };
            let recorded_policy = recorded
                .map(|plan| {
                    plan.bindings
                        .get(slot)
                        .map(|binding| binding.invocation_policy_id.as_str())
                        .ok_or("Recorded Review plan lacks a Worker binding")
                })
                .transpose()?;
            let policy = capture_or_read(
                cas,
                REVIEW_INVOCATION_POLICY_V1,
                "invocation",
                vec![self.policy_id.clone(), package.artifact_id.clone()],
                &policy,
                recorded_policy,
            )?;
            bindings.insert(
                slot.clone(),
                EffectiveWorkerBindingV1 {
                    package_digest: package.content_digest.clone(),
                    package_artifact_id: package.artifact_id.clone(),
                    execution: self.policy.settings.executions[review_node].clone(),
                    invocation_policy_id: policy.artifact_id,
                },
            );
            dependencies.insert(package.name.clone(), package);
        }
        let graph = &mut captured.compilation.graph;
        graph.install_provider_admission(&bindings, &self.policy.settings.provider_admission)?;
        graph.budget(task.limits.clone())?;
        let graph = capture_or_read(
            cas,
            review_config::task::catalog::COMPILED_TASK_V1,
            "graph",
            vec![
                revision_id.into(),
                self.policy.engine_id.clone(),
                self.policy_id.clone(),
            ],
            graph,
            recorded.map(|plan| plan.compiled_graph_id.as_str()),
        )?;
        let plan = ExecutionPlanV1 {
            preparation: None,
            task_revision_id: revision_id.into(),
            engine_id: self.policy.engine_id.clone(),
            pipeline_id,
            compiled_graph_id: graph.artifact_id,
            authority: task.authority,
            limits: task.limits,
            inputs: task.inputs,
            bindings,
            dependencies,
            generated_origins: vec![],
            acceptance: captured
                .compilation
                .graph
                .coverage
                .iter()
                .map(|(name, address)| (name.clone(), BTreeSet::from([address.qualified()])))
                .collect(),
        };
        plan.validate()?;
        Ok((plan, captured))
    }

    pub fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        let revision = cas
            .get_artifact(&plan.task_revision_id)
            .map_err(|e| e.to_string())?;
        if serde_json::from_value::<TaskRevisionV1>(revision.payload).map_err(|e| e.to_string())?
            != *task
        {
            return Err("Review plan names a different Task revision".into());
        }
        let (expected, _) = self.compile_inner(cas, &plan.task_revision_id, Some(plan))?;
        if &expected != plan {
            return Err("Review plan differs from trusted captured recompilation".into());
        }
        Ok(vec![])
    }
}

fn dependency(
    cas: &Cas,
    value: &ReviewDependency,
    plan: Option<&ExecutionPlanV1>,
) -> Result<PlanDependencyV1, String> {
    let recorded = plan
        .map(|plan| {
            plan.dependencies
                .get(&value.name)
                .map(|d| d.artifact_id.as_str())
                .ok_or("Recorded Review plan lacks a dependency")
        })
        .transpose()?;
    let artifact = capture_or_read(
        cas,
        REVIEW_DEPENDENCY_V1,
        "dependency",
        value.refs(),
        value,
        recorded,
    )?;
    Ok(PlanDependencyV1 {
        name: value.name.clone(),
        content_digest: artifact.content_id,
        artifact_id: artifact.artifact_id,
    })
}

fn capture_or_read<T: Serialize>(
    cas: &Cas,
    ty: &str,
    operation: &str,
    refs: Vec<String>,
    payload: &T,
    recorded: Option<&str>,
) -> Result<ArtifactEnvelope, String> {
    for id in &refs {
        cas.verify(id).map_err(|e| e.to_string())?;
    }
    let producer = Producer::KernelOperation {
        run_id: "legacy-review-task-v1".into(),
        node_id: None,
        operation_id: format!("capture-{operation}@1"),
    };
    let payload = serde_json::to_value(payload).map_err(|e| e.to_string())?;
    if let Some(id) = recorded {
        let artifact = cas.get_artifact(id).map_err(|e| e.to_string())?;
        if artifact.artifact_type != ty
            || artifact.producer != producer
            || artifact.input_artifacts != refs
            || artifact.subject_snapshot_id.is_some()
            || artifact.payload != payload
        {
            return Err(format!(
                "Recorded {ty} differs from captured Review authority"
            ));
        }
        Ok(artifact)
    } else {
        cas.put_artifact(ty, producer, refs, None, payload)
            .map(|(_, artifact)| artifact)
            .map_err(|e| e.to_string())
    }
}

fn validate_executions(
    loaded: &review_config::Loaded,
    executions: &BTreeMap<String, WorkerExecutionV1>,
) -> Result<(), String> {
    for (node, execution) in executions {
        let runner = &loaded.reviewers()[node];
        let native = std::path::Path::new(&runner.program)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "claude" | "codex"));
        match execution {
            WorkerExecutionV1::Command {} if native => {
                return Err("Native Review model runner requires common Provider admission".into());
            }
            WorkerExecutionV1::Command {} => {}
            WorkerExecutionV1::Model {
                provider_kind,
                model,
                effort,
                ..
            } => {
                if !native {
                    return Err("Model binding differs from captured Review command runner".into());
                }
                let runner = serde_json::from_value(
                    serde_json::to_value(runner).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let manifest = review_config::lock::PackageManifest {
                    name: "af/captured-review-runner".into(),
                    version: "1.0.0".into(),
                    subjects: vec![loaded.subject_kind()],
                    runner,
                };
                let settings =
                    review_config::lock::reviewer_runner_settings_from_manifest(&manifest)
                        .map_err(|e| e.to_string())?;
                if settings.backend.to_string() != *provider_kind
                    || settings.model != *model
                    || settings.effort != *effort
                {
                    return Err(
                        "Model binding changes the captured Review backend, model or effort".into(),
                    );
                }
            }
        }
    }
    Ok(())
}
