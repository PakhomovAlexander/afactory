//! The Project and its Workspaces (ADR-0157).
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
//! The Project Store also holds the Project's Surveys (ADR-0158): the Surveyor in [`survey`]
//! records each in the CAS and `SurveyRecorded@1` or `SurveyFailed@1` in the log.
//!
//! `crate::project` is something else: the parser of the `.af/af.toml` declaration.

pub(crate) mod commands;
mod file;
mod git;
mod removal;
pub(crate) mod survey;
#[cfg(test)]
mod tests;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use review_core::project::{
    ProjectRegisteredPayloadV1, SurveyFailedPayloadV1, SurveyRecordedPayloadV1, WorkspaceOriginV1,
    WorkspaceRegisteredPayloadV1, WorkspaceRemovedPayloadV1,
};
use review_core::{EventType, RunEvent};
use review_store::{Cas, EventStore, NewEvent};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(crate) use file::WorkspacePaths;
pub(crate) use git::Git;

/// The domain of the Project ID digest.
const PROJECT_ID_DOMAIN: &[u8] = b"af/project-id@1\0";
/// The domain of the Workspace ID digest.
const WORKSPACE_ID_DOMAIN: &[u8] = b"af/workspace-id@1\0";
/// The one run of a Project Store's log.
const RUN_ID: &str = "project";
/// What a shallow clone is told; the message names the way out.
pub(crate) const UNSHALLOW: &str = "this checkout is a shallow clone: its boundary commit is not \
     a root commit, so af cannot tell which Project it belongs to and registers nothing; run \
     `git fetch --unshallow`, then run the command again";

/// The Project ID of a repository: a domain-separated digest of its `repository_id`.
pub(crate) fn project_id(repository_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(PROJECT_ID_DOMAIN);
    digest.update(repository_id.as_bytes());
    format!("sha256:{}", review_core::hex::encode(&digest.finalize()))
}

/// The Workspace ID of a canonical checkout path within a Project: opaque, so the path itself
/// never enters a record.
pub(crate) fn workspace_id(project_id: &str, checkout: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(WORKSPACE_ID_DOMAIN);
    digest.update(project_id.as_bytes());
    digest.update(b"\0");
    digest.update(checkout.as_os_str().as_encoded_bytes());
    let hex = review_core::hex::encode(&digest.finalize());
    format!(
        "w-{}",
        &hex[..review_core::project::PROJECT_WORKSPACE_ID_HEX_LEN]
    )
}

/// Every Project Store of this machine lives below this directory.
pub(crate) fn projects_root(state_home: &Path) -> PathBuf {
    state_home.join("af").join("project")
}

/// The Project Store of `project_id`: the hex of the digest, never the `sha256:` prefix.
pub(crate) fn store_directory(state_home: &Path, project_id: &str) -> PathBuf {
    projects_root(state_home).join(project_id.trim_start_matches("sha256:"))
}

/// A git checkout with at least one commit: what a Project and a Workspace are made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkout {
    /// The canonical top level of the worktree.
    pub(crate) toplevel: PathBuf,
    pub(crate) repository_id: String,
    pub(crate) project_id: String,
}

/// What a directory is, as registration sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Located {
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
pub(crate) fn locate(directory: &Path) -> Result<Located, String> {
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
pub(crate) fn require_checkout(directory: &Path) -> Result<Checkout, String> {
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
fn declared(checkout: &Path) -> (Option<String>, Option<String>) {
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
    if crate::project::ProjectFile::parse(&text).is_err() {
        return (None, None);
    }
    match toml::from_str::<Declared>(&text) {
        Ok(declared) => (Some(declared.project.name), Some(declared.project.min_af)),
        Err(_) => (None, None),
    }
}

/// One registered Workspace, as the log records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registered {
    pub(crate) workspace_id: String,
    pub(crate) label: String,
    pub(crate) origin: WorkspaceOriginV1,
    pub(crate) registered_unix_ms: u64,
}

/// The fold of a Project Store's log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Log {
    pub(crate) project: Option<ProjectRegisteredPayloadV1>,
    /// Registered Workspaces by ID.
    pub(crate) workspaces: std::collections::BTreeMap<String, Registered>,
    /// Workspaces whose last event is their `WorkspaceRemoved@1`, by ID, as they were registered.
    pub(crate) removed: std::collections::BTreeMap<String, Registered>,
    /// Every Survey recorded and every Survey that failed, in log order.
    pub(crate) surveys: Vec<survey::Outcome>,
}

impl Log {
    /// Fold the log's events in sequence order. A Workspace registered again after its removal
    /// is registered with what the newer event says.
    pub(crate) fn fold(events: &[RunEvent]) -> Result<Self, String> {
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
                        },
                    );
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

    pub(crate) fn by_label(&self, label: &str) -> Option<&Registered> {
        self.workspaces
            .values()
            .find(|workspace| workspace.label == label)
    }

    /// The removed Workspaces that were labelled `label`, which no registered one holds now.
    pub(crate) fn removed_by_label(&self, label: &str) -> Vec<&Registered> {
        self.removed
            .values()
            .filter(|workspace| workspace.label == label)
            .collect()
    }
}

/// One open Project Store, held exclusively for the life of the value: registrations of two
/// processes never interleave.
pub(crate) struct ProjectStore {
    pub(crate) directory: PathBuf,
    pub(crate) project_id: String,
    cas: Cas,
    events: EventStore,
    _lock: nix::fcntl::Flock<std::fs::File>,
}

/// How long a bounded wait for `project.lock` sleeps between two tries.
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(10);

/// How an opening of a Project Store waits for its lock.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Wait {
    /// Until the holder releases it: registration and the commands that read the Store.
    Blocking,
    /// Trying without blocking until this instant, and at least once: a refresh, whose one
    /// Survey deadline a holder never extends (ADR-0158 clause 2).
    Until(std::time::Instant),
}

/// Why a Project Store was not opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unopened {
    /// Another process held its lock until the deadline of a bounded wait.
    Locked,
    /// Anything else, in af's words.
    Failed(String),
}

impl Unopened {
    /// The message of a failure that is not a bounded wait's.
    pub(crate) fn message(self) -> String {
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
    pub(crate) fn open(state_home: &Path, project_id: &str) -> Result<Self, String> {
        Self::open_waiting(state_home, project_id, Wait::Blocking).map_err(Unopened::message)
    }

    /// [`ProjectStore::open`], waiting for its lock as `wait` says.
    pub(crate) fn open_waiting(
        state_home: &Path,
        project_id: &str,
        wait: Wait,
    ) -> Result<Self, Unopened> {
        let store = Self::open_unrecorded_waiting(state_home, project_id, wait)?;
        crate::storage::registry::record(
            &store.directory,
            crate::storage::registry::StoreKind::Project,
        );
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
    pub(crate) fn open_read_only(state_home: &Path, project_id: &str) -> Result<Self, String> {
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
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("project.lock"))
            .map_err(|error| error.to_string())?;
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

    pub(crate) fn log(&self) -> Result<Log, String> {
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
        let event = event.at(review_core::task::collection::collected_time(
            crate::storage::now_unix_ms(),
        ));
        self.events
            .append(RUN_ID, &self.cas, event)
            .map(|_| ())
            .map_err(|error| format!("recording {event_type}: {error}"))
    }

    fn append(&mut self, event_type: EventType, payload: serde_json::Value) -> Result<(), String> {
        let event = NewEvent::new(event_type, payload).at(
            review_core::task::collection::collected_time(crate::storage::now_unix_ms()),
        );
        self.events
            .append(RUN_ID, &self.cas, event)
            .map(|_| ())
            .map_err(|error| format!("recording {event_type}: {error}"))
    }

    fn workspaces_file(&self) -> PathBuf {
        file::location(&self.directory)
    }

    pub(crate) fn paths(&self) -> Result<Vec<WorkspacePaths>, String> {
        file::read(&self.workspaces_file())
    }

    /// Record the Project when the log does not hold it yet.
    pub(crate) fn ensure_project(&mut self, checkout: &Checkout) -> Result<Log, String> {
        let log = self.log()?;
        if log.project.is_some() {
            return Ok(log);
        }
        let (name, min_af) = declared(&checkout.toplevel);
        let mut payload = ProjectRegisteredPayloadV1 {
            project_id: checkout.project_id.clone(),
            repository_id: checkout.repository_id.clone(),
            name,
            min_af,
            registered_unix_ms: crate::storage::now_unix_ms().into(),
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
    pub(crate) fn register(
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
            registered_unix_ms: crate::storage::now_unix_ms().into(),
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
        }))
    }

    /// Unregister one Workspace: its event, then its paths.
    pub(crate) fn unregister(&mut self, workspace_id: &str) -> Result<(), String> {
        let payload = WorkspaceRemovedPayloadV1 {
            workspace_id: workspace_id.to_string(),
            removed_unix_ms: crate::storage::now_unix_ms().into(),
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
    /// left: its removal still owns it (ADR-0157 clause 5).
    pub(crate) fn settle_removal(&mut self, log: &Log, workspace_id: &str) -> Result<bool, String> {
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
    pub(crate) fn workspace_directory(&self, label: &str) -> PathBuf {
        self.directory.join("workspaces").join(label)
    }
}

/// What registering a checkout did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Registration {
    New(Registered),
    Existing(Registered),
}

impl Registration {
    pub(crate) fn workspace(&self) -> &Registered {
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
pub(crate) fn free_label(log: &Log, preferred: &str, workspace_id: &str) -> Result<String, String> {
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

/// The automatic label of a directory name (ADR-0157 clause 4): every byte outside ASCII
/// letters, digits, `-` and `_` becomes `-`; leading `-` and `_` are stripped; the result is cut
/// to 119 bytes; an empty result is `checkout`.
pub(crate) fn label_from_name(name: &OsStr) -> String {
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
pub(crate) fn default_store(checkout: &Path) -> Result<PathBuf, String> {
    crate::task_execution::default_task_state(&crate::xdg_state_root()?, checkout)
}

/// Register the checkout `paths` describes as a Workspace of `checkout`'s Project, creating the
/// Project when it is new. `label` is the preferred label; `exact` refuses a taken one instead
/// of deriving another.
pub(crate) fn register(
    checkout: &Checkout,
    origin: WorkspaceOriginV1,
    label: &str,
    exact: bool,
    paths: WorkspacePaths,
) -> Result<(ProjectStore, Registration), String> {
    register_waiting(checkout, origin, label, exact, paths, Wait::Blocking)
        .map_err(Unopened::message)
}

/// [`register`], waiting for the Project Store's lock as `wait` says.
fn register_waiting(
    checkout: &Checkout,
    origin: WorkspaceOriginV1,
    label: &str,
    exact: bool,
    paths: WorkspacePaths,
    wait: Wait,
) -> Result<(ProjectStore, Registration), Unopened> {
    let mut store =
        ProjectStore::open_waiting(&crate::xdg_state_root()?, &checkout.project_id, wait)?;
    let registration = register_in(&mut store, checkout, origin, label, exact, paths)?;
    Ok((store, registration))
}

/// [`register`] in a Project Store this process already holds.
pub(crate) fn register_in(
    store: &mut ProjectStore,
    checkout: &Checkout,
    origin: WorkspaceOriginV1,
    label: &str,
    exact: bool,
    mut paths: WorkspacePaths,
) -> Result<Registration, String> {
    let log = store.ensure_project(checkout)?;
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
pub(crate) fn adopt(
    checkout: &Checkout,
    state: Option<&Path>,
) -> Result<(ProjectStore, Registration), String> {
    adopt_waiting(checkout, state, Wait::Blocking).map_err(Unopened::message)
}

/// [`adopt`], waiting for the Project Store's lock as `wait` says.
pub(crate) fn adopt_waiting(
    checkout: &Checkout,
    state: Option<&Path>,
    wait: Wait,
) -> Result<(ProjectStore, Registration), Unopened> {
    let state = match state {
        Some(state) => {
            let state = crate::resolve_filesystem_path(state)?;
            if state.starts_with(&checkout.toplevel) {
                default_store(&checkout.toplevel)?
            } else {
                state
            }
        }
        None => default_store(&checkout.toplevel)?,
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
        checkout,
        WorkspaceOriginV1::Adopted,
        &label,
        false,
        paths,
        wait,
    )
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

/// Register the worktree `af task deliver` created as a Workspace of origin `delivered`,
/// labelled by the Task ID. Best effort, after delivery succeeded: the receipt is already out.
pub(crate) fn register_delivered(worktree: &Path, task_id: &str) {
    let outcome = (|| -> Result<(), String> {
        let Located::Checkout(checkout) = locate(worktree)? else {
            return Ok(());
        };
        let paths = WorkspacePaths {
            id: workspace_id(&checkout.project_id, &checkout.toplevel),
            label: task_id.to_string(),
            path: checkout.toplevel.clone(),
            state: default_store(&checkout.toplevel)?,
            bindings: None,
            tasks: None,
        };
        register(
            &checkout,
            WorkspaceOriginV1::Delivered,
            task_id,
            false,
            paths,
        )
        .map(|_| ())
    })();
    if let Err(error) = outcome {
        eprintln!("af: the delivered worktree was not registered as a Workspace: {error}");
    }
}

/// The paths `--workspace LABEL` stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selected {
    pub(crate) state: PathBuf,
    /// The Workspace's bindings file, when it names one and the file exists.
    pub(crate) bindings: Option<PathBuf>,
}

/// Resolve `--workspace LABEL` against the Project of the checkout `repo` belongs to.
pub(crate) fn select(repo: &Path, label: &str) -> Result<Selected, String> {
    let checkout = require_checkout(repo)?;
    let unknown = || {
        format!(
            "this Project has no Workspace labelled `{label}`; `af project workspaces` lists them"
        )
    };
    let state_home = crate::xdg_state_root()?;
    if !store_directory(&state_home, &checkout.project_id)
        .join("events.sqlite")
        .is_file()
    {
        return Err(unknown());
    }
    let store = ProjectStore::open(&state_home, &checkout.project_id)?;
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
        | T::Run { inspect, .. }
        | T::Explain { inspect, .. }
        | T::Export { inspect, .. }
        | T::ObserveAdoption { inspect, .. }
        | T::List { inspect, .. }
        | T::Gc { inspect, .. }
        | T::Show { inspect, .. }
        | T::Report { inspect, .. } => Some((
            inspect.repo.as_path(),
            &mut inspect.workspace,
            &mut inspect.state,
        )),
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
