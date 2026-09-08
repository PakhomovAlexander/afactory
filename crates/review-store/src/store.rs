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

mod artifacts;
mod authority;
mod integration;
mod proposals;
mod reports;
mod transition;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use review_core::{EventType, RunEvent};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use crate::cas::{Cas, CasError};
use crate::store::artifacts::typed_json_artifacts;
use crate::store::reports::report_closes;
use crate::store::transition::validate_campaign_transition;

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
    /// A Broker completion lost the atomic race with Attempt fencing or replacement.
    AttemptNotCurrent,
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
            StoreError::AttemptNotCurrent => {
                write!(
                    f,
                    "event store conflict: Broker Attempt is no longer current"
                )
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
/// This is a **sidecar**, not an event: it carries no identity, and nothing in replay, the
/// Ledger, or convergence reads it — the event stream stays byte-for-byte deterministic. It
/// exists so a person can see how long a review took and what it consumed, through
/// `af review report`, `af review campaigns`, and `af review ledger`. An absent row means "not
/// recorded", never "zero".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttemptWall {
    pub run_id: String,
    pub attempt_id: String,
    pub node_id: String,
    pub round: u32,
    pub epoch: u32,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AttemptUsage>,
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
        #[derive(Default)]
        struct AttemptCommitment {
            dispatched: u64,
            broker_authority: u64,
            broker_observed: u64,
            terminal: Option<u64>,
        }
        let mut attempts: BTreeMap<String, AttemptCommitment> = BTreeMap::new();
        let mut statement = self.conn.prepare(
            "SELECT event.type, event.attempt_id, event.payload
             FROM events AS event
             WHERE event.run_id = ?1 AND event.attempt_id IS NOT NULL
               AND event.type IN ('AttemptDispatched@1', 'ReviewerExecutionBound@1',
                                  'BrokerOperationCompleted@1', 'AttemptAdmitted@1',
                                  'AttemptFailed@1', 'AttemptFenced@1', 'AttemptReleased@1')
               AND EXISTS (
                 SELECT 1 FROM events AS round
                 WHERE round.run_id = event.run_id
                   AND round.event_id = event.causation_id
                   AND round.type = 'RoundStarted@1'
                   AND json_extract(round.payload, '$.round') = ?2
                   AND json_extract(round.payload, '$.campaign_manifest_id') = ?3
               )
             ORDER BY event.sequence",
        )?;
        let rows = statement.query_map(
            params![run_id, round_number, round.campaign_manifest_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        for row in rows {
            let (event_type, attempt_id, payload) = row?;
            let event_type: EventType =
                event_type
                    .parse()
                    .map_err(|error: review_core::UnknownEventType| {
                        StoreError::Conflict(error.to_string())
                    })?;
            let commitment = attempts.entry(attempt_id).or_default();
            match event_type {
                EventType::AttemptDispatchedV1 => {
                    let payload: review_core::event::AttemptDispatchedPayloadV1 =
                        serde_json::from_str(&payload)?;
                    commitment.dispatched = payload.reserved.unwrap_or(0);
                }
                EventType::ReviewerExecutionBoundV1 => {
                    let binding: review_core::ReviewerExecutionBindingV1 =
                        serde_json::from_str(&payload)?;
                    commitment.broker_authority =
                        review_core::broker_authority_usage(&binding.operations)
                            .map_err(StoreError::Conflict)?;
                }
                EventType::BrokerOperationCompletedV1 => {
                    let receipt: review_core::BrokerOperationReceiptV1 =
                        serde_json::from_str(&payload)?;
                    commitment.broker_observed = commitment
                        .broker_observed
                        .checked_add(receipt.charged_usage)
                        .ok_or_else(|| {
                            StoreError::Conflict("Broker Attempt usage overflow".into())
                        })?;
                }
                EventType::AttemptAdmittedV1 if commitment.terminal.is_none() => {
                    commitment.terminal = Some(
                        serde_json::from_str::<review_core::event::AttemptAdmittedPayloadV1>(
                            &payload,
                        )?
                        .cost_tokens,
                    );
                }
                EventType::AttemptFailedV1 if commitment.terminal.is_none() => {
                    commitment.terminal = Some(
                        serde_json::from_str::<review_core::event::AttemptFailedPayloadV1>(
                            &payload,
                        )?
                        .charged
                        .unwrap_or(0),
                    );
                }
                EventType::AttemptFencedV1 if commitment.terminal.is_none() => {
                    commitment.terminal = Some(
                        serde_json::from_str::<review_core::event::AttemptFencedPayloadV1>(
                            &payload,
                        )?
                        .charged
                        .unwrap_or(0),
                    );
                }
                EventType::AttemptReleasedV1 if commitment.terminal.is_none() => {
                    commitment.terminal = Some(0);
                }
                _ => {}
            }
        }
        let attempt_charges = attempts.into_values().try_fold(0_u64, |sum, attempt| {
            let charged = attempt.terminal.map_or_else(
                || {
                    attempt
                        .dispatched
                        .max(attempt.broker_authority)
                        .max(attempt.broker_observed)
                },
                |settled| settled.max(attempt.broker_observed),
            );
            sum.checked_add(charged)
                .ok_or_else(|| StoreError::Conflict("replayed token charge overflow".into()))
        })?;
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
        attempt_charges
            .checked_add(provider_charges)
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

    /// Records the wall-clock sidecar for one Attempt. A second write for the same Attempt
    /// replaces the first, so a retried write cannot double-count.
    pub fn record_attempt_wall(&self, wall: &AttemptWall) -> Result<(), StoreError> {
        fn bounded(value: u64, what: &str) -> Result<i64, StoreError> {
            i64::try_from(value).map_err(|_| {
                StoreError::Conflict(format!("attempt wall {what} exceeds SQLite range"))
            })
        }
        fn optional(value: Option<u64>, what: &str) -> Result<Option<i64>, StoreError> {
            value.map(|value| bounded(value, what)).transpose()
        }
        let (input, output, cache_read, cache_write, reasoning, chargeable) = match &wall.usage {
            Some(usage) => (
                optional(usage.input_tokens, "input tokens")?,
                optional(usage.output_tokens, "output tokens")?,
                optional(usage.cache_read_tokens, "cache-read tokens")?,
                optional(usage.cache_write_tokens, "cache-write tokens")?,
                optional(usage.reasoning_tokens, "reasoning tokens")?,
                Some(bounded(usage.chargeable_tokens, "chargeable tokens")?),
            ),
            None => (None, None, None, None, None, None),
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO attempt_wall (
                 run_id, attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                 reasoning_tokens, chargeable_tokens
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                wall.run_id,
                wall.attempt_id,
                wall.node_id,
                i64::from(wall.round),
                i64::from(wall.epoch),
                bounded(wall.started_unix_ms, "start")?,
                bounded(wall.elapsed_ms, "elapsed")?,
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
                chargeable,
            ],
        )?;
        Ok(())
    }

    /// The recorded wall-clock rows of a run, oldest first. A store written before the sidecar
    /// existed has no table; that reads as no rows, not as an error.
    pub fn attempt_wall(&self, run_id: &str) -> Result<Vec<AttemptWall>, StoreError> {
        let present: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'attempt_wall'",
            [],
            |row| row.get(0),
        )?;
        if present == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens, chargeable_tokens
             FROM attempt_wall WHERE run_id = ?1
             ORDER BY started_unix_ms, attempt_id",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
            let optional = |value: Option<i64>| value.map(unsigned);
            let chargeable: Option<i64> = row.get(11)?;
            Ok(AttemptWall {
                run_id: run_id.to_string(),
                attempt_id: row.get(0)?,
                node_id: row.get(1)?,
                round: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                epoch: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                started_unix_ms: unsigned(row.get(4)?),
                elapsed_ms: unsigned(row.get(5)?),
                usage: chargeable.map(|chargeable| AttemptUsage {
                    input_tokens: optional(row.get(6).ok().flatten()),
                    output_tokens: optional(row.get(7).ok().flatten()),
                    cache_read_tokens: optional(row.get(8).ok().flatten()),
                    cache_write_tokens: optional(row.get(9).ok().flatten()),
                    reasoning_tokens: optional(row.get(10).ok().flatten()),
                    chargeable_tokens: unsigned(chargeable),
                }),
            })
        })?;
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

pub fn validate_reviewer_result(value: &Value) -> Result<(), StoreError> {
    review_core::validate_reviewer_result(value).map_err(StoreError::Conflict)
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
    use crate::store::artifacts::validate_artifact_payload;
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
            "AttemptDispatched@1",
            Some("broker-running"),
            None,
            json!({"reserved": null, "prior_findings": null}),
        );
        insert(
            &store,
            &mut sequence,
            "ReviewerExecutionBound@1",
            Some("broker-running"),
            None,
            json!({
                "node": "reviewer",
                "attempt_id": "broker-running",
                "lease_epoch": 1,
                "credential_mode": "brokered",
                "auto_apply": false,
                "broker_handle": "bbbbbbbbbbbbbbbbbbbbbbbbbb",
                "operations": [{
                    "name": "model_inference",
                    "destination": "provider.test",
                    "method": "responses.create",
                    "max_request_bytes": 32,
                    "max_response_bytes": 32,
                    "max_calls": 1,
                    "max_usage": 100
                }],
                "admitted": true
            }),
        );
        insert(
            &store,
            &mut sequence,
            "BrokerOperationCompleted@1",
            Some("broker-running"),
            None,
            json!({
                "handle_id": "bbbbbbbbbbbbbbbbbbbbbbbbbb",
                "node": "reviewer",
                "attempt_id": "broker-running",
                "lease_epoch": 1,
                "operation": "model_inference",
                "destination": "provider.test",
                "method": "responses.create",
                "ordinal": 1,
                "outcome": "succeeded",
                "request_digest": format!("sha256:{}", "c".repeat(64)),
                "response_digest": format!("sha256:{}", "d".repeat(64)),
                "request_bytes": 7,
                "response_bytes": 8,
                "reserved_usage": 10,
                "charged_usage": 7
            }),
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
            156
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
