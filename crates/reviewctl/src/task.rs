//! The minimal v2 implement Task: one implementer, read-only gates, one evaluator, one sealed
//! internal Snapshot. Delivery is intentionally absent; the source checkout is never writable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_check::{CheckDefinition, CheckResult, CheckRunner, CheckStatus};
use review_config::lock::{Lockfile, Registry};
use review_core::{Arg, Command, SubjectKind};
use review_runner::{ContextManifest, ModelRunner, ResolvedReviewer, TokenUsage, extract_result};
use review_sandbox::{Mode, Sandbox};
use review_source_git::{Capture, EntryKind, Manifest, Repo};
use review_store::Cas;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{candidate_identity, normalize_absolute, resolve_filesystem_path, xdg_state_root};

#[derive(Debug, Clone)]
pub(super) struct TaskOptions {
    repo: PathBuf,
    pipeline: PathBuf,
    state: Option<PathBuf>,
    goal: String,
    authority: String,
    uncommitted: bool,
    timeout: Option<Duration>,
    json: bool,
}

pub(super) fn parse(mut args: impl Iterator<Item = String>) -> Result<TaskOptions, String> {
    let mut options = TaskOptions {
        repo: PathBuf::from("."),
        pipeline: PathBuf::from(".af/pipelines/implement.toml"),
        state: None,
        goal: String::new(),
        authority: "HEAD".into(),
        uncommitted: false,
        timeout: None,
        json: false,
    };
    let mut kind = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--kind" => kind = Some(value()?),
            "--goal" => options.goal = value()?,
            "--repo" => options.repo = PathBuf::from(value()?),
            "--pipeline" => options.pipeline = PathBuf::from(value()?),
            "--state" => options.state = Some(PathBuf::from(value()?)),
            "--authority" => options.authority = value()?,
            "--uncommitted" => options.uncommitted = true,
            "--timeout-secs" => {
                let seconds = value()?
                    .parse::<u64>()
                    .map_err(|_| "--timeout-secs must be an integer".to_string())?;
                options.timeout = Some(Duration::from_secs(seconds));
            }
            "--json" => options.json = true,
            _ => return Err(format!("unknown task flag `{flag}`")),
        }
    }
    if kind.as_deref() != Some("implement") {
        return Err("v2 supports exactly `--kind implement`".into());
    }
    if options.goal.trim().is_empty() {
        return Err("an implement Task requires a non-empty --goal".into());
    }
    if options.uncommitted && options.authority != "HEAD" {
        return Err("--uncommitted and an explicit --authority cannot be combined".into());
    }
    Ok(options)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskPipeline {
    version: u32,
    kind: String,
    implementer: String,
    evaluator: String,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default = "default_check_timeout_seconds")]
    check_timeout_seconds: u64,
    attempt_tokens: u64,
    run_tokens: u64,
    checks: Vec<TaskCheck>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCheck {
    name: String,
    program: String,
    #[serde(default)]
    args: Vec<TaskArg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskArg {
    value: String,
}

fn default_timeout_seconds() -> u64 {
    1800
}

fn default_check_timeout_seconds() -> u64 {
    3600
}

impl TaskPipeline {
    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.kind != "implement" {
            return Err(
                "implement pipeline must declare version = 1 and kind = \"implement\"".into(),
            );
        }
        if self.implementer.trim().is_empty()
            || self.evaluator.trim().is_empty()
            || self.implementer == self.evaluator
        {
            return Err(
                "implement pipeline needs distinct implementer and evaluator Workers".into(),
            );
        }
        if self.timeout_seconds == 0 || self.check_timeout_seconds == 0 {
            return Err("Task timeouts must be positive".into());
        }
        if self.attempt_tokens == 0 || self.run_tokens == 0 || self.attempt_tokens > self.run_tokens
        {
            return Err(
                "Task token budgets must be positive and attempt_tokens <= run_tokens".into(),
            );
        }
        if self.checks.is_empty()
            || self
                .checks
                .iter()
                .any(|check| check.name.trim().is_empty() || check.program.trim().is_empty())
        {
            return Err("implement pipeline needs at least one named acceptance gate".into());
        }
        Ok(())
    }

    fn check_definitions(&self) -> Vec<CheckDefinition> {
        self.checks
            .iter()
            .map(|check| {
                CheckDefinition::new(
                    &check.name,
                    Command::new(
                        &check.program,
                        check
                            .args
                            .iter()
                            .map(|argument| Arg::literal(&argument.value))
                            .collect(),
                    ),
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotReceipt {
    schema: &'static str,
    kind: &'static str,
    content_digest: String,
    manifest_artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_snapshot_id: Option<String>,
    repository_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerAuthority {
    role: String,
    name: String,
    version: String,
    digest: String,
    package_artifact_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerEvidence {
    role: String,
    package_artifact_id: String,
    raw_artifact: String,
    usage: TokenUsage,
    context_manifest: ContextManifest,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct GateEvidence {
    name: String,
    status: CheckStatus,
    result_artifact: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evaluation {
    verdict: EvaluationVerdict,
    summary: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EvaluationVerdict {
    Approve,
    Reject,
}

struct TaskStore {
    connection: Connection,
}

impl TaskStore {
    fn open(path: &Path) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 CREATE TABLE IF NOT EXISTS task_events (
                   task_id TEXT NOT NULL,
                   sequence INTEGER NOT NULL,
                   event_type TEXT NOT NULL,
                   artifact_id TEXT NOT NULL,
                   PRIMARY KEY(task_id, sequence)
                 );",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self { connection })
    }

    fn append(
        &mut self,
        cas: &Cas,
        task_id: &str,
        event_type: &str,
        artifact_id: &str,
    ) -> Result<(), String> {
        cas.flush().map_err(|error| error.to_string())?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let sequence: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM task_events WHERE task_id = ?1",
                [task_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO task_events(task_id, sequence, event_type, artifact_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![task_id, sequence, event_type, artifact_id],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())
    }
}

struct LoadedAuthority {
    pipeline: TaskPipeline,
    pipeline_artifact_id: String,
    lock_artifact_id: String,
    project_artifact_id: String,
    implementer: ResolvedReviewer,
    evaluator: ResolvedReviewer,
    implementer_authority: WorkerAuthority,
    evaluator_authority: WorkerAuthority,
}

pub(super) fn start(options: TaskOptions) -> Result<bool, String> {
    let repo_path = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let state = task_state(&options, &repo_path)?;
    if state.starts_with(&repo_path) {
        return Err("Task state must be outside the repository".into());
    }
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store = TaskStore::open(&state.join("tasks.sqlite"))?;
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(&repo_path, &git_home);
    let captured = if options.uncommitted {
        Capture::new(&repo, &cas)
            .dirty()
            .map_err(|error| format!("capturing the worktree: {error}"))?
    } else {
        Capture::new(&repo, &cas)
            .committed(&options.authority)
            .map_err(|error| format!("capturing authority `{}`: {error}", options.authority))?
    };
    let source_manifest_id = put_manifest(&cas, &captured.manifest)?;
    let source_snapshot_id = cas
        .put_json(
            &serde_json::to_value(SnapshotReceipt {
                schema: "af/snapshot@1",
                kind: "source",
                content_digest: captured.content_digest.clone(),
                manifest_artifact_id: source_manifest_id,
                parent_snapshot_id: None,
                repository_id: captured.repository_id.clone(),
                source_revision: captured.source_revision.clone(),
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    let goal_artifact_id = cas
        .put(options.goal.as_bytes())
        .map_err(|error| error.to_string())?;
    let loaded = load_authority(&options, &captured.manifest, &cas)?;
    let task_id = task_id(&repo_path, &source_snapshot_id, &goal_artifact_id);
    let opened = cas
        .put_json(&serde_json::json!({
            "schema": "af/task-opened@1",
            "task_id": task_id,
            "kind": "implement",
            "goal_artifact_id": goal_artifact_id,
            "source_snapshot_id": source_snapshot_id,
            "pipeline_artifact_id": loaded.pipeline_artifact_id,
            "lock_artifact_id": loaded.lock_artifact_id,
            "project_artifact_id": loaded.project_artifact_id,
            "workers": [&loaded.implementer_authority, &loaded.evaluator_authority],
        }))
        .map_err(|error| error.to_string())?;
    store.append(&cas, &task_id, "TaskOpened@1", &opened)?;
    progress(&options, format!("task     {task_id}"));
    progress(&options, format!("source   {source_snapshot_id}"));

    let template = review_sandbox::Template::materialize(&captured.manifest, &cas)
        .map_err(|error| error.to_string())?;
    let implement_sandbox = Sandbox::from_template(&template, Mode::EphemeralWrite)
        .map_err(|error| error.to_string())?;
    let implement_input = serde_json::json!({
        "schema": "af/implement-input@1",
        "task_id": task_id,
        "goal": options.goal,
        "source_snapshot_id": source_snapshot_id,
        "constraints": [
            "Edit only the provided sandbox.",
            "Leave the sandbox in the complete state that should be evaluated.",
            "Do not publish, push, create a branch, or write back to the source checkout."
        ],
        "budget": {"reserved_tokens": loaded.pipeline.attempt_tokens},
    });
    let implement_evidence = match invoke_worker(
        "implementer",
        &loaded.implementer,
        &loaded.implementer_authority,
        implement_sandbox.root(),
        &cas,
        &implement_input,
        options
            .timeout
            .unwrap_or(Duration::from_secs(loaded.pipeline.timeout_seconds)),
    ) {
        Ok(evidence) => evidence,
        Err(reason) => {
            return finish(
                &options,
                &cas,
                &mut store,
                &task_id,
                &source_snapshot_id,
                None,
                &goal_artifact_id,
                &loaded,
                &[],
                &[],
                "implementer",
                &reason,
            );
        }
    };
    let implement_event = cas
        .put_json(&serde_json::to_value(&implement_evidence).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    store.append(&cas, &task_id, "WorkerCompleted@1", &implement_event)?;
    if implement_evidence.usage.chargeable_tokens > loaded.pipeline.attempt_tokens
        || implement_evidence.usage.chargeable_tokens > loaded.pipeline.run_tokens
    {
        return finish(
            &options,
            &cas,
            &mut store,
            &task_id,
            &source_snapshot_id,
            None,
            &goal_artifact_id,
            &loaded,
            &[implement_evidence],
            &[],
            "budget",
            "implementer exceeded the admitted token budget",
        );
    }

    let sealed = implement_sandbox
        .seal()
        .map_err(|error| format!("sealing implementer sandbox: {error}"))?;
    let mutations = serde_json::json!({
        "added": sealed.mutations.added,
        "modified": sealed.mutations.modified,
        "deleted": sealed.mutations.deleted,
    });
    let derived_manifest = sealed
        .capture_snapshot(&cas)
        .map_err(|error| format!("capturing derived Snapshot: {error}"))?;
    let derived_manifest_id = put_manifest(&cas, &derived_manifest)?;
    let derived_snapshot_id = cas
        .put_json(
            &serde_json::to_value(SnapshotReceipt {
                schema: "af/snapshot@1",
                kind: "derived",
                content_digest: derived_manifest.content_digest(),
                manifest_artifact_id: derived_manifest_id,
                parent_snapshot_id: Some(source_snapshot_id.clone()),
                repository_id: captured.repository_id,
                source_revision: None,
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    verify_snapshot(&cas, &derived_manifest, &derived_snapshot_id)?;
    let snapshot_event = cas
        .put_json(&serde_json::json!({
            "schema": "af/derived-snapshot@1",
            "snapshot_id": derived_snapshot_id,
            "mutations": mutations,
        }))
        .map_err(|error| error.to_string())?;
    store.append(&cas, &task_id, "SnapshotDerived@1", &snapshot_event)?;
    progress(&options, format!("derived  {derived_snapshot_id}"));

    let mut gates = Vec::new();
    for definition in loaded.pipeline.check_definitions() {
        let gate_sandbox = Sandbox::materialize(&derived_manifest, &cas, Mode::ReadOnly)
            .map_err(|error| error.to_string())?;
        // Build systems may write caches, but acceptance gates may not mutate the Snapshot.
        // Give them an external disposable target directory instead of weakening read-only mode.
        let gate_scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut result = CheckRunner::new(&cas, gate_sandbox.root())
            .with_env(
                "CARGO_TARGET_DIR",
                gate_scratch
                    .path()
                    .join("cargo-target")
                    .display()
                    .to_string(),
            )
            .with_timeout(Duration::from_secs(loaded.pipeline.check_timeout_seconds))
            .run(&definition);
        let gate_sealed = gate_sandbox.seal().map_err(|error| error.to_string())?;
        if !gate_sealed.unchanged() {
            result = mutated_gate_result(result, gate_sealed.mutations.paths());
        }
        let result_artifact = cas
            .put_json(&serde_json::to_value(&result).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        gates.push(GateEvidence {
            name: result.name.clone(),
            status: result.status,
            result_artifact: result_artifact.clone(),
        });
        store.append(&cas, &task_id, "GateCompleted@1", &result_artifact)?;
        progress(
            &options,
            format!("gate     {} -> {:?}", result.name, result.status),
        );
    }
    if gates.iter().any(|gate| gate.status != CheckStatus::Passed) {
        return finish(
            &options,
            &cas,
            &mut store,
            &task_id,
            &source_snapshot_id,
            Some(&derived_snapshot_id),
            &goal_artifact_id,
            &loaded,
            &[implement_evidence],
            &gates,
            "gates",
            "one or more required acceptance gates did not pass",
        );
    }
    if loaded
        .pipeline
        .run_tokens
        .saturating_sub(implement_evidence.usage.chargeable_tokens)
        < loaded.pipeline.attempt_tokens
    {
        return finish(
            &options,
            &cas,
            &mut store,
            &task_id,
            &source_snapshot_id,
            Some(&derived_snapshot_id),
            &goal_artifact_id,
            &loaded,
            &[implement_evidence],
            &gates,
            "budget",
            "remaining run budget cannot reserve the evaluator attempt",
        );
    }

    let evaluator_sandbox = Sandbox::materialize(&derived_manifest, &cas, Mode::ReadOnly)
        .map_err(|error| error.to_string())?;
    let evaluation_input = serde_json::json!({
        "schema": "af/evaluate-input@1",
        "task_id": task_id,
        "goal": options.goal,
        "source_snapshot_id": source_snapshot_id,
        "derived_snapshot_id": derived_snapshot_id,
        "mutations": mutations,
        "gates": gates,
        "budget": {"reserved_tokens": loaded.pipeline.attempt_tokens},
        "output_contract": {"verdict": "approve|reject", "summary": "string"},
    });
    let evaluator_evidence = match invoke_worker(
        "evaluator",
        &loaded.evaluator,
        &loaded.evaluator_authority,
        evaluator_sandbox.root(),
        &cas,
        &evaluation_input,
        options
            .timeout
            .unwrap_or(Duration::from_secs(loaded.pipeline.timeout_seconds)),
    ) {
        Ok(evidence) => evidence,
        Err(reason) => {
            return finish(
                &options,
                &cas,
                &mut store,
                &task_id,
                &source_snapshot_id,
                Some(&derived_snapshot_id),
                &goal_artifact_id,
                &loaded,
                &[implement_evidence],
                &gates,
                "evaluator",
                &reason,
            );
        }
    };
    let evaluator_sealed = evaluator_sandbox
        .seal()
        .map_err(|error| error.to_string())?;
    if !evaluator_sealed.unchanged() {
        return finish(
            &options,
            &cas,
            &mut store,
            &task_id,
            &source_snapshot_id,
            Some(&derived_snapshot_id),
            &goal_artifact_id,
            &loaded,
            &[implement_evidence, evaluator_evidence],
            &gates,
            "evaluator",
            "evaluator mutated its read-only Snapshot",
        );
    }
    let evaluator_event = cas
        .put_json(&serde_json::to_value(&evaluator_evidence).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    store.append(&cas, &task_id, "WorkerCompleted@1", &evaluator_event)?;
    if evaluator_evidence.usage.chargeable_tokens > loaded.pipeline.attempt_tokens
        || implement_evidence
            .usage
            .chargeable_tokens
            .saturating_add(evaluator_evidence.usage.chargeable_tokens)
            > loaded.pipeline.run_tokens
    {
        return finish(
            &options,
            &cas,
            &mut store,
            &task_id,
            &source_snapshot_id,
            Some(&derived_snapshot_id),
            &goal_artifact_id,
            &loaded,
            &[implement_evidence, evaluator_evidence],
            &gates,
            "budget",
            "evaluator exceeded the admitted token budget",
        );
    }
    let raw = cas
        .get(&evaluator_evidence.raw_artifact)
        .map_err(|error| error.to_string())?;
    let answer = match worker_answer(&loaded.evaluator.runner.program, &raw) {
        Ok(answer) => answer,
        Err(reason) => {
            return finish(
                &options,
                &cas,
                &mut store,
                &task_id,
                &source_snapshot_id,
                Some(&derived_snapshot_id),
                &goal_artifact_id,
                &loaded,
                &[implement_evidence, evaluator_evidence],
                &gates,
                "evaluation",
                &reason,
            );
        }
    };
    let evaluation: Evaluation = match serde_json::from_str(extract_result(&answer)) {
        Ok(evaluation) => evaluation,
        Err(error) => {
            return finish(
                &options,
                &cas,
                &mut store,
                &task_id,
                &source_snapshot_id,
                Some(&derived_snapshot_id),
                &goal_artifact_id,
                &loaded,
                &[implement_evidence, evaluator_evidence],
                &gates,
                "evaluation",
                &format!("evaluator returned malformed verdict: {error}"),
            );
        }
    };
    let evaluation_artifact = cas
        .put_json(&serde_json::json!({
            "verdict": match evaluation.verdict {
                EvaluationVerdict::Approve => "approve",
                EvaluationVerdict::Reject => "reject",
            },
            "summary": evaluation.summary,
        }))
        .map_err(|error| error.to_string())?;
    store.append(
        &cas,
        &task_id,
        "EvaluationCompleted@1",
        &evaluation_artifact,
    )?;
    let evidence = vec![implement_evidence, evaluator_evidence];
    if evaluation.verdict == EvaluationVerdict::Reject {
        return finish(
            &options,
            &cas,
            &mut store,
            &task_id,
            &source_snapshot_id,
            Some(&derived_snapshot_id),
            &goal_artifact_id,
            &loaded,
            &evidence,
            &gates,
            "evaluation",
            "independent evaluator rejected the derived Snapshot",
        );
    }
    finish_verified(
        &options,
        &cas,
        &mut store,
        &task_id,
        &source_snapshot_id,
        &derived_snapshot_id,
        &goal_artifact_id,
        &loaded,
        &evidence,
        &gates,
    )
}

fn task_state(options: &TaskOptions, repository: &Path) -> Result<PathBuf, String> {
    match &options.state {
        Some(state) => resolve_filesystem_path(state),
        None => {
            let identity = Sha256::digest(repository.as_os_str().as_encoded_bytes());
            normalize_absolute(
                &xdg_state_root()?
                    .join("af/task/local")
                    .join(&format!("{identity:x}")[..16]),
            )
        }
    }
}

fn task_id(repository: &Path, source: &str, goal: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut digest = Sha256::new();
    digest.update(repository.as_os_str().as_encoded_bytes());
    digest.update(source.as_bytes());
    digest.update(goal.as_bytes());
    digest.update(now.to_be_bytes());
    digest.update(std::process::id().to_be_bytes());
    format!("task-{}", &format!("{:x}", digest.finalize())[..20])
}

fn load_authority(
    options: &TaskOptions,
    manifest: &Manifest,
    cas: &Cas,
) -> Result<LoadedAuthority, String> {
    let pipeline_path = canonical_authority_path(&options.pipeline)?;
    let pipeline_bytes = manifest_bytes(manifest, cas, &pipeline_path)?;
    let project_bytes = manifest_bytes(manifest, cas, ".af/af.toml")?;
    let lock_bytes = manifest_bytes(manifest, cas, ".af/af.lock")?;
    validate_project(&project_bytes, &pipeline_path)?;
    let lockfile = Lockfile::from_toml(
        std::str::from_utf8(&lock_bytes)
            .map_err(|error| format!(".af/af.lock is not UTF-8: {error}"))?,
    )
    .map_err(|error| error.to_string())?;
    validate_pipeline_pin(&lockfile, &pipeline_path, &pipeline_bytes)?;
    let pipeline: TaskPipeline = toml::from_str(
        std::str::from_utf8(&pipeline_bytes)
            .map_err(|error| format!("implement pipeline is not UTF-8: {error}"))?,
    )
    .map_err(|error| format!("implement pipeline: {error}"))?;
    pipeline.validate()?;
    let registry = Registry::captured(captured_registry(manifest, cas)?);
    let implementer = lockfile
        .resolve_for_subject(&pipeline.implementer, &registry, SubjectKind::WholeTree)
        .map_err(|error| error.to_string())?;
    let evaluator = lockfile
        .resolve_for_subject(&pipeline.evaluator, &registry, SubjectKind::WholeTree)
        .map_err(|error| error.to_string())?;
    let pipeline_artifact_id = cas
        .put(&pipeline_bytes)
        .map_err(|error| error.to_string())?;
    let lock_artifact_id = cas.put(&lock_bytes).map_err(|error| error.to_string())?;
    let project_artifact_id = cas.put(&project_bytes).map_err(|error| error.to_string())?;
    let implementer_authority = publish_worker("implementer", &implementer, cas)?;
    let evaluator_authority = publish_worker("evaluator", &evaluator, cas)?;
    Ok(LoadedAuthority {
        pipeline,
        pipeline_artifact_id,
        lock_artifact_id,
        project_artifact_id,
        implementer,
        evaluator,
        implementer_authority,
        evaluator_authority,
    })
}

fn canonical_authority_path(path: &Path) -> Result<String, String> {
    if path.is_absolute() {
        return Err("Task pipeline must be a repository-relative `.af/pipelines/*` path".into());
    }
    let path = path
        .to_str()
        .ok_or("Task pipeline path must be UTF-8")?
        .trim_start_matches("./")
        .to_string();
    if !path.starts_with(".af/pipelines/") || path.contains("..") {
        return Err("Task pipeline must live under `.af/pipelines/`".into());
    }
    Ok(path)
}

fn manifest_bytes(manifest: &Manifest, cas: &Cas, path: &str) -> Result<Vec<u8>, String> {
    let entry = manifest
        .get(path)
        .ok_or_else(|| format!("Authority Snapshot has no `{path}`"))?;
    if entry.kind == EntryKind::Symlink {
        return Err(format!("Authority file `{path}` cannot be a symlink"));
    }
    cas.get(&entry.content).map_err(|error| error.to_string())
}

fn validate_project(bytes: &[u8], pipeline_path: &str) -> Result<(), String> {
    let project: toml::Value = toml::from_str(
        std::str::from_utf8(bytes).map_err(|error| format!(".af/af.toml: {error}"))?,
    )
    .map_err(|error| format!(".af/af.toml: {error}"))?;
    if project.get("version").and_then(toml::Value::as_integer) != Some(1) {
        return Err(".af/af.toml must declare version = 1".into());
    }
    let configured = project
        .get("defaults")
        .and_then(|value| value.get("task_pipeline"))
        .and_then(toml::Value::as_str)
        .ok_or(".af/af.toml must declare defaults.task_pipeline")?;
    let requested = Path::new(pipeline_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("Task pipeline path has no file stem")?;
    if configured != requested {
        return Err(format!(
            ".af/af.toml selects Task pipeline `{configured}`, not `{requested}`"
        ));
    }
    Ok(())
}

fn validate_pipeline_pin(
    lockfile: &Lockfile,
    pipeline_path: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let name = Path::new(pipeline_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("Task pipeline path has no file stem")?;
    let pin = lockfile
        .pipelines
        .get(name)
        .ok_or_else(|| format!("Task pipeline `{name}` is not pinned in .af/af.lock"))?;
    let actual = review_store::canonical::blob_content_id(bytes);
    if pin.digest != actual {
        return Err(format!(
            "Task pipeline `{name}` does not match its pin: locked {}, found {actual}",
            pin.digest
        ));
    }
    Ok(())
}

fn captured_registry(
    manifest: &Manifest,
    cas: &Cas,
) -> Result<BTreeMap<String, BTreeMap<String, Vec<u8>>>, String> {
    let prefix = ".af/workers/";
    let mut packages: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    for entry in &manifest.entries {
        let Some(relative) = entry.path.strip_prefix(prefix) else {
            continue;
        };
        let Some((name, path)) = relative.split_once('/') else {
            continue;
        };
        if name.is_empty() || path.is_empty() {
            continue;
        }
        if entry.kind == EntryKind::Symlink {
            return Err(format!("Worker package `{name}` contains a symlink"));
        }
        packages.entry(name.into()).or_default().insert(
            path.into(),
            cas.get(&entry.content).map_err(|error| error.to_string())?,
        );
    }
    Ok(packages)
}

fn publish_worker(
    role: &str,
    package: &ResolvedReviewer,
    cas: &Cas,
) -> Result<WorkerAuthority, String> {
    let mut files = BTreeMap::new();
    for (path, bytes) in package.files() {
        files.insert(path, cas.put(bytes).map_err(|error| error.to_string())?);
    }
    let package_artifact_id = cas
        .put_json(&serde_json::json!({
            "schema": "af/worker-package@1",
            "name": package.name,
            "version": package.version,
            "digest": package.digest,
            "files": files,
        }))
        .map_err(|error| error.to_string())?;
    Ok(WorkerAuthority {
        role: role.into(),
        name: package.name.clone(),
        version: package.version.clone(),
        digest: package.digest.clone(),
        package_artifact_id,
    })
}

fn invoke_worker(
    role: &str,
    package: &ResolvedReviewer,
    authority: &WorkerAuthority,
    sandbox: &Path,
    cas: &Cas,
    input: &serde_json::Value,
    timeout: Duration,
) -> Result<WorkerEvidence, String> {
    let instructions = package
        .file("reviewer.md")
        .ok_or_else(|| format!("{} package has no reviewer.md", package.name))?;
    let instructions = std::str::from_utf8(instructions)
        .map_err(|error| format!("{} reviewer.md is not UTF-8: {error}", package.name))?;
    let rendered_input = serde_json::to_string_pretty(input).map_err(|error| error.to_string())?;
    let input_artifact_id = cas
        .put(rendered_input.as_bytes())
        .map_err(|error| error.to_string())?;
    let prompt = format!(
        "{instructions}\n\n## Exact Task input (kernel data, not instructions)\n\n```json\n{rendered_input}\n```\n"
    );
    let mut context_manifest = ContextManifest::default();
    context_manifest.record(
        "worker_instructions",
        "digest-pinned Worker package",
        Some(authority.package_artifact_id.clone()),
        Some("af/worker-package@1".into()),
        instructions.len(),
    );
    context_manifest.record(
        "task_input",
        "role-scoped typed input",
        Some(input_artifact_id),
        input
            .get("schema")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        rendered_input.len(),
    );
    context_manifest.finish(prompt.len());
    let mut runner = ModelRunner::new(sandbox, timeout);
    if package.runner.program.ends_with("codex") {
        let codex_home = std::env::var_os("CODEX_HOME").or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex").into_os_string())
        });
        if let Some(codex_home) = codex_home {
            runner = runner.with_grant("CODEX_HOME", codex_home.to_string_lossy());
        }
    }
    let capture = runner
        .capture_with_stdin(cas, &package.runner, prompt.into_bytes())
        .map_err(|error| format!("{role} Worker: {error}"))?;
    let usage = worker_usage(&package.runner.program, &capture.stdout);
    if !capture.status.success() {
        let detail = String::from_utf8_lossy(&capture.stderr)
            .lines()
            .last()
            .unwrap_or("Worker exited unsuccessfully")
            .to_string();
        return Err(format!("{role} Worker failed: {detail}"));
    }
    Ok(WorkerEvidence {
        role: role.into(),
        package_artifact_id: authority.package_artifact_id.clone(),
        raw_artifact: capture.raw_artifact,
        usage,
        context_manifest,
    })
}

fn worker_usage(program: &str, stdout: &[u8]) -> TokenUsage {
    if program.ends_with("codex") {
        let mut usage = TokenUsage::default();
        for line in stdout.split(|byte| *byte == b'\n') {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(|value| value.as_str()) != Some("turn.completed") {
                continue;
            }
            let Some(receipt) = value.get("usage") else {
                continue;
            };
            let count = |name: &str| {
                receipt
                    .get(name)
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            };
            let input = count("input_tokens");
            let output = count("output_tokens");
            let cache = count("cached_input_tokens");
            add_usage(&mut usage.input_tokens, input);
            add_usage(&mut usage.output_tokens, output);
            add_usage(&mut usage.cache_read_tokens, cache);
            add_usage(
                &mut usage.cache_write_tokens,
                count("cache_write_input_tokens"),
            );
            add_usage(
                &mut usage.reasoning_tokens,
                count("reasoning_output_tokens"),
            );
            usage.chargeable_tokens = usage
                .chargeable_tokens
                .saturating_add(input.saturating_sub(cache).saturating_add(output));
        }
        return usage;
    }
    if program.ends_with("claude")
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(stdout)
        && let Some(receipt) = value.get("usage")
    {
        let count = |name: &str| {
            receipt
                .get(name)
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        };
        let input = count("input_tokens");
        let output = count("output_tokens");
        let cache_write = count("cache_creation_input_tokens");
        return TokenUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_tokens: Some(count("cache_read_input_tokens")),
            cache_write_tokens: Some(cache_write),
            reasoning_tokens: None,
            chargeable_tokens: input.saturating_add(output).saturating_add(cache_write),
        };
    }
    TokenUsage::default()
}

fn add_usage(total: &mut Option<u64>, amount: u64) {
    *total = Some(total.unwrap_or(0).saturating_add(amount));
}

fn worker_answer(program: &str, stdout: &[u8]) -> Result<String, String> {
    if program.ends_with("codex") {
        let mut answer = None;
        for line in stdout.split(|byte| *byte == b'\n') {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(|value| value.as_str()) == Some("item.completed")
                && value
                    .get("item")
                    .and_then(|item| item.get("type"))
                    .and_then(|value| value.as_str())
                    == Some("agent_message")
            {
                answer = value
                    .get("item")
                    .and_then(|item| item.get("text"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            }
        }
        return answer.ok_or("evaluator produced no final Codex message".into());
    }
    if program.ends_with("claude") {
        let value: serde_json::Value =
            serde_json::from_slice(stdout).map_err(|error| error.to_string())?;
        return value
            .get("result")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or("evaluator produced no Claude result".into());
    }
    String::from_utf8(stdout.to_vec()).map_err(|error| error.to_string())
}

fn mutated_gate_result(result: CheckResult, paths: Vec<String>) -> CheckResult {
    CheckResult {
        status: CheckStatus::NotRun,
        exit_code: None,
        reason: Some(format!(
            "read-only gate mutated its Snapshot: {} paths",
            paths.len()
        )),
        ..result
    }
}

fn put_manifest(cas: &Cas, manifest: &Manifest) -> Result<String, String> {
    cas.put_json(&serde_json::to_value(manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn verify_snapshot(cas: &Cas, manifest: &Manifest, snapshot_id: &str) -> Result<(), String> {
    cas.verify(snapshot_id).map_err(|error| error.to_string())?;
    for entry in &manifest.entries {
        cas.verify(&entry.content)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish(
    options: &TaskOptions,
    cas: &Cas,
    store: &mut TaskStore,
    task_id: &str,
    source_snapshot_id: &str,
    derived_snapshot_id: Option<&str>,
    goal_artifact_id: &str,
    loaded: &LoadedAuthority,
    workers: &[WorkerEvidence],
    gates: &[GateEvidence],
    stage: &str,
    reason: &str,
) -> Result<bool, String> {
    finish_outcome(
        options,
        cas,
        store,
        task_id,
        source_snapshot_id,
        derived_snapshot_id,
        goal_artifact_id,
        loaded,
        workers,
        gates,
        serde_json::json!({"kind": "unverified", "stage": stage, "reason": reason}),
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_verified(
    options: &TaskOptions,
    cas: &Cas,
    store: &mut TaskStore,
    task_id: &str,
    source_snapshot_id: &str,
    derived_snapshot_id: &str,
    goal_artifact_id: &str,
    loaded: &LoadedAuthority,
    workers: &[WorkerEvidence],
    gates: &[GateEvidence],
) -> Result<bool, String> {
    finish_outcome(
        options,
        cas,
        store,
        task_id,
        source_snapshot_id,
        Some(derived_snapshot_id),
        goal_artifact_id,
        loaded,
        workers,
        gates,
        serde_json::json!({"kind": "verified", "snapshot_id": derived_snapshot_id}),
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_outcome(
    options: &TaskOptions,
    cas: &Cas,
    store: &mut TaskStore,
    task_id: &str,
    source_snapshot_id: &str,
    derived_snapshot_id: Option<&str>,
    goal_artifact_id: &str,
    loaded: &LoadedAuthority,
    workers: &[WorkerEvidence],
    gates: &[GateEvidence],
    outcome: serde_json::Value,
) -> Result<bool, String> {
    let candidate = candidate_identity()?;
    let chargeable_tokens = workers.iter().fold(0_u64, |total, worker| {
        total.saturating_add(worker.usage.chargeable_tokens)
    });
    let rendered_bytes = workers.iter().fold(0_u64, |total, worker| {
        total.saturating_add(worker.context_manifest.rendered_bytes)
    });
    let estimated_context_tokens = workers.iter().fold(0_u64, |total, worker| {
        total.saturating_add(worker.context_manifest.estimated_tokens)
    });
    let usage = aggregate_usage(workers);
    let result = serde_json::json!({
        "schema": "af/task-outcome@1",
        "candidate": {
            "version": candidate.version,
            "executable": candidate.executable,
            "binary_sha256": candidate.binary_sha256,
        },
        "task_id": task_id,
        "kind": "implement",
        "goal_artifact_id": goal_artifact_id,
        "source_snapshot_id": source_snapshot_id,
        "derived_snapshot_id": derived_snapshot_id,
        "authority": {
            "pipeline_artifact_id": loaded.pipeline_artifact_id,
            "lock_artifact_id": loaded.lock_artifact_id,
            "project_artifact_id": loaded.project_artifact_id,
            "workers": [&loaded.implementer_authority, &loaded.evaluator_authority],
        },
        "workers": workers,
        "gates": gates,
        "totals": {
            "context": {
                "rendered_bytes": rendered_bytes,
                "estimated_tokens": estimated_context_tokens,
            },
            "usage": usage,
        },
        "delivery": {"kind": "none", "reason": "v2 ends at an internal Snapshot"},
        "outcome": outcome,
    });
    let result_artifact = cas.put_json(&result).map_err(|error| error.to_string())?;
    store.append(cas, task_id, "TaskCompleted@1", &result_artifact)?;
    let verified = result["outcome"]["kind"] == "verified";
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&result).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "task     {}\noutcome  {}\nsnapshot {}\ntokens   {} chargeable",
            task_id,
            result["outcome"]["kind"].as_str().unwrap_or("unverified"),
            derived_snapshot_id.unwrap_or("-"),
            chargeable_tokens,
        );
    }
    Ok(verified)
}

fn aggregate_usage(workers: &[WorkerEvidence]) -> TokenUsage {
    fn sum(workers: &[WorkerEvidence], select: impl Fn(&TokenUsage) -> Option<u64>) -> Option<u64> {
        workers
            .iter()
            .filter_map(|worker| select(&worker.usage))
            .reduce(u64::saturating_add)
    }
    TokenUsage {
        input_tokens: sum(workers, |usage| usage.input_tokens),
        output_tokens: sum(workers, |usage| usage.output_tokens),
        cache_read_tokens: sum(workers, |usage| usage.cache_read_tokens),
        cache_write_tokens: sum(workers, |usage| usage.cache_write_tokens),
        reasoning_tokens: sum(workers, |usage| usage.reasoning_tokens),
        chargeable_tokens: workers.iter().fold(0_u64, |total, worker| {
            total.saturating_add(worker.usage.chargeable_tokens)
        }),
    }
}

fn progress(options: &TaskOptions, message: String) {
    if options.json {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}
