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

use std::fs::File;
use std::os::fd::OwnedFd;
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
/// The lock file in `v1/` publishers and readers of blobs share and the sweep takes alone.
pub const GC_LOCK: &str = "gc.lock";
/// The lock file in `v1/` every index connection shares and a replacement takes alone.
pub const INDEX_LOCK: &str = "index.lock";
/// The lock file in `v1/` held while a connection or a replacement takes `index.lock`.
pub const INDEX_INTENT: &str = "index.intent";
/// The index database's file name in `v1/`.
pub const INDEX: &str = "index.sqlite";

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

    /// The opened root's `v1/`, when it is a real directory: the root is opened once by its
    /// pathname, and `v1/` relative to it without following a link (a link in its place is
    /// what `status` reports as unrecognized).
    pub fn open_versioned(&self) -> Option<Confined> {
        Confined::root(&self.root)
            .ok()
            .flatten()?
            .child_dir(VERSION_DIR)
            .ok()
            .flatten()
    }

    pub fn segments(&self) -> PathBuf {
        self.versioned().join("segments")
    }

    pub fn blobs(&self) -> PathBuf {
        self.versioned().join("blobs")
    }

    pub fn gc_lock(&self) -> PathBuf {
        self.versioned().join(GC_LOCK)
    }

    pub fn index_lock(&self) -> PathBuf {
        self.versioned().join(INDEX_LOCK)
    }

    /// Taken shared for the moment a connection takes `index.lock`, and exclusively by a
    /// replacement for its whole duration, so a stream of readers cannot starve it.
    pub fn index_intent(&self) -> PathBuf {
        self.versioned().join(INDEX_INTENT)
    }

    pub fn index(&self) -> PathBuf {
        self.versioned().join(INDEX)
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
/// are listed, judged, created, renamed and removed relative to the open descriptor, so a link
/// swapped in for a directory or a file never takes a walk, a write or a removal outside the
/// root, and a directory renamed or replaced after it was opened changes nothing outside it.
#[derive(Debug)]
pub struct Confined {
    fd: OwnedFd,
    path: PathBuf,
}

fn directory_flags() -> nix::fcntl::OFlag {
    use nix::fcntl::OFlag;
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC
}

/// Files under a telemetry root are created readable and writable by their owner.
fn file_mode() -> nix::sys::stat::Mode {
    nix::sys::stat::Mode::from_bits_truncate(0o644)
}

fn not_a_file(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{} is not a regular file", path.display()),
    )
}

/// The refusal of a file system with no path naming an open directory itself.
#[cfg(not(target_os = "linux"))]
fn no_anchored_path(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "{}: this file system offers no stable inode path for the opened directory; the \
             telemetry index is never opened by pathname",
            path.display()
        ),
    )
}

/// The allocated bytes a `stat` reports (what eviction would free), never the apparent length.
fn stat_allocated(stat: &nix::sys::stat::FileStat) -> u64 {
    u64::try_from(stat.st_blocks)
        .unwrap_or(0)
        .saturating_mul(512)
}

/// What identifies one file's contents under its name: device, inode, size and modification
/// time. A file replaced or rewritten in place under the same name has another identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
}

impl FileIdentity {
    #[allow(clippy::useless_conversion)]
    fn of(stat: &nix::sys::stat::FileStat) -> Self {
        Self {
            dev: u64::try_from(stat.st_dev).unwrap_or(0),
            ino: u64::from(stat.st_ino),
            size: u64::try_from(stat.st_size).unwrap_or(0),
            mtime_ns: i128::from(stat.st_mtime) * 1_000_000_000 + i128::from(stat.st_mtime_nsec),
        }
    }

    /// The identity of an open file.
    pub fn of_file(file: &File) -> std::io::Result<Self> {
        Ok(Self::of(&nix::sys::stat::fstat(file)?))
    }

    /// One text, for the index.
    pub fn text(&self) -> String {
        format!("{}:{}:{}:{}", self.dev, self.ino, self.size, self.mtime_ns)
    }
}

impl Confined {
    /// Open the telemetry root `path` itself, which may be reached through a link the user
    /// chose; everything beneath it is opened with [`Self::child_dir`]. `None` when it is
    /// absent or not a directory.
    pub fn root(path: &Path) -> std::io::Result<Option<Self>> {
        let flags = directory_flags() & !nix::fcntl::OFlag::O_NOFOLLOW;
        match nix::fcntl::openat(
            nix::fcntl::AT_FDCWD,
            path,
            flags,
            nix::sys::stat::Mode::empty(),
        ) {
            Ok(fd) => Ok(Some(Self {
                fd,
                path: path.to_path_buf(),
            })),
            Err(
                nix::errno::Errno::ENOENT | nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR,
            ) => Ok(None),
            Err(errno) => Err(errno.into()),
        }
    }

    /// Open the telemetry root `path` for a command that reads it: `None` when nothing is
    /// there, an error naming it when it is not a directory. The one pathname operation of the
    /// command; everything after is relative to what this returns.
    pub fn open_root(path: &Path) -> std::io::Result<Option<Self>> {
        let flags = directory_flags() & !nix::fcntl::OFlag::O_NOFOLLOW;
        match nix::fcntl::openat(
            nix::fcntl::AT_FDCWD,
            path,
            flags,
            nix::sys::stat::Mode::empty(),
        ) {
            Ok(fd) => Ok(Some(Self {
                fd,
                path: path.to_path_buf(),
            })),
            Err(nix::errno::Errno::ENOENT) => Ok(None),
            Err(nix::errno::Errno::ENOTDIR) => Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!("{} is not a directory", path.display()),
            )),
            Err(errno) => Err(errno.into()),
        }
    }

    /// Create the telemetry root `path` when it is absent (through whatever the user's path
    /// names) and open it.
    pub fn create_root(path: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(path)?;
        Self::root(path)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a directory", path.display()),
            )
        })
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

    /// Another descriptor of the same opened directory, for a holder that outlives this one.
    pub fn try_clone(&self) -> std::io::Result<Self> {
        Ok(Self {
            fd: self.fd.try_clone()?,
            path: self.path.clone(),
        })
    }

    /// The opened directory's own device and inode: what it is wherever its path now leads.
    pub fn own_identity(&self) -> std::io::Result<(u64, u64)> {
        let identity = FileIdentity::of(&nix::sys::stat::fstat(&self.fd)?);
        Ok((identity.dev, identity.ino))
    }

    /// A path naming this opened directory itself, never a pathname looked up again, for a
    /// library that opens files only by path (SQLite); valid while this descriptor is open.
    ///
    /// On macOS it is the directory's inode path `/.vol/<dev>/<ino>`, which no rename and no
    /// link put in its place can redirect; a file system that offers none refuses (fail
    /// closed), never falling back to the directory's pathname. On Linux it is the descriptor's
    /// own entry `/proc/self/fd/<fd>`, which the kernel resolves to this open directory wherever
    /// it now is. Anywhere else there is none, and the request is refused.
    #[cfg(target_vendor = "apple")]
    pub fn anchored_path(&self) -> std::io::Result<PathBuf> {
        use std::os::unix::fs::MetadataExt as _;
        let (dev, ino) = self.own_identity()?;
        let inode = PathBuf::from(format!("/.vol/{dev}/{ino}"));
        // Looked up by inode, never by name: what it reaches is this directory or nothing.
        let names_this = std::fs::symlink_metadata(&inode).is_ok_and(|metadata| {
            metadata.is_dir() && metadata.dev() == dev && metadata.ino() == ino
        });
        if names_this {
            Ok(inode)
        } else {
            Err(no_anchored_path(&self.path))
        }
    }

    /// See the macOS variant.
    #[cfg(target_os = "linux")]
    pub fn anchored_path(&self) -> std::io::Result<PathBuf> {
        use std::os::fd::AsRawFd as _;
        Ok(PathBuf::from(format!(
            "/proc/self/fd/{}",
            self.fd.as_raw_fd()
        )))
    }

    /// See the macOS variant.
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    pub fn anchored_path(&self) -> std::io::Result<PathBuf> {
        Err(no_anchored_path(&self.path))
    }

    /// Where the kernel says this open directory now is, read from its descriptor (Linux): the
    /// text a library expanding [`Self::anchored_path`] reaches, compared, never opened.
    #[cfg(target_os = "linux")]
    pub fn descriptor_path(&self) -> std::io::Result<PathBuf> {
        std::fs::read_link(self.anchored_path()?)
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
        match nix::fcntl::openat(
            &self.fd,
            name,
            directory_flags(),
            nix::sys::stat::Mode::empty(),
        ) {
            Ok(fd) => Ok(Some(Self {
                fd,
                path: self.path.join(name),
            })),
            Err(
                nix::errno::Errno::ENOENT | nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR,
            ) => Ok(None),
            Err(errno) => Err(errno.into()),
        }
    }

    /// The entry `name`, created as a directory when absent, opened relative to this one; an
    /// existing entry that is not a real directory (a link included) is refused: a writer never
    /// follows a link out of the telemetry directory.
    pub fn create_child_dir(&self, name: &str) -> std::io::Result<Self> {
        match nix::sys::stat::mkdirat(
            &self.fd,
            name,
            nix::sys::stat::Mode::from_bits_truncate(0o755),
        ) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
            Err(errno) => return Err(errno.into()),
        }
        self.child_dir(name)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{} is not a real directory; telemetry never writes through a link",
                    self.join(name).display()
                ),
            )
        })
    }

    /// The names of the entries, `.` and `..` excluded; a name that is not UTF-8 is never one
    /// this layout gives, and is skipped.
    pub fn names(&self) -> std::io::Result<Vec<String>> {
        let mut dir = nix::dir::Dir::openat(
            &self.fd,
            ".",
            directory_flags(),
            nix::sys::stat::Mode::empty(),
        )?;
        let mut names = Vec::new();
        for entry in dir.iter() {
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
        nix::sys::stat::fstatat(&self.fd, name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW).ok()
    }

    /// What `name` is, without following a link; `None` when it is gone.
    pub fn kind(&self, name: &str) -> Option<EntryKind> {
        self.stat(name).map(|stat| EntryKind::of(&stat))
    }

    /// The allocated bytes of the entry `name` itself (a link's own, never its target's); 0
    /// when it is gone.
    pub fn allocated(&self, name: &str) -> u64 {
        self.stat(name).map_or(0, |stat| stat_allocated(&stat))
    }

    /// The allocated bytes of this directory itself.
    pub fn own_allocated(&self) -> u64 {
        nix::sys::stat::fstat(&self.fd).map_or(0, |stat| stat_allocated(&stat))
    }

    /// The allocated bytes of the entry `name` with everything beneath it, walked relative to
    /// this directory without following a link.
    pub fn allocated_tree(&self, name: &str) -> u64 {
        let own = self.allocated(name);
        if self.kind(name) != Some(EntryKind::Directory) {
            return own;
        }
        let Ok(Some(child)) = self.child_dir(name) else {
            return own;
        };
        own + child
            .names()
            .unwrap_or_default()
            .iter()
            .map(|inner| child.allocated_tree(inner))
            .sum::<u64>()
    }

    /// The identity of the regular file `name`, judged without following a link.
    pub fn identity(&self, name: &str) -> Option<FileIdentity> {
        self.stat(name)
            .filter(|stat| EntryKind::of(stat) == EntryKind::File)
            .map(|stat| FileIdentity::of(&stat))
    }

    /// The device and inode of the entry `name`, whatever it is, judged without following a
    /// link; `None` when it is gone.
    pub fn entry_identity(&self, name: &str) -> Option<(u64, u64)> {
        self.stat(name).map(|stat| {
            let identity = FileIdentity::of(&stat);
            (identity.dev, identity.ino)
        })
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

    /// When the entry `name` itself (never a link's target) was last modified, in unix
    /// milliseconds; `None` when it is gone.
    pub fn modified_unix_ms(&self, name: &str) -> Option<u64> {
        self.stat(name).map(|stat| {
            let seconds = u64::try_from(stat.st_mtime).unwrap_or(0);
            let nanos = u64::try_from(stat.st_mtime_nsec).unwrap_or(0);
            seconds.saturating_mul(1000) + nanos / 1_000_000
        })
    }

    /// The newest modification time of the entry `name` and everything beneath it, walked
    /// without following a link, in unix milliseconds.
    pub fn newest_modified_unix_ms(&self, name: &str) -> u64 {
        let own = self.modified_unix_ms(name).unwrap_or(0);
        if self.kind(name) != Some(EntryKind::Directory) {
            return own;
        }
        let Ok(Some(child)) = self.child_dir(name) else {
            return own;
        };
        child
            .names()
            .unwrap_or_default()
            .iter()
            .map(|inner| child.newest_modified_unix_ms(inner))
            .fold(own, u64::max)
    }

    /// Remove the directory `name` and everything beneath it relative to this directory,
    /// never following a link: a link beneath it is removed itself, never what it points to.
    pub fn remove_tree(&self, name: &str) -> std::io::Result<()> {
        match self.kind(name) {
            None => return Ok(()),
            Some(EntryKind::Directory) => {}
            Some(_) => return self.unlink(name),
        }
        if let Some(child) = self.child_dir(name)? {
            for inner in child.names()? {
                child.remove_tree(&inner)?;
            }
        }
        nix::unistd::unlinkat(&self.fd, name, nix::unistd::UnlinkatFlags::RemoveDir)
            .map_err(std::io::Error::from)
    }

    /// Remove the regular file `name` relative to this directory. Anything else (a directory, a
    /// link, a device) is left in place and reported as an error.
    pub fn remove_file(&self, name: &str) -> std::io::Result<()> {
        if self.kind(name) != Some(EntryKind::File) {
            return Err(not_a_file(&self.join(name)));
        }
        self.unlink(name)
    }

    /// Unlink the entry `name`, whatever it is but a directory: a link is removed itself, never
    /// what it points to.
    pub fn unlink(&self, name: &str) -> std::io::Result<()> {
        nix::unistd::unlinkat(&self.fd, name, nix::unistd::UnlinkatFlags::NoRemoveDir)
            .map_err(std::io::Error::from)
    }

    fn open_with(&self, name: &str, flags: nix::fcntl::OFlag) -> std::io::Result<File> {
        use nix::fcntl::OFlag;
        let fd = nix::fcntl::openat(
            &self.fd,
            name,
            flags | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            file_mode(),
        )?;
        let file = File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(not_a_file(&self.join(name)));
        }
        Ok(file)
    }

    /// Open the regular file `name` for reading relative to this directory, never through a
    /// link.
    pub fn open_file(&self, name: &str) -> std::io::Result<File> {
        self.open_with(name, nix::fcntl::OFlag::O_RDONLY)
    }

    /// Open the regular file `name` for reading and writing, never through a link.
    pub fn open_read_write(&self, name: &str) -> std::io::Result<File> {
        self.open_with(name, nix::fcntl::OFlag::O_RDWR)
    }

    /// Create the file `name`, which must not exist, for reading and writing; `append` opens it
    /// in append mode.
    pub fn create_file(&self, name: &str, append: bool) -> std::io::Result<File> {
        use nix::fcntl::OFlag;
        let mut flags = OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_EXCL;
        if append {
            flags |= OFlag::O_APPEND;
        }
        self.open_with(name, flags)
    }

    /// Open the regular file `name` for reading and writing, creating it when absent. The open
    /// and the exclusive creation are tried in turn, so two processes creating it at once both
    /// end with the one file (a plain `O_CREAT` open racing another creator can fail with
    /// `ENOENT` on some systems).
    pub fn open_or_create(&self, name: &str) -> std::io::Result<File> {
        let mut last = None;
        for _ in 0..16 {
            match self.open_read_write(name) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                opened => return opened,
            }
            match self.create_file(name, false) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotFound
                    ) =>
                {
                    last = Some(error);
                }
                created => return created,
            }
        }
        Err(last.unwrap_or_else(|| std::io::ErrorKind::NotFound.into()))
    }

    /// Rename the entry `from` to `to`, both in this directory.
    pub fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        nix::fcntl::renameat(&self.fd, from, &self.fd, to).map_err(std::io::Error::from)
    }

    /// Give the entry `from` the name `to`, both in this directory, only while `to` names
    /// nothing: a hard link that refuses an existing name, then the unlink of `from`. A file
    /// that took `to` meanwhile is never replaced.
    pub fn rename_no_replace(&self, from: &str, to: &str) -> std::io::Result<()> {
        nix::unistd::linkat(&self.fd, from, &self.fd, to, nix::fcntl::AtFlags::empty())
            .map_err(std::io::Error::from)?;
        self.unlink(from)
    }

    /// Write `contents` to a temporary file in this directory and rename it to `name`.
    pub fn write_atomically(&self, name: &str, contents: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        let temp = temp_name(WRITE_TEMP_LABEL);
        let written = self.create_file(&temp, false).and_then(|mut file| {
            file.write_all(contents)?;
            file.sync_all()
        });
        match written.and_then(|()| self.rename(&temp, name)) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.unlink(&temp);
                Err(error)
            }
        }
    }
}

/// The opened telemetry root, its `v1/` and the directory `v1/<name>/`, created as needed,
/// each component a real directory.
pub fn create_versioned(layout: &Layout, name: &str) -> std::io::Result<Confined> {
    Confined::create_root(layout.root())?
        .create_child_dir(VERSION_DIR)?
        .create_child_dir(name)
}

/// A held advisory lock; dropping it releases the lock.
pub type Held = Flock<File>;

/// Open the lock file at `path`, creating it when absent, never through a link. A layout's
/// `<root>/v1/<name>` is opened relative to the opened root and its `v1/`, so a link anywhere
/// beneath the root is never followed; any other lock file (the directory registry's, beside
/// the registry) refuses a link as its last component.
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a lock file", path.display()),
        )
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let parent = path.parent().ok_or_else(invalid)?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    if parent.file_name() == Some(std::ffi::OsStr::new(VERSION_DIR)) {
        let root = match parent.parent() {
            Some(root) if !root.as_os_str().is_empty() => root,
            _ => Path::new("."),
        };
        return Confined::create_root(root)?
            .create_child_dir(VERSION_DIR)?
            .open_or_create(name);
    }
    Confined::create_root(parent)?.open_or_create(name)
}

/// Take the lock file `name` of the opened directory `directory` (created when absent, never
/// through a link) with `arg`, trying until `wait` has passed: the lock is reached through the
/// held descriptor, whatever has since become of the directory's path.
pub fn lock_in(
    directory: &Confined,
    name: &str,
    wait: Duration,
    arg: FlockArg,
) -> std::io::Result<Option<Held>> {
    Ok(wait_for(directory.open_or_create(name)?, wait, arg))
}

/// Try `arg` once on `file`.
pub fn try_lock(file: File, arg: FlockArg) -> Result<Held, File> {
    Flock::lock(file, arg).map_err(|(file, _)| file)
}

/// Take the lock file at `path` exclusively, waiting at most `wait`, for a holder that has no
/// opened directory: the directory registry's lock beside the registry, or a fixture holding a
/// root's `gc.lock` from outside as a sweep would. The file's directory is opened once by its
/// pathname ([`open_lock_file`]); the crate's own code under a telemetry root takes its locks
/// through the directory it holds ([`lock_in`]).
pub fn exclusive_within(path: &Path, wait: Duration) -> std::io::Result<Option<Held>> {
    Ok(wait_for(
        open_lock_file(path)?,
        wait,
        FlockArg::LockExclusiveNonblock,
    ))
}

/// Try `arg` on `file` until `wait` has passed.
fn wait_for(mut file: File, wait: Duration, arg: FlockArg) -> Option<Held> {
    let deadline = Instant::now() + wait;
    loop {
        match try_lock(file, arg) {
            Ok(held) => return Some(held),
            Err(back) => file = back,
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
