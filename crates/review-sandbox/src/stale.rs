//! Stale sandbox removal: the one place that knows how a sandbox directory and a check runtime
//! directory are named, and the sweep that removes the directories a dead process left behind.
//!
//! Every temporary sandbox and temporary template lives in a directory under the process's
//! temporary root named `af-sandbox-<pid>-<random>`, and every check's runtime directory (its
//! `HOME`, `TMPDIR`, `AF_CHECK_SCRATCH` and `XDG_CACHE_HOME`) in one named `af-check-<pid>-<random>`. The handle that owns the directory removes
//! it on drop, so a process that ends normally leaves nothing. A process that is killed, or
//! that aborts, never runs its drops: its trees stay in the temporary root forever, and a tree
//! that a Gate built into carries a whole `target/` directory. The pid in the name is what lets
//! a later process tell such a leftover from a sandbox another live process is using right now.
//!
//! The sweep is conservative. It touches only directories with this exact name shape; it keeps a
//! directory whenever a process with that pid exists (a recycled pid keeps a leftover until the
//! next sweep, which is the harmless direction); it keeps a directory the kernel preserved on
//! purpose, marked by [`PRESERVED`] beside the tree, because a container may still hold that
//! tree as a writable bind; and it removes a tree only through directory descriptors opened
//! without following links, so a child that outlived its killed `af` parent cannot redirect the
//! walk by swapping a directory for a symlink while it runs. The temporary root is the one
//! `std::env::temp_dir()` names for this process, so a sweep under a redirected `TMPDIR` sees
//! exactly the sandboxes created under that `TMPDIR`. A check that outlived its killed parent
//! loses its tree here; it was orphaned already, and `review-process` ends a supervised group
//! with its leader on every path short of `SIGKILL`.

use std::path::Path;

const PREFIX: &str = "af-sandbox-";
/// The name prefix of a check's runtime directory (see [`crate::CheckRuntime`]).
pub(crate) const CHECK_PREFIX: &str = "af-check-";

/// The marker a provider writes beside a sandbox tree (never inside it, where a container bind
/// could see it) when it releases the handle without removing the directory on purpose. A
/// marked directory is an operator's to recover; no sweep touches it.
pub(crate) const PRESERVED: &str = "preserved";

/// Directories deeper than this are not walked; a sandbox tree is nowhere near it, and a bound
/// keeps a hostile tree from exhausting the stack.
pub(crate) const MAX_DEPTH: u32 = 128;

/// A fresh, private directory for one sandbox or template, named so a later sweep can
/// attribute it to this process.
pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(&format!("{PREFIX}{}-", std::process::id()))
        .tempdir()
}

/// A fresh, private directory for one Worker Attempt or admission probe that needs a working
/// directory of its own: named like a sandbox, so the crash sweep attributes it to this process
/// and a harness that keys its history by working directory can be told it was af's.
pub fn attempt_directory() -> std::io::Result<tempfile::TempDir> {
    tempdir()
}

/// A fresh, private check runtime directory named `af-check-<pid>-<random>`.
pub(crate) fn check_tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(&format!("{CHECK_PREFIX}{}-", std::process::id()))
        .tempdir()
}

/// Whether `name` is the name af gives a sandbox or a check runtime directory: either prefix,
/// a decimal pid and a non-empty random part.
pub fn is_af_directory_name(name: &str) -> bool {
    owner_pid(name).is_some()
}

/// Write the [`PRESERVED`] marker into a sandbox's own directory (the parent of its tree).
pub(crate) fn mark_preserved(sandbox_dir: &Path) -> std::io::Result<()> {
    std::fs::write(
        sandbox_dir.join(PRESERVED),
        b"container cleanup was not confirmed\n",
    )
}

/// The owning pid encoded in a sandbox directory name, or `None` for any other entry.
fn owner_pid(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix(PREFIX)
        .or_else(|| name.strip_prefix(CHECK_PREFIX))?;
    let (pid, random) = rest.split_once('-')?;
    if pid.is_empty() || random.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

#[cfg(unix)]
pub(crate) fn process_exists(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    // Signal 0 performs the permission and existence checks without delivering anything.
    // `EPERM` means the process exists under another user: keep its directory.
    !matches!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH))
}

#[cfg(not(unix))]
pub(crate) fn process_exists(_pid: u32) -> bool {
    true
}

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Directories removed because no process with their pid exists.
    pub removed: usize,
    /// Directories whose removal failed part-way; the next sweep retries them.
    pub failed: usize,
    /// Directories kept because their process is still running.
    pub live: usize,
    /// Directories kept because the kernel preserved them on purpose.
    pub preserved: usize,
}

/// Remove every sandbox and check runtime directory under this process's temporary root whose
/// owning process no longer exists. Best-effort and silent: an unreadable root or a failed removal is counted,
/// never raised, because nothing a review does depends on this housekeeping.
pub fn sweep_stale_sandboxes() -> SweepReport {
    sweep_stale_sandboxes_in(&std::env::temp_dir())
}

/// [`sweep_stale_sandboxes`] against an explicit root, for tests and for operators pointing at
/// another process's `TMPDIR`.
pub fn sweep_stale_sandboxes_in(root: &Path) -> SweepReport {
    let mut report = SweepReport::default();
    // A runtime or sandbox removal a dead process left claimed is finished first (ADR-0144).
    #[cfg(unix)]
    {
        let (removed, left) = crate::finish_abandoned_claims(root);
        report.removed += removed.len();
        report.failed += left.len();
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return report;
    };
    let own_pid = std::process::id();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(owner_pid) else {
            continue;
        };
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        if pid == own_pid || process_exists(pid) {
            report.live += 1;
            continue;
        }
        if path.join(PRESERVED).exists() {
            report.preserved += 1;
            continue;
        }
        if remove_listed(root, &entry.file_name(), &metadata) {
            report.removed += 1;
        } else {
            report.failed += 1;
        }
    }
    report
}

/// Remove the directory a sweep listed, only while its name still holds that directory: the
/// identity the listing measured binds the claimed removal (ADR-0144).
#[cfg(unix)]
fn remove_listed(root: &Path, name: &std::ffi::OsStr, listed: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let identity = crate::Identity {
        device: listed.dev(),
        inode: listed.ino(),
    };
    crate::open_anchor(root)
        .and_then(|anchor| crate::remove_tree_at(&anchor, name, Some(identity)))
        .is_ok()
}

#[cfg(not(unix))]
fn remove_listed(root: &Path, name: &std::ffi::OsStr, _listed: &std::fs::Metadata) -> bool {
    remove_tree_nofollow(root, name)
}

/// Remove `<parent>/<name>` and everything below it without following a single link: every
/// directory is opened relative to its parent's descriptor with `O_NOFOLLOW`, made writable
/// through that descriptor (a read-only sandbox left its directories at 0o555), and emptied
/// with `unlinkat`. A name that turns into a symlink between the listing and the open is
/// unlinked as the link it now is. Returns whether the whole tree is gone.
#[cfg(unix)]
pub fn remove_tree_nofollow(parent: &Path, name: &std::ffi::OsStr) -> bool {
    use nix::dir::Dir;
    use nix::fcntl::OFlag;
    use nix::sys::stat::Mode;
    use nix::unistd::{UnlinkatFlags, unlinkat};

    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY;
    let Ok(parent) = Dir::open(parent, flags, Mode::empty()) else {
        return false;
    };
    let Ok(mut root) = Dir::openat(&parent, name, flags, Mode::empty()) else {
        return false;
    };
    if remove_children_nofollow(&mut root, 0).is_err() {
        return false;
    }
    drop(root);
    unlinkat(&parent, name, UnlinkatFlags::RemoveDir).is_ok()
}

#[cfg(unix)]
pub(crate) fn remove_children_nofollow(
    directory: &mut nix::dir::Dir,
    depth: u32,
) -> nix::Result<()> {
    use nix::dir::{Dir, Type};
    use nix::errno::Errno;
    use nix::fcntl::OFlag;
    use nix::sys::stat::{Mode, fchmod};
    use nix::unistd::{UnlinkatFlags, unlinkat};
    use std::ffi::CString;

    if depth > MAX_DEPTH {
        return Err(Errno::ELOOP);
    }
    // Owner read, write and search on the descriptor itself: unlinking an entry needs write on
    // its directory, and nothing a path lookup could be redirected to is touched.
    fchmod(&*directory, Mode::S_IRWXU)?;
    // List first, unlink after: a directory stream read while its entries vanish is undefined.
    let mut entries = Vec::new();
    for entry in directory.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        entries.push((CString::from(name), entry.file_type()));
    }
    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY;
    for (name, kind) in entries {
        let name = name.as_c_str();
        if kind.is_some_and(|kind| kind != Type::Directory) {
            unlinkat(&*directory, name, UnlinkatFlags::NoRemoveDir)?;
            continue;
        }
        // A directory, or a filesystem that does not say: open it without following. `ENOTDIR`
        // and `ELOOP` mean the entry is not a directory after all (a symlink put in its place
        // included), so it is unlinked as what it is.
        match Dir::openat(&*directory, name, flags, Mode::empty()) {
            Ok(mut child) => {
                remove_children_nofollow(&mut child, depth + 1)?;
                drop(child);
                unlinkat(&*directory, name, UnlinkatFlags::RemoveDir)?;
            }
            Err(Errno::ENOTDIR | Errno::ELOOP) => {
                unlinkat(&*directory, name, UnlinkatFlags::NoRemoveDir)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn remove_tree_nofollow(parent: &Path, name: &std::ffi::OsStr) -> bool {
    std::fs::remove_dir_all(parent.join(name)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sandbox_directory_names_its_owner() {
        assert_eq!(owner_pid("af-sandbox-4242-Ab3xYz"), Some(4242));
        assert_eq!(owner_pid("af-sandbox-4242-"), None);
        assert_eq!(owner_pid("af-sandbox--Ab3xYz"), None);
        assert_eq!(owner_pid("af-sandbox-42x-Ab3xYz"), None);
        assert_eq!(owner_pid(".tmpAb3xYz"), None);
        assert_eq!(owner_pid("af-sandbox-99999999999-Ab3xYz"), None);
        assert_eq!(owner_pid("af-check-4242-Ab3xYz"), Some(4242));
        assert_eq!(owner_pid("af-check--Ab3xYz"), None);
        assert!(is_af_directory_name("af-check-17-x"));
        assert!(!is_af_directory_name("af-checks-17-x"));
    }

    #[test]
    fn a_fresh_check_runtime_is_attributed_to_this_process() {
        let dir = check_tempdir().unwrap();
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(CHECK_PREFIX), "{name}");
        assert_eq!(owner_pid(name), Some(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn the_sweep_removes_a_dead_check_runtime_and_keeps_a_live_one() {
        let root = tempfile::tempdir().unwrap();
        let dead = root.path().join("af-check-2000000000-dead03");
        std::fs::create_dir_all(dead.join("home").join(".cache")).unwrap();
        std::fs::write(dead.join("scratch-file"), b"x").unwrap();
        let live = root
            .path()
            .join(format!("af-check-{}-live02", std::process::id()));
        std::fs::create_dir_all(live.join("tmp")).unwrap();

        let report = sweep_stale_sandboxes_in(root.path());

        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.live, 1, "{report:?}");
        assert!(!dead.exists());
        assert!(live.join("tmp").exists());
    }

    #[test]
    fn a_fresh_tempdir_is_attributed_to_this_process() {
        let dir = tempdir().unwrap();
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(owner_pid(name), Some(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn the_sweep_removes_dead_owners_and_keeps_live_ones_and_strangers() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        // A pid no process has: above every platform's `pid_max`, still inside `pid_t`.
        let dead = root.path().join("af-sandbox-2000000000-dead01");
        std::fs::create_dir_all(dead.join("tree").join("src")).unwrap();
        std::fs::write(dead.join("tree").join("src").join("lib.rs"), b"fn x() {}").unwrap();
        std::fs::set_permissions(dead.join("tree"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let live = root
            .path()
            .join(format!("af-sandbox-{}-live01", std::process::id()));
        std::fs::create_dir_all(live.join("tree")).unwrap();
        let stranger = root.path().join(".tmpStranger");
        std::fs::create_dir_all(stranger.join("tree")).unwrap();
        let file = root.path().join("af-sandbox-2000000000-notadir");
        std::fs::write(&file, b"").unwrap();

        let report = sweep_stale_sandboxes_in(root.path());

        assert_eq!(
            report,
            SweepReport {
                removed: 1,
                failed: 0,
                live: 1,
                preserved: 0,
            }
        );
        assert!(!dead.exists());
        assert!(live.exists());
        assert!(stranger.exists());
        assert!(file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_preserved_sandbox_of_a_dead_owner_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let kept = root.path().join("af-sandbox-2000000000-kept01");
        std::fs::create_dir_all(kept.join("tree")).unwrap();
        mark_preserved(&kept).unwrap();

        let report = sweep_stale_sandboxes_in(root.path());

        assert_eq!(
            report,
            SweepReport {
                removed: 0,
                failed: 0,
                live: 0,
                preserved: 1,
            }
        );
        assert!(kept.join("tree").exists());
        assert!(kept.join(PRESERVED).exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_sweep_never_follows_a_link_out_of_a_dead_sandbox() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(outside.join("keep")).unwrap();
        std::fs::write(outside.join("keep").join("data"), b"precious").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o555)).unwrap();

        let dead = root.path().join("af-sandbox-2000000000-dead02");
        std::fs::create_dir_all(dead.join("tree")).unwrap();
        // A directory entry that is a link: the swap a surviving child could make mid-walk.
        symlink(&outside, dead.join("tree").join("escape")).unwrap();
        symlink(
            outside.join("keep").join("data"),
            dead.join("tree").join("file"),
        )
        .unwrap();

        let report = sweep_stale_sandboxes_in(root.path());

        assert_eq!(report.removed, 1, "{report:?}");
        assert!(!dead.exists());
        assert!(outside.join("keep").join("data").exists());
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o555,
            "the link target's mode is untouched"
        );
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn an_unreadable_root_sweeps_nothing() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("absent");
        assert_eq!(sweep_stale_sandboxes_in(&missing), SweepReport::default());
    }
}
