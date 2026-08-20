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

use crate::cas::Cas;

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
                 ON events (run_id, correlation_id);",
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
        review_core::json::admit(&event.payload)
            .map_err(|error| StoreError::Conflict(format!("invalid event payload: {error}")))?;
        for digest in &event.artifact_refs {
            if !cas.contains(digest) {
                return Err(StoreError::DanglingArtifact {
                    digest: digest.clone(),
                });
            }
        }
        if !event.artifact_refs.is_empty() {
            cas.flush()
                .map_err(|e| StoreError::Durability(e.to_string()))?;
        }

        let tx = self.conn.transaction()?;
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence) + 1, 0) FROM events WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);

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
        tx.commit()?;

        Ok(RunEvent {
            event_id,
            run_id: run_id.to_string(),
            sequence: next as u64,
            event_type: event.event_type,
            occurred_at: event.occurred_at,
            node_id: event.node_id,
            attempt_id: event.attempt_id,
            causation_id: event.causation_id,
            correlation_id: event.correlation_id,
            artifact_refs: event.artifact_refs,
            payload: event.payload,
        })
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
            Ok((
                RunEvent {
                    event_id: row.get(0)?,
                    run_id: run_id.to_string(),
                    sequence: row.get::<_, i64>(1)? as u64,
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
                refs,
                payload,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            // A row that does not parse is refused, never degraded: replaying it as an empty
            // event would rebuild a different state than the run committed, silently — the
            // exact failure the publication ordering exists to prevent, on the read side.
            let (mut event, refs, payload) = row?;
            event.artifact_refs = serde_json::from_str(&refs).map_err(StoreError::Json)?;
            event.payload = serde_json::from_str(&payload).map_err(StoreError::Json)?;
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
