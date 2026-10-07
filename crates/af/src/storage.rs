//! The Storage Budget (ADR-0144): one machine-wide bound on every byte af keeps between runs,
//! a free-disk floor below which no check or Worker Attempt starts, and `af storage`.
//!
//! The budget counts entries — the unit af evicts whole: a warm toolchain key, a warm
//! Workspace, a review campaign, a finished Task, a Store this release cannot replay, an
//! installed version. One sweep serves every trigger: the end of `af task run` and `af review
//! run` (the budget; age-based collection too only when `auto_gc` is on, which it is not by
//! default), a warm check about to create a new key (budget only),
//! the free-disk floor, and `af storage prune --apply`. It never evicts an entry in use (a held
//! lock, a live writer lease, a running campaign, the default or a pinned version, the running
//! binary) or one used within the last hour, and it removes nothing without going through the
//! rule that owns it: finished Tasks through the Store's own collection, so tombstones stay;
//! directories without following a link.

pub(crate) mod gate;
mod hint;
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
    UnreadableStore,
    Version,
    /// A removal a process that died left claimed under a private name: never inventoried, only
    /// finished and reported.
    Claim,
}

impl Kind {
    const ALL: [Kind; 7] = [
        Kind::WarmKey,
        Kind::Workspace,
        Kind::Campaign,
        Kind::Task,
        Kind::TaskStore,
        Kind::UnreadableStore,
        Kind::Version,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Kind::WarmKey => "warm_key",
            Kind::Workspace => "workspace",
            Kind::Campaign => "campaign",
            Kind::Task => "task",
            Kind::TaskStore => "task_store",
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
            Kind::UnreadableStore => "unreadable Stores",
            Kind::Version => "installed versions",
            Kind::Claim => "finished removals",
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
}

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

    pub(crate) fn total(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.bytes)
            .fold(0, u64::saturating_add)
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
fn lock_held(path: &Path) -> bool {
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
    let mut campaigns: BTreeSet<PathBuf> = listed(&mut inventory, &roots.campaigns)
        .into_iter()
        .chain(listed(&mut inventory, &roots.local_reviews))
        .collect();
    let mut tasks: BTreeSet<PathBuf> = listed(&mut inventory, &roots.tasks).into_iter().collect();
    for entry in &registered {
        match entry.kind {
            StoreKind::Task => {
                tasks.insert(entry.path.clone());
            }
            StoreKind::Review => {
                campaigns.insert(entry.path.clone());
            }
            StoreKind::ReviewRoot => campaigns.extend(listed(&mut inventory, &entry.path)),
        }
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
    inventory
}

fn measure_into(
    inventory: &mut Inventory,
    kind: Kind,
    path: &Path,
    judge: impl FnOnce(&Path) -> (u64, Option<String>),
) {
    match review_sandbox::storage::allocated_bytes(path) {
        Ok(bytes) => {
            let (last_use_unix_ms, in_use) = judge(path);
            inventory.entries.push(Entry {
                kind,
                path: path.to_path_buf(),
                task_id: None,
                bytes,
                last_use_unix_ms,
                in_use,
                reason: None,
                identity: review_sandbox::Identity::of(path).ok(),
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
                }),
                Err(error) => inventory.problems.push(Problem {
                    path: key,
                    reason: format!("cannot be measured: {error}"),
                }),
            }
        }
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
    });
    record_store(inventory, roots, state.to_path_buf());
}

/// Which rule took an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Rule {
    /// Idle at least `keep_days` beyond the newest `keep_*`.
    Collection,
    /// Least recently used while af held more than `max_bytes`.
    Budget,
    /// The finish of a removal a process that died left claimed.
    Recovery,
}

/// One entry a sweep removed, or would remove.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Removal {
    pub(crate) kind: Kind,
    pub(crate) path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<String>,
    pub(crate) bytes: u64,
    pub(crate) rule: Rule,
}

/// Why the budget step stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stop {
    /// The total fits the budget.
    Fits,
    /// Over the budget, and every remaining entry is in use or was used within the hour.
    NothingEvictable,
}

/// What one sweep did (or, previewed, would do).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SweepReport {
    pub(crate) applied: bool,
    pub(crate) collection: bool,
    pub(crate) max_bytes: u64,
    pub(crate) total_before: u64,
    pub(crate) total_after: u64,
    pub(crate) stop: Stop,
    pub(crate) removals: Vec<Removal>,
    /// Gate cleanups of finished Tasks this sweep tried (collection only).
    pub(crate) gate_cleanups: Vec<gate::Attempted>,
    /// Registry entries whose path no longer holds a Store.
    pub(crate) registry_dropped: Vec<PathBuf>,
    /// Removals that failed; the next sweep tries again.
    pub(crate) failures: Vec<String>,
}

/// Run the sweep: collection first when `collection`, then the budget step. Without `apply`
/// it only says what it would remove.
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
        total_after: 0,
        stop: Stop::Fits,
        removals: Vec::new(),
        gate_cleanups: Vec::new(),
        registry_dropped: Vec::new(),
        failures: Vec::new(),
    };
    if apply {
        // Removals a process that died left claimed under a private name are finished first,
        // so their bytes are neither lost from the count nor left behind (ADR-0144).
        let (finished, left) = finish_abandoned_claims(roots);
        // Bytes that were af's until this moment: counted, and said, like any removal.
        for (path, bytes) in finished {
            report.total_before = report.total_before.saturating_add(bytes);
            report.removals.push(Removal {
                kind: Kind::Claim,
                path,
                task_id: None,
                bytes,
                rule: Rule::Recovery,
            });
        }
        report.failures.extend(left);
    }
    match drop_gone_registrations(roots, apply) {
        Ok(dropped) => report.registry_dropped = dropped,
        Err(error) => report.failures.push(error),
    }
    let mut stock = inventory(roots, Detail::Stores);
    report.total_before = report.total_before.saturating_add(stock.total());
    // What could not be measured is said with the result: a total that fits the budget is only
    // as complete as the inventory behind it.
    report.failures.extend(
        stock
            .problems
            .iter()
            .map(|problem| format!("measuring {}: {}", problem.path.display(), problem.reason)),
    );
    // Gate leftovers of finished Tasks are retried by every applied sweep, whatever the budget
    // and whether age-based collection is on: the pull request and branches are GitHub's
    // clutter, not something the budget weighs (ADR-0144).
    let mut retried: BTreeMap<PathBuf, Vec<gate::Attempted>> = BTreeMap::new();
    if apply && !policy.keep_gate_pull_requests {
        for state in &stock.task_stores {
            if let Err(why) = stock.still_the_store(state) {
                report.failures.push(format!("{}: {why}", state.display()));
                continue;
            }
            let tried = gate::retry_in_store(state);
            report.gate_cleanups.extend(tried.iter().cloned());
            retried.insert(state.clone(), tried);
        }
    }
    if collection {
        collect(roots, policy, &mut stock, now, apply, &retried, &mut report);
        if apply {
            stock = inventory(roots, Detail::Stores);
        }
    }
    // Finished Tasks are told apart only when there is something to evict.
    if stock.total() > policy.max_bytes {
        let removed: BTreeSet<(PathBuf, Option<String>)> = report
            .removals
            .iter()
            .map(|removal| (removal.path.clone(), removal.task_id.clone()))
            .collect();
        stock = inventory(roots, Detail::Tasks);
        // Telling Tasks apart can meet problems the Store-level look did not: said too.
        for problem in &stock.problems {
            let line = format!("measuring {}: {}", problem.path.display(), problem.reason);
            if !report.failures.contains(&line) {
                report.failures.push(line);
            }
        }
        if !apply {
            // A preview keeps what its collection would have taken out of the count.
            stock
                .entries
                .retain(|entry| !removed.contains(&(entry.path.clone(), entry.task_id.clone())));
        }
    }
    let mut total = stock.total();
    let mut candidates: Vec<&Entry> = stock
        .entries
        .iter()
        .filter(|entry| {
            entry.kind != Kind::TaskStore
                && entry.in_use.is_none()
                && entry.last_use_unix_ms.saturating_add(HOUR_MS) <= now
                && !entry
                    .task_id
                    .as_deref()
                    .is_some_and(|task_id| is_run_task(&entry.path, task_id))
        })
        .collect();
    candidates.sort_by(|a, b| {
        a.last_use_unix_ms
            .cmp(&b.last_use_unix_ms)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    let mut candidates = candidates.into_iter();
    while total > policy.max_bytes {
        let Some(entry) = candidates.next() else {
            report.stop = Stop::NothingEvictable;
            break;
        };
        let removed = if apply {
            evict(roots, policy, &stock, entry)
        } else {
            Ok(entry.bytes)
        };
        match removed {
            // What the removal freed, measured: a Task's collection can free more than the
            // Task's own bytes, and the loop stops as soon as af fits.
            Ok(freed) => {
                // At least what inventory measured is gone; a Task's collection may free more.
                let freed = freed.max(entry.bytes);
                total = total.saturating_sub(freed);
                report.removals.push(Removal {
                    kind: entry.kind,
                    path: entry.path.clone(),
                    task_id: entry.task_id.clone(),
                    bytes: freed,
                    rule: Rule::Budget,
                });
            }
            Err(error) => report.failures.push(error),
        }
    }
    report.total_after = total;
    report
}

/// Finish the removals dead processes left claimed in every directory af removes entries from:
/// each warm project level, the Workspaces, campaigns, local reviews, Task Stores and installed
/// versions, and the parent of every registered Store. Returns each claim it finished with its
/// bytes, and why any claim was left.
fn finish_abandoned_claims(roots: &Roots) -> (Vec<(PathBuf, u64)>, Vec<String>) {
    let mut parents: Vec<PathBuf> = real_directories(&roots.warm);
    parents.extend([
        roots.workspaces.clone(),
        roots.campaigns.clone(),
        roots.local_reviews.clone(),
        roots.tasks.clone(),
        roots.installs.versions.clone(),
    ]);
    if let Ok(registered) = registry::read(&roots.registry) {
        parents.extend(
            registered
                .iter()
                .filter_map(|entry| entry.path.parent().map(Path::to_path_buf)),
        );
    }
    // Claude Attempt and probe histories are removed through the same claim.
    parents.extend(
        crate::providers::registered_claude_config_dirs()
            .into_iter()
            .map(|directory| directory.join("projects")),
    );
    parents.sort();
    parents.dedup();
    let mut finished = Vec::new();
    let mut left = Vec::new();
    for parent in parents {
        let (removed, failures) = review_sandbox::finish_abandoned_claims(&parent);
        finished.extend(
            removed
                .into_iter()
                .map(|(name, bytes)| (parent.join(name), bytes)),
        );
        left.extend(
            failures
                .into_iter()
                .map(|failure| format!("finishing a removal in {}: {failure}", parent.display())),
        );
    }
    (finished, left)
}

fn drop_gone_registrations(roots: &Roots, apply: bool) -> Result<Vec<PathBuf>, String> {
    let holds = |entry: &registry::Registered| match entry.kind {
        StoreKind::Task | StoreKind::Review => has_store(&entry.path),
        StoreKind::ReviewRoot => entry.path.is_dir(),
    };
    if !apply {
        return Ok(registry::read(&roots.registry)?
            .into_iter()
            .filter(|entry| !holds(entry))
            .map(|entry| entry.path)
            .collect());
    }
    if !roots.registry.exists() {
        return Ok(Vec::new());
    }
    Ok(registry::retain(&roots.registry, holds)?
        .into_iter()
        .map(|entry| entry.path)
        .collect())
}

/// Collection: gate leftovers of finished Tasks, finished Tasks beyond the newest `keep_tasks`
/// of each Store idle at least `keep_days`, campaigns beyond the newest `keep_campaigns` idle
/// at least `keep_days`, and Stores this release cannot read idle at least `keep_days`.
fn collect(
    roots: &Roots,
    policy: &StoragePolicy,
    stock: &mut Inventory,
    now: u64,
    apply: bool,
    retried: &BTreeMap<PathBuf, Vec<gate::Attempted>>,
    report: &mut SweepReport,
) {
    let idle_ms = policy.keep_days.saturating_mul(DAY_MS);
    let idle = |entry: &Entry| entry.last_use_unix_ms.saturating_add(idle_ms) <= now;
    // A Store that went away since inventory, or one still being created that has no CAS yet,
    // holds nothing to collect: neither is a failure.
    let gone = |state: &Path| !has_store(state) || !state.join("cas").is_dir();
    let stores = stock.task_stores.clone();
    for state in &stores {
        if gone(state) {
            continue;
        }
        if let Err(why) = stock.still_the_store(state) {
            report
                .failures
                .push(format!("collecting {}: {why}", state.display()));
            continue;
        }
        // An unchanged Store whose last look found nothing to do until later is not read again.
        let hint = hint::Hint::read(roots, state);
        if hint
            .as_ref()
            .is_some_and(|hint| hint.quiet(state, now, policy))
        {
            continue;
        }
        // This sweep already retried the Store's gate leftovers; a Task whose retry did not end
        // done stays below.
        let tried = retried.get(state).cloned().unwrap_or_default();
        let mut kept = Vec::new();
        let collected = (|| -> Result<Vec<(String, u64)>, String> {
            let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
            // The plan reads only; the Store's writer lock is taken only when it names a Task.
            let plan = EventStore::open_read_only(state.join("events.sqlite"))
                .and_then(|store| store.plan_task_collection(&cas, idle_ms, policy.keep_tasks))
                .map_err(|e| e.to_string())?;
            let planned: Vec<(String, u64)> = plan
                .tasks
                .iter()
                .filter(|task| task.disposition.collects() && !is_run_task(state, &task.task_id))
                .map(|task| (task.task_id.clone(), task.footprint.exclusive_bytes))
                .collect();
            if apply && planned.is_empty() {
                hint::Hint::after(&plan, state, policy, gate::has_pending(state))
                    .write(roots, state);
            }
            if !apply || planned.is_empty() {
                return Ok(planned);
            }
            // Gate leftovers go first: a planned Task whose cleanup does not end done stays,
            // and the next sweep tries again. One this sweep already tried and failed is not
            // tried twice.
            let mut cleared = BTreeSet::new();
            for (task_id, _) in &planned {
                let failed_now = tried.iter().find(|attempted| {
                    attempted.task_id == *task_id
                        && attempted.outcome
                            != review_core::task::remote_check::GateCleanupOutcomeV1::Done
                });
                let verdict = match failed_now {
                    Some(attempted) => Err(gate::stays(task_id, &attempted.summary)),
                    None => gate::before_collection(state, task_id, policy.keep_gate_pull_requests),
                };
                match verdict {
                    Ok(()) => {
                        cleared.insert(task_id.clone());
                    }
                    Err(why) => kept.push(format!("collecting {}: {why}", state.display())),
                }
            }
            if cleared.is_empty() {
                return Ok(Vec::new());
            }
            // Exactly the planned Tasks whose leftovers are gone: the plan already applied the
            // age and newest rules, so a cleanup recorded just now does not make one recent.
            let mut store =
                EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
            let outcome = store
                .apply_task_collection_of(&cas, &cleared)
                .map_err(|e| e.to_string())?;
            Ok(outcome
                .plan
                .tasks
                .iter()
                .filter(|task| outcome.tombstoned.contains(&task.task_id))
                .map(|task| (task.task_id.clone(), task.footprint.exclusive_bytes))
                .collect())
        })();
        match collected {
            Ok(tasks) => {
                for (task_id, bytes) in tasks {
                    report.removals.push(Removal {
                        kind: Kind::Task,
                        path: state.clone(),
                        task_id: Some(task_id.clone()),
                        bytes,
                        rule: Rule::Collection,
                    });
                    stock.entries.retain(|entry| {
                        !(entry.kind == Kind::Task
                            && entry.path == *state
                            && entry.task_id.as_deref() == Some(task_id.as_str()))
                    });
                }
            }
            Err(_) if gone(state) => {}
            Err(error) => report
                .failures
                .push(format!("collecting {}: {error}", state.display())),
        }
        report.failures.extend(kept);
    }
    let mut campaigns: Vec<&Entry> = stock
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::Campaign)
        .collect();
    campaigns.sort_by(|a, b| b.last_use_unix_ms.cmp(&a.last_use_unix_ms));
    let mut taken: Vec<PathBuf> = Vec::new();
    for entry in campaigns.into_iter().skip(policy.keep_campaigns) {
        if entry.in_use.is_some() || !idle(entry) {
            continue;
        }
        match if apply {
            evict(roots, policy, stock, entry)
        } else {
            Ok(entry.bytes)
        } {
            Ok(_) => {
                report.removals.push(Removal {
                    kind: entry.kind,
                    path: entry.path.clone(),
                    task_id: None,
                    bytes: entry.bytes,
                    rule: Rule::Collection,
                });
                taken.push(entry.path.clone());
            }
            Err(error) => report.failures.push(error),
        }
    }
    for entry in stock
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::UnreadableStore)
    {
        if entry.in_use.is_some() || !idle(entry) {
            continue;
        }
        match if apply {
            evict(roots, policy, stock, entry)
        } else {
            Ok(entry.bytes)
        } {
            Ok(_) => {
                report.removals.push(Removal {
                    kind: entry.kind,
                    path: entry.path.clone(),
                    task_id: None,
                    bytes: entry.bytes,
                    rule: Rule::Collection,
                });
                taken.push(entry.path.clone());
            }
            Err(error) => report.failures.push(error),
        }
    }
    stock.entries.retain(|entry| {
        !(matches!(entry.kind, Kind::Campaign | Kind::UnreadableStore)
            && taken.contains(&entry.path))
    });
}

/// Remove one entry through the rule that owns it. Returns the bytes removed.
fn evict(
    roots: &Roots,
    policy: &StoragePolicy,
    stock: &Inventory,
    entry: &Entry,
) -> Result<u64, String> {
    let failed = |error: String| format!("removing {}: {error}", entry.path.display());
    match entry.kind {
        Kind::WarmKey => {
            let toolchain = entry.path.file_name().and_then(|name| name.to_str());
            let project = entry
                .path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str());
            let (Some(project), Some(toolchain)) = (project, toolchain) else {
                return Err(failed("not a toolchain key".into()));
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
                        ));
                    };
                    match key.identity() {
                        Ok(locked) if locked == measured => key.remove_key().map_err(failed),
                        Ok(_) => Err(failed(
                            "another directory holds its name since it was measured; left in \
                             place"
                                .into(),
                        )),
                        Err(error) => Err(failed(format!("inspecting the locked key: {error}"))),
                    }
                }
                None => Err(failed("a check holds it now".into())),
            }
        }
        Kind::Workspace => {
            // Not even its lock file is touched unless it is still the measured directory.
            still_measured(entry).map_err(failed)?;
            // Held exclusively until it is gone: no run clones from it in between.
            let _held = review_sandbox::hold_workspace_for_removal(&entry.path).map_err(failed)?;
            remove_entry(roots, entry).map_err(failed)
        }
        Kind::Campaign => {
            still_measured(entry).map_err(failed)?;
            // Held exclusively until it is gone: no `af review run` starts on it in between.
            let _held = hold_campaign_for_removal(&entry.path).map_err(failed)?;
            remove_entry(roots, entry).map_err(failed)
        }
        Kind::UnreadableStore => {
            if let Some(why) = unreadable_in_use(&entry.path) {
                return Err(failed(why));
            }
            remove_entry(roots, entry).map_err(failed)
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
                )));
            }
            remove_entry(roots, entry).map_err(failed)
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
                Ok(match (before, after) {
                    (Some(before), Some(after)) => before.saturating_sub(after).max(entry.bytes),
                    _ => entry.bytes,
                })
            } else {
                Err(failed(format!("Task `{task_id}` is protected now")))
            }
        }
        Kind::TaskStore => Err(failed("a Task Store is never removed whole".into())),
        Kind::Claim => Err(failed(
            "a claim is finished by recovery, never evicted".into(),
        )),
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
/// below the anchor, and only when it is still the directory inventory measured. The measured
/// bytes are what was removed.
fn remove_entry(roots: &Roots, entry: &Entry) -> Result<u64, String> {
    let identity = entry
        .identity
        .ok_or("its identity was not measured, so it is not removed")?;
    remove_directory(roots.anchor_of(&entry.path), &entry.path, Some(identity))
}

/// Remove `path`, strictly below `anchor`, and everything below it through descriptors opened
/// from `anchor` without following a link, refusing any directory but `identity`. The measured
/// bytes are what was removed.
fn remove_directory(
    anchor: &Path,
    path: &Path,
    identity: Option<review_sandbox::Identity>,
) -> Result<u64, String> {
    let bytes = review_sandbox::storage::allocated_bytes(path).unwrap_or(0);
    review_sandbox::remove_beneath(anchor, path, identity)
        .map(|()| bytes)
        .map_err(|error| format!("{error}; it is left, and the next sweep tries again"))
}

/// Print a sweep's removals and problems to stderr, one line each.
pub(crate) fn report_to_stderr(report: &SweepReport) {
    for removal in &report.removals {
        eprintln!(
            "af storage: {} {} {}{} ({}, {})",
            if report.applied {
                "removed"
            } else {
                "would remove"
            },
            removal.kind.as_str(),
            removal.path.display(),
            removal
                .task_id
                .as_deref()
                .map(|task| format!(" Task {task}"))
                .unwrap_or_default(),
            human_bytes(removal.bytes),
            match removal.rule {
                Rule::Collection => "collection",
                Rule::Budget => "budget",
                Rule::Recovery => "recovery",
            }
        );
    }
    for cleanup in &report.gate_cleanups {
        eprintln!(
            "af storage: gate cleanup of Task {}: {}",
            cleanup.task_id, cleanup.summary
        );
    }
    for failure in &report.failures {
        eprintln!("af storage: warning: {failure}");
    }
    if report.stop == Stop::NothingEvictable {
        eprintln!(
            "af storage: warning: af holds {} against a budget of {}, and nothing else may be \
             removed now (in use, or used within the hour); see `af storage`",
            human_bytes(report.total_after),
            human_bytes(report.max_bytes)
        );
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
    if !report.applied || (report.removals.is_empty() && report.failures.is_empty()) {
        return None;
    }
    let listable: Vec<StorageSweepRemovalV1> = report
        .removals
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
                Kind::TaskStore => return None,
            };
            let path = removal.path.to_str()?.to_string();
            (path.len() <= MAX_SWEEP_PATH_BYTES && !path.chars().any(char::is_control)).then(|| {
                StorageSweepRemovalV1 {
                    kind,
                    path,
                    task_id: removal.task_id.clone(),
                    bytes: removal.bytes,
                    rule: match removal.rule {
                        Rule::Collection => StorageSweepRuleV1::Collection,
                        Rule::Budget => StorageSweepRuleV1::Budget,
                        Rule::Recovery => StorageSweepRuleV1::Recovery,
                    },
                }
            })
        })
        .collect();
    let removals: Vec<_> = listable.into_iter().take(MAX_SWEEP_REMOVALS).collect();
    // Beyond the bound, or with a path no record can name: counted, not listed.
    let omitted_removals = report.removals.len().saturating_sub(removals.len()) as u64;
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
        },
        max_bytes: report.max_bytes,
        total_before: report.total_before,
        total_after: report.total_after,
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
            StoreKind::Task | StoreKind::Review => vec![entry.path.clone()],
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
            match remove_directory(Path::new("/"), &store, Some(identity)) {
                Ok(_) => lines.push(format!("removed {}", store.display())),
                Err(error) => lines.push(format!("left {}: {error}", store.display())),
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
    let mut kinds = Vec::new();
    for kind in Kind::ALL {
        let of: Vec<&Entry> = stock.entries.iter().filter(|e| e.kind == kind).collect();
        let bytes = of.iter().map(|e| e.bytes).fold(0, u64::saturating_add);
        kinds.push(json!({
            "kind": kind.as_str(),
            "entries": of.len(),
            "bytes": bytes,
            "oldest_use_unix_ms": of.iter().map(|e| e.last_use_unix_ms).min(),
            "newest_use_unix_ms": of.iter().map(|e| e.last_use_unix_ms).max(),
        }));
    }
    let total = stock.total();
    if json_output {
        let document = json!({
            "schema": "af/storage@1",
            "max_bytes": policy.max_bytes,
            "total_bytes": total,
            "min_free_bytes": policy.min_free_bytes,
            "free_bytes": free.as_ref().map(|(free, _)| *free),
            "kinds": kinds,
            "entries": stock.entries,
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
        let of: Vec<&Entry> = stock.entries.iter().filter(|e| e.kind == kind).collect();
        let bytes = of.iter().map(|e| e.bytes).fold(0, u64::saturating_add);
        let when = |value: Option<u64>| value.filter(|ms| *ms > 0).map_or("-".to_string(), iso);
        println!(
            "{:<20} {:>7} {:>10}  {:<20}  {:<20}",
            kind.label(),
            of.len(),
            human_bytes(bytes),
            when(of.iter().map(|e| e.last_use_unix_ms).min()),
            when(of.iter().map(|e| e.last_use_unix_ms).max()),
        );
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
        document["schema"] = json!("af/storage-prune@1");
        println!(
            "{}",
            serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
        );
        return Ok(0);
    }
    let verb = if apply { "removed" } else { "would remove" };
    for removal in &report.removals {
        println!(
            "{verb} {} {}{} {} ({})",
            removal.kind.as_str(),
            removal.path.display(),
            removal
                .task_id
                .as_deref()
                .map(|task| format!(" Task {task}"))
                .unwrap_or_default(),
            human_bytes(removal.bytes),
            match removal.rule {
                Rule::Collection => "collection",
                Rule::Budget => "budget",
                Rule::Recovery => "recovery",
            }
        );
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
    for failure in &report.failures {
        println!("failed: {failure}");
    }
    println!(
        "{} → {} of a {} budget; {}{}",
        human_bytes(report.total_before),
        human_bytes(report.total_after),
        human_bytes(report.max_bytes),
        match report.stop {
            Stop::Fits => "fits",
            Stop::NothingEvictable =>
                "still over: everything left is in use or was used within the hour",
        },
        if apply {
            String::new()
        } else {
            ". Nothing was removed; run with --apply to remove it.".into()
        }
    );
    Ok(0)
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
