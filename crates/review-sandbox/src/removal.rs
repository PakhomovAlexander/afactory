//! Removal through directory descriptors (ADR-0144): every directory the Storage Budget, `af
//! storage prune` or `af self uninstall --purge` removes is opened from a trusted anchor, one
//! component at a time, and removed through the descriptor that walk ends at — never through a
//! path a later lookup could resolve somewhere else.
//!
//! The anchor is a root as the operator configured it (`$XDG_CACHE_HOME/af/…`, a Claude config
//! directory's `projects`, or `/` for a Store registered elsewhere) and is opened as given.
//! Every component below it is opened `O_NOFOLLOW | O_DIRECTORY`, so a component that became a
//! symlink, or is no directory at all, makes the removal fail instead of being followed. The
//! directory reached must be the one an inventory measured — the same device and inode — or the
//! removal fails too: a name that now holds another directory is somebody else's.

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path};

/// The deepest directory level below the directory it removes that [`remove_tree_at`]'s walk
/// reaches, the directory itself at level 0: a tree nested deeper is only ever partly removed.
pub const REMOVAL_MAX_DEPTH: u32 = crate::stale::MAX_DEPTH;

/// Whether a tree whose deepest directory is at level `depth` (as
/// [`crate::storage::allocated_bytes_and_depth`] measures it) is deeper than the removal walk:
/// [`remove_tree_at`] would leave part of it, so a planner leaves all of it.
pub fn deeper_than_removal_walk(depth: u32) -> bool {
    depth > REMOVAL_MAX_DEPTH
}

/// A removal that unlinked some of a tree and left the rest under its claim: the error
/// [`remove_tree_at`] returns then, which names the claim, and, from a measured removal
/// ([`remove_tree_measured_at`]), the bytes that left the disk.
#[derive(Debug)]
pub struct PartlyRemoved {
    /// The private name in the parent that holds what is left, `.af-removing-<pid>-…`.
    pub claim: String,
    /// The allocated bytes that left the disk: what the claim held immediately before the walk
    /// began unlinking, less what it still holds after it stopped, both stated through the
    /// claim's own descriptor and within the walk's reach. `None` when the removal measured
    /// nothing or either could not be measured.
    pub freed: Option<u64>,
    message: String,
}

impl std::fmt::Display for PartlyRemoved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PartlyRemoved {}

/// The claim a removal that failed part way left, when `error` is that failure: the name in
/// the removed directory's parent that holds what is left of it.
pub fn partly_removed(error: &io::Error) -> Option<&str> {
    partial(error).map(|partly| partly.claim.as_str())
}

/// The failure part way that `error` is, when it is one.
pub fn partial(error: &io::Error) -> Option<&PartlyRemoved> {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<PartlyRemoved>())
}

/// What names one directory on this machine independent of its path: its device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
}

impl Identity {
    /// The identity of what `path` names, without following a final link.
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    /// The identity a `stat` reports.
    #[allow(clippy::unnecessary_cast)]
    pub fn of_stat(stat: &nix::sys::stat::FileStat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        }
    }

    /// The identity of the directory or file `descriptor` holds open.
    #[allow(clippy::unnecessary_cast)]
    pub fn of_descriptor(descriptor: impl AsFd) -> io::Result<Self> {
        let stat = nix::sys::stat::fstat(descriptor).map_err(io::Error::from)?;
        // `std::os::unix::fs::MetadataExt` widens the same fields the same way.
        Ok(Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        })
    }
}

fn directory_flags() -> nix::fcntl::OFlag {
    use nix::fcntl::OFlag;
    OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY
}

/// The failure of a component that is a symlink or no directory: never followed, always said.
fn not_followed(name: &OsStr, errno: nix::errno::Errno) -> io::Error {
    match errno {
        nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR => io::Error::other(format!(
            "`{}` is a symlink or not a directory; af never removes through one",
            name.to_string_lossy()
        )),
        other => io::Error::from(other),
    }
}

/// Open an anchor as given: the one directory whose path is trusted as the operator wrote it.
pub fn open_anchor(anchor: &Path) -> io::Result<OwnedFd> {
    nix::fcntl::open(anchor, directory_flags(), nix::sys::stat::Mode::empty())
        .map_err(io::Error::from)
}

/// Open the directory `relative` names below `anchor`, every component without following a
/// link. `relative` is plain: only names, no `..` and no root.
pub fn open_beneath(anchor: impl AsFd, relative: &Path) -> io::Result<OwnedFd> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    let mut current = openat(&anchor, ".", directory_flags(), Mode::empty())?;
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                current = openat(
                    &current,
                    name,
                    directory_flags() | OFlag::O_NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|errno| not_followed(name, errno))?;
            }
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a plain relative path", relative.display()),
                ));
            }
        }
    }
    Ok(current)
}

/// Remove the directory `name` of `parent` and everything below it through descriptors: the
/// directory is opened without following a link and compared with `expected` when given, then
/// claimed under a private name in `parent` and checked there by identity, emptied through its
/// own descriptor (an entry below it, a link included, is unlinked, never followed) and unlinked
/// under its private name. A name that is a symlink, no directory, or another directory than
/// `expected` is left and is an error; so is a directory that changed while being claimed. A
/// tree emptied only part way (nested deeper than [`REMOVAL_MAX_DEPTH`], a child that cannot be
/// unlinked) is a [`PartlyRemoved`] error naming the claim that holds the rest, and so is one
/// whose children were unlinked but whose final `unlinkat` failed (a child appeared meanwhile,
/// or the claim's name holds another directory).
pub fn remove_tree_at(
    parent: impl AsFd,
    name: &OsStr,
    expected: Option<Identity>,
) -> io::Result<()> {
    remove_tree(parent, name, expected, false).map(drop)
}

/// [`remove_tree_at`], accounting from the removal's own start: once the directory is claimed,
/// its allocated bytes are stated through its own descriptor, within the walk's reach,
/// immediately before anything is unlinked. Returns those bytes (`None` when they cannot be
/// measured); a removal that stops part way is a [`PartlyRemoved`] whose `freed` is those bytes
/// less what the claim still holds, stated the same way after it stopped, so a tree that grew
/// since an inventory measured it never reports nothing for what the walk did unlink.
pub fn remove_tree_measured_at(
    parent: impl AsFd,
    name: &OsStr,
    expected: Option<Identity>,
) -> io::Result<Option<u64>> {
    remove_tree(parent, name, expected, true)
}

fn remove_tree(
    parent: impl AsFd,
    name: &OsStr,
    expected: Option<Identity>,
    measure: bool,
) -> io::Result<Option<u64>> {
    use nix::dir::Dir;
    use nix::fcntl::OFlag;
    use nix::fcntl::renameat;
    use nix::sys::stat::Mode;
    use nix::unistd::{UnlinkatFlags, unlinkat};

    if name.is_empty() || name == "." || name == ".." || name.as_encoded_bytes().contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{}` is not one directory name", name.to_string_lossy()),
        ));
    }
    let mut directory = Dir::openat(
        &parent,
        name,
        directory_flags() | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|errno| not_followed(name, errno))?;
    let found = Identity::of_descriptor(&directory)?;
    if let Some(expected) = expected {
        if found != expected {
            return Err(io::Error::other(format!(
                "`{}` is not the directory that was measured (device {} inode {}, now device {} \
                 inode {}); it changed since and is left",
                name.to_string_lossy(),
                expected.device,
                expected.inode,
                found.device,
                found.inode
            )));
        }
    }
    // Claim the directory under a private name before anything is removed: whatever takes
    // `name` from now on is never touched. The claim is checked by identity, so a directory
    // that took `name` between the open above and the rename is put back and left.
    let claimed = claim_name();
    renameat(&parent, name, &parent, claimed.as_str()).map_err(io::Error::from)?;
    let at_claim = nix::sys::stat::fstatat(
        &parent,
        claimed.as_str(),
        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(io::Error::from)?;
    if Identity::of_stat(&at_claim) != found {
        // Not ours: return it to its name, unless that name was taken again meanwhile.
        let _ = renameat(&parent, claimed.as_str(), &parent, name);
        return Err(io::Error::other(format!(
            "`{}` changed while it was being removed; it is left",
            name.to_string_lossy()
        )));
    }
    // What the claim holds now, immediately before the walk unlinks anything, through the
    // descriptor the walk empties: the start every partial removal is accounted from.
    let before = if measure {
        within_reach(&directory).ok()
    } else {
        None
    };
    // What is left stays under its claim: renaming it back could replace whatever took the old
    // name since. Once this process has exited, a later sweep finishes the claim
    // ([`finish_abandoned_claims`]) and reports it while it cannot.
    let partly = |directory: &Dir, why: String| {
        let freed = before.and_then(|before| {
            within_reach(directory)
                .ok()
                .map(|remaining| before.saturating_sub(remaining))
        });
        io::Error::other(PartlyRemoved {
            message: format!(
                "`{}` was only partly removed ({why}); the rest is left under `{claimed}` for a \
                 later sweep",
                name.to_string_lossy()
            ),
            claim: claimed.clone(),
            freed,
        })
    };
    if let Err(errno) = crate::stale::remove_children_nofollow(&mut directory, 0) {
        return Err(partly(&directory, errno.to_string()));
    }
    // The claimed name is checked once more right before it is unlinked. POSIX has no unlink by
    // descriptor, so a directory put at this private name in the instant between the two could
    // still go; only an empty one can (`unlinkat` removes no other), and nothing af or a user
    // works with is ever named like a claim (ADR-0144). The descriptor stays open until then,
    // so what a failure here leaves is measured through it.
    before_final_unlink(&claimed);
    let still = nix::sys::stat::fstatat(
        &parent,
        claimed.as_str(),
        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(|errno| partly(&directory, format!("inspecting its claim: {errno}")))?;
    if Identity::of_stat(&still) != found {
        return Err(partly(
            &directory,
            "its claim changed while it was being removed, so what holds it is left".into(),
        ));
    }
    unlinkat(&parent, claimed.as_str(), UnlinkatFlags::RemoveDir)
        .map_err(|errno| partly(&directory, format!("removing its claim: {errno}")))?;
    Ok(before)
}

/// Allocated bytes of the directory `directory` holds open and of everything below it that the
/// removal walk reaches ([`REMOVAL_MAX_DEPTH`] levels of directories and their entries), stated
/// through descriptors without following a link; a file reached through several hard links
/// counts once. What lies deeper is never touched by the walk, so it is in neither of the two
/// measurements a partial removal is accounted from.
fn within_reach(directory: &impl AsFd) -> io::Result<u64> {
    let mut seen = std::collections::BTreeSet::new();
    let own = nix::sys::stat::fstat(directory).map_err(io::Error::from)?;
    Ok(allocated(&own).saturating_add(below(directory, 0, &mut seen)?))
}

#[allow(clippy::unnecessary_cast)]
fn allocated(stat: &nix::sys::stat::FileStat) -> u64 {
    (stat.st_blocks as u64).saturating_mul(512)
}

/// The entries of the directory at level `depth`, as [`within_reach`] counts them.
fn below(
    directory: &impl AsFd,
    depth: u32,
    seen: &mut std::collections::BTreeSet<Identity>,
) -> io::Result<u64> {
    use nix::errno::Errno;
    use nix::fcntl::{AtFlags, OFlag, openat};
    use nix::sys::stat::{Mode, SFlag, fstatat};
    if depth > REMOVAL_MAX_DEPTH {
        return Ok(0);
    }
    let mut listing = nix::dir::Dir::openat(directory, ".", directory_flags(), Mode::empty())
        .map_err(io::Error::from)?;
    let mut names = Vec::new();
    for entry in listing.iter() {
        let entry = entry.map_err(io::Error::from)?;
        let name = entry.file_name();
        if !matches!(name.to_bytes(), b"." | b"..") {
            names.push(std::ffi::CString::from(name));
        }
    }
    let mut total = 0_u64;
    for name in names {
        let stat = match fstatat(directory, name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            // Unlinked between the listing and the stat: it holds nothing now.
            Err(Errno::ENOENT) => continue,
            Err(errno) => return Err(errno.into()),
        };
        #[allow(clippy::unnecessary_cast)]
        let kind = SFlag::from_bits_truncate(stat.st_mode as nix::libc::mode_t) & SFlag::S_IFMT;
        if kind != SFlag::S_IFDIR {
            if stat.st_nlink > 1 && !seen.insert(Identity::of_stat(&stat)) {
                continue;
            }
            total = total.saturating_add(allocated(&stat));
            continue;
        }
        total = total.saturating_add(allocated(&stat));
        match openat(
            directory,
            name.as_c_str(),
            directory_flags() | OFlag::O_NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(child) => total = total.saturating_add(below(&child, depth + 1, seen)?),
            // Gone, or no directory any more: counted as it was stated.
            Err(Errno::ENOENT | Errno::ENOTDIR | Errno::ELOOP) => {}
            Err(errno) => return Err(errno.into()),
        }
    }
    Ok(total)
}

/// The hook [`set_before_final_unlink`] installs.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub type BeforeFinalUnlink = Box<dyn FnMut(&str)>;

#[cfg(debug_assertions)]
thread_local! {
    static BEFORE_FINAL_UNLINK: std::cell::RefCell<Option<BeforeFinalUnlink>> =
        const { std::cell::RefCell::new(None) };
}

/// A debug build's test hook, run on the removing thread with the claim's name after its
/// children were unlinked and before its final `unlinkat`: where a test makes a child appear,
/// as another process could. Release builds have no such hook.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_before_final_unlink(hook: Option<BeforeFinalUnlink>) {
    BEFORE_FINAL_UNLINK.with(|slot| *slot.borrow_mut() = hook);
}

fn before_final_unlink(claim: &str) {
    #[cfg(debug_assertions)]
    BEFORE_FINAL_UNLINK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(claim);
        }
    });
    #[cfg(not(debug_assertions))]
    let _ = claim;
}

/// Whether `name` is a claim [`remove_tree_at`] makes, and the pid of the process that made it.
fn claim_owner(name: &str) -> Option<u32> {
    name.strip_prefix(".af-removing-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

/// A removal a process that has since died left claimed, as one listing found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedClaim {
    /// Its private name in its parent, `.af-removing-<pid>-…`.
    pub name: String,
    /// The directory the listing stated, without following a link.
    pub identity: Identity,
    /// Its allocated bytes and the level of its deepest directory, as one walk measured them,
    /// or why that walk could not: an unmeasurable claim is never finished, since nothing could
    /// say what its removal freed.
    pub measured: Result<ClaimSize, Unmeasured>,
}

/// What the listing's walk measured of a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimSize {
    pub bytes: u64,
    /// The level of its deepest directory: one deeper than [`REMOVAL_MAX_DEPTH`] cannot be
    /// finished whole.
    pub depth: u32,
}

/// Why the listing's walk could not measure a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unmeasured {
    /// Nested deeper than the measurement walk reaches.
    TooDeep(String),
    /// Anything else the walk could not read.
    Unreadable(String),
}

/// List, reading only, the removals a process that has since died left claimed in `parent`:
/// every `.af-removing-<pid>-…` directory whose process is gone, with its identity and what
/// the walk measured of it, or why it could not. A claim of a live process is its own business
/// and is not listed.
pub fn abandoned_claims(parent: &Path) -> Vec<AbandonedClaim> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut claims = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(pid) = claim_owner(name) else {
            continue;
        };
        if crate::stale::process_exists(pid) {
            continue;
        }
        let Ok(identity) = Identity::of(&entry.path()) else {
            continue;
        };
        let measured = match crate::storage::allocated_bytes_and_depth(&entry.path()) {
            Ok((bytes, depth)) => Ok(ClaimSize { bytes, depth }),
            Err(error) if crate::storage::is_nested_too_deep(&error) => {
                Err(Unmeasured::TooDeep(error.to_string()))
            }
            Err(error) => Err(Unmeasured::Unreadable(error.to_string())),
        };
        claims.push(AbandonedClaim {
            name: name.to_string(),
            identity,
            measured,
        });
    }
    claims.sort_by(|a, b| a.name.cmp(&b.name));
    claims
}

/// Finish the removals a process that has since died left claimed in `parent`: every claim
/// [`abandoned_claims`] lists and measured is removed through [`remove_tree_at`], only while
/// it is the directory that was listed. Returns the names it removed with the bytes each held,
/// and why each other one was left, an unmeasurable claim included.
pub fn finish_abandoned_claims(parent: &Path) -> (Vec<(String, u64)>, Vec<String>) {
    let mut removed = Vec::new();
    let mut left = Vec::new();
    let claims = abandoned_claims(parent);
    if claims.is_empty() {
        return (removed, left);
    }
    let Ok(anchor) = open_anchor(parent) else {
        return (removed, left);
    };
    for claim in claims {
        let size = match &claim.measured {
            Ok(size) => *size,
            Err(Unmeasured::TooDeep(why) | Unmeasured::Unreadable(why)) => {
                left.push(format!(
                    "{}: cannot be measured, so it is left: {why}",
                    claim.name
                ));
                continue;
            }
        };
        match remove_tree_at(&anchor, OsStr::new(&claim.name), Some(claim.identity)) {
            Ok(()) => removed.push((claim.name, size.bytes)),
            Err(error) => left.push(format!("{}: {error}", claim.name)),
        }
    }
    (removed, left)
}

/// A private name for a directory being removed: hidden, unique to this process and call, and
/// never one a caller passes in.
fn claim_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    format!(
        ".af-removing-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Remove `target`, a directory strictly below `anchor`, through descriptors: `anchor` opened as
/// given, every component below it without following a link, and the last one through
/// [`remove_tree_at`] against `expected`.
pub fn remove_beneath(anchor: &Path, target: &Path, expected: Option<Identity>) -> io::Result<()> {
    let relative = target.strip_prefix(anchor).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not below {}", target.display(), anchor.display()),
        )
    })?;
    let (Some(parent), Some(name)) = (relative.parent(), relative.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is the anchor itself", target.display()),
        ));
    };
    let anchor = open_anchor(anchor)?;
    let parent = open_beneath(&anchor, parent)?;
    remove_tree_at(&parent, name, expected)
}

/// [`remove_beneath`] through [`remove_tree_measured_at`]: the bytes the directory held when its
/// removal started, and a partial removal's freed bytes accounted from them.
pub fn remove_beneath_measured(
    anchor: &Path,
    target: &Path,
    expected: Option<Identity>,
) -> io::Result<Option<u64>> {
    let relative = target.strip_prefix(anchor).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not below {}", target.display(), anchor.display()),
        )
    })?;
    let (Some(parent), Some(name)) = (relative.parent(), relative.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is the anchor itself", target.display()),
        ));
    };
    let anchor = open_anchor(anchor)?;
    let parent = open_beneath(&anchor, parent)?;
    remove_tree_measured_at(&parent, name, expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_tree_below_its_anchor_is_removed_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let anchor = root.path().join("anchor");
        let target = anchor.join("a/b/target");
        std::fs::create_dir_all(target.join("deep/er")).unwrap();
        std::fs::write(target.join("deep/er/file"), b"x").unwrap();
        let identity = Identity::of(&target).unwrap();
        remove_beneath(&anchor, &target, Some(identity)).unwrap();
        assert!(!target.exists());
        assert!(anchor.join("a/b").is_dir());
        // The private name it was claimed under is gone too: nothing is left beside it.
        assert_eq!(std::fs::read_dir(anchor.join("a/b")).unwrap().count(), 0);
    }

    #[test]
    fn a_removal_that_fails_part_way_stays_under_its_claim_and_a_dead_owners_claim_is_finished() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        // Deeper than the removal walks: emptying fails part way.
        let mut deep = parent.join("target");
        for _ in 0..=(crate::stale::MAX_DEPTH + 1) {
            deep = deep.join("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        let identity = Identity::of(&parent.join("target")).unwrap();
        let fd = open_anchor(&parent).unwrap();
        let (_, depth) = crate::storage::allocated_bytes_and_depth(&parent.join("target")).unwrap();
        assert_eq!(depth, crate::stale::MAX_DEPTH + 2);
        assert!(deeper_than_removal_walk(depth));
        let error = remove_tree_at(&fd, OsStr::new("target"), Some(identity)).unwrap_err();
        assert!(error.to_string().contains("left under"), "{error}");
        assert!(!parent.join("target").exists(), "it is never renamed back");
        let claim = partly_removed(&error).expect("the error names its claim");
        assert!(parent.join(claim).is_dir(), "what is left is under {claim}");
        let claims: Vec<String> = std::fs::read_dir(&parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(claims.len(), 1, "{claims:?}");
        assert!(claims[0].starts_with(&format!(".af-removing-{}-", std::process::id())));
        // This process is alive: its claim is its own and stays.
        let (removed, _) = finish_abandoned_claims(&parent);
        assert!(removed.is_empty());
        // A claim whose process is gone is finished; one that is not a claim is never touched.
        std::fs::create_dir_all(parent.join(".af-removing-999999999-1-0/inner")).unwrap();
        std::fs::create_dir(parent.join("neighbour")).unwrap();
        // Listing reads only: the dead owner's claim with what it is, and nothing else.
        let listed = abandoned_claims(&parent);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].name, ".af-removing-999999999-1-0");
        assert_eq!(
            listed[0].identity,
            Identity::of(&parent.join(".af-removing-999999999-1-0")).unwrap()
        );
        assert!(parent.join(".af-removing-999999999-1-0/inner").is_dir());
        let (removed, _) = finish_abandoned_claims(&parent);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].0, ".af-removing-999999999-1-0");
        assert!(parent.join("neighbour").is_dir());
    }

    /// The claim `parent` holds after a removal that stopped part way.
    fn the_claim(parent: &Path, error: &io::Error) -> std::path::PathBuf {
        parent.join(partly_removed(error).expect("the error names its claim"))
    }

    #[test]
    fn a_partial_removal_frees_what_its_claim_held_when_it_started_less_what_it_still_holds() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let target = parent.join("target");
        std::fs::create_dir_all(&target).unwrap();
        for n in 0..8 {
            std::fs::write(target.join(format!("blob-{n}")), vec![5_u8; 64 * 1024]).unwrap();
        }
        // Deeper than the walk reaches, a file at every level.
        let mut deep = target.join("deep");
        for n in 0..=REMOVAL_MAX_DEPTH + 2 {
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(deep.join(format!("f{n}")), vec![9_u8; 4096]).unwrap();
            deep = deep.join("d");
        }
        let identity = Identity::of(&target).unwrap();
        let before = crate::storage::allocated_bytes(&target).unwrap();
        let fd = open_anchor(&parent).unwrap();
        let error = remove_tree_measured_at(&fd, OsStr::new("target"), Some(identity)).unwrap_err();
        let claim = the_claim(&parent, &error);
        let remaining = crate::storage::allocated_bytes(&claim).unwrap();
        assert!(remaining > 0, "the walk could not reach all of it");
        let freed = partial(&error).unwrap().freed;
        assert_eq!(freed, Some(before - remaining), "{error}");
        // An unmeasured removal says nothing of what it freed.
        let again = Identity::of(&claim).unwrap();
        let name = claim.file_name().unwrap().to_os_string();
        let error = remove_tree_at(&fd, &name, Some(again)).unwrap_err();
        assert_eq!(partial(&error).unwrap().freed, None);
        std::fs::remove_dir_all(the_claim(&parent, &error)).unwrap();
    }

    #[test]
    fn a_child_that_appears_before_the_final_unlink_makes_the_removal_partial() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let target = parent.join("target");
        std::fs::create_dir_all(target.join("inner")).unwrap();
        std::fs::write(target.join("inner/file"), vec![4_u8; 128 * 1024]).unwrap();
        std::fs::write(target.join("file"), vec![4_u8; 64 * 1024]).unwrap();
        let identity = Identity::of(&target).unwrap();
        let before = crate::storage::allocated_bytes(&target).unwrap();
        let late = parent.clone();
        set_before_final_unlink(Some(Box::new(move |claim: &str| {
            std::fs::write(late.join(claim).join("late"), b"").unwrap();
        })));
        let fd = open_anchor(&parent).unwrap();
        let outcome = remove_tree_measured_at(&fd, OsStr::new("target"), Some(identity));
        set_before_final_unlink(None);
        let error = outcome.unwrap_err();
        assert!(error.to_string().contains("removing its claim"), "{error}");
        let claim = the_claim(&parent, &error);
        assert!(claim.join("late").is_file(), "what holds the claim is left");
        let remaining = crate::storage::allocated_bytes(&claim).unwrap();
        let freed = partial(&error).unwrap().freed.unwrap();
        assert_eq!(freed, before - remaining);
        assert!(freed >= 192 * 1024, "the children it removed: {freed}");
    }

    #[test]
    fn a_measured_removal_returns_what_it_held_when_it_started() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir_all(parent.join("target/inner")).unwrap();
        std::fs::write(parent.join("target/inner/file"), vec![1_u8; 64 * 1024]).unwrap();
        let before = crate::storage::allocated_bytes(&parent.join("target")).unwrap();
        let removed = remove_beneath_measured(root.path(), &parent.join("target"), None).unwrap();
        assert_eq!(removed, Some(before));
        assert!(!parent.join("target").exists());
    }

    #[test]
    fn an_unmeasurable_dead_claim_is_listed_as_such_and_never_finished() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let claim = parent.join(".af-removing-999999999-2-0");
        let mut deep = claim.clone();
        for _ in 0..300 {
            deep = deep.join("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("file"), b"x").unwrap();
        let listed = abandoned_claims(&parent);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(
            matches!(listed[0].measured, Err(Unmeasured::TooDeep(_))),
            "{listed:?}"
        );
        let (removed, left) = finish_abandoned_claims(&parent);
        assert!(removed.is_empty());
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(deep.join("file").is_file(), "nothing inside it is deleted");
    }

    #[test]
    fn a_directory_is_claimed_under_a_private_name_and_its_old_name_is_never_unlinked() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir_all(parent.join("target/inner")).unwrap();
        std::fs::write(parent.join("target/inner/file"), b"x").unwrap();
        std::fs::create_dir_all(parent.join("neighbour")).unwrap();
        let identity = Identity::of(&parent.join("target")).unwrap();
        let fd = open_anchor(&parent).unwrap();
        remove_tree_at(&fd, OsStr::new("target"), Some(identity)).unwrap();
        let left: Vec<_> = std::fs::read_dir(&parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, [std::ffi::OsString::from("neighbour")]);
        // A replacement made while the measured directory still exists is refused by
        // `a_directory_that_changed_since_it_was_measured_is_left`; after the removal a file
        // system may hand the freed inode to the next directory, so no identity check here.
    }

    #[test]
    fn a_symlinked_ancestor_is_never_followed_and_its_target_stays() {
        let root = tempfile::tempdir().unwrap();
        let anchor = root.path().join("anchor");
        let decoy = root.path().join("decoy");
        std::fs::create_dir_all(decoy.join("store")).unwrap();
        std::fs::write(decoy.join("store/precious"), b"keep").unwrap();
        std::fs::create_dir_all(&anchor).unwrap();
        symlink(&decoy, anchor.join("a")).unwrap();
        let error = remove_beneath(&anchor, &anchor.join("a/store"), None).unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
        assert_eq!(
            std::fs::read(decoy.join("store/precious")).unwrap(),
            b"keep"
        );
        // The last component is no-follow too.
        symlink(decoy.join("store"), anchor.join("last")).unwrap();
        assert!(remove_beneath(&anchor, &anchor.join("last"), None).is_err());
        assert!(decoy.join("store/precious").exists());
    }

    #[test]
    fn a_directory_that_changed_since_it_was_measured_is_left() {
        let root = tempfile::tempdir().unwrap();
        let measured = root.path().join("entry");
        std::fs::create_dir_all(&measured).unwrap();
        let identity = Identity::of(&measured).unwrap();
        std::fs::rename(&measured, root.path().join("moved")).unwrap();
        std::fs::create_dir_all(measured.join("other")).unwrap();
        let error = remove_beneath(root.path(), &measured, Some(identity)).unwrap_err();
        assert!(error.to_string().contains("not the directory"), "{error}");
        assert!(measured.join("other").is_dir());
        assert!(root.path().join("moved").is_dir());
    }

    #[test]
    fn only_a_plain_name_below_the_anchor_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        assert!(remove_beneath(root.path(), root.path(), None).is_err());
        assert!(remove_beneath(root.path(), Path::new("/elsewhere/x"), None).is_err());
        let anchor = open_anchor(root.path()).unwrap();
        assert!(open_beneath(&anchor, Path::new("../x")).is_err());
        assert!(remove_tree_at(&anchor, OsStr::new(".."), None).is_err());
    }
}
