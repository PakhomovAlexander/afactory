//! `af project`: show the Project, list its Workspaces, and create, adopt or remove one.
//!
//! Every command first registers the Project and the invoking checkout when they are new, as
//! the lazy registration of the other commands does, but a failure here is the command's own
//! failure: it cannot answer without the Project Store. Text goes to stdout, `--json` prints one
//! versioned document, and nothing is written inside a checkout.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use review_core::project::WorkspaceOriginV1;
use review_source_git::{Capture, Snapshot};
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
        ProjectCommand::Refresh { json } => refresh(json),
        ProjectCommand::History { json } => history(json),
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

fn refresh(json_output: bool) -> Result<i32, String> {
    let (checkout, store) = open_here()?;
    // Released while the head is measured; the Surveyor opens the Store again to record.
    drop(store);
    let (word, recorded) = match super::survey::refresh(&checkout)? {
        super::survey::Refreshed::Recorded(recorded) => ("recorded", recorded),
        super::survey::Refreshed::Unchanged(recorded) => ("unchanged", recorded),
    };
    if json_output {
        if word == "unchanged" {
            eprintln!("unchanged: the head, policy and Surveyor equal the newest Survey's");
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

fn history(json_output: bool) -> Result<i32, String> {
    let (_, store) = open_here()?;
    let log = store.log()?;
    let chain = log.chain();
    if json_output {
        print_json(&json!({
            "schema": "af/project-history@1",
            "project_id": store.project_id,
            "surveys": chain
                .iter()
                .map(|recorded| json!({
                    "survey_id": recorded.survey_id,
                    "recorded_unix_ms": recorded.recorded_unix_ms.get().to_string(),
                    "head": recorded.head,
                    "cutoff_unix_s": recorded.cutoff_unix_s.get().to_string(),
                    "completeness": recorded.completeness.as_str(),
                }))
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
    /// Directories git created above the worktree: `from` is the highest, `to` the worktree's
    /// parent.
    Ancestors {
        from: PathBuf,
        to: PathBuf,
    },
    /// The worktree at `path`, with the names `administrative` (`<common dir>/worktrees`) held
    /// before `git worktree add` ran.
    Worktree {
        path: PathBuf,
        administrative: PathBuf,
        before: BTreeSet<OsString>,
    },
    Registration(String),
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

/// The highest ancestor of `path` that does not exist yet, when its parent does not.
fn first_absent_ancestor(path: &Path) -> Option<PathBuf> {
    let mut highest = None;
    let mut current = path.parent();
    while let Some(directory) = current {
        if std::fs::symlink_metadata(directory).is_ok() {
            break;
        }
        highest = Some(directory.to_path_buf());
        current = directory.parent();
    }
    highest
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
    if let (Some(from), Some(to)) = (first_absent_ancestor(worktree), worktree.parent()) {
        made.push(Made::Ancestors {
            from,
            to: to.to_path_buf(),
        });
    }
    // Recorded before git runs: an add that fails or is stopped can leave a partial worktree, or
    // only its administrative entry, which the names held before tell apart from older ones.
    let administrative = git.common_dir()?.join("worktrees");
    let before = administrative_names(&administrative)?;
    made.push(Made::Worktree {
        path: worktree.clone(),
        administrative,
        before,
    });
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
fn administrative_names(administrative: &Path) -> Result<BTreeSet<OsString>, String> {
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
/// and so is left where it is.
fn left_alone(administrative: &Path, entry: &NewEntry) -> String {
    let location = administrative.join(&entry.name);
    match &entry.gitdir {
        // `git worktree prune` keeps a locked entry, so it is no way to remove one.
        None if entry.locked => format!(
            "{}: a locked entry without a gitdir, created during this add and possibly another \
             add's; remove it once no add is running by deleting that directory, since `git \
             worktree prune` keeps a locked entry",
            location.display()
        ),
        None => format!(
            "{}: an entry without a gitdir, created during this add and possibly another add's; \
             remove it with `git worktree prune` once no add is running",
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

/// Remove whatever exists of the worktree `workspace new` was adding at `path`: its directory,
/// tolerating its absence, and every administrative entry positively this add's, both deleted
/// directly. `git worktree prune` never runs: it would also remove stale entries of other
/// worktrees, beyond this add's ownership (ADR-0031). What was removed, empty when nothing of it
/// existed, and the new entries named after `path` that were left alone because they are not
/// attributably this add's; the error says what remains.
fn remove_worktree(
    git: &Git,
    path: &Path,
    administrative: &Path,
    before: &BTreeSet<OsString>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let found = new_entries(path, administrative, before)?;
    let (entries, others): (Vec<_>, Vec<_>) = found
        .into_iter()
        .partition(|entry| owns_entry(path, &entry.name, entry.gitdir.as_deref()));
    let entries: Vec<OsString> = entries.into_iter().map(|entry| entry.name).collect();
    let directory = std::fs::symlink_metadata(path).is_ok();
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
            return Err(match cleared {
                Ok(()) => listing,
                Err(error) => format!("{error}; {listing}"),
            });
        }
    };
    match cleared {
        Ok(()) if remaining.is_empty() => {}
        Ok(()) => {
            return Err(format!(
                "its administrative entry {} remains in {}",
                entry_names(&remaining),
                administrative.display()
            ));
        }
        Err(error) if remaining.is_empty() => return Err(error),
        Err(error) => {
            return Err(format!(
                "{error}; its administrative entry {} remains in {}",
                entry_names(&remaining),
                administrative.display()
            ));
        }
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
    let left = others
        .iter()
        .map(|entry| left_alone(administrative, entry))
        .collect();
    Ok((removed, left))
}

/// Undo what `workspace new` made, newest first, and say what was undone and what, if an undo
/// failed, remains.
pub(super) fn undo(git: &Git, store: &mut ProjectStore, made: Vec<Made>) -> String {
    let mut undone = Vec::new();
    let mut remains = Vec::new();
    for step in made.into_iter().rev() {
        match step {
            Made::Registration(id) => match store.unregister(&id) {
                Ok(()) => undone.push(format!("unregistered Workspace {id}")),
                Err(error) => remains.push(format!(
                    "the registration of Workspace {id} ({error}; remove it with `af project \
                     workspace remove`)"
                )),
            },
            Made::Worktree {
                path,
                administrative,
                before,
            } => match remove_worktree(git, &path, &administrative, &before) {
                // Empty when git made nothing of it: nothing to undo or report.
                Ok((removed, left)) => {
                    undone.extend(removed);
                    remains.extend(left);
                }
                Err(error) => remains.push(format!(
                    "the worktree {} ({error}; remove it with `git worktree remove --force \
                     --force`, or remove the directory and run `git worktree prune`)",
                    path.display()
                )),
            },
            Made::Ancestors { from, to } => {
                let mut current = Some(to.as_path());
                let mut removed = false;
                let mut failed = None;
                while let Some(directory) = current {
                    match std::fs::remove_dir(directory) {
                        Ok(()) => removed = true,
                        // A directory git never created needs no undo.
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            failed = Some(format!("{} ({error})", directory.display()));
                            break;
                        }
                    }
                    if directory == from {
                        break;
                    }
                    current = directory.parent();
                }
                match failed {
                    None if removed => {
                        undone.push(format!("removed the directory {}", from.display()));
                    }
                    None => {}
                    Some(failed) => remains.push(format!("the directory {failed}")),
                }
            }
            Made::Branch { reference, commit } => {
                match git.require(["update-ref", "-d", &reference, &commit], None) {
                    Ok(_) => undone.push(format!("deleted the branch {reference}")),
                    Err(error) => remains.push(format!(
                        "the branch {reference} ({error}; delete it with `git update-ref -d \
                         {reference}`)"
                    )),
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

/// The administrative entry of the linked worktree at `path` (canonical): the directory under
/// `<common dir>/worktrees/` that its `.git` file names and whose `gitdir` names `<path>/.git`.
fn own_entry(git: &Git, path: &Path) -> Result<PathBuf, String> {
    let not_linked = |why: String| {
        format!(
            "{} is not a linked worktree of this repository whose administrative entry names it \
             ({why})",
            path.display()
        )
    };
    let dot_git = path.join(".git");
    let text = match std::fs::symlink_metadata(&dot_git) {
        Ok(metadata) if metadata.is_file() => std::fs::read(&dot_git)
            .map_err(|error| format!("reading {}: {error}", dot_git.display()))?,
        _ => return Err(not_linked("its .git is no file".into())),
    };
    let named = text
        .strip_prefix(b"gitdir: ")
        .map(|rest| rest.strip_suffix(b"\n").unwrap_or(rest))
        .ok_or_else(|| not_linked("its .git file names no gitdir".into()))?;
    let resolve = |base: &Path, bytes: &[u8]| {
        let named = Path::new(<OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(bytes));
        std::fs::canonicalize(base.join(named)).ok()
    };
    let administrative = std::fs::canonicalize(git.common_dir()?.join("worktrees"))
        .map_err(|error| not_linked(format!("the repository has no worktrees/: {error}")))?;
    let entry = resolve(path, named)
        .filter(|entry| entry.parent() == Some(administrative.as_path()))
        .ok_or_else(|| not_linked("its .git file names no entry under worktrees/".into()))?;
    let gitdir = std::fs::read(entry.join("gitdir"))
        .map_err(|error| not_linked(format!("{}: {error}", entry.join("gitdir").display())))?;
    let gitdir = gitdir.strip_suffix(b"\n").unwrap_or(&gitdir);
    if resolve(&entry, gitdir).as_deref() != Some(dot_git.as_path()) {
        return Err(not_linked(format!(
            "the gitdir of {} names another path",
            entry.display()
        )));
    }
    Ok(entry)
}

/// The first path at which the files below `worktree` differ from `snapshot`, in content, mode
/// or presence; git's own `.git` at the top is no file of the worktree. `None` when they equal.
fn first_difference(worktree: &Path, snapshot: &Snapshot) -> Result<Option<String>, String> {
    let scanned = review_source_git::scan_tree(worktree)
        .map_err(|error| format!("reading the worktree {}: {error}", worktree.display()))?;
    let mut on_disk = scanned
        .entries
        .iter()
        .filter(|entry| entry.path != ".git")
        .peekable();
    let mut committed = snapshot.manifest.entries.iter().peekable();
    loop {
        match (committed.peek(), on_disk.peek()) {
            (None, None) => return Ok(None),
            (Some(want), Some(have)) if want.path == have.path => {
                if (want.kind, &want.content) != (have.kind, &have.content) {
                    return Ok(Some(want.path.clone()));
                }
                committed.next();
                on_disk.next();
            }
            (Some(want), Some(have)) => {
                return Ok(Some(
                    if want.path.as_bytes() < have.path.as_bytes() {
                        want
                    } else {
                        have
                    }
                    .path
                    .clone(),
                ));
            }
            (Some(only), None) | (None, Some(only)) => return Ok(Some(only.path.clone())),
        }
    }
}

/// Delete the linked worktree at `path` when its files equal its `HEAD` commit's tree, compared
/// through the committed capture `workspace new` materializes from, so no filter, hook or other
/// program the repository configures runs. Never `git worktree remove`, whose cleanliness check
/// runs the repository's clean filters (ADR-0001): the directory and the worktree's own
/// administrative entry are deleted directly, and nothing else. A worktree that differs is
/// refused with the first differing path and left as it was.
fn clean_delete(git: &Git, path: &Path) -> Result<(), String> {
    let entry = own_entry(git, path)?;
    if std::fs::symlink_metadata(entry.join("locked")).is_ok() {
        return Err(format!(
            "the worktree {} is locked; unlock it with `git worktree unlock` first",
            path.display()
        ));
    }
    let scratch = tempfile::Builder::new()
        .prefix("af-workspace-remove-")
        .tempdir()
        .map_err(|error| format!("creating a scratch directory: {error}"))?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    let snapshot = Capture::new(&git.at(path).repo(), &cas)
        .committed("HEAD")
        .map_err(|error| format!("capturing the HEAD of {}: {error}", path.display()))?;
    if let Some(differing) = first_difference(path, &snapshot)? {
        return Err(format!(
            "the worktree {} differs from its commit {} at `{}`",
            path.display(),
            snapshot.source_revision.as_deref().unwrap_or("HEAD"),
            review_source_git::fs_path(&differing).display()
        ));
    }
    std::fs::remove_dir_all(path)
        .map_err(|error| format!("removing the worktree {}: {error}", path.display()))?;
    std::fs::remove_dir_all(&entry).map_err(|error| {
        format!(
            "the worktree {} was removed, but its administrative entry {} remains ({error}); \
             remove it with `git worktree prune`",
            path.display(),
            entry.display()
        )
    })
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
        clean_delete(&Git::open(&checkout.toplevel)?, &path).map_err(|error| {
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
