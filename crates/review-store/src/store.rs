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

use review_core::{EventType, RunEvent};
use rusqlite::{Connection, OptionalExtension, params};
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
}

impl EventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
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
        Ok(Self { conn })
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
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let mut has_artifacts = false;
        for event in events {
            review_core::json::admit(&event.payload)
                .map_err(|error| StoreError::Conflict(format!("invalid event payload: {error}")))?;
            review_core::event::validate_event_payload(event.event_type, &event.payload)
                .map_err(|error| StoreError::Conflict(format!("invalid event payload: {error}")))?;
            for digest in &event.artifact_refs {
                has_artifacts = true;
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
        if has_artifacts {
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
        validate_campaign_transition(&tx, run_id, events, first)?;
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

    /// Every event of a run, in sequence order. This is the only read replay needs.
    pub fn replay(&self, run_id: &str) -> Result<Vec<RunEvent>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, type, occurred_at, node_id, attempt_id,
                    causation_id, correlation_id, artifact_refs, payload
             FROM events WHERE run_id = ?1 ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
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
        for (expected_sequence, row) in (0u64..).zip(rows) {
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

fn validate_campaign_transition(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    events: &[NewEvent],
    first_sequence: i64,
) -> Result<(), StoreError> {
    let campaign_opened: i64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1'",
        params![run_id],
        |row| row.get(0),
    )?;
    let mut opened = campaign_opened > 0;
    let mut active = latest_round(tx, run_id)?;
    let mut terminal = match &active {
        Some((event_id, _)) => round_has_terminal_report(tx, run_id, event_id)?,
        None => false,
    };
    let mut pending_supersession: Option<review_core::RoundInputSupersededPayloadV1> = None;

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
                     ) AND type = 'FindingReported@1'",
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
                terminal = false;
            }
            event_type if round_runtime_event(event_type) => {
                if let Some((active_id, _)) = &active {
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
                    if matches!(event_type, EventType::RunReportV1 | EventType::RunReportV2)
                        && report_closes(event_type, &event.payload)?
                    {
                        if terminal {
                            return Err(StoreError::Conflict(
                                "the active Round epoch already has a terminal conclusion".into(),
                            ));
                        }
                        if event_type == EventType::RunReportV2 {
                            validate_report_receipts(tx, run_id, active_id, &event.payload)?;
                        }
                        terminal = true;
                    }
                }
            }
            EventType::FindingResolvedV1 => {
                if let (Some(causation), Some((active_id, _))) =
                    (event.causation_id.as_deref(), &active)
                    && causation != active_id
                {
                    return Err(StoreError::Conflict(
                        "FindingResolved@1 is bound to a stale Round epoch".into(),
                    ));
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

fn round_runtime_event(event_type: EventType) -> bool {
    matches!(
        event_type,
        EventType::AttemptAdmittedV1
            | EventType::AttemptDispatchedV1
            | EventType::AttemptFailedV1
            | EventType::AttemptFencedV1
            | EventType::AttemptReleasedV1
            | EventType::CheckCompletedV1
            | EventType::FindingReportedV1
            | EventType::GateDecisionV1
            | EventType::GenerationAdvancedV1
            | EventType::NodeInvocationV1
            | EventType::NodeOutputReceiptV1
            | EventType::RunReportV1
            | EventType::RunReportV2
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

fn round_has_terminal_report(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
) -> Result<bool, StoreError> {
    let mut statement = tx.prepare(
        "SELECT type, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type IN ('RunReport@1', 'RunReport@2')
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (event_type, payload) = row?;
        let event_type = event_type
            .parse::<EventType>()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        if report_closes(event_type, &serde_json::from_str(&payload)?)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_report_receipts(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    payload: &Value,
) -> Result<(), StoreError> {
    let report: review_core::RunReportPayloadV2 = serde_json::from_value(payload.clone())?;
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
                "RunReport@2 has ambiguous durable output receipts".into(),
            ));
        }
    }
    for outcome in report.outcomes {
        if let review_core::RunNodeOutcomeV2::Completed {
            mut output_artifacts,
        } = outcome.outcome
        {
            output_artifacts.sort();
            let receipt = receipts.remove(&outcome.node).ok_or_else(|| {
                StoreError::Conflict(format!(
                    "RunReport@2 completed node '{}' without a durable receipt",
                    outcome.node
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
                    "RunReport@2 contradicts the receipt for node '{}'",
                    outcome.node
                )));
            }
        } else if receipts.contains_key(&outcome.node) {
            return Err(StoreError::Conflict(format!(
                "RunReport@2 suppresses or fails node '{}' after it published a receipt",
                outcome.node
            )));
        }
    }
    if !receipts.is_empty() {
        return Err(StoreError::Conflict(
            "RunReport@2 omits nodes with durable output receipts".into(),
        ));
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

    fn fixture() -> (tempfile::TempDir, EventStore, Cas) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        (dir, store, cas)
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
