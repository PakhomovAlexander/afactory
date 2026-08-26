//! The content- and artifact-addressed immutable store.
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

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::canonical;

const STREAM_BUFFER_BYTES: usize = 64 * 1024;

thread_local! {
    /// Verification is per-object, but scratch is per worker thread. Source trees commonly have
    /// files smaller than this buffer, so zeroing a new 64 KiB array per object can exceed the
    /// actual content traffic by several times.
    static VERIFY_SCRATCH: RefCell<Vec<u8>> = RefCell::new(vec![0_u8; STREAM_BUFFER_BYTES]);
}

fn with_verify_scratch<T>(f: impl FnOnce(&mut [u8]) -> Result<T, CasError>) -> Result<T, CasError> {
    VERIFY_SCRATCH.with(|scratch| f(&mut scratch.borrow_mut()))
}

#[derive(Debug)]
pub enum CasError {
    Io(std::io::Error),
    Canonical(canonical::CanonicalError),
    InvalidDigest(String),
    /// The stored bytes do not validate under the content or envelope identity they are filed under.
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
            CasError::InvalidDigest(digest) => {
                write!(f, "invalid content digest `{digest}`")
            }
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

/// One opened immutable object whose stored length and verifying stream share the same file handle.
///
/// The bytes are not trusted until [`OpenedCasObject::copy_to_and_verify`] succeeds. Keeping the
/// handle private prevents callers from accidentally replacing the verified stream with a second
/// path lookup after using the length for framing.
pub struct OpenedCasObject {
    digest: String,
    file: fs::File,
    stored_len: u64,
}

impl OpenedCasObject {
    pub fn len(&self) -> u64 {
        self.stored_len
    }

    pub fn is_empty(&self) -> bool {
        self.stored_len == 0
    }

    /// Stream the already-open object while verifying its content identity.
    pub fn copy_to_and_verify(&mut self, writer: &mut impl Write) -> Result<u64, CasError> {
        let mut copying = CopyingReader {
            source: &mut self.file,
            destination: writer,
        };
        let (content_id, size) = with_verify_scratch(|buffer| {
            canonical::blob_content_id_reader_with_buffer(&mut copying, buffer)
                .map_err(CasError::Io)
        })?;
        if content_id == self.digest {
            return Ok(size);
        }
        self.file.rewind()?;
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes)?;
        verify_object_bytes(&self.digest, &bytes)?;
        Ok(size)
    }
}

pub struct Cas {
    root: PathBuf,
    /// Objects renamed into place but not yet synced. Drained by [`Cas::flush`].
    pending: Mutex<BTreeSet<PathBuf>>,
    /// Objects whose bytes and publication chain this process has already synced. CAS objects
    /// are immutable, so repeated references need neither another hash pass nor another fsync.
    durable: Mutex<BTreeSet<PathBuf>>,
}

impl Cas {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CasError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("objects"))?;
        Ok(Self {
            root,
            pending: Mutex::new(BTreeSet::new()),
            durable: Mutex::new(BTreeSet::new()),
        })
    }

    fn path_for(&self, digest: &str) -> PathBuf {
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        let (prefix, rest) = hex.split_at(2.min(hex.len()));
        self.root.join("objects").join(prefix).join(rest)
    }

    /// Store content bytes, returning their content digest. Idempotent: storing the same bytes twice is one
    /// object and one digest.
    pub fn put(&self, bytes: &[u8]) -> Result<String, CasError> {
        let digest = canonical::blob_content_id(bytes);
        let final_path = self.path_for(&digest);
        if final_path.exists() {
            // Durability is cacheable, integrity is not: an external mutation after a prior
            // publication must still be detected before idempotent put accepts this object.
            with_verify_scratch(|buffer| verify_exact_bytes(&final_path, &digest, bytes, buffer))?;
            if !self
                .durable
                .lock()
                .expect("cas durable")
                .contains(&final_path)
            {
                // A reopened CAS cannot know whether an existing object and its directory entry
                // reached stable storage. Re-pend verified bytes so the next referencing event
                // establishes that durability instead of trusting existence alone.
                self.pending.lock().expect("cas pending").insert(final_path);
            }
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
        self.pending.lock().expect("cas pending").insert(final_path);
        Ok(digest)
    }

    /// Hash a seekable source through caller-owned scratch, then publish it only when its object
    /// is absent. A warm CAS performs no temporary write; on a cold CAS the second read is the
    /// authoritative publication pass, so a changing source is filed under its actual identity
    /// for the caller's surrounding stability comparison to reject and retry.
    pub fn put_reader_with_buffer(
        &self,
        reader: &mut (impl Read + Seek),
        buffer: &mut [u8],
    ) -> Result<(String, u64), CasError> {
        let (digest, size) = canonical::blob_content_id_reader_with_buffer(&mut *reader, buffer)?;
        let final_path = self.path_for(&digest);
        if final_path.exists() {
            return self.accept_existing_streamed_object(digest, size, final_path, buffer);
        }

        let shard = final_path.parent().expect("object path has a parent");
        fs::create_dir_all(shard)?;
        reader.rewind()?;
        let mut temporary = tempfile::NamedTempFile::new_in(shard)?;
        let (published_digest, published_size) = {
            let mut copying = CopyingReader {
                source: reader,
                destination: temporary.as_file_mut(),
            };
            canonical::blob_content_id_reader_with_buffer(&mut copying, buffer)?
        };
        self.publish_streamed_object(temporary, published_digest, published_size, buffer)
    }

    /// Publish a source whose digest was established by a preceding stability pass. On the normal
    /// cold path this streams the source once; if the source changed meanwhile, the actual bytes
    /// are still filed under their actual digest and the caller's pass comparison rejects them.
    pub fn put_reader_with_expected(
        &self,
        expected_digest: &str,
        reader: &mut (impl Read + Seek),
        buffer: &mut [u8],
    ) -> Result<(String, u64), CasError> {
        if !valid_digest(expected_digest) {
            return Err(CasError::InvalidDigest(expected_digest.to_string()));
        }
        let expected_path = self.path_for(expected_digest);
        if expected_path.exists() {
            let (actual_digest, actual_size) =
                canonical::blob_content_id_reader_with_buffer(&mut *reader, buffer)?;
            let actual_path = self.path_for(&actual_digest);
            if actual_path.exists() {
                return self.accept_existing_streamed_object(
                    actual_digest,
                    actual_size,
                    actual_path,
                    buffer,
                );
            }
            reader.rewind()?;
            return self.put_reader_with_expected(&actual_digest, reader, buffer);
        }

        let expected_shard = expected_path.parent().expect("object path has a parent");
        fs::create_dir_all(expected_shard)?;
        let mut temporary = tempfile::NamedTempFile::new_in(expected_shard)?;
        let (actual_digest, actual_size) = {
            let mut copying = CopyingReader {
                source: reader,
                destination: temporary.as_file_mut(),
            };
            canonical::blob_content_id_reader_with_buffer(&mut copying, buffer)?
        };
        self.publish_streamed_object(temporary, actual_digest, actual_size, buffer)
    }

    fn publish_streamed_object(
        &self,
        temporary: tempfile::NamedTempFile,
        digest: String,
        size: u64,
        buffer: &mut [u8],
    ) -> Result<(String, u64), CasError> {
        let final_path = self.path_for(&digest);
        if final_path.exists() {
            return self.accept_existing_streamed_object(digest, size, final_path, buffer);
        }
        fs::create_dir_all(final_path.parent().expect("object path has a parent"))?;
        temporary
            .persist(&final_path)
            .map_err(|error| CasError::Io(error.error))?;
        self.pending.lock().expect("cas pending").insert(final_path);
        Ok((digest, size))
    }

    fn accept_existing_streamed_object(
        &self,
        digest: String,
        size: u64,
        final_path: PathBuf,
        buffer: &mut [u8],
    ) -> Result<(String, u64), CasError> {
        let mut existing = fs::File::open(&final_path)?;
        let (actual, existing_size) =
            canonical::blob_content_id_reader_with_buffer(&mut existing, buffer)?;
        if actual != digest || existing_size != size {
            return Err(CasError::Corrupt { digest });
        }
        self.pend_existing_if_needed(final_path);
        Ok((digest, size))
    }

    fn pend_existing_if_needed(&self, path: PathBuf) {
        self.mark_for_publication(path);
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
        let mut dirs = BTreeSet::new();
        for path in pending.iter() {
            if let Some(dir) = path.parent() {
                dirs.insert(dir.to_path_buf());
            }
        }
        // Sync every publication ancestor. Syncing only the shard directory does not make a
        // newly created `objects/ab` entry durable in `objects`, and the same applies to the
        // initial `objects` entry in the CAS root.
        dirs.insert(self.root.join("objects"));
        dirs.insert(self.root.clone());
        let root_parent = self
            .root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        dirs.insert(root_parent.to_path_buf());
        sync_concurrently(pending.iter().map(PathBuf::as_path), fsync)?;
        sync_concurrently(dirs.iter().map(PathBuf::as_path), fsync)?;
        self.durable
            .lock()
            .expect("cas durable")
            .extend(pending.iter().cloned());
        pending.clear();
        Ok(())
    }

    /// Store a JSON payload in its canonical form. The digest is then the payload's identity,
    /// independent of how the producer happened to order its fields.
    pub fn put_json(&self, value: &Value) -> Result<String, CasError> {
        review_core::json::admit(value).map_err(|error| {
            CasError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        let bytes = canonical::canonicalize(value)?;
        self.put(&bytes)
    }

    /// Publish a typed artifact envelope and return the CAS ID of the complete immutable record.
    ///
    /// The payload is also stored under its `content_id`, so equal content is retained once even
    /// when distinct producers create distinct provenance records. The returned ID addresses the
    /// envelope bytes; the envelope's domain-separated `artifact_id` is the semantic Report or
    /// Set ID used by reducers.
    pub fn put_artifact(
        &self,
        artifact_type: impl Into<String>,
        producer: review_core::Producer,
        input_artifacts: Vec<String>,
        subject_snapshot_id: Option<String>,
        payload: Value,
    ) -> Result<(String, review_core::ArtifactEnvelope), CasError> {
        let content_id = self.put_json(&payload)?;
        let mut envelope = review_core::ArtifactEnvelope {
            artifact_type: artifact_type.into(),
            artifact_id: String::new(),
            content_id,
            producer,
            input_artifacts,
            subject_snapshot_id,
            payload,
        };
        envelope.artifact_id = canonical::artifact_id(&envelope)?;
        canonical::validate_envelope(&envelope).map_err(|error| {
            CasError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        let value = serde_json::to_value(&envelope).map_err(|error| {
            CasError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        review_core::json::admit(&value).map_err(|error| {
            CasError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        let bytes = canonical::canonicalize(&value)?;
        self.put_artifact_bytes(&envelope.artifact_id, &bytes)?;
        Ok((envelope.artifact_id.clone(), envelope))
    }

    fn put_artifact_bytes(&self, artifact_id: &str, bytes: &[u8]) -> Result<(), CasError> {
        let final_path = self.path_for(artifact_id);
        if final_path.exists() {
            with_verify_scratch(|buffer| {
                verify_exact_bytes(&final_path, artifact_id, bytes, buffer)
            })?;
            verify_object_bytes(artifact_id, bytes)?;
            self.pend_existing_if_needed(final_path);
            return Ok(());
        }
        let directory = final_path.parent().expect("object path has a parent");
        fs::create_dir_all(directory)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(bytes)?;
        temporary
            .persist(&final_path)
            .map_err(|error| CasError::Io(error.error))?;
        self.pending.lock().expect("cas pending").insert(final_path);
        Ok(())
    }

    pub fn get(&self, digest: &str) -> Result<Vec<u8>, CasError> {
        self.get_bounded(digest, u64::MAX)
    }

    /// Read an object's stored length without allocating or claiming its digest is verified.
    /// Callers that act on the bytes must still use a verifying read such as
    /// [`Cas::copy_to_and_verify`].
    pub fn stored_len(&self, digest: &str) -> Result<u64, CasError> {
        if !valid_digest(digest) {
            return Err(CasError::InvalidDigest(digest.to_string()));
        }
        let path = self.path_for(digest);
        let file = fs::File::open(&path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => CasError::NotFound {
                digest: digest.to_string(),
            },
            _ => CasError::Io(error),
        })?;
        Ok(file.metadata()?.len())
    }

    /// Open an object once for length-framed, content-verified streaming.
    pub fn open_for_verified_read(&self, digest: &str) -> Result<OpenedCasObject, CasError> {
        if !valid_digest(digest) {
            return Err(CasError::InvalidDigest(digest.to_string()));
        }
        let path = self.path_for(digest);
        let file = fs::File::open(&path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => CasError::NotFound {
                digest: digest.to_string(),
            },
            _ => CasError::Io(error),
        })?;
        let stored_len = file.metadata()?.len();
        Ok(OpenedCasObject {
            digest: digest.to_string(),
            file,
            stored_len,
        })
    }

    /// Verify and read one object only when its authoritative stored length fits `max_bytes`.
    pub fn get_bounded(&self, digest: &str, max_bytes: u64) -> Result<Vec<u8>, CasError> {
        if !valid_digest(digest) {
            return Err(CasError::InvalidDigest(digest.to_string()));
        }
        let path = self.path_for(digest);
        let mut file = fs::File::open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => CasError::NotFound {
                digest: digest.to_string(),
            },
            _ => CasError::Io(e),
        })?;
        let length = file.metadata()?.len();
        if length > max_bytes {
            return Err(CasError::Io(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!("CAS object {digest} is {length} bytes; limit is {max_bytes}"),
            )));
        }
        let capacity = usize::try_from(length).map_err(|error| {
            CasError::Io(std::io::Error::new(std::io::ErrorKind::FileTooLarge, error))
        })?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|error| CasError::Io(std::io::Error::other(error)))?;
        Read::by_ref(&mut file)
            .take(length)
            .read_to_end(&mut bytes)?;
        let mut trailing = [0_u8; 1];
        if bytes.len() != capacity || file.read(&mut trailing)? != 0 {
            return Err(CasError::Corrupt {
                digest: digest.to_string(),
            });
        }
        // Verify on read: a CAS that trusts its own filenames cannot detect corruption at all.
        verify_object_bytes(digest, &bytes)?;
        Ok(bytes)
    }

    /// Atomically materialize one verified object at `target`.
    /// Unverified bytes remain in a sibling temporary file and are never published at the
    /// caller-visible path. Memory is bounded by the fixed hash buffer regardless of object size.
    pub fn materialize_verified(&self, digest: &str, target: &Path) -> Result<u64, CasError> {
        if !valid_digest(digest) {
            return Err(CasError::InvalidDigest(digest.to_string()));
        }
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let source = self.path_for(digest);
        let cloned = tempfile::Builder::new()
            .prefix(".materialize-")
            .make_in(parent, |path| {
                if let Err(error) = reflink_copy::reflink(&source, path) {
                    // An AlreadyExists path belongs to whoever won the randomized-name race;
                    // never unlink it. Other reflink failures may leave their own partial target.
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        let _ = fs::remove_file(path);
                    }
                    return Err(error);
                }
                match fs::File::open(path) {
                    Ok(file) => Ok(file),
                    Err(error) => {
                        let _ = fs::remove_file(path);
                        Err(error)
                    }
                }
            });
        if let Ok(mut temporary) = cloned {
            let size = with_verify_scratch(|buffer| {
                Self::verify_seekable(digest, temporary.as_file_mut(), buffer)
            })?;
            temporary
                .persist(target)
                .map_err(|error| CasError::Io(error.error))?;
            return Ok(size);
        }

        // Cross-device and non-COW filesystems retain the fixed-buffer fallback. It has the same
        // publish-after-verification contract, but copies the bytes into the private temp file.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        let size = self.copy_to_and_verify(digest, temporary.as_file_mut())?;
        temporary
            .persist(target)
            .map_err(|error| CasError::Io(error.error))?;
        Ok(size)
    }

    /// Stream one verified object into a caller-owned sink without retaining candidate-sized
    /// bytes. The sink may receive a prefix before corruption is detected and must publish only
    /// after this method succeeds.
    pub fn copy_to_and_verify(
        &self,
        digest: &str,
        writer: &mut impl Write,
    ) -> Result<u64, CasError> {
        self.open_for_verified_read(digest)?
            .copy_to_and_verify(writer)
    }

    fn verify_seekable(
        digest: &str,
        reader: &mut (impl Read + Seek),
        buffer: &mut [u8],
    ) -> Result<u64, CasError> {
        let (actual, size) = canonical::blob_content_id_reader_with_buffer(&mut *reader, buffer)?;
        if actual == digest {
            return Ok(size);
        }
        reader.rewind()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        verify_object_bytes(digest, &bytes)?;
        Ok(size)
    }

    pub fn get_json(&self, digest: &str) -> Result<Value, CasError> {
        let bytes = self.get(digest)?;
        serde_json::from_slice(&bytes).map_err(|_| CasError::Corrupt {
            digest: digest.to_string(),
        })
    }

    /// Verify, read, parse, and schedule one JSON object for publication in a single content
    /// pass. Callers may retain the returned value as the exact authority they just verified.
    pub fn get_json_for_publication(&self, digest: &str) -> Result<Value, CasError> {
        let bytes = self.get(digest)?;
        let value = serde_json::from_slice(&bytes).map_err(|_| CasError::Corrupt {
            digest: digest.to_string(),
        })?;
        self.mark_for_publication(self.path_for(digest));
        Ok(value)
    }

    /// Stream and verify an object without retaining its bytes.
    pub fn verify(&self, digest: &str) -> Result<u64, CasError> {
        if !valid_digest(digest) {
            return Err(CasError::InvalidDigest(digest.to_string()));
        }
        let path = self.path_for(digest);
        let mut file = fs::File::open(&path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => CasError::NotFound {
                digest: digest.to_string(),
            },
            _ => CasError::Io(error),
        })?;
        with_verify_scratch(|buffer| Self::verify_seekable(digest, &mut file, buffer))
    }

    /// Verify an object and schedule its bytes and directory entries for the next publication
    /// barrier. A reopened process cannot infer durability from existence, so every new event
    /// reference must pass through this method before commit.
    pub fn prepare_for_publication(&self, digest: &str) -> Result<(), CasError> {
        let path = self.path_for(digest);
        // Hash verification is mandatory for every new reference. The cache below suppresses
        // redundant fsyncs only; it must never turn existence into an integrity assertion.
        self.verify(digest)?;
        self.mark_for_publication(path);
        Ok(())
    }

    fn mark_for_publication(&self, path: PathBuf) {
        if !self.durable.lock().expect("cas durable").contains(&path) {
            self.pending.lock().expect("cas pending").insert(path);
        }
    }

    pub fn contains(&self, digest: &str) -> bool {
        self.get(digest).is_ok()
    }
}

fn verify_exact_bytes(
    path: &Path,
    digest: &str,
    expected: &[u8],
    buffer: &mut [u8],
) -> Result<(), CasError> {
    let mut file = fs::File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => CasError::NotFound {
            digest: digest.to_string(),
        },
        _ => CasError::Io(error),
    })?;
    let mut offset = 0usize;
    loop {
        let read = file.read(buffer)?;
        if read == 0 {
            break;
        }
        let Some(end) = offset.checked_add(read) else {
            return Err(CasError::Corrupt {
                digest: digest.to_string(),
            });
        };
        if expected.get(offset..end) != Some(&buffer[..read]) {
            return Err(CasError::Corrupt {
                digest: digest.to_string(),
            });
        }
        offset = end;
    }
    if offset != expected.len() {
        return Err(CasError::Corrupt {
            digest: digest.to_string(),
        });
    }
    Ok(())
}

fn verify_object_bytes(digest: &str, bytes: &[u8]) -> Result<(), CasError> {
    if canonical::blob_content_id(bytes) == digest {
        return Ok(());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| CasError::Corrupt {
        digest: digest.to_string(),
    })?;
    if canonical::canonicalize(&value).ok().as_deref() != Some(bytes) {
        return Err(CasError::Corrupt {
            digest: digest.to_string(),
        });
    }
    let envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(value).map_err(|_| CasError::Corrupt {
            digest: digest.to_string(),
        })?;
    if envelope.artifact_id != digest || canonical::validate_envelope(&envelope).is_err() {
        return Err(CasError::Corrupt {
            digest: digest.to_string(),
        });
    }
    Ok(())
}

struct CopyingReader<'a, R, W> {
    source: &'a mut R,
    destination: &'a mut W,
}

impl<R: Read, W: Write> Read for CopyingReader<'_, R, W> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.source.read(buffer)?;
        self.destination.write_all(&buffer[..read])?;
        Ok(read)
    }
}

fn valid_digest(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    const MAX_SYNC_WORKERS: usize = 16;
    let workers = MAX_SYNC_WORKERS.min(paths.len().max(1));
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

    struct ChangesOnRewind {
        current: std::io::Cursor<Vec<u8>>,
        replacement: Option<Vec<u8>>,
    }

    impl Read for ChangesOnRewind {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.current.read(buffer)
        }
    }

    impl Seek for ChangesOnRewind {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            if position == std::io::SeekFrom::Start(0)
                && let Some(replacement) = self.replacement.take()
            {
                self.current = std::io::Cursor::new(replacement);
            }
            self.current.seek(position)
        }
    }

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
    fn idempotent_put_stream_compares_content_and_exact_length() {
        for tampered in [
            b"the original bytez".as_slice(),
            b"short".as_slice(),
            b"the original bytes with a suffix".as_slice(),
        ] {
            let (_dir, cas) = cas();
            let expected = b"the original bytes";
            let digest = cas.put(expected).unwrap();
            fs::write(cas.path_for(&digest), tampered).unwrap();
            assert!(matches!(cas.put(expected), Err(CasError::Corrupt { .. })));
        }
    }

    #[test]
    fn streaming_put_uses_caller_scratch_and_reverifies_existing_content() {
        let (directory, cas) = cas();
        let bytes = vec![0x5a; 256 * 1024 + 3];
        let mut scratch = vec![0_u8; 4 * 1024];
        let mut source = std::io::Cursor::new(bytes.as_slice());
        let (digest, size) = cas
            .put_reader_with_buffer(&mut source, &mut scratch)
            .unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(cas.get(&digest).unwrap(), bytes);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                directory.path().join("objects"),
                fs::Permissions::from_mode(0o555),
            )
            .unwrap();
        }
        source.rewind().unwrap();
        let repeated = cas
            .put_reader_with_buffer(&mut source, &mut scratch)
            .unwrap();
        assert_eq!(repeated, (digest.clone(), size));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                directory.path().join("objects"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        fs::write(cas.path_for(&digest), b"tampered").unwrap();
        source.rewind().unwrap();
        assert!(matches!(
            cas.put_reader_with_buffer(&mut source, &mut scratch),
            Err(CasError::Corrupt { .. })
        ));
        drop(directory);
    }

    #[test]
    fn a_changing_cold_source_publishes_its_second_pass_identity() {
        let (_directory, cas) = cas();
        let replacement = b"second authoritative read".to_vec();
        let mut source = ChangesOnRewind {
            current: std::io::Cursor::new(b"first observation".to_vec()),
            replacement: Some(replacement.clone()),
        };
        let mut scratch = vec![0_u8; 64];

        let (digest, size) = cas
            .put_reader_with_buffer(&mut source, &mut scratch)
            .unwrap();

        assert_eq!(digest, canonical::blob_content_id(&replacement));
        assert_eq!(size, replacement.len() as u64);
        assert_eq!(cas.get(&digest).unwrap(), replacement);
    }

    #[test]
    fn verified_copy_streams_exact_bytes_and_detects_corruption() {
        let (_dir, cas) = cas();
        let bytes = vec![0x5a; 256 * 1024];
        let digest = cas.put(&bytes).unwrap();
        let mut copied = Vec::new();
        assert_eq!(
            cas.copy_to_and_verify(&digest, &mut copied).unwrap(),
            bytes.len() as u64
        );
        assert_eq!(copied, bytes);

        let target = _dir.path().join("published");
        assert_eq!(
            cas.materialize_verified(&digest, &target).unwrap(),
            bytes.len() as u64
        );
        assert_eq!(fs::read(&target).unwrap(), bytes);

        fs::write(cas.path_for(&digest), b"tampered").unwrap();
        assert!(matches!(
            cas.copy_to_and_verify(&digest, &mut Vec::new()),
            Err(CasError::Corrupt { .. })
        ));

        let target = _dir.path().join("corrupt-published");
        assert!(matches!(
            cas.materialize_verified(&digest, &target),
            Err(CasError::Corrupt { .. })
        ));
        assert!(!target.exists(), "corrupt bytes must never be published");
    }

    #[test]
    fn bounded_reads_refuse_the_stored_length_before_allocation() {
        let (_dir, cas) = cas();
        let digest = cas.put(b"five!").unwrap();
        assert_eq!(cas.stored_len(&digest).unwrap(), 5);
        assert_eq!(cas.get_bounded(&digest, 5).unwrap(), b"five!");
        assert!(matches!(
            cas.get_bounded(&digest, 4),
            Err(CasError::Io(error)) if error.kind() == std::io::ErrorKind::FileTooLarge
        ));
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
    fn typed_artifacts_are_addressed_by_their_envelope_identity() {
        let (_directory, cas) = cas();
        let payload = json!({"title": "claim"});
        let input = cas.put(b"input").unwrap();
        let (artifact_id, envelope) = cas
            .put_artifact(
                review_core::contract::FINDING_REPORT_V1,
                review_core::Producer::Attempt {
                    run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
                    node_id: "correctness".into(),
                    attempt_id: "01jd8m4qz9k7v3n2p6r8t0w202".into(),
                },
                vec![input],
                None,
                payload.clone(),
            )
            .unwrap();
        assert_eq!(artifact_id, envelope.artifact_id);
        assert_ne!(artifact_id, envelope.content_id);
        assert_eq!(cas.get_json(&artifact_id).unwrap()["payload"], payload);
        assert_eq!(cas.get_json(&envelope.content_id).unwrap(), payload);
        assert!(cas.verify(&artifact_id).is_ok());
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
