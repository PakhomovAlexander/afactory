//! The segment writer (design §6).
//!
//! One live segment per process under `v1/segments/<started_unix_ms>-<pid>-<nonce>.jsonl`,
//! opened lazily at the first record and never before, under an exclusive `flock` for the
//! writer's lifetime. Records are encoded into a buffer of `flush_bytes` guarded by a mutex and
//! appended with one `write(2)` of whole lines when the buffer is full, when a span of depth at
//! most 2 ends, and at exit.
//!
//! **One owner.** The open file, every write, the manifest, the seal, the compression and the
//! rename are the consumer's work ([`Writer::consume`] and the sealing below) and no other
//! code's. Appending threads only encode into the buffer and signal [`Shared::wake`].
//!
//! **The consumer thread** (design §6) exists while a run is open: [`Writer::run_started`]
//! starts it when the process opens its first `af.run` span and [`Writer::run_ended`] stops it
//! when the last one ends. It waits on one condition with one deadline and wakes when a kernel
//! thread appends a record, when the stream queue receives a chunk ([`Writer::stream_chunk`],
//! the hook package P6 fills), or when the deadline passes — the oldest pending record's time
//! plus `flush_latency_ms`. After each wake it drains the stream queue, writes every full
//! buffer, and writes the current buffer when the queue is empty or the deadline has passed;
//! then it recomputes the deadline and sleeps. There is no periodic timer and no polling: a
//! quiet writer sleeps without a deadline. While no consumer thread runs (before the first
//! `af.run`, after the last, and in a process that opens none) the appending thread runs the
//! same consumer code after each append, under the consumer's own mutex, so there is never a
//! second owner.
//!
//! **Bounded shutdown.** Stopping signals the consumer and waits for it at most
//! [`layout::LOCK_WAIT`] (2 s). A consumer that does not stop in time — a write blocked on the
//! volume — is abandoned: no other thread writes, seals, compresses or replaces the segment, the
//! records never written are absent, the lock ends with the process, and the next reader
//! recovers the flushed prefix.
//!
//! **Segment limit.** `segment_max_bytes` bounds the file across every record kind. The last
//! [`TERMINAL_RESERVE_BYTES`] are kept for terminal records (`problem`, `seal`): past the rest no
//! record of any kind is accepted, bounded in-memory counters are kept, one `problem` is
//! recorded, and at exit the seal is written from the reserve with the `dropped` count.
//!
//! **Publication.** Content becomes a blob in this order: the blob is written to a temporary
//! file; `gc.lock` is taken shared without waiting; the file is renamed into place; the `blob`
//! object (once per digest per segment), the `blob_ref` occurrence and the owning record are
//! appended and the buffer flushed; the lock is released. When the lock is not free (a sweep
//! holds it) the blob is dropped and counted, and the occurrence says `state = dropped`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use nix::fcntl::FlockArg;
use serde_json::Value;

use super::{Clock, TERMINAL_RESERVE_BYTES, new_segment_id};
use crate::blob::{self, Media, Prepared};
use crate::dirs::{self, Accounted};
use crate::layout::{self, Confined, FileIdentity, Held, Layout};
use crate::level::{Level, Settings};
use crate::record::{
    self, Attributes, Attrs, BlobObject, BlobRef, BlobState, ConfigPart, ConfigRow, Counters,
    Demand, Event, Finding, Host, MAX_KEY_BYTES, MAX_STRING_BYTES, Node, OCCURRENCE_PREFIX,
    Oversized, Problem, ProblemWhat, RECORD_MAX_BYTES, Record, Seal, SealExit, SegmentHeader,
    SpanEnd, SpanKind, SpanStart, SpanStatus, schema,
};
use crate::redact::{self, Redactor};
use crate::trace;

/// A step of the blob publication protocol, offered to a [`PublishHook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStep {
    /// The blob is in its temporary file.
    Staged,
    /// `gc.lock` is held shared.
    Locked,
    /// The blob file is in place, no reference to it written yet.
    Renamed,
    /// The references are flushed; the lock is not yet released.
    Flushed,
}

/// What a [`PublishHook`] tells the writer to do after a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    Continue,
    /// Stop as a killed process would: nothing more is written, the lock on the segment ends,
    /// and the segment is left unsealed for a reader to recover.
    Crash,
}

/// Observes, pauses or crashes the publication protocol at each step; fixtures use it to prove
/// the protocol's order. A hook that blocks pauses the publisher.
pub type PublishHook = Arc<dyn Fn(PublishStep) -> HookAction + Send + Sync>;

/// Called by the segment's owner before each `write(2)` with the write's ordinal (1 for the
/// first): fixtures block it to stall the consumer as a write blocked on the volume would, or
/// return the error a full quota would give, which fails the write.
pub type WriteHook = Arc<dyn Fn(u64) -> std::io::Result<()> + Send + Sync>;

/// The writer's clock in unix nanoseconds, injected by fixtures that compare what two runs
/// recorded; production writers read the monotonic clock from the header's origin.
pub type ClockHook = Arc<dyn Fn() -> u128 + Send + Sync>;

/// The stream queue of design §7.3, filled by package P6: the consumer drains it at every wake,
/// before it writes.
pub trait StreamQueue: Send + Sync {
    /// Frame and record every queued chunk through `writer`; whether the queue is empty now.
    fn drain(&self, writer: &Writer) -> bool;
}

/// Registration of the telemetry directory before the first write (design §6).
#[derive(Debug, Clone)]
pub struct Registration {
    /// `$XDG_STATE_HOME/af/telemetry-dirs.v1.json`.
    pub registry: PathBuf,
    /// Every directory the storage inventory accounts on its own.
    pub accounted: Vec<Accounted>,
    /// Why `accounted` is known incomplete (the Store registry could not be read): the overlap
    /// check is then unknown and registration refuses, one problem and no telemetry.
    pub accounted_unknown: Option<String>,
}

/// What a writer is configured with.
#[derive(Clone)]
pub struct WriterOptions {
    pub layout: Layout,
    pub level: Level,
    pub flush_bytes: u64,
    pub segment_max_bytes: u64,
    pub content_max_blob_bytes: u64,
    pub af_version: String,
    pub af_commit: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub redactor: Redactor,
    pub registration: Option<Registration>,
    pub publish_hook: Option<PublishHook>,
    /// `[telemetry] flush_latency_ms`: the longest a record waits in the buffer while a run is
    /// open.
    pub flush_latency_ms: u64,
    pub write_hook: Option<WriteHook>,
    pub clock: Option<ClockHook>,
    /// The segment id, for fixtures that compare what two runs recorded; `None` names a fresh
    /// one at the first record.
    pub segment_id: Option<String>,
}

impl WriterOptions {
    /// Options with the `[telemetry]` defaults for `level` in `root`, this process's argv and
    /// working directory, no registration and no hook.
    pub fn new(root: impl Into<PathBuf>, level: Level) -> Self {
        let defaults = Settings::default();
        Self {
            layout: Layout::new(root),
            level,
            flush_bytes: defaults.flush_bytes,
            segment_max_bytes: defaults.segment_max_bytes,
            content_max_blob_bytes: defaults.content_max_blob_bytes,
            af_version: env!("CARGO_PKG_VERSION").to_string(),
            af_commit: "unknown".to_string(),
            argv: std::env::args().collect(),
            cwd: std::env::current_dir().unwrap_or_default(),
            redactor: Redactor::for_this_process(),
            registration: None,
            publish_hook: None,
            flush_latency_ms: defaults.flush_latency_ms,
            write_hook: None,
            clock: None,
            segment_id: None,
        }
    }

    /// The bounds of `settings`.
    pub fn with_settings(mut self, settings: &Settings) -> Self {
        self.level = settings.level;
        self.flush_bytes = settings.flush_bytes;
        self.segment_max_bytes = settings.segment_max_bytes;
        self.content_max_blob_bytes = settings.content_max_blob_bytes;
        self.flush_latency_ms = settings.flush_latency_ms;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing opened yet: no file, no directory.
    Unopened,
    Open,
    /// The seal is appended; no other record is accepted.
    Sealing,
    Sealed,
    /// A telemetry-only failure: nothing more is written, the run continues.
    Failed,
    /// Stopped as a killed process would be.
    Crashed,
    /// The consumer thread did not stop within the bounded shutdown: nothing more is written,
    /// sealed or replaced by any thread, and the segment is left for the next reader.
    Abandoned,
}

/// Who owns the segment's writes now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// No consumer thread: the appending thread runs the consumer code.
    Inline,
    /// The consumer thread owns every write.
    Running,
    /// The consumer thread is asked to write what is pending and exit.
    Stopping,
}

/// The buffer and the in-memory counters, guarded by one mutex. Appending threads hold it only
/// to encode.
struct Pending {
    state: State,
    segment_id: String,
    next_seq: u64,
    /// Full buffers waiting for the consumer, oldest first.
    ready: VecDeque<Vec<u8>>,
    buffer: Vec<u8>,
    flush_requested: bool,
    /// Bytes of every record accepted, written or buffered.
    accepted_bytes: u64,
    exhausted: bool,
    spans: u64,
    events: u64,
    blobs: u64,
    dropped: u64,
    dropped_chunks: u64,
    problems: BTreeSet<(ProblemWhat, String)>,
    /// Problems that could not reach the segment (it never opened, or writes failed).
    unwritten: Vec<(ProblemWhat, String)>,
    blobs_in_segment: BTreeSet<String>,
    referenced: BTreeSet<String>,
    mode: Mode,
    /// `af.run` spans open in this process: the consumer thread lives while one is.
    runs_open: u64,
    /// Records accepted so far; the consumer wakes when it has seen fewer.
    appends: u64,
    /// When the oldest record not yet written was accepted.
    oldest: Option<Instant>,
    /// Flushes asked for and flushes done: a publication waits for its references.
    flush_asked: u64,
    flush_done: u64,
    /// The stream queue received a chunk since the consumer last drained it.
    stream_pending: bool,
    /// The consumer thread has exited.
    consumer_exited: bool,
}

/// The consumer's own state: the open file and its lock, and the `segments/` directory it was
/// created in, held open so the manifest, the seal, the compression, the rename and the removal
/// of the live file happen relative to it whatever later becomes of the paths above it.
struct Consumer {
    file: Option<Held>,
    segments: Option<Confined>,
}

/// The telemetry root and its `v1/` as the writer opened them at its first write, held for the
/// writer's whole lifetime with the `blobs/` and prefix directories derived from them: every
/// blob is staged, placed and removed, and `gc.lock` taken, relative to these descriptors with
/// no link followed, so a root renamed and replaced by a link afterwards takes nothing outside
/// the root the writer opened.
struct Tree {
    _root: Confined,
    versioned: Confined,
    blobs: Mutex<BlobDirs>,
}

/// `blobs/` and its prefix directories, each opened once from its parent and kept.
#[derive(Default)]
struct BlobDirs {
    blobs: Option<Confined>,
    prefixes: BTreeMap<String, Confined>,
}

impl Tree {
    /// The prefix directory `blobs/<prefix>/`, created when absent, derived from the held
    /// descriptors.
    fn prefix(&self, prefix: &str) -> std::io::Result<Confined> {
        let mut dirs = lock(&self.blobs);
        let dirs = &mut *dirs;
        let blobs = match &mut dirs.blobs {
            Some(blobs) => blobs,
            empty => empty.insert(self.versioned.create_child_dir("blobs")?),
        };
        if !dirs.prefixes.contains_key(prefix) {
            let opened = blobs.create_child_dir(prefix)?;
            dirs.prefixes.insert(prefix.to_string(), opened);
        }
        dirs.prefixes[prefix].try_clone()
    }
}

/// The problem an abandoned consumer leaves.
pub const ABANDONED: &str = "the telemetry consumer did not stop within 2 s; the segment is \
                             left unsealed for the next reader to recover";

/// Shared between every handle of one writer.
pub struct Shared {
    options: WriterOptions,
    clock: Clock,
    pending: Mutex<Pending>,
    /// Signalled on every append; the consumer thread waits on it.
    pub wake: Condvar,
    /// Signalled by the consumer thread after each write and when it exits.
    done: Condvar,
    consumer: Mutex<Consumer>,
    stream: Mutex<Option<Arc<dyn StreamQueue>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Writes made so far, for the write hook.
    writes: std::sync::atomic::AtomicU64,
    /// Set once, when the consumer opens the segment.
    tree: OnceLock<Tree>,
}

/// A sealed segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub segment_id: String,
    pub path: PathBuf,
    pub refs: PathBuf,
}

/// One process's segment writer. Cloning shares it.
#[derive(Clone)]
pub struct Writer {
    shared: Arc<Shared>,
}

/// One encoded line of a batch, with the blob digests its acceptance makes known.
struct Line {
    bytes: Vec<u8>,
    kind: &'static str,
    /// The digest of a `blob` object: known in this segment once the line is accepted.
    object: Option<String>,
    /// The digest a stored `blob_ref` names: in the `.refs` manifest once the line is accepted.
    referenced: Option<String>,
}

/// Records encoded under the pending lock, accepted or refused together.
pub struct Batch<'a> {
    segment_id: &'a str,
    base: u64,
    lines: Vec<Line>,
    known: &'a BTreeSet<String>,
    counts: SealCounts,
}

/// The in-memory counters a seal carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SealCounts {
    pub spans: u64,
    pub events: u64,
    pub blobs: u64,
    pub dropped: u64,
    pub dropped_chunks: u64,
}

impl Batch<'_> {
    /// The segment id records are built in.
    pub fn segment_id(&self) -> &str {
        self.segment_id
    }

    /// Whether this segment, or this batch, already holds the `blob` object of `digest`.
    pub fn knows_blob(&self, digest: &str) -> bool {
        self.known.contains(digest)
            || self
                .lines
                .iter()
                .any(|line| line.object.as_deref() == Some(digest))
    }

    /// The `seq` the next pushed record takes.
    pub fn seq(&self) -> u64 {
        self.base + self.lines.len() as u64
    }

    /// Encode `record` under the bound with the next `seq`; `None` when it cannot fit.
    pub fn push(&mut self, mut record: Record, counters: Counters) -> Option<u64> {
        let seq = self.seq();
        record.set_seq(seq);
        let kind = record.kind();
        let (object, referenced) = match &record {
            Record::Blob(object) => (Some(object.digest.clone()), None),
            Record::BlobRef(occurrence) if occurrence.state == BlobState::Stored => {
                (None, occurrence.digest.clone())
            }
            _ => (None, None),
        };
        let bytes = record::encode(&mut record, counters)?;
        self.lines.push(Line {
            bytes,
            kind,
            object,
            referenced,
        });
        Some(seq)
    }
}

/// Build a batch against `pending` without changing it.
fn build_batch<T>(
    pending: &Pending,
    build: impl FnOnce(&mut Batch<'_>) -> Option<T>,
) -> (Option<T>, Vec<Line>) {
    let mut batch = Batch {
        segment_id: &pending.segment_id,
        base: pending.next_seq,
        lines: Vec::new(),
        known: &pending.blobs_in_segment,
        counts: SealCounts {
            spans: pending.spans,
            events: pending.events,
            blobs: pending.blobs,
            dropped: pending.dropped,
            dropped_chunks: pending.dropped_chunks,
        },
    };
    let value = build(&mut batch);
    (value, batch.lines)
}

/// The bytes `lines` take in the segment.
fn line_bytes(lines: &[Line]) -> u64 {
    lines.iter().map(|line| line.bytes.len() as u64 + 1).sum()
}

/// The batch of a publication: the `blob` object of every stored content new to the segment,
/// one `blob_ref` per content (stored or dropped as `stored` says), then the owning record
/// `build` makes from the occurrence ids, at the `seq` right after them.
fn publication<T>(
    pending: &Pending,
    contents: &[Content],
    span: Option<&str>,
    build: &mut impl FnMut(&mut Batch<'_>, &[String]) -> Option<T>,
) -> (Option<T>, Vec<Line>) {
    build_batch(pending, |batch| {
        let mut new_objects = BTreeSet::new();
        for content in contents {
            if content.stored && !batch.knows_blob(&content.prepared.digest) {
                new_objects.insert(content.prepared.digest.clone());
            }
        }
        let owner_seq = batch.seq() + new_objects.len() as u64 + contents.len() as u64;
        let span_id = match span {
            Some(span) => span.to_string(),
            None => trace::span_id(batch.segment_id(), owner_seq),
        };
        let mut occurrences = Vec::new();
        for content in contents {
            let prepared = &content.prepared;
            if new_objects.remove(&prepared.digest) {
                batch.push(
                    Record::Blob(BlobObject {
                        seq: 0,
                        schema: schema::BLOB.into(),
                        digest: prepared.digest.clone(),
                        bytes: prepared.retained.len() as u64,
                        stored_bytes: prepared.compressed.len() as u64,
                        redaction: crate::redact::REDACTION_VERSION.into(),
                    }),
                    Counters::default(),
                )?;
            }
            let occurrence = trace::occurrence_id(batch.segment_id(), batch.seq());
            batch.push(
                Record::BlobRef(BlobRef {
                    seq: 0,
                    schema: schema::BLOB_REF.into(),
                    occurrence_id: occurrence.clone(),
                    digest: Some(prepared.digest.clone()),
                    span_id: span_id.clone(),
                    event_seq: owner_seq,
                    source_digest: prepared.source_digest.clone(),
                    original_bytes: prepared.original_bytes,
                    truncated: prepared.truncated,
                    state: if content.stored {
                        BlobState::Stored
                    } else {
                        BlobState::Dropped
                    },
                    media_type: prepared.media_type.into(),
                    redaction: crate::redact::REDACTION_VERSION.into(),
                }),
                Counters::default(),
            )?;
            occurrences.push(occurrence);
        }
        if batch.seq() != owner_seq {
            return None;
        }
        build(batch, &occurrences)
    })
}

/// Why a batch was refused.
enum Refusal {
    /// A record of it could not be encoded under the record bound.
    Unbuilt,
    /// It did not fit the segment, or the segment no longer takes records: the records it held.
    Full(u64),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ns(value: u128) -> String {
    value.to_string()
}

/// The host facts read once per segment; a fact the platform does not deliver is absent.
fn host_facts(near: &std::path::Path) -> Host {
    let cpus = std::thread::available_parallelism()
        .map(|cpus| u32::try_from(cpus.get()).unwrap_or(u32::MAX))
        .unwrap_or(1);
    let existing = near.ancestors().find(|path| path.exists());
    let free_bytes = existing
        .and_then(|path| nix::sys::statvfs::statvfs(path).ok())
        .map(free_of);
    Host {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpus,
        memory_bytes: memory_bytes(),
        load1: load1(),
        free_bytes,
    }
}

/// The available bytes a `statvfs` reports; its field types differ per platform.
#[allow(clippy::useless_conversion)]
fn free_of(stat: nix::sys::statvfs::Statvfs) -> u64 {
    u64::from(stat.blocks_available()).saturating_mul(u64::from(stat.fragment_size()))
}

#[cfg(target_os = "linux")]
fn memory_bytes() -> Option<u64> {
    use nix::unistd::{SysconfVar, sysconf};
    let pages = sysconf(SysconfVar::_PHYS_PAGES).ok().flatten()?;
    let size = sysconf(SysconfVar::PAGE_SIZE).ok().flatten()?;
    u64::try_from(pages)
        .ok()?
        .checked_mul(u64::try_from(size).ok()?)
}

#[cfg(not(target_os = "linux"))]
fn memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn load1() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn load1() -> Option<f64> {
    None
}

/// Attributes bounded, with the values that became pending blobs: `named[i]` is the key whose
/// value `contents[i]` holds.
struct Bounded {
    attributes: Attributes,
    counters: Counters,
    contents: Vec<Prepared>,
    named: Vec<String>,
}

/// Name each pending blob's occurrence in the attribute that held its value; an attribute
/// whose content found no occurrence is cut to the marker.
fn fill(attributes: &mut Attributes, named: &[String], occurrences: &[String]) {
    for (index, key) in named.iter().enumerate() {
        if let Some(value) = attributes.get_mut(key) {
            *value = Value::String(match occurrences.get(index) {
                Some(occurrence) => format!("{OCCURRENCE_PREFIX}{occurrence}"),
                None => record::ELLIPSIS.to_string(),
            });
        }
    }
}

/// One content waiting for publication, and how far it got.
struct Content {
    prepared: Prepared,
    staged: Option<blob::Staged>,
    stored: bool,
}

impl Writer {
    /// A writer for `options`; `None` at `off`, where nothing is ever touched.
    pub fn new(options: WriterOptions) -> Option<Self> {
        if !options.level.records() {
            return None;
        }
        Some(Self {
            shared: Arc::new(Shared {
                clock: Clock::start(),
                pending: Mutex::new(Pending {
                    state: State::Unopened,
                    segment_id: String::new(),
                    next_seq: 1,
                    ready: VecDeque::new(),
                    buffer: Vec::new(),
                    flush_requested: false,
                    accepted_bytes: 0,
                    exhausted: false,
                    spans: 0,
                    events: 0,
                    blobs: 0,
                    dropped: 0,
                    dropped_chunks: 0,
                    problems: BTreeSet::new(),
                    unwritten: Vec::new(),
                    blobs_in_segment: BTreeSet::new(),
                    referenced: BTreeSet::new(),
                    mode: Mode::Inline,
                    runs_open: 0,
                    appends: 0,
                    oldest: None,
                    flush_asked: 0,
                    flush_done: 0,
                    stream_pending: false,
                    consumer_exited: true,
                }),
                wake: Condvar::new(),
                done: Condvar::new(),
                consumer: Mutex::new(Consumer {
                    file: None,
                    segments: None,
                }),
                stream: Mutex::new(None),
                thread: Mutex::new(None),
                writes: std::sync::atomic::AtomicU64::new(0),
                tree: OnceLock::new(),
                options,
            }),
        })
    }

    pub fn level(&self) -> Level {
        self.shared.options.level
    }

    pub fn layout(&self) -> &Layout {
        &self.shared.options.layout
    }

    /// Unix nanoseconds now on the writer's clock.
    pub fn now_unix_ns(&self) -> u128 {
        match &self.shared.options.clock {
            Some(clock) => clock(),
            None => self.shared.clock.now_unix_ns(),
        }
    }

    fn origin_unix_ns(&self) -> u128 {
        match &self.shared.options.clock {
            Some(clock) => clock(),
            None => self.shared.clock.origin_unix_ns(),
        }
    }

    /// The segment id, once the segment is open.
    pub fn segment_id(&self) -> Option<String> {
        let pending = lock(&self.shared.pending);
        (pending.state != State::Unopened).then(|| pending.segment_id.clone())
    }

    /// The live segment's path, once it is open.
    pub fn path(&self) -> Option<PathBuf> {
        self.segment_id()
            .map(|id| self.shared.options.layout.live_segment(&id))
    }

    /// Every distinct problem this writer met, recorded in the segment or not.
    pub fn problems(&self) -> Vec<(ProblemWhat, String)> {
        let pending = lock(&self.shared.pending);
        let mut all: Vec<(ProblemWhat, String)> = pending.problems.iter().cloned().collect();
        for problem in &pending.unwritten {
            if !all.contains(problem) {
                all.push(problem.clone());
            }
        }
        all
    }

    /// Records and blobs not kept so far.
    pub fn dropped(&self) -> u64 {
        lock(&self.shared.pending).dropped
    }

    /// Whether telemetry stopped after a telemetry-only failure.
    pub fn failed(&self) -> bool {
        matches!(
            lock(&self.shared.pending).state,
            State::Failed | State::Crashed | State::Abandoned
        )
    }

    /// Whether the consumer thread was abandoned at shutdown.
    pub fn abandoned(&self) -> bool {
        lock(&self.shared.pending).state == State::Abandoned
    }

    /// Whether a consumer thread owns the segment's writes now.
    pub fn consumer_running(&self) -> bool {
        lock(&self.shared.pending).mode != Mode::Inline
    }

    fn header(&self, segment_id: &str) -> Record {
        let options = &self.shared.options;
        let redactor = &options.redactor;
        // An option's value under a secret name is withheld first, then each argument is judged
        // whole and bounded.
        let (argv, _) = redact::withhold_option_values(&options.argv);
        let (argv, _) =
            record::bound_attributes(Attrs::new().texts("argv", argv), redactor, &mut |_, _| None);
        let argv = match argv.get("argv") {
            Some(Value::Array(values)) => values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        // Free text is judged whole before it is cut: a credential in the working directory
        // leaves the marker, never a prefix of itself.
        let cwd = record::cut(
            &redactor.value(&options.cwd.to_string_lossy()).0,
            MAX_STRING_BYTES,
        );
        // The build's commit is kept when it is a commit id (40 or 64 lowercase hex digits) or
        // a kernel identity, and judged as text otherwise.
        let commit = &options.af_commit;
        let af_commit = if (matches!(commit.len(), 40 | 64) && redact::is_lower_hex(commit))
            || redact::is_kernel_identity(commit)
        {
            options.af_commit.clone()
        } else {
            redactor.value(&options.af_commit).0
        };
        Record::Segment(SegmentHeader {
            seq: 1,
            schema: schema::SEGMENT.into(),
            segment_id: segment_id.to_string(),
            af_version: record::cut(&redactor.value(&options.af_version).0, 128),
            af_commit: record::cut(&af_commit, 128),
            pid: std::process::id(),
            parent_pid: std::os::unix::process::parent_id(),
            argv,
            cwd,
            host: host_facts(options.layout.root()),
            started_unix_ns: ns(self.now_unix_ns()),
            mono_origin_unix_ns: ns(self.origin_unix_ns()),
            level: options.level,
        })
    }

    /// Open the segment in memory at the first record: choose its id and buffer its header,
    /// which the consumer writes at once.
    fn ensure_open(&self, pending: &mut Pending) -> bool {
        match pending.state {
            State::Open => true,
            State::Unopened => {
                let segment_id = match &self.shared.options.segment_id {
                    Some(segment_id) => segment_id.clone(),
                    None => new_segment_id(self.now_unix_ns()),
                };
                let mut header = self.header(&segment_id);
                let Some(line) = record::encode(&mut header, Counters::default()) else {
                    pending.state = State::Failed;
                    pending
                        .unwritten
                        .push((ProblemWhat::Write, "the segment header does not fit".into()));
                    return false;
                };
                pending.segment_id = segment_id;
                pending.next_seq = 2;
                pending.accepted_bytes = line.len() as u64 + 1;
                pending.buffer.extend_from_slice(&line);
                pending.buffer.push(b'\n');
                // An environment the redactor could not read is one problem right after the
                // header, never a silent omission.
                if let Some(reason) = self.shared.options.redactor.home_unknown() {
                    let mut problem = Record::Problem(Problem {
                        seq: 2,
                        schema: schema::PROBLEM.into(),
                        time_unix_ns: ns(self.now_unix_ns()),
                        what: ProblemWhat::Redaction,
                        reason: reason.to_string(),
                    });
                    if let Some(line) = record::encode(&mut problem, Counters::default()) {
                        pending
                            .problems
                            .insert((ProblemWhat::Redaction, reason.to_string()));
                        pending.next_seq = 3;
                        pending.accepted_bytes += line.len() as u64 + 1;
                        pending.buffer.extend_from_slice(&line);
                        pending.buffer.push(b'\n');
                    }
                }
                pending.flush_requested = true;
                pending.appends += 1;
                pending.oldest.get_or_insert_with(Instant::now);
                pending.state = State::Open;
                true
            }
            State::Sealing | State::Sealed | State::Failed | State::Crashed | State::Abandoned => {
                false
            }
        }
    }

    /// Encode a batch under the pending lock and accept or refuse it whole against the segment
    /// limit. `terminal` batches (a problem, the seal) may use the reserve.
    fn commit<T>(
        &self,
        terminal: bool,
        build: impl FnOnce(&mut Batch<'_>) -> Option<T>,
    ) -> Option<T> {
        let mut guard = lock(&self.shared.pending);
        let pending = &mut *guard;
        if !self.ensure_open(pending) {
            if pending.state != State::Unopened {
                pending.dropped += 1;
            }
            return None;
        }
        let (value, lines) = build_batch(pending, build);
        let Some(value) = value else {
            self.refuse(guard, terminal, Refusal::Unbuilt);
            return None;
        };
        if !self.fits(pending, terminal, &lines, 0) {
            let records = lines.len().max(1) as u64;
            self.refuse(guard, terminal, Refusal::Full(records));
            return None;
        }
        self.accept(pending, lines);
        drop(guard);
        self.shared.wake.notify_all();
        Some(value)
    }

    /// Whether `lines`, and `slack` bytes more, fit the segment. `terminal` batches (a problem,
    /// the seal) may use the reserve; nothing else is accepted once the segment is exhausted.
    fn fits(&self, pending: &Pending, terminal: bool, lines: &[Line], slack: u64) -> bool {
        let bytes = line_bytes(lines) + slack;
        let limit = self.shared.options.segment_max_bytes;
        if terminal {
            // Room for the seal is kept until the seal itself is written.
            let sealing = lines.iter().any(|line| line.kind == "seal");
            let room = if sealing {
                limit
            } else {
                limit.saturating_sub(4096)
            };
            return pending.accepted_bytes + bytes <= room;
        }
        let budget = limit.saturating_sub(TERMINAL_RESERVE_BYTES);
        !pending.exhausted && pending.accepted_bytes + bytes <= budget
    }

    /// Count a refused batch; the first non-terminal batch the limit refuses exhausts the
    /// segment and records its one `problem`.
    fn refuse(&self, mut guard: MutexGuard<'_, Pending>, terminal: bool, refusal: Refusal) {
        let records = match refusal {
            Refusal::Unbuilt => 1,
            Refusal::Full(records) => records,
        };
        guard.dropped += records;
        if matches!(refusal, Refusal::Full(_)) && !terminal && !guard.exhausted {
            guard.exhausted = true;
            drop(guard);
            self.problem(
                ProblemWhat::Budget,
                "segment_max_bytes reached: further records are dropped and counted",
            );
        }
    }

    /// Append an accepted batch to the buffer. The blob objects it carries become known to the
    /// segment, and the digests its stored occurrences name become referenced, in the same
    /// critical section, so no other batch can see the digest unknown once this one is in.
    fn accept(&self, pending: &mut Pending, lines: Vec<Line>) {
        let flush_bytes = usize::try_from(self.shared.options.flush_bytes).unwrap_or(usize::MAX);
        for line in lines {
            match line.kind {
                "span_start" => pending.spans += 1,
                "event" => pending.events += 1,
                "blob" => pending.blobs += 1,
                _ => {}
            }
            if let Some(digest) = line.object {
                pending.blobs_in_segment.insert(digest);
            }
            if let Some(digest) = line.referenced {
                pending.referenced.insert(digest);
            }
            if !pending.buffer.is_empty()
                && pending.buffer.len() + line.bytes.len() + 1 > flush_bytes
            {
                let full = std::mem::take(&mut pending.buffer);
                pending.ready.push_back(full);
            }
            pending.buffer.extend_from_slice(&line.bytes);
            pending.buffer.push(b'\n');
            pending.accepted_bytes += line.bytes.len() as u64 + 1;
            pending.next_seq += 1;
            if line.kind == "seal" {
                // Nothing is accepted after the seal, in the same critical section.
                pending.state = State::Sealing;
                pending.flush_requested = true;
            }
            pending.appends += 1;
            pending.oldest.get_or_insert_with(Instant::now);
        }
    }

    /// The consumer's work: write every full buffer, and the current buffer when `force` or a
    /// flush point asked for it, each with one write of whole lines. Opens the file (and
    /// registers the directory) at its first write. Only the segment's owner calls it: the
    /// consumer thread while one runs, the appending thread otherwise.
    pub fn consume(&self, force: bool) {
        let mut consumer = lock(&self.shared.consumer);
        let (chunks, segment_id) = {
            let mut pending = lock(&self.shared.pending);
            if matches!(
                pending.state,
                State::Failed | State::Crashed | State::Abandoned
            ) {
                return;
            }
            let mut chunks: Vec<Vec<u8>> = pending.ready.drain(..).collect();
            if (force || pending.flush_requested) && !pending.buffer.is_empty() {
                chunks.push(std::mem::take(&mut pending.buffer));
            }
            if pending.buffer.is_empty() {
                pending.oldest = None;
            }
            pending.flush_requested = false;
            (chunks, pending.segment_id.clone())
        };
        if chunks.is_empty() {
            return;
        }
        if consumer.file.is_none()
            && let Err(reason) = self.open_file(&mut consumer, &segment_id)
        {
            self.fail(ProblemWhat::Write, reason);
            return;
        }
        let file = consumer.file.as_mut().expect("opened above");
        for chunk in chunks {
            let hooked = match &self.shared.options.write_hook {
                Some(hook) => hook(
                    self.shared
                        .writes
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        + 1,
                ),
                None => Ok(()),
            };
            if let Err(error) = hooked.and_then(|()| file.write_all(&chunk)) {
                drop(consumer);
                self.fail(
                    ProblemWhat::Write,
                    format!("writing the telemetry segment: {error}"),
                );
                return;
            }
        }
    }

    /// After an append: without a consumer thread the appending thread runs the consumer code
    /// (`flush` at a flush point); with one, the append has already signalled it.
    fn after_append(&self, flush: bool) {
        if lock(&self.shared.pending).mode == Mode::Inline {
            self.consume(flush);
        }
    }

    /// Have everything appended so far written, by whoever owns the writes: at once without a
    /// consumer thread; with one, by asking it and waiting at most [`layout::LOCK_WAIT`].
    /// Whether it was written.
    fn flush_now(&self) -> bool {
        let mut pending = lock(&self.shared.pending);
        if pending.mode == Mode::Inline {
            drop(pending);
            self.consume(true);
            return true;
        }
        pending.flush_asked += 1;
        let ticket = pending.flush_asked;
        self.shared.wake.notify_all();
        let deadline = Instant::now() + layout::LOCK_WAIT;
        while pending.flush_done < ticket && !pending.consumer_exited {
            if matches!(
                pending.state,
                State::Failed | State::Crashed | State::Abandoned
            ) {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            pending = self
                .shared
                .done
                .wait_timeout(pending, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        true
    }

    /// The process opened an `af.run` span: the first one starts the consumer thread, the
    /// segment's sole owner while a run is open. At `off` there is no writer, so no thread.
    pub fn run_started(&self) {
        let mut pending = lock(&self.shared.pending);
        pending.runs_open += 1;
        if pending.runs_open != 1
            || pending.mode != Mode::Inline
            || !matches!(pending.state, State::Unopened | State::Open)
        {
            return;
        }
        pending.mode = Mode::Running;
        pending.consumer_exited = false;
        drop(pending);
        let writer = self.clone();
        let spawned = std::thread::Builder::new()
            .name("af-telemetry".into())
            .spawn(move || writer.consumer_loop());
        match spawned {
            Ok(handle) => *lock(&self.shared.thread) = Some(handle),
            Err(error) => {
                {
                    let mut pending = lock(&self.shared.pending);
                    pending.mode = Mode::Inline;
                    pending.consumer_exited = true;
                }
                self.problem(
                    ProblemWhat::Write,
                    &format!(
                        "the telemetry consumer thread did not start ({}); the appending \
                         thread writes instead",
                        error.kind()
                    ),
                );
            }
        }
    }

    /// The process ended an `af.run` span: when it was the last one open, the consumer thread
    /// is stopped within the bounded shutdown. Whether it stopped (it was abandoned otherwise).
    pub fn run_ended(&self) -> bool {
        {
            let mut pending = lock(&self.shared.pending);
            pending.runs_open = pending.runs_open.saturating_sub(1);
            if pending.runs_open > 0 {
                return true;
            }
        }
        self.stop_consumer()
    }

    /// The bounded shutdown: signal the consumer thread and wait for it at most
    /// [`layout::LOCK_WAIT`]; one that does not stop in time is abandoned, and from then on no
    /// thread writes, seals, compresses or replaces the segment. Whether the writes are the
    /// appending thread's again (false once abandoned).
    fn stop_consumer(&self) -> bool {
        let mut pending = lock(&self.shared.pending);
        pending.runs_open = 0;
        if pending.state == State::Abandoned {
            return false;
        }
        if pending.mode == Mode::Inline {
            return pending.state != State::Abandoned;
        }
        pending.mode = Mode::Stopping;
        self.shared.wake.notify_all();
        let deadline = Instant::now() + layout::LOCK_WAIT;
        while !pending.consumer_exited {
            let now = Instant::now();
            if now >= deadline {
                pending.state = State::Abandoned;
                pending
                    .unwritten
                    .push((ProblemWhat::Write, ABANDONED.into()));
                return false;
            }
            pending = self
                .shared
                .done
                .wait_timeout(pending, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        drop(pending);
        if let Some(handle) = lock(&self.shared.thread).take() {
            // It has exited: the join returns at once.
            let _ = handle.join();
        }
        true
    }

    /// Install the stream queue the consumer drains at every wake (package P6).
    pub fn set_stream_queue(&self, queue: Arc<dyn StreamQueue>) {
        *lock(&self.shared.stream) = Some(queue);
    }

    /// The stream queue received a chunk: wake the consumer. Never blocks.
    pub fn stream_chunk(&self) {
        lock(&self.shared.pending).stream_pending = true;
        self.shared.wake.notify_all();
    }

    /// The consumer thread: one condition, one deadline, no timer and no polling.
    fn consumer_loop(&self) {
        let latency = Duration::from_millis(self.shared.options.flush_latency_ms.max(1));
        let mut seen = 0_u64;
        loop {
            // Wait for an append, a stream chunk, a flush request, the stop, or the deadline.
            let (stopping, asked, due) = {
                let mut pending = lock(&self.shared.pending);
                loop {
                    if matches!(
                        pending.state,
                        State::Failed | State::Crashed | State::Abandoned
                    ) {
                        self.exit_consumer(pending);
                        return;
                    }
                    let stopping = pending.mode == Mode::Stopping;
                    let deadline = pending.oldest.map(|oldest| oldest + latency);
                    let now = Instant::now();
                    let due = deadline.is_some_and(|deadline| now >= deadline);
                    let woken = stopping
                        || pending.appends != seen
                        || !pending.ready.is_empty()
                        || pending.flush_asked > pending.flush_done
                        || pending.stream_pending;
                    if woken || due {
                        seen = pending.appends;
                        pending.stream_pending = false;
                        break (stopping, pending.flush_asked, due);
                    }
                    pending = match deadline {
                        None => self
                            .shared
                            .wake
                            .wait(pending)
                            .unwrap_or_else(|poisoned| poisoned.into_inner()),
                        Some(deadline) => {
                            self.shared
                                .wake
                                .wait_timeout(pending, deadline - now)
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .0
                        }
                    };
                }
            };
            // Drain the stream queue, then write: the buffer goes when the queue is empty, the
            // deadline has passed, a flush was asked for, or the consumer is stopping.
            let stream = lock(&self.shared.stream).clone();
            let queue_empty = stream.is_none_or(|queue| queue.drain(self));
            let pending_ask = {
                let pending = lock(&self.shared.pending);
                asked > pending.flush_done
            };
            self.consume(queue_empty || due || stopping || pending_ask);
            let pending = {
                let mut pending = lock(&self.shared.pending);
                pending.flush_done = pending.flush_done.max(asked);
                pending
            };
            self.shared.done.notify_all();
            if stopping
                && pending.ready.is_empty()
                && pending.buffer.is_empty()
                && pending.appends == seen
            {
                self.exit_consumer(pending);
                return;
            }
        }
    }

    /// The consumer thread's last act: the writes are the appending thread's again, unless the
    /// shutdown abandoned it.
    fn exit_consumer(&self, mut pending: MutexGuard<'_, Pending>) {
        if pending.state != State::Abandoned {
            pending.mode = Mode::Inline;
        }
        pending.consumer_exited = true;
        drop(pending);
        self.shared.done.notify_all();
    }

    fn open_file(&self, consumer: &mut Consumer, segment_id: &str) -> Result<(), String> {
        let options = &self.shared.options;
        if let Some(registration) = &options.registration {
            if let Some(reason) = &registration.accounted_unknown {
                return Err(format!(
                    "the overlap check of telemetry directory {} is unknown: {reason}",
                    options.layout.root().display()
                ));
            }
            dirs::register(
                &registration.registry,
                options.layout.root(),
                &registration.accounted,
            )?;
        }
        // Every directory from the root down is created and opened without following a link,
        // and the live file is created relative to the opened `segments/`. The root and `v1/`
        // stay open for the writer's lifetime: blobs and `gc.lock` are reached through them.
        let creating = |path: PathBuf| {
            move |error: std::io::Error| format!("creating {}: {error}", path.display())
        };
        let root = Confined::create_root(options.layout.root())
            .map_err(creating(options.layout.root().to_path_buf()))?;
        let versioned = root
            .create_child_dir(layout::VERSION_DIR)
            .map_err(creating(options.layout.versioned()))?;
        let segments = versioned
            .create_child_dir("segments")
            .map_err(creating(options.layout.segments()))?;
        let name = format!("{segment_id}{}", layout::LIVE);
        let file = segments
            .create_file(&name, true)
            .map_err(creating(segments.join(&name)))?;
        let held = layout::try_lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|_| format!("locking {}", segments.join(&name).display()))?;
        consumer.file = Some(held);
        consumer.segments = Some(segments);
        // Opened once: a writer whose segment failed to open never opens another.
        let _ = self.shared.tree.set(Tree {
            _root: root,
            versioned,
            blobs: Mutex::new(BlobDirs::default()),
        });
        Ok(())
    }

    fn fail(&self, what: ProblemWhat, reason: String) {
        let mut pending = lock(&self.shared.pending);
        pending.state = State::Failed;
        pending.unwritten.push((what, reason));
    }

    /// Write everything buffered now (with a consumer thread: have it written, waiting at most
    /// [`layout::LOCK_WAIT`]).
    pub fn flush(&self) {
        self.flush_now();
    }

    /// Stop as a killed process would: nothing buffered is written, the lock on the segment
    /// ends, the file stays `.jsonl` for a reader to recover.
    pub fn abandon(&self) {
        let mut consumer = lock(&self.shared.consumer);
        lock(&self.shared.pending).state = State::Crashed;
        consumer.file = None;
    }

    /// Bound `attrs`; at `content`, a string or string array over its bound becomes a pending
    /// blob whose occurrence id the attribute will name.
    fn bound(&self, attrs: Attrs) -> Bounded {
        let options = &self.shared.options;
        let mut contents = Vec::new();
        let mut named = Vec::new();
        let keep = options.level.keeps_content();
        let (attributes, counters) =
            record::bound_attributes(attrs, &options.redactor, &mut |key, value| {
                if !keep {
                    return None;
                }
                // Both arrive as the writer gave them, so the occurrence's source identity is
                // the original's: text is redacted per line before it is retained, an array as
                // one JSON document, measured before it is encoded so an array over the blob
                // bound is withheld before anything proportional to it is allocated.
                contents.push(match value {
                    Oversized::Text(text) => blob::prepare(
                        text.as_bytes(),
                        Media::Text,
                        options.content_max_blob_bytes,
                        &options.redactor,
                    ),
                    Oversized::Texts(values) => blob::prepare_texts(
                        values,
                        options.content_max_blob_bytes,
                        &options.redactor,
                    ),
                });
                named.push(key.to_string());
                Some(String::new())
            });
        Bounded {
            attributes,
            counters,
            contents,
            named,
        }
    }

    fn hook(&self, step: PublishStep) -> bool {
        match &self.shared.options.publish_hook {
            Some(hook) => {
                if hook(step) == HookAction::Crash {
                    self.abandon();
                    false
                } else {
                    true
                }
            }
            None => true,
        }
    }

    /// Publish `contents` and append the owning record `build` makes, in the protocol's order.
    /// `build` gets the batch and the occurrence ids of the contents; the owner takes the `seq`
    /// right after the occurrences. `span` is the span the contents belong to, or `None` for
    /// the span the owner itself starts. `build` runs twice for a batch with content: once to
    /// judge the batch against the segment limit before any file is placed, once for the
    /// batch accepted.
    fn publish<T>(
        &self,
        contents: Vec<Prepared>,
        span: Option<&str>,
        flush: bool,
        mut build: impl FnMut(&mut Batch<'_>, &[String]) -> Option<T>,
    ) -> Option<T> {
        if contents.is_empty() {
            let value = self.commit(false, |batch| build(batch, &[]));
            self.after_append(flush);
            return value;
        }
        // The directory is registered and the segment open before any blob file is written.
        {
            let mut pending = lock(&self.shared.pending);
            if !self.ensure_open(&mut pending) {
                return None;
            }
        }
        self.flush_now();
        if self.failed() {
            return None;
        }
        // Exhaustion is judged before any blob file is staged: past the limit the owning record
        // is refused anyway, so its content never reaches `blobs/`.
        {
            let mut pending = lock(&self.shared.pending);
            if pending.exhausted {
                pending.dropped += 1 + contents.len() as u64;
                return None;
            }
        }
        // The segment is open, so the directories it was opened in are held.
        let Some(tree) = self.shared.tree.get() else {
            lock(&self.shared.pending).dropped += 1 + contents.len() as u64;
            return None;
        };
        let mut contents: Vec<Content> = contents
            .into_iter()
            .map(|prepared| Content {
                prepared,
                staged: None,
                stored: false,
            })
            .collect();
        for content in &mut contents {
            let staged = tree
                .prefix(blob::prefix_of(&content.prepared.digest))
                .and_then(|directory| blob::stage_in(directory, &content.prepared));
            match staged {
                Ok(staged) => content.staged = Some(staged),
                Err(error) => self.problem(
                    ProblemWhat::Blob,
                    &format!("staging a blob: {}", error.kind()),
                ),
            }
        }
        if !self.hook(PublishStep::Staged) {
            return None;
        }
        let gc = if contents.iter().any(|content| content.staged.is_some()) {
            layout::lock_in(
                &tree.versioned,
                layout::GC_LOCK,
                std::time::Duration::ZERO,
                FlockArg::LockSharedNonblock,
            )
            .ok()
            .flatten()
        } else {
            None
        };
        if gc.is_some() && !self.hook(PublishStep::Locked) {
            return None;
        }
        // From the fit decision to the acceptance, one critical section: the batch is judged
        // against the segment limit before any file is placed, the files are placed, and the
        // batch is accepted with the digests it makes known, so no other batch can take the
        // room it was judged against or see one of its digests unknown.
        let mut guard = lock(&self.shared.pending);
        let pending = &mut *guard;
        if pending.state != State::Open {
            for content in &mut contents {
                if let Some(staged) = content.staged.take() {
                    staged.discard();
                }
            }
            pending.dropped += 1;
            return None;
        }
        let open = !pending.exhausted;
        for content in &mut contents {
            content.stored = open && gc.is_some() && content.staged.is_some();
        }
        // The batch as it is when every staged content is placed. A content that then fails to
        // place turns its `stored` into `dropped`, one byte longer, and loses at most its
        // object record, so one byte per content of slack covers the batch actually accepted.
        let (planned, lines) = publication(pending, &contents, span, &mut build);
        let slack = contents.len() as u64;
        if !open || planned.is_none() || !self.fits(pending, false, &lines, slack) {
            for content in &mut contents {
                if let Some(staged) = content.staged.take() {
                    staged.discard();
                }
            }
            let refusal = if !open || planned.is_some() {
                Refusal::Full(lines.len().max(1) as u64)
            } else {
                Refusal::Unbuilt
            };
            self.refuse(guard, false, refusal);
            return None;
        }
        // The files this batch itself renamed into place; a file of the digest that was
        // already there belongs to whichever batch placed it.
        let mut renamed = Vec::new();
        for content in &mut contents {
            let Some(staged) = content.staged.take() else {
                continue;
            };
            if !content.stored {
                staged.discard();
                continue;
            }
            match blob::place(&staged) {
                Ok(blob::Placed::Renamed) => {
                    renamed.push((content.prepared.digest.clone(), staged));
                }
                Ok(blob::Placed::Reused) => {}
                Err(_) => {
                    staged.discard();
                    content.stored = false;
                }
            }
        }
        let (value, lines) = publication(pending, &contents, span, &mut build);
        if value.is_none() || !self.fits(pending, false, &lines, 0) {
            // Refused after placement: the files it renamed go again, still under gc.lock,
            // except a digest a record of this segment already references.
            for (digest, staged) in renamed {
                if !pending.referenced.contains(&digest) {
                    staged.remove_placed();
                }
            }
            let refusal = if value.is_some() {
                Refusal::Full(lines.len().max(1) as u64)
            } else {
                Refusal::Unbuilt
            };
            self.refuse(guard, false, refusal);
            return None;
        }
        self.accept(pending, lines);
        // Dropped because the sweep held gc.lock or the file could not be placed.
        pending.dropped += contents.iter().filter(|content| !content.stored).count() as u64;
        drop(guard);
        self.shared.wake.notify_all();
        if gc.is_some() && !self.hook(PublishStep::Renamed) {
            return None;
        }
        // The references are written before gc.lock is released. A consumer that does not
        // write them within the bounded wait leaves the blob to its one-hour protection.
        self.flush_now();
        if gc.is_some() && !self.hook(PublishStep::Flushed) {
            return None;
        }
        drop(gc);
        value
    }

    /// A span's or an event's name: free text like any other, judged whole before it is cut, so
    /// a credential in it leaves the withheld marker and is counted in `af.redacted`.
    fn name(&self, name: &str, counters: &mut Counters) -> String {
        let (name, withheld) = self.shared.options.redactor.value(name);
        if withheld {
            counters.redacted += 1;
        }
        record::cut(&name, MAX_KEY_BYTES)
    }

    /// Start a span; returns its `span_id`. `trace_id` is the run's trace, when known.
    pub fn span_start(
        &self,
        trace_id: Option<&str>,
        parent_span_id: Option<&str>,
        name: &str,
        kind: SpanKind,
        attrs: Attrs,
    ) -> Option<String> {
        let Bounded {
            mut attributes,
            mut counters,
            contents,
            named,
        } = self.bound(attrs);
        let start = ns(self.now_unix_ns());
        let name = self.name(name, &mut counters);
        let trace_id = trace_id.map(|id| self.typed("trace_id", id, MAX_KEY_BYTES));
        let parent_span_id =
            parent_span_id.map(|id| self.typed("parent_span_id", id, MAX_KEY_BYTES));
        self.publish(contents, None, false, |batch, occurrences| {
            fill(&mut attributes, &named, occurrences);
            let span_id = trace::span_id(batch.segment_id(), batch.seq());
            batch.push(
                Record::SpanStart(SpanStart {
                    seq: 0,
                    schema: schema::SPAN.into(),
                    trace_id: trace_id.clone(),
                    span_id: span_id.clone(),
                    parent_span_id: parent_span_id.clone(),
                    name: name.clone(),
                    kind,
                    start_unix_ns: start.clone(),
                    attributes: attributes.clone(),
                }),
                counters,
            )?;
            Some(span_id)
        })
    }

    /// End a span with the attributes known only at its end. A span of `depth` at most 2 (the
    /// root is depth 0) is a flush point.
    pub fn span_end(&self, span_id: &str, status: SpanStatus, attrs: Attrs, depth: usize) {
        let Bounded {
            mut attributes,
            counters,
            contents,
            named,
        } = self.bound(attrs);
        let end = ns(self.now_unix_ns());
        self.publish(contents, Some(span_id), depth <= 2, |batch, occurrences| {
            fill(&mut attributes, &named, occurrences);
            batch.push(
                Record::SpanEnd(SpanEnd {
                    seq: 0,
                    schema: schema::SPAN.into(),
                    span_id: span_id.to_string(),
                    end_unix_ns: Some(end.clone()),
                    status,
                    attributes: attributes.clone(),
                }),
                counters,
            )
        });
    }

    /// An event on a span; returns its `seq`.
    pub fn event(&self, span_id: &str, name: &str, attrs: Attrs) -> Option<u64> {
        self.event_with(span_id, name, attrs, None)
    }

    /// An event naming content: at `content` the content becomes a blob occurrence the event
    /// names; at every other level the event is recorded without it. Returns the event's `seq`.
    pub fn event_with_content(
        &self,
        span_id: &str,
        name: &str,
        attrs: Attrs,
        content: &[u8],
        media: Media,
    ) -> Option<u64> {
        self.event_with(span_id, name, attrs, Some((content, media)))
    }

    fn event_with(
        &self,
        span_id: &str,
        name: &str,
        attrs: Attrs,
        content: Option<(&[u8], Media)>,
    ) -> Option<u64> {
        let options = &self.shared.options;
        let Bounded {
            mut attributes,
            mut counters,
            mut contents,
            named,
        } = self.bound(attrs);
        let mut media_type = None;
        if let Some((bytes, media)) = content
            && options.level.keeps_content()
        {
            let prepared = blob::prepare(
                bytes,
                media,
                options.content_max_blob_bytes,
                &options.redactor,
            );
            media_type = Some(prepared.media_type.to_string());
            // The event's own content is the first occurrence.
            contents.insert(0, prepared);
        }
        let own = media_type.is_some();
        let time = ns(self.now_unix_ns());
        let name = self.name(name, &mut counters);
        self.publish(contents, Some(span_id), false, |batch, occurrences| {
            // The attributes' contents follow the event's own.
            let attribute_occurrences = if own { &occurrences[1..] } else { occurrences };
            fill(&mut attributes, &named, attribute_occurrences);
            batch.push(
                Record::Event(Event {
                    seq: 0,
                    schema: schema::EVENT.into(),
                    span_id: span_id.to_string(),
                    time_unix_ns: time.clone(),
                    name: name.clone(),
                    attributes: attributes.clone(),
                    blob: if own {
                        occurrences.first().cloned()
                    } else {
                        None
                    },
                    blob_media_type: media_type.clone(),
                }),
                counters,
            )
        })
    }

    /// A telemetry-only problem: recorded once per distinct `what` and `reason`, from the
    /// terminal reserve.
    pub fn problem(&self, what: ProblemWhat, reason: &str) {
        let (reason, _) = self.shared.options.redactor.value(reason);
        let reason = record::cut(&reason, MAX_STRING_BYTES);
        {
            let mut pending = lock(&self.shared.pending);
            if !pending.problems.insert((what, reason.clone())) {
                return;
            }
        }
        let time = ns(self.now_unix_ns());
        let written = self.commit(true, |batch| {
            batch.push(
                Record::Problem(Problem {
                    seq: 0,
                    schema: schema::PROBLEM.into(),
                    time_unix_ns: time,
                    what,
                    reason: reason.clone(),
                }),
                Counters::default(),
            )
        });
        if written.is_none() {
            lock(&self.shared.pending).unwritten.push((what, reason));
        }
    }

    /// An execution's configuration snapshot, split into as many `config` parts as the record
    /// bound needs, in the rows' order. Each row is judged by its original key first: a key the
    /// credential rule judges a credential (a token prefix, a JSON Web Token, a long secret
    /// run, an assignment) or the secret-name rule matches (`key`, `token`, `secret`,
    /// `password`, …) withholds the whole pair: the value and the origin become the withheld
    /// marker whatever their length or shape, a credential-shaped key becomes it too, and the
    /// pair counts once in `af.redacted`. Other keys, values, layers and origins are untrusted
    /// text, each judged whole and only then bounded; each part counts its withheld values in
    /// `af.redacted`.
    pub fn config(&self, execution_id: &str, rows: Vec<ConfigRow>) -> Option<u32> {
        let redactor = &self.shared.options.redactor;
        let rows: Vec<(ConfigRow, u32)> = rows
            .into_iter()
            .map(|row| {
                let mut redacted = 0;
                let mut judge = |text: &str| {
                    let (text, withheld) = redactor.value(text);
                    redacted += u32::from(withheld);
                    text
                };
                let credential_key = redact::looks_like_credential(&row.key);
                let pair_withheld = credential_key || redact::secret_name(&row.key);
                let (key, value, origin) = if pair_withheld {
                    let key = if credential_key {
                        redact::WITHHELD.to_string()
                    } else {
                        redactor.replace(&row.key)
                    };
                    let withheld = row.origin.map(|_| redact::WITHHELD.to_string());
                    (key, redact::WITHHELD.to_string(), withheld)
                } else {
                    let key = judge(&row.key);
                    let value = judge(&row.value);
                    let origin = row.origin.map(|origin| judge(&origin));
                    (key, value, origin)
                };
                let layer = judge(&row.layer);
                redacted += u32::from(pair_withheld);
                let row = ConfigRow {
                    key: record::cut(&key, MAX_KEY_BYTES),
                    value: record::cut(&value, MAX_STRING_BYTES),
                    layer: record::cut(&layer, MAX_KEY_BYTES),
                    origin: origin.map(|origin| record::cut(&origin, MAX_STRING_BYTES)),
                };
                (row, redacted)
            })
            .collect();
        // Pack rows into parts that fit with room for the part's own fields.
        let room = RECORD_MAX_BYTES - 512;
        let mut parts: Vec<(Vec<ConfigRow>, u32)> = vec![(Vec::new(), 0)];
        let mut size = 0;
        for (row, redacted) in rows {
            let bytes = serde_json::to_vec(&row).expect("a row serializes").len() + 1;
            let current = parts.last_mut().expect("one part at least");
            if !current.0.is_empty() && size + bytes > room {
                parts.push((Vec::new(), 0));
                size = 0;
            }
            let current = parts.last_mut().expect("pushed");
            current.0.push(row);
            current.1 += redacted;
            size += bytes;
        }
        let count = u32::try_from(parts.len()).unwrap_or(u32::MAX);
        let execution_id = self.typed("execution_id", execution_id, MAX_KEY_BYTES);
        self.commit(false, |batch| {
            for (index, (rows, redacted)) in parts.into_iter().enumerate() {
                let part = u32::try_from(index).unwrap_or(u32::MAX) + 1;
                batch.push(
                    Record::Config(ConfigPart {
                        seq: 0,
                        schema: schema::CONFIG.into(),
                        execution_id: execution_id.clone(),
                        part,
                        parts: count,
                        complete: part == count,
                        rows,
                        redacted: (redacted > 0).then_some(redacted),
                    }),
                    Counters::default(),
                )?;
            }
            Some(count)
        })
    }

    /// A record field: kept as given only when `position` is an identity position
    /// ([`record::is_identity_position`]) and the value is exactly a kernel identity
    /// ([`redact::is_kernel_identity`]); judged whole as untrusted text otherwise, so a
    /// credential in it leaves the withheld marker; then cut to `max`.
    fn typed(&self, position: &str, value: &str, max: usize) -> String {
        if record::is_identity_position(position) && redact::is_kernel_identity(value) {
            record::cut(value, max)
        } else {
            record::cut(&self.shared.options.redactor.value(value).0, max)
        }
    }

    /// One plan node of an execution. At most 64 `depends_on` ids are kept; every field is a
    /// kernel identity in its identity position or judged whole.
    pub fn node(&self, mut node: Node) -> Option<u64> {
        node.depends_on.truncate(64);
        for id in &mut node.depends_on {
            *id = self.typed("depends_on", id, MAX_KEY_BYTES);
        }
        node.execution_id = self.typed("execution_id", &node.execution_id, MAX_KEY_BYTES);
        node.plan_digest = self.typed("plan_digest", &node.plan_digest, MAX_KEY_BYTES);
        node.node_id = self.typed("node_id", &node.node_id, MAX_KEY_BYTES);
        node.kind = self.typed("kind", &node.kind, MAX_KEY_BYTES);
        node.package = node
            .package
            .map(|package| self.typed("package", &package, MAX_KEY_BYTES));
        node.schema = schema::NODE.into();
        self.commit(false, |batch| {
            batch.push(Record::Node(node), Counters::default())
        })
    }

    /// The bounded summary of a Finding; its free text (title, file) is judged and bounded,
    /// and every other field is a kernel identity in its identity position or judged whole.
    pub fn finding(&self, mut finding: Finding) -> Option<u64> {
        let redactor = &self.shared.options.redactor;
        finding.schema = schema::FINDING.into();
        for (position, field) in [
            ("span_id", &mut finding.span_id),
            ("key", &mut finding.key),
            ("severity", &mut finding.severity),
            ("effective_severity", &mut finding.effective_severity),
            ("status", &mut finding.status),
            ("scope", &mut finding.scope),
            ("report_artifact_id", &mut finding.report_artifact_id),
            ("node_id", &mut finding.node_id),
            ("worker", &mut finding.worker),
        ] {
            *field = self.typed(position, field, MAX_KEY_BYTES);
        }
        finding.model = finding
            .model
            .map(|model| self.typed("model", &model, MAX_KEY_BYTES));
        finding.title = record::cut(&redactor.value(&finding.title).0, MAX_STRING_BYTES);
        finding.file = finding
            .file
            .map(|file| record::cut(&redactor.value(&file).0, MAX_STRING_BYTES));
        self.commit(false, |batch| {
            batch.push(Record::Finding(finding), Counters::default())
        })
    }

    /// The bounded summary of a Demand, keyed by its id; every field is a kernel identity in
    /// its identity position or judged whole.
    pub fn demand(&self, mut demand: Demand) -> Option<u64> {
        for (position, field) in [
            ("span_id", &mut demand.span_id),
            ("demand_id", &mut demand.demand_id),
            ("kind", &mut demand.kind),
            ("subject_id", &mut demand.subject_id),
            ("snapshot_id", &mut demand.snapshot_id),
            ("status", &mut demand.status),
        ] {
            *field = self.typed(position, field, MAX_KEY_BYTES);
        }
        demand.schema = schema::DEMAND.into();
        self.commit(false, |batch| {
            batch.push(Record::Demand(demand), Counters::default())
        })
    }

    /// Count chunks a stream observer dropped (P6); kept in memory and written in the seal.
    pub fn count_dropped_chunks(&self, chunks: u64) {
        lock(&self.shared.pending).dropped_chunks += chunks;
    }

    /// Seal the segment at exit: append the seal from the reserve, write the buffer, the `.refs`
    /// manifest, compress and rename. `Ok(None)` when nothing was ever opened.
    pub fn finish(&self, exit: SealExit) -> Result<Option<Sealed>, String> {
        // A consumer thread still running (a run span never ended) is stopped first, within
        // the bounded shutdown; an abandoned one leaves the segment unsealed.
        if !self.stop_consumer() {
            return Err(ABANDONED.into());
        }
        let ended = ns(self.now_unix_ns());
        {
            let state = lock(&self.shared.pending).state;
            match state {
                State::Unopened => return Ok(None),
                State::Open => {}
                State::Abandoned => return Err(ABANDONED.into()),
                State::Failed | State::Crashed | State::Sealing | State::Sealed => {
                    return Err("the segment is not open".into());
                }
            }
        }
        let sealed = self.commit(true, |batch| {
            let refs = format!("{}{}", batch.segment_id(), layout::REFS);
            let counts = batch.counts;
            batch.push(
                Record::Seal(Seal {
                    seq: 0,
                    schema: schema::SEAL.into(),
                    ended_unix_ns: ended.clone(),
                    exit,
                    spans: counts.spans,
                    events: counts.events,
                    blobs: counts.blobs,
                    dropped: Some(counts.dropped),
                    dropped_chunks: Some(counts.dropped_chunks),
                    discarded_bytes: 0,
                    refs,
                }),
                Counters::default(),
            )
        });
        // A seal that could not be appended leaves the segment for a reader to recover.
        if sealed.is_none() {
            return Err("the seal did not fit the segment".into());
        }
        self.consume(true);
        let mut consumer = lock(&self.shared.consumer);
        let (segment_id, referenced) = {
            let pending = lock(&self.shared.pending);
            if pending.state != State::Sealing {
                return Err("writing the segment failed before the seal".into());
            }
            (pending.segment_id.clone(), pending.referenced.clone())
        };
        let result = seal_file(&mut consumer, &segment_id, &referenced);
        let mut pending = lock(&self.shared.pending);
        match result {
            Ok(sealed) => {
                pending.state = State::Sealed;
                Ok(Some(sealed))
            }
            Err(error) => {
                pending.state = State::Failed;
                pending.unwritten.push((ProblemWhat::Write, error.clone()));
                Err(error)
            }
        }
    }
}

/// The sorted digests, one per line.
pub(crate) fn manifest(referenced: &BTreeSet<String>) -> Vec<u8> {
    let mut text = String::new();
    for digest in referenced {
        text.push_str(digest);
        text.push('\n');
    }
    text.into_bytes()
}

/// Write the manifest, compress the segment to `.jsonl.gz` and remove the `.jsonl`, under the
/// writer's lock, which ends when the file closes. Every step is relative to the `segments/`
/// directory the live file was created in, and the live bytes are read back through the held
/// descriptor: a directory above it renamed or replaced by a link since changes nothing
/// outside the root.
fn seal_file(
    consumer: &mut Consumer,
    segment_id: &str,
    referenced: &BTreeSet<String>,
) -> Result<Sealed, String> {
    let segments = consumer
        .segments
        .as_ref()
        .ok_or("the segment was never written")?;
    let held = consumer
        .file
        .as_mut()
        .ok_or("the segment was never written")?;
    let live = format!("{segment_id}{}", layout::LIVE);
    let refs = format!("{segment_id}{}", layout::REFS);
    let sealed = format!("{segment_id}{}", layout::SEALED);
    let failed = |name: &str| {
        let path = segments.join(name);
        move |error: std::io::Error| format!("{}: {error}", path.display())
    };
    held.sync_all().map_err(failed(&live))?;
    segments
        .write_atomically(&refs, &manifest(referenced))
        .map_err(failed(&refs))?;
    let mut bytes = Vec::new();
    held.seek(SeekFrom::Start(0))
        .and_then(|_| held.read_to_end(&mut bytes))
        .map_err(failed(&live))?;
    segments
        .write_atomically(&sealed, &blob::compress(&bytes))
        .map_err(failed(&sealed))?;
    // Only the file this writer created is removed: a name that no longer holds it is left.
    let opened = FileIdentity::of_file(held).map_err(failed(&live))?;
    if segments
        .identity(&live)
        .is_some_and(|current| current.dev == opened.dev && current.ino == opened.ino)
    {
        segments.unlink(&live).map_err(failed(&live))?;
    }
    let sealed = Sealed {
        segment_id: segment_id.to_string(),
        path: segments.join(&sealed),
        refs: segments.join(&refs),
    };
    consumer.file = None;
    Ok(sealed)
}
