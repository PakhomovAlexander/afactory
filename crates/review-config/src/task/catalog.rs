//! Captured Task packages. The caller supplies files from the trusted project Snapshot and
//! exact reviewed pins. Parsing happens only after checking the digest of those same bytes.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::pipeline::{PipelineDefinitionV1, TaskOperatorV1};
use review_core::task::plan::{
    EffectiveWorkerBindingV1, ExecutionPlanV1, GeneratedOriginV1, IndependencePolicyV1,
    PlanDependencyV1, WorkerExecutionV1, validate_independent_bindings,
};
use review_core::task::{TaskRevisionV1, is_package_name};
use review_core::{ArtifactEnvelope, Producer};
use review_graph::task::{
    CompileContext, CompiledOperator, CompiledTask, OperatorSignature, compile_task,
};
use review_store::{Cas, validate_envelope};
use serde::{Deserialize, Serialize};

use crate::{CommandSpec, lock::package_digest_from_files};

pub const TASK_PACKAGE_V1: &str = "af/TaskPackage@1";
pub const COMPILED_TASK_V1: &str = "af/CompiledTask@1";

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPackagePin {
    pub version: String,
    pub digest: String,
    /// Relative package root in an already captured project file set.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWorkerManifest {
    pub schema: String,
    pub name: String,
    pub version: String,
    pub signature: OperatorSignature,
    pub runner: TaskWorkerRunner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskWorkerRunner {
    Command {
        command: CommandSpec,
    },
    LegacyTaskCommand {
        command: CommandSpec,
        protocol: review_runner::task::legacy::LegacyTaskProtocol,
    },
    Model {
        provider_kind: String,
        model: String,
        effort: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageBytes {
    schema: String,
    name: String,
    version: String,
    digest: String,
    files: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone)]
struct Package {
    bytes: PackageBytes,
    dependency: PlanDependencyV1,
}

/// Resolved by the host's Provider and invocation-policy admission, never by a Worker or a
/// plan declaration. The principal in Model execution must come from Provider identity proof.
#[derive(Debug, Clone)]
pub struct AdmittedWorkerSettings {
    pub execution: WorkerExecutionV1,
    pub invocation_policy_id: String,
}

/// Immutable plan compiler rooted in reviewed package pins and installed operator signatures.
/// It cannot be deserialized from a model response. Resume reconstructs it from captured
/// authority and calls validate_plan again instead of trusting a serialized compiled graph.
#[derive(Debug, Clone)]
pub struct TaskPlanCompiler {
    engine_id: String,
    policy_id: String,
    packages: BTreeMap<String, Package>,
    pipelines: BTreeMap<String, PipelineDefinitionV1>,
    workers: BTreeMap<String, TaskWorkerManifest>,
    signatures: BTreeMap<String, OperatorSignature>,
    settings: BTreeMap<String, AdmittedWorkerSettings>,
    generated: BTreeMap<String, GeneratedOriginV1>,
    acceptance_outputs: BTreeMap<String, String>,
    independence: IndependencePolicyV1,
    provider_admission: Option<review_graph::task::OperatorAttemptCost>,
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.chars().any(char::is_control))
}

fn exact_version(version: &str) -> bool {
    let fields: Vec<_> = version.split('.').collect();
    fields.len() == 3
        && fields
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn capture_producer() -> Producer {
    Producer::KernelOperation {
        run_id: "task-catalog-v1".into(),
        node_id: None,
        operation_id: "capture-package@1".into(),
    }
}

fn read_envelope(cas: &Cas, id: &str, expected: &str) -> Result<ArtifactEnvelope, String> {
    let envelope: ArtifactEnvelope =
        serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    validate_envelope(&envelope)?;
    if envelope.artifact_id != id || envelope.artifact_type != expected {
        return Err(format!("Expected exact {expected} envelope"));
    }
    Ok(envelope)
}

impl TaskPlanCompiler {
    /// A production host enables this when a fresh paid capability probe is required.
    /// Identity-only probes occur before planning; these model calls belong to Task execution.
    pub fn with_provider_admission(
        mut self,
        cost: review_graph::task::OperatorAttemptCost,
    ) -> Self {
        self.provider_admission = Some(cost);
        self
    }
    /// Apply only constructors explicitly declared on this root contract. A child call has
    /// no access to this adapter operation and must bind every required input itself.
    pub fn normalize_root_inputs(
        &self,
        cas: &Cas,
        root: &str,
        mut inputs: BTreeMap<String, review_core::task::ArtifactInputV1>,
    ) -> Result<BTreeMap<String, review_core::task::ArtifactInputV1>, String> {
        use review_core::task::pipeline::RootDefaultV1;
        let pipeline = self.pipelines.get(root).ok_or("Unknown root Pipeline")?;
        if inputs
            .keys()
            .any(|name| !pipeline.contract.inputs.contains_key(name))
        {
            return Err("Task binds an undeclared root input".into());
        }
        for (name, port) in &pipeline.contract.inputs {
            if inputs.contains_key(name) {
                continue;
            }
            if let Some(RootDefaultV1::EmptyReviewHistory) = port.root_default {
                let id = cas
                    .put_artifact(
                        review_core::task::REVIEW_HISTORY_V1,
                        capture_producer(),
                        vec![],
                        None,
                        serde_json::to_value(review_core::task::review::ReviewHistoryV1::Empty {})
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?
                    .0;
                inputs.insert(
                    name.clone(),
                    review_core::task::ArtifactInputV1 {
                        artifact_ids: vec![id],
                        artifact_type: review_core::task::REVIEW_HISTORY_V1.into(),
                        cardinality: review_core::PortCardinality::One,
                        snapshot_id: None,
                    },
                );
            } else if !port.optional {
                return Err(format!(
                    "Required Task input {name} has no declared root constructor"
                ));
            }
        }
        Ok(inputs)
    }

    /// Installed operators are supplied by the engine, not loaded from Pipeline source.
    pub fn new(
        engine_id: String,
        policy_id: String,
        installed: BTreeMap<String, OperatorSignature>,
        acceptance_outputs: BTreeMap<String, String>,
        independence: IndependencePolicyV1,
    ) -> Result<Self, String> {
        if !review_core::is_digest(&engine_id)
            || !review_core::is_digest(&policy_id)
            || installed.keys().any(|key| !key.starts_with("operator/"))
        {
            return Err("Task compiler needs exact trusted engine and policy identities".into());
        }
        for signature in installed.values() {
            signature.contract.validate()?;
        }
        Ok(Self {
            engine_id,
            policy_id,
            packages: BTreeMap::new(),
            pipelines: BTreeMap::new(),
            workers: BTreeMap::new(),
            signatures: installed,
            settings: BTreeMap::new(),
            generated: BTreeMap::new(),
            acceptance_outputs,
            independence,
            provider_admission: None,
        })
    }

    /// Capture a project-local package from one immutable file map. This shares the release
    /// lock resolver's byte digest algorithm; no manifest is read from the live working tree.
    pub fn capture_package(
        &mut self,
        cas: &Cas,
        name: &str,
        pin: &TaskPackagePin,
        project: &BTreeMap<String, Vec<u8>>,
    ) -> Result<String, String> {
        if !is_package_name(name)
            || !safe_path(&pin.path)
            || !exact_version(&pin.version)
            || !review_core::is_digest(&pin.digest)
        {
            return Err("Task package has an invalid exact pin".into());
        }
        let prefix = format!("{}/", pin.path);
        let files: BTreeMap<_, _> = project
            .iter()
            .filter_map(|(path, bytes)| {
                path.strip_prefix(&prefix)
                    .map(|relative| (relative.to_owned(), bytes.clone()))
            })
            .collect();
        if files.is_empty()
            || files.len() > 4096
            || files.keys().any(|path| !safe_path(path))
            || files
                .values()
                .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
                .is_none_or(|bytes| bytes > 16 * 1024 * 1024)
        {
            return Err("Task package is missing or exceeds safe capture bounds".into());
        }
        if package_digest_from_files(&files) != pin.digest {
            return Err(format!("Task package {name} changed since it was locked"));
        }
        let bytes = PackageBytes {
            schema: "af.task-package/1".into(),
            name: name.into(),
            version: pin.version.clone(),
            digest: pin.digest.clone(),
            files,
        };
        // Validate before publishing even an unreachable captured artifact.
        Self::parse_package(&bytes)?;
        let (id, _) = cas
            .put_artifact(
                TASK_PACKAGE_V1,
                capture_producer(),
                vec![],
                None,
                serde_json::to_value(&bytes).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        self.restore_package(cas, name, &pin.digest, &id)?;
        Ok(id)
    }

    /// The artifact ID and digest must come from captured trusted policy, not the proposed
    /// ExecutionPlan. This permits replay without consulting mutable registries or files.
    pub fn restore_package(
        &mut self,
        cas: &Cas,
        name: &str,
        expected_digest: &str,
        artifact_id: &str,
    ) -> Result<(), String> {
        let envelope = read_envelope(cas, artifact_id, TASK_PACKAGE_V1)?;
        let bytes: PackageBytes =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        if bytes.name != name
            || bytes.digest != expected_digest
            || package_digest_from_files(&bytes.files) != expected_digest
        {
            return Err("Captured Task package does not match trusted authority".into());
        }
        let (pipeline, worker) = Self::parse_package(&bytes)?;
        if let Some(old) = self.packages.get(name) {
            if old.dependency.artifact_id == artifact_id {
                return Ok(());
            }
            return Err("Task package cannot change inside captured authority".into());
        }
        if let Some(pipeline) = pipeline {
            self.pipelines.insert(name.into(), pipeline);
        }
        if let Some(worker) = worker {
            self.signatures
                .insert(format!("worker/{name}"), worker.signature.clone());
            self.workers.insert(name.into(), worker);
        }
        self.packages.insert(
            name.into(),
            Package {
                bytes,
                dependency: PlanDependencyV1 {
                    name: name.into(),
                    content_digest: envelope.content_id,
                    artifact_id: artifact_id.into(),
                },
            },
        );
        Ok(())
    }

    fn parse_package(
        bytes: &PackageBytes,
    ) -> Result<(Option<PipelineDefinitionV1>, Option<TaskWorkerManifest>), String> {
        if bytes.schema != "af.task-package/1"
            || !is_package_name(&bytes.name)
            || !exact_version(&bytes.version)
            || bytes.files.keys().any(|path| !safe_path(path))
            || bytes.files.is_empty()
            || bytes.files.len() > 4096
            || bytes
                .files
                .values()
                .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
                .is_none_or(|total| total > 16 * 1024 * 1024)
        {
            return Err("Invalid captured Task package".into());
        }
        match (
            bytes.files.get("pipeline.toml"),
            bytes.files.get("worker.toml"),
        ) {
            (Some(source), None) => {
                let pipeline = super::parse_task_pipeline(
                    std::str::from_utf8(source).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                if pipeline.name != bytes.name || pipeline.version != bytes.version {
                    return Err("Pipeline manifest disagrees with its pin".into());
                }
                Ok((Some(pipeline), None))
            }
            (None, Some(source)) => {
                let worker: TaskWorkerManifest =
                    toml::from_str(std::str::from_utf8(source).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                if worker.schema != "af.worker/1"
                    || worker.name != bytes.name
                    || worker.version != bytes.version
                    || worker.signature.roles.is_empty()
                    || worker.signature.worker_input_type.is_none()
                    || worker.signature.worker_output_type.is_none()
                {
                    return Err("Worker manifest has incompatible identity or protocol".into());
                }
                worker.signature.contract.validate()?;
                let cost = worker
                    .signature
                    .attempt
                    .as_ref()
                    .ok_or("Worker signature requires bounded Attempt cost")?;
                if cost.wall_ms == 0 {
                    return Err("Worker Attempt wall limit is zero".into());
                }
                match &worker.runner {
                    TaskWorkerRunner::Command {command} | TaskWorkerRunner::LegacyTaskCommand {command, ..} if command.program.trim().is_empty() || cost.tokens != 0 => return Err("Command Worker requires a program and zero model-token reservation".into()),
                    TaskWorkerRunner::Model {provider_kind, model, effort} if !review_core::task::is_name(provider_kind) || model.trim().is_empty() || !review_core::task::is_name(effort) || cost.tokens == 0 => return Err("Model Worker needs explicit Provider/model/effort and token reservation".into()),
                    _ => (),
                }
                Ok((None, Some(worker)))
            }
            _ => Err("Task package must have exactly one pipeline.toml or worker.toml".into()),
        }
    }

    pub fn bind_worker(
        &mut self,
        name: &str,
        settings: AdmittedWorkerSettings,
    ) -> Result<(), String> {
        let worker = self
            .workers
            .get(name)
            .ok_or("Worker package is not captured")?;
        match (&worker.runner, &settings.execution) {
            (
                TaskWorkerRunner::Command { .. } | TaskWorkerRunner::LegacyTaskCommand { .. },
                WorkerExecutionV1::Command {},
            ) => (),
            (
                TaskWorkerRunner::Model {
                    provider_kind: wanted_kind,
                    model: wanted_model,
                    effort: wanted_effort,
                },
                WorkerExecutionV1::Model {
                    provider_kind,
                    model,
                    effort,
                    ..
                },
            ) if wanted_kind == provider_kind
                && wanted_model == model
                && wanted_effort == effort => {}
            _ => return Err("Provider admission disagrees with captured Worker settings".into()),
        }
        let package = &self.packages[name];
        EffectiveWorkerBindingV1 {
            package_digest: package.bytes.digest.clone(),
            package_artifact_id: package.dependency.artifact_id.clone(),
            execution: settings.execution.clone(),
            invocation_policy_id: settings.invocation_policy_id.clone(),
        }
        .validate()?;
        self.settings.insert(name.into(), settings);
        Ok(())
    }

    pub fn pipelines(&self) -> &BTreeMap<String, PipelineDefinitionV1> {
        &self.pipelines
    }
    pub fn worker(&self, name: &str) -> Option<&TaskWorkerManifest> {
        self.workers.get(name)
    }
    pub fn package_files(&self, name: &str) -> Option<&BTreeMap<String, Vec<u8>>> {
        self.packages.get(name).map(|package| &package.bytes.files)
    }

    pub fn compile(
        &self,
        cas: &Cas,
        task_revision_id: &str,
        root: &str,
    ) -> Result<(ExecutionPlanV1, CompiledTask), String> {
        self.compile_inner(cas, task_revision_id, root, None)
    }

    /// Pure preflight for the selected definition, before any account probes. Unused catalog
    /// Workers must not make an otherwise fitting Pipeline require unrelated credentials.
    pub fn required_worker_packages(
        &self,
        cas: &Cas,
        task_revision_id: &str,
        root: &str,
    ) -> Result<BTreeSet<String>, String> {
        let revision = read_envelope(cas, task_revision_id, review_core::task::TASK_REVISION_V1)?;
        let task: TaskRevisionV1 =
            serde_json::from_value(revision.payload).map_err(|e| e.to_string())?;
        if task.authority.policy_id != self.policy_id {
            return Err("Task does not belong to captured authority".into());
        }
        let graph = compile_task(
            &task,
            root,
            &CompileContext {
                pipelines: &self.pipelines,
                signatures: &self.signatures,
                acceptance_outputs: self.acceptance_outputs.clone(),
                max_nodes: 64,
                max_depth: 4,
            },
        )?;
        Ok(graph
            .slots
            .values()
            .map(|slot| slot.worker.clone())
            .collect())
    }

    fn compile_inner(
        &self,
        cas: &Cas,
        task_revision_id: &str,
        root: &str,
        recorded_graph: Option<&ArtifactEnvelope>,
    ) -> Result<(ExecutionPlanV1, CompiledTask), String> {
        let revision = read_envelope(cas, task_revision_id, review_core::task::TASK_REVISION_V1)?;
        let task: TaskRevisionV1 =
            serde_json::from_value(revision.payload).map_err(|e| e.to_string())?;
        if task.authority.policy_id != self.policy_id {
            return Err("Task does not belong to captured project authority".into());
        }
        cas.verify(&self.engine_id).map_err(|e| e.to_string())?;
        cas.verify(&self.policy_id).map_err(|e| e.to_string())?;
        let mut graph = compile_task(
            &task,
            root,
            &CompileContext {
                pipelines: &self.pipelines,
                signatures: &self.signatures,
                acceptance_outputs: self.acceptance_outputs.clone(),
                max_nodes: 64,
                max_depth: 4,
            },
        )?;
        let mut bindings = BTreeMap::new();
        let mut used: BTreeSet<String> = graph
            .calls
            .values()
            .map(|call| call.pipeline.clone())
            .collect();
        for (slot, declaration) in &graph.slots {
            let package = self
                .packages
                .get(&declaration.worker)
                .ok_or("Compiled Worker is not captured")?;
            let settings = self
                .settings
                .get(&declaration.worker)
                .ok_or("Worker lacks trusted runtime admission")?;
            cas.verify(&settings.invocation_policy_id)
                .map_err(|e| e.to_string())?;
            bindings.insert(
                slot.clone(),
                EffectiveWorkerBindingV1 {
                    package_digest: package.bytes.digest.clone(),
                    package_artifact_id: package.dependency.artifact_id.clone(),
                    execution: settings.execution.clone(),
                    invocation_policy_id: settings.invocation_policy_id.clone(),
                },
            );
            used.insert(declaration.worker.clone());
        }
        for (slot, declaration) in &graph.slots {
            for other in &declaration.independent_from {
                validate_independent_bindings(
                    &bindings[slot],
                    &bindings[other],
                    self.independence,
                )?;
            }
        }
        // A syntactically declared but unused slot cannot smuggle an unrelated package into
        // a Worker's context. Only used graph slots belong to executable authority.
        let used_slots: BTreeSet<_> = graph
            .nodes
            .values()
            .filter_map(|node| match &node.operator {
                CompiledOperator::Primitive {
                    operator:
                        TaskOperatorV1::Worker { slot }
                        | TaskOperatorV1::Verify { slot }
                        | TaskOperatorV1::FixVerify { slot },
                    ..
                } => Some(slot),
                _ => None,
            })
            .collect();
        if graph.slots.keys().any(|slot| !used_slots.contains(slot)) {
            return Err("Pipeline declares an unused Worker slot".into());
        }
        let dependencies: BTreeMap<_, _> = used
            .iter()
            .map(|name| (name.clone(), self.packages[name].dependency.clone()))
            .collect();
        for dependency in dependencies.values() {
            let current = read_envelope(cas, &dependency.artifact_id, TASK_PACKAGE_V1)?;
            if current.content_id != dependency.content_digest {
                return Err("Captured dependency changed".into());
            }
        }
        let generated_origins = used
            .iter()
            .filter_map(|name| self.generated.get(name).cloned())
            .collect();
        if let Some(cost) = &self.provider_admission {
            graph.require_provider_admission(&bindings, cost, &task.limits)?;
        }
        let graph_value = serde_json::to_value(&graph).map_err(|e| e.to_string())?;
        let compiled_graph_id = if let Some(recorded) = recorded_graph {
            if recorded.payload != graph_value
                || recorded.producer != capture_producer()
                || recorded.input_artifacts != [task_revision_id.to_owned(), self.engine_id.clone()]
                || recorded.subject_snapshot_id.is_some()
            {
                return Err("Recorded Task graph differs from trusted recompilation".into());
            }
            recorded.artifact_id.clone()
        } else {
            cas.put_artifact(
                COMPILED_TASK_V1,
                capture_producer(),
                vec![task_revision_id.into(), self.engine_id.clone()],
                None,
                graph_value,
            )
            .map_err(|e| e.to_string())?
            .0
        };
        let plan = ExecutionPlanV1 {
            task_revision_id: task_revision_id.into(),
            engine_id: self.engine_id.clone(),
            pipeline_id: self.packages[root].dependency.artifact_id.clone(),
            compiled_graph_id,
            authority: task.authority,
            limits: task.limits,
            inputs: task.inputs,
            bindings,
            dependencies,
            generated_origins,
            acceptance: graph
                .coverage
                .iter()
                .map(|(name, address)| (name.clone(), BTreeSet::from([address.qualified()])))
                .collect(),
        };
        plan.validate()?;
        Ok((plan, graph))
    }

    /// Reconstruct from the compiler's trusted closure and compare every effective authority
    /// field. Neither an edited graph nor a stripped generated-origin marker can pass.
    pub fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        let revision = read_envelope(
            cas,
            &plan.task_revision_id,
            review_core::task::TASK_REVISION_V1,
        )?;
        if serde_json::from_value::<TaskRevisionV1>(revision.payload).map_err(|e| e.to_string())?
            != *task
        {
            return Err("Plan names another Task revision".into());
        }
        let root = self
            .packages
            .iter()
            .find(|(_, package)| package.dependency.artifact_id == plan.pipeline_id)
            .map(|(name, _)| name)
            .ok_or("Root is absent from trusted catalog")?;
        let recorded = read_envelope(cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
        let (expected, _) =
            self.compile_inner(cas, &plan.task_revision_id, root, Some(&recorded))?;
        if &expected != plan {
            return Err("Execution Plan differs from recompilation of captured authority".into());
        }
        Ok(expected.generated_origins)
    }
}
