//! Local Tasks: v2 seals one independently verified internal Snapshot; the first v3 slice may
//! deliver that exact result only to a new local branch and linked worktree.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_check::CheckDefinition;
use review_config::lock::{Lockfile, Registry};
use review_core::{Arg, Command, SubjectKind};
use review_process::{ExitPolicy, SupervisedOutput, run_supervised_with_policy};
use review_runner::ResolvedReviewer;
use review_source_git::{
    Capture, Entry, EntryKind, Manifest, PathEncoding, Repo, decode_path, digest_bytes,
};
use review_store::Cas;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{normalize_absolute, resolve_filesystem_path, xdg_state_root};
mod delivery_common;
use delivery_common::DeliveryJournal;

#[derive(Debug, Clone)]
pub(crate) struct TaskOptions {
    pub(crate) repo: PathBuf,
    pub(crate) pipeline: PathBuf,
    pub(crate) state: Option<PathBuf>,
    pub(crate) goal: String,
    pub(crate) authority: String,
    pub(crate) uncommitted: bool,
    pub(crate) timeout: Option<Duration>,
    pub(crate) json: bool,
}

#[derive(Debug, Clone)]
pub(super) struct DeliveryOptions {
    repo: PathBuf,
    state: Option<PathBuf>,
    task_id: String,
    branch: String,
    worktree: PathBuf,
    json: bool,
}

#[derive(Debug, Clone)]
pub(super) struct InspectOptions {
    repo: PathBuf,
    state: Option<PathBuf>,
    task_id: Option<String>,
    json: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn options_from_cli(
    goal: String,
    repo: PathBuf,
    pipeline: PathBuf,
    state: Option<PathBuf>,
    authority: String,
    uncommitted: bool,
    timeout_secs: Option<u64>,
    json: bool,
) -> Result<TaskOptions, String> {
    if goal.trim().is_empty() {
        return Err("an implement Task requires a non-empty --goal".into());
    }
    if uncommitted && authority != "HEAD" {
        return Err("--uncommitted and an explicit --authority cannot be combined".into());
    }
    Ok(TaskOptions {
        repo,
        pipeline,
        state,
        goal,
        authority,
        uncommitted,
        timeout: timeout_secs.map(Duration::from_secs),
        json,
    })
}

pub(super) fn delivery_from_cli(
    task_id: String,
    repo: PathBuf,
    branch: String,
    worktree: PathBuf,
    confirm: String,
    state: Option<PathBuf>,
    json: bool,
) -> Result<DeliveryOptions, String> {
    validate_task_id(&task_id)?;
    if branch.is_empty() || worktree.as_os_str().is_empty() {
        return Err("task deliver requires --branch and --worktree".into());
    }
    if confirm != task_id {
        return Err("--confirm must exactly equal the Task ID".into());
    }
    Ok(DeliveryOptions {
        repo,
        state,
        task_id,
        branch,
        worktree,
        json,
    })
}

pub(super) fn inspect_from_cli(
    task_id: Option<String>,
    repo: PathBuf,
    state: Option<PathBuf>,
    json: bool,
) -> Result<InspectOptions, String> {
    if let Some(task_id) = &task_id {
        validate_task_id(task_id)?;
    }
    Ok(InspectOptions {
        repo,
        state,
        task_id,
        json,
    })
}

fn validate_task_id(task_id: &str) -> Result<(), String> {
    if review_core::task::is_name(task_id) {
        return Ok(());
    }
    let digest = task_id
        .strip_prefix("task-")
        .ok_or("Task ID must start with `task-`")?;
    if digest.len() != 20
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("Task ID must contain exactly 20 lowercase hexadecimal digits".into());
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskPipeline {
    pub(crate) version: u32,
    pub(crate) kind: String,
    pub(crate) implementer: String,
    pub(crate) evaluator: String,
    #[serde(default = "default_timeout_seconds")]
    pub(crate) timeout_seconds: u64,
    #[serde(default = "default_check_timeout_seconds")]
    pub(crate) check_timeout_seconds: u64,
    pub(crate) attempt_tokens: u64,
    pub(crate) run_tokens: u64,
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

    pub(crate) fn check_definitions(&self) -> Vec<CheckDefinition> {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotReceipt {
    schema: String,
    kind: String,
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
pub(crate) struct WorkerAuthority {
    pub(crate) role: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) digest: String,
    pub(crate) package_artifact_id: String,
}

struct TaskStore {
    connection: Connection,
}

#[derive(Debug, Clone, Serialize)]
struct TaskEvent {
    sequence: u64,
    event_type: String,
    artifact_id: String,
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

    fn events(&self, task_id: &str) -> Result<Vec<TaskEvent>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT sequence, event_type, artifact_id
                 FROM task_events WHERE task_id = ?1 ORDER BY sequence",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([task_id], |row| {
                Ok(TaskEvent {
                    sequence: row.get(0)?,
                    event_type: row.get(1)?,
                    artifact_id: row.get(2)?,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn task_ids(&self) -> Result<Vec<String>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT task_id FROM task_events
                 GROUP BY task_id ORDER BY MIN(rowid) DESC, task_id",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryTarget {
    repository: String,
    repository_id: String,
    branch: String,
    worktree: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryPrepared {
    schema: String,
    delivery_id: String,
    task_id: String,
    /// New common-Task deliveries bind one exact result. Absent in historical receipts.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    result_id: Option<String>,
    source_snapshot_id: String,
    derived_snapshot_id: String,
    source_revision: String,
    target: DeliveryTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryReceipt {
    schema: String,
    delivery_id: String,
    task_id: String,
    /// New common-Task deliveries bind one exact result. Absent in historical receipts.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    result_id: Option<String>,
    source_snapshot_id: String,
    derived_snapshot_id: String,
    target: DeliveryTarget,
    outcome: DeliveryOutcome,
    /// Losslessly encoded Snapshot paths that the operator's ordinary `git add` will ignore in
    /// the delivered worktree. Old pilot receipts predate this advisory field and deserialize as
    /// an empty set.
    #[serde(default)]
    ignored_paths: Vec<String>,
    remote_actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DeliveryOutcome {
    Delivered,
    Failed { reason: String },
}

struct DeliveryAssets {
    source_snapshot_id: String,
    source: SnapshotReceipt,
    source_manifest: Manifest,
    derived_snapshot_id: String,
    derived: SnapshotReceipt,
    derived_manifest: Manifest,
}

const DELIVERY_GIT_TIMEOUT: Duration = Duration::from_secs(30);

struct DeliveryGit {
    directory: PathBuf,
    home: PathBuf,
}

struct DeliveryLock {
    _connection: Connection,
}

impl DeliveryLock {
    fn acquire(state: &Path) -> Result<Self, String> {
        let connection = Connection::open(state.join("task-delivery-lock.sqlite"))
            .map_err(|error| error.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE IF NOT EXISTS delivery_lock (singleton INTEGER PRIMARY KEY);
                 BEGIN IMMEDIATE;",
            )
            .map_err(|error| format!("another Task delivery is active: {error}"))?;
        Ok(Self {
            _connection: connection,
        })
    }
}

impl DeliveryGit {
    fn new(directory: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            home: home.into(),
        }
    }

    fn command(&self) -> ProcessCommand {
        let mut command = ProcessCommand::new("git");
        command.env_clear();
        command
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .current_dir(&self.directory)
            .args([
                "--no-optional-locks",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "core.sparseCheckout=false",
                "-c",
                "core.sparseCheckoutCone=false",
                "-c",
                "diff.external=",
                "-c",
                "protocol.allow=never",
                "-c",
                "credential.helper=",
            ]);
        command
    }

    fn run<I, S>(&self, args: I, input: Option<Vec<u8>>) -> Result<SupervisedOutput, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect();
        let rendered = args
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = self.command();
        command.args(&args);
        run_supervised_with_policy(
            &mut command,
            input,
            DELIVERY_GIT_TIMEOUT,
            ExitPolicy::KillProcessGroup,
        )
        .map_err(|error| format!("git {rendered}: {error}"))
    }

    fn require<I, S>(&self, args: I, input: Option<Vec<u8>>) -> Result<Vec<u8>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args, input)?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(output.stdout)
    }

    fn check_ignore(&self, input: Vec<u8>) -> Result<SupervisedOutput, String> {
        let mut command = self.command();
        command.env_remove("GIT_CONFIG_GLOBAL");
        command.env_remove("XDG_CONFIG_HOME");
        if let Some(home) = std::env::var_os("HOME") {
            command.env("HOME", home);
        } else {
            command.env_remove("HOME");
        }
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            command.env("XDG_CONFIG_HOME", xdg);
        }
        if let Some(global) = std::env::var_os("GIT_CONFIG_GLOBAL") {
            command.env("GIT_CONFIG_GLOBAL", global);
        }
        command.args(["check-ignore", "-z", "--stdin"]);
        run_supervised_with_policy(
            &mut command,
            Some(input),
            DELIVERY_GIT_TIMEOUT,
            ExitPolicy::KillProcessGroup,
        )
        .map_err(|error| format!("git check-ignore -z --stdin: {error}"))
    }

    fn ref_oid(&self, reference: &str) -> Result<Option<String>, String> {
        let commit = format!("{reference}^{{commit}}");
        let output = self.run(["rev-parse", "--verify", "--quiet", &commit], None)?;
        if output.status.success() {
            let oid = String::from_utf8(output.stdout)
                .map_err(|_| format!("Git returned a non-UTF-8 object ID for {reference}"))?;
            return Ok(Some(oid.trim().to_string()));
        }
        // `rev-parse --verify --quiet` uses exit 1 to mean that the ref does not
        // resolve. Git may still print a platform warning on stderr (for example
        // macOS falling back from DARWIN_USER_TEMP_DIR), which does not turn the
        // missing ref into an infrastructure failure.
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        Err(format!(
            "reading Git ref {reference}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub(super) fn deliver(options: DeliveryOptions) -> Result<(), String> {
    let repository = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let state = resolve_task_state(&options.state, &repository)?;
    if state.starts_with(&repository) {
        return Err("Task state must be outside the repository".into());
    }
    if !state.join("tasks.sqlite").is_file() && !state.join("events.sqlite").is_file() {
        return Err(format!("Task state {} does not exist", state.display()));
    }
    // A separate SQLite write transaction is an OS-released process lock. Prepared delivery
    // events remain durable in the Task database while an exact concurrent command is refused;
    // a crash releases this lock and leaves the prepared event available for reconciliation.
    let _delivery_lock = DeliveryLock::acquire(&state)?;
    let worktree = resolve_delivery_path(&options.worktree, &repository, &state)?;
    let repository_text = utf8_path(&repository, "repository")?;
    let worktree_text = utf8_path(&worktree, "worktree")?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let common = delivery_common::projection(&state, &cas, &options.task_id)?;
    let (mut store, events, assets): (Box<dyn DeliveryJournal>, _, _) = if let Some(task) = &common
    {
        let events = delivery_common::events(task);
        let assets = delivery_common::assets(&cas, task)?;
        (
            Box::new(delivery_common::CommonDelivery::open(&state, task)?),
            events,
            assets,
        )
    } else {
        if !state.join("tasks.sqlite").is_file() {
            return Err("Unknown Task".into());
        }
        let store = TaskStore::open(&state.join("tasks.sqlite"))?;
        let events = store.events(&options.task_id)?;
        let assets = load_delivery_assets(&cas, &events)?;
        (Box::new(store), events, assets)
    };
    let source_revision = assets
        .source
        .source_revision
        .clone()
        .ok_or("v3a delivery requires a Task captured from a committed source")?;
    let target = DeliveryTarget {
        repository: repository_text,
        repository_id: assets.source.repository_id.clone(),
        branch: options.branch.clone(),
        worktree: worktree_text,
    };
    let result_id = if let Some(task) = &common {
        // Resume the captured ownership scheme for an existing delivery of this result.
        if let Some(event) = events.last() {
            cas.get_json(&event.artifact_id)
                .map_err(|e| e.to_string())?
                .get("result_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        } else if let review_core::task::TaskPhaseV1::Finished { result_id } = &task.phase {
            Some(result_id.clone())
        } else {
            return Err("Task has no finished result".into());
        }
    } else {
        None
    };
    let prepared = DeliveryPrepared {
        schema: "af/task-delivery-prepared@1".into(),
        delivery_id: delivery_id(
            &options.task_id,
            &assets.source_snapshot_id,
            &assets.derived_snapshot_id,
            &target,
            result_id.as_deref(),
        )?,
        task_id: options.task_id.clone(),
        result_id,
        source_snapshot_id: assets.source_snapshot_id.clone(),
        derived_snapshot_id: assets.derived_snapshot_id.clone(),
        source_revision,
        target,
    };
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let git = DeliveryGit::new(&repository, &git_home);
    validate_branch(&git, &prepared.target.branch)?;

    if let Some(receipt) = latest_delivery_receipt(&cas, &events, "TaskDelivered@1")? {
        ensure_same_delivery(&prepared, &receipt)?;
        verify_sealed_delivery_identity(&git, &git_home, &prepared)?;
        print_delivery(&options, &receipt)?;
        return Ok(());
    }

    let unresolved = latest_delivery_transition(&cas, &events)?;
    if let Some(existing) = unresolved {
        ensure_same_preparation(&prepared, &existing)?;
        match verify_existing_delivery(&git, &git_home, &assets, &existing) {
            Ok(()) => {
                let ignored_paths = ignored_delivery_paths(
                    &DeliveryGit::new(&existing.target.worktree, &git_home),
                    &assets.derived_manifest,
                )?;
                let receipt = delivered_receipt(&existing, ignored_paths);
                append_delivery_receipt(store.as_mut(), &cas, &receipt, "TaskDelivered@1")?;
                print_delivery(&options, &receipt)?;
                return Ok(());
            }
            Err(verification) => match rollback_owned_delivery(
                &git,
                &existing,
                &assets.derived_manifest,
                RollbackContext::Recovery,
            ) {
                Ok(()) => {
                    let receipt = failed_receipt(
                        &existing,
                        &format!(
                            "Incomplete delivery was rolled back before retry: {verification}"
                        ),
                    );
                    append_delivery_receipt(
                        store.as_mut(),
                        &cas,
                        &receipt,
                        "TaskDeliveryFailed@1",
                    )?;
                }
                Err(rollback) => {
                    let reason = format!(
                        "delivery recovery preserved the unsealed branch/worktree because it could not prove the content was delivery-owned: verification failed: {verification}; rollback refused: {rollback}"
                    );
                    let receipt = failed_receipt(&existing, &reason);
                    append_delivery_receipt(
                        store.as_mut(),
                        &cas,
                        &receipt,
                        "TaskDeliveryFailed@1",
                    )?;
                    return Err(format!(
                        "delivery recovery stopped and preserved branch `{}` at `{}`; retry this Task with a new absent branch and worktree (or remove the preserved target with normal Git controls before reusing it): {rollback}",
                        existing.target.branch, existing.target.worktree
                    ));
                }
            },
        }
    } else if let Some(failed) = latest_terminal_delivery_failure(&cas, &events)? {
        release_failed_delivery_owner(&git, &prepared, &failed)?;
    }

    verify_source_authority(&repository, &git_home, &git, &cas, &assets)?;
    ensure_delivery_target_absent(&git, &prepared)?;
    let prepared_artifact = cas
        .put_json(&serde_json::to_value(&prepared).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    store.append(
        &cas,
        &options.task_id,
        "TaskDeliveryPrepared@1",
        &prepared_artifact,
    )?;

    match execute_delivery(&git, &git_home, &cas, &assets, &prepared) {
        Ok(ignored_paths) => {
            let receipt = delivered_receipt(&prepared, ignored_paths);
            append_delivery_receipt(store.as_mut(), &cas, &receipt, "TaskDelivered@1")?;
            print_delivery(&options, &receipt)
        }
        Err(reason) => match rollback_owned_delivery(
            &git,
            &prepared,
            &assets.derived_manifest,
            RollbackContext::CurrentAttempt,
        ) {
            Ok(()) => {
                let receipt = failed_receipt(&prepared, &reason);
                append_delivery_receipt(store.as_mut(), &cas, &receipt, "TaskDeliveryFailed@1")?;
                Err(format!("delivery failed and was rolled back: {reason}"))
            }
            Err(rollback) => Err(format!(
                "delivery failed: {reason}; rollback is incomplete: {rollback}; rerun the exact command to recover"
            )),
        },
    }
}

pub(super) fn list(options: InspectOptions) -> Result<(), String> {
    let repository = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let state = resolve_task_state(&options.state, &repository)?;
    let mut tasks = super::task_execution::list_common(&state)?;
    if !state.join("tasks.sqlite").is_file() {
        return print_task_list(&options, tasks);
    }
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let store = TaskStore::open(&state.join("tasks.sqlite"))?;
    for task_id in store.task_ids()? {
        if tasks.iter().any(|task| task["task_id"] == task_id) {
            return Err("Task ID is ambiguous across common and legacy stores".into());
        }
        let events = store.events(&task_id)?;
        let outcome = latest_event_json(&cas, &events, "TaskCompleted@1")?;
        let delivery = latest_delivery_value(&cas, &events)?;
        tasks.push(serde_json::json!({
            "task_id": task_id,
            "outcome": outcome.as_ref().and_then(|value| value.pointer("/outcome/kind")).cloned(),
            "derived_snapshot_id": outcome.as_ref().and_then(|value| value.get("derived_snapshot_id")).cloned(),
            "chargeable_tokens": outcome.as_ref().and_then(|value| value.pointer("/totals/usage/chargeable_tokens")).cloned(),
            "delivery": delivery,
        }));
    }
    print_task_list(&options, tasks)
}

pub(super) fn show(options: InspectOptions) -> Result<(), String> {
    let repository = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let state = resolve_task_state(&options.state, &repository)?;
    let task_id = options.task_id.as_deref().expect("validated by parser");
    if super::task_execution::show_if_common(task_id, &state, options.json)? {
        return Ok(());
    }
    if !state.join("tasks.sqlite").is_file() {
        return Err(format!("Task `{task_id}` was not found"));
    }
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let store = TaskStore::open(&state.join("tasks.sqlite"))?;
    let events = store.events(task_id)?;
    if events.is_empty() {
        return Err(format!("Task `{task_id}` was not found"));
    }
    let outcome = latest_event_json(&cas, &events, "TaskCompleted@1")?;
    let delivery = latest_delivery_value(&cas, &events)?;
    let view = serde_json::json!({
        "schema": "af/task-inspection@1",
        "task_id": task_id,
        "outcome": outcome,
        "delivery": delivery,
        "history": events,
    });
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&view).map_err(|error| error.to_string())?
        );
    } else {
        let outcome = view
            .pointer("/outcome/outcome/kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("incomplete");
        let snapshot = view
            .pointer("/outcome/derived_snapshot_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-");
        let tokens = view
            .pointer("/outcome/totals/usage/chargeable_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let delivery = view
            .pointer("/delivery/outcome/kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("none");
        println!(
            "task     {task_id}\noutcome  {outcome}\nsnapshot {snapshot}\ntokens   {tokens} chargeable\ndelivery {delivery}"
        );
        for event in &events {
            println!(
                "  {:>3} {:<24} {}",
                event.sequence, event.event_type, event.artifact_id
            );
        }
    }
    Ok(())
}

fn resolve_delivery_path(
    requested: &Path,
    repository: &Path,
    state: &Path,
) -> Result<PathBuf, String> {
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("reading current directory: {error}"))?
            .join(requested)
    };
    let absolute = normalize_absolute(&absolute)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&absolute)
        && metadata.file_type().is_symlink()
    {
        return Err("delivery worktree path cannot be a symlink".into());
    }
    let resolved = resolve_filesystem_path(&absolute)?;
    let parent = resolved
        .parent()
        .ok_or("delivery worktree must have a parent directory")?;
    if !parent.is_dir() {
        return Err(format!(
            "delivery worktree parent {} must already exist",
            parent.display()
        ));
    }
    if resolved == repository || resolved.starts_with(repository) {
        return Err("delivery worktree must be outside the source checkout".into());
    }
    if resolved == state || resolved.starts_with(state) {
        return Err("delivery worktree must be outside the Task state directory".into());
    }
    Ok(resolved)
}

fn utf8_path(path: &Path, kind: &str) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("delivery {kind} path must be valid UTF-8"))
}

fn load_delivery_assets(cas: &Cas, events: &[TaskEvent]) -> Result<DeliveryAssets, String> {
    let outcome = latest_event_json(cas, events, "TaskCompleted@1")?
        .ok_or("Task has no completed outcome")?;
    if outcome.get("schema").and_then(serde_json::Value::as_str) != Some("af/task-outcome@1") {
        return Err("Task completed artifact has an unsupported schema".into());
    }
    if outcome
        .pointer("/outcome/kind")
        .and_then(serde_json::Value::as_str)
        != Some("verified")
    {
        return Err("only a verified Task can be delivered".into());
    }
    let source_snapshot_id = outcome
        .get("source_snapshot_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("verified Task has no source Snapshot")?
        .to_string();
    let derived_snapshot_id = outcome
        .get("derived_snapshot_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("verified Task has no derived Snapshot")?
        .to_string();
    if outcome
        .pointer("/outcome/snapshot_id")
        .and_then(serde_json::Value::as_str)
        != Some(derived_snapshot_id.as_str())
    {
        return Err("verified Task outcome disagrees with its derived Snapshot".into());
    }
    let source = load_snapshot(cas, &source_snapshot_id)?;
    let derived = load_snapshot(cas, &derived_snapshot_id)?;
    if source.schema != "af/snapshot@1" || source.kind != "source" {
        return Err("Task source Snapshot has an unsupported contract".into());
    }
    if derived.schema != "af/snapshot@1"
        || derived.kind != "derived"
        || derived.parent_snapshot_id.as_deref() != Some(source_snapshot_id.as_str())
        || derived.repository_id != source.repository_id
    {
        return Err("Task derived Snapshot is not exactly parented by its source".into());
    }
    if source.parent_snapshot_id.is_some() || source.source_revision.is_none() {
        return Err("v3a delivery requires a committed source Snapshot".into());
    }
    let source_manifest = load_manifest(cas, &source)?;
    let derived_manifest = load_manifest(cas, &derived)?;
    verify_snapshot(cas, &source_manifest, &source_snapshot_id)?;
    verify_snapshot(cas, &derived_manifest, &derived_snapshot_id)?;
    if source.content_digest != source_manifest.content_digest()
        || derived.content_digest != derived_manifest.content_digest()
    {
        return Err("Snapshot receipt content digest disagrees with its Manifest".into());
    }
    ensure_no_git_admin_paths(&derived_manifest)?;
    Ok(DeliveryAssets {
        source_snapshot_id,
        source,
        source_manifest,
        derived_snapshot_id,
        derived,
        derived_manifest,
    })
}

fn load_snapshot(cas: &Cas, snapshot_id: &str) -> Result<SnapshotReceipt, String> {
    serde_json::from_value(
        cas.get_json(snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("reading Snapshot {snapshot_id}: {error}"))
}

fn load_manifest(cas: &Cas, snapshot: &SnapshotReceipt) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_value(
        cas.get_json(&snapshot.manifest_artifact_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("reading Snapshot Manifest: {error}"))?;
    manifest.validate().map_err(|error| error.to_string())?;
    Ok(manifest)
}

fn ensure_no_git_admin_paths(manifest: &Manifest) -> Result<(), String> {
    for entry in &manifest.entries {
        let decoded = decode_path(&entry.path);
        let first = decoded
            .split(|byte| *byte == b'/')
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".git") {
            return Err(format!(
                "derived Snapshot contains reserved Git administration path `{}`",
                entry.path
            ));
        }
    }
    Ok(())
}

fn delivery_id(
    task_id: &str,
    source_snapshot_id: &str,
    derived_snapshot_id: &str,
    target: &DeliveryTarget,
    result_id: Option<&str>,
) -> Result<String, String> {
    let mut value = serde_json::json!({
        "task_id": task_id,
        "source_snapshot_id": source_snapshot_id,
        "derived_snapshot_id": derived_snapshot_id,
        "target": target,
    });
    if let Some(id) = result_id {
        value["result_id"] = serde_json::json!(id);
    }
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    Ok(format!("delivery-{:x}", Sha256::digest(bytes)))
}

fn owner_ref(prepared: &DeliveryPrepared) -> Result<String, String> {
    match &prepared.result_id {
        Some(id) if review_core::is_digest(id) => Ok(format!(
            "refs/afactory/task-deliveries/{}/{}",
            prepared.task_id,
            &id[7..]
        )),
        Some(_) => Err("Delivery has an invalid result identity".into()),
        None => Ok(format!("refs/afactory/deliveries/{}", prepared.task_id)),
    }
}

fn branch_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

fn validate_branch(git: &DeliveryGit, branch: &str) -> Result<(), String> {
    if branch.starts_with('-') || branch.as_bytes().contains(&0) {
        return Err("delivery branch is not a safe local branch name".into());
    }
    let output = git.run(["check-ref-format", "--branch", branch], None)?;
    if !output.status.success() {
        return Err(format!(
            "invalid delivery branch `{branch}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn latest_event_json(
    cas: &Cas,
    events: &[TaskEvent],
    event_type: &str,
) -> Result<Option<serde_json::Value>, String> {
    events
        .iter()
        .rev()
        .find(|event| event.event_type == event_type)
        .map(|event| {
            cas.get_json(&event.artifact_id)
                .map_err(|error| error.to_string())
        })
        .transpose()
}

fn latest_delivery_receipt(
    cas: &Cas,
    events: &[TaskEvent],
    event_type: &str,
) -> Result<Option<DeliveryReceipt>, String> {
    latest_event_json(cas, events, event_type)?
        .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
        .transpose()
}

fn latest_delivery_transition(
    cas: &Cas,
    events: &[TaskEvent],
) -> Result<Option<DeliveryPrepared>, String> {
    let latest = events.iter().rev().find(|event| {
        matches!(
            event.event_type.as_str(),
            "TaskDeliveryPrepared@1" | "TaskDelivered@1" | "TaskDeliveryFailed@1"
        )
    });
    let Some(event) = latest else {
        return Ok(None);
    };
    if event.event_type != "TaskDeliveryPrepared@1" {
        return Ok(None);
    }
    serde_json::from_value(
        cas.get_json(&event.artifact_id)
            .map_err(|error| error.to_string())?,
    )
    .map(Some)
    .map_err(|error| error.to_string())
}

fn latest_terminal_delivery_failure(
    cas: &Cas,
    events: &[TaskEvent],
) -> Result<Option<DeliveryReceipt>, String> {
    let latest = events.iter().rev().find(|event| {
        matches!(
            event.event_type.as_str(),
            "TaskDeliveryPrepared@1" | "TaskDelivered@1" | "TaskDeliveryFailed@1"
        )
    });
    let Some(event) = latest.filter(|event| event.event_type == "TaskDeliveryFailed@1") else {
        return Ok(None);
    };
    let receipt: DeliveryReceipt = serde_json::from_value(
        cas.get_json(&event.artifact_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if !matches!(&receipt.outcome, DeliveryOutcome::Failed { .. }) {
        return Err("TaskDeliveryFailed@1 does not carry a failed delivery receipt".into());
    }
    Ok(Some(receipt))
}

fn release_failed_delivery_owner(
    git: &DeliveryGit,
    requested: &DeliveryPrepared,
    failed: &DeliveryReceipt,
) -> Result<(), String> {
    if failed.task_id != requested.task_id
        || failed.result_id != requested.result_id
        || failed.source_snapshot_id != requested.source_snapshot_id
        || failed.derived_snapshot_id != requested.derived_snapshot_id
        || failed.target.repository != requested.target.repository
        || failed.target.repository_id != requested.target.repository_id
    {
        return Err("terminal failed delivery disagrees with the requested Task authority".into());
    }
    let reference = owner_ref(requested)?;
    let Some(oid) = git.ref_oid(&reference)? else {
        return Ok(());
    };
    if oid != requested.source_revision {
        return Err("failed delivery ownership ref moved; refusing to release it".into());
    }
    git.require(["update-ref", "-d", &reference, &oid], None)?;
    Ok(())
}

fn latest_delivery_value(
    cas: &Cas,
    events: &[TaskEvent],
) -> Result<Option<serde_json::Value>, String> {
    events
        .iter()
        .rev()
        .find(|event| {
            matches!(
                event.event_type.as_str(),
                "TaskDeliveryPrepared@1" | "TaskDelivered@1" | "TaskDeliveryFailed@1"
            )
        })
        .map(|event| {
            cas.get_json(&event.artifact_id)
                .map_err(|error| error.to_string())
        })
        .transpose()
}

fn ensure_same_delivery(
    requested: &DeliveryPrepared,
    existing: &DeliveryReceipt,
) -> Result<(), String> {
    if existing.schema != "af/task-delivery@1"
        || existing.delivery_id != requested.delivery_id
        || existing.task_id != requested.task_id
        || existing.result_id != requested.result_id
        || existing.source_snapshot_id != requested.source_snapshot_id
        || existing.derived_snapshot_id != requested.derived_snapshot_id
        || existing.target != requested.target
        || existing.outcome != DeliveryOutcome::Delivered
    {
        return Err("Task was already delivered to a different target".into());
    }
    Ok(())
}

fn ensure_same_preparation(
    requested: &DeliveryPrepared,
    existing: &DeliveryPrepared,
) -> Result<(), String> {
    if requested != existing {
        return Err("Task has an incomplete delivery prepared for a different target".into());
    }
    Ok(())
}

fn verify_source_authority(
    repository: &Path,
    git_home: &Path,
    git: &DeliveryGit,
    cas: &Cas,
    assets: &DeliveryAssets,
) -> Result<(), String> {
    let head = git.require(["rev-parse", "--verify", "HEAD^{commit}"], None)?;
    let head = String::from_utf8(head)
        .map_err(|_| "Git HEAD object ID is not UTF-8".to_string())?
        .trim()
        .to_string();
    if assets.source.source_revision.as_deref() != Some(head.as_str()) {
        return Err("target repository HEAD no longer equals the Task source revision".into());
    }
    let repo = Repo::open(repository, git_home);
    let committed = Capture::new(&repo, cas)
        .committed("HEAD")
        .map_err(|error| format!("capturing target HEAD: {error}"))?;
    let committed_manifest_id = put_manifest(cas, &committed.manifest)?;
    if committed.repository_id != assets.source.repository_id
        || committed.content_digest != assets.source.content_digest
        || committed.source_revision != assets.source.source_revision
        || committed_manifest_id != assets.source.manifest_artifact_id
        || committed.manifest != assets.source_manifest
    {
        return Err("target repository does not exactly match the Task source Snapshot".into());
    }
    let staged = git.run(
        [
            "diff-index",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
        None,
    )?;
    if !staged.status.success() {
        if staged.status.code() == Some(1) {
            return Err("target repository has staged changes".into());
        }
        return Err(format!(
            "checking staged changes: {}",
            String::from_utf8_lossy(&staged.stderr).trim()
        ));
    }
    let dirty = Capture::new(&repo, cas)
        .dirty()
        .map_err(|error| format!("checking target worktree: {error}"))?;
    if dirty.repository_id != assets.source.repository_id
        || dirty.content_digest != assets.source.content_digest
        || dirty.manifest != assets.source_manifest
    {
        return Err("target repository worktree is not clean at the Task source Snapshot".into());
    }
    Ok(())
}

fn ensure_delivery_target_absent(
    git: &DeliveryGit,
    prepared: &DeliveryPrepared,
) -> Result<(), String> {
    if Path::new(&prepared.target.worktree).exists() {
        return Err("delivery worktree path already exists".into());
    }
    if git.ref_oid(&branch_ref(&prepared.target.branch))?.is_some() {
        return Err("delivery branch already exists".into());
    }
    if git.ref_oid(&owner_ref(prepared)?)?.is_some() {
        return Err("Task already owns an unresolved local delivery ref".into());
    }
    Ok(())
}

fn execute_delivery(
    git: &DeliveryGit,
    git_home: &Path,
    cas: &Cas,
    assets: &DeliveryAssets,
    prepared: &DeliveryPrepared,
) -> Result<Vec<String>, String> {
    create_delivery_refs(git, prepared)?;
    git.require(
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-checkout"),
            OsStr::new("--"),
            OsStr::new(&prepared.target.worktree),
            OsStr::new(&prepared.target.branch),
        ],
        None,
    )?;
    let worktree = Path::new(&prepared.target.worktree);
    ensure_empty_linked_worktree(worktree)?;
    // `--no-checkout` prevents candidate-controlled filters from executing, but also starts with
    // an empty per-worktree index. Populate that index from the immutable source tree through
    // plumbing only, without touching the still-empty worktree.
    DeliveryGit::new(worktree, git_home).require(["read-tree", "HEAD"], None)?;
    review_source_git::materialize(&assets.derived_manifest, cas, worktree)
        .map_err(|error| format!("materializing verified Snapshot: {error}"))?;
    verify_existing_delivery(git, git_home, assets, prepared)?;
    ignored_delivery_paths(
        &DeliveryGit::new(&prepared.target.worktree, git_home),
        &assets.derived_manifest,
    )
}

fn create_delivery_refs(git: &DeliveryGit, prepared: &DeliveryPrepared) -> Result<(), String> {
    let input = format!(
        "start\ncreate {} {}\ncreate {} {}\nprepare\ncommit\n",
        owner_ref(prepared)?,
        prepared.source_revision,
        branch_ref(&prepared.target.branch),
        prepared.source_revision,
    )
    .into_bytes();
    git.require(["update-ref", "--stdin"], Some(input))?;
    Ok(())
}

fn ensure_empty_linked_worktree(worktree: &Path) -> Result<(), String> {
    if !worktree.join(".git").is_file() {
        return Err("Git did not create a linked-worktree administration file".into());
    }
    for entry in std::fs::read_dir(worktree).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name() != OsStr::new(".git") {
            return Err(format!(
                "new no-checkout worktree unexpectedly contains `{}`",
                entry.file_name().to_string_lossy()
            ));
        }
    }
    Ok(())
}

fn verify_existing_delivery(
    source_git: &DeliveryGit,
    git_home: &Path,
    assets: &DeliveryAssets,
    prepared: &DeliveryPrepared,
) -> Result<(), String> {
    let git = verify_sealed_delivery_identity(source_git, git_home, prepared)?;
    if source_git
        .ref_oid(&branch_ref(&prepared.target.branch))?
        .as_deref()
        != Some(prepared.source_revision.as_str())
    {
        return Err("delivery branch ref is absent or has moved".into());
    }
    let worktree = Path::new(&prepared.target.worktree);
    let branch = git.require(["symbolic-ref", "--quiet", "--short", "HEAD"], None)?;
    if String::from_utf8(branch)
        .map_err(|_| "delivered branch is not UTF-8".to_string())?
        .trim()
        != prepared.target.branch
    {
        return Err("delivered worktree is on a different branch".into());
    }
    let head = git.require(["rev-parse", "--verify", "HEAD^{commit}"], None)?;
    if String::from_utf8(head)
        .map_err(|_| "delivered HEAD is not UTF-8".to_string())?
        .trim()
        != prepared.source_revision
    {
        return Err("delivered branch no longer points at the Task source revision".into());
    }
    if !index_matches_head(&git)? {
        return Err("delivered worktree index no longer equals the Task source tree".into());
    }
    let first = scan_delivery_manifest(worktree, assets.derived_manifest.path_encoding)?;
    let actual = scan_delivery_manifest(worktree, assets.derived_manifest.path_encoding)?;
    if first != actual
        || actual != assets.derived_manifest
        || actual.content_digest() != assets.derived.content_digest
    {
        return Err("delivered worktree bytes do not equal the verified Snapshot".into());
    }
    Ok(())
}

fn verify_sealed_delivery_identity(
    source_git: &DeliveryGit,
    git_home: &Path,
    prepared: &DeliveryPrepared,
) -> Result<DeliveryGit, String> {
    if source_git.ref_oid(&owner_ref(prepared)?)?.as_deref()
        != Some(prepared.source_revision.as_str())
    {
        return Err("delivery ownership ref is absent or has moved".into());
    }
    let worktree = Path::new(&prepared.target.worktree);
    let metadata = std::fs::symlink_metadata(worktree)
        .map_err(|error| format!("opening delivered worktree: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !worktree.join(".git").is_file() {
        return Err("delivered worktree path is not the expected linked worktree".into());
    }
    let git = DeliveryGit::new(worktree, git_home);
    if git_common_dir(&git)? != git_common_dir(source_git)? {
        return Err("delivered worktree is attached to a different Git repository".into());
    }
    let top = git.require(["rev-parse", "--show-toplevel"], None)?;
    let top = String::from_utf8(top).map_err(|_| "delivered Git root is not UTF-8".to_string())?;
    let top = std::fs::canonicalize(top.trim()).map_err(|error| error.to_string())?;
    if top != worktree {
        return Err("delivered path resolves to a different Git worktree".into());
    }
    let delivered_repo = Repo::open(worktree, git_home);
    if delivered_repo
        .repository_id()
        .map_err(|error| error.to_string())?
        != prepared.target.repository_id
    {
        return Err("delivered worktree belongs to a different repository".into());
    }
    Ok(git)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RollbackContext {
    CurrentAttempt,
    Recovery,
}

fn rollback_owned_delivery(
    git: &DeliveryGit,
    prepared: &DeliveryPrepared,
    derived_manifest: &Manifest,
    context: RollbackContext,
) -> Result<(), String> {
    let owner = git.ref_oid(&owner_ref(prepared)?)?;
    let branch = git.ref_oid(&branch_ref(&prepared.target.branch))?;
    let worktree = Path::new(&prepared.target.worktree);
    if owner.is_none() {
        if branch.is_some() || worktree.exists() {
            return Err(
                "delivery has no ownership ref; refusing to remove the branch or path".into(),
            );
        }
        return Ok(());
    }
    if owner
        .as_deref()
        .is_some_and(|oid| oid != prepared.source_revision)
        || branch
            .as_deref()
            .is_some_and(|oid| oid != prepared.source_revision)
    {
        return Err("owned delivery refs moved; refusing to remove operator work".into());
    }
    if worktree.exists() {
        if worktree.join(".git").is_file() {
            let target_git = DeliveryGit::new(worktree, &git.home);
            if git_common_dir(&target_git)? != git_common_dir(git)? {
                return Err(
                    "worktree is attached to a different repository; refusing rollback".into(),
                );
            }
            let target_branch = target_git
                .require(["symbolic-ref", "--quiet", "--short", "HEAD"], None)
                .and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string()))?;
            let target_head = target_git
                .require(["rev-parse", "--verify", "HEAD^{commit}"], None)
                .and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string()))?;
            if target_branch.trim() != prepared.target.branch
                || target_head.trim() != prepared.source_revision
                || owner.as_deref() != Some(prepared.source_revision.as_str())
            {
                return Err("worktree ownership changed; refusing rollback".into());
            }
            if !index_matches_head(&target_git)? && !index_is_empty(&target_git)? {
                return Err("delivered worktree index changed; refusing rollback".into());
            }
            let actual = scan_delivery_manifest(worktree, derived_manifest.path_encoding)?;
            let removable = actual.entries.is_empty()
                || context == RollbackContext::CurrentAttempt
                    && manifest_is_subset(&actual, derived_manifest);
            if !removable {
                return Err(
                    "delivered worktree contains content recovery cannot prove belongs to the current attempt; refusing rollback"
                        .into(),
                );
            }
            git.require(
                [
                    OsStr::new("worktree"),
                    OsStr::new("remove"),
                    OsStr::new("--force"),
                    OsStr::new("--"),
                    OsStr::new(&prepared.target.worktree),
                ],
                None,
            )?;
        } else if std::fs::read_dir(worktree)
            .map_err(|error| error.to_string())?
            .next()
            .is_none()
            && owner.as_deref() == Some(prepared.source_revision.as_str())
        {
            std::fs::remove_dir(worktree).map_err(|error| error.to_string())?;
        } else {
            return Err("delivery path is not an owned linked worktree; refusing rollback".into());
        }
    }
    let owner = git.ref_oid(&owner_ref(prepared)?)?;
    let branch = git.ref_oid(&branch_ref(&prepared.target.branch))?;
    let mut commands = String::from("start\n");
    if let Some(oid) = branch {
        commands.push_str(&format!(
            "delete {} {}\n",
            branch_ref(&prepared.target.branch),
            oid
        ));
    }
    if let Some(oid) = owner {
        commands.push_str(&format!("delete {} {}\n", owner_ref(prepared)?, oid));
    }
    commands.push_str("prepare\ncommit\n");
    git.require(["update-ref", "--stdin"], Some(commands.into_bytes()))?;
    Ok(())
}

fn manifest_is_subset(actual: &Manifest, expected: &Manifest) -> bool {
    actual.path_encoding == expected.path_encoding
        && actual.entries.iter().all(|entry| {
            expected
                .entries
                .binary_search_by(|candidate| candidate.path.as_bytes().cmp(entry.path.as_bytes()))
                .is_ok_and(|index| expected.entries[index] == *entry)
        })
}

fn index_matches_head(git: &DeliveryGit) -> Result<bool, String> {
    let output = git.run(
        [
            "diff-index",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
        None,
    )?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(format!(
        "checking delivered worktree index: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn index_is_empty(git: &DeliveryGit) -> Result<bool, String> {
    Ok(git.require(["ls-files", "-z"], None)?.is_empty())
}

fn ignored_delivery_paths(git: &DeliveryGit, manifest: &Manifest) -> Result<Vec<String>, String> {
    let mut input = Vec::new();
    let mut paths = BTreeMap::new();
    for entry in &manifest.entries {
        let raw = decode_path(&entry.path);
        input.extend_from_slice(&raw);
        input.push(0);
        paths.insert(raw, entry.path.clone());
    }
    let output = git.check_ignore(input)?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(format!(
            "classifying ignored delivered paths: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut ignored = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let path = paths.get(raw).ok_or_else(|| {
            "git check-ignore returned a path outside the verified Snapshot".to_string()
        })?;
        ignored.push(path.clone());
    }
    ignored.sort();
    ignored.dedup();
    Ok(ignored)
}

fn git_common_dir(git: &DeliveryGit) -> Result<PathBuf, String> {
    let bytes = git.require(["rev-parse", "--git-common-dir"], None)?;
    let value =
        String::from_utf8(bytes).map_err(|_| "Git common directory is not UTF-8".to_string())?;
    let path = PathBuf::from(value.trim());
    let path = if path.is_absolute() {
        path
    } else {
        git.directory.join(path)
    };
    std::fs::canonicalize(&path)
        .map_err(|error| format!("opening Git common directory {}: {error}", path.display()))
}

fn delivered_receipt(prepared: &DeliveryPrepared, ignored_paths: Vec<String>) -> DeliveryReceipt {
    DeliveryReceipt {
        schema: "af/task-delivery@1".into(),
        delivery_id: prepared.delivery_id.clone(),
        task_id: prepared.task_id.clone(),
        result_id: prepared.result_id.clone(),
        source_snapshot_id: prepared.source_snapshot_id.clone(),
        derived_snapshot_id: prepared.derived_snapshot_id.clone(),
        target: prepared.target.clone(),
        outcome: DeliveryOutcome::Delivered,
        ignored_paths,
        remote_actions: Vec::new(),
    }
}

fn failed_receipt(prepared: &DeliveryPrepared, reason: &str) -> DeliveryReceipt {
    DeliveryReceipt {
        outcome: DeliveryOutcome::Failed {
            reason: reason.to_string(),
        },
        ..delivered_receipt(prepared, Vec::new())
    }
}

fn append_delivery_receipt(
    store: &mut dyn DeliveryJournal,
    cas: &Cas,
    receipt: &DeliveryReceipt,
    event_type: &str,
) -> Result<(), String> {
    let artifact = cas
        .put_json(&serde_json::to_value(receipt).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    store.append(cas, &receipt.task_id, event_type, &artifact)
}

fn print_delivery(options: &DeliveryOptions, receipt: &DeliveryReceipt) -> Result<(), String> {
    if options.json {
        println!(
            "{}",
            serde_json::to_string(receipt).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "task     {}\ndelivery {}\nbranch   {}\nworktree {}\nremote   none",
            receipt.task_id, receipt.delivery_id, receipt.target.branch, receipt.target.worktree,
        );
        for path in &receipt.ignored_paths {
            println!("ignored  {path}");
        }
    }
    Ok(())
}

fn print_task_list(options: &InspectOptions, tasks: Vec<serde_json::Value>) -> Result<(), String> {
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "schema": "af/task-list@2",
                "tasks": tasks,
            }))
            .map_err(|error| error.to_string())?
        );
    } else if tasks.is_empty() {
        println!("no Tasks");
    } else {
        for task in tasks {
            println!(
                "{}  {:<10} {:>8} tokens  {}",
                task["task_id"].as_str().unwrap_or("-"),
                task["outcome"].as_str().unwrap_or("incomplete"),
                task["chargeable_tokens"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| task["chargeable_tokens"]
                        .as_u64()
                        .map_or_else(|| "-".into(), |tokens| tokens.to_string())),
                task.pointer("/delivery/outcome/kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("not-delivered"),
            );
        }
    }
    Ok(())
}

fn scan_delivery_manifest(root: &Path, encoding: PathEncoding) -> Result<Manifest, String> {
    let mut entries = Vec::new();
    scan_delivery_directory(root, root, encoding, &mut entries)?;
    Manifest::new_with_encoding(entries, encoding).map_err(|error| error.to_string())
}

fn scan_delivery_directory(
    root: &Path,
    directory: &Path,
    encoding: PathEncoding,
    entries: &mut Vec<Entry>,
) -> Result<(), String> {
    for child in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let child = child.map_err(|error| error.to_string())?;
        if directory == root && child.file_name() == OsStr::new(".git") {
            continue;
        }
        let path = child.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            scan_delivery_directory(root, &path, encoding, entries)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "delivered path escaped its worktree".to_string())?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("delivered path has an unsafe component".into());
        }
        let raw_path = os_path_bytes(relative)?;
        let encoded = Manifest {
            path_encoding: encoding,
            entries: Vec::new(),
        }
        .encode_key(&raw_path);
        let (kind, content, size) = if metadata.file_type().is_symlink() {
            let bytes = read_link_bytes(&path)?;
            let size = bytes.len() as u64;
            (EntryKind::Symlink, digest_bytes(&bytes), size)
        } else if metadata.is_file() {
            let kind = if is_executable(&metadata) {
                EntryKind::Executable
            } else {
                EntryKind::File
            };
            let mut file = File::open(&path).map_err(|error| error.to_string())?;
            let mut buffer = vec![0_u8; 64 * 1024];
            let (content, size) =
                review_store::canonical::blob_content_id_reader_with_buffer(&mut file, &mut buffer)
                    .map_err(|error| error.to_string())?;
            (kind, content, size)
        } else {
            return Err(format!(
                "delivered Snapshot contains unsupported filesystem entry {}",
                relative.display()
            ));
        };
        entries.push(Entry {
            path: encoded,
            kind,
            content,
            size,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn os_path_bytes(path: &Path) -> Result<Vec<u8>, String> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn os_path_bytes(path: &Path) -> Result<Vec<u8>, String> {
    path.to_str()
        .map(|path| path.as_bytes().to_vec())
        .ok_or("delivered path is not valid UTF-8".into())
}

#[cfg(unix)]
fn read_link_bytes(path: &Path) -> Result<Vec<u8>, String> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::fs::read_link(path)
        .map_err(|error| error.to_string())?
        .as_os_str()
        .as_bytes()
        .to_vec())
}

#[cfg(not(unix))]
fn read_link_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read_link(path)
        .map_err(|error| error.to_string())?
        .into_os_string()
        .into_string()
        .map(String::into_bytes)
        .map_err(|_| "symlink target is not UTF-8".into())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

pub(crate) struct LoadedAuthority {
    pub(crate) pipeline: TaskPipeline,
    pub(crate) pipeline_artifact_id: String,
    pub(crate) lock_artifact_id: String,
    pub(crate) project_artifact_id: String,
    pub(crate) implementer: ResolvedReviewer,
    pub(crate) evaluator: ResolvedReviewer,
    pub(crate) implementer_authority: WorkerAuthority,
    pub(crate) evaluator_authority: WorkerAuthority,
}

fn resolve_task_state(state: &Option<PathBuf>, repository: &Path) -> Result<PathBuf, String> {
    match state {
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

pub(crate) fn task_id(repository: &Path, source: &str, goal: &str) -> String {
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

pub(crate) fn load_authority(
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
    if let Some(note) = crate::project::check_lock_af_version(&lockfile, ".af/af.lock")? {
        eprintln!("af task: note: {note}");
    }
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
    let text = std::str::from_utf8(bytes).map_err(|error| format!(".af/af.toml: {error}"))?;
    let project = crate::project::ProjectFile::parse(text)?;
    let configured = project.task_pipeline()?;
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
