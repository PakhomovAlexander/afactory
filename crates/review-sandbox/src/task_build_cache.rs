//! The Warm Check Cache's host directory (ADR-0131): one machine-local build directory per
//! project, toolchain and kind, below `$XDG_CACHE_HOME/af/task-build-cache`.
//!
//! This is explicitly unsafe, candidate-built state, and nothing here pretends otherwise. What
//! this module owns is the directory's shape: private `0700` directories created and checked
//! without following links, one exclusive advisory lock over the whole toolchain key and one per
//! kind, held for as long as a check may write, a no-follow byte count, and removal. Every
//! operation below the toolchain key goes through the key directory's open descriptor — `mkdirat`,
//! `openat(O_NOFOLLOW)`, `fstatat(AT_SYMLINK_NOFOLLOW)`, `unlinkat` — never through a path a
//! check could have swapped for a link while it ran: a check that renames the key's parent and
//! plants a link in its place changes nothing the kernel resolves. The cache is never below a
//! sandbox root, so no seal, candidate capture or delivery can reach it; only the kernel's Task
//! check runner opens it, and only while it holds the lock.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::dir::Dir;
use nix::errno::Errno;
use nix::fcntl::{AtFlags, Flock, FlockArg, OFlag};
use nix::sys::stat::{FchmodatFlags, FileStat, Mode, SFlag};
use nix::sys::time::TimeSpec;
use nix::unistd::UnlinkatFlags;

/// The fixed directory name below `$XDG_CACHE_HOME/af`.
pub const TASK_BUILD_CACHE_DIRECTORY: &str = "task-build-cache";

/// The lock file of the whole toolchain key, beside the kind directories. Its modification
/// time is the key's last use: every acquisition touches it.
pub const KEY_LOCK: &str = "warm.lock";

/// The suffix of a key's size record, `<project>/<toolchain>.size`, beside the key and never
/// below it, where every entry is the key's to count and remove.
const SIZE_SUFFIX: &str = ".size";

/// The closed set of warm kinds a key may hold. Only these names are ever locked as kinds, and
/// only their lock files and the key's are the kernel's: anything else below a key — whatever a
/// check put there, however it is named — is a kind entry to count and remove.
pub const WARM_KINDS: &[&str] = &["cargo_target", "cargo_home"];

/// The lock files this process holds below one key, each by the name it was opened under and
/// the inode it holds open. Only the top-level entry at that exact name holding that exact inode
/// is the kernel's: a file a check put at a lock's name after unlinking it is not, a hard link
/// to the lock's inode somewhere else is not, and both are counted and removed like anything
/// else. A held lock is truncated on acquisition, so any byte it holds later is a check's.
#[derive(Debug, Default)]
struct HeldLocks(Mutex<Vec<(CString, u64, u64)>>);

impl HeldLocks {
    fn register(&self, name: &str, lock: &Flock<File>) -> Result<(), String> {
        let stat = nix::sys::stat::fstat(&**lock)
            .map_err(|errno| format!("inspecting a Task build cache lock: {errno}"))?;
        let (dev, ino) = inode(&stat);
        let name = CString::new(name).map_err(|_| "a lock name holds a NUL byte".to_string())?;
        self.0
            .lock()
            .expect("held lock registry")
            .push((name, dev, ino));
        Ok(())
    }

    /// Whether the top-level entry `name` with status `stat` is one of this holder's locks: the
    /// same name and the same inode, nothing less.
    fn holds_entry(&self, name: &CStr, stat: &FileStat) -> bool {
        let identity = inode(stat);
        self.0
            .lock()
            .expect("held lock registry")
            .iter()
            .any(|(held, dev, ino)| held.as_c_str() == name && (*dev, *ino) == identity)
    }

    fn names(&self) -> Vec<(CString, u64, u64)> {
        self.0.lock().expect("held lock registry").clone()
    }
}

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

/// One locked warm directory: a kind below one toolchain key. The lock is released when this
/// value is dropped, so a caller that must remove the directory before release does so while it
/// still holds this value. Every operation goes through the key's descriptor and the kind's
/// name; the path is kept for display and for the check's environment only.
#[derive(Debug)]
pub struct WarmDirectory {
    key: Arc<OwnedFd>,
    name: String,
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

/// The exclusive lock over one whole toolchain key, `<root>/<project>/<toolchain>/warm.lock`,
/// with the key directory held open. A check holds it from preparation to the end of its
/// removal step, so every warm operation of one project and toolchain is serialized whatever
/// kinds each check declares, and the key's shared bound is measured and enforced over every
/// kind directory below it, held by this check or left by another.
#[derive(Debug)]
pub struct TaskBuildCacheKeyLock {
    key: Arc<OwnedFd>,
    /// The project level the key sits in, held open so the key can be checked to still occupy
    /// its name: a check that renames the key and recreates a directory at its path has moved
    /// the kernel's directory, not replaced it, and the kernel notices before accepting.
    project: OwnedFd,
    toolchain: String,
    path: PathBuf,
    held: Arc<HeldLocks>,
    lock: Flock<File>,
}

/// Lock the toolchain key `<root>/<project>/<toolchain>` for exclusive use, waiting at most
/// `wait`. `None` when another holder kept it for the whole wait. The three levels below the
/// root are created private and opened without following links.
pub fn lock_task_build_cache_key(
    root: &Path,
    project: &str,
    toolchain: &str,
    wait: Duration,
) -> Result<Option<TaskBuildCacheKeyLock>, String> {
    let (project_fd, key, path, private) = open_key(root, project, toolchain)?;
    let Some(lock) = exclusive_lock_at(&key, KEY_LOCK, wait)? else {
        return Ok(None);
    };
    // The lock's modification time is the key's last use, read by the Storage Budget's sweep.
    if let Err(errno) = nix::sys::stat::futimens(&*lock, &TimeSpec::UTIME_NOW, &TimeSpec::UTIME_NOW)
    {
        eprintln!("warm check cache diagnostic: touching the key lock: {errno}");
    }
    let held = Arc::new(HeldLocks::default());
    held.register(KEY_LOCK, &lock)?;
    let key = TaskBuildCacheKeyLock {
        key,
        project: project_fd,
        toolchain: toolchain.to_string(),
        path,
        held,
        lock,
    };
    if !private {
        // A key that is no longer a private directory is suspect state, not a mode to fix:
        // everything below it goes before anything is reused, and only then is it private again.
        eprintln!(
            "warm check cache diagnostic: toolchain key {} was not private; emptied before reuse",
            key.path.display()
        );
        key.remove_kinds()?;
        nix::sys::stat::fchmod(&*key.key, Mode::S_IRWXU)
            .map_err(|errno| format!("securing a Task build cache directory: {errno}"))?;
    }
    Ok(Some(key))
}

/// Lock `<root>/<project>/<toolchain>/<kind>` for exclusive use without the key lock, waiting
/// at most `wait`. The kernel's check runner takes the key lock first and locks kinds through
/// [`TaskBuildCacheKeyLock::lock_kind`]; this entry point serves callers that own one kind.
pub fn lock_task_build_cache(
    root: &Path,
    project: &str,
    toolchain: &str,
    kind: &str,
    wait: Duration,
) -> Result<TaskBuildCacheLock, String> {
    let (_, key, path, _) = open_key(root, project, toolchain)?;
    lock_kind_at(key, &path, kind, wait, &Arc::new(HeldLocks::default()))
}

impl TaskBuildCacheKeyLock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The device and inode of the key directory this lock holds open.
    pub fn identity(&self) -> std::io::Result<crate::Identity> {
        crate::Identity::of_descriptor(&*self.key)
    }

    /// Lock one kind below this key, waiting at most `wait`.
    pub fn lock_kind(&self, kind: &str, wait: Duration) -> Result<TaskBuildCacheLock, String> {
        lock_kind_at(Arc::clone(&self.key), &self.path, kind, wait, &self.held)
    }

    /// The reason the key itself must not be trusted right now: it is no longer a private
    /// directory of this user, or a lock file this process holds was unlinked or replaced at its
    /// name. `None` when the key is as the kernel left it.
    pub fn inspect(&self) -> Option<String> {
        match nix::sys::stat::fstatat(
            &self.project,
            self.toolchain.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(at_name) => match nix::sys::stat::fstat(&*self.key) {
                Ok(held) if inode(&at_name) == inode(&held) => {}
                Ok(_) => return Some("the toolchain key was displaced from its name".into()),
                Err(errno) => {
                    return Some(format!("the toolchain key could not be inspected: {errno}"));
                }
            },
            Err(Errno::ENOENT) => return Some("the toolchain key is gone from its name".into()),
            Err(errno) => return Some(format!("the toolchain key's name: {errno}")),
        }
        match nix::sys::stat::fstat(&*self.key) {
            Ok(stat) if stat.st_uid != nix::unistd::geteuid().as_raw() => {
                return Some("the toolchain key belongs to another user".into());
            }
            Ok(stat) if stat.st_mode & 0o777 != 0o700 => {
                return Some(format!(
                    "the toolchain key has mode {:o}, not 700",
                    stat.st_mode & 0o777
                ));
            }
            Ok(_) => {}
            Err(errno) => {
                return Some(format!("the toolchain key could not be inspected: {errno}"));
            }
        }
        for (name, dev, ino) in self.held.names() {
            let shown = name.to_string_lossy();
            match nix::sys::stat::fstatat(&*self.key, name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
            {
                Ok(stat) if inode(&stat) != (dev, ino) => {
                    return Some(format!("the lock file `{shown}` was replaced"));
                }
                Ok(stat) if stat.st_nlink != 1 => {
                    return Some(format!("the lock file `{shown}` is linked elsewhere"));
                }
                Ok(stat) if stat.st_size != 0 => {
                    return Some(format!("the lock file `{shown}` grew"));
                }
                Ok(_) => {}
                Err(Errno::ENOENT) => {
                    return Some(format!("the lock file `{shown}` was removed"));
                }
                Err(errno) => return Some(format!("the lock file `{shown}`: {errno}")),
            }
        }
        None
    }

    /// Bytes below the whole key — every kind directory, whoever left it, and the lock files —
    /// counted without following links. This holder's own locks are left out only while they
    /// are the empty, singly linked files the kernel made; one a check grew or linked counts
    /// like anything else, so growing it cannot slip past the bound while the check runs. A key
    /// the count cannot fully inspect is `Uninspectable`, never smaller.
    pub fn bytes(&self) -> Result<u64, Uninspectable> {
        let descriptor = nix::unistd::dup(&*self.key).map_err(|errno| Uninspectable {
            counted: 0,
            detail: format!(
                "reopening the toolchain key {}: {errno}",
                self.path.display()
            ),
        })?;
        let held = Arc::clone(&self.held);
        let key = self.path.clone();
        count_skipping(descriptor, &self.path, &move |path, stat| {
            path.parent() == Some(key.as_path())
                && path
                    .file_name()
                    .and_then(|name| CString::new(name.as_bytes()).ok())
                    .is_some_and(|name| held.holds_entry(&name, stat))
                && stat.st_size == 0
                && stat.st_nlink == 1
        })
    }

    /// The kind entries currently below the key, by name, sorted: every entry that is not one
    /// of this holder's lock files, whatever a check made of it and however it is named. Names
    /// that are not UTF-8 are shown lossily here; removal addresses them by their exact bytes.
    pub fn kinds(&self) -> Result<Vec<String>, String> {
        let mut names: Vec<String> = self
            .entries()?
            .into_iter()
            .map(|(name, _)| {
                OsStr::from_bytes(name.as_bytes())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Every top-level entry below the key that is not one of this holder's locks, with its
    /// no-follow status, by exact name. An entry whose status cannot be read is an error, never
    /// skipped: what cannot be inspected cannot be left behind either.
    fn entries(&self) -> Result<Vec<(CString, FileStat)>, String> {
        let descriptor = nix::unistd::dup(&*self.key)
            .map_err(|errno| format!("reopening the toolchain key: {errno}"))?;
        let mut directory = Dir::from_fd(descriptor)
            .map_err(|errno| format!("reading the toolchain key: {errno}"))?;
        let mut names = Vec::new();
        for entry in directory.iter() {
            let entry = entry.map_err(|errno| format!("reading the toolchain key: {errno}"))?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            names.push(name.to_owned());
        }
        drop(directory);
        let mut entries = Vec::new();
        for name in names {
            let stat = match nix::sys::stat::fstatat(
                &*self.key,
                name.as_c_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => stat,
                Err(Errno::ENOENT) => continue,
                Err(errno) => {
                    return Err(format!(
                        "inspecting `{}` below the toolchain key: {errno}",
                        name.to_string_lossy()
                    ));
                }
            };
            // This holder's locks stay; everything else below the key — a kind directory,
            // whatever a check put where a kind was, a file a check named to look like a lock, a
            // lock file another holder left — is an entry to count and remove.
            if self.held.holds_entry(&name, &stat) {
                continue;
            }
            entries.push((name, stat));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    /// Remove every entry below the key that is not one of this holder's locks, whoever left it
    /// and however it is named, and report each one's bytes before removal (a lower bound when
    /// it could not be fully counted). The key is made writable through its held descriptor
    /// first, so a check that took write permission away cannot keep its leavings; afterwards
    /// nothing but this holder's locks may remain, or the eviction is an error.
    pub fn remove_kinds(&self) -> Result<Vec<(String, u64)>, String> {
        nix::sys::stat::fchmod(&*self.key, Mode::S_IRWXU)
            .map_err(|errno| format!("securing the toolchain key for eviction: {errno}"))?;
        let mut removed = Vec::new();
        for (name, stat) in self.entries()? {
            let display = self.path.join(OsStr::from_bytes(name.as_bytes()));
            let bytes = if file_kind(&stat) == SFlag::S_IFDIR {
                match nix::fcntl::openat(
                    &*self.key,
                    name.as_c_str(),
                    directory_flags(),
                    Mode::empty(),
                ) {
                    Ok(descriptor) => match count(descriptor, &display) {
                        Ok(bytes) => bytes,
                        Err(uninspectable) => uninspectable.counted,
                    },
                    Err(_) => 0,
                }
            } else {
                u64::try_from(stat.st_size).unwrap_or(0)
            };
            remove_c(&self.key, &name, &display)?;
            removed.push((
                OsStr::from_bytes(name.as_bytes())
                    .to_string_lossy()
                    .into_owned(),
                bytes,
            ));
        }
        // The held locks: a sound one — one name, this holder's inode — is emptied through a
        // fresh descriptor to that inode, since any byte in it is a check's; one that was
        // replaced or linked elsewhere is unlinked at its name, never written through.
        for (name, dev, ino) in self.held.names() {
            let shown = name.to_string_lossy().into_owned();
            let stat = match nix::sys::stat::fstatat(
                &*self.key,
                name.as_c_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => stat,
                Err(Errno::ENOENT) => continue,
                Err(errno) => return Err(format!("inspecting the lock `{shown}`: {errno}")),
            };
            let sound = inode(&stat) == (dev, ino)
                && file_kind(&stat) == SFlag::S_IFREG
                && stat.st_nlink == 1
                && stat.st_uid == nix::unistd::geteuid().as_raw();
            if sound {
                if stat.st_size != 0 {
                    let descriptor = nix::fcntl::openat(
                        &*self.key,
                        name.as_c_str(),
                        OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(|errno| format!("reopening the lock `{shown}`: {errno}"))?;
                    let reopened = nix::sys::stat::fstat(&descriptor)
                        .map_err(|errno| format!("inspecting the lock `{shown}`: {errno}"))?;
                    if inode(&reopened) != (dev, ino) {
                        return Err(format!("the lock `{shown}` changed under eviction"));
                    }
                    nix::unistd::ftruncate(&descriptor, 0)
                        .map_err(|errno| format!("emptying the lock `{shown}`: {errno}"))?;
                    removed.push((shown, u64::try_from(stat.st_size).unwrap_or(0)));
                }
            } else {
                match nix::unistd::unlinkat(&*self.key, name.as_c_str(), UnlinkatFlags::NoRemoveDir)
                {
                    Ok(()) | Err(Errno::ENOENT) => {}
                    Err(errno) => return Err(format!("removing the lock `{shown}`: {errno}")),
                }
                removed.push((shown, u64::try_from(stat.st_size).unwrap_or(0)));
            }
        }
        let left = self.entries()?;
        if !left.is_empty() {
            return Err(format!(
                "eviction left {} entr{} below the toolchain key, first `{}`",
                left.len(),
                if left.len() == 1 { "y" } else { "ies" },
                left[0].0.to_string_lossy()
            ));
        }
        for (name, dev, ino) in self.held.names() {
            match nix::sys::stat::fstatat(&*self.key, name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
            {
                Err(Errno::ENOENT) => {}
                Ok(stat)
                    if inode(&stat) == (dev, ino) && stat.st_nlink == 1 && stat.st_size == 0 => {}
                Ok(_) => {
                    return Err(format!(
                        "eviction left the lock `{}` neither absent nor sound and empty",
                        name.to_string_lossy()
                    ));
                }
                Err(errno) => {
                    return Err(format!(
                        "inspecting the lock `{}`: {errno}",
                        name.to_string_lossy()
                    ));
                }
            }
        }
        Ok(removed)
    }

    /// Record `bytes` as the key's measured size, stamped with its lock's current modification
    /// time, so the Storage Budget's sweep reuses the measurement for as long as nothing has
    /// used the key since and measures it again once something has.
    pub fn record_size(&self, bytes: u64) -> Result<(), String> {
        let stat = nix::sys::stat::fstat(&*self.lock)
            .map_err(|errno| format!("inspecting the key lock: {errno}"))?;
        let name = format!("{}{SIZE_SUFFIX}", self.toolchain);
        let descriptor = nix::fcntl::openat(
            &self.project,
            name.as_str(),
            OFlag::O_WRONLY
                | OFlag::O_CREAT
                | OFlag::O_TRUNC
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(|errno| format!("writing the key's size record: {errno}"))?;
        let record = format!("{bytes} {} {}\n", stat.st_mtime, stat.st_mtime_nsec);
        nix::unistd::write(&descriptor, record.as_bytes())
            .map_err(|errno| format!("writing the key's size record: {errno}"))?;
        Ok(())
    }

    /// Evict the whole key: everything below it, its lock files, its size record and the key
    /// directory itself, while this holder still holds the lock, so no check can be using any of
    /// it. Returns the bytes removed below the key. A waiter that held the old lock file open
    /// finds it gone from its name and the key unavailable, and runs cold.
    pub fn remove_key(self) -> Result<u64, String> {
        let removed: u64 = self.remove_kinds()?.iter().map(|(_, bytes)| *bytes).sum();
        for (name, _, _) in self.held.names() {
            match nix::unistd::unlinkat(&*self.key, name.as_c_str(), UnlinkatFlags::NoRemoveDir) {
                Ok(()) | Err(Errno::ENOENT) => {}
                Err(errno) => {
                    return Err(format!(
                        "removing the lock `{}`: {errno}",
                        name.to_string_lossy()
                    ));
                }
            }
        }
        // The key goes through the same claim as every other removal: only the directory this
        // lock holds open is unlinked, never one that took its name since (ADR-0144).
        let identity = crate::Identity::of_descriptor(&*self.key)
            .map_err(|error| format!("inspecting the toolchain key: {error}"))?;
        match crate::removal::remove_tree_at(
            &self.project,
            std::ffi::OsStr::new(self.toolchain.as_str()),
            Some(identity),
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("removing the toolchain key: {error}")),
        }
        let record = format!("{}{SIZE_SUFFIX}", self.toolchain);
        match nix::unistd::unlinkat(&self.project, record.as_str(), UnlinkatFlags::NoRemoveDir) {
            Ok(()) | Err(Errno::ENOENT) => {}
            Err(errno) => return Err(format!("removing the key's size record: {errno}")),
        }
        Ok(removed)
    }
}

/// The size recorded for `<root>/<project>/<toolchain>` by [`TaskBuildCacheKeyLock::record_size`],
/// when it is still current: its stamp equals the key lock's modification time now.
pub fn recorded_key_size(root: &Path, project: &str, toolchain: &str) -> Option<u64> {
    let key = root.join(project).join(toolchain);
    let lock = std::fs::symlink_metadata(key.join(KEY_LOCK)).ok()?;
    let record =
        std::fs::read_to_string(root.join(project).join(format!("{toolchain}{SIZE_SUFFIX}")))
            .ok()?;
    let mut fields = record.split_whitespace();
    let bytes = fields.next()?.parse().ok()?;
    let seconds: i64 = fields.next()?.parse().ok()?;
    let nanos: i64 = fields.next()?.parse().ok()?;
    use std::os::unix::fs::MetadataExt;
    (lock.mtime() == seconds && lock.mtime_nsec() == nanos).then_some(bytes)
}

/// Whether `name` is a key's size record below a project directory.
pub fn is_size_record(name: &str) -> bool {
    name.strip_suffix(SIZE_SUFFIX).is_some_and(is_key)
}

impl WarmDirectory {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The kind this directory holds.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Treat any of `names` directly below the directory as suspect, so [`Self::ensure`] and
    /// [`Self::inspect`] refuse the whole directory rather than let a check reuse it.
    pub fn forbidding(mut self, names: &'static [&'static str]) -> Self {
        self.forbidden = names;
        self
    }

    /// Whether something other than a real directory — a link, a file — sits at the kind's name
    /// right now. An absent name is not swapped: there is nothing there to count, and the
    /// inspection after the check reports it as gone.
    pub fn is_swapped(&self) -> bool {
        matches!(
            nix::sys::stat::fstatat(&*self.key, self.name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW),
            Ok(stat) if file_kind(&stat) != SFlag::S_IFDIR
        )
    }

    /// Bytes currently below the directory, counted without following links; zero when it is
    /// absent. A directory the count cannot fully inspect, or a link or file where the
    /// directory should be, is `Uninspectable`, never smaller.
    pub fn bytes(&self) -> Result<u64, Uninspectable> {
        count_child(&self.key, &self.name, &self.path)
    }

    /// Make the directory a private real directory owned by this user, holding only real files
    /// and directories of this user. Anything else at the name — a link, a file, another
    /// owner's or a widened directory — or anywhere below it — a link, a special file, another
    /// owner's entry, an entry the walk cannot inspect, a forbidden name — is suspect: the whole
    /// directory is removed first, never repaired in place, so a check can never reuse content
    /// that lives outside the keyed, bounded cache. The result says which happened, so a caller
    /// never reports a discarded directory's old bytes as available.
    pub fn ensure(&self) -> Result<Ensured, String> {
        let found = match self.judge() {
            Judged::Sound => return Ok(Ensured::Reused),
            Judged::Absent => None,
            Judged::Suspect(reason) => {
                self.remove()?;
                Some(reason)
            }
        };
        create_private(&self.key, &self.name)?;
        Ok(found.map_or(Ensured::Created, Ensured::Discarded))
    }

    /// The reason this directory must not be reused, judged exactly as [`Self::ensure`] judges
    /// it but without removing anything: after a check, a directory that grew a link, a special
    /// file, a forbidden name or a root that is no longer a private real directory is suspect,
    /// and the caller removes it under its lock before accepting the check.
    pub fn inspect(&self) -> Option<String> {
        match self.judge() {
            Judged::Sound => None,
            Judged::Absent => Some("the directory is gone".to_string()),
            Judged::Suspect(reason) => Some(reason),
        }
    }

    fn judge(&self) -> Judged {
        let me = nix::unistd::geteuid().as_raw();
        let stat = match nix::sys::stat::fstatat(
            &*self.key,
            self.name.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(Errno::ENOENT) => return Judged::Absent,
            Err(errno) => {
                return Judged::Suspect(format!("the directory could not be inspected: {errno}"));
            }
        };
        let kind = file_kind(&stat);
        if kind == SFlag::S_IFLNK {
            return Judged::Suspect("the directory is a link".into());
        }
        if kind != SFlag::S_IFDIR {
            return Judged::Suspect("the directory is not a directory".into());
        }
        if stat.st_uid != me {
            return Judged::Suspect("the directory belongs to another user".into());
        }
        if stat.st_mode & 0o777 != 0o700 {
            return Judged::Suspect(format!(
                "the directory has mode {:o}, not 700",
                stat.st_mode & 0o777
            ));
        }
        let child = match open_child(&self.key, &self.name) {
            Ok(Some(child)) => child,
            Ok(None) => return Judged::Absent,
            Err(detail) => return Judged::Suspect(detail),
        };
        for forbidden in self.forbidden {
            if nix::sys::stat::fstatat(&child, *forbidden, AtFlags::AT_SYMLINK_NOFOLLOW).is_ok() {
                return Judged::Suspect(format!(
                    "a forbidden entry at {}",
                    self.path.join(forbidden).display()
                ));
            }
        }
        match suspect(child, &self.path) {
            None => Judged::Sound,
            Some(reason) => Judged::Suspect(reason),
        }
    }

    /// Remove the directory and everything below it, through the key's descriptor and never
    /// through a path. Absence is success.
    pub fn remove(&self) -> Result<(), String> {
        remove_at(&self.key, &self.name, &self.path)
    }
}

enum Judged {
    Sound,
    Absent,
    Suspect(String),
}

/// Open `<root>/<project>/<toolchain>` level by level, creating each level private and refusing
/// a link at any of them. The root itself is the kernel's own cache directory, resolved by
/// path once; everything below is reached through descriptors.
fn open_key(
    root: &Path,
    project: &str,
    toolchain: &str,
) -> Result<(OwnedFd, Arc<OwnedFd>, PathBuf, bool), String> {
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
    let root_fd = open_root(root)?;
    let (project_fd, _) = open_level(&root_fd, project, true)?;
    // The key level is judged, not repaired: a widened key is emptied under its lock first.
    let (key, private) = open_level(&project_fd, toolchain, false)?;
    Ok((
        project_fd,
        Arc::new(key),
        root.join(project).join(toolchain),
        private,
    ))
}

/// The root level, by path: created private when absent, refused when it is a link or not a
/// directory, secured to `0700` when it is wider.
fn open_root(root: &Path) -> Result<OwnedFd, String> {
    match nix::sys::stat::lstat(root) {
        Ok(stat) if file_kind(&stat) == SFlag::S_IFDIR => {}
        Ok(_) => return Err("a Task build cache path is not a real directory".into()),
        Err(Errno::ENOENT) => match nix::unistd::mkdir(root, Mode::S_IRWXU) {
            Ok(()) | Err(Errno::EEXIST) => {}
            Err(errno) => return Err(format!("creating a Task build cache directory: {errno}")),
        },
        Err(errno) => return Err(format!("inspecting a Task build cache directory: {errno}")),
    }
    let descriptor =
        nix::fcntl::openat(nix::fcntl::AT_FDCWD, root, directory_flags(), Mode::empty())
            .map_err(|errno| format!("opening the Task build cache root: {errno}"))?;
    secure_level(&descriptor)?;
    Ok(descriptor)
}

/// One level below an open level: created private when absent, opened without following links
/// (a link or a file where the level should be is refused), owner checked on the open
/// descriptor. With `repair`, a wider mode is set back to `0700`; without it the caller learns
/// whether the level was private and decides what to discard first.
fn open_level(parent: &OwnedFd, name: &str, repair: bool) -> Result<(OwnedFd, bool), String> {
    match nix::sys::stat::mkdirat(parent, name, Mode::S_IRWXU) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(errno) => return Err(format!("creating a Task build cache directory: {errno}")),
    }
    let descriptor =
        nix::fcntl::openat(parent, name, directory_flags(), Mode::empty()).map_err(|errno| {
            match errno {
                Errno::ELOOP | Errno::ENOTDIR => {
                    "a Task build cache path is not a real directory".to_string()
                }
                errno => format!("opening a Task build cache directory: {errno}"),
            }
        })?;
    let stat = nix::sys::stat::fstat(&descriptor)
        .map_err(|errno| format!("inspecting a Task build cache directory: {errno}"))?;
    if stat.st_uid != nix::unistd::geteuid().as_raw() {
        return Err("a Task build cache directory belongs to another user".into());
    }
    let private = stat.st_mode & 0o777 == 0o700;
    if !private && repair {
        nix::sys::stat::fchmod(&descriptor, Mode::S_IRWXU)
            .map_err(|errno| format!("securing a Task build cache directory: {errno}"))?;
    }
    Ok((descriptor, private))
}

fn secure_level(descriptor: &OwnedFd) -> Result<(), String> {
    let stat = nix::sys::stat::fstat(descriptor)
        .map_err(|errno| format!("inspecting a Task build cache directory: {errno}"))?;
    if stat.st_uid != nix::unistd::geteuid().as_raw() {
        return Err("a Task build cache directory belongs to another user".into());
    }
    if stat.st_mode & 0o777 != 0o700 {
        nix::sys::stat::fchmod(descriptor, Mode::S_IRWXU)
            .map_err(|errno| format!("securing a Task build cache directory: {errno}"))?;
    }
    Ok(())
}

/// Create the kind directory below the key, private, when it is absent.
fn create_private(key: &OwnedFd, name: &str) -> Result<(), String> {
    match nix::sys::stat::mkdirat(key, name, Mode::S_IRWXU) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(errno) => return Err(format!("creating a Task build cache directory: {errno}")),
    }
    nix::sys::stat::fchmodat(key, name, Mode::S_IRWXU, FchmodatFlags::NoFollowSymlink)
        .map_err(|errno| format!("securing a Task build cache directory: {errno}"))
}

fn lock_kind_at(
    key: Arc<OwnedFd>,
    key_path: &Path,
    kind: &str,
    wait: Duration,
    held: &Arc<HeldLocks>,
) -> Result<TaskBuildCacheLock, String> {
    if !is_kind(kind) || !WARM_KINDS.contains(&kind) {
        return Err("Task build cache key is not an opaque identity".into());
    }
    let name = format!("{kind}.lock");
    match exclusive_lock_at(&key, &name, wait)? {
        Some(lock) => {
            held.register(&name, &lock)?;
            Ok(TaskBuildCacheLock::Held(WarmDirectory {
                path: key_path.join(kind),
                name: kind.to_string(),
                key,
                forbidden: &[],
                _lock: lock,
            }))
        }
        None => Ok(TaskBuildCacheLock::Busy),
    }
}

/// Open `name` below `directory` without following links and take its exclusive advisory
/// lock, retrying a held lock until `wait` has elapsed.
fn exclusive_lock_at(
    directory: &OwnedFd,
    name: &str,
    wait: Duration,
) -> Result<Option<Flock<File>>, String> {
    exclusive_lock_validated(directory, name, wait, true)
}

/// [`exclusive_lock_at`], refusing to write through an inode that is not a plain, singly linked
/// lock file of this user at that name: a hard link a check planted at the name would otherwise be
/// truncated, and its other name may lie anywhere. Such an entry is unlinked at the name and the
/// acquisition retried once on a fresh file.
fn exclusive_lock_validated(
    directory: &OwnedFd,
    name: &str,
    wait: Duration,
    retry: bool,
) -> Result<Option<Flock<File>>, String> {
    // Whatever sits at the lock's name that is not a plain file — a link an interrupted check
    // left, a directory — is removed by name before the name is opened, so recovery never stalls
    // on `O_NOFOLLOW` refusing it and never writes through it.
    match nix::sys::stat::fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) if file_kind(&stat) == SFlag::S_IFREG => {}
        Ok(stat) if file_kind(&stat) == SFlag::S_IFDIR => {
            eprintln!(
                "warm check cache diagnostic: lock `{name}` was a directory; removed before use"
            );
            remove_at(directory, name, Path::new(name))?;
        }
        Ok(_) => {
            eprintln!(
                "warm check cache diagnostic: lock `{name}` was not a plain file; removed before use"
            );
            match nix::unistd::unlinkat(directory, name, UnlinkatFlags::NoRemoveDir) {
                Ok(()) | Err(Errno::ENOENT) => {}
                Err(errno) => {
                    return Err(format!("removing a suspect Task build cache lock: {errno}"));
                }
            }
        }
        Err(Errno::ENOENT) => {}
        Err(errno) => return Err(format!("inspecting the Task build cache lock: {errno}")),
    }
    let descriptor = nix::fcntl::openat(
        directory,
        name,
        OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )
    .map_err(|errno| format!("opening the Task build cache lock: {errno}"))?;
    let opened = nix::sys::stat::fstat(&descriptor)
        .map_err(|errno| format!("inspecting the Task build cache lock: {errno}"))?;
    let at_name = nix::sys::stat::fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(|errno| format!("inspecting the Task build cache lock: {errno}"))?;
    let sound = inode(&opened) == inode(&at_name)
        && file_kind(&opened) == SFlag::S_IFREG
        && opened.st_nlink == 1
        && opened.st_uid == nix::unistd::geteuid().as_raw();
    if !sound {
        drop(descriptor);
        if !retry {
            return Err("the Task build cache lock is not a plain lock file of this user".into());
        }
        eprintln!(
            "warm check cache diagnostic: lock `{name}` was not a plain lock file; replaced without writing through it"
        );
        match nix::unistd::unlinkat(directory, name, UnlinkatFlags::NoRemoveDir) {
            Ok(()) | Err(Errno::ENOENT) => {}
            Err(errno) => return Err(format!("removing a suspect Task build cache lock: {errno}")),
        }
        return exclusive_lock_validated(directory, name, wait, false);
    }
    let mut file = File::from(descriptor);
    let started = Instant::now();
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => {
                // The judgement above may be stale by now: a waiter that validated the inode
                // and then waited on another holder's lock can find the inode linked elsewhere
                // by the time it acquires. It is judged again, on the open descriptor and at
                // the name, before anything is written through it.
                let held = nix::sys::stat::fstat(&*lock)
                    .map_err(|errno| format!("inspecting the Task build cache lock: {errno}"))?;
                let at_name =
                    nix::sys::stat::fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW);
                let still_sound = matches!(at_name, Ok(at_name) if inode(&at_name) == inode(&held))
                    && file_kind(&held) == SFlag::S_IFREG
                    && held.st_nlink == 1
                    && held.st_uid == nix::unistd::geteuid().as_raw();
                if !still_sound {
                    drop(lock);
                    if !retry {
                        return Err("the Task build cache lock changed while it was awaited".into());
                    }
                    eprintln!(
                        "warm check cache diagnostic: lock `{name}` changed while awaited; replaced without writing through it"
                    );
                    match nix::unistd::unlinkat(directory, name, UnlinkatFlags::NoRemoveDir) {
                        Ok(()) | Err(Errno::ENOENT) => {}
                        Err(errno) => {
                            return Err(format!(
                                "removing a suspect Task build cache lock: {errno}"
                            ));
                        }
                    }
                    return exclusive_lock_validated(directory, name, wait, false);
                }
                // The kernel's lock files carry no payload: whatever a check wrote into one
                // while it could is dropped here, so it never counts toward the key's bound.
                nix::unistd::ftruncate(&*lock, 0)
                    .map_err(|errno| format!("emptying the Task build cache lock: {errno}"))?;
                return Ok(Some(lock));
            }
            Err((returned, Errno::EWOULDBLOCK)) => {
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

/// The child `name` of `parent` as an open directory descriptor, without following links.
/// `None` when absent; an error names a link, a file or an inspection failure.
fn open_child(parent: &OwnedFd, name: &str) -> Result<Option<OwnedFd>, String> {
    match nix::fcntl::openat(parent, name, directory_flags(), Mode::empty()) {
        Ok(descriptor) => Ok(Some(descriptor)),
        Err(Errno::ENOENT) => Ok(None),
        Err(Errno::ELOOP) => Err("the directory is a link".into()),
        Err(Errno::ENOTDIR) => Err("the directory is not a directory".into()),
        Err(errno) => Err(format!("the directory could not be inspected: {errno}")),
    }
}

/// Bytes below the child `name` of `parent`; zero when absent, `Uninspectable` when it is not
/// a real directory or cannot be fully counted.
fn count_child(parent: &OwnedFd, name: &str, display: &Path) -> Result<u64, Uninspectable> {
    match open_child(parent, name) {
        Ok(Some(descriptor)) => count(descriptor, display),
        Ok(None) => Ok(0),
        Err(detail) => Err(Uninspectable {
            counted: 0,
            detail: format!("{}: {detail}", display.display()),
        }),
    }
}

/// Remove the child `name` of `parent` and everything below it through descriptors: every
/// directory is opened relative to its parent with `O_NOFOLLOW`, every entry is unlinked
/// relative to the directory that holds it, and no path is resolved. A link or a file at the
/// name is unlinked as such. Absence is success.
fn remove_at(parent: &OwnedFd, name: &str, display: &Path) -> Result<(), String> {
    let failed = |errno: Errno| {
        format!(
            "removing the Task build cache {}: {errno}",
            display.display()
        )
    };
    let exact = CString::new(name).map_err(|_| {
        format!(
            "removing the Task build cache {}: the name holds a NUL byte",
            display.display()
        )
    })?;
    remove_c(parent, &exact, display)?;
    match nix::sys::stat::fstatat(parent, exact.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(Errno::ENOENT) => Ok(()),
        Ok(_) => Err(format!(
            "Task build cache removal left {} behind",
            display.display()
        )),
        Err(errno) => Err(failed(errno)),
    }
}

/// Remove the entry `name` below `parent` by its exact bytes, never by a spelling of them: a
/// directory is made the kernel's own again, emptied child by child and unlinked; a link or a
/// file is unlinked as such. Absence is success.
fn remove_c(parent: &OwnedFd, name: &CStr, display: &Path) -> Result<(), String> {
    let failed = |errno: Errno| {
        format!(
            "removing the Task build cache {}: {errno}",
            display.display()
        )
    };
    let stat = match nix::sys::stat::fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::ENOENT) => return Ok(()),
        Err(errno) => return Err(failed(errno)),
    };
    if file_kind(&stat) == SFlag::S_IFDIR {
        // A check may have left a directory it made unreadable; the kernel owns the tree and
        // makes it its own again, without following a link, before looking inside.
        nix::sys::stat::fchmodat(parent, name, Mode::S_IRWXU, FchmodatFlags::NoFollowSymlink)
            .map_err(failed)?;
        let descriptor =
            nix::fcntl::openat(parent, name, directory_flags(), Mode::empty()).map_err(failed)?;
        let names = entry_names(descriptor, display)?;
        let descriptor =
            nix::fcntl::openat(parent, name, directory_flags(), Mode::empty()).map_err(failed)?;
        for child in names {
            remove_c(
                &descriptor,
                &child,
                &display.join(OsStr::from_bytes(child.as_bytes())),
            )?;
        }
        drop(descriptor);
        nix::unistd::unlinkat(parent, name, UnlinkatFlags::RemoveDir)
            .or_else(|errno| {
                if errno == Errno::ENOENT {
                    Ok(())
                } else {
                    Err(errno)
                }
            })
            .map_err(failed)
    } else {
        nix::unistd::unlinkat(parent, name, UnlinkatFlags::NoRemoveDir)
            .or_else(|errno| {
                if errno == Errno::ENOENT {
                    Ok(())
                } else {
                    Err(errno)
                }
            })
            .map_err(failed)
    }
}

/// The entry names of an open directory, consuming its descriptor.
fn entry_names(descriptor: OwnedFd, display: &Path) -> Result<Vec<CString>, String> {
    let mut directory = Dir::from_fd(descriptor).map_err(|errno| {
        format!(
            "reading the Task build cache {}: {errno}",
            display.display()
        )
    })?;
    let mut names = Vec::new();
    for entry in directory.iter() {
        let entry = entry.map_err(|errno| {
            format!(
                "reading the Task build cache {}: {errno}",
                display.display()
            )
        })?;
        let name = entry.file_name();
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// The first entry below `root` that a check must not build on, walked descriptor-relative
/// without following links: a symbolic link (its target lies outside the keyed, bounded
/// directory), anything that is neither a regular file nor a directory, an entry another user
/// owns, or any entry the walk could not inspect. An unreadable subtree is never skipped: what
/// the kernel cannot see it cannot bound, so it is suspect. `None` when every entry is a real
/// file or directory of this user.
pub fn suspect_entry(root: &Path) -> Option<String> {
    match open_path(root) {
        Ok(Some(descriptor)) => suspect(descriptor, root),
        Ok(None) => None,
        Err(uninspectable) => Some(uninspectable.detail),
    }
}

fn suspect(descriptor: OwnedFd, display: &Path) -> Option<String> {
    let me = nix::unistd::geteuid().as_raw();
    let mut found = None;
    let walked = walk_from(descriptor, display, &mut |path, stat| {
        let kind = file_kind(stat);
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
                found = Some(format!("{reason} at {}", path.display()));
                false
            }
            None => true,
        }
    });
    match walked {
        Ok(()) => found,
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
/// `Uninspectable`, which a caller treats as a directory above every bound. So is a link or a
/// file where the directory should be: a check that swapped its root for a link wrote through
/// it, elsewhere.
pub fn directory_bytes(path: &Path) -> Result<u64, Uninspectable> {
    match open_path(path)? {
        Some(descriptor) => count(descriptor, path),
        None => Ok(0),
    }
}

/// Open `path` as a directory without following a link at its last component. `None` when
/// absent; a link or a file there is `Uninspectable`.
fn open_path(path: &Path) -> Result<Option<OwnedFd>, Uninspectable> {
    match nix::fcntl::openat(nix::fcntl::AT_FDCWD, path, directory_flags(), Mode::empty()) {
        Ok(descriptor) => Ok(Some(descriptor)),
        Err(Errno::ENOENT) => Ok(None),
        Err(Errno::ELOOP | Errno::ENOTDIR) => Err(Uninspectable {
            counted: 0,
            detail: format!("{} is not a real directory", path.display()),
        }),
        Err(errno) => Err(Uninspectable {
            counted: 0,
            detail: format!("inspecting {}: {errno}", path.display()),
        }),
    }
}

fn count(descriptor: OwnedFd, display: &Path) -> Result<u64, Uninspectable> {
    count_skipping(descriptor, display, &|_, _| false)
}

/// [`count`], leaving out the entries `skip` names (the lock inodes this process holds below a
/// key).
fn count_skipping(
    descriptor: OwnedFd,
    display: &Path,
    skip: &dyn Fn(&Path, &FileStat) -> bool,
) -> Result<u64, Uninspectable> {
    let mut total = 0_u64;
    walk_from(descriptor, display, &mut |path, stat| {
        if file_kind(stat) != SFlag::S_IFDIR && !skip(path, stat) {
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

/// Visit every entry below an open directory with its no-follow status, until `visit` returns
/// `false`. Each subdirectory is opened relative to its parent's descriptor with `O_NOFOLLOW`,
/// and each entry is inspected with `fstatat(AT_SYMLINK_NOFOLLOW)`, so a path swapped for a
/// link during the walk is never followed. An entry that vanished is skipped; any other failure
/// — an unreadable directory, a failed read or status — is returned rather than skipped.
fn walk_from(
    root: OwnedFd,
    display: &Path,
    visit: &mut dyn FnMut(&Path, &FileStat) -> bool,
) -> Result<(), Uninspectable> {
    let failed = |path: &Path, errno: Errno| Uninspectable {
        counted: 0,
        detail: format!("an uninspectable entry at {}: {errno}", path.display()),
    };
    // Subdirectories wait beside their parent's descriptor and are opened only when walked,
    // so the open descriptors stay near the tree's depth however wide a level is.
    let mut next = Some((root, display.to_path_buf()));
    let mut pending: Vec<(Rc<Dir>, CString, PathBuf)> = Vec::new();
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
                    Mode::empty(),
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
            let child = path.join(OsStr::from_bytes(name.to_bytes()));
            let stat = match nix::sys::stat::fstatat(
                &*directory,
                name.as_c_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => stat,
                Err(Errno::ENOENT) => continue,
                Err(errno) => return Err(failed(&child, errno)),
            };
            if !visit(&child, &stat) {
                return Ok(());
            }
            if file_kind(&stat) == SFlag::S_IFDIR {
                pending.push((Rc::clone(&directory), name, child));
            }
        }
    }
}

/// The device and inode numbers of a status, in one width on every platform: `st_dev` and
/// `st_ino` are narrower on some targets and already `u64` on others, so the conversion is
/// identity on one platform and a widening on another.
#[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
fn inode(stat: &FileStat) -> (u64, u64) {
    (
        u64::try_from(stat.st_dev).unwrap_or_default(),
        u64::try_from(stat.st_ino).unwrap_or_default(),
    )
}

fn file_kind(stat: &FileStat) -> SFlag {
    SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT
}

fn directory_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

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

    #[test]
    fn removal_never_follows_a_swapped_parent_out_of_the_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let directory = held(&cache, "cargo_target");
        directory.ensure().unwrap();
        std::fs::write(directory.path().join("built"), [1_u8; 128]).unwrap();
        // A check renames the toolchain key and plants a link to a decoy in its place.
        let key_path = directory.path().parent().unwrap().to_path_buf();
        let moved = key_path.with_file_name("moved");
        std::fs::rename(&key_path, &moved).unwrap();
        let decoy = root.path().join("decoy");
        std::fs::create_dir_all(decoy.join("cargo_target")).unwrap();
        std::fs::write(decoy.join("cargo_target").join("precious"), b"keep me").unwrap();
        std::os::unix::fs::symlink(&decoy, &key_path).unwrap();
        directory.remove().unwrap();
        assert!(
            decoy.join("cargo_target").join("precious").exists(),
            "cleanup went through the key descriptor, not the swapped path"
        );
        assert!(
            !moved.join("cargo_target").exists(),
            "the directory the kernel actually held is gone"
        );
        assert_eq!(directory.bytes(), Ok(0));
    }

    #[test]
    fn the_key_counts_and_removes_every_kind_whoever_left_it() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let home = held(&cache, "cargo_home");
        home.ensure().unwrap();
        std::fs::write(home.path().join("registry"), [0_u8; 3000]).unwrap();
        drop(home);
        let key = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        assert_eq!(
            key.kinds().unwrap(),
            ["cargo_home", "cargo_home.lock"],
            "a lock nobody holds is an entry like any other"
        );
        assert!(
            key.bytes().unwrap() >= 3000,
            "the key counts a kind this holder never locked"
        );
        let TaskBuildCacheLock::Held(target) =
            key.lock_kind("cargo_target", Duration::ZERO).unwrap()
        else {
            panic!("the kind is free under a held key");
        };
        target.ensure().unwrap();
        std::fs::write(target.path().join("deps"), [0_u8; 1000]).unwrap();
        let removed = key.remove_kinds().unwrap();
        assert_eq!(
            removed,
            [
                ("cargo_home".to_string(), 3000),
                ("cargo_home.lock".to_string(), 0),
                ("cargo_target".to_string(), 1000)
            ]
        );
        assert_eq!(key.kinds().unwrap(), Vec::<String>::new());
        assert_eq!(target.bytes(), Ok(0));
    }

    #[test]
    fn only_the_lock_inodes_this_holder_opened_are_exempt_from_the_key() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let key = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        let TaskBuildCacheLock::Held(target) =
            key.lock_kind("cargo_target", Duration::ZERO).unwrap()
        else {
            panic!("free kind");
        };
        target.ensure().unwrap();
        // A file named like a lock, and a kind lock nobody holds, both count and both go.
        std::fs::write(key.path().join("extra.lock"), [0_u8; 4096]).unwrap();
        std::fs::write(key.path().join("cargo_home.lock"), [0_u8; 2048]).unwrap();
        // The held kind lock is empty: acquisition truncated it, and its inode is exempt.
        assert_eq!(key.bytes().unwrap(), 4096 + 2048);
        assert_eq!(
            key.kinds().unwrap(),
            ["cargo_home.lock", "cargo_target", "extra.lock"]
        );
        let removed = key.remove_kinds().unwrap();
        assert_eq!(
            removed,
            [
                ("cargo_home.lock".to_string(), 2048),
                ("cargo_target".to_string(), 0),
                ("extra.lock".to_string(), 4096)
            ]
        );
        assert!(
            key.path().join("cargo_target.lock").exists(),
            "the held lock stays"
        );
        assert!(key.path().join(KEY_LOCK).exists());
        assert!(
            key.lock_kind("extra", Duration::ZERO).is_err(),
            "only the closed kind set is ever locked"
        );
        assert_eq!(key.inspect(), None);
    }

    #[test]
    fn a_replaced_lock_or_a_widened_key_is_suspect_and_a_widened_key_is_emptied_before_reuse() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        let TaskBuildCacheLock::Held(target) =
            lock.lock_kind("cargo_target", Duration::ZERO).unwrap()
        else {
            panic!("free kind");
        };
        target.ensure().unwrap();
        std::fs::write(target.path().join("built"), [1_u8; 64]).unwrap();
        // A check unlinks the held key lock and parks an oversized file at its name.
        std::fs::remove_file(lock.path().join(KEY_LOCK)).unwrap();
        std::fs::write(lock.path().join(KEY_LOCK), [0_u8; 8192]).unwrap();
        assert!(
            lock.bytes().unwrap() >= 8192,
            "the impostor is not the held inode"
        );
        assert!(
            lock.inspect()
                .is_some_and(|reason| reason.contains("replaced")),
            "the held lock's name no longer holds its inode"
        );
        assert!(lock.kinds().unwrap().contains(&KEY_LOCK.to_string()));
        lock.remove_kinds().unwrap();
        assert!(
            !lock.path().join(KEY_LOCK).exists(),
            "the impostor went with the key"
        );
        drop(target);
        drop(lock);

        // A check widens the key directory; the next holder empties it before reusing it.
        let key_path = cache.join(key('a')).join(key('b'));
        std::fs::create_dir_all(key_path.join("cargo_target")).unwrap();
        std::fs::write(key_path.join("cargo_target").join("stale"), [2_u8; 32]).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o777)).unwrap();
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::symlink_metadata(&key_path).unwrap().mode() & 0o777,
            0o700,
            "private again"
        );
        assert!(
            !key_path.join("cargo_target").exists(),
            "emptied, never repaired and reused"
        );
        assert_eq!(lock.inspect(), None);
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            lock.inspect().is_some_and(|reason| reason.contains("mode")),
            "a widening during the check is caught after it"
        );
    }

    #[test]
    fn a_hard_link_to_a_held_lock_counts_and_a_held_lock_that_grew_or_was_linked_is_suspect() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        let TaskBuildCacheLock::Held(target) =
            lock.lock_kind("cargo_target", Duration::ZERO).unwrap()
        else {
            panic!("free kind");
        };
        target.ensure().unwrap();
        assert_eq!(lock.inspect(), None);
        // A check hard-links the held key lock into its target and fills it through the link.
        std::fs::hard_link(lock.path().join(KEY_LOCK), target.path().join("payload")).unwrap();
        std::fs::write(target.path().join("payload"), [0_u8; 8192]).unwrap();
        assert!(
            lock.bytes().unwrap() >= 8192,
            "an alias of a held lock elsewhere is counted like any file"
        );
        assert!(
            lock.inspect()
                .is_some_and(|reason| reason.contains("linked")),
            "a held lock with a second name is suspect"
        );
        let removed = lock.remove_kinds().unwrap();
        assert!(!target.path().exists());
        // The alias went with the kind directory, so the lock has one name again, and eviction
        // emptied the bytes the check wrote into it through that alias.
        assert!(
            removed
                .iter()
                .any(|(name, bytes)| name == KEY_LOCK && *bytes == 8192),
            "{removed:?}"
        );
        assert_eq!(
            std::fs::metadata(lock.path().join(KEY_LOCK)).unwrap().len(),
            0
        );
        assert_eq!(
            lock.inspect(),
            None,
            "one name, one inode, zero bytes again"
        );
    }

    #[test]
    fn a_read_only_key_and_a_non_utf8_entry_do_not_survive_eviction() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        // APFS refuses a name that is not UTF-8; there the entry is an ordinary one.
        let odd = lock.path().join(OsStr::from_bytes(b"\xff\xfe-junk"));
        let odd = match std::fs::write(&odd, [0_u8; 4096]) {
            Ok(()) => odd,
            Err(_) => {
                let plain = lock.path().join("junk");
                std::fs::write(&plain, [0_u8; 4096]).unwrap();
                plain
            }
        };
        std::fs::set_permissions(lock.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(lock.inspect().is_some_and(|reason| reason.contains("mode")));
        let removed = lock.remove_kinds().unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].1, 4096);
        assert!(
            std::fs::symlink_metadata(&odd).is_err(),
            "addressed by its exact bytes, not a lossy spelling"
        );
        assert_eq!(lock.kinds().unwrap(), Vec::<String>::new());
        assert_eq!(
            std::fs::symlink_metadata(lock.path()).unwrap().mode() & 0o777,
            0o700,
            "writable again for the eviction, and private"
        );
    }

    #[test]
    fn acquisition_never_truncates_a_planted_hard_link_and_a_displaced_key_is_suspect() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let key_path = cache.join(key('a')).join(key('b'));
        std::fs::create_dir_all(&key_path).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let precious = root.path().join("precious");
        std::fs::write(&precious, [7_u8; 4096]).unwrap();
        // A check left a hard link to a user file where the key lock lives.
        std::fs::hard_link(&precious, key_path.join(KEY_LOCK)).unwrap();
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::metadata(&precious).unwrap().len(),
            4096,
            "the planted inode was never written through"
        );
        assert_eq!(
            std::fs::metadata(&precious).unwrap().nlink(),
            1,
            "the planted name is gone"
        );
        assert_eq!(
            lock.inspect(),
            None,
            "a fresh plain lock file took its place"
        );
        // A check renames the key and recreates a directory at its path while the lock is held.
        let moved = key_path.with_file_name("moved");
        std::fs::rename(&key_path, &moved).unwrap();
        std::fs::create_dir(&key_path).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            lock.inspect()
                .is_some_and(|reason| reason.contains("displaced")),
            "the held directory no longer occupies its name"
        );
    }

    #[test]
    fn eviction_empties_a_grown_held_lock_and_drops_a_compromised_one() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        std::fs::write(lock.path().join(KEY_LOCK), [0_u8; 8192]).unwrap();
        assert!(lock.inspect().is_some_and(|reason| reason.contains("grew")));
        let removed = lock.remove_kinds().unwrap();
        assert_eq!(removed, [(KEY_LOCK.to_string(), 8192)]);
        assert_eq!(
            std::fs::metadata(lock.path().join(KEY_LOCK)).unwrap().len(),
            0
        );
        assert_eq!(
            lock.inspect(),
            None,
            "emptied, and the same inode is still held"
        );
        // Replaced at its name: unlinked, never written through, and the key reads suspect.
        std::fs::remove_file(lock.path().join(KEY_LOCK)).unwrap();
        std::fs::write(lock.path().join(KEY_LOCK), [1_u8; 16]).unwrap();
        lock.remove_kinds().unwrap();
        assert!(!lock.path().join(KEY_LOCK).exists());
        assert!(
            lock.inspect()
                .is_some_and(|reason| reason.contains("removed"))
        );
    }

    #[test]
    fn a_link_or_a_directory_at_a_lock_name_is_replaced_and_the_key_recovers() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let key_path = cache.join(key('a')).join(key('b'));
        std::fs::create_dir_all(&key_path).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, [3_u8; 64]).unwrap();
        std::os::unix::fs::symlink(&outside, key_path.join(KEY_LOCK)).unwrap();
        std::fs::create_dir(key_path.join("cargo_target.lock")).unwrap();
        std::fs::write(key_path.join("cargo_target.lock").join("junk"), b"x").unwrap();
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .expect("a link at the lock's name is replaced, not followed and not fatal");
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            vec![3_u8; 64],
            "never written through"
        );
        assert!(
            !std::fs::symlink_metadata(key_path.join(KEY_LOCK))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let TaskBuildCacheLock::Held(_target) =
            lock.lock_kind("cargo_target", Duration::ZERO).unwrap()
        else {
            panic!("a directory at a kind lock's name is removed and replaced");
        };
        assert!(
            std::fs::metadata(key_path.join("cargo_target.lock"))
                .unwrap()
                .is_file()
        );
        assert_eq!(lock.inspect(), None);
    }

    #[test]
    fn a_held_lock_a_check_grew_counts_toward_the_keys_bytes() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("af").join(TASK_BUILD_CACHE_DIRECTORY);
        let lock = lock_task_build_cache_key(&cache, &key('a'), &key('b'), Duration::ZERO)
            .unwrap()
            .unwrap();
        assert_eq!(
            lock.bytes().unwrap(),
            0,
            "the kernel's own empty lock is not counted"
        );
        std::fs::write(lock.path.join(KEY_LOCK), [7_u8; 8192]).unwrap();
        assert!(
            lock.bytes().unwrap() >= 8192,
            "a lock a check grew is counted while the check runs, not only judged after it"
        );
        assert!(lock.inspect().is_some());
    }

    #[test]
    fn removal_addresses_every_child_by_its_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("key");
        let subject = parent.join("cargo_target");
        std::fs::create_dir_all(&subject).unwrap();
        std::fs::write(
            subject.join("\u{FFFD}"),
            b"the replacement character, spelled out",
        )
        .unwrap();
        if std::fs::write(subject.join(OsStr::from_bytes(b"\xff")), b"raw").is_err() {
            eprintln!("this filesystem refuses names that are not UTF-8; nothing collides here");
            return;
        }
        let descriptor = OwnedFd::from(std::fs::File::open(&parent).unwrap());
        remove_at(&descriptor, "cargo_target", &subject).unwrap();
        assert!(
            std::fs::symlink_metadata(&subject).is_err(),
            "both the raw name and the one its lossy spelling collides with are gone"
        );
    }
}
