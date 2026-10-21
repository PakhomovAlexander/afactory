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
/// `expected` is left and is an error; so is a directory that changed while being claimed.
pub fn remove_tree_at(
    parent: impl AsFd,
    name: &OsStr,
    expected: Option<Identity>,
) -> io::Result<()> {
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
    let claimed = claim_name(std::process::id(), found);
    renameat(&parent, name, &parent, claimed.as_str()).map_err(io::Error::from)?;
    let at_claim = nix::sys::stat::fstatat(
        &parent,
        claimed.as_str(),
        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(io::Error::from)?;
    #[allow(clippy::unnecessary_cast)]
    let claimed_identity = Identity {
        device: at_claim.st_dev as u64,
        inode: at_claim.st_ino as u64,
    };
    if claimed_identity != found {
        // Not ours: return it to its name, unless that name was taken again meanwhile.
        let _ = renameat(&parent, claimed.as_str(), &parent, name);
        return Err(io::Error::other(format!(
            "`{}` changed while it was being removed; it is left",
            name.to_string_lossy()
        )));
    }
    if let Err(errno) = crate::stale::remove_children_nofollow(&mut directory, 0) {
        // What is left stays under its claim: renaming it back could replace whatever took the
        // old name since. Once this process has exited, a later sweep finishes the claim
        // ([`finish_abandoned_claims`]) and reports it while it cannot.
        return Err(io::Error::other(format!(
            "`{}` was only partly removed ({errno}); the rest is left under `{claimed}` for a \
             later sweep",
            name.to_string_lossy()
        )));
    }
    drop(directory);
    // The claimed name is checked once more right before it is unlinked. POSIX has no unlink by
    // descriptor, so a directory put at this private name in the instant between the two could
    // still go; only an empty one can (`unlinkat` removes no other), and nothing af or a user
    // works with is ever named like a claim (ADR-0144).
    let still = nix::sys::stat::fstatat(
        &parent,
        claimed.as_str(),
        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(io::Error::from)?;
    #[allow(clippy::unnecessary_cast)]
    let still = Identity {
        device: still.st_dev as u64,
        inode: still.st_ino as u64,
    };
    if still != found {
        return Err(io::Error::other(format!(
            "`{}` changed while it was being removed; what holds its claim is left",
            name.to_string_lossy()
        )));
    }
    unlinkat(&parent, claimed.as_str(), UnlinkatFlags::RemoveDir).map_err(io::Error::from)
}

/// The pid of the process that made the claim `name` in `parent`, when [`remove_tree_at`] made
/// it: `name` is exactly the name [`claim_name`] gives, and `parent` holds there, not followed
/// through a link, a directory with the very device and inode that name binds. Any other name,
/// however claim-shaped, is not af's (ADR-0144).
pub fn claim_owner_at(parent: impl AsFd, name: &OsStr) -> Option<u32> {
    let text = name.to_str()?;
    let mut fields = text.strip_prefix(".af-removing-")?.split('-');
    let pid: u32 = fields.next()?.parse().ok()?;
    let bound = Identity {
        device: fields.next()?.parse().ok()?,
        inode: fields.next()?.parse().ok()?,
    };
    // Exactly as written: no fourth field, sign or leading zero.
    if claim_name(pid, bound) != text {
        return None;
    }
    let stat =
        nix::sys::stat::fstatat(&parent, name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW).ok()?;
    let directory = nix::sys::stat::SFlag::from_bits_truncate(stat.st_mode)
        & nix::sys::stat::SFlag::S_IFMT
        == nix::sys::stat::SFlag::S_IFDIR;
    (directory && Identity::of_stat(&stat) == bound).then_some(pid)
}

/// Finish the removals a process that has since died left claimed in `parent`: every
/// `.af-removing-<pid>-…` directory that is a claim af made ([`claim_owner_at`]) and whose
/// process is gone is removed through [`remove_tree_at`]. A claim of a live process is its own
/// business and stays; a claim-shaped name af cannot prove its own is never touched. Returns
/// the names it removed with the bytes each held, and why each other one was left.
pub fn finish_abandoned_claims(parent: &Path) -> (Vec<(String, u64)>, Vec<String>) {
    let mut removed = Vec::new();
    let mut left = Vec::new();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return (removed, left);
    };
    let Ok(anchor) = open_anchor(parent) else {
        return (removed, left);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = claim_owner_at(&anchor, &name) else {
            continue;
        };
        if crate::stale::process_exists(pid) {
            continue;
        }
        let Ok(identity) = Identity::of(&entry.path()) else {
            continue;
        };
        let bytes = crate::storage::allocated_bytes(&entry.path()).unwrap_or(0);
        match remove_tree_at(&anchor, &name, Some(identity)) {
            Ok(()) => removed.push((name.to_string_lossy().into_owned(), bytes)),
            Err(error) => left.push(format!("{}: {error}", name.to_string_lossy())),
        }
    }
    (removed, left)
}

/// The private name the process `pid` claims the directory `identity` under while removing it:
/// hidden, unique to the process and the directory, never one a caller passes in, and the proof
/// [`claim_owner_at`] checks against what is on disk (`.af-removing-<pid>-<device>-<inode>`).
pub fn claim_name(pid: u32, identity: Identity) -> String {
    format!(".af-removing-{pid}-{}-{}", identity.device, identity.inode)
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
        let error = remove_tree_at(&fd, OsStr::new("target"), Some(identity)).unwrap_err();
        assert!(error.to_string().contains("left under"), "{error}");
        assert!(!parent.join("target").exists(), "it is never renamed back");
        let claims: Vec<String> = std::fs::read_dir(&parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(claims, [claim_name(std::process::id(), identity)]);
        assert_eq!(
            claim_owner_at(&fd, OsStr::new(&claims[0])),
            Some(std::process::id())
        );
        // This process is alive: its claim is its own and stays.
        let (removed, _) = finish_abandoned_claims(&parent);
        assert!(removed.is_empty());
        // A claim whose process is gone is finished; one that is not a claim is never touched,
        // nor is a claim-shaped name whose entry is not the directory it binds (audit C2).
        std::fs::create_dir_all(parent.join("abandoned/inner")).unwrap();
        let abandoned = claim_name(
            999_999_999,
            Identity::of(&parent.join("abandoned")).unwrap(),
        );
        std::fs::rename(parent.join("abandoned"), parent.join(&abandoned)).unwrap();
        std::fs::create_dir(parent.join("neighbour")).unwrap();
        std::fs::create_dir_all(parent.join(".af-removing-999999999-1-0/inner")).unwrap();
        std::fs::write(parent.join(".af-removing-1-2-3"), b"mine").unwrap();
        let (removed, _) = finish_abandoned_claims(&parent);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].0, abandoned);
        assert!(parent.join("neighbour").is_dir());
        assert!(parent.join(".af-removing-999999999-1-0/inner").is_dir());
        assert_eq!(
            std::fs::read(parent.join(".af-removing-1-2-3")).unwrap(),
            b"mine"
        );
    }

    /// A claim-shaped name is af's only while it names exactly the directory it binds: the
    /// device and inode in its name, a directory, written as `claim_name` writes it (audit C2).
    #[test]
    fn a_claim_is_proven_only_by_the_directory_its_name_binds() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path();
        std::fs::create_dir(parent.join("made")).unwrap();
        let identity = Identity::of(&parent.join("made")).unwrap();
        let name = claim_name(42, identity);
        std::fs::rename(parent.join("made"), parent.join(&name)).unwrap();
        let fd = open_anchor(parent).unwrap();
        assert_eq!(claim_owner_at(&fd, OsStr::new(&name)), Some(42));
        // Another directory, a file, or a link at a name binding some identity is not a claim.
        let other = Identity::of(&parent.join(&name)).unwrap();
        let padded = format!(".af-removing-42-0{}-{}", other.device, other.inode);
        std::fs::rename(parent.join(&name), parent.join(&padded)).unwrap();
        assert_eq!(claim_owner_at(&fd, OsStr::new(&padded)), None);
        std::fs::rename(parent.join(&padded), parent.join(&name)).unwrap();
        std::fs::write(parent.join("file"), b"").unwrap();
        let file = claim_name(42, Identity::of(&parent.join("file")).unwrap());
        std::fs::rename(parent.join("file"), parent.join(&file)).unwrap();
        assert_eq!(claim_owner_at(&fd, OsStr::new(&file)), None);
        let link = claim_name(43, identity);
        symlink(&name, parent.join(&link)).unwrap();
        assert_eq!(claim_owner_at(&fd, OsStr::new(&link)), None);
        std::fs::write(parent.join(".af-removing-1-2-3"), b"mine").unwrap();
        for name in [
            ".af-removing-1-2-3",
            ".af-removing-1-2",
            ".af-removing-",
            ".af-removing-absent-1-2",
            "made",
        ] {
            assert_eq!(claim_owner_at(&fd, OsStr::new(name)), None, "{name}");
        }
        let extra = format!("{name}-0");
        std::fs::create_dir(parent.join(&extra)).unwrap();
        assert_eq!(claim_owner_at(&fd, OsStr::new(&extra)), None);
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
