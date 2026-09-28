//! The Warm Check Cache's host directory (ADR-0123): one machine-local build directory per
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
use std::sync::Arc;
use std::time::{Duration, Instant};

use nix::dir::Dir;
use nix::errno::Errno;
use nix::fcntl::{AtFlags, Flock, FlockArg, OFlag};
use nix::sys::stat::{FchmodatFlags, FileStat, Mode, SFlag};
use nix::unistd::UnlinkatFlags;

/// The fixed directory name below `$XDG_CACHE_HOME/af`.
pub const TASK_BUILD_CACHE_DIRECTORY: &str = "task-build-cache";

/// The lock file of the whole toolchain key, beside the kind directories.
const KEY_LOCK: &str = "warm.lock";

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
    path: PathBuf,
    _lock: Flock<File>,
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
    let (key, path) = open_key(root, project, toolchain)?;
    Ok(
        exclusive_lock_at(&key, KEY_LOCK, wait)?.map(|lock| TaskBuildCacheKeyLock {
            key,
            path,
            _lock: lock,
        }),
    )
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
    let (key, path) = open_key(root, project, toolchain)?;
    lock_kind_at(key, &path, kind, wait)
}

impl TaskBuildCacheKeyLock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lock one kind below this key, waiting at most `wait`.
    pub fn lock_kind(&self, kind: &str, wait: Duration) -> Result<TaskBuildCacheLock, String> {
        lock_kind_at(Arc::clone(&self.key), &self.path, kind, wait)
    }

    /// Bytes below the whole key — every kind directory, whoever left it, and the lock files —
    /// counted without following links. A key the count cannot fully inspect is
    /// `Uninspectable`, never smaller.
    pub fn bytes(&self) -> Result<u64, Uninspectable> {
        let descriptor = nix::unistd::dup(&*self.key).map_err(|errno| Uninspectable {
            counted: 0,
            detail: format!(
                "reopening the toolchain key {}: {errno}",
                self.path.display()
            ),
        })?;
        count(descriptor, &self.path)
    }

    /// The kind entries currently below the key, by name, sorted: every entry that
    /// is not one of the key's lock files, whatever a check made of it.
    pub fn kinds(&self) -> Result<Vec<String>, String> {
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
            let Ok(stat) = nix::sys::stat::fstatat(&*self.key, name, AtFlags::AT_SYMLINK_NOFOLLOW)
            else {
                continue;
            };
            let text = OsStr::from_bytes(name.to_bytes())
                .to_string_lossy()
                .into_owned();
            // The key's own lock files stay; everything else below the key — a kind directory,
            // or whatever a check put where a kind was — is a kind entry to count and remove.
            if file_kind(&stat) == SFlag::S_IFREG && text.ends_with(".lock") {
                continue;
            }
            names.push(text);
        }
        names.sort();
        Ok(names)
    }

    /// Remove every kind directory below the key, whoever left it, and report each one's bytes
    /// before removal (a lower bound when it could not be fully counted). Lock files stay.
    pub fn remove_kinds(&self) -> Result<Vec<(String, u64)>, String> {
        let mut removed = Vec::new();
        for name in self.kinds()? {
            let bytes = match count_child(&self.key, &name, &self.path.join(&name)) {
                Ok(bytes) => bytes,
                Err(uninspectable) => uninspectable.counted,
            };
            remove_at(&self.key, &name, &self.path.join(&name))?;
            removed.push((name, bytes));
        }
        Ok(removed)
    }
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

    /// Whether a real directory, not a link or a file, sits at the kind's name right now.
    pub fn is_directory(&self) -> bool {
        matches!(
            nix::sys::stat::fstatat(&*self.key, self.name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW),
            Ok(stat) if file_kind(&stat) == SFlag::S_IFDIR
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
        if u32::from(stat.st_mode) & 0o777 != 0o700 {
            return Judged::Suspect(format!(
                "the directory has mode {:o}, not 700",
                u32::from(stat.st_mode) & 0o777
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
) -> Result<(Arc<OwnedFd>, PathBuf), String> {
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
    let project_fd = open_level(&root_fd, project)?;
    let key = open_level(&project_fd, toolchain)?;
    Ok((Arc::new(key), root.join(project).join(toolchain)))
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
/// (a link or a file where the level should be is refused), owner and mode checked on the open
/// descriptor.
fn open_level(parent: &OwnedFd, name: &str) -> Result<OwnedFd, String> {
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
    secure_level(&descriptor)?;
    Ok(descriptor)
}

fn secure_level(descriptor: &OwnedFd) -> Result<(), String> {
    let stat = nix::sys::stat::fstat(descriptor)
        .map_err(|errno| format!("inspecting a Task build cache directory: {errno}"))?;
    if stat.st_uid != nix::unistd::geteuid().as_raw() {
        return Err("a Task build cache directory belongs to another user".into());
    }
    if u32::from(stat.st_mode) & 0o777 != 0o700 {
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
) -> Result<TaskBuildCacheLock, String> {
    if !is_kind(kind) {
        return Err("Task build cache key is not an opaque identity".into());
    }
    match exclusive_lock_at(&key, &format!("{kind}.lock"), wait)? {
        Some(lock) => Ok(TaskBuildCacheLock::Held(WarmDirectory {
            path: key_path.join(kind),
            name: kind.to_string(),
            key,
            forbidden: &[],
            _lock: lock,
        })),
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
    let descriptor = nix::fcntl::openat(
        directory,
        name,
        OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )
    .map_err(|errno| format!("opening the Task build cache lock: {errno}"))?;
    let mut file = File::from(descriptor);
    let started = Instant::now();
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(Some(lock)),
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
            let child_name = OsStr::from_bytes(child.as_bytes());
            remove_at(
                &descriptor,
                &child_name.to_string_lossy(),
                &display.join(child_name),
            )
            .or_else(|detail| {
                // Names that are not UTF-8 go through the C string directly.
                remove_c(&descriptor, &child, &display.join(child_name)).map_err(|_| detail)
            })?;
        }
        drop(descriptor);
        match nix::unistd::unlinkat(parent, name, UnlinkatFlags::RemoveDir) {
            Ok(()) | Err(Errno::ENOENT) => {}
            Err(errno) => return Err(failed(errno)),
        }
    } else {
        match nix::unistd::unlinkat(parent, name, UnlinkatFlags::NoRemoveDir) {
            Ok(()) | Err(Errno::ENOENT) => {}
            Err(errno) => return Err(failed(errno)),
        }
    }
    match nix::sys::stat::fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(Errno::ENOENT) => Ok(()),
        Ok(_) => Err(format!(
            "Task build cache removal left {} behind",
            display.display()
        )),
        Err(errno) => Err(failed(errno)),
    }
}

/// [`remove_at`] for an entry whose name is not UTF-8.
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
    let mut total = 0_u64;
    walk_from(descriptor, display, &mut |_, stat| {
        if file_kind(stat) != SFlag::S_IFDIR {
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
        assert_eq!(key.kinds().unwrap(), ["cargo_home"]);
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
                ("cargo_target".to_string(), 1000)
            ]
        );
        assert_eq!(key.kinds().unwrap(), Vec::<String>::new());
        assert_eq!(target.bytes(), Ok(0));
    }
}
