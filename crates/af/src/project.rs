//! Project CLI adapters and machine-specific service wiring.

use std::path::{Path, PathBuf};

pub(crate) mod brief;
pub(crate) mod commands;
pub(crate) mod survey;
pub(crate) use af_project::{
    Checkout, Git, Located, Log, ProjectStore, Registered, UNSHALLOW, WorkspacePaths,
    label_from_name, locate, require_checkout, store_directory,
};

#[cfg(test)]
pub(crate) use af_project::{register_in, workspace_id};

/// The application owns XDG/configuration lookup and the machine Store registry.
pub(crate) struct Application;

impl af_project::Host for Application {
    fn state_root(&self) -> Result<PathBuf, String> {
        crate::xdg_state_root()
    }
    fn record_project_store(&self, directory: &Path) {
        crate::storage::registry::record(directory, crate::storage::registry::StoreKind::Project);
    }
    fn default_task_store(&self, checkout: &Path) -> Result<PathBuf, String> {
        crate::task_commands::default_task_state(&crate::xdg_state_root()?, checkout)
    }
    fn resolve_path(&self, path: &Path) -> Result<PathBuf, String> {
        crate::resolve_filesystem_path(path)
    }
    fn valid_project_declaration(&self, text: &str) -> bool {
        crate::project_policy::ProjectFile::parse(text).is_ok()
    }
    fn configuration_table(
        &self,
        checkout: &Path,
        key: &str,
    ) -> Result<Option<toml::Value>, String> {
        crate::config::load(Some(checkout)).map(|config| config.effective.get(key).cloned())
    }
    fn warning(&self, message: &str) {
        eprintln!("{message}");
    }
}

pub(crate) fn adopt(
    checkout: &Checkout,
    state: Option<&Path>,
) -> Result<(ProjectStore, af_project::Registration), String> {
    af_project::adopt(&Application, checkout, state)
}

/// What a command that registers lazily registers: the checkout of `repo`, with an explicit
/// `--state` when one was given.
#[derive(Debug, Clone)]
pub(crate) struct Lazy {
    pub(crate) repo: PathBuf,
    pub(crate) state: Option<PathBuf>,
    /// Register before the command runs rather than after: `af task plan|start`, whose
    /// `--workspace` resolves against the registered Workspaces, so the first `--workspace
    /// <checkout>` in a checkout without a Project Store finds that checkout.
    pub(crate) first: bool,
}

/// The lazy registration of `af onboard --apply`, `af task plan|start` and `af review run`:
/// best effort, token-free, never inside the checkout. Silent unless it fails or meets a
/// shallow clone, which it reports in one line on stderr; the command's result is unchanged.
pub(crate) fn register_lazily(lazy: &Lazy) {
    let outcome = match locate(&lazy.repo) {
        Ok(Located::Checkout(checkout)) => adopt(&checkout, lazy.state.as_deref()).map(|_| ()),
        Ok(Located::NotGit | Located::NoCommit) => Ok(()),
        Ok(Located::Shallow) => Err(UNSHALLOW.to_string()),
        Err(error) => Err(error),
    };
    if let Err(error) = outcome {
        eprintln!("af: the Project was not registered: {error}");
    }
}

pub(crate) fn register_delivered(worktree: &Path, task_id: &str) {
    if let Err(error) = af_project::register_delivered(&Application, worktree, task_id) {
        eprintln!("af: the delivered worktree was not registered as a Workspace: {error}");
    }
}

fn select(repo: &Path, label: &str) -> Result<af_project::Selected, String> {
    af_project::select(&Application, repo, label)
}

/// Apply `--workspace` to an `af task` command: its Store becomes `--state`, and on `plan` and
/// `start` its bindings file becomes `--bindings` when none was given. clap already refused
/// `--workspace` beside `--state`.
pub(crate) fn resolve_task_workspace(
    mut command: crate::cli::TaskCommand,
) -> Result<crate::cli::TaskCommand, String> {
    use crate::cli::TaskCommand as T;
    let selector: Option<(&Path, &mut Option<String>, &mut Option<PathBuf>)> = match &mut command {
        T::Start {
            repo,
            state,
            workspace,
            bindings,
            ..
        }
        | T::Plan {
            repo,
            state,
            workspace,
            bindings,
            ..
        } => {
            if let Some(label) = workspace.take() {
                let selected = select(repo, &label)?;
                *state = Some(selected.state);
                if bindings.is_none() {
                    *bindings = selected.bindings;
                }
            }
            None
        }
        // A continuation plans a new Task, so the Workspace's bindings come with its Store.
        T::Run {
            inspect, bindings, ..
        } => {
            if let Some(label) = inspect.workspace.take() {
                let selected = select(&inspect.repo, &label)?;
                inspect.state = Some(selected.state);
                if bindings.is_none() {
                    *bindings = selected.bindings;
                }
            }
            None
        }
        T::Deliver {
            repo,
            state,
            workspace,
            ..
        } => Some((repo.as_path(), workspace, state)),
        T::Refresh { inspect, .. }
        | T::DecisionPayload { inspect, .. }
        | T::Output { inspect, .. }
        | T::Approve { inspect, .. }
        | T::Reject { inspect, .. }
        | T::Explain { inspect, .. }
        | T::Export { inspect, .. }
        | T::ObserveAdoption { inspect, .. }
        | T::List { inspect, .. }
        | T::Gc { inspect, .. }
        | T::Show { inspect, .. }
        | T::Report { inspect, .. }
        | T::Wait { inspect, .. }
        | T::Status { inspect, .. }
        | T::Cancel { inspect, .. }
        | T::Findings { inspect, .. }
        | T::Materialize { inspect, .. }
        | T::Diff { inspect, .. } => Some((
            inspect.repo.as_path(),
            &mut inspect.workspace,
            &mut inspect.state,
        )),
        T::Events { store, .. } => {
            Some((store.repo.as_path(), &mut store.workspace, &mut store.state))
        }
    };
    if let Some((repo, workspace, state)) = selector
        && let Some(label) = workspace.take()
    {
        *state = Some(select(repo, &label)?.state);
    }
    Ok(command)
}

/// What a command registers, if anything, and whether before or after it runs.
pub(crate) fn lazy_target(command: &crate::cli::Command) -> Option<Lazy> {
    use crate::cli::{Command as C, ReviewCommand as R, TaskCommand as T};
    match command {
        // After `--apply`, so the Project records the `.af/af.toml` onboarding wrote.
        C::Onboard(args) if args.apply => Some(Lazy {
            repo: args.repo.clone(),
            state: None,
            first: false,
        }),
        C::Task {
            command: T::Start { repo, state, .. } | T::Plan { repo, state, .. },
        } => Some(Lazy {
            repo: repo.clone(),
            state: state.clone(),
            first: true,
        }),
        C::Review(namespace) => match &namespace.command {
            None => Some(Lazy {
                repo: namespace.run.repo.clone(),
                state: None,
                first: false,
            }),
            Some(R::Run(args)) => Some(Lazy {
                repo: args.repo.clone(),
                state: None,
                first: false,
            }),
            Some(_) => None,
        },
        _ => None,
    }
}

/// The worktree and Task ID of an `af task deliver`, for registration after it succeeded.
pub(crate) fn delivered_target(command: &crate::cli::Command) -> Option<(PathBuf, String)> {
    match command {
        crate::cli::Command::Task {
            command:
                crate::cli::TaskCommand::Deliver {
                    worktree, task_id, ..
                },
        } => crate::resolve_filesystem_path(worktree)
            .ok()
            .map(|worktree| (worktree, task_id.clone())),
        _ => None,
    }
}
