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
    let claimed = claim_name();
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
    crate::stale::remove_children_nofollow(&mut directory, 0).map_err(io::Error::from)?;
    drop(directory);
    unlinkat(&parent, claimed.as_str(), UnlinkatFlags::RemoveDir).map_err(io::Error::from)
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
