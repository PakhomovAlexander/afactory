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
        validate_campaign_transition(
            &tx,
            cas,
            run_id,
            events,
            first,
            &mut self.validated_change_sets,
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

    /// Committed and crash-reserved token spend for one exact Round, without replaying
    /// unrelated Campaign events or re-reading output artifacts from the CAS.
    pub fn round_committed_tokens(
        &self,
        run_id: &str,
        round_event_id: &str,
    ) -> Result<u64, StoreError> {
        let sum = |query: &str| -> Result<u64, StoreError> {
            let value: i64 =
                self.conn
                    .query_row(query, params![run_id, round_event_id], |row| row.get(0))?;
            u64::try_from(value)
                .map_err(|_| StoreError::Conflict("replayed token charge overflow".into()))
        };
        let terminal_attempts = sum(
            "SELECT COALESCE(SUM(CAST(json_extract(payload, '$.charged') AS INTEGER)), 0)
             FROM events WHERE run_id = ?1 AND causation_id = ?2
               AND type IN ('AttemptAdmitted@1', 'AttemptFailed@1', 'AttemptFenced@1')",
        )?;
        let outstanding_attempts = sum(
            "SELECT COALESCE(SUM(CAST(json_extract(dispatched.payload, '$.reserved') AS INTEGER)), 0)
             FROM events AS dispatched
             WHERE dispatched.run_id = ?1 AND dispatched.causation_id = ?2
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
            "SELECT COALESCE(SUM(CAST(json_extract(payload, '$.charged_tokens') AS INTEGER)), 0)
             FROM events WHERE run_id = ?1 AND causation_id = ?2
               AND type = 'ProviderOperationTransition@1'",
        )?;
        let outstanding_providers = sum(
            "SELECT COALESCE(SUM(CAST(json_extract(operation.payload, '$.reserved_tokens') AS INTEGER)), 0)
             FROM events AS operation
             WHERE operation.run_id = ?1 AND operation.causation_id = ?2
               AND operation.type = 'ProviderOperationTransition@1'
               AND json_extract(operation.payload, '$.state') = 'running'
               AND json_type(operation.payload, '$.failure_class') IS NULL
               AND operation.sequence = (
                 SELECT MAX(latest.sequence) FROM events AS latest
                 WHERE latest.run_id = operation.run_id
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
            Self::Name(_) => "review.kernel/Opaque@1",
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

fn validate_plan_ports(
    cas: &Cas,
    validated_change_sets: &mut std::collections::BTreeMap<String, Arc<review_core::ChangeSetV1>>,
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
            if let Some(change_set) = validate_artifact_payload(
                cas,
                validated_change_sets,
                &port.artifact_type,
                artifact,
            )? {
                validated_change_set = Some(change_set);
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
    cas: &Cas,
    validated_change_sets: &mut std::collections::BTreeMap<String, Arc<review_core::ChangeSetV1>>,
    artifact_type: &str,
    artifact_id: &str,
) -> Result<Option<Arc<review_core::ChangeSetV1>>, StoreError> {
    if artifact_type == "review.kernel/Opaque@1" {
        cas.get(artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        return Ok(None);
    }
    if artifact_type == review_core::contract::CHANGE_SET_V1
        && let Some(change_set) = validated_change_sets.get(artifact_id)
    {
        // The cache removes JSON/base64 reconstruction only. Integrity is still re-established
        // from the current object bytes for every reference.
        cas.verify(artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        return Ok(Some(Arc::clone(change_set)));
    }
    let value = cas
        .get_json(artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let object = value.as_object().ok_or_else(|| {
        StoreError::Conflict(format!("{artifact_type} artifact is not a JSON object"))
    })?;
    match artifact_type {
        review_core::contract::CHANGE_SET_V1 => {
            let change_set: review_core::ChangeSetV1 = serde_json::from_value(value)
                .map_err(|error| StoreError::Conflict(error.to_string()))?;
            change_set.validate().map_err(StoreError::Conflict)?;
            let change_set = Arc::new(change_set);
            validated_change_sets.insert(artifact_id.to_string(), Arc::clone(&change_set));
            return Ok(Some(change_set));
        }
        "review.kernel/GateDecision@1" => {
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
        "review.kernel/PriorFindings@1" => {
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
        "review.kernel/ReviewerResult@1" => validate_reviewer_result(&value)?,
        "review.kernel/ReportSet@1" => {
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
        "review.kernel/FindingSet@1" => {
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
        _ => {
            return Err(StoreError::Conflict(format!(
                "no payload validator is registered for {artifact_type}"
            )));
        }
    }
    Ok(None)
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
    validated_change_sets: &mut std::collections::BTreeMap<String, Arc<review_core::ChangeSetV1>>,
) -> Result<(), StoreError> {
    let campaign_opened: i64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1'",
        params![run_id],
        |row| row.get(0),
    )?;
    let mut opened = campaign_opened > 0;
    let mut authority_plan = if opened {
        Some(load_authority_plan(tx, cas, run_id)?)
    } else {
        None
    };
    let mut active = latest_round(tx, run_id)?;
    let mut terminal = match &active {
        Some((event_id, _)) => round_has_terminal_report(tx, run_id, event_id)?,
        None => false,
    };
    let mut pending_supersession: Option<review_core::RoundInputSupersededPayloadV1> = None;
    let mut pending_fences = std::collections::BTreeSet::new();
    let mut batch_dispatches = std::collections::BTreeMap::new();
    let mut batch_latest_dispatch = std::collections::BTreeMap::new();
    let mut batch_attempt_inputs = std::collections::BTreeMap::new();
    let mut batch_terminals: std::collections::BTreeMap<String, EventType> =
        std::collections::BTreeMap::new();
    let mut batch_selected = std::collections::BTreeMap::new();
    let mut batch_invocations = std::collections::BTreeSet::new();
    let mut batch_receipts = std::collections::BTreeSet::new();
    let mut batch_findings = std::collections::BTreeSet::new();
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
                    let subject: review_core::SubjectV1 = serde_json::from_value(
                        cas.get_json(&active_payload.subject_id)
                            .map_err(|error| StoreError::Conflict(error.to_string()))?,
                    )?;
                    let subject_snapshot_id = subject.head_snapshot_id;
                    let subject_base_snapshot_id = subject.base_snapshot_id;
                    let subject_change_set_id = subject.change_set_id;
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
                                    cas,
                                    validated_change_sets,
                                    &expected.inputs,
                                    &invocation.inputs,
                                    &subject_snapshot_id,
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
                                    cas,
                                    validated_change_sets,
                                    &expected.outputs,
                                    &receipt.outputs,
                                    &subject_snapshot_id,
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
    Ok(())
}

fn round_runtime_event(event_type: EventType) -> bool {
    event_type.is_run_report()
        || matches!(
            event_type,
            EventType::AttemptAdmittedV1
                | EventType::AttemptDispatchedV1
                | EventType::AttemptInputV1
                | EventType::AttemptFailedV1
                | EventType::AttemptFencedV1
                | EventType::AttemptReleasedV1
                | EventType::CheckCompletedV1
                | EventType::FindingReportedV1
                | EventType::GateDecisionV1
                | EventType::GenerationAdvancedV1
                | EventType::NodeInvocationV1
                | EventType::NodeOutputReceiptV1
                | EventType::ProviderOperationTransitionV1
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
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/reviewer-result-v1-conformance.json");
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
        let mut cache = std::collections::BTreeMap::new();

        assert!(
            validate_artifact_payload(
                &cas,
                &mut cache,
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
        let error = validate_artifact_payload(
            &cas,
            &mut cache,
            review_core::contract::CHANGE_SET_V1,
            &artifact_id,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("does not match its digest"),
            "{error}"
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
