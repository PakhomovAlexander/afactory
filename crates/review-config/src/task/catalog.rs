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
use review_store::Cas;
use serde::{Deserialize, Serialize};

use super::kind::TaskKindManifest;
use crate::{CommandSpec, lock::package_digest_from_files};

pub const TASK_PACKAGE_V1: &str = "af/TaskPackage@1";
pub const COMPILED_TASK_V1: &str = "af/CompiledTask@1";

pub mod export;
pub mod planning;
mod requirements;
mod validation;
pub use validation::CapturedTaskPlanValidator;
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
        /// Legacy wire data, not a command token reservation. Absence is read-only
        /// compatibility for packages captured before explicit legacy context existed.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "review_core::task::present_option"
        )]
        legacy_budget_tokens: Option<u64>,
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

/// A fully validated captured Worker package. Runtime adapters use this only for a package
/// artifact already named by separately approved execution authority; loading it does not add it
/// to the ordinary compiler catalog or grant a new binding.
#[derive(Debug, Clone)]
pub struct CapturedWorkerPackage {
    pub name: String,
    pub version: String,
    pub digest: String,
    pub files: BTreeMap<String, Vec<u8>>,
    pub worker: TaskWorkerManifest,
}

#[derive(Debug, Clone)]
struct Package {
    bytes: PackageBytes,
    dependency: PlanDependencyV1,
}

#[derive(Debug)]
enum ParsedPackage {
    Pipeline(PipelineDefinitionV1),
    Worker(TaskWorkerManifest),
    TaskKind(TaskKindManifest),
}

/// `af/WorkerNotes@1` ports are node-private warm layers: one optional Notes in, one optional
/// Notes out, never a required input and never a Subject-bound one. The compiler separately
/// refuses wiring them between different slots.
fn validate_notes_ports(
    contract: &review_core::task::pipeline::PipelineContractV1,
) -> Result<(), String> {
    use review_core::task::pipeline::PortAffinityV1;
    let is_notes = |port: &review_core::task::pipeline::PipelinePortV1| {
        port.artifact_type == review_core::task::WORKER_NOTES_V1
    };
    let notes_inputs = contract
        .inputs
        .values()
        .filter(|port| is_notes(port))
        .count();
    let notes_outputs = contract
        .outputs
        .values()
        .filter(|port| is_notes(port))
        .count();
    if notes_inputs > 1 || notes_outputs > 1 {
        return Err("A Worker declares at most one Notes input and one Notes output".into());
    }
    for (name, port) in contract.inputs.iter().filter(|(_, port)| is_notes(port)) {
        if !port.optional
            || port.cardinality != review_core::PortCardinality::One
            || port.affinity != (PortAffinityV1::Unbound {})
        {
            return Err(format!(
                "Worker notes input {name} must be one optional unbound af/WorkerNotes@1 port"
            ));
        }
    }
    for (name, port) in contract.outputs.iter().filter(|(_, port)| is_notes(port)) {
        if !port.optional || port.cardinality != review_core::PortCardinality::One {
            return Err(format!(
                "Worker notes output {name} must be one optional af/WorkerNotes@1 port"
            ));
        }
    }
    Ok(())
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
    kinds: BTreeMap<String, TaskKindManifest>,
    active_kind: Option<String>,
    signatures: BTreeMap<String, OperatorSignature>,
    settings: BTreeMap<String, AdmittedWorkerSettings>,
    slot_workers: BTreeMap<String, String>,
    generated: BTreeMap<String, GeneratedOriginV1>,
    acceptance_outputs: BTreeMap<String, String>,
    independence: IndependencePolicyV1,
    provider_admission: Option<review_graph::task::OperatorAttemptCost>,
    preparation_roots: BTreeSet<String>,
    experimental_slots: BTreeMap<String, review_graph::task::ExperimentalSlotTemplateV1>,
    /// Installed domain artifact types that establish authorship independently of a
    /// Worker's removable role/effect declarations (for example a data-only document draft).
    authored_artifacts: BTreeSet<String>,
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
    let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
    if envelope.artifact_id != id || envelope.artifact_type != expected {
        return Err(format!("Expected exact {expected} envelope"));
    }
    Ok(envelope)
}

impl TaskPlanCompiler {
    /// Validate an exact TaskPackage artifact as a Worker without installing it into the
    /// compiler's ordinary trusted package map.
    pub fn captured_worker_package(
        cas: &Cas,
        artifact_id: &str,
    ) -> Result<CapturedWorkerPackage, String> {
        let envelope = read_envelope(cas, artifact_id, TASK_PACKAGE_V1)?;
        let bytes: PackageBytes =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        if package_digest_from_files(&bytes.files) != bytes.digest {
            return Err("Captured Worker package digest disagrees with its bytes".into());
        }
        let ParsedPackage::Worker(worker) = Self::parse_package(&bytes)? else {
            return Err("Captured experimental package is not a Worker".into());
        };
        Ok(CapturedWorkerPackage {
            name: bytes.name,
            version: bytes.version,
            digest: bytes.digest,
            files: bytes.files,
            worker,
        })
    }

    /// Publish the sole supported prospective Worker derivation: one replacement of
    /// `instructions.md`. All runner, manifest, contract, effects, limits, dependencies and
    /// remaining package bytes stay byte-identical to the originally captured package.
    #[allow(clippy::too_many_arguments)]
    pub fn derive_worker_instructions_package(
        cas: &Cas,
        original_artifact_id: &str,
        expected_original_digest: &str,
        expected_derived_digest: &str,
        instructions_id: &str,
        instructions: &str,
        producer: Producer,
        mut refs: Vec<String>,
    ) -> Result<String, String> {
        let original = Self::captured_worker_package(cas, original_artifact_id)?;
        if original.digest != expected_original_digest
            || review_store::canonical::blob_content_id(instructions.as_bytes()) != instructions_id
        {
            return Err(
                "Instruction derivation changed its original package or instruction bytes".into(),
            );
        }
        let mut files = original.files.clone();
        let old = files
            .insert("instructions.md".into(), instructions.as_bytes().to_vec())
            .ok_or("Instruction derivation requires an existing instructions.md")?;
        if old == instructions.as_bytes() {
            return Err("Instruction derivation did not change instructions.md".into());
        }
        let digest = package_digest_from_files(&files);
        if digest != expected_derived_digest {
            return Err("Instruction derivation disagrees with the trusted candidate repin".into());
        }
        let bytes = PackageBytes {
            schema: "af.task-package/1".into(),
            name: original.name,
            version: original.version,
            digest,
            files,
        };
        let ParsedPackage::Worker(derived) = Self::parse_package(&bytes)? else {
            return Err("Instruction derivation no longer describes a Worker".into());
        };
        if derived != original.worker {
            return Err("Instruction derivation changed Worker manifest authority".into());
        }
        refs.extend([original_artifact_id.into(), instructions_id.into()]);
        refs.sort();
        refs.dedup();
        cas.put_artifact(
            TASK_PACKAGE_V1,
            producer,
            refs,
            None,
            serde_json::to_value(bytes).map_err(|e| e.to_string())?,
        )
        .map(|(id, _)| id)
        .map_err(|e| e.to_string())
    }

    pub fn with_authored_artifacts(mut self, types: BTreeSet<String>) -> Result<Self, String> {
        if types.len() > 32 || types.iter().any(|ty| !review_core::is_artifact_type(ty)) {
            return Err("Domain authorship requires bounded versioned artifact types".into());
        }
        self.authored_artifacts = types;
        Ok(self)
    }
    /// A production host enables this when a fresh paid capability probe is required.
    /// Identity-only probes occur before planning; these model calls belong to Task execution.
    pub fn with_provider_admission(
        mut self,
        cost: review_graph::task::OperatorAttemptCost,
    ) -> Self {
        self.provider_admission = Some(cost);
        self
    }

    /// Install a protected dynamic slot from trusted Task-kind policy. Pipeline or Worker bytes
    /// cannot call this method; recompilation retains the same slot artifact and parent.
    pub fn with_experimental_slot(
        mut self,
        parent: String,
        slot: review_graph::task::ExperimentalSlotTemplateV1,
    ) -> Result<Self, String> {
        if !parent.split('.').all(review_core::task::is_name)
            || !review_core::is_digest(&slot.slot_id)
            || slot.max_concurrency == 0
            || self.experimental_slots.insert(parent, slot).is_some()
        {
            return Err(
                "Experimental slot must have one exact trusted parent and bounded concurrency"
                    .into(),
            );
        }
        Ok(self)
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
            kinds: BTreeMap::new(),
            active_kind: None,
            signatures: installed,
            settings: BTreeMap::new(),
            slot_workers: BTreeMap::new(),
            generated: BTreeMap::new(),
            acceptance_outputs,
            independence,
            provider_admission: None,
            preparation_roots: BTreeSet::new(),
            experimental_slots: BTreeMap::new(),
            authored_artifacts: BTreeSet::new(),
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
        if matches!(
            Self::parse_package(&bytes)?,
            ParsedPackage::Worker(TaskWorkerManifest {
                runner: TaskWorkerRunner::LegacyTaskCommand {
                    legacy_budget_tokens: None,
                    ..
                },
                ..
            })
        ) {
            return Err(
                "New legacy command packages require explicit runner.legacy_budget_tokens".into(),
            );
        }
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
        let parsed = Self::parse_package(&bytes)?;
        if let Some(old) = self.packages.get(name) {
            if old.dependency.artifact_id == artifact_id {
                return Ok(());
            }
            return Err("Task package cannot change inside captured authority".into());
        }
        match parsed {
            ParsedPackage::Pipeline(pipeline) => {
                self.pipelines.insert(name.into(), pipeline);
            }
            ParsedPackage::Worker(worker) => {
                self.signatures
                    .insert(format!("worker/{name}"), worker.signature.clone());
                self.workers.insert(name.into(), worker);
            }
            ParsedPackage::TaskKind(kind) => {
                self.kinds.insert(name.into(), kind);
            }
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

    fn parse_package(bytes: &PackageBytes) -> Result<ParsedPackage, String> {
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
            bytes.files.get("kind.toml"),
        ) {
            (Some(source), None, None) => {
                let pipeline = super::parse_task_pipeline(
                    std::str::from_utf8(source).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                if pipeline.name != bytes.name || pipeline.version != bytes.version {
                    return Err("Pipeline manifest disagrees with its pin".into());
                }
                Ok(ParsedPackage::Pipeline(pipeline))
            }
            (None, Some(source), None) => {
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
                validate_notes_ports(&worker.signature.contract)?;
                let cost = worker
                    .signature
                    .attempt
                    .as_ref()
                    .ok_or("Worker signature requires bounded Attempt cost")?;
                if cost.wall_ms == 0 {
                    return Err("Worker Attempt wall limit is zero".into());
                }
                match &worker.runner {
                    TaskWorkerRunner::LegacyTaskCommand { legacy_budget_tokens: Some(tokens), .. }
                        if *tokens > 9_007_199_254_740_991 => return Err("Legacy wire budget exceeds the safe integer range".into()),
                    TaskWorkerRunner::Command {command} | TaskWorkerRunner::LegacyTaskCommand {command, ..} if command.program.trim().is_empty() || cost.tokens != 0 => return Err("Command Worker requires a program and zero model-token reservation".into()),
                    TaskWorkerRunner::Model {provider_kind, model, effort} if !review_core::task::is_name(provider_kind) || model.trim().is_empty() || !review_core::task::is_name(effort) || cost.tokens == 0 => return Err("Model Worker needs explicit Provider/model/effort and token reservation".into()),
                    _ => (),
                }
                Ok(ParsedPackage::Worker(worker))
            }
            (None, None, Some(source)) => {
                let kind: TaskKindManifest =
                    toml::from_str(std::str::from_utf8(source).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                kind.validate()?;
                if kind.name != bytes.name || kind.version != bytes.version {
                    return Err("Task-kind manifest disagrees with its pin".into());
                }
                Ok(ParsedPackage::TaskKind(kind))
            }
            _ => Err(
                "Task package must have exactly one pipeline.toml, worker.toml or kind.toml".into(),
            ),
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

    /// The caller captures these local settings as part of Run authority. Replacement is
    /// checked against every Pipeline boundary by the compiler, then against payload schemas.
    pub fn replace_slot_workers(&mut self, slots: BTreeMap<String, String>) -> Result<(), String> {
        if slots.len() > 64
            || slots.iter().any(|(slot, worker)| {
                !slot.starts_with("root.")
                    || slot
                        .split('.')
                        .any(|part| !review_core::task::is_name(part))
                    || !self.workers.contains_key(worker)
            })
        {
            return Err(
                "Local bindings require qualified slots and captured Worker packages".into(),
            );
        }
        self.slot_workers = slots;
        Ok(())
    }

    fn validate_replacement_schemas(&self, graph: &CompiledTask) -> Result<(), String> {
        for (slot, defaults) in &graph.replaced_workers {
            let effective = &self.packages[&graph.slots[slot].worker].bytes.files;
            for original in defaults {
                let files = &self.packages[original].bytes.files;
                let schemas = |files: &BTreeMap<String, Vec<u8>>| -> Result<BTreeMap<String, serde_json::Value>, String> {
                    files.iter().filter(|(path, _)| path.as_str() == "input.schema.json" || (path.starts_with("outputs/") && path.ends_with(".schema.json")))
                        .map(|(path, bytes)| Ok((path.clone(), serde_json::from_slice(bytes).map_err(|e| format!("Worker schema {path}: {e}"))?))).collect()
                };
                if schemas(files)? != schemas(effective)? {
                    return Err(format!(
                        "Replacement for {slot} changes its payload schemas"
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn pipelines(&self) -> &BTreeMap<String, PipelineDefinitionV1> {
        &self.pipelines
    }
    pub fn task_kind(&self, name: &str) -> Option<&TaskKindManifest> {
        self.kinds.get(name)
    }
    pub fn select_task_kind(&mut self, name: &str) -> Result<(), String> {
        if !self.kinds.contains_key(name) {
            return Err("Task-kind package is not captured".into());
        }
        self.active_kind = Some(name.into());
        Ok(())
    }

    /// A sync validates names and dependency availability without installing or running any
    /// Worker. Exact Task compilation subsequently proves contracts, authority and resources.
    pub fn validate_dependency_closure(&self) -> Result<(), String> {
        for pipeline in self.pipelines.values() {
            for slot in pipeline.slots.values() {
                if !self.workers.contains_key(&slot.worker) {
                    return Err(format!(
                        "Pipeline {} requires missing Worker {}",
                        pipeline.name, slot.worker
                    ));
                }
            }
            for node in &pipeline.nodes {
                if let TaskOperatorV1::Call {
                    pipeline: child, ..
                } = &node.operator
                    && !self.pipelines.contains_key(child)
                {
                    return Err(format!(
                        "Pipeline {} requires missing child {child}",
                        pipeline.name
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn worker(&self, name: &str) -> Option<&TaskWorkerManifest> {
        self.workers.get(name)
    }
    pub fn worker_contract(
        &self,
        cas: &Cas,
        name: &str,
        kernel_outputs: &BTreeSet<String>,
    ) -> Result<review_runner::task::WorkerContract, String> {
        let worker = self
            .worker(name)
            .ok_or("Missing captured Worker manifest")?;
        let files = self
            .package_files(name)
            .ok_or("Missing captured Worker files")?;
        let schema = |path: &str| -> Result<serde_json::Value, String> {
            serde_json::from_slice(
                files
                    .get(path)
                    .ok_or_else(|| format!("Worker {name} lacks {path}"))?,
            )
            .map_err(|e| e.to_string())
        };
        let outputs = worker
            .signature
            .contract
            .outputs
            .keys()
            .filter(|port| !kernel_outputs.contains(*port))
            .map(|port| {
                Ok((
                    port.clone(),
                    schema(&format!("outputs/{port}.schema.json"))?,
                ))
            })
            .collect::<Result<_, String>>()?;
        let contract = review_runner::task::WorkerContract::capture(
            cas,
            schema("input.schema.json")?,
            outputs,
        )?;
        match &worker.runner {
            TaskWorkerRunner::LegacyTaskCommand {
                protocol,
                legacy_budget_tokens,
                ..
            } => match legacy_budget_tokens {
                Some(tokens) => contract.with_legacy_protocol_and_budget(cas, *protocol, *tokens),
                None => contract.with_legacy_protocol(cas, *protocol),
            },
            _ => Ok(contract),
        }
    }
    pub fn package_files(&self, name: &str) -> Option<&BTreeMap<String, Vec<u8>>> {
        self.packages.get(name).map(|package| &package.bytes.files)
    }

    /// Read-only routing preflight. Resource refusals are distinct from a malformed graph;
    /// neither result constitutes Store admission or a generated-plan developer decision.
    pub fn candidate_structure(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        root: &str,
    ) -> Result<(TaskRevisionV1, CompiledTask, Vec<String>), String> {
        if task.authority.policy_id != self.policy_id {
            return Err("Task does not belong to captured authority".into());
        }
        if self
            .active_kind
            .as_ref()
            .is_some_and(|kind| self.kinds[kind].kind != task.kind)
        {
            return Err("Task kind differs from its captured kind package".into());
        }
        let mut normalized = task.clone();
        normalized.inputs = self.normalize_root_inputs(cas, root, normalized.inputs)?;
        let (graph, mut resources) = review_graph::task::compile_task_structure(
            &normalized,
            root,
            &CompileContext {
                pipelines: &self.pipelines,
                signatures: &self.signatures,
                slot_workers: self.slot_workers.clone(),
                acceptance_outputs: self.acceptance_outputs.clone(),
                max_nodes: 64,
                max_depth: 4,
            },
        )?;
        self.validate_requirements_inputs(&graph)?;
        self.validate_replacement_schemas(&graph)?;
        if let Err(reason) = graph.budget(task.limits.clone()) {
            resources.push(reason);
        }
        Ok((normalized, graph, resources))
    }

    fn effective_bindings(
        &self,
        cas: &Cas,
        graph: &CompiledTask,
    ) -> Result<BTreeMap<String, EffectiveWorkerBindingV1>, String> {
        let mut bindings = BTreeMap::new();
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
        }
        Ok(bindings)
    }

    /// Account identities are captured by the host before this check. Provider admission is
    /// still a paid runtime operation, whose full reservation must fit the selected Task.
    pub fn candidate_resources(
        &self,
        cas: &Cas,
        mut graph: CompiledTask,
        limits: &review_core::task::TaskLimitsV1,
        now_unix_ms: u64,
    ) -> Result<Vec<String>, String> {
        let bindings = self.effective_bindings(cas, &graph)?;
        if let Some(cost) = &self.provider_admission {
            graph.install_provider_admission(&bindings, cost)?;
        }
        self.compiled_resources(&graph, limits, now_unix_ms)
    }

    /// A compiled graph already includes paid Provider admission. Recheck remaining capacity
    /// without inserting another operation or altering the recorded plan.
    pub fn compiled_resources(
        &self,
        graph: &CompiledTask,
        limits: &review_core::task::TaskLimitsV1,
        now_unix_ms: u64,
    ) -> Result<Vec<String>, String> {
        let mut reasons = Vec::new();
        if let Err(reason) = graph.budget(limits.clone()) {
            reasons.push(reason);
        }
        let remaining = limits.deadline_unix_ms.saturating_sub(now_unix_ms);
        // Bound mandatory non-verifier time along the dependency path; independent Workers
        // may overlap. Verification keeps its separately protected aggregate allocation.
        let mut path_wall: BTreeMap<String, u64> = BTreeMap::new();
        for name in &graph.order {
            let node = &graph.nodes[name];
            let prior = node
                .inputs
                .values()
                .map(|address| &address.node)
                .chain(
                    node.conditions
                        .iter()
                        .map(|condition| &condition.source.node),
                )
                .filter_map(|source| path_wall.get(source))
                .copied()
                .max()
                .unwrap_or(0);
            let own = graph
                .allowances
                .get(name)
                .filter(|a| a.verification_attempts == 0)
                .filter(|_| {
                    node.conditions.iter().all(|condition| {
                        matches!(
                            graph.nodes[&condition.source.node].operator,
                            CompiledOperator::ProviderAdmission { .. }
                        )
                    })
                })
                .map_or(0, |a| a.wall_ms_per_attempt);
            path_wall.insert(
                name.clone(),
                prior.checked_add(own).ok_or("Task wall path overflow")?,
            );
        }
        let required = path_wall
            .values()
            .copied()
            .max()
            .unwrap_or(0)
            .checked_add(limits.verification.wall_ms)
            .ok_or("Task wall reservation overflow")?;
        if now_unix_ms >= limits.deadline_unix_ms || remaining < required {
            reasons.push(
                "Remaining Task deadline cannot protect verification and declared Attempts".into(),
            );
        }
        Ok(reasons)
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
        if self
            .active_kind
            .as_ref()
            .is_some_and(|kind| self.kinds[kind].kind != task.kind)
        {
            return Err("Task kind differs from its captured kind package".into());
        }
        let graph = self.compile_graph(&task, root)?;
        self.validate_replacement_schemas(&graph)?;
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
        if self
            .active_kind
            .as_ref()
            .is_some_and(|kind| self.kinds[kind].kind != task.kind)
        {
            return Err("Task kind differs from its captured kind package".into());
        }
        cas.verify(&self.engine_id).map_err(|e| e.to_string())?;
        cas.verify(&self.policy_id).map_err(|e| e.to_string())?;
        let mut graph = self.compile_graph(&task, root)?;
        graph.experimental_slots = self.experimental_slots.clone();
        graph.validate_experimental_slots()?;
        self.validate_replacement_schemas(&graph)?;
        let bindings = self.effective_bindings(cas, &graph)?;
        let mut used: BTreeSet<String> = graph
            .calls
            .values()
            .map(|call| call.pipeline.clone())
            .collect();
        used.extend(graph.replaced_workers.values().flatten().cloned());
        used.extend(self.active_kind.iter().cloned());
        used.extend(graph.slots.values().map(|slot| slot.worker.clone()));
        for (slot, declaration) in &graph.slots {
            for other in &declaration.independent_from {
                validate_independent_bindings(
                    &bindings[slot],
                    &bindings[other],
                    self.independence,
                )?;
            }
        }
        // An author cannot remove mandatory independence by omitting a slot annotation.
        // All verification Workers are independent from every source-writing Worker in this
        // Task, including embedded calls and bounded repair paths, under captured policy.
        let writers: BTreeSet<_> = graph
            .slots
            .iter()
            .filter(|(_, slot)| {
                self.workers[&slot.worker]
                    .signature
                    .effects
                    .contains("write-source")
                    || self.workers[&slot.worker]
                        .signature
                        .contract
                        .outputs
                        .values()
                        .any(|port| self.authored_artifacts.contains(&port.artifact_type))
            })
            .map(|(name, _)| name)
            .collect();
        for node in graph.nodes.values() {
            if let CompiledOperator::Primitive {
                operator: TaskOperatorV1::Verify { slot } | TaskOperatorV1::FixVerify { slot },
                ..
            } = &node.operator
            {
                for writer in &writers {
                    validate_independent_bindings(
                        &bindings[slot],
                        &bindings[*writer],
                        self.independence,
                    )?;
                }
            }
        }
        // A syntactically declared but unused slot cannot smuggle an unrelated package into
        // a Worker's context. Only used graph slots belong to executable authority.
        let used_slots: BTreeSet<_> = graph
            .nodes
            .values()
            .flat_map(|node| match &node.operator {
                CompiledOperator::Primitive {
                    operator:
                        TaskOperatorV1::Worker { slot }
                        | TaskOperatorV1::Verify { slot }
                        | TaskOperatorV1::FixVerify { slot },
                    ..
                } => vec![slot],
                CompiledOperator::Primitive {
                    operator:
                        TaskOperatorV1::OptimizationExperiment {
                            baseline_slot,
                            candidate_slot,
                        },
                    ..
                } => vec![baseline_slot, candidate_slot],
                _ => vec![],
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
            preparation: self
                .preparation_roots
                .contains(root)
                .then_some(review_core::task::plan::PlanPreparationV1::Planning {}),
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
