//! The Project and its Workspaces (ADR-0158).
//!
//! A Project is the identity of one repository across every clone, worktree and fork af has
//! seen: `sha256:` + hex(SHA-256(`af/project-id@1\0` ‖ repository_id)), where `repository_id` is
//! the sorted root commits every Snapshot origin already carries. It owns one Project Store,
//! `$XDG_STATE_HOME/af/project/<hex>/` (`cas/` and the event log), which the Store registry
//! records so the Storage Budget reaches it. A Workspace is one registered checkout of the
//! Project, with its own Task Store, bindings file and Task-file directory, under one label.
//!
//! The log names a Workspace by an opaque ID and a label only. Every host path lives in the
//! machine-local `workspaces.toml` beside the log, written whole and atomically like
//! `stores.toml`. Registration is lazy, token-free and best effort: a command that registers
//! never changes its own result because of it, and nothing is ever written inside a checkout.
//!
//! The Project Store also holds the Project's Surveys (ADR-0159): the Surveyor in [`survey`]
//! records each in the CAS and `SurveyRecorded@1` or `SurveyFailed@1` in the log.
//!
//! The application supplies machine services through [`Host`]; `.af/af.toml` validation
//! remains the CLI's separate `project_policy` responsibility.

/// Project Briefs (ADR-0160), rendered from the Surveyor's records.
pub mod brief;
mod environment;
pub mod workspace;
pub use environment::Host;
mod file;
mod git;
mod removal;
pub mod survey;
#[cfg(test)]
mod tests;
pub mod worktree_git;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use af_contracts::project::{
    ProjectRegisteredPayloadV1, SurveyFailedPayloadV1, SurveyRecordedPayloadV1,
    WorkspaceDeliveredPayloadV1, WorkspaceOriginV1, WorkspaceRegisteredPayloadV1,
    WorkspaceRemovedPayloadV1,
};
use af_contracts::{EventType, RunEvent};
use af_store::{Cas, EventStore, NewEvent};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub use file::WorkspacePaths;
pub use git::Git;

/// The domain of the Project ID digest.
const PROJECT_ID_DOMAIN: &[u8] = b"af/project-id@1\0";
/// The domain of the Workspace ID digest.
const WORKSPACE_ID_DOMAIN: &[u8] = b"af/workspace-id@1\0";
/// The one run of a Project Store's log.
const RUN_ID: &str = "project";
/// What a shallow clone is told; the message names the way out.
pub const UNSHALLOW: &str = "this checkout is a shallow clone: its boundary commit is not \
     a root commit, so af cannot tell which Project it belongs to and registers nothing; run \
     `git fetch --unshallow`, then run the command again";

/// The Project ID of a repository: a domain-separated digest of its `repository_id`.
pub fn project_id(repository_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(PROJECT_ID_DOMAIN);
    digest.update(repository_id.as_bytes());
    format!("sha256:{}", af_contracts::hex::encode(&digest.finalize()))
}

/// The Workspace ID of a canonical checkout path within a Project: opaque, so the path itself
/// never enters a record.
pub fn workspace_id(project_id: &str, checkout: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(WORKSPACE_ID_DOMAIN);
    digest.update(project_id.as_bytes());
    digest.update(b"\0");
    digest.update(checkout.as_os_str().as_encoded_bytes());
    let hex = af_contracts::hex::encode(&digest.finalize());
    format!(
        "w-{}",
        &hex[..af_contracts::project::PROJECT_WORKSPACE_ID_HEX_LEN]
    )
}

/// Every Project Store of this machine lives below this directory.
pub fn projects_root(state_home: &Path) -> PathBuf {
    state_home.join("af").join("project")
}

/// The Project Store of `project_id`: the hex of the digest, never the `sha256:` prefix.
pub fn store_directory(state_home: &Path, project_id: &str) -> PathBuf {
    projects_root(state_home).join(project_id.trim_start_matches("sha256:"))
}

/// A git checkout with at least one commit: what a Project and a Workspace are made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The canonical top level of the worktree.
    pub toplevel: PathBuf,
    pub repository_id: String,
    pub project_id: String,
}

/// What a directory is, as registration sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    /// Not inside a git worktree: nothing to register.
    NotGit,
    /// A repository without commits has no root commit, so it has no Project.
    NoCommit,
    /// A shallow clone's boundary commit is not a root: refused, with [`UNSHALLOW`].
    Shallow,
    Checkout(Checkout),
}

/// Find the checkout `directory` belongs to, with read-only git plumbing. A git that cannot
/// answer within its deadline is an error, never "not a checkout".
pub fn locate(directory: &Path) -> Result<Located, String> {
    let Ok(directory) = std::fs::canonicalize(directory) else {
        return Ok(Located::NotGit);
    };
    let git = Git::open(&directory)?;
    let toplevel = git.run(["rev-parse", "--show-toplevel"], None)?;
    if !toplevel.status.success() {
        return Ok(Located::NotGit);
    }
    let toplevel = String::from_utf8_lossy(&toplevel.stdout);
    let toplevel = std::fs::canonicalize(toplevel.trim_end_matches(['\n', '\r']))
        .map_err(|error| format!("resolving the checkout's top level: {error}"))?;
    let git = git.at(&toplevel);
    if !git
        .run(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"], None)?
        .status
        .success()
    {
        return Ok(Located::NoCommit);
    }
    if git.require(["rev-parse", "--is-shallow-repository"], None)? == "true" {
        return Ok(Located::Shallow);
    }
    let repository_id = git.roots("HEAD")?;
    Ok(Located::Checkout(Checkout {
        project_id: project_id(&repository_id),
        repository_id,
        toplevel,
    }))
}

/// The checkout `directory` belongs to, or why an `af project` command cannot work there.
pub fn require_checkout(directory: &Path) -> Result<Checkout, String> {
    match locate(directory)? {
        Located::Checkout(checkout) => Ok(checkout),
        Located::NotGit => Err(format!(
            "{} is not inside a git checkout; run `af project` inside a checkout of the repository",
            directory.display()
        )),
        Located::NoCommit => Err(
            "this repository has no commit yet, so it has no Project; commit once, then run the \
             command again"
                .into(),
        ),
        Located::Shallow => Err(UNSHALLOW.into()),
    }
}

/// The `[project] name` and `min_af` of the checkout's `.af/af.toml`, when that file parses.
fn declared(host: &dyn Host, checkout: &Path) -> (Option<String>, Option<String>) {
    #[derive(Deserialize)]
    struct Declared {
        project: DeclaredProject,
    }
    #[derive(Deserialize)]
    struct DeclaredProject {
        name: String,
        min_af: String,
    }
    let Ok(text) = std::fs::read_to_string(checkout.join(".af").join("af.toml")) else {
        return (None, None);
    };
    if !host.valid_project_declaration(&text) {
        return (None, None);
    }
    match toml::from_str::<Declared>(&text) {
        Ok(declared) => (Some(declared.project.name), Some(declared.project.min_af)),
        Err(_) => (None, None),
    }
}

/// One registered Workspace, as the log records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub workspace_id: String,
    pub label: String,
    pub origin: WorkspaceOriginV1,
    pub registered_unix_ms: u64,
    /// For a `delivered` Workspace, the Task it was made from as its `WorkspaceDelivered@1`
    /// records it: data apart from the label, which carries a suffix when the Task ID was taken.
    pub source_task: Option<String>,
}

impl Registered {
    /// The Task a `delivered` Workspace was made from: its recorded source Task, else its label,
    /// which is the Task ID unless that was taken (a registration whose `WorkspaceDelivered@1`
    /// never landed, after a crash between the two appends). `None` for any other origin.
    pub fn delivered_task(&self) -> Option<&str> {
        (self.origin == WorkspaceOriginV1::Delivered)
            .then(|| self.source_task.as_deref().unwrap_or(&self.label))
    }
}

/// The fold of a Project Store's log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    pub project: Option<ProjectRegisteredPayloadV1>,
    /// Registered Workspaces by ID.
    pub workspaces: std::collections::BTreeMap<String, Registered>,
    /// Workspaces whose last event is their `WorkspaceRemoved@1`, by ID, as they were registered.
    pub removed: std::collections::BTreeMap<String, Registered>,
    /// Every Survey recorded and every Survey that failed, in log order.
    pub surveys: Vec<survey::Outcome>,
}

impl Log {
    /// Fold the log's events in sequence order. A Workspace registered again after its removal
    /// is registered with what the newer event says.
    pub fn fold(events: &[RunEvent]) -> Result<Self, String> {
        let mut log = Log::default();
        for event in events {
            match event.event_type {
                EventType::ProjectRegisteredV1 => {
                    if log.project.is_some() {
                        return Err("the Project Store records its Project twice".into());
                    }
                    let payload: ProjectRegisteredPayloadV1 =
                        serde_json::from_value(event.payload.clone())
                            .map_err(|error| format!("ProjectRegistered@1: {error}"))?;
                    log.project = Some(payload);
                }
                EventType::WorkspaceRegisteredV1 => {
                    let payload: WorkspaceRegisteredPayloadV1 =
                        serde_json::from_value(event.payload.clone())
                            .map_err(|error| format!("WorkspaceRegistered@1: {error}"))?;
                    log.removed.remove(&payload.workspace_id);
                    log.workspaces.insert(
                        payload.workspace_id.clone(),
                        Registered {
                            workspace_id: payload.workspace_id,
                            label: payload.label,
                            origin: payload.origin,
                            registered_unix_ms: payload.registered_unix_ms.get(),
                            source_task: None,
                        },
                    );
                }
                EventType::WorkspaceDeliveredV1 => {
                    let payload: WorkspaceDeliveredPayloadV1 =
                        serde_json::from_value(event.payload.clone())
                            .map_err(|error| format!("WorkspaceDelivered@1: {error}"))?;
                    let workspace = log
                        .workspaces
                        .get_mut(&payload.workspace_id)
                        .filter(|workspace| workspace.origin == WorkspaceOriginV1::Delivered)
                        .ok_or_else(|| {
                            format!(
                                "WorkspaceDelivered@1 names {}, which is no registered delivered \
                                 Workspace",
                                payload.workspace_id
                            )
                        })?;
                    workspace.source_task = Some(payload.task_id);
                }
                EventType::WorkspaceRemovedV1 => {
                    let payload: WorkspaceRemovedPayloadV1 =
                        serde_json::from_value(event.payload.clone())
                            .map_err(|error| format!("WorkspaceRemoved@1: {error}"))?;
                    if let Some(workspace) = log.workspaces.remove(&payload.workspace_id) {
                        log.removed.insert(payload.workspace_id, workspace);
                    }
                }
                EventType::SurveyRecordedV1 => {
                    let payload: SurveyRecordedPayloadV1 =
                        serde_json::from_value(event.payload.clone())
                            .map_err(|error| format!("SurveyRecorded@1: {error}"))?;
                    log.surveys.push(survey::Outcome::Recorded(payload));
                }
                EventType::SurveyFailedV1 => {
                    let payload: SurveyFailedPayloadV1 =
                        serde_json::from_value(event.payload.clone())
                            .map_err(|error| format!("SurveyFailed@1: {error}"))?;
                    log.surveys.push(survey::Outcome::Failed(payload));
                }
                other => {
                    return Err(format!("a Project Store does not hold {other} events"));
                }
            }
        }
        Ok(log)
    }

    pub fn by_label(&self, label: &str) -> Option<&Registered> {
        self.workspaces
            .values()
            .find(|workspace| workspace.label == label)
    }

    /// The removed Workspaces that were labelled `label`, which no registered one holds now.
    pub fn removed_by_label(&self, label: &str) -> Vec<&Registered> {
        self.removed
            .values()
            .filter(|workspace| workspace.label == label)
            .collect()
    }
}

/// One open Project Store, held exclusively for the life of the value: registrations of two
/// processes never interleave.
pub struct ProjectStore {
    pub directory: PathBuf,
    pub project_id: String,
    cas: Cas,
    events: EventStore,
    _lock: nix::fcntl::Flock<std::fs::File>,
}

/// How long a bounded wait for `project.lock` sleeps between two tries.
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(10);

/// How an opening of a Project Store waits for its lock.
#[derive(Debug, Clone, Copy)]
pub enum Wait {
    /// Until the holder releases it: registration and the commands that read the Store.
    Blocking,
    /// Trying without blocking until this instant, and at least once: a refresh, whose one
    /// Survey deadline a holder never extends (ADR-0159 clause 2).
    Until(std::time::Instant),
}

/// Why a Project Store was not opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unopened {
    /// Another process held its lock until the deadline of a bounded wait.
    Locked,
    /// Anything else, in af's words.
    Failed(String),
}

impl Unopened {
    /// The message of a failure that is not a bounded wait's.
    pub fn message(self) -> String {
        match self {
            Unopened::Locked => {
                "another af process holds the Project Store's lock; run the command again once \
                 it ends"
                    .into()
            }
            Unopened::Failed(message) => message,
        }
    }
}

impl From<String> for Unopened {
    fn from(message: String) -> Self {
        Unopened::Failed(message)
    }
}

/// Take `project.lock`, held open as `file`, exclusively: waiting for it, or trying until a
/// deadline.
fn take_lock(
    file: std::fs::File,
    wait: Wait,
) -> Result<nix::fcntl::Flock<std::fs::File>, Unopened> {
    use nix::fcntl::{Flock, FlockArg};
    let deadline = match wait {
        Wait::Blocking => {
            return Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, errno)| {
                Unopened::Failed(format!("locking the Project Store: {errno}"))
            });
        }
        Wait::Until(deadline) => deadline,
    };
    let mut file = file;
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(lock),
            Err((back, nix::errno::Errno::EWOULDBLOCK)) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Err(Unopened::Locked);
                }
                std::thread::sleep(LOCK_RETRY.min(deadline - now));
                file = back;
            }
            Err((_, errno)) => {
                return Err(Unopened::Failed(format!(
                    "locking the Project Store: {errno}"
                )));
            }
        }
    }
}

impl ProjectStore {
    /// Open (creating when absent) the Project Store of `project_id` below `state_home`, and
    /// record it in the Store registry so the Storage Budget reaches it.
    pub fn open(host: &dyn Host, state_home: &Path, project_id: &str) -> Result<Self, String> {
        Self::open_waiting(host, state_home, project_id, Wait::Blocking).map_err(Unopened::message)
    }

    /// [`ProjectStore::open`], waiting for its lock as `wait` says.
    pub fn open_waiting(
        host: &dyn Host,
        state_home: &Path,
        project_id: &str,
        wait: Wait,
    ) -> Result<Self, Unopened> {
        let store = Self::open_unrecorded_waiting(state_home, project_id, wait)?;
        host.record_project_store(&store.directory);
        Ok(store)
    }

    /// An existing Project Store opened to be read and nothing else, so a preview (`af project
    /// brief`) changes no byte of the Store, the Store registry or the state root: no directory
    /// or file is created, `project.lock` is opened read-only and held shared (every writer
    /// holds it exclusively, so none of them writes while it is read), the CAS is opened as it
    /// is, and the log is read through a connection that initializes no schema and writes no
    /// journal ([`EventStore::open_unchanged`]). No registry entry is written and the Store's
    /// last use is not moved. A Store directory and log nobody may write are read all the same.
    /// The one append made without the lock, the `SurveyFailed@1` of a refresh that could not
    /// take it, is not seen by a read already open; at worst that read fails and is run again.
    pub fn open_read_only(state_home: &Path, project_id: &str) -> Result<Self, String> {
        use nix::fcntl::{Flock, FlockArg};
        let directory = store_directory(state_home, project_id);
        let unreadable =
            |error: String| format!("the Project Store {}: {error}", directory.display());
        let lock = std::fs::File::open(directory.join("project.lock"))
            .map_err(|error| unreadable(format!("project.lock: {error}")))?;
        let lock = Flock::lock(lock, FlockArg::LockShared)
            .map_err(|(_, errno)| unreadable(format!("locking it to read: {errno}")))?;
        let cas = Cas::open_existing(directory.join("cas"))
            .map_err(|error| unreadable(error.to_string()))?;
        let events = EventStore::open_unchanged(directory.join("events.sqlite"))
            .map_err(|error| unreadable(error.to_string()))?;
        Ok(Self {
            directory,
            project_id: project_id.to_string(),
            cas,
            events,
            _lock: lock,
        })
    }

    /// [`ProjectStore::open`] without the registry, which lives in the machine's own state home.
    #[cfg(test)]
    fn open_unrecorded(state_home: &Path, project_id: &str) -> Result<Self, String> {
        Self::open_unrecorded_waiting(state_home, project_id, Wait::Blocking)
            .map_err(Unopened::message)
    }

    fn open_unrecorded_waiting(
        state_home: &Path,
        project_id: &str,
        wait: Wait,
    ) -> Result<Self, Unopened> {
        let directory = store_directory(state_home, project_id);
        // af's state is private whatever the umask: the Project Store and every directory af
        // creates for it 0700, its files 0600, and af's state root narrowed (ADR-0144). The
        // state home itself is the user's, created as any directory is.
        std::fs::create_dir_all(state_home)
            .map_err(|error| format!("creating {}: {error}", state_home.display()))?;
        af_store::local_fs::create_private_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;
        let _ = af_store::local_fs::narrow_owned_dir(&state_home.join("af"));
        let _ = af_store::local_fs::narrow_owned_dir(&directory);
        // Never through a link: the lock is af's own file, and narrowing it must not reach
        // anything else.
        use std::os::unix::fs::OpenOptionsExt as _;
        let lock = af_store::local_fs::private_file_options()
            .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("project.lock"))
            .map_err(|error| error.to_string())?;
        // Created 0600; one an older release left open to others is narrowed (ADR-0144).
        let _ = af_store::local_fs::narrow_owned_file(&lock);
        let lock = take_lock(lock, wait)?;
        let cas = Cas::open(directory.join("cas")).map_err(|error| error.to_string())?;
        let events = EventStore::open(directory.join("events.sqlite"))
            .map_err(|error| format!("the Project Store {}: {error}", directory.display()))?;
        Ok(Self {
            directory,
            project_id: project_id.to_string(),
            cas,
            events,
            _lock: lock,
        })
    }

    pub fn log(&self) -> Result<Log, String> {
        let events = self
            .events
            .replay(RUN_ID)
            .map_err(|error| format!("the Project Store {}: {error}", self.directory.display()))?;
        let log = Log::fold(&events)?;
        if let Some(project) = &log.project
            && project.project_id != self.project_id
        {
            return Err(format!(
                "the Project Store {} records Project {}, not {}",
                self.directory.display(),
                project.project_id,
                self.project_id
            ));
        }
        Ok(log)
    }

    /// Append `event`, stamped now: the Surveyor's events, which may reference the CAS.
    fn append_new(&mut self, event: NewEvent) -> Result<(), String> {
        let event_type = event.event_type;
        let event = event.at(af_contracts::task::collection::collected_time(now_unix_ms()));
        self.events
            .append(RUN_ID, &self.cas, event)
            .map(|_| ())
            .map_err(|error| format!("recording {event_type}: {error}"))
    }

    fn append(&mut self, event_type: EventType, payload: serde_json::Value) -> Result<(), String> {
        let event = NewEvent::new(event_type, payload)
            .at(af_contracts::task::collection::collected_time(now_unix_ms()));
        self.events
            .append(RUN_ID, &self.cas, event)
            .map(|_| ())
            .map_err(|error| format!("recording {event_type}: {error}"))
    }

    fn workspaces_file(&self) -> PathBuf {
        file::location(&self.directory)
    }

    pub fn paths(&self) -> Result<Vec<WorkspacePaths>, String> {
        file::read(&self.workspaces_file())
    }

    /// Record the Project when the log does not hold it yet.
    pub fn ensure_project(&mut self, host: &dyn Host, checkout: &Checkout) -> Result<Log, String> {
        let log = self.log()?;
        if log.project.is_some() {
            return Ok(log);
        }
        let (name, min_af) = declared(host, &checkout.toplevel);
        let mut payload = ProjectRegisteredPayloadV1 {
            project_id: checkout.project_id.clone(),
            repository_id: checkout.repository_id.clone(),
            name,
            min_af,
            registered_unix_ms: now_unix_ms().into(),
        };
        // A declaration the record cannot hold is left out rather than refused.
        if payload.validate().is_err() {
            payload.name = None;
            payload.min_af = None;
        }
        payload.validate()?;
        self.append(
            EventType::ProjectRegisteredV1,
            serde_json::to_value(&payload).map_err(|error| error.to_string())?,
        )?;
        self.log()
    }

    /// Register `paths` as a Workspace of `origin`, or say which Workspace the checkout already
    /// is. The log is written first; the paths file follows, and a later registration of the
    /// same checkout repairs it.
    pub fn register(
        &mut self,
        log: &Log,
        origin: WorkspaceOriginV1,
        paths: WorkspacePaths,
    ) -> Result<Registration, String> {
        // A completed removal whose unregistration is durable leaves nothing to a new
        // registration of the same checkout path: its record goes before the new event.
        self.settle_removal(log, &paths.id)?;
        if let Some(existing) = log.workspaces.get(&paths.id) {
            let mut known = self.paths()?;
            if !known.iter().any(|entry| entry.id == paths.id) {
                known.push(WorkspacePaths {
                    label: existing.label.clone(),
                    ..paths
                });
                file::write(&self.workspaces_file(), known)?;
            }
            return Ok(Registration::Existing(existing.clone()));
        }
        let payload = WorkspaceRegisteredPayloadV1 {
            workspace_id: paths.id.clone(),
            label: paths.label.clone(),
            origin,
            registered_unix_ms: now_unix_ms().into(),
        };
        payload.validate()?;
        self.append(
            EventType::WorkspaceRegisteredV1,
            serde_json::to_value(&payload).map_err(|error| error.to_string())?,
        )?;
        let mut known = self.paths()?;
        known.retain(|entry| entry.id != paths.id);
        known.push(paths);
        file::write(&self.workspaces_file(), known)?;
        Ok(Registration::New(Registered {
            workspace_id: payload.workspace_id,
            label: payload.label,
            origin,
            registered_unix_ms: payload.registered_unix_ms.get(),
            source_task: None,
        }))
    }

    /// Record the Task a `delivered` Workspace was made from, right after its registration.
    pub fn record_delivered(&mut self, workspace_id: &str, task_id: &str) -> Result<(), String> {
        let payload = WorkspaceDeliveredPayloadV1 {
            workspace_id: workspace_id.to_string(),
            task_id: task_id.to_string(),
        };
        payload.validate()?;
        self.append(
            EventType::WorkspaceDeliveredV1,
            serde_json::to_value(&payload).map_err(|error| error.to_string())?,
        )
    }

    /// Unregister one Workspace: its event, then its paths.
    pub fn unregister(&mut self, workspace_id: &str) -> Result<(), String> {
        let payload = WorkspaceRemovedPayloadV1 {
            workspace_id: workspace_id.to_string(),
            removed_unix_ms: now_unix_ms().into(),
        };
        payload.validate()?;
        self.append(
            EventType::WorkspaceRemovedV1,
            serde_json::to_value(&payload).map_err(|error| error.to_string())?,
        )?;
        let mut known = self.paths()?;
        known.retain(|entry| entry.id != workspace_id);
        file::write(&self.workspaces_file(), known)
    }

    /// Finish the removal of `workspace_id` once its unregistration is durable: a completed
    /// removal record of a Workspace the log no longer registers (its `WorkspaceRemoved@1` is
    /// appended) has the Workspace's paths dropped, when a crash left them, and is then deleted.
    /// Idempotent: whichever of the three a crash left, the next call finishes the rest. `true`
    /// when it settled a record. A record of a registered Workspace, or one not complete, is
    /// left: its removal still owns it (ADR-0158 clause 5).
    pub fn settle_removal(&mut self, log: &Log, workspace_id: &str) -> Result<bool, String> {
        if log.workspaces.contains_key(workspace_id) {
            return Ok(false);
        }
        match removal::read(&self.directory, workspace_id)? {
            Some(record) if removal::complete(&record) => {}
            _ => return Ok(false),
        }
        let mut known = self.paths()?;
        if known.iter().any(|entry| entry.id == workspace_id) {
            known.retain(|entry| entry.id != workspace_id);
            file::write(&self.workspaces_file(), known)?;
        }
        removal::delete(&self.directory, workspace_id)?;
        Ok(true)
    }

    /// The directory `af project workspace new` makes for a Workspace's Store, Task files and
    /// bindings.
    pub fn workspace_directory(&self, label: &str) -> PathBuf {
        self.directory.join("workspaces").join(label)
    }
}

/// What registering a checkout did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    New(Registered),
    Existing(Registered),
}

impl Registration {
    pub fn workspace(&self) -> &Registered {
        match self {
            Registration::New(workspace) | Registration::Existing(workspace) => workspace,
        }
    }
}

/// The longest automatic label before its collision suffix: `-` and eight hex digits then fit
/// within the 128-byte safe-name limit.
const AUTOMATIC_LABEL_BYTES: usize = 119;

/// A label for `preferred` that no other Workspace holds, explicit or automatic: `preferred`
/// itself, or `preferred` cut to 119 bytes, `-` and the first eight hex digits of the Workspace
/// ID. Both taken is an error naming `--label`.
pub fn free_label(log: &Log, preferred: &str, workspace_id: &str) -> Result<String, String> {
    if log.by_label(preferred).is_none() {
        return Ok(preferred.to_string());
    }
    let suffix = &workspace_id.trim_start_matches("w-")[..8];
    let stem = preferred.get(..AUTOMATIC_LABEL_BYTES).unwrap_or(preferred);
    let label = format!("{stem}-{suffix}");
    if log.by_label(&label).is_none() {
        Ok(label)
    } else {
        Err(format!(
            "the labels `{preferred}` and `{label}` are both taken; name one with --label"
        ))
    }
}

/// The automatic label of a directory name (ADR-0158 clause 4): every byte outside ASCII
/// letters, digits, `-` and `_` becomes `-`; leading `-` and `_` are stripped; the result is cut
/// to 119 bytes; an empty result is `checkout`.
pub fn label_from_name(name: &OsStr) -> String {
    let mapped: String = name
        .as_encoded_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                char::from(*byte)
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_start_matches(['-', '_']);
    let cut = &trimmed[..trimmed.len().min(AUTOMATIC_LABEL_BYTES)];
    if cut.is_empty() {
        "checkout".into()
    } else {
        cut.to_string()
    }
}

/// The default Task Store of a checkout: the one `af task` uses there without `--state`.
pub fn default_store(host: &dyn Host, checkout: &Path) -> Result<PathBuf, String> {
    host.default_task_store(checkout)
}

/// Register the checkout `paths` describes as a Workspace of `checkout`'s Project, creating the
/// Project when it is new. `label` is the preferred label; `exact` refuses a taken one instead
/// of deriving another.
pub fn register(
    host: &dyn Host,
    checkout: &Checkout,
    origin: WorkspaceOriginV1,
    label: &str,
    exact: bool,
    paths: WorkspacePaths,
) -> Result<(ProjectStore, Registration), String> {
    register_waiting(host, checkout, origin, label, exact, paths, Wait::Blocking)
        .map_err(Unopened::message)
}

/// [`register`], waiting for the Project Store's lock as `wait` says.
fn register_waiting(
    host: &dyn Host,
    checkout: &Checkout,
    origin: WorkspaceOriginV1,
    label: &str,
    exact: bool,
    paths: WorkspacePaths,
    wait: Wait,
) -> Result<(ProjectStore, Registration), Unopened> {
    let mut store =
        ProjectStore::open_waiting(host, &host.state_root()?, &checkout.project_id, wait)?;
    let registration = register_in(host, &mut store, checkout, origin, label, exact, paths)?;
    Ok((store, registration))
}

/// [`register`] in a Project Store this process already holds.
pub fn register_in(
    host: &dyn Host,
    store: &mut ProjectStore,
    checkout: &Checkout,
    origin: WorkspaceOriginV1,
    label: &str,
    exact: bool,
    mut paths: WorkspacePaths,
) -> Result<Registration, String> {
    let log = store.ensure_project(host, checkout)?;
    if !log.workspaces.contains_key(&paths.id) {
        paths.label = if exact {
            if let Some(holder) = log.by_label(label) {
                return Err(format!(
                    "the label `{label}` is taken by Workspace {}; choose another",
                    holder.workspace_id
                ));
            }
            label.to_string()
        } else {
            free_label(&log, label, &paths.id)?
        };
    }
    store.register(&log, origin, paths)
}

/// Register the invoking checkout as an `adopted` Workspace when it is new: labelled by its
/// directory name, its Store the explicit `state` or the default Task Store.
pub fn adopt(
    host: &dyn Host,
    checkout: &Checkout,
    state: Option<&Path>,
) -> Result<(ProjectStore, Registration), String> {
    adopt_waiting(host, checkout, state, Wait::Blocking).map_err(Unopened::message)
}

/// [`adopt`], waiting for the Project Store's lock as `wait` says.
pub fn adopt_waiting(
    host: &dyn Host,
    checkout: &Checkout,
    state: Option<&Path>,
    wait: Wait,
) -> Result<(ProjectStore, Registration), Unopened> {
    let state = match state {
        Some(state) => {
            let state = host.resolve_path(state)?;
            if state.starts_with(&checkout.toplevel) {
                default_store(host, &checkout.toplevel)?
            } else {
                state
            }
        }
        None => default_store(host, &checkout.toplevel)?,
    };
    let label = label_from_name(checkout.toplevel.file_name().unwrap_or_default());
    let paths = WorkspacePaths {
        id: workspace_id(&checkout.project_id, &checkout.toplevel),
        label: label.clone(),
        path: checkout.toplevel.clone(),
        state,
        bindings: None,
        tasks: None,
    };
    register_waiting(
        host,
        checkout,
        WorkspaceOriginV1::Adopted,
        &label,
        false,
        paths,
        wait,
    )
}

/// Register the worktree `af task deliver` created as a Workspace of origin `delivered`,
/// labelled by the Task ID (suffixed when that label is taken), and record the Task ID as data
/// in its `WorkspaceDelivered@1`, so the Storage Budget keeps that Task whatever the label. Best
/// effort, after delivery succeeded: the receipt is already out.
pub fn register_delivered(host: &dyn Host, worktree: &Path, task_id: &str) -> Result<(), String> {
    let Located::Checkout(checkout) = locate(worktree)? else {
        return Ok(());
    };
    let paths = WorkspacePaths {
        id: workspace_id(&checkout.project_id, &checkout.toplevel),
        label: task_id.to_string(),
        path: checkout.toplevel.clone(),
        state: default_store(host, &checkout.toplevel)?,
        bindings: None,
        tasks: None,
    };
    let (mut store, registration) = register(
        host,
        &checkout,
        WorkspaceOriginV1::Delivered,
        task_id,
        false,
        paths,
    )?;
    match registration {
        Registration::New(workspace) => store.record_delivered(&workspace.workspace_id, task_id),
        Registration::Existing(_) => Ok(()),
    }
}

/// The paths `--workspace LABEL` stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub state: PathBuf,
    /// The Workspace's bindings file, when it names one and the file exists.
    pub bindings: Option<PathBuf>,
}

/// Resolve `--workspace LABEL` against the Project of the checkout `repo` belongs to.
pub fn select(host: &dyn Host, repo: &Path, label: &str) -> Result<Selected, String> {
    let checkout = require_checkout(repo)?;
    let unknown = || {
        format!(
            "this Project has no Workspace labelled `{label}`; `af project workspaces` lists them"
        )
    };
    let state_home = host.state_root()?;
    if !store_directory(&state_home, &checkout.project_id)
        .join("events.sqlite")
        .is_file()
    {
        return Err(unknown());
    }
    let store = ProjectStore::open(host, &state_home, &checkout.project_id)?;
    let log = store.log()?;
    let workspace = log.by_label(label).ok_or_else(unknown)?;
    let paths = store
        .paths()?
        .into_iter()
        .find(|entry| entry.id == workspace.workspace_id)
        .ok_or_else(|| {
            format!(
                "Workspace `{label}` has no paths on this machine; add them with `af project \
                 workspace remove {label}` and `af project workspace add`"
            )
        })?;
    Ok(Selected {
        state: paths.state,
        bindings: paths.bindings.filter(|bindings| bindings.is_file()),
    })
}

/// Wall-clock timestamps of Project events, with the same saturating millisecond spelling as
/// the machine Storage Budget. No state or environment is consulted.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
}
