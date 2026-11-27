use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
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
    let runtime = ObservationRuntime::lock(&args.state_home)?;
    let project = ProjectStore::open_origin_snapshot(&args.state_home, &args.project, |path| {
        runtime.snapshot(path)
    })?;
    #[cfg(debug_assertions)]
    pause_with_snapshot();
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
    origin_state_home: &'a Path,
    _admission: Flock<OwnedFd>,
}

impl<'a> ObservationRuntime<'a> {
    fn lock(origin_state_home: &'a Path) -> Result<Self, String> {
        let policy = crate::storage::policy()?;
        let account_home = crate::config::machine_account_home()?
            .ok_or("Cannot determine the OS account home for observation admission")?;
        let acquire = || -> io::Result<Flock<OwnedFd>> {
            // Lock a stable, existing account inode. Creating a lock or runtime
            // directory here could itself consume space below the volume floor.
            let home = fs::canonicalize(account_home)?;
            let relative = home.strip_prefix("/").map_err(io::Error::other)?;
            let directory =
                af_sandbox::open_beneath(af_sandbox::open_anchor(Path::new("/"))?, relative)?;
            if nix::sys::stat::fstat(&directory)?.st_uid != nix::unistd::geteuid().as_raw() {
                return Err(io::Error::other(
                    "observation account home is not owned by the reader",
                ));
            }
            Flock::lock(directory, FlockArg::LockExclusive).map_err(|(_, error)| error.into())
        };
        let admission = acquire().map_err(|e| format!("Cannot lock observation admission: {e}"))?;
        Ok(Self {
            policy,
            origin_state_home,
            _admission: admission,
        })
    }

    fn snapshot(&self, path: &Path) -> Result<EventStore, StoreError> {
        // The account lock spans inventory and replay, so different TMPDIR and XDG
        // settings cannot multiply the in-memory allowance.
        crate::storage::check_observation_storage(&self.policy, self.origin_state_home).map_err(
            |error| StoreError::Conflict(format!("cannot observe {}: {error}", path.display())),
        )?;
        EventStore::open_observation_snapshot(path)
    }
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
