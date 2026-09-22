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

use review_core::{EventType, RunEvent};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use crate::cas::{Cas, CasError};

mod attempt_wall;
pub mod task;

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
    /// Parsed Change Sets keyed by their content digest. A cache hit is accepted only after the
    /// current on-disk object is streamed and verified again; this removes repeated JSON/base64
    /// allocations without turning process history into integrity authority.
    validated_change_sets: std::collections::BTreeMap<String, Arc<review_core::ChangeSetV1>>,
}

#[derive(Default)]
struct PreparedArtifacts {
    verified: std::collections::BTreeSet<String>,
    json: std::collections::BTreeMap<String, Value>,
    /// Every Change Set parsed for this exact append batch. This is validation authority for the
    /// transaction; the EventStore cache is only a bounded cross-batch parse memo.
    change_sets: std::collections::BTreeMap<String, Arc<review_core::ChangeSetV1>>,
}

fn remember_validated_change_set(
    cache: &mut std::collections::BTreeMap<String, Arc<review_core::ChangeSetV1>>,
    artifact_id: String,
    change_set: Arc<review_core::ChangeSetV1>,
) {
    cache.clear();
    cache.insert(artifact_id, change_set);
}

/// Provider usage for one Attempt as the adapter reported it, per token kind. Absent kinds are
/// ones the Provider did not expose, never zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttemptUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    pub chargeable_tokens: u64,
}

/// Wall-clock and provider usage for one reviewer Attempt.
///
/// This is a **sidecar**, not an event: Review replay, the Finding Ledger and convergence
/// do not read it. Common Task recovery may raise an abandoned Attempt's canonical charge
/// from its durable usage floor before permitting further work. It also lets a person see
/// how long a review took and what it consumed, through
/// `af review report`, `af review campaigns`, and `af review ledger`. An absent row means "not
/// recorded", never "zero".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttemptWall<U = AttemptUsage> {
    pub run_id: String,
    pub attempt_id: String,
    pub node_id: String,
    pub round: u32,
    pub epoch: u32,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<U>,
}

/// Common Task usage may aggregate several native Provider counters in one Attempt.
pub type TaskAttemptWall = AttemptWall<review_core::task::usage::TaskTokenUsageV3>;

impl EventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            conn,
            task_cache: std::cell::RefCell::new(None),
            validated_change_sets: std::collections::BTreeMap::new(),
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
             CREATE TABLE IF NOT EXISTS attempt_wall (
                 run_id             TEXT    NOT NULL,
                 attempt_id         TEXT    NOT NULL,
                 node_id            TEXT    NOT NULL,
                 round              INTEGER NOT NULL,
                 epoch              INTEGER NOT NULL,
                 started_unix_ms    INTEGER NOT NULL,
                 elapsed_ms         INTEGER NOT NULL,
                 input_tokens       INTEGER,
                 output_tokens      INTEGER,
                 cache_read_tokens  INTEGER,
                 cache_write_tokens INTEGER,
                 reasoning_tokens   INTEGER,
                 chargeable_tokens  INTEGER,
                 PRIMARY KEY (run_id, attempt_id)
             );",
        )?;
        attempt_wall::migrate(&conn)?;
        Ok(Self {
            conn,
            task_cache: std::cell::RefCell::new(None),
            validated_change_sets: std::collections::BTreeMap::new(),
        })
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
        self.append_batch_inner(run_id, cas, events, None, None)
    }

    fn append_batch_inner(
        &mut self,
        run_id: &str,
        cas: &Cas,
        events: &[NewEvent],
        task_permit: Option<&task::WritePermit>,
        review_permit: Option<&task::execution::review::WritePermit>,
    ) -> Result<Vec<RunEvent>, StoreError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        if events.iter().any(|e| {
            matches!(
                e.event_type,
                EventType::TaskReviewResultSelectedV1 | EventType::RunReportV6
            )
        }) && review_permit.is_none()
        {
            return Err(StoreError::Conflict(
                "Task Review selection requires the trusted Task publication entry point".into(),
            ));
        }
        if events.iter().any(|e| {
            matches!(
                e.event_type,
                EventType::TaskTransitionV1
                    | EventType::TaskTransitionV2
                    | EventType::TaskTransitionV3
                    | EventType::TaskTransitionV4
                    | EventType::TaskTransitionV5
                    | EventType::TaskBrokerTransitionV1
            )
        }) && task_permit.is_none()
        {
            return Err(StoreError::Conflict(
                "Task events require the trusted Task entry point".into(),
            ));
        }
        let prepared = self.prepare_event_artifacts(cas, events)?;

        // Acquire the writer lock before reading aggregate state. A deferred transaction lets
        // two openers both observe an empty Campaign and only races at INSERT; IMMEDIATE makes
        // the compare-and-append decision itself serial.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let first: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence) + 1, 0) FROM events WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if let Some(permit) = review_permit {
            permit.validate(&tx, run_id, first, events)?;
        }
        if let Some(permit) = task_permit {
            permit.validate(&tx, run_id, first, events)?;
        } else {
            let task_log: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE run_id = ?1 AND type = 'TaskTransition@1')",
                [run_id], |row| row.get(0),
            )?;
            if task_log {
                return Err(StoreError::Conflict(
                    "Task log cannot accept Campaign or generic append authority".into(),
                ));
            }
            if events.iter().any(|event| {
                matches!(
                    event.event_type,
                    EventType::IntegrationPreparedV1
                        | EventType::IntegrationConflictV1
                        | EventType::IntegrationChecksCompletedV1
                        | EventType::IntegrationCommittedV1
                )
            }) {
                let task_round: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND type='RunReport@6' AND causation_id=(SELECT event_id FROM events WHERE run_id=?1 AND type='RoundStarted@1' ORDER BY sequence DESC LIMIT 1))", [run_id], |r|r.get(0))?;
                if task_round {
                    return Err(StoreError::Conflict(
                        "Task-backed Integration requires its protected phase publication".into(),
                    ));
                }
            }
            validate_campaign_transition(&tx, cas, run_id, events, first, &prepared)?;
        }
        let appended = insert_events(&tx, run_id, events, first)?;
        tx.commit()?;
        Ok(appended)
    }

    fn prepare_event_artifacts(
        &mut self,
        cas: &Cas,
        events: &[NewEvent],
    ) -> Result<PreparedArtifacts, StoreError> {
        for event in events {
            review_core::json::admit(&event.payload)
                .map_err(|error| StoreError::Conflict(format!("invalid event payload: {error}")))?;
            review_core::event::validate_event_payload(event.event_type, &event.payload)
                .map_err(|error| StoreError::Conflict(format!("invalid event payload: {error}")))?;
        }
        let typed_artifacts = typed_json_artifacts(events)?;
        let mut prepared = PreparedArtifacts::default();
        for event in events {
            for digest in &event.artifact_refs {
                if !prepared.verified.insert(digest.clone()) {
                    continue;
                }
                let prepare_error = |error| match error {
                    CasError::NotFound { .. } | CasError::InvalidDigest(_) => {
                        StoreError::DanglingArtifact {
                            digest: digest.clone(),
                        }
                    }
                    other => StoreError::Artifact(format!(
                        "referenced artifact {digest} failed verification: {other}"
                    )),
                };
                match typed_artifacts.get(digest).map(String::as_str) {
                    Some(review_core::contract::CHANGE_SET_V1)
                        if self.validated_change_sets.contains_key(digest) =>
                    {
                        cas.prepare_for_publication(digest).map_err(prepare_error)?;
                        prepared.change_sets.insert(
                            digest.clone(),
                            Arc::clone(&self.validated_change_sets[digest]),
                        );
                    }
                    Some(artifact_type) if artifact_type != review_core::contract::OPAQUE_V1 => {
                        let value = cas
                            .get_json_for_publication(digest)
                            .map_err(prepare_error)?;
                        if artifact_type == review_core::contract::CHANGE_SET_V1 {
                            let change_set: review_core::ChangeSetV1 =
                                serde_json::from_value(value)
                                    .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            change_set.validate().map_err(StoreError::Conflict)?;
                            let change_set = Arc::new(change_set);
                            prepared
                                .change_sets
                                .insert(digest.clone(), Arc::clone(&change_set));
                            // One Campaign Round has one Change Set authority. Keep only the
                            // newest parsed value so storage memory cannot grow with Round count;
                            // current-byte integrity is still re-established on every reference.
                            remember_validated_change_set(
                                &mut self.validated_change_sets,
                                digest.clone(),
                                change_set,
                            );
                        } else {
                            prepared.json.insert(digest.clone(), value);
                        }
                    }
                    _ => cas.prepare_for_publication(digest).map_err(prepare_error)?,
                }
            }
        }
        if !prepared.verified.is_empty() {
            cas.flush()
                .map_err(|e| StoreError::Durability(e.to_string()))?;
        }

        Ok(prepared)
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

    /// Every event of a run, in sequence order. This is the only read replay needs.
    pub fn replay(&self, run_id: &str) -> Result<Vec<RunEvent>, StoreError> {
        self.replay_from(run_id, 0)
    }

    /// The ordered suffix beginning at `first_sequence`, for advancing a watermarked projection
    /// without parsing and validating the prefix it already covers.
    pub fn replay_from(
        &self,
        run_id: &str,
        first_sequence: u64,
    ) -> Result<Vec<RunEvent>, StoreError> {
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
            let (mut event, raw_sequence, refs, payload) = row?;
            let sequence = u64::try_from(raw_sequence).map_err(|_| {
                StoreError::Conflict(format!("negative sequence {raw_sequence} in run {run_id}"))
            })?;
            if sequence != expected_sequence {
                return Err(StoreError::Conflict(format!(
                    "event sequence gap in run {run_id}: expected {expected_sequence}, found {sequence}"
                )));
            }
            let expected_id = derive_event_id(run_id, raw_sequence);
            if event.event_id != expected_id {
                return Err(StoreError::Conflict(format!(
                    "event {} has invalid derived id; expected {expected_id}",
                    event.event_id
                )));
            }
            event.sequence = sequence;
            event.artifact_refs = serde_json::from_str(&refs).map_err(StoreError::Json)?;
            event.payload = serde_json::from_str(&payload).map_err(StoreError::Json)?;
            review_core::event::validate_event_payload(event.event_type, &event.payload).map_err(
                |error| StoreError::Conflict(format!("invalid replayed event payload: {error}")),
            )?;
            out.push(event);
        }
        Ok(out)
    }

    /// The immutable Campaign opening event, read through the type/sequence index.
    pub fn campaign_opened(&self, run_id: &str) -> Result<Option<RunEvent>, StoreError> {
        self.indexed_event(run_id, EventType::CampaignOpenedV1, false)
    }

    /// The active Round epoch, read through the type/sequence index.
    pub fn latest_round_started(&self, run_id: &str) -> Result<Option<RunEvent>, StoreError> {
        self.indexed_event(run_id, EventType::RoundStartedV1, true)
    }

    fn indexed_event(
        &self,
        run_id: &str,
        event_type: EventType,
        latest: bool,
    ) -> Result<Option<RunEvent>, StoreError> {
        let sql = if latest {
            "SELECT event_id, sequence, type, occurred_at, node_id, attempt_id,
                    causation_id, correlation_id, artifact_refs, payload
             FROM events WHERE run_id = ?1 AND type = ?2 ORDER BY sequence DESC LIMIT 1"
        } else {
            "SELECT event_id, sequence, type, occurred_at, node_id, attempt_id,
                    causation_id, correlation_id, artifact_refs, payload
             FROM events WHERE run_id = ?1 AND type = ?2 ORDER BY sequence LIMIT 1"
        };
        let event_type = event_type.to_string();
        let row = self
            .conn
            .query_row(sql, params![run_id, event_type], |row| {
                Ok(StoredEventRow {
                    event_id: row.get(0)?,
                    sequence: row.get(1)?,
                    event_type: row.get(2)?,
                    occurred_at: row.get(3)?,
                    node_id: row.get(4)?,
                    attempt_id: row.get(5)?,
                    causation_id: row.get(6)?,
                    correlation_id: row.get(7)?,
                    artifact_refs: row.get(8)?,
                    payload: row.get(9)?,
                })
            })
            .optional()?;
        row.map(|row| decode_stored_event(run_id, row)).transpose()
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
        Ok(self.len(run_id)? == 0)
    }
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityDefinition {
    nodes: Vec<AuthorityNode>,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    subject: Option<toml::Value>,
    #[serde(default)]
    checks: Vec<toml::Value>,
    #[serde(default)]
    check_timeout_seconds: Option<u64>,
    #[serde(default)]
    gate: Option<AuthorityGate>,
    #[serde(default)]
    edges: Vec<toml::Value>,
    #[serde(default)]
    budgets: Option<toml::Value>,
    #[serde(default)]
    convergence: Option<toml::Value>,
    #[serde(default)]
    integration: Option<AuthorityIntegration>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityIntegration {
    #[serde(default)]
    protected_paths: Vec<String>,
    post_apply_checks: Vec<String>,
    #[serde(default)]
    reviewer_priority: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct AuthorityGate {
    #[serde(default)]
    caches: Vec<String>,
    #[serde(flatten)]
    _binding: std::collections::BTreeMap<String, toml::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityNode {
    id: String,
    kind: String,
    #[serde(default)]
    demands: Option<review_core::DemandRequirement>,
    #[serde(default)]
    inputs: Vec<AuthorityPort>,
    #[serde(default = "default_authority_outputs")]
    outputs: Vec<AuthorityPort>,
    #[serde(default)]
    gated_by: Option<String>,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    runner: Option<toml::Value>,
    #[serde(default)]
    execution: Option<AuthorityReviewerExecution>,
    #[serde(default)]
    slicing: Option<AuthoritySlicing>,
    #[serde(default)]
    closeout_for: Option<String>,
    /// A Worker's own Attempt cap (review-config `NodeBudgetSpec`); mirrored so pinned authority
    /// that declares one still validates here.
    #[serde(default)]
    budget: Option<AuthorityNodeBudget>,
    /// A reviewer's warm-layer policy (review-config `WarmSpec`); mirrored for the same reason.
    #[serde(default)]
    warm: Option<AuthorityWarm>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityNodeBudget {
    attempt: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityWarm {
    #[serde(default)]
    notes: Option<bool>,
    #[serde(default)]
    notes_max_bytes: Option<u64>,
    /// Package P2: build cache kinds the node's Gate carries; the config crate validates the
    /// closed vocabulary, the Store only keeps the pinned shape readable.
    #[serde(default)]
    build_cache: Vec<String>,
    /// Package P3: the Warm Workspace basis (`fresh` or `rebase`); the config crate validates
    /// the closed vocabulary, the Store only keeps the pinned shape readable.
    #[serde(default)]
    workspace: Option<String>,
    /// Package P4: the session layer (`off`, `if_recent` or `always`) and its age bound; the
    /// config crate validates the closed vocabulary and the provider restriction, the Store
    /// only keeps the pinned shape readable.
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    session_max_age_secs: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoritySlicing {
    scatter: String,
    max_paths_per_slice: usize,
    max_fanout: u32,
    coverage: review_core::SliceCoverageV1,
    all_shards_required: bool,
    closeout: AuthorityCloseoutMode,
    #[serde(default)]
    waiver_policy_id: Option<String>,
    #[serde(default)]
    waiver_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AuthorityCloseoutMode {
    Required,
    Waived,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityReviewerExecution {
    credential_mode: review_core::BrokerCredentialModeV1,
    #[serde(default)]
    auto_apply: bool,
    #[serde(default)]
    operations: Vec<review_core::BrokerOperationPolicyV1>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum AuthorityPort {
    Name(String),
    Detailed(AuthorityPortDetails),
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityPortDetails {
    name: String,
    #[serde(rename = "type")]
    artifact_type: String,
    cardinality: String,
    #[serde(default)]
    optional: bool,
    snapshot_affinity: String,
}

impl AuthorityPort {
    fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Detailed(port) => &port.name,
        }
    }

    fn artifact_type(&self) -> &str {
        match self {
            Self::Name(_) => review_core::contract::OPAQUE_V1,
            Self::Detailed(port) => &port.artifact_type,
        }
    }

    fn cardinality(&self) -> &str {
        match self {
            Self::Name(_) => "one",
            Self::Detailed(port) => &port.cardinality,
        }
    }

    fn optional(&self) -> bool {
        match self {
            Self::Name(_) => false,
            Self::Detailed(port) => port.optional,
        }
    }

    fn snapshot_affinity(&self) -> &str {
        match self {
            Self::Name(_) => "any",
            Self::Detailed(port) => &port.snapshot_affinity,
        }
    }
}

struct AuthorityPlan {
    version: u32,
    pipeline_policy_id: String,
    nodes: std::collections::BTreeMap<String, AuthorityNode>,
    gate_bound: bool,
    gate_nodes: std::collections::BTreeSet<String>,
    cache_kinds: std::collections::BTreeSet<String>,
    reviewer_execution: std::collections::BTreeMap<String, AuthorityReviewerExecution>,
    integration: Option<AuthorityIntegration>,
    check_names: Vec<String>,
}

impl AuthorityPlan {
    fn reviewer_execution_for(&self, node: &str) -> Option<&AuthorityReviewerExecution> {
        self.reviewer_execution.get(node).or_else(|| {
            let (owner, _) = node.split_once("#slice:")?;
            (self.nodes.get(owner)?.kind == "scatter")
                .then(|| self.reviewer_execution.get(owner))
                .flatten()
        })
    }
}

struct DynamicNodeAuthority {
    slice: review_core::ReviewSliceV1,
    inputs: Vec<AuthorityPort>,
    outputs: Vec<AuthorityPort>,
}

fn dynamic_node_authority(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    round_event_id: &str,
    plan: &AuthorityPlan,
    runtime_node: &str,
) -> Result<Option<DynamicNodeAuthority>, StoreError> {
    if plan.nodes.contains_key(runtime_node) {
        return Ok(None);
    }
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'SliceSetAccepted@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut resolved = None;
    for row in rows {
        let (slicer_id, payload) = row?;
        let payload: review_core::SliceSetAcceptedPayloadV1 = serde_json::from_str(&payload)?;
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&payload.slice_set_artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?,
        )?;
        crate::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
        if envelope.artifact_type != review_core::contract::SLICE_SET_V1
            || envelope.artifact_id != payload.slice_set_id
        {
            return Err(StoreError::Conflict(
                "durable SliceSet authority contradicts its accepted payload".into(),
            ));
        }
        let set: review_core::SliceSetV1 = serde_json::from_value(envelope.payload)?;
        set.validate().map_err(StoreError::Conflict)?;
        let Some(slice) = set
            .slices
            .iter()
            .find(|slice| slice.runtime_node_id == runtime_node)
        else {
            continue;
        };
        if resolved.is_some() {
            return Err(StoreError::Conflict(format!(
                "runtime node `{runtime_node}` is authorized by multiple Slice Sets"
            )));
        }
        let slicer = plan.nodes.get(&slicer_id).ok_or_else(|| {
            StoreError::Conflict("SliceSetAccepted@1 names a non-plan Slicer".into())
        })?;
        if slicer.kind != "slicer" {
            return Err(StoreError::Conflict(
                "SliceSetAccepted@1 producer is not a pinned Slicer".into(),
            ));
        }
        let owner = slicer
            .slicing
            .as_ref()
            .map(|policy| policy.scatter.clone())
            .ok_or_else(|| StoreError::Conflict("pinned Slicer has no Scatter owner".into()))?;
        let scatter = plan.nodes.get(&owner).ok_or_else(|| {
            StoreError::Conflict("pinned Slicer names an absent Scatter owner".into())
        })?;
        if scatter.kind != "scatter" || !runtime_node.starts_with(&format!("{owner}#slice:")) {
            return Err(StoreError::Conflict(
                "runtime node identity disagrees with its pinned Scatter".into(),
            ));
        }
        let mut inputs = scatter
            .inputs
            .iter()
            .filter(|port| port.artifact_type() != review_core::contract::SLICE_SET_V1)
            .cloned()
            .collect::<Vec<_>>();
        inputs.push(AuthorityPort::Detailed(AuthorityPortDetails {
            name: "slice".into(),
            artifact_type: review_core::contract::REVIEW_SLICE_V1.into(),
            cardinality: "one".into(),
            optional: false,
            snapshot_affinity: "same_subject".into(),
        }));
        let result_type = if inputs
            .iter()
            .any(|port| port.artifact_type() == review_core::contract::FINDING_SET_V1)
        {
            review_core::contract::REVIEWER_RESULT_V2
        } else {
            review_core::contract::REVIEWER_RESULT_V1
        };
        resolved = Some(DynamicNodeAuthority {
            slice: slice.clone(),
            inputs,
            outputs: vec![AuthorityPort::Detailed(AuthorityPortDetails {
                name: "out".into(),
                artifact_type: result_type.into(),
                cardinality: "one".into(),
                optional: false,
                snapshot_affinity: "same_subject".into(),
            })],
        });
    }
    Ok(resolved)
}

fn default_authority_outputs() -> Vec<AuthorityPort> {
    vec![AuthorityPort::Name("out".into())]
}

fn load_authority_plan(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
) -> Result<AuthorityPlan, StoreError> {
    let raw: String = tx.query_row(
        "SELECT payload FROM events
         WHERE run_id = ?1 AND type = 'CampaignOpened@1'
         ORDER BY sequence LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let opened: review_core::CampaignOpenedPayloadV1 = serde_json::from_str(&raw)?;
    load_authority_plan_id(
        cas,
        &opened.campaign_manifest_id,
        &opened.authority_snapshot_id,
    )
}

fn load_authority_plan_id(
    cas: &Cas,
    manifest_id: &str,
    authority_snapshot_id: &str,
) -> Result<AuthorityPlan, StoreError> {
    let manifest = cas
        .get_json(manifest_id)
        .map_err(|error| StoreError::Conflict(format!("unreadable CampaignManifest: {error}")))?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(manifest)?;
    manifest.validate().map_err(StoreError::Conflict)?;
    if manifest.authority_snapshot_id != authority_snapshot_id {
        return Err(StoreError::Conflict(
            "CampaignManifest authority does not match CampaignOpened@1".into(),
        ));
    }
    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let pipeline = std::str::from_utf8(&pipeline)
        .map_err(|error| StoreError::Conflict(format!("pinned pipeline is not UTF-8: {error}")))?;
    let definition: AuthorityDefinition = toml::from_str(pipeline)
        .map_err(|error| StoreError::Conflict(format!("pinned pipeline is invalid: {error}")))?;
    if !(2..=5).contains(&definition.version) {
        return Err(StoreError::Conflict(
            "pinned pipeline has no supported version".into(),
        ));
    }
    let version = definition.version;
    let integration = definition.integration.clone();
    let check_names = definition
        .checks
        .iter()
        .filter_map(|check| check.get("name").and_then(toml::Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let gate_bound = definition.gate.is_some();
    let cache_kinds: std::collections::BTreeSet<String> = definition
        .gate
        .as_ref()
        .into_iter()
        .flat_map(|gate| gate.caches.iter().cloned())
        .collect();
    if definition
        .gate
        .as_ref()
        .is_some_and(|gate| gate.caches.len() != cache_kinds.len())
        || cache_kinds.iter().any(|kind| kind != "cargo")
    {
        return Err(StoreError::Conflict(
            "pinned pipeline has duplicate or unsupported cache kinds".into(),
        ));
    }
    let mut nodes = std::collections::BTreeMap::new();
    for node in definition.nodes {
        if node.id.trim().is_empty() || nodes.insert(node.id.clone(), node).is_some() {
            return Err(StoreError::Conflict(
                "pinned pipeline has empty or duplicate node IDs".into(),
            ));
        }
    }
    if nodes.is_empty() {
        return Err(StoreError::Conflict("pinned pipeline has no nodes".into()));
    }
    let gate_nodes = nodes
        .values()
        .filter(|node| node.kind == "gate")
        .map(|node| node.id.clone())
        .collect();
    let mut reviewer_execution = std::collections::BTreeMap::new();
    for node in nodes.values() {
        match (definition.version, node.kind.as_str(), &node.execution) {
            (4, "reviewer", Some(execution)) | (5, "reviewer" | "scatter", Some(execution)) => {
                let mut names = std::collections::BTreeSet::new();
                for operation in &execution.operations {
                    operation.validate().map_err(StoreError::Conflict)?;
                    if !names.insert(operation.name.as_str()) {
                        return Err(StoreError::Conflict(
                            "pinned reviewer execution has duplicate Broker operations".into(),
                        ));
                    }
                }
                review_core::broker_authority_usage(&execution.operations)
                    .map_err(StoreError::Conflict)?;
                let valid_shape = match execution.credential_mode {
                    review_core::BrokerCredentialModeV1::Brokered => {
                        !execution.operations.is_empty()
                    }
                    review_core::BrokerCredentialModeV1::CredentialFree
                    | review_core::BrokerCredentialModeV1::TrustedUnsafe => {
                        execution.operations.is_empty()
                    }
                };
                if !valid_shape
                    || (execution.auto_apply
                        && execution.credential_mode
                            == review_core::BrokerCredentialModeV1::TrustedUnsafe)
                {
                    return Err(StoreError::Conflict(
                        "pinned reviewer Execution Binding contradicts its credential mode".into(),
                    ));
                }
                reviewer_execution.insert(node.id.clone(), execution.clone());
            }
            (4, "reviewer", None) | (5, "reviewer" | "scatter", None) => {
                return Err(StoreError::Conflict(
                    "pinned reviewer-capable node has no Execution Binding".into(),
                ));
            }
            (_, _, Some(_))
                if !matches!(
                    (definition.version, node.kind.as_str()),
                    (4, "reviewer") | (5, "reviewer" | "scatter")
                ) =>
            {
                return Err(StoreError::Conflict(
                    "pinned reviewer Execution Binding is not valid for this pipeline version or node kind"
                        .into(),
                ));
            }
            _ => {}
        }
    }
    Ok(AuthorityPlan {
        version,
        pipeline_policy_id: manifest.pipeline.artifact_id,
        nodes,
        gate_bound,
        gate_nodes,
        cache_kinds,
        reviewer_execution,
        integration,
        check_names,
    })
}

fn typed_json_artifacts(
    events: &[NewEvent],
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let mut artifacts = std::collections::BTreeMap::new();
    for event in events {
        let ports = match event.event_type {
            EventType::NodeInvocationV1 => {
                serde_json::from_value::<review_core::NodeInvocationPayloadV1>(
                    event.payload.clone(),
                )?
                .inputs
            }
            EventType::NodeOutputReceiptV1 => {
                serde_json::from_value::<review_core::NodeOutputReceiptPayloadV1>(
                    event.payload.clone(),
                )?
                .outputs
            }
            EventType::CacheSnapshotMaterializedV1 => {
                let snapshot: review_core::RunCacheSnapshotV5 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    snapshot.source_digest,
                    review_core::contract::CACHE_MANIFEST_V1.into(),
                )?;
                continue;
            }
            EventType::BuildCacheCapturedV1 => {
                let captured: review_core::BuildCacheCapturedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                if let Some(artifact_id) = captured.build_cache_artifact_id {
                    insert_artifact_type(
                        &mut artifacts,
                        artifact_id,
                        review_core::contract::BUILD_CACHE_V1.into(),
                    )?;
                }
                continue;
            }
            EventType::SessionSnapshotPreparedV1 => {
                let prepared: review_core::SessionSnapshotPreparedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    prepared.session_artifact_id,
                    review_core::contract::SESSION_SNAPSHOT_V1.into(),
                )?;
                continue;
            }
            EventType::RunReportV6 => {
                let report: review_core::RunReportPayloadV6 =
                    serde_json::from_value(event.payload.clone())?;
                if let review_core::RunReportExecutionV6::Cached {
                    cache_snapshots, ..
                } = report.execution
                {
                    for snapshot in cache_snapshots {
                        insert_artifact_type(
                            &mut artifacts,
                            snapshot.source_digest,
                            review_core::contract::CACHE_MANIFEST_V1.into(),
                        )?;
                    }
                }
                continue;
            }
            EventType::SliceSetAcceptedV1 => {
                let payload: review_core::SliceSetAcceptedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.slice_set_artifact_id,
                    review_core::contract::SLICE_SET_V1.into(),
                )?;
                continue;
            }
            EventType::ShardSetRecordedV1 | EventType::SemanticClosureCheckedV1 => {
                let payload: review_core::RecordedSetPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                let artifact_type = if event.event_type == EventType::ShardSetRecordedV1 {
                    review_core::contract::SHARD_SET_V1
                } else {
                    review_core::contract::SEMANTIC_CLOSURE_V1
                };
                insert_artifact_type(&mut artifacts, payload.record_id, artifact_type.into())?;
                continue;
            }
            EventType::IntegrationPreparedV1 => {
                let payload: review_core::IntegrationPreparedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.plan_artifact_id,
                    review_core::contract::INTEGRATION_PLAN_V1.into(),
                )?;
                continue;
            }
            EventType::IntegrationChecksCompletedV1 => {
                let payload: review_core::IntegrationChecksCompletedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.checks_artifact_id,
                    review_core::contract::INTEGRATION_CHECKS_V1.into(),
                )?;
                continue;
            }
            EventType::DemandRecordedV1
            | EventType::DemandWaivedV1
            | EventType::EvidenceAddedV1
            | EventType::EvidenceReuseAdmittedV1
            | EventType::EvidenceSatisfiedV1
            | EventType::ChangeAttestedV1
            | EventType::FixVerifiedV1
            | EventType::FindingResolutionRecordedV1
            | EventType::FindingResolutionChallengedV1
            | EventType::PolicyTimeAdvancedV1 => {
                let payload: review_core::RecordedArtifactPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                let artifact_type = match event.event_type {
                    EventType::DemandRecordedV1 => review_core::contract::DEMAND_V1,
                    EventType::DemandWaivedV1 => review_core::contract::DEMAND_WAIVER_V1,
                    EventType::EvidenceAddedV1 => review_core::contract::EVIDENCE_V1,
                    EventType::EvidenceReuseAdmittedV1 => {
                        review_core::contract::EVIDENCE_REUSE_ADMISSION_V1
                    }
                    EventType::EvidenceSatisfiedV1 => {
                        review_core::contract::EVIDENCE_SATISFACTION_V1
                    }
                    EventType::ChangeAttestedV1 => review_core::contract::CHANGE_ATTESTATION_V1,
                    EventType::FixVerifiedV1 => review_core::contract::FIX_VERIFICATION_V1,
                    EventType::FindingResolutionRecordedV1 => {
                        review_core::contract::FINDING_RESOLUTION_V1
                    }
                    EventType::FindingResolutionChallengedV1 => {
                        review_core::contract::RESOLUTION_CHALLENGE_V1
                    }
                    EventType::PolicyTimeAdvancedV1 => review_core::contract::POLICY_TIME_V1,
                    _ => unreachable!(),
                };
                insert_artifact_type(&mut artifacts, payload.artifact_id, artifact_type.into())?;
                continue;
            }
            EventType::FindingsGroupedV1 | EventType::FindingsUngroupedV1 => {
                let payload: review_core::FindingGroupingEventPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.grouping_artifact_id,
                    review_core::contract::FINDING_GROUPING_V1.into(),
                )?;
                continue;
            }
            _ => continue,
        };
        for port in ports {
            for artifact_id in port.artifact_ids {
                insert_artifact_type(&mut artifacts, artifact_id, port.artifact_type.clone())?;
            }
        }
    }
    Ok(artifacts)
}

fn insert_artifact_type(
    artifacts: &mut std::collections::BTreeMap<String, String>,
    artifact_id: String,
    artifact_type: String,
) -> Result<(), StoreError> {
    if let Some(previous) = artifacts.insert(artifact_id.clone(), artifact_type.clone())
        && previous != artifact_type
    {
        return Err(StoreError::Conflict(format!(
            "artifact {artifact_id} is assigned conflicting types"
        )));
    }
    Ok(())
}

fn validate_plan_ports(
    prepared: &PreparedArtifacts,
    expected: &[AuthorityPort],
    actual: &[review_core::PortArtifactsV1],
    subject_snapshot_id: &str,
    subject_base_snapshot_id: Option<&str>,
    subject_change_set_id: Option<&str>,
) -> Result<(), StoreError> {
    if expected.len() != actual.len() {
        return Err(StoreError::Conflict(
            "durable port map does not cover the pinned node contract".into(),
        ));
    }
    let actual: std::collections::BTreeMap<&str, &review_core::PortArtifactsV1> = actual
        .iter()
        .map(|port| (port.port.as_str(), port))
        .collect();
    for expected in expected {
        let port = actual.get(expected.name()).ok_or_else(|| {
            StoreError::Conflict(format!(
                "durable port map omits pinned port '{}'",
                expected.name()
            ))
        })?;
        let cardinality = match port.cardinality {
            review_core::PortCardinality::One => "one",
            review_core::PortCardinality::Many => "many",
        };
        let affinity = match port.snapshot_affinity {
            review_core::SnapshotAffinity::SameSubject => "same_subject",
            review_core::SnapshotAffinity::Unbound => "unbound",
            review_core::SnapshotAffinity::Any => "any",
        };
        if port.artifact_type != expected.artifact_type()
            || cardinality != expected.cardinality()
            || port.optional != expected.optional()
            || affinity != expected.snapshot_affinity()
        {
            return Err(StoreError::Conflict(format!(
                "durable port '{}' contradicts the pinned contract",
                expected.name()
            )));
        }
        if affinity == "same_subject"
            && port.subject_snapshot_id.as_deref() != Some(subject_snapshot_id)
        {
            return Err(StoreError::Conflict(format!(
                "durable port '{}' is bound to the wrong Subject snapshot",
                expected.name()
            )));
        }
        let mut validated_change_set = None;
        for artifact in &port.artifact_ids {
            if let Some(change_set) =
                validate_artifact_payload(prepared, &port.artifact_type, artifact)?
            {
                validated_change_set = Some(change_set);
            }
            if affinity == "same_subject"
                && let Some(value) = prepared.json.get(artifact)
                && value.get("type").is_some()
            {
                let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                    .map_err(|error| {
                        StoreError::Conflict(format!(
                            "typed artifact {artifact} is not an envelope: {error}"
                        ))
                    })?;
                if envelope.subject_snapshot_id.as_deref() != Some(subject_snapshot_id) {
                    return Err(StoreError::Conflict(format!(
                        "typed artifact {artifact} is bound to the wrong Subject snapshot"
                    )));
                }
            }
        }
        if port.artifact_type == review_core::contract::CHANGE_SET_V1 {
            let expected = subject_change_set_id.ok_or_else(|| {
                StoreError::Conflict("whole-tree Subject cannot carry a ChangeSet@1 port".into())
            })?;
            if port.artifact_ids.first().map(String::as_str) != Some(expected)
                || port.artifact_ids.len() != 1
            {
                return Err(StoreError::Conflict(
                    "ChangeSet@1 port does not carry the Subject's exact Change Set".into(),
                ));
            }
            let change_set = validated_change_set
                .ok_or_else(|| StoreError::Conflict("ChangeSet@1 port was not validated".into()))?;
            if change_set.head_snapshot_id != subject_snapshot_id
                || Some(change_set.base_snapshot_id.as_str()) != subject_base_snapshot_id
            {
                return Err(StoreError::Conflict(
                    "ChangeSet@1 Base or head contradicts the active Subject".into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_artifact_payload(
    prepared: &PreparedArtifacts,
    artifact_type: &str,
    artifact_id: &str,
) -> Result<Option<Arc<review_core::ChangeSetV1>>, StoreError> {
    if !prepared.verified.contains(artifact_id) {
        return Err(StoreError::Conflict(format!(
            "typed artifact {artifact_id} is absent from the event's verified references"
        )));
    }
    if artifact_type == review_core::contract::OPAQUE_V1 {
        return Ok(None);
    }
    if artifact_type == review_core::contract::CHANGE_SET_V1
        && let Some(change_set) = prepared.change_sets.get(artifact_id)
    {
        // Publication preparation re-established current CAS integrity for this exact reference
        // and made every Change Set in the batch independently available to validation.
        return Ok(Some(Arc::clone(change_set)));
    }
    let value = prepared.json.get(artifact_id).ok_or_else(|| {
        StoreError::Conflict(format!(
            "typed artifact {artifact_id} has no value from publication preparation"
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        StoreError::Conflict(format!("{artifact_type} artifact is not a JSON object"))
    })?;
    match artifact_type {
        review_core::contract::CHANGE_SET_V1 => {
            return Err(StoreError::Conflict(format!(
                "ChangeSet@1 artifact {artifact_id} was not prepared for validation"
            )));
        }
        review_core::contract::GATE_DECISION_V1 => {
            exact_keys(
                object,
                &["outcome", "blocking", "reasons", "executed", "required"],
                artifact_type,
            )?;
            if !matches!(value["outcome"].as_str(), Some("Passed" | "Blocked"))
                || !string_array(&value["blocking"])
                || !string_array(&value["reasons"])
                || value["executed"].as_u64().is_none()
                || value["required"].as_u64().is_none()
            {
                return Err(StoreError::Conflict(
                    "GateDecision@1 artifact violates its payload contract".into(),
                ));
            }
        }
        review_core::contract::PRIOR_FINDINGS_V1 => {
            exact_keys(
                object,
                &["subject_id", "round", "prior_findings"],
                artifact_type,
            )?;
            if value["subject_id"].as_str().is_none()
                || value["round"].as_u64().is_none()
                || value["prior_findings"].as_array().is_none()
            {
                return Err(StoreError::Conflict(
                    "PriorFindings@1 artifact violates its payload contract".into(),
                ));
            }
        }
        review_core::contract::REVIEWER_RESULT_V1 => {
            review_core::validate_reviewer_result(value).map_err(StoreError::Conflict)?
        }
        review_core::contract::REVIEWER_RESULT_V2 => {
            review_core::validate_reviewer_result_v2(value).map_err(StoreError::Conflict)?
        }
        review_core::contract::REVIEW_SLICE_V1 => {
            let payload: review_core::ReviewSliceV1 =
                validated_envelope_payload(value, review_core::contract::REVIEW_SLICE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::SLICE_SET_V1 => {
            let payload: review_core::SliceSetV1 =
                validated_envelope_payload(value, review_core::contract::SLICE_SET_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::SHARD_SET_V1 => {
            let payload: review_core::ShardSetV1 =
                validated_envelope_payload(value, review_core::contract::SHARD_SET_V1)?;
            payload.validate_shape().map_err(StoreError::Conflict)?;
        }
        review_core::contract::SEMANTIC_CLOSURE_V1 => {
            let payload: review_core::SemanticClosureV1 =
                validated_envelope_payload(value, review_core::contract::SEMANTIC_CLOSURE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::INTEGRATION_PLAN_V1 => {
            let payload: review_core::IntegrationPlanV1 = serde_json::from_value(value.clone())?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::INTEGRATION_CHECKS_V1 => {
            let payload: review_core::IntegrationChecksV1 = serde_json::from_value(value.clone())?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FINDING_DISPOSITION_V1 => {
            if value.get("type").is_none() {
                return Err(StoreError::Conflict(
                    "FindingDisposition@1 artifact is not an envelope".into(),
                ));
            }
            let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingDisposition@1 artifact is not an envelope: {error}"
                    ))
                })?;
            crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
            if envelope.artifact_type != review_core::contract::FINDING_DISPOSITION_V1 {
                return Err(StoreError::Conflict(
                    "FindingDisposition@1 envelope carries the wrong type".into(),
                ));
            }
            let payload: review_core::FindingDispositionV1 =
                serde_json::from_value(envelope.payload).map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingDisposition@1 envelope has an invalid payload: {error}"
                    ))
                })?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FINDING_GROUPING_V1 => {
            if value.get("type").is_none() {
                return Err(StoreError::Conflict(
                    "FindingGrouping@1 artifact is not an envelope".into(),
                ));
            }
            let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingGrouping@1 artifact is not an envelope: {error}"
                    ))
                })?;
            crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
            if envelope.artifact_type != review_core::contract::FINDING_GROUPING_V1 {
                return Err(StoreError::Conflict(
                    "FindingGrouping@1 envelope carries the wrong type".into(),
                ));
            }
            let payload: review_core::FindingGroupingV1 = serde_json::from_value(envelope.payload)
                .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingGrouping@1 envelope has an invalid payload: {error}"
                    ))
                })?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::DEMAND_V1 => {
            let payload: review_core::DemandV1 =
                validated_envelope_payload(value, review_core::contract::DEMAND_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::DEMAND_SET_V1 => {
            let payload: review_core::DemandSetV1 =
                validated_envelope_payload(value, review_core::contract::DEMAND_SET_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::EVIDENCE_V1 => {
            let payload: review_core::EvidenceV1 =
                validated_envelope_payload(value, review_core::contract::EVIDENCE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::EVIDENCE_SATISFACTION_V1 => {
            let payload: review_core::EvidenceSatisfactionV1 =
                validated_envelope_payload(value, review_core::contract::EVIDENCE_SATISFACTION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::EVIDENCE_REUSE_ADMISSION_V1 => {
            let payload: review_core::EvidenceReuseAdmissionV1 = validated_envelope_payload(
                value,
                review_core::contract::EVIDENCE_REUSE_ADMISSION_V1,
            )?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::DEMAND_WAIVER_V1 => {
            let payload: review_core::DemandWaiverV1 =
                validated_envelope_payload(value, review_core::contract::DEMAND_WAIVER_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::CHANGE_ATTESTATION_V1 => {
            let payload: review_core::ChangeAttestationV1 =
                validated_envelope_payload(value, review_core::contract::CHANGE_ATTESTATION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FIX_VERIFICATION_V1 => {
            let payload: review_core::FixVerificationV1 =
                validated_envelope_payload(value, review_core::contract::FIX_VERIFICATION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FINDING_RESOLUTION_V1 => {
            let payload: review_core::FindingResolutionV1 =
                validated_envelope_payload(value, review_core::contract::FINDING_RESOLUTION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::RESOLUTION_CHALLENGE_V1 => {
            let payload: review_core::ResolutionChallengeV1 =
                validated_envelope_payload(value, review_core::contract::RESOLUTION_CHALLENGE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::POLICY_TIME_V1 => {
            let payload: review_core::PolicyTimeV1 =
                validated_envelope_payload(value, review_core::contract::POLICY_TIME_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::REPORT_SET_V1 => {
            if object.is_empty()
                || object.values().any(|ids| {
                    ids.as_array().is_none_or(|ids| {
                        ids.is_empty()
                            || ids
                                .iter()
                                .any(|id| id.as_str().is_none_or(|id| !is_digest(id)))
                    })
                })
            {
                return Err(StoreError::Conflict(
                    "ReportSet@1 artifact violates its payload contract".into(),
                ));
            }
        }
        review_core::contract::FINDING_SET_V1 => {
            let payload: review_core::FindingSetV1 =
                validated_envelope_payload(value, review_core::contract::FINDING_SET_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        _ => {
            return Err(StoreError::Conflict(format!(
                "no payload validator is registered for {artifact_type}"
            )));
        }
    }
    Ok(None)
}

fn validated_envelope_payload<T: serde::de::DeserializeOwned>(
    value: &Value,
    expected_type: &str,
) -> Result<T, StoreError> {
    let envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(value.clone()).map_err(|error| {
            StoreError::Conflict(format!(
                "{expected_type} artifact is not an envelope: {error}"
            ))
        })?;
    crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
    if envelope.artifact_type != expected_type {
        return Err(StoreError::Conflict(format!(
            "{expected_type} envelope carries type {}",
            envelope.artifact_type
        )));
    }
    serde_json::from_value(envelope.payload).map_err(StoreError::from)
}

fn exact_keys(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
    artifact_type: &str,
) -> Result<(), StoreError> {
    if object.len() != expected.len() || object.keys().any(|key| !expected.contains(&key.as_str()))
    {
        return Err(StoreError::Conflict(format!(
            "{artifact_type} artifact has unexpected or missing fields"
        )));
    }
    Ok(())
}

fn string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(|item| item.as_str().is_some()))
}

fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// A RunReport@6 payload that satisfies its own contract.
fn task_report(payload: &Value) -> Result<review_core::RunReportPayloadV6, StoreError> {
    let report: review_core::RunReportPayloadV6 = serde_json::from_value(payload.clone())?;
    report.validate().map_err(StoreError::Conflict)?;
    Ok(report)
}

fn validate_report_plan(
    plan: &AuthorityPlan,
    report: &review_core::RunReportPayloadV6,
) -> Result<(), StoreError> {
    let incomplete = matches!(report.verdict, review_core::RunVerdictV3::Incomplete { .. });
    let required_gates: std::collections::BTreeSet<String> = plan
        .gate_nodes
        .iter()
        .filter(|node| {
            !incomplete
                || report.outcomes.iter().any(|entry| {
                    &entry.node == *node
                        && matches!(
                            entry.outcome,
                            review_core::RunNodeOutcomeV2::Completed { .. }
                        )
                })
        })
        .cloned()
        .collect();
    let expected: std::collections::BTreeSet<&str> =
        plan.nodes.keys().map(String::as_str).collect();
    let actual: std::collections::BTreeSet<&str> = report
        .outcomes
        .iter()
        .map(|outcome| outcome.node.as_str())
        .collect();
    if expected != actual || actual.len() != report.outcomes.len() {
        return Err(StoreError::Conflict(
            "RunReport@6 does not cover exactly the pinned Campaign plan".into(),
        ));
    }
    let reports_bindings = !matches!(
        report.execution,
        review_core::RunReportExecutionV6::Unbound {}
    );
    if reports_bindings != plan.gate_bound {
        return Err(StoreError::Conflict(
            "RunReport@6 does not match the pinned pipeline's Gate Execution Binding version"
                .into(),
        ));
    }
    let caches = match &report.execution {
        review_core::RunReportExecutionV6::Cached {
            cache_snapshots,
            cache_failures,
            ..
        } => Some((cache_snapshots, cache_failures)),
        _ => None,
    };
    if caches.is_some() == plan.cache_kinds.is_empty() {
        return Err(StoreError::Conflict(
            "RunReport@6 does not match the pinned pipeline's Cache Snapshot authority".into(),
        ));
    }
    if reports_bindings {
        let binding_nodes: std::collections::BTreeSet<String> = report
            .execution
            .bindings()
            .iter()
            .map(|binding| binding.node.clone())
            .collect();
        if !required_gates.is_subset(&binding_nodes) || !binding_nodes.is_subset(&plan.gate_nodes) {
            return Err(StoreError::Conflict(
                "RunReport@6 does not cover exactly the pinned Gate nodes".into(),
            ));
        }
    }
    if let Some((cache_snapshots, cache_failures)) = caches {
        let actual: std::collections::BTreeSet<(String, String)> = cache_snapshots
            .iter()
            .map(|snapshot| (snapshot.node.clone(), snapshot.kind))
            .chain(
                cache_failures
                    .iter()
                    .map(|failure| (failure.node.clone(), failure.kind)),
            )
            .map(|(node, kind)| {
                let kind = match kind {
                    review_core::RunCacheKindV5::Cargo => "cargo",
                };
                (node, kind.to_string())
            })
            .collect();
        let expected: std::collections::BTreeSet<(String, String)> = plan
            .gate_nodes
            .iter()
            .flat_map(|node| {
                plan.cache_kinds
                    .iter()
                    .map(move |kind| (node.clone(), kind.clone()))
            })
            .collect();
        let required: std::collections::BTreeSet<_> = expected
            .iter()
            .filter(|(node, _)| required_gates.contains(node))
            .cloned()
            .collect();
        if !required.is_subset(&actual) || !actual.is_subset(&expected) {
            return Err(StoreError::Conflict(
                "RunReport@6 does not cover exactly the pinned Gate cache requests".into(),
            ));
        }
    }
    Ok(())
}

fn validate_report_gate_bindings(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    bindings: &[review_core::RunExecutionBindingV4],
) -> Result<(), StoreError> {
    let reported: std::collections::BTreeMap<_, _> = bindings
        .iter()
        .map(|binding| (binding.node.clone(), binding.clone()))
        .collect();
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'GateExecutionBound@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut durable = std::collections::BTreeMap::new();
    for row in rows {
        let (node, payload) = row?;
        let binding: review_core::RunExecutionBindingV4 = serde_json::from_str(&payload)?;
        if binding.node != node {
            return Err(StoreError::Conflict(
                "durable Gate Execution Binding metadata disagrees with its payload".into(),
            ));
        }
        durable.insert(node, binding);
    }
    if durable != reported {
        return Err(StoreError::Conflict(
            "RunReport@6 bindings differ from the durable Gate execution facts".into(),
        ));
    }
    Ok(())
}

fn validate_report_cache_snapshots(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    (cache_snapshots, cache_failures): (
        &[review_core::RunCacheSnapshotV5],
        &[review_core::RunCacheFailureV5],
    ),
    artifact_refs: &[String],
    prepared: &PreparedArtifacts,
) -> Result<(), StoreError> {
    let expected_refs: std::collections::BTreeSet<_> = cache_snapshots
        .iter()
        .map(|snapshot| snapshot.source_digest.clone())
        .collect();
    let reported_refs: std::collections::BTreeSet<_> = artifact_refs.iter().cloned().collect();
    if artifact_refs.len() != reported_refs.len() || !expected_refs.is_subset(&reported_refs) {
        return Err(StoreError::Conflict(
            "RunReport@6 must reference each successful Cache Snapshot manifest exactly once"
                .into(),
        ));
    }
    for snapshot in cache_snapshots {
        let manifest = prepared.json.get(&snapshot.source_digest).ok_or_else(|| {
            StoreError::Conflict("RunReport@6 Cache Snapshot manifest was not prepared".into())
        })?;
        let manifest: review_core::CacheManifestV1 = serde_json::from_value(manifest.clone())?;
        manifest.validate().map_err(StoreError::Conflict)?;
        if manifest.kind != snapshot.kind
            || u64::try_from(manifest.entries.len()).ok() != Some(snapshot.files)
            || manifest.bytes() != snapshot.bytes
        {
            return Err(StoreError::Conflict(
                "RunReport@6 Cache Snapshot receipt contradicts CacheManifest@1".into(),
            ));
        }
    }
    let failed: std::collections::BTreeSet<_> = cache_failures
        .iter()
        .map(|failure| (failure.node.clone(), failure.kind))
        .collect();
    let reported: std::collections::BTreeMap<_, _> = cache_snapshots
        .iter()
        .map(|snapshot| ((snapshot.node.clone(), snapshot.kind), snapshot.clone()))
        .collect();
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'CacheSnapshotMaterialized@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut durable = std::collections::BTreeMap::new();
    for row in rows {
        let (node, payload) = row?;
        let snapshot: review_core::RunCacheSnapshotV5 = serde_json::from_str(&payload)?;
        if snapshot.node != node {
            return Err(StoreError::Conflict(
                "durable Cache Snapshot metadata disagrees with its payload".into(),
            ));
        }
        if !failed.contains(&(node.clone(), snapshot.kind)) {
            durable.insert((node, snapshot.kind), snapshot);
        }
    }
    if durable != reported {
        return Err(StoreError::Conflict(
            "RunReport@6 Cache Snapshots differ from the durable materialization facts".into(),
        ));
    }
    Ok(())
}

fn validate_campaign_transition(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    events: &[NewEvent],
    first_sequence: i64,
    prepared: &PreparedArtifacts,
) -> Result<(), StoreError> {
    let campaign_opened: i64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1'",
        params![run_id],
        |row| row.get(0),
    )?;
    let mut opened = campaign_opened > 0;
    let needs_authority_plan = events
        .iter()
        .any(|event| event_uses_authority_plan(event.event_type));
    let mut authority_plan = if opened && needs_authority_plan {
        Some(load_authority_plan(tx, cas, run_id)?)
    } else {
        None
    };
    let mut active = latest_round(tx, run_id)?;
    let mut active_subject: Option<(String, review_core::SubjectV1)> = None;
    let mut terminal = match &active {
        Some((event_id, _)) => round_has_terminal_report(tx, run_id, event_id)?,
        None => false,
    };
    let mut pending_supersession: Option<review_core::RoundInputSupersededPayloadV1> = None;
    let mut batch_proposal_attempts = std::collections::BTreeSet::new();
    let mut batch_prepared_proposals = std::collections::BTreeMap::new();
    let mut batch_accepted_proposals = std::collections::BTreeSet::new();
    let mut batch_invocations = std::collections::BTreeSet::new();
    let mut batch_receipts = std::collections::BTreeSet::new();
    let mut batch_findings = std::collections::BTreeSet::new();
    let mut batch_demands = std::collections::BTreeSet::new();
    let batch_integration_attestations: std::collections::BTreeSet<String> = events
        .iter()
        .filter(|event| event.event_type == EventType::ChangeAttestedV1)
        .filter_map(|event| {
            serde_json::from_value::<review_core::RecordedArtifactPayloadV1>(event.payload.clone())
                .ok()
                .map(|payload| payload.artifact_id)
        })
        .collect();
    let mut active_groupings = load_active_groupings(tx, run_id)?;

    for (offset, event) in events.iter().enumerate() {
        let sequence = first_sequence
            .checked_add(i64::try_from(offset).map_err(|_| {
                StoreError::Conflict("event batch is too large for transition validation".into())
            })?)
            .ok_or_else(|| StoreError::Conflict("event sequence overflow".into()))?;
        let event_id = derive_event_id(run_id, sequence);
        match event.event_type {
            EventType::CampaignOpenedV1 => {
                if opened {
                    return Err(StoreError::Conflict(
                        "CampaignOpened@1 already exists for this run".into(),
                    ));
                }
                opened = true;
                let payload: review_core::CampaignOpenedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                if !event.artifact_refs.contains(&payload.campaign_manifest_id)
                    || !event.artifact_refs.contains(&payload.authority_snapshot_id)
                {
                    return Err(StoreError::Conflict(
                        "CampaignOpened@1 does not publish its manifest and authority snapshot"
                            .into(),
                    ));
                }
                authority_plan = Some(load_authority_plan_id(
                    cas,
                    &payload.campaign_manifest_id,
                    &payload.authority_snapshot_id,
                )?);
            }
            EventType::RoundInputSupersededV1 => {
                let payload: review_core::RoundInputSupersededPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                let Some((active_id, active_payload)) = &active else {
                    return Err(StoreError::Conflict(
                        "cannot supersede a Campaign with no active Round".into(),
                    ));
                };
                if event.causation_id.as_deref() != Some(active_id)
                    || terminal
                    || payload.round != active_payload.round
                    || payload.old_epoch != active_payload.epoch
                    || payload.old_subject_id != active_payload.subject_id
                {
                    return Err(StoreError::Conflict(
                        "RoundInputSuperseded@1 does not match the active Round epoch".into(),
                    ));
                }
                let published: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events
                     WHERE run_id = ?1 AND sequence > (
                         SELECT sequence FROM events WHERE event_id = ?2
                     ) AND (type = 'FindingReported@1'
                         OR (type = 'FindingResolved@1' AND causation_id = ?2))",
                    params![run_id, active_id],
                    |row| row.get(0),
                )?;
                if published > 0 {
                    return Err(StoreError::Conflict(
                        "cannot supersede a Round after it published finding state".into(),
                    ));
                }
                pending_supersession = Some(payload);
            }
            EventType::RoundStartedV1 => {
                let payload: review_core::RoundStartedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                if !opened {
                    return Err(StoreError::Conflict(
                        "RoundStarted@1 requires a durable CampaignOpened@1".into(),
                    ));
                }
                if let Some(superseded) = pending_supersession.take() {
                    let Some((active_id, _)) = &active else {
                        return Err(StoreError::Conflict(
                            "replacement RoundStarted@1 has no active predecessor".into(),
                        ));
                    };
                    if payload.round != superseded.round
                        || payload.epoch != superseded.new_epoch
                        || payload.subject_id != superseded.replacement_subject_id
                        || payload.campaign_manifest_id != superseded.campaign_manifest_id
                        || event.causation_id.as_deref() != Some(active_id)
                    {
                        return Err(StoreError::Conflict(
                            "replacement RoundStarted@1 disagrees with its supersession".into(),
                        ));
                    }
                } else if let Some((_, prior)) = &active {
                    if !terminal
                        || prior.round.checked_add(1) != Some(payload.round)
                        || payload.epoch != 1
                    {
                        return Err(StoreError::Conflict(
                            "RoundStarted@1 is neither the next closed Round nor an atomic supersession"
                                .into(),
                        ));
                    }
                } else if payload.round != 1 || payload.epoch != 1 {
                    return Err(StoreError::Conflict(
                        "the first RoundStarted@1 must be round 1 epoch 1".into(),
                    ));
                }
                active = Some((event_id, payload));
                active_subject = None;
                terminal = false;
            }
            event_type if round_runtime_event(event_type) => {
                if active.is_none() {
                    return Err(StoreError::Conflict(format!(
                        "{event_type} requires an active Round"
                    )));
                }
                if let Some((active_id, active_payload)) = &active {
                    let plan = authority_plan.as_ref();
                    if active_subject
                        .as_ref()
                        .is_none_or(|(id, _)| id != &active_payload.subject_id)
                    {
                        let subject: review_core::SubjectV1 = serde_json::from_value(
                            cas.get_json(&active_payload.subject_id)
                                .map_err(|error| StoreError::Conflict(error.to_string()))?,
                        )?;
                        active_subject = Some((active_payload.subject_id.clone(), subject));
                    }
                    let subject = &active_subject.as_ref().expect("active Subject cached").1;
                    let subject_snapshot_id = &subject.head_snapshot_id;
                    let subject_base_snapshot_id = &subject.base_snapshot_id;
                    let subject_change_set_id = &subject.change_set_id;
                    if terminal {
                        return Err(StoreError::Conflict(format!(
                            "{event_type} cannot publish after the active Round concluded"
                        )));
                    }
                    if event.causation_id.as_deref() != Some(active_id) {
                        return Err(StoreError::Conflict(format!(
                            "{event_type} is not bound to the active Round epoch"
                        )));
                    }
                    if event.attempt_id.as_deref().is_some_and(|attempt| {
                        attempt.len() != 26
                            || !attempt
                                .bytes()
                                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                    }) {
                        return Err(StoreError::Conflict(format!(
                            "{event_type} carries a non-schema attempt ID"
                        )));
                    }
                    match event_type {
                        EventType::TaskReviewResultSelectedV1 => {
                            task::execution::review::validate_selection(
                                tx,
                                cas,
                                run_id,
                                active_id,
                                active_payload,
                                event,
                            )?;
                            let node = event.node_id.as_deref().expect("validated Review node");
                            let plan = plan.ok_or_else(|| {
                                StoreError::Conflict(
                                    "Task Review selection needs pinned Review authority".into(),
                                )
                            })?;
                            let outputs = if let Some(expected) = plan.nodes.get(node) {
                                if expected.kind != "reviewer" {
                                    return Err(StoreError::Conflict(
                                        "Task Review selection belongs to a non-reviewer node"
                                            .into(),
                                    ));
                                }
                                expected.outputs.clone()
                            } else {
                                dynamic_node_authority(tx, cas, run_id, active_id, plan, node)?
                                    .ok_or_else(|| {
                                        StoreError::Conflict(
                                            "Task Review selection has no declared reviewer".into(),
                                        )
                                    })?
                                    .outputs
                            };
                            let selected: review_core::task::review_compat::TaskReviewResultSelectedV1 =
                                serde_json::from_value(event.payload.clone())?;
                            let result: review_core::ArtifactEnvelope = serde_json::from_value(
                                cas.get_json(&selected.result_envelope_id)
                                    .map_err(|e| StoreError::Artifact(e.to_string()))?,
                            )?;
                            // Historical name-only Reviewer ports carry exactly the v1 flat
                            // result. The Task frontend makes that existing contract explicit;
                            // this does not authorize another result generation or shape.
                            let result_type =
                                outputs.first().map(|port| match port.artifact_type() {
                                    review_core::contract::OPAQUE_V1 => {
                                        review_core::contract::REVIEWER_RESULT_V1
                                    }
                                    ty => ty,
                                });
                            if outputs.len() != 1
                                || result_type != Some(result.artifact_type.as_str())
                                || outputs[0].cardinality() != "one"
                                || outputs[0].optional()
                            {
                                return Err(StoreError::Conflict(
                                    "Selected Task result differs from the pinned Review output contract".into(),
                                ));
                            }
                        }
                        EventType::GateExecutionBoundV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("GateExecutionBound@1 has no node ID".into())
                            })?;
                            let binding: review_core::RunExecutionBindingV4 =
                                serde_json::from_value(event.payload.clone())?;
                            if binding.node != node {
                                return Err(StoreError::Conflict(
                                    "GateExecutionBound@1 metadata disagrees with its payload"
                                        .into(),
                                ));
                            }
                            if plan.is_none_or(|plan| !plan.gate_nodes.contains(node)) {
                                return Err(StoreError::Conflict(format!(
                                    "Gate Execution Binding node '{node}' is absent from the pinned Gate plan"
                                )));
                            }
                        }
                        EventType::CacheSnapshotMaterializedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "CacheSnapshotMaterialized@1 has no node ID".into(),
                                )
                            })?;
                            let snapshot: review_core::RunCacheSnapshotV5 =
                                serde_json::from_value(event.payload.clone())?;
                            if snapshot.node != node {
                                return Err(StoreError::Conflict(
                                    "CacheSnapshotMaterialized@1 metadata disagrees with its payload"
                                        .into(),
                                ));
                            }
                            if plan.is_none_or(|plan| !plan.gate_nodes.contains(node)) {
                                return Err(StoreError::Conflict(format!(
                                    "Cache Snapshot node '{node}' is absent from the pinned Gate plan"
                                )));
                            }
                            let kind = match snapshot.kind {
                                review_core::RunCacheKindV5::Cargo => "cargo",
                            };
                            if plan.is_none_or(|plan| !plan.cache_kinds.contains(kind)) {
                                return Err(StoreError::Conflict(format!(
                                    "Cache Snapshot kind '{kind}' is absent from pinned authority"
                                )));
                            }
                            let has_manifest = event
                                .artifact_refs
                                .iter()
                                .any(|artifact| artifact == &snapshot.source_digest);
                            let receipt_bytes = crate::canonical::canonicalize(&event.payload)
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            let receipt_artifact =
                                crate::canonical::blob_content_id(&receipt_bytes);
                            let has_receipt = event.artifact_refs.contains(&receipt_artifact);
                            if !has_manifest || !has_receipt {
                                return Err(StoreError::Conflict(
                                    "CacheSnapshotMaterialized@1 lacks its manifest or exact receipt artifact"
                                        .into(),
                                ));
                            }
                            let manifest =
                                prepared.json.get(&snapshot.source_digest).ok_or_else(|| {
                                    StoreError::Conflict(
                                        "Cache Snapshot manifest was not prepared".into(),
                                    )
                                })?;
                            let manifest: review_core::CacheManifestV1 =
                                serde_json::from_value(manifest.clone())?;
                            manifest.validate().map_err(StoreError::Conflict)?;
                            if manifest.kind != snapshot.kind
                                || u64::try_from(manifest.entries.len()).ok()
                                    != Some(snapshot.files)
                                || manifest.bytes() != snapshot.bytes
                            {
                                return Err(StoreError::Conflict(
                                    "Cache Snapshot receipt contradicts CacheManifest@1".into(),
                                ));
                            }
                        }
                        EventType::NodeInvocationV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("NodeInvocation@1 has no node ID".into())
                            })?;
                            let invocation: review_core::NodeInvocationPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if invocation.node != node {
                                return Err(StoreError::Conflict(
                                    "NodeInvocation@1 metadata disagrees with its payload".into(),
                                ));
                            }
                            if let Some(plan) = plan {
                                if let Some(expected) = plan.nodes.get(node) {
                                    validate_plan_ports(
                                        prepared,
                                        &expected.inputs,
                                        &invocation.inputs,
                                        subject_snapshot_id,
                                        subject_base_snapshot_id.as_deref(),
                                        subject_change_set_id.as_deref(),
                                    )?;
                                } else {
                                    let dynamic = dynamic_node_authority(
                                        tx, cas, run_id, active_id, plan, node,
                                    )?
                                    .ok_or_else(|| {
                                        StoreError::Conflict(format!(
                                            "node '{node}' is absent from the pinned Campaign plan and accepted Slice Sets"
                                        ))
                                    })?;
                                    validate_plan_ports(
                                        prepared,
                                        &dynamic.inputs,
                                        &invocation.inputs,
                                        subject_snapshot_id,
                                        subject_base_snapshot_id.as_deref(),
                                        subject_change_set_id.as_deref(),
                                    )?;
                                    let slice_record = invocation
                                        .inputs
                                        .iter()
                                        .find(|port| port.port == "slice")
                                        .and_then(|port| port.artifact_ids.first())
                                        .ok_or_else(|| {
                                            StoreError::Conflict(
                                                "dynamic invocation has no exact Slice artifact"
                                                    .into(),
                                            )
                                        })?;
                                    let value =
                                        prepared.json.get(slice_record).ok_or_else(|| {
                                            StoreError::Conflict(
                                                "dynamic invocation Slice was not prepared".into(),
                                            )
                                        })?;
                                    let slice: review_core::ReviewSliceV1 =
                                        validated_envelope_payload(
                                            value,
                                            review_core::contract::REVIEW_SLICE_V1,
                                        )?;
                                    if slice != dynamic.slice {
                                        return Err(StoreError::Conflict(
                                            "dynamic invocation Slice contradicts accepted SliceSet authority"
                                                .into(),
                                        ));
                                    }
                                }
                            }
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'NodeInvocation@1' AND node_id = ?3",
                                params![run_id, active_id, node],
                                |row| row.get(0),
                            )?;
                            if existing > 0 || !batch_invocations.insert(node.to_string()) {
                                return Err(StoreError::Conflict(format!(
                                    "node '{node}' already has a durable invocation"
                                )));
                            }
                        }
                        EventType::NodeOutputReceiptV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("NodeOutputReceipt@1 has no node ID".into())
                            })?;
                            let receipt: review_core::NodeOutputReceiptPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if receipt.node != node {
                                return Err(StoreError::Conflict(
                                    "NodeOutputReceipt@1 metadata disagrees with its payload"
                                        .into(),
                                ));
                            }
                            if let Some(plan) = plan {
                                let reviewer = match plan.nodes.get(node) {
                                    Some(expected) => {
                                        validate_plan_ports(
                                            prepared,
                                            &expected.outputs,
                                            &receipt.outputs,
                                            subject_snapshot_id,
                                            subject_base_snapshot_id.as_deref(),
                                            subject_change_set_id.as_deref(),
                                        )?;
                                        expected.kind == "reviewer"
                                    }
                                    None => {
                                        let dynamic = dynamic_node_authority(
                                            tx, cas, run_id, active_id, plan, node,
                                        )?
                                        .ok_or_else(|| {
                                            StoreError::Conflict(format!(
                                                "node '{node}' is absent from the pinned Campaign plan and accepted Slice Sets"
                                            ))
                                        })?;
                                        validate_plan_ports(
                                            prepared,
                                            &dynamic.outputs,
                                            &receipt.outputs,
                                            subject_snapshot_id,
                                            subject_base_snapshot_id.as_deref(),
                                            subject_change_set_id.as_deref(),
                                        )?;
                                        true
                                    }
                                };
                                if reviewer && event.attempt_id.is_none() {
                                    return Err(StoreError::Conflict(
                                        "reviewer receipt has no selected attempt ID".into(),
                                    ));
                                }
                            }
                            let invocation: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'NodeInvocation@1' AND node_id = ?3",
                                params![run_id, active_id, node],
                                |row| row.get(0),
                            )?;
                            let receipts: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'NodeOutputReceipt@1' AND node_id = ?3",
                                params![run_id, active_id, node],
                                |row| row.get(0),
                            )?;
                            if invocation == 0 && !batch_invocations.contains(node) {
                                return Err(StoreError::Conflict(format!(
                                    "node '{node}' receipt has no durable invocation"
                                )));
                            }
                            if receipts > 0 || !batch_receipts.insert(node.to_string()) {
                                return Err(StoreError::Conflict(format!(
                                    "node '{node}' already has a durable output receipt"
                                )));
                            }
                            if let Some(attempt) = event.attempt_id.as_deref() {
                                task::execution::owned::check_canonical_child_receipt(
                                    tx, cas, run_id, active_id, node, attempt,
                                )?;
                                let result = selected_attempt_result(
                                    tx, run_id, active_id, event_type, node, attempt,
                                )?;
                                let outputs: Vec<&String> = receipt
                                    .outputs
                                    .iter()
                                    .flat_map(|port| &port.artifact_ids)
                                    .collect();
                                if outputs.len() != 1 || outputs[0] != &result {
                                    return Err(StoreError::Conflict(
                                        "reviewer receipt contradicts its selected Task result"
                                            .into(),
                                    ));
                                }
                            }
                        }
                        EventType::ProposalPreparedV1 | EventType::ProposalRefusedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no node ID"))
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no Attempt ID"))
                            })?;
                            task::execution::review::validate_proposal(
                                tx, cas, run_id, active_id, event,
                            )?;
                            let selected_result = selected_attempt_result(
                                tx, run_id, active_id, event_type, node, attempt,
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2 AND attempt_id = ?3
                                   AND type IN ('ProposalPrepared@1', 'ProposalRefused@1')",
                                params![run_id, active_id, attempt],
                                |row| row.get(0),
                            )?;
                            if existing != 0 || !batch_proposal_attempts.insert(attempt.to_string())
                            {
                                return Err(StoreError::Conflict(
                                    "selected Attempt has duplicate Proposal disposition".into(),
                                ));
                            }
                            if event_type == EventType::ProposalPreparedV1 {
                                let payload: review_core::ProposalPreparedPayloadV1 =
                                    serde_json::from_value(event.payload.clone())?;
                                let candidate = proposal_candidate(
                                    cas,
                                    prepared,
                                    &payload.candidate_artifact_id,
                                )?;
                                if payload.result_artifact_id != selected_result
                                    || candidate.result_artifact_id != selected_result
                                    || candidate.base_snapshot_id != subject_snapshot_id.as_str()
                                {
                                    return Err(StoreError::Conflict(
                                        "ProposalPrepared@1 contradicts its selected Attempt or Subject"
                                            .into(),
                                    ));
                                }
                                validate_candidate_manifest(cas, subject, &candidate)?;
                                let mut expected = vec![
                                    payload.candidate_artifact_id.clone(),
                                    selected_result.clone(),
                                    candidate.patch_artifact_id.clone(),
                                    candidate.derived_manifest_artifact_id.clone(),
                                ];
                                expected.extend(candidate.evidence_ids.iter().cloned());
                                require_exact_round_artifact_refs(
                                    tx,
                                    run_id,
                                    active_payload,
                                    subject,
                                    event,
                                    expected,
                                    "ProposalPrepared@1",
                                )?;
                                batch_prepared_proposals.insert(
                                    payload.candidate_artifact_id,
                                    (node.to_string(), attempt.to_string(), selected_result),
                                );
                            } else {
                                let payload: review_core::ProposalRefusedPayloadV1 =
                                    serde_json::from_value(event.payload.clone())?;
                                if payload.result_artifact_id != selected_result {
                                    return Err(StoreError::Conflict(
                                        "ProposalRefused@1 contradicts its selected Attempt".into(),
                                    ));
                                }
                                require_exact_round_artifact_refs(
                                    tx,
                                    run_id,
                                    active_payload,
                                    subject,
                                    event,
                                    vec![selected_result],
                                    "ProposalRefused@1",
                                )?;
                            }
                        }
                        EventType::ProposalAcceptedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("ProposalAccepted@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("ProposalAccepted@1 has no Attempt ID".into())
                            })?;
                            let payload: review_core::ProposalAcceptedPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            payload
                                .validate()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            let selected_result = selected_attempt_result(
                                tx, run_id, active_id, event_type, node, attempt,
                            )?;
                            let prepared_authority = prepared_proposal_authority(
                                tx,
                                run_id,
                                active_id,
                                &batch_prepared_proposals,
                                &payload.candidate_artifact_id,
                            )?;
                            if prepared_authority
                                != (
                                    node.to_string(),
                                    attempt.to_string(),
                                    selected_result.clone(),
                                )
                            {
                                return Err(StoreError::Conflict(
                                    "ProposalAccepted@1 contradicts its durable preparation".into(),
                                ));
                            }
                            let candidate =
                                proposal_candidate(cas, prepared, &payload.candidate_artifact_id)?;
                            validate_candidate_manifest(cas, subject, &candidate)?;
                            let envelope_value = prepared
                                .json
                                .get(&payload.proposal_artifact_id)
                                .cloned()
                                .map(Ok)
                                .unwrap_or_else(|| {
                                    cas.get_json(&payload.proposal_artifact_id)
                                        .map_err(|error| StoreError::Conflict(error.to_string()))
                                })?;
                            let envelope: review_core::ArtifactEnvelope =
                                serde_json::from_value(envelope_value)?;
                            crate::canonical::validate_envelope(&envelope)
                                .map_err(StoreError::Conflict)?;
                            if envelope.artifact_type != review_core::contract::PATCH_PROPOSAL_V1
                                || envelope.artifact_id != payload.proposal_id
                                || envelope.subject_snapshot_id.as_deref()
                                    != Some(subject_snapshot_id.as_str())
                            {
                                return Err(StoreError::Conflict(
                                    "ProposalAccepted@1 contradicts its typed Proposal envelope"
                                        .into(),
                                ));
                            }
                            match &envelope.producer {
                                review_core::Producer::Attempt {
                                    run_id: producer_run,
                                    node_id: producer_node,
                                    attempt_id: producer_attempt,
                                } if producer_run == run_id
                                    && producer_node == node
                                    && producer_attempt == attempt => {}
                                _ => {
                                    return Err(StoreError::Conflict(
                                        "accepted Proposal producer is not its selected Attempt"
                                            .into(),
                                    ));
                                }
                            }
                            let proposal: review_core::PatchProposal =
                                serde_json::from_value(envelope.payload.clone())?;
                            proposal
                                .check_shape()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            validate_accepted_proposal_claims(
                                tx, run_id, active_id, node, &candidate, &proposal,
                            )?;
                            if proposal.base_snapshot_id != candidate.base_snapshot_id
                                || proposal.patch_artifact_id != candidate.patch_artifact_id
                                || proposal.evidence_ids != candidate.evidence_ids
                                || proposal.paths != candidate.paths
                                || proposal.description != candidate.description
                                || proposal.auto_apply_nominated != candidate.auto_apply_nominated
                            {
                                return Err(StoreError::Conflict(
                                    "accepted Proposal contradicts its sealed candidate".into(),
                                ));
                            }
                            let mut expected_inputs = vec![
                                payload.candidate_artifact_id.clone(),
                                candidate.result_artifact_id.clone(),
                                candidate.patch_artifact_id.clone(),
                                candidate.derived_manifest_artifact_id.clone(),
                            ];
                            expected_inputs.extend(candidate.evidence_ids.iter().cloned());
                            if normalized_ids(envelope.input_artifacts.clone())
                                != normalized_ids(expected_inputs)
                            {
                                return Err(StoreError::Conflict(
                                    "accepted Proposal envelope omits exact candidate inputs"
                                        .into(),
                                ));
                            }
                            require_exact_round_artifact_refs(
                                tx,
                                run_id,
                                active_payload,
                                subject,
                                event,
                                vec![
                                    payload.proposal_artifact_id.clone(),
                                    payload.candidate_artifact_id.clone(),
                                ],
                                "ProposalAccepted@1",
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events WHERE run_id = ?1
                                 AND causation_id = ?2 AND type = 'ProposalAccepted@1'
                                 AND (json_extract(payload, '$.proposal_id') = ?3
                                   OR json_extract(payload, '$.candidate_artifact_id') = ?4)",
                                params![
                                    run_id,
                                    active_id,
                                    payload.proposal_id,
                                    payload.candidate_artifact_id
                                ],
                                |row| row.get(0),
                            )?;
                            if existing != 0
                                || !batch_accepted_proposals.insert(payload.proposal_id)
                            {
                                return Err(StoreError::Conflict(
                                    "Proposal was accepted more than once".into(),
                                ));
                            }
                        }
                        EventType::FindingReportedV1 => {
                            let key = event.payload["key"].as_str().ok_or_else(|| {
                                StoreError::Conflict("FindingReported@1 has no finding key".into())
                            })?;
                            if event.correlation_id.as_deref() != Some(key) {
                                return Err(StoreError::Conflict(
                                    "FindingReported@1 correlation disagrees with its key".into(),
                                ));
                            }
                            match event.payload.get("report_id").and_then(Value::as_str) {
                                Some(report_id)
                                    if event.artifact_refs.as_slice() == [report_id] => {}
                                Some(_) => {
                                    return Err(StoreError::Conflict(
                                        "FindingReported@1 report ID disagrees with its artifact reference"
                                            .into(),
                                    ));
                                }
                                None => {
                                    return Err(StoreError::Conflict(
                                        "FindingReported@1 has no authoritative report artifact"
                                            .into(),
                                    ));
                                }
                            }
                            batch_findings.insert(key.to_string());
                        }
                        EventType::DemandRecordedV1 => {
                            let demand: review_core::DemandV1 = validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::DEMAND_V1,
                            )?;
                            if event.correlation_id.as_deref() != Some(demand.demand_id.as_str())
                                || demand.round != active_payload.round
                                || demand.subject_id != active_payload.subject_id
                            {
                                return Err(StoreError::Conflict(
                                    "DemandRecorded@1 is not bound to its Demand, Round, and Subject"
                                        .into(),
                                ));
                            }
                            batch_demands.insert(demand.demand_id);
                        }
                        EventType::SliceSetAcceptedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("SliceSetAccepted@1 has no Slicer node".into())
                            })?;
                            let payload: review_core::SliceSetAcceptedPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            payload
                                .validate()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            if payload.slice_set_id != payload.slice_set_artifact_id
                                || !event.artifact_refs.contains(&payload.slice_set_artifact_id)
                                || plan
                                    .and_then(|plan| plan.nodes.get(node))
                                    .is_none_or(|node| node.kind != "slicer")
                            {
                                return Err(StoreError::Conflict(
                                    "SliceSetAccepted@1 contradicts pinned Slicer authority".into(),
                                ));
                            }
                            let value = prepared
                                .json
                                .get(&payload.slice_set_artifact_id)
                                .ok_or_else(|| {
                                    StoreError::Conflict(
                                        "SliceSetAccepted@1 artifact was not prepared".into(),
                                    )
                                })?;
                            let set: review_core::SliceSetV1 = validated_envelope_payload(
                                value,
                                review_core::contract::SLICE_SET_V1,
                            )?;
                            set.validate().map_err(StoreError::Conflict)?;
                            let plan = plan.ok_or_else(|| {
                                StoreError::Conflict(
                                    "SliceSetAccepted@1 has no captured pipeline authority".into(),
                                )
                            })?;
                            let slicer = plan.nodes.get(node).ok_or_else(|| {
                                StoreError::Conflict(
                                    "SliceSetAccepted@1 Slicer is absent from the captured plan"
                                        .into(),
                                )
                            })?;
                            let policy = slicer.slicing.as_ref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "SliceSetAccepted@1 Slicer has no captured slicing policy"
                                        .into(),
                                )
                            })?;
                            let expected = expected_slice_set(
                                cas,
                                plan,
                                policy,
                                &active_payload.subject_id,
                                subject,
                            )?;
                            if set.subject_id != active_payload.subject_id || set != expected {
                                return Err(StoreError::Conflict(
                                    "SliceSetAccepted@1 contradicts the exact captured slicing policy"
                                        .into(),
                                ));
                            }
                        }
                        EventType::ShardSetRecordedV1 | EventType::SemanticClosureCheckedV1 => {
                            let payload: review_core::RecordedSetPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            payload
                                .validate()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            if payload.artifact_id != payload.record_id
                                || !event.artifact_refs.contains(&payload.record_id)
                            {
                                return Err(StoreError::Conflict(format!(
                                    "{} contradicts its recorded artifact",
                                    event.event_type
                                )));
                            }
                            let value = prepared.json.get(&payload.record_id).ok_or_else(|| {
                                StoreError::Conflict(format!(
                                    "{} artifact was not prepared",
                                    event.event_type
                                ))
                            })?;
                            if event.event_type == EventType::ShardSetRecordedV1 {
                                let set: review_core::ShardSetV1 = validated_envelope_payload(
                                    value,
                                    review_core::contract::SHARD_SET_V1,
                                )?;
                                set.validate_shape().map_err(StoreError::Conflict)?;
                                if set.subject_id != active_payload.subject_id {
                                    return Err(StoreError::Conflict(
                                        "ShardSetRecorded@1 belongs to another Subject".into(),
                                    ));
                                }
                            } else {
                                let closure: review_core::SemanticClosureV1 =
                                    validated_envelope_payload(
                                        value,
                                        review_core::contract::SEMANTIC_CLOSURE_V1,
                                    )?;
                                closure.validate().map_err(StoreError::Conflict)?;
                                if closure.subject_id != active_payload.subject_id {
                                    return Err(StoreError::Conflict(
                                        "SemanticClosureChecked@1 belongs to another Subject"
                                            .into(),
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                    if event_type.is_run_report() {
                        // Even an incomplete report that keeps the Round open carries execution
                        // authority: its plan, binding, cache and receipt claims are validated.
                        let report = task_report(&event.payload)?;
                        if let Some(plan) = plan {
                            validate_report_plan(plan, &report)?;
                        }
                        let bindings = report.execution.bindings();
                        match &report.execution {
                            review_core::RunReportExecutionV6::Unbound {} => {}
                            review_core::RunReportExecutionV6::Bound { .. } => {
                                validate_report_gate_bindings(tx, run_id, active_id, bindings)?;
                            }
                            review_core::RunReportExecutionV6::Cached {
                                cache_snapshots,
                                cache_failures,
                                ..
                            } => {
                                validate_report_gate_bindings(tx, run_id, active_id, bindings)?;
                                validate_report_cache_snapshots(
                                    tx,
                                    run_id,
                                    active_id,
                                    (cache_snapshots, cache_failures),
                                    &event.artifact_refs,
                                    prepared,
                                )?;
                            }
                        }
                        validate_report_receipts(tx, cas, run_id, active_id, &report.outcomes)?;
                        let closes =
                            !matches!(report.verdict, review_core::RunVerdictV3::Incomplete { .. });
                        if closes {
                            if terminal {
                                return Err(StoreError::Conflict(
                                    "the active Round epoch already has a terminal conclusion"
                                        .into(),
                                ));
                            }
                            terminal = true;
                        }
                    }
                }
            }
            EventType::FindingResolvedV1 => {
                let key = event.payload["key"].as_str().ok_or_else(|| {
                    StoreError::Conflict("FindingResolved@1 has no finding key".into())
                })?;
                if event.correlation_id.as_deref() != Some(key) {
                    return Err(StoreError::Conflict(
                        "FindingResolved@1 correlation disagrees with its key".into(),
                    ));
                }
                let existing: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events
                     WHERE run_id = ?1 AND type = 'FindingReported@1' AND correlation_id = ?2",
                    params![run_id, key],
                    |row| row.get(0),
                )?;
                if existing == 0 && !batch_findings.contains(key) {
                    return Err(StoreError::Conflict(format!(
                        "FindingResolved@1 names unknown finding key '{key}'"
                    )));
                }
                if let (Some(causation), Some((active_id, _))) =
                    (event.causation_id.as_deref(), &active)
                    && causation != active_id
                {
                    return Err(StoreError::Conflict(
                        "FindingResolved@1 is bound to a stale Round epoch".into(),
                    ));
                }
            }
            EventType::FindingsGroupedV1 | EventType::FindingsUngroupedV1 => {
                let Some((_, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                if !terminal || event.causation_id.is_some() {
                    return Err(StoreError::Conflict(format!(
                        "{} is an operator transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let payload: review_core::FindingGroupingEventPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                payload.validate().map_err(StoreError::Conflict)?;
                if event.correlation_id.as_deref() != Some(payload.from.as_str())
                    || event.artifact_refs.as_slice() != [payload.grouping_artifact_id.as_str()]
                {
                    return Err(StoreError::Conflict(format!(
                        "{} metadata disagrees with its grouping payload",
                        event.event_type
                    )));
                }
                let action = if event.event_type == EventType::FindingsGroupedV1 {
                    review_core::FindingGroupingAction::Group
                } else {
                    review_core::FindingGroupingAction::Ungroup
                };
                validate_grouping_event_artifact(prepared, &payload, action, active_payload.round)?;
                for key in [&payload.from, &payload.into] {
                    let existing: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM events
                         WHERE run_id = ?1 AND type = 'FindingReported@1' AND correlation_id = ?2",
                        params![run_id, key],
                        |row| row.get(0),
                    )?;
                    if existing == 0 && !batch_findings.contains(key) {
                        return Err(StoreError::Conflict(format!(
                            "{} names unknown Finding `{key}`",
                            event.event_type
                        )));
                    }
                }
                match action {
                    review_core::FindingGroupingAction::Group => {
                        if payload.from == payload.into
                            || active_groupings.contains_key(&payload.from)
                            || grouping_root(&active_groupings, &payload.into)
                                != Some(payload.into.as_str())
                        {
                            return Err(StoreError::Conflict(
                                "FindingsGrouped@1 would create an ambiguous or cyclic grouping"
                                    .into(),
                            ));
                        }
                        active_groupings.insert(payload.from, payload.into);
                    }
                    review_core::FindingGroupingAction::Ungroup => {
                        if active_groupings.get(&payload.from) != Some(&payload.into) {
                            return Err(StoreError::Conflict(
                                "FindingsUngrouped@1 has no matching active grouping".into(),
                            ));
                        }
                        active_groupings.remove(&payload.from);
                    }
                }
            }
            EventType::IntegrationPreparedV1
            | EventType::IntegrationConflictV1
            | EventType::IntegrationChecksCompletedV1
            | EventType::IntegrationCommittedV1 => {
                let Some((_, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                if !terminal || event.causation_id.is_some() {
                    return Err(StoreError::Conflict(format!(
                        "{} is an internal transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let integration_authority = authority_plan
                    .as_ref()
                    .filter(|plan| plan.version == 5 && plan.integration.is_some())
                    .ok_or_else(|| {
                        StoreError::Conflict(format!(
                            "{} has no captured automatic-Integration authority",
                            event.event_type
                        ))
                    })?;
                match event.event_type {
                    EventType::IntegrationPreparedV1 => {
                        let payload: review_core::IntegrationPreparedPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        let plan = validate_artifact_payload(
                            prepared,
                            review_core::contract::INTEGRATION_PLAN_V1,
                            &payload.plan_artifact_id,
                        )?;
                        if plan.is_some() {
                            unreachable!("IntegrationPlan validation never returns a Change Set")
                        }
                        let plan: review_core::IntegrationPlanV1 = serde_json::from_value(
                            prepared
                                .json
                                .get(&payload.plan_artifact_id)
                                .expect("validated IntegrationPlan")
                                .clone(),
                        )?;
                        if plan.subject_id != active_payload.subject_id
                            || event.correlation_id.as_deref()
                                != Some(active_payload.subject_id.as_str())
                        {
                            return Err(StoreError::Conflict(
                                "IntegrationPrepared@1 contradicts the active Subject or its references"
                                    .into(),
                            ));
                        }
                        let source: review_core::SourceSnapshot = serde_json::from_value(
                            cas.get_json(&payload.derived_snapshot_id)
                                .map_err(|error| {
                                    StoreError::Conflict(format!(
                                        "prepared derived Snapshot was not readable: {error}"
                                    ))
                                })?,
                        )?;
                        if !source.is_derived()
                            || source.parent_snapshot_id.as_deref()
                                != Some(plan.base_snapshot_id.as_str())
                            || source.artifact_manifest.as_deref()
                                != Some(plan.derived_manifest_artifact_id.as_str())
                        {
                            return Err(StoreError::Conflict(
                                "IntegrationPrepared@1 derived Snapshot contradicts its plan"
                                    .into(),
                            ));
                        }
                        validate_integration_plan_authority(
                            tx,
                            cas,
                            run_id,
                            active_payload,
                            integration_authority,
                            &plan,
                            IntegrationTarget {
                                batch_id: &payload.batch_id,
                                derived_snapshot_id: &payload.derived_snapshot_id,
                            },
                        )?;
                        require_exact_artifact_refs(
                            event,
                            vec![
                                payload.plan_artifact_id.clone(),
                                payload.derived_snapshot_id.clone(),
                                plan.derived_manifest_artifact_id.clone(),
                            ],
                            "IntegrationPrepared@1",
                        )?;
                        let duplicate: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND (json_extract(payload, '$.batch_id') = ?2
                               OR json_extract(payload, '$.plan_artifact_id') = ?3)",
                            params![run_id, payload.batch_id, payload.plan_artifact_id],
                            |row| row.get(0),
                        )?;
                        if duplicate != 0 {
                            return Err(StoreError::Conflict(
                                "Integration preparation is duplicated".into(),
                            ));
                        }
                    }
                    EventType::IntegrationConflictV1 => {
                        let payload: review_core::IntegrationConflictPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        payload.validate().map_err(StoreError::Conflict)?;
                        let subject: review_core::SubjectV1 = serde_json::from_value(
                            cas.get_json(&active_payload.subject_id)
                                .map_err(|error| StoreError::Conflict(error.to_string()))?,
                        )?;
                        if payload.base_snapshot_id != subject.head_snapshot_id {
                            return Err(StoreError::Conflict(
                                "IntegrationConflict@1 is not bound to the active head".into(),
                            ));
                        }
                    }
                    EventType::IntegrationChecksCompletedV1 => {
                        let payload: review_core::IntegrationChecksCompletedPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        validate_artifact_payload(
                            prepared,
                            review_core::contract::INTEGRATION_CHECKS_V1,
                            &payload.checks_artifact_id,
                        )?;
                        let checks: review_core::IntegrationChecksV1 = serde_json::from_value(
                            prepared
                                .json
                                .get(&payload.checks_artifact_id)
                                .expect("validated IntegrationChecks")
                                .clone(),
                        )?;
                        if checks.passed() != payload.passed {
                            return Err(StoreError::Conflict(
                                "IntegrationChecksCompleted@1 contradicts its exact results".into(),
                            ));
                        }
                        let prepared_raw: String = tx.query_row(
                            "SELECT payload FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        let integration_prepared: review_core::IntegrationPreparedPayloadV1 =
                            serde_json::from_str(&prepared_raw)?;
                        let policy = integration_authority
                            .integration
                            .as_ref()
                            .expect("checked Integration authority");
                        let check_names = checks
                            .checks
                            .iter()
                            .map(|check| check.name.as_str())
                            .collect::<Vec<_>>();
                        let expected_names = policy
                            .post_apply_checks
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>();
                        if checks.derived_snapshot_id != integration_prepared.derived_snapshot_id
                            || check_names != expected_names
                            || expected_names.iter().any(|name| {
                                !integration_authority
                                    .check_names
                                    .iter()
                                    .any(|check| check == name)
                            })
                        {
                            return Err(StoreError::Conflict(
                                "Integration checks contradict the prepared Snapshot or captured check policy"
                                    .into(),
                            ));
                        }
                        let mut expected_refs = vec![
                            payload.checks_artifact_id.clone(),
                            integration_prepared.derived_snapshot_id,
                        ];
                        expected_refs.extend(
                            checks
                                .checks
                                .iter()
                                .map(|check| check.result_artifact_id.clone()),
                        );
                        require_exact_artifact_refs(
                            event,
                            expected_refs,
                            "IntegrationChecksCompleted@1",
                        )?;
                        let prepared_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        if prepared_count != 1 {
                            return Err(StoreError::Conflict(
                                "Integration checks have no unique durable preparation".into(),
                            ));
                        }
                    }
                    EventType::IntegrationCommittedV1 => {
                        let payload: review_core::IntegrationCommittedPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        if payload.prior_subject_id != active_payload.subject_id {
                            return Err(StoreError::Conflict(
                                "IntegrationCommitted@1 expected Subject is stale".into(),
                            ));
                        }
                        let prepared_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        let passed_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationChecksCompleted@1'
                             AND json_extract(payload, '$.batch_id') = ?2
                             AND json_extract(payload, '$.passed') = 1",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        let committed_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationCommitted@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        if prepared_count != 1 || passed_count != 1 || committed_count != 0 {
                            return Err(StoreError::Conflict(
                                "Integration commit lacks unique preparation and passing checks, or is duplicated"
                                    .into(),
                            ));
                        }
                        validate_integration_commit_authority(
                            tx,
                            cas,
                            run_id,
                            active_payload,
                            &payload,
                            &batch_integration_attestations,
                            integration_authority,
                        )?;
                        let subject: review_core::SubjectV1 = serde_json::from_value(
                            cas.get_json(&payload.derived_subject_id).map_err(|error| {
                                StoreError::Conflict(format!(
                                    "derived Subject was not readable at commit: {error}"
                                ))
                            })?,
                        )?;
                        subject.validate().map_err(StoreError::Conflict)?;
                        if subject.head_snapshot_id != payload.derived_snapshot_id
                            || !event.artifact_refs.contains(&payload.derived_subject_id)
                            || !event.artifact_refs.contains(&payload.derived_snapshot_id)
                            || !event.artifact_refs.contains(&payload.semantic_closure_id)
                            || payload
                                .attestation_ids
                                .iter()
                                .any(|id| !event.artifact_refs.contains(id))
                        {
                            return Err(StoreError::Conflict(
                                "IntegrationCommitted@1 does not expose its complete atomic authority"
                                    .into(),
                            ));
                        }
                    }
                    _ => unreachable!(),
                }
            }
            EventType::EvidenceAddedV1
            | EventType::EvidenceReuseAdmittedV1
            | EventType::EvidenceSatisfiedV1
            | EventType::DemandWaivedV1 => {
                let Some((_, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                if !terminal || event.causation_id.is_some() {
                    return Err(StoreError::Conflict(format!(
                        "{} is an operator transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let (demand_id, subject_id) = match event.event_type {
                    EventType::EvidenceAddedV1 => {
                        let value: review_core::EvidenceV1 = validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::EVIDENCE_V1,
                        )?;
                        (value.demand_id, value.subject_id)
                    }
                    EventType::EvidenceSatisfiedV1 => {
                        let value: review_core::EvidenceSatisfactionV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::EVIDENCE_SATISFACTION_V1,
                            )?;
                        (value.demand_id, value.subject_id)
                    }
                    EventType::EvidenceReuseAdmittedV1 => {
                        let value: review_core::EvidenceReuseAdmissionV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::EVIDENCE_REUSE_ADMISSION_V1,
                            )?;
                        (value.demand_id, value.subject_id)
                    }
                    EventType::DemandWaivedV1 => {
                        let value: review_core::DemandWaiverV1 = validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::DEMAND_WAIVER_V1,
                        )?;
                        (value.demand_id, value.subject_id)
                    }
                    _ => unreachable!(),
                };
                if event.correlation_id.as_deref() != Some(demand_id.as_str())
                    || subject_id != active_payload.subject_id
                {
                    return Err(StoreError::Conflict(format!(
                        "{} is not bound to its Demand and active Subject",
                        event.event_type
                    )));
                }
                let existing: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events
                     WHERE run_id = ?1 AND type = 'DemandRecorded@1' AND correlation_id = ?2",
                    params![run_id, demand_id],
                    |row| row.get(0),
                )?;
                if existing == 0 && !batch_demands.contains(&demand_id) {
                    return Err(StoreError::Conflict(format!(
                        "{} names unknown Demand `{demand_id}`",
                        event.event_type
                    )));
                }
            }
            EventType::ChangeAttestedV1
            | EventType::FixVerifiedV1
            | EventType::FindingResolutionRecordedV1
            | EventType::FindingResolutionChallengedV1
            | EventType::PolicyTimeAdvancedV1 => {
                let Some((active_id, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                let automatic_challenge = if event.event_type
                    == EventType::FindingResolutionChallengedV1
                    && event.causation_id.as_deref() == Some(active_id.as_str())
                {
                    let challenge: review_core::ResolutionChallengeV1 =
                        validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::RESOLUTION_CHALLENGE_V1,
                        )?;
                    let recorded: review_core::RecordedArtifactPayloadV1 =
                        serde_json::from_value(event.payload.clone())?;
                    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        prepared
                            .json
                            .get(&recorded.artifact_id)
                            .expect("validated recorded artifact")
                            .clone(),
                    )?;
                    challenge.actor == "review.kernel/resolution-policy@1"
                        && !challenge.evidence_ids.is_empty()
                        && matches!(
                            envelope.producer,
                            review_core::Producer::KernelOperation {
                                run_id: producer_run,
                                node_id: None,
                                operation_id,
                            } if producer_run == run_id
                                && operation_id.starts_with("automatic-resolution-challenge:")
                        )
                } else {
                    false
                };
                if !automatic_challenge && (!terminal || event.causation_id.is_some()) {
                    return Err(StoreError::Conflict(format!(
                        "{} is an operator transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let (correlation, subject_id) = match event.event_type {
                    EventType::ChangeAttestedV1 => {
                        let value: review_core::ChangeAttestationV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::CHANGE_ATTESTATION_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::FixVerifiedV1 => {
                        let value: review_core::FixVerificationV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::FIX_VERIFICATION_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::FindingResolutionRecordedV1 => {
                        let value: review_core::FindingResolutionV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::FINDING_RESOLUTION_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::FindingResolutionChallengedV1 => {
                        let value: review_core::ResolutionChallengeV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::RESOLUTION_CHALLENGE_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::PolicyTimeAdvancedV1 => {
                        let _: review_core::PolicyTimeV1 = validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::POLICY_TIME_V1,
                        )?;
                        ("policy-time".into(), None)
                    }
                    _ => unreachable!(),
                };
                if event.correlation_id.as_deref() != Some(correlation.as_str())
                    || subject_id
                        .as_deref()
                        .is_some_and(|subject| subject != active_payload.subject_id)
                {
                    return Err(StoreError::Conflict(format!(
                        "{} is not bound to its Finding and active Subject",
                        event.event_type
                    )));
                }
                if event.event_type != EventType::PolicyTimeAdvancedV1 {
                    let existing: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM events
                         WHERE run_id = ?1 AND type = 'FindingReported@1' AND correlation_id = ?2",
                        params![run_id, correlation],
                        |row| row.get(0),
                    )?;
                    if existing == 0 && !batch_findings.contains(&correlation) {
                        return Err(StoreError::Conflict(format!(
                            "{} names unknown Finding `{correlation}`",
                            event.event_type
                        )));
                    }
                }
            }
            _ => {}
        }
    }
    if pending_supersession.is_some() {
        return Err(StoreError::Conflict(
            "RoundInputSuperseded@1 and its replacement RoundStarted@1 must append atomically"
                .into(),
        ));
    }
    Ok(())
}

fn load_active_groupings(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let mut groupings = std::collections::BTreeMap::new();
    let mut statement = tx.prepare(
        "SELECT type, payload FROM events
         WHERE run_id = ?1 AND type IN ('FindingsGrouped@1', 'FindingsUngrouped@1')
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (event_type, raw) = row?;
        let payload: review_core::FindingGroupingEventPayloadV1 = serde_json::from_str(&raw)?;
        if event_type == EventType::FindingsGroupedV1.as_str() {
            groupings.insert(payload.from, payload.into);
        } else {
            groupings.remove(&payload.from);
        }
    }
    Ok(groupings)
}

fn grouping_root<'a>(
    groupings: &'a std::collections::BTreeMap<String, String>,
    key: &'a str,
) -> Option<&'a str> {
    let mut root = key;
    for _ in 0..=groupings.len() {
        let Some(next) = groupings.get(root) else {
            return Some(root);
        };
        root = next;
    }
    None
}

fn validate_grouping_event_artifact(
    prepared: &PreparedArtifacts,
    payload: &review_core::FindingGroupingEventPayloadV1,
    action: review_core::FindingGroupingAction,
    round: u32,
) -> Result<(), StoreError> {
    let value = prepared
        .json
        .get(&payload.grouping_artifact_id)
        .ok_or_else(|| StoreError::Conflict("grouping artifact was not prepared".into()))?;
    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())?;
    crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
    if envelope.artifact_type != review_core::contract::FINDING_GROUPING_V1 {
        return Err(StoreError::Conflict(
            "grouping event references the wrong artifact type".into(),
        ));
    }
    let grouping: review_core::FindingGroupingV1 = serde_json::from_value(envelope.payload)?;
    grouping.validate().map_err(StoreError::Conflict)?;
    if grouping.from != payload.from
        || grouping.into != payload.into
        || grouping.action != action
        || grouping.round != round
    {
        return Err(StoreError::Conflict(
            "grouping event contradicts its immutable artifact".into(),
        ));
    }
    Ok(())
}

fn validate_recorded_event_artifact<T: serde::de::DeserializeOwned>(
    prepared: &PreparedArtifacts,
    event: &NewEvent,
    expected_type: &str,
) -> Result<T, StoreError> {
    let payload: review_core::RecordedArtifactPayloadV1 =
        serde_json::from_value(event.payload.clone())?;
    payload.validate().map_err(StoreError::Conflict)?;
    if event.artifact_refs.as_slice() != [payload.artifact_id.as_str()] {
        return Err(StoreError::Conflict(format!(
            "{} recorded artifact disagrees with its sole reference",
            event.event_type
        )));
    }
    let value = prepared.json.get(&payload.artifact_id).ok_or_else(|| {
        StoreError::Conflict(format!("{} artifact was not prepared", event.event_type))
    })?;
    validated_envelope_payload(value, expected_type)
}

fn exact_subject_paths(
    cas: &Cas,
    subject: &review_core::SubjectV1,
) -> Result<Vec<String>, StoreError> {
    match subject.kind {
        review_core::SubjectKind::Diff => {
            let change_set_id = subject.change_set_id.as_deref().ok_or_else(|| {
                StoreError::Conflict("diff Subject has no ChangeSet authority".into())
            })?;
            let change_set: review_core::ChangeSetV1 = serde_json::from_value(
                cas.get_json(change_set_id)
                    .map_err(|error| StoreError::Conflict(error.to_string()))?,
            )?;
            change_set.validate().map_err(StoreError::Conflict)?;
            if change_set.head_snapshot_id != subject.head_snapshot_id
                || Some(change_set.base_snapshot_id.as_str()) != subject.base_snapshot_id.as_deref()
            {
                return Err(StoreError::Conflict(
                    "Subject path authority contradicts its ChangeSet".into(),
                ));
            }
            Ok(change_set.changed_paths)
        }
        review_core::SubjectKind::WholeTree => {
            let snapshot: review_core::SourceSnapshot = serde_json::from_value(
                cas.get_json(&subject.head_snapshot_id)
                    .map_err(|error| StoreError::Conflict(error.to_string()))?,
            )?;
            let manifest_id = snapshot.artifact_manifest.as_deref().ok_or_else(|| {
                StoreError::Conflict("whole-tree Subject Snapshot has no Manifest".into())
            })?;
            let manifest = cas
                .get_json(manifest_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?;
            Ok(manifest_entries(&manifest)?.into_keys().collect())
        }
    }
}

fn expected_slice_set(
    cas: &Cas,
    plan: &AuthorityPlan,
    policy: &AuthoritySlicing,
    subject_id: &str,
    subject: &review_core::SubjectV1,
) -> Result<review_core::SliceSetV1, StoreError> {
    if policy.max_paths_per_slice == 0 || policy.max_fanout == 0 {
        return Err(StoreError::Conflict(
            "captured slicing policy has a zero bound".into(),
        ));
    }
    let paths = exact_subject_paths(cas, subject)?;
    if paths.is_empty()
        || paths.len().div_ceil(policy.max_paths_per_slice) > policy.max_fanout as usize
    {
        return Err(StoreError::Conflict(
            "captured slicing policy cannot cover the exact Subject paths".into(),
        ));
    }
    let closeout = match policy.closeout {
        AuthorityCloseoutMode::Required => {
            if policy.waiver_policy_id.is_some() || policy.waiver_reason.is_some() {
                return Err(StoreError::Conflict(
                    "captured required closeout carries waiver authority".into(),
                ));
            }
            review_core::CloseoutPolicyV1::Required
        }
        AuthorityCloseoutMode::Waived => review_core::CloseoutPolicyV1::Waived {
            policy_id: policy.waiver_policy_id.clone().ok_or_else(|| {
                StoreError::Conflict("captured closeout waiver has no policy ID".into())
            })?,
            reason: policy
                .waiver_reason
                .clone()
                .filter(|reason| !reason.trim().is_empty())
                .ok_or_else(|| {
                    StoreError::Conflict("captured closeout waiver has no reason".into())
                })?,
        },
    };
    let slices = paths
        .chunks(policy.max_paths_per_slice)
        .enumerate()
        .map(|(ordinal, paths)| {
            let slice_id = crate::canonical::content_id(&serde_json::json!({
                "domain": "review.kernel/slice-id@1",
                "subject_id": subject_id,
                "paths": paths,
            }))
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
            let runtime_node_id = format!(
                "{}#slice:{}:{}",
                policy.scatter,
                ordinal + 1,
                &slice_id[7..23]
            );
            if plan.nodes.contains_key(&runtime_node_id) {
                return Err(StoreError::Conflict(
                    "captured Slice runtime identity collides with a static node".into(),
                ));
            }
            Ok(review_core::ReviewSliceV1 {
                slice_id,
                runtime_node_id,
                paths: paths.to_vec(),
                overlaps: vec![],
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let set = review_core::SliceSetV1 {
        subject_id: subject_id.to_string(),
        coverage: policy.coverage,
        max_fanout: policy.max_fanout,
        all_shards_required: policy.all_shards_required,
        closeout,
        slices,
    };
    set.validate_coverage(&paths)
        .map_err(StoreError::Conflict)?;
    Ok(set)
}

fn normalized_ids(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

fn require_exact_artifact_refs(
    event: &NewEvent,
    expected: Vec<String>,
    label: &str,
) -> Result<(), StoreError> {
    if normalized_ids(event.artifact_refs.clone()) != normalized_ids(expected) {
        return Err(StoreError::Conflict(format!(
            "{label} does not reference its exact authority"
        )));
    }
    Ok(())
}

fn require_exact_round_artifact_refs(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    active: &review_core::RoundStartedPayloadV1,
    subject: &review_core::SubjectV1,
    event: &NewEvent,
    mut expected: Vec<String>,
    label: &str,
) -> Result<(), StoreError> {
    let opened_raw: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1'
         ORDER BY sequence LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let opened: review_core::CampaignOpenedPayloadV1 = serde_json::from_str(&opened_raw)?;
    expected.extend([
        opened.authority_snapshot_id,
        active.campaign_manifest_id.clone(),
        active.subject_id.clone(),
        subject.head_snapshot_id.clone(),
    ]);
    expected.sort();
    expected.dedup();
    require_exact_artifact_refs(event, expected, label)
}

fn selected_attempt_result(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    event_type: EventType,
    node: &str,
    attempt: &str,
) -> Result<String, StoreError> {
    let rows = tx
        .prepare(
            "SELECT payload FROM events
             WHERE run_id = ?1 AND causation_id = ?2 AND node_id = ?3 AND attempt_id = ?4
               AND type = 'TaskReviewResultSelected@1' ORDER BY sequence",
        )?
        .query_map(params![run_id, round_event_id, node, attempt], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let [raw] = rows.as_slice() else {
        return Err(StoreError::Conflict(format!(
            "{event_type} has no unique Task Review selection"
        )));
    };
    let selected: review_core::task::review_compat::TaskReviewResultSelectedV1 =
        serde_json::from_str(raw)?;
    selected.validate().map_err(StoreError::Conflict)?;
    Ok(selected.result_artifact_id)
}

fn proposal_candidate(
    cas: &Cas,
    prepared: &PreparedArtifacts,
    artifact_id: &str,
) -> Result<review_core::ProposalCandidateV1, StoreError> {
    let value = prepared
        .json
        .get(artifact_id)
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| {
            cas.get_json(artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))
        })?;
    let candidate: review_core::ProposalCandidateV1 = serde_json::from_value(value)?;
    candidate
        .validate()
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    Ok(candidate)
}

fn prepared_proposal_authority(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    batch: &std::collections::BTreeMap<String, (String, String, String)>,
    candidate_id: &str,
) -> Result<(String, String, String), StoreError> {
    if let Some(authority) = batch.get(candidate_id) {
        return Ok(authority.clone());
    }
    let rows = tx
        .prepare(
            "SELECT node_id, attempt_id, payload FROM events
             WHERE run_id = ?1 AND causation_id = ?2 AND type = 'ProposalPrepared@1'
               AND json_extract(payload, '$.candidate_artifact_id') = ?3
             ORDER BY sequence",
        )?
        .query_map(params![run_id, round_event_id, candidate_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let [(node, attempt, raw)] = rows.as_slice() else {
        return Err(StoreError::Conflict(
            "accepted Proposal has no unique durable preparation".into(),
        ));
    };
    let payload: review_core::ProposalPreparedPayloadV1 = serde_json::from_str(raw)?;
    Ok((node.clone(), attempt.clone(), payload.result_artifact_id))
}

fn manifest_entries(
    value: &Value,
) -> Result<std::collections::BTreeMap<String, Value>, StoreError> {
    let object = value
        .as_object()
        .ok_or_else(|| StoreError::Conflict("Snapshot Manifest is not an object".into()))?;
    if object.keys().any(|key| key != "entries") {
        return Err(StoreError::Conflict(
            "Snapshot Manifest has an unsupported shape".into(),
        ));
    }
    let entries = object
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| StoreError::Conflict("Snapshot Manifest has no entries".into()))?;
    let mut mapped = std::collections::BTreeMap::new();
    let mut prior: Option<&str> = None;
    for entry in entries {
        let entry_object = entry.as_object().ok_or_else(|| {
            StoreError::Conflict("Snapshot Manifest entry is not an object".into())
        })?;
        if entry_object.len() != 4
            || entry_object
                .keys()
                .any(|key| !matches!(key.as_str(), "path" | "kind" | "content" | "size"))
        {
            return Err(StoreError::Conflict(
                "Snapshot Manifest entry has an unsupported shape".into(),
            ));
        }
        let path = entry_object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| StoreError::Conflict("Snapshot Manifest entry has no path".into()))?;
        if prior.is_some_and(|prior| prior.as_bytes() >= path.as_bytes())
            || !matches!(
                entry_object.get("kind").and_then(Value::as_str),
                Some("file" | "executable" | "symlink")
            )
            || !entry_object
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(is_digest)
            || entry_object.get("size").and_then(Value::as_u64).is_none()
        {
            return Err(StoreError::Conflict(
                "Snapshot Manifest entry is invalid or not canonically ordered".into(),
            ));
        }
        prior = Some(path);
        mapped.insert(path.to_string(), entry.clone());
    }
    Ok(mapped)
}

fn manifest_value(entries: std::collections::BTreeMap<String, Value>) -> Value {
    serde_json::json!({ "entries": Value::Array(entries.into_values().collect()) })
}

fn validate_candidate_manifest(
    cas: &Cas,
    subject: &review_core::SubjectV1,
    candidate: &review_core::ProposalCandidateV1,
) -> Result<(), StoreError> {
    if candidate.base_snapshot_id != subject.head_snapshot_id {
        return Err(StoreError::Conflict(
            "Proposal candidate is stale for the active Subject".into(),
        ));
    }
    let base_snapshot: review_core::SourceSnapshot = serde_json::from_value(
        cas.get_json(&candidate.base_snapshot_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    let base_manifest_id = base_snapshot
        .artifact_manifest
        .as_deref()
        .ok_or_else(|| StoreError::Conflict("Proposal Base Snapshot has no Manifest".into()))?;
    let base = cas
        .get_json(base_manifest_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let derived = cas
        .get_json(&candidate.derived_manifest_artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let base_entries = manifest_entries(&base)?;
    let derived_entries = manifest_entries(&derived)?;
    let changed = base_entries
        .keys()
        .chain(derived_entries.keys())
        .filter(|path| base_entries.get(*path) != derived_entries.get(*path))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if changed
        != candidate
            .paths
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(StoreError::Conflict(
            "Proposal candidate Manifest changes paths outside its declaration".into(),
        ));
    }
    cas.verify(&candidate.patch_artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    Ok(())
}

fn validate_accepted_proposal_claims(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    node: &str,
    candidate: &review_core::ProposalCandidateV1,
    proposal: &review_core::PatchProposal,
) -> Result<(), StoreError> {
    let report_rows = tx
        .prepare(
            "SELECT payload FROM events WHERE run_id = ?1 AND causation_id = ?2
             AND type = 'FindingReported@1' AND node_id = ?3 ORDER BY sequence",
        )?
        .query_map(params![run_id, round_event_id, node], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let report_ids = report_rows
        .iter()
        .filter_map(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter_map(|payload| {
            payload
                .get("report_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut findings = Vec::new();
    let mut reports = Vec::new();
    let mut unique = std::collections::BTreeSet::new();
    for claim in &proposal.finding_refs {
        let kind = match claim.kind {
            review_core::ClaimRefKind::Finding => "finding",
            review_core::ClaimRefKind::Report => "report",
        };
        if !is_digest(&claim.id) || !unique.insert((kind, claim.id.as_str())) {
            return Err(StoreError::Conflict(
                "accepted Proposal repeats or malforms a claim reference".into(),
            ));
        }
        match claim.kind {
            review_core::ClaimRefKind::Finding => findings.push(claim.id.clone()),
            review_core::ClaimRefKind::Report => reports.push(claim.id.clone()),
        }
    }
    findings.sort();
    reports.sort();
    if findings != candidate.finding_ids
        || reports.len() != candidate.report_indexes.len()
        || reports.iter().any(|report| !report_ids.contains(report))
    {
        return Err(StoreError::Conflict(
            "accepted Proposal claim links contradict its selected Attempt reduction".into(),
        ));
    }
    for finding in &findings {
        let known: i64 = tx.query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1
             AND type = 'FindingReported@1' AND correlation_id = ?2",
            params![run_id, finding],
            |row| row.get(0),
        )?;
        if known == 0 {
            return Err(StoreError::Conflict(
                "accepted Proposal names an unknown Finding".into(),
            ));
        }
    }
    Ok(())
}

fn round_runtime_event(event_type: EventType) -> bool {
    event_type.is_run_report()
        || matches!(
            event_type,
            EventType::TaskReviewResultSelectedV1
                | EventType::CheckCompletedV1
                | EventType::DemandRecordedV1
                | EventType::FindingReportedV1
                | EventType::GateDecisionV1
                | EventType::GateExecutionBoundV1
                | EventType::CacheSnapshotMaterializedV1
                | EventType::GenerationAdvancedV1
                | EventType::NodeInvocationV1
                | EventType::NodeOutputReceiptV1
                | EventType::ProposalPreparedV1
                | EventType::ProposalRefusedV1
                | EventType::ProposalAcceptedV1
                | EventType::SliceSetAcceptedV1
                | EventType::ShardSetRecordedV1
                | EventType::SemanticClosureCheckedV1
                | EventType::WarmSetSelectedV1
                | EventType::WorkerNotesRecordedV1
                | EventType::BuildCacheCapturedV1
                | EventType::WorkspaceRebasedV1
                | EventType::SessionSnapshotPreparedV1
                | EventType::SessionSnapshotCleanedV1
                | EventType::ColdCloseoutDispatchedV1
        )
}

fn event_uses_authority_plan(event_type: EventType) -> bool {
    event_type.is_run_report()
        || matches!(
            event_type,
            EventType::TaskReviewResultSelectedV1
                | EventType::NodeInvocationV1
                | EventType::GateExecutionBoundV1
                | EventType::CacheSnapshotMaterializedV1
                | EventType::NodeOutputReceiptV1
                | EventType::ProposalPreparedV1
                | EventType::ProposalRefusedV1
                | EventType::ProposalAcceptedV1
                | EventType::SliceSetAcceptedV1
                | EventType::ShardSetRecordedV1
                | EventType::SemanticClosureCheckedV1
                | EventType::IntegrationPreparedV1
                | EventType::IntegrationConflictV1
                | EventType::IntegrationChecksCompletedV1
                | EventType::IntegrationCommittedV1
        )
}

fn latest_round(
    tx: &rusqlite::Connection,
    run_id: &str,
) -> Result<Option<(String, review_core::RoundStartedPayloadV1)>, StoreError> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT event_id, payload FROM events
             WHERE run_id = ?1 AND type = 'RoundStarted@1'
             ORDER BY sequence DESC LIMIT 1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(event_id, payload)| Ok((event_id, serde_json::from_str(&payload)?)))
        .transpose()
}

/// Scans every `RunReport@` row, not only the current version, so that a Round closed by a
/// report this release cannot read refuses the append instead of looking open.
const ROUND_TERMINAL_REPORT_SQL: &str = "SELECT type, payload FROM events
     WHERE run_id = ?1 AND causation_id = ?2
       AND type >= 'RunReport@' AND type < 'RunReportA'
     ORDER BY sequence";

fn round_has_terminal_report(
    tx: &rusqlite::Connection,
    run_id: &str,
    round_event_id: &str,
) -> Result<bool, StoreError> {
    let mut statement = tx.prepare(ROUND_TERMINAL_REPORT_SQL)?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (event_type, payload) = row?;
        if event_type != EventType::RunReportV6.as_str() {
            return Err(StoreError::Conflict(
                review_core::UnknownEventType(event_type).to_string(),
            ));
        }
        if report_closes(&payload)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn integration_binding_node<'a>(plan: &'a AuthorityPlan, node: &'a str) -> Option<&'a str> {
    if plan.nodes.contains_key(node) {
        return Some(node);
    }
    let (owner, _) = node.split_once("#slice:")?;
    plan.nodes
        .get(owner)
        .is_some_and(|node| node.kind == "scatter")
        .then_some(owner)
}

#[derive(Clone, Copy)]
struct IntegrationTarget<'a> {
    batch_id: &'a str,
    derived_snapshot_id: &'a str,
}

fn authority_paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn integration_finding_ids(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    proposal: &review_core::PatchProposal,
) -> Result<Vec<String>, StoreError> {
    let mut findings = std::collections::BTreeSet::new();
    for claim in &proposal.finding_refs {
        match claim.kind {
            review_core::ClaimRefKind::Finding => {
                let known: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events WHERE run_id = ?1
                     AND type = 'FindingReported@1' AND correlation_id = ?2",
                    params![run_id, claim.id],
                    |row| row.get(0),
                )?;
                if known == 0 {
                    return Err(StoreError::Conflict(
                        "Integration Proposal names an unknown Finding".into(),
                    ));
                }
                findings.insert(claim.id.clone());
            }
            review_core::ClaimRefKind::Report => {
                let rows = tx
                    .prepare(
                        "SELECT correlation_id FROM events WHERE run_id = ?1
                         AND causation_id = ?2 AND type = 'FindingReported@1'
                         AND json_extract(payload, '$.report_id') = ?3",
                    )?
                    .query_map(params![run_id, round_event_id, claim.id], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let [finding] = rows.as_slice() else {
                    return Err(StoreError::Conflict(
                        "Integration Proposal Report does not resolve uniquely in the current Round"
                            .into(),
                    ));
                };
                findings.insert(finding.clone());
            }
        }
    }
    Ok(findings.into_iter().collect())
}

fn validate_integration_plan_authority(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    active: &review_core::RoundStartedPayloadV1,
    authority: &AuthorityPlan,
    integration_plan: &review_core::IntegrationPlanV1,
    target: IntegrationTarget<'_>,
) -> Result<(), StoreError> {
    let policy = authority.integration.as_ref().ok_or_else(|| {
        StoreError::Conflict("captured pipeline has no automatic-Integration policy".into())
    })?;
    let subject: review_core::SubjectV1 = serde_json::from_value(
        cas.get_json(&active.subject_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    subject.validate().map_err(StoreError::Conflict)?;
    integration_plan.validate().map_err(StoreError::Conflict)?;
    if integration_plan.subject_id != active.subject_id
        || integration_plan.base_snapshot_id != subject.head_snapshot_id
        || integration_plan.policy_id != authority.pipeline_policy_id
        || integration_plan.protected_paths != policy.protected_paths
    {
        return Err(StoreError::Conflict(
            "Integration plan contradicts the captured Subject or policy".into(),
        ));
    }
    let plan_value = serde_json::to_value(integration_plan)?;
    let plan_id = crate::canonical::content_id(&plan_value)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    if target.batch_id != format!("integration-{}", &plan_id[7..23]) {
        return Err(StoreError::Conflict(
            "Integration batch identity is not derived from its exact plan".into(),
        ));
    }

    let round_event_id: String = tx.query_row(
        "SELECT event_id FROM events WHERE run_id = ?1 AND type = 'RoundStarted@1'
         ORDER BY sequence DESC LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let priorities = policy
        .reviewer_priority
        .iter()
        .enumerate()
        .map(|(index, node)| (node.as_str(), index as u32))
        .collect::<std::collections::BTreeMap<_, _>>();
    let default_priority = u32::try_from(priorities.len()).unwrap_or(u32::MAX);
    let mut prior_paths = Vec::<String>::new();
    let mut seen_patches = std::collections::BTreeSet::new();

    let base_snapshot: review_core::SourceSnapshot = serde_json::from_value(
        cas.get_json(&subject.head_snapshot_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    let base_manifest_id = base_snapshot
        .artifact_manifest
        .as_deref()
        .ok_or_else(|| StoreError::Conflict("Integration Base Snapshot has no Manifest".into()))?;
    let base_manifest = cas
        .get_json(base_manifest_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let mut composed_entries = manifest_entries(&base_manifest)?;

    for candidate in &integration_plan.candidates {
        let rows = tx
            .prepare(
                "SELECT node_id, attempt_id, payload FROM events
                 WHERE run_id = ?1 AND causation_id = ?2 AND type = 'ProposalAccepted@1'
                   AND json_extract(payload, '$.proposal_id') = ?3 ORDER BY sequence",
            )?
            .query_map(
                params![run_id, round_event_id, candidate.proposal_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let [(node, attempt, raw)] = rows.as_slice() else {
            return Err(StoreError::Conflict(
                "Integration candidate has no unique current-Round accepted Proposal".into(),
            ));
        };
        let accepted: review_core::ProposalAcceptedPayloadV1 = serde_json::from_str(raw)?;
        if accepted.candidate_artifact_id != candidate.candidate_artifact_id
            || node != &candidate.node_id
        {
            return Err(StoreError::Conflict(
                "Integration candidate contradicts its accepted Proposal event".into(),
            ));
        }
        let binding_node = integration_binding_node(authority, node).ok_or_else(|| {
            StoreError::Conflict(
                "Integration candidate node is absent from captured authority".into(),
            )
        })?;
        let execution = authority.reviewer_execution_for(node).ok_or_else(|| {
            StoreError::Conflict("Integration candidate has no reviewer Execution Binding".into())
        })?;
        if !execution.auto_apply
            || candidate.priority
                != priorities
                    .get(binding_node)
                    .copied()
                    .unwrap_or(default_priority)
        {
            return Err(StoreError::Conflict(
                "Integration candidate lacks captured auto-apply or priority authority".into(),
            ));
        }
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&accepted.proposal_artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?,
        )?;
        crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
        match &envelope.producer {
            review_core::Producer::Attempt {
                run_id: producer_run,
                node_id: producer_node,
                attempt_id: producer_attempt,
            } if producer_run == run_id && producer_node == node && producer_attempt == attempt => {
            }
            _ => {
                return Err(StoreError::Conflict(
                    "Integration Proposal producer is not its selected Attempt".into(),
                ));
            }
        }
        if envelope.artifact_type != review_core::contract::PATCH_PROPOSAL_V1
            || envelope.artifact_id != accepted.proposal_id
            || envelope.subject_snapshot_id.as_deref() != Some(subject.head_snapshot_id.as_str())
        {
            return Err(StoreError::Conflict(
                "Integration candidate has an invalid Proposal envelope".into(),
            ));
        }
        let proposal: review_core::PatchProposal = serde_json::from_value(envelope.payload)?;
        proposal
            .check_shape()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let prepared_candidate: review_core::ProposalCandidateV1 = serde_json::from_value(
            cas.get_json(&candidate.candidate_artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?,
        )?;
        prepared_candidate
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        validate_candidate_manifest(cas, &subject, &prepared_candidate)?;
        let finding_ids = integration_finding_ids(tx, run_id, &round_event_id, &proposal)?;
        if !proposal.auto_apply_nominated
            || proposal.base_snapshot_id != subject.head_snapshot_id
            || proposal.patch_artifact_id != candidate.patch_artifact_id
            || proposal.patch_artifact_id != prepared_candidate.patch_artifact_id
            || proposal.paths != candidate.paths
            || proposal.paths != prepared_candidate.paths
            || proposal.evidence_ids != candidate.evidence_ids
            || proposal.evidence_ids != prepared_candidate.evidence_ids
            || prepared_candidate.derived_manifest_artifact_id
                != candidate.derived_manifest_artifact_id
            || finding_ids != candidate.finding_ids
            || !seen_patches.insert(candidate.patch_artifact_id.clone())
        {
            return Err(StoreError::Conflict(
                "Integration candidate contradicts its sealed Proposal authority".into(),
            ));
        }
        if candidate.paths.iter().any(|path| {
            policy
                .protected_paths
                .iter()
                .any(|protected| authority_paths_overlap(path, protected))
                || prior_paths
                    .iter()
                    .any(|prior| authority_paths_overlap(path, prior))
        }) {
            return Err(StoreError::Conflict(
                "Integration plan changes a protected or overlapping path".into(),
            ));
        }
        prior_paths.extend(candidate.paths.iter().cloned());
        let candidate_manifest = cas
            .get_json(&candidate.derived_manifest_artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let candidate_entries = manifest_entries(&candidate_manifest)?;
        for path in &candidate.paths {
            match candidate_entries.get(path).cloned() {
                Some(entry) => {
                    composed_entries.insert(path.clone(), entry);
                }
                None => {
                    composed_entries.remove(path);
                }
            }
        }
    }

    let expected_manifest = manifest_value(composed_entries);
    let expected_manifest_id = crate::canonical::content_id(&expected_manifest)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let recorded_manifest = cas
        .get_json(&integration_plan.derived_manifest_artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    if expected_manifest_id != integration_plan.derived_manifest_artifact_id
        || recorded_manifest != expected_manifest
    {
        return Err(StoreError::Conflict(
            "Integration derived Manifest is not the deterministic Proposal composition".into(),
        ));
    }
    let derived: review_core::SourceSnapshot = serde_json::from_value(
        cas.get_json(target.derived_snapshot_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    let capture_matches = matches!(
        &derived.capture,
        review_core::Capture::Derived {
            tree_id,
            parent_snapshot_id,
            integration_batch_id,
        } if tree_id == &derived.content_digest
            && parent_snapshot_id == &subject.head_snapshot_id
            && integration_batch_id == target.batch_id
    );
    if !capture_matches
        || derived.repository_id != base_snapshot.repository_id
        || derived.vcs != base_snapshot.vcs
        || derived.parent_snapshot_id.as_deref() != Some(subject.head_snapshot_id.as_str())
        || derived.artifact_manifest.as_deref()
            != Some(integration_plan.derived_manifest_artifact_id.as_str())
        || derived.source_revision != base_snapshot.source_revision
        || derived.submodules != base_snapshot.submodules
    {
        return Err(StoreError::Conflict(
            "Integration derived Snapshot is not the exact sealed child of its Base".into(),
        ));
    }
    Ok(())
}

fn validate_integration_commit_authority(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    active: &review_core::RoundStartedPayloadV1,
    committed: &review_core::IntegrationCommittedPayloadV1,
    batch_attestations: &std::collections::BTreeSet<String>,
    authority: &AuthorityPlan,
) -> Result<(), StoreError> {
    let subject: review_core::SubjectV1 = serde_json::from_value(
        cas.get_json(&active.subject_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    if subject.head_snapshot_id != committed.prior_snapshot_id {
        return Err(StoreError::Conflict(
            "Integration commit expected head is stale".into(),
        ));
    }
    let opened: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1' LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let opened: review_core::CampaignOpenedPayloadV1 = serde_json::from_str(&opened)?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&opened.campaign_manifest_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    if committed.policy_id != manifest.pipeline.artifact_id
        || !manifest
            .execution_policy_ids
            .iter()
            .any(|policy| policy == &committed.policy_id)
    {
        return Err(StoreError::Conflict(
            "Integration commit policy is absent from captured Campaign authority".into(),
        ));
    }

    let prepared_raw: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1
         AND type = 'IntegrationPrepared@1'
         AND json_extract(payload, '$.batch_id') = ?2",
        params![run_id, committed.batch_id],
        |row| row.get(0),
    )?;
    let prepared: review_core::IntegrationPreparedPayloadV1 = serde_json::from_str(&prepared_raw)?;
    let plan: review_core::IntegrationPlanV1 = serde_json::from_value(
        cas.get_json(&prepared.plan_artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    plan.validate().map_err(StoreError::Conflict)?;
    let planned_proposals: Vec<&str> = plan
        .candidates
        .iter()
        .map(|candidate| candidate.proposal_id.as_str())
        .collect();
    let committed_proposals: Vec<&str> =
        committed.proposal_ids.iter().map(String::as_str).collect();
    if plan.subject_id != active.subject_id
        || plan.base_snapshot_id != committed.prior_snapshot_id
        || plan.policy_id != committed.policy_id
        || prepared.derived_snapshot_id != committed.derived_snapshot_id
        || planned_proposals != committed_proposals
    {
        return Err(StoreError::Conflict(
            "Integration commit contradicts its exact prepared plan".into(),
        ));
    }
    validate_integration_plan_authority(
        tx,
        cas,
        run_id,
        active,
        authority,
        &plan,
        IntegrationTarget {
            batch_id: &committed.batch_id,
            derived_snapshot_id: &committed.derived_snapshot_id,
        },
    )?;

    let checks_raw: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1
         AND type = 'IntegrationChecksCompleted@1'
         AND json_extract(payload, '$.batch_id') = ?2
         AND json_extract(payload, '$.passed') = 1",
        params![run_id, committed.batch_id],
        |row| row.get(0),
    )?;
    let checked: review_core::IntegrationChecksCompletedPayloadV1 =
        serde_json::from_str(&checks_raw)?;
    let checks: review_core::IntegrationChecksV1 = serde_json::from_value(
        cas.get_json(&checked.checks_artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    if !checks.passed() || checks.derived_snapshot_id != committed.derived_snapshot_id {
        return Err(StoreError::Conflict(
            "Integration commit is not bound to passing checks on its exact Snapshot".into(),
        ));
    }

    let mut finding_set = None;
    let mut demand_set = None;
    let mut statement = tx.prepare(
        "SELECT payload FROM events WHERE run_id = ?1 AND causation_id = (
             SELECT event_id FROM events WHERE run_id = ?1 AND type = 'RoundStarted@1'
             ORDER BY sequence DESC LIMIT 1
         ) AND type = 'NodeOutputReceipt@1' ORDER BY sequence",
    )?;
    for raw in statement
        .query_map(params![run_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
    {
        let receipt: review_core::NodeOutputReceiptPayloadV1 = serde_json::from_str(&raw)?;
        for port in receipt.outputs {
            let [artifact] = port.artifact_ids.as_slice() else {
                continue;
            };
            if port.artifact_type == review_core::contract::FINDING_SET_V1 {
                finding_set = Some(artifact.clone());
            } else if port.artifact_type == review_core::contract::DEMAND_SET_V1 {
                demand_set = Some(artifact.clone());
            }
        }
    }
    let task_report:Option<String>=tx.query_row(
        "SELECT payload FROM events WHERE run_id=?1 AND type='RunReport@6' AND causation_id=(SELECT event_id FROM events WHERE run_id=?1 AND type='RoundStarted@1' ORDER BY sequence DESC LIMIT 1) ORDER BY sequence DESC LIMIT 1", [run_id],|r|r.get(0)).optional()?;
    if let Some(raw) = task_report {
        let report: review_core::RunReportPayloadV6 = serde_json::from_str(&raw)?;
        report.validate().map_err(StoreError::Conflict)?;
        let (findings, demands) =
            task::review_integration::canonical_task_integration_views(cas, &report)?;
        finding_set = Some(findings);
        demand_set = Some(demands);
    }
    if finding_set.as_deref() != Some(committed.expected_finding_set_id.as_str())
        || demand_set.as_deref() != Some(committed.expected_demand_set_id.as_str())
    {
        return Err(StoreError::Conflict(
            "Integration commit expected Finding or Demand view is stale".into(),
        ));
    }

    let closure_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1
         AND causation_id = (SELECT event_id FROM events WHERE run_id = ?1
             AND type = 'RoundStarted@1' ORDER BY sequence DESC LIMIT 1)
         AND type = 'SemanticClosureChecked@1'
         AND json_extract(payload, '$.record_id') = ?2",
        params![run_id, committed.semantic_closure_id],
        |row| row.get(0),
    )?;
    if closure_count != 1 {
        return Err(StoreError::Conflict(
            "Integration commit lacks the current Round semantic-closure proof".into(),
        ));
    }
    let mut accepted = std::collections::BTreeSet::new();
    let mut statement = tx.prepare(
        "SELECT payload FROM events WHERE run_id = ?1
         AND causation_id = (SELECT event_id FROM events WHERE run_id = ?1
             AND type = 'RoundStarted@1' ORDER BY sequence DESC LIMIT 1)
         AND type = 'ProposalAccepted@1'",
    )?;
    for raw in statement
        .query_map(params![run_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
    {
        let payload: review_core::ProposalAcceptedPayloadV1 = serde_json::from_str(&raw)?;
        accepted.insert(payload.proposal_id);
    }
    if committed
        .proposal_ids
        .iter()
        .any(|proposal| !accepted.contains(proposal))
        || committed
            .attestation_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            != batch_attestations.iter().collect()
    {
        return Err(StoreError::Conflict(
            "Integration commit names unselected Proposals or non-atomic attestations".into(),
        ));
    }
    Ok(())
}

fn validate_report_receipts(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    round_event_id: &str,
    outcomes: &[review_core::RunNodeReportV2],
) -> Result<(), StoreError> {
    let reported_outputs: std::collections::BTreeMap<String, Vec<String>> = outcomes
        .iter()
        .filter_map(|outcome| match &outcome.outcome {
            review_core::RunNodeOutcomeV2::Completed { output_artifacts } => {
                Some((outcome.node.clone(), output_artifacts.clone()))
            }
            _ => None,
        })
        .collect();
    let mut receipts = std::collections::BTreeMap::new();
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'NodeOutputReceipt@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (node, raw) = row?;
        let receipt: review_core::NodeOutputReceiptPayloadV1 = serde_json::from_str(&raw)?;
        let node =
            node.ok_or_else(|| StoreError::Conflict("NodeOutputReceipt@1 has no node ID".into()))?;
        if node != receipt.node || receipts.insert(node, receipt).is_some() {
            return Err(StoreError::Conflict(
                "RunReport@6 has ambiguous durable output receipts".into(),
            ));
        }
    }
    for outcome in outcomes {
        if let review_core::RunNodeOutcomeV2::Completed { output_artifacts } = &outcome.outcome {
            let mut output_artifacts = output_artifacts.clone();
            output_artifacts.sort();
            let receipt = receipts.remove(&outcome.node).ok_or_else(|| {
                StoreError::Conflict(format!(
                    "RunReport@6 completed node '{}' without a durable receipt",
                    outcome.node,
                ))
            })?;
            let mut durable: Vec<String> = receipt
                .outputs
                .into_iter()
                .flat_map(|port| port.artifact_ids)
                .collect();
            durable.sort();
            if durable != output_artifacts {
                return Err(StoreError::Conflict(format!(
                    "RunReport@6 contradicts the receipt for node '{}'",
                    outcome.node,
                )));
            }
        } else if receipts.contains_key(&outcome.node) {
            return Err(StoreError::Conflict(format!(
                "RunReport@6 suppresses or fails node '{}' after it published a receipt",
                outcome.node,
            )));
        }
    }
    if !receipts.is_empty() {
        // Dynamic shard nodes are owned by one static Scatter and therefore are not top-level
        // plan outcomes. They are nevertheless complete only when every exact receipt is
        // represented in that Scatter's reported ShardSet; this is transitive receipt coverage,
        // not an exception to it.
        let plan = load_authority_plan(tx, cas, run_id)?;
        for (node, receipt) in receipts {
            let authority = dynamic_node_authority(tx, cas, run_id, round_event_id, &plan, &node)?
                .ok_or_else(|| {
                    StoreError::Conflict(format!(
                        "RunReport@6 omits static node `{node}` with a durable output receipt"
                    ))
                })?;
            let owner = node
                .split_once("#slice:")
                .map(|(owner, _)| owner)
                .ok_or_else(|| {
                    StoreError::Conflict("dynamic receipt has no tagged Scatter owner".into())
                })?;
            let shard_set_record = reported_outputs
                .get(owner)
                .into_iter()
                .flatten()
                .find_map(|artifact| {
                    let value = cas.get_json(artifact).ok()?;
                    let envelope =
                        serde_json::from_value::<review_core::ArtifactEnvelope>(value).ok()?;
                    (envelope.artifact_type == review_core::contract::SHARD_SET_V1)
                        .then_some(envelope)
                })
                .ok_or_else(|| {
                    StoreError::Conflict(format!(
                        "dynamic receipt `{node}` has no reported owner ShardSet"
                    ))
                })?;
            crate::validate_envelope(&shard_set_record).map_err(StoreError::Conflict)?;
            let shard_set: review_core::ShardSetV1 =
                serde_json::from_value(shard_set_record.payload)?;
            shard_set.validate_shape().map_err(StoreError::Conflict)?;
            let shard = shard_set
                .shards
                .iter()
                .find(|shard| {
                    shard.runtime_node_id == node && shard.slice_id == authority.slice.slice_id
                })
                .ok_or_else(|| {
                    StoreError::Conflict(format!(
                        "dynamic receipt `{node}` is absent from its owner ShardSet"
                    ))
                })?;
            let review_core::ShardOutcomeV1::Completed {
                result_artifact_ids,
            } = &shard.outcome
            else {
                return Err(StoreError::Conflict(format!(
                    "dynamic receipt `{node}` contradicts a non-completed Shard outcome"
                )));
            };
            let mut durable: Vec<String> = receipt
                .outputs
                .into_iter()
                .flat_map(|port| port.artifact_ids)
                .collect();
            durable.sort();
            let mut represented = result_artifact_ids.clone();
            represented.sort();
            if durable != represented {
                return Err(StoreError::Conflict(format!(
                    "dynamic receipt `{node}` contradicts its exact Shard outcome"
                )));
            }
        }
    }
    Ok(())
}

fn report_closes(payload: &str) -> Result<bool, StoreError> {
    let report: review_core::RunReportPayloadV6 = serde_json::from_str(payload)?;
    report
        .validate()
        .map_err(|error| StoreError::Json(serde::de::Error::custom(error)))?;
    Ok(!matches!(
        report.verdict,
        review_core::RunVerdictV3::Incomplete { .. }
    ))
}

/// Event IDs are derived, not random: a replay of the same run must reproduce them, and a
/// random ID would make two otherwise identical runs incomparable.
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
    review_core::event::validate_event_payload(event_type, &payload).map_err(|error| {
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

fn derive_event_id(run_id: &str, sequence: i64) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"review.kernel/event-id/v1\0");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(sequence.to_string().as_bytes());
    review_core::hex::encode(&hasher.finalize())[..26].to_string()
}

fn insert_events(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    events: &[NewEvent],
    first: i64,
) -> Result<Vec<RunEvent>, StoreError> {
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

    #[test]
    fn terminal_report_lookup_uses_the_full_type_index_prefix() {
        let (_dir, store, _cas) = fixture();
        let mut statement = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {ROUND_TERMINAL_REPORT_SQL}"))
            .unwrap();
        let details: Vec<String> = statement
            .query_map(params!["run", "round"], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("type>? AND type<?")),
            "query plan did not seek the report type range: {details:?}"
        );
    }

    /// A Round concluded by a report this release cannot read is neither open nor closed: the
    /// lookup refuses instead of letting a new event treat the Round as still open.
    #[test]
    fn a_round_with_a_retired_report_refuses_instead_of_looking_open() {
        let (_dir, store, _cas) = fixture();
        store
            .conn
            .execute(
                "INSERT INTO events (run_id, sequence, event_id, type, occurred_at,
                                     causation_id, artifact_refs, payload)
                 VALUES ('run', 0, 'event', 'RunReport@5', '1970-01-01T00:00:00Z',
                         'round', '[]', '{}')",
                [],
            )
            .unwrap();
        let error = round_has_terminal_report(&store.conn, "run", "round").unwrap_err();
        assert!(
            matches!(&error, StoreError::Conflict(message)
                if message.contains("unknown review-kernel event type: RunReport@5")),
            "{error}"
        );
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
                "verdict": "request-changes",
                "summary": null,
                "reports": [report],
                "benchmark_demands": [],
                "disputes": [],
            })
        };
        let legacy = json!({
            "severity": "major",
            "file": "src/a.rs",
            "line": 1,
            "title": "legacy",
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

        assert!(review_core::validate_reviewer_result(&result(legacy)).is_ok());
        assert!(review_core::validate_reviewer_result(&result(typed)).is_err());
    }

    #[test]
    fn reviewer_result_legacy_conformance_corpus_matches_durable_reader() {
        let path = workspace_root().join("schemas/reviewer-result-v1-conformance.json");
        let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for case in corpus["valid"].as_array().unwrap() {
            assert!(
                review_core::validate_reviewer_result(&case["payload"]).is_ok(),
                "{}",
                case["name"]
            );
        }
        for case in corpus["invalid"].as_array().unwrap() {
            assert!(
                review_core::validate_reviewer_result(&case["payload"]).is_err(),
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn cached_change_set_validation_still_detects_later_cas_corruption() {
        let (directory, _store, cas) = fixture();
        let change_set = review_core::ChangeSetV1::new(
            crate::canonical::blob_content_id(b"base"),
            crate::canonical::blob_content_id(b"head"),
            vec!["src/lib.rs".into()],
            vec![],
            b"diff --git a/src/lib.rs b/src/lib.rs\n",
            "git version test",
            "test-policy@1",
        )
        .unwrap();
        let artifact_id = cas
            .put_json(&serde_json::to_value(change_set).unwrap())
            .unwrap();
        let value = cas.get_json_for_publication(&artifact_id).unwrap();
        let change_set: review_core::ChangeSetV1 = serde_json::from_value(value).unwrap();
        let prepared = PreparedArtifacts {
            verified: std::collections::BTreeSet::from([artifact_id.clone()]),
            json: std::collections::BTreeMap::new(),
            change_sets: std::collections::BTreeMap::from([(
                artifact_id.clone(),
                Arc::new(change_set),
            )]),
        };

        assert!(
            validate_artifact_payload(
                &prepared,
                review_core::contract::CHANGE_SET_V1,
                &artifact_id,
            )
            .unwrap()
            .is_some()
        );

        let hex = artifact_id.strip_prefix("sha256:").unwrap();
        std::fs::write(
            directory
                .path()
                .join("cas/objects")
                .join(&hex[..2])
                .join(&hex[2..]),
            b"tampered after validation",
        )
        .unwrap();
        let error = cas.prepare_for_publication(&artifact_id).unwrap_err();
        assert!(
            error.to_string().contains("does not match its digest"),
            "{error}"
        );
    }

    #[test]
    fn an_unprepared_change_set_is_a_conflict_not_a_panic() {
        let artifact_id = crate::canonical::blob_content_id(b"change set");
        let prepared = PreparedArtifacts {
            verified: std::collections::BTreeSet::from([artifact_id.clone()]),
            json: std::collections::BTreeMap::from([(artifact_id.clone(), json!({}))]),
            change_sets: std::collections::BTreeMap::new(),
        };
        let error = validate_artifact_payload(
            &prepared,
            review_core::contract::CHANGE_SET_V1,
            &artifact_id,
        )
        .unwrap_err();
        assert!(error.to_string().contains("was not prepared"), "{error}");
    }

    #[test]
    fn every_change_set_in_one_batch_remains_available_to_validation() {
        let change_set = |base: &[u8], head: &[u8]| {
            Arc::new(
                review_core::ChangeSetV1::new(
                    crate::canonical::blob_content_id(base),
                    crate::canonical::blob_content_id(head),
                    vec!["src/lib.rs".into()],
                    vec![],
                    b"patch",
                    "git version test",
                    "test-policy@1",
                )
                .unwrap(),
            )
        };
        let first_id = crate::canonical::blob_content_id(b"first change set");
        let second_id = crate::canonical::blob_content_id(b"second change set");
        let prepared = PreparedArtifacts {
            verified: std::collections::BTreeSet::from([first_id.clone(), second_id.clone()]),
            json: std::collections::BTreeMap::new(),
            change_sets: std::collections::BTreeMap::from([
                (first_id.clone(), change_set(b"base-1", b"head-1")),
                (second_id.clone(), change_set(b"base-2", b"head-2")),
            ]),
        };

        for artifact_id in [&first_id, &second_id] {
            assert!(
                validate_artifact_payload(
                    &prepared,
                    review_core::contract::CHANGE_SET_V1,
                    artifact_id,
                )
                .unwrap()
                .is_some()
            );
        }
    }

    #[test]
    fn parsed_change_set_cache_retains_only_the_latest_authority() {
        let change_set = Arc::new(
            review_core::ChangeSetV1::new(
                crate::canonical::blob_content_id(b"base"),
                crate::canonical::blob_content_id(b"head"),
                vec!["src/lib.rs".into()],
                vec![],
                b"patch",
                "git version test",
                "test-policy@1",
            )
            .unwrap(),
        );
        let mut cache = std::collections::BTreeMap::new();
        remember_validated_change_set(&mut cache, "first".into(), Arc::clone(&change_set));
        remember_validated_change_set(&mut cache, "second".into(), change_set);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key("second"));
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
}
