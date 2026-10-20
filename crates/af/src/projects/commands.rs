//! `af project`: show the Project, list its Workspaces, and create, adopt or remove one.
//!
//! Every command first registers the Project and the invoking checkout when they are new, as
//! the lazy registration of the other commands does, but a failure here is the command's own
//! failure: it cannot answer without the Project Store. Text goes to stdout, `--json` prints one
//! versioned document, and nothing is written inside a checkout.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use review_core::project::{SurveyRecordedPayloadV1, WorkspaceOriginV1};
use review_source_git::{Capture, Manifest, Snapshot};
use review_store::{Cas, EventStore};
use serde_json::{Value, json};

use super::{
    Checkout, Git, Located, Log, ProjectStore, Registered, UNSHALLOW, WorkspacePaths, adopt,
    default_store, label_from_name, locate, register_in, require_checkout, workspace_id,
};
use crate::cli::{ProjectCommand, WorkspaceCommand};

pub(crate) fn run(command: ProjectCommand) -> Result<i32, String> {
    match command {
        ProjectCommand::Show { json } => show(json),
        ProjectCommand::Workspaces { json } => workspaces(json),
        ProjectCommand::Survey { json } => survey(json),
        ProjectCommand::Refresh { json, force } => refresh(json, force),
        ProjectCommand::History { json } => super::survey::history(json),
        ProjectCommand::Workspace { command } => match command {
            WorkspaceCommand::New {
                label,
                branch,
                path,
                from,
                json,
            } => new(&label, branch, path, from.as_deref(), json),
            WorkspaceCommand::Add {
                path,
                label,
                state,
                bindings,
                tasks,
                json,
            } => add(&path, label, state, bindings, tasks, json),
            WorkspaceCommand::Remove {
                label,
                delete_worktree,
                json,
            } => remove(&label, delete_worktree, json),
        },
    }
}

/// The invoking checkout, its Project Store (held), registered when new.
fn open_here() -> Result<(Checkout, ProjectStore), String> {
    let checkout = require_checkout(Path::new("."))?;
    let (store, _) = adopt(&checkout, None)?;
    Ok((checkout, store))
}

fn time(unix_ms: u64) -> String {
    review_core::task::collection::collected_time(unix_ms)
}

/// One Workspace as the documents carry it: the log's record joined with its paths.
fn workspace_value(workspace: &Registered, paths: Option<&WorkspacePaths>) -> Value {
    let mut value = json!({
        "workspace_id": workspace.workspace_id,
        "label": workspace.label,
        "origin": workspace.origin.as_str(),
        "registered_unix_ms": workspace.registered_unix_ms.to_string(),
    });
    if let Some(paths) = paths {
        value["path"] = json!(paths.path.to_string_lossy());
        value["state"] = json!(paths.state.to_string_lossy());
        if let Some(bindings) = &paths.bindings {
            value["bindings"] = json!(bindings.to_string_lossy());
        }
        if let Some(tasks) = &paths.tasks {
            value["tasks"] = json!(tasks.to_string_lossy());
        }
    }
    value
}

/// Every registered Workspace in label order, each with its paths when this machine knows them.
fn joined<'a>(
    log: &'a Log,
    paths: &'a [WorkspacePaths],
) -> Vec<(&'a Registered, Option<&'a WorkspacePaths>)> {
    let mut workspaces: Vec<_> = log
        .workspaces
        .values()
        .map(|workspace| {
            let known = paths
                .iter()
                .find(|entry| entry.id == workspace.workspace_id);
            (workspace, known)
        })
        .collect();
    workspaces.sort_by(|a, b| a.0.label.cmp(&b.0.label));
    workspaces
}

/// `af/project@1`.
pub(crate) fn project_document(store: &ProjectStore, log: &Log, paths: &[WorkspacePaths]) -> Value {
    let project = log
        .project
        .as_ref()
        .expect("the Project is recorded before it is shown");
    let mut document = json!({
        "schema": "af/project@1",
        "project_id": project.project_id,
        "repository_id": project.repository_id,
    });
    if let Some(name) = &project.name {
        document["name"] = json!(name);
    }
    if let Some(min_af) = &project.min_af {
        document["min_af"] = json!(min_af);
    }
    document["store"] = json!(store.directory.to_string_lossy());
    document["registered_unix_ms"] = json!(project.registered_unix_ms.get().to_string());
    document["workspaces"] = joined(log, paths)
        .into_iter()
        .map(|(workspace, paths)| workspace_value(workspace, paths))
        .collect();
    document
}

/// `af/project-workspaces@1`.
pub(crate) fn workspaces_document(
    store: &ProjectStore,
    log: &Log,
    paths: &[WorkspacePaths],
) -> Value {
    json!({
        "schema": "af/project-workspaces@1",
        "project_id": store.project_id,
        "workspaces": joined(log, paths)
            .into_iter()
            .map(|(workspace, paths)| workspace_value(workspace, paths))
            .collect::<Vec<_>>(),
    })
}

fn print_json(document: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(document).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn shown(path: Option<&Path>) -> String {
    path.map_or_else(
        || "unknown on this machine".to_string(),
        |path| path.display().to_string(),
    )
}

/// One Workspace in text: its label line, then one line per path.
fn print_workspace(workspace: &Registered, paths: Option<&WorkspacePaths>, here: &Path) {
    let this = if paths.is_some_and(|paths| paths.path == here) {
        ", this checkout"
    } else {
        ""
    };
    println!(
        "{} ({}, registered {}{this})",
        workspace.label,
        workspace.origin.as_str(),
        time(workspace.registered_unix_ms)
    );
    println!("  id        {}", workspace.workspace_id);
    println!(
        "  path      {}",
        shown(paths.map(|paths| paths.path.as_path()))
    );
    println!(
        "  store     {}",
        shown(paths.map(|paths| paths.state.as_path()))
    );
    if let Some(bindings) = paths.and_then(|paths| paths.bindings.as_deref()) {
        println!("  bindings  {}", bindings.display());
    }
    if let Some(tasks) = paths.and_then(|paths| paths.tasks.as_deref()) {
        println!("  tasks     {}", tasks.display());
    }
}

fn print_workspaces(log: &Log, paths: &[WorkspacePaths], here: &Path) {
    let all = joined(log, paths);
    println!(
        "{} Workspace{}",
        all.len(),
        if all.len() == 1 { "" } else { "s" }
    );
    for (workspace, paths) in all {
        print_workspace(workspace, paths, here);
    }
}

fn show(json_output: bool) -> Result<i32, String> {
    let (checkout, store) = open_here()?;
    let log = store.log()?;
    let paths = store.paths()?;
    if json_output {
        print_json(&project_document(&store, &log, &paths))?;
        return Ok(0);
    }
    let project = log
        .project
        .as_ref()
        .ok_or("the Project Store records no Project")?;
    println!("Project {}", project.project_id);
    println!(
        "  name           {}",
        project.name.as_deref().unwrap_or("-")
    );
    println!(
        "  min_af         {}",
        project.min_af.as_deref().unwrap_or("-")
    );
    println!("  repository_id  {}", project.repository_id);
    println!("  store          {}", store.directory.display());
    println!(
        "  registered     {}",
        time(project.registered_unix_ms.get())
    );
    println!();
    print_workspaces(&log, &paths, &checkout.toplevel);
    // A newest Survey whose record is of a superseded version is said, never read.
    if log.failure_since_newest_survey().is_none()
        && let Some(newest) = log.newest_survey()
        && store.collected(newest)?.is_none()
        && store.superseded(newest)
    {
        println!();
        println!(
            "The newest Survey {} is {}; `af project refresh` records one",
            newest.survey_id,
            super::survey::SUPERSEDED_LINE
        );
        return Ok(0);
    }
    let newest = super::survey::newest(&store, &log)?;
    match (log.failure_since_newest_survey(), newest) {
        (Some(failed), _) => {
            println!();
            println!(
                "Survey failed at {} ({}): {}",
                &failed.head[..failed.head.len().min(12)],
                time(failed.recorded_unix_ms.get()),
                failed.reason
            );
        }
        (None, Some(recorded)) => {
            println!();
            for line in super::survey::summary(&recorded) {
                println!("{line}");
            }
        }
        (None, None) => {
            println!();
            println!("No Survey yet; `af project refresh` records one");
        }
    }
    Ok(0)
}

/// The record's canonical bytes, one line.
fn print_record(recorded: &super::survey::Recorded) -> Result<(), String> {
    println!(
        "{}",
        std::str::from_utf8(&recorded.bytes).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn survey(json_output: bool) -> Result<i32, String> {
    let (_, store) = open_here()?;
    let log = store.log()?;
    let recorded = super::survey::newest(&store, &log)?
        .ok_or("this Project has no Survey yet; run `af project refresh` to record one")?;
    if json_output {
        print_record(&recorded)?;
    } else {
        for line in super::survey::summary(&recorded) {
            println!("{line}");
        }
    }
    Ok(0)
}

fn refresh(json_output: bool, force: bool) -> Result<i32, String> {
    let checkout = require_checkout(Path::new("."))?;
    // The Surveyor registers the checkout itself, its lock taken within the Survey's deadline,
    // and releases the Store while the head is measured.
    let (word, recorded) = match super::survey::refresh(&checkout, force, true)? {
        super::survey::Refreshed::Recorded(recorded) => ("recorded".to_string(), recorded),
        super::survey::Refreshed::Unchanged(recorded) => {
            (super::survey::unchanged_line(&recorded.event), recorded)
        }
    };
    if json_output {
        if word != "recorded" {
            eprintln!("{word}");
        }
        print_record(&recorded)?;
    } else {
        println!("{word}");
        for line in super::survey::summary(&recorded) {
            println!("{line}");
        }
    }
    Ok(0)
}

/// `af project history` once `projects::survey::history` verified the chain: every Survey
/// newest first (`af/project-history@2`), one whose record is of a superseded version
/// (`superseded`) with its ID, when it was recorded and `unreadable: superseded version`, and
/// none of its event's unverified fields.
pub(crate) fn history(json_output: bool, superseded: &BTreeSet<String>) -> Result<i32, String> {
    let (_, store) = open_here()?;
    let log = store.log()?;
    let chain = log.chain();
    let unreadable = |recorded: &SurveyRecordedPayloadV1| superseded.contains(&recorded.survey_id);
    if json_output {
        print_json(&json!({
            "schema": "af/project-history@2",
            "project_id": store.project_id,
            "surveys": chain
                .iter()
                .map(|recorded| if unreadable(recorded) {
                    json!({
                        "survey_id": recorded.survey_id,
                        "recorded_unix_ms": recorded.recorded_unix_ms.get().to_string(),
                        "unreadable": "superseded version",
                    })
                } else {
                    json!({
                        "survey_id": recorded.survey_id,
                        "recorded_unix_ms": recorded.recorded_unix_ms.get().to_string(),
                        "head": recorded.head,
                        "cutoff_unix_s": recorded.cutoff_unix_s.get().to_string(),
                        "completeness": recorded.completeness.as_str(),
                    })
                })
                .collect::<Vec<_>>(),
        }))?;
        return Ok(0);
    }
    println!(
        "{} Survey{}",
        chain.len(),
        if chain.len() == 1 { "" } else { "s" }
    );
    for recorded in chain {
        if unreadable(recorded) {
            println!(
                "{}  {}  {}",
                recorded.survey_id,
                time(recorded.recorded_unix_ms.get()),
                super::survey::SUPERSEDED_LINE
            );
            continue;
        }
        println!(
            "{}  {}  head {}  cutoff {}  {}",
            recorded.survey_id,
            time(recorded.recorded_unix_ms.get()),
            &recorded.head[..recorded.head.len().min(12)],
            time(recorded.cutoff_unix_s.get().saturating_mul(1000)),
            recorded.completeness.as_str()
        );
    }
    Ok(0)
}

fn workspaces(json_output: bool) -> Result<i32, String> {
    let (checkout, store) = open_here()?;
    let log = store.log()?;
    let paths = store.paths()?;
    if json_output {
        print_json(&workspaces_document(&store, &log, &paths))?;
    } else {
        print_workspaces(&log, &paths, &checkout.toplevel);
    }
    Ok(0)
}

/// What `bindings.toml` holds when `af project workspace new` writes it: no account.
fn bindings_template(label: &str) -> String {
    format!(
        "# Local bindings of the Workspace `{label}`: `af task plan --workspace {label}` and \
         `af task start\n# --workspace {label}` pass this file as --bindings. Name the Provider \
         of a Worker under\n# [providers], for example `implementer = \"claude-main\"`; this \
         template names none.\nschema = \"af.task-bindings/1\"\n\n[providers]\n"
    )
}

/// Why `workspace new` made nothing: a usage error (exit 2) or a failure (exit 1).
enum Refused {
    Usage(String),
    Failure(String),
}

/// What `workspace new` makes once every check has passed.
struct Admitted {
    directory: PathBuf,
    worktree: PathBuf,
    branch: String,
    reference: String,
    commit: String,
}

/// One thing `workspace new` made, recorded as it is made and undone in reverse order.
pub(super) enum Made {
    Directory(PathBuf),
    Branch {
        reference: String,
        commit: String,
    },
    /// The directories above the worktree that this command's own `create_dir` made, highest
    /// first; never one another process made meanwhile.
    Ancestors(Vec<Ancestor>),
    Worktree(Adding),
    Registration(String),
}

/// A directory above the worktree that this command's own `create_dir` made at `path`, with the
/// filesystem `identity` (device, inode) it had then: the undo removes only that directory, never
/// another one at the same path.
pub(super) struct Ancestor {
    pub(super) path: PathBuf,
    pub(super) identity: (u64, u64),
}

/// The worktree `workspace new` adds at `path`, whose directory this command's own `create_dir`
/// made with the filesystem `identity` (device, inode), with the names `administrative`
/// (`<common dir>/worktrees`) held before `git worktree add` ran, and the `commit`, the captured
/// `manifest` and the `submodules` (paths relative to the worktree, each an empty directory) it
/// fills the worktree from.
pub(super) struct Adding {
    pub(super) path: PathBuf,
    pub(super) identity: (u64, u64),
    pub(super) administrative: PathBuf,
    pub(super) before: BTreeSet<OsString>,
    pub(super) commit: String,
    pub(super) manifest: Manifest,
    pub(super) submodules: Vec<PathBuf>,
}

/// Every place a worktree of this Project already occupies, or af's own state: a new
/// worktree at or beneath one of them is refused. The reason, when it is.
fn inadmissible(
    worktree: &Path,
    checkout: &Checkout,
    store: &ProjectStore,
    git: &Git,
) -> Result<Option<String>, String> {
    if worktree.starts_with(&checkout.toplevel) {
        return Ok(Some(format!(
            "it is {} the checkout {} this command runs in, and af writes nothing inside a \
             checkout",
            if worktree == checkout.toplevel {
                "exactly"
            } else {
                "inside"
            },
            checkout.toplevel.display()
        )));
    }
    for entry in store.paths()? {
        if worktree.starts_with(&entry.path) {
            return Ok(Some(format!(
                "it is inside the Workspace `{}` at {}",
                entry.label,
                entry.path.display()
            )));
        }
    }
    let listed = git.require(["worktree", "list", "--porcelain"], None)?;
    for line in listed.lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
        if worktree.starts_with(&path) {
            return Ok(Some(format!(
                "it is inside the worktree {} of this repository",
                path.display()
            )));
        }
    }
    let state = crate::resolve_filesystem_path(&crate::xdg_state_root()?.join("af"))?;
    if worktree.starts_with(&state) {
        return Ok(Some(format!(
            "it is inside af's state directory {}",
            state.display()
        )));
    }
    Ok(None)
}

/// Every check that can refuse `workspace new`, in order, before anything is made: the label is
/// free; the path is admissible and absent; the branch is valid and absent; `--from` is a commit
/// of this Project; the Workspace directory is absent.
fn admit(
    checkout: &Checkout,
    store: &ProjectStore,
    git: &Git,
    label: &str,
    branch: Option<String>,
    path: Option<PathBuf>,
    from: Option<&str>,
) -> Result<Admitted, Refused> {
    use Refused::{Failure, Usage};
    let log = store.log().map_err(Failure)?;
    if let Some(holder) = log.by_label(label) {
        return Err(Failure(format!(
            "the label `{label}` is taken by Workspace {}; choose another label",
            holder.workspace_id
        )));
    }
    let explicit = path.is_some();
    let worktree = match path {
        Some(path) => crate::resolve_filesystem_path(&path).map_err(Usage)?,
        None => {
            let name = checkout
                .toplevel
                .file_name()
                .ok_or_else(|| Failure("the checkout has no directory name".into()))?;
            let parent = checkout.toplevel.parent().ok_or_else(|| {
                Failure("the checkout has no parent directory; name one with --path".into())
            })?;
            parent.join(format!("{}-wt-{label}", name.to_string_lossy()))
        }
    };
    if let Some(reason) = inadmissible(&worktree, checkout, store, git).map_err(Failure)? {
        let message = format!(
            "the worktree path {} is refused: {reason}; name a place outside it with --path",
            worktree.display()
        );
        return Err(if explicit {
            Usage(message)
        } else {
            Failure(message)
        });
    }
    if std::fs::symlink_metadata(&worktree).is_ok() {
        return Err(Failure(format!(
            "{} already exists; name another place with --path",
            worktree.display()
        )));
    }
    let branch = branch.unwrap_or_else(|| format!("af/{label}"));
    let formatted = git
        .run(["check-ref-format", "--branch", &branch], None)
        .map_err(Failure)?;
    if !formatted.status.success() || String::from_utf8_lossy(&formatted.stdout).trim() != branch {
        return Err(Failure(format!(
            "`{branch}` is not a valid branch name; name another with --branch"
        )));
    }
    let reference = format!("refs/heads/{branch}");
    let existing = git
        .run(["rev-parse", "--verify", "--quiet", &reference], None)
        .map_err(Failure)?;
    if existing.status.success() {
        return Err(Failure(format!(
            "branch `{branch}` already exists; name another with --branch"
        )));
    }
    if existing.status.code() != Some(1) {
        return Err(Failure(format!(
            "reading the branch `{branch}`: {}",
            String::from_utf8_lossy(&existing.stderr).trim()
        )));
    }
    let from = from.unwrap_or("HEAD");
    let resolved = git
        .run(
            [
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{from}^{{commit}}"),
            ],
            None,
        )
        .map_err(Failure)?;
    if !resolved.status.success() {
        return Err(Failure(format!(
            "`{from}` names no commit in this repository; name one with --from"
        )));
    }
    let commit = String::from_utf8_lossy(&resolved.stdout).trim().to_string();
    let roots = git.roots(&commit).map_err(Failure)?;
    if roots != checkout.repository_id {
        return Err(Failure(format!(
            "`{from}` ({commit}) has the root commits {roots}, not this Project's {}, so a \
             worktree of it would be a checkout of Project {}, not of {}; name a commit of this \
             Project with --from",
            checkout.repository_id,
            super::project_id(&roots),
            checkout.project_id
        )));
    }
    let directory = store.workspace_directory(label);
    if std::fs::symlink_metadata(&directory).is_ok() {
        return Err(Failure(format!(
            "{} already exists from an earlier Workspace `{label}`; choose another label, or move \
             that directory away first",
            directory.display()
        )));
    }
    Ok(Admitted {
        directory,
        worktree,
        branch,
        reference,
        commit,
    })
}

/// Make the missing directories above `path` one at a time, from the deepest that exists down,
/// adding to `created` each one this command's own `create_dir` made, with its identity. One
/// another process made meanwhile is not this command's, so it is not recorded and the undo never
/// removes it.
fn make_ancestors(path: &Path, created: &mut Vec<Ancestor>) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut current = path.parent();
    while let Some(directory) = current {
        if std::fs::symlink_metadata(directory).is_ok() {
            break;
        }
        missing.push(directory.to_path_buf());
        current = directory.parent();
    }
    for directory in missing.into_iter().rev() {
        match std::fs::create_dir(&directory) {
            // Its identity proves later that the directory the undo finds is still this one.
            Ok(()) => match std::fs::symlink_metadata(&directory) {
                Ok(metadata) => created.push(Ancestor {
                    identity: identity(&metadata),
                    path: directory,
                }),
                Err(error) => {
                    return Err(format!(
                        "reading {}: {error}; that directory, which this command made, is left \
                         behind",
                        directory.display()
                    ));
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("creating {}: {error}", directory.display())),
        }
    }
    Ok(())
}

/// An empty directory for each submodule, as a checkout leaves an uninitialized one; never
/// through a symbolic link.
fn submodule_directories(worktree: &Path, snapshot: &Snapshot) -> Result<(), String> {
    for submodule in &snapshot.submodules {
        let relative = review_source_git::fs_path(&submodule.path);
        let mut current = worktree.to_path_buf();
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(format!(
                    "the submodule path `{}` is not plain",
                    submodule.path
                ));
            };
            current.push(name);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(format!(
                        "the submodule path `{}` crosses a file or a symbolic link",
                        submodule.path
                    ));
                }
                Err(_) => std::fs::create_dir(&current)
                    .map_err(|error| format!("creating {}: {error}", current.display()))?,
            }
        }
    }
    Ok(())
}

/// Make the Workspace directory, the branch and the worktree, and fill the worktree the way
/// delivery does (ADR-0031): the branch by `update-ref`, the worktree without a checkout, its
/// index by `read-tree`, its files from the CAS. No checkout, filter or hook runs.
fn make(
    git: &Git,
    admitted: &Admitted,
    label: &str,
    snapshot: &Snapshot,
    cas: &Cas,
    made: &mut Vec<Made>,
) -> Result<(), String> {
    let directory = &admitted.directory;
    let parent = directory.parent().unwrap_or(directory);
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    std::fs::create_dir(directory)
        .map_err(|error| format!("creating {}: {error}", directory.display()))?;
    made.push(Made::Directory(directory.clone()));
    (|| -> std::io::Result<()> {
        std::fs::create_dir(directory.join("state"))?;
        std::fs::create_dir(directory.join("tasks"))?;
        std::fs::write(directory.join("bindings.toml"), bindings_template(label))
    })()
    .map_err(|error| format!("creating {}: {error}", directory.display()))?;

    let input = format!(
        "start\ncreate {} {}\nprepare\ncommit\n",
        admitted.reference, admitted.commit
    );
    git.require(["update-ref", "--stdin"], Some(input.into_bytes()))?;
    made.push(Made::Branch {
        reference: admitted.reference.clone(),
        commit: admitted.commit.clone(),
    });

    let worktree = &admitted.worktree;
    let mut ancestors = Vec::new();
    let ancestors_made = make_ancestors(worktree, &mut ancestors);
    if !ancestors.is_empty() {
        made.push(Made::Ancestors(ancestors));
    }
    ancestors_made?;
    // Recorded before git runs: an add that fails or is stopped can leave a partial worktree, or
    // only its administrative entry, which the names held before tell apart from older ones.
    let administrative = git.common_dir()?.join("worktrees");
    let before = administrative_names(&administrative)?;
    // The worktree's directory is this command's only when its own `create_dir` made it; git
    // adds a worktree into an empty directory. One that appeared since the checks is another's.
    match std::fs::create_dir(worktree) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "{} appeared after the checks passed, so it is not this command's and nothing of \
                 it was deleted; name another place with --path",
                worktree.display()
            ));
        }
        Err(error) => return Err(format!("creating {}: {error}", worktree.display())),
    }
    // Its identity proves later that the directory the undo finds is still this one.
    let identity = std::fs::symlink_metadata(worktree)
        .map(|metadata| identity(&metadata))
        .map_err(|error| {
            format!(
                "reading {}: {error}; that directory, which this command made, is left behind",
                worktree.display()
            )
        })?;
    made.push(Made::Worktree(Adding {
        path: worktree.clone(),
        identity,
        administrative,
        before,
        commit: admitted.commit.clone(),
        manifest: snapshot.manifest.clone(),
        submodules: snapshot
            .submodules
            .iter()
            .map(|submodule| review_source_git::fs_path(&submodule.path))
            .collect(),
    }));
    git.require(
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-checkout"),
            OsStr::new("--"),
            worktree.as_os_str(),
            OsStr::new(&admitted.branch),
        ],
        None,
    )?;
    crate::task::ensure_empty_linked_worktree(worktree)?;
    // `--no-checkout` leaves the index empty: fill it from the commit through plumbing, without
    // touching the still-empty worktree, then write the files from the CAS.
    git.at(worktree).require(["read-tree", "HEAD"], None)?;
    review_source_git::materialize(&snapshot.manifest, cas, worktree)
        .map_err(|error| format!("writing the tree of {}: {error}", admitted.commit))?;
    submodule_directories(worktree, snapshot)
}

/// Whether git lists a worktree, whole or partial, at `path`.
fn lists_worktree(git: &Git, path: &Path) -> Result<bool, String> {
    let listed = git.require(["worktree", "list", "--porcelain"], None)?;
    Ok(listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| {
            let listed = Path::new(listed);
            listed == path || std::fs::canonicalize(listed).is_ok_and(|real| real == path)
        }))
}

/// The names in `<common dir>/worktrees`; a missing directory holds none.
pub(super) fn administrative_names(administrative: &Path) -> Result<BTreeSet<OsString>, String> {
    let listing = |error: std::io::Error| format!("listing {}: {error}", administrative.display());
    match std::fs::read_dir(administrative) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<_, _>>()
            .map_err(listing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(listing(error)),
    }
}

/// Whether `name` is one git gives the administrative entry of a worktree at `path`: the path's
/// basename, followed by digits when that name is taken.
fn named_after(path: &Path, name: &OsStr) -> bool {
    path.file_name().is_some_and(|basename| {
        name.as_encoded_bytes()
            .strip_prefix(basename.as_encoded_bytes())
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit))
    })
}

/// Whether the administrative entry `name`, whose `gitdir` file holds `gitdir` (`None` while it
/// has none), is positively one `git worktree add` made for the worktree at `path`: named after
/// the path, with a `gitdir` naming `<path>/.git`. An entry without a `gitdir` is never
/// attributable: a concurrent add of another path with the same basename makes one too.
pub(super) fn owns_entry(path: &Path, name: &OsStr, gitdir: Option<&[u8]>) -> bool {
    named_after(path, name)
        && gitdir.is_some_and(|gitdir| {
            let gitdir = gitdir.strip_suffix(b"\n").unwrap_or(gitdir);
            gitdir == path.join(".git").as_os_str().as_encoded_bytes()
        })
}

/// An administrative entry new since `git worktree add` began and named after its path.
struct NewEntry {
    name: OsString,
    gitdir: Option<Vec<u8>>,
    locked: bool,
}

/// The entries of `administrative` that are new since `before` and named after `path`: this
/// add's, and those another add of the same basename may have made meanwhile.
fn new_entries(
    path: &Path,
    administrative: &Path,
    before: &BTreeSet<OsString>,
) -> Result<Vec<NewEntry>, String> {
    let mut found = Vec::new();
    for name in administrative_names(administrative)?.difference(before) {
        if !named_after(path, name) {
            continue;
        }
        let file = administrative.join(name).join("gitdir");
        let gitdir = match std::fs::read(&file) {
            Ok(gitdir) => Some(gitdir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("reading {}: {error}", file.display())),
        };
        let locked = std::fs::symlink_metadata(administrative.join(name).join("locked")).is_ok();
        found.push(NewEntry {
            name: name.clone(),
            gitdir,
            locked,
        });
    }
    Ok(found)
}

/// The entries of `administrative` that are new since `before` and belong to the add of `path`.
fn added_entries(
    path: &Path,
    administrative: &Path,
    before: &BTreeSet<OsString>,
) -> Result<Vec<OsString>, String> {
    Ok(new_entries(path, administrative, before)?
        .into_iter()
        .filter(|entry| owns_entry(path, &entry.name, entry.gitdir.as_deref()))
        .map(|entry| entry.name)
        .collect())
}

/// What the undo says of a new entry named after `path` that is not attributably this add's,
/// and so is left where it is. Each line is ADR-0151's, word for word.
fn left_alone(administrative: &Path, entry: &NewEntry) -> String {
    let location = administrative.join(&entry.name);
    match &entry.gitdir {
        // `git worktree prune` keeps a locked entry, so it is no way to remove one.
        None if entry.locked => format!(
            "{}: a locked entry without a gitdir, created during this add and possibly another add's; delete that directory once no add is running (git worktree prune keeps a locked entry)",
            location.display()
        ),
        None => format!(
            "{}: an entry without a gitdir, created during this add and possibly another add's; run git worktree prune once no add is running",
            location.display()
        ),
        Some(gitdir) => {
            let gitdir = gitdir.strip_suffix(b"\n").unwrap_or(gitdir);
            format!(
                "{}: an entry created during this add whose gitdir names {}, another add's",
                location.display(),
                String::from_utf8_lossy(gitdir)
            )
        }
    }
}

fn entry_names(entries: &[OsString]) -> String {
    entries
        .iter()
        .map(|name| format!("worktrees/{}", name.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why the directory `workspace new` made at `path` stays while the entries of this add go, `None`
/// when its `.git` does not stop it going: that `.git` is absent, or a file naming one of `owned`,
/// the entries of `administrative` this add owns.
fn foreign_git(path: &Path, administrative: &Path, owned: &[OsString]) -> Option<String> {
    let dot_git = path.join(".git");
    Some(match std::fs::symlink_metadata(&dot_git) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Ok(metadata) if !metadata.is_file() => "its .git is no file".to_string(),
        Err(error) => format!("its .git could not be read: {error}"),
        Ok(_) => match std::fs::read(&dot_git) {
            Err(error) => format!("its .git could not be read: {error}"),
            Ok(text) => match text.strip_prefix(b"gitdir: ") {
                None => "its .git names no gitdir".to_string(),
                Some(named) => {
                    let named = named.strip_suffix(b"\n").unwrap_or(named);
                    let named =
                        path.join(<OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(named));
                    let real = std::fs::canonicalize(&named).ok();
                    let ours = owned.iter().any(|name| {
                        let entry = administrative.join(name);
                        named == entry
                            || real.is_some() && std::fs::canonicalize(&entry).ok() == real
                    });
                    if ours {
                        return None;
                    }
                    format!(
                        "its .git names {}, not an entry of this add",
                        named.display()
                    )
                }
            },
        },
    })
}

/// A directory's filesystem identity: its device and inode.
pub(super) fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

/// Every directory below `root`, relative to it, found without following a symbolic link and
/// without entering the top `.git`.
fn directories_below(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            let child = relative.join(entry.file_name());
            if child == Path::new(".git") {
                continue;
            }
            // `DirEntry::file_type` does not follow a symbolic link.
            if entry.file_type()?.is_dir() {
                pending.push(child.clone());
                found.push(child);
            }
        }
    }
    Ok(found)
}

/// Why the entries below `path` are not provably only what materializing `manifest` and the empty
/// directories of `submodules` wrote, `None` when they are. Every entry but the top `.git`,
/// directories included, must be a path of `manifest` equal in kind, mode and content, the
/// comparison delivery's rollback makes (`manifest_is_subset`), or a directory above such a path
/// or at or above a submodule's; any other entry, an empty directory included, is not this add's.
/// The reason names the first path that is neither.
pub(super) fn foreign_content(
    path: &Path,
    manifest: &Manifest,
    submodules: &[PathBuf],
) -> Option<String> {
    let unreadable = |error: std::io::Error| format!("its files could not be read: {error}");
    let scanned = match review_source_git::scan_tree(path) {
        Ok(scanned) => scanned,
        Err(error) => return Some(unreadable(error)),
    };
    let directories = match directories_below(path) {
        Ok(directories) => directories,
        Err(error) => return Some(unreadable(error)),
    };
    let written: BTreeSet<PathBuf> = manifest
        .entries
        .iter()
        .map(|entry| review_source_git::fs_path(&entry.path))
        .collect();
    // The directories materializing makes: those above a path of the manifest, and a submodule's
    // own with those above it.
    let above: BTreeSet<&Path> = written
        .iter()
        .flat_map(|written| written.ancestors().skip(1))
        .chain(
            submodules
                .iter()
                .flat_map(|submodule| submodule.ancestors()),
        )
        .filter(|directory| !directory.as_os_str().is_empty())
        .collect();
    let files = scanned
        .entries
        .iter()
        .filter(|entry| entry.path != ".git")
        .filter_map(|entry| {
            let shown = review_source_git::fs_path(&entry.path);
            let differs = match manifest
                .entries
                .binary_search_by(|candidate| candidate.path.as_bytes().cmp(entry.path.as_bytes()))
            {
                Ok(index) if manifest.entries[index] == *entry => return None,
                Ok(_) => true,
                Err(_) => false,
            };
            Some((shown, differs))
        });
    let directories = directories
        .into_iter()
        .filter(|directory| !above.contains(directory.as_path()))
        .map(|directory| {
            let differs = written.contains(&directory);
            (directory, differs)
        });
    let (first, differs) = files.chain(directories).min_by(|(a, _), (b, _)| {
        a.as_os_str()
            .as_encoded_bytes()
            .cmp(b.as_os_str().as_encoded_bytes())
    })?;
    Some(if differs {
        format!("`{}` differs from what this add wrote", first.display())
    } else {
        format!(
            "it holds `{}`, which this add did not write",
            first.display()
        )
    })
}

/// `--git-dir=<git_dir>`, for git to read an administrative entry as a repository.
fn git_dir_option(git_dir: &Path) -> OsString {
    let mut option = OsString::from("--git-dir=");
    option.push(git_dir);
    option
}

/// The paths at which an index differs from `commit`, staged or unmerged, through delivery's
/// supervised, filter-free cached-index comparison (`git diff-index --cached` with external diff
/// and textconv disabled, as `index_matches_head` makes it): the index of the worktree `git` runs
/// in, or of the administrative entry `git_dir`.
pub(super) fn staged_paths(
    git: &Git,
    git_dir: Option<&Path>,
    commit: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut args: Vec<OsString> = git_dir.map(git_dir_option).into_iter().collect();
    args.extend(
        [
            "diff-index",
            "--cached",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            commit,
            "--",
        ]
        .map(OsString::from),
    );
    let output = git.run(&args, None)?;
    if !output.status.success() {
        return Err(format!(
            "comparing the index with {commit}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(<OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(path)))
        .collect())
}

/// Why the index of an entry of `owned` is not one this add wrote, `None` when each is: absent
/// or empty, as `git worktree add --no-checkout` leaves it, or the tree of `commit`, as `git
/// read-tree` fills it.
fn changed_index(
    git: &Git,
    administrative: &Path,
    owned: &[OsString],
    commit: &str,
) -> Option<String> {
    owned.iter().find_map(|name| {
        let entry = administrative.join(name);
        let index = entry.join("index");
        match std::fs::symlink_metadata(&index) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                return Some(format!(
                    "its index {} could not be read: {error}",
                    index.display()
                ));
            }
            Ok(_) => {}
        }
        let differing = match staged_paths(git, Some(&entry), commit) {
            Ok(differing) => differing,
            Err(error) => {
                return Some(format!(
                    "its index {} could not be compared: {error}",
                    index.display()
                ));
            }
        };
        let first = differing.first()?;
        match git.run(
            [git_dir_option(&entry), "ls-files".into(), "-z".into()],
            None,
        ) {
            Ok(listed) if listed.status.success() && listed.stdout.is_empty() => None,
            Ok(_) | Err(_) => Some(format!(
                "its index differs from {commit} at `{}`",
                first.display()
            )),
        }
    })
}

/// What the undo says of a worktree it keeps whole, its directory and this add's entries, because
/// of `reason`.
fn kept_whole(
    path: &Path,
    directory: bool,
    administrative: &Path,
    entries: &[OsString],
    reason: &str,
) -> String {
    match (directory, entries.is_empty()) {
        (true, true) => format!(
            "the directory {}, kept because {reason}; delete it once no add uses it",
            path.display()
        ),
        (true, false) => format!(
            "the worktree {}, kept with its administrative entry {} because {reason}; remove it \
             with `git worktree remove --force {}` once nothing in it is needed",
            path.display(),
            entry_names(entries),
            path.display()
        ),
        (false, _) => format!(
            "the administrative entry {}, kept because {reason}; delete it once nothing in it is \
             needed",
            entries
                .iter()
                .map(|name| administrative.join(name).display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Why an undo of a worktree did not finish, with the reports of what it left alone.
type Unremoved = (String, Vec<String>);

/// What the undo of a worktree did: the lines of what it removed, empty when nothing of it
/// existed, and of what it left alone; and whether it kept an entry of this add, whose worktree is
/// on the new branch.
struct WorktreeUndone {
    removed: Vec<String>,
    left: Vec<String>,
    kept: bool,
}

/// Remove whatever exists of the worktree `workspace new` was adding, deleting only what is
/// provably this add's (ADR-0031). The worktree stays whole, its directory and this add's entries,
/// unless the directory still has the identity it was created with, every entry in it but `.git`,
/// directories included, equals the captured manifest in path, kind, mode and content or is a
/// directory materializing it makes, and the index this add wrote is unchanged: directory identity
/// and path membership never prove ownership of bytes. Otherwise the
/// directory goes while its `.git` is absent or names an entry this add owns, and every
/// administrative entry positively this add's goes too, both deleted directly. `git worktree
/// prune` never runs: it would also remove stale entries of other worktrees. The new entries
/// named after `path` that are not this add's are left alone and reported. The error says what
/// remains and carries the same left-alone reports, so a failing final check never hides a
/// surviving entry.
fn remove_worktree(git: &Git, adding: &Adding) -> Result<WorktreeUndone, Unremoved> {
    let Adding {
        path,
        identity: created,
        administrative,
        before,
        commit,
        manifest,
        submodules,
    } = adding;
    let found = new_entries(path, administrative, before).map_err(|error| (error, Vec::new()))?;
    let (entries, others): (Vec<_>, Vec<_>) = found
        .into_iter()
        .partition(|entry| owns_entry(path, &entry.name, entry.gitdir.as_deref()));
    let entries: Vec<OsString> = entries.into_iter().map(|entry| entry.name).collect();
    let others: Vec<String> = others
        .iter()
        .map(|entry| left_alone(administrative, entry))
        .collect();
    let metadata = std::fs::symlink_metadata(path);
    let directory = !matches!(
        &metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    // A `.git` that names another add's entry keeps the directory alone; anything else this add
    // cannot prove it wrote keeps the whole worktree.
    let mut foreign = None;
    let mut changed = match &metadata {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!("it could not be read: {error}")),
        Ok(metadata) if !metadata.is_dir() || identity(metadata) != *created => {
            Some("it was replaced since this add created it".to_string())
        }
        Ok(_) => {
            foreign = foreign_git(path, administrative, &entries);
            foreign
                .is_none()
                .then(|| foreign_content(path, manifest, submodules))
                .flatten()
        }
    };
    if changed.is_none() {
        changed = changed_index(git, administrative, &entries, commit);
    }
    if let Some(reason) = changed {
        let mut left = vec![kept_whole(
            path,
            directory,
            administrative,
            &entries,
            &reason,
        )];
        left.extend(others);
        return Ok(WorktreeUndone {
            removed: Vec::new(),
            left,
            kept: !entries.is_empty(),
        });
    }
    let left: Vec<String> = foreign
        .iter()
        .map(|reason| {
            format!(
                "the directory {}, kept because {reason}; delete it once no add uses it",
                path.display()
            )
        })
        .chain(others)
        .collect();
    let directory = directory && foreign.is_none();
    let cleared = (|| -> Result<(), String> {
        if directory {
            match std::fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("removing its directory: {error}")),
            }
        }
        // Git made these entries for this add: they are ours to delete.
        for name in &entries {
            let entry = administrative.join(name);
            match std::fs::remove_dir_all(&entry) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("removing {}: {error}", entry.display())),
            }
        }
        if lists_worktree(git, path)? {
            return Err("git still lists it".into());
        }
        Ok(())
    })();
    let remaining = match added_entries(path, administrative, before) {
        Ok(remaining) => remaining,
        Err(listing) => {
            let error = match cleared {
                Ok(()) => listing,
                Err(error) => format!("{error}; {listing}"),
            };
            return Err((error, left));
        }
    };
    let error = match cleared {
        Ok(()) if remaining.is_empty() => None,
        Ok(()) => Some(format!(
            "its administrative entry {} remains in {}",
            entry_names(&remaining),
            administrative.display()
        )),
        Err(error) if remaining.is_empty() => Some(error),
        Err(error) => Some(format!(
            "{error}; its administrative entry {} remains in {}",
            entry_names(&remaining),
            administrative.display()
        )),
    };
    if let Some(error) = error {
        return Err((error, left));
    }
    let mut removed = Vec::new();
    if directory {
        removed.push(format!("removed the worktree {}", path.display()));
    }
    removed.extend(entries.iter().map(|name| {
        format!(
            "removed the administrative entry worktrees/{}",
            name.to_string_lossy()
        )
    }));
    Ok(WorktreeUndone {
        removed,
        left,
        kept: false,
    })
}

/// The commit `reference` holds, `None` when it does not exist.
fn branch_tip(git: &Git, reference: &str) -> Result<Option<String>, String> {
    let resolved = git.run(["rev-parse", "--verify", "--quiet", reference], None)?;
    match resolved.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&resolved.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => Err(String::from_utf8_lossy(&resolved.stderr).trim().to_string()),
    }
}

/// Undo what `workspace new` made, newest first, and say what was undone and what, if an undo
/// failed, remains.
pub(super) fn undo(git: &Git, store: &mut ProjectStore, made: Vec<Made>) -> String {
    let mut undone = Vec::new();
    let mut remains = Vec::new();
    let mut kept_worktree: Option<PathBuf> = None;
    for step in made.into_iter().rev() {
        match step {
            Made::Registration(id) => match store.unregister(&id) {
                Ok(()) => undone.push(format!("unregistered Workspace {id}")),
                Err(error) => remains.push(format!(
                    "the registration of Workspace {id} ({error}; remove it with `af project \
                     workspace remove`)"
                )),
            },
            Made::Worktree(adding) => match remove_worktree(git, &adding) {
                // Empty when git made nothing of it: nothing to undo or report.
                Ok(worktree) => {
                    undone.extend(worktree.removed);
                    remains.extend(worktree.left);
                    if worktree.kept {
                        kept_worktree = Some(adding.path);
                    }
                }
                Err((error, left)) => {
                    remains.push(format!(
                        "the worktree {} ({error}; remove it with `git worktree remove --force \
                         --force`, or remove the directory and run `git worktree prune`)",
                        adding.path.display()
                    ));
                    remains.extend(left);
                }
            },
            // Only the directories this command made, deepest first, each only while it is still
            // that directory (its identity unchanged) and empty: path membership proves nothing,
            // and a directory recreated at the same path is another's.
            Made::Ancestors(created) => {
                for ancestor in created.iter().rev() {
                    let directory = ancestor.path.display();
                    match std::fs::symlink_metadata(&ancestor.path) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            remains.push(format!("the directory {directory} ({error})"));
                            continue;
                        }
                        Ok(metadata)
                            if !metadata.is_dir() || identity(&metadata) != ancestor.identity =>
                        {
                            remains.push(format!(
                                "the directory {directory}, kept because it was replaced since \
                                 this add created it"
                            ));
                            continue;
                        }
                        Ok(_) => {}
                    }
                    match std::fs::remove_dir(&ancestor.path) {
                        Ok(()) => undone.push(format!("removed the directory {directory}")),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                            remains.push(format!(
                                "the directory {directory}, kept because it is not empty"
                            ));
                        }
                        Err(error) => remains.push(format!("the directory {directory} ({error})")),
                    }
                }
            }
            // Deleted only while it still holds the commit it was created at: a branch that moved
            // meanwhile holds someone's work. A worktree kept with its entry is on it, so it stays.
            Made::Branch { reference, commit } => {
                if let Some(worktree) = &kept_worktree {
                    remains.push(format!(
                        "the branch {reference}, kept because the worktree {} kept above is on it",
                        worktree.display()
                    ));
                    continue;
                }
                match git.require(["update-ref", "-d", &reference, &commit], None) {
                    Ok(_) => undone.push(format!("deleted the branch {reference}")),
                    Err(error) => match branch_tip(git, &reference) {
                        Ok(None) => {}
                        Ok(Some(tip)) if tip != commit => remains.push(format!(
                            "the branch {reference} moved since this add created it and is kept"
                        )),
                        _ => remains.push(format!(
                            "the branch {reference} ({error}; delete it with `git update-ref -d \
                             {reference} {commit}`)"
                        )),
                    },
                }
            }
            Made::Directory(directory) => match std::fs::remove_dir_all(&directory) {
                Ok(()) => undone.push(format!("removed {}", directory.display())),
                Err(error) => {
                    remains.push(format!("the directory {} ({error})", directory.display()));
                }
            },
        }
    }
    match (undone.is_empty(), remains.is_empty()) {
        (true, true) => "nothing was created".into(),
        (false, true) => format!(
            "undone, newest first: {}; nothing was left behind",
            undone.join(", ")
        ),
        (true, false) => format!("left behind: {}", remains.join("; ")),
        (false, false) => format!(
            "undone, newest first: {}; left behind: {}",
            undone.join(", "),
            remains.join("; ")
        ),
    }
}

fn new(
    label: &str,
    branch: Option<String>,
    path: Option<PathBuf>,
    from: Option<&str>,
    json_output: bool,
) -> Result<i32, String> {
    let (checkout, mut store) = open_here()?;
    let git = Git::open(&checkout.toplevel)?;
    let admitted = match admit(&checkout, &store, &git, label, branch, path, from) {
        Ok(admitted) => admitted,
        Err(Refused::Usage(message)) => {
            eprintln!(
                "error: {message}; nothing was created\n  try: af project workspace new --help"
            );
            return Ok(2);
        }
        Err(Refused::Failure(message)) => return Err(format!("{message}; nothing was created")),
    };
    // The commit's tree, captured the way every committed Snapshot is, into a CAS that lives as
    // long as this command: the files the worktree receives, read without a filter.
    let scratch = tempfile::Builder::new()
        .prefix("af-workspace-new-")
        .tempdir()
        .map_err(|error| format!("creating a scratch directory: {error}"))?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    let snapshot = Capture::new(&git.repo(), &cas)
        .committed(&admitted.commit)
        .map_err(|error| {
            format!(
                "capturing the tree of {}: {error}; nothing was created",
                admitted.commit
            )
        })?;

    let mut made = Vec::new();
    let outcome = make(&git, &admitted, label, &snapshot, &cas, &mut made).and_then(|()| {
        let created = match locate(&admitted.worktree)? {
            Located::Checkout(created) if created.project_id == checkout.project_id => created,
            _ => {
                return Err(format!(
                    "git does not see a checkout of this Project at {}",
                    admitted.worktree.display()
                ));
            }
        };
        let id = workspace_id(&checkout.project_id, &created.toplevel);
        let paths = WorkspacePaths {
            id: id.clone(),
            label: label.to_string(),
            path: created.toplevel.clone(),
            state: admitted.directory.join("state"),
            bindings: Some(admitted.directory.join("bindings.toml")),
            tasks: Some(admitted.directory.join("tasks")),
        };
        let registered = register_in(
            &mut store,
            &checkout,
            WorkspaceOriginV1::Created,
            label,
            true,
            paths,
        );
        if registered.is_err()
            && store
                .log()
                .is_ok_and(|log| log.workspaces.contains_key(&id))
        {
            made.push(Made::Registration(id));
        }
        registered.map(|_| created)
    });
    let created = match outcome {
        Ok(created) => created,
        Err(error) => return Err(format!("{error}; {}", undo(&git, &mut store, made))),
    };
    if json_output {
        let log = store.log()?;
        print_json(&workspaces_document(&store, &log, &store.paths()?))?;
        return Ok(0);
    }
    let short = &admitted.commit[..admitted.commit.len().min(12)];
    let directory = &admitted.directory;
    println!("created Workspace {label}");
    println!(
        "  path      {} (branch {} at {short})",
        created.toplevel.display(),
        admitted.branch
    );
    println!("  store     {}", directory.join("state").display());
    println!("  bindings  {}", directory.join("bindings.toml").display());
    println!("  tasks     {}", directory.join("tasks").display());
    println!(
        "Plan a Task in it from the worktree: af task plan --workspace {label} --file {}",
        directory.join("tasks").join("TASK.json").display()
    );
    Ok(0)
}

fn add(
    path: &Path,
    label: Option<String>,
    state: Option<PathBuf>,
    bindings: Option<PathBuf>,
    tasks: Option<PathBuf>,
    json_output: bool,
) -> Result<i32, String> {
    let (checkout, mut store) = open_here()?;
    let target = match locate(path)? {
        Located::Checkout(target) => target,
        Located::NotGit => {
            return Err(format!(
                "{} is not inside a git checkout; name a worktree or clone of this repository",
                path.display()
            ));
        }
        Located::NoCommit => {
            return Err(format!(
                "{} is a repository without commits, so it is no checkout of this Project",
                path.display()
            ));
        }
        Located::Shallow => return Err(format!("{}: {UNSHALLOW}", path.display())),
    };
    if target.project_id != checkout.project_id {
        return Err(format!(
            "{} is a checkout of Project {}, not of this checkout's Project {}; run the command \
             in a checkout of {}",
            target.toplevel.display(),
            target.project_id,
            checkout.project_id,
            target.project_id
        ));
    }
    let id = workspace_id(&checkout.project_id, &target.toplevel);
    let log = store.log()?;
    if let Some(existing) = log.workspaces.get(&id) {
        return Err(format!(
            "{} is already the Workspace `{}`; run `af project workspace remove {}` first to \
             register it again",
            target.toplevel.display(),
            existing.label,
            existing.label
        ));
    }
    let state = match state {
        Some(state) => {
            let state = crate::resolve_filesystem_path(&state)?;
            if state.starts_with(&target.toplevel) {
                return Err("--state must be outside the checkout".into());
            }
            state
        }
        None => default_store(&target.toplevel)?,
    };
    let bindings = bindings
        .map(|bindings| {
            let bindings = crate::resolve_filesystem_path(&bindings)?;
            if bindings.is_file() {
                Ok(bindings)
            } else {
                Err(format!("--bindings {} is no file", bindings.display()))
            }
        })
        .transpose()?;
    let tasks = tasks
        .map(|tasks| {
            let tasks = crate::resolve_filesystem_path(&tasks)?;
            if tasks.is_dir() {
                Ok(tasks)
            } else {
                Err(format!("--tasks {} is no directory", tasks.display()))
            }
        })
        .transpose()?;
    let (preferred, exact) = match label {
        Some(label) => (label, true),
        None => (
            label_from_name(target.toplevel.file_name().unwrap_or_default()),
            false,
        ),
    };
    let paths = WorkspacePaths {
        id,
        label: preferred.clone(),
        path: target.toplevel.clone(),
        state,
        bindings,
        tasks,
    };
    let registration = register_in(
        &mut store,
        &checkout,
        WorkspaceOriginV1::Adopted,
        &preferred,
        exact,
        paths,
    )?;
    let log = store.log()?;
    let paths = store.paths()?;
    if json_output {
        print_json(&workspaces_document(&store, &log, &paths))?;
        return Ok(0);
    }
    let workspace = registration.workspace();
    println!("added Workspace {}", workspace.label);
    print_workspace(
        workspace,
        paths
            .iter()
            .find(|entry| entry.id == workspace.workspace_id),
        &checkout.toplevel,
    );
    Ok(0)
}

/// The Tasks of the Store at `state` that are not collected, read-only.
fn uncollected(state: &Path) -> Result<Vec<String>, String> {
    if !state.join("events.sqlite").is_file() {
        return Ok(Vec::new());
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|error| error.to_string())?;
    let events = EventStore::open_read_only(state.join("events.sqlite"))
        .map_err(|error| error.to_string())?;
    events.task_ids(&cas).map_err(|error| error.to_string())
}

/// What became of a removed Workspace's Store: always kept, and why that matters.
fn store_note(state: &Path) -> String {
    match uncollected(state) {
        Ok(tasks) if tasks.is_empty() => format!(
            "kept its Store {}: it holds no Task that is not collected (af never removes a Store)",
            state.display()
        ),
        Ok(tasks) => format!(
            "kept its Store {}: it holds {} Task{} not collected ({}); af never removes a Store, \
             and `af task gc --state {} --older-than DAYS --keep N --apply` collects its finished \
             Tasks",
            state.display(),
            tasks.len(),
            if tasks.len() == 1 { "" } else { "s" },
            tasks.join(", "),
            state.display()
        ),
        Err(error) => format!(
            "kept its Store {}; its Tasks could not be listed: {error}",
            state.display()
        ),
    }
}

fn remove(label: &str, delete_worktree: bool, json_output: bool) -> Result<i32, String> {
    let (checkout, mut store) = open_here()?;
    let log = store.log()?;
    let workspace = log.by_label(label).cloned().ok_or_else(|| {
        format!(
            "this Project has no Workspace labelled `{label}`; `af project workspaces` lists them"
        )
    })?;
    let paths = store
        .paths()?
        .into_iter()
        .find(|entry| entry.id == workspace.workspace_id);
    let record = super::removal::location(&store.directory, &workspace.workspace_id);
    let mut deleted = None;
    if delete_worktree {
        let path = paths.as_ref().map(|paths| paths.path.clone()).ok_or_else(|| {
            format!(
                "the checkout of Workspace `{label}` is unknown on this machine, so nothing was \
                 changed; run without --delete-worktree"
            )
        })?;
        if path == checkout.toplevel {
            return Err(format!(
                "Workspace `{label}` is the checkout this command runs in, so nothing was \
                 changed; run the command from another checkout of the Project"
            ));
        }
        let git = Git::open(&checkout.toplevel)?;
        // A rerun finishes what the removal record of an earlier run holds.
        deleted = Some(super::removal::delete_worktree(
            &git,
            &store.directory,
            &workspace.workspace_id,
            &path,
        )?);
    } else if record.exists() {
        return Err(format!(
            "an unfinished `--delete-worktree` of Workspace `{label}` is recorded in {}; finish \
             it with `af project workspace remove {label} --delete-worktree`; nothing was changed",
            record.display()
        ));
    }
    store.unregister(&workspace.workspace_id)?;
    let note = paths.as_ref().map(|paths| store_note(&paths.state));
    if json_output {
        let log = store.log()?;
        print_json(&workspaces_document(&store, &log, &store.paths()?))?;
        if let Some(deleted) = deleted {
            for line in std::iter::once(&deleted.line).chain(&deleted.notes) {
                eprintln!("af project workspace remove: {line}");
            }
        }
        if let Some(note) = note {
            eprintln!("af project workspace remove: {note}");
        }
        return Ok(0);
    }
    println!("removed Workspace {label} ({})", workspace.workspace_id);
    if let Some(deleted) = deleted {
        println!("{}", deleted.line);
        for note in &deleted.notes {
            println!("{note}");
        }
    } else if let Some(paths) = &paths {
        println!("kept the checkout {}", paths.path.display());
    }
    if let Some(note) = note {
        println!("{note}");
    }
    Ok(0)
}
