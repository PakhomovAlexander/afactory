//! The content-addressed artifact store.
//!
//! Publication order is the whole contract: an object must be durable *before* any event can
//! reference it, or a crash leaves the log pointing at bytes that were never written. The
//! invariant names the *event* as the deadline, not the put — so durability is deferred:
//! `put` writes a temp file and atomically renames it into place, and [`Cas::flush`] is the
//! barrier that syncs everything pending. The event store calls `flush` before an event that
//! references artifacts lands, which is the one moment the invariant actually binds. A crash
//! before that loses unreferenced objects nobody ever promised.
//!
//! Durability is `fsync(2)`, matching the grade the event store's own SQLite runs at
//! (`synchronous=FULL`, `PRAGMA fullfsync` left at its default 0 — i.e. plain fsync). This is
//! deliberate and load-bearing: the CAS exists to serve the log, so a crash that loses an
//! fsync'd event row also loses the object it would have referenced — making the object *more*
//! durable than the row buys nothing and costs a great deal. On macOS `File::sync_data`/
//! `sync_all` are `fcntl(F_FULLFSYNC)`, a full device-cache barrier ~70x slower than `fsync`;
//! calling those here made the CAS strictly more durable than the record it protects. So the
//! barrier syncs through `nix::unistd::fsync` instead.
//!
//! The failure this ordering prevents is not "the object is missing" — it is a run that replays
//! into a *different* state than it committed, silently.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::canonical;

#[derive(Debug)]
pub enum CasError {
    Io(std::io::Error),
    Canonical(canonical::CanonicalError),
    /// The stored bytes do not hash to the digest they are filed under.
    Corrupt {
        digest: String,
    },
    NotFound {
        digest: String,
    },
}

impl std::fmt::Display for CasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CasError::Io(e) => write!(f, "cas io: {e}"),
            CasError::Canonical(e) => write!(f, "cas canonicalization: {e}"),
            CasError::Corrupt { digest } => {
                write!(f, "cas object does not match its digest: {digest}")
            }
            CasError::NotFound { digest } => write!(f, "cas object not found: {digest}"),
        }
    }
}

impl std::error::Error for CasError {}

impl From<std::io::Error> for CasError {
    fn from(e: std::io::Error) -> Self {
        CasError::Io(e)
    }
}

impl From<canonical::CanonicalError> for CasError {
    fn from(e: canonical::CanonicalError) -> Self {
        CasError::Canonical(e)
    }
}

pub struct Cas {
    root: PathBuf,
    /// Objects renamed into place but not yet synced. Drained by [`Cas::flush`].
    pending: Mutex<Vec<PathBuf>>,
}

impl Cas {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CasError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("objects"))?;
        Ok(Self {
            root,
            pending: Mutex::new(Vec::new()),
        })
    }

    fn path_for(&self, digest: &str) -> PathBuf {
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        let (prefix, rest) = hex.split_at(2.min(hex.len()));
        self.root.join("objects").join(prefix).join(rest)
    }

    /// Store bytes, returning their digest. Idempotent: storing the same bytes twice is one
    /// object and one digest.
    pub fn put(&self, bytes: &[u8]) -> Result<String, CasError> {
        let digest = canonical::blob_content_id(bytes);
        let final_path = self.path_for(&digest);
        if final_path.exists() {
            return Ok(digest);
        }
        let dir = final_path.parent().expect("object path has a parent");
        fs::create_dir_all(dir)?;

        // The temp name must be unique per call, not per digest: two threads storing the
        // same bytes would otherwise race on one temp file, and the loser's rename fails.
        static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp_path = dir.join(format!(".tmp-{}-{sequence}", &digest[7..23]));
        {
            let mut file = fs::File::create(&temp_path)?;
            file.write_all(bytes)?;
        }
        fs::rename(&temp_path, &final_path)?;
        // Durability is deferred, not skipped: the object is pending until `flush`, and no
        // event may reference it before then.
        self.pending.lock().expect("cas pending").push(final_path);
        Ok(digest)
    }

    /// Make every pending object durable: the object bytes, then each touched directory so
    /// the renames themselves survive. This is the publication barrier — the event store
    /// calls it before an event that references artifacts lands.
    ///
    /// Syncs run on a small thread pool: each one is device latency, not CPU, and a capture
    /// can leave thousands pending. The barrier returns only when every sync came back clean.
    pub fn flush(&self) -> Result<(), CasError> {
        // The list is cleared only after everything synced: a failed flush keeps its objects
        // pending, so the next barrier retries them rather than forgetting them.
        let mut pending = self.pending.lock().expect("cas pending");
        if pending.is_empty() {
            return Ok(());
        }
        let mut dirs: BTreeSet<&Path> = BTreeSet::new();
        for path in pending.iter() {
            if let Some(dir) = path.parent() {
                dirs.insert(dir);
            }
        }
        sync_concurrently(pending.iter().map(PathBuf::as_path), fsync)?;
        sync_concurrently(dirs.into_iter(), fsync)?;
        pending.clear();
        Ok(())
    }

    /// Store a JSON payload in its canonical form. The digest is then the payload's identity,
    /// independent of how the producer happened to order its fields.
    pub fn put_json(&self, value: &Value) -> Result<String, CasError> {
        let bytes = canonical::canonicalize(value)?;
        self.put(&bytes)
    }

    pub fn get(&self, digest: &str) -> Result<Vec<u8>, CasError> {
        let path = self.path_for(digest);
        let bytes = fs::read(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => CasError::NotFound {
                digest: digest.to_string(),
            },
            _ => CasError::Io(e),
        })?;
        // Verify on read: a CAS that trusts its own filenames cannot detect corruption at all.
        if canonical::blob_content_id(&bytes) != digest {
            return Err(CasError::Corrupt {
                digest: digest.to_string(),
            });
        }
        Ok(bytes)
    }

    pub fn get_json(&self, digest: &str) -> Result<Value, CasError> {
        let bytes = self.get(digest)?;
        serde_json::from_slice(&bytes).map_err(|_| CasError::Corrupt {
            digest: digest.to_string(),
        })
    }

    pub fn contains(&self, digest: &str) -> bool {
        self.path_for(digest).exists()
    }
}

/// `fsync(2)` on a file or directory — the log's durability grade, not `F_FULLFSYNC`. On a
/// directory it makes a rename durable; on a file, its bytes.
#[cfg(unix)]
fn fsync(file: &fs::File) -> std::io::Result<()> {
    nix::unistd::fsync(file).map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn fsync(file: &fs::File) -> std::io::Result<()> {
    // No fsync/F_FULLFSYNC split to worry about off-unix; sync_data is the plain-fsync analog.
    file.sync_data()
}

/// Open and sync every path, fanned out over a bounded pool. The first error wins; success
/// means every sync completed.
fn sync_concurrently<'p>(
    paths: impl Iterator<Item = &'p Path>,
    sync: fn(&fs::File) -> std::io::Result<()>,
) -> Result<(), CasError> {
    let paths: Vec<&Path> = paths.collect();
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(paths.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| -> std::io::Result<()> {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(path) = paths.get(i) else {
                            return Ok(());
                        };
                        sync(&fs::File::open(path)?)?;
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("sync worker")?;
        }
        Ok::<(), std::io::Error>(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cas() -> (tempfile::TempDir, Cas) {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        (dir, cas)
    }

    #[test]
    fn put_is_idempotent_and_content_addressed() {
        let (_dir, cas) = cas();
        let a = cas.put_json(&json!({ "x": 1, "y": 2 })).unwrap();
        let b = cas.put_json(&json!({ "y": 2, "x": 1 })).unwrap();
        assert_eq!(a, b, "field order must not change identity");
        assert_eq!(cas.get_json(&a).unwrap(), json!({ "x": 1, "y": 2 }));
    }

    #[test]
    fn a_corrupted_object_is_detected_not_returned() {
        let (_dir, cas) = cas();
        let digest = cas.put(b"the original bytes").unwrap();
        let path = cas.path_for(&digest);
        fs::write(&path, b"tampered").unwrap();
        assert!(matches!(cas.get(&digest), Err(CasError::Corrupt { .. })));
    }

    #[test]
    fn missing_is_distinct_from_corrupt() {
        let (_dir, cas) = cas();
        let digest = canonical::blob_content_id(b"never stored");
        assert!(matches!(cas.get(&digest), Err(CasError::NotFound { .. })));
        assert!(!cas.contains(&digest));
    }

    #[test]
    fn flush_makes_pending_objects_durable_and_is_idempotent() {
        let (_dir, cas) = cas();
        let digests: Vec<String> = (0..64u32)
            .map(|i| cas.put(format!("object {i}").as_bytes()).unwrap())
            .collect();
        cas.flush().unwrap();
        // Nothing pending: a no-op, not an error.
        cas.flush().unwrap();
        for digest in &digests {
            assert!(cas.contains(digest));
        }
    }

    #[test]
    #[ignore = "benchmark; run with --release -- --ignored"]
    fn bench_put_throughput() {
        let (_dir, cas) = cas();
        let payload = vec![0x42u8; 8192];
        let count = 2000u32;
        let start = std::time::Instant::now();
        for i in 0..count {
            let mut bytes = payload.clone();
            bytes.extend_from_slice(&i.to_le_bytes());
            cas.put(&bytes).unwrap();
        }
        cas.flush().unwrap();
        let elapsed = start.elapsed();
        eprintln!(
            "put+flush: {count} x 8KiB in {elapsed:?} = {:.0} files/s",
            f64::from(count) / elapsed.as_secs_f64()
        );
    }

    #[test]
    fn no_temp_files_survive_a_put() {
        let (dir, cas) = cas();
        cas.put(b"some bytes").unwrap();
        let strays: Vec<_> = walk(dir.path())
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(".tmp-"))
            })
            .collect();
        assert!(strays.is_empty(), "temp files left behind: {strays:?}");
    }

    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out
    }
}
