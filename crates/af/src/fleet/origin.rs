use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Instant;

use af_contracts::fleet::{
    Freshness, ORIGIN_OBSERVATION_V1, OriginObservationV1, ProjectOrigin, Reading,
};
use af_contracts::is_digest;
use af_contracts::task::{TaskWaitingReasonV1, is_name};
use af_project::ProjectStore;
use af_store::{EventStore, StoreError};
use clap::Args;
use nix::fcntl::{Flock, FlockArg};

use super::context::{Config, Origin};
use super::transport::{ORIGIN, ReadFailure};
use super::{is_label, now_ms, observation, unavailable};

const MAX_AGE_MS: u64 = 5 * 60 * 1000;
const CLOCK_SKEW_MS: u64 = 60 * 1000;

/// These arguments are set by the Project owner's forced command on the server.
#[derive(Debug, Args)]
pub(crate) struct ReaderArgs {
    /// Infrastructure scope declared by the owner
    #[arg(long)]
    scope: String,
    /// Origin label declared by the owner
    #[arg(long)]
    origin: String,
    /// Exact native Project ID
    #[arg(long)]
    project: String,
    /// Registered Workspace label
    #[arg(long)]
    workspace: String,
    /// Principal authenticated by the server's SSH key authorization
    #[arg(long)]
    principal: String,
    /// Allow this principal's configured native plan Decisions to be projected
    #[arg(long)]
    allow_plan_decisions: bool,
    /// Absolute XDG state home that holds the native Project
    #[arg(long)]
    state_home: PathBuf,
}

pub(super) fn serve(args: ReaderArgs) -> Result<i32, String> {
    if !is_label(&args.scope)
        || !is_label(&args.origin)
        || !is_name(&args.principal)
        || !is_digest(&args.project)
        || !args.state_home.is_absolute()
    {
        return Err("Invalid Project reader configuration".into());
    }
    let runtime = ObservationRuntime::create(&args.state_home)?;
    let project = ProjectStore::open_origin_snapshot(&args.state_home, &args.project, |path| {
        runtime.snapshot(path)
    })?;
    let log = project.log()?;
    if log.project.as_ref().map(|p| p.project_id.as_str()) != Some(&args.project) {
        return Err("Project reader identity mismatch".into());
    }
    let workspace = log
        .by_label(&args.workspace)
        .ok_or("Workspace is not registered")?;
    let state = project
        .paths()?
        .into_iter()
        .find(|paths| paths.id == workspace.workspace_id && paths.label == workspace.label)
        .ok_or("Registered Workspace has no Task Store binding")?
        .state;
    let store_id = format!(
        "sha256:{}",
        af_telemetry::trace::store_id(&state).map_err(|e| e.to_string())?
    );
    let now = now_ms()?;
    let tasks = crate::task_commands::fleet_waiting_tasks(
        &state,
        |path| runtime.snapshot(path),
        &args.origin,
        args.allow_plan_decisions.then_some(args.principal.as_str()),
        now,
    )?;
    #[cfg(debug_assertions)]
    pause_with_snapshot();
    let response = OriginObservationV1 {
        schema: ORIGIN_OBSERVATION_V1.into(),
        scope: args.scope,
        observed_unix_ms: now,
        origin: ProjectOrigin {
            origin: args.origin,
            project_id: args.project,
            store_id,
            principal: args.principal,
            tasks,
        },
    };
    let bytes = serde_json::to_string(&response).map_err(|error| error.to_string())?;
    if bytes.len() >= super::transport::MAX_REPLY_BYTES {
        return Err("Project observation exceeds its output bound".into());
    }
    drop(project);
    drop(runtime);
    println!("{bytes}");
    Ok(0)
}

struct ObservationRuntime<'a> {
    policy: crate::config::StoragePolicy,
    admission: PathBuf,
    origin_state_home: &'a Path,
    directory: tempfile::TempDir,
}

impl<'a> ObservationRuntime<'a> {
    fn create(origin_state_home: &'a Path) -> Result<Self, String> {
        let policy = crate::storage::policy()?;
        let account_home = crate::config::machine_account_home()?
            .ok_or("Cannot determine the OS account home for observation admission")?;
        let admission = crate::config::canonical_root(&account_home.join(".local/state"))
            .join("af-observation-admission");
        let root = af_sandbox::stale::observation_root(&crate::config::canonical_root(
            &crate::storage::sandbox_root(&policy),
        ));
        let prepare = || {
            af_store::local_fs::create_private_dir_all(&admission)?;
            let _lock = admission_lock(&admission)?;
            af_store::local_fs::create_private_dir_all(&root)?;
            open_private_directory(&root)?;
            let link = admission.join(runtime_key(&root));
            match symlink(&root, &link) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::AlreadyExists
                        && fs::read_link(&link).is_ok_and(|target| target == root) => {}
                Err(error) => return Err(error),
            }
            af_sandbox::stale::observation_directory(&root)
        };
        let directory =
            prepare().map_err(|e| format!("Cannot register observation runtime: {e}"))?;
        Ok(Self {
            policy,
            admission,
            origin_state_home,
            directory,
        })
    }

    fn snapshot(&self, path: &Path) -> Result<EventStore, StoreError> {
        // Hold one account-wide lock through measurement and the copy. Once released, the
        // registered root exposes the actual allocated bytes throughout replay.
        let invalid = |error: io::Error| StoreError::Conflict(error.to_string());
        let _copy = admission_lock(&self.admission).map_err(invalid)?;
        let runtimes = retained_roots(&self.admission).map_err(invalid)?;
        EventStore::open_observation_snapshot(path, self.directory.path(), |destination| {
            crate::storage::observation_allowance(
                destination,
                &runtimes,
                &self.policy,
                self.origin_state_home,
            )
            .map_err(StoreError::Conflict)
        })
    }
}

fn admission_lock(path: &Path) -> io::Result<Flock<OwnedFd>> {
    Flock::lock(open_private_directory(path)?, FlockArg::LockExclusive)
        .map_err(|(_, error)| error.into())
}

fn open_private_directory(path: &Path) -> io::Result<OwnedFd> {
    use nix::sys::stat::fstat;

    let relative = path.strip_prefix("/").map_err(io::Error::other)?;
    let directory = af_sandbox::open_beneath(af_sandbox::open_anchor(Path::new("/"))?, relative)?;
    let metadata = fstat(&directory)?;
    if metadata.st_uid != nix::unistd::geteuid().as_raw() || metadata.st_mode & 0o077 != 0 {
        return Err(io::Error::other(format!(
            "{} must be a private directory owned by the reader",
            path.display()
        )));
    }
    Ok(directory)
}

fn runtime_key(root: &Path) -> String {
    af_store::canonical::blob_content_id(root.as_os_str().as_encoded_bytes())
}

/// The private registry stores only runtime roots. Each registration is an atomic link, written
/// before the runtime can hold a copy. Failed crash cleanup stays registered and accounted.
fn retained_roots(admission: &Path) -> io::Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for entry in fs::read_dir(admission)? {
        let entry = entry?;
        let root = fs::read_link(entry.path())?;
        if !root.is_absolute()
            || root
                .parent()
                .is_none_or(|parent| af_sandbox::stale::observation_root(parent) != root)
            || entry.file_name() != std::ffi::OsStr::new(&runtime_key(&root))
        {
            return Err(io::Error::other("Invalid observation runtime registration"));
        }
        match open_private_directory(&root) {
            Ok(_) => {
                af_sandbox::stale::sweep_stale_observations_in(&root);
                if fs::read_dir(&root)?.next().transpose()?.is_some() {
                    roots.push(root);
                    continue;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::remove_file(entry.path())?;
    }
    Ok(roots)
}

/// Pause with the live Project snapshot so the crash regression can kill a real reader.
#[cfg(debug_assertions)]
fn pause_with_snapshot() {
    let Some(marker) = std::env::var_os("AF_TEST_FLEET_SNAPSHOT_PAUSE") else {
        return;
    };
    let marker = PathBuf::from(marker);
    if fs::write(&marker, []).is_ok() {
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        while marker.exists() && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

pub(super) fn read(
    config: &Config,
    alias: &str,
    scope: &str,
    name: &str,
    origin: &Origin,
    deadline: Instant,
) -> Reading<ProjectOrigin> {
    let result = origin
        .target
        .read(&config.origin_key(alias, name), ORIGIN, deadline)
        .and_then(|response: OriginObservationV1| {
            let now = now_ms().map_err(|_| ReadFailure::Unavailable)?;
            validate(response, scope, name, origin, now)
        });
    match result {
        Ok(reading) => reading,
        Err(error) => unavailable(name, "waiting_tasks", error),
    }
}

fn validate(
    response: OriginObservationV1,
    scope: &str,
    name: &str,
    origin: &Origin,
    now: u64,
) -> Result<Reading<ProjectOrigin>, ReadFailure> {
    let mut view = response.origin;
    if response.scope != scope
        || view.origin != name
        || view.project_id != origin.project_id
        || view.store_id != origin.store_id
    {
        return Err(ReadFailure::IdentityMismatch);
    }
    if response.schema != ORIGIN_OBSERVATION_V1
        || !is_name(&view.principal)
        || response.observed_unix_ms == 0
        || response.observed_unix_ms > now.saturating_add(CLOCK_SKEW_MS)
    {
        return Err(ReadFailure::InvalidResponse);
    }
    let age = now.saturating_sub(response.observed_unix_ms);
    let freshness = if age > MAX_AGE_MS {
        Freshness::Stale
    } else {
        Freshness::Fresh
    };
    let mut seen = BTreeSet::new();
    for task in &mut view.tasks {
        if !is_name(&task.task_id)
            || !seen.insert(task.task_id.clone())
            || task.observation.source != name
            || task.observation.subject != task.task_id
            || task.observation.observed_unix_ms.is_none_or(|time| {
                time == 0 || time > response.observed_unix_ms.saturating_add(CLOCK_SKEW_MS)
            })
            || task.decision.as_ref().is_some_and(|decision| {
                task.reason != TaskWaitingReasonV1::NeedsPlanReview
                    || decision.developer != view.principal
                    || !is_digest(&decision.plan_id)
                    || !is_digest(&decision.task_revision_id)
                    || decision
                        .prepared_id
                        .as_deref()
                        .is_some_and(|id| !is_digest(id))
            })
        {
            return Err(ReadFailure::InvalidResponse);
        }
        task.observation.freshness = freshness;
        task.observation.age_ms = task
            .observation
            .observed_unix_ms
            .map(|time| now.saturating_sub(time));
        if freshness == Freshness::Stale {
            task.decision = None;
        }
    }
    Ok(Reading {
        observation: observation(
            name,
            "waiting_tasks",
            Some((response.observed_unix_ms, age)),
            freshness,
        ),
        timed_out: false,
        data: Some(view),
    })
}
