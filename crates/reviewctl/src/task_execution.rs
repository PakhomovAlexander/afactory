//! Task-file adapter. All new-format dispatch is delegated to TaskRuntime; this module owns
//! source normalization, trusted policy capture and CLI presentation, never Worker execution.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use review_config::task::catalog::*;
use review_config::task::kind::TaskKindProfile;
use review_core::task::plan::*;
use review_core::task::review::{TASK_REVIEW_ROUND_V1, TaskReviewRoundV1};
use review_core::task::verification::VERIFICATION_RESULT_V1;
use review_core::task::*;
use review_core::{ArtifactEnvelope, PortCardinality, Producer};
use review_graph::task::CompiledTask;
use review_pipeline::task::TaskRuntime;
use review_pipeline::task::code::{CodeTaskDomain, CodeTaskPolicy, code_signatures};
use review_pipeline::task::document::{
    DocumentTaskDomain, DocumentTaskPolicy, document_signatures,
};
use review_pipeline::task::host::{
    CapturedTaskAuthority, CommandTaskHost, NoTaskDeveloper, TaskDomain, TaskModelBinding,
};
use review_pipeline::task::provider::ProviderTaskDomain;
use review_pipeline::task::review::{ReviewTaskDomain, ReviewTaskPolicy, review_signatures};
use review_pipeline::task::source::SnapshotTaskEnvironment;
use review_source_git::task::{SOURCE_TREE_V1, capture_snapshot, source_tree};
use review_source_git::{Capture, EntryKind, Manifest, Repo};
use review_store::store::task::{TaskLease, TaskProjection};
use review_store::{Cas, EventStore, validate_envelope};
use serde::{Deserialize, Serialize};
use serde_json::json;
mod bindings;
pub(crate) mod catalog;
pub(super) mod developer;
pub(crate) mod domain;
pub(super) mod export;
mod input_file;
mod inspection;
mod issue;
mod legacy;
mod planning;
mod provider_admission;
pub(super) mod refresh;
mod selection;
pub(crate) mod starter;
pub(super) use legacy::start_legacy;

pub(super) struct StartOptions {
    pub file: PathBuf,
    pub bindings: Option<PathBuf>,
    pub source_bindings: Option<PathBuf>,
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
    /// Optional machine-readable business specification, captured as input data only.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    requirements: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    document_sources: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    issue: Option<issue::IssueSource>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pipeline: Option<PipelineChoiceV1>,
    strategy: String,
    /// The requested acceptance profile is Task input, captured before selecting a Pipeline.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    verification: Option<FileVerification>,
    #[serde(default)]
    facts: BTreeMap<String, TaskFactV1>,
    limits: FileLimits,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FileVerification {
    #[default]
    Evaluation,
    Review,
    ReviewOrTargetedFixes,
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    provider_admission: Option<review_graph::task::OperatorAttemptCost>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    code_policy: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    document_policy: Option<String>,
    /// Lower numbers win within a strategy; missing entries have equal last priority.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    selection: BTreeMap<String, BTreeMap<String, u32>>,
    #[serde(default)]
    no_match: review_config::task::selection::NoMatchPolicy,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    developers: Option<developer::DeveloperPolicy>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    planner: Option<review_config::task::catalog::planning::PlannerSettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    review: Option<ReviewSettings>,
    packages: BTreeMap<String, TaskPackagePin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    kinds: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    imports: BTreeSet<String>,
    independence: IndependencePolicyV1,
    #[serde(default)]
    providers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    generation: Option<u32>,
    reviewers: BTreeMap<String, review_core::DemandRequirement>,
    gate: review_core::Severity,
    clean_rounds: u32,
    max_rounds: u32,
    #[serde(default)]
    allow_targeted_repairs: bool,
}

impl ReviewSettings {
    fn policy_generation(&self) -> Result<u32, String> {
        match self.generation {
            None => Ok(1),
            Some(2) => Ok(2),
            Some(_) => {
                Err("Review generation must be omitted for compatibility or explicitly 2".into())
            }
        }
    }
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    provider_admission: Option<review_graph::task::OperatorAttemptCost>,
    engine_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    code_policy_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    document_policy_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    review_policy_id: Option<String>,
    catalog_id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    selection: BTreeMap<String, BTreeMap<String, u32>>,
    #[serde(default)]
    no_match: review_config::task::selection::NoMatchPolicy,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    developers: Option<developer::DeveloperPolicy>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    planner: Option<review_config::task::catalog::planning::PlannerSettings>,
    packages: BTreeMap<String, CapturedPackage>,
    independence: IndependencePolicyV1,
    #[serde(default)]
    providers: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    local_bindings_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    slot_workers: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    kind_package: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    import_locks: BTreeSet<String>,
}

impl RunAuthority {
    fn invocation_policy_id(&self) -> Result<&str, String> {
        match (&self.code_policy_id, &self.document_policy_id) {
            (Some(id), None) | (None, Some(id)) => Ok(id),
            _ => Err("Task requires one captured domain policy".into()),
        }
    }
    fn code_policy_id(&self) -> Result<&str, String> {
        self.code_policy_id
            .as_deref()
            .ok_or_else(|| "Task has no captured code policy".into())
    }
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

pub(super) fn engine(cas: &Cas) -> Result<String, String> {
    // Process-local only: every fresh process proves its running engine bytes. Reconstructing
    // the compiler within one capture must not reread a large debug executable a second time.
    static DIGEST: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    let digest = DIGEST
        .get_or_init(|| {
            let executable =
                std::fs::File::open(std::env::current_exe().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            review_source_git::digest_reader_with_buffer(executable, &mut vec![0; 64 * 1024])
                .map(|(digest, _)| digest)
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)?;
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
    local: Option<&Path>,
    task_kind: &str,
) -> Result<(String, RunAuthority, TaskPlanCompiler), String> {
    let bytes = captured_file(cas, manifest, ".af/task-catalog.toml")?;
    let mut catalog: TaskCatalog = parse(Path::new(".af/task-catalog.toml"), &bytes)?;
    let provider_admission = provider_admission::catalog_cost(&catalog)?;
    let import_locks = catalog::restore_imports(cas, manifest, &mut catalog)?;
    if catalog.packages.is_empty()
        || catalog.packages.len() > 128
        || catalog.kinds.len() > 128
        || catalog
            .kinds
            .iter()
            .any(|(kind, name)| !is_package_name(kind) || !is_package_name(name))
    {
        return Err("Task catalog requires one to 128 exactly pinned packages".into());
    }
    let code_policy_id = domain::capture_policy::<CodeTaskPolicy>(
        cas,
        manifest,
        catalog.code_policy.as_deref(),
        CodeTaskPolicy::validate,
    )?;
    let document_policy_id = domain::capture_policy::<DocumentTaskPolicy>(
        cas,
        manifest,
        catalog.document_policy.as_deref(),
        DocumentTaskPolicy::validate,
    )?;
    let initial_policy = code_policy_id
        .as_ref()
        .or(document_policy_id.as_ref())
        .ok_or("Catalog has no installed domain policy")?;
    let engine_id = engine(cas)?;
    // Package capture does not compile a Task or choose its business acceptance profile.
    let mut capture = TaskPlanCompiler::new(
        engine_id.clone(),
        initial_policy.clone(),
        BTreeMap::new(),
        BTreeMap::new(),
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
    for (kind, package) in &catalog.kinds {
        if capture
            .task_kind(package)
            .is_none_or(|definition| &definition.kind != kind)
        {
            return Err(format!(
                "Task kind {kind} has no matching captured kind package"
            ));
        }
    }
    if catalog.selection.len() > 32
        || catalog.selection.iter().any(|(strategy, priorities)| {
            !is_name(strategy)
                || priorities.len() > 128
                || priorities
                    .keys()
                    .any(|name| !capture.pipelines().contains_key(name))
        })
    {
        return Err(
            "Selection priorities require bounded strategies and captured Pipelines".into(),
        );
    }
    if let Some(developers) = &catalog.developers {
        developers.validate()?;
    }
    if let Some(planner) = &catalog.planner {
        planner.validate()?;
        if catalog.developers.is_none() || capture.worker(&planner.worker).is_none() {
            return Err(
                "Planning requires a captured Planner Worker and developer signing keys".into(),
            );
        }
    }
    let local = local.map(bindings::read).transpose()?;
    let local_bindings_id = local
        .as_ref()
        .map(|local| {
            cas.put_json(&serde_json::to_value(&local.definition).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let mut providers = catalog.providers;
    let mut slot_workers = BTreeMap::new();
    if let Some(local) = local {
        if packages.len() + local.definition.packages.len() > 128
            || total + local.files.values().map(Vec::len).sum::<usize>() > 64 * 1024 * 1024
        {
            return Err("Combined Task catalog exceeds capture bounds".into());
        }
        for (name, pin) in &local.definition.packages {
            if packages.contains_key(name) {
                return Err(format!("Local package {name} shadows a captured package"));
            }
            let artifact_id = capture.capture_package(cas, name, pin, &local.files)?;
            if capture.worker(name).is_none() {
                return Err("Local bindings can add Worker packages only".into());
            }
            packages.insert(
                name.clone(),
                CapturedPackage {
                    digest: pin.digest.clone(),
                    artifact_id,
                },
            );
        }
        providers.extend(local.definition.providers);
        slot_workers = local.definition.slots;
    }
    let is_document = catalog
        .kinds
        .get(task_kind)
        .and_then(|name| capture.task_kind(name))
        .map_or(task_kind == "document", |kind| {
            kind.profile == TaskKindProfile::Document
        });
    let (code_policy_id, document_policy_id) = if is_document {
        (
            None,
            Some(document_policy_id.ok_or("Document Task requires a captured document policy")?),
        )
    } else {
        (
            Some(code_policy_id.ok_or("Code/Review Task requires a captured code policy")?),
            None,
        )
    };
    let authority = RunAuthority {
        schema: if provider_admission.is_some() {
            "af.task-run-authority/2"
        } else {
            "af.task-run-authority/1"
        }
        .into(),
        provider_admission,
        engine_id,
        code_policy_id: code_policy_id.clone(),
        document_policy_id,
        review_policy_id: if is_document {
            None
        } else {
            catalog
                .review
                .map(|review| {
                    let review = ReviewTaskPolicy {
                        schema: format!("af.review-task-policy/{}", review.policy_generation()?),
                        check_policy_id: code_policy_id
                            .clone()
                            .ok_or("Review requires code checks")?,
                        reviewers: review.reviewers,
                        gate: review.gate,
                        clean_rounds: review.clean_rounds,
                        max_rounds: review.max_rounds,
                        allow_targeted_repairs: review.allow_targeted_repairs,
                    };
                    review.validate()?;
                    cas.put_json(&serde_json::to_value(review).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())
                })
                .transpose()?
        },
        catalog_id: cas.put(&bytes).map_err(|e| e.to_string())?,
        selection: catalog.selection,
        no_match: catalog.no_match,
        developers: catalog.developers,
        planner: catalog.planner,
        packages,
        independence: catalog.independence,
        providers,
        local_bindings_id,
        slot_workers,
        kind_package: catalog.kinds.get(task_kind).cloned(),
        import_locks,
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
    if authority.engine_id != engine(cas)? {
        return Err(
            "Task requires the exact recorded compatible engine; inspect remains available".into(),
        );
    }
    cas.verify(&authority.catalog_id)
        .map_err(|e| e.to_string())?;
    let admission_cost = provider_admission::restore_cost(cas, authority)?;
    if let Some(local) = &authority.local_bindings_id {
        cas.verify(local).map_err(|e| e.to_string())?;
    }
    for lock in &authority.import_locks {
        cas.verify(lock).map_err(|e| e.to_string())?;
    }
    authority.invocation_policy_id()?;
    let (mut signatures, mut coverage) = if let Some(id) = &authority.document_policy_id {
        let policy: DocumentTaskPolicy =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        (
            document_signatures(id, &policy)?,
            BTreeMap::from([("verified".into(), "document".into())]),
        )
    } else {
        let id = authority.code_policy_id()?;
        let policy: CodeTaskPolicy =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        (
            code_signatures(id, &policy)?,
            BTreeMap::from([
                ("verified".into(), "snapshot".into()),
                ("goal".into(), "snapshot".into()),
            ]),
        )
    };
    if let Some(id) = &authority.review_policy_id {
        let review: ReviewTaskPolicy =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if review.check_policy_id != authority.code_policy_id()? {
            return Err("Review policy changed its captured checks".into());
        }
        signatures.extend(review_signatures(id, &review)?);
        coverage.insert("reviewed".into(), "review".into());
    }
    let mut compiler = TaskPlanCompiler::new(
        authority.engine_id.clone(),
        id.into(),
        signatures,
        coverage,
        authority.independence,
    )?;
    if authority.document_policy_id.is_some() {
        compiler = compiler.with_authored_artifacts(BTreeSet::from([
            review_core::task::document::DOCUMENT_DRAFT_V1.into(),
        ]))?;
    }
    for (name, package) in &authority.packages {
        compiler.restore_package(cas, name, &package.digest, &package.artifact_id)?;
    }
    compiler.replace_slot_workers(authority.slot_workers.clone())?;
    if let Some(kind) = &authority.kind_package {
        compiler.select_task_kind(kind)?;
    }
    for name in authority.packages.keys() {
        if let Some(worker) = compiler.worker(name) {
            if !matches!(
                worker.runner,
                TaskWorkerRunner::Command { .. } | TaskWorkerRunner::LegacyTaskCommand { .. }
            ) {
                continue;
            }
            compiler.bind_worker(
                name,
                AdmittedWorkerSettings {
                    execution: WorkerExecutionV1::Command {},
                    invocation_policy_id: authority.invocation_policy_id()?.into(),
                },
            )?;
        }
    }
    Ok(compiler.with_provider_admission(admission_cost))
}

fn bind_models(
    cas: &Cas,
    compiler: &mut TaskPlanCompiler,
    authority: &RunAuthority,
    revision_id: &str,
    root: &str,
) -> Result<BTreeMap<String, Box<dyn review_runner::task::WorkerModelAdapter>>, String> {
    let required = compiler.required_worker_packages(cas, revision_id, root)?;
    bind_named_models(compiler, authority, required)
}

fn bind_named_models(
    compiler: &mut TaskPlanCompiler,
    authority: &RunAuthority,
    required: BTreeSet<String>,
) -> Result<BTreeMap<String, Box<dyn review_runner::task::WorkerModelAdapter>>, String> {
    let mut identities = BTreeMap::new();
    let mut adapters = BTreeMap::new();
    for name in required {
        let worker = compiler.worker(&name).ok_or("Required Worker is absent")?;
        let TaskWorkerRunner::Model {
            provider_kind,
            model,
            effort,
        } = &worker.runner
        else {
            continue;
        };
        let alias = authority.providers.get(&name).ok_or_else(||format!("Worker {name} requires an explicit local Provider binding in the catalog providers table"))?;
        if !identities.contains_key(alias) {
            identities.insert(
                alias.clone(),
                super::providers::task::TaskProviderIdentity::probe(alias, provider_kind)?,
            );
        }
        let identity = &identities[alias];
        let execution = identity.execution(model, effort)?;
        if !matches!(&execution,WorkerExecutionV1::Model {provider_kind:kind,..} if kind==provider_kind)
        {
            return Err("Worker and Provider implementation families differ".into());
        }
        adapters.insert(name.clone(), identity.adapter(&execution)?);
        compiler.bind_worker(
            &name,
            AdmittedWorkerSettings {
                execution,
                invocation_policy_id: authority.invocation_policy_id()?.into(),
            },
        )?;
    }
    Ok(adapters)
}

fn model_bindings<'a>(
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
    adapters: &'a BTreeMap<String, Box<dyn review_runner::task::WorkerModelAdapter>>,
) -> Result<BTreeMap<String, TaskModelBinding<'a>>, String> {
    plan.bindings
        .iter()
        .filter(|(_, binding)| matches!(binding.execution, WorkerExecutionV1::Model { .. }))
        .map(|(slot, binding)| {
            let name = &graph
                .slots
                .get(slot)
                .ok_or("Captured Model slot is absent")?
                .worker;
            let adapter = adapters
                .get(name)
                .ok_or("Captured Model adapter is absent")?;
            Ok((
                slot.clone(),
                TaskModelBinding {
                    binding: binding.clone(),
                    adapter: adapter.as_ref(),
                },
            ))
        })
        .collect()
}

pub(super) fn start(options: StartOptions) -> Result<i32, String> {
    start_kind(options, None)
}

pub(super) fn start_review(options: StartOptions) -> Result<i32, String> {
    start_kind(options, Some("review"))
}

fn effective_task_wall_ms(file_wall_ms: u64, timeout_secs: Option<u64>) -> Result<u64, String> {
    if file_wall_ms == 0 || timeout_secs == Some(0) {
        return Err("Task wall_ms and --timeout-secs must be positive".into());
    }
    let timeout_ms = timeout_secs
        .map(|seconds| seconds.checked_mul(1000).ok_or("Task timeout overflow"))
        .transpose()?;
    Ok(timeout_ms.map_or(file_wall_ms, |limit| limit.min(file_wall_ms)))
}

fn start_kind(options: StartOptions, expected_kind: Option<&str>) -> Result<i32, String> {
    let started = clock()?;
    let bytes = input_file::read(&options.file, 16 * 1024 * 1024)?;
    let file: TaskFile = parse(&options.file, &bytes)?;
    if file.schema != "af.task-file/1"
        || !is_name(&file.task_id)
        || !is_package_name(&file.kind)
        || file.goal.trim().is_empty()
    {
        return Err("Task file requires schema af.task-file/1, a valid ID, supported kind and nonempty goal".into());
    }
    if let Some(specification) = &file.requirements {
        if specification.is_empty()
            || serde_json::to_vec(specification)
                .map_err(|e| e.to_string())?
                .len()
                > 65536
        {
            return Err("Structured requirements must be nonempty and at most 64 KiB".into());
        }
    }
    // Reject invalid duration before creating state, capturing authority or occupying an ID.
    let wall = effective_task_wall_ms(file.limits.wall_ms, options.timeout_secs)?;
    started.checked_add(wall).ok_or("Task deadline overflow")?;
    let (repo, state) = state_path(&options.repo, options.state.as_deref())?;
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
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
    let authority = capture_authority(
        &cas,
        &policy_source.manifest,
        options.bindings.as_deref(),
        &file.kind,
    )?;
    if expected_kind.is_some()
        && selected_profile(&authority.2, &authority.1, &file.kind, file.verification)?
            != TaskKindProfile::Review
    {
        return Err("af review --file requires a Review Task profile".into());
    }
    let source = if options.uncommitted {
        Capture::new(&source_repo, &cas)
            .dirty()
            .map_err(|e| e.to_string())?
    } else {
        policy_source
    };
    start_captured(
        options, file, bytes, started, cas, store, source, authority, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn start_captured(
    options: StartOptions,
    file: TaskFile,
    bytes: Vec<u8>,
    started: u64,
    cas: Cas,
    mut store: EventStore,
    source: review_source_git::Snapshot,
    (authority_id, authority, mut compiler): (String, RunAuthority, TaskPlanCompiler),
    legacy_budget: Option<u64>,
) -> Result<i32, String> {
    let profile = selected_profile(&compiler, &authority, &file.kind, file.verification)?;
    let origin=cas.put_json(&json!({"schema":"af.task-source-origin/1","repository_id":source.repository_id,"source_revision":source.source_revision,"content_digest":source.content_digest})).map_err(|e|e.to_string())?;
    let source_port = if profile == TaskKindProfile::Document {
        None
    } else {
        if file.document_sources.is_some() {
            return Err("Document source input is only valid for a document Task".into());
        }
        let snapshot = capture_snapshot(&cas, &source.manifest, &origin, None)?;
        Some(source_tree(
            &cas,
            producer(),
            &snapshot,
            vec![origin.clone()],
        )?)
    };
    let input_file = cas.put(&bytes).map_err(|e| e.to_string())?;
    let wall = effective_task_wall_ms(file.limits.wall_ms, options.timeout_secs)?;
    let deadline = started.checked_add(wall).ok_or("Task deadline overflow")?;
    let issue = file
        .issue
        .as_ref()
        .map(|selected| {
            if legacy_budget.is_some() {
                return Err("Issue requirements need an explicit Task file".into());
            }
            issue::capture(
                &cas,
                &source.manifest,
                selected,
                options.source_bindings.as_deref(),
                file.requirements.clone(),
                deadline,
            )
        })
        .transpose()?;
    // Runner/context authority owns legacy identity and wire budgets; business data
    // retains the explicit Task-file specification and Issue capture below.
    let mut requirements_payload = json!({"text":file.goal});
    if let Some(specification) = &file.requirements {
        requirements_payload["specification"] = json!(specification);
    }
    let mut input_refs = vec![input_file.clone()];
    if let Some(issue) = &issue {
        requirements_payload =
            serde_json::to_value(&issue.requirements).map_err(|e| e.to_string())?;
        input_refs.push(issue.capture_id.clone());
    }
    let requirements = cas
        .put_artifact(
            "af/Requirements@1",
            producer(),
            input_refs,
            None,
            requirements_payload,
        )
        .map_err(|e| e.to_string())?
        .0;
    let mut adapter = json!({"schema":"af.task-file-adapter/1","source_file_id":input_file});
    if let Some(issue) = &issue {
        adapter["source_capture_id"] = json!(issue.capture_id);
    }
    let adapter = cas.put_json(&adapter).map_err(|e| e.to_string())?;
    let goal = match &issue {
        Some(issue) => format!("{}\n\n{}", file.goal, issue.requirements.text),
        None => file.goal.clone(),
    };
    let mut revision=TaskRevisionV1 {
        task_id:file.task_id,revision:1,previous_revision_id:None,kind:file.kind,goal,
        inputs:BTreeMap::from([("requirements".into(),ArtifactInputV1 {artifact_ids:vec![requirements.clone()],artifact_type:"af/Requirements@1".into(),cardinality:PortCardinality::One,snapshot_id:None})]),
        required_outputs:serde_json::from_value(json!({"snapshot":{"artifact_type":SOURCE_TREE_V1,"cardinality":"one"},"verification":{"artifact_type":VERIFICATION_RESULT_V1,"cardinality":"one"}})).map_err(|e|e.to_string())?,
        acceptance:BTreeMap::from([("verified".into(),AcceptanceObligationV1 {evidence_type:VERIFICATION_RESULT_V1.into(),verifier_policy:authority.invocation_policy_id()?.into()})]),
        provenance:TaskProvenanceV1 {adapter_id:adapter,input_artifact_ids:vec![requirements]},
        authority:TaskAuthorityV1 {policy_id:authority_id,allowed_effects:BTreeSet::from(["read-source".into(),"write-source".into(),"execute-checks".into()]),data_destinations:BTreeSet::new()},
        limits:TaskLimitsV1 {tokens:file.limits.tokens,max_attempts:file.limits.max_attempts,deadline_unix_ms:deadline,verification:file.limits.verification},
        strategy:file.strategy,pipeline:file.pipeline,facts:file.facts,
    };
    if let Some(source_port) = source_port {
        revision.inputs.insert("source".into(), source_port);
    }
    if profile == TaskKindProfile::Document {
        use review_core::task::document::*;
        let path = file
            .document_sources
            .as_deref()
            .ok_or("Document Task needs a captured document_sources file")?;
        if !review_config::task::shared::safe_relative_path(path) {
            return Err("Document source path must be project-relative".into());
        }
        let bytes = captured_file(&cas, &source.manifest, path)?;
        let sources: DocumentSourcesV1 = parse(Path::new(path), &bytes)?;
        sources.validate()?;
        let raw = cas.put(&bytes).map_err(|e| e.to_string())?;
        let id = cas
            .put_artifact(
                DOCUMENT_SOURCES_V1,
                producer(),
                vec![raw, origin],
                None,
                serde_json::to_value(sources).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        revision.inputs.insert(
            "sources".into(),
            ArtifactInputV1 {
                artifact_ids: vec![id.clone()],
                artifact_type: DOCUMENT_SOURCES_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: None,
            },
        );
        revision.provenance.input_artifact_ids.push(id);
        revision.provenance.input_artifact_ids.sort();
        revision.authority.allowed_effects.clear();
        revision.required_outputs = serde_json::from_value(json!({"document":{"artifact_type":DOCUMENT_V1,"cardinality":"one"},"verification":{"artifact_type":DOCUMENT_VERIFICATION_V1,"cardinality":"one"}})).map_err(|e|e.to_string())?;
        revision.acceptance = BTreeMap::from([(
            "verified".into(),
            AcceptanceObligationV1 {
                evidence_type: DOCUMENT_VERIFICATION_V1.into(),
                verifier_policy: authority.invocation_policy_id()?.into(),
            },
        )]);
    }
    if profile == TaskKindProfile::Review {
        if file.requirements.is_none() && file.issue.is_none() {
            revision.inputs.remove("requirements");
        }
        revision.authority.allowed_effects.remove("write-source");
        revision.required_outputs = serde_json::from_value(json!({"review":{"artifact_type":TASK_REVIEW_ROUND_V1,"cardinality":"one"},"history":{"artifact_type":REVIEW_HISTORY_V1,"cardinality":"one"}})).map_err(|e|e.to_string())?;
        revision.acceptance = BTreeMap::from([(
            "reviewed".into(),
            AcceptanceObligationV1 {
                evidence_type: TASK_REVIEW_ROUND_V1.into(),
                verifier_policy: authority
                    .review_policy_id
                    .clone()
                    .ok_or("Review Task requires configured Review policy")?,
            },
        )]);
    } else if matches!(
        profile,
        TaskKindProfile::ReviewedImplementation | TaskKindProfile::RepairAllowedImplementation
    ) {
        use review_core::task::verification::{
            REPAIR_ALLOWED_IMPLEMENTATION_V1, REVIEWED_IMPLEMENTATION_V1,
        };
        let evidence_type = if profile == TaskKindProfile::RepairAllowedImplementation {
            REPAIR_ALLOWED_IMPLEMENTATION_V1
        } else {
            REVIEWED_IMPLEMENTATION_V1
        };
        revision
            .required_outputs
            .get_mut("verification")
            .expect("implementation output")
            .artifact_type = evidence_type.into();
        revision.required_outputs.insert(
            "evaluation".into(),
            revision.required_outputs["verification"].clone(),
        );
        revision
            .required_outputs
            .get_mut("evaluation")
            .unwrap()
            .artifact_type = VERIFICATION_RESULT_V1.into();
        revision.acceptance.insert(
            "goal".into(),
            AcceptanceObligationV1 {
                evidence_type: VERIFICATION_RESULT_V1.into(),
                verifier_policy: authority.code_policy_id()?.into(),
            },
        );
        revision.acceptance.insert(
            "verified".into(),
            AcceptanceObligationV1 {
                evidence_type: evidence_type.into(),
                verifier_policy: authority
                    .review_policy_id
                    .clone()
                    .ok_or("Review acceptance requires configured Review policy")?,
            },
        );
    }
    let Some(selected) = selection::prepare(&cas, &authority, &compiler, revision, options.json)?
    else {
        return Ok(1);
    };
    let selected = match selected {
        selection::PreparedSelection::Selected(selected) => selected,
        selection::PreparedSelection::Generation {
            revision,
            revision_id,
        } => {
            return planning::start(
                options,
                cas,
                store,
                authority,
                compiler,
                *revision,
                revision_id,
            );
        }
    };
    let selection::SelectedTask {
        revision,
        revision_id,
        compiler: selected_compiler,
        adapters,
        plan,
        graph,
    } = *selected;
    compiler = selected_compiler;
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
    let inner = captured_domain(&cas, &authority, profile, graph.clone())?;
    let models = model_bindings(&plan, &graph, &adapters)?;
    let domain = ProviderTaskDomain {
        graph: &graph,
        models: &models,
        inner: inner.as_ref(),
    };
    let environment = domain::environment(&cas, &authority)?;
    let host = CommandTaskHost::capture_with_models(
        &cas,
        &compiler,
        &revision,
        &plan,
        graph.clone(),
        environment.as_ref(),
        &domain,
        &models,
    )?;
    let developer = developer::host(&cas, &authority, None);
    let trusted = CapturedTaskAuthority::new(&compiler, &host, developer.as_ref());
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

fn selected_profile(
    compiler: &TaskPlanCompiler,
    authority: &RunAuthority,
    kind: &str,
    verification: Option<FileVerification>,
) -> Result<TaskKindProfile, String> {
    let profile = if let Some(name) = &authority.kind_package {
        let definition = compiler
            .task_kind(name)
            .ok_or("Captured Task-kind package is absent")?;
        if definition.kind != kind {
            return Err("Task-kind mapping changed its business kind".into());
        }
        definition.profile
    } else {
        match kind {
            "implement" if verification == Some(FileVerification::Review) => {
                TaskKindProfile::ReviewedImplementation
            }
            "implement" if verification == Some(FileVerification::ReviewOrTargetedFixes) => {
                TaskKindProfile::RepairAllowedImplementation
            }
            "implement" => TaskKindProfile::Implementation,
            "review" => TaskKindProfile::Review,
            "document" => TaskKindProfile::Document,
            _ => return Err("Task requires a configured kind package".into()),
        }
    };
    if verification.is_some_and(|requested| {
        !matches!(
            (profile, requested),
            (
                TaskKindProfile::Implementation,
                FileVerification::Evaluation
            ) | (
                TaskKindProfile::ReviewedImplementation,
                FileVerification::Review
            ) | (
                TaskKindProfile::RepairAllowedImplementation,
                FileVerification::ReviewOrTargetedFixes
            )
        )
    }) {
        return Err("Task verification request contradicts its captured kind profile".into());
    }
    Ok(profile)
}

fn captured_domain(
    cas: &Cas,
    authority: &RunAuthority,
    profile: TaskKindProfile,
    graph: CompiledTask,
) -> Result<Box<dyn TaskDomain>, String> {
    match profile {
        TaskKindProfile::Implementation if authority.review_policy_id.is_none() => Ok(Box::new(
            CodeTaskDomain::captured(cas, authority.code_policy_id()?, graph)?,
        )),
        TaskKindProfile::Review
        | TaskKindProfile::Implementation
        | TaskKindProfile::ReviewedImplementation
        | TaskKindProfile::RepairAllowedImplementation => Ok(Box::new(
            ReviewTaskDomain::captured(
                cas,
                authority
                    .review_policy_id
                    .as_deref()
                    .ok_or("Review Task lost its captured policy")?,
                graph,
            )?
            .with_review_task(profile == TaskKindProfile::Review),
        )),
        TaskKindProfile::Document => Ok(Box::new(DocumentTaskDomain::captured(
            cas,
            authority
                .document_policy_id
                .as_deref()
                .ok_or("Task lost its document policy")?,
            graph,
        )?)),
    }
}

fn execute(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    authority: &CapturedTaskAuthority<'_>,
    host: &CommandTaskHost<'_>,
    domain: &dyn TaskDomain,
) -> Result<(), String> {
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let runtime = TaskRuntime::new(store, cas, lease.clone(), authority, host)?
        .with_cancellation(&cancellation);
    let report = runtime.execute()?;
    let projection = runtime.projection()?;
    if matches!(projection.phase, TaskPhaseV1::Waiting { .. }) {
        return Ok(());
    }
    let result = domain.assemble_result(cas, &projection, &report)?;
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
    if projection.plan_id.is_none() {
        present(&cas, &store, id, json, true)?;
        return Ok(4);
    }
    let authority: RunAuthority = serde_json::from_value(
        cas.get_json(&projection.revision.authority.policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut compiler = planning::restore(&cas, &projection, &authority)?;
    let plan: ExecutionPlanV1 = artifact(
        &cas,
        projection
            .plan_id
            .as_deref()
            .ok_or("Task has no captured plan")?,
        EXECUTION_PLAN_V1,
    )?;
    let graph: CompiledTask = artifact(&cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
    if plan.preparation.is_some() {
        return planning::resume(
            cas, store, authority, compiler, projection, plan, graph, json,
        );
    }
    let adapters = bind_models(
        &cas,
        &mut compiler,
        &authority,
        &projection.revision_id,
        &graph
            .calls
            .get("root")
            .ok_or("Task has no captured root Pipeline")?
            .pipeline,
    )?;
    compiler.validate_plan(&cas, &projection.revision, &plan)?;
    let verification = projection
        .revision
        .acceptance
        .values()
        .any(|a| a.evidence_type == review_core::task::verification::REVIEWED_IMPLEMENTATION_V1)
        .then_some(FileVerification::Review)
        .or_else(|| {
            projection
                .revision
                .acceptance
                .values()
                .any(|a| {
                    a.evidence_type
                        == review_core::task::verification::REPAIR_ALLOWED_IMPLEMENTATION_V1
                })
                .then_some(FileVerification::ReviewOrTargetedFixes)
        });
    let profile = selected_profile(
        &compiler,
        &authority,
        &projection.revision.kind,
        verification,
    )?;
    let inner = captured_domain(&cas, &authority, profile, graph.clone())?;
    let models = model_bindings(&plan, &graph, &adapters)?;
    let domain = ProviderTaskDomain {
        graph: &graph,
        models: &models,
        inner: inner.as_ref(),
    };
    let environment = domain::environment(&cas, &authority)?;
    let host = CommandTaskHost::capture_with_models(
        &cas,
        &compiler,
        &projection.revision,
        &plan,
        graph.clone(),
        environment.as_ref(),
        &domain,
        &models,
    )?;
    let developer = developer::host(&cas, &authority, None);
    let trusted = CapturedTaskAuthority::new(&compiler, &host, developer.as_ref());
    let lease = store
        .take_task_lease(&cas, id, &format!("cli-{}", std::process::id()), 15_000)
        .map_err(|e| e.to_string())?;
    let outcome = (|| {
        store
            .recover_task_attempts(&cas, &lease)
            .map_err(|e| e.to_string())?;
        if projection
            .waiting_for_domain_publication(&cas)
            .map_err(|e| e.to_string())?
        {
            store
                .resume_task(&cas, &lease, &trusted)
                .map_err(|e| e.to_string())?;
        }
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
    plan: Option<&str>,
) -> Result<i32, String> {
    let (_, state) = state_path(repo, state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    if let Some(plan_id) = plan {
        return inspection::explain_plan(&cas, &store, id, plan_id, json);
    }
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
    store.map_tasks(&cas, |task| {
        let result: Option<TaskResultV1> = match &task.phase {
            TaskPhaseV1::Finished { result_id } => Some(artifact(&cas, result_id, TASK_RESULT_V1)?),
            _ => None,
        };
        Ok(json!({"schema":"af/task-list-entry@2","task_id":task.task_id,"kind":task.revision.kind,"phase":task.phase,
            "outcome":result.as_ref().map(|r| &r.domain_conclusion),
            "chargeable_tokens":task.execution.as_ref().map_or(0,|e| e.budget.committed_tokens()).to_string(),
            "derived_snapshot_id":result.as_ref().and_then(|r| r.outputs.get("snapshot")).and_then(|o| o.snapshot_id.as_ref()),
            "delivery":delivery_view(&cas, &task)?}))
    }).map_err(|e| e.to_string())?.into_iter().collect()
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
    let mut value = json!({"schema":"af/task-inspection@3","task_id":state.task_id,"revision_id":state.revision_id,"phase":state.phase,"plan_id":state.plan_id,
        "chargeable_tokens":state.execution.as_ref().map_or(0,|e|e.budget.committed_tokens()).to_string(),"attempts":state.execution.as_ref().map_or(0,|e|e.budget.begun_attempts())});
    if let Some(selection) = selection::recorded(cas, &state.revision)? {
        value["selection"] = selection;
    }
    if let Some(proof) = &state.planning {
        value["planning"] = json!({"bootstrap_plan_id":proof.bootstrap_plan_id(), "proposal_id":proof.proposal_id(), "request_revision_id":proof.revision_id()});
    }
    let events = store
        .replay(&review_store::store::task::task_run_id(id).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let recording_recovery = events
        .iter()
        .any(|event| event.event_type == review_core::EventType::TaskTransitionV4);
    let mut history = Vec::new();
    let mut execution = Vec::new();
    let mut broker_records = Vec::new();
    let mut owned_child_sets = Vec::new();
    let mut decisions = Vec::new();
    for event in events {
        if event.event_type == review_core::EventType::TaskBrokerTransitionV1 {
            let transition: review_core::task::broker::TaskBrokerTransitionV1 =
                serde_json::from_value(event.payload).map_err(|e| e.to_string())?;
            let record = review_store::store::task::execution::broker::read_task_broker_record(
                cas,
                &transition.record_id,
            )
            .map_err(|e| e.to_string())?;
            broker_records.push(json!({"artifact_id":record.artifact_id,"artifact_type":record.artifact_type,"record":record.payload}));
            history.push(json!({"sequence":event.sequence,"broker_transition":transition}));
            continue;
        }
        let transition =
            review_store::store::task::read_task_transition(&event).map_err(|e| e.to_string())?;
        if let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
            &transition.change
        {
            let decoded =
                review_store::store::task::execution::read_execution_record(cas, record_id)
                    .map_err(|error| error.to_string())?;
            let mut entry = json!({"artifact_id":record_id,"artifact_type":decoded.envelope.artifact_type,"record":decoded.envelope.payload});
            let record = decoded.record;
            if let review_core::task::execution::TaskExecutionRecordV1::OwnedChildrenRegistered {
                child_set_id,
            } = &record
            {
                let set = review_store::store::task::execution::owned::read_task_owned_children(
                    cas,
                    child_set_id,
                )
                .map_err(|e| e.to_string())?;
                owned_child_sets.push(json!({"artifact_id":child_set_id,"artifact_type":review_core::task::owned_children::TASK_OWNED_CHILD_SET_V1,"record":set}));
            }
            if let review_core::task::execution::TaskExecutionRecordV1::Settled {
                result:
                    review_core::task::execution::TaskAttemptResultV1::Failed { diagnostic_id, .. },
                ..
            } = &record
            {
                entry["diagnostic"] = cas.get_json(diagnostic_id).map_err(|e| e.to_string())?;
            }
            execution.push(entry);
        }
        if let review_core::task::event::TaskChangeV1::PlanDecided {
            decision_id,
            valid_until_unix_ms,
        } = &transition.change
        {
            let decision: PlanDecisionV1 = artifact(cas, decision_id, PLAN_DECISION_V1)?;
            decisions.push(json!({"artifact_id":decision_id, "decision":decision, "valid_until_unix_ms":valid_until_unix_ms}));
        }
        history.push(json!({"sequence":event.sequence,"transition":event.payload}));
    }
    if !decisions.is_empty() {
        value["plan_decisions"] = json!(decisions);
    }
    value["history"] = json!(history);
    value["execution_records"] = json!(execution);
    if !broker_records.is_empty() {
        value["schema"] = json!("af/task-inspection@4");
        value["broker_records"] = json!(broker_records);
    }
    if !owned_child_sets.is_empty() {
        value["schema"] = json!("af/task-inspection@5");
        value["owned_child_sets"] = json!(owned_child_sets);
    }
    if !state.review_handoffs.is_empty() {
        value["schema"] = json!("af/task-inspection@6");
        let mut handoffs = Vec::new();
        for (id, _) in &state.review_handoffs {
            // The checked projection normalizes both generations. Keep the original
            // payload: an integrated handoff must never be re-encoded as generation one.
            let recorded = cas.get_artifact(id).map_err(|e| e.to_string())?;
            handoffs.push(json!({"artifact_id":id,"artifact_type":recorded.artifact_type,"record":recorded.payload}));
        }
        value["review_handoffs"] = json!(handoffs);
    }
    if let Some(execution) = &state.execution {
        let phases = execution.review_integrations();
        if !phases.is_empty() {
            value["schema"] = json!("af/task-inspection@7");
            value["review_integrations"] = json!(phases.iter().map(|phase| json!({
                "artifact_id":phase.phase_id(),
                "artifact_type":review_core::task::review_integration::TASK_REVIEW_INTEGRATION_PHASE_V1,
                "record":phase.phase(),
                "node":phase.node(),
                "requires_checks":phase.requires_checks(),
                "finished":phase.finished(),
                "report_id":phase.report_id(),
                "integration_committed_event_id":phase.integration_committed_event_id(),
            })).collect::<Vec<_>>());
        }
    }
    if recording_recovery {
        value["schema"] = json!("af/task-inspection@8");
    }
    let mut reports = Vec::new();
    for id in &state.run_reports {
        use review_core::task::report::*;
        let (report, phase_id) =
            review_store::store::task::read_task_run_report(cas, id).map_err(|e| e.to_string())?;
        let mut diagnostics = BTreeMap::new();
        for node in &report.nodes {
            if let TaskNodeOutcomeV1::Failed { diagnostic_id, .. } = &node.outcome {
                let diagnostic: TaskDiagnosticV1 =
                    artifact(cas, diagnostic_id, TASK_DIAGNOSTIC_V1)?;
                diagnostic.validate()?;
                diagnostics.insert(node.node.clone(), diagnostic);
            }
        }
        let report = if phase_id.is_some() {
            cas.get_artifact(id).map_err(|e| e.to_string())?.payload
        } else {
            serde_json::to_value(report).map_err(|e| e.to_string())?
        };
        reports.push(json!({"artifact_id":id,"report":report,"diagnostics":diagnostics}));
    }
    value["run_reports"] = json!(reports);
    if let Some(delivery) = delivery_view(cas, &state)? {
        value["delivery"] = delivery;
    }
    if let Some(result) = &result {
        value["result"] = serde_json::to_value(result).map_err(|e| e.to_string())?;
        if let TaskPhaseV1::Finished { result_id } = &state.phase {
            let envelope: ArtifactEnvelope =
                serde_json::from_value(cas.get_json(result_id).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            for id in &envelope.input_artifacts {
                let diagnostic = cas.get_json(id).map_err(|e| e.to_string())?;
                if diagnostic["schema"] == "af.planning-diagnostic/1" {
                    value["planning_diagnostic"] = diagnostic;
                }
            }
        }
        if let Some(execution) = &state.execution {
            let rounds: Vec<_> = execution
                .outputs
                .values()
                .flat_map(|(_, output)| output.outputs.values())
                .filter(|p| p.artifact_type == TASK_REVIEW_ROUND_V1)
                .flat_map(|p| p.artifact_ids.iter())
                .map(|id| artifact::<TaskReviewRoundV1>(cas, id, TASK_REVIEW_ROUND_V1))
                .collect::<Result<_, _>>()?;
            let repairs: Vec<_> = execution
                .outputs
                .values()
                .flat_map(|(_, output)| output.outputs.values())
                .filter(|p| p.artifact_type == REPAIR_ASSESSMENT_V1)
                .flat_map(|p| p.artifact_ids.iter())
                .map(|id| {
                    artifact::<review_core::task::review::RepairAssessmentV1>(
                        cas,
                        id,
                        REPAIR_ASSESSMENT_V1,
                    )
                })
                .collect::<Result<_, _>>()?;
            if !repairs.is_empty() {
                value["repair_assessments"] =
                    serde_json::to_value(repairs).map_err(|e| e.to_string())?;
            }
            if state.revision.kind == "review" || !rounds.is_empty() {
                value["review_rounds"] = serde_json::to_value(rounds).map_err(|e| e.to_string())?;
            }
        }
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
            result.as_ref().map_or(
                match &state.phase {
                    TaskPhaseV1::Waiting { reason } => match reason {
                        TaskWaitingReasonV1::NeedsPlanReview => "needs-plan-review",
                        TaskWaitingReasonV1::NeedsResources => "needs-resources",
                        TaskWaitingReasonV1::NeedsInput => "needs-input",
                        TaskWaitingReasonV1::NeedsHuman => "needs-human",
                    },
                    _ => "planned",
                },
                |r| r.domain_conclusion.as_str()
            )
        );
        if let Some(id) = state.plan_id {
            println!("Plan {id}");
        }
        if let Some(last) = reports.last().and_then(|r| r["diagnostics"].as_object()) {
            for (node, diagnostic) in last {
                if let Some(message) = diagnostic["message"].as_str() {
                    println!("{node}: {message}");
                }
            }
        }
        if explain {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
            );
        }
    }
    if result.as_ref().is_some_and(|r| {
        matches!(
            r.domain_conclusion.as_str(),
            "pass" | "changes_requested" | "convergence_exhausted" | "incomplete"
        )
    }) {
        return Ok(result.map_or(0, |r| match r.domain_conclusion.as_str() {
            "pass" => 0,
            "changes_requested" | "convergence_exhausted" => 3,
            _ => 4,
        }));
    }
    let pending = if state.phase
        == (TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsHuman,
        }) {
        4
    } else {
        0
    };
    Ok(result.map_or(pending, |r| match r.acceptance {
        TaskAcceptanceV1::Satisfied => 0,
        TaskAcceptanceV1::Unsatisfied => 3,
        TaskAcceptanceV1::Inconclusive => 4,
    }))
}

fn delivery_view(cas: &Cas, task: &TaskProjection) -> Result<Option<serde_json::Value>, String> {
    let TaskPhaseV1::Finished { result_id } = &task.phase else {
        return Ok(None);
    };
    task.deliveries
        .iter()
        .rev()
        .find(|(_, delivery)| &delivery.result_id == result_id)
        .map(|(_, record)| {
            let value = cas
                .get_json(&record.receipt_id)
                .map_err(|e| e.to_string())?;
            super::task::validate_delivery_view(&value)?;
            Ok(value)
        })
        .transpose()
}

#[cfg(test)]
mod review_generation_tests {
    use super::*;

    #[test]
    fn review_generation_is_explicit_and_preserves_absent_compatibility() {
        let value = serde_json::json!({"reviewers":{"correctness":"required"},"gate":"major","clean_rounds":1,"max_rounds":2,"allow_targeted_repairs":false});
        let settings: ReviewSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(settings.policy_generation().unwrap(), 1);
        assert_eq!(serde_json::to_value(settings).unwrap(), value);
        let mut explicit = value;
        explicit["generation"] = serde_json::json!(2);
        assert_eq!(
            serde_json::from_value::<ReviewSettings>(explicit.clone())
                .unwrap()
                .policy_generation()
                .unwrap(),
            2
        );
        for invalid in [
            serde_json::json!(0),
            serde_json::json!(1),
            serde_json::json!(3),
            serde_json::json!(null),
            serde_json::json!("2"),
            serde_json::json!(4294967296_u64),
        ] {
            explicit["generation"] = invalid;
            assert!(
                serde_json::from_value::<ReviewSettings>(explicit.clone())
                    .map_err(|e| e.to_string())
                    .and_then(|v| v.policy_generation())
                    .is_err()
            );
        }
    }
}
