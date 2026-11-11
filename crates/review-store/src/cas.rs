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
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::canonical;

const STREAM_BUFFER_BYTES: usize = 64 * 1024;
/// Typed artifact envelopes are control-plane records, not arbitrary source blobs. Bounding them
/// makes the content-ID-to-envelope-ID compatibility fallback safe to buffer after a mismatch.
const MAX_ARTIFACT_ENVELOPE_BYTES: u64 = 8 * 1024 * 1024;

thread_local! {
    /// Verification is per-object, but scratch is per worker thread. Source trees commonly have
    /// files smaller than this buffer, so zeroing a new 64 KiB array per object can exceed the
    /// actual content traffic by several times.
    static READ_TRACES: RefCell<Vec<ReadTrace>> = const { RefCell::new(Vec::new()) };
    static VERIFY_SCRATCH: RefCell<Vec<u8>> = RefCell::new(vec![0_u8; STREAM_BUFFER_BYTES]);
}

// Trace only this thread's reads of this CAS. Nested projections contribute to their
// parent's closure; other threads and other Stores cannot supply authority for it.
struct ReadTrace {
    cas: usize,
    ids: BTreeSet<String>,
}
struct ReadTraceGuard;
impl Drop for ReadTraceGuard {
    fn drop(&mut self) {
        READ_TRACES.with(|traces| {
            traces.borrow_mut().pop();
        });
    }
}

// The issue_255_large wire envelope is 5.6 MB but its numeric-byte JSON array alone
// occupies about 192 MiB as ordered Values after trimming array capacity. Bound decoded
// memory while retaining that real workload.
const VALIDATED_CACHE_BYTES: usize = 256 * 1024 * 1024;
const VALIDATED_CACHE_ENTRIES: usize = 128;

struct ValidatedArtifact {
    fingerprint: String,
    envelope: Arc<review_core::ArtifactEnvelope>,
    weight: usize,
}
#[derive(Default)]
struct ValidatedArtifacts {
    entries: BTreeMap<String, ValidatedArtifact>,
    order: VecDeque<String>,
    bytes: usize,
}
impl ValidatedArtifacts {
    fn remove(&mut self, id: &str) {
        if let Some(old) = self.entries.remove(id) {
            self.bytes -= old.weight;
            self.order.retain(|key| key != id);
        }
    }
    fn get(&mut self, id: &str, fingerprint: &str) -> Option<Arc<review_core::ArtifactEnvelope>> {
        if let Some(entry) = self.entries.get(id) {
            if entry.fingerprint == fingerprint {
                return Some(Arc::clone(&entry.envelope));
            }
        }
        self.remove(id);
        None
    }
    fn insert(
        &mut self,
        id: &str,
        fingerprint: String,
        envelope: Arc<review_core::ArtifactEnvelope>,
    ) {
        self.remove(id);
        // Account for decoded allocations, not just wire bytes. Even small JSON objects
        // allocate map buckets; charge 1 KiB per member, plus allocator/page
        // slop for strings and arrays. Dense objects can exceed the memo and bypass it.
        let producer_bytes = match &envelope.producer {
            review_core::Producer::Attempt {
                run_id,
                node_id,
                attempt_id,
            } => string_weight(run_id) + string_weight(node_id) + string_weight(attempt_id),
            review_core::Producer::KernelOperation {
                run_id,
                node_id,
                operation_id,
            } => {
                string_weight(run_id)
                    + node_id.as_ref().map_or(0, string_weight)
                    + string_weight(operation_id)
            }
        };
        let weight = value_weight(&envelope.payload)
            .saturating_add(producer_bytes)
            .saturating_add(string_weight(&envelope.artifact_type))
            .saturating_add(
                envelope
                    .subject_snapshot_id
                    .as_ref()
                    .map_or(0, String::capacity),
            )
            .saturating_add(
                envelope
                    .input_artifacts
                    .iter()
                    .map(|s| s.capacity() + 64)
                    .sum::<usize>(),
            )
            .saturating_add(4096);
        if weight > VALIDATED_CACHE_BYTES {
            return;
        }
        while self.entries.len() >= VALIDATED_CACHE_ENTRIES
            || self.bytes + weight > VALIDATED_CACHE_BYTES
        {
            let oldest = self.order.front().expect("nonempty bounded cache").clone();
            self.remove(&oldest);
        }
        self.order.push_back(id.to_owned());
        self.bytes += weight;
        self.entries.insert(
            id.to_owned(),
            ValidatedArtifact {
                fingerprint,
                envelope,
                weight,
            },
        );
    }
}
fn string_weight(text: &String) -> usize {
    text.capacity().saturating_add(4096)
}

// A parsed byte array can reserve the next power-of-two number of 72-byte ordered Values.
// Trim that spare allocation once on a validated miss, before sharing immutable decoded data.
fn compact_arrays(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for value in items.iter_mut() {
                compact_arrays(value);
            }
            items.shrink_to_fit();
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                compact_arrays(value);
            }
        }
        _ => {}
    }
}

fn value_weight(value: &Value) -> usize {
    match value {
        Value::String(text) => string_weight(text),
        Value::Array(items) => items.iter().fold(
            items
                .capacity()
                .saturating_mul(std::mem::size_of::<Value>())
                .saturating_add(4096),
            |size, value| size.saturating_add(value_weight(value)),
        ),
        Value::Object(fields) => fields.iter().fold(0usize, |size, (key, value)| {
            size.saturating_add(1024)
                .saturating_add(string_weight(key))
                .saturating_add(value_weight(value))
        }),
        _ => 0,
    }
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
        if self.stored_len > MAX_ARTIFACT_ENVELOPE_BYTES {
            return Err(CasError::Corrupt {
                digest: self.digest.clone(),
            });
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
    validated: Mutex<ValidatedArtifacts>,
}

impl Cas {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CasError> {
        let root = root.as_ref().to_path_buf();
        // The CAS holds whole source snapshots, prompts and results: private, whatever the
        // umask (crate::local_fs).
        crate::local_fs::create_private_dir_all(&root.join("objects"))?;
        Ok(Self::at_root(root))
    }

    /// Open an existing CAS without creating or changing any directory.
    pub fn open_existing(root: impl AsRef<Path>) -> Result<Self, CasError> {
        let root = root.as_ref().to_path_buf();
        let root_metadata = fs::symlink_metadata(&root)?;
        let metadata = fs::symlink_metadata(root.join("objects"))?;
        if !root_metadata.is_dir()
            || root_metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
        {
            return Err(CasError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "CAS objects path is not a real directory: {}",
                    root.display()
                ),
            )));
        }
        Ok(Self::at_root(root))
    }

    fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            pending: Mutex::new(BTreeSet::new()),
            durable: Mutex::new(BTreeSet::new()),
            validated: Mutex::new(ValidatedArtifacts::default()),
        }
    }

    fn path_for(&self, digest: &str) -> PathBuf {
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        let (prefix, rest) = hex.split_at(2.min(hex.len()));
        self.root.join("objects").join(prefix).join(rest)
    }

    /// Store content bytes, returning their content digest. Idempotent: storing the same bytes twice is one
    /// object and one digest.
    pub fn put(&self, bytes: &[u8]) -> Result<String, CasError> {
        let _span = review_core::perf_trace::span(review_core::perf_trace::Phase::CasPut);
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
        // The temp name must be unique per call across every process sharing this Store, not
        // per digest or per process: two `af task plan` storing the same bytes in the same
        // second once staged them under one name, so one truncated the other's file and the
        // loser's rename found nothing (issue #159). A lost race is retried once.
        match self.publish_object(dir, &final_path, bytes) {
            Ok(()) => {}
            Err(_) if self.published_intact(&final_path, &digest, bytes) => {}
            Err(_) => self.publish_object(dir, &final_path, bytes)?,
        }
        // Durability is deferred, not skipped: the object is pending until `flush`, and no
        // event may reference it before then.
        self.pending.lock().expect("cas pending").insert(final_path);
        Ok(digest)
    }

    /// Stage `bytes` under a temporary name no other call or process can hold, private, in `dir`,
    /// and move it to `final_path` only while that path is absent. An object another writer
    /// published meanwhile keeps its directory entry, which may already be durable and
    /// referenced: it is verified to hold exactly `bytes` and accepted, never replaced.
    fn publish_object(&self, dir: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), CasError> {
        crate::local_fs::create_private_dir_all(dir)?;
        // `tempfile` creates the file with `O_EXCL` and mode 0600.
        let mut temporary = tempfile::Builder::new().prefix(".tmp-").tempfile_in(dir)?;
        temporary.write_all(bytes)?;
        match temporary.persist_noclobber(final_path) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let digest = canonical::blob_content_id(bytes);
                with_verify_scratch(|buffer| verify_exact_bytes(final_path, &digest, bytes, buffer))
            }
            Err(error) => Err(CasError::Io(error.error)),
        }
    }

    /// Whether another writer already published exactly `bytes` at `final_path`: the race a
    /// failed publication lost, which leaves the object as this call would have written it.
    fn published_intact(&self, final_path: &Path, digest: &str, bytes: &[u8]) -> bool {
        final_path.exists()
            && with_verify_scratch(|buffer| verify_exact_bytes(final_path, digest, bytes, buffer))
                .is_ok()
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
        crate::local_fs::create_private_dir_all(shard)?;
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
        crate::local_fs::create_private_dir_all(expected_shard)?;
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
        crate::local_fs::create_private_dir_all(
            final_path.parent().expect("object path has a parent"),
        )?;
        // The move happens only while the object is absent: one another writer published
        // (before this call or racing it) keeps its directory entry, verified, never replaced.
        match temporary.persist_noclobber(&final_path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                return self.accept_existing_streamed_object(digest, size, final_path, buffer);
            }
            Err(error) => return Err(CasError::Io(error.error)),
        }
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
        review_core::json::admit(&payload).map_err(|error| {
            CasError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        let payload_bytes = canonical::canonicalize(&payload)?;
        let content_id = canonical::blob_content_id(&payload_bytes);
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
        // The payload already passed numeric admission; every other envelope field is a
        // string, string list or string-only Producer. Borrow it directly instead of cloning
        // the payload into a second Value and walking the same numeric domain again.
        let bytes = canonical::canonicalize_envelope(&envelope)?;
        if bytes.len() as u64 > MAX_ARTIFACT_ENVELOPE_BYTES {
            return Err(artifact_envelope_too_large(bytes.len() as u64));
        }
        self.put(&payload_bytes)?;
        self.put_artifact_bytes(&envelope.artifact_id, &bytes)?;
        Ok((envelope.artifact_id.clone(), envelope))
    }

    fn put_artifact_bytes(&self, artifact_id: &str, bytes: &[u8]) -> Result<(), CasError> {
        if bytes.len() as u64 > MAX_ARTIFACT_ENVELOPE_BYTES {
            return Err(artifact_envelope_too_large(bytes.len() as u64));
        }
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
        crate::local_fs::create_private_dir_all(directory)?;
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
        let _span = review_core::perf_trace::span(review_core::perf_trace::Phase::CasGet);
        let bytes = self
            .read_bounded_bytes(digest, max_bytes)
            .inspect_err(|_| {
                self.validated.lock().expect("cas validated").remove(digest);
            })?;
        // Verify on read: a CAS that trusts its own filenames cannot detect corruption at all.
        let fingerprint = canonical::blob_content_id(&bytes);
        let checked = if fingerprint == digest
            || self
                .validated
                .lock()
                .expect("cas validated")
                .get(digest, &fingerprint)
                .is_some()
        {
            Ok(())
        } else {
            decode_artifact_bytes(digest, &bytes).map(|mut envelope| {
                compact_arrays(&mut envelope.payload);
                self.validated.lock().expect("cas validated").insert(
                    digest,
                    fingerprint,
                    Arc::new(envelope),
                );
            })
        };
        self.finish_read(digest, checked)?;
        Ok(bytes)
    }

    /// Read and validate a typed envelope. Owned callers retain their API; projection readers
    /// can share immutable decoded data through [`Cas::get_artifact_shared`].
    pub fn get_artifact(&self, digest: &str) -> Result<review_core::ArtifactEnvelope, CasError> {
        self.get_artifact_shared(digest)
            .map(|envelope| (*envelope).clone())
    }

    /// Reuse fully validated decoding only after hashing the current file's complete raw
    /// bytes. Metadata is never an integrity key. A miss decodes the same open handle, then
    /// checks that the bytes decoded still have the observed fingerprint.
    pub fn get_artifact_shared(
        &self,
        digest: &str,
    ) -> Result<Arc<review_core::ArtifactEnvelope>, CasError> {
        let _span = review_core::perf_trace::span(review_core::perf_trace::Phase::CasGetArtifact);
        let result = (|| {
            let mut opened = self.open_for_verified_read(digest)?;
            if opened.len() > MAX_ARTIFACT_ENVELOPE_BYTES {
                return Err(CasError::Io(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    format!(
                        "CAS object {digest} is {} bytes; limit is {MAX_ARTIFACT_ENVELOPE_BYTES}",
                        opened.len()
                    ),
                )));
            }
            let (fingerprint, size) = self.fingerprint(&mut opened)?;
            self.decode_opened(&mut opened, fingerprint, size)
        })();
        self.finish_read(digest, result)
    }

    /// Mixed raw/typed evidence retains the content-domain distinction. A raw JSON blob
    /// never gains artifact authority, even when it spells an envelope.
    pub fn get_optional_artifact(
        &self,
        digest: &str,
    ) -> Result<Option<review_core::ArtifactEnvelope>, CasError> {
        self.get_optional_artifact_shared(digest)
            .map(|value| value.map(|envelope| (*envelope).clone()))
    }

    /// Shared form of [`Cas::get_optional_artifact`], with the same fresh raw-byte checks.
    pub fn get_optional_artifact_shared(
        &self,
        digest: &str,
    ) -> Result<Option<Arc<review_core::ArtifactEnvelope>>, CasError> {
        let result = (|| {
            let mut opened = self.open_for_verified_read(digest)?;
            let (fingerprint, size) = self.fingerprint(&mut opened)?;
            if fingerprint == digest {
                return Ok(None);
            }
            self.decode_opened(&mut opened, fingerprint, size).map(Some)
        })();
        self.finish_read(digest, result)
    }

    fn fingerprint(&self, opened: &mut OpenedCasObject) -> Result<(String, u64), CasError> {
        let result = with_verify_scratch(|buffer| {
            // Metadata frames the read, never establishes integrity. One extra byte detects
            // growth without letting a concurrently appended file make a typed read unbounded.
            let bounded = Read::by_ref(&mut opened.file).take(opened.stored_len.saturating_add(1));
            canonical::blob_content_id_reader_with_buffer(bounded, buffer).map_err(CasError::Io)
        })?;
        if result.1 != opened.stored_len {
            return Err(CasError::Corrupt {
                digest: opened.digest.clone(),
            });
        }
        Ok(result)
    }

    fn decode_opened(
        &self,
        opened: &mut OpenedCasObject,
        fingerprint: String,
        size: u64,
    ) -> Result<Arc<review_core::ArtifactEnvelope>, CasError> {
        if size > MAX_ARTIFACT_ENVELOPE_BYTES {
            return Err(CasError::Corrupt {
                digest: opened.digest.clone(),
            });
        }
        if let Some(envelope) = self
            .validated
            .lock()
            .expect("cas validated")
            .get(&opened.digest, &fingerprint)
        {
            return Ok(envelope);
        }
        opened.file.rewind()?;
        let mut bytes = Vec::with_capacity(size as usize);
        Read::by_ref(&mut opened.file)
            .take(size + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size || canonical::blob_content_id(&bytes) != fingerprint {
            return Err(CasError::Corrupt {
                digest: opened.digest.clone(),
            });
        }
        let mut envelope = decode_artifact_bytes(&opened.digest, &bytes)?;
        compact_arrays(&mut envelope.payload);
        let envelope = Arc::new(envelope);
        self.validated.lock().expect("cas validated").insert(
            &opened.digest,
            fingerprint,
            Arc::clone(&envelope),
        );
        Ok(envelope)
    }

    fn finish_read<T>(&self, digest: &str, result: Result<T, CasError>) -> Result<T, CasError> {
        match &result {
            Ok(_) => READ_TRACES.with(|traces| {
                for trace in traces
                    .borrow_mut()
                    .iter_mut()
                    .filter(|trace| trace.cas == self as *const Self as usize)
                {
                    trace.ids.insert(digest.to_owned());
                }
            }),
            Err(_) => self.validated.lock().expect("cas validated").remove(digest),
        }
        result
    }

    /// Record successful integrity-checked reads made by `read` on this thread and CAS.
    /// The returned IDs are an observation closure, never a durable integrity assertion:
    /// callers must verify them again before reusing a derived observation.
    pub fn track_reads<T>(&self, read: impl FnOnce() -> T) -> (T, BTreeSet<String>) {
        READ_TRACES.with(|traces| {
            traces.borrow_mut().push(ReadTrace {
                cas: self as *const Self as usize,
                ids: BTreeSet::new(),
            })
        });
        let guard = ReadTraceGuard;
        let result = read();
        let ids = READ_TRACES.with(|traces| {
            std::mem::take(&mut traces.borrow_mut().last_mut().expect("read trace").ids)
        });
        drop(guard);
        (result, ids)
    }

    /// Private unverified read; only the verifying public methods may expose its bytes.
    fn read_bounded_bytes(&self, digest: &str, max_bytes: u64) -> Result<Vec<u8>, CasError> {
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
        if size > MAX_ARTIFACT_ENVELOPE_BYTES {
            return Err(CasError::Corrupt {
                digest: digest.to_string(),
            });
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
        let result = (|| {
            let mut opened = self.open_for_verified_read(digest)?;
            let (fingerprint, size) = self.fingerprint(&mut opened)?;
            if fingerprint != digest {
                self.decode_opened(&mut opened, fingerprint, size)?;
            }
            Ok(size)
        })();
        self.finish_read(digest, result)
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

    /// Whether an object is filed under `digest`, without reading or verifying it. The event
    /// store asks this under its write lock, where collection also removes objects, so an
    /// append can never commit a reference to an object a concurrent sweep removed.
    pub fn is_filed(&self, digest: &str) -> bool {
        valid_digest(digest) && fs::symlink_metadata(self.path_for(digest)).is_ok()
    }

    /// Every object filed in this CAS: its digest, stored length and modification time. Only a
    /// regular file whose two-level name spells a valid digest is an object; temporary files
    /// of an in-flight `put` and anything else are not listed.
    pub fn filed_objects(&self) -> Result<Vec<FiledObject>, CasError> {
        let mut objects = Vec::new();
        for prefix in fs::read_dir(self.root.join("objects"))? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            let Some(head) = prefix.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            for entry in fs::read_dir(prefix.path())? {
                let entry = entry?;
                let Some(tail) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let digest = format!("sha256:{head}{tail}");
                let metadata = entry.metadata()?;
                if !valid_digest(&digest) || head.len() != 2 || !metadata.is_file() {
                    continue;
                }
                objects.push(FiledObject {
                    digest,
                    len: metadata.len(),
                    modified: metadata.modified()?,
                });
            }
        }
        objects.sort_by(|left, right| left.digest.cmp(&right.digest));
        Ok(objects)
    }

    /// Every CAS digest spelled anywhere in an object's bytes — an envelope's `content_id` and
    /// `input_artifacts`, a Manifest's file digests, any identity a payload names. Read in
    /// bounded chunks; an object that spells none (a source file, a transcript) has no edge.
    /// Collection walks reachability through this, so a reference is never missed however a
    /// record nests it; a spelling that is not a reference only keeps an object longer.
    pub fn spelled_digests(&self, digest: &str) -> Result<Vec<String>, CasError> {
        if !valid_digest(digest) {
            return Err(CasError::InvalidDigest(digest.to_string()));
        }
        let mut file =
            fs::File::open(self.path_for(digest)).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => CasError::NotFound {
                    digest: digest.to_string(),
                },
                _ => CasError::Io(error),
            })?;
        let mut found = BTreeSet::new();
        let mut window: Vec<u8> = Vec::with_capacity(STREAM_BUFFER_BYTES + SPELLED_DIGEST_LEN);
        let mut chunk = vec![0_u8; STREAM_BUFFER_BYTES];
        loop {
            let read = file.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            window.extend_from_slice(&chunk[..read]);
            scan_digests(&window, &mut found);
            // Keep the tail that could still begin a spelling the next chunk completes.
            let keep = window.len().min(SPELLED_DIGEST_LEN - 1);
            window.drain(..window.len() - keep);
        }
        Ok(found.into_iter().collect())
    }

    /// Remove one object. Only Task collection calls this (ADR-0135), under the event store's
    /// exclusive write lock and only for an object no uncollected record reaches. Survey
    /// collection (ADR-0159 clause 13) removes a Survey's record itself, through directory
    /// descriptors that follow no link, under the Project Store's lock and only after the
    /// Survey's tombstone is in the log; no other command removes a CAS object.
    pub fn remove_unreachable(&self, digest: &str) -> Result<u64, CasError> {
        if !valid_digest(digest) {
            return Err(CasError::InvalidDigest(digest.to_string()));
        }
        let path = self.path_for(digest);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            return Err(CasError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("CAS object is not a regular file: {digest}"),
            )));
        }
        fs::remove_file(&path)?;
        self.durable.lock().expect("cas durable").remove(&path);
        self.pending.lock().expect("cas pending").remove(&path);
        Ok(metadata.len())
    }
}

/// One object [`Cas::filed_objects`] lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiledObject {
    pub digest: String,
    pub len: u64,
    pub modified: std::time::SystemTime,
}

/// `sha256:` and 64 lowercase hex digits.
const SPELLED_DIGEST_LEN: usize = 7 + 64;

pub(crate) fn scan_digests(bytes: &[u8], found: &mut BTreeSet<String>) {
    let mut from = 0;
    while let Some(offset) = bytes[from..]
        .windows(7)
        .position(|window| window == b"sha256:")
    {
        let start = from + offset;
        let end = start + SPELLED_DIGEST_LEN;
        if let Some(candidate) = bytes.get(start..end)
            && candidate[7..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            found.insert(String::from_utf8_lossy(candidate).into_owned());
        }
        from = start + 1;
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
    decode_artifact_bytes(digest, bytes).map(|_| ())
}

fn decode_artifact_bytes(
    digest: &str,
    bytes: &[u8],
) -> Result<review_core::ArtifactEnvelope, CasError> {
    if bytes.len() as u64 > MAX_ARTIFACT_ENVELOPE_BYTES {
        return Err(CasError::Corrupt {
            digest: digest.to_string(),
        });
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| CasError::Corrupt {
        digest: digest.to_string(),
    })?;
    if canonical::is_canonical(&value, bytes).ok() != Some(true) {
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
    Ok(envelope)
}

fn artifact_envelope_too_large(size: u64) -> CasError {
    CasError::Io(std::io::Error::new(
        std::io::ErrorKind::FileTooLarge,
        format!("typed artifact envelope is {size} bytes; limit is {MAX_ARTIFACT_ENVELOPE_BYTES}"),
    ))
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
fn fsync(file: &fs::File) -> std::io::Result<()> {
    nix::unistd::fsync(file).map_err(std::io::Error::from)
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

    fn cached_probe(cas: &Cas, value: Value) -> String {
        cas.put_artifact(
            "af/CacheProbe@1",
            review_core::Producer::KernelOperation {
                run_id: "cache-probe".into(),
                node_id: None,
                operation_id: "read@1".into(),
            },
            vec![],
            None,
            value,
        )
        .unwrap()
        .0
    }

    #[test]
    fn validated_cache_reuses_decoding_but_rejects_in_place_replacement_deletion_and_corruption() {
        let (_dir, cas) = cas();
        let id = cached_probe(&cas, serde_json::json!({"value": "first"}));
        let original = fs::read(cas.path_for(&id)).unwrap();
        let first = cas.get_artifact_shared(&id).unwrap();
        let second = cas.get_artifact_shared(&id).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        cas.verify(&id).unwrap();
        assert!(Arc::ptr_eq(&first, &cas.get_artifact_shared(&id).unwrap()));
        for damage in ["in_place", "replace", "delete", "noncanonical"] {
            let path = cas.path_for(&id);
            let mut damaged = original.clone();
            let offset = damaged
                .windows(5)
                .position(|window| window == b"first")
                .unwrap();
            damaged[offset..offset + 5].copy_from_slice(b"other");
            match damage {
                "delete" => fs::remove_file(&path).unwrap(),
                "replace" => {
                    let mut replacement =
                        tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
                    replacement.write_all(&damaged).unwrap();
                    replacement.persist(&path).unwrap();
                }
                "noncanonical" => {
                    fs::write(&path, serde_json::to_vec_pretty(&*first).unwrap()).unwrap()
                }
                _ => {
                    let modified = fs::metadata(&path).unwrap().modified().unwrap();
                    let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
                    file.write_all(&damaged).unwrap();
                    file.set_times(fs::FileTimes::new().set_modified(modified))
                        .unwrap();
                    assert_eq!(fs::metadata(&path).unwrap().len(), original.len() as u64);
                }
            }
            assert!(cas.verify(&id).is_err(), "{damage}");
            assert!(cas.get_artifact_shared(&id).is_err(), "{damage}");
            assert!(cas.get_optional_artifact(&id).is_err(), "{damage}");
            assert!(!cas.validated.lock().unwrap().entries.contains_key(&id));
            fs::write(&path, &original).unwrap();
            assert_eq!(*cas.get_artifact_shared(&id).unwrap(), *first);
        }
    }

    #[test]
    fn fingerprint_refuses_growth_after_open_without_reading_an_unbounded_tail() {
        let (_dir, cas) = cas();
        let id = cached_probe(&cas, serde_json::json!({"value": "bounded"}));
        cas.get_artifact_shared(&id).unwrap();
        let mut opened = cas.open_for_verified_read(&id).unwrap();
        let length = opened.len();
        let mut appending = fs::OpenOptions::new()
            .append(true)
            .open(cas.path_for(&id))
            .unwrap();
        appending
            .write_all(&vec![b' '; STREAM_BUFFER_BYTES * 2])
            .unwrap();
        assert!(matches!(
            cas.fingerprint(&mut opened),
            Err(CasError::Corrupt { .. })
        ));
        assert_eq!(opened.file.stream_position().unwrap(), length + 1);
        assert!(cas.get_artifact_shared(&id).is_err());
        assert!(!cas.validated.lock().unwrap().entries.contains_key(&id));
    }

    #[test]
    fn validated_cache_bounds_entries_and_decoded_memory_and_is_thread_safe() {
        let (_dir, cas) = cas();
        let first_id = cached_probe(&cas, serde_json::json!({"number": 0}));
        cas.get_artifact_shared(&first_id).unwrap();
        for number in 1..=VALIDATED_CACHE_ENTRIES {
            let id = cached_probe(&cas, serde_json::json!({"number": number}));
            cas.get_artifact_shared(&id).unwrap();
        }
        {
            let cache = cas.validated.lock().unwrap();
            assert_eq!(cache.entries.len(), VALIDATED_CACHE_ENTRIES);
            assert!(!cache.entries.contains_key(&first_id));
            assert!(cache.bytes <= VALIDATED_CACHE_BYTES);
        }
        // Exercise the byte ceiling without publishing a huge wire fixture or allocating
        // a whole limit's worth of live data. Each cache entry retains its own Arc charge.
        let large = Arc::new(review_core::ArtifactEnvelope {
            payload: serde_json::json!({"dense": (0..10_000).map(|n| (n.to_string(), Value::Null)).collect::<serde_json::Map<_, _>>()}),
            ..(*cas.get_artifact_shared(&first_id).unwrap()).clone()
        });
        let mut cache = ValidatedArtifacts::default();
        for number in 0..32 {
            cache.insert(
                &format!("id-{number}"),
                "fingerprint".into(),
                Arc::clone(&large),
            );
            assert!(cache.bytes <= VALIDATED_CACHE_BYTES);
        }
        assert!(!cache.entries.is_empty());
        assert!(cache.entries.len() < 32);
        let expected = cas.get_artifact_shared(&first_id).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let expected = &expected;
                let cas = &cas;
                let id = &first_id;
                scope.spawn(move || {
                    for _ in 0..4 {
                        assert!(Arc::ptr_eq(expected, &cas.get_artifact_shared(id).unwrap()));
                        cas.verify(id).unwrap();
                    }
                });
            }
        });
    }

    #[test]
    fn read_traces_are_nested_cas_scoped_and_unwind_safe() {
        let (_left_dir, left) = cas();
        let (_right_dir, right) = cas();
        let a = left.put(b"a").unwrap();
        let b = right.put(b"b").unwrap();
        let (((), inner), outer) = left.track_reads(|| {
            left.track_reads(|| {
                left.verify(&a).unwrap();
                right.get(&b).unwrap();
            })
        });
        assert_eq!(inner, BTreeSet::from([a.clone()]));
        assert_eq!(outer, inner);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            left.track_reads(|| panic!("probe"))
        }));
        assert!(READ_TRACES.with(|traces| traces.borrow().is_empty()));
    }

    /// Another writer publishes the object between this call's existence check and its move:
    /// the publication keeps that writer's directory entry (the same inode), verifies its bytes
    /// and succeeds, in both the buffered and the streamed path; a different object there is
    /// refused, never replaced.
    #[test]
    fn publication_never_replaces_an_object_another_writer_published() {
        use std::os::unix::fs::MetadataExt;
        let (_dir, cas) = cas();
        let bytes = b"published once".as_slice();
        let digest = canonical::blob_content_id(bytes);
        let final_path = cas.path_for(&digest);
        let shard = final_path.parent().unwrap().to_path_buf();
        crate::local_fs::create_private_dir_all(&shard).unwrap();
        fs::write(&final_path, bytes).unwrap();
        let inode = fs::metadata(&final_path).unwrap().ino();
        cas.publish_object(&shard, &final_path, bytes).unwrap();
        assert_eq!(fs::metadata(&final_path).unwrap().ino(), inode);

        let mut temporary = tempfile::NamedTempFile::new_in(&shard).unwrap();
        temporary.write_all(bytes).unwrap();
        let mut buffer = vec![0; 64];
        let size = bytes.len() as u64;
        assert_eq!(
            cas.publish_streamed_object(temporary, digest.clone(), size, &mut buffer)
                .unwrap(),
            (digest.clone(), size)
        );
        assert_eq!(fs::metadata(&final_path).unwrap().ino(), inode);
        assert_eq!(fs::read(&final_path).unwrap(), bytes);

        fs::write(&final_path, b"other bytes").unwrap();
        let inode = fs::metadata(&final_path).unwrap().ino();
        assert!(matches!(
            cas.publish_object(&shard, &final_path, bytes),
            Err(CasError::Corrupt { .. })
        ));
        assert_eq!(fs::metadata(&final_path).unwrap().ino(), inode);
        assert_eq!(fs::read(&final_path).unwrap(), b"other bytes");
    }

    #[test]
    fn typed_read_retains_exact_identity_and_rechecks_replaced_bytes() {
        let (dir, cas) = cas();
        let (id, envelope) = cas
            .put_artifact(
                "af/ReadProbe@1",
                review_core::Producer::KernelOperation {
                    run_id: "read-probe".into(),
                    node_id: None,
                    operation_id: "capture@1".into(),
                },
                vec![],
                None,
                serde_json::json!({"value": "captured", "nested": [1, 2]}),
            )
            .unwrap();
        assert_eq!(cas.get_artifact(&id).unwrap(), envelope);
        cas.flush().unwrap();
        let reopened = Cas::open_existing(dir.path()).unwrap();
        assert_eq!(reopened.get_artifact(&id).unwrap(), envelope);
        let bytes = cas.get(&id).unwrap();
        let raw_id = cas.put(&bytes).unwrap();
        assert_ne!(raw_id, id);
        assert!(cas.get_json(&raw_id).is_ok(), "raw JSON remains readable");
        assert!(
            matches!(cas.get_artifact(&raw_id), Err(CasError::Corrupt { .. })),
            "raw content identity cannot impersonate an artifact identity"
        );
        for replacement in [
            serde_json::to_vec_pretty(&envelope).unwrap(),
            br#"{"artifact_id":"forged"}"#.to_vec(),
            bytes[..bytes.len() - 1].to_vec(),
            [bytes.as_slice(), b"\n"].concat(),
        ] {
            fs::write(cas.path_for(&id), replacement).unwrap();
            assert!(matches!(
                cas.get_artifact(&id),
                Err(CasError::Corrupt { .. })
            ));
            assert!(matches!(
                reopened.get_artifact(&id),
                Err(CasError::Corrupt { .. })
            ));
        }
        fs::write(cas.path_for(&id), &bytes).unwrap();
        assert_eq!(cas.get_artifact(&id).unwrap(), envelope);
        fs::remove_file(cas.path_for(&id)).unwrap();
        assert!(matches!(
            cas.get_artifact(&id),
            Err(CasError::NotFound { .. })
        ));
    }

    #[test]
    fn typed_read_validates_both_digests_and_rejects_duplicate_fields() {
        let (_dir, cas) = cas();
        let (id, envelope) = cas
            .put_artifact(
                "af/ReadProbe@1",
                review_core::Producer::KernelOperation {
                    run_id: "read-probe".into(),
                    node_id: None,
                    operation_id: "capture@1".into(),
                },
                vec![],
                None,
                serde_json::json!({"value": "captured"}),
            )
            .unwrap();
        let mut forged = envelope.clone();
        forged.payload = serde_json::json!({"value": "changed"});
        // Even a recomputed provenance ID cannot hide an unchanged, false content ID.
        forged.artifact_id = canonical::artifact_id(&forged).unwrap();
        let path = cas.path_for(&forged.artifact_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            canonical::canonicalize(&serde_json::to_value(&forged).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            cas.get_artifact(&forged.artifact_id),
            Err(CasError::Corrupt { .. })
        ));
        forged.content_id = canonical::content_id(&forged.payload).unwrap();
        forged.artifact_id = id.clone();
        fs::write(
            cas.path_for(&id),
            canonical::canonicalize(&serde_json::to_value(&forged).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            cas.get_artifact(&id),
            Err(CasError::Corrupt { .. })
        ));
        let bytes = canonical::canonicalize(&serde_json::to_value(&envelope).unwrap()).unwrap();
        let duplicate = [b"{\"artifact_id\":\"ignored\",".as_slice(), &bytes[1..]].concat();
        fs::write(cas.path_for(&id), duplicate).unwrap();
        assert!(matches!(
            cas.get_artifact(&id),
            Err(CasError::Corrupt { .. })
        ));
        assert!(matches!(
            cas.get_artifact("invalid"),
            Err(CasError::InvalidDigest(_))
        ));
    }

    #[test]
    fn opening_an_existing_cas_never_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        assert!(Cas::open_existing(&root).is_err());
        assert!(!root.exists());

        Cas::open(&root).unwrap();
        Cas::open_existing(&root).unwrap();
    }
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

    struct SeekFails {
        current: std::io::Cursor<Vec<u8>>,
    }

    impl Read for SeekFails {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.current.read(buffer)
        }
    }

    impl Seek for SeekFails {
        fn seek(&mut self, _position: std::io::SeekFrom) -> std::io::Result<u64> {
            Err(std::io::Error::other("rewind must not be attempted"))
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
    fn a_large_content_mismatch_never_enters_the_buffered_envelope_fallback() {
        let mut reader = SeekFails {
            current: std::io::Cursor::new(vec![
                b'x';
                usize::try_from(MAX_ARTIFACT_ENVELOPE_BYTES)
                    .unwrap()
                    + 1
            ]),
        };
        let digest = canonical::blob_content_id(b"different bytes");
        let mut scratch = vec![0_u8; 4096];

        assert!(matches!(
            Cas::verify_seekable(&digest, &mut reader, &mut scratch),
            Err(CasError::Corrupt { .. })
        ));
    }

    #[test]
    fn oversized_typed_artifact_envelopes_are_rejected_at_write_time() {
        let (_dir, cas) = cas();
        let bytes = vec![b'x'; usize::try_from(MAX_ARTIFACT_ENVELOPE_BYTES).unwrap() + 1];
        let artifact_id = canonical::blob_content_id(b"typed artifact identity");

        assert!(matches!(
            cas.put_artifact_bytes(&artifact_id, &bytes),
            Err(CasError::Io(error)) if error.kind() == std::io::ErrorKind::FileTooLarge
        ));
    }

    #[test]
    fn mixed_evidence_streams_large_raw_blobs_and_requires_typed_identity() {
        let (_dir, cas) = cas();
        let raw = cas
            .put(&vec![b'x'; MAX_ARTIFACT_ENVELOPE_BYTES as usize + 1])
            .unwrap();
        assert!(cas.get_optional_artifact(&raw).unwrap().is_none());
        let (typed, envelope) = cas
            .put_artifact(
                "af/TaskUsageObservation@1",
                review_core::Producer::Attempt {
                    run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
                    node_id: "node".into(),
                    attempt_id: "01jd8m4qz9k7v3n2p6r8t0w202".into(),
                },
                vec![],
                None,
                json!({"fixture":true}),
            )
            .unwrap();
        assert_eq!(
            cas.get_optional_artifact(&typed).unwrap(),
            Some(envelope.clone())
        );
        let raw_lookalike = cas
            .put(&canonical::canonicalize(&serde_json::to_value(envelope).unwrap()).unwrap())
            .unwrap();
        assert!(cas.get_optional_artifact(&raw_lookalike).unwrap().is_none());
        for id in [raw, typed] {
            fs::write(cas.path_for(&id), b"corrupted").unwrap();
            assert!(matches!(
                cas.get_optional_artifact(&id),
                Err(CasError::Corrupt { .. })
            ));
            fs::remove_file(cas.path_for(&id)).unwrap();
            assert!(matches!(
                cas.get_optional_artifact(&id),
                Err(CasError::NotFound { .. })
            ));
        }
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

    #[test]
    fn every_digest_an_object_spells_is_found_across_chunk_boundaries() {
        let (_dir, cas) = cas();
        let first = cas.put(b"first").unwrap();
        let second = cas.put(b"second").unwrap();
        // The second spelling straddles the first read's 64 KiB boundary.
        let mut bytes = vec![b' '; STREAM_BUFFER_BYTES - 30];
        bytes.extend_from_slice(first.as_bytes());
        bytes.extend_from_slice(b"\",\"");
        bytes.extend_from_slice(second.as_bytes());
        bytes.extend_from_slice(b" sha256:ABC sha256:");
        bytes.extend_from_slice(&[b'a'; 63]);
        let holder = cas.put(&bytes).unwrap();
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(cas.spelled_digests(&holder).unwrap(), expected);
        assert!(cas.spelled_digests(&expected[0]).unwrap().is_empty());
    }

    #[test]
    fn only_objects_are_filed_and_only_collection_removes_one() {
        let (dir, cas) = cas();
        let kept = cas.put(b"kept").unwrap();
        let gone = cas.put(b"gone").unwrap();
        let hex = &gone[7..];
        fs::write(
            dir.path().join("objects").join(&hex[..2]).join(".tmp-x-1"),
            b"t",
        )
        .unwrap();
        let listed: Vec<_> = cas
            .filed_objects()
            .unwrap()
            .into_iter()
            .map(|object| (object.digest, object.len))
            .collect();
        let mut expected = vec![(kept.clone(), 4), (gone.clone(), 4)];
        expected.sort();
        assert_eq!(listed, expected, "a temporary file is not an object");
        assert!(cas.is_filed(&gone));
        assert_eq!(cas.remove_unreachable(&gone).unwrap(), 4);
        assert!(!cas.is_filed(&gone));
        assert!(cas.is_filed(&kept));
        assert!(matches!(
            cas.remove_unreachable("sha256:nope"),
            Err(CasError::InvalidDigest(_))
        ));
    }

    /// The objects both racers of [`two_processes_putting_the_same_objects_never_collide`]
    /// store, in the same order, so their calls meet on the same digest at the same moment.
    const RACED_OBJECTS: usize = 400;

    fn raced_object(n: usize) -> Vec<u8> {
        format!("raced object {n}").into_bytes()
    }

    /// Store every raced object into the CAS at `root` once `go` exists.
    fn race_puts(root: &Path, go: &Path) {
        let cas = Cas::open(root).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !go.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the race never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        for n in 0..RACED_OBJECTS {
            if let Err(error) = cas.put(&raced_object(n)) {
                panic!("put of raced object {n} failed: {error}");
            }
        }
    }

    /// One process of [`two_processes_putting_the_same_objects_never_collide`]; it stores
    /// nothing unless that test started it.
    #[test]
    #[ignore = "one process of two_processes_putting_the_same_objects_never_collide"]
    fn one_of_two_racing_cas_writers() {
        if let (Some(root), Some(go)) = (
            std::env::var_os("AF_CAS_RACE_ROOT"),
            std::env::var_os("AF_CAS_RACE_GO"),
        ) {
            race_puts(Path::new(&root), Path::new(&go));
        }
    }

    /// Issue #159: two `af task plan` into one Store in the same second staged the same object
    /// under one temporary name, a digest prefix and a counter every process starts at zero, so
    /// one truncated the other's file and the loser's rename failed. Two processes, and two
    /// threads in this one, store the same 400 objects at once; every put succeeds, every object
    /// verifies, and no temporary file is left behind.
    #[test]
    fn two_processes_putting_the_same_objects_never_collide() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cas");
        let go = directory.path().join("go");
        let test = format!(
            "{}::one_of_two_racing_cas_writers",
            module_path!().split_once("::").unwrap().1
        );
        let children: Vec<_> = (0..2)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &test, "--ignored", "--test-threads", "1"])
                    .env("AF_CAS_RACE_ROOT", &root)
                    .env("AF_CAS_RACE_GO", &go)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let (root, go) = (root.clone(), go.clone());
                std::thread::spawn(move || race_puts(&root, &go))
            })
            .collect();
        fs::write(&go, b"").unwrap();
        for thread in threads {
            thread.join().unwrap();
        }
        for child in children {
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            // A child that selected no test also exits 0: it must report the racer as passed.
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let cas = Cas::open(&root).unwrap();
        for n in 0..RACED_OBJECTS {
            let digest = canonical::blob_content_id(&raced_object(n));
            assert_eq!(cas.get(&digest).unwrap(), raced_object(n), "object {n}");
        }
        let left: Vec<_> = walk(&root.join("objects"))
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".tmp-"))
            })
            .collect();
        assert!(left.is_empty(), "temporary files left behind: {left:?}");
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
