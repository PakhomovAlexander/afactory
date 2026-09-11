//! Task-file adapter. All new-format dispatch is delegated to TaskRuntime; this module owns
//! source normalization, trusted policy capture and CLI presentation, never Worker execution.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use review_config::task::catalog::*;
use review_core::task::plan::*;
use review_core::task::verification::VERIFICATION_RESULT_V1;
use review_core::task::*;
use review_core::{ArtifactEnvelope, PortCardinality, Producer};
use review_graph::task::CompiledTask;
use review_pipeline::task::TaskRuntime;
use review_pipeline::task::code::{CodeTaskDomain, CodeTaskPolicy, code_signatures};
use review_pipeline::task::host::{CapturedTaskAuthority, CommandTaskHost, NoTaskDeveloper};
use review_pipeline::task::source::SnapshotTaskEnvironment;
use review_source_git::task::{SOURCE_TREE_V1, capture_snapshot, source_tree};
use review_source_git::{Capture, EntryKind, Manifest, Repo};
use review_store::store::task::{TaskLease, TaskProjection};
use review_store::{Cas, EventStore, validate_envelope};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub(super) struct StartOptions {
    pub file: PathBuf,
    pub repo: PathBuf,
    pub state: Option<PathBuf>,
    pub authority: String,
    pub uncommitted: bool,
    pub json: bool,
    pub plan_only: bool,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskFile {
    schema: String,
    task_id: String,
    kind: String,
    goal: String,
    pipeline: PipelineChoiceV1,
    strategy: String,
    #[serde(default)]
    facts: BTreeMap<String, TaskFactV1>,
    limits: FileLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLimits {
    tokens: u64,
    max_attempts: u32,
    wall_ms: u64,
    verification: VerificationReserveV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCatalog {
    schema: String,
    code_policy: String,
    packages: BTreeMap<String, TaskPackagePin>,
    independence: IndependencePolicyV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedPackage {
    digest: String,
    artifact_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunAuthority {
    schema: String,
    engine_id: String,
    code_policy_id: String,
    catalog_id: String,
    packages: BTreeMap<String, CapturedPackage>,
    independence: IndependencePolicyV1,
}

fn clock() -> Result<u64, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64)
}
fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "task-file-adapter-v1".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    }
}

fn parse<T: serde::de::DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Task configuration exceeds capture bounds".into());
    }
    if path.extension().is_some_and(|e| e == "json") {
        serde_json::from_slice(bytes).map_err(|e| format!("{}: {e}", path.display()))
    } else {
        toml::from_str(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn captured_file(cas: &Cas, manifest: &Manifest, path: &str) -> Result<Vec<u8>, String> {
    if path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Task policy paths must be repository-relative captured files".into());
    }
    let entry = manifest
        .get(path)
        .ok_or_else(|| format!("Task authority Snapshot lacks {path}"))?;
    if entry.kind == EntryKind::Symlink {
        return Err("Task authority cannot follow a policy symlink".into());
    }
    cas.get_bounded(&entry.content, 16 * 1024 * 1024)
        .map_err(|e| e.to_string())
}

fn engine(cas: &Cas) -> Result<String, String> {
    let executable = std::fs::File::open(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let (digest, _) =
        review_source_git::digest_reader_with_buffer(executable, &mut vec![0; 64 * 1024])
            .map_err(|e| e.to_string())?;
    cas.put_json(&json!({"schema":"af.task-engine/1","binary_digest":digest,"version":env!("CARGO_PKG_VERSION"),"graph":"af.compiled-task/1"})).map_err(|e|e.to_string())
}

fn state_path(repo: &Path, state: Option<&Path>) -> Result<(PathBuf, PathBuf), String> {
    let repo = std::fs::canonicalize(repo).map_err(|e| e.to_string())?;
    let state = match state {
        Some(path) => super::resolve_filesystem_path(path)?,
        None => {
            use sha2::{Digest, Sha256};
            let identity = Sha256::digest(repo.as_os_str().as_encoded_bytes());
            super::normalize_absolute(
                &super::xdg_state_root()?
                    .join("af/task/local")
                    .join(&format!("{identity:x}")[..16]),
            )?
        }
    };
    if state.starts_with(&repo) {
        return Err("Task state must be outside the repository".into());
    }
    Ok((repo, state))
}

fn artifact<T: serde::de::DeserializeOwned>(cas: &Cas, id: &str, kind: &str) -> Result<T, String> {
    let value: ArtifactEnvelope =
        serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    validate_envelope(&value)?;
    if value.artifact_id != id || value.artifact_type != kind {
        return Err(format!("Expected exact {kind} artifact"));
    }
    serde_json::from_value(value.payload).map_err(|e| e.to_string())
}

fn capture_authority(
    cas: &Cas,
    manifest: &Manifest,
) -> Result<(String, RunAuthority, TaskPlanCompiler), String> {
    let bytes = captured_file(cas, manifest, ".af/task-catalog.toml")?;
    let catalog: TaskCatalog = parse(Path::new(".af/task-catalog.toml"), &bytes)?;
    if catalog.schema != "af.task-catalog/1"
        || catalog.packages.is_empty()
        || catalog.packages.len() > 128
    {
        return Err("Task catalog requires one to 128 exactly pinned packages".into());
    }
    let policy: CodeTaskPolicy = parse(
        Path::new(&catalog.code_policy),
        &captured_file(cas, manifest, &catalog.code_policy)?,
    )?;
    policy.validate()?;
    let policy_id = cas
        .put_json(&serde_json::to_value(&policy).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let engine_id = engine(cas)?;
    // Capture first; compilation is reconstructed under the complete authority ID below.
    let mut capture = TaskPlanCompiler::new(
        engine_id.clone(),
        policy_id.clone(),
        code_signatures(&policy_id, &policy)?,
        BTreeMap::from([("verified".into(), "snapshot".into())]),
        catalog.independence,
    )?;
    let mut packages = BTreeMap::new();
    let mut total = 0usize;
    for (name, pin) in &catalog.packages {
        let prefix = format!("{}/", pin.path);
        let mut files = BTreeMap::new();
        for entry in &manifest.entries {
            if entry.path.starts_with(&prefix) {
                let bytes = captured_file(cas, manifest, &entry.path)?;
                total = total
                    .checked_add(bytes.len())
                    .ok_or("Task catalog byte count overflow")?;
                if total > 64 * 1024 * 1024 {
                    return Err("Task catalog exceeds 64 MiB captured closure bound".into());
                }
                files.insert(entry.path.clone(), bytes);
            }
        }
        let artifact_id = capture.capture_package(cas, name, pin, &files)?;
        packages.insert(
            name.clone(),
            CapturedPackage {
                digest: pin.digest.clone(),
                artifact_id,
            },
        );
    }
    let authority = RunAuthority {
        schema: "af.task-run-authority/1".into(),
        engine_id,
        code_policy_id: policy_id,
        catalog_id: cas.put(&bytes).map_err(|e| e.to_string())?,
        packages,
        independence: catalog.independence,
    };
    let id = cas
        .put_json(&serde_json::to_value(&authority).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let compiler = restore_compiler(cas, &id, &authority)?;
    Ok((id, authority, compiler))
}

fn restore_compiler(
    cas: &Cas,
    id: &str,
    authority: &RunAuthority,
) -> Result<TaskPlanCompiler, String> {
    if authority.schema != "af.task-run-authority/1" || authority.engine_id != engine(cas)? {
        return Err(
            "Task requires the exact recorded compatible engine; inspect remains available".into(),
        );
    }
    cas.verify(&authority.catalog_id)
        .map_err(|e| e.to_string())?;
    let policy: CodeTaskPolicy = serde_json::from_value(
        cas.get_json(&authority.code_policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut compiler = TaskPlanCompiler::new(
        authority.engine_id.clone(),
        id.into(),
        code_signatures(&authority.code_policy_id, &policy)?,
        BTreeMap::from([("verified".into(), "snapshot".into())]),
        authority.independence,
    )?;
    for (name, package) in &authority.packages {
        compiler.restore_package(cas, name, &package.digest, &package.artifact_id)?;
    }
    for name in authority.packages.keys() {
        if let Some(worker) = compiler.worker(name) {
            if !matches!(worker.runner, TaskWorkerRunner::Command { .. }) {
                return Err(format!(
                    "Worker {name} requires a configured admitted model Provider"
                ));
            }
            compiler.bind_worker(
                name,
                AdmittedWorkerSettings {
                    execution: WorkerExecutionV1::Command {},
                    invocation_policy_id: authority.code_policy_id.clone(),
                },
            )?;
        }
    }
    Ok(compiler)
}

pub(super) fn start(options: StartOptions) -> Result<i32, String> {
    let started = clock()?;
    let mut bytes = Vec::new();
    std::fs::File::open(&options.file)
        .map_err(|e| e.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let file: TaskFile = parse(&options.file, &bytes)?;
    if file.schema != "af.task-file/1"
        || !is_name(&file.task_id)
        || file.kind != "implement"
        || file.goal.trim().is_empty()
    {
        return Err("Task file requires schema af.task-file/1, a valid ID, implement kind and nonempty goal".into());
    }
    let (repo, state) = state_path(&options.repo, options.state.as_deref())?;
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    if store
        .task_projection(&cas, &file.task_id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!(
            "Task {} already exists; use af task run {} or a new Task ID",
            file.task_id, file.task_id
        ));
    }
    let git_home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let source_repo = Repo::open(&repo, git_home.path());
    let policy_source = Capture::new(&source_repo, &cas)
        .committed(&options.authority)
        .map_err(|e| e.to_string())?;
    let (authority_id, authority, compiler) = capture_authority(&cas, &policy_source.manifest)?;
    let source = if options.uncommitted {
        Capture::new(&source_repo, &cas)
            .dirty()
            .map_err(|e| e.to_string())?
    } else {
        policy_source
    };
    let origin=cas.put_json(&json!({"schema":"af.task-source-origin/1","repository_id":source.repository_id,"source_revision":source.source_revision,"content_digest":source.content_digest})).map_err(|e|e.to_string())?;
    let snapshot = capture_snapshot(&cas, &source.manifest, &origin, None)?;
    let source_port = source_tree(&cas, producer(), &snapshot, vec![origin])?;
    let input_file = cas.put(&bytes).map_err(|e| e.to_string())?;
    let requirements = cas
        .put_artifact(
            "af/Requirements@1",
            producer(),
            vec![input_file.clone()],
            None,
            json!({"text":file.goal}),
        )
        .map_err(|e| e.to_string())?
        .0;
    let adapter = cas
        .put_json(&json!({"schema":"af.task-file-adapter/1","source_file_id":input_file}))
        .map_err(|e| e.to_string())?;
    let wall = options
        .timeout_secs
        .map(|seconds| seconds.checked_mul(1000).ok_or("Task timeout overflow"))
        .transpose()?
        .map_or(file.limits.wall_ms, |limit| limit.min(file.limits.wall_ms));
    let mut revision=TaskRevisionV1 {
        task_id:file.task_id,revision:1,previous_revision_id:None,kind:file.kind,goal:file.goal,
        inputs:BTreeMap::from([("source".into(),source_port),("requirements".into(),ArtifactInputV1 {artifact_ids:vec![requirements.clone()],artifact_type:"af/Requirements@1".into(),cardinality:PortCardinality::One,snapshot_id:None})]),
        required_outputs:serde_json::from_value(json!({"snapshot":{"artifact_type":SOURCE_TREE_V1,"cardinality":"one"},"verification":{"artifact_type":VERIFICATION_RESULT_V1,"cardinality":"one"}})).map_err(|e|e.to_string())?,
        acceptance:BTreeMap::from([("verified".into(),AcceptanceObligationV1 {evidence_type:VERIFICATION_RESULT_V1.into(),verifier_policy:authority.code_policy_id.clone()})]),
        provenance:TaskProvenanceV1 {adapter_id:adapter,input_artifact_ids:vec![requirements]},
        authority:TaskAuthorityV1 {policy_id:authority_id,allowed_effects:BTreeSet::from(["read-source".into(),"write-source".into(),"execute-checks".into()]),data_destinations:BTreeSet::new()},
        limits:TaskLimitsV1 {tokens:file.limits.tokens,max_attempts:file.limits.max_attempts,deadline_unix_ms:started.checked_add(wall).ok_or("Task deadline overflow")?,verification:file.limits.verification},
        strategy:file.strategy,pipeline:Some(file.pipeline),facts:file.facts,
    };
    revision.inputs = compiler.normalize_root_inputs(
        &cas,
        &revision
            .pipeline
            .as_ref()
            .ok_or("Task has no selected Pipeline")?
            .name,
        revision.inputs,
    )?;
    revision.provenance.input_artifact_ids = revision
        .inputs
        .values()
        .flat_map(|port| port.artifact_ids.iter().cloned())
        .collect();
    revision.validate()?;
    let revision_id = cas
        .put_artifact(
            TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&revision).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    let (plan, graph) = compiler.compile(
        &cas,
        &revision_id,
        &revision
            .pipeline
            .as_ref()
            .ok_or("Task has no selected Pipeline")?
            .name,
    )?;
    let plan_id = cas
        .put_artifact(
            EXECUTION_PLAN_V1,
            producer(),
            vec![revision_id.clone()],
            None,
            serde_json::to_value(&plan).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    let domain = CodeTaskDomain::captured(&cas, &authority.code_policy_id, graph.clone())?;
    let policy: CodeTaskPolicy = serde_json::from_value(
        cas.get_json(&authority.code_policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let environment = SnapshotTaskEnvironment {
        policy: policy.isolation(),
    };
    let host = CommandTaskHost::capture(
        &cas,
        &compiler,
        &revision,
        &plan,
        graph,
        &environment,
        &domain,
    )?;
    let trusted = CapturedTaskAuthority {
        compiler: &compiler,
        domain: &host,
        developer: &NoTaskDeveloper,
    };
    let lease = store
        .open_task(
            &cas,
            &revision_id,
            &format!("cli-{}", std::process::id()),
            15_000,
        )
        .map_err(|e| e.to_string())?;
    let outcome = (|| {
        store
            .propose_task_plan(&cas, &lease, &plan_id, &trusted)
            .map_err(|e| e.to_string())?;
        if options.plan_only {
            return Ok(());
        }
        store
            .admit_task_plan(&cas, &lease, &trusted)
            .map_err(|e| e.to_string())?;
        execute(&cas, &mut store, &lease, &trusted, &host, &domain)?;
        Ok(())
    })();
    release(&cas, &mut store, &lease, outcome)?;
    present(
        &cas,
        &store,
        &revision.task_id,
        options.json,
        options.plan_only,
    )
}

fn release(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    outcome: Result<(), String>,
) -> Result<(), String> {
    let released = store
        .release_task_lease(cas, lease)
        .map_err(|e| e.to_string());
    match outcome {
        Err(error) => Err(error),
        Ok(()) => {
            released?;
            Ok(())
        }
    }
}

fn execute(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    authority: &CapturedTaskAuthority<'_>,
    host: &CommandTaskHost<'_>,
    domain: &CodeTaskDomain,
) -> Result<(), String> {
    let runtime = TaskRuntime::new(store, cas, lease.clone(), authority, host)?;
    let report = runtime.execute()?;
    let result = domain.result(cas, &runtime.projection()?, &report)?;
    let result_id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![result.task_revision_id.clone()],
            None,
            serde_json::to_value(result).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    runtime.finish(&result_id)
}

pub(super) fn run(id: &str, repo: &Path, state: Option<&Path>, json: bool) -> Result<i32, String> {
    let (_, state) = state_path(repo, state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    if !state.join("events.sqlite").is_file() {
        return Err("No common Task Store exists".into());
    }
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let projection = store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    if matches!(projection.phase, TaskPhaseV1::Finished { .. }) {
        return present(&cas, &store, id, json, false);
    }
    let authority: RunAuthority = serde_json::from_value(
        cas.get_json(&projection.revision.authority.policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let compiler = restore_compiler(&cas, &projection.revision.authority.policy_id, &authority)?;
    let plan: ExecutionPlanV1 = artifact(
        &cas,
        projection
            .plan_id
            .as_deref()
            .ok_or("Task has no captured plan")?,
        EXECUTION_PLAN_V1,
    )?;
    let graph: CompiledTask = artifact(&cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
    let domain = CodeTaskDomain::captured(&cas, &authority.code_policy_id, graph.clone())?;
    let policy: CodeTaskPolicy = serde_json::from_value(
        cas.get_json(&authority.code_policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let environment = SnapshotTaskEnvironment {
        policy: policy.isolation(),
    };
    let host = CommandTaskHost::capture(
        &cas,
        &compiler,
        &projection.revision,
        &plan,
        graph,
        &environment,
        &domain,
    )?;
    let trusted = CapturedTaskAuthority {
        compiler: &compiler,
        domain: &host,
        developer: &NoTaskDeveloper,
    };
    let lease = store
        .take_task_lease(&cas, id, &format!("cli-{}", std::process::id()), 15_000)
        .map_err(|e| e.to_string())?;
    let outcome = (|| {
        store
            .recover_task_attempts(&cas, &lease)
            .map_err(|e| e.to_string())?;
        if !projection.admitted {
            store
                .admit_task_plan(&cas, &lease, &trusted)
                .map_err(|e| e.to_string())?;
        }
        execute(&cas, &mut store, &lease, &trusted, &host, &domain)?;
        Ok(())
    })();
    release(&cas, &mut store, &lease, outcome)?;
    present(&cas, &store, id, json, false)
}

pub(super) fn explain(
    id: &str,
    repo: &Path,
    state: Option<&Path>,
    json: bool,
) -> Result<i32, String> {
    let (_, state) = state_path(repo, state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    present(&cas, &store, id, json, true)
}

pub(super) fn show_if_common(id: &str, state: &Path, json: bool) -> Result<bool, String> {
    if !state.join("events.sqlite").is_file() {
        return Ok(false);
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    if store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Ok(false);
    }
    present(&cas, &store, id, json, false)?;
    Ok(true)
}

pub(super) fn list_common(state: &Path) -> Result<Vec<serde_json::Value>, String> {
    if !state.join("events.sqlite").is_file() {
        return Ok(vec![]);
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    store.task_ids(&cas).map_err(|e| e.to_string())?.into_iter().map(|id| {
        let task = store.task_projection(&cas, &id).map_err(|e| e.to_string())?.ok_or("Unknown Task")?;
        let result: Option<TaskResultV1> = match &task.phase {
            TaskPhaseV1::Finished { result_id } => Some(artifact(&cas, result_id, TASK_RESULT_V1)?),
            _ => None,
        };
        Ok(json!({"task_id":id,"kind":task.revision.kind,"phase":task.phase,
            "outcome":result.as_ref().map(|r| &r.domain_conclusion),
            "chargeable_tokens":task.execution.as_ref().map_or(0,|e| e.budget.committed_tokens()),
            "derived_snapshot_id":result.as_ref().and_then(|r| r.outputs.get("snapshot")).and_then(|o| o.snapshot_id.as_ref()),
            "delivery":delivery_view(&cas, &task)?}))
    }).collect()
}

fn present(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    json_output: bool,
    explain: bool,
) -> Result<i32, String> {
    let state: TaskProjection = store
        .task_projection(cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    let result: Option<TaskResultV1> = match &state.phase {
        TaskPhaseV1::Finished { result_id } => Some(artifact(cas, result_id, TASK_RESULT_V1)?),
        _ => None,
    };
    let mut value = json!({"schema":"af/task-inspection@2","task_id":state.task_id,"revision_id":state.revision_id,"phase":state.phase,"plan_id":state.plan_id,
        "chargeable_tokens":state.execution.as_ref().map_or(0,|e|e.budget.committed_tokens()),"attempts":state.execution.as_ref().map_or(0,|e|e.budget.begun_attempts())});
    if let Some(delivery) = delivery_view(cas, &state)? {
        value["delivery"] = delivery;
    }
    if let Some(result) = &result {
        value["result"] = serde_json::to_value(result).map_err(|e| e.to_string())?;
    }
    if explain && let Some(id) = &state.plan_id {
        let plan: ExecutionPlanV1 = artifact(cas, id, EXECUTION_PLAN_V1)?;
        value["graph"] = serde_json::to_value(artifact::<CompiledTask>(
            cas,
            &plan.compiled_graph_id,
            COMPILED_TASK_V1,
        )?)
        .map_err(|e| e.to_string())?;
        value["plan"] = serde_json::to_value(plan).map_err(|e| e.to_string())?;
    }
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Task {}: {}",
            state.task_id,
            result
                .as_ref()
                .map_or("planned", |r| r.domain_conclusion.as_str())
        );
        if let Some(id) = state.plan_id {
            println!("Plan {id}");
        }
        if explain {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
            );
        }
    }
    Ok(result.map_or(0, |r| match r.acceptance {
        TaskAcceptanceV1::Satisfied => 0,
        TaskAcceptanceV1::Unsatisfied => 3,
        TaskAcceptanceV1::Inconclusive => 4,
    }))
}

fn delivery_view(cas: &Cas, task: &TaskProjection) -> Result<Option<serde_json::Value>, String> {
    task.deliveries
        .last()
        .map(|(_, record)| cas.get_json(&record.receipt_id).map_err(|e| e.to_string()))
        .transpose()
}
