//! `af project`: show the Project, list its Workspaces, and create, adopt or remove one.
//!
//! Every command first registers the Project and the invoking checkout when they are new, as
//! the lazy registration of the other commands does, but a failure here is the command's own
//! failure: it cannot answer without the Project Store. Text goes to stdout, `--json` prints one
//! versioned document, and nothing is written inside a checkout.

use super::{Checkout, Log, ProjectStore, Registered, WorkspacePaths, adopt, require_checkout};
use crate::cli::{ProjectCommand, WorkspaceCommand};
use af_contracts::project::SurveyRecordedPayloadV1;
use af_store::{Cas, EventStore};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
    af_contracts::task::collection::collected_time(unix_ms)
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

/// `af project history` once `project::survey::history` verified the chain: every Survey
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

/// Create one Workspace and present its registered paths.
fn new(
    label: &str,
    branch: Option<String>,
    path: Option<PathBuf>,
    from: Option<&str>,
    json_output: bool,
) -> Result<i32, String> {
    let (checkout, mut store) = open_here()?;
    let created = match af_project::workspace::create(
        &super::Application,
        &checkout,
        &mut store,
        af_project::workspace::NewWorkspace {
            label,
            branch,
            path,
            from,
        },
    ) {
        Ok(created) => created,
        Err(af_project::workspace::Refused::Usage(message)) => {
            eprintln!(
                "error: {message}; nothing was created\n  try: af project workspace new --help"
            );
            return Ok(2);
        }
        Err(af_project::workspace::Refused::Failure(message)) => return Err(message),
    };
    if json_output {
        let log = store.log()?;
        print_json(&workspaces_document(&store, &log, &store.paths()?))?;
        return Ok(0);
    }
    let short = &created.commit[..created.commit.len().min(12)];
    let directory = &created.directory;
    println!("created Workspace {label}");
    println!(
        "  path      {} (branch {} at {short})",
        created.checkout.toplevel.display(),
        created.branch
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
    let registration = af_project::workspace::add(
        &super::Application,
        &checkout,
        &mut store,
        path,
        af_project::workspace::AdoptWorkspace {
            label,
            state,
            bindings,
            tasks,
        },
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
    let removed = af_project::workspace::remove(&checkout, &mut store, label, delete_worktree)?;
    let workspace_id = &removed.workspace_id;
    if removed.already_unregistered {
        if json_output {
            let log = store.log()?;
            print_json(&workspaces_document(&store, &log, &store.paths()?))?;
            eprintln!(
                "af project workspace remove: Workspace `{label}` ({workspace_id}) was already \
                 unregistered; its finished removal record is deleted"
            );
        } else {
            println!("removed Workspace {label} ({workspace_id})");
            println!(
                "it was already unregistered; its finished removal record is deleted, and its \
                 checkout path was not inspected again"
            );
        }
        return Ok(0);
    }
    let paths = removed.paths;
    let deleted = removed.deleted;
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
    println!("removed Workspace {label} ({workspace_id})");
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
