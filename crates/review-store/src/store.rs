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
    legacy_import: bool,
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
            legacy_import: false,
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

    pub(crate) fn legacy_import(mut self) -> Self {
        self.legacy_import = true;
        self
    }
}

pub struct EventStore {
    conn: Connection,
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

impl EventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            conn,
            validated_change_sets: std::collections::BTreeMap::new(),
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
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
                 ON events (run_id, causation_id, type, sequence);",
        )?;
        Ok(Self {
            conn,
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

    /// Append through the frozen pre-campaign compatibility path.
    ///
    /// New campaign code must use [`append`](Self::append); this explicit entry point exists for
    /// import/parity tooling whose historical events predate CampaignOpened@1.
    pub fn append_legacy(
        &mut self,
        run_id: &str,
        cas: &Cas,
        event: NewEvent,
    ) -> Result<RunEvent, StoreError> {
        self.append(run_id, cas, event.legacy_import())
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
        if events.is_empty() {
            return Ok(Vec::new());
        }
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
        validate_campaign_transition(&tx, cas, run_id, events, first, &prepared)?;
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
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
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
        tx.commit()?;
        Ok(appended)
    }

    /// Ordered transitions for one provider operation. The correlation index keeps admission
    /// proportional to that operation rather than to the Campaign's entire append-only log.
    pub fn provider_operation_transitions(
        &self,
        run_id: &str,
        operation_id: &str,
    ) -> Result<Vec<review_core::ProviderOperationTransitionPayloadV1>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload FROM events
             WHERE run_id = ?1 AND type = 'ProviderOperationTransition@1'
               AND correlation_id = ?2 ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![run_id, operation_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let payload = row?;
            serde_json::from_str(&payload).map_err(StoreError::from)
        })
        .collect()
    }

    /// Reviewer nodes carrying Provider Operations under one exact Round authority epoch.
    pub fn provider_operation_nodes(
        &self,
        run_id: &str,
        round_event_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT node_id FROM events
             WHERE run_id = ?1 AND causation_id = ?2
               AND type = 'ProviderOperationTransition@1' AND node_id IS NOT NULL
             ORDER BY node_id",
        )?;
        stmt.query_map(params![run_id, round_event_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Committed and crash-reserved token spend for one Round lineage, without replaying
    /// unrelated Campaign events or re-reading output artifacts from the CAS. Every epoch with
    /// the active Round's number and Campaign Manifest contributes because supersession never
    /// refunds already-spent work.
    pub fn round_committed_tokens(
        &self,
        run_id: &str,
        round_event_id: &str,
    ) -> Result<u64, StoreError> {
        let round_payload: String = self.conn.query_row(
            "SELECT payload FROM events
             WHERE run_id = ?1 AND event_id = ?2 AND type = 'RoundStarted@1'",
            params![run_id, round_event_id],
            |row| row.get(0),
        )?;
        let round: review_core::RoundStartedPayloadV1 = serde_json::from_str(&round_payload)?;
        let round_number = i64::from(round.round);
        let sum = |query: &str| -> Result<u64, StoreError> {
            let value: i64 = self.conn.query_row(
                query,
                params![run_id, round_number, round.campaign_manifest_id.as_str()],
                |row| row.get(0),
            )?;
            u64::try_from(value)
                .map_err(|_| StoreError::Conflict("replayed token charge overflow".into()))
        };
        let terminal_attempts = sum(
            "SELECT COALESCE(SUM(CASE terminal.type
                       WHEN 'AttemptAdmitted@1' THEN CAST(json_extract(terminal.payload, '$.cost_tokens') AS INTEGER)
                       WHEN 'AttemptFailed@1' THEN COALESCE(CAST(json_extract(terminal.payload, '$.charged') AS INTEGER), 0)
                       WHEN 'AttemptFenced@1' THEN COALESCE(CAST(json_extract(terminal.payload, '$.charged') AS INTEGER), 0)
                       ELSE 0
                     END), 0)
             FROM events AS terminal
             WHERE terminal.run_id = ?1
               AND EXISTS (
                 SELECT 1 FROM events AS round
                 WHERE round.run_id = terminal.run_id
                   AND round.event_id = terminal.causation_id
                   AND round.type = 'RoundStarted@1'
                   AND json_extract(round.payload, '$.round') = ?2
                   AND json_extract(round.payload, '$.campaign_manifest_id') = ?3
               )
               AND terminal.type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                     'AttemptFenced@1', 'AttemptReleased@1')
               AND terminal.sequence = (
                 SELECT MIN(first.sequence) FROM events AS first
                 WHERE first.run_id = terminal.run_id
                   AND first.causation_id = terminal.causation_id
                   AND first.attempt_id = terminal.attempt_id
                   AND first.type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                      'AttemptFenced@1', 'AttemptReleased@1')
               )",
        )?;
        let outstanding_attempts = sum(
            "SELECT COALESCE(SUM(CAST(json_extract(dispatched.payload, '$.reserved') AS INTEGER)), 0)
             FROM events AS dispatched
             WHERE dispatched.run_id = ?1
               AND EXISTS (
                 SELECT 1 FROM events AS round
                 WHERE round.run_id = dispatched.run_id
                   AND round.event_id = dispatched.causation_id
                   AND round.type = 'RoundStarted@1'
                   AND json_extract(round.payload, '$.round') = ?2
                   AND json_extract(round.payload, '$.campaign_manifest_id') = ?3
               )
               AND dispatched.type = 'AttemptDispatched@1'
               AND NOT EXISTS (
                 SELECT 1 FROM events AS terminal
                 WHERE terminal.run_id = dispatched.run_id
                   AND terminal.causation_id = dispatched.causation_id
                   AND terminal.attempt_id = dispatched.attempt_id
                   AND terminal.type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                         'AttemptFenced@1', 'AttemptReleased@1')
               )",
        )?;
        let provider_charges = sum(
            "SELECT COALESCE(SUM(CAST(json_extract(operation.payload, '$.charged_tokens') AS INTEGER)), 0)
             FROM events AS operation
             WHERE operation.run_id = ?1
               AND operation.type = 'ProviderOperationTransition@1'
               AND EXISTS (
                 SELECT 1 FROM events AS round
                 WHERE round.run_id = operation.run_id
                   AND round.event_id = operation.causation_id
                   AND round.type = 'RoundStarted@1'
                   AND json_extract(round.payload, '$.round') = ?2
                   AND json_extract(round.payload, '$.campaign_manifest_id') = ?3
               )",
        )?;
        let outstanding_providers = sum(
            "SELECT COALESCE(SUM(CAST(json_extract(operation.payload, '$.reserved_tokens') AS INTEGER)), 0)
             FROM events AS operation
             WHERE operation.run_id = ?1
               AND operation.type = 'ProviderOperationTransition@1'
               AND EXISTS (
                 SELECT 1 FROM events AS round
                 WHERE round.run_id = operation.run_id
                   AND round.event_id = operation.causation_id
                   AND round.type = 'RoundStarted@1'
                   AND json_extract(round.payload, '$.round') = ?2
                   AND json_extract(round.payload, '$.campaign_manifest_id') = ?3
               )
               AND json_extract(operation.payload, '$.state') = 'running'
               AND json_type(operation.payload, '$.failure_class') IS NULL
               AND operation.sequence = (
                 SELECT MAX(latest.sequence) FROM events AS latest
                 WHERE latest.run_id = operation.run_id
                   AND latest.causation_id = operation.causation_id
                   AND latest.type = operation.type
                   AND latest.correlation_id = operation.correlation_id
               )",
        )?;
        terminal_attempts
            .checked_add(outstanding_attempts)
            .and_then(|value| value.checked_add(provider_charges))
            .and_then(|value| value.checked_add(outstanding_providers))
            .ok_or_else(|| StoreError::Conflict("replayed token charge overflow".into()))
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
    edges: Vec<toml::Value>,
    #[serde(default)]
    budgets: Option<toml::Value>,
    #[serde(default)]
    convergence: Option<toml::Value>,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
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
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum AuthorityPort {
    Name(String),
    Detailed(AuthorityPortDetails),
}

#[derive(Debug, serde::Deserialize)]
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
    nodes: std::collections::BTreeMap<String, AuthorityNode>,
    budgeted: bool,
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
    let budgeted = manifest.budgets.is_some();
    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let pipeline = std::str::from_utf8(&pipeline)
        .map_err(|error| StoreError::Conflict(format!("pinned pipeline is not UTF-8: {error}")))?;
    let definition: AuthorityDefinition = toml::from_str(pipeline)
        .map_err(|error| StoreError::Conflict(format!("pinned pipeline is invalid: {error}")))?;
    if definition.version == 0 {
        return Err(StoreError::Conflict(
            "pinned pipeline has no supported version".into(),
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
    Ok(AuthorityPlan { nodes, budgeted })
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
            EventType::AttemptInputV1 => {
                let payload: review_core::event::AttemptInputPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.refusal_history_id,
                    review_core::contract::REFUSAL_HISTORY_V1.into(),
                )?;
                continue;
            }
            EventType::AttemptFeedbackV1 => {
                let payload: review_core::event::AttemptFeedbackPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.refusal_history_id,
                    review_core::contract::REFUSAL_HISTORY_V1.into(),
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
        review_core::contract::REVIEWER_RESULT_V1 => validate_reviewer_result(value)?,
        review_core::contract::REVIEWER_RESULT_V2 => {
            review_core::validate_reviewer_result_v2(value).map_err(StoreError::Conflict)?
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
            if value.get("type").is_some() {
                let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                    .map_err(|error| {
                        StoreError::Conflict(format!(
                            "FindingSet@1 artifact is not an envelope: {error}"
                        ))
                    })?;
                crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
                if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
                    return Err(StoreError::Conflict(
                        "FindingSet@1 envelope carries the wrong type".into(),
                    ));
                }
                let payload: review_core::FindingSetV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingSet@1 envelope has an invalid payload: {error}"
                    ))
                })?;
                payload.validate().map_err(StoreError::Conflict)?;
            } else {
                // Permanent reader for the pre-M3 summary artifact.
                exact_keys(object, &["round", "sources", "findings"], artifact_type)?;
                if value["round"].as_u64().is_none()
                    || !string_array(&value["sources"])
                    || value["findings"].as_u64().is_none()
                {
                    return Err(StoreError::Conflict(
                        "FindingSet@1 artifact violates its payload contract".into(),
                    ));
                }
            }
        }
        _ => {
            return Err(StoreError::Conflict(format!(
                "no payload validator is registered for {artifact_type}"
            )));
        }
    }
    Ok(None)
}

fn validate_prepared_refusal_history(
    prepared: &PreparedArtifacts,
    artifact_id: &str,
    event_type: &str,
) -> Result<(), StoreError> {
    let history = prepared
        .json
        .get(artifact_id)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            StoreError::Conflict(format!(
                "{event_type} refusal history is not a verified JSON array"
            ))
        })?;
    if history.is_empty()
        || history
            .iter()
            .any(|entry| entry.as_str().is_none_or(|entry| entry.trim().is_empty()))
    {
        return Err(StoreError::Conflict(format!(
            "{event_type} has empty refusal history"
        )));
    }
    Ok(())
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

pub fn validate_reviewer_result(value: &Value) -> Result<(), StoreError> {
    review_core::validate_reviewer_result(value).map_err(StoreError::Conflict)
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

fn report_outcomes(
    event_type: EventType,
    payload: &Value,
) -> Result<Vec<review_core::RunNodeReportV2>, StoreError> {
    match event_type {
        EventType::RunReportV2 => Ok(serde_json::from_value::<review_core::RunReportPayloadV2>(
            payload.clone(),
        )?
        .outcomes),
        EventType::RunReportV3 => Ok(serde_json::from_value::<review_core::RunReportPayloadV3>(
            payload.clone(),
        )?
        .outcomes),
        _ => Err(StoreError::Conflict(format!(
            "{event_type} has no structural run-report outcomes"
        ))),
    }
}

fn validate_report_plan(
    plan: &AuthorityPlan,
    event_type: EventType,
    payload: &Value,
) -> Result<(), StoreError> {
    let outcomes = report_outcomes(event_type, payload)?;
    let expected: std::collections::BTreeSet<&str> =
        plan.nodes.keys().map(String::as_str).collect();
    let actual: std::collections::BTreeSet<&str> = outcomes
        .iter()
        .map(|outcome| outcome.node.as_str())
        .collect();
    if expected != actual || actual.len() != outcomes.len() {
        return Err(StoreError::Conflict(format!(
            "{event_type} does not cover exactly the pinned Campaign plan"
        )));
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
    let mut pending_fences = std::collections::BTreeSet::new();
    let mut batch_dispatches = std::collections::BTreeMap::new();
    let mut batch_latest_dispatch = std::collections::BTreeMap::new();
    let mut batch_attempt_inputs = std::collections::BTreeMap::new();
    let mut batch_attempt_feedback = std::collections::BTreeMap::new();
    let mut batch_terminals: std::collections::BTreeMap<String, EventType> =
        std::collections::BTreeMap::new();
    let mut batch_terminal_nodes = std::collections::BTreeMap::new();
    let mut batch_selected = std::collections::BTreeMap::new();
    let mut batch_invocations = std::collections::BTreeSet::new();
    let mut batch_receipts = std::collections::BTreeSet::new();
    let mut batch_findings = std::collections::BTreeSet::new();
    let mut batch_demands = std::collections::BTreeSet::new();
    let mut active_groupings = load_active_groupings(tx, run_id)?;
    let mut batch_provider_operations: std::collections::BTreeMap<
        String,
        review_core::ProviderOperationTransitionPayloadV1,
    > = std::collections::BTreeMap::new();

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
                let mut statement = tx.prepare(
                    "SELECT dispatch.attempt_id FROM events AS dispatch
                     WHERE dispatch.run_id = ?1 AND dispatch.causation_id = ?2
                       AND dispatch.type = 'AttemptDispatched@1'
                       AND NOT EXISTS (
                           SELECT 1 FROM events AS terminal
                           WHERE terminal.run_id = dispatch.run_id
                             AND terminal.causation_id = dispatch.causation_id
                             AND terminal.attempt_id = dispatch.attempt_id
                             AND terminal.type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                                   'AttemptFenced@1', 'AttemptReleased@1')
                       )",
                )?;
                pending_fences = statement
                    .query_map(params![run_id, active_id], |row| row.get::<_, String>(0))?
                    .collect::<Result<_, _>>()?;
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
                    if !pending_fences.is_empty() {
                        return Err(StoreError::Conflict(format!(
                            "replacement RoundStarted@1 leaves {} outstanding attempts unfenced",
                            pending_fences.len()
                        )));
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
                    if event.legacy_import
                        && matches!(
                            event_type,
                            EventType::CheckCompletedV1
                                | EventType::FindingReportedV1
                                | EventType::GenerationAdvancedV1
                        )
                    {
                        if event_type == EventType::FindingReportedV1 {
                            let key = event.payload["key"].as_str().ok_or_else(|| {
                                StoreError::Conflict(
                                    "legacy FindingReported@1 has no finding key".into(),
                                )
                            })?;
                            batch_findings.insert(key.to_string());
                        }
                        continue;
                    }
                    return Err(StoreError::Conflict(format!(
                        "{event_type} requires an active Round"
                    )));
                }
                if event_type == EventType::RunReportV1 {
                    return Err(StoreError::Conflict(
                        "RunReport@1 is replay-only and cannot be appended".into(),
                    ));
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
                        EventType::ProviderOperationTransitionV1 => {
                            let transition: review_core::ProviderOperationTransitionPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if transition.round != active_payload.round
                                || transition.round_epoch != active_payload.epoch
                                || event.node_id.as_deref() != Some(transition.node_id.as_str())
                                || event.attempt_id != transition.attempt_id
                                || event.correlation_id.as_deref()
                                    != Some(transition.operation_id.as_str())
                            {
                                return Err(StoreError::Conflict(
                                    "ProviderOperationTransition@1 is not bound to its active Round, node, attempt, and operation".into(),
                                ));
                            }
                            let previous = match batch_provider_operations
                                .get(&transition.operation_id)
                            {
                                Some(previous) => Some(previous.clone()),
                                None => tx
                                    .query_row(
                                        "SELECT payload FROM events WHERE run_id = ?1 AND type = 'ProviderOperationTransition@1' AND correlation_id = ?2 ORDER BY sequence DESC LIMIT 1",
                                        params![run_id, transition.operation_id],
                                        |row| row.get::<_, String>(0),
                                    )
                                    .optional()?
                                    .map(|payload| serde_json::from_str(&payload))
                                    .transpose()?,
                            };
                            transition
                                .validate_after(previous.as_ref())
                                .map_err(StoreError::Conflict)?;
                            batch_provider_operations
                                .insert(transition.operation_id.clone(), transition);
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
                                let expected = plan.nodes.get(node).ok_or_else(|| {
                                    StoreError::Conflict(format!(
                                        "node '{node}' is absent from the pinned Campaign plan"
                                    ))
                                })?;
                                validate_plan_ports(
                                    prepared,
                                    &expected.inputs,
                                    &invocation.inputs,
                                    subject_snapshot_id,
                                    subject_base_snapshot_id.as_deref(),
                                    subject_change_set_id.as_deref(),
                                )?;
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
                        EventType::AttemptDispatchedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptDispatched@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptDispatched@1 has no attempt ID".into())
                            })?;
                            let dispatch: review_core::event::AttemptDispatchedPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if plan.is_some_and(|plan| plan.budgeted) && dispatch.reserved.is_none()
                            {
                                return Err(StoreError::Conflict(
                                    "budgeted AttemptDispatched@1 has no reservation".into(),
                                ));
                            }
                            let provider: Option<String> = tx
                                .query_row(
                                    "SELECT payload FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2 AND node_id = ?3
                                       AND type = 'ProviderOperationTransition@1'
                                     ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, active_id, node],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            if let Some(provider) = provider {
                                let provider: review_core::ProviderOperationTransitionPayloadV1 =
                                    serde_json::from_str(&provider)?;
                                if provider.state != review_core::ProviderOperationStateV1::Done {
                                    return Err(StoreError::Conflict(format!(
                                        "attempt for node '{node}' dispatched without completed Provider Admission"
                                    )));
                                }
                            }
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2 AND attempt_id = ?3",
                                params![run_id, active_id, attempt],
                                |row| row.get(0),
                            )?;
                            if existing > 0
                                || batch_dispatches
                                    .insert(attempt.to_string(), node.to_string())
                                    .is_some()
                            {
                                return Err(StoreError::Conflict(format!(
                                    "attempt '{attempt}' was already dispatched"
                                )));
                            }
                            batch_latest_dispatch.insert(node.to_string(), attempt.to_string());
                        }
                        EventType::AttemptAdmittedV1
                        | EventType::AttemptFailedV1
                        | EventType::AttemptFencedV1
                        | EventType::AttemptReleasedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no node ID"))
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no attempt ID"))
                            })?;
                            let dispatched: Option<String> = tx
                                .query_row(
                                    "SELECT node_id FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2
                                       AND attempt_id = ?3 AND type = 'AttemptDispatched@1'
                                     LIMIT 1",
                                    params![run_id, active_id, attempt],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            let dispatched =
                                dispatched.or_else(|| batch_dispatches.get(attempt).cloned());
                            if dispatched.as_deref() != Some(node) {
                                return Err(StoreError::Conflict(format!(
                                    "{event_type} has no matching dispatch"
                                )));
                            }
                            let admitted = (event_type == EventType::AttemptAdmittedV1)
                                .then(|| {
                                    serde_json::from_value::<
                                        review_core::event::AttemptAdmittedPayloadV1,
                                    >(event.payload.clone())
                                })
                                .transpose()?;
                            let quarantined = admitted
                                .as_ref()
                                .is_some_and(|payload| payload.selection == "quarantined");
                            let existing_terminal: Option<String> = tx
                                .query_row(
                                    "SELECT type FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2 AND attempt_id = ?3
                                   AND type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                                'AttemptFenced@1', 'AttemptReleased@1')
                                 ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, active_id, attempt],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            let prior_terminal = batch_terminals
                                .get(attempt)
                                .map(|event_type| event_type.as_str())
                                .or(existing_terminal.as_deref());
                            if prior_terminal.is_some()
                                && !(quarantined
                                    && prior_terminal == Some(EventType::AttemptFencedV1.as_str()))
                            {
                                return Err(StoreError::Conflict(format!(
                                    "attempt '{attempt}' already has a terminal event"
                                )));
                            }
                            if !quarantined {
                                batch_terminals.insert(attempt.to_string(), event_type);
                                batch_terminal_nodes.insert(attempt.to_string(), node.to_string());
                            }
                            if plan.is_some_and(|plan| plan.budgeted) {
                                let settled = match event_type {
                                    EventType::AttemptFailedV1 => {
                                        serde_json::from_value::<
                                            review_core::event::AttemptFailedPayloadV1,
                                        >(
                                            event.payload.clone()
                                        )?
                                        .charged
                                    }
                                    EventType::AttemptFencedV1 => {
                                        serde_json::from_value::<
                                            review_core::event::AttemptFencedPayloadV1,
                                        >(
                                            event.payload.clone()
                                        )?
                                        .charged
                                    }
                                    EventType::AttemptReleasedV1 => {
                                        serde_json::from_value::<
                                            review_core::event::AttemptReleasedPayloadV1,
                                        >(
                                            event.payload.clone()
                                        )?
                                        .released
                                    }
                                    EventType::AttemptAdmittedV1 => Some(0),
                                    _ => unreachable!(),
                                };
                                if settled.is_none() {
                                    return Err(StoreError::Conflict(format!(
                                        "budgeted {event_type} has no settled accounting"
                                    )));
                                }
                            }
                            if event_type == EventType::AttemptFencedV1 {
                                pending_fences.remove(attempt);
                            }
                            if let Some(admitted) = admitted {
                                if admitted.selection == "selected" {
                                    let latest: Option<String> = tx
                                        .query_row(
                                            "SELECT attempt_id FROM events
                                             WHERE run_id = ?1 AND causation_id = ?2
                                               AND node_id = ?3 AND type = 'AttemptDispatched@1'
                                             ORDER BY sequence DESC LIMIT 1",
                                            params![run_id, active_id, node],
                                            |row| row.get(0),
                                        )
                                        .optional()?;
                                    let latest =
                                        batch_latest_dispatch.get(node).cloned().or(latest);
                                    if latest.as_deref() != Some(attempt) {
                                        return Err(StoreError::Conflict(
                                            "only the latest reviewer attempt may be selected"
                                                .into(),
                                        ));
                                    }
                                    let result = admitted.result_artifact.ok_or_else(|| {
                                        StoreError::Conflict(
                                            "selected AttemptAdmitted@1 has no result artifact"
                                                .into(),
                                        )
                                    })?;
                                    let provenance =
                                        admitted.provenance_artifact.ok_or_else(|| {
                                            StoreError::Conflict(
                                                "selected AttemptAdmitted@1 has no provenance artifact"
                                                    .into(),
                                            )
                                        })?;
                                    if !event.artifact_refs.contains(&result)
                                        || !event.artifact_refs.contains(&provenance)
                                    {
                                        return Err(StoreError::Conflict(
                                            "selected AttemptAdmitted@1 does not publish its result and provenance"
                                                .into(),
                                        ));
                                    }
                                    batch_selected
                                        .insert(attempt.to_string(), (node.to_string(), result));
                                }
                            }
                        }
                        EventType::AttemptInputV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptInput@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptInput@1 has no attempt ID".into())
                            })?;
                            let input: review_core::event::AttemptInputPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if !event.artifact_refs.contains(&input.refusal_history_id) {
                                return Err(StoreError::Conflict(
                                    "AttemptInput@1 does not reference its refusal history".into(),
                                ));
                            }
                            validate_prepared_refusal_history(
                                prepared,
                                &input.refusal_history_id,
                                "AttemptInput@1",
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'AttemptInput@1' AND node_id = ?3 AND attempt_id = ?4",
                                params![run_id, active_id, node, attempt],
                                |row| row.get(0),
                            )?;
                            if existing > 0
                                || batch_attempt_inputs
                                    .insert(attempt.to_string(), node.to_string())
                                    .is_some()
                            {
                                return Err(StoreError::Conflict(
                                    "attempt has duplicate durable input events".into(),
                                ));
                            }
                        }
                        EventType::AttemptFeedbackV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptFeedback@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptFeedback@1 has no attempt ID".into())
                            })?;
                            let feedback: review_core::event::AttemptFeedbackPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if !event.artifact_refs.contains(&feedback.refusal_history_id) {
                                return Err(StoreError::Conflict(
                                    "AttemptFeedback@1 does not reference its refusal history"
                                        .into(),
                                ));
                            }
                            validate_prepared_refusal_history(
                                prepared,
                                &feedback.refusal_history_id,
                                "AttemptFeedback@1",
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'AttemptFeedback@1' AND attempt_id = ?3",
                                params![run_id, active_id, attempt],
                                |row| row.get(0),
                            )?;
                            if existing > 0
                                || batch_attempt_feedback
                                    .insert(attempt.to_string(), node.to_string())
                                    .is_some()
                            {
                                return Err(StoreError::Conflict(
                                    "attempt has duplicate durable feedback events".into(),
                                ));
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
                                let expected = plan.nodes.get(node).ok_or_else(|| {
                                    StoreError::Conflict(format!(
                                        "node '{node}' is absent from the pinned Campaign plan"
                                    ))
                                })?;
                                validate_plan_ports(
                                    prepared,
                                    &expected.outputs,
                                    &receipt.outputs,
                                    subject_snapshot_id,
                                    subject_base_snapshot_id.as_deref(),
                                    subject_change_set_id.as_deref(),
                                )?;
                                if expected.kind == "reviewer" && event.attempt_id.is_none() {
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
                                let selected: Option<(String, String)> = tx
                                    .query_row(
                                        "SELECT node_id, payload FROM events
                                         WHERE run_id = ?1 AND causation_id = ?2
                                           AND attempt_id = ?3 AND type = 'AttemptAdmitted@1'
                                         LIMIT 1",
                                        params![run_id, active_id, attempt],
                                        |row| {
                                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                                        },
                                    )
                                    .optional()?
                                    .and_then(|(node, raw)| {
                                        serde_json::from_str::<
                                            review_core::event::AttemptAdmittedPayloadV1,
                                        >(&raw)
                                        .ok()
                                        .and_then(|payload| {
                                            (payload.selection == "selected")
                                                .then_some(payload.result_artifact)
                                                .flatten()
                                                .map(|result| (node, result))
                                        })
                                    })
                                    .or_else(|| batch_selected.get(attempt).cloned());
                                let Some((selected_node, result)) = selected else {
                                    return Err(StoreError::Conflict(
                                        "reviewer receipt has no selected admitted attempt".into(),
                                    ));
                                };
                                let outputs: Vec<&String> = receipt
                                    .outputs
                                    .iter()
                                    .flat_map(|port| &port.artifact_ids)
                                    .collect();
                                if selected_node != node
                                    || outputs.len() != 1
                                    || outputs[0] != &result
                                {
                                    return Err(StoreError::Conflict(
                                        "reviewer receipt contradicts its selected admitted result"
                                            .into(),
                                    ));
                                }
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
                                None if event.payload.get("imported").and_then(Value::as_bool)
                                    == Some(true)
                                    && event.artifact_refs.is_empty() => {}
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
                        _ => {}
                    }
                    if event_type.is_run_report() && report_closes(event_type, &event.payload)? {
                        if terminal {
                            return Err(StoreError::Conflict(
                                "the active Round epoch already has a terminal conclusion".into(),
                            ));
                        }
                        if event_type.run_report_requires_receipts() {
                            if let Some(plan) = plan {
                                validate_report_plan(plan, event_type, &event.payload)?;
                            }
                            validate_report_receipts(
                                tx,
                                run_id,
                                active_id,
                                event_type,
                                &event.payload,
                            )?;
                        }
                        terminal = true;
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
    for (attempt, node) in batch_attempt_inputs {
        if batch_dispatches.get(&attempt) != Some(&node) {
            return Err(StoreError::Conflict(
                "AttemptInput@1 must append atomically with its matching dispatch".into(),
            ));
        }
    }
    for (attempt, node) in batch_attempt_feedback {
        if batch_terminal_nodes.get(&attempt) != Some(&node)
            || !matches!(
                batch_terminals.get(&attempt),
                Some(EventType::AttemptFailedV1 | EventType::AttemptFencedV1)
            )
        {
            return Err(StoreError::Conflict(
                "AttemptFeedback@1 must append atomically with its matching failed or fenced attempt"
                    .into(),
            ));
        }
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

fn round_runtime_event(event_type: EventType) -> bool {
    event_type.is_run_report()
        || matches!(
            event_type,
            EventType::AttemptAdmittedV1
                | EventType::AttemptDispatchedV1
                | EventType::AttemptFeedbackV1
                | EventType::AttemptInputV1
                | EventType::AttemptFailedV1
                | EventType::AttemptFencedV1
                | EventType::AttemptReleasedV1
                | EventType::CheckCompletedV1
                | EventType::DemandRecordedV1
                | EventType::FindingReportedV1
                | EventType::GateDecisionV1
                | EventType::GenerationAdvancedV1
                | EventType::NodeInvocationV1
                | EventType::NodeOutputReceiptV1
                | EventType::ProviderOperationTransitionV1
        )
}

fn event_uses_authority_plan(event_type: EventType) -> bool {
    event_type.is_run_report()
        || matches!(
            event_type,
            EventType::NodeInvocationV1
                | EventType::AttemptDispatchedV1
                | EventType::NodeOutputReceiptV1
        )
}

fn latest_round(
    tx: &rusqlite::Transaction<'_>,
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

const ROUND_TERMINAL_REPORT_SQL: &str = "SELECT type, payload FROM events
     WHERE run_id = ?1 AND causation_id = ?2
       AND type >= 'RunReport@' AND type < 'RunReportA'
     ORDER BY sequence";

fn round_has_terminal_report(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
) -> Result<bool, StoreError> {
    let mut statement = tx.prepare(ROUND_TERMINAL_REPORT_SQL)?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (event_type, payload) = row?;
        let event_type = event_type
            .parse::<EventType>()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        if event_type.is_run_report()
            && report_closes(event_type, &serde_json::from_str(&payload)?)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_report_receipts(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    event_type: EventType,
    payload: &Value,
) -> Result<(), StoreError> {
    let outcomes = report_outcomes(event_type, payload)?;
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
            return Err(StoreError::Conflict(format!(
                "{event_type} has ambiguous durable output receipts"
            )));
        }
    }
    for outcome in outcomes {
        if let review_core::RunNodeOutcomeV2::Completed {
            mut output_artifacts,
        } = outcome.outcome
        {
            output_artifacts.sort();
            let receipt = receipts.remove(&outcome.node).ok_or_else(|| {
                StoreError::Conflict(format!(
                    "{event_type} completed node '{}' without a durable receipt",
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
                    "{event_type} contradicts the receipt for node '{}'",
                    outcome.node,
                )));
            }
        } else if receipts.contains_key(&outcome.node) {
            return Err(StoreError::Conflict(format!(
                "{event_type} suppresses or fails node '{}' after it published a receipt",
                outcome.node,
            )));
        }
    }
    if !receipts.is_empty() {
        return Err(StoreError::Conflict(format!(
            "{event_type} omits nodes with durable output receipts"
        )));
    }
    Ok(())
}

fn report_closes(event_type: EventType, payload: &Value) -> Result<bool, StoreError> {
    let event = RunEvent {
        event_id: String::new(),
        run_id: String::new(),
        sequence: 0,
        event_type,
        occurred_at: "1970-01-01T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs: Vec::new(),
        payload: payload.clone(),
    };
    review_core::run_report_closes_round(&event)
        .map_err(StoreError::Json)
        .map(Option::unwrap_or_default)
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
    format!("{:x}", hasher.finalize())[..26].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace_root() -> std::path::PathBuf {
        std::env::var_os("AFACTORY_WORKSPACE_ROOT")
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
    fn round_spend_uses_admitted_cost_first_terminal_and_every_epoch() {
        let (_dir, store, _cas) = fixture();
        for (sequence, event_id, epoch) in [(0, "round-old", 1), (1, "round-new", 2)] {
            store
                .conn
                .execute(
                    "INSERT INTO events
                     (run_id, sequence, event_id, type, occurred_at, node_id, attempt_id,
                      causation_id, correlation_id, artifact_refs, payload)
                     VALUES ('run', ?1, ?2, 'RoundStarted@1', '2026-08-28T00:00:00Z', NULL,
                             NULL, NULL, NULL, '[]', ?3)",
                    params![
                        sequence,
                        event_id,
                        json!({
                            "round": 1,
                            "epoch": epoch,
                            "campaign_manifest_id": "manifest",
                            "subject_id": format!("subject-{epoch}"),
                            "prior_finding_set_id": "findings",
                            "prior_demand_set_id": "demands"
                        })
                        .to_string()
                    ],
                )
                .unwrap();
        }
        let mut sequence = 2_i64;
        let insert = |store: &EventStore,
                      sequence: &mut i64,
                      event_type: &str,
                      attempt_id: Option<&str>,
                      correlation_id: Option<&str>,
                      payload: serde_json::Value| {
            let causation_id = if matches!(attempt_id, Some("selected" | "fenced")) {
                "round-old"
            } else {
                "round-new"
            };
            store
                .conn
                .execute(
                    "INSERT INTO events
                     (run_id, sequence, event_id, type, occurred_at, node_id, attempt_id,
                      causation_id, correlation_id, artifact_refs, payload)
                     VALUES ('run', ?1, ?2, ?3, '2026-08-28T00:00:00Z', 'reviewer', ?4,
                             ?5, ?6, '[]', ?7)",
                    params![
                        *sequence,
                        format!("event-{sequence}"),
                        event_type,
                        attempt_id,
                        causation_id,
                        correlation_id,
                        payload.to_string()
                    ],
                )
                .unwrap();
            *sequence += 1;
        };

        insert(
            &store,
            &mut sequence,
            "AttemptDispatched@1",
            Some("selected"),
            None,
            json!({"reserved": 100, "prior_findings": null}),
        );
        insert(
            &store,
            &mut sequence,
            "AttemptAdmitted@1",
            Some("selected"),
            None,
            json!({"selection": "selected", "cost_tokens": 31}),
        );
        insert(
            &store,
            &mut sequence,
            "AttemptDispatched@1",
            Some("fenced"),
            None,
            json!({"reserved": 50, "prior_findings": null}),
        );
        insert(
            &store,
            &mut sequence,
            "AttemptFenced@1",
            Some("fenced"),
            None,
            json!({"reason": "deadline", "charged": 11}),
        );
        insert(
            &store,
            &mut sequence,
            "AttemptAdmitted@1",
            Some("fenced"),
            None,
            json!({"selection": "quarantined", "cost_tokens": 11}),
        );
        insert(
            &store,
            &mut sequence,
            "AttemptDispatched@1",
            Some("released"),
            None,
            json!({"reserved": 20, "prior_findings": null}),
        );
        insert(
            &store,
            &mut sequence,
            "AttemptReleased@1",
            Some("released"),
            None,
            json!({"error": "not run", "released": 20}),
        );
        insert(
            &store,
            &mut sequence,
            "AttemptDispatched@1",
            Some("running"),
            None,
            json!({"reserved": 7, "prior_findings": null}),
        );
        insert(
            &store,
            &mut sequence,
            "ProviderOperationTransition@1",
            None,
            Some("failed-provider"),
            json!({"state": "failed", "charged_tokens": 2, "reserved_tokens": 5}),
        );
        insert(
            &store,
            &mut sequence,
            "ProviderOperationTransition@1",
            None,
            Some("running-provider"),
            json!({"state": "running", "charged_tokens": 0, "reserved_tokens": 5}),
        );

        assert_eq!(
            store.round_committed_tokens("run", "round-new").unwrap(),
            56
        );
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

        assert!(validate_reviewer_result(&result(legacy)).is_ok());
        assert!(validate_reviewer_result(&result(typed)).is_err());
    }

    #[test]
    fn reviewer_result_legacy_conformance_corpus_matches_durable_reader() {
        let path = workspace_root().join("schemas/reviewer-result-v1-conformance.json");
        let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for case in corpus["valid"].as_array().unwrap() {
            assert!(
                validate_reviewer_result(&case["payload"]).is_ok(),
                "{}",
                case["name"]
            );
        }
        for case in corpus["invalid"].as_array().unwrap() {
            assert!(
                validate_reviewer_result(&case["payload"]).is_err(),
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
