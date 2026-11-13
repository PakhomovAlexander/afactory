//! The append-only run event log.
//!
//! SQLite in WAL mode, one writer, a monotonic per-run sequence. `sequence` is dense and
//! gapless, and it — not `occurred_at` — is the ordering authority, so replay cannot depend on
//! a clock two events might share.
//!
//! One invariant is enforced here rather than documented: an event may not reference an artifact
//! the CAS does not already hold. That is the ordering the design demands ("SQLite may reference
//! a filesystem CAS object only after that object is durable"), and enforcing it at append time
//! turns a class of crash-corruption into an immediate error.

use std::path::Path;
use std::sync::Arc;

use af_contracts::{EventType, RunEvent};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use crate::cas::{Cas, CasError};

thread_local! {
    static REPLAY_READS: std::cell::RefCell<Vec<ReplayReads>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct ReplayReads {
    store: usize,
    prefixes: std::collections::BTreeMap<String, u64>,
}

struct ReplayReadsGuard;
impl Drop for ReplayReadsGuard {
    fn drop(&mut self) {
        REPLAY_READS.with(|reads| reads.borrow_mut().pop());
    }
}

mod attempt_wall;
pub mod observer;
pub mod task;
pub mod writers;

/// Read a non-negative SQLite integer column as `u64`.
///
/// SQLite stores integers as `i64`; a negative value in a column that the schema treats as a
/// count or sequence is refused as a conversion failure rather than wrapped.
pub(crate) fn u64_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    /// An event referenced an artifact that is not durable yet.
    DanglingArtifact {
        digest: String,
    },
    /// Two events claimed the same sequence, or an event id repeated.
    Conflict(String),
    /// The captured domain rejected structurally valid output. This is distinct from
    /// an authority, artifact or persistence failure at the Store boundary.
    TaskOutputRejected(String),
    /// The CAS could not make a referenced object durable.
    Durability(String),
    /// A referenced artifact required for replay was missing or malformed.
    Artifact(String),
    /// The Task was collected by `af task gc --apply` (ADR-0135): its log ends in a tombstone,
    /// its projection stops there, and nothing may act on it again.
    Collected {
        task_id: String,
        collected_unix_ms: u64,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "event store: {e}"),
            StoreError::Json(e) => write!(f, "event store json: {e}"),
            StoreError::DanglingArtifact { digest } => write!(
                f,
                "event references an artifact that is not durable: {digest}"
            ),
            StoreError::Conflict(what) => write!(f, "event store conflict: {what}"),
            StoreError::TaskOutputRejected(what) => {
                write!(f, "Task output admission rejected: {what}")
            }
            StoreError::Durability(what) => {
                write!(f, "a referenced artifact could not be made durable: {what}")
            }
            StoreError::Artifact(what) => write!(f, "event store artifact: {what}"),
            StoreError::Collected {
                task_id,
                collected_unix_ms,
            } => write!(
                f,
                "Task `{task_id}` was collected {}",
                af_contracts::task::collection::collected_time(*collected_unix_ms)
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

struct StoredEventRow {
    event_id: String,
    sequence: i64,
    event_type: String,
    occurred_at: String,
    node_id: Option<String>,
    attempt_id: Option<String>,
    causation_id: Option<String>,
    correlation_id: Option<String>,
    artifact_refs: String,
    payload: String,
}

/// What an appender supplies. `event_id` and `sequence` are the store's to assign — a caller
/// that could choose its own sequence could rewrite history by racing.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub event_type: EventType,
    pub occurred_at: String,
    pub node_id: Option<String>,
    pub attempt_id: Option<String>,
    pub causation_id: Option<String>,
    pub correlation_id: Option<String>,
    pub artifact_refs: Vec<String>,
    pub payload: Value,
}

impl NewEvent {
    pub fn new(event_type: EventType, payload: Value) -> Self {
        Self {
            event_type,
            // Deliberately fixed: this store has no clock of its own, and nothing in replay may
            // read this field. A caller that wants a real timestamp passes one.
            occurred_at: "1970-01-01T00:00:00Z".to_string(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload,
        }
    }

    pub fn at(mut self, occurred_at: impl Into<String>) -> Self {
        self.occurred_at = occurred_at.into();
        self
    }

    pub fn node(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }

    pub fn attempt(mut self, attempt_id: impl Into<String>) -> Self {
        self.attempt_id = Some(attempt_id.into());
        self
    }

    pub fn correlating(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    pub fn caused_by(mut self, causation_id: impl Into<String>) -> Self {
        self.causation_id = Some(causation_id.into());
        self
    }

    pub fn referencing(mut self, artifact_refs: Vec<String>) -> Self {
        self.artifact_refs = artifact_refs;
        self
    }
}

pub struct EventStore {
    conn: Connection,
    /// At most one live Task projection; append-only sequence is its cache watermark.
    task_cache: std::cell::RefCell<Option<task::TaskProjection>>,
    /// Told of every commit through this connection ([`observer`]); `None` defers to the
    /// process's observer, if any.
    commit_observer: Option<Arc<dyn observer::CommitObserver>>,
}

/// A bounded, process-local observation of this connection's database. This is only an
/// invalidation token: event decoding, projection validation and exact CAS checks still own
/// the evidence. Tokens cannot be compared across connections or retained across reopening.
#[derive(Debug, PartialEq, Eq)]
pub struct DatabaseObservation {
    store: usize,
    data_version: u64,
    total_changes: u64,
}

/// Wall-clock and exact cumulative usage for one Task Attempt.
///
/// This is a **sidecar**, not an event: Review replay, the Finding Ledger and convergence
/// do not read it. Common Task recovery may raise an abandoned Attempt's canonical charge
/// from its durable usage floor before permitting further work. It also lets a person see
/// how long an Attempt took and what it consumed, through `af review report` and the
/// `attempt_walls` section of Task inspection. An absent row means "not recorded", never
/// "zero". Native turn components and the charge keep their full aggregate range.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskAttemptWall {
    pub run_id: String,
    pub attempt_id: String,
    pub node_id: String,
    pub round: u32,
    pub epoch: u32,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<af_contracts::task::usage::TaskTokenUsageV3>,
}

/// The ceilings one bounded projection reads a Store's log under (`EventStore::replay_within`):
/// rows, the bytes of one row (its payload and artifact references) and the bytes of every row
/// it read, over every run it reads from the Store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBudget {
    rows_left: u64,
    max_row_bytes: u64,
    buffered_left: u64,
}

impl ReplayBudget {
    pub fn new(max_rows: u64, max_row_bytes: u64, max_buffered_bytes: u64) -> Self {
        Self {
            rows_left: max_rows,
            max_row_bytes,
            buffered_left: max_buffered_bytes,
        }
    }

    /// Charge one row of `bytes`, before it is decoded.
    fn charge(&mut self, bytes: usize) -> Result<(), ReplayCeiling> {
        let bytes = bytes as u64;
        if self.rows_left == 0 {
            return Err(ReplayCeiling::Rows);
        }
        if bytes > self.max_row_bytes {
            return Err(ReplayCeiling::RowBytes { bytes });
        }
        if bytes > self.buffered_left {
            return Err(ReplayCeiling::BufferedBytes);
        }
        self.rows_left -= 1;
        self.buffered_left -= bytes;
        Ok(())
    }
}

/// Which ceiling of a [`ReplayBudget`] stopped a bounded read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayCeiling {
    /// More rows than the budget allows.
    Rows,
    /// One row of `bytes`, more than one row may hold.
    RowBytes { bytes: u64 },
    /// More bytes over every row read than the budget buffers.
    BufferedBytes,
}

impl ReplayCeiling {
    /// Its typed name.
    pub fn as_str(self) -> &'static str {
        match self {
            ReplayCeiling::Rows => "max_rows",
            ReplayCeiling::RowBytes { .. } => "max_row_bytes",
            ReplayCeiling::BufferedBytes => "max_buffered_bytes",
        }
    }
}

impl std::fmt::Display for ReplayCeiling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayCeiling::Rows => write!(
                f,
                "its log holds more rows than a read may take ({})",
                self.as_str()
            ),
            ReplayCeiling::RowBytes { bytes } => write!(
                f,
                "a row of its log holds {bytes} bytes, more than one row may ({})",
                self.as_str()
            ),
            ReplayCeiling::BufferedBytes => write!(
                f,
                "its log holds more bytes than a read may buffer ({})",
                self.as_str()
            ),
        }
    }
}

/// Why a bounded replay stopped.
enum Bounded {
    Store(StoreError),
    Ceiling(ReplayCeiling),
}

impl From<StoreError> for Bounded {
    fn from(error: StoreError) -> Self {
        Bounded::Store(error)
    }
}

impl From<rusqlite::Error> for Bounded {
    fn from(error: rusqlite::Error) -> Self {
        Bounded::Store(error.into())
    }
}

impl From<ReplayCeiling> for Bounded {
    fn from(ceiling: ReplayCeiling) -> Self {
        Bounded::Ceiling(ceiling)
    }
}

/// The SQLite URI that opens the database at `path` immutable: every byte of the path that is not
/// unreserved, `/` aside, percent-encoded, so `?`, `#` and `%` in a directory name stay path.
fn immutable_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(*byte));
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri.push_str("?immutable=1");
    uri
}

impl EventStore {
    /// Capture before an indexed discovery read, then compare after every dependent read and
    /// byte check has finished. `data_version` detects other connections' commits, including
    /// newly qualifying streams, updates, deletions and collection; `total_changes` also
    /// detects this connection's writes. Unrelated writes conservatively invalidate the read.
    /// Neither operation scans event history or relies on filesystem metadata.
    ///
    /// A live cursor or explicit transaction could pin an old SQLite snapshot and conceal a
    /// concurrent commit. Both capture and comparison therefore require an idle connection;
    /// the caller must drop discovery cursors before checking the completed observation.
    pub fn database_observation(&self) -> Result<DatabaseObservation, StoreError> {
        if !self.conn.is_autocommit() || self.conn.is_busy() {
            return Err(StoreError::Conflict(
                "Database observation requires no active transaction or cursor".into(),
            ));
        }
        let data_version = self
            .conn
            .query_row("PRAGMA main.data_version", [], |row| u64_column(row, 0))?;
        Ok(DatabaseObservation {
            store: self as *const Self as usize,
            data_version,
            total_changes: self.conn.total_changes(),
        })
    }

    pub fn database_observation_unchanged(
        &self,
        observed: &DatabaseObservation,
    ) -> Result<bool, StoreError> {
        Ok(self.database_observation()? == *observed)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        // Created private before SQLite sees it: SQLite creates a missing database 0644 under
        // the umask, and gives its `-wal` and `-shm` files the database's own mode. A failure
        // here is SQLite's to report when it opens the same path.
        if !path.as_ref().as_os_str().is_empty() && path.as_ref() != Path::new(":memory:") {
            let _ = crate::local_fs::create_private_file(path.as_ref());
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            conn,
            task_cache: std::cell::RefCell::new(None),
            commit_observer: None,
        })
    }

    /// The path of this Store's database file, when it has one: its directory is the Store's
    /// instance, which Telemetry names by its canonical path (design §5). A read; it changes
    /// nothing.
    pub fn database_path(&self) -> Option<std::path::PathBuf> {
        self.conn
            .path()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
    }

    /// The highest committed event sequence of `run_id`, `None` before its first event. A read;
    /// it changes nothing.
    pub fn latest_run_sequence(&self, run_id: &str) -> Result<Option<u64>, StoreError> {
        let latest: Option<i64> = self.conn.query_row(
            "SELECT MAX(sequence) FROM events WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )?;
        Ok(latest.and_then(|sequence| u64::try_from(sequence).ok()))
    }

    /// Open an existing database to read it and change nothing: no file is created, no schema is
    /// initialized and no journal mode is set. A plain read-only connection to a write-ahead-log
    /// database creates its `-wal` and `-shm` files beside it and fails where it cannot, so a
    /// database whose `-wal` is absent or empty, which holds every committed event in the file
    /// itself, is opened `immutable`: SQLite then takes no lock and writes nothing at all. One whose
    /// `-wal` still holds frames (its writer is open or stopped before a checkpoint) is opened
    /// read-only instead, so those events are read, through the `-shm` index SQLite keeps.
    ///
    /// An immutable reader does not see a write made while it is open: the caller excludes
    /// writers for the life of the value, as a Project Store's lock does.
    pub fn open_unchanged(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        if std::fs::metadata(&wal).is_ok_and(|log| log.len() > 0) {
            return Self::open_read_only(path);
        }
        if !std::fs::metadata(path).is_ok_and(|database| database.is_file()) {
            return Err(StoreError::Conflict(format!(
                "no database file at {}",
                path.display()
            )));
        }
        let conn = Connection::open_with_flags(
            immutable_uri(path),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        Ok(Self {
            conn,
            task_cache: std::cell::RefCell::new(None),
            commit_observer: None,
        })
    }

    /// Open a second connection to this Store's database file, with the same durability. A
    /// Task writer's heartbeat uses it to observe and renew its own lease while the caller's
    /// connection is busy (ADR-0128). It grants nothing by itself: every append through it
    /// passes the same validation and exact sequence fence as the caller's own. A Store with no
    /// database file (in-memory or temporary) has nothing to share and is refused.
    ///
    /// A write through it waits at most `busy_timeout` for another connection's write lock
    /// before it fails with a busy error, so its caller can still decide before a deadline.
    pub fn reopen(&self, busy_timeout: std::time::Duration) -> Result<Self, StoreError> {
        let path = self
            .conn
            .path()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| StoreError::Conflict("Store has no database file to reopen".into()))?;
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(busy_timeout)?;
        Ok(Self {
            conn,
            task_cache: std::cell::RefCell::new(None),
            commit_observer: self.commit_observer.clone(),
        })
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // FULL, not NORMAL: an accepted effect must survive process death, which is the entire
        // reason the log exists.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                 run_id         TEXT    NOT NULL,
                 sequence       INTEGER NOT NULL,
                 event_id       TEXT    NOT NULL UNIQUE,
                 type           TEXT    NOT NULL,
                 occurred_at    TEXT    NOT NULL,
                 node_id        TEXT,
                 attempt_id     TEXT,
                 causation_id   TEXT,
                 correlation_id TEXT,
                 artifact_refs  TEXT    NOT NULL,
                 payload        TEXT    NOT NULL,
                 PRIMARY KEY (run_id, sequence)
             );
             CREATE INDEX IF NOT EXISTS events_by_correlation
                 ON events (run_id, correlation_id);
             CREATE INDEX IF NOT EXISTS events_by_type_sequence
                 ON events (run_id, type, sequence DESC);
             CREATE INDEX IF NOT EXISTS events_by_causation_type_sequence
                 ON events (run_id, causation_id, type, sequence);
             CREATE INDEX IF NOT EXISTS task_execution_history
                 ON events (run_id, sequence)
                 WHERE type = 'TaskTransition@6'
                   AND json_extract(payload, '$.change.kind') = 'execution_recorded';
             CREATE TABLE IF NOT EXISTS attempt_wall (
                 run_id             TEXT    NOT NULL,
                 attempt_id         TEXT    NOT NULL,
                 node_id            TEXT    NOT NULL,
                 round              INTEGER NOT NULL,
                 epoch              INTEGER NOT NULL,
                 started_unix_ms    INTEGER NOT NULL,
                 elapsed_ms         INTEGER NOT NULL,
                 usage_v3_json      TEXT,
                 usage_observation_v1_json TEXT,
                 PRIMARY KEY (run_id, attempt_id)
             );
             CREATE TABLE IF NOT EXISTS attempt_wall_end (
                 run_id             TEXT    NOT NULL,
                 attempt_id         TEXT    NOT NULL,
                 wall_end_id        TEXT    NOT NULL,
                 PRIMARY KEY (run_id, attempt_id)
             );",
        )?;
        // The table only: this release's marker is written with its first appended event.
        conn.execute_batch(writers::CREATE)?;
        Ok(Self {
            conn,
            task_cache: std::cell::RefCell::new(None),
            commit_observer: None,
        })
    }

    /// Read exactly one canonical Scenario template pin without replaying its stream.
    /// The indexed probe visits at most two sequence entries (including negative ones).
    /// Each returned text column is capped at 256 UTF-8 bytes in SQLite, before Rust
    /// allocates or decodes it; optional metadata is observed only as NULL predicates.
    /// No CAS access occurs here, so this is safe inside pin publication's transaction.
    pub fn scenario_template_pin(&self, run_id: &str) -> Result<String, StoreError> {
        let invalid = || StoreError::Conflict("Template has no exact durable native pin".into());
        let Some(digest) = run_id.strip_prefix("scenario-template-") else {
            return Err(invalid());
        };
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let mut statement = self.conn.prepare(SCENARIO_PIN_SQL)?;
        let mut rows = statement.query([run_id])?;
        let row = rows.next()?.ok_or_else(invalid)?;
        let mut fields = Vec::with_capacity(5);
        for column in 0..5 {
            fields.push(row.get::<_, Option<String>>(column)?.ok_or_else(invalid)?);
        }
        if !row.get::<_, bool>(5)?
            || fields[0] != derive_event_id(run_id, 0)
            || fields[1] != EventType::ScenarioTemplatePinnedV1.to_string()
            || fields[2] != "1970-01-01T00:00:00Z"
        {
            return Err(invalid());
        }
        let payload: serde_json::Value = serde_json::from_str(&fields[3])?;
        let references: Vec<String> = serde_json::from_str(&fields[4])?;
        let artifact = payload["artifact_id"].as_str().ok_or_else(invalid)?;
        af_contracts::event::validate_event_payload(EventType::ScenarioTemplatePinnedV1, &payload)
            .map_err(|_| invalid())?;
        if payload != serde_json::json!({"artifact_id": artifact}) || references != [artifact] {
            return Err(invalid());
        }
        Ok(artifact.to_owned())
    }

    /// Append one event, assigning it the next sequence for its run.
    ///
    /// Every referenced artifact must already be in `cas`, and durable before the row lands.
    /// This is the enforcement point for publication order — the CAS defers its syncs to
    /// exactly this barrier, so it is refusal or `flush`, never a log that replays into bytes
    /// the filesystem forgot.
    pub fn append(
        &mut self,
        run_id: &str,
        cas: &Cas,
        event: NewEvent,
    ) -> Result<RunEvent, StoreError> {
        self.append_batch(run_id, cas, std::slice::from_ref(&event))?
            .into_iter()
            .next()
            .ok_or_else(|| StoreError::Conflict("single-event append produced no event".into()))
    }

    /// Append only when the run is empty, under the same writer lock as the insert.
    /// `None` means another event already exists; callers must validate that history
    /// before treating creation as idempotent success.
    pub fn append_if_empty(
        &mut self,
        run_id: &str,
        cas: &Cas,
        event: NewEvent,
    ) -> Result<Option<RunEvent>, StoreError> {
        Ok(self
            .append_batch_committed(run_id, cas, &[event], None, None, true)?
            .into_iter()
            .next())
    }

    /// Capture a Scenario template under a process-owned CAS reservation, leaving SQLite
    /// available for Task heartbeats. `capture` must validate the complete captured closure;
    /// `validate` checks the exact durable pin, including an already existing stream. Any
    /// failure rolls back publication. CAS objects left by a failed capture remain scratch.
    /// All CAS verification belongs in `capture`; `validate` must use only SQLite and bounded
    /// in-memory checks, because it runs under the writer lock.
    pub fn capture_scenario_template<T>(
        &self,
        cas: &Cas,
        capture: impl FnOnce() -> Result<(String, NewEvent, T), String>,
        validate: impl FnOnce(&Self, &T) -> Result<(), String>,
    ) -> Result<T, String> {
        let _capture = cas.hold_capture(false).map_err(|e| e.to_string())?;
        let (run, event, value) = capture()?;
        if event.event_type != EventType::ScenarioTemplatePinnedV1 {
            return Err("Template capture requires a Scenario template pin event".into());
        }
        // Captured source bytes can leave thousands of pending syncs. The reservation
        // protects them through this durability barrier without blocking SQLite renewal.
        self.prepare_event_artifacts(cas, std::slice::from_ref(&event))
            .map_err(|e| e.to_string())?;
        let verified = VerifiedScenarioPin {
            event: &event,
            _reservation: &_capture,
        };
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|e| e.to_string())?;
        let appended = if self.is_empty(&run).map_err(|e| e.to_string())? {
            verified.insert(&tx, &run).map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };
        validate(self, &value)?;
        tx.commit().map_err(|e| e.to_string())?;
        self.committed(cas, &appended, None);
        Ok(value)
    }

    /// Atomically append an ordered event batch.
    ///
    /// Every payload and artifact reference is validated before the publication barrier. The
    /// CAS is flushed once, then every row lands in one FULL-synchronous SQLite transaction, so
    /// replay observes the complete logical effect or none of it.
    pub fn append_batch(
        &mut self,
        run_id: &str,
        cas: &Cas,
        events: &[NewEvent],
    ) -> Result<Vec<RunEvent>, StoreError> {
        self.append_batch_inner(run_id, cas, events, None)
    }

    fn append_batch_inner(
        &mut self,
        run_id: &str,
        cas: &Cas,
        events: &[NewEvent],
        task_permit: Option<&task::WritePermit>,
    ) -> Result<Vec<RunEvent>, StoreError> {
        self.append_batch_observed(run_id, cas, events, task_permit, None)
    }

    /// [`Self::append_batch_inner`], telling the commit observer of `applied`, the Task
    /// projection the one Task transition of `events` leaves, beside the committed events.
    fn append_batch_observed(
        &mut self,
        run_id: &str,
        cas: &Cas,
        events: &[NewEvent],
        task_permit: Option<&task::WritePermit>,
        applied: Option<&task::TaskProjection>,
    ) -> Result<Vec<RunEvent>, StoreError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let span = store_span("af.store.append", "append");
        let appended = span.in_scope(|| {
            self.append_batch_committed(run_id, cas, events, task_permit, applied, false)
        });
        if !span.is_disabled() {
            let bytes = events
                .iter()
                .map(|event| event.payload.to_string().len() as u64)
                .fold(0_u64, u64::saturating_add);
            record_store(&span, events.len(), bytes, appended.as_ref().err());
        }
        appended
    }

    fn append_batch_committed(
        &mut self,
        run_id: &str,
        cas: &Cas,
        events: &[NewEvent],
        task_permit: Option<&task::WritePermit>,
        applied: Option<&task::TaskProjection>,
        require_empty: bool,
    ) -> Result<Vec<RunEvent>, StoreError> {
        if events.iter().any(|e| {
            matches!(
                e.event_type,
                EventType::TaskTransitionV6
                    | EventType::TaskExecutionStartedV1
                    | EventType::TaskGateRetriedV1
                    | EventType::TaskProviderEventV1
            )
        }) && task_permit.is_none()
        {
            return Err(StoreError::Conflict(
                "Task events require the trusted Task entry point".into(),
            ));
        }
        self.prepare_event_artifacts(cas, events)?;

        // Acquire the writer lock before reading aggregate state. A deferred transaction lets
        // two openers both observe an empty log and only races at INSERT; IMMEDIATE makes
        // the compare-and-append decision itself serial.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if require_empty {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1)",
                [run_id],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(Vec::new());
            }
        }
        let first: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence) + 1, 0) FROM events WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if let Some(permit) = task_permit {
            permit.validate(&tx, run_id, first, events)?;
        } else {
            let task_log: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE run_id = ?1 AND type = 'TaskTransition@6')",
                [run_id], |row| row.get(0),
            )?;
            if task_log {
                return Err(StoreError::Conflict(
                    "Task log cannot accept generic append authority".into(),
                ));
            }
            if events.iter().any(|event| {
                !matches!(
                    event.event_type,
                    EventType::SourceCapturedV1
                        | EventType::ScenarioTemplatePinnedV1
                        | EventType::CheckCompletedV1
                        | EventType::ProjectRegisteredV1
                        | EventType::WorkspaceRegisteredV1
                        | EventType::WorkspaceRemovedV1
                        | EventType::WorkspaceDeliveredV1
                        | EventType::SurveyRecordedV1
                        | EventType::SurveyFailedV1
                        | EventType::SurveyCollectedV1
                )
            }) {
                return Err(StoreError::Conflict("Review domain events require native Task publication; Campaign execution is retired".into()));
            }
        }
        let appended = insert_events(&tx, cas, run_id, events, first)?;
        tx.commit()?;
        self.committed(cas, &appended, applied);
        Ok(appended)
    }

    fn prepare_event_artifacts(&self, cas: &Cas, events: &[NewEvent]) -> Result<(), StoreError> {
        let mut verified = std::collections::BTreeSet::new();
        for event in events {
            af_contracts::json::admit(&event.payload)
                .map_err(|error| StoreError::Conflict(format!("invalid event payload: {error}")))?;
            af_contracts::event::validate_event_payload(event.event_type, &event.payload)
                .map_err(|error| StoreError::Conflict(format!("invalid event payload: {error}")))?;
            for digest in &event.artifact_refs {
                if verified.insert(digest) {
                    cas.prepare_for_publication(digest)
                        .map_err(|error| match error {
                            CasError::NotFound { .. } | CasError::InvalidDigest(_) => {
                                StoreError::DanglingArtifact {
                                    digest: digest.clone(),
                                }
                            }
                            other => StoreError::Artifact(format!(
                                "referenced artifact {digest} failed verification: {other}"
                            )),
                        })?;
                }
            }
        }
        if !verified.is_empty() {
            cas.flush()
                .map_err(|e| StoreError::Durability(e.to_string()))?;
        }
        Ok(())
    }

    /// Stable identifiers for every run that has at least one event.
    pub fn run_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT run_id FROM events ORDER BY run_id")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Sqlite)
    }

    /// The first event type this release does not know, when another af release wrote one
    /// into this log: such a Store cannot be replayed here at all, only removed whole by the
    /// Storage Budget (ADR-0144). `None` when every type is known.
    pub fn unknown_event_type(&self) -> Result<Option<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT type FROM events ORDER BY type")?;
        let types = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(types
            .into_iter()
            .find(|kind| kind.parse::<af_contracts::EventType>().is_err()))
    }

    /// Every event of a run, in sequence order. This is the only read replay needs.
    pub fn replay(&self, run_id: &str) -> Result<Vec<RunEvent>, StoreError> {
        self.replay_from(run_id, 0)
    }

    /// Record the exact prefixes replayed by this operation, including nested projections.
    /// These watermarks grant no authority; byte verification and a final watermark check
    /// are required before an observation can be reused.
    pub fn track_replays<T>(
        &self,
        read: impl FnOnce() -> T,
    ) -> (T, std::collections::BTreeMap<String, u64>) {
        REPLAY_READS.with(|reads| {
            reads.borrow_mut().push(ReplayReads {
                store: self as *const Self as usize,
                prefixes: Default::default(),
            })
        });
        let guard = ReplayReadsGuard;
        let result = read();
        let prefixes = REPLAY_READS.with(|reads| {
            std::mem::take(
                &mut reads
                    .borrow_mut()
                    .last_mut()
                    .expect("replay trace")
                    .prefixes,
            )
        });
        drop(guard);
        (result, prefixes)
    }

    /// The ordered events of `run_id` read through one cursor under `budget`, shared by every
    /// run one projection reads from the same Store: each row is charged before it is decoded,
    /// and the first row past a ceiling stops the read with that ceiling, nothing of the run
    /// returned. `Ok(Err(ceiling))` says which.
    pub fn replay_within(
        &self,
        run_id: &str,
        budget: &mut ReplayBudget,
    ) -> Result<Result<Vec<RunEvent>, ReplayCeiling>, StoreError> {
        match self.replay_bounded(run_id, 0, Some(budget), &mut 0) {
            Ok(events) => Ok(Ok(events)),
            Err(Bounded::Ceiling(ceiling)) => Ok(Err(ceiling)),
            Err(Bounded::Store(error)) => Err(error),
        }
    }

    /// The ordered suffix beginning at `first_sequence`, for advancing a watermarked projection
    /// without parsing and validating the prefix it already covers.
    pub fn replay_from(
        &self,
        run_id: &str,
        first_sequence: u64,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let span = store_span("af.store.read", "replay");
        let mut bytes = 0_u64;
        let replayed = span.in_scope(|| {
            self.replay_bounded(run_id, first_sequence, None, &mut bytes)
                .map_err(|bounded| match bounded {
                    Bounded::Store(error) => error,
                    Bounded::Ceiling(ceiling) => StoreError::Conflict(ceiling.to_string()),
                })
        });
        if !span.is_disabled() {
            let events = replayed.as_ref().map_or(0, Vec::len);
            record_store(&span, events, bytes, replayed.as_ref().err());
        }
        replayed
    }

    fn replay_bounded(
        &self,
        run_id: &str,
        first_sequence: u64,
        mut budget: Option<&mut ReplayBudget>,
        bytes: &mut u64,
    ) -> Result<Vec<RunEvent>, Bounded> {
        let negative: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND sequence < 0)",
            [run_id],
            |row| row.get(0),
        )?;
        if negative {
            return Err(StoreError::Conflict(format!("negative sequence in run {run_id}")).into());
        }
        let first_sequence_sql = i64::try_from(first_sequence).map_err(|_| {
            StoreError::Conflict("event replay sequence exceeds SQLite range".into())
        })?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, type, occurred_at, node_id, attempt_id,
                    causation_id, correlation_id, artifact_refs, payload
             FROM events WHERE run_id = ?1 AND sequence >= ?2 ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![run_id, first_sequence_sql], |row| {
            let refs: String = row.get(8)?;
            let payload: String = row.get(9)?;
            let sequence: i64 = row.get(1)?;
            Ok((
                RunEvent {
                    event_id: row.get(0)?,
                    run_id: run_id.to_string(),
                    sequence: 0,
                    event_type: row
                        .get::<_, String>(2)?
                        .parse::<EventType>()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    occurred_at: row.get(3)?,
                    node_id: row.get(4)?,
                    attempt_id: row.get(5)?,
                    causation_id: row.get(6)?,
                    correlation_id: row.get(7)?,
                    artifact_refs: Vec::new(),
                    payload: Value::Null,
                },
                sequence,
                refs,
                payload,
            ))
        })?;
        let mut out = Vec::new();
        for (expected_sequence, row) in (first_sequence..).zip(rows) {
            // A row that does not parse is refused, never degraded: replaying it as an empty
            // event would rebuild a different state than the run committed, silently — the
            // exact failure the publication ordering exists to prevent, on the read side.
            let (mut event, raw_sequence, refs, payload) = row.map_err(StoreError::from)?;
            *bytes = bytes.saturating_add(payload.len() as u64);
            if let Some(budget) = budget.as_deref_mut() {
                budget.charge(refs.len() + payload.len())?;
            }
            let sequence = u64::try_from(raw_sequence).map_err(|_| {
                StoreError::Conflict(format!("negative sequence {raw_sequence} in run {run_id}"))
            })?;
            if sequence != expected_sequence {
                return Err(StoreError::Conflict(format!(
                    "event sequence gap in run {run_id}: expected {expected_sequence}, found {sequence}"
                ))
                .into());
            }
            let expected_id = derive_event_id(run_id, raw_sequence);
            if event.event_id != expected_id {
                return Err(StoreError::Conflict(format!(
                    "event {} has invalid derived id; expected {expected_id}",
                    event.event_id
                ))
                .into());
            }
            event.sequence = sequence;
            event.artifact_refs = serde_json::from_str(&refs).map_err(StoreError::Json)?;
            event.payload = serde_json::from_str(&payload).map_err(StoreError::Json)?;
            af_contracts::event::validate_event_payload(event.event_type, &event.payload).map_err(
                |error| StoreError::Conflict(format!("invalid replayed event payload: {error}")),
            )?;
            out.push(event);
        }
        REPLAY_READS.with(|reads| {
            for read in reads
                .borrow_mut()
                .iter_mut()
                .filter(|read| read.store == self as *const Self as usize)
            {
                let prefix = out
                    .last()
                    .map_or(first_sequence, |event| event.sequence + 1);
                // A writer advancing between two reads invalidates reuse: retain the older
                // prefix so the final comparison cannot bless a mixed observation.
                read.prefixes
                    .entry(run_id.to_owned())
                    .and_modify(|old| *old = (*old).min(prefix))
                    .or_insert(prefix);
            }
        });
        Ok(out)
    }

    pub fn len(&self, run_id: &str) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn is_empty(&self, run_id: &str) -> Result<bool, StoreError> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1)",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(!exists)
    }
}

/// A Store phase span (`af.store.append`, `af.store.read`; telemetry design §7.1), opened only
/// inside a span of a run, so a Store touched outside one records nothing. Telemetry
/// only: what the Store commits or returns never depends on it (ADR-0153).
fn store_span(name: &'static str, op: &'static str) -> tracing::Span {
    if !af_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    match name {
        "af.store.append" => tracing::info_span!(
            "af.store.append",
            af.store.op = op,
            af.store.events = tracing::field::Empty,
            af.store.bytes = tracing::field::Empty,
            af.outcome = tracing::field::Empty,
            af.failure.class = tracing::field::Empty,
            af.failure.reason = tracing::field::Empty,
            error.type = tracing::field::Empty,
            af.status = tracing::field::Empty,
        ),
        _ => tracing::info_span!(
            "af.store.read",
            af.store.op = op,
            af.store.events = tracing::field::Empty,
            af.store.bytes = tracing::field::Empty,
            af.outcome = tracing::field::Empty,
            af.failure.class = tracing::field::Empty,
            af.failure.reason = tracing::field::Empty,
            error.type = tracing::field::Empty,
            af.status = tracing::field::Empty,
        ),
    }
}

/// End a Store phase span: its event count and payload bytes, `ok` or the Store's refusal.
fn record_store(span: &tracing::Span, events: usize, bytes: u64, error: Option<&StoreError>) {
    span.record("af.store.events", events as u64);
    span.record("af.store.bytes", bytes);
    match error {
        None => {
            span.record("af.outcome", "succeeded");
            span.record("af.status", "ok");
        }
        Some(error) => {
            span.record("af.outcome", "failed");
            span.record("af.status", "error");
            span.record("af.failure.class", "refused");
            span.record("error.type", "refused");
            span.record("af.failure.reason", error.to_string().as_str());
        }
    }
}

#[allow(dead_code)]
fn decode_stored_event(run_id: &str, row: StoredEventRow) -> Result<RunEvent, StoreError> {
    let sequence = u64::try_from(row.sequence).map_err(|_| {
        StoreError::Conflict(format!(
            "negative sequence {} in run {run_id}",
            row.sequence
        ))
    })?;
    let expected_id = derive_event_id(run_id, row.sequence);
    if row.event_id != expected_id {
        return Err(StoreError::Conflict(format!(
            "event {} has invalid derived id; expected {expected_id}",
            row.event_id
        )));
    }
    let event_type = row
        .event_type
        .parse::<EventType>()
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let artifact_refs = serde_json::from_str(&row.artifact_refs)?;
    let payload = serde_json::from_str(&row.payload)?;
    af_contracts::event::validate_event_payload(event_type, &payload).map_err(|error| {
        StoreError::Conflict(format!("invalid replayed event payload: {error}"))
    })?;
    Ok(RunEvent {
        event_id: row.event_id,
        run_id: run_id.to_string(),
        sequence,
        event_type,
        occurred_at: row.occurred_at,
        node_id: row.node_id,
        attempt_id: row.attempt_id,
        causation_id: row.causation_id,
        correlation_id: row.correlation_id,
        artifact_refs,
        payload,
    })
}

// MATERIALIZED bounds the complete history probe, independently of the outer lookup.
// The primary key covers (run_id, sequence), so no sort, table scan or COUNT is needed.
// Bundled SQLite's octet_length uses OPFLAG_BYTELENARG on the direct column: it
// reads the record's byte length without loading text from overflow pages or counting
// Unicode characters. CASE fetches the actual column only after its budget passes,
// preventing oversized allocations inside SQLite as well as in Rust.
const SCENARIO_PIN_SQL: &str = "
    WITH pins AS MATERIALIZED (
        SELECT sequence FROM events WHERE run_id=?1 ORDER BY sequence LIMIT 2
    )
    SELECT
        CASE WHEN octet_length(event_id) <= 256 THEN event_id END,
        CASE WHEN octet_length(type) <= 256 THEN type END,
        CASE WHEN octet_length(occurred_at) <= 256 THEN occurred_at END,
        CASE WHEN octet_length(payload) <= 256 THEN payload END,
        CASE WHEN octet_length(artifact_refs) <= 256 THEN artifact_refs END,
        node_id IS NULL AND attempt_id IS NULL AND causation_id IS NULL
            AND correlation_id IS NULL
    FROM events WHERE run_id=?1 AND sequence=0
        AND (SELECT sequence FROM pins LIMIT 1)=0
        AND NOT EXISTS (SELECT 1 FROM pins LIMIT 1 OFFSET 1)";

fn derive_event_id(run_id: &str, sequence: i64) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"review.kernel/event-id/v1\0");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(sequence.to_string().as_bytes());
    af_contracts::hex::encode(&hasher.finalize())[..26].to_string()
}

fn insert_events(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    events: &[NewEvent],
    first: i64,
) -> Result<Vec<RunEvent>, StoreError> {
    // Every reference was verified before the writer lock was taken. Task collection removes
    // objects only while it holds this same lock (ADR-0135), so a reference is rechecked here,
    // where no sweep can run: an object a sweep removed after that verification is refused as
    // dangling, never committed.
    for event in events {
        if let Some(digest) = event
            .artifact_refs
            .iter()
            .find(|digest| !cas.is_filed(digest))
        {
            return Err(StoreError::DanglingArtifact {
                digest: digest.clone(),
            });
        }
    }
    insert_preverified_events(tx, run_id, events, first)
}

// Only constructed by capture_scenario_template, after verification and flush under its
// live shared reservation. Collection cannot invalidate these references before commit.
struct VerifiedScenarioPin<'a> {
    event: &'a NewEvent,
    _reservation: &'a nix::fcntl::Flock<std::fs::File>,
}

impl VerifiedScenarioPin<'_> {
    fn insert(
        &self,
        tx: &rusqlite::Transaction<'_>,
        run_id: &str,
    ) -> Result<Vec<RunEvent>, StoreError> {
        insert_preverified_events(tx, run_id, std::slice::from_ref(self.event), 0)
    }
}

// SQLite and in-memory work only. Ordinary append must go through insert_events' CAS
// recheck; only a verified, reservation-protected Scenario pin may bypass that recheck.
fn insert_preverified_events(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    events: &[NewEvent],
    first: i64,
) -> Result<Vec<RunEvent>, StoreError> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    // Every event lands with this release's writer marker, in the same transaction.
    writers::mark(
        tx,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX)),
    )?;
    let mut appended = Vec::with_capacity(events.len());
    for (offset, event) in events.iter().enumerate() {
        let offset = i64::try_from(offset)
            .map_err(|_| StoreError::Conflict("event batch is too large".into()))?;
        let next = first
            .checked_add(offset)
            .ok_or_else(|| StoreError::Conflict("event sequence overflow".into()))?;
        let event_id = derive_event_id(run_id, next);
        let refs = serde_json::to_string(&event.artifact_refs)?;
        let payload = serde_json::to_string(&event.payload)?;
        tx.execute(
            "INSERT INTO events
                   (run_id, sequence, event_id, type, occurred_at, node_id, attempt_id,
                    causation_id, correlation_id, artifact_refs, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run_id,
                next,
                event_id,
                event.event_type.as_str(),
                event.occurred_at,
                event.node_id,
                event.attempt_id,
                event.causation_id,
                event.correlation_id,
                refs,
                payload,
            ],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(err, _)
                if matches!(
                    err.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                        | rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                ) =>
            {
                StoreError::Conflict(format!("sequence {next} already taken for run {run_id}"))
            }
            other => StoreError::Sqlite(other),
        })?;
        appended.push(RunEvent {
            event_id,
            run_id: run_id.to_string(),
            sequence: next as u64,
            event_type: event.event_type,
            occurred_at: event.occurred_at.clone(),
            node_id: event.node_id.clone(),
            attempt_id: event.attempt_id.clone(),
            causation_id: event.causation_id.clone(),
            correlation_id: event.correlation_id.clone(),
            artifact_refs: event.artifact_refs.clone(),
            payload: event.payload.clone(),
        });
    }
    Ok(appended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace_root() -> std::path::PathBuf {
        std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
    }

    fn fixture() -> (tempfile::TempDir, EventStore, Cas) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        (dir, store, cas)
    }

    /// Every file beneath `directory` and its bytes.
    fn files(directory: &Path) -> Vec<(String, Vec<u8>)> {
        let mut found: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                (name, std::fs::read(entry.path()).unwrap())
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn an_unchanged_reader_writes_nothing_and_reads_an_unwritable_database() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("a?b#c%d e");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("events.sqlite");
        {
            let mut store = EventStore::open(&path).unwrap();
            let cas = Cas::open(root.path().join("cas")).unwrap();
            store
                .append(
                    "run",
                    &cas,
                    NewEvent::new(EventType::SourceCapturedV1, json!({ "i": 1 })),
                )
                .unwrap();
        }
        let before = files(&directory);
        assert_eq!(before.len(), 1, "a closed writer leaves its database alone");
        {
            let reader = EventStore::open_unchanged(&path).unwrap();
            assert_eq!(reader.replay("run").unwrap().len(), 1);
            assert!(reader.replay("other").unwrap().is_empty());
        }
        assert_eq!(files(&directory), before);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
        let read = EventStore::open_unchanged(&path).and_then(|reader| reader.replay("run"));
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(read.unwrap().len(), 1);
        assert_eq!(files(&directory), before);
        assert!(EventStore::open_unchanged(directory.join("absent.sqlite")).is_err());
        assert_eq!(files(&directory), before);
    }

    #[test]
    fn database_observation_detects_both_connections_and_rejects_pinned_snapshots() {
        let (dir, mut writer, cas) = fixture();
        let reader = EventStore::open_read_only(dir.path().join("events.sqlite")).unwrap();
        let observed = reader.database_observation().unwrap();
        assert!(reader.database_observation_unchanged(&observed).unwrap());
        assert!(!writer.database_observation_unchanged(&observed).unwrap());
        let own = writer.database_observation().unwrap();
        writer
            .append(
                "new-stream",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({"i": 1})),
            )
            .unwrap();
        assert!(!writer.database_observation_unchanged(&own).unwrap());
        assert!(!reader.database_observation_unchanged(&observed).unwrap());

        let observed = reader.database_observation().unwrap();
        let mut statement = reader.conn.prepare("SELECT * FROM events").unwrap();
        let mut rows = statement.query([]).unwrap();
        assert!(rows.next().unwrap().is_some());
        assert!(reader.database_observation().is_err());
        assert!(reader.database_observation_unchanged(&observed).is_err());
        writer
            .append(
                "new-stream",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({"i": 2})),
            )
            .unwrap();
        drop(rows);
        drop(statement);
        assert!(!reader.database_observation_unchanged(&observed).unwrap());
        reader.conn.execute_batch("BEGIN").unwrap();
        assert!(reader.database_observation().is_err());
        reader.conn.execute_batch("ROLLBACK").unwrap();

        // Corruption/repair of an indexed row must invalidate too, even without an append.
        for sql in [
            "UPDATE events SET occurred_at='changed' WHERE run_id='new-stream'",
            "DELETE FROM events WHERE run_id='new-stream'",
        ] {
            let observed = reader.database_observation().unwrap();
            let own = writer.database_observation().unwrap();
            writer.conn.execute(sql, []).unwrap();
            assert!(!reader.database_observation_unchanged(&observed).unwrap());
            assert!(!writer.database_observation_unchanged(&own).unwrap());
        }
        let observed = reader.database_observation().unwrap();
        reader.replay("absent").unwrap();
        assert!(reader.database_observation_unchanged(&observed).unwrap());
    }

    #[test]
    fn replay_traces_include_nested_prefixes_but_not_another_connection_and_unwind_cleanly() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events.sqlite");
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let mut writer = EventStore::open(&path).unwrap();
        writer
            .append(
                "run",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({"i": 1})),
            )
            .unwrap();
        let reader = EventStore::open_read_only(&path).unwrap();
        let ((_, nested), outer) = reader.track_replays(|| {
            writer.replay("run").unwrap();
            reader.track_replays(|| reader.replay("run").unwrap())
        });
        assert_eq!(
            nested,
            std::collections::BTreeMap::from([("run".into(), 1)])
        );
        assert_eq!(outer, nested);
        let (_, only_other) = reader.track_replays(|| writer.replay("run").unwrap());
        assert!(only_other.is_empty());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.track_replays(|| panic!("unwind the trace"));
        }));
        assert!(REPLAY_READS.with(|reads| reads.borrow().is_empty()));
        let (_, changed) = reader.track_replays(|| {
            reader.replay("run").unwrap();
            writer
                .append(
                    "run",
                    &cas,
                    NewEvent::new(EventType::SourceCapturedV1, json!({"i": 2})),
                )
                .unwrap();
            reader.replay("run").unwrap();
        });
        assert_eq!(
            changed["run"], 1,
            "a mixed read cannot bless the newer prefix"
        );
    }

    /// While a writer's log still holds frames the reader reads them, events the database file
    /// does not hold yet.
    #[test]
    fn an_unchanged_reader_reads_the_frames_of_an_open_log() {
        let (dir, mut store, cas) = fixture();
        store
            .append(
                "run",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({ "i": 1 })),
            )
            .unwrap();
        let wal = dir.path().join("events.sqlite-wal");
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        let reader = EventStore::open_unchanged(dir.path().join("events.sqlite")).unwrap();
        assert_eq!(reader.replay("run").unwrap(), store.replay("run").unwrap());
    }

    /// One bounded read over the runs of a Store charges every row before it is decoded: the
    /// rows, one row's bytes and the bytes of every row read, across runs, each stop the read
    /// with its own ceiling and return nothing of that run; within them it is `replay`.
    #[test]
    fn a_bounded_replay_stops_at_each_discovery_ceiling() {
        let (_dir, mut store, cas) = fixture();
        for (run, count) in [("run-a", 5), ("run-b", 2)] {
            for i in 0..count {
                store
                    .append(
                        run,
                        &cas,
                        NewEvent::new(EventType::SourceCapturedV1, json!({ "i": i })),
                    )
                    .unwrap();
            }
        }
        let row = |i: u64| (json!({ "i": i }).to_string().len() + "[]".len()) as u64;
        let mut ample = ReplayBudget::new(7, 1 << 20, 16 << 20);
        let read = store.replay_within("run-a", &mut ample).unwrap().unwrap();
        assert_eq!(read, store.replay("run-a").unwrap());
        assert_eq!(
            store
                .replay_within("run-b", &mut ample)
                .unwrap()
                .unwrap()
                .len(),
            2
        );

        // Rows: the budget is shared by the runs of one projection.
        let mut rows = ReplayBudget::new(6, 1 << 20, 16 << 20);
        assert_eq!(
            store
                .replay_within("run-a", &mut rows)
                .unwrap()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            store.replay_within("run-b", &mut rows).unwrap(),
            Err(ReplayCeiling::Rows)
        );
        // One row's bytes.
        let mut row_bytes = ReplayBudget::new(100, row(0) - 1, 16 << 20);
        assert_eq!(
            store.replay_within("run-a", &mut row_bytes).unwrap(),
            Err(ReplayCeiling::RowBytes { bytes: row(0) })
        );
        // Every row's bytes.
        let mut buffered = ReplayBudget::new(100, 1 << 20, row(0) * 3);
        assert_eq!(
            store.replay_within("run-a", &mut buffered).unwrap(),
            Err(ReplayCeiling::BufferedBytes)
        );
        for (ceiling, name) in [
            (ReplayCeiling::Rows, "max_rows"),
            (ReplayCeiling::RowBytes { bytes: 1 }, "max_row_bytes"),
            (ReplayCeiling::BufferedBytes, "max_buffered_bytes"),
        ] {
            assert_eq!(ceiling.as_str(), name);
            assert!(ceiling.to_string().contains(name));
        }
    }

    #[test]
    fn sequences_are_dense_and_start_at_zero() {
        let (_dir, mut store, cas) = fixture();
        for i in 0..5 {
            let event = store
                .append(
                    "run-a",
                    &cas,
                    NewEvent::new(EventType::SourceCapturedV1, json!({ "i": i })),
                )
                .unwrap();
            assert_eq!(event.sequence, i as u64);
        }
        let replayed = store.replay("run-a").unwrap();
        assert_eq!(
            replayed.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn runs_do_not_share_a_sequence_space() {
        let (_dir, mut store, cas) = fixture();
        store
            .append(
                "run-a",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({})),
            )
            .unwrap();
        let b = store
            .append(
                "run-b",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({})),
            )
            .unwrap();
        assert_eq!(b.sequence, 0);
    }

    #[test]
    fn an_event_cannot_reference_an_artifact_that_is_not_durable() {
        let (_dir, mut store, cas) = fixture();
        let missing = crate::canonical::blob_content_id(b"never stored");
        let err = store
            .append(
                "run-a",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({}))
                    .referencing(vec![missing.clone()]),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::DanglingArtifact { .. }));
        assert!(store.is_empty("run-a").unwrap(), "nothing may be recorded");

        let digest = cas.put(b"never stored").unwrap();
        assert_eq!(digest, missing);
        assert!(
            store
                .append(
                    "run-a",
                    &cas,
                    NewEvent::new(EventType::SourceCapturedV1, json!({})).referencing(vec![digest])
                )
                .is_ok()
        );
    }

    #[test]
    fn reviewer_result_admission_accepts_only_the_live_flat_shape() {
        let result = |report| {
            json!({
                "reports": [report],
                "benchmark_demands": [],
                "dispositions": [],
            })
        };
        let flat = json!({
            "severity": "major",
            "file": "src/a.rs",
            "line": 1,
            "title": "flat",
            "body": "body",
            "fix": "fix",
            "confidence": 0.9,
        });
        let typed = json!({
            "title": "typed",
            "severity": "major",
            "locations": [{"path": "src/a.rs", "line": 1}],
            "body": "body",
            "fix": "fix",
            "confidence": 0.9,
        });

        assert!(af_contracts::validate_reviewer_result_v2(&result(flat)).is_ok());
        assert!(af_contracts::validate_reviewer_result_v2(&result(typed)).is_err());
    }

    #[test]
    fn reviewer_result_conformance_corpus_matches_durable_reader() {
        let path = workspace_root().join("schemas/reviewer-result-v2-conformance.json");
        let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for case in corpus["valid"].as_array().unwrap() {
            assert!(
                af_contracts::validate_reviewer_result_v2(&case["payload"]).is_ok(),
                "{}",
                case["name"]
            );
        }
        for case in corpus["invalid"].as_array().unwrap() {
            assert!(
                af_contracts::validate_reviewer_result_v2(&case["payload"]).is_err(),
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn event_ids_are_derived_so_replay_reproduces_them() {
        let (_dir, mut store, cas) = fixture();
        let first = store
            .append(
                "run-a",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({})),
            )
            .unwrap();
        assert_eq!(first.event_id, derive_event_id("run-a", 0));
        assert_ne!(derive_event_id("run-a", 0), derive_event_id("run-b", 0));
    }

    #[test]
    fn concurrent_append_if_empty_publishes_exactly_one_event() {
        let (dir, store, cas) = fixture();
        let path = store.database_path().unwrap();
        let cas_path = dir.path().join("cas");
        let barrier = std::sync::Barrier::new(8);
        let winners = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut contender = EventStore::open(&path).unwrap();
                        let cas = Cas::open(&cas_path).unwrap();
                        assert!(contender.is_empty("pin").unwrap());
                        barrier.wait();
                        contender
                            .append_if_empty(
                                "pin",
                                &cas,
                                NewEvent::new(EventType::SourceCapturedV1, json!({})),
                            )
                            .unwrap()
                            .is_some()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(winners, 1);
        let before = store.replay("pin").unwrap();
        assert_eq!(before.len(), 1);
        let mut reopened = EventStore::open(&path).unwrap();
        assert!(
            reopened
                .append_if_empty(
                    "pin",
                    &cas,
                    NewEvent::new(EventType::SourceCapturedV1, json!({"conflict":true}))
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(reopened.replay("pin").unwrap(), before);
    }

    #[test]
    fn template_capture_validation_failure_rolls_back_its_root() {
        let (_dir, store, cas) = fixture();
        let artifact = cas.put_json(&json!({"captured":"template"})).unwrap();
        let error = store
            .capture_scenario_template(
                &cas,
                || {
                    Ok((
                        "pin".into(),
                        NewEvent::new(
                            EventType::ScenarioTemplatePinnedV1,
                            json!({"artifact_id":artifact}),
                        )
                        .referencing(vec![artifact.clone()]),
                        (),
                    ))
                },
                |locked, ()| {
                    assert_eq!(locked.replay("pin").unwrap().len(), 1);
                    Err("incomplete captured closure".into())
                },
            )
            .unwrap_err();
        assert_eq!(error, "incomplete captured closure");
        assert!(store.is_empty("pin").unwrap());
        // Failure releases the process reservation along with rolling back the root. Native
        // collection can recover the unpublished objects immediately, with no stale lease.
        let mut collector = store.reopen(std::time::Duration::ZERO).unwrap();
        collector.apply_task_collection(&cas, 0, 0, false).unwrap();
        assert!(cas.get_json(&artifact).is_err());
    }

    #[test]
    fn template_capture_refuses_a_collector_before_reading_or_reusing_objects() {
        let (_dir, store, cas) = fixture();
        let collection = cas.hold_capture(true).unwrap();
        let error = store
            .capture_scenario_template(
                &cas,
                || -> Result<(String, NewEvent, ()), String> {
                    panic!("capture must not start while collection owns the CAS")
                },
                |_, _| Ok(()),
            )
            .unwrap_err();
        assert!(error.contains("CAS capture/collection reservation"));
        drop(collection);
        assert!(cas.hold_capture(false).is_ok());
    }

    #[test]
    fn append_if_empty_and_replay_refuse_corrupt_sequences_without_mutation() {
        for sequences in [vec![-1], vec![-9], vec![-1, 0], vec![1], vec![0, 2]] {
            let (_dir, mut store, cas) = fixture();
            for sequence in &sequences {
                store.conn.execute(
                    "INSERT INTO events (run_id,sequence,event_id,type,occurred_at,artifact_refs,payload)
                     VALUES ('pin',?1,?2,'SourceCaptured@1','2026-11-13T00:00:00Z','[]','{}')",
                    params![sequence, derive_event_id("pin", *sequence)],
                ).unwrap();
            }
            assert!(
                store
                    .append_if_empty(
                        "pin",
                        &cas,
                        NewEvent::new(EventType::SourceCapturedV1, json!({}))
                    )
                    .unwrap()
                    .is_none()
            );
            assert!(store.replay("pin").is_err(), "{sequences:?}");
            let after: Vec<i64> = store
                .conn
                .prepare("SELECT sequence FROM events WHERE run_id='pin' ORDER BY sequence")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(after, sequences);
        }
    }

    #[test]
    fn scenario_pin_read_bounds_rows_bytes_and_query_work() {
        let (_dir, mut store, cas) = fixture();
        let run = format!("scenario-template-{}", "a".repeat(64));
        let artifact = cas.put_json(&json!({"template": true})).unwrap();
        let event = NewEvent::new(
            EventType::ScenarioTemplatePinnedV1,
            json!({"artifact_id":artifact}),
        )
        .referencing(vec![artifact.clone()]);
        store.append(&run, &cas, event.clone()).unwrap();
        assert_eq!(store.scenario_template_pin(&run).unwrap(), artifact);
        for field in ["payload", "artifact_refs"] {
            let original: String = store
                .conn
                .query_row(
                    &format!("SELECT {field} FROM events WHERE run_id=?1"),
                    [&run],
                    |r| r.get(0),
                )
                .unwrap();
            // Whitespace remains semantically exact up to the byte budget, inclusive.
            for bytes in [255, 256, 257] {
                let padded = format!("{original:bytes$}");
                store
                    .conn
                    .execute(
                        &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
                        params![padded, run],
                    )
                    .unwrap();
                assert_eq!(store.scenario_template_pin(&run).is_ok(), bytes <= 256);
            }
            store
                .conn
                .execute(
                    &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
                    params![original, run],
                )
                .unwrap();
        }
        for field in [
            "event_id",
            "type",
            "occurred_at",
            "node_id",
            "attempt_id",
            "causation_id",
            "correlation_id",
            "payload",
            "artifact_refs",
        ] {
            let original: Option<String> = store
                .conn
                .query_row(
                    &format!("SELECT {field} FROM events WHERE run_id=?1"),
                    [&run],
                    |r| r.get(0),
                )
                .unwrap();
            // 129 characters but 258 bytes: a character-count guard would be insufficient.
            let oversized = "é".repeat(129);
            store
                .conn
                .execute(
                    &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
                    params![oversized, run],
                )
                .unwrap();
            assert!(store.scenario_template_pin(&run).is_err(), "{field}");
            let after: String = store
                .conn
                .query_row(
                    &format!("SELECT {field} FROM events WHERE run_id=?1"),
                    [&run],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(after, oversized);
            store
                .conn
                .execute(
                    &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
                    params![original, run],
                )
                .unwrap();
        }
        for sequence in [-1, 1, 9] {
            store
                .conn
                .execute(
                    "UPDATE events SET sequence=?1 WHERE run_id=?2",
                    params![sequence, run],
                )
                .unwrap();
            assert!(store.scenario_template_pin(&run).is_err());
        }
        store
            .conn
            .execute("UPDATE events SET sequence=0 WHERE run_id=?1", [&run])
            .unwrap();
        store.append_batch(&run, &cas, &vec![event; 4096]).unwrap();
        assert!(store.scenario_template_pin(&run).is_err());
        let mut query = store.conn.prepare(SCENARIO_PIN_SQL).unwrap();
        assert!(!query.exists([&run]).unwrap());
        assert!(query.get_status(rusqlite::StatementStatus::VmStep) < 200);
        // SQLite counts the single step to the second row of the bounded `pins`
        // materialization as a full-scan step; the event table itself is index-only probed.
        assert_eq!(query.get_status(rusqlite::StatementStatus::FullscanStep), 1);
        assert_eq!(query.get_status(rusqlite::StatementStatus::Sort), 0);
        // Audit the byte guards too: VM step counts alone cannot detect an opcode
        // that loads a huge text value. All five size probes must use metadata only.
        let byte_probes = store
            .conn
            .prepare(&format!("EXPLAIN {SCENARIO_PIN_SQL}"))
            .unwrap()
            .query_map([&run], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, u32>(6)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|(opcode, flags)| opcode == "Column" && flags & 0xc0 == 0xc0)
            .count();
        assert_eq!(byte_probes, 5);
        let plan = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {SCENARIO_PIN_SQL}"))
            .unwrap()
            .query_map([&run], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|line| line.contains("COVERING INDEX") && line.contains("run_id=?")),
            "{plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|line| line.contains("SCAN events") || line.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }

    #[test]
    fn a_reopened_store_continues_the_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        {
            let mut store = EventStore::open(&path).unwrap();
            store
                .append(
                    "run-a",
                    &cas,
                    NewEvent::new(EventType::SourceCapturedV1, json!({ "n": 0 })),
                )
                .unwrap();
            store
                .append(
                    "run-a",
                    &cas,
                    NewEvent::new(EventType::SourceCapturedV1, json!({ "n": 1 })),
                )
                .unwrap();
        }
        let mut store = EventStore::open(&path).unwrap();
        let third = store
            .append(
                "run-a",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({ "n": 2 })),
            )
            .unwrap();
        assert_eq!(third.sequence, 2);
        assert_eq!(store.replay("run-a").unwrap().len(), 3);
    }
    #[test]
    fn retired_campaign_events_refuse_replay_and_remain_foreign_storage() {
        for retired in [
            "CampaignOpened@1",
            "RoundStarted@1",
            "RoundInputSuperseded@1",
            "RunReport@6",
            "NodeInvocation@1",
            "TaskReviewResultSelected@1",
        ] {
            let (_, store, _) = fixture();
            store.conn.execute("INSERT INTO events (run_id,sequence,event_id,type,occurred_at,artifact_refs,payload) VALUES ('old',0,?1,?2,'2026-11-10T00:00:00Z','[]','{}')",params![derive_event_id("old",0),retired]).unwrap();
            assert_eq!(
                store.unknown_event_type().unwrap().as_deref(),
                Some(retired)
            );
            let error = store.replay("old").unwrap_err().to_string();
            assert!(
                error.contains(af_contracts::event::ANOTHER_RELEASE),
                "{error}"
            );
        }
    }

    #[test]
    fn canonical_review_facts_cannot_acquire_generic_store_publication_authority() {
        let (_, mut store, cas) = fixture();
        let event = NewEvent::new(
            EventType::FindingResolvedV1,
            json!({"key":"claim","status":"contested","note":"reviewer disputed it","round":1}),
        );
        let error = store.append("review", &cas, event).unwrap_err().to_string();
        assert!(error.contains("native Task publication"), "{error}");
        assert!(store.is_empty("review").unwrap());
    }

    #[test]
    fn ordinary_insert_refuses_a_reference_collected_after_preparation() {
        let (directory, mut store, cas) = fixture();
        let artifact = cas.put(b"unreserved scratch").unwrap();
        let event = NewEvent::new(EventType::SourceCapturedV1, json!({}))
            .referencing(vec![artifact.clone()]);
        store
            .prepare_event_artifacts(&cas, std::slice::from_ref(&event))
            .unwrap();
        // Ordinary append has no capture reservation. A sweep may remove its references
        // between preparation and acquiring SQLite; the insertion must still fail closed.
        let digest = artifact.strip_prefix("sha256:").unwrap();
        std::fs::remove_file(
            directory
                .path()
                .join("cas/objects")
                .join(&digest[..2])
                .join(&digest[2..]),
        )
        .unwrap();
        let tx = store
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        assert!(matches!(
            insert_events(&tx, &cas, "ordinary", &[event], 0),
            Err(StoreError::DanglingArtifact { digest }) if digest == artifact
        ));
        tx.rollback().unwrap();
        assert!(store.is_empty("ordinary").unwrap());
    }

    #[test]
    fn publication_barrier_reverifies_cas_bytes_after_an_earlier_append() {
        let (directory, mut store, cas) = fixture();
        let digest = cas.put(b"original bytes").unwrap();
        let event = || {
            NewEvent::new(EventType::SourceCapturedV1, json!({})).referencing(vec![digest.clone()])
        };
        store.append("run", &cas, event()).unwrap();
        let hex = digest.strip_prefix("sha256:").unwrap();
        let path = directory
            .path()
            .join("cas/objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        std::fs::write(&path, b"tampered bytes").unwrap();
        assert!(
            store
                .append("run", &cas, event())
                .unwrap_err()
                .to_string()
                .contains("does not match its digest")
        );
        assert_eq!(store.len("run").unwrap(), 1);
        std::fs::write(&path, b"original bytes").unwrap();
        assert_eq!(store.append("run", &cas, event()).unwrap().sequence, 1);
    }
}
