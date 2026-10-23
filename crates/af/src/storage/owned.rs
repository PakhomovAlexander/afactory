//! Who owns a Store directory (ADR-0144, amended 2026-10-20).
//!
//! af creates a Store only in a directory that is absent, empty, or already an af Store (it
//! holds nothing but af's own entries), and a Store it creates in an absent or empty directory
//! carries the owner marker [`MARKER`]. A Store outside af's own roots — an explicit `--state`
//! or a campaign below an explicit `--state-root` — is removed whole only while it is marked and
//! holds nothing but af's entries; otherwise only af's entries go, and everything else is left
//! in place and named. Either way af's entries go one by one through the directory it opened,
//! and the directory itself only by `rmdir`, so an entry that appears meanwhile keeps it. Such a
//! Store is also measured by its af entries alone.
//!
//! Every directory af creates for its state is private (0700) and every file there 0600,
//! whatever the umask; an existing Store and af's state root are narrowed to 0700 when the
//! effective user owns them.

use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use review_store::local_fs;

/// The owner marker: af created this Store directory and may remove it whole.
pub(crate) const MARKER: &str = ".af-store";

const MARKER_TEXT: &str = "af created this Store directory: it is af-owned, and af removes it \
                           whole once it holds nothing but af's entries (ADR-0144).\n";

/// The event log and SQLite's files beside it (write-ahead log, shared memory, and the rollback
/// journal SQLite keeps while it switches a new database to WAL).
const EVENT_LOG: [&str; 4] = [
    "events.sqlite",
    "events.sqlite-wal",
    "events.sqlite-shm",
    "events.sqlite-journal",
];

/// Every name af itself creates directly in a Store directory, exactly: the event log and its
/// SQLite files, the CAS, the Git home (`task.rs`), the run records and detached logs of `af
/// task run` (`runs`, ADR-0198), the owner marker, the run lock ([`super::RUNNING_LOCK`]), and
/// the Task delivery lock database with its SQLite files (`task.rs`). Nothing else af writes
/// there.
const AF_ENTRIES: [&str; 13] = [
    EVENT_LOG[0],
    EVENT_LOG[1],
    EVENT_LOG[2],
    EVENT_LOG[3],
    "cas",
    "git-home",
    "runs",
    MARKER,
    super::RUNNING_LOCK,
    "task-delivery-lock.sqlite",
    "task-delivery-lock.sqlite-wal",
    "task-delivery-lock.sqlite-shm",
    "task-delivery-lock.sqlite-journal",
];

/// Whether `name` is an entry af itself writes directly in the opened Store directory `store`:
/// exactly one of [`AF_ENTRIES`], or a claim a removal of one of them left, as
/// `review_sandbox::claim_owner_at` proves it against what is on disk (its name binds the very
/// directory it holds). No prefix, suffix or extension makes a name af's: `notes.lock`,
/// `backup.lock`, `events.sqlite.notes` and a claim-shaped `.af-removing-1-2-3` that binds no
/// directory there are the user's.
pub(crate) fn is_af_entry(store: impl AsFd, name: &OsStr) -> bool {
    is_af_name(name) || review_sandbox::claim_owner_at(store, name).is_some()
}

/// Whether `name` is exactly one of [`AF_ENTRIES`].
fn is_af_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| AF_ENTRIES.contains(&name))
}

/// Whether `name` is one of the event log's files, which a partial removal takes last.
fn is_event_log(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| EVENT_LOG.contains(&name))
}

/// What a directory holds, as the ownership rule sees it.
struct Holdings {
    directory: OwnedFd,
    names: Vec<OsString>,
}

impl Holdings {
    fn foreign(&self) -> impl Iterator<Item = &OsString> {
        self.names
            .iter()
            .filter(|name| !is_af_entry(&self.directory, name))
    }
}

/// `None` when nothing is at `path`.
fn holdings(path: &Path) -> Result<Option<Holdings>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(format!("{} is not a directory", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspecting {}: {error}", path.display())),
    }
    let mut names: Vec<OsString> = std::fs::read_dir(path)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect()
        })
        .map_err(|error| format!("listing {}: {error}", path.display()))?;
    names.sort();
    let directory = review_sandbox::open_anchor(path)
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    Ok(Some(Holdings { directory, names }))
}

/// Refuse an explicit state directory af must not adopt: one that holds anything but af's own
/// entries. An absent or empty directory, or an af Store, is admitted. Writes nothing.
pub(crate) fn admit(state: &Path) -> Result<(), String> {
    let Some(holdings) = holdings(state)? else {
        return Ok(());
    };
    let foreign: Vec<&OsString> = holdings.foreign().collect();
    let Some(first) = foreign.first() else {
        return Ok(());
    };
    Err(format!(
        "state directory {} holds what is not af's ({} entr{}, such as {:?}); an explicit state \
         directory must be absent, empty or an af Store, because it becomes af-owned and af may \
         remove it; choose a new or empty directory",
        state.display(),
        foreign.len(),
        if foreign.len() == 1 { "y" } else { "ies" },
        first.to_string_lossy()
    ))
}

/// Create the Store directory `state` for writing: admitted first when it was named explicitly,
/// created private, marked when it was absent or empty, and narrowed to 0700. Below af's own
/// state root, that root is narrowed too.
pub(crate) fn prepare(state: &Path, explicit: bool) -> Result<(), String> {
    if explicit {
        admit(state)?;
    }
    let fresh = holdings(state)?.is_none_or(|holdings| holdings.names.is_empty());
    let af_root = crate::config::state_home().ok().map(|home| home.join("af"));
    match &af_root {
        Some(root) if state.starts_with(root) => {
            if let Some(home) = root.parent() {
                std::fs::create_dir_all(home)
                    .map_err(|error| format!("creating {}: {error}", home.display()))?;
            }
            local_fs::create_private_dir_all(state)
                .map_err(|error| format!("creating {}: {error}", state.display()))?;
            let _ = local_fs::narrow_owned_dir(root);
        }
        _ => {
            if let Some(parent) = state.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("creating {}: {error}", parent.display()))?;
            }
            local_fs::create_private_dir_all(state)
                .map_err(|error| format!("creating {}: {error}", state.display()))?;
        }
    }
    if fresh {
        mark(state)?;
    }
    local_fs::narrow_owned_dir(state)
        .map(|_| ())
        .map_err(|error| format!("narrowing {} to 0700: {error}", state.display()))
}

fn mark(state: &Path) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = state.join(MARKER);
    match local_fs::private_file_options()
        .write(true)
        .create_new(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(mut file) => file
            .write_all(MARKER_TEXT.as_bytes())
            .map_err(|error| format!("writing {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(format!("writing {}: {error}", path.display())),
    }
}

/// `path`, with any missing ancestor, created private (0700) when absent: a Store's
/// `git-home/`, and the like.
pub(crate) fn private_dir(path: &Path) -> Result<(), String> {
    local_fs::create_private_dir_all(path)
        .map_err(|error| format!("creating {}: {error}", path.display()))
}

/// The allocated bytes of af's entries in the Store `state` ([`af_bytes_and_depth`]).
pub(crate) fn af_bytes(state: &Path) -> std::io::Result<u64> {
    af_bytes_and_depth(state).map(|(bytes, _)| bytes)
}

/// The allocated bytes of af's entries in the Store `state`, and the level of the deepest
/// directory among them (the Store itself at level 0): what a Store outside af's roots counts
/// against the budget. Anything else in the directory is not af's to count.
pub(crate) fn af_bytes_and_depth(state: &Path) -> std::io::Result<(u64, u32)> {
    let directory = review_sandbox::open_anchor(state)?;
    let (mut bytes, mut depth) = (0_u64, 0_u32);
    for entry in std::fs::read_dir(state)? {
        let entry = entry?;
        if is_af_entry(&directory, &entry.file_name()) {
            let (held, below) = review_sandbox::storage::allocated_bytes_and_depth(&entry.path())?;
            bytes = bytes.saturating_add(held);
            if entry.file_type()?.is_dir() {
                depth = depth.max(below.saturating_add(1));
            }
        }
    }
    Ok((bytes, depth))
}

/// What removing a Store outside af's roots did.
#[derive(Debug, Default)]
pub(crate) struct Removed {
    pub(crate) bytes: u64,
    /// What left the disk, as measured entry by entry just before each removal: the sweep's
    /// running total takes it in place of inventory's count, so a Store that shrank or grew since
    /// is accounted as it was removed.
    pub(crate) started: Option<u64>,
    /// What is still there: entries that are not af's, and the directory itself when it was not
    /// removed whole.
    pub(crate) left: Vec<PathBuf>,
}

/// Remove the Store `path`, strictly below `anchor`, reached from `anchor` without following a
/// link and only while it is still `identity`: af's entries, one by one through the opened
/// directory, and the rest is left. The directory itself goes too when it is marked, held
/// nothing but af's entries, and is empty once they went: it is removed by `rmdir`, which a
/// file created in it meanwhile refuses, so such a file is never removed with it.
pub(crate) fn remove(
    anchor: &Path,
    path: &Path,
    identity: review_sandbox::Identity,
) -> Result<Removed, String> {
    remove_with(anchor, path, identity).map_err(|failure| failure.reason)
}

/// [`remove`], failing with the sweep's [`super::plan::Failed`].
pub(crate) fn remove_with(
    anchor: &Path,
    path: &Path,
    identity: review_sandbox::Identity,
) -> Result<Removed, super::plan::Failed> {
    let relative = path
        .strip_prefix(anchor)
        .map_err(|_| format!("{} is not below {}", path.display(), anchor.display()))?;
    let opened = review_sandbox::open_anchor(anchor)
        .and_then(|anchor| review_sandbox::open_beneath(&anchor, relative))
        .map_err(|error| {
            format!("its path cannot be followed without a link, so it is left: {error}")
        })?;
    let found = review_sandbox::Identity::of_descriptor(&opened)
        .map_err(|error| format!("inspecting it: {error}"))?;
    if found != identity {
        return Err(
            "it is not the directory that was measured; it changed since and is left"
                .to_string()
                .into(),
        );
    }
    let names = local_fs::entry_names(&opened).map_err(|error| format!("listing it: {error}"))?;
    let marked = names.iter().any(|name| name == MARKER);
    let whole = marked && names.iter().all(|name| is_af_entry(&opened, name));
    let mut removed = Removed::default();
    let mut failures = Vec::new();
    // The event log goes after everything else of af's, and the owner marker last, each only
    // once everything before it went: while the log is there the Store stays registered, and
    // while the marker is there the next sweep still knows the Store as af's, so it finishes
    // what this one could not.
    let last = |name: &OsString| match name {
        name if name == MARKER => 2,
        name if is_event_log(name) => 1,
        _ => 0,
    };
    let mut ordered: Vec<&OsString> = names.iter().collect();
    ordered.sort_by_key(|name| last(name));
    // af's bytes before anything goes: a removal that stops part way inside an entry still
    // accounts what left the disk, as this less what af holds there afterwards.
    let before = af_bytes(path).ok();
    for name in ordered {
        if !is_af_entry(&opened, name) {
            removed.left.push(path.join(name));
            continue;
        }
        if !failures.is_empty() && last(name) > 0 {
            continue;
        }
        let bytes = review_sandbox::storage::allocated_bytes(&path.join(name)).unwrap_or(0);
        match remove_entry_at(&opened, name) {
            Ok(()) => removed.bytes = removed.bytes.saturating_add(bytes),
            Err(error) => failures.push(format!("{}: {error}", name.to_string_lossy())),
        }
    }
    if failures.is_empty() && whole {
        // Proven af's and emptied of af's entries: the directory goes by `rmdir`, so an entry
        // created in it since the listing keeps it, and is reported.
        match remove_emptied(anchor, path, identity) {
            Ok(()) => {
                return Ok(Removed {
                    started: Some(removed.bytes),
                    ..removed
                });
            }
            Err(_) => {
                let names = local_fs::entry_names(&opened).unwrap_or_default();
                removed
                    .left
                    .extend(names.iter().map(|name| path.join(name)));
            }
        }
    }
    // Not proven af's, holding what is not, or given something since: the directory stays.
    removed.left.insert(0, path.to_path_buf());
    if failures.is_empty() {
        // What left the disk is exactly what was measured entry by entry, not inventory's count.
        return Ok(Removed {
            started: Some(removed.bytes),
            ..removed
        });
    }
    // Stopped part way: what af still holds there replaces inventory's count (unknown when it
    // cannot be measured), and what went is counted freed.
    let after = af_bytes(path).ok();
    Err(super::plan::Failed {
        reason: format!(
            "af's entries were only partly removed ({}); the rest is left, and the next sweep \
             tries again",
            failures.join("; ")
        ),
        freed: match (before, after) {
            (Some(before), Some(after)) => before.saturating_sub(after),
            _ => removed.bytes,
        },
        remains: after.map_or(
            super::plan::Remains::Unmeasured,
            super::plan::Remains::Measured,
        ),
    })
}

/// Remove the emptied Store directory `path`, strictly below `anchor`, by `rmdir` from its
/// parent reached without following a link, and only while it is still `identity`.
fn remove_emptied(
    anchor: &Path,
    path: &Path,
    identity: review_sandbox::Identity,
) -> std::io::Result<()> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::other("a Store has a parent"));
    };
    let relative = parent.strip_prefix(anchor).map_err(std::io::Error::other)?;
    let parent = review_sandbox::open_beneath(review_sandbox::open_anchor(anchor)?, relative)?;
    let stat = nix::sys::stat::fstatat(&parent, name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)?;
    let found = review_sandbox::Identity::of_stat(&stat);
    if (found.device, found.inode) != (identity.device, identity.inode) {
        return Err(std::io::Error::other("it changed since it was measured"));
    }
    nix::unistd::unlinkat(&parent, name, nix::unistd::UnlinkatFlags::RemoveDir)
        .map_err(std::io::Error::from)
}

/// Remove the entry `name` of the opened directory: a directory through
/// [`review_sandbox::remove_tree_at`], anything else (a link included) unlinked itself.
fn remove_entry_at(directory: &std::os::fd::OwnedFd, name: &OsStr) -> std::io::Result<()> {
    let stat = nix::sys::stat::fstatat(directory, name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)?;
    if nix::sys::stat::SFlag::from_bits_truncate(stat.st_mode) & nix::sys::stat::SFlag::S_IFMT
        == nix::sys::stat::SFlag::S_IFDIR
    {
        review_sandbox::remove_tree_at(
            directory,
            name,
            Some(review_sandbox::Identity::of_stat(&stat)),
        )
    } else {
        nix::unistd::unlinkat(directory, name, nix::unistd::UnlinkatFlags::NoRemoveDir)
            .map_err(std::io::Error::from)
    }
}

/// One line for what a removal left in place, or `None` when it left nothing.
pub(crate) fn left_line(path: &Path, left: &[PathBuf]) -> Option<String> {
    if left.is_empty() {
        return None;
    }
    let others: Vec<String> = left
        .iter()
        .filter(|entry| entry.as_path() != path)
        .map(|entry| entry.display().to_string())
        .collect();
    Some(if others.is_empty() {
        format!(
            "removed af's entries of {} and left the directory in place: it carries no owner \
             marker, so it is not proven af's",
            path.display()
        )
    } else {
        format!(
            "removed af's entries of {} and left the directory with what is not af's: {}",
            path.display(),
            others.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn af_entries_are_the_ones_af_writes_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let store = review_sandbox::open_anchor(root.path()).unwrap();
        // A claim af made: its name binds the directory it holds.
        std::fs::create_dir(root.path().join("cas")).unwrap();
        let identity = review_sandbox::Identity::of(&root.path().join("cas")).unwrap();
        let claim = review_sandbox::claim_name(12, identity);
        std::fs::rename(root.path().join("cas"), root.path().join(&claim)).unwrap();
        assert!(is_af_entry(&store, OsStr::new(&claim)), "{claim}");
        // Claim-shaped names that bind no directory there are the user's (audit C2).
        std::fs::write(root.path().join(".af-removing-1-2-3"), b"mine").unwrap();
        std::fs::create_dir(root.path().join(".af-removing-12-345-0")).unwrap();
        for name in [
            "events.sqlite",
            "events.sqlite-wal",
            "events.sqlite-shm",
            "cas",
            "git-home",
            "runs",
            "events.sqlite-journal",
            "af-running.lock",
            "task-delivery-lock.sqlite",
            "task-delivery-lock.sqlite-wal",
            "task-delivery-lock.sqlite-shm",
            ".af-store",
        ] {
            assert!(is_af_entry(&store, OsStr::new(name)), "{name}");
        }
        for name in [
            "important.txt",
            "photos",
            ".lock",
            "cas2",
            ".DS_Store",
            "events",
            // No prefix, suffix or extension match: these are the user's (audit V1).
            "notes.lock",
            "backup.lock",
            "events.sqlite.notes",
            "events.sqlite-wal.bak",
            "task-delivery-lock.sqlite.old",
            "cas.lock",
            ".af-store~",
            ".af-removing-12-ab",
            ".af-removing-1-2-3-4",
            ".af-removing-",
            ".af-removing-1-2-3",
            ".af-removing-12-345-0",
        ] {
            assert!(!is_af_entry(&store, OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn only_an_absent_empty_or_af_directory_is_admitted() {
        let root = tempfile::tempdir().unwrap();
        assert!(admit(&root.path().join("absent")).is_ok());
        let empty = root.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(admit(&empty).is_ok());
        let store = root.path().join("store");
        std::fs::create_dir_all(store.join("cas")).unwrap();
        std::fs::write(store.join("events.sqlite"), b"").unwrap();
        assert!(admit(&store).is_ok());
        std::fs::write(store.join("important.txt"), b"mine").unwrap();
        let error = admit(&store).unwrap_err();
        assert!(error.contains("holds what is not af's (1 entry"), "{error}");
        assert!(error.contains("important.txt"), "{error}");
        assert_eq!(error.lines().count(), 1);
        let file = root.path().join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(admit(&file).unwrap_err().contains("not a directory"));
    }

    #[test]
    fn a_new_store_is_private_and_marked_and_an_old_one_is_not_marked() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let mode = |path: &Path| std::fs::symlink_metadata(path).unwrap().mode() & 0o777;
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join("outside/state");
        prepare(&fresh, true).unwrap();
        assert_eq!(mode(&fresh), 0o700);
        assert_eq!(mode(&fresh.join(MARKER)), 0o600);
        let old = root.path().join("old");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("events.sqlite"), b"").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        prepare(&old, true).unwrap();
        assert!(!old.join(MARKER).exists());
        assert_eq!(mode(&old), 0o700);
        let foreign = root.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("notes.md"), b"").unwrap();
        assert!(prepare(&foreign, true).is_err());
        assert_eq!(
            std::fs::read_dir(&foreign).unwrap().count(),
            1,
            "nothing is written into a refused directory"
        );
    }

    fn store(path: &Path, marked: bool) {
        std::fs::create_dir_all(path.join("cas/objects/ab")).unwrap();
        std::fs::write(path.join("cas/objects/ab/cd"), vec![1; 8192]).unwrap();
        std::fs::create_dir_all(path.join("git-home")).unwrap();
        std::fs::write(path.join("events.sqlite"), vec![2; 8192]).unwrap();
        std::fs::write(path.join("af-running.lock"), b"").unwrap();
        if marked {
            std::fs::write(path.join(MARKER), MARKER_TEXT).unwrap();
        }
    }

    /// A removal that stops part way keeps the event log and the owner marker (the next sweep
    /// still knows the Store as af's), counts what went as freed and measures what af still holds
    /// there; once nothing stands in the way, the next removal takes the rest and the directory,
    /// accounting exactly what it measured.
    #[test]
    fn a_partial_removal_keeps_the_marker_and_the_log_and_accounts_what_went() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let identity = |path: &Path| review_sandbox::Identity::of(path).unwrap();
        let stuck = root.join("stuck");
        store(&stuck, true);
        std::fs::write(stuck.join("git-home/config"), vec![3; 8192]).unwrap();
        // A tree grown deeper than the removal walk stops the removal of `cas` part way.
        let deep = (0..=review_sandbox::REMOVAL_MAX_DEPTH)
            .fold(stuck.join("cas/deep"), |path, _| path.join("d"));
        std::fs::create_dir_all(&deep).unwrap();
        let failed = remove_with(&root, &stuck, identity(&stuck)).unwrap_err();
        // What stopped it, under `cas` or the claim the removal renamed `cas` to, goes by hand.
        for entry in std::fs::read_dir(&stuck).unwrap() {
            let entry = entry.unwrap().path();
            if entry.is_dir() {
                std::fs::remove_dir_all(entry).unwrap();
            }
        }
        assert!(stuck.join(MARKER).exists(), "{failed:?}");
        assert!(stuck.join("events.sqlite").exists());
        assert!(failed.freed >= 8192, "git-home went: {failed:?}");
        assert!(
            matches!(failed.remains, crate::storage::plan::Remains::Measured(bytes) if bytes > 0),
            "{failed:?}"
        );

        let removed = remove_with(&root, &stuck, identity(&stuck)).unwrap();
        assert!(!stuck.exists());
        assert!(removed.left.is_empty());
        assert_eq!(removed.started, Some(removed.bytes));
    }

    /// The last step of removing a proven Store is `rmdir`: a file created in it after its
    /// entries were listed keeps the directory, and the file, in place; a directory replaced
    /// since it was measured is not removed either.
    #[test]
    fn an_emptied_store_goes_only_by_rmdir_and_only_while_it_is_the_measured_one() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let emptied = root.join("emptied");
        std::fs::create_dir(&emptied).unwrap();
        let identity = review_sandbox::Identity::of(&emptied).unwrap();
        std::fs::write(emptied.join("notes.txt"), b"mine").unwrap();
        assert!(remove_emptied(&root, &emptied, identity).is_err());
        assert_eq!(std::fs::read(emptied.join("notes.txt")).unwrap(), b"mine");
        std::fs::remove_file(emptied.join("notes.txt")).unwrap();

        let replaced = root.join("replaced");
        std::fs::create_dir(&replaced).unwrap();
        let measured = review_sandbox::Identity::of(&replaced).unwrap();
        // The measured directory stays alive beside it, so its replacement cannot reuse its
        // inode (Linux file systems hand a freed inode straight back).
        std::fs::rename(&replaced, root.join("measured")).unwrap();
        std::fs::create_dir(&replaced).unwrap();
        assert!(remove_emptied(&root, &replaced, measured).is_err());
        assert!(replaced.exists());

        remove_emptied(&root, &emptied, identity).unwrap();
        assert!(!emptied.exists());
    }

    #[test]
    fn a_marked_af_only_store_goes_whole_and_anything_else_keeps_what_is_not_afs() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let identity = |path: &Path| review_sandbox::Identity::of(path).unwrap();

        let whole = root.join("whole");
        store(&whole, true);
        let removed = remove(&root, &whole, identity(&whole)).unwrap();
        assert!(!whole.exists());
        assert!(removed.left.is_empty());
        assert!(removed.bytes >= 16384);

        let mixed = root.join("mixed");
        store(&mixed, true);
        std::fs::write(mixed.join("important.txt"), b"mine").unwrap();
        std::fs::create_dir_all(mixed.join("photos")).unwrap();
        std::fs::write(mixed.join("photos/1.jpg"), b"jpg").unwrap();
        let measured = af_bytes(&mixed).unwrap();
        let removed = remove(&root, &mixed, identity(&mixed)).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&mixed)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["important.txt", "photos"]);
        assert_eq!(std::fs::read(mixed.join("photos/1.jpg")).unwrap(), b"jpg");
        assert_eq!(
            removed.left,
            [
                mixed.clone(),
                mixed.join("important.txt"),
                mixed.join("photos")
            ]
        );
        assert!(removed.bytes >= measured.min(16384));
        let line = left_line(&mixed, &removed.left).unwrap();
        assert!(
            line.contains("important.txt") && line.contains("photos"),
            "{line}"
        );

        // A Store an earlier release made carries no marker: its directory is not proven af's.
        let unmarked = root.join("unmarked");
        store(&unmarked, false);
        let removed = remove(&root, &unmarked, identity(&unmarked)).unwrap();
        assert!(unmarked.is_dir());
        assert_eq!(std::fs::read_dir(&unmarked).unwrap().count(), 0);
        assert_eq!(removed.left, [unmarked.clone()]);
        assert!(
            left_line(&unmarked, &removed.left)
                .unwrap()
                .contains("no owner marker")
        );
    }

    #[test]
    fn a_store_is_measured_by_its_af_entries_alone() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        store(&state, true);
        let af = af_bytes(&state).unwrap();
        std::fs::write(state.join("big.iso"), vec![3; 1 << 20]).unwrap();
        assert_eq!(af_bytes(&state).unwrap(), af);
        assert!(review_sandbox::storage::allocated_bytes(&state).unwrap() > af + (1 << 19));
    }
}
