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
    CapturedTaskAuthority, CapturedTaskHost, NoTaskDeveloper, TaskDomain, TaskModelBinding,
};
use review_pipeline::task::optimization::{
    OptimizationCandidateTaskDomain, OptimizationTaskDomain, optimization_signatures,
};
use review_pipeline::task::provider::ProviderTaskDomain;
use review_pipeline::task::report_task::{ReportTaskDomain, ReportTaskPolicy, report_signatures};
use review_pipeline::task::review::{
    REVIEW_TASK_POLICY_SCHEMA, ReviewTaskDomain, ReviewTaskPolicy, review_signatures,
};
use review_pipeline::task::source::SnapshotTaskEnvironment;
use review_source_git::task::{SOURCE_TREE_V1, capture_snapshot, source_tree};
use review_source_git::{Capture, EntryKind, Manifest, Repo};
use review_store::store::task::{TaskLease, TaskProjection};
use review_store::{Cas, EventStore, validate_envelope};
use serde::{Deserialize, Serialize};
use serde_json::json;
mod bindings;
pub(super) mod candidate;
pub(crate) mod catalog;
pub(crate) mod collection;
mod context;
mod continuation;
pub(crate) mod defaults;
pub(super) mod developer;
pub(crate) mod domain;
pub(super) mod export;
pub(super) mod findings;
mod input_bindings;
mod input_file;
mod inspection;
mod issue;
mod planning;
pub(super) mod pr_body;
mod preview;
mod provider_admission;
pub(crate) mod recovery;
pub(super) mod refresh;
pub(crate) mod runs;
mod selection;
pub(crate) mod starter;
pub(crate) mod state;
mod task_file_home;
mod task_file_shape;
pub(super) mod task_report;

#[derive(Clone)]
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
    /// Pre-captured by the token-free `self optimize` adapter. Ordinary Task files leave this
    /// absent and may instead name a project-contained deterministic fixture.
    pub optimization_history: Option<review_core::task::optimization::OptimizationHistoryV1>,
    /// `af task plan --fit`: an infeasible plan is planned again at the smallest limits that fit.
    pub fit: bool,
    /// `af task plan --replace`: an unstarted Task of the same ID is re-planned in place.
    pub replace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskFile {
    /// Absent, the file is read as `af.task-file/1`, the one version this release reads.
    #[serde(default = "task_file_schema")]
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
    /// Root input ports bound to a recorded Task's output or to an exact artifact in the same
    /// Store, instead of being constructed by this adapter (ADR-0117), or — for any other root
    /// input the selected Pipeline declares — to one or several recorded outputs (ADR-0134). Absent by default, and
    /// absent on round-trip, so a Task file written before this decision keeps its exact
    /// revision, plan and `--json` documents.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    inputs: Option<BTreeMap<String, input_bindings::TaskInputRefV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    document_sources: Option<String>,
    /// A report Task's captured sources, in the `af.document-sources/1` file shape. Absent,
    /// the report reads the empty set (ADR-0133).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    report_sources: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    optimization_history: Option<String>,
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
    /// Absent, the Task's kind fills it in, with `verification` and `facts.standard` (see
    /// [`KIND_DEFAULTS`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    strategy: Option<String>,
    /// The requested acceptance profile is Task input, captured before selecting a Pipeline.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    verification: Option<FileVerification>,
    #[serde(default)]
    facts: BTreeMap<String, TaskFactV1>,
    /// Absent, the project's `[task.defaults] limits` of `.af/af.toml` fill it in (ADR-0203).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    limits: Option<FileLimits>,
    /// Continue a recorded Task from its reviewed candidate: that round's candidate Snapshot is
    /// the source and its findings the `findings` port (ADR-0203). Absent by default, and absent
    /// on round-trip, so a Task file without it keeps its exact revision and plan.
    #[serde(
        rename = "continue",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    continue_from: Option<continuation::ContinueRefV1>,
    /// Files the Worker reads first, captured by digest from the tree the Task reads, each a
    /// repository path with an optional `#section` (ADR-0203).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    context: Vec<String>,
    /// The tree before the change the Task's judging Workers read at `../base`; absent, the
    /// source Snapshot's recorded base (ADR-0203).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    base: Option<context::BaseRefV1>,
}

fn task_file_schema() -> String {
    "af.task-file/1".into()
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    report_policy: Option<String>,
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
    /// Task Review has one generation. Omitted or `2`, it selects that generation.
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
    fn check_generation(&self) -> Result<(), String> {
        match self.generation {
            None | Some(2) => Ok(()),
            Some(_) => Err("Review generation must be omitted or 2".into()),
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
    provider_admission: review_graph::task::OperatorAttemptCost,
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
    report_policy_id: Option<String>,
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
    /// What delivery does about `.af/` paths the declared layout does not name, as the project
    /// declared it at plan time. Part of the captured project policy identity, so editing
    /// `.af/af.toml` afterwards cannot change what an admitted plan agreed to. The default is
    /// not written down: a project that never heard of the knob keeps its exact policy ID.
    #[serde(
        default,
        skip_serializing_if = "review_config::layout::UndeclaredAfPathsPolicy::is_default"
    )]
    delivery_undeclared_af_paths: review_config::layout::UndeclaredAfPathsPolicy,
}

/// The captured delivery policy for undeclared `.af/` paths, read back from one recorded project
/// policy artifact — never from `.af/af.toml` on disk, which is what makes a later edit unable to
/// change an admitted plan. Records written before the policy existed, and every project that
/// keeps the default, declare nothing and get the default.
pub(crate) fn captured_undeclared_af_paths(
    cas: &Cas,
    policy_id: &str,
) -> Result<review_config::layout::UndeclaredAfPathsPolicy, String> {
    let recorded = cas.get_json(policy_id).map_err(|e| e.to_string())?;
    let declared = recorded.get("delivery_undeclared_af_paths").is_some();
    match serde_json::from_value::<RunAuthority>(recorded) {
        Ok(authority) => Ok(authority.delivery_undeclared_af_paths),
        // A recorded policy that is not this kernel's run authority declares no delivery
        // policy at all; one that names the field has to be readable exactly.
        Err(error) if declared => Err(format!("captured delivery policy: {error}")),
        Err(_) => Ok(Default::default()),
    }
}

/// The bindings a recorded Task revision kept, for the one consumer outside this module:
/// delivery names where this Task's source came from, and only the current revision's record
/// says that (ADR-0117).
pub(crate) fn recorded_input_bindings(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<review_core::task::input_bindings::TaskInputBindingsV1>, String> {
    input_bindings::recorded(cas, revision)
}

impl RunAuthority {
    fn invocation_policy_id(&self) -> Result<&str, String> {
        match (
            &self.code_policy_id,
            &self.document_policy_id,
            &self.report_policy_id,
        ) {
            (Some(id), None, None) | (None, Some(id), None) | (None, None, Some(id)) => Ok(id),
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
/// The recorded undeclared `.af/` paths of a Task whose profile has no `source` port.
const UNDECLARED_AF_PATHS_V1: &str = "af/UndeclaredAfPaths@1";

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "task-file-adapter-v1".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    }
}

/// A Task file, or the refusal that names the field path and the shape it takes.
fn parse_task_file(path: &Path, bytes: &[u8]) -> Result<TaskFile, String> {
    parse(path, bytes).map_err(|parser| task_file_shape::refusal(path, bytes, &parser))
}

/// The refusal of a Task file whose fields parse but do not name a Task: which field, what it
/// takes, and what it holds.
fn task_file_identity(path: &Path, file: &TaskFile) -> Result<(), String> {
    let named = format!("task file {}", path.display());
    if file.schema != "af.task-file/1" {
        return Err(format!(
            "{named}: schema must be \"af.task-file/1\", got {:?}",
            file.schema
        ));
    }
    if !is_name(&file.task_id) {
        return Err(format!(
            "{named}: task_id {:?} is not a Task ID: 1 to 128 ASCII letters, digits, `-` and \
             `_`, starting with a letter or digit",
            file.task_id
        ));
    }
    if !is_package_name(&file.kind) {
        return Err(format!(
            "{named}: kind {:?} is not a Task kind: names like `software` or `team/report`, \
             separated by `/`",
            file.kind
        ));
    }
    if file.goal.trim().is_empty() {
        return Err(format!("{named}: goal is empty; say what the Task is for"));
    }
    Ok(())
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

/// Where every repository's default Task state lives under an XDG state root.
pub(crate) fn local_task_states(xdg_state: &Path) -> PathBuf {
    xdg_state.join("af/task/local")
}

/// The Task state `af task` reads and writes for a canonical repository path when no `--state`
/// is given: one opaque directory per repository under [`local_task_states`].
pub(crate) fn default_task_state(xdg_state: &Path, repo: &Path) -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};
    let identity = Sha256::digest(repo.as_os_str().as_encoded_bytes());
    super::normalize_absolute(
        &local_task_states(xdg_state).join(&review_core::hex::encode(&identity)[..16]),
    )
}

pub(crate) fn state_path(repo: &Path, state: Option<&Path>) -> Result<(PathBuf, PathBuf), String> {
    let repo = std::fs::canonicalize(repo).map_err(|e| e.to_string())?;
    let state_given = state.is_some();
    let state = match state {
        Some(path) => super::resolve_filesystem_path(path)?,
        None => default_task_state(&super::xdg_state_root()?, &repo)?,
    };
    if state.starts_with(&repo) {
        return Err(format!(
            "Task state {} must be outside the repository {}; name a --state directory outside \
             it, or omit --state for the default under $XDG_STATE_HOME/af",
            state.display(),
            repo.display()
        ));
    }
    // A Store outside the default root is known to the Storage Budget only through the
    // registry (ADR-0144); recording is best effort. An explicit directory becomes af-owned, so
    // one holding anything else is refused first.
    if state_given {
        crate::storage::owned::admit(&state)?;
    }
    crate::storage::registry::record(&state, crate::storage::registry::StoreKind::Task);
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

/// The project's delivery policy for undeclared `.af/` paths, read out of the Authority
/// Snapshot. A project without `.af/af.toml`, or without the table, keeps the default.
fn captured_delivery_policy(
    cas: &Cas,
    manifest: &Manifest,
) -> Result<review_config::layout::UndeclaredAfPathsPolicy, String> {
    if manifest.get(".af/af.toml").is_none() {
        return Ok(Default::default());
    }
    let bytes = captured_file(cas, manifest, ".af/af.toml")?;
    let text = std::str::from_utf8(&bytes).map_err(|e| format!(".af/af.toml: {e}"))?;
    crate::project::ProjectFile::parse_delivery(text)
}

fn capture_authority(
    cas: &Cas,
    manifest: &Manifest,
    local: Option<&Path>,
    task_kind: &str,
    delivery_undeclared_af_paths: review_config::layout::UndeclaredAfPathsPolicy,
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
    let report_policy_id = domain::capture_policy::<ReportTaskPolicy>(
        cas,
        manifest,
        catalog.report_policy.as_deref(),
        ReportTaskPolicy::validate,
    )?;
    let initial_policy = code_policy_id
        .as_ref()
        .or(document_policy_id.as_ref())
        .or(report_policy_id.as_ref())
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
    let kind_profile = catalog
        .kinds
        .get(task_kind)
        .and_then(|name| capture.task_kind(name))
        .map(|kind| kind.profile);
    let is_document = kind_profile.map_or(task_kind == "document", |profile| {
        profile == TaskKindProfile::Document
    });
    let is_report = kind_profile.map_or(task_kind == "report", |profile| {
        profile == TaskKindProfile::Report
    });
    let (code_policy_id, document_policy_id, report_policy_id) = if is_document {
        (
            None,
            Some(document_policy_id.ok_or("Document Task requires a captured document policy")?),
            None,
        )
    } else if is_report {
        (
            None,
            None,
            Some(report_policy_id.ok_or(
                "Report Task requires a captured report policy: name it as report_policy in .af/task-catalog.toml",
            )?),
        )
    } else {
        (
            Some(code_policy_id.ok_or("Code/Review Task requires a captured code policy")?),
            None,
            None,
        )
    };
    let data_only = is_document || is_report;
    let authority = RunAuthority {
        schema: provider_admission::RUN_AUTHORITY_SCHEMA.into(),
        provider_admission,
        engine_id,
        code_policy_id: code_policy_id.clone(),
        document_policy_id,
        report_policy_id,
        review_policy_id: if data_only {
            None
        } else {
            catalog
                .review
                .map(|review| {
                    review.check_generation()?;
                    let review = ReviewTaskPolicy {
                        schema: REVIEW_TASK_POLICY_SCHEMA.into(),
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
        delivery_undeclared_af_paths,
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
    } else if let Some(id) = &authority.report_policy_id {
        let policy: ReportTaskPolicy =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        (
            report_signatures(id, &policy)?,
            BTreeMap::from([("verified".into(), "report".into())]),
        )
    } else {
        let id = authority.code_policy_id()?;
        let policy: CodeTaskPolicy =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        (
            {
                let mut signatures = code_signatures(id, &policy)?;
                signatures.extend(optimization_signatures(id)?);
                signatures
            },
            BTreeMap::from([
                ("verified".into(), "snapshot".into()),
                ("goal".into(), "snapshot".into()),
                ("analysis".into(), "report".into()),
                ("experiment".into(), "comparison".into()),
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
    // A report's author is whoever returns a draft of either version, whatever it declares,
    // so no Pipeline can make the author its own verifier.
    if authority.report_policy_id.is_some() {
        compiler = compiler.with_authored_artifacts(BTreeSet::from([
            review_core::task::document::DOCUMENT_DRAFT_V1.into(),
            review_core::task::document::DOCUMENT_DRAFT_V2.into(),
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
            if !matches!(worker.runner, TaskWorkerRunner::Command { .. }) {
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
    Ok(compiler
        .with_provider_admission(admission_cost)
        .with_project_briefs(std::sync::Arc::new(crate::project_brief::Resolver)))
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
    // First pass, token-free: every Model Worker's binding passes the static rules, in Worker-name
    // order, before any Provider CLI starts (ADR-0147, ADR-0148).
    let mut models = Vec::new();
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
        crate::model_check::static_rules(&crate::model_check::Binding {
            owner: format!("Worker {name}"),
            label: &name,
            provider: Some(alias),
            kind: provider_kind,
            model,
            effort,
        })?;
        models.push((
            name,
            alias.clone(),
            provider_kind.clone(),
            model.clone(),
            effort.clone(),
        ));
    }
    // At plan time, each Provider's catalog, read token-free once, judges every binding and a
    // `model` line names each verdict, before any Provider identity is probed.
    let owners: Vec<String> = models
        .iter()
        .map(|(name, ..)| format!("Worker {name}"))
        .collect();
    crate::model_check::catalog_rules(
        &models
            .iter()
            .zip(&owners)
            .map(
                |((name, alias, kind, model, effort), owner)| crate::model_check::Binding {
                    owner: owner.clone(),
                    label: name,
                    provider: Some(alias),
                    kind,
                    model,
                    effort,
                },
            )
            .collect::<Vec<_>>(),
    )?;
    // Second pass: one probe per Provider label, then each Worker's execution and adapter.
    let mut identities = BTreeMap::new();
    let mut adapters = BTreeMap::new();
    for (name, alias, provider_kind, model, effort) in models {
        if !identities.contains_key(&alias) {
            identities.insert(
                alias.clone(),
                super::providers::task::TaskProviderIdentity::probe(&alias, &provider_kind)?,
            );
        }
        let identity = &identities[&alias];
        let execution = identity.execution(&model, &effort)?;
        if !matches!(&execution,WorkerExecutionV1::Model {provider_kind:kind,..} if *kind==provider_kind)
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

/// How a started Task is reported: printed, as every CLI command does, or not at all, for the
/// browser's Pipelines pane, which renders the recorded plan itself.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Presentation {
    Print,
    Silent,
}

pub(super) fn start(options: StartOptions) -> Result<i32, String> {
    start_kind(options, None, Presentation::Print)
}

pub(super) fn start_review(options: StartOptions) -> Result<i32, String> {
    start_kind(options, Some("review"), Presentation::Print)
}

/// A plan preview as `af task explain --tree` prints it, and the Worker binding of every line
/// that names a slot, keyed by line index.
pub(crate) struct TreePreview {
    pub(crate) text: String,
    pub(crate) slots: BTreeMap<usize, String>,
}

/// The `--tree` preview of the plan `af task plan --file FILE` captures, compiled the same way
/// into a scratch Store that is discarded on return. No `--state` or XDG Store is written,
/// nothing is admitted, no Worker runs, and nothing is printed; the policy comes from the
/// committed authority, exactly as `af task plan --authority` takes it.
/// `authority` is the selector the plan compiles: a commit id binds the preview to the exact
/// commit whose declarations a caller shows, however `HEAD` moves meanwhile.
pub(crate) fn plan_tree_preview_at(
    file: &Path,
    repo: &Path,
    authority: &str,
) -> Result<TreePreview, String> {
    let bytes = input_file::read(file, 16 * 1024 * 1024, "Task file")?;
    let task = parse_task_file(file, &bytes)?;
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let state = scratch.path().join("state");
    let options = StartOptions {
        file: file.to_path_buf(),
        bindings: None,
        source_bindings: None,
        repo: repo.to_path_buf(),
        state: Some(state.clone()),
        authority: authority.into(),
        uncommitted: false,
        json: false,
        plan_only: true,
        timeout_secs: None,
        optimization_history: None,
        fit: false,
        replace: false,
    };
    start_kind(options, None, Presentation::Silent)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let projection = store
        .task_projection(&cas, &task.task_id)
        .map_err(|e| e.to_string())?
        .ok_or("The preview recorded no Task")?;
    let (text, slots) = preview::current_marked(&cas, &projection, true)?;
    Ok(TreePreview {
        text,
        slots: slots.into_iter().collect(),
    })
}

/// The Task's wall: the Task file's `limits.wall_ms`, shortened by `--timeout-secs`. A zero is
/// refused by the name it was given: the flag, or the field of the Task file `file`, with the
/// command that plans the file once it is fixed.
fn effective_task_wall_ms(
    file: &Path,
    file_wall_ms: u64,
    timeout_secs: Option<u64>,
) -> Result<u64, String> {
    if timeout_secs == Some(0) {
        return Err(
            "--timeout-secs must be positive: 0 seconds leaves the Task no time to run".into(),
        );
    }
    if file_wall_ms == 0 {
        use crate::error_document::{Cause, Detail};
        let shown = file.display().to_string();
        let again = plan_again(file);
        return Err(crate::error_document::typed(
            format!(
                "Task file {shown}: limits.wall_ms=0 must be positive: 0 ms leaves the Task no \
                 time to run; set limits.wall_ms to the Task's wall-clock budget in \
                 milliseconds, then run `{again}` again"
            ),
            Detail::new(Cause::TaskFileInvalid)
                .next(again)
                .with("file", shown)
                .with("field", "limits.wall_ms")
                .with("value", 0),
        ));
    }
    let timeout_ms = timeout_secs
        .map(|seconds| seconds.checked_mul(1000).ok_or("Task timeout overflow"))
        .transpose()?;
    Ok(timeout_ms.map_or(file_wall_ms, |limit| limit.min(file_wall_ms)))
}

/// What a Task file of each kind gets when it omits `strategy`: that strategy, then, unless the
/// file names them, this `verification` and `facts.standard = true`. A file that names its
/// strategy keeps the meaning it always had.
const KIND_DEFAULTS: [(&str, &str, Option<FileVerification>, bool); 3] = [
    (
        "implement",
        "reviewed",
        Some(FileVerification::Review),
        true,
    ),
    (
        "verification",
        "light",
        Some(FileVerification::Evaluation),
        true,
    ),
    ("report", "report", None, false),
];

/// The profile table a contradiction names: `implement -> reviewed/review/standard, ...`.
pub(crate) fn kind_defaults_table() -> String {
    KIND_DEFAULTS
        .iter()
        .map(|(kind, strategy, verification, standard)| {
            format!(
                "{kind} -> strategy {strategy}, verification {}, facts {}",
                verification.map_or("-", FileVerification::as_str),
                if *standard { "standard = true" } else { "-" }
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

impl FileVerification {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Evaluation => "evaluation",
            Self::Review => "review",
            Self::ReviewOrTargetedFixes => "review_or_targeted_fixes",
        }
    }
}

/// Fill what a Task file without `strategy` leaves to its kind; an unknown kind without a
/// strategy is refused with the table.
fn apply_kind_defaults(file: &mut TaskFile) -> Result<(), String> {
    if file.strategy.is_some() {
        return Ok(());
    }
    let Some((_, strategy, verification, standard)) =
        KIND_DEFAULTS.iter().find(|(kind, ..)| *kind == file.kind)
    else {
        return Err(format!(
            "Task file `{}` names no `strategy`, and kind `{}` has no default; add `strategy` to \
             the Task file, or use a kind that has one: {}",
            file.task_id,
            file.kind,
            kind_defaults_table()
        ));
    };
    file.strategy = Some((*strategy).to_owned());
    if file.verification.is_none() {
        file.verification = *verification;
    }
    if *standard {
        file.facts
            .entry("standard".into())
            .or_insert(TaskFactV1::Boolean(true));
    }
    Ok(())
}

/// What `start_captured` ended with: an exit code, or an infeasible plan `--fit` plans again.
enum Started {
    Code(i32),
    Refit(Box<review_core::task::limits_fit::TaskLimitsFitV1>),
}

fn start_kind(
    options: StartOptions,
    expected_kind: Option<&str>,
    presentation: Presentation,
) -> Result<i32, String> {
    // A duration flag is judged before any file is read: a wrong command line is said as such,
    // whatever the file holds.
    if options.timeout_secs == Some(0) {
        return Err(
            "--timeout-secs must be positive: 0 seconds leaves the Task no time to run".into(),
        );
    }
    if let Some(seconds) = options.timeout_secs {
        seconds.checked_mul(1000).ok_or("Task timeout overflow")?;
    }
    let bytes = input_file::read(&options.file, 16 * 1024 * 1024, "Task file")?;
    start_kind_bytes(options, bytes, expected_kind, presentation)
}

/// `start_kind` over Task file bytes already in hand: a file read from disk, or the file
/// `af task run NEW --from TASK` derives from the continued Task's captured one.
fn start_kind_bytes(
    options: StartOptions,
    mut bytes: Vec<u8>,
    expected_kind: Option<&str>,
    presentation: Presentation,
) -> Result<i32, String> {
    let original = parse_task_file(&options.file, &bytes)?;
    let mut refitted = false;
    // `--fit` plans again at the limits the planner computed; a second refusal can only raise
    // them further (Provider admission is counted once the Workers are bound), so this ends.
    for _ in 0..3 {
        match start_bytes(options.clone(), bytes.clone(), expected_kind, presentation)? {
            Started::Code(code) => {
                if refitted && presentation == Presentation::Print {
                    let fitted = parse_task_file(&options.file, &bytes)?;
                    if let (Some(original), Some(fitted)) = (&original.limits, &fitted.limits) {
                        eprintln!("{}", fit_difference(original, fitted));
                    }
                }
                return Ok(code);
            }
            Started::Refit(fit) => {
                let mut file = parse_task_file(&options.file, &bytes)?;
                file.limits = Some(FileLimits {
                    tokens: fit.required.tokens,
                    max_attempts: fit.required.max_attempts,
                    wall_ms: fit.required.wall_ms,
                    verification: fit.required.verification.clone(),
                });
                // The request planned is the fitted one; the Store captures exactly it.
                bytes = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
                bytes.push(b'\n');
                refitted = true;
            }
        }
    }
    Err(format!(
        "af task plan --fit: {} still does not fit after three plans; read the refusal above",
        options.file.display()
    ))
}

/// The line `af task plan --fit` prints on stderr: every limit it raised, from what the Task
/// file supplied to what the plan was made with.
fn fit_difference(supplied: &FileLimits, fitted: &FileLimits) -> String {
    let pairs = [
        ("tokens", supplied.tokens, fitted.tokens),
        (
            "max_attempts",
            u64::from(supplied.max_attempts),
            u64::from(fitted.max_attempts),
        ),
        ("wall_ms", supplied.wall_ms, fitted.wall_ms),
        (
            "verification.tokens",
            supplied.verification.tokens,
            fitted.verification.tokens,
        ),
        (
            "verification.attempts",
            u64::from(supplied.verification.attempts),
            u64::from(fitted.verification.attempts),
        ),
        (
            "verification.wall_ms",
            supplied.verification.wall_ms,
            fitted.verification.wall_ms,
        ),
    ];
    let raised = pairs
        .iter()
        .filter(|(_, from, to)| from != to)
        .map(|(name, from, to)| format!("{name} {from} -> {to}"))
        .collect::<Vec<_>>();
    format!(
        "af task plan --fit: planned with {}; the Task file on disk is unchanged",
        raised.join(", ")
    )
}

/// The record a Task file's revision names as its adapter: the file it captured.
pub(crate) const TASK_FILE_ADAPTER_V1: &str = "af.task-file-adapter/1";

/// The Task file adapter record of `revision`, reached through a selection's wrapper; `None`
/// for a revision no Task file made.
pub(crate) fn task_file_adapter(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<serde_json::Value>, String> {
    let mut id = revision.provenance.adapter_id.clone();
    // A selection wraps the adapter once; a bounded walk, never a loop.
    for _ in 0..4 {
        let bytes = cas.get(&id).map_err(|e| e.to_string())?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Ok(None);
        };
        match value["schema"].as_str() {
            Some(TASK_FILE_ADAPTER_V1) => return Ok(Some(value)),
            Some("af.selected-task-adapter/1") => {
                id = value["source_adapter_id"]
                    .as_str()
                    .ok_or("Selected Task adapter names no source adapter")?
                    .to_owned();
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// What `af task plan` does with a Task file whose ID the Store already holds.
enum Existing {
    /// Its content and bindings equal the planned Task's: the plan stands.
    Same,
    /// `--replace` of an unstarted Task: plan it again as this next revision.
    Replace(Box<TaskProjection>),
}

fn existing_task(
    cas: &Cas,
    store: &EventStore,
    options: &StartOptions,
    bytes: &[u8],
    task_id: &str,
) -> Result<Option<Existing>, String> {
    let Some(current) = store
        .task_projection(cas, task_id)
        .map_err(|e| e.to_string())?
    else {
        if store
            .collected_task(task_id)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err(format!(
                "Task `{task_id}` was collected from this Store; give the Task file a new \
                 `task_id` to plan it again"
            ));
        }
        return Ok(None);
    };
    let file = options.file.display();
    let captured = task_file_adapter(cas, &current.revision)?
        .and_then(|adapter| adapter["source_file_id"].as_str().map(str::to_owned));
    let same_file = captured.as_deref() == Some(&review_store::canonical::blob_content_id(bytes));
    let authority: RunAuthority = serde_json::from_value(
        cas.get_json(&current.revision.authority.policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let bindings = options
        .bindings
        .as_deref()
        .map(bindings::read)
        .transpose()?
        .map(|local| {
            serde_json::to_value(&local.definition)
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    review_store::canonical::canonicalize(&value)
                        .map(|bytes| review_store::canonical::blob_content_id(&bytes))
                        .map_err(|e| e.to_string())
                })
        })
        .transpose()?;
    let started = state::started(&current);
    if same_file && bindings == authority.local_bindings_id {
        return Ok(Some(Existing::Same));
    }
    if !options.replace {
        let why = if same_file {
            "with other --bindings"
        } else {
            "from a different Task file"
        };
        return Err(if started {
            format!(
                "Task `{task_id}` already exists in this Store and has run; {file} plans it {why}. \
                 Give the Task file a new `task_id`, or resume the existing Task with \
                 `af task run {task_id}`"
            )
        } else {
            format!(
                "Task `{task_id}` is already planned {why}: {file} differs from the plan it \
                 recorded. Re-plan it in place with `af task plan --file {} --replace`, or give \
                 the Task file a new `task_id`",
                shell_words::quote(&options.file.display().to_string())
            )
        });
    }
    if same_file {
        return Err(format!(
            "Task `{task_id}` is planned from this same Task file with other --bindings; \
             --replace plans an unstarted Task again from a changed Task file, so give the Task \
             file a new `task_id` to plan it with these bindings"
        ));
    }
    if started
        || !matches!(
            current.phase,
            TaskPhaseV1::Ready {} | TaskPhaseV1::Submitted {}
        )
    {
        return Err(format!(
            "Task `{task_id}` has already run, so --replace cannot plan it again; give the Task \
             file a new `task_id`"
        ));
    }
    Ok(Some(Existing::Replace(Box::new(current))))
}

fn start_bytes(
    options: StartOptions,
    bytes: Vec<u8>,
    expected_kind: Option<&str>,
    presentation: Presentation,
) -> Result<Started, String> {
    let started = clock()?;
    let mut file = parse_task_file(&options.file, &bytes)?;
    task_file_identity(&options.file, &file)?;
    apply_kind_defaults(&mut file)?;
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
    // Limits the project's defaults fill in are judged once they are read.
    if let Some(limits) = &file.limits {
        let wall = effective_task_wall_ms(&options.file, limits.wall_ms, options.timeout_secs)?;
        started.checked_add(wall).ok_or("Task deadline overflow")?;
    }
    if file.continue_from.is_some()
        && file
            .inputs
            .as_ref()
            .is_some_and(|inputs| inputs.contains_key("source"))
    {
        return Err(format!(
            "Task file `{}` continues a Task and binds `inputs.source`; a continuation reads the \
             continued Task's reviewed candidate as its source, so drop `inputs.source`",
            file.task_id
        ));
    }
    let (repo, state) = state_path(&options.repo, options.state.as_deref())?;
    crate::storage::owned::prepare(&state, options.state.is_some())?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let replace = match existing_task(&cas, &store, &options, &bytes, &file.task_id)? {
        None => None,
        Some(Existing::Same) if !options.plan_only => {
            return Err(format!(
                "Task `{}` already exists in this Store from this Task file; run or resume it \
                 with `af task run {}`, or give the Task file a new `task_id`",
                file.task_id, file.task_id
            ));
        }
        Some(Existing::Same) => {
            // The same request: nothing is captured again and the plan stands.
            if presentation == Presentation::Print {
                eprintln!(
                    "Task {} is already planned from this Task file; showing its plan",
                    file.task_id
                );
            }
            return match presentation {
                Presentation::Print => present(&cas, &store, &file.task_id, options.json, true)
                    .map(|presented| Started::Code(presented.code)),
                Presentation::Silent => Ok(Started::Code(0)),
            };
        }
        Some(Existing::Replace(current)) => Some(current),
    };
    let git_home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let source_repo = Repo::open(&repo, git_home.path());
    let policy_source = Capture::new(&source_repo, &cas)
        .committed(&options.authority)
        .map_err(|e| e.to_string())?;
    let delivery = captured_delivery_policy(&cas, &policy_source.manifest)?;
    let authority = capture_authority(
        &cas,
        &policy_source.manifest,
        options.bindings.as_deref(),
        &file.kind,
        delivery,
    )?;
    if expected_kind.is_some()
        && selected_profile(&authority.2, &authority.1, &file.kind, file.verification)?
            != TaskKindProfile::Review
    {
        return Err("af review --file requires a Review Task profile".into());
    }
    // What the Task file leaves to the project's `[task.defaults]` (ADR-0203), filled in from
    // the Authority Snapshot before anything is recorded, and judged as a stated limit is.
    let project_defaults = defaults::captured(&cas, &policy_source.manifest)?;
    let filled = defaults::apply(
        &mut file,
        project_defaults.as_ref(),
        authority.2.pipelines(),
    )?;
    let limits = file.limits.as_ref().ok_or("Task file has no limits")?;
    let wall = effective_task_wall_ms(&options.file, limits.wall_ms, options.timeout_secs)?;
    started.checked_add(wall).ok_or("Task deadline overflow")?;
    if presentation == Presentation::Print
        && let Some(line) = defaults::line(&filled)
    {
        eprintln!("{line}");
    }
    let binds_source = file
        .inputs
        .as_ref()
        .is_some_and(|inputs| inputs.contains_key("source"));
    if options.uncommitted && binds_source {
        return Err(
            "--uncommitted captures the invoking checkout, which a bound `source` \
                    replaces; plan from the checkout or drop the binding"
                .into(),
        );
    }
    if options.uncommitted && file.continue_from.is_some() {
        return Err(
            "--uncommitted captures the invoking checkout, and a continuation reads the \
             continued Task's reviewed candidate instead; drop --uncommitted"
                .into(),
        );
    }
    if file.base.is_some() && file.continue_from.is_some() {
        return Err(format!(
            "Task file `{}` names a `base` and continues a Task; a continuation's base is the \
             candidate it continues, so drop `base`",
            file.task_id
        ));
    }
    // A declared base is a committed tree of this repository, captured now like the source.
    let declared_base = file
        .base
        .as_ref()
        .map(|base| context::capture_base(&source_repo, &cas, base))
        .transpose()?;
    // The standing rules are policy: read from the Authority Snapshot, like the catalog.
    let rules = context::rules(&policy_source.manifest)?;
    let source = if options.uncommitted {
        Capture::new(&source_repo, &cas)
            .dirty()
            .map_err(|e| e.to_string())?
    } else {
        policy_source
    };
    start_captured(
        options,
        file,
        bytes,
        started,
        cas,
        store,
        (source, declared_base, rules),
        authority,
        presentation,
        replace,
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
    (source, declared_base, rules): (
        review_source_git::Snapshot,
        Option<context::DeclaredBase>,
        Option<review_core::task::findings::TaskReadFirstEntryV1>,
    ),
    (authority_id, authority, mut compiler): (String, RunAuthority, TaskPlanCompiler),
    presentation: Presentation,
    replace: Option<Box<TaskProjection>>,
) -> Result<Started, String> {
    let profile = selected_profile(&compiler, &authority, &file.kind, file.verification)?;
    let (_, state) = state_path(&options.repo, options.state.as_deref())?;
    // Resolved once, here, from the `--state` Store alone, before any Worker is dispatched or
    // any Provider admitted. A bound port replaces this adapter's construction of it and
    // nothing else changes (ADR-0117).
    let declared = file.inputs.as_ref().map(|_| {
        let named = file.pipeline.as_ref().map(|choice| choice.name.as_str());
        input_bindings::DeclaredPorts::of(compiler.pipelines(), &file.kind, named)
    });
    let bound = file
        .inputs
        .as_ref()
        .zip(declared.as_ref())
        .map(|(references, declared)| {
            input_bindings::resolve(&cas, &store, profile, declared, references)
        })
        .transpose()?;
    let bound_ports = bound
        .as_ref()
        .map(|bound| bound.ports.clone())
        .unwrap_or_default();
    if bound_ports.contains_key("sources") && file.document_sources.is_some() {
        return Err("A bound sources port and document_sources claim one input".into());
    }
    if bound_ports.contains_key("sources") && file.report_sources.is_some() {
        return Err("A bound sources port and report_sources claim one input".into());
    }
    if file.report_sources.is_some() && profile != TaskKindProfile::Report {
        return Err("report_sources is only valid for a report Task".into());
    }
    if bound_ports.contains_key("history")
        && (file.optimization_history.is_some() || options.optimization_history.is_some())
    {
        return Err("A bound history port and a history file claim one port".into());
    }
    // A continuation is resolved from this Store too, before anything is recorded: the
    // reviewed candidate becomes the source and the round's findings the `findings` port, which
    // the selected Pipeline must take, or the implementer would never read them (ADR-0203).
    let continued = file
        .continue_from
        .as_ref()
        .map(|reference| continuation::resolve(&cas, &store, reference))
        .transpose()?;
    if file.base.is_some()
        && matches!(
            profile,
            TaskKindProfile::Document | TaskKindProfile::OptimizationAnalysis
        )
    {
        return Err(format!(
            "base: a {} Task reads no source tree, so it has no change to set a base for; drop \
             `base`",
            file.kind
        ));
    }
    if let Some(continued) = &continued {
        if matches!(
            profile,
            TaskKindProfile::Document | TaskKindProfile::OptimizationAnalysis
        ) {
            return Err(format!(
                "continue: a {} Task reads no source tree, so it cannot continue Task `{}`",
                file.kind,
                preview::text(&continued.findings.continues.task_id)
            ));
        }
        let named = file.pipeline.as_ref().map(|choice| choice.name.as_str());
        let declared = input_bindings::DeclaredPorts::of(compiler.pipelines(), &file.kind, named);
        let takes = declared
            .declares(
                continuation::FINDINGS_PORT,
                review_core::task::findings::TASK_REVIEW_FINDINGS_V1,
            )
            .map_err(|error| format!("continue: {error}"))?;
        if !takes {
            let whose = match named {
                Some(name) => format!("the Pipeline {}", preview::text(name)),
                None => format!(
                    "no captured Pipeline accepting kind {}",
                    preview::text(&file.kind)
                ),
            };
            return Err(format!(
                "continue: {whose} declares the optional `findings` input, so its implementer \
                 could never read the findings; name a Pipeline that does in the Task file's \
                 `pipeline` (`af task catalog` lists the captured ones)"
            ));
        }
    }
    // What the declared layout says about the Snapshot this Task will read. A pure function of
    // a recorded manifest: no working tree is walked, nothing is removed, and Snapshot identity
    // is whatever the capture already made it. The result is advice here and a receipt field
    // later. A bound `source` reads the bound tree, so that is the tree classified.
    let read = match (continued.as_ref(), bound.as_ref()) {
        (Some(continued), _) => &continued.manifest,
        (None, Some(bound)) => bound.source_manifest.as_ref().unwrap_or(&source.manifest),
        (None, None) => &source.manifest,
    };
    // The rules and context the Worker reads first, bound by digest: the rules from the
    // Authority Snapshot, the context from the tree the Task reads.
    let read_first = context::capture(&file.context, rules, read)?;
    let undeclared = review_config::layout::classify_manifest(read).undeclared;
    if presentation == Presentation::Print {
        super::af_paths::warn(&undeclared);
    }
    let origin=cas.put_json(&json!({"schema":"af.task-source-origin/1","repository_id":source.repository_id,"source_revision":source.source_revision,"content_digest":source.content_digest})).map_err(|e|e.to_string())?;
    let source_port = if matches!(
        profile,
        TaskKindProfile::Document | TaskKindProfile::OptimizationAnalysis
    ) {
        None
    } else {
        if file.document_sources.is_some() {
            return Err("Document source input is only valid for a document Task".into());
        }
        // A bound or continued `source` captures no Git tree from the invoking checkout.
        match continued
            .as_ref()
            .map(|continued| &continued.source)
            .or(bound_ports.get("source"))
        {
            Some(port) => Some(port.clone()),
            None => {
                let snapshot = capture_snapshot(&cas, &source.manifest, &origin, None)?;
                Some(source_tree(
                    &cas,
                    producer(),
                    &snapshot,
                    vec![origin.clone()],
                )?)
            }
        }
    };
    // A profile without a `source` port records nothing a later presentation could classify
    // again, so the observation itself is kept: one typed artifact, referenced from the
    // revision's provenance, and only when there is something to report, so a Task over a
    // fully declared tree keeps the revision identity it always had.
    let undeclared_record = if source_port.is_none() && !undeclared.is_empty() {
        let (id, _) = cas
            .put_artifact(
                UNDECLARED_AF_PATHS_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&undeclared).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        Some(id)
    } else {
        None
    };
    let input_file = cas.put(&bytes).map_err(|e| e.to_string())?;
    let limits = file.limits.clone().ok_or("Task file has no limits")?;
    let wall = effective_task_wall_ms(&options.file, limits.wall_ms, options.timeout_secs)?;
    // The deadline is taken from as late as the revision allows: its clock starts at the
    // first admission (P5), and the time spent here before the Task is recorded would come out
    // of its wall.
    let deadline_base = clock()?;
    let deadline = deadline_base
        .checked_add(wall)
        .ok_or("Task deadline overflow")?;
    let issue = file
        .issue
        .as_ref()
        .map(|selected| {
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
    // The Worker's requirements end with what to read first, each file by the digest of the
    // bytes the Task captured, so a changed file is a changed plan.
    let base_id = declared_base
        .as_ref()
        .map(|base| context::record_base(&cas, base, &source.repository_id))
        .transpose()?;
    let read_first_id = match &read_first {
        Some(record) => {
            let text = requirements_payload["text"].as_str().unwrap_or_default();
            requirements_payload["text"] =
                json!(format!("{text}\n\n{}", record.lines().join("\n")));
            let id = context::record(&cas, record)?;
            input_refs.push(id.clone());
            Some(id)
        }
        None => None,
    };
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
    let mut adapter = json!({"schema":TASK_FILE_ADAPTER_V1,"source_file_id":input_file});
    if let Some(issue) = &issue {
        adapter["source_capture_id"] = json!(issue.capture_id);
    }
    let adapter = cas.put_json(&adapter).map_err(|e| e.to_string())?;
    let goal = match &issue {
        Some(issue) => format!("{}\n\n{}", file.goal, issue.requirements.text),
        None => file.goal.clone(),
    };
    let optimization_finalize = file.facts.get("finalize") == Some(&TaskFactV1::Boolean(true));
    // `--replace` plans the Task again as its next revision; nothing of it ran.
    let (revision_number, previous_revision_id) = match &replace {
        Some(current) => (
            current
                .revision
                .revision
                .checked_add(1)
                .ok_or("Task revision overflow")?,
            Some(current.revision_id.clone()),
        ),
        None => (1, None),
    };
    let strategy = file.strategy.clone().ok_or("Task file has no strategy")?;
    let mut revision=TaskRevisionV1 {
        task_id:file.task_id,revision:revision_number,previous_revision_id,kind:file.kind,goal,
        inputs:BTreeMap::from([("requirements".into(),ArtifactInputV1 {artifact_ids:vec![requirements.clone()],artifact_type:"af/Requirements@1".into(),cardinality:PortCardinality::One,snapshot_id:None})]),
        required_outputs:serde_json::from_value(json!({"snapshot":{"artifact_type":SOURCE_TREE_V1,"cardinality":"one"},"verification":{"artifact_type":VERIFICATION_RESULT_V1,"cardinality":"one"}})).map_err(|e|e.to_string())?,
        acceptance:BTreeMap::from([("verified".into(),AcceptanceObligationV1 {evidence_type:VERIFICATION_RESULT_V1.into(),verifier_policy:authority.invocation_policy_id()?.into()})]),
        provenance:TaskProvenanceV1 {adapter_id:adapter,input_artifact_ids:std::iter::once(requirements).chain(undeclared_record).chain(read_first_id).chain(base_id).chain(continued.as_ref().map(|continued|continued.findings_id.clone())).collect()},
        authority:TaskAuthorityV1 {policy_id:authority_id,allowed_effects:BTreeSet::from(["read-source".into(),"write-source".into(),"execute-checks".into()]),data_destinations:BTreeSet::new()},
        limits:TaskLimitsV1 {tokens:limits.tokens,max_attempts:limits.max_attempts,deadline_unix_ms:deadline,verification:limits.verification},
        strategy,pipeline:file.pipeline,facts:file.facts,
    };
    if let Some(source_port) = source_port {
        revision.inputs.insert("source".into(), source_port);
    }
    if let Some(continued) = &continued {
        revision
            .inputs
            .insert(continuation::FINDINGS_PORT.into(), continued.port());
    }
    if profile == TaskKindProfile::Report {
        use review_core::task::report_task::*;
        // A bound `sources` port replaces this construction. Without `report_sources` the
        // report reads the empty set, so every report names the exact sources it was checked
        // against.
        if !bound_ports.contains_key("sources") {
            let (sources, refs) = match file.report_sources.as_deref() {
                Some(path) => {
                    if !review_config::task::shared::safe_relative_path(path) {
                        return Err("Report source path must be project-relative".into());
                    }
                    let bytes = captured_file(&cas, &source.manifest, path)?;
                    // The file's own bytes first: identities and text together, before parsing.
                    if bytes.len() > MAX_REPORT_SOURCES_FILE_BYTES {
                        return Err(format!(
                            "{path}: report sources file is {} bytes, over its {} KiB bound",
                            bytes.len(),
                            MAX_REPORT_SOURCES_FILE_BYTES / 1024
                        ));
                    }
                    let sources: ReportSourcesV1 = parse(Path::new(path), &bytes)?;
                    sources
                        .validate()
                        .map_err(|error| format!("{path}: {error}"))?;
                    let raw = cas.put(&bytes).map_err(|e| e.to_string())?;
                    (sources, vec![raw, origin.clone()])
                }
                None => (ReportSourcesV1::empty(), vec![origin.clone()]),
            };
            let id = cas
                .put_artifact(
                    REPORT_SOURCES_V1,
                    producer(),
                    refs,
                    None,
                    serde_json::to_value(sources).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .0;
            revision.inputs.insert(
                "sources".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![id.clone()],
                    artifact_type: REPORT_SOURCES_V1.into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: None,
                },
            );
            revision.provenance.input_artifact_ids.push(id);
            revision.provenance.input_artifact_ids.sort();
        }
        // An author reads the source and may run commands in a clone that seals nothing back;
        // nothing in a report Task may write source, so `af task deliver` has nothing to place.
        revision.authority.allowed_effects =
            BTreeSet::from(["read-source".into(), "execute-checks".into()]);
        revision.required_outputs = serde_json::from_value(json!({"report":{"artifact_type":review_core::task::document::DOCUMENT_V1,"cardinality":"one"},"verification":{"artifact_type":REPORT_VERIFICATION_V1,"cardinality":"one"}})).map_err(|e|e.to_string())?;
        revision.acceptance = BTreeMap::from([(
            "verified".into(),
            AcceptanceObligationV1 {
                evidence_type: REPORT_VERIFICATION_V1.into(),
                verifier_policy: authority.invocation_policy_id()?.into(),
            },
        )]);
    }
    if profile == TaskKindProfile::Document {
        use review_core::task::document::*;
        // A bound `sources` port replaces this construction, which is what makes
        // `document_sources` — and the exported file it names — unnecessary.
        if !bound_ports.contains_key("sources") {
            let declared = file.document_sources.as_deref();
            let path =
                declared.ok_or("Document Task needs document_sources or a bound sources port")?;
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
                    vec![raw, origin.clone()],
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
        }
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
    if matches!(
        profile,
        TaskKindProfile::OptimizationAnalysis | TaskKindProfile::OptimizationCandidate
    ) {
        use review_core::task::optimization::*;
        if file.document_sources.is_some() {
            return Err("Document source input is not valid for an Optimization Task".into());
        }
        // A bound `history` replaces the captured or pre-captured construction below; the
        // bound port itself is installed with every other binding, after this block.
        let history_port = if bound_ports.contains_key("history") {
            None
        } else {
            let (history, raw) = if let Some(history) = options.optimization_history.clone() {
                let bytes = serde_json::to_vec(&history).map_err(|e| e.to_string())?;
                let raw = cas.put(&bytes).map_err(|e| e.to_string())?;
                (history, raw)
            } else {
                let path = file.optimization_history.as_deref().ok_or(
                    "Optimization Task needs captured history or an optimization_history fixture",
                )?;
                if !review_config::task::shared::safe_relative_path(path) {
                    return Err("Optimization history path must be project-relative".into());
                }
                let bytes = captured_file(&cas, &source.manifest, path)?;
                let history: OptimizationHistoryV1 = parse(Path::new(path), &bytes)?;
                let raw = cas.put(&bytes).map_err(|e| e.to_string())?;
                (history, raw)
            };
            history.validate()?;
            let mut refs = vec![raw, origin.clone()];
            if let Some(previous) = &history.previous_capture_id {
                cas.verify(previous).map_err(|_| {
                    "Optimization history predecessor is not retained in this Task Store"
                        .to_string()
                })?;
                refs.push(previous.clone());
            }
            let id = cas
                .put_artifact(
                    OPTIMIZATION_HISTORY_V1,
                    producer(),
                    refs,
                    None,
                    serde_json::to_value(history).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .0;
            revision.provenance.input_artifact_ids.push(id.clone());
            revision.provenance.input_artifact_ids.sort();
            Some(ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: OPTIMIZATION_HISTORY_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: None,
            })
        };
        if profile == TaskKindProfile::OptimizationAnalysis {
            revision.inputs.clear();
        }
        if let Some(history_port) = history_port {
            revision.inputs.insert("history".into(), history_port);
        }
        revision.authority.allowed_effects.clear();
        revision.required_outputs = if profile == TaskKindProfile::OptimizationAnalysis {
            serde_json::from_value(json!({
                "economics":{"artifact_type":OPTIMIZATION_ECONOMICS_V1,"cardinality":"one"},
                "report":{"artifact_type":OPTIMIZATION_REPORT_V1,"cardinality":"one"}
            }))
        } else {
            serde_json::from_value(json!({
                "comparison":{"artifact_type":review_core::task::optimization_experiment::EXPERIMENT_COMPARISON_V1,"cardinality":"one"}
            }))
        }
        .map_err(|e| e.to_string())?;
        revision.acceptance = BTreeMap::from([(
            if profile == TaskKindProfile::OptimizationAnalysis {
                "analysis".into()
            } else {
                "experiment".into()
            },
            AcceptanceObligationV1 {
                evidence_type: if profile == TaskKindProfile::OptimizationAnalysis {
                    OPTIMIZATION_REPORT_V1.into()
                } else {
                    review_core::task::optimization_experiment::EXPERIMENT_COMPARISON_V1.into()
                },
                verifier_policy: authority.code_policy_id()?.into(),
            },
        )]);
    }
    if profile == TaskKindProfile::OptimizationCandidate
        && (file
            .requirements
            .as_ref()
            .is_some_and(|v| v.get("candidate").is_some())
            || optimization_finalize)
    {
        revision.required_outputs.insert(
            "snapshot".into(),
            serde_json::from_value(json!({"artifact_type":"af/SourceTree@1","cardinality":"one"}))
                .map_err(|e| e.to_string())?,
        );
        revision.required_outputs.insert(
            "verification".into(),
            serde_json::from_value(
                json!({"artifact_type":"af/OptimizationVerification@1","cardinality":"one"}),
            )
            .map_err(|e| e.to_string())?,
        );
        revision.acceptance.insert(
            "verified".into(),
            AcceptanceObligationV1 {
                evidence_type: "af/OptimizationVerification@1".into(),
                verifier_policy: authority.code_policy_id()?.into(),
            },
        );
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
    // Installed last, so a profile that rebuilds its own input set cannot drop a bound port,
    // and recorded once as `af/TaskInputBindings@1` in the revision's provenance — written only
    // when the Task file carried an `inputs` table, so a Task without bindings keeps the exact
    // revision identity it has today.
    for (port, input) in &bound_ports {
        revision.inputs.insert(port.clone(), input.clone());
    }
    if let Some(bound) = &bound {
        let ids = &mut revision.provenance.input_artifact_ids;
        ids.push(bound.record_id.clone());
        ids.sort();
        ids.dedup();
    }
    // The Project Brief's initial capture, before any pipeline is selected: the pinned Survey,
    // the `[brief]` policy and the ancestry of every Survey head against the source commit, so
    // every later normalization renders from them alone (ADR-0160). The Survey is the Project's
    // of the resolved source: a bound `source` names its own repository, which need not be the
    // invoking checkout's. Construction is decided over the bound ports, so a Brief port the
    // Task file binds reads no Survey and writes no pin.
    let constructs = crate::project_brief::constructs(
        compiler.pipelines(),
        revision.pipeline.as_ref(),
        authority.no_match,
        authority.planner.is_some(),
        &bound_ports,
    );
    let source_origin = match bound_ports.get("source") {
        Some(port) => input_bindings::bound_source_origin(&cas, port)?,
        None => crate::project_brief::SourceOrigin {
            repository_id: source.repository_id.clone(),
            commit: source.source_revision.clone(),
        },
    };
    if let Some(id) = crate::project_brief::capture(
        &cas,
        &options.repo,
        &source_origin,
        constructs,
        &bound_ports,
    )? {
        let ids = &mut revision.provenance.input_artifact_ids;
        ids.push(id);
        ids.sort();
        ids.dedup();
    }
    // The deadline starts when the Task first runs, so planning judges its whole wall.
    let plan_clock = selection::PlanClock {
        now: deadline_base,
        wall_ms: wall,
    };
    let prepared = match presentation {
        Presentation::Print => match selection::prepare(
            &cas,
            &authority,
            &compiler,
            revision,
            options.json,
            plan_clock,
            options.fit,
            &options.file,
        )? {
            selection::Prepared::Ready(prepared) => Some(prepared),
            selection::Prepared::Refused => None,
            selection::Prepared::Refit(fit) => return Ok(Started::Refit(fit)),
        },
        Presentation::Silent => {
            silent_selection(&cas, &authority, &compiler, revision, plan_clock)?
        }
    };
    let Some(selected) = prepared else {
        return Ok(Started::Code(1));
    };
    let selected = match selected {
        selection::PreparedSelection::Selected(selected) => selected,
        selection::PreparedSelection::Generation { .. } if presentation == Presentation::Silent => {
            return Err(
                "A generated plan needs developer review; preview it with af task plan".into(),
            );
        }
        selection::PreparedSelection::Generation {
            revision,
            revision_id,
        } => {
            if replace.is_some() {
                return Err(
                    "--replace cannot re-plan a Task whose request needs a generated plan; give \
                     the Task file a new `task_id`"
                        .into(),
                );
            }
            return planning::start(
                options,
                cas,
                store,
                authority,
                compiler,
                *revision,
                revision_id,
                undeclared,
            )
            .map(Started::Code);
        }
    };
    let selection::SelectedTask {
        revision,
        revision_id,
        compiler: selected_compiler,
        adapters,
        mut plan,
        mut graph,
    } = *selected;
    compiler = selected_compiler;
    if profile == TaskKindProfile::OptimizationCandidate {
        (compiler, plan, graph) = install_candidate_experiment(
            &cas,
            compiler,
            &authority,
            &revision,
            &revision_id,
            &graph,
            &plan,
        )?;
    }
    if plan.task_revision_id != revision_id
        || plan.authority != revision.authority
        || plan.limits != revision.limits
        || plan.inputs != revision.inputs
    {
        return Err("Optimization plan recompilation changed captured Task authority".into());
    }
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
    let inner = captured_domain(
        &cas,
        &authority,
        profile,
        graph.clone(),
        &plan,
        &compiler,
        &state,
        &options.repo,
    )?;
    let models = model_bindings(&plan, &graph, &adapters)?;
    let domain = ProviderTaskDomain {
        graph: &graph,
        models: &models,
        inner: inner.as_ref(),
    };
    let environment = domain::environment(&cas, &authority, profile)?;
    let host = CapturedTaskHost::capture_with_models(
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
    let writer = match &replace {
        Some(_) => format!("cli-{}", writer_pid()),
        // The opening writer is what makes one Store's Task distinguishable from another
        // Store's Task of the same file (ADR-0140 derives the gate owner from this transition),
        // so it carries 64 bits from the operating system, not the PID alone.
        None => format!("cli-{}-{:016x}", std::process::id(), opening_nonce()),
    };
    // A run records its process before it takes the lease, so `af task cancel` finds it
    // (ADR-0202); a plan runs nothing to cancel.
    let _run_record = (!options.plan_only)
        .then(|| runs::begin(&state, &revision.task_id, &writer))
        .transpose()?;
    let lease = match &replace {
        // The unstarted Task keeps its log: its next revision and plan replace the recorded
        // ones in one barrier, as a source refresh does.
        Some(_) => store
            .take_task_lease(&cas, &revision.task_id, &writer, recovery::lease_ms())
            .map_err(|e| e.to_string())?,
        None => store
            .open_task(&cas, &revision_id, &writer, recovery::lease_ms())
            .map_err(|e| e.to_string())?,
    };
    let outcome = (|| {
        if replace.is_some() {
            let decisions = developer::DecisionAuthority {
                compiler: &compiler,
                developer: developer.as_ref(),
            };
            store
                .refresh_task_source(&cas, &lease, &revision_id, Some(&plan_id), None, &decisions)
                .map_err(|e| e.to_string())?;
        } else {
            store
                .propose_task_plan(&cas, &lease, &plan_id, &trusted)
                .map_err(|e| e.to_string())?;
        }
        if options.plan_only {
            return Ok(());
        }
        admit_checks(&cas, &authority, &domain, &revision)?;
        store
            .admit_task_plan(&cas, &lease, &trusted)
            .map_err(|e| e.to_string())?;
        // The sweep that ends this run records what it removed on this Task (ADR-0144).
        crate::storage::note_task_run(&state, lease.task_id());
        execute(&cas, &mut store, &lease, &trusted, &host, &domain)?;
        Ok(())
    })();
    let released = release(&cas, &mut store, &lease, outcome);
    // Whatever the outcome, a model the bound one did not answer for is named on stderr
    // (ADR-0147).
    if presentation == Presentation::Print {
        crate::model_failures::print(&cas, &store, &revision.task_id, started);
    }
    released?;
    if presentation == Presentation::Silent {
        return Ok(Started::Code(0));
    }
    if replace.is_some() {
        eprintln!(
            "Task {} planned again as revision {revision_number}; confirm its new plan to run it",
            revision.task_id
        );
    }
    let presented = present_with_advisory(
        &cas,
        &store,
        &revision.task_id,
        options.json,
        options.plan_only,
        &undeclared,
    );
    match &presented {
        Ok(shown) if !options.plan_only => shown.tail(options.json),
        Ok(shown) => shown.note(shown.code),
        Err(_) => {}
    }
    // The request is in the Store, so a copy the repository would carry is redundant. Advice
    // only, and given last: the Task deadline was derived and the plan compiled, recorded and
    // presented before this runs, so a slow `git check-ignore` can neither shorten the Task's
    // remaining wall nor change the exit code or the `--json` document already written.
    task_file_home::warn_if_captured_from_the_repository(&options.file, &options.repo);
    presented.map(|shown| Started::Code(shown.code))
}

/// Selection for a silent preview: a refused selection is an error to show, not a document to
/// print.
fn silent_selection(
    cas: &Cas,
    authority: &RunAuthority,
    compiler: &TaskPlanCompiler,
    revision: TaskRevisionV1,
    clock: selection::PlanClock,
) -> Result<Option<selection::PreparedSelection>, String> {
    match selection::assess(cas, authority, compiler, revision, None, clock)? {
        selection::SelectionAssessment::Prepared(prepared) => Ok(Some(prepared)),
        selection::SelectionAssessment::Refused { decision, view, .. } => {
            Err(selection::refusal_line(&decision, &view)?)
        }
    }
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
        // A writer that lost its lease cannot release it either: it says so, and how to
        // continue (ADR-0200).
        Err(error) => Err(recovery::writer_lost(lease.task_id(), &error).unwrap_or(error)),
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
            "report" => TaskKindProfile::Report,
            "optimize" => TaskKindProfile::OptimizationAnalysis,
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
        let asked = verification.map_or("-", FileVerification::as_str);
        let verifies = match profile {
            TaskKindProfile::Implementation => "evaluation",
            TaskKindProfile::ReviewedImplementation => "review",
            TaskKindProfile::RepairAllowedImplementation => "review_or_targeted_fixes",
            _ => "its own acceptance, with no `verification` field",
        };
        let profile_name = serde_json::to_value(profile)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        return Err(format!(
            "Task verification request contradicts its captured kind profile: the Task file asks \
             for verification `{asked}`, but kind `{kind}` is the `{profile_name}` profile, which \
             verifies by {verifies}. Remove `verification` from the Task file to take the \
             kind's, or set it to the profile's. Kind defaults: {}",
            kind_defaults_table()
        ));
    }
    Ok(profile)
}

#[allow(clippy::too_many_arguments)]
fn install_candidate_experiment(
    cas: &Cas,
    compiler: TaskPlanCompiler,
    authority: &RunAuthority,
    revision: &TaskRevisionV1,
    revision_id: &str,
    graph: &CompiledTask,
    plan: &ExecutionPlanV1,
) -> Result<(TaskPlanCompiler, ExecutionPlanV1, CompiledTask), String> {
    use review_core::task::optimization_experiment::{
        EXPERIMENTAL_SLOT_V2, ExperimentAllowanceV1, ExperimentalSlotV2,
    };
    use review_graph::task::ExperimentalSlotTemplateV1;

    let coordinators = graph
        .nodes
        .iter()
        .filter_map(|(name, node)| match &node.operator {
            review_graph::task::CompiledOperator::Primitive {
                operator:
                    pipeline::TaskOperatorV1::OptimizationExperiment {
                        baseline_slot,
                        candidate_slot,
                    },
                ..
            } => Some((name.clone(), baseline_slot.clone(), candidate_slot.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(parent, baseline_local, candidate_local)] = coordinators.as_slice() else {
        return Err(
            "Candidate Optimization Pipeline needs exactly one installed experiment coordinator"
                .into(),
        );
    };
    let scope = parent
        .split_once(".nodes.")
        .map(|(scope, _)| scope)
        .ok_or("Optimization coordinator has no compiled scope")?;
    let qualify = |slot: &str| {
        if graph.slots.contains_key(slot) {
            slot.to_owned()
        } else {
            format!("{scope}.slots.{slot}")
        }
    };
    let baseline_slot = qualify(baseline_local);
    let candidate_slot = qualify(candidate_local);
    let slots = [&baseline_slot, &candidate_slot];
    let mut packages = BTreeSet::new();
    let mut package_ids = BTreeSet::new();
    let mut effects = BTreeSet::new();
    let mut efforts = BTreeSet::new();
    for slot in slots {
        let package = &graph
            .slots
            .get(slot)
            .ok_or("Optimization experiment lost a declared Worker slot")?
            .worker;
        let worker = compiler
            .worker(package)
            .ok_or("Optimization experiment Worker package is absent")?;
        let binding = plan
            .bindings
            .get(slot)
            .ok_or("Optimization experiment Worker has no exact binding")?;
        efforts.insert(match &binding.execution {
            WorkerExecutionV1::Command {} => "command".into(),
            WorkerExecutionV1::Model { effort, .. } => effort.clone(),
        });
        packages.insert(package.clone());
        package_ids.insert(binding.package_artifact_id.clone());
        effects.extend(worker.signature.effects.iter().cloned());
        worker
            .signature
            .attempt
            .as_ref()
            .filter(|attempt| attempt.wall_ms > 0)
            .ok_or("Optimization experiment Worker has no Attempt bound")?;
    }
    if packages.len() != 2 || package_ids.len() != 2 {
        return Err("Baseline and candidate must be separate captured Worker packages".into());
    }
    let policy_id = authority.code_policy_id()?.to_owned();
    let oracle_id = revision
        .inputs
        .get("requirements")
        .and_then(|port| port.artifact_ids.first())
        .cloned()
        .ok_or("Candidate Optimization lacks captured Requirements")?;
    let binding_id =
        review_store::content_id(&json!([revision_id, policy_id, "optimization-experiment"]))
            .map_err(|error| error.to_string())?;
    let slot = ExperimentalSlotV2 {
        schema: "af.experimental-slot/2".into(),
        slot: "optimization-experiment".into(),
        outer_plan_binding_id: binding_id,
        policy_id: policy_id.clone(),
        protected_oracle_id: oracle_id.clone(),
        allowed_task_kinds: BTreeSet::from(["optimize".into()]),
        allowed_packages: packages,
        allowed_worker_package_ids: package_ids,
        allowed_efforts: efforts,
        allowed_effects: effects,
        max_children: revision.limits.max_attempts.min(4096),
        max_depth: 8,
        max_concurrency: 1,
        max_development_candidates: 1,
        allowance: ExperimentAllowanceV1 {
            tokens: revision.limits.tokens,
            attempts: revision.limits.max_attempts,
            wall_ms: revision.limits.deadline_unix_ms,
        },
    };
    slot.validate()?;
    let slot_id = cas
        .put_artifact(
            EXPERIMENTAL_SLOT_V2,
            producer(),
            vec![revision_id.into(), policy_id, oracle_id],
            None,
            serde_json::to_value(slot).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
        .0;
    let compiler = compiler.with_experimental_slot(
        parent.clone(),
        ExperimentalSlotTemplateV1 {
            slot_id,
            max_concurrency: 1,
        },
    )?;
    let root = &graph
        .calls
        .get("root")
        .ok_or("Optimization plan has no root Pipeline")?
        .pipeline;
    let (plan, graph) = compiler.compile(cas, revision_id, root)?;
    Ok((compiler, plan, graph))
}

pub(super) fn restore_experimental_slots(
    mut compiler: TaskPlanCompiler,
    graph: &CompiledTask,
) -> Result<TaskPlanCompiler, String> {
    for (parent, slot) in &graph.experimental_slots {
        compiler = compiler.with_experimental_slot(parent.clone(), slot.clone())?;
    }
    Ok(compiler)
}

#[allow(clippy::too_many_arguments)]
fn captured_domain(
    cas: &Cas,
    authority: &RunAuthority,
    profile: TaskKindProfile,
    graph: CompiledTask,
    plan: &ExecutionPlanV1,
    compiler: &TaskPlanCompiler,
    state: &Path,
    repo: &Path,
) -> Result<Box<dyn TaskDomain>, String> {
    match profile {
        TaskKindProfile::Implementation if authority.review_policy_id.is_none() => Ok(Box::new(
            domain::code_domain(cas, authority.code_policy_id()?, graph, state, repo)?
                .with_cache_source_resolver(crate::caches::resolve_kind),
        )),
        TaskKindProfile::Review
        | TaskKindProfile::Implementation
        | TaskKindProfile::ReviewedImplementation
        | TaskKindProfile::RepairAllowedImplementation => {
            let remote = domain::remote_checks(state, &graph)?;
            Ok(Box::new(
                ReviewTaskDomain::captured(
                    cas,
                    authority
                        .review_policy_id
                        .as_deref()
                        .ok_or("Review Task lost its captured policy")?,
                    graph,
                )?
                .with_rust_toolchain_mapping(domain::rust_toolchain_mapping(repo)?)
                .with_task_build_cache_root(domain::build_cache_root()?)
                .with_remote_checks(remote)
                .with_review_task(profile == TaskKindProfile::Review)
                .with_cache_source_resolver(crate::caches::resolve_kind),
            ))
        }
        TaskKindProfile::Document => Ok(Box::new(DocumentTaskDomain::captured(
            cas,
            authority
                .document_policy_id
                .as_deref()
                .ok_or("Task lost its document policy")?,
            graph,
        )?)),
        TaskKindProfile::Report => Ok(Box::new(ReportTaskDomain::captured(
            cas,
            authority
                .report_policy_id
                .as_deref()
                .ok_or("Task lost its report policy")?,
            graph,
        )?)),
        TaskKindProfile::OptimizationAnalysis => Ok(Box::new(OptimizationTaskDomain::captured(
            authority.code_policy_id()?,
            graph,
        )?)),
        TaskKindProfile::OptimizationCandidate => Ok(Box::new(
            OptimizationCandidateTaskDomain::captured(graph, plan.clone(), compiler)?,
        )),
    }
}

fn execute(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    authority: &CapturedTaskAuthority<'_>,
    host: &CapturedTaskHost<'_>,
    domain: &dyn TaskDomain,
) -> Result<(), String> {
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let runtime = TaskRuntime::new(store, cas, lease.clone(), authority, host)?
        .with_cancellation(&cancellation)
        .with_lease_timing(recovery::timing());
    crate::interrupt::note_task(lease.task_id());
    let report = crate::interrupt::forwarding(&cancellation, || runtime.execute());
    // Interrupted work is neither assembled nor finished: the lease is released and the Task
    // stays resumable with `af task run` (ADR-0129).
    crate::interrupt::check()?;
    // So is work the free-disk floor stopped before a Worker started (ADR-0144).
    if let Some(refusal) = runtime.disk_refusal() {
        return Err(refusal);
    }
    let report = report?;
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
    runtime.finish(&result_id)?;
    drop(runtime);
    // The result is recorded; what the Task's remote checks left on GitHub goes now, under the
    // same lease, and only ever as a warning when it cannot (ADR-0144).
    crate::storage::gate::after_finish(cas, store, lease);
    Ok(())
}

/// Refuse, before the plan is admitted and before any Attempt, a Task whose captured checks
/// cannot run on this machine (ADR-0196): a check whose Rust toolchain does not resolve here.
/// The message starts with the code policy file the refused field is in.
fn admit_checks(
    cas: &Cas,
    authority: &RunAuthority,
    domain: &dyn TaskDomain,
    revision: &review_core::task::TaskRevisionV1,
) -> Result<(), String> {
    domain.admit_checks(cas, revision).map_err(|reason| {
        let path = cas
            .get(&authority.catalog_id)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
            .and_then(|catalog| catalog.get("code_policy")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| ".af/code-policy.toml".into());
        format!(
            "{}: {reason}; nothing ran and nothing was charged",
            preview::text(&path)
        )
    })
}

fn confirm_current_plan(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    expected: Option<&str>,
) -> Result<(), String> {
    let current = store
        .task_projection(cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    if current.plan_id.as_deref() != expected {
        return Err(
            "Captured plan changed before execution; inspect and confirm the new plan".into(),
        );
    }
    Ok(())
}

/// `af task run NEW --from TASK`: what the command line names.
pub(super) struct ContinueOptions {
    pub task_id: String,
    pub from: String,
    pub round: Option<u32>,
    pub goal: Option<PathBuf>,
    pub bindings: Option<PathBuf>,
    pub repo: PathBuf,
    pub state: Option<PathBuf>,
    pub json: bool,
    pub execute: bool,
}

/// A `--goal` file that cannot be read, refused with its fix and, under `--json`, the command
/// that plans the continuation once the file is there.
fn unreadable_goal(options: &ContinueOptions, path: &Path, error: String) -> String {
    use crate::error_document::{Cause, Detail};
    let fix = if error.starts_with("cannot open") || error.ends_with("must be a regular file") {
        "; write the goal there, or pass --goal FILE naming an existing Markdown file"
    } else {
        ""
    };
    let quote = |word: &str| shell_words::quote(word).into_owned();
    let mut again = format!(
        "af task run {} --from {}",
        quote(&options.task_id),
        quote(&options.from)
    );
    if let Some(round) = options.round {
        again.push_str(&format!(" --round {round}"));
    }
    again.push_str(&format!(" --goal {}", quote(&path.display().to_string())));
    if let Some(bindings) = &options.bindings {
        again.push_str(&format!(
            " --bindings {}",
            quote(&bindings.display().to_string())
        ));
    }
    if options.execute {
        again.push_str(" --execute");
    }
    again.push_str(&store_selectors());
    crate::error_document::typed(
        format!("{error}{fix}"),
        Detail::new(Cause::InputUnreadable)
            .next(again)
            .with("file", path.display().to_string())
            .with("what", "goal file"),
    )
}

/// `af task run NEW --from TASK [--round N] [--goal FILE]` (ADR-0203): plan, and with
/// `--execute` run, a new Task from the finished Task's own captured Task file with a
/// `continue` table, so its source is the reviewed candidate and its implementer reads the
/// round's findings. Without `--execute` it plans, exactly as `af task plan` does, and the plan
/// waits for `--confirm-plan`.
pub(super) fn run_from(options: ContinueOptions) -> Result<i32, String> {
    let (_, state) = state_path(&options.repo, options.state.as_deref())?;
    require_store(&state)?;
    let goal = options
        .goal
        .as_deref()
        .map(|path| -> Result<String, String> {
            let bytes = input_file::read(path, 256 * 1024, "goal file")
                .map_err(|error| unreadable_goal(&options, path, error))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("goal file {} is not UTF-8 text", path.display()))?;
            if text.trim().is_empty() {
                return Err(format!(
                    "goal file {} is empty; say what the new Task is for",
                    path.display()
                ));
            }
            Ok(text)
        })
        .transpose()?;
    let bytes = {
        let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
        let store =
            EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        if store
            .task_projection(&cas, &options.from)
            .map_err(|e| crate::store_release::explain(&state, e.to_string()))?
            .is_none()
        {
            return Err(format!("--from: {}", task_not_found(&state, &options.from)));
        }
        continuation::derived_file(
            &cas,
            &store,
            &options.task_id,
            &options.from,
            options.round,
            goal,
        )?
    };
    if !options.execute {
        crate::model_check::enable_plan_time();
    }
    let start = StartOptions {
        // Named for the messages that quote it; the bytes are the derived file, never read
        // from this path.
        file: PathBuf::from(format!(
            "{} (af task run --from {}).json",
            options.task_id, options.from
        )),
        bindings: options.bindings,
        source_bindings: None,
        repo: options.repo,
        state: options.state,
        authority: "HEAD".into(),
        uncommitted: false,
        json: options.json,
        plan_only: !options.execute,
        timeout_secs: None,
        optimization_history: None,
        fit: false,
        replace: false,
    };
    start_kind_bytes(start, bytes, None, Presentation::Print)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    id: &str,
    repo: &Path,
    state: Option<&Path>,
    json: bool,
    confirm_plan: Option<&str>,
    execute_now: bool,
    retry_gate: bool,
    quiet: bool,
) -> Result<i32, String> {
    let started = clock()?;
    let (_, state) = state_path(repo, state)?;
    require_store(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let projection = store
        .task_projection(&cas, id)
        .map_err(|e| crate::store_release::explain(&state, e.to_string()))?
        .ok_or_else(|| task_not_found(&state, id))?;
    // `--retry-gate` reopens only a finished Task whose last failure was a gate; anything else
    // is refused before a lease is taken or anything is recorded.
    if retry_gate
        && let review_store::store::task::gate_retry::GateRetry::Refused(why) =
            projection.failed_gates(&cas).map_err(|e| e.to_string())?
    {
        return Err(format!("Task {id} cannot retry its gate: {why}."));
    }
    if let Some(expected) = confirm_plan {
        if !review_core::is_digest(expected) || projection.plan_id.as_deref() != Some(expected) {
            return Err(format!(
                "Plan confirmation differs from the current captured plan; inspect it again: \
                 Task {id}'s plan is {}; read it with `af task explain {id} --tree` and confirm \
                 that one",
                projection.plan_id.as_deref().unwrap_or("not recorded")
            ));
        }
    }
    if !execute_now
        && confirm_plan.is_none()
        && !projection.admitted
        && !matches!(projection.phase, TaskPhaseV1::Finished { .. })
        && projection.plan_id.is_some()
    {
        if !json {
            present(&cas, &store, id, false, true)?;
        }
        return Err("Confirm the captured plan with --confirm-plan PLAN_ID (or explicitly opt into --execute automation)".into());
    }
    if matches!(projection.phase, TaskPhaseV1::Finished { .. }) && !retry_gate {
        // A finished Task returns its recorded result, and its run log ends as the first did.
        let presented = present(&cas, &store, id, json, false)?;
        if json {
            presented.tail(json);
        } else {
            presented.note(presented.code);
        }
        return Ok(presented.code);
    }
    if projection.plan_id.is_none() {
        present(&cas, &store, id, json, true)?.note(4);
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
    compiler = restore_experimental_slots(compiler, &graph)?;
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
    // The wall clock starts at the first admission (P5): a plan that has not run yet gets its
    // whole wall from now, and one that ran keeps its deadline.
    let deadline = projection.deadline_at(clock()?);
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
    let inner = captured_domain(
        &cas,
        &authority,
        profile,
        graph.clone(),
        &plan,
        &compiler,
        &state,
        repo,
    )?;
    let models = model_bindings(&plan, &graph, &adapters)?;
    let domain = ProviderTaskDomain {
        graph: &graph,
        models: &models,
        inner: inner.as_ref(),
    };
    let environment = domain::environment(&cas, &authority, profile)?;
    let host = CapturedTaskHost::capture_with_models(
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
    // The process that takes the lease, recorded before it does, so `af task cancel` finds it
    // by the lease's writer and `af task wait` knows a run is on its way (ADR-0202).
    let writer = format!("cli-{}", writer_pid());
    let run_record = runs::begin(&state, id, &writer)?;
    // A run that died holds its lease until it expires: wait it out rather than refuse
    // (ADR-0200).
    let lease = recovery::take_lease(&cas, &mut store, id, &writer)?;
    let outcome = (|| {
        confirm_current_plan(&cas, &store, id, projection.plan_id.as_deref())?;
        let before = store
            .task_projection(&cas, id)
            .map_err(|e| e.to_string())?
            .ok_or("Unknown Task")?;
        store
            .recover_task_attempts(&cas, &lease)
            .map_err(|e| e.to_string())?;
        // The first line says which interrupted node runs again, or why the Task cannot
        // continue.
        let after = store
            .task_projection(&cas, id)
            .map_err(|e| e.to_string())?
            .ok_or("Unknown Task")?;
        recovery::announce(id, &before, &after)?;
        // After the resume's line: the first line says what the run does with an interrupted
        // node (ADR-0200).
        eprintln!(
            "af task run: Task {id} {}",
            state::deadline_line(deadline, clock()?)
        );
        if retry_gate {
            let gates = store
                .retry_task_gate(&cas, &lease, &trusted)
                .map_err(|e| format!("Task {id} cannot retry its gate: {e}"))?;
            eprintln!(
                "retrying gate {} of Task {id} on the same candidate",
                gates
                    .iter()
                    .map(|gate| preview::text(gate))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if projection
            .waiting_for_domain_publication(&cas)
            .map_err(|e| e.to_string())?
        {
            store
                .resume_task(&cas, &lease, &trusted)
                .map_err(|e| e.to_string())?;
        }
        if !projection.admitted {
            admit_checks(&cas, &authority, &domain, &projection.revision)?;
            store
                .admit_task_plan(&cas, &lease, &trusted)
                .map_err(|e| e.to_string())?;
        }
        // The sweep that ends this run records what it removed on this Task (ADR-0144).
        crate::storage::note_task_run(&state, lease.task_id());
        // One line per node on stderr while the run waits, and a heartbeat (ADR-0202).
        let _progress = (!quiet).then(|| runs::Progress::start(&state, id, started));
        execute(&cas, &mut store, &lease, &trusted, &host, &domain)?;
        Ok(())
    })();
    drop(run_record);
    let released = release(&cas, &mut store, &lease, outcome);
    // Whatever the outcome, a model the bound one did not answer for is named on stderr
    // (ADR-0147).
    crate::model_failures::print(&cas, &store, id, started);
    released?;
    let presented = present(&cas, &store, id, json, false)?;
    presented.tail(json);
    Ok(presented.code)
}

/// The run log's last lines on stderr, held until the command's housekeeping has said what it
/// did: `main` prints them ([`print_last_lines`]) after the stale sandbox sweep and the Storage
/// Budget sweep that end a run, so the Task's state stays the last line however late those
/// report.
static LAST_LINES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn defer_last_line(line: String) {
    LAST_LINES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(line);
}

/// Print the run log's last lines this command held back, once, on stderr.
pub(crate) fn print_last_lines() {
    let lines = std::mem::take(
        &mut *LAST_LINES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    for line in lines {
        eprintln!("{line}");
    }
}

/// What one presentation of a Task printed: the exit code it ends with, and from the same view
/// that code was judged by, the run log's last line (the Task's state and the next command)
/// and a failed gate's failing tests. Nothing is read again after the Task was shown, so a
/// Task that changed or was collected meanwhile can neither silence nor contradict stderr.
pub(super) struct Presented {
    pub(super) code: i32,
    line: String,
    gate_tail: Vec<String>,
}

impl Presented {
    fn new(id: &str, code: i32, view: &state::TaskView) -> Self {
        Self {
            code,
            line: state_line(id, view),
            gate_tail: state::failed_gate_tail(id, view),
        }
    }

    /// A command whose exit status `code` reports a Task that did not verify, or one waiting
    /// for a person, while its result went to stdout, ends with the run log's last line on
    /// stderr: a non-zero exit never leaves stderr silent.
    pub(super) fn note(&self, code: i32) {
        if code != 0 {
            defer_last_line(self.line.clone());
        }
    }

    /// The run log's last lines: under `--json`, on stderr beside the document on stdout, the
    /// failing tests of a failed gate (the text on stdout already named them); in both
    /// formats the state line, so a run's log always ends with the state and the next command
    /// (ADR-0202).
    pub(super) fn tail(&self, json: bool) {
        if json {
            for line in &self.gate_tail {
                defer_last_line(line.clone());
            }
        }
        defer_last_line(self.line.clone());
    }
}

/// The run log's last line: the Task's state and the next command.
pub(super) fn state_line(id: &str, view: &state::TaskView) -> String {
    format!(
        "Task {id}: {}; next: {}",
        view.state.label(),
        view.state.next_action.as_deref().unwrap_or("none")
    )
}

/// [`present`], then [`Presented::note`] for a non-zero exit.
fn present_noting(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    json_output: bool,
    explain: bool,
) -> Result<i32, String> {
    let presented = present(cas, store, id, json_output, explain)?;
    presented.note(presented.code);
    Ok(presented.code)
}

pub(super) fn explain(
    id: &str,
    repo: &Path,
    state: Option<&Path>,
    json: bool,
    plan: Option<&str>,
    tree: bool,
) -> Result<i32, String> {
    let (_, state) = state_path(repo, state)?;
    require_store(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    if store
        .task_projection(&cas, id)
        .map_err(|e| crate::store_release::explain(&state, e.to_string()))?
        .is_none()
    {
        return Err(task_not_found(&state, id));
    }
    if let Some(plan_id) = plan {
        return inspection::explain_plan(&cas, &store, id, plan_id, json, tree);
    }
    let presented = present_with_format(&cas, &store, id, json, true, tree, None)?;
    presented.note(presented.code);
    Ok(presented.code)
}

pub(super) fn show_if_common(
    id: &str,
    state: &Path,
    json: bool,
    checks: bool,
) -> Result<bool, String> {
    if !state.join("events.sqlite").is_file() {
        return Ok(false);
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    // A collected Task shows its retained summary; its projection stops at the tombstone.
    if let Some(collected) = store.collected_task(id).map_err(|e| e.to_string())? {
        if checks {
            return Err(format!(
                "Task `{id}` was collected: its check receipts are no longer recorded; \
                 `af task show {id}` prints its summary"
            ));
        }
        collection::present(&collected, json)?;
        return Ok(true);
    }
    let Some(task) = store.task_projection(&cas, id).map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    if checks {
        let Inspection { view, .. } = inspection(&cas, &store, id, false, None)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&state::checks_document(id, &view))
                    .map_err(|e| e.to_string())?
            );
        } else {
            for line in state::checks_lines(id, &view) {
                println!("{line}");
            }
        }
        return Ok(true);
    }
    // Who cancelled the run that stopped it, above the `next:` line (ADR-0202).
    if let Some(line) = runs::cancel_line(state, &task) {
        runs::note_for_show(line);
    }
    present(&cas, &store, id, json, false)?;
    Ok(true)
}

/// One Task of the Store `--repo` and `--state` name, opened read-only for a command of the
/// agent loop that reads it (ADR-0202): refused by name when the Store or the Task is not there,
/// and when the Task was collected, whose records are gone.
pub(super) struct OpenedTask {
    pub(super) state: PathBuf,
    pub(super) cas: Cas,
    pub(super) store: EventStore,
    pub(super) task: TaskProjection,
}

pub(super) fn open_task(id: &str, repo: &Path, state: Option<&Path>) -> Result<OpenedTask, String> {
    if !is_name(id) {
        return Err(format!(
            "`{}`: a Task ID is 1 to 128 ASCII letters, digits, `-` or `_`, starting with a \
             letter or digit; `af task list` lists the Tasks",
            preview::text(id)
        ));
    }
    let (_, state) = state_path(repo, state)?;
    require_store(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    if store
        .collected_task(id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!(
            "Task `{id}` was collected: its Snapshots and records are no longer kept, only its \
             summary; `af task show {id}` prints it"
        ));
    }
    let task = store
        .task_projection(&cas, id)
        .map_err(|e| crate::store_release::explain(&state, e.to_string()))?
        .ok_or_else(|| task_not_found(&state, id))?;
    Ok(OpenedTask {
        state,
        cas,
        store,
        task,
    })
}

impl OpenedTask {
    /// The Task's recorded result and the state every rendering prints, read again now.
    pub(super) fn view(&self) -> Result<(Option<TaskResultV1>, state::TaskView), String> {
        let result: Option<TaskResultV1> = match &self.task.phase {
            TaskPhaseV1::Finished { result_id } => {
                Some(artifact(&self.cas, result_id, TASK_RESULT_V1)?)
            }
            _ => None,
        };
        let cancelled = runs::cancelled_epoch(Some(&self.state), &self.task);
        let view = selected_view(&self.cas, &self.task, result.as_ref(), clock()?, cancelled)?;
        Ok((result, view))
    }

    /// Project the Task again from the Store, for a command that watches it change.
    pub(super) fn refresh(&mut self) -> Result<(), String> {
        self.task = self
            .store
            .task_projection(&self.cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| task_not_found(&self.state, &self.task.task_id))?;
        Ok(())
    }
}

/// The leaf subcommand's matches of this process's command line when it works on a Task Store:
/// an `af task` command, or `af review` with a Task file (`--file`), whose `--state` names a
/// Task Store too. `None` for every other command, a Campaign review and the browser included.
fn task_invocation() -> Option<&'static clap::ArgMatches> {
    static MATCHES: std::sync::OnceLock<Option<clap::ArgMatches>> = std::sync::OnceLock::new();
    MATCHES
        .get_or_init(|| {
            use clap::CommandFactory as _;
            let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
            let matches = crate::cli::Af::command().try_get_matches_from(argv).ok()?;
            let (namespace, mut leaf) = matches.subcommand()?;
            while let Some((_, deeper)) = leaf.subcommand() {
                leaf = deeper;
            }
            let task_file = matches!(leaf.try_get_raw("task_file"), Ok(Some(_)));
            (namespace == "task" || (namespace == "review" && task_file)).then(|| leaf.clone())
        })
        .as_ref()
}

/// The flags of `ids` this command was given on its command line ([`task_invocation`]), as typed
/// and shell quoted: ` --repo ../x --state ../state`. A default value is not repeated.
pub(crate) fn invocation_flags(ids: &[&str]) -> String {
    let Some(matches) = task_invocation() else {
        return String::new();
    };
    let mut flags = String::new();
    for id in ids {
        let flag = id.replace('_', "-");
        if let Ok(Some(true)) = matches.try_get_one::<bool>(id) {
            flags.push_str(&format!(" --{flag}"));
            continue;
        }
        let Ok(Some(values)) = matches.try_get_raw(id) else {
            continue;
        };
        if matches.value_source(id) != Some(clap::parser::ValueSource::CommandLine) {
            continue;
        }
        for value in values {
            flags.push_str(&format!(
                " --{flag} {}",
                shell_words::quote(&value.to_string_lossy())
            ));
        }
    }
    flags
}

/// `af task plan --file FILE` with the capture flags (`--bindings`, `--source-bindings`,
/// `--authority`, `--uncommitted`) and the Store selectors this command was given: run as
/// printed, it plans the same capture in the same Store.
pub(super) fn plan_again(file: &Path) -> String {
    format!(
        "af task plan --file {}{}{}",
        shell_words::quote(&file.display().to_string()),
        invocation_flags(&["bindings", "source_bindings", "authority", "uncommitted"]),
        store_selectors()
    )
}

/// The Store selectors this command was given (`--repo`, then `--state` or `--workspace`), as
/// typed; empty for the default Store.
pub(crate) fn store_selectors() -> String {
    invocation_flags(&["repo", "state", "workspace"])
}

/// `command` as a printed next command: an `af task` command carries the Store selectors this
/// command was given, so run as printed it acts on the same Store. Every other command, and
/// every command under the default Store, is unchanged.
pub(crate) fn with_selectors(command: &str) -> String {
    if command.starts_with("af task ") {
        format!("{command}{}", store_selectors())
    } else {
        command.to_owned()
    }
}

/// [`state::view`], with the next command carrying this command's Store selectors.
fn selected_view(
    cas: &Cas,
    task: &TaskProjection,
    result: Option<&TaskResultV1>,
    now: u64,
    cancelled: Option<u64>,
) -> Result<state::TaskView, String> {
    let mut view = state::view(cas, task, result, now, cancelled)?;
    if let Some(next) = view.state.next_action.as_mut() {
        *next = with_selectors(next);
    }
    Ok(view)
}

/// The not-found refusal of a Task, in the shape every not-found refusal takes:
/// `<Thing> <name> not found in <where>; <what exists or the fix>`. `<where>` is the Store as
/// the command was told to find it (the path given, the Workspace named, or this checkout's
/// own) and the Tasks it does hold; the fix lists them, with the selectors this command was
/// given.
pub(crate) fn task_not_found(state: &Path, id: &str) -> String {
    let words: Vec<String> = std::env::args().collect();
    let given = |flag: &str| {
        words
            .iter()
            .position(|word| word == flag)
            .and_then(|at| words.get(at + 1).cloned())
            .or_else(|| {
                words
                    .iter()
                    .find_map(|word| word.strip_prefix(&format!("{flag}=")).map(str::to_owned))
            })
    };
    let (place, selector) = if given("--state").is_some() {
        (
            format!("the Task Store {}", state.display()),
            format!(
                " --state {}",
                shell_words::quote(&state.display().to_string())
            ),
        )
    } else if let Some(label) = given("--workspace") {
        (
            format!("the Task Store of Workspace {}", preview::text(&label)),
            format!(" --workspace {}", shell_words::quote(&label)),
        )
    } else {
        ("this checkout's Task Store".into(), String::new())
    };
    // What the Store does hold: its Tasks, the collected ones included, up to ten of them;
    // nothing when there is no Store there.
    let listed = (|| -> Result<Option<Vec<String>>, String> {
        if !store_present(state)? {
            return Ok(None);
        }
        let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
        let store =
            EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        let mut ids = store.task_ids(&cas).map_err(|e| e.to_string())?;
        ids.extend(
            store
                .collected_tasks()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|task| task.collected.task_id),
        );
        ids.sort();
        ids.dedup();
        Ok(Some(ids))
    })();
    let holds = match listed {
        Ok(None) => String::new(),
        Ok(Some(ids)) => {
            let shown = ids.iter().take(10).cloned().collect::<Vec<_>>().join(", ");
            let more = if ids.len() > 10 { ", …" } else { "" };
            match ids.len() {
                0 => " (no Tasks there)".to_owned(),
                1 => format!(" (1 Task there: {shown})"),
                n => format!(" ({n} Tasks there: {shown}{more})"),
            }
        }
        Err(error) => format!(" (its Tasks cannot be listed: {error})"),
    };
    format!(
        "Task {} not found in {place}{holds}; af task list{selector} lists its Tasks",
        preview::text(id)
    )
}

/// The not-found refusal of a Task Store, in the shape every not-found refusal takes.
pub(crate) fn store_not_found(state: &Path) -> String {
    format!(
        "Task Store not found in {}; af task plan --file FILE creates it, and af task list \
         lists the Tasks of the Store this checkout uses",
        state.display()
    )
}

/// Refuse a command that reads a Task Store before anything opens one that is not there.
pub(crate) fn require_store(state: &Path) -> Result<(), String> {
    if store_present(state)? {
        Ok(())
    } else {
        Err(store_not_found(state))
    }
}

/// Whether a Task state directory holds a Store. Only a missing `events.sqlite` means none;
/// a permission or any other error reading it is the caller's error, never an empty Store.
pub(crate) fn store_present(state: &Path) -> Result<bool, String> {
    let path = state.join("events.sqlite");
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!("{} is not a file", path.display())),
        // A link whose target is gone is a Store that cannot be read, not a missing one.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(&path) {
                Ok(_) => Err(format!("{} is a link to nothing", path.display())),
                Err(_) => Ok(false),
            }
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

pub(super) fn list_common(state: &Path) -> Result<Vec<serde_json::Value>, String> {
    if !store_present(state)? {
        return Ok(vec![]);
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    // Uncollected Tasks are projected first and the tombstones read after: a Task that
    // `gc --apply` collects between the two is skipped by the projection and listed from its
    // tombstone, so no Task the log retains is ever omitted (ADR-0135).
    let now = clock()?;
    let mut entries = store.map_tasks(&cas, |task| {
        let result: Option<TaskResultV1> = match &task.phase {
            TaskPhaseV1::Finished { result_id } => Some(artifact(&cas, result_id, TASK_RESULT_V1)?),
            _ => None,
        };
        // The same state `af task show` prints, from the same derivation.
        let cancelled = runs::cancelled_epoch(Some(state), &task);
        let view = selected_view(&cas, &task, result.as_ref(), now, cancelled)?;
        Ok(json!({"schema":TASK_LIST_ENTRY_V3,"task_id":task.task_id,"kind":task.revision.kind,"phase":task.phase,
            "state":view.state,
            "round":review_round(&task),
            "chargeable_tokens":task.execution.as_ref().map_or(0,|e| e.budget.committed_tokens()).to_string(),
            "derived_snapshot_id":result.as_ref().and_then(|r| r.outputs.get("snapshot")).and_then(|o| o.snapshot_id.as_ref()),
            "delivery":delivery_view(&cas, &task)?,
            "updated_unix_ms":task.last_event_unix_ms()}))
        .map(|mut entry| {
            if let Some(gate) = &view.gate {
                entry["gate"] = json!(gate);
            }
            let unknown = unknown_usage(&task).values().sum::<u64>();
            if unknown > 0 {
                entry["unknown_usage_attempts"] = json!(unknown);
            }
            entry
        })
    }).map_err(|e| e.to_string())?.into_iter().collect::<Result<Vec<_>, String>>()?;
    let collected = store
        .collected_tasks()
        .map_err(|e| e.to_string())?
        .iter()
        .map(collection::list_entry)
        .collect::<Vec<_>>();
    // A Task collected after its projection above appears in both reads: the tombstone wins,
    // so no Task is listed twice or in two states.
    let tombstoned: BTreeSet<&str> = collected
        .iter()
        .filter_map(|entry| entry["task_id"].as_str())
        .collect();
    entries.retain(|entry| {
        entry["task_id"]
            .as_str()
            .is_none_or(|id| !tombstoned.contains(id))
    });
    // Collected Tasks keep their place in label order, with their retained summary.
    entries.extend(collected);
    entries.sort_by(|left, right| left["task_id"].as_str().cmp(&right["task_id"].as_str()));
    Ok(entries)
}

/// One entry of `af task list --json`.
pub(crate) const TASK_LIST_ENTRY_V3: &str = "af/task-list-entry@3";

/// The review round a Task is in: the rounds it handed off to a successor plan, plus the one its
/// current execution recorded; 0 before a review recorded a round.
fn review_round(task: &TaskProjection) -> u64 {
    let current = task.execution.as_ref().is_some_and(|execution| {
        execution
            .outputs
            .values()
            .flat_map(|(_, output)| output.outputs.values())
            .any(|port| port.artifact_type == TASK_REVIEW_ROUND_V1)
    });
    task.review_handoffs.len() as u64 + u64::from(current)
}

/// `af task list --sizes`: the Store's totals and each uncollected Task's CAS footprint. The
/// same reachability walk `af task gc` sweeps by (ADR-0135). Nothing is written.
pub(super) fn list_sizes(
    state: &Path,
) -> Result<(serde_json::Value, BTreeMap<String, serde_json::Value>), String> {
    if !store_present(state)? {
        return Ok((
            json!({"objects":0,"bytes":0,"unreachable_objects":0,"unreachable_bytes":0}),
            BTreeMap::new(),
        ));
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let inventory = store.store_inventory(&cas).map_err(|e| e.to_string())?;
    let totals = &inventory.totals;
    Ok((
        json!({
            "objects": totals.objects,
            "bytes": totals.bytes,
            "unreachable_objects": totals.unreachable_objects,
            "unreachable_bytes": totals.unreachable_bytes,
        }),
        inventory
            .tasks
            .iter()
            .map(|(task_id, footprint)| (task_id.clone(), collection::footprint(footprint)))
            .collect(),
    ))
}

fn present(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    json_output: bool,
    explain: bool,
) -> Result<Presented, String> {
    present_with_format(cas, store, id, json_output, explain, false, None)
}

/// Present a Task that was just planned or started, carrying the undeclared `.af/` paths of the
/// Snapshot it captured. Inspection commands derive the same group from the recorded source
/// Snapshot, so every document of one Task carries the same field.
fn present_with_advisory(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    json: bool,
    explain: bool,
    undeclared: &review_config::layout::PathGroup,
) -> Result<Presented, String> {
    let group = Some(undeclared);
    present_with_format(cas, store, id, json, explain, false, group)
}

/// The undeclared `.af/` paths of a recorded Task's source Snapshot, read back from the Store:
/// the `source` input's Snapshot receipt names its Manifest, and the Manifest is classified
/// exactly as it was at capture. `None` for a Task without a source Snapshot (a Document
/// Task) or one whose source predates Snapshot receipts.
fn recorded_undeclared_af_paths(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<review_config::layout::PathGroup>, String> {
    let Some(source) = revision.inputs.get("source") else {
        // A source-less profile kept the observation as a typed artifact in its provenance.
        for id in &revision.provenance.input_artifact_ids {
            let envelope = cas.get_json(id).map_err(|e| e.to_string())?;
            if envelope.get("type").and_then(serde_json::Value::as_str)
                == Some(UNDECLARED_AF_PATHS_V1)
                && let Some(payload) = envelope.get("payload")
            {
                let group: review_config::layout::PathGroup =
                    serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?;
                return Ok(Some(group));
            }
        }
        return Ok(None);
    };
    let port = serde_json::to_value(source).map_err(|e| e.to_string())?;
    let Some(snapshot_id) = port.get("snapshot_id").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let receipt = cas.get_json(snapshot_id).map_err(|e| e.to_string())?;
    // A Task source Snapshot (`af.task-snapshot/1`) names its Manifest as `manifest_id`; a
    // Review Snapshot receipt (`af/snapshot@1`) as `manifest_artifact_id`.
    let Some(manifest_id) = ["manifest_id", "manifest_artifact_id"]
        .into_iter()
        .find_map(|key| receipt.get(key).and_then(serde_json::Value::as_str))
    else {
        return Ok(None);
    };
    let manifest: Manifest =
        serde_json::from_value(cas.get_json(manifest_id).map_err(|e| e.to_string())?)
            .map_err(|e| format!("reading source Snapshot Manifest: {e}"))?;
    Ok(Some(
        review_config::layout::classify_manifest(&manifest).undeclared,
    ))
}

/// The document `af task show --json` prints for a Task, or `af task explain --json` with
/// `explain`, read from `state` with the same default resolution those commands use: the
/// `--json` document, byte for byte, as a value. `None` when the Store holds no such Task.
/// Nothing is written.
pub(crate) fn inspection_document(
    state: &Path,
    id: &str,
    explain: bool,
) -> Result<Option<serde_json::Value>, String> {
    if !store_present(state)? {
        return Ok(None);
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    if store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(inspection(&cas, &store, id, explain, None)?.value))
}

/// One recorded artifact of a Task state directory, as the Store holds it: the envelope an
/// inspection document names by ID. Nothing is written.
pub(crate) fn recorded_artifact(state: &Path, id: &str) -> Result<serde_json::Value, String> {
    if !review_core::is_digest(id) {
        return Err(format!("{id} is not an artifact ID"));
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    cas.get_json(id).map_err(|e| e.to_string())
}

/// `af task start --execute` without printing: a test records a real Task this way.
#[cfg(test)]
pub(crate) fn run_silently(file: &Path, repo: &Path, state: &Path) -> Result<i32, String> {
    let options = StartOptions {
        file: file.to_path_buf(),
        bindings: None,
        source_bindings: None,
        repo: repo.to_path_buf(),
        state: Some(state.to_path_buf()),
        authority: "HEAD".into(),
        uncommitted: false,
        json: false,
        plan_only: false,
        timeout_secs: None,
        optimization_history: None,
        fit: false,
        replace: false,
    };
    start_kind(options, None, Presentation::Silent)
}

/// The inspection document of one Task, and what its text form needs besides.
struct Inspection {
    value: serde_json::Value,
    state: TaskProjection,
    result: Option<TaskResultV1>,
    bindings: Option<review_core::task::input_bindings::TaskInputBindingsV1>,
    view: state::TaskView,
}

/// The document `af task show --json` and `af task explain --json` print.
pub(crate) const TASK_INSPECTION_V13: &str = "af/task-inspection@13";

#[allow(clippy::too_many_arguments)]
fn present_with_format(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    json_output: bool,
    explain: bool,
    tree: bool,
    undeclared: Option<&review_config::layout::PathGroup>,
) -> Result<Presented, String> {
    let Inspection {
        value,
        state,
        result,
        bindings,
        view,
    } = inspection(cas, store, id, explain, undeclared)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|e| e.to_string())?
        );
    } else if explain && state.plan_id.is_some() {
        print!("{}", preview::current(cas, &state, tree)?);
    } else {
        for line in show_lines(
            cas,
            &value,
            &state,
            result.as_ref(),
            bindings.as_ref(),
            &view,
        )? {
            println!("{line}");
        }
        if explain {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
            );
        }
    }
    Ok(Presented::new(id, exit_code(&state, &view), &view))
}

/// The exit code a presentation of `state` ends with: its outcome's (`af help exit-codes`), 4
/// for a Task that waits for a person's decision, else 0.
fn exit_code(state: &TaskProjection, view: &state::TaskView) -> i32 {
    match view.state.outcome {
        // Only a finished Task ended with an outcome a command reports; an abandoned one is
        // resumable, and showing it succeeds.
        Some(outcome)
            if view.state.phase == review_core::task::state::TaskStatePhaseV1::Finished =>
        {
            i32::from(outcome.exit_code())
        }
        _ if state.phase
            == (TaskPhaseV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsHuman,
            }) =>
        {
            4
        }
        _ => 0,
    }
}

/// `af task show ID` as text: the state, then what the Store recorded about it, then each
/// failed node and the failing tests of a failed gate, and as the last line the next command.
fn show_lines(
    cas: &Cas,
    value: &serde_json::Value,
    state: &TaskProjection,
    result: Option<&TaskResultV1>,
    bindings: Option<&review_core::task::input_bindings::TaskInputBindingsV1>,
    view: &state::TaskView,
) -> Result<Vec<String>, String> {
    let mut lines = vec![format!(
        "Task {}: {}",
        preview::text(&state.task_id),
        view.state.label()
    )];
    if let Some(summary) = state::summary(view) {
        lines.push(format!("why: {}", preview::text(&summary)));
    } else if view.state.phase == review_core::task::state::TaskStatePhaseV1::Abandoned
        && let Some(abandoned) = recovery::abandoned(state, clock()?)
    {
        // The interrupted node of an abandoned Task, and when it stopped (ADR-0200).
        lines.push(format!("why: {}", abandoned.why()));
    }
    if let Some(id) = &state.plan_id {
        lines.push(format!("Plan {id}"));
    }
    if let Some(line) = unknown_usage_line(state) {
        lines.push(line);
    }
    // One line per bound output, through the same sanitizer the preview uses: a referenced
    // Task ID is untrusted display data wherever it is printed. A port bound from several
    // outputs prints one line per output (ADR-0134).
    let bound = bindings.iter().flat_map(|record| &record.bindings);
    let bound = bound.flat_map(|(port, binding)| {
        preview::bound_outputs(binding).map(move |output| (port, output))
    });
    for (port, binding) in bound {
        lines.push(match &binding.task {
            Some(task) => format!(
                "bound {} <- task {}/{} ({})",
                preview::text(port),
                preview::text(&task.task_id),
                preview::text(&task.port),
                preview::acceptance_name(task.acceptance)
            ),
            None => format!(
                "bound {} <- artifact {}",
                preview::text(port),
                preview::text(&binding.artifact_id)
            ),
        });
    }
    lines.extend(continuation::show_line(value));
    lines.extend(check_cache_lines(value));
    lines.extend(check_report_lines(cas, state));
    lines.extend(remote_check_lines(value));
    lines.extend(storage_sweep_line(value).filter(|_| crate::storage::shows_recorded_sweeps()));
    lines.extend(measurement_lines(cas, state)?);
    lines.extend(report_lines(cas, result)?);
    for retry in &state.gate_retries {
        lines.push(format!(
            "gate retried: {} ran again on the same candidate",
            retry
                .gates
                .iter()
                .map(|gate| preview::text(&gate.node))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // A running Attempt's Provider wait or re-ask (ADR-0199); a settled one's diagnostic,
    // printed by the failure lines, carries its retry history.
    lines.extend(provider_event_lines(state));
    lines.extend(state::failed_gate_tail(&state.task_id, view));
    lines.extend(state::failure_lines(view));
    lines.extend(runs::take_show_notes());
    lines.push(format!(
        "next: {}",
        view.state.next_action.as_deref().unwrap_or("none")
    ));
    Ok(lines)
}

/// One line per running Attempt that recorded a Provider event (ADR-0199), from its latest:
/// `NODE: waiting 120 s for capacity (retry 2 of 3)` while the wait lasts, then
/// `NODE: retry 2 of 3 after capacity is running`. A settled Attempt prints none: its
/// diagnostic carries its retry history.
fn provider_event_lines(state: &TaskProjection) -> Vec<String> {
    use review_core::task::provider_event::ProviderEventV1;
    let running: BTreeSet<String> = state
        .execution
        .iter()
        .flat_map(|execution| execution.attempt_accounting())
        .filter(|attempt| attempt.started && attempt.settled_unix_ms.is_none() && !attempt.released)
        .map(|attempt| attempt.attempt_id)
        .collect();
    let mut latest = BTreeMap::new();
    for event in &state.provider_events {
        if running.contains(&event.attempt_id) {
            latest.insert(event.attempt_id.clone(), event);
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    latest
        .values()
        .map(|event| {
            let line = match &event.event {
                ProviderEventV1::RetryWait {
                    class,
                    retry,
                    max_retries,
                    wait_ms,
                    ..
                } if event.now_unix_ms.saturating_add(*wait_ms) <= now => format!(
                    "retry {retry} of {max_retries} after {} is running",
                    class.label()
                ),
                _ => event.describe(),
            };
            format!("{}: {line}", event.node)
        })
        .collect()
}

/// The Task's settled Attempts whose usage is unknown, by cause (ADR-0143): no usage was
/// reported, so each was charged zero.
fn unknown_usage(
    state: &TaskProjection,
) -> BTreeMap<review_core::task::usage::TaskUnknownUsageCauseV1, u64> {
    let mut causes = BTreeMap::new();
    for attempt in state
        .execution
        .iter()
        .flat_map(|execution| execution.attempt_accounting())
    {
        if let Some(cause) = attempt.unknown_usage {
            *causes.entry(cause).or_default() += 1;
        }
    }
    causes
}

/// `tokens 1200 (+1 unknown: capacity)`: the line `af task show` adds when an Attempt's usage
/// is unknown, so the charged total never reads as the whole spend. None when every usage is
/// known.
fn unknown_usage_line(state: &TaskProjection) -> Option<String> {
    let causes = unknown_usage(state);
    let unknown = causes.values().sum::<u64>();
    (unknown > 0).then(|| {
        let causes = causes
            .iter()
            .map(|(cause, n)| {
                if *n == unknown {
                    cause.label().to_owned()
                } else {
                    format!("{n} {}", cause.label())
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "tokens {} (+{unknown} unknown: {causes})",
            state
                .execution
                .as_ref()
                .map_or(0, |e| e.budget.committed_tokens())
        )
    })
}

/// One line per recorded Measurement and per recorded comparison (ADR-0132), in node order: a
/// measurement's median elapsed time, or the repetition and reason it failed at, and a
/// comparison's conclusion on its objective's metric. A Task without either prints nothing.
fn measurement_lines(cas: &Cas, state: &TaskProjection) -> Result<Vec<String>, String> {
    use review_core::task::measurement::*;
    use review_core::task::pipeline::ReceiptOutcomeV1;
    let mut lines = Vec::new();
    let Some(execution) = &state.execution else {
        return Ok(lines);
    };
    for (node, (_, output)) in &execution.outputs {
        for (port, value) in &output.outputs {
            let [id] = value.artifact_ids.as_slice() else {
                continue;
            };
            if value.artifact_type == MEASUREMENT_V1 {
                let measurement: MeasurementV1 = artifact(cas, id, MEASUREMENT_V1)?;
                let detail = match (&measurement.failure, measurement.metric(ELAPSED_MS)) {
                    (Some(failure), _) => format!(
                        "failed at repetition {} of {}: {}",
                        failure.repetition,
                        measurement.repetitions,
                        failure.reason.as_str()
                    ),
                    (None, Some(elapsed)) => format!(
                        "median elapsed {} ms over {} repetitions{}",
                        elapsed.median,
                        elapsed.n,
                        cache_condition(&measurement)
                    ),
                    (None, None) => "passed".into(),
                };
                lines.push(format!(
                    "measurement {} {}: {detail}",
                    preview::text(node),
                    preview::text(port)
                ));
            } else if value.artifact_type == MEASUREMENT_COMPARISON_V1 {
                let comparison: MeasurementComparisonV1 =
                    artifact(cas, id, MEASUREMENT_COMPARISON_V1)?;
                let row = comparison
                    .metrics
                    .get(&comparison.metric)
                    .ok_or("Comparison lacks its objective's metric")?;
                lines.push(format!(
                    "comparison {} {}: {} on {} ({} → {}), {}",
                    preview::text(node),
                    preview::text(&comparison.objective),
                    row.conclusion.as_str(),
                    preview::text(&comparison.metric),
                    row.baseline_median
                        .as_ref()
                        .map_or_else(|| "—".to_string(), ToString::to_string),
                    row.candidate_median
                        .as_ref()
                        .map_or_else(|| "—".to_string(), ToString::to_string),
                    match comparison.outcome {
                        ReceiptOutcomeV1::Passed => "passed",
                        ReceiptOutcomeV1::Failed => "failed",
                        ReceiptOutcomeV1::Inconclusive => "inconclusive",
                    }
                ));
            }
        }
    }
    Ok(lines)
}

/// A report Task's title, its verifier's outcome and the source Snapshot its citations were
/// checked against (ADR-0133), from the recorded public acceptance receipt. Any other Task, and
/// a report Task with no result yet, prints nothing.
fn report_lines(cas: &Cas, result: Option<&TaskResultV1>) -> Result<Vec<String>, String> {
    use review_core::task::document::{DOCUMENT_V1, DocumentV1};
    use review_core::task::pipeline::ReceiptOutcomeV1;
    use review_core::task::report_task::*;
    let Some(verification) = result.and_then(|result| {
        result
            .outputs
            .get("verification")
            .filter(|port| port.artifact_type == REPORT_VERIFICATION_V1)
    }) else {
        return Ok(Vec::new());
    };
    let [id] = verification.artifact_ids.as_slice() else {
        return Ok(Vec::new());
    };
    let name = |outcome: ReceiptOutcomeV1| match outcome {
        ReceiptOutcomeV1::Passed => "passed",
        ReceiptOutcomeV1::Failed => "failed",
        ReceiptOutcomeV1::Inconclusive => "inconclusive",
    };
    let receipt: ReportVerificationV1 = artifact(cas, id, REPORT_VERIFICATION_V1)?;
    let document: DocumentV1 = artifact(cas, &receipt.document_id, DOCUMENT_V1)?;
    let draft = cas
        .get_artifact(&document.draft_id)
        .map_err(|e| e.to_string())?;
    let title = draft.payload["title"]
        .as_str()
        .ok_or("Report draft has no title")?;
    let verifier = match &receipt.evaluation_id {
        Some(id) => {
            let evaluation: ReportEvaluationV1 = artifact(cas, id, REPORT_EVALUATION_V1)?;
            name(evaluation.outcome).to_owned()
        }
        None => format!("not run (report acceptance {})", name(receipt.outcome)),
    };
    Ok(vec![
        format!("report: {}", preview::text(title)),
        format!("report verifier: {verifier}"),
        format!("report Snapshot: {}", receipt.source_snapshot_id),
    ])
}

/// The cache condition a warm measure's repetitions actually ran under: `, warm` when every one
/// had the Warm Check Cache, `, cold (<reason>)` when none did, `, warm N of M` when they differ.
/// A measure that did not ask for the cache adds nothing.
fn cache_condition(measurement: &review_core::task::measurement::MeasurementV1) -> String {
    if !measurement.warm {
        return String::new();
    }
    let total = measurement.runs.len();
    let warm = measurement
        .runs
        .iter()
        .filter(|run| run.cache.as_ref().is_some_and(|cache| cache.warm))
        .count();
    if warm == total {
        ", warm".into()
    } else if warm == 0 {
        let reason = measurement
            .runs
            .iter()
            .find_map(|run| run.cache.as_ref().and_then(|cache| cache.reason.clone()))
            .unwrap_or_else(|| "unbound".into());
        format!(", cold ({reason})")
    } else {
        format!(", warm {warm} of {total}")
    }
}

/// Every remote check's evidence a Task's selected outputs name: what its gate pushed and
/// opened, for the gate cleanup of ADR-0144.
pub(crate) fn remote_check_evidence(
    cas: &Cas,
    state: &TaskProjection,
) -> Result<Vec<review_core::task::remote_check::RemoteCheckEvidenceV1>, String> {
    remote_check_records(cas, state)?
        .into_iter()
        .map(|record| serde_json::from_value(record["record"].clone()).map_err(|e| e.to_string()))
        .collect()
}

/// Every remote check result of the Task's current check receipts (ADR-0140), in node and
/// check order, with the evidence document it derives from. Replay reads only the Store.
fn remote_check_records(
    cas: &Cas,
    state: &TaskProjection,
) -> Result<Vec<serde_json::Value>, String> {
    use review_core::task::remote_check::{REMOTE_CHECK_EVIDENCE_V1, RemoteCheckEvidenceV1};
    use review_core::task::verification::{TASK_CHECK_RECEIPT_V1, TaskCheckReceiptV1};
    let mut records = Vec::new();
    let Some(execution) = &state.execution else {
        return Ok(records);
    };
    for (node, (_, output)) in &execution.outputs {
        for value in output.outputs.values() {
            if value.artifact_type != TASK_CHECK_RECEIPT_V1 {
                continue;
            }
            for id in &value.artifact_ids {
                let receipt: TaskCheckReceiptV1 = artifact(cas, id, TASK_CHECK_RECEIPT_V1)?;
                for (check, result_id) in &receipt.checks {
                    // The result's second shape names its evidence; a local result has none.
                    let result = cas.get_json(result_id).map_err(|e| e.to_string())?;
                    let Some(evidence_id) = result["remote"].as_str() else {
                        continue;
                    };
                    let evidence: RemoteCheckEvidenceV1 =
                        artifact(cas, evidence_id, REMOTE_CHECK_EVIDENCE_V1)?;
                    evidence.validate()?;
                    let mut record = json!({
                        "node": node,
                        "check": check,
                        "status": result["status"],
                        "artifact_id": evidence_id,
                        "artifact_type": REMOTE_CHECK_EVIDENCE_V1,
                        "record": evidence,
                    });
                    // The kept log excerpt of the jobs that did not succeed, when there is one.
                    if let Some(log) = result["stdout"].as_str() {
                        record["log_id"] = json!(log);
                    }
                    records.push(record);
                }
            }
        }
    }
    Ok(records)
}

/// 64 bits from the operating system's random source, through the standard library's
/// per-process hash keys. It is the only entropy a Task's opening transition carries, and it is
/// journaled there: replay reads it back and never draws it again.
/// The process id a resuming writer's name records. A fixture that compares two runs' Store
/// event logs byte for byte under the injected clock (`AF_TEST_CLOCK_FIXED_MS`) names it with
/// `AF_TEST_WRITER_PID`, the one other thing that tells two runs of one fixture apart.
/// Production release binaries never read the setting.
fn writer_pid() -> u32 {
    #[cfg(debug_assertions)]
    if let Some(pid) = std::env::var("AF_TEST_WRITER_PID")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return pid;
    }
    std::process::id()
}

fn opening_nonce() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// Whole seconds since the epoch of an evidence timestamp (`YYYY-MM-DDTHH:MM:SS[.f]Z`).
fn evidence_seconds(text: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    // Days from the civil date (proleptic Gregorian), after Howard Hinnant's algorithm.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// The text `af task show` prints for each remote check (ADR-0140): the executor and pull
/// request, the run and its attempt, each required job with its conclusion and duration, the
/// unsuccessful steps, the kept log excerpt and the refusal reason; then the latest gate cleanup
/// (ADR-0144), and the two commands that clean up after the gate whenever af has not.
/// One line for the latest Storage Budget sweep recorded on the Task (ADR-0144): what the sweep
/// that ended a run of it removed, the bytes that left the disk, what failed, and the totals
/// against the budget, the total after it `unknown` when af could not measure it.
fn storage_sweep_line(inspection: &serde_json::Value) -> Option<String> {
    let sweep = inspection["storage_sweeps"].as_array()?.last()?;
    let count = |field: &str| sweep[field].as_array().map_or(0, Vec::len) as u64;
    let removed = count("removals") + sweep["omitted_removals"].as_u64().unwrap_or(0);
    let failed = count("failures") + sweep["omitted_failures"].as_u64().unwrap_or(0);
    let bytes = sweep["freed_bytes"].as_u64().unwrap_or(0);
    let total = |field: &str| match sweep[field].as_u64() {
        Some(bytes) => crate::storage::human_bytes(bytes),
        None => "unknown".into(),
    };
    Some(format!(
        "storage sweep: removed {removed} entr{} ({}){}; {} → {} of a {} budget{}",
        if removed == 1 { "y" } else { "ies" },
        crate::storage::human_bytes(bytes),
        if failed > 0 {
            format!(
                ", {failed} removal{} failed",
                if failed == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        },
        total("total_before"),
        total("total_after"),
        total("max_bytes"),
        match sweep["stop"].as_str() {
            Some("nothing_evictable") => {
                ", still over: the rest is in use or was used within the hour"
            }
            Some("unknown") => {
                ", what is left unknown: a removal stopped part way and what it left cannot be \
                 measured"
            }
            _ => "",
        }
    ))
}

fn remote_check_lines(inspection: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cleanups = BTreeSet::new();
    for entry in inspection["remote_checks"].as_array().into_iter().flatten() {
        let record = &entry["record"];
        let name = preview::text(entry["check"].as_str().unwrap_or_default());
        let status = preview::text(entry["status"].as_str().unwrap_or("not_run"));
        let executor = preview::text(record["executor"].as_str().unwrap_or_default());
        let github = preview::text(record["github"].as_str().unwrap_or_default());
        let mut line = format!("remote check {name}: {status} by {executor} on {github}");
        if let Some(url) = record["pull_request"]["url"].as_str() {
            line.push_str(&format!(", pull request {}", preview::text(url)));
        }
        if let (Some(id), Some(attempt)) = (
            record["run"]["id"].as_u64(),
            record["run"]["attempt"].as_u64(),
        ) {
            line.push_str(&format!(", run {id} attempt {attempt}"));
        }
        lines.push(line);
        for job in record["jobs"].as_array().into_iter().flatten() {
            let duration = match (
                job["started_at"].as_str().and_then(evidence_seconds),
                job["completed_at"].as_str().and_then(evidence_seconds),
            ) {
                (Some(start), Some(end)) if end >= start => format!(" in {} s", end - start),
                _ => String::new(),
            };
            let mut text = format!(
                "  job {}: {}{duration}",
                preview::text(job["name"].as_str().unwrap_or_default()),
                preview::text(job["conclusion"].as_str().unwrap_or_default())
            );
            let steps = job["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|step| {
                    format!(
                        "{} ({})",
                        preview::text(step["name"].as_str().unwrap_or_default()),
                        preview::text(step["conclusion"].as_str().unwrap_or_default())
                    )
                })
                .collect::<Vec<_>>();
            if !steps.is_empty() {
                text.push_str(&format!("; unsuccessful steps: {}", steps.join(", ")));
            }
            if job["conclusion"] != "success"
                && let Some(url) = job["url"].as_str()
            {
                text.push_str(&format!("; log at {}", preview::text(url)));
            }
            lines.push(text);
        }
        if let Some(log) = entry["log_id"].as_str() {
            lines.push(format!(
                "  log excerpt of the unsuccessful jobs: CAS object {}",
                preview::text(log)
            ));
        }
        if let Some(reason) = record["reason"].as_str() {
            lines.push(format!("  reason: {}", preview::text(reason)));
        }
        if let Some(diagnostic) = record["diagnostic"].as_str() {
            lines.push(format!(
                "  diagnostic: {}",
                preview::text(&diagnostic.replace('\n', " | "))
            ));
        }
        if record["state"] != "refused" {
            cleanups.insert((
                github.clone(),
                record["pull_request"]["number"].as_u64(),
                preview::text(inspection["task_id"].as_str().unwrap_or_default()),
            ));
        }
    }
    let latest = inspection["gate_cleanups"]
        .as_array()
        .and_then(|cleanups| cleanups.last());
    if let Some(cleanup) = latest {
        let pulls = cleanup["pull_requests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_u64)
            .map(|number| format!("pull request {number}"))
            .collect::<Vec<_>>();
        let branches = cleanup["branches"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        lines.push(match cleanup["outcome"].as_str() {
            Some("done") => format!(
                "gate cleanup: done; closed {} and deleted {}",
                if pulls.is_empty() {
                    "no pull request".to_string()
                } else {
                    pulls.join(", ")
                },
                preview::text(&branches.join(" and "))
            ),
            _ => format!(
                "gate cleanup: failed; {}",
                preview::text(cleanup["reason"].as_str().unwrap_or("no reason recorded"))
            ),
        });
        if cleanup["outcome"] == "done" {
            return lines;
        }
    }
    for (github, number, task_id) in cleanups {
        lines.push(format!(
            "gate branches af-gate/{task_id}/base and af-gate/{task_id}/head stay until af removes them (`af storage prune --apply` retries) or you do:"
        ));
        if let Some(number) = number {
            lines.push(format!("  gh pr close {number} --repo {github}"));
        }
        lines.push(format!(
            "  git push <push-url> --delete af-gate/{task_id}/base af-gate/{task_id}/head"
        ));
    }
    lines
}

/// One line per warm check (ADR-0131), named by its evidence group's check binding: its outcome,
/// its host-observed elapsed time, or that it never started or was run remotely, and, per
/// declared kind,
/// `warm <bytes>` or `cold <reason>`. A check without observations prints nothing, so a Task
/// whose policy has no `[warm]` reads as before, and no line is ever printed without a name.
fn check_cache_lines(inspection: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    for observation in inspection["runtime_observations"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let record = &observation["record"];
        let Some(caches) = record["caches"].as_array().filter(|c| !c.is_empty()) else {
            continue;
        };
        let checks = record["spans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|span| span["kind"] == "check")
            .collect::<Vec<_>>();
        let span = match checks.as_slice() {
            [] => None,
            [check] => Some(check),
            _ => continue,
        };
        let binding = &record["check"];
        let Some(name) = binding["name"]
            .as_str()
            .or_else(|| span.and_then(|check| check["label"].as_str()))
        else {
            continue;
        };
        let timing = match (span, binding["outcome"].as_str()) {
            (Some(check), Some(outcome)) => {
                format!(
                    "{} in {}",
                    preview::text(outcome),
                    crate::human::duration(check["elapsed_ms"].as_u64().unwrap_or(0))
                )
            }
            (Some(check), None) => {
                crate::human::duration(check["elapsed_ms"].as_u64().unwrap_or(0))
            }
            // A remote check ran nothing here: every declared kind was skipped as `remote`
            // (ADR-0140), and its own `remote check` lines say what ran and where.
            (None, outcome)
                if caches.iter().all(|cache| {
                    cache["kind"]
                        .as_str()
                        .is_some_and(|kind| kind.ends_with(":remote"))
                }) =>
            {
                format!(
                    "{}, run remotely",
                    preview::text(outcome.unwrap_or("not_run"))
                )
            }
            (None, outcome) => format!(
                "{}, never started",
                preview::text(outcome.unwrap_or("not_run"))
            ),
        };
        lines.push(format!(
            "check {}: {timing}, {}",
            preview::text(name),
            cache_states(caches)
        ));
    }
    lines
}

/// Each check node's latest `af/TaskCheckReport@1`, with its artifact ID, by node ID: the
/// report of the node's latest settled Attempt, so a retried gate's report, not the failed one
/// before it. A report that does not decode or validate is skipped, as it was never admitted.
fn latest_check_reports(
    cas: &Cas,
    state: &TaskProjection,
) -> BTreeMap<String, (String, review_core::task::check_report::TaskCheckReportV1)> {
    use review_core::task::check_report::*;
    let mut latest = BTreeMap::new();
    for attempt in state
        .execution
        .iter()
        .flat_map(|execution| execution.attempt_accounting())
    {
        for id in &attempt.raw_artifact_ids {
            let Ok(Some(artifact)) = cas.get_optional_artifact(id) else {
                continue;
            };
            if artifact.artifact_type != TASK_CHECK_REPORT_V1 {
                continue;
            }
            let Ok(report) = serde_json::from_value::<TaskCheckReportV1>(artifact.payload) else {
                continue;
            };
            if report.validate().is_err() {
                continue;
            }
            let node = match &artifact.producer {
                review_core::Producer::Attempt { node_id, .. } => node_id.clone(),
                _ => continue,
            };
            // The node's latest settled Attempt wins: a retried gate's report, not the failed
            // one before it.
            let settled = attempt.settled_unix_ms.unwrap_or(0);
            if latest
                .get(&node)
                .is_none_or(|(earlier, _, _): &(u64, String, TaskCheckReportV1)| {
                    *earlier <= settled
                })
            {
                latest.insert(node, (settled, id.clone(), report));
            }
        }
    }
    latest
        .into_iter()
        .map(|(node, (_, id, report))| (node, (id, report)))
        .collect()
}

/// What the latest check report of each check node says (`af/TaskCheckReport@1`): where a
/// check stopped, by stage, step and the fix its gate names; the tests that failed in its
/// parallel run but passed alone (`flaky`), each list on its own line so an operator sees what
/// nearly failed; the tests that failed twice; and, for a check that did not pass or had flaky
/// tests, the host's load around it. A report the check wrote but af refused says so.
fn check_report_lines(cas: &Cas, state: &TaskProjection) -> Vec<String> {
    use review_core::task::check_report::*;
    let mut lines = Vec::new();
    for (_, report) in latest_check_reports(cas, state).values() {
        let statuses = cas
            .get_artifact(&report.receipt_id)
            .ok()
            .and_then(|receipt| receipt.payload["checks"].as_object().cloned())
            .unwrap_or_default();
        for (check, run) in &report.checks {
            let name = preview::text(check);
            let passed = statuses
                .get(check)
                .and_then(|id| id.as_str())
                .and_then(|id| cas.get_json(id).ok())
                .is_some_and(|result| result["status"] == "passed");
            if run.report == CheckReportStateV1::Refused {
                lines.push(format!(
                    "check {name}: its report was refused: {}",
                    preview::text(run.refusal.as_deref().unwrap_or("unreadable"))
                ));
            }
            if let Some(step) = run.stopped_at() {
                let then = match &step.fix {
                    Some(fix) => format!("fix: {}", preview::text(fix)),
                    None => format!("rerun it with: {}", preview::text(&step.command.join(" "))),
                };
                lines.push(format!(
                    "check {name}: stopped at {} step {} (exit {}); {then}",
                    step.stage.as_str(),
                    preview::text(&step.label),
                    step.exit_code
                ));
            }
            let tests = run.tests.as_ref();
            let flaky = tests.map_or(&[][..], |tests| tests.flaky.as_slice());
            if !flaky.is_empty() {
                lines.push(format!(
                    "check {name} flaky: {} (failed in the parallel run, passed alone)",
                    flaky
                        .iter()
                        .map(|test| preview::text(&test.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(tests) = tests.filter(|tests| tests.failed_count > 0) {
                let more = tests.failed_count as usize - tests.failed.len();
                let verb = match tests.rerun {
                    CheckRerunV1::Isolated => "failed twice",
                    _ => "failed",
                };
                lines.push(format!(
                    "check {name} {verb}: {}{}",
                    tests
                        .failed
                        .iter()
                        .map(|test| preview::text(test))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if more > 0 {
                        format!(" and {more} more")
                    } else {
                        String::new()
                    }
                ));
            }
            if !passed || !flaky.is_empty() {
                let load = |load: &Option<String>| load.clone().unwrap_or_else(|| "unknown".into());
                let cpus = if run.host.cpus_start == run.host.cpus_end {
                    format!("{} CPUs", run.host.cpus_end)
                } else {
                    format!(
                        "{} CPUs at start, {} at end",
                        run.host.cpus_start, run.host.cpus_end
                    )
                };
                lines.push(format!(
                    "check {name} host: load {} at start, {} at end, {cpus}{}",
                    load(&run.host.load_1m_start),
                    load(&run.host.load_1m_end),
                    tests.map_or(String::new(), |tests| format!(
                        ", {} test threads",
                        tests.test_threads
                    ))
                ));
            }
        }
    }
    lines
}

/// Why a warm directory was removed after its check, and which byte bound acted when one did:
/// `bound_exceeded max_bytes` for an eviction whose check's result stood, `bound_exceeded
/// hard_max_bytes` for a check the bound ended (ADR-0135).
fn eviction(cache: &serde_json::Value) -> String {
    let why = cache["evicted_reason"].as_str().unwrap_or("bound_exceeded");
    match cache["bound"].as_str() {
        Some(bound) => format!("{} {}", preview::text(why), preview::text(bound)),
        None => preview::text(why),
    }
}

fn cache_states(caches: &[serde_json::Value]) -> String {
    let mut seen = BTreeSet::new();
    caches
        .iter()
        .map(|cache| {
            let kind = cache["kind"].as_str().unwrap_or_default();
            let (base, reason) = kind.split_once(':').unwrap_or((kind, ""));
            let bytes = crate::human::bytes(cache["bytes_available"].as_u64().unwrap_or(0));
            let available = cache["bytes_available"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0);
            let first = seen.insert(base.to_owned());
            let base = preview::text(base);
            let reason = preview::text(reason);
            if cache["eligible"] == true {
                let state = if available {
                    format!("{base} warm {bytes}")
                } else {
                    format!("{base} cold empty")
                };
                match cache["evicted_bytes"].as_u64() {
                    Some(evicted) => format!(
                        "{state}, removed {} ({})",
                        crate::human::bytes(evicted),
                        eviction(cache)
                    ),
                    None => state,
                }
            } else {
                let state = if reason == "cloned" {
                    // A private copy of a busy layer (ADR-0198): warm with the holder's build.
                    format!("{base} copied {bytes} (layer busy)")
                } else if first {
                    format!("{base} cold {reason}")
                } else {
                    format!("{base} removed {bytes} ({reason})")
                };
                match cache["evicted_bytes"].as_u64() {
                    Some(evicted) => format!(
                        "{state}, removed {} ({})",
                        crate::human::bytes(evicted),
                        eviction(cache)
                    ),
                    None => state,
                }
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the inspection document every `af task` presentation of one Task prints.
fn inspection(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    explain: bool,
    undeclared: Option<&review_config::layout::PathGroup>,
) -> Result<Inspection, String> {
    let state: TaskProjection = store
        .task_projection(cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    let result: Option<TaskResultV1> = match &state.phase {
        TaskPhaseV1::Finished { result_id } => Some(artifact(cas, result_id, TASK_RESULT_V1)?),
        _ => None,
    };
    // One state model for every rendering of this Task: `list`, `show`, `report` and `run`.
    let store_dir = store.database_path();
    let cancelled = runs::cancelled_epoch(store_dir.as_deref().and_then(Path::parent), &state);
    let view = selected_view(cas, &state, result.as_ref(), clock()?, cancelled)?;
    let mut value = json!({"schema":TASK_INSPECTION_V13,"task_id":state.task_id,"revision_id":state.revision_id,"phase":state.phase,"state":view.state,"plan_id":state.plan_id,
        "chargeable_tokens":state.execution.as_ref().map_or(0,|e|e.budget.committed_tokens()).to_string(),"attempts":state.execution.as_ref().map_or(0,|e|e.budget.begun_attempts())});
    // The wall clock starts at the first admission: the wall the Task has, when its clock
    // started, and the deadline it has since.
    value["clock"] = json!({
        "wall_ms": state.wall_ms(),
        "started_unix_ms": state.clock_started_unix_ms(),
        "deadline_unix_ms": state.clock_started_unix_ms().map(|_| state.deadline_unix_ms()),
    });
    if let Some(gate) = &view.gate {
        value["gate"] = serde_json::to_value(gate).map_err(|e| e.to_string())?;
        // The gate's evidence the text prints beside the state (ADR-0196): each check node's
        // latest check report, and every retry of a failed gate.
        let reports = latest_check_reports(cas, &state);
        if !reports.is_empty() {
            value["gate"]["check_reports"] =
                json!(reports
                .iter()
                .map(|(node, (id, report))| json!({"node":node,"artifact_id":id,"record":report}))
                .collect::<Vec<_>>());
        }
        if !state.gate_retries.is_empty() {
            value["gate"]["retries"] = json!(state.gate_retries);
        }
    }
    if !view.failures.is_empty() {
        value["failures"] = state::failures_value(&view);
    }
    let unknown = unknown_usage(&state).values().sum::<u64>();
    if unknown > 0 {
        value["unknown_usage_attempts"] = json!(unknown);
    }
    if let Some(selection) = selection::recorded(cas, &state.revision)? {
        value["selection"] = selection;
    }
    if let Some(proof) = &state.planning {
        value["planning"] = json!({"bootstrap_plan_id":proof.bootstrap_plan_id(), "proposal_id":proof.proposal_id(), "request_revision_id":proof.revision_id()});
    }
    let events = store
        .replay(&review_store::store::task::task_run_id(id).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut history = Vec::new();
    let mut execution = Vec::new();
    let mut owned_child_sets = Vec::new();
    let mut experiments = Vec::new();
    let mut runtime_observations = Vec::new();
    let mut runtime_attempt_ids = BTreeSet::new();
    let mut decisions = Vec::new();
    let mut provider_events = Vec::new();
    for event in events {
        // A lease's execution marker, a gate retry and a Provider event are their own event
        // kinds, not `TaskTransition@5`: the history lists the transitions only; the Provider
        // events are listed beside it.
        if review_store::store::task::is_provider_event(&event) {
            let value = review_store::store::task::read_provider_event(&event)
                .map_err(|e| e.to_string())?;
            provider_events.push(json!({"sequence":event.sequence,"record":value}));
            continue;
        }
        if review_store::store::task::is_beside_transitions(&event) {
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
            if let review_core::task::execution::TaskExecutionRecordV1::Settled {
                attempt_id,
                raw_artifact_ids,
                ..
            } = &record
            {
                for id in raw_artifact_ids {
                    // Raw Worker/provider captures deliberately share this list with typed
                    // sidecars and may use a provider-native identity rather than a CAS blob
                    // digest. Only successfully decoded envelopes can be public observations.
                    let Ok(artifact) = cas.get_artifact(id) else {
                        continue;
                    };
                    if artifact.artifact_type
                        == review_core::task::runtime::TASK_RUNTIME_EVIDENCE_V1
                    {
                        let evidence: review_core::task::runtime::TaskRuntimeEvidenceV1 =
                            serde_json::from_value(artifact.payload.clone())
                                .map_err(|e| e.to_string())?;
                        evidence.validate()?;
                        if evidence.task_id != state.task_id
                            || evidence.attempt_id != *attempt_id
                            || artifact.producer
                                != (review_core::Producer::Attempt {
                                    run_id: review_store::store::task::task_run_id(&state.task_id)
                                        .map_err(|e| e.to_string())?,
                                    node_id: evidence.node.clone(),
                                    attempt_id: attempt_id.clone(),
                                })
                        {
                            return Err(
                                "Task runtime evidence contradicts its settled Attempt".into()
                            );
                        }
                        runtime_attempt_ids.insert(attempt_id.clone());
                        runtime_observations.push(json!({
                            "artifact_id": id,
                            "artifact_type": artifact.artifact_type,
                            "record": evidence,
                        }));
                    }
                }
            }
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
            match &record {
                review_core::task::execution::TaskExecutionRecordV1::ExperimentPrepared { prepared_id } => {
                    let prepared = cas.get_artifact(prepared_id).map_err(|e| e.to_string())?;
                    experiments.push(json!({"kind":"prepared","artifact_id":prepared_id,"artifact_type":prepared.artifact_type,"record":prepared.payload}));
                }
                review_core::task::execution::TaskExecutionRecordV1::ExperimentPlanDecided { prepared_id, decision_id } => {
                    let decision = cas.get_artifact(decision_id).map_err(|e| e.to_string())?;
                    experiments.push(json!({"kind":"decision","prepared_id":prepared_id,"artifact_id":decision_id,"artifact_type":decision.artifact_type,"record":decision.payload}));
                }
                review_core::task::execution::TaskExecutionRecordV1::ExperimentChildrenRegistered { prepared_id, decision_id, child_plan_id } => {
                    let plan = cas.get_artifact(child_plan_id).map_err(|e| e.to_string())?;
                    experiments.push(json!({"kind":"registered","prepared_id":prepared_id,"decision_id":decision_id,"artifact_id":child_plan_id,"artifact_type":plan.artifact_type,"record":plan.payload}));
                }
                _ => {}
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
    if !provider_events.is_empty() {
        value["provider_events"] = json!(provider_events);
    }
    if !runtime_observations.is_empty() {
        let run_id = review_store::store::task::task_run_id(id).map_err(|e| e.to_string())?;
        let walls = store
            .task_attempt_wall(&run_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|wall| runtime_attempt_ids.contains(&wall.attempt_id))
            .collect::<Vec<_>>();
        value["attempt_walls"] = serde_json::to_value(walls).map_err(|e| e.to_string())?;
        value["runtime_observations"] = json!(runtime_observations);
    }
    let remote_checks = remote_check_records(cas, &state)?;
    if !remote_checks.is_empty() {
        value["remote_checks"] = json!(remote_checks);
    }
    if !state.gate_cleanups.is_empty() {
        value["gate_cleanups"] = json!(state.gate_cleanups);
    }
    if !state.storage_sweeps.is_empty() {
        value["storage_sweeps"] = json!(state.storage_sweeps);
    }
    if !owned_child_sets.is_empty() {
        value["owned_child_sets"] = json!(owned_child_sets);
    }
    if !experiments.is_empty() {
        value["experiments"] = json!(experiments);
    }
    if !state.review_handoffs.is_empty() {
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
    let mut reports = Vec::new();
    for id in &state.run_reports {
        use review_core::task::report::*;
        let report =
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
        let report = serde_json::to_value(report).map_err(|e| e.to_string())?;
        reports.push(json!({"artifact_id":id,"report":report,"diagnostics":diagnostics}));
    }
    value["run_reports"] = json!(reports);
    if !state.adoption_observations.is_empty() {
        use review_core::task::optimization_light::{
            OPTIMIZATION_ADOPTION_TASK_EVIDENCE_V1, OptimizationAdoptionTaskEvidenceV1,
        };
        let mut observations = Vec::new();
        for (observation_id, observation) in &state.adoption_observations {
            let envelope = cas
                .get_artifact(observation_id)
                .map_err(|error| error.to_string())?;
            let mut task_evidence = Vec::new();
            for id in &envelope.input_artifacts {
                let Some(candidate) = cas
                    .get_optional_artifact(id)
                    .map_err(|error| error.to_string())?
                else {
                    continue;
                };
                if candidate.artifact_type != OPTIMIZATION_ADOPTION_TASK_EVIDENCE_V1 {
                    continue;
                }
                let evidence: OptimizationAdoptionTaskEvidenceV1 =
                    serde_json::from_value(candidate.payload).map_err(|error| error.to_string())?;
                evidence.validate()?;
                if evidence.adoption_receipt_id != observation.adoption_receipt_id
                    || evidence.commit_snapshot_id != observation.commit_snapshot_id
                {
                    return Err("adoption Task evidence contradicts its observation".into());
                }
                task_evidence.push(json!({"artifact_id":id,"record":evidence}));
            }
            observations.push(json!({
                "artifact_id": observation_id,
                "record": observation,
                "task_evidence": task_evidence,
            }));
        }
        value["adoption_observations"] = json!(observations);
    }
    // Present only when the Task file carried an `inputs` table, so a Task without bindings
    // emits neither the field nor the new generation and keeps the document it has today.
    let bindings = input_bindings::recorded(cas, &state.revision)?;
    if let Some(record) = &bindings {
        value["input_bindings"] = serde_json::to_value(record).map_err(|e| e.to_string())?;
    }
    // The lineage of a continued Task (ADR-0203), read from its revision's provenance.
    if let Some((findings_id, record)) = continuation::recorded(cas, &state.revision)? {
        value["continues"] = continuation::inspection_value(&findings_id, &record);
    }
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
    // The whole list the one-line advisory could only summarize. A presentation that did not
    // just classify the captured manifest derives the same group from the recorded source
    // Snapshot, so `start`, `run` and `show` documents of one Task agree. Present only when
    // the Snapshot actually carries undeclared paths, so a document that had no such field
    // still has none.
    let derived = match undeclared {
        Some(group) => Some(group.clone()),
        None => recorded_undeclared_af_paths(cas, &state.revision)?,
    };
    if let Some(group) = derived.filter(|group| !group.is_empty()) {
        value["undeclared_af_paths"] = serde_json::to_value(group).map_err(|e| e.to_string())?;
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
    Ok(Inspection {
        value,
        state,
        result,
        bindings,
        view,
    })
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
    fn review_generation_is_omitted_or_two() {
        let value = serde_json::json!({"reviewers":{"correctness":"required"},"gate":"major","clean_rounds":1,"max_rounds":2,"allow_targeted_repairs":false});
        let settings: ReviewSettings = serde_json::from_value(value.clone()).unwrap();
        settings.check_generation().unwrap();
        assert_eq!(serde_json::to_value(settings).unwrap(), value);
        let mut explicit = value;
        explicit["generation"] = serde_json::json!(2);
        let settings: ReviewSettings = serde_json::from_value(explicit.clone()).unwrap();
        settings.check_generation().unwrap();
        assert_eq!(serde_json::to_value(settings).unwrap(), explicit);
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
                    .and_then(|v| v.check_generation())
                    .is_err()
            );
        }
    }
}

#[cfg(test)]
mod tree_preview_tests {
    use super::*;
    use crate::tui::panes::pipelines::{PREVIEW_TASK_ID, preview_task};
    use crate::tui::tests::{hub_repo, stable_lines};

    #[test]
    fn the_silent_tree_preview_is_what_task_explain_tree_prints() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = hub_repo(&root);
        let task = preview_task("fixture/implementation", "implement", 3);
        let file = root.join("preview.json");
        std::fs::write(&file, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
        // `af task plan --file` records the Task, and `af task explain --tree` prints this.
        let state = root.join("state");
        let options = StartOptions {
            file: file.clone(),
            bindings: None,
            source_bindings: None,
            repo: repo.clone(),
            state: Some(state.clone()),
            authority: "HEAD".into(),
            uncommitted: false,
            json: false,
            plan_only: true,
            timeout_secs: None,
            optimization_history: None,
            fit: false,
            replace: false,
        };
        start(options).unwrap();
        let cas = Cas::open_existing(state.join("cas")).unwrap();
        let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let projection = store.task_projection(&cas, PREVIEW_TASK_ID).unwrap();
        let printed = preview::current(&cas, &projection.unwrap(), true).unwrap();
        let silent = plan_tree_preview_at(&file, &repo, "HEAD").unwrap();
        assert_eq!(stable_lines(&silent.text), stable_lines(&printed));
        let text = &silent.text;
        assert!(
            text.contains("PIPE  fixture/implementation@1.0.0  [configured]"),
            "{text}"
        );
        // Every Worker row carries its slot's binding, keyed by the row's line.
        let lines: Vec<&str> = text.lines().collect();
        assert!(!silent.slots.is_empty(), "{text}");
        for (line, binding) in &silent.slots {
            assert!(binding.contains(" -> command Worker"), "{binding}");
            let row = lines[*line];
            assert!(row.contains("command Worker"), "{row}");
        }
    }
}

#[cfg(test)]
mod check_cache_line_tests {
    use super::*;

    fn group(record: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"runtime_observations": [{"record": record}]})
    }

    #[test]
    fn every_warm_line_is_named_by_its_check_binding() {
        let span = serde_json::json!({"kind": "check", "label": "kernel", "elapsed_ms": 812});
        let warm =
            serde_json::json!({"kind": "cargo_target", "eligible": true, "bytes_available": 4096});
        let started = group(serde_json::json!({
            "check": {"name": "kernel", "outcome": "passed", "rustup_home": "kernel_home"},
            "spans": [span],
            "caches": [warm, {"kind": "cargo_home", "eligible": true, "bytes_available": 0}],
        }));
        assert_eq!(
            check_cache_lines(&started),
            ["check kernel: passed in 812 ms, cargo_target warm 4.0 KiB, cargo_home cold empty"]
        );
        let skipped = group(serde_json::json!({
            "check": {"name": "markdownlint", "outcome": "not_run", "rustup_home": "kernel_home"},
            "caches": [
                {"kind": "cargo_target:deadline_exhausted", "eligible": false, "bytes_available": 0},
                {"kind": "cargo_home:deadline_exhausted", "eligible": false, "bytes_available": 0},
            ],
        }));
        assert_eq!(
            check_cache_lines(&skipped),
            [
                "check markdownlint: not_run, never started, cargo_target cold deadline_exhausted, \
              cargo_home cold deadline_exhausted"
            ]
        );
        // A remote check ran nothing here, and its line says so rather than "never started".
        let remote = group(serde_json::json!({
            "check": {"name": "kernel", "outcome": "passed", "rustup_home": "kernel_home"},
            "caches": [
                {"kind": "cargo_target:remote", "eligible": false, "bytes_available": 0},
                {"kind": "cargo_home:remote", "eligible": false, "bytes_available": 0},
            ],
        }));
        assert_eq!(
            check_cache_lines(&remote),
            [
                "check kernel: passed, run remotely, cargo_target cold remote, cargo_home cold remote"
            ]
        );
        // A group that names no check prints nothing rather than an unnamed line.
        let unnamed = group(serde_json::json!({
            "caches": [{"kind": "cargo_target:busy", "eligible": false, "bytes_available": 0}],
        }));
        assert!(check_cache_lines(&unnamed).is_empty());
    }
}

#[cfg(test)]
mod remote_check_line_tests {
    use super::*;

    fn fixture(name: &str) -> serde_json::Value {
        let path = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/remote-checks/evidence")
            .join(name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn entry(check: &str, status: &str, record: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"node": "root.nodes.check", "check": check, "status": status,
            "artifact_id": format!("sha256:{}", "e".repeat(64)),
            "artifact_type": "af/RemoteCheckEvidence@1", "record": record})
    }

    #[test]
    fn a_remote_check_prints_its_pull_request_run_jobs_steps_and_cleanup() {
        let mut failed = entry("kernel", "failed", fixture("observed-failed.json"));
        failed["log_id"] = serde_json::json!(format!("sha256:{}", "a".repeat(64)));
        let inspection = serde_json::json!({"task_id": "rc1", "remote_checks": [
            failed,
            entry("docs", "not_run", fixture("published-check-missing.json")),
        ]});
        let lines = remote_check_lines(&inspection);
        assert_eq!(
            lines,
            [
                "remote check kernel: failed by github-pr on octo/gate, pull request \
                 https://github.com/octo/gate/pull/12, run 77 attempt 2",
                "  job validation / lint: success in 750 s",
                "  job validation / check (ubuntu-latest): failure in 750 s; unsuccessful steps: \
                 Run make check (failure), Upload report (skipped); log at \
                 https://github.com/octo/gate/actions/runs/77/job/1002",
                &format!(
                    "  log excerpt of the unsuccessful jobs: CAS object sha256:{}",
                    "a".repeat(64)
                ),
                "remote check docs: not_run by github-pr on octo/gate, pull request \
                 https://github.com/octo/gate/pull/12",
                "  reason: remote_check_missing",
                "gate branches af-gate/rc1/base and af-gate/rc1/head stay until af removes them \
                 (`af storage prune --apply` retries) or you do:",
                "  gh pr close 12 --repo octo/gate",
                "  git push <push-url> --delete af-gate/rc1/base af-gate/rc1/head",
            ]
        );
        // ADR-0144: a done cleanup replaces the manual commands; a failed one says why first.
        let mut cleaned = inspection.clone();
        cleaned["gate_cleanups"] = serde_json::json!([{"schema": "af/TaskGateCleanup@1",
            "github": "octo/gate", "pull_requests": [12],
            "branches": ["af-gate/rc1/base", "af-gate/rc1/head"], "outcome": "done"}]);
        let lines = remote_check_lines(&cleaned);
        assert_eq!(
            lines.last().unwrap(),
            "gate cleanup: done; closed pull request 12 and deleted af-gate/rc1/base and \
             af-gate/rc1/head"
        );
        assert!(!lines.iter().any(|line| line.contains("gh pr close")));
        let mut failed = inspection.clone();
        failed["gate_cleanups"] = serde_json::json!([{"schema": "af/TaskGateCleanup@1",
            "github": "octo/gate", "pull_requests": [12],
            "branches": ["af-gate/rc1/base", "af-gate/rc1/head"], "outcome": "failed",
            "reason": "closing pull request #12 failed: HTTP 403"}]);
        let lines = remote_check_lines(&failed);
        assert!(
            lines.contains(
                &"gate cleanup: failed; closing pull request #12 failed: HTTP 403".to_string()
            ),
            "{lines:?}"
        );
        assert!(lines.iter().any(|line| line.contains("gh pr close 12")));
        // A refusal published nothing, so it prints no cleanup; its redacted diagnostic shows.
        let refused = serde_json::json!({"task_id": "rc1", "remote_checks": [
            entry("kernel", "not_run", fixture("refused-push-refused.json")),
        ]});
        let lines = remote_check_lines(&refused);
        assert_eq!(
            lines[0],
            "remote check kernel: not_run by github-pr on octo/gate"
        );
        assert_eq!(lines[1], "  reason: remote_push_refused");
        assert!(
            lines[2].starts_with("  diagnostic: To <push-url> | "),
            "{}",
            lines[2]
        );
        assert_eq!(lines.len(), 3);
        assert!(remote_check_lines(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn the_latest_storage_sweep_is_one_line() {
        assert_eq!(storage_sweep_line(&serde_json::json!({})), None);
        let mut inspection = serde_json::json!({"storage_sweeps": [
            {"schema": "af/TaskStorageSweep@2", "removals": [], "omitted_removals": 0,
             "failures": ["removing /x: it changed"], "omitted_failures": 0, "freed_bytes": 0,
             "stop": "fits", "max_bytes": 1024, "total_before": 0, "total_after": 0},
            {"schema": "af/TaskStorageSweep@2", "removals": [
                {"kind": "task", "path": "/s", "task_id": "gc-older", "bytes": 2048,
                 "rule": "collection"}],
             "omitted_removals": 0, "failures": [], "omitted_failures": 0,
             "freed_bytes": 2048, "stop": "nothing_evictable", "max_bytes": 1024,
             "total_before": 4096, "total_after": 2048}]});
        assert_eq!(
            storage_sweep_line(&inspection).as_deref(),
            Some(
                "storage sweep: removed 1 entry (2.0 KiB); 4.0 KiB → 2.0 KiB of a 1.0 KiB \
                 budget, still over: the rest is in use or was used within the hour"
            )
        );
        // A removal that stopped part way, 1 KiB of it freed, what it left unmeasured.
        inspection["storage_sweeps"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(
            {"schema": "af/TaskStorageSweep@2", "removals": [
                {"kind": "task", "path": "/s", "task_id": "gc-older", "bytes": 2048,
                 "rule": "collection"}],
             "omitted_removals": 0, "failures": ["removing /c: only partly removed"],
             "omitted_failures": 0, "freed_bytes": 3072, "stop": "unknown", "max_bytes": 1024,
             "total_before": 4096, "total_after": "unknown"}));
        assert_eq!(
            storage_sweep_line(&inspection).as_deref(),
            Some(
                "storage sweep: removed 1 entry (3.0 KiB), 1 removal failed; 4.0 KiB → unknown \
                 of a 1.0 KiB budget, what is left unknown: a removal stopped part way and what \
                 it left cannot be measured"
            )
        );
    }

    #[test]
    fn evidence_times_are_whole_seconds_since_the_epoch() {
        assert_eq!(evidence_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(evidence_seconds("2000-03-01T00:00:01Z"), Some(951_868_801));
        assert_eq!(
            evidence_seconds("2026-10-04T10:12:30.5Z"),
            Some(1_791_108_750)
        );
        assert_eq!(evidence_seconds("not a time"), None);
    }

    #[test]
    fn the_task_owner_survives_rereading_and_differs_between_stores() {
        use crate::tui::panes::pipelines::{PREVIEW_TASK_ID, preview_task};
        use crate::tui::tests::hub_repo;
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = hub_repo(&root);
        let file = root.join("task.json");
        let task = preview_task("fixture/implementation", "implement", 3);
        std::fs::write(&file, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
        let owner = |state: &Path| {
            start(StartOptions {
                file: file.clone(),
                bindings: None,
                source_bindings: None,
                repo: repo.clone(),
                state: Some(state.to_path_buf()),
                authority: "HEAD".into(),
                uncommitted: false,
                json: false,
                plan_only: true,
                timeout_secs: None,
                optimization_history: None,
                fit: false,
                replace: false,
            })
            .unwrap();
            let host = domain::machine_remote_checks(state).unwrap();
            let resolve = host.owner.unwrap();
            let first = resolve(PREVIEW_TASK_ID).unwrap();
            assert_eq!(resolve(PREVIEW_TASK_ID).unwrap(), first, "stable on resume");
            assert!(review_core::is_digest(&first));
            assert!(resolve("no-such-task").is_err());
            first
        };
        // No pause between the two: the opening writer's nonce, not the clock, tells the
        // Stores apart, even for one process opening both.
        let one = owner(&root.join("state-one"));
        let two = owner(&root.join("state-two"));
        assert_ne!(
            one, two,
            "the same Task ID in another Store has another owner"
        );
    }

    #[test]
    fn the_opening_writer_carries_a_fresh_nonce_and_stays_a_name() {
        let (first, second) = (opening_nonce(), opening_nonce());
        assert_ne!(first, second);
        let writer = format!("cli-{}-{first:016x}", std::process::id());
        assert!(review_core::task::is_name(&writer), "{writer}");
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    /// The state line a non-zero exit ends stderr with is the state the exit code was judged
    /// by: the Store is not read again, so a Task collected (here: a Store removed) between
    /// showing it and noting it neither silences stderr nor changes what it says.
    #[test]
    fn the_state_note_comes_from_the_view_that_chose_the_exit_code() {
        let root = tempfile::tempdir().unwrap();
        let (_repo, state) = crate::tui::tests::hub_with_tasks(root.path());
        let id = "pagination-unfinished";
        let cas = Cas::open_existing(state.join("cas")).unwrap();
        let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let expected = state_line(id, &inspection(&cas, &store, id, false, None).unwrap().view);
        let presented = present(&cas, &store, id, true, false).unwrap();
        assert_ne!(presented.code, 0, "{expected}");
        drop((cas, store));
        std::fs::remove_dir_all(&state).unwrap();
        presented.note(presented.code);
        let noted: Vec<String> = std::mem::take(
            &mut *LAST_LINES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
        .into_iter()
        .filter(|line| line.starts_with(&format!("Task {id}:")))
        .collect();
        assert_eq!(noted, [expected.clone()]);
        assert!(expected.contains("; next: af task "), "{expected}");
        presented.note(0);
        assert!(
            !LAST_LINES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .any(|line| line.starts_with(&format!("Task {id}:"))),
            "an exit 0 adds no line"
        );
    }
}
