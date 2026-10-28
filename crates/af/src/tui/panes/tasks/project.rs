//! The Project's other checkouts, whose default Task Stores the project scope lists beside
//! this checkout's own (docs/design/tui.md section 5.5): every Workspace the Project Store
//! registers (ADR-0158) and every worktree `git worktree list` names, each by its default Task
//! Store when that Store exists on disk. Read on the Tasks pane's thread, never by a load
//! (ADR-0240): the Project Store is opened read-only under its shared lock, and git runs
//! read-only plumbing under the deadlines `af project` gives it.

use std::path::{Path, PathBuf};

use crate::projects::{self, Located, ProjectStore};
use crate::task_execution;

/// The default Task Store of one checkout of the Project other than this one.
pub(super) struct Checkout {
    pub(super) dir: PathBuf,
    /// Its Workspace label, or its directory name as a label when no Workspace is registered
    /// for it.
    pub(super) name: String,
    pub(super) registered: bool,
}

/// What one discovery found.
pub(super) struct Project {
    /// The other checkouts' Stores: the registered Workspaces in label order, then the
    /// worktrees no Workspace names, by name. Each Store once, and never this checkout's own.
    pub(super) checkouts: Vec<Checkout>,
    /// The Workspaces that name a `--state` Store, by Store: what the folder says beside a
    /// Store the registry lists.
    pub(super) named: Vec<(PathBuf, Vec<String>)>,
    /// This checkout's Workspace label, when it is registered with its default Store.
    pub(super) label: Option<String>,
    /// Why the registered Workspaces are not listed: the Project Store cannot be read.
    pub(super) warning: Option<String>,
}

/// The Project of the checkout at `toplevel`, read from the Project Store below `state` and
/// from git. An error is git's: a checkout that is no repository, has no commit, or is a
/// shallow clone has no Project to list and answers with nothing.
pub(super) fn discover(state: &Path, toplevel: &Path) -> Result<Project, String> {
    let toplevel = std::fs::canonicalize(toplevel)
        .map_err(|error| format!("{}: {error}", toplevel.display()))?;
    let own = task_execution::default_task_state(state, &toplevel)?;
    let mut project = Project {
        checkouts: Vec::new(),
        named: Vec::new(),
        label: None,
        warning: None,
    };
    let located = projects::locate(&toplevel)?;
    if located == Located::NotGit {
        return Ok(project);
    }
    let mut listed = vec![own.clone()];
    if let Located::Checkout(checkout) = &located {
        match workspaces(state, &checkout.project_id) {
            Ok(Some((log, paths))) => {
                let mut registered: Vec<_> = log
                    .workspaces
                    .values()
                    .filter_map(|workspace| {
                        let paths = paths.iter().find(|p| p.id == workspace.workspace_id)?;
                        Some((workspace.label.clone(), paths.state.clone()))
                    })
                    .collect();
                registered.sort();
                let local = task_execution::local_task_states(state);
                for (label, store) in registered {
                    if super::same_store(&store, &own) {
                        project.label = Some(label);
                    } else if !store.starts_with(&local) {
                        // A `--state` Store: the registry lists it; the Workspace is noted.
                        match project
                            .named
                            .iter_mut()
                            .find(|(dir, _)| super::same_store(dir, &store))
                        {
                            Some((_, labels)) => labels.push(label),
                            None => project.named.push((store, vec![label])),
                        }
                    } else if exists(&store)
                        && !listed.iter().any(|dir| super::same_store(dir, &store))
                    {
                        listed.push(store.clone());
                        project.checkouts.push(Checkout {
                            dir: store,
                            name: label,
                            registered: true,
                        });
                    }
                }
            }
            Ok(None) => {}
            Err(warning) => project.warning = Some(warning),
        }
    }
    let git = projects::Git::open(&toplevel)?;
    let output = git.run(["worktree", "list", "--porcelain"], None)?;
    if !output.status.success() {
        return Err(format!(
            "git worktree list: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut worktrees = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        let Ok(path) = std::fs::canonicalize(path) else {
            continue;
        };
        if path == toplevel {
            continue;
        }
        let dir = task_execution::default_task_state(state, &path)?;
        if !exists(&dir) || listed.iter().any(|known| super::same_store(known, &dir)) {
            continue;
        }
        listed.push(dir.clone());
        let name = projects::label_from_name(path.file_name().unwrap_or_default());
        worktrees.push(Checkout {
            dir,
            name,
            registered: false,
        });
    }
    worktrees.sort_by(|a, b| a.name.cmp(&b.name));
    project.checkouts.extend(worktrees);
    Ok(project)
}

/// The Project Store's registered Workspaces and their paths on this machine; `None` when no
/// Project Store was ever made for the Project. The Store is opened to be read and nothing
/// else, under its shared lock, for as long as this takes.
fn workspaces(
    state: &Path,
    project_id: &str,
) -> Result<Option<(projects::Log, Vec<projects::WorkspacePaths>)>, String> {
    let directory = projects::store_directory(state, project_id);
    if !exists(&directory.join("project.lock")) {
        return Ok(None);
    }
    let unreadable = |error: String| format!("the Project's Workspaces are not listed: {error}");
    let store = ProjectStore::open_read_only(state, project_id).map_err(unreadable)?;
    let log = store.log().map_err(unreadable)?;
    let paths = store.paths().map_err(unreadable)?;
    Ok(Some((log, paths)))
}

/// Whether anything is at `path`: a Store directory that was never made holds no Task and is
/// not listed; a link to nothing is, so the pane shows its refusal.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}
