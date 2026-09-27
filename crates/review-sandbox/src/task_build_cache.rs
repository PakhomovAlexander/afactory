//! The Warm Check Cache's host directory (ADR-0123): one machine-local build directory per
//! project, toolchain and kind, below `$XDG_CACHE_HOME/af/task-build-cache`.
//!
//! This is explicitly unsafe, candidate-built state, and nothing here pretends otherwise. What
//! this module owns is the directory's shape: private `0700` directories created and checked
//! without following links, one exclusive advisory lock per directory held for as long as a
//! check may write it, a no-follow byte count, and removal. It is never below a sandbox root, so
//! no seal, candidate capture or delivery can reach it; only the kernel's Task check runner
//! opens it, and only while it holds the lock.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::fcntl::{Flock, FlockArg};

/// The fixed directory name below `$XDG_CACHE_HOME/af`.
pub const TASK_BUILD_CACHE_DIRECTORY: &str = "task-build-cache";

/// How often a waiter retries a held lock. Short against the 60-second bound, long enough not
/// to spin.
const LOCK_RETRY: Duration = Duration::from_millis(100);

/// `$XDG_CACHE_HOME/af/task-build-cache`, or `$HOME/.cache/af/task-build-cache`. Resolved from
/// the kernel's own environment, never from a check's: every check receives a fresh
/// `XDG_CACHE_HOME` of its own.
pub fn default_task_build_cache_root() -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        Some(cache) => PathBuf::from(cache),
        None => std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(|home| PathBuf::from(home).join(".cache"))
            .ok_or("neither XDG_CACHE_HOME nor HOME locates the Task build cache")?,
    };
    if !base.is_absolute() {
        return Err("the Task build cache root must be an absolute directory".into());
    }
    Ok(base.join("af").join(TASK_BUILD_CACHE_DIRECTORY))
}

/// The outcome of asking for one directory's lock.
#[derive(Debug)]
pub enum TaskBuildCacheLock {
    /// This process holds the exclusive lock until the directory is dropped.
    Held(WarmDirectory),
    /// Another holder kept the lock for the whole wait.
    Busy,
}

/// One locked warm directory. The lock is released when this value is dropped, so a caller
/// that must remove the directory before release does so while it still holds this value.
#[derive(Debug)]
pub struct WarmDirectory {
    path: PathBuf,
    _lock: Flock<File>,
}

/// Lock `<root>/<project>/<toolchain>/<kind>` for exclusive use, waiting at most `wait`. The
/// three components are opaque lowercase-hex keys and a closed kind name, so no component can
/// name a parent or a sibling. The directory itself is not created here; see
/// [`WarmDirectory::ensure`].
pub fn lock_task_build_cache(
    root: &Path,
    project: &str,
    toolchain: &str,
    kind: &str,
    wait: Duration,
) -> Result<TaskBuildCacheLock, String> {
    if !is_key(project) || !is_key(toolchain) || !is_kind(kind) {
        return Err("Task build cache key is not an opaque identity".into());
    }
    if !root.is_absolute() {
        return Err("the Task build cache root must be an absolute directory".into());
    }
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating the Task build cache parent: {error}"))?;
    }
    let mut directory = root.to_path_buf();
    private_directory(&directory)?;
    for component in [project, toolchain] {
        directory.push(component);
        private_directory(&directory)?;
    }
    let lock_path = directory.join(format!("{kind}.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|error| format!("opening the Task build cache lock: {error}"))?;
    let started = Instant::now();
    let mut file = file;
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => {
                return Ok(TaskBuildCacheLock::Held(WarmDirectory {
                    path: directory.join(kind),
                    _lock: lock,
                }));
            }
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => {
                if started.elapsed() >= wait {
                    return Ok(TaskBuildCacheLock::Busy);
                }
                file = returned;
                std::thread::sleep(LOCK_RETRY.min(wait.saturating_sub(started.elapsed())));
            }
            Err((_, errno)) => {
                return Err(format!("locking the Task build cache: {errno}"));
            }
        }
    }
}

impl WarmDirectory {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes currently below the directory, counted without following links; zero when it is
    /// absent.
    pub fn bytes(&self) -> u64 {
        directory_bytes(&self.path)
    }

    /// Make the directory a private real directory owned by this user, holding only real files
    /// and directories of this user. Anything else at the path — a link, a file, another
    /// owner's or a widened directory — or anywhere below it — a link, a special file, another
    /// owner's entry — is suspect: the whole directory is removed first, never repaired in
    /// place, so a check can never reuse content that lives outside the keyed, bounded cache.
    pub fn ensure(&self) -> Result<(), String> {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            let private = metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.mode() & 0o777 == 0o700;
            if private && suspect_entry(&self.path).is_none() {
                return Ok(());
            }
            self.remove()?;
        }
        private_directory(&self.path)
    }

    /// Remove the directory and everything below it. Absence is success.
    pub fn remove(&self) -> Result<(), String> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                crate::restore_writable_dirs(&self.path);
                std::fs::remove_dir_all(&self.path)
                    .map_err(|error| format!("removing the Task build cache: {error}"))?;
            }
            Ok(_) => std::fs::remove_file(&self.path)
                .map_err(|error| format!("removing the Task build cache: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("inspecting the Task build cache: {error}")),
        }
        if std::fs::symlink_metadata(&self.path).is_ok() {
            return Err("Task build cache removal left the directory behind".into());
        }
        Ok(())
    }
}

/// The first entry below `root` that a check must not build on, walked without following
/// links: a symbolic link (its target lies outside the keyed, bounded directory), anything that
/// is neither a regular file nor a directory, or an entry another user owns. `None` when every
/// entry is a real file or directory of this user.
pub fn suspect_entry(root: &Path) -> Option<String> {
    let me = nix::unistd::geteuid().as_raw();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let kind = metadata.file_type();
            let reason = if kind.is_symlink() {
                Some("a link")
            } else if !kind.is_dir() && !kind.is_file() {
                Some("a special file")
            } else if metadata.uid() != me {
                Some("another user's entry")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Some(format!("{reason} at {}", path.display()));
            }
            if kind.is_dir() {
                pending.push(path);
            }
        }
    }
    None
}

/// The apparent size of every entry below `path`, walked without following links. Entries that
/// disappear during the walk — a build is running — are skipped; an absent path is zero bytes.
pub fn directory_bytes(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return metadata.len();
    }
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

/// Create one private directory level, or accept an existing real directory. A link at a
/// level this module owns is refused rather than followed.
fn private_directory(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if metadata.uid() != nix::unistd::geteuid().as_raw() {
                return Err("a Task build cache directory belongs to another user".into());
            }
            if metadata.mode() & 0o777 != 0o700 {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("securing a Task build cache directory: {error}"))?;
            }
            Ok(())
        }
        Ok(_) => Err("a Task build cache path is not a real directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(|error| format!("creating a Task build cache directory: {error}"))?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("securing a Task build cache directory: {error}"))
        }
        Err(error) => Err(format!("inspecting a Task build cache directory: {error}")),
    }
}

fn is_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_kind(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fill: char) -> String {
        fill.to_string().repeat(64)
    }

    #[test]
    fn a_locked_directory_is_private_counted_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let TaskBuildCacheLock::Held(directory) =
            lock_task_build_cache(&cache, &key('a'), &key('b'), "cargo_target", Duration::ZERO)
                .unwrap()
        else {
            panic!("an uncontended lock is held");
        };
        assert_eq!(directory.bytes(), 0);
        directory.ensure().unwrap();
        for level in [
            cache.clone(),
            cache.join(key('a')),
            cache.join(key('a')).join(key('b')),
            directory.path().to_path_buf(),
        ] {
            let metadata = std::fs::symlink_metadata(&level).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o700, "{}", level.display());
        }
        std::fs::create_dir(directory.path().join("debug")).unwrap();
        std::fs::write(directory.path().join("debug/out"), [0_u8; 100]).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", directory.path().join("link")).unwrap();
        assert_eq!(directory.bytes(), 100 + "/etc/hosts".len() as u64);
        directory.remove().unwrap();
        assert!(!directory.path().exists());
        assert_eq!(directory.bytes(), 0);
    }

    #[test]
    fn a_second_holder_is_busy_until_the_first_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let first = lock_task_build_cache(
            root.path(),
            &key('c'),
            &key('d'),
            "cargo_target",
            Duration::ZERO,
        )
        .unwrap();
        assert!(matches!(first, TaskBuildCacheLock::Held(_)));
        let started = Instant::now();
        let second = lock_task_build_cache(
            root.path(),
            &key('c'),
            &key('d'),
            "cargo_target",
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(matches!(second, TaskBuildCacheLock::Busy));
        assert!(started.elapsed() >= Duration::from_millis(300));
        drop(first);
        assert!(matches!(
            lock_task_build_cache(
                root.path(),
                &key('c'),
                &key('d'),
                "cargo_target",
                Duration::ZERO,
            )
            .unwrap(),
            TaskBuildCacheLock::Held(_)
        ));
    }

    #[test]
    fn a_suspect_directory_is_removed_rather_than_repaired() {
        let root = tempfile::tempdir().unwrap();
        let TaskBuildCacheLock::Held(directory) = lock_task_build_cache(
            root.path(),
            &key('e'),
            &key('f'),
            "cargo_target",
            Duration::ZERO,
        )
        .unwrap() else {
            panic!("an uncontended lock is held");
        };
        std::fs::create_dir(directory.path()).unwrap();
        std::fs::write(directory.path().join("planted"), b"x").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        directory.ensure().unwrap();
        assert!(!directory.path().join("planted").exists());
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("kept"), b"x").unwrap();
        directory.remove().unwrap();
        std::os::unix::fs::symlink(&outside, directory.path()).unwrap();
        directory.ensure().unwrap();
        assert!(outside.join("kept").exists(), "a link is never followed");
        assert!(
            !std::fs::symlink_metadata(directory.path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn keys_cannot_name_another_path() {
        let root = tempfile::tempdir().unwrap();
        for (project, toolchain, kind) in [
            ("..".to_string(), key('a'), "cargo_target"),
            (key('a'), "B".repeat(64), "cargo_target"),
            (key('a'), key('b'), "../x"),
        ] {
            assert!(
                lock_task_build_cache(root.path(), &project, &toolchain, kind, Duration::ZERO)
                    .is_err()
            );
        }
    }

    #[test]
    fn a_directory_holding_a_link_below_its_root_is_removed_before_reuse() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let TaskBuildCacheLock::Held(directory) =
            lock_task_build_cache(&cache, &key('a'), &key('b'), "cargo_target", Duration::ZERO)
                .unwrap()
        else {
            panic!("an uncontended lock is held");
        };
        directory.ensure().unwrap();
        std::fs::create_dir(directory.path().join("debug")).unwrap();
        std::fs::write(directory.path().join("debug").join("real"), b"ok").unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, [1_u8; 32]).unwrap();
        std::os::unix::fs::symlink(&outside, directory.path().join("debug").join("link")).unwrap();
        assert!(
            suspect_entry(directory.path()).is_some(),
            "the link is found below the root"
        );
        directory.ensure().unwrap();
        assert!(
            !directory.path().join("debug").exists(),
            "the whole directory was removed, never repaired"
        );
        assert!(outside.exists(), "the link's target is never followed");
        assert_eq!(suspect_entry(directory.path()), None);
    }
}
