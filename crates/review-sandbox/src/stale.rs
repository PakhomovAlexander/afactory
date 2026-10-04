//! Stale sandbox removal: the one place that knows how a sandbox directory is named, and the
//! sweep that removes the directories a dead process left behind.
//!
//! Every temporary sandbox and temporary template lives in a directory under the process's
//! temporary root named `af-sandbox-<pid>-<random>`. The handle that owns the directory removes
//! it on drop, so a process that ends normally leaves nothing. A process that is killed, or
//! that aborts, never runs its drops: its trees stay in the temporary root forever, and a tree
//! that a Gate built into carries a whole `target/` directory. The pid in the name is what lets
//! a later process tell such a leftover from a sandbox another live process is using right now.
//!
//! The sweep is conservative. It touches only directories with this exact name shape, it keeps a
//! directory whenever a process with that pid exists (a recycled pid keeps a leftover until the
//! next sweep, which is the harmless direction), and it never follows a symlink into or out of a
//! sandbox. The temporary root is the one `std::env::temp_dir()` names for this process, so a
//! sweep under a redirected `TMPDIR` sees exactly the sandboxes created under that `TMPDIR`. A
//! check that outlived its killed `af` parent loses its tree here; it was orphaned already, and
//! `review-process` ends a supervised group with its leader on every path short of `SIGKILL`.

use std::path::Path;

use crate::restore_writable_dirs;

const PREFIX: &str = "af-sandbox-";

/// A fresh, private directory for one sandbox or template, named so a later sweep can
/// attribute it to this process.
pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(&format!("{PREFIX}{}-", std::process::id()))
        .tempdir()
}

/// The owning pid encoded in a sandbox directory name, or `None` for any other entry.
fn owner_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(PREFIX)?;
    let (pid, random) = rest.split_once('-')?;
    if pid.is_empty() || random.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
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
fn process_exists(_pid: u32) -> bool {
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
}

/// Remove every sandbox directory under this process's temporary root whose owning process no
/// longer exists. Best-effort and silent: an unreadable root or a failed removal is counted,
/// never raised, because nothing a review does depends on this housekeeping.
pub fn sweep_stale_sandboxes() -> SweepReport {
    sweep_stale_sandboxes_in(&std::env::temp_dir())
}

/// [`sweep_stale_sandboxes`] against an explicit root, for tests and for operators pointing at
/// another process's `TMPDIR`.
pub fn sweep_stale_sandboxes_in(root: &Path) -> SweepReport {
    let mut report = SweepReport::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return report;
    };
    let own_pid = std::process::id();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(owner_pid) else {
            continue;
        };
        let path = entry.path();
        let is_directory = std::fs::symlink_metadata(&path)
            .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            .unwrap_or(false);
        if !is_directory {
            continue;
        }
        if pid == own_pid || process_exists(pid) {
            report.live += 1;
            continue;
        }
        if remove_sandbox_dir(&path) {
            report.removed += 1;
        } else {
            report.failed += 1;
        }
    }
    report
}

/// A read-only sandbox left its directories at 0o555; restore owner write before unlinking,
/// exactly as a live handle's drop does.
fn remove_sandbox_dir(path: &Path) -> bool {
    restore_writable_dirs(path);
    std::fs::remove_dir_all(path).is_ok()
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
                live: 1
            }
        );
        assert!(!dead.exists());
        assert!(live.exists());
        assert!(stranger.exists());
        assert!(file.exists());
    }

    #[test]
    fn an_unreadable_root_sweeps_nothing() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("absent");
        assert_eq!(sweep_stale_sandboxes_in(&missing), SweepReport::default());
    }
}
