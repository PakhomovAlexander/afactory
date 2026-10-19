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

/// A held advisory lock; dropping it releases the lock.
pub type Held = Flock<File>;

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
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
    let Ok(file) = File::open(path) else {
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
        assert_eq!(parse_segment_name("notes.jsonl"), None);
        let layout = Layout::new("/t");
        assert_eq!(
            layout.blob(&format!("sha256:{}", "ab".repeat(32))),
            PathBuf::from(format!("/t/v1/blobs/ab/{}.gz", "ab".repeat(32)))
        );
    }
}
