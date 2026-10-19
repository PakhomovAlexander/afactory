//! `af project`: show the Project, list its Workspaces, and create, adopt or remove one.
//!
//! Every command first registers the Project and the invoking checkout when they are new, as
//! the lazy registration of the other commands does, but a failure here is the command's own
//! failure: it cannot answer without the Project Store. Text goes to stdout, `--json` prints one
//! versioned document, and nothing is written inside a checkout.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use review_core::project::WorkspaceOriginV1;
use review_store::{Cas, EventStore};
use serde_json::{Value, json};

use super::{
    Checkout, Located, Log, ProjectStore, Registered, UNSHALLOW, WorkspacePaths, adopt,
    default_store, label_from_name, locate, register_in, require_checkout, run_git, workspace_id,
};
use crate::cli::{ProjectCommand, WorkspaceCommand};

pub(crate) fn run(command: ProjectCommand) -> Result<i32, String> {
    match command {
        ProjectCommand::Show { json } => show(json),
        ProjectCommand::Workspaces { json } => workspaces(json),
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

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

fn new(
    label: &str,
    branch: Option<String>,
    path: Option<PathBuf>,
    from: Option<&str>,
    json_output: bool,
) -> Result<i32, String> {
    let (checkout, mut store) = open_here()?;
    let log = store.log()?;
    if let Some(holder) = log.by_label(label) {
        return Err(format!(
            "the label `{label}` is taken by Workspace {}; choose another label",
            holder.workspace_id
        ));
    }
    let directory = store.workspace_directory(label);
    if std::fs::symlink_metadata(&directory).is_ok() {
        return Err(format!(
            "{} already exists from an earlier Workspace `{label}`; choose another label, or move \
             that directory away first",
            directory.display()
        ));
    }
    let worktree = match path {
        Some(path) => crate::resolve_filesystem_path(&path)?,
        None => {
            let name = checkout
                .toplevel
                .file_name()
                .ok_or("the checkout has no directory name")?;
            let parent = checkout
                .toplevel
                .parent()
                .ok_or("the checkout has no parent directory; name one with --path")?;
            parent.join(format!("{}-wt-{label}", name.to_string_lossy()))
        }
    };
    if std::fs::symlink_metadata(&worktree).is_ok() {
        return Err(format!(
            "{} already exists; name another place with --path",
            worktree.display()
        ));
    }
    let branch = branch.unwrap_or_else(|| format!("af/{label}"));
    run_git(
        &checkout.toplevel,
        &[os("check-ref-format"), os("--branch"), os(&branch)],
    )
    .map_err(|_| format!("`{branch}` is not a valid branch name; name another with --branch"))?;
    let reference = format!("refs/heads/{branch}");
    if run_git(
        &checkout.toplevel,
        &[
            os("rev-parse"),
            os("--verify"),
            os("--quiet"),
            os(&reference),
        ],
    )
    .is_ok()
    {
        return Err(format!(
            "branch `{branch}` already exists; name another with --branch"
        ));
    }
    let from = from.unwrap_or("HEAD");
    let commit = format!("{from}^{{commit}}");
    let start = run_git(
        &checkout.toplevel,
        &[os("rev-parse"), os("--verify"), os("--quiet"), os(&commit)],
    )
    .map_err(|_| format!("`{from}` names no commit in this repository; name one with --from"))?;

    // The Workspace directory first, so a refusal from git leaves nothing behind.
    let state = directory.join("state");
    let tasks = directory.join("tasks");
    let bindings = directory.join("bindings.toml");
    let created = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(directory.parent().unwrap_or(&directory))?;
        std::fs::create_dir(&directory)?;
        std::fs::create_dir(&state)?;
        std::fs::create_dir(&tasks)?;
        std::fs::write(&bindings, bindings_template(label))
    })();
    if let Err(error) = created {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(format!("creating {}: {error}", directory.display()));
    }
    if let Err(error) = run_git(
        &checkout.toplevel,
        &[
            os("worktree"),
            os("add"),
            os("-b"),
            os(&branch),
            worktree.as_os_str(),
            os(&start),
        ],
    ) {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(format!("{error}; nothing was created"));
    }
    let created = match locate(&worktree)? {
        Located::Checkout(created) if created.project_id == checkout.project_id => created,
        Located::Checkout(created) => {
            return Err(format!(
                "the worktree {} was created on branch `{branch}`, but `{from}` belongs to \
                 Project {}, not {}, so it was not registered; remove it with `git worktree \
                 remove`",
                worktree.display(),
                created.project_id,
                checkout.project_id
            ));
        }
        _ => {
            return Err(format!(
                "the worktree {} was created, but git does not see a checkout there",
                worktree.display()
            ));
        }
    };
    let paths = WorkspacePaths {
        id: workspace_id(&checkout.project_id, &created.toplevel),
        label: label.to_string(),
        path: created.toplevel.clone(),
        state: state.clone(),
        bindings: Some(bindings.clone()),
        tasks: Some(tasks.clone()),
    };
    register_in(
        &mut store,
        &checkout,
        WorkspaceOriginV1::Created,
        label,
        true,
        paths,
    )?;
    if json_output {
        let log = store.log()?;
        print_json(&workspaces_document(&store, &log, &store.paths()?))?;
        return Ok(0);
    }
    let short = &start[..start.len().min(12)];
    println!("created Workspace {label}");
    println!(
        "  path      {} (branch {branch} at {short})",
        created.toplevel.display()
    );
    println!("  store     {}", state.display());
    println!("  bindings  {}", bindings.display());
    println!("  tasks     {}", tasks.display());
    println!(
        "Plan a Task in it from the worktree: af task plan --workspace {label} --file {}",
        tasks.join("TASK.json").display()
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
            label_from_name(
                &target
                    .toplevel
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
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
    let mut deleted = None;
    if delete_worktree {
        let path = paths.as_ref().map(|paths| paths.path.clone()).ok_or_else(|| {
            format!(
                "the checkout of Workspace `{label}` is unknown on this machine, so nothing was \
                 removed; run without --delete-worktree"
            )
        })?;
        if path == checkout.toplevel {
            return Err(format!(
                "Workspace `{label}` is the checkout this command runs in; run the command from \
                 another checkout of the Project"
            ));
        }
        run_git(
            &checkout.toplevel,
            &[os("worktree"), os("remove"), path.as_os_str()],
        )
        .map_err(|error| {
            format!(
                "{error}; nothing was changed: commit or discard the worktree's changes, or \
                 remove the Workspace without --delete-worktree"
            )
        })?;
        deleted = Some(path);
    }
    store.unregister(&workspace.workspace_id)?;
    let note = paths.as_ref().map(|paths| store_note(&paths.state));
    if json_output {
        let log = store.log()?;
        print_json(&workspaces_document(&store, &log, &store.paths()?))?;
        if let Some(note) = note {
            eprintln!("af project workspace remove: {note}");
        }
        return Ok(0);
    }
    println!("removed Workspace {label} ({})", workspace.workspace_id);
    if let Some(path) = deleted {
        println!(
            "removed the worktree {}; its branch is kept",
            path.display()
        );
    } else if let Some(paths) = &paths {
        println!("kept the checkout {}", paths.path.display());
    }
    if let Some(note) = note {
        println!("{note}");
    }
    Ok(0)
}
