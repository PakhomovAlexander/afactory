//! The Storage Budget (ADR-0144): one machine-wide bound on every byte af keeps between runs,
//! a free-disk floor below which no check or Worker Attempt starts, and `af storage`.
//!
//! The budget counts entries — the unit af evicts whole: a warm toolchain key, a warm
//! Workspace, a review campaign, a finished Task, a Survey (ADR-0158 clause 13), a Store this
//! release cannot replay, an installed version. One sweep serves every trigger: the end of `af
//! task run` and `af review run` (the budget; age-based collection too only when `auto_gc` is
//! on, which it is not by default), a warm check about to create a new key (budget only),
//! the free-disk floor, and `af storage prune --apply`. It never evicts an entry in use (a held
//! lock, a live writer lease, a running campaign, the default or a pinned version, the running
//! binary) or one used within the last hour, and it removes nothing without going through the
//! rule that owns it: finished Tasks through the Store's own collection and Surveys behind their
//! `SurveyCollected@1`, so tombstones stay; directories without following a link. A sweep
//! plans once over one inventory snapshot (`plan`), the claims dead processes left included:
//! the preview prints that plan and the applied sweep runs it entry by entry, never choosing
//! again and never removing what the plan does not list.

pub(crate) mod gate;
mod hint;
pub(crate) mod plan;
pub(crate) mod registry;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_store::{Cas, EventStore};
use serde::Serialize;
use serde_json::json;

use crate::config::{self, StoragePolicy};
use registry::StoreKind;

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 86_400_000;
/// The lock file `af review run` holds while it runs a campaign, so a sweep never removes it.
pub(crate) const RUNNING_LOCK: &str = "af-running.lock";
/// How the outcome of a planned removal that failed begins.
pub(crate) const FAILED: &str = "failed: ";

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
}

/// The machine's `[storage]` policy, from machine layers only.
pub(crate) fn policy() -> Result<StoragePolicy, String> {
    config::load_machine()?.storage_policy()
}

/// Where every kind of entry lives on this machine.
pub(crate) struct Roots {
    pub(crate) warm: PathBuf,
    pub(crate) workspaces: PathBuf,
    pub(crate) campaigns: PathBuf,
    pub(crate) local_reviews: PathBuf,
    pub(crate) tasks: PathBuf,
    pub(crate) registry: PathBuf,
    /// Where installed versions, the default symlink and the pins seen live.
    pub(crate) installs: crate::selfmgmt::Paths,
}

impl Roots {
    pub(crate) fn current() -> Result<Self, String> {
        let cache = config::cache_home()?.join("af");
        let state_home = config::state_home()?;
        let state = state_home.join("af");
        Ok(Self {
            warm: cache.join(review_sandbox::task_build_cache::TASK_BUILD_CACHE_DIRECTORY),
            workspaces: cache.join("workspaces"),
            campaigns: state.join("review").join("campaigns"),
            local_reviews: state.join("review").join("local"),
            tasks: state.join("task").join("local"),
            registry: registry::location(&state_home),
            installs: crate::selfmgmt::paths()?,
        })
    }

    /// The trusted anchors every removal starts from, as configured: an entry below one is
    /// opened from it, every component below without following a link.
    fn anchors(&self) -> [&Path; 6] {
        [
            &self.warm,
            &self.workspaces,
            &self.campaigns,
            &self.local_reviews,
            &self.tasks,
            &self.installs.versions,
        ]
    }

    /// The anchor `path` is removed from: the configured root it lies strictly below, or `/`
    /// for a Store registered elsewhere, which is then opened one component at a time.
    fn anchor_of(&self, path: &Path) -> &Path {
        self.anchors()
            .into_iter()
            .find(|anchor| path.starts_with(anchor) && path != *anchor)
            .unwrap_or(Path::new("/"))
    }
}

/// The kinds of entry the budget counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    WarmKey,
    Workspace,
    Campaign,
    Task,
    /// What a Task Store holds beyond its finished Tasks: never evicted itself; its Tasks are,
    /// one at a time.
    TaskStore,
    /// A Project Store (ADR-0157) beyond the Task Stores of its Workspaces and its Surveys:
    /// never evicted, since it indexes the Project's Workspaces.
    ProjectStore,
    /// One Survey's record in its Project Store's CAS (ADR-0158 clause 13), last used at its
    /// cutoff: collected one at a time behind a tombstone.
    Survey,
    UnreadableStore,
    Version,
    /// A removal a process that died left claimed under a private name (`state` `abandoned`,
    /// listed by `af storage` unless an entry measured it as part of its own), or a container a
    /// dead Survey collection left: in the sweep's snapshot and finished by its plan under
    /// `recovery`.
    Claim,
}

impl Kind {
    const ALL: [Kind; 10] = [
        Kind::WarmKey,
        Kind::Workspace,
        Kind::Campaign,
        Kind::Task,
        Kind::TaskStore,
        Kind::ProjectStore,
        Kind::Survey,
        Kind::UnreadableStore,
        Kind::Version,
        Kind::Claim,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Kind::WarmKey => "warm_key",
            Kind::Workspace => "workspace",
            Kind::Campaign => "campaign",
            Kind::Task => "task",
            Kind::TaskStore => "task_store",
            Kind::ProjectStore => "project_store",
            Kind::Survey => "survey",
            Kind::UnreadableStore => "unreadable_store",
            Kind::Version => "version",
            Kind::Claim => "claim",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Kind::WarmKey => "warm keys",
            Kind::Workspace => "warm Workspaces",
            Kind::Campaign => "review campaigns",
            Kind::Task => "finished Tasks",
            Kind::TaskStore => "Task Stores (rest)",
            Kind::ProjectStore => "Project Stores",
            Kind::Survey => "Surveys",
            Kind::UnreadableStore => "unreadable Stores",
            Kind::Version => "installed versions",
            Kind::Claim => "abandoned removals",
        }
    }
}

/// One entry: the unit the budget evicts whole.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Entry {
    pub(crate) kind: Kind,
    pub(crate) path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<String>,
    /// Allocated bytes; for a finished Task, the bytes only it reaches.
    pub(crate) bytes: u64,
    pub(crate) last_use_unix_ms: u64,
    /// Why the entry is never evicted now, when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) in_use: Option<String>,
    /// For an unreadable Store: why this release cannot read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    /// The directory inventory measured, by device and inode: a removal opens the entry through
    /// descriptors and refuses any other directory found under its name.
    #[serde(skip)]
    pub(crate) identity: Option<review_sandbox::Identity>,
    /// Nested deeper than the removal walk reaches (`review_sandbox::REMOVAL_MAX_DEPTH`), as
    /// the walk that measured its bytes found it: in use, and left `too_deep` by every plan.
    #[serde(skip)]
    pub(crate) too_deep: bool,
    /// A claim the walk could not measure (nested deeper than it reaches, or unreadable): why.
    /// It holds no bytes the inventory knows of, and every plan leaves it.
    #[serde(skip)]
    pub(crate) unmeasurable: Option<String>,
    /// What an entry that is not yet or no longer one af keeps is: `abandoned` for a removal a
    /// dead process left claimed, `pending` for a Survey published to its Project Store's CAS
    /// whose `SurveyRecorded@1` never committed (ADR-0158 clause 13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<&'static str>,
}

/// Why an entry nested deeper than the removal walk is never removed.
const TOO_DEEP: &str = "left: deeper than the removal walk, so no removal could take it whole";

/// A directory the inventory could not judge: listed, never removed.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Problem {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

/// Everything af holds, as one sweep sees it.
#[derive(Debug, Default)]
pub(crate) struct Inventory {
    pub(crate) entries: Vec<Entry>,
    pub(crate) problems: Vec<Problem>,
    /// Readable Task Stores, for collection.
    pub(crate) task_stores: Vec<PathBuf>,
    /// Each Store's directory identity as inventory reached it, opened from `/` without
    /// following a link: what collection and gate cleanup check again before they touch it.
    pub(crate) store_identities: BTreeMap<PathBuf, (PathBuf, review_sandbox::Identity)>,
    /// Each readable Project Store as its one read found it (ADR-0158 clause 13): what a
    /// sweep's plan needs of its Surveys, in inventory order.
    pub(crate) projects: Vec<(PathBuf, crate::projects::survey::collect::Look)>,
    /// The removals dead processes left claimed, entries of kind `claim` with the identity and
    /// bytes listed: the sweep's plan finishes them first. `af storage` lists every one; those
    /// no entry measured as part of its own ([`Inventory::counted_claims`]) are in
    /// [`Inventory::total`].
    pub(crate) claims: Vec<Entry>,
    /// The registered paths that held no Store when inventory read the registry. Inventory is
    /// taken over every registration as it is, so such a path (renamed into a dead process's
    /// claim, say) is still reached through its parent; only the plan drops it.
    pub(crate) gone_registrations: Vec<PathBuf>,
}

impl Inventory {
    /// Whether the Store at `state` is still the directory inventory reached: every component
    /// from `/` a plain directory (no link) and the same device and inode. A Store whose path
    /// now leads through a link, or names another directory, is left (ADR-0144).
    pub(crate) fn still_the_store(&self, state: &Path) -> Result<(), String> {
        let (anchor, measured) = self
            .store_identities
            .get(state)
            .ok_or("its identity was not measured, so it is left")?;
        match plain_path(anchor, state) {
            Ok(now) if now == *measured => Ok(()),
            Ok(_) => Err("another directory holds its path since it was measured; left".into()),
            Err(why) => Err(why),
        }
    }

    /// What af holds: every entry and every claim no entry measured as part of its own, the
    /// total a sweep's plan starts from.
    pub(crate) fn total(&self) -> u64 {
        self.entries
            .iter()
            .chain(self.counted_claims())
            .map(|entry| entry.bytes)
            .fold(0, u64::saturating_add)
    }

    /// The claims whose bytes count on their own: those no entry measured whole holds (a claim
    /// under a Project Store is in that Store's bytes already, and `af storage` lists it
    /// `counted_in` that Store), as the plan counts them.
    pub(crate) fn counted_claims(&self) -> impl Iterator<Item = &Entry> {
        self.claims
            .iter()
            .filter(|claim| plan::holder(&self.entries, &claim.path).is_none())
    }
}

/// The identity of the directory `path` names, reached from `anchor` (opened as given: a
/// configured root, or `/` for a Store registered elsewhere) one component at a time without
/// following a link: `Err` when any component below the anchor is a link or no directory.
fn plain_path(anchor: &Path, path: &Path) -> Result<review_sandbox::Identity, String> {
    let relative = path
        .strip_prefix(anchor)
        .map_err(|_| format!("{} is not below {}", path.display(), anchor.display()))?;
    let anchor = review_sandbox::open_anchor(anchor)
        .map_err(|error| format!("opening {}: {error}", anchor.display()))?;
    let directory = review_sandbox::open_beneath(&anchor, relative).map_err(|error| {
        format!("its path cannot be followed without a link, so it is left: {error}")
    })?;
    review_sandbox::Identity::of_descriptor(&directory)
        .map_err(|error| format!("inspecting it: {error}"))
}

fn is_hex_key(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn real_directories(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            std::fs::symlink_metadata(entry.path())
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        })
        .map(|entry| entry.path())
        .collect();
    directories.sort();
    directories
}

/// [`real_directories`] for an inventory: a root that exists but cannot be listed, or an entry
/// that cannot be read, is a problem the inventory reports, never a root that holds nothing. An
/// absent root is simply empty.
fn listed(inventory: &mut Inventory, root: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            inventory.problems.push(Problem {
                path: root.to_path_buf(),
                reason: format!("cannot be listed, so what it holds is not counted: {error}"),
            });
            return Vec::new();
        }
    };
    let mut directories = Vec::new();
    for entry in entries {
        match entry.and_then(|entry| {
            std::fs::symlink_metadata(entry.path()).map(|metadata| (entry.path(), metadata))
        }) {
            Ok((path, metadata)) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                directories.push(path);
            }
            Ok(_) => {}
            Err(error) => inventory.problems.push(Problem {
                path: root.to_path_buf(),
                reason: format!("an entry cannot be read, so it is not counted: {error}"),
            }),
        }
    }
    directories.sort();
    directories
}

fn modified_ms(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(unix_ms)
}

/// Whether some process holds the advisory lock of the file at `path`: it cannot be locked
/// exclusively right now. Taking it changes nothing about the file.
pub(crate) fn lock_held(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).is_err()
}

/// Whether a file is SQLite's shared-memory index, or a write-ahead log nothing was written to:
/// a reader creates both, so neither says the Store was used.
fn reader_artifact(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name.ends_with("-shm")
        || (name.ends_with("-wal")
            && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.len() == 0))
}

/// The newest write to a Store's event log, falling back to its directory.
fn newest_store_write(state: &Path) -> u64 {
    ["events.sqlite", "events.sqlite-wal"]
        .iter()
        .map(|name| state.join(name))
        .filter(|path| !reader_artifact(path))
        .filter_map(|path| modified_ms(&path))
        .max()
        .or_else(|| modified_ms(state))
        .unwrap_or(0)
}

/// The newest file write below a Store directory, without following a link and without the
/// files a reader — `af storage` itself included — creates.
fn newest_file_write(root: &Path) -> u64 {
    let mut newest = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(path);
            } else if !reader_artifact(&path) {
                newest = newest.max(metadata.modified().map_or(0, unix_ms));
            }
        }
    }
    newest
}

/// The writer leases a Store's Tasks hold now, read-only.
fn live_writers(state: &Path) -> Vec<(String, u64)> {
    let (Ok(cas), Ok(store)) = (
        Cas::open_existing(state.join("cas")),
        EventStore::open_read_only(state.join("events.sqlite")),
    ) else {
        return Vec::new();
    };
    store
        .plan_task_collection(&cas, 0, 0)
        .map(|plan| plan.live_writers)
        .unwrap_or_default()
}

/// Why a review campaign's Store must not be removed now: a running `af review run` holds it,
/// or one of its Tasks holds a live writer lease. Used by the sweep and by `af review gc`.
pub(crate) fn campaign_in_use(state: &Path) -> Option<String> {
    if lock_held(&state.join(RUNNING_LOCK)) {
        return Some("a running `af review run` holds it".into());
    }
    live_writers(state)
        .first()
        .map(|(task, _)| format!("Task `{task}` holds a live writer lease"))
}

/// Hold the campaign at `state` exclusively for its removal: the run lock `af review run` takes
/// shared, taken exclusive and kept through the identity-bound removal, so no run can start on
/// it in between (ADR-0144). `Err` says why it is in use: a run holds it, or a Task of it holds
/// a live writer lease.
pub(crate) fn hold_campaign_for_removal(
    state: &Path,
) -> Result<nix::fcntl::Flock<std::fs::File>, String> {
    let path = state.join(RUNNING_LOCK);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    let held = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|_| "a running `af review run` holds it".to_string())?;
    if let Some((task, _)) = live_writers(state).first() {
        return Err(format!("Task `{task}` holds a live writer lease"));
    }
    Ok(held)
}

/// Held by `af review run` for as long as it runs one campaign: while it lives, no sweep and
/// no `af review gc` removes the campaign's state.
pub(crate) fn hold_running(state: &Path) -> Result<nix::fcntl::Flock<std::fs::File>, String> {
    let path = state.join(RUNNING_LOCK);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockSharedNonblock).map_err(|(_, errno)| {
        format!(
            "the campaign's run lock {} cannot be held ({errno}); a collection may be \
                 removing this campaign, so the run does not start",
            path.display()
        )
    })
}

/// Why a Store this release cannot read must stay: a lock file in it is held.
fn unreadable_in_use(state: &Path) -> Option<String> {
    let Ok(entries) = std::fs::read_dir(state) else {
        return None;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "lock")
        })
        .find(|path| lock_held(path))
        .map(|path| format!("its lock {} is held", path.display()))
}

/// Why a Store at `state` cannot be read by this release, when another release wrote it.
fn written_by_another_release(state: &Path) -> Result<Option<String>, String> {
    let store = EventStore::open_read_only(state.join("events.sqlite"))
        .map_err(|error| error.to_string())?;
    match store.unknown_event_type() {
        Ok(Some(kind)) => Ok(Some(format!(
            "unknown event type {kind}; {}",
            review_core::event::ANOTHER_RELEASE
        ))),
        Ok(None) => Ok(None),
        Err(error)
            if error
                .to_string()
                .contains(review_core::event::ANOTHER_RELEASE) =>
        {
            Ok(Some(error.to_string()))
        }
        Err(error) => Err(error.to_string()),
    }
}

fn has_store(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join("events.sqlite")).is_ok_and(|metadata| metadata.is_file())
}

/// How finely a Task Store is taken stock of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Detail {
    /// Each Task Store as one entry of its allocated bytes: cheap, enough to know whether af
    /// is over its budget.
    Stores,
    /// Each finished Task as an entry of the bytes only it reaches, plus the rest of its Store:
    /// what `af storage` shows and what the budget evicts from once over.
    Tasks,
}

/// Take stock of every entry: the default roots and the registered Stores. Reads only.
pub(crate) fn inventory(roots: &Roots, detail: Detail) -> Inventory {
    let mut inventory = Inventory::default();
    warm_keys(roots, &mut inventory);
    for path in listed(&mut inventory, &roots.workspaces) {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !review_core::is_workspace_id(name))
        {
            continue;
        }
        measure_into(&mut inventory, Kind::Workspace, &path, |path| {
            let last = review_sandbox::storage::newest_write(path).map_or(0, unix_ms);
            let in_use = review_sandbox::workspace_in_use(path)
                .then(|| "a run holds its workspace lock".to_string());
            (last, in_use)
        });
    }
    let registered = registry::read(&roots.registry).unwrap_or_else(|error| {
        inventory.problems.push(Problem {
            path: roots.registry.clone(),
            reason: format!("the Store registry cannot be read: {error}"),
        });
        Vec::new()
    });
    inventory.gone_registrations = registered
        .iter()
        .filter(|entry| !holds_store(entry))
        .map(|entry| entry.path.clone())
        .collect();
    let mut campaigns: BTreeSet<PathBuf> = listed(&mut inventory, &roots.campaigns)
        .into_iter()
        .chain(listed(&mut inventory, &roots.local_reviews))
        .collect();
    let mut tasks: BTreeSet<PathBuf> = listed(&mut inventory, &roots.tasks).into_iter().collect();
    let mut projects: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in &registered {
        match entry.kind {
            StoreKind::Task => {
                tasks.insert(entry.path.clone());
            }
            StoreKind::Review => {
                campaigns.insert(entry.path.clone());
            }
            StoreKind::ReviewRoot => campaigns.extend(listed(&mut inventory, &entry.path)),
            StoreKind::Project => {
                projects.insert(entry.path.clone());
            }
        }
    }
    // The Surveys of at most so many Project Stores are read, in path order (the Store
    // discovery ceiling); every Project Store is measured all the same.
    let discovery = crate::projects::survey::collect::Discovery::current();
    for (index, path) in projects
        .into_iter()
        .filter(|path| has_store(path))
        .enumerate()
    {
        project_store(&mut inventory, &path, index < discovery.stores, discovery);
    }
    for path in campaigns.into_iter().filter(|path| has_store(path)) {
        match written_by_another_release(&path) {
            Ok(Some(reason)) => unreadable(&mut inventory, &path, reason),
            // Only the run lock here: a lease is checked again, in full, before a removal.
            Ok(None) => measure_into(&mut inventory, Kind::Campaign, &path, |path| {
                (
                    newest_store_write(path),
                    lock_held(&path.join(RUNNING_LOCK))
                        .then(|| "a running `af review run` holds it".to_string()),
                )
            }),
            Err(reason) => inventory.problems.push(Problem { path, reason }),
        }
    }
    for path in tasks.into_iter().filter(|path| has_store(path)) {
        match written_by_another_release(&path) {
            Ok(Some(reason)) => unreadable(&mut inventory, &path, reason),
            Ok(None) if detail == Detail::Tasks => task_store(&mut inventory, roots, &path),
            Ok(None) => {
                measure_into(&mut inventory, Kind::TaskStore, &path, |path| {
                    (
                        newest_store_write(path),
                        Some(
                            "the Store is kept; its finished Tasks are collected one by one".into(),
                        ),
                    )
                });
                record_store(&mut inventory, roots, path);
            }
            Err(reason) => inventory.problems.push(Problem { path, reason }),
        }
    }
    match crate::selfmgmt::installed_for_storage(&roots.installs) {
        Ok(versions) => {
            for version in versions {
                let protected = version.protected.map(str::to_string);
                measure_into(&mut inventory, Kind::Version, &version.directory, |_| {
                    (version.installed_unix_ms, protected)
                });
            }
        }
        Err(reason) => inventory.problems.push(Problem {
            path: PathBuf::from("versions"),
            reason,
        }),
    }
    abandoned_claims(roots, &registered, &mut inventory);
    inventory
}

/// Measure the entry at `path` with one walk: its bytes and, for a kind a removal walk takes
/// whole, whether it is nested deeper than that walk reaches, which leaves it in use.
fn measure_into(
    inventory: &mut Inventory,
    kind: Kind,
    path: &Path,
    judge: impl FnOnce(&Path) -> (u64, Option<String>),
) {
    match review_sandbox::storage::allocated_bytes_and_depth(path) {
        Ok((bytes, depth)) => {
            let (last_use_unix_ms, in_use) = judge(path);
            let too_deep = matches!(
                kind,
                Kind::Workspace | Kind::Campaign | Kind::UnreadableStore | Kind::Version
            ) && review_sandbox::deeper_than_removal_walk(depth);
            inventory.entries.push(Entry {
                kind,
                path: path.to_path_buf(),
                task_id: None,
                bytes,
                last_use_unix_ms,
                in_use: if too_deep {
                    Some(TOO_DEEP.into())
                } else {
                    in_use
                },
                reason: None,
                identity: review_sandbox::Identity::of(path).ok(),
                too_deep,
                unmeasurable: None,
                state: None,
            });
        }
        Err(error) => inventory.problems.push(Problem {
            path: path.to_path_buf(),
            reason: format!("cannot be measured: {error}"),
        }),
    }
}

fn unreadable(inventory: &mut Inventory, path: &Path, reason: String) {
    measure_into(inventory, Kind::UnreadableStore, path, |path| {
        (newest_file_write(path), unreadable_in_use(path))
    });
    if let Some(entry) = inventory.entries.last_mut()
        && entry.kind == Kind::UnreadableStore
    {
        entry.reason = Some(reason);
    }
}

fn warm_keys(roots: &Roots, inventory: &mut Inventory) {
    for project in listed(inventory, &roots.warm) {
        let Some(project_name) = project.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_hex_key(project_name) {
            continue;
        }
        for key in listed(inventory, &project) {
            let Some(toolchain) = key.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_hex_key(toolchain) {
                continue;
            }
            let lock = key.join(review_sandbox::KEY_LOCK);
            let last_use_unix_ms = modified_ms(&lock)
                .or_else(|| modified_ms(&key))
                .unwrap_or(0);
            let in_use = lock_held(&lock).then(|| "a check holds its warm.lock".to_string());
            let recorded = review_sandbox::recorded_key_size(&roots.warm, project_name, toolchain);
            let bytes = match recorded {
                Some(bytes) => Ok(bytes),
                None => review_sandbox::storage::allocated_bytes(&key),
            };
            match bytes {
                Ok(bytes) => inventory.entries.push(Entry {
                    kind: Kind::WarmKey,
                    identity: review_sandbox::Identity::of(&key).ok(),
                    path: key,
                    task_id: None,
                    bytes,
                    last_use_unix_ms,
                    in_use,
                    reason: None,
                    too_deep: false,
                    unmeasurable: None,
                    state: None,
                }),
                Err(error) => inventory.problems.push(Problem {
                    path: key,
                    reason: format!("cannot be measured: {error}"),
                }),
            }
        }
    }
}

/// A Project Store (ADR-0157): its own log and CAS, `workspaces.toml` and the Task files and
/// bindings of the Workspaces `af project workspace new` made. Its Surveys are read with it
/// unless `surveys` is off, past the Store discovery ceiling, which is then reported. The Task Stores below it are
/// registered and counted on their own, so they are left out of its bytes. It is never evicted:
/// it indexes the Project's Workspaces, and one another release wrote is kept while it still
/// holds a Workspace directory, whose Task Store this release may well read.
fn project_store(
    inventory: &mut Inventory,
    path: &Path,
    surveys: bool,
    discovery: crate::projects::survey::collect::Discovery,
) {
    let workspace_stores: Vec<PathBuf> = real_directories(&path.join("workspaces"))
        .into_iter()
        .map(|workspace| workspace.join("state"))
        .filter(|state| state.is_dir())
        .collect();
    match written_by_another_release(path) {
        Ok(Some(reason)) => {
            unreadable(inventory, path, reason);
            if let Some(entry) = inventory.entries.last_mut()
                && entry.kind == Kind::UnreadableStore
                && entry.in_use.is_none()
                && !real_directories(&path.join("workspaces")).is_empty()
            {
                entry.in_use = Some("it holds Workspace directories".into());
            }
        }
        Ok(None) => {
            measure_into(inventory, Kind::ProjectStore, path, |path| {
                (
                    newest_store_write(path),
                    Some("the Project Store is kept: it indexes the Project's Workspaces".into()),
                )
            });
            if let Some(entry) = inventory.entries.last_mut()
                && entry.kind == Kind::ProjectStore
            {
                let below = workspace_stores
                    .iter()
                    .filter_map(|state| review_sandbox::storage::allocated_bytes(state).ok())
                    .fold(0, u64::saturating_add);
                entry.bytes = entry.bytes.saturating_sub(below);
            }
            if surveys {
                surveys_into(inventory, path);
            } else {
                inventory.problems.push(Problem {
                    path: path.to_path_buf(),
                    reason: discovery.beyond_stores(),
                });
            }
        }
        Err(reason) => inventory.problems.push(Problem {
            path: path.to_path_buf(),
            reason,
        }),
    }
}

/// A readable Task Store joins collection only when its path is a plain directory from `/`;
/// one whose path leads through a link is a problem, and is left.
fn record_store(inventory: &mut Inventory, roots: &Roots, state: PathBuf) {
    // Below af's own root the root is the anchor, as configured; elsewhere (a registered
    // `--state` Store, recorded resolved) every component from `/` is checked.
    let anchor = if state.starts_with(&roots.tasks) {
        roots.tasks.clone()
    } else {
        PathBuf::from("/")
    };
    match plain_path(&anchor, &state) {
        Ok(identity) => {
            inventory
                .store_identities
                .insert(state.clone(), (anchor, identity));
            inventory.task_stores.push(state);
        }
        Err(reason) => inventory.problems.push(Problem {
            path: state,
            reason,
        }),
    }
}

fn task_store(inventory: &mut Inventory, roots: &Roots, state: &Path) {
    let plan = Cas::open_existing(state.join("cas"))
        .map_err(|error| error.to_string())
        .and_then(|cas| {
            EventStore::open_read_only(state.join("events.sqlite"))
                .and_then(|store| store.plan_task_collection(&cas, 0, 0))
                .map_err(|error| error.to_string())
        });
    let plan = match plan {
        Ok(plan) => plan,
        Err(reason) => {
            // Its Tasks cannot be told apart, but its bytes are still on disk: counted, kept
            // whole (nothing in it can be collected Task by Task), and reported.
            let bytes = match review_sandbox::storage::allocated_bytes(state) {
                Ok(bytes) => bytes,
                Err(error) => {
                    inventory.problems.push(Problem {
                        path: state.to_path_buf(),
                        reason: format!("cannot be measured whole: {error}"),
                    });
                    0
                }
            };
            inventory.entries.push(Entry {
                kind: Kind::TaskStore,
                path: state.to_path_buf(),
                task_id: None,
                bytes,
                last_use_unix_ms: newest_store_write(state),
                in_use: Some(format!("its Tasks cannot be listed: {reason}")),
                reason: None,
                identity: None,
                too_deep: false,
                unmeasurable: None,
                state: None,
            });
            inventory.problems.push(Problem {
                path: state.to_path_buf(),
                reason,
            });
            return;
        }
    };
    // A Store that cannot be measured whole is a problem, not an empty one: its Tasks are still
    // listed, and the remainder the Store holds is reported unknown rather than as zero.
    let allocated = match review_sandbox::storage::allocated_bytes(state) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            inventory.problems.push(Problem {
                path: state.to_path_buf(),
                reason: format!("cannot be measured whole: {error}"),
            });
            None
        }
    };
    let mut tasks_bytes = 0_u64;
    for task in &plan.tasks {
        if task.result_id.is_none() {
            continue;
        }
        let in_use = match &task.disposition {
            review_store::store::task::collection::CollectionDisposition::Collect => None,
            review_store::store::task::collection::CollectionDisposition::WriterLease {
                ..
            } => Some("a writer holds its lease".to_string()),
            review_store::store::task::collection::CollectionDisposition::BoundBy { tasks } => {
                Some(format!("bound by {}", tasks.join(", ")))
            }
            other => Some(format!("{other:?}").to_lowercase()),
        };
        let bytes = task.footprint.exclusive_bytes;
        tasks_bytes = tasks_bytes.saturating_add(bytes);
        inventory.entries.push(Entry {
            kind: Kind::Task,
            path: state.to_path_buf(),
            task_id: Some(task.task_id.clone()),
            bytes,
            last_use_unix_ms: task.last_event_unix_ms,
            in_use,
            reason: None,
            identity: None,
            too_deep: false,
            unmeasurable: None,
            state: None,
        });
    }
    inventory.entries.push(Entry {
        kind: Kind::TaskStore,
        path: state.to_path_buf(),
        task_id: None,
        bytes: allocated.map_or(0, |allocated| allocated.saturating_sub(tasks_bytes)),
        last_use_unix_ms: newest_store_write(state),
        in_use: Some("the Store is kept; its finished Tasks are collected one by one".into()),
        reason: None,
        identity: None,
        too_deep: false,
        unmeasurable: None,
        state: None,
    });
    record_store(inventory, roots, state.to_path_buf());
}

/// Which rule took an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Rule {
    /// Idle at least `keep_days` beyond the newest `keep_*`.
    Collection,
    /// A Survey idle at least `keep_days` beyond the newest `[storage] keep_surveys` of its
    /// Project (ADR-0158 clause 13).
    KeepSurveys,
    /// Least recently used while af held more than `max_bytes`.
    Budget,
    /// The finish of a removal a process that died left claimed, or of a Survey's deletion an
    /// interruption left.
    Recovery,
}

impl Rule {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Rule::Collection => "collection",
            Rule::KeepSurveys => "keep_surveys",
            Rule::Budget => "budget",
            Rule::Recovery => "recovery",
        }
    }
}

/// One entry a sweep removed, or would remove.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Removal {
    pub(crate) kind: Kind,
    pub(crate) path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<String>,
    /// What it frees: as planned in a preview, as freed by the applied sweep.
    pub(crate) bytes: u64,
    pub(crate) rule: Rule,
    /// af's total after it, the same way; `unknown` (`None`) from a removal on that stopped part
    /// way and could not measure what it left.
    #[serde(serialize_with = "total")]
    pub(crate) total_after: Option<u64>,
    /// A Survey collected `unreadable` (ADR-0158 clause 13): why its record's bytes were no
    /// Survey, as its tombstone says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unreadable: Option<String>,
    /// The applied sweep only, any kind: `failed: <reason>` when running the planned entry
    /// failed (the lock lost, what it names replaced or gone since inventory, an I/O error), so
    /// it freed nothing and the total after it is the one before it; or, when a deletion stopped
    /// part way, `bytes` is what left the disk before it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<String>,
}

/// One file or Survey a sweep's plan left where it is, with its reason (ADR-0158 clause 13). A
/// preview lists exactly what its pass leaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Left {
    pub(crate) kind: Kind,
    /// Its path as text: as it is when it is UTF-8, otherwise in `review_core::encode_path`'s
    /// spelling with `path_encoded`.
    pub(crate) path: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) path_encoded: bool,
    pub(crate) bytes: u64,
    /// `held`, `foreign`, `oversize`, `unreadable_name`, `too_deep` or `unmeasurable`.
    pub(crate) reason: &'static str,
    pub(crate) detail: String,
    /// The raw path, for the text output's lossy display.
    #[serde(skip)]
    pub(crate) raw: PathBuf,
}

impl Left {
    fn of(left: plan::Left) -> Self {
        use std::os::unix::ffi::OsStrExt;
        let raw = left.left.path;
        let (path, path_encoded) = match raw.to_str() {
            Some(text) => (text.to_string(), false),
            None => (review_core::encode_path(raw.as_os_str().as_bytes()), true),
        };
        Self {
            kind: left.kind,
            path,
            path_encoded,
            bytes: left.left.bytes,
            reason: left.left.reason.as_str(),
            detail: left.left.detail,
            raw,
        }
    }
}

/// Why the budget step stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stop {
    /// The total fits the budget.
    Fits,
    /// Over the budget, and every remaining entry is in use or was used within the hour.
    NothingEvictable,
    /// The total is unknown: a removal stopped part way and what it left could not be
    /// measured, so neither is claimed.
    Unknown,
}

/// A total as the prune document spells it: its bytes, or `unknown`.
fn total<S: serde::Serializer>(total: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
    match total {
        Some(bytes) => serializer.serialize_u64(*bytes),
        None => serializer.serialize_str("unknown"),
    }
}

/// A total as a line says it.
fn total_text(total: Option<u64>) -> String {
    total.map_or_else(|| "unknown".to_string(), human_bytes)
}

/// What one sweep did (or, previewed, would do).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SweepReport {
    pub(crate) applied: bool,
    pub(crate) collection: bool,
    pub(crate) max_bytes: u64,
    pub(crate) total_before: u64,
    /// af's total after the sweep; `unknown` (`None`) once a removal stopped part way and could
    /// not measure what it left.
    #[serde(serialize_with = "total")]
    pub(crate) total_after: Option<u64>,
    pub(crate) stop: Stop,
    pub(crate) removals: Vec<Removal>,
    /// What the plan left where it is, with its reason.
    pub(crate) left: Vec<Left>,
    /// Gate cleanups of finished Tasks this sweep tried (collection only).
    pub(crate) gate_cleanups: Vec<gate::Attempted>,
    /// Registry entries whose path no longer holds a Store.
    pub(crate) registry_dropped: Vec<PathBuf>,
    /// Removals that failed; the next sweep tries again.
    pub(crate) failures: Vec<String>,
}

/// Run the sweep over one snapshot: [`plan::make`] plans the recovery of interrupted Survey
/// deletions, collection when `collection`, and the budget step, and without `apply` that plan
/// is the report; with it, [`plan::run`] runs the same plan entry by entry.
pub(crate) fn sweep(
    roots: &Roots,
    policy: &StoragePolicy,
    collection: bool,
    apply: bool,
) -> SweepReport {
    // One sweep at a time per process: a floor check and a new warm key may race.
    static RUNNING: Mutex<()> = Mutex::new(());
    let _one = RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = now_unix_ms();
    let mut report = SweepReport {
        applied: apply,
        collection,
        max_bytes: policy.max_bytes,
        total_before: 0,
        total_after: Some(0),
        stop: Stop::Fits,
        removals: Vec::new(),
        left: Vec::new(),
        gate_cleanups: Vec::new(),
        registry_dropped: Vec::new(),
        failures: Vec::new(),
    };
    // The one snapshot: Task Stores are told apart into their finished Tasks only when it holds
    // more than the budget, and nothing is measured twice. Removals dead processes left claimed
    // are in it, so they are planned like every other removal, never finished beside the plan,
    // and it is taken over the registrations as they are: one whose path holds no Store is
    // dropped by the plan, after its parent's claims were inventoried, never before.
    let mut stock = inventory(roots, Detail::Stores);
    if stock.total() > policy.max_bytes {
        refine_tasks(roots, &mut stock);
    }
    // What could not be measured is said with the result: a total that fits the budget is only
    // as complete as the inventory behind it.
    for problem in &stock.problems {
        let line = format!("measuring {}: {}", problem.path.display(), problem.reason);
        if !report.failures.contains(&line) {
            report.failures.push(line);
        }
    }
    // Gate leftovers of finished Tasks are retried by every applied sweep, whatever the budget
    // and whether age-based collection is on: the pull request and branches are GitHub's
    // clutter, not something the budget weighs (ADR-0144).
    let mut retried: BTreeMap<PathBuf, Vec<gate::Attempted>> = BTreeMap::new();
    if apply && !policy.keep_gate_pull_requests {
        for state in &stock.task_stores {
            // A Store gone since inventory holds no gate leftovers to retry: no failure, as
            // collection says of it below.
            if !has_store(state) {
                continue;
            }
            if let Err(why) = stock.still_the_store(state) {
                report.failures.push(format!("{}: {why}", state.display()));
                continue;
            }
            let tried = gate::retry_in_store(state);
            report.gate_cleanups.extend(tried.iter().cloned());
            retried.insert(state.clone(), tried);
        }
    }
    let picks = if collection {
        task_picks(roots, policy, &stock, now, apply, &mut report)
    } else {
        Vec::new()
    };
    let mut entries = stock.entries.clone();
    for entry in &mut entries {
        // The sweep that ends a run never takes the Task the run executed.
        if entry.in_use.is_none()
            && entry
                .task_id
                .as_deref()
                .is_some_and(|task_id| is_run_task(&entry.path, task_id))
        {
            entry.in_use = Some("the Task this run executed".into());
        }
    }
    let made = plan::make(
        plan::Inputs {
            entries,
            claims: stock.claims.clone(),
            projects: &stock.projects,
            tasks: picks.clone(),
            registrations: stock.gone_registrations.clone(),
            collection,
            max_bytes: policy.max_bytes,
            keep_campaigns: policy.keep_campaigns,
            keep_surveys: policy.keep_surveys,
            idle_ms: policy.keep_days.saturating_mul(DAY_MS),
            now,
        },
        &mut |store: &Path, live: &crate::projects::survey::collect::Live, inventoried| {
            crate::projects::survey::collect::judge(store, live, inventoried)
        },
    );
    report.total_before = made.total_before;
    if !apply {
        plan::preview(made, &mut report);
        return report;
    }
    let registrations = made.registrations.clone();
    let mut collected = BTreeMap::new();
    plan::run(made, &mut report, &mut |planned| {
        before_execute(planned);
        execute_planned(
            roots,
            policy,
            &stock,
            (&picks, &retried, &mut collected),
            planned,
        )
    });
    match drop_registrations(roots, &registrations) {
        Ok(dropped) => report.registry_dropped = dropped,
        Err(error) => report.failures.push(error),
    }
    report
}

#[cfg(test)]
type BeforeExecute = Box<dyn FnMut(&plan::Planned)>;

#[cfg(test)]
thread_local! {
    /// A unit test's hook, run on the sweeping thread just before each planned entry of an
    /// applied sweep: where a test changes what the plan measured, as another process could.
    static BEFORE_EXECUTE: std::cell::RefCell<Option<BeforeExecute>> =
        const { std::cell::RefCell::new(None) };
}

fn before_execute(planned: &plan::Planned) {
    #[cfg(test)]
    BEFORE_EXECUTE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(planned);
        }
    });
    #[cfg(not(test))]
    let _ = planned;
}

/// Tell the snapshot's readable Task Stores apart into their finished Tasks and the rest, in
/// place: only their entries change, so what the snapshot says of every other entry, a Survey's
/// pending recovery included, stays as it was read.
fn refine_tasks(roots: &Roots, stock: &mut Inventory) {
    for state in stock.task_stores.clone() {
        let mut told = Inventory::default();
        task_store(&mut told, roots, &state);
        stock
            .entries
            .retain(|entry| !(entry.kind == Kind::TaskStore && entry.path == state));
        stock.entries.extend(told.entries);
        for problem in told.problems {
            if !stock
                .problems
                .iter()
                .any(|known| known.path == problem.path && known.reason == problem.reason)
            {
                stock.problems.push(problem);
            }
        }
    }
}

/// What one Task collection did, per Task of one Store.
type Collected = BTreeMap<PathBuf, BTreeMap<String, Result<u64, String>>>;

/// Run one planned entry of an applied sweep: what it freed, and for a Survey why it was
/// collected `unreadable`. The finished Tasks of one Store are collected together, at that
/// Store's first planned Task.
fn execute_planned(
    roots: &Roots,
    policy: &StoragePolicy,
    stock: &Inventory,
    (picks, retried, collected): (
        &[plan::TaskPick],
        &BTreeMap<PathBuf, Vec<gate::Attempted>>,
        &mut Collected,
    ),
    planned: &plan::Planned,
) -> plan::Outcome {
    let removal = &planned.removal;
    match &planned.job {
        plan::Job::Survey(job) => {
            let verb = match removal.rule {
                Rule::Recovery => "finishing",
                Rule::Budget => "removing",
                Rule::Collection | Rule::KeepSurveys => "collecting",
            };
            let failed = |why: String| format!("{verb} {}: {why}", removal.path.display());
            stock.still_the_store(job.store()).map_err(failed)?;
            crate::projects::survey::collect::execute(job)
                .map(|done| plan::Ran {
                    freed: done.bytes,
                    unreadable: done.unreadable,
                    started: None,
                })
                .map_err(|why| failed(why).into())
        }
        plan::Job::Evict(entry) => {
            evict(roots, policy, stock, entry).map(|(freed, started)| match started {
                // What it held when its removal started is what left the disk.
                Some(started) => plan::Ran {
                    freed: started,
                    unreadable: None,
                    started: Some(started),
                },
                // At least what inventory measured is gone; a Task's collection may free more.
                None if removal.rule == Rule::Budget => plan::Ran::freed(freed.max(entry.bytes)),
                None => plan::Ran::freed(entry.bytes),
            })
        }
        plan::Job::Task => {
            let state = &removal.path;
            let task_id = removal.task_id.clone().unwrap_or_default();
            if !collected.contains_key(state) {
                let ids: Vec<String> = picks
                    .iter()
                    .filter(|pick| pick.state == *state)
                    .map(|pick| pick.task_id.clone())
                    .collect();
                let tried = retried.get(state).cloned().unwrap_or_default();
                let outcome = collect_tasks(stock, policy, state, &ids, &tried);
                collected.insert(state.clone(), outcome);
            }
            collected
                .get_mut(state)
                .and_then(|outcome| outcome.remove(&task_id))
                .unwrap_or_else(|| {
                    Err(format!(
                        "collecting {}: Task `{task_id}` was not collected",
                        state.display()
                    ))
                })
                .map(plan::Ran::freed)
                .map_err(plan::Failed::from)
        }
    }
}

/// Collect the planned finished Tasks `task_ids` of the Store at `state`: gate leftovers go
/// first, so a Task whose cleanup does not end done stays and the next sweep tries again (one
/// this sweep already tried and failed is not tried twice); then exactly the Tasks whose
/// leftovers are gone, through the Store's own collection, each with the bytes only it reached.
fn collect_tasks(
    stock: &Inventory,
    policy: &StoragePolicy,
    state: &Path,
    task_ids: &[String],
    tried: &[gate::Attempted],
) -> BTreeMap<String, Result<u64, String>> {
    let failed = |why: String| format!("collecting {}: {why}", state.display());
    let mut outcome = BTreeMap::new();
    if let Err(why) = stock.still_the_store(state) {
        for task_id in task_ids {
            outcome.insert(task_id.clone(), Err(failed(why.clone())));
        }
        return outcome;
    }
    let mut cleared = BTreeSet::new();
    for task_id in task_ids {
        let failed_now = tried.iter().find(|attempted| {
            attempted.task_id == *task_id
                && attempted.outcome != review_core::task::remote_check::GateCleanupOutcomeV1::Done
        });
        let verdict = match failed_now {
            Some(attempted) => Err(gate::stays(task_id, &attempted.summary)),
            None => gate::before_collection(state, task_id, policy.keep_gate_pull_requests),
        };
        match verdict {
            Ok(()) => {
                cleared.insert(task_id.clone());
            }
            Err(why) => {
                outcome.insert(task_id.clone(), Err(failed(why)));
            }
        }
    }
    if cleared.is_empty() {
        return outcome;
    }
    // Exactly the planned Tasks whose leftovers are gone: the plan already applied the age and
    // newest rules, so a cleanup recorded just now does not make one recent.
    let applied = (|| -> Result<_, String> {
        let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
        let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        store
            .apply_task_collection_of(&cas, &cleared)
            .map_err(|e| e.to_string())
    })();
    for task_id in cleared {
        let result = match &applied {
            Err(error) => Err(failed(error.clone())),
            Ok(done) if done.tombstoned.contains(&task_id) => Ok(done
                .plan
                .tasks
                .iter()
                .find(|task| task.task_id == task_id)
                .map_or(0, |task| task.footprint.exclusive_bytes)),
            Ok(_) => Err(failed(format!(
                "Task `{task_id}` is protected now, so it is kept"
            ))),
        };
        outcome.insert(task_id, result);
    }
    outcome
}

/// List, reading only, the removals dead processes left claimed in every directory af removes
/// entries from: each warm project level, the Workspaces, campaigns, local reviews, Task Stores
/// and installed versions, the parent of every registered Store and the Claude project
/// directories. Each is an entry of kind `claim` with the identity and bytes the listing found,
/// which its removal checks again (ADR-0144).
fn abandoned_claims(roots: &Roots, registered: &[registry::Registered], inventory: &mut Inventory) {
    let mut parents: Vec<PathBuf> = real_directories(&roots.warm);
    parents.extend([
        roots.workspaces.clone(),
        roots.campaigns.clone(),
        roots.local_reviews.clone(),
        roots.tasks.clone(),
        roots.installs.versions.clone(),
    ]);
    parents.extend(
        registered
            .iter()
            .filter_map(|entry| entry.path.parent().map(Path::to_path_buf)),
    );
    // Claude Attempt and probe histories are removed through the same claim.
    parents.extend(
        crate::providers::registered_claude_config_dirs()
            .into_iter()
            .map(|directory| directory.join("projects")),
    );
    parents.sort();
    parents.dedup();
    for parent in parents {
        for claim in review_sandbox::abandoned_claims(&parent) {
            // A claim the walk cannot measure holds no bytes the plan could account for: it is
            // left, never planned, and said.
            let (bytes, too_deep, unmeasurable) = match claim.measured {
                Ok(size) => (
                    size.bytes,
                    review_sandbox::deeper_than_removal_walk(size.depth),
                    None,
                ),
                Err(review_sandbox::Unmeasured::TooDeep(_)) => (0, true, None),
                Err(review_sandbox::Unmeasured::Unreadable(why)) => (0, false, Some(why)),
            };
            inventory.claims.push(Entry {
                kind: Kind::Claim,
                path: parent.join(&claim.name),
                task_id: None,
                bytes,
                last_use_unix_ms: 0,
                in_use: match &unmeasurable {
                    Some(why) => Some(format!("left: unmeasurable: {why}")),
                    None => too_deep.then(|| TOO_DEEP.into()),
                },
                reason: None,
                identity: Some(claim.identity),
                too_deep,
                unmeasurable,
                state: Some("abandoned"),
            });
        }
    }
}

/// Whether a registered path still holds what it was registered for.
fn holds_store(entry: &registry::Registered) -> bool {
    match entry.kind {
        StoreKind::Task | StoreKind::Review | StoreKind::Project => has_store(&entry.path),
        StoreKind::ReviewRoot => entry.path.is_dir(),
    }
}

/// Drop the registrations the applied sweep's plan listed, each only while its path still holds
/// no Store, and return those it dropped. One that holds a Store again is kept, and one another
/// sweep dropped meanwhile is already what the plan wanted: neither is a failure.
fn drop_registrations(roots: &Roots, planned: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    if planned.is_empty() || !roots.registry.exists() {
        return Ok(Vec::new());
    }
    Ok(registry::retain(&roots.registry, |entry| {
        !planned.contains(&entry.path) || holds_store(entry)
    })?
    .into_iter()
    .map(|entry| entry.path)
    .collect())
}

/// The finished Tasks collection takes: per readable Task Store, those beyond the newest
/// `keep_tasks` idle at least `keep_days`, as the Store's own read-only plan names them, never
/// the Task the run executed. An unchanged Store whose last look found nothing to do until
/// later is not read again, and an applied sweep that finds nothing writes that look down.
fn task_picks(
    roots: &Roots,
    policy: &StoragePolicy,
    stock: &Inventory,
    now: u64,
    apply: bool,
    report: &mut SweepReport,
) -> Vec<plan::TaskPick> {
    let idle_ms = policy.keep_days.saturating_mul(DAY_MS);
    // A Store that went away since inventory, or one still being created that has no CAS yet,
    // holds nothing to collect: neither is a failure.
    let gone = |state: &Path| !has_store(state) || !state.join("cas").is_dir();
    let mut picks = Vec::new();
    for state in &stock.task_stores {
        if gone(state) {
            continue;
        }
        if let Err(why) = stock.still_the_store(state) {
            report
                .failures
                .push(format!("collecting {}: {why}", state.display()));
            continue;
        }
        let hint = hint::Hint::read(roots, state);
        if hint
            .as_ref()
            .is_some_and(|hint| hint.quiet(state, now, policy))
        {
            continue;
        }
        let planned = (|| -> Result<Vec<plan::TaskPick>, String> {
            let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
            let plan = EventStore::open_read_only(state.join("events.sqlite"))
                .and_then(|store| store.plan_task_collection(&cas, idle_ms, policy.keep_tasks))
                .map_err(|e| e.to_string())?;
            let picked: Vec<plan::TaskPick> = plan
                .tasks
                .iter()
                .filter(|task| task.disposition.collects() && !is_run_task(state, &task.task_id))
                .map(|task| plan::TaskPick {
                    state: state.clone(),
                    task_id: task.task_id.clone(),
                    bytes: task.footprint.exclusive_bytes,
                })
                .collect();
            if apply && picked.is_empty() {
                hint::Hint::after(&plan, state, policy, gate::has_pending(state))
                    .write(roots, state);
            }
            Ok(picked)
        })();
        match planned {
            Ok(picked) => picks.extend(picked),
            Err(_) if gone(state) => {}
            Err(error) => report
                .failures
                .push(format!("collecting {}: {error}", state.display())),
        }
    }
    picks
}

/// Remove one entry through the rule that owns it. Returns the bytes removed and, for a
/// directory removed through descriptors, what it held when its removal started; a removal that
/// stopped part way fails with the bytes it freed before it did and what it left.
fn evict(
    roots: &Roots,
    policy: &StoragePolicy,
    stock: &Inventory,
    entry: &Entry,
) -> Result<(u64, Option<u64>), plan::Failed> {
    let failed = |error: String| format!("removing {}: {error}", entry.path.display());
    let partly = |failure: plan::Failed| plan::Failed {
        reason: failed(failure.reason),
        ..failure
    };
    match entry.kind {
        Kind::WarmKey => {
            let toolchain = entry.path.file_name().and_then(|name| name.to_str());
            let project = entry
                .path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str());
            let (Some(project), Some(toolchain)) = (project, toolchain) else {
                return Err(failed("not a toolchain key".into()).into());
            };
            match review_sandbox::lock_task_build_cache_key(
                &roots.warm,
                project,
                toolchain,
                Duration::ZERO,
            )
            .map_err(failed)?
            {
                Some(key) => {
                    // Locking reaches the key by name; only the directory inventory measured
                    // may go. A key renamed away and replaced since is another directory.
                    let Some(measured) = entry.identity else {
                        return Err(failed(
                            "its identity was not measured, so it is not removed".into(),
                        )
                        .into());
                    };
                    match key.identity() {
                        Ok(locked) if locked == measured => {
                            Ok((key.remove_key().map_err(failed)?, None))
                        }
                        Ok(_) => Err(failed(
                            "another directory holds its name since it was measured; left in \
                             place"
                                .into(),
                        )
                        .into()),
                        Err(error) => {
                            Err(failed(format!("inspecting the locked key: {error}")).into())
                        }
                    }
                }
                None => Err(failed("a check holds it now".into()).into()),
            }
        }
        Kind::Workspace => {
            // Not even its lock file is touched unless it is still the measured directory.
            still_measured(entry).map_err(failed)?;
            // Held exclusively until it is gone: no run clones from it in between.
            let _held = review_sandbox::hold_workspace_for_removal(&entry.path).map_err(failed)?;
            remove_entry(roots, entry).map_err(partly)
        }
        Kind::Campaign => {
            still_measured(entry).map_err(failed)?;
            // Held exclusively until it is gone: no `af review run` starts on it in between.
            let _held = hold_campaign_for_removal(&entry.path).map_err(failed)?;
            remove_entry(roots, entry).map_err(partly)
        }
        Kind::UnreadableStore => {
            if let Some(why) = unreadable_in_use(&entry.path) {
                return Err(failed(why).into());
            }
            remove_entry(roots, entry).map_err(partly)
        }
        Kind::Version => {
            let version = entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| failed("names no version".into()))?;
            // Under the lock `af self` holds while it changes the default or a pin, the
            // protection is read again: what became the default, a pin or the running binary
            // since inventory stays.
            let _versions = crate::selfmgmt::try_lock_versions(&roots.installs).map_err(failed)?;
            if let Some(why) = crate::selfmgmt::protected_now(&roots.installs, version) {
                return Err(failed(format!(
                    "af {version} became {why} since it was measured, so it is kept"
                ))
                .into());
            }
            remove_entry(roots, entry).map_err(partly)
        }
        Kind::Task => {
            let task_id = entry
                .task_id
                .clone()
                .ok_or_else(|| failed("names no Task".into()))?;
            // Only the Store inventory reached: a path that now leads through a link, or names
            // another Store, is left (ADR-0144).
            stock.still_the_store(&entry.path).map_err(failed)?;
            // A Task whose gate leftovers are not cleaned up is never collected before they are.
            gate::before_collection(&entry.path, &task_id, policy.keep_gate_pull_requests)
                .map_err(failed)?;
            let cas =
                Cas::open_existing(entry.path.join("cas")).map_err(|e| failed(e.to_string()))?;
            let mut store = EventStore::open(entry.path.join("events.sqlite"))
                .map_err(|e| failed(e.to_string()))?;
            let before = review_sandbox::storage::allocated_bytes(&entry.path).ok();
            let outcome = store
                .apply_task_collection_of(&cas, &BTreeSet::from([task_id.clone()]))
                .map_err(|e| failed(e.to_string()))?;
            if outcome.tombstoned.contains(&task_id) {
                // Collection also sweeps whatever no Task reaches any more, an interrupted
                // earlier collection's leftovers included: what the Store freed is measured.
                let after = review_sandbox::storage::allocated_bytes(&entry.path).ok();
                Ok((
                    match (before, after) {
                        (Some(before), Some(after)) => {
                            before.saturating_sub(after).max(entry.bytes)
                        }
                        _ => entry.bytes,
                    },
                    None,
                ))
            } else {
                Err(failed(format!("Task `{task_id}` is protected now")).into())
            }
        }
        Kind::TaskStore => Err(failed("a Task Store is never removed whole".into()).into()),
        Kind::ProjectStore => Err(failed("a Project Store is never evicted".into()).into()),
        Kind::Survey => Err(failed(
            "a Survey is collected by its planned job, behind its tombstone".into(),
        )
        .into()),
        Kind::Claim => {
            // The claim's parent is its anchor, as the listing opened it; only the directory the
            // listing stated is removed.
            let parent = entry
                .path
                .parent()
                .ok_or_else(|| failed("names no claim".into()))?;
            remove_directory(parent, &entry.path, entry.identity, entry.bytes).map_err(partly)
        }
    }
}

/// Whether an entry's path still names the directory inventory measured, before anything (a
/// lock file) is written into it. The removal checks again through descriptors.
fn still_measured(entry: &Entry) -> Result<(), String> {
    let measured = entry
        .identity
        .ok_or("its identity was not measured, so it is not removed")?;
    match review_sandbox::Identity::of(&entry.path) {
        Ok(now) if now == measured => Ok(()),
        Ok(_) => {
            Err("it is not the directory that was measured; it changed since and is left".into())
        }
        Err(error) => Err(format!("inspecting it: {error}")),
    }
}

/// Remove an entry's directory from its anchor through descriptors, never following a link
/// below the anchor, and only when it is still the directory inventory measured. Returns what
/// was removed and what the directory held when its removal started ([`remove_directory`]).
fn remove_entry(roots: &Roots, entry: &Entry) -> Result<(u64, Option<u64>), plan::Failed> {
    let identity = entry
        .identity
        .ok_or("its identity was not measured, so it is not removed".to_string())?;
    remove_directory(
        roots.anchor_of(&entry.path),
        &entry.path,
        Some(identity),
        entry.bytes,
    )
}

/// Remove `path`, strictly below `anchor`, and everything below it through descriptors opened
/// from `anchor` without following a link, refusing any directory but `identity`. What was
/// removed is what the directory held when its removal started, stated through its own
/// descriptor immediately before anything was unlinked (`measured`, the bytes inventory
/// counted, when that cannot be stated), and that start is returned with it so the sweep's
/// running total reconciles a directory that grew or shrank since inventory. A removal that
/// stopped part way (a tree grown deeper than the removal walk since inventory, a child that
/// cannot be unlinked, a final `rmdir` that failed) fails with what left the disk, accounted
/// from that same start: its bytes then less what its claim still holds, so a tree that grew
/// since inventory never reports nothing for what the walk unlinked; and with what the claim
/// still holds, which takes the place of inventory's bytes in the total. When the start cannot
/// be measured nothing is counted freed, and when what the claim still holds cannot be
/// measured the total is unknown from there on.
fn remove_directory(
    anchor: &Path,
    path: &Path,
    identity: Option<review_sandbox::Identity>,
    measured: u64,
) -> Result<(u64, Option<u64>), plan::Failed> {
    let error = match review_sandbox::remove_beneath_measured(anchor, path, identity) {
        Ok(started) => return Ok((started.unwrap_or(measured), started)),
        Err(error) => error,
    };
    match review_sandbox::partial(&error) {
        None => Err(format!("{error}; it is left, and the next sweep tries again").into()),
        Some(partly) => {
            let remains = match partly.remaining {
                Some(left) => plan::Remains::Measured(left),
                None => plan::Remains::Unmeasured,
            };
            Err(match (partly.freed, partly.remaining) {
                (Some(freed), Some(left)) => plan::Failed {
                    reason: format!(
                        "{error}; {freed} bytes of it were freed and the rest, {left} bytes, is \
                         left under its claim"
                    ),
                    freed,
                    remains,
                },
                (None, Some(left)) => plan::Failed {
                    reason: format!(
                        "{error}; what it held when its removal started cannot be measured, so \
                         nothing of it is counted freed, and the rest, {left} bytes, is left \
                         under its claim"
                    ),
                    freed: 0,
                    remains,
                },
                (_, None) => plan::Failed {
                    reason: format!(
                        "{error}; what is left under its claim cannot be measured, so nothing of \
                         it is counted freed and af's total after it is unknown"
                    ),
                    freed: 0,
                    remains,
                },
            })
        }
    }
}

/// Print a sweep's removals and problems to stderr, one line each.
pub(crate) fn report_to_stderr(report: &SweepReport) {
    for removal in &report.removals {
        eprintln!("af storage: {}", removal_line(removal, report.applied));
    }
    for left in &report.left {
        eprintln!("af storage: {}", left_line(left));
    }
    for cleanup in &report.gate_cleanups {
        eprintln!(
            "af storage: gate cleanup of Task {}: {}",
            cleanup.task_id, cleanup.summary
        );
    }
    for failure in unmarked_failures(report) {
        eprintln!("af storage: warning: {failure}");
    }
    match report.stop {
        Stop::NothingEvictable => eprintln!(
            "af storage: warning: af holds {} against a budget of {}, and nothing else may be \
             removed now (in use, or used within the hour); see `af storage`",
            total_text(report.total_after),
            human_bytes(report.max_bytes)
        ),
        Stop::Unknown => eprintln!(
            "af storage: warning: what af holds after this sweep is unknown: a removal stopped \
             part way and what it left cannot be measured; see `af storage`"
        ),
        Stop::Fits => {}
    }
}

/// The Task the run that ends with the sweep executed — its Store and its ID — when it was one.
static RUN_TASK: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

/// Note the Task this process's `af task run` (or `af task start --execute`) executes, so the
/// sweep that ends the run records what it removed on it (ADR-0144).
pub(crate) fn note_task_run(state: &Path, task_id: &str) {
    *RUN_TASK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some((state.to_path_buf(), task_id.to_string()));
}

/// Whether `task_id` in the Store at `state` is the Task this process's run executed. The sweep
/// that ends the run never collects or evicts it: the sweep is recorded on that Task afterwards,
/// and a later sweep takes it under the same rules as any other.
fn is_run_task(state: &Path, task_id: &str) -> bool {
    let run = RUN_TASK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let Some((run_state, run_task)) = run else {
        return false;
    };
    if run_task != task_id {
        return false;
    }
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(&run_state) == canonical(state)
}

/// What a sweep removed and failed, as the bounded record a Task keeps: `None` for a preview,
/// and for a sweep that removed nothing and failed nothing.
pub(crate) fn observation(
    report: &SweepReport,
) -> Option<review_core::task::storage_sweep::TaskStorageSweepV1> {
    use review_core::task::storage_sweep::{
        MAX_SWEEP_FAILURE_BYTES, MAX_SWEEP_FAILURES, MAX_SWEEP_PATH_BYTES, MAX_SWEEP_REMOVALS,
        StorageSweepKindV1, StorageSweepRemovalV1, StorageSweepRuleV1, StorageSweepStopV1,
        TASK_STORAGE_SWEEP_V1, TaskStorageSweepV1,
    };
    // What failed freed nothing: a failure of the record, never a removal.
    let removed: Vec<&Removal> = report
        .removals
        .iter()
        .filter(|removal| removal.outcome.is_none())
        .collect();
    if !report.applied || (removed.is_empty() && report.failures.is_empty()) {
        return None;
    }
    // A total that is unknown has no number the record could hold: the sweep's own warning says
    // so, and the record is not written (its shape keeps its version).
    let total_after = report.total_after?;
    let listable: Vec<StorageSweepRemovalV1> = removed
        .iter()
        .filter_map(|removal| {
            let kind = match removal.kind {
                Kind::WarmKey => StorageSweepKindV1::WarmKey,
                Kind::Workspace => StorageSweepKindV1::Workspace,
                Kind::Campaign => StorageSweepKindV1::Campaign,
                Kind::Task => StorageSweepKindV1::Task,
                Kind::UnreadableStore => StorageSweepKindV1::UnreadableStore,
                Kind::Version => StorageSweepKindV1::Version,
                Kind::Claim => StorageSweepKindV1::Claim,
                // A Survey's removal has no kind in `af/TaskStorageSweep@1`: counted, not listed.
                Kind::TaskStore | Kind::ProjectStore | Kind::Survey => return None,
            };
            let path = removal.path.to_str()?.to_string();
            (path.len() <= MAX_SWEEP_PATH_BYTES && !path.chars().any(char::is_control)).then(|| {
                StorageSweepRemovalV1 {
                    kind,
                    path,
                    task_id: removal.task_id.clone(),
                    bytes: removal.bytes,
                    rule: match removal.rule {
                        // A Survey's rule: never listed, as its kind is not.
                        Rule::Collection | Rule::KeepSurveys => StorageSweepRuleV1::Collection,
                        Rule::Budget => StorageSweepRuleV1::Budget,
                        Rule::Recovery => StorageSweepRuleV1::Recovery,
                    },
                }
            })
        })
        .collect();
    let removals: Vec<_> = listable.into_iter().take(MAX_SWEEP_REMOVALS).collect();
    // Beyond the bound, or with a path no record can name: counted, not listed.
    let omitted_removals = removed.len().saturating_sub(removals.len()) as u64;
    let failures: Vec<String> = report
        .failures
        .iter()
        .filter(|failure| !failure.trim().is_empty())
        .take(MAX_SWEEP_FAILURES)
        .map(|failure| {
            let mut end = failure.len().min(MAX_SWEEP_FAILURE_BYTES);
            while !failure.is_char_boundary(end) {
                end -= 1;
            }
            failure[..end].to_string()
        })
        .collect();
    let omitted_failures = report.failures.len().saturating_sub(MAX_SWEEP_FAILURES) as u64;
    let sweep = TaskStorageSweepV1 {
        schema: TASK_STORAGE_SWEEP_V1.into(),
        omitted_removals,
        removals,
        omitted_failures: if failures.len() == MAX_SWEEP_FAILURES {
            omitted_failures
        } else {
            0
        },
        failures,
        stop: match report.stop {
            Stop::Fits => StorageSweepStopV1::Fits,
            Stop::NothingEvictable => StorageSweepStopV1::NothingEvictable,
            Stop::Unknown => return None,
        },
        max_bytes: report.max_bytes,
        total_before: report.total_before,
        total_after,
    };
    sweep.validate().is_ok().then_some(sweep)
}

/// Record the sweep that ended a Task's run as an observation of that Task, under a short lease
/// of its own. A warning when it cannot be: the sweep is housekeeping, never the run's outcome.
fn record_on_task(report: &SweepReport) {
    let Some((state, task_id)) = RUN_TASK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    let Some(sweep) = observation(report) else {
        return;
    };
    let recorded = (|| -> Result<(), String> {
        let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
        let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        let lease = store
            .take_task_lease(
                &cas,
                &task_id,
                &format!("af-storage-sweep-{}", std::process::id()),
                30_000,
            )
            .map_err(|e| e.to_string())?;
        let recorded = store
            .record_task_storage_sweep(&cas, &lease, sweep)
            .map(|_| ())
            .map_err(|e| e.to_string());
        let released = store
            .release_task_lease(&cas, &lease)
            .map(|_| ())
            .map_err(|e| e.to_string());
        recorded.and(released)
    })();
    if let Err(error) = recorded {
        eprintln!(
            "af storage: warning: the sweep after the run was not recorded on Task {task_id}: \
             {error}"
        );
    }
}

/// The end of `af task run` and `af review run`, whatever the outcome of the work they started:
/// the sweep that holds af to `[storage] max_bytes`, with age-based collection only when
/// `[storage] auto_gc` is on (off by default: nothing a user still has room for goes by age
/// alone). Its failure is a warning, never the command's.
pub(crate) fn after_run() {
    let outcome =
        policy().and_then(|policy| Ok(sweep(&Roots::current()?, &policy, policy.auto_gc, true)));
    match outcome {
        Ok(report) => {
            report_to_stderr(&report);
            record_on_task(&report);
        }
        Err(error) => {
            eprintln!("af storage: warning: the sweep after the run did not run: {error}")
        }
    }
}

/// The machine's Storage Budget as the pipeline meets it.
struct MachineStorage {
    policy: StoragePolicy,
    roots: Roots,
}

impl review_pipeline::storage::StorageHost for MachineStorage {
    fn ensure_free_disk(&self) -> Result<(), String> {
        let floor = self.policy.min_free_bytes;
        if judge_floor(&volumes(), floor).is_ok() {
            return Ok(());
        }
        // Every time the floor is met: what became evictable since the last one (a check that
        // finished, a key released) may restore the room. The same sweep as after a run: the
        // budget, and collection by age only when the operator turned it on; a disk that is
        // full for other reasons is not a licence to delete what af keeps by age.
        report_to_stderr(&sweep(&self.roots, &self.policy, self.policy.auto_gc, true));
        judge_floor(&volumes(), floor)
    }

    fn before_new_warm_key(&self) {
        let report = sweep(&self.roots, &self.policy, false, true);
        report_to_stderr(&report);
    }
}

/// The refusal a check or a Worker Attempt carries below the floor: the free bytes, the floor,
/// `af storage` and the knob.
pub(crate) fn refusal(free: u64, floor: u64, volume: &Path) -> String {
    format!(
        "{}: {} ({free} bytes) free on the volume of {}, below the free-disk floor of {} \
         ({floor} bytes); see what af holds with `af storage`, reclaim it with `af storage \
         prune --apply`, or change the floor with [storage] min_free_bytes \
         (AF_STORAGE__MIN_FREE_BYTES)",
        review_pipeline::storage::INSUFFICIENT_DISK,
        human_bytes(free),
        volume.display(),
        human_bytes(floor)
    )
}

/// The refusal when the free bytes of a volume af works on cannot be measured: an unknown
/// amount is never taken for enough.
fn unmeasured_refusal(floor: u64, errors: &[&str]) -> String {
    format!(
        "{}: the free bytes af needs cannot be measured ({}), so nothing starts against the \
         free-disk floor of {} ({floor} bytes); see `af storage`, or change the floor with \
         [storage] min_free_bytes (AF_STORAGE__MIN_FREE_BYTES)",
        review_pipeline::storage::INSUFFICIENT_DISK,
        errors.join("; "),
        human_bytes(floor)
    )
}

/// One volume af works on: a path on it and its free bytes, or why they cannot be measured.
#[derive(Debug, Clone)]
pub(crate) struct Volume {
    pub(crate) path: PathBuf,
    pub(crate) free: Result<u64, String>,
}

/// The floor over every volume af works on: `Ok` only when every one was measured at or above
/// `floor`. A volume below it refuses even when another could not be measured; a volume that
/// cannot be measured refuses too, with its measurement error.
pub(crate) fn judge_floor(volumes: &[Volume], floor: u64) -> Result<(), String> {
    let lowest = volumes
        .iter()
        .filter_map(|volume| volume.free.as_ref().ok().map(|free| (*free, &volume.path)))
        .min_by_key(|(free, _)| *free);
    let unmeasured: Vec<&str> = volumes
        .iter()
        .filter_map(|volume| volume.free.as_ref().err().map(String::as_str))
        .collect();
    if let Some((free, path)) = lowest
        && free < floor
    {
        let mut message = refusal(free, floor, path);
        if !unmeasured.is_empty() {
            message.push_str(&format!(
                "; the free bytes of another volume cannot be measured either ({})",
                unmeasured.join("; ")
            ));
        }
        return Err(message);
    }
    if volumes.is_empty() {
        return Err(unmeasured_refusal(floor, &["no volume to measure"]));
    }
    if !unmeasured.is_empty() {
        return Err(unmeasured_refusal(floor, &unmeasured));
    }
    Ok(())
}

/// Every volume af works on — the one holding the temporary directory (sandboxes, check
/// runtimes) and the one holding `$XDG_CACHE_HOME` (warm caches) — each measured.
pub(crate) fn volumes() -> Vec<Volume> {
    let mut volumes = vec![measure_volume(&std::env::temp_dir())];
    volumes.push(match config::cache_home() {
        Ok(cache) => measure_volume(&cache),
        Err(error) => Volume {
            path: PathBuf::from("$XDG_CACHE_HOME"),
            free: Err(format!("the cache home cannot be named: {error}")),
        },
    });
    volumes
}

/// The free bytes of the volume holding `path`. A directory not created yet is measured where
/// it will be, at its nearest existing ancestor; any other failure is the measurement's.
pub(crate) fn measure_volume(path: &Path) -> Volume {
    let mut existing = path;
    loop {
        match std::fs::metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match existing.parent() {
                Some(parent) => existing = parent,
                None => {
                    return Volume {
                        path: path.to_path_buf(),
                        free: Err(format!("{}: {error}", path.display())),
                    };
                }
            },
            Err(error) => {
                return Volume {
                    path: existing.to_path_buf(),
                    free: Err(format!("{}: {error}", existing.display())),
                };
            }
        }
    }
    let free = match faked_free_bytes() {
        Some(free) => Ok(free),
        None => review_sandbox::storage::free_bytes(existing)
            .map_err(|error| format!("{}: {error}", existing.display())),
    };
    Volume {
        path: existing.to_path_buf(),
        free,
    }
}

/// Deterministic fixtures fake the free bytes, as a full disk would show them: a number, or
/// `@FILE` for the number a file holds now, so a fixture can fill the disk between two
/// Attempts. Production release binaries never read this setting.
fn faked_free_bytes() -> Option<u64> {
    #[cfg(debug_assertions)]
    {
        let value = std::env::var("AF_TEST_FREE_BYTES").ok()?;
        let value = match value.strip_prefix('@') {
            Some(file) => std::fs::read_to_string(file).ok()?,
            None => value,
        };
        value.trim().parse().ok()
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// Install the Storage Budget for this process's Task and review work. A machine whose
/// `[storage]` cannot be read runs without it, and says so.
pub(crate) fn install() -> Result<(), String> {
    let host = policy().and_then(|policy| {
        Ok(MachineStorage {
            policy,
            roots: Roots::current()?,
        })
    })?;
    review_pipeline::storage::install(Arc::new(host));
    Ok(())
}

/// What `af self uninstall --purge` removes beyond af's own directories (ADR-0144): the
/// project directories af-made working directories left in every registered Claude config
/// directory, and every registered Store. Gate pull requests and branches finished Tasks left
/// are cleaned up where the mapping still names their repository; the rest are listed. Runs
/// before the state and config directories go, while the registries can still be read.
/// Returns one line per thing done or left.
pub(crate) fn purge_outside_roots() -> Vec<String> {
    let mut lines = Vec::new();
    // The temporary roots the earlier `<temp>/.tmp*/tree` form lived below, without a trailing
    // separator, as given and resolved.
    let mut temps: Vec<PathBuf> = Vec::new();
    // The temporary directory af uses now, and `/tmp`, where it lives when TMPDIR is unset.
    for temp in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        temps.push(temp.components().collect());
        if let Ok(resolved) = temp.canonicalize() {
            temps.push(resolved);
        }
    }
    temps.sort();
    temps.dedup();
    let temp_slugs: Vec<String> = temps
        .iter()
        .map(|path| review_runner_claude::ClaudeSessionStore::project_slug(path))
        .collect();
    for directory in crate::providers::registered_claude_config_dirs() {
        let store = review_runner_claude::ClaudeSessionStore::new(&directory);
        let names = match store.project_names() {
            Ok(names) => names,
            Err(error) => {
                lines.push(format!(
                    "left Claude history in {}: {error}",
                    directory.display()
                ));
                continue;
            }
        };
        let ours: Vec<(String, Option<review_sandbox::Identity>)> = names
            .into_iter()
            .filter(|name| review_runner_claude::session::is_af_project_slug(name, &temp_slugs))
            .map(|name| {
                let identity =
                    review_sandbox::Identity::of(&directory.join("projects").join(&name)).ok();
                (name, identity)
            })
            .collect();
        let (removed, failures) = remove_claude_projects(&directory, &ours);
        if removed > 0 {
            lines.push(format!(
                "removed {removed} af Worker project director{} in {}",
                if removed == 1 { "y" } else { "ies" },
                directory.join("projects").display()
            ));
        }
        for failure in failures {
            lines.push(format!(
                "left Claude history in {}: {failure}",
                directory.display()
            ));
        }
    }
    let Ok(roots) = Roots::current() else {
        return lines;
    };
    let registered = registry::read(&roots.registry).unwrap_or_default();
    let mut task_stores: Vec<PathBuf> = real_directories(&roots.tasks);
    task_stores.extend(
        registered
            .iter()
            .filter(|entry| entry.kind == StoreKind::Task)
            .map(|entry| entry.path.clone()),
    );
    for state in task_stores.iter().filter(|state| has_store(state)) {
        for attempted in gate::retry_in_store(state) {
            lines.push(format!(
                "gate cleanup of Task {}: {}",
                attempted.task_id, attempted.summary
            ));
        }
        lines.extend(gate::unreached(state));
    }
    for entry in &registered {
        let stores = match entry.kind {
            StoreKind::Task | StoreKind::Review | StoreKind::Project => vec![entry.path.clone()],
            StoreKind::ReviewRoot => real_directories(&entry.path),
        };
        for store in stores.into_iter().filter(|path| has_store(path)) {
            // Outside every root af owns: opened from `/` one component at a time, and only
            // while it is still the directory just listed.
            let identity = match review_sandbox::Identity::of(&store) {
                Ok(identity) => identity,
                Err(error) => {
                    lines.push(format!(
                        "left {}: its identity could not be measured: {error}",
                        store.display()
                    ));
                    continue;
                }
            };
            match remove_directory(Path::new("/"), &store, Some(identity), 0) {
                Ok(_) => lines.push(format!("removed {}", store.display())),
                Err(failure) => lines.push(format!("left {}: {}", store.display(), failure.reason)),
            }
        }
    }
    lines
}

/// Remove the named project directories of the Claude config directory `config` through
/// descriptors opened from its `projects` directory, the anchor: a name that is a symlink, no
/// directory, or another directory than its listed identity is left and reported. Returns how
/// many were removed and why each other one was left; an absent one is neither.
pub(crate) fn remove_claude_projects(
    config: &Path,
    projects: &[(String, Option<review_sandbox::Identity>)],
) -> (usize, Vec<String>) {
    if projects.is_empty() {
        return (0, Vec::new());
    }
    let anchor = match review_sandbox::open_anchor(&config.join("projects")) {
        Ok(anchor) => anchor,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (0, Vec::new()),
        Err(error) => {
            return (
                0,
                vec![format!(
                    "the Claude projects directory cannot be opened: {error}"
                )],
            );
        }
    };
    let mut removed = 0;
    let mut failures = Vec::new();
    for (name, identity) in projects {
        // Only a directory measured when it was chosen is removed, and only while it is still
        // that directory.
        let Some(identity) = identity else {
            // Nothing measured and nothing there is nothing to remove; anything there is left.
            match nix::sys::stat::fstatat(
                &anchor,
                std::ffi::OsStr::new(name),
                nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Err(nix::errno::Errno::ENOENT) => {}
                _ => failures.push(format!(
                    "project directory {name}: its identity was not measured, so it was left"
                )),
            }
            continue;
        };
        match review_sandbox::remove_tree_at(&anchor, std::ffi::OsStr::new(name), Some(*identity)) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!("project directory {name}: {error}")),
        }
    }
    (removed, failures)
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn iso(unix_ms: u64) -> String {
    review_core::task::collection::collected_time(unix_ms)
}

/// `af storage [--json]`: what af holds, per kind, against the budget and the floor.
pub(crate) fn show(json_output: bool) -> Result<i32, String> {
    let policy = policy()?;
    let roots = Roots::current()?;
    let stock = inventory(&roots, Detail::Tasks);
    let volumes = volumes();
    let free = volumes
        .iter()
        .filter_map(|volume| volume.free.as_ref().ok().map(|free| (*free, &volume.path)))
        .min_by_key(|(free, _)| *free);
    // Every entry, and every abandoned claim inventory found, wherever it sits, as the plan of a
    // sweep recovers them: a claim inside an entry measured whole is listed `counted_in` that
    // entry, whose bytes hold its own, so `kinds` and the total count its bytes once.
    let listed: Vec<(&Entry, Option<&Path>)> = stock
        .entries
        .iter()
        .map(|entry| (entry, None))
        .chain(stock.claims.iter().map(|claim| {
            let holder = plan::holder(&stock.entries, &claim.path);
            (claim, holder.map(|holder| holder.path.as_path()))
        }))
        .collect();
    let counted = |of: &[&(&Entry, Option<&Path>)]| {
        of.iter()
            .filter(|(_, counted_in)| counted_in.is_none())
            .map(|(entry, _)| entry.bytes)
            .fold(0, u64::saturating_add)
    };
    let mut kinds = Vec::new();
    for kind in Kind::ALL {
        let of: Vec<&(&Entry, Option<&Path>)> =
            listed.iter().filter(|(e, _)| e.kind == kind).collect();
        let bytes = counted(&of);
        kinds.push(json!({
            "kind": kind.as_str(),
            "entries": of.len(),
            "bytes": bytes,
            "oldest_use_unix_ms": of.iter().map(|(e, _)| e.last_use_unix_ms).min(),
            "newest_use_unix_ms": of.iter().map(|(e, _)| e.last_use_unix_ms).max(),
        }));
    }
    let total = stock.total();
    if json_output {
        let mut entries = Vec::new();
        for (entry, counted_in) in &listed {
            let mut value = serde_json::to_value(entry).map_err(|e| e.to_string())?;
            if let Some(counted_in) = counted_in {
                value["counted_in"] = json!(counted_in);
            }
            entries.push(value);
        }
        let document = json!({
            "schema": "af/storage@3",
            "max_bytes": policy.max_bytes,
            "total_bytes": total,
            "min_free_bytes": policy.min_free_bytes,
            "free_bytes": free.as_ref().map(|(free, _)| *free),
            "kinds": kinds,
            "entries": entries,
            "problems": stock.problems,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
        );
        return Ok(0);
    }
    println!(
        "{:<20} {:>7} {:>10}  {:<20}  {:<20}",
        "KIND", "ENTRIES", "BYTES", "OLDEST USE", "NEWEST USE"
    );
    for kind in Kind::ALL {
        let of: Vec<&(&Entry, Option<&Path>)> =
            listed.iter().filter(|(e, _)| e.kind == kind).collect();
        let bytes = counted(&of);
        let when = |value: Option<u64>| value.filter(|ms| *ms > 0).map_or("-".to_string(), iso);
        println!(
            "{:<20} {:>7} {:>10}  {:<20}  {:<20}",
            kind.label(),
            of.len(),
            human_bytes(bytes),
            when(of.iter().map(|(e, _)| e.last_use_unix_ms).min()),
            when(of.iter().map(|(e, _)| e.last_use_unix_ms).max()),
        );
    }
    for (claim, counted_in) in &listed {
        if let Some(counted_in) = counted_in {
            println!(
                "abandoned removal {} ({}) counted in {}",
                claim.path.display(),
                human_bytes(claim.bytes),
                counted_in.display()
            );
        }
    }
    for entry in stock
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::UnreadableStore)
    {
        println!(
            "unreadable Store {} ({}): {}",
            entry.path.display(),
            human_bytes(entry.bytes),
            entry.reason.as_deref().unwrap_or("cannot be read")
        );
    }
    for problem in &stock.problems {
        println!("skipped {}: {}", problem.path.display(), problem.reason);
    }
    println!(
        "total {} of a {} budget ([storage] max_bytes){}",
        human_bytes(total),
        human_bytes(policy.max_bytes),
        if total > policy.max_bytes {
            "; over budget: `af storage prune --apply` evicts the least recently used"
        } else {
            ""
        }
    );
    match free {
        Some((free, volume)) => println!(
            "free {} on the volume of {}, floor {} ([storage] min_free_bytes){}",
            human_bytes(free),
            volume.display(),
            human_bytes(policy.min_free_bytes),
            if free < policy.min_free_bytes {
                "; below the floor: checks and Worker Attempts are refused"
            } else {
                ""
            }
        ),
        None => println!("free bytes unknown"),
    }
    for volume in &volumes {
        if let Err(error) = &volume.free {
            println!(
                "free bytes of {} cannot be measured ({error}): checks and Worker Attempts are \
                 refused",
                volume.path.display()
            );
        }
    }
    Ok(0)
}

/// `af storage prune [--apply] [--json]`: the sweep with collection on; a preview without
/// `--apply`.
pub(crate) fn prune(apply: bool, json_output: bool) -> Result<i32, String> {
    let policy = policy()?;
    let roots = Roots::current()?;
    let report = sweep(&roots, &policy, true, apply);
    if json_output {
        let mut document = serde_json::to_value(&report).map_err(|e| e.to_string())?;
        document["schema"] = json!("af/storage-prune@3");
        println!(
            "{}",
            serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
        );
        return Ok(0);
    }
    for removal in &report.removals {
        println!("{}", removal_line(removal, apply));
    }
    for left in &report.left {
        println!("{}", left_line(left));
    }
    for path in &report.registry_dropped {
        println!(
            "{} registry entry {} (no Store there)",
            if apply { "dropped" } else { "would drop" },
            path.display()
        );
    }
    for cleanup in &report.gate_cleanups {
        println!(
            "gate cleanup of Task {}: {}",
            cleanup.task_id, cleanup.summary
        );
    }
    for failure in unmarked_failures(&report) {
        println!("failed: {failure}");
    }
    println!(
        "{} → {} of a {} budget; {}{}",
        human_bytes(report.total_before),
        total_text(report.total_after),
        human_bytes(report.max_bytes),
        match report.stop {
            Stop::Fits => "fits",
            Stop::NothingEvictable =>
                "still over: everything left is in use or was used within the hour",
            Stop::Unknown =>
                "unknown: a removal stopped part way and what it left cannot be measured",
        },
        if apply {
            String::new()
        } else {
            ". Nothing was removed; run with --apply to remove it.".into()
        }
    );
    Ok(0)
}

/// The Surveys of the readable Project Store at `path` (ADR-0158 clause 13), from its one look:
/// one entry of kind `survey` each, its record's CAS bytes, last used at its cutoff and stated
/// by device and inode, taken out of the Project Store's own bytes; an object a
/// pending-publication entry describes whose `SurveyRecorded@1` never committed is one too, its
/// `state` `pending`, in use, last used when its entry was written. A record an interruption
/// moved under `cas/removing`, a foreign file at a collected Survey's name and an object no
/// event or entry names stay in the Project Store's bytes, never a Survey's. The Store's identity is recorded as a registered Task
/// Store's is, so a Survey's collection reaches only the directory inventory measured.
fn surveys_into(inventory: &mut Inventory, path: &Path) {
    let look = match crate::projects::survey::collect::look(path) {
        Ok(look) => look,
        Err(reason) => {
            inventory.problems.push(Problem {
                path: path.to_path_buf(),
                reason: format!("its Surveys cannot be listed, so they are kept: {reason}"),
            });
            return;
        }
    };
    let reached = plain_path(Path::new("/"), path);
    match &reached {
        Ok(identity) => {
            inventory
                .store_identities
                .insert(path.to_path_buf(), (PathBuf::from("/"), *identity));
        }
        Err(reason) => inventory.problems.push(Problem {
            path: path.to_path_buf(),
            reason: reason.clone(),
        }),
    }
    let bytes = look
        .surveys
        .iter()
        .map(|survey| survey.bytes)
        .fold(0, u64::saturating_add);
    if let Some(entry) = inventory
        .entries
        .iter_mut()
        .rev()
        .find(|entry| entry.kind == Kind::ProjectStore && entry.path == path)
    {
        entry.bytes = entry.bytes.saturating_sub(bytes);
    }
    for survey in &look.surveys {
        let in_use = match &reached {
            Ok(_) => survey.in_use.clone(),
            Err(_) => Some("its Project Store cannot be reached without a link".into()),
        };
        inventory.entries.push(Entry {
            kind: Kind::Survey,
            path: survey.object.clone(),
            task_id: None,
            bytes: survey.bytes,
            last_use_unix_ms: survey.last_use_unix_ms,
            in_use,
            reason: None,
            identity: survey.identity,
            too_deep: false,
            unmeasurable: None,
            state: survey.pending.then_some("pending"),
        });
    }
    if reached.is_ok() {
        inventory.projects.push((path.to_path_buf(), look));
    }
}

/// One removal as a line: what it freed, or would, its rule, and a Survey's note; a failed one
/// says so, with what a deletion that stopped part way freed.
fn removal_line(removal: &Removal, applied: bool) -> String {
    let what = format!(
        "{} {}{}",
        removal.kind.as_str(),
        removal.path.display(),
        removal
            .task_id
            .as_deref()
            .map(|task| format!(" Task {task}"))
            .unwrap_or_default()
    );
    match &removal.outcome {
        Some(outcome) if removal.bytes > 0 => format!(
            "planned {what} ({}) partly removed ({} freed), {outcome}",
            removal.rule.as_str(),
            human_bytes(removal.bytes)
        ),
        Some(outcome) => format!(
            "planned {what} ({}) not removed, {outcome}",
            removal.rule.as_str()
        ),
        None => format!(
            "{} {what} ({}, {}){}",
            if applied { "removed" } else { "would remove" },
            human_bytes(removal.bytes),
            removal.rule.as_str(),
            unreadable_note(removal)
        ),
    }
}

/// The failures a removal's line does not already say.
fn unmarked_failures(report: &SweepReport) -> impl Iterator<Item = &String> {
    report.failures.iter().filter(|failure| {
        !report.removals.iter().any(|removal| {
            removal
                .outcome
                .as_deref()
                .and_then(|outcome| outcome.strip_prefix(FAILED))
                == Some(failure.as_str())
        })
    })
}

/// The note a removal line gives a Survey collected `unreadable`.
fn unreadable_note(removal: &Removal) -> String {
    removal
        .unreadable
        .as_deref()
        .map(|why| format!(": collected (unreadable), {why}"))
        .unwrap_or_default()
}

/// The line that says the plan left a file or a Survey, and why: `held survey …` while a lock
/// holds it, `left survey …, <reason>` otherwise; a name that is not UTF-8 is shown lossily.
fn left_line(left: &Left) -> String {
    let what = format!(
        "{} {} ({})",
        left.kind.as_str(),
        left.raw.display(),
        human_bytes(left.bytes)
    );
    if left.reason == "held" {
        format!("held {what}: {}", left.detail)
    } else {
        format!(
            "left {what}, {}: {}",
            left.reason.replace('_', " "),
            left.detail
        )
    }
}

/// The kinds and totals of an inventory, by kind, for a caller that only needs bytes.
#[cfg(test)]
fn bytes_by_kind(stock: &Inventory) -> std::collections::BTreeMap<Kind, u64> {
    let mut totals = std::collections::BTreeMap::new();
    for entry in &stock.entries {
        *totals.entry(entry.kind).or_insert(0) += entry.bytes;
    }
    totals
}

#[cfg(test)]
mod tests;
