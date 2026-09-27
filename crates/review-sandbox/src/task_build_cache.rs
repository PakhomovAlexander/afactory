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
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use nix::dir::Dir;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use nix::sys::stat::SFlag;

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
    /// Names that must never exist directly below the directory, such as Cargo's
    /// `credentials.toml` in a `cargo_home`; one present makes the whole directory suspect.
    forbidden: &'static [&'static str],
    _lock: Flock<File>,
}

/// What [`WarmDirectory::ensure`] found before the check could use the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ensured {
    /// A private directory whose every entry passed inspection: its bytes may be reused.
    Reused,
    /// Nothing was there; an empty private directory now is.
    Created,
    /// The directory was suspect, removed and recreated empty. The reason names what was found.
    Discarded(String),
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
    match exclusive_lock(&directory.join(format!("{kind}.lock")), wait)? {
        Some(lock) => Ok(TaskBuildCacheLock::Held(WarmDirectory {
            path: directory.join(kind),
            forbidden: &[],
            _lock: lock,
        })),
        None => Ok(TaskBuildCacheLock::Busy),
    }
}

/// The exclusive lock over one whole toolchain key, `<root>/<project>/<toolchain>/warm.lock`.
/// A check holds it from preparation to the end of its removal step, so every warm operation of
/// one project and toolchain is serialized whatever kinds each check declares: two checks that
/// each hold one kind can never together exceed the key's shared bound.
#[derive(Debug)]
pub struct TaskBuildCacheKeyLock(#[allow(dead_code)] Flock<File>);

/// Lock the toolchain key `<root>/<project>/<toolchain>` for exclusive use, waiting at most
/// `wait`. `None` when another holder kept it for the whole wait.
pub fn lock_task_build_cache_key(
    root: &Path,
    project: &str,
    toolchain: &str,
    wait: Duration,
) -> Result<Option<TaskBuildCacheKeyLock>, String> {
    if !is_key(project) || !is_key(toolchain) {
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
    Ok(exclusive_lock(&directory.join("warm.lock"), wait)?.map(TaskBuildCacheKeyLock))
}

/// Open `path` without following links and take its exclusive advisory lock, retrying a held
/// lock until `wait` has elapsed.
fn exclusive_lock(path: &Path, wait: Duration) -> Result<Option<Flock<File>>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("opening the Task build cache lock: {error}"))?;
    let started = Instant::now();
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(Some(lock)),
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => {
                if started.elapsed() >= wait {
                    return Ok(None);
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

    /// Treat any of `names` directly below the directory as suspect, so [`Self::ensure`]
    /// removes the whole directory rather than let a check reuse it.
    pub fn forbidding(mut self, names: &'static [&'static str]) -> Self {
        self.forbidden = names;
        self
    }

    /// Bytes currently below the directory, counted without following links; zero when it is
    /// absent. A directory the count cannot fully inspect is `Uninspectable`, never smaller.
    pub fn bytes(&self) -> Result<u64, Uninspectable> {
        directory_bytes(&self.path)
    }

    /// Make the directory a private real directory owned by this user, holding only real files
    /// and directories of this user. Anything else at the path — a link, a file, another
    /// owner's or a widened directory — or anywhere below it — a link, a special file, another
    /// owner's entry, an entry the walk cannot inspect, a forbidden name — is suspect: the whole
    /// directory is removed first, never repaired in place, so a check can never reuse content
    /// that lives outside the keyed, bounded cache. The result says which happened, so a caller
    /// never reports a discarded directory's old bytes as available.
    pub fn ensure(&self) -> Result<Ensured, String> {
        let found = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => {
                let kind = metadata.file_type();
                let reason = if kind.is_symlink() {
                    Some("the directory is a link".to_string())
                } else if !metadata.is_dir() {
                    Some("the directory is not a directory".to_string())
                } else if metadata.uid() != nix::unistd::geteuid().as_raw() {
                    Some("the directory belongs to another user".to_string())
                } else if metadata.mode() & 0o777 != 0o700 {
                    Some(format!(
                        "the directory has mode {:o}, not 700",
                        metadata.mode() & 0o777
                    ))
                } else {
                    self.forbidden_entry().or_else(|| suspect_entry(&self.path))
                };
                match reason {
                    None => return Ok(Ensured::Reused),
                    Some(reason) => {
                        self.remove()?;
                        Some(reason)
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                // Not even its status could be read: suspect, and removal must succeed.
                self.remove()?;
                Some(format!("the directory could not be inspected: {error}"))
            }
        };
        private_directory(&self.path)?;
        Ok(found.map_or(Ensured::Created, Ensured::Discarded))
    }

    /// The reason this directory must not be reused, judged exactly as [`Self::ensure`] judges
    /// it but without removing anything: after a check, a directory that grew a link, a special
    /// file, a forbidden name or a root that is no longer a private real directory is suspect,
    /// and the caller removes it under its lock before accepting the check.
    pub fn inspect(&self) -> Option<String> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => {
                let kind = metadata.file_type();
                if kind.is_symlink() {
                    Some("the directory is a link".to_string())
                } else if !metadata.is_dir() {
                    Some("the directory is not a directory".to_string())
                } else if metadata.uid() != nix::unistd::geteuid().as_raw() {
                    Some("the directory belongs to another user".to_string())
                } else if metadata.mode() & 0o777 != 0o700 {
                    Some(format!(
                        "the directory has mode {:o}, not 700",
                        metadata.mode() & 0o777
                    ))
                } else {
                    self.forbidden_entry().or_else(|| suspect_entry(&self.path))
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Some("the directory is gone".to_string())
            }
            Err(error) => Some(format!("the directory could not be inspected: {error}")),
        }
    }

    fn forbidden_entry(&self) -> Option<String> {
        self.forbidden.iter().find_map(|name| {
            let path = self.path.join(name);
            std::fs::symlink_metadata(&path)
                .is_ok()
                .then(|| format!("a forbidden entry at {}", path.display()))
        })
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

/// The first entry below `root` that a check must not build on, walked descriptor-relative
/// without following links: a symbolic link (its target lies outside the keyed, bounded
/// directory), anything that is neither a regular file nor a directory, an entry another user
/// owns, or any entry the walk could not inspect. An unreadable subtree is never skipped: what
/// the kernel cannot see it cannot bound, so it is suspect. `None` when every entry is a real
/// file or directory of this user.
pub fn suspect_entry(root: &Path) -> Option<String> {
    let me = nix::unistd::geteuid().as_raw();
    let mut suspect = None;
    let walked = walk(root, &mut |path, stat| {
        let kind = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT;
        let reason = if kind == SFlag::S_IFLNK {
            Some("a link")
        } else if kind != SFlag::S_IFDIR && kind != SFlag::S_IFREG {
            Some("a special file")
        } else if stat.st_uid != me {
            Some("another user's entry")
        } else {
            None
        };
        match reason {
            Some(reason) => {
                suspect = Some(format!("{reason} at {}", path.display()));
                false
            }
            None => true,
        }
    });
    match walked {
        Ok(()) => suspect,
        Err(uninspectable) => Some(uninspectable.detail),
    }
}

/// A byte count that could not see every entry: `counted` is what it saw before the first
/// entry it could not inspect, a lower bound and never the directory's size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uninspectable {
    pub counted: u64,
    pub detail: String,
}

impl std::fmt::Display for Uninspectable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

/// The apparent size of every entry below `path`, walked descriptor-relative without following
/// links. Only an entry that disappears during the walk — a build is running — is skipped, and
/// an absent path is zero bytes. Any other inspection failure fails closed: the count is
/// `Uninspectable`, which a caller treats as a directory above every bound.
pub fn directory_bytes(path: &Path) -> Result<u64, Uninspectable> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(Uninspectable {
                counted: 0,
                detail: format!("inspecting {}: {error}", path.display()),
            });
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        // A link or a file where the directory should be is not a count of anything the kernel
        // bounds: a check that swapped its root for a link wrote through it, elsewhere.
        return Err(Uninspectable {
            counted: 0,
            detail: format!("{} is not a real directory", path.display()),
        });
    }
    let mut total = 0_u64;
    walk(path, &mut |_, stat| {
        if SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT != SFlag::S_IFDIR {
            total = total.saturating_add(u64::try_from(stat.st_size).unwrap_or(0));
        }
        true
    })
    .map_err(|mut uninspectable| {
        uninspectable.counted = total;
        uninspectable
    })?;
    Ok(total)
}

/// Visit every entry below `root` with its no-follow status, until `visit` returns `false`.
/// Each directory is opened relative to its parent's descriptor with `O_NOFOLLOW`, and each
/// entry is inspected with `fstatat(AT_SYMLINK_NOFOLLOW)`, so a path swapped for a link during
/// the walk is never followed. An entry that vanished is skipped; any other failure — an
/// unreadable directory, a failed read or status — is returned rather than skipped. An absent
/// root is an empty walk.
fn walk(
    root: &Path,
    visit: &mut dyn FnMut(&Path, &nix::sys::stat::FileStat) -> bool,
) -> Result<(), Uninspectable> {
    let failed = |path: &Path, errno: Errno| Uninspectable {
        counted: 0,
        detail: format!("an uninspectable entry at {}: {errno}", path.display()),
    };
    let opened = match nix::fcntl::openat(
        nix::fcntl::AT_FDCWD,
        root,
        directory_flags(),
        nix::sys::stat::Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(Errno::ENOENT) => return Ok(()),
        Err(errno) => return Err(failed(root, errno)),
    };
    // Subdirectories wait beside their parent's descriptor and are opened only when walked,
    // so the open descriptors stay near the tree's depth however wide a level is.
    let mut next = Some((opened, root.to_path_buf()));
    let mut pending: Vec<(Rc<Dir>, std::ffi::CString, PathBuf)> = Vec::new();
    loop {
        let (descriptor, path) = match next.take() {
            Some(opened) => opened,
            None => {
                let Some((parent, name, path)) = pending.pop() else {
                    return Ok(());
                };
                match nix::fcntl::openat(
                    &*parent,
                    name.as_c_str(),
                    directory_flags(),
                    nix::sys::stat::Mode::empty(),
                ) {
                    Ok(descriptor) => (descriptor, path),
                    Err(Errno::ENOENT) => continue,
                    Err(errno) => return Err(failed(&path, errno)),
                }
            }
        };
        let mut directory = Dir::from_fd(descriptor).map_err(|errno| failed(&path, errno))?;
        let mut names = Vec::new();
        for entry in directory.iter() {
            let entry = entry.map_err(|errno| failed(&path, errno))?;
            let name = entry.file_name();
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                names.push(name.to_owned());
            }
        }
        let directory = Rc::new(directory);
        for name in names {
            let child = path.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
            let stat = match nix::sys::stat::fstatat(
                &*directory,
                name.as_c_str(),
                nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => stat,
                Err(Errno::ENOENT) => continue,
                Err(errno) => return Err(failed(&child, errno)),
            };
            if !visit(&child, &stat) {
                return Ok(());
            }
            if SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR {
                pending.push((Rc::clone(&directory), name, child));
            }
        }
    }
}

fn directory_flags() -> nix::fcntl::OFlag {
    nix::fcntl::OFlag::O_RDONLY
        | nix::fcntl::OFlag::O_DIRECTORY
        | nix::fcntl::OFlag::O_NOFOLLOW
        | nix::fcntl::OFlag::O_CLOEXEC
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
        assert_eq!(directory.bytes(), Ok(0));
        assert_eq!(directory.ensure(), Ok(Ensured::Created));
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
        assert_eq!(directory.bytes(), Ok(100 + "/etc/hosts".len() as u64));
        directory.remove().unwrap();
        assert!(!directory.path().exists());
        assert_eq!(directory.bytes(), Ok(0));
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

    fn held(root: &Path, kind: &str) -> WarmDirectory {
        let TaskBuildCacheLock::Held(directory) =
            lock_task_build_cache(root, &key('a'), &key('b'), kind, Duration::ZERO).unwrap()
        else {
            panic!("an uncontended lock is held");
        };
        directory
    }

    /// Running as root reads through mode 000, so the unreadable fixtures cannot be built.
    fn permissions_bind() -> bool {
        !nix::unistd::geteuid().is_root()
    }

    #[test]
    fn ensure_says_whether_it_reused_created_or_discarded_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = held(root.path(), "cargo_target");
        assert_eq!(directory.ensure(), Ok(Ensured::Created));
        std::fs::write(directory.path().join("out"), [0_u8; 16]).unwrap();
        assert_eq!(directory.ensure(), Ok(Ensured::Reused));
        assert_eq!(directory.bytes(), Ok(16));
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let Ok(Ensured::Discarded(reason)) = directory.ensure() else {
            panic!("a widened directory is discarded");
        };
        assert!(reason.contains("mode 755"), "{reason}");
        assert_eq!(directory.bytes(), Ok(0), "recreated empty");
    }

    #[test]
    fn an_unreadable_subtree_fails_the_count_and_makes_the_directory_suspect() {
        if !permissions_bind() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let directory = held(root.path(), "cargo_target");
        directory.ensure().unwrap();
        let hidden = directory.path().join("hidden");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("big"), vec![0_u8; 8192]).unwrap();
        std::fs::write(directory.path().join("seen"), [0_u8; 3]).unwrap();
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();

        let error = directory.bytes().unwrap_err();
        assert!(error.detail.contains("hidden"), "{error}");
        assert!(
            error.counted <= 3,
            "a lower bound, never the size: {error:?}"
        );
        assert!(
            suspect_entry(directory.path()).is_some_and(|reason| reason.contains("hidden")),
            "an uninspectable subtree is never skipped"
        );
        let Ok(Ensured::Discarded(reason)) = directory.ensure() else {
            panic!("an uninspectable directory is discarded before reuse");
        };
        assert!(reason.contains("uninspectable"), "{reason}");
        assert!(!hidden.exists());
        assert_eq!(directory.bytes(), Ok(0));
    }

    #[test]
    fn a_forbidden_root_entry_discards_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = held(root.path(), "cargo_home").forbidding(&["credentials.toml"]);
        directory.ensure().unwrap();
        std::fs::create_dir(directory.path().join("registry")).unwrap();
        std::fs::write(directory.path().join("registry/index"), b"i").unwrap();
        assert_eq!(directory.ensure(), Ok(Ensured::Reused));
        std::fs::write(directory.path().join("credentials.toml"), b"token = \"x\"").unwrap();
        let Ok(Ensured::Discarded(reason)) = directory.ensure() else {
            panic!("a credential makes the directory suspect");
        };
        assert!(reason.contains("credentials.toml"), "{reason}");
        assert!(!directory.path().join("registry").exists());
        assert!(!directory.path().join("credentials.toml").exists());
    }

    #[test]
    fn a_wide_directory_is_counted_without_holding_a_descriptor_per_entry() {
        let root = tempfile::tempdir().unwrap();
        let directory = held(root.path(), "cargo_target");
        directory.ensure().unwrap();
        for index in 0..2_000 {
            let child = directory.path().join(format!("d{index}"));
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("f"), [0_u8; 2]).unwrap();
        }
        assert_eq!(directory.bytes(), Ok(4_000));
        assert_eq!(suspect_entry(directory.path()), None);
    }

    #[test]
    fn the_key_lock_serializes_every_kind_of_one_toolchain() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let held = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .expect("an uncontended key lock is held");
        assert!(
            lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
                .unwrap()
                .is_none(),
            "a second holder of the same key is busy"
        );
        assert!(
            lock_task_build_cache_key(&cache, &key('a'), &key('c'), Duration::ZERO)
                .unwrap()
                .is_some(),
            "another toolchain key is independent"
        );
        drop(held);
        assert!(
            lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_link_where_the_warm_root_should_be_is_uninspectable_not_small() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("big"), [1_u8; 4096]).unwrap();
        let link = root.path().join("cargo_target");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(
            directory_bytes(&link).is_err(),
            "a link root cannot be bounded"
        );
        std::fs::write(root.path().join("file"), b"x").unwrap();
        assert!(directory_bytes(&root.path().join("file")).is_err());
        assert_eq!(directory_bytes(&root.path().join("absent")).unwrap(), 0);
    }
}
