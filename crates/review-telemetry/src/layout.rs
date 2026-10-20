//! The Telemetry Store's layout (design §6) and its advisory locks.
//!
//! ```text
//! <root>/v1/segments/<started_unix_ms>-<pid>-<nonce>.jsonl      live: one writer under an exclusive lock
//! <root>/v1/segments/<started_unix_ms>-<pid>-<nonce>.jsonl.gz   sealed: compressed on exit, immutable
//! <root>/v1/segments/<started_unix_ms>-<pid>-<nonce>.refs       sealed: the blob digests it references
//! <root>/v1/segments/<started_unix_ms>-<pid>-<nonce>.corrupt    interior corruption: never read
//! <root>/v1/blobs/<aa>/<sha256>.gz                               content-addressed, compressed, shared
//! <root>/v1/gc.lock                                              shared by publishers and readers, exclusive by the sweep
//! <root>/v1/index.lock                                           shared by every index connection, exclusive to replace it
//! <root>/v1/index.intent                                         held while a connection or a replacement takes index.lock
//! <root>/v1/index.sqlite (+ -wal, -shm)                          rebuildable projection over the segments
//! ```
//!
//! The lock, never a PID, says whether a writer is alive: a PID is reused, an `flock` ends with
//! the process that held it.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::fcntl::{Flock, FlockArg};

/// The directory of this release's record shapes. Another version's directory is listed
/// unreadable and never read (ADR-0145).
pub const VERSION_DIR: &str = "v1";
/// A live or unsealed segment.
pub const LIVE: &str = ".jsonl";
/// A sealed segment, compressed with gzip (the extension names the codec).
pub const SEALED: &str = ".jsonl.gz";
/// A sealed segment's manifest of referenced blob digests.
pub const REFS: &str = ".refs";
/// A segment with interior corruption.
pub const CORRUPT: &str = ".corrupt";
/// The prefix of every temporary file under `segments/` and `blobs/`.
pub const TEMP_PREFIX: &str = ".tmp-";
/// How long a command waits for a lock it must have before it gives up or falls back.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);

/// One telemetry directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The telemetry directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn versioned(&self) -> PathBuf {
        self.root.join(VERSION_DIR)
    }

    /// Whether `v1/` is a real directory: a link in its place is never entered (it is what
    /// `status` reports as unrecognized).
    pub fn has_versioned(&self) -> bool {
        std::fs::symlink_metadata(self.versioned()).is_ok_and(|metadata| metadata.is_dir())
    }

    /// Create `path`, a directory beneath the root, one component at a time, refusing any
    /// existing component that is not a real directory: a writer never follows a link out of
    /// the telemetry directory (the root itself may be reached through one the user chose).
    pub fn create_dirs(&self, path: &Path) -> std::io::Result<()> {
        let relative = path.strip_prefix(&self.root).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not beneath {}", path.display(), self.root.display()),
            )
        })?;
        std::fs::create_dir_all(&self.root)?;
        let mut current = self.root.clone();
        for component in relative.components() {
            current.push(component);
            match std::fs::create_dir(&current) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            if !std::fs::symlink_metadata(&current)?.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "{} is not a real directory; telemetry never writes through a link",
                        current.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    pub fn segments(&self) -> PathBuf {
        self.versioned().join("segments")
    }

    pub fn blobs(&self) -> PathBuf {
        self.versioned().join("blobs")
    }

    pub fn gc_lock(&self) -> PathBuf {
        self.versioned().join("gc.lock")
    }

    pub fn index_lock(&self) -> PathBuf {
        self.versioned().join("index.lock")
    }

    /// Taken shared for the moment a connection takes `index.lock`, and exclusively by a
    /// replacement for its whole duration, so a stream of readers cannot starve it.
    pub fn index_intent(&self) -> PathBuf {
        self.versioned().join("index.intent")
    }

    pub fn index(&self) -> PathBuf {
        self.versioned().join("index.sqlite")
    }

    /// The database with its WAL and shared-memory files.
    pub fn index_files(&self) -> [PathBuf; 3] {
        let index = self.index();
        let name = index
            .file_name()
            .expect("named")
            .to_string_lossy()
            .to_string();
        [
            index.clone(),
            index.with_file_name(format!("{name}-wal")),
            index.with_file_name(format!("{name}-shm")),
        ]
    }

    pub fn live_segment(&self, segment_id: &str) -> PathBuf {
        self.segments().join(format!("{segment_id}{LIVE}"))
    }

    pub fn sealed_segment(&self, segment_id: &str) -> PathBuf {
        self.segments().join(format!("{segment_id}{SEALED}"))
    }

    pub fn refs(&self, segment_id: &str) -> PathBuf {
        self.segments().join(format!("{segment_id}{REFS}"))
    }

    pub fn corrupt_segment(&self, segment_id: &str) -> PathBuf {
        self.segments().join(format!("{segment_id}{CORRUPT}"))
    }

    /// The file of a blob: `blobs/<aa>/<64 hex>.gz`, for a `sha256:` digest.
    pub fn blob(&self, digest: &str) -> PathBuf {
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        self.blobs()
            .join(&hex[..2.min(hex.len())])
            .join(format!("{hex}.gz"))
    }
}

/// What a segment file name says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SegmentFile {
    Live,
    Sealed,
    Refs,
    Corrupt,
}

/// The segment id and kind a file name under `segments/` carries; `None` for anything else
/// (temporary files included).
pub fn parse_segment_name(name: &str) -> Option<(String, SegmentFile)> {
    if name.starts_with('.') {
        return None;
    }
    for (suffix, kind) in [
        (SEALED, SegmentFile::Sealed),
        (LIVE, SegmentFile::Live),
        (REFS, SegmentFile::Refs),
        (CORRUPT, SegmentFile::Corrupt),
    ] {
        if let Some(id) = name.strip_suffix(suffix)
            && is_segment_id(id)
        {
            return Some((id.to_string(), kind));
        }
    }
    None
}

/// `<started_unix_ms>-<pid>-<nonce>`: digits, digits, lowercase hex.
pub fn is_segment_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 3
        && parts[..2]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && crate::redact::is_lower_hex(parts[2])
}

/// The label of a publisher's temporary file in `blobs/<aa>/`.
pub const BLOB_TEMP_LABEL: &str = "blob";
/// The label of an atomic write's temporary file in `segments/` (manifests, sealed segments).
pub const WRITE_TEMP_LABEL: &str = "write";

/// A blob prefix directory's name: two lowercase hex digits.
pub fn is_blob_prefix(name: &str) -> bool {
    name.len() == 2 && crate::redact::is_lower_hex(name)
}

/// A blob object's file name in the prefix directory `prefix`: 64 lowercase hex digits starting
/// with `prefix`, then `.gz`. Returns the digest it names.
pub fn blob_object_digest(prefix: &str, name: &str) -> Option<String> {
    let hex = name.strip_suffix(".gz")?;
    (hex.len() == 64 && crate::redact::is_lower_hex(hex) && hex.starts_with(prefix))
        .then(|| format!("sha256:{hex}"))
}

/// A temporary file name [`temp_name`] makes with `label`: `.tmp-<label>-<pid>-<nonce>`, the
/// pid decimal digits and the nonce eight lowercase hex digits. Nothing else is a temporary.
pub fn is_temp_name(name: &str, label: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(TEMP_PREFIX)
        .and_then(|rest| rest.strip_prefix(label))
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let Some((pid, nonce)) = rest.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && nonce.len() == 8
        && crate::redact::is_lower_hex(nonce)
}

/// What a directory entry is, judged without following a symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    /// A symbolic link, a socket, a device, a FIFO: never entered, read or removed.
    Other,
}

impl EntryKind {
    fn of(stat: &nix::sys::stat::FileStat) -> Self {
        use nix::sys::stat::SFlag;
        match SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT {
            SFlag::S_IFDIR => Self::Directory,
            SFlag::S_IFREG => Self::File,
            _ => Self::Other,
        }
    }
}

/// A directory beneath a telemetry root opened without following a symbolic link: its entries
/// are listed, judged and removed relative to the open descriptor, so a link swapped in for a
/// directory or a file never takes a walk or a removal outside the root.
pub struct Confined {
    dir: nix::dir::Dir,
    path: PathBuf,
}

fn directory_flags() -> nix::fcntl::OFlag {
    use nix::fcntl::OFlag;
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC
}

impl Confined {
    /// Open the telemetry root `path` itself, which may be reached through a link the user
    /// chose; everything beneath it is opened with [`Self::child_dir`]. `None` when it is
    /// absent or not a directory.
    pub fn root(path: &Path) -> std::io::Result<Option<Self>> {
        let flags = directory_flags() & !nix::fcntl::OFlag::O_NOFOLLOW;
        match nix::dir::Dir::open(path, flags, nix::sys::stat::Mode::empty()) {
            Ok(dir) => Ok(Some(Self {
                dir,
                path: path.to_path_buf(),
            })),
            Err(
                nix::errno::Errno::ENOENT | nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR,
            ) => Ok(None),
            Err(errno) => Err(errno.into()),
        }
    }

    /// The telemetry root's `v1/<name>/` (`segments`, `blobs`), each component a real
    /// directory.
    pub fn versioned(layout: &Layout, name: &str) -> std::io::Result<Option<Self>> {
        let Some(root) = Self::root(layout.root())? else {
            return Ok(None);
        };
        let Some(versioned) = root.child_dir(VERSION_DIR)? else {
            return Ok(None);
        };
        versioned.child_dir(name)
    }

    /// The directory's path, for messages and reports.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path of the entry `name`, for messages and reports only.
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// The entry `name` when it is a real directory, opened relative to this one.
    pub fn child_dir(&self, name: &str) -> std::io::Result<Option<Self>> {
        if self.kind(name) != Some(EntryKind::Directory) {
            return Ok(None);
        }
        match nix::dir::Dir::openat(
            &self.dir,
            name,
            directory_flags(),
            nix::sys::stat::Mode::empty(),
        ) {
            Ok(dir) => Ok(Some(Self {
                dir,
                path: self.path.join(name),
            })),
            Err(
                nix::errno::Errno::ENOENT | nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR,
            ) => Ok(None),
            Err(errno) => Err(errno.into()),
        }
    }

    /// The names of the entries, `.` and `..` excluded; a name that is not UTF-8 is never one
    /// this layout gives, and is skipped.
    pub fn names(&mut self) -> std::io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in self.dir.iter() {
            let entry = entry?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name != "." && name != ".." {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    fn stat(&self, name: &str) -> Option<nix::sys::stat::FileStat> {
        nix::sys::stat::fstatat(&self.dir, name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW).ok()
    }

    /// What `name` is, without following a link; `None` when it is gone.
    pub fn kind(&self, name: &str) -> Option<EntryKind> {
        self.stat(name).map(|stat| EntryKind::of(&stat))
    }

    /// Whether `name` is a regular file last modified at least `age` before `now`.
    pub fn file_older_than(&self, name: &str, age: Duration, now: std::time::SystemTime) -> bool {
        let Some(stat) = self.stat(name) else {
            return false;
        };
        if EntryKind::of(&stat) != EntryKind::File {
            return false;
        }
        let modified = std::time::UNIX_EPOCH
            + Duration::new(
                u64::try_from(stat.st_mtime).unwrap_or(0),
                u32::try_from(stat.st_mtime_nsec).unwrap_or(0),
            );
        now.duration_since(modified).is_ok_and(|since| since >= age)
    }

    /// Remove the regular file `name` relative to this directory. Anything else (a directory, a
    /// link, a device) is left in place and reported as an error.
    pub fn remove_file(&self, name: &str) -> std::io::Result<()> {
        if self.kind(name) != Some(EntryKind::File) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", self.join(name).display()),
            ));
        }
        nix::unistd::unlinkat(&self.dir, name, nix::unistd::UnlinkatFlags::NoRemoveDir)
            .map_err(std::io::Error::from)
    }

    /// Open the regular file `name` for reading relative to this directory, never through a
    /// link.
    pub fn open_file(&self, name: &str) -> std::io::Result<File> {
        use nix::fcntl::OFlag;
        let fd = nix::fcntl::openat(
            &self.dir,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )?;
        let file = File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", self.join(name).display()),
            ));
        }
        Ok(file)
    }
}

/// Open the file at `path` for reading, refusing a symbolic link as its last component: a
/// segment, manifest or blob file is only ever read where the layout put it.
pub fn open_nofollow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(path)
}

/// A held advisory lock; dropping it releases the lock.
pub type Held = Flock<File>;

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Never through a link: a lock file is created and held only where the layout puts it.
    OpenOptions::new()
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(path)
}

/// Try `arg` once on `file`.
pub fn try_lock(file: File, arg: FlockArg) -> Result<Held, File> {
    Flock::lock(file, arg).map_err(|(file, _)| file)
}

/// Take `gc.lock` or `index.lock` shared without waiting.
pub fn shared_now(path: &Path) -> std::io::Result<Option<Held>> {
    let file = open_lock_file(path)?;
    Ok(try_lock(file, FlockArg::LockSharedNonblock).ok())
}

/// Take a lock file shared, waiting at most `wait`.
pub fn shared_within(path: &Path, wait: Duration) -> std::io::Result<Option<Held>> {
    within(path, wait, FlockArg::LockSharedNonblock)
}

/// Take a lock file exclusively, waiting at most `wait`.
pub fn exclusive_within(path: &Path, wait: Duration) -> std::io::Result<Option<Held>> {
    within(path, wait, FlockArg::LockExclusiveNonblock)
}

fn within(path: &Path, wait: Duration, arg: FlockArg) -> std::io::Result<Option<Held>> {
    let deadline = Instant::now() + wait;
    let mut file = open_lock_file(path)?;
    loop {
        match try_lock(file, arg) {
            Ok(held) => return Ok(Some(held)),
            Err(back) => file = back,
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether some process holds a lock on the file at `path` (it is live). A file that cannot be
/// opened is reported as not locked.
pub fn is_locked(path: &Path) -> bool {
    let Ok(file) = open_nofollow(path) else {
        return false;
    };
    try_lock(file, FlockArg::LockSharedNonblock).is_err()
}

/// Unix nanoseconds now, from the system clock.
pub fn unix_ns_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

/// A file name for a temporary file below a directory: `.tmp-<label>-<pid>-<nonce>`.
pub fn temp_name(label: &str) -> String {
    format!(
        "{TEMP_PREFIX}{label}-{}-{}",
        std::process::id(),
        crate::segment::nonce()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_names_parse_and_temporaries_do_not() {
        assert_eq!(
            parse_segment_name("1700000000000-42-0a1b2c3d.jsonl.gz"),
            Some(("1700000000000-42-0a1b2c3d".into(), SegmentFile::Sealed))
        );
        assert_eq!(
            parse_segment_name("1700000000000-42-0a1b2c3d.jsonl").map(|(_, kind)| kind),
            Some(SegmentFile::Live)
        );
        assert_eq!(parse_segment_name(".tmp-gz-1-ab"), None);
        assert!(is_temp_name(".tmp-blob-42-0a1b2c3d", BLOB_TEMP_LABEL));
        assert!(is_temp_name(&temp_name(WRITE_TEMP_LABEL), WRITE_TEMP_LABEL));
        for stray in [
            ".tmp-blob-42-0a1b2c3d.bak",
            ".tmp-blob-x-0a1b2c3d",
            ".tmp-blob-42-0A1B2C3D",
            ".tmp-write-42-0a1b2c3d",
            ".tmp-blob",
        ] {
            assert!(!is_temp_name(stray, BLOB_TEMP_LABEL), "{stray}");
        }
        let hex = "ab".repeat(32);
        assert_eq!(
            blob_object_digest("ab", &format!("{hex}.gz")),
            Some(format!("sha256:{hex}"))
        );
        assert_eq!(blob_object_digest("cd", &format!("{hex}.gz")), None);
        assert_eq!(blob_object_digest("ab", "stray.gz.bak"), None);
        assert_eq!(blob_object_digest("ab", &format!("{hex}.gz.bak")), None);
        assert!(is_blob_prefix("0f") && !is_blob_prefix("0F") && !is_blob_prefix("abc"));
        assert_eq!(parse_segment_name("notes.jsonl"), None);
        let layout = Layout::new("/t");
        assert_eq!(
            layout.blob(&format!("sha256:{}", "ab".repeat(32))),
            PathBuf::from(format!("/t/v1/blobs/ab/{}.gz", "ab".repeat(32)))
        );
    }
}
