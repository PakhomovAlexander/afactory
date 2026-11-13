//! Bounded notification control state in the Store's FULL-synchronous transaction boundary.
//! No row here is an execution event, a route grant, or a delivery receipt.
use super::*;
use crate::content_id;
use af_contracts::task::wake::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub mod delivery;

pub const MAX_SUBSCRIPTIONS: u64 = 128;
pub const MAX_ENVELOPES: u64 = 4096;
pub const MAX_CONTROL_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_SUBSCRIPTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

fn refusal(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeRegistration {
    pub schema: String,
    pub subscription: WakeSubscriptionV1,
    /// Explicit catch-up from opening, bounded by max_observations. No implicit history scope.
    pub policy: CatchUp,
    pub registered_prefix: WakeBoundaryV1,
    /// A retained intent is never authority for the notification driver to launch a process.
    pub launch_intent: bool,
}
impl WakeRegistration {
    pub fn validate(&self) -> Result<(), String> {
        self.subscription.validate()?;
        self.registered_prefix.validate()?;
        if self.schema != "af/task-wake-registration@1"
            || self.subscription.expires_unix_ms - self.subscription.registered_unix_ms
                > MAX_SUBSCRIPTION_MS
            || self.subscription.configuration_digest != configuration_digest(&self.subscription)?
        {
            return Err("Invalid wake registration version, lifetime or configuration".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchUp {
    FromOpening,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeJournal {
    pub registration: WakeRegistration,
    pub checkpoint: Option<String>,
    pub extracted_through: Option<u64>,
    pub source_available: bool,
    pub envelopes: Vec<WakeEnvelopeV1>,
    pub closure: Option<String>,
    pub expired: bool,
}

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS wake_incarnation_v1 (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), identity TEXT NOT NULL, owner TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS wake_subscriptions_v1 (
 id TEXT PRIMARY KEY, registration_key TEXT UNIQUE NOT NULL, task_id TEXT NOT NULL,
 registration TEXT NOT NULL, checkpoint TEXT, checkpoint_digest TEXT, through_sequence INTEGER,
 source_sequence INTEGER, expires INTEGER NOT NULL, valid INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS wake_subscriptions_task_v1 ON wake_subscriptions_v1(task_id);
CREATE TABLE IF NOT EXISTS wake_outbox_v1 (
 delivery_id TEXT PRIMARY KEY, subscription_id TEXT NOT NULL, digest TEXT NOT NULL,
 envelope TEXT NOT NULL, expires INTEGER NOT NULL,
 FOREIGN KEY(subscription_id) REFERENCES wake_subscriptions_v1(id)
);
CREATE TABLE IF NOT EXISTS wake_routes_v1 (
 id TEXT PRIMARY KEY, route TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, expires INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS wake_delivery_v1 (
 id TEXT PRIMARY KEY, subscription_id TEXT NOT NULL, route_id TEXT NOT NULL, record TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS wake_closed_v1 (
 subscription_id TEXT PRIMARY KEY, reason TEXT NOT NULL, closed INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS wake_session_fences_v1 (
 session_key TEXT PRIMARY KEY, subscription_id TEXT NOT NULL, delivery_id TEXT NOT NULL,
 digest TEXT NOT NULL, intent_id TEXT NOT NULL, expires INTEGER NOT NULL,
 task_id TEXT NOT NULL, causal_sequence INTEGER NOT NULL, causal_event_id TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS wake_session_stores_v1 (
 session_key TEXT PRIMARY KEY, store_incarnation TEXT NOT NULL, binding_digest TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS wake_source_updated_v1 AFTER UPDATE ON events BEGIN
 UPDATE wake_subscriptions_v1 SET valid=0
 WHERE json_extract(registration,'$.subscription.task.task_run_id')=OLD.run_id;
END;
CREATE TRIGGER IF NOT EXISTS wake_source_deleted_v1 AFTER DELETE ON events BEGIN
 UPDATE wake_subscriptions_v1 SET valid=0
 WHERE json_extract(registration,'$.subscription.task.task_run_id')=OLD.run_id;
END;
CREATE TRIGGER IF NOT EXISTS wake_source_inserted_v1 BEFORE INSERT ON events BEGIN
 UPDATE wake_subscriptions_v1 SET valid=0
 WHERE json_extract(registration,'$.subscription.task.task_run_id')=NEW.run_id
 AND EXISTS(SELECT 1 FROM events WHERE run_id=NEW.run_id AND sequence=NEW.sequence);
END;";

fn owner(store: &EventStore) -> Result<String, StoreError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if !cfg!(target_os = "linux") {
        return Err(refusal(
            "Wake Store incarnation admission currently requires Linux machine identity",
        ));
    }
    let machine = crate::local_fs::read_bounded(std::path::Path::new("/etc/machine-id"), 128)
        .map_err(|e| refusal(&e.to_string()))?;
    let machine = std::str::from_utf8(&machine)
        .map_err(|e| refusal(&e.to_string()))?
        .trim();
    if machine.len() != 32 || !machine.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(refusal("Invalid machine identity for wake Store admission"));
    }

    let path = store
        .database_path()
        .ok_or_else(|| refusal("Wake journal requires a durable Store"))?;
    let parent = path.parent().ok_or_else(|| refusal("Invalid Store root"))?;
    for (path, directory) in [(parent, true), (path.as_path(), false)] {
        let info = std::fs::symlink_metadata(path).map_err(|e| refusal(&e.to_string()))?;
        if info.file_type().is_symlink()
            || info.is_dir() != directory
            || (!directory && (!info.is_file() || info.nlink() != 1))
            || info.uid() != nix::unistd::geteuid().as_raw()
            || info.permissions().mode() & 0o077 != 0
            || path.canonicalize().map_err(|e| refusal(&e.to_string()))? != path
        {
            return Err(refusal("Wake Store ownership, path or permissions refused"));
        }
    }
    let info = std::fs::symlink_metadata(&path).map_err(|e| refusal(&e.to_string()))?;
    // Same-filesystem relocation preserves this binding. A restored/copied database is not
    // independently activated. Rebinding a copy is intentionally unavailable.
    crate::content_id(&serde_json::json!({"machine":machine,"uid":info.uid(),"device":info.dev(),"inode":info.ino()}))
        .map_err(|e| refusal(&e.to_string()))
}
fn capacity(conn: &Connection) -> Result<(), StoreError> {
    let (subscriptions, envelopes, bytes): (u64, u64, u64) = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM wake_subscriptions_v1),
         (SELECT COUNT(*) FROM wake_outbox_v1),
         COALESCE((SELECT SUM(length(registration)+COALESCE(length(checkpoint),0)+COALESCE(length(checkpoint_digest),0)) FROM wake_subscriptions_v1),0)
         + COALESCE((SELECT SUM(length(envelope)) FROM wake_outbox_v1),0)", [],
        |row| Ok((u64_column(row,0)?,u64_column(row,1)?,u64_column(row,2)?)))?;
    let (routes, deliveries, fences, extra): (u64, u64, u64, u64) = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM wake_routes_v1), (SELECT COUNT(*) FROM wake_delivery_v1),
        (SELECT COUNT(*) FROM wake_session_fences_v1),
        COALESCE((SELECT SUM(length(route)) FROM wake_routes_v1),0)
        + COALESCE((SELECT SUM(length(record)) FROM wake_delivery_v1),0)
        + (SELECT COUNT(*)*1024 FROM wake_session_fences_v1)
        + (SELECT COUNT(*)*512 FROM wake_session_stores_v1)",
        [],
        |r| {
            Ok((
                u64_column(r, 0)?,
                u64_column(r, 1)?,
                u64_column(r, 2)?,
                u64_column(r, 3)?,
            ))
        },
    )?;
    let bindings: u64 = conn.query_row("SELECT COUNT(*) FROM wake_session_stores_v1", [], |r| {
        u64_column(r, 0)
    })?;
    if bindings > MAX_ENVELOPES
        || subscriptions > MAX_SUBSCRIPTIONS
        || envelopes > MAX_ENVELOPES
        || routes > MAX_SUBSCRIPTIONS
        || deliveries > MAX_ENVELOPES
        || fences > MAX_ENVELOPES
        || bytes + extra > MAX_CONTROL_BYTES
    {
        return Err(refusal(
            "Wake control capacity exhausted; nothing committed",
        ));
    }
    Ok(())
}
fn sql_number(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| refusal("Wake number exceeds SQLite range"))
}
fn checkpoint_digest(text: &str) -> Result<String, StoreError> {
    if text.len() > MAX_WAKE_BYTES {
        return Err(refusal("Wake checkpoint too large"));
    }
    let value: WakeCheckpointV1 = serde_json::from_str(text)?;
    value.validate().map_err(|e| refusal(&e))?;
    content_id(&serde_json::to_value(value)?).map_err(|e| refusal(&e.to_string()))
}
impl EventStore {
    /// Explicit owner admission; opening a Store alone never allocates notification identity.
    pub fn admit_wake_incarnation(&self) -> Result<String, StoreError> {
        use std::io::Read;
        let owner = owner(self)?;
        let lock = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some((identity, binding)) = lock
            .query_row(
                "SELECT identity,owner FROM wake_incarnation_v1 WHERE singleton=1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if owner != binding || !af_contracts::is_digest(&identity) {
                return Err(refusal("Foreign wake Store incarnation"));
            }
            lock.commit()?;
            return Ok(identity);
        }
        let mut random = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|e| refusal(&e.to_string()))?;
        let identity = crate::content_id(
            &serde_json::json!({"domain":"af/wake-store/1","nonce":random,"owner":owner}),
        )
        .map_err(|e| refusal(&e.to_string()))?;
        lock.execute(
            "INSERT INTO wake_incarnation_v1 VALUES(1,?1,?2)",
            params![identity, owner],
        )?;
        lock.commit()?;
        Ok(identity)
    }
    pub fn wake_incarnation(&self) -> Result<String, StoreError> {
        let binding = owner(self)?;
        let (identity, expected): (String, String) = self.conn.query_row(
            "SELECT identity,owner FROM wake_incarnation_v1 WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if binding != expected || !af_contracts::is_digest(&identity) {
            return Err(refusal("Foreign wake Store incarnation"));
        }
        Ok(identity)
    }
    /// Registration and launch intent commit before the caller may dispatch. This method
    /// cannot authenticate recipient_ref; authenticated route admission is a separate prerequisite.
    pub fn register_wake(
        &self,
        cas: &Cas,
        registration: &WakeRegistration,
    ) -> Result<WakeRegistration, StoreError> {
        let mut admitted = registration.clone();
        admitted.subscription.subscription_id = content_id(&json!({
            "domain":"af/wake-subscription/1",
            "store":admitted.subscription.task.store_incarnation,
            "key":admitted.subscription.registration_key
        }))
        .map_err(|e| refusal(&e.to_string()))?;
        let registration = &admitted;
        let sub = &registration.subscription;
        registration.validate().map_err(|e| refusal(&e))?;
        let now = task::now()?;
        if sub.registered_unix_ms > now || sub.expires_unix_ms <= now {
            return Err(refusal("Wake registration is future-dated or expired"));
        }
        if sub.configuration_digest != configuration_digest(sub).map_err(|e| refusal(&e))? {
            return Err(refusal("Wake configuration digest mismatch"));
        }
        if registration.schema != "af/task-wake-registration@1"
            || sub.expires_unix_ms - sub.registered_unix_ms > MAX_SUBSCRIPTION_MS
            || self.wake_incarnation()? != sub.task.store_incarnation
        {
            return Err(refusal("Invalid wake registration scope or lifetime"));
        }
        registration
            .registered_prefix
            .validate()
            .map_err(|e| refusal(&e))?;
        let lock = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        // Idempotency compares the complete request, including explicit launch intent/policy.
        let encoded = serde_json::to_string(registration)?;
        if encoded.len() > MAX_WAKE_BYTES {
            return Err(refusal("Wake registration too large"));
        }
        if let Some(existing) = lock
            .query_row(
                "SELECT registration FROM wake_subscriptions_v1 WHERE registration_key=?1",
                [&sub.registration_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing != encoded {
                return Err(refusal("Wake registration key conflict"));
            }
            // Collected identities cannot be registered again, even under an old key.
            self.task_projection(cas, &sub.task.task_id)?
                .ok_or_else(|| refusal("Unknown wake Task"))?;
            return Ok(registration.clone());
        }
        if lock.query_row(
            "SELECT EXISTS(SELECT 1 FROM wake_session_fences_v1 WHERE subscription_id=?1)",
            [&sub.subscription_id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(refusal(
                "Wake identity has uncertain native admission; replacement refused",
            ));
        }
        let state = self
            .task_projection(cas, &sub.task.task_id)?
            .ok_or_else(|| refusal("Unknown wake Task"))?;
        let events = self.replay_window(&sub.task.task_run_id, 0, 1)?;
        if task::task_run_id(&sub.task.task_id)? != sub.task.task_run_id
            || events
                .first()
                .is_none_or(|e| e.event_id != sub.task.opening_event_id)
            || state.next_sequence != registration.registered_prefix.sequence + 1
            || self
                .replay_window(
                    &sub.task.task_run_id,
                    registration.registered_prefix.sequence,
                    1,
                )?
                .first()
                .is_none_or(|e| e.event_id != registration.registered_prefix.event_id)
        {
            return Err(refusal(
                "Wake registration does not bind current checked opening/prefix",
            ));
        }
        lock.execute(
            "INSERT INTO wake_subscriptions_v1(id,registration_key,task_id,registration,expires)
            VALUES(?1,?2,?3,?4,?5)",
            params![
                sub.subscription_id,
                sub.registration_key,
                sub.task.task_id,
                encoded,
                sql_number(sub.expires_unix_ms)?
            ],
        )?;
        capacity(&lock)?;
        if !self.task_observation_unchanged(cas, &state)? {
            return Err(refusal("Wake registration source changed"));
        }
        lock.commit()?;
        Ok(registration.clone())
    }
    pub fn wake_registrations(&self) -> Result<Vec<WakeRegistration>, StoreError> {
        self.wake_incarnation()?;
        let mut statement = self
            .conn
            .prepare("SELECT registration FROM wake_subscriptions_v1 ORDER BY id LIMIT 129")?;
        let encoded = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if encoded.len() > MAX_SUBSCRIPTIONS as usize {
            return Err(refusal("Wake subscription capacity corrupt"));
        }
        encoded
            .into_iter()
            .map(|text| {
                if text.len() > MAX_WAKE_BYTES {
                    return Err(refusal("Wake registration too large"));
                }
                let value: WakeRegistration = serde_json::from_str(&text)?;
                value.validate().map_err(|e| refusal(&e))?;
                if value.subscription.task.store_incarnation != self.wake_incarnation()? {
                    return Err(refusal("Foreign wake subscription incarnation"));
                }
                if value.schema != "af/task-wake-registration@1" {
                    return Err(refusal("Unknown wake registration version"));
                }
                Ok(value)
            })
            .collect()
    }
    pub fn inspect_wake(&self, id: &str) -> Result<WakeJournal, StoreError> {
        let registration = self
            .wake_registrations()?
            .into_iter()
            .find(|r| r.subscription.subscription_id == id)
            .ok_or_else(|| refusal("Unknown wake subscription"))?;
        let (checkpoint, through, valid, digest): (Option<String>, Option<i64>, i64, Option<String>) =
            self.conn.query_row(
                "SELECT checkpoint,through_sequence,valid,checkpoint_digest FROM wake_subscriptions_v1 WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        match (checkpoint.as_deref(), digest.as_deref()) {
            (None, None) => {}
            (Some(text), Some(digest)) if checkpoint_digest(text)? == digest => {}
            _ => return Err(refusal("Wake checkpoint digest mismatch")),
        }
        if valid != 1 {
            return Err(refusal("Wake source was mutated"));
        }
        let mut statement = self.conn.prepare("SELECT envelope FROM wake_outbox_v1 WHERE subscription_id=?1 ORDER BY rowid LIMIT 4097")?;
        let rows = statement
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > MAX_ENVELOPES as usize {
            return Err(refusal("Wake outbox capacity corrupt"));
        }
        let envelopes = rows
            .into_iter()
            .map(|text| {
                let value = decode_envelope(text.as_bytes()).map_err(|e| refusal(&e))?;
                Ok(value)
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(WakeJournal {
            source_available: self
                .task_tombstone(&registration.subscription.task.task_id)?
                .is_none(),
            expired: registration.subscription.expires_unix_ms <= task::now()?,
            registration,
            checkpoint,
            extracted_through: through
                .map(|n| u64::try_from(n).map_err(|_| refusal("Negative wake cursor")))
                .transpose()?,
            envelopes,
            closure: self
                .conn
                .query_row(
                    "SELECT reason FROM wake_closed_v1 WHERE subscription_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?,
        })
    }
    /// Exact same-release retained continuation. Canonical event mutation invalidates it in
    /// the same SQLite transaction; appending a suffix leaves its checked ancestry intact.
    /// This admits no external checkpoint or lifecycle summary.
    pub fn wake_checkpoint_is_retained(&self, checkpoint: &str) -> Result<bool, StoreError> {
        if checkpoint.len() > MAX_WAKE_BYTES {
            return Err(refusal("Wake checkpoint too large"));
        }
        let value: Option<(String, i64, String)> = self
            .conn
            .query_row(
                "SELECT registration,valid,checkpoint_digest FROM wake_subscriptions_v1 WHERE checkpoint=?1 LIMIT 1",
                [checkpoint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((registration, valid, digest)) = value else {
            return Ok(false);
        };
        if checkpoint_digest(checkpoint)? != digest {
            return Err(refusal("Wake checkpoint digest mismatch"));
        }
        if valid != 1 {
            return Err(refusal("Wake checkpoint source was mutated"));
        }
        let registration: WakeRegistration = serde_json::from_str(&registration)?;
        if self.wake_incarnation()? != registration.subscription.task.store_incarnation {
            return Err(refusal("Foreign wake checkpoint incarnation"));
        }
        Ok(true)
    }

    /// Immutable envelopes and cursor update are one transaction, fenced on the previous
    /// cursor and the current checked Task source. Failure leaves neither effect committed.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_wake_batch(
        &self,
        cas: &Cas,
        id: &str,
        expected_checkpoint: Option<&str>,
        checkpoint: &str,
        through: u64,
        source: &task::TaskProjection,
        envelopes: &[WakeEnvelopeV1],
    ) -> Result<(), StoreError> {
        self.commit_wake_batch_with(
            cas,
            id,
            expected_checkpoint,
            checkpoint,
            through,
            source,
            envelopes,
            || Ok(()),
        )
    }
    /// Trusted recorder files are revalidated under the same transaction as source/cursor.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_wake_batch_with(
        &self,
        cas: &Cas,
        id: &str,
        expected_checkpoint: Option<&str>,
        checkpoint: &str,
        through: u64,
        source: &task::TaskProjection,
        envelopes: &[WakeEnvelopeV1],
        recorder_unchanged: impl Fn() -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.wake_incarnation()?;
        if checkpoint.len() > MAX_WAKE_BYTES
            || envelopes.len() > 4096
            || through >= source.next_sequence
        {
            return Err(refusal("Wake batch exceeds bounds"));
        }
        let parsed: WakeCheckpointV1 = serde_json::from_str(checkpoint)?;
        parsed.validate().map_err(|e| refusal(&e))?;
        if parsed.through.sequence != through {
            return Err(refusal("Wake checkpoint sequence mismatch"));
        }
        let lock = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let (old, task_id, valid): (Option<String>, String, i64) = lock.query_row(
            "SELECT checkpoint,task_id,valid FROM wake_subscriptions_v1 WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        recorder_unchanged()?;
        if valid != 1
            || old.as_deref() != expected_checkpoint
            || task_id != source.task_id
            || !self.task_observation_unchanged(cas, source)?
        {
            return Err(refusal("Wake cursor/source changed; retry"));
        }
        let registration: String = lock.query_row(
            "SELECT registration FROM wake_subscriptions_v1 WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        let registration: WakeRegistration = serde_json::from_str(&registration)?;
        registration.validate().map_err(|e| refusal(&e))?;
        if lock.query_row(
            "SELECT EXISTS(SELECT 1 FROM wake_closed_v1 WHERE subscription_id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )? || task::now()? >= registration.subscription.expires_unix_ms
        {
            return Err(refusal("Wake subscription closed or expired"));
        }
        if parsed.configuration_digest != registration.subscription.configuration_digest
            || parsed.observations > registration.subscription.max_observations
            || self
                .replay_window(&registration.subscription.task.task_run_id, through, 1)?
                .first()
                .is_none_or(|e| e.event_id != parsed.through.event_id)
        {
            return Err(refusal("Wake checkpoint scope/boundary mismatch"));
        }
        for envelope in envelopes {
            if envelope.observation.task != registration.subscription.task
                || envelope.expires_unix_ms != registration.subscription.expires_unix_ms
                || envelope.observation.checked_prefix.sequence > through
            {
                return Err(refusal("Wake envelope scope/prefix mismatch"));
            }
            decode_envelope(&serde_json::to_vec(envelope)?).map_err(|e| refusal(&e))?;
            if envelope.subscription_id != id {
                return Err(refusal("Foreign wake envelope"));
            }
            let encoded = serde_json::to_string(envelope)?;
            if encoded.len() > MAX_WAKE_BYTES {
                return Err(refusal("Wake envelope too large"));
            }
            if let Some(existing) = lock
                .query_row(
                    "SELECT envelope FROM wake_outbox_v1 WHERE delivery_id=?1",
                    [&envelope.delivery_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                if existing != encoded {
                    return Err(refusal("Conflicting wake delivery bytes"));
                }
            } else {
                lock.execute(
                    "INSERT INTO wake_outbox_v1 VALUES(?1,?2,?3,?4,?5)",
                    params![
                        envelope.delivery_id,
                        id,
                        envelope.payload_digest,
                        encoded,
                        sql_number(envelope.expires_unix_ms)?
                    ],
                )?;
            }
        }
        // Rebuilding a checkpoint after authenticated recorder changes cannot replenish
        // the subscription's cumulative allowance: retained distinct packets still count.
        let retained: u64 = lock.query_row(
            "SELECT COUNT(*) FROM wake_outbox_v1 WHERE subscription_id=?1",
            [id],
            |r| u64_column(r, 0),
        )?;
        if retained > registration.subscription.max_observations {
            return Err(refusal("Wake subscription observation allowance exhausted"));
        }
        lock.execute("UPDATE wake_subscriptions_v1 SET checkpoint=?2,through_sequence=?3,source_sequence=?4,checkpoint_digest=?5 WHERE id=?1",
            params![id,checkpoint,sql_number(through)?,sql_number(source.next_sequence-1)?,checkpoint_digest(checkpoint)?])?;
        capacity(&lock)?;
        if !self.task_observation_unchanged(cas, source)? {
            return Err(refusal("Wake source changed at commit"));
        }
        recorder_unchanged()?;
        lock.commit()?;
        Ok(())
    }
    /// Collection calls this under its existing writer lock, before any tombstone can commit.
    /// No notification reference participates in the CAS digest walk.
    pub(super) fn require_wake_extracted(
        &self,
        task_id: &str,
        sequence: u64,
    ) -> Result<(), StoreError> {
        // Called inside collection's writer transaction: expiry and the barrier are atomic.
        self.conn.execute("INSERT OR IGNORE INTO wake_closed_v1 SELECT id,'expired_unextracted',?2 FROM wake_subscriptions_v1 WHERE task_id=?1 AND expires<=?2",
            params![task_id,sql_number(task::now()?)?])?;
        let pending: u64 = self.conn.query_row("SELECT COUNT(*) FROM wake_subscriptions_v1
            WHERE task_id=?1 AND id NOT IN (SELECT subscription_id FROM wake_closed_v1)
            AND (valid!=1 OR through_sequence IS NULL OR through_sequence!=?2 OR source_sequence!=?2)",
            params![task_id,sql_number(sequence)?],|row|u64_column(row,0))?;
        if pending != 0 {
            return Err(refusal(
                "Collection blocked: reconcile subscribed Task through its current prefix first",
            ));
        }
        Ok(())
    }
}

/// Materialize once, then retain these exact bytes for retries. Attempts/receipt stages never
/// change the delivery key or digest. In particular a later lease clock must not replace a
/// previously materialized envelope under its existing delivery key.
pub fn envelope(
    subscription: &WakeSubscriptionV1,
    observation: WakeObservationV1,
) -> Result<WakeEnvelopeV1, String> {
    subscription.validate()?;
    if subscription.configuration_digest != configuration_digest(subscription)? {
        return Err("Wake configuration digest mismatch".into());
    }
    observation.validate()?;
    if observation.observation_id != observation_identity(&observation)? {
        return Err("Wake observation identity mismatch".into());
    }
    if observation.task != subscription.task {
        return Err("Wake subscription Task mismatch".into());
    }
    let delivery_id =
        delivery_identity(&subscription.subscription_id, &observation.observation_id)?;
    let payload_digest = String::new();
    let mut value = WakeEnvelopeV1 {
        schema: WAKE_ENVELOPE_V1.into(),
        subscription_id: subscription.subscription_id.clone(),
        delivery_id,
        payload_digest,
        observation,
        expires_unix_ms: subscription.expires_unix_ms,
    };
    value.payload_digest = self::payload_digest(&value)?;
    value.validate()?;
    if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > MAX_WAKE_BYTES {
        return Err("Wake envelope exceeds byte limit".into());
    }
    Ok(value)
}

/// Check logical identities and exact payload bytes when reading a retained envelope. Shape
/// validation alone cannot establish hashes; no current Task acceptance is inferred here.
pub fn decode_envelope(bytes: &[u8]) -> Result<WakeEnvelopeV1, String> {
    let value: WakeEnvelopeV1 = decode_bounded(bytes)?;
    let observation = &value.observation;
    if task::task_run_id(&observation.task.task_id).map_err(|e| e.to_string())?
        != observation.task.task_run_id
        || observation.observation_id != observation_identity(observation)?
        || value.delivery_id
            != delivery_identity(&value.subscription_id, &observation.observation_id)?
        || value.payload_digest != payload_digest(&value)?
    {
        return Err("Wake envelope content identity mismatch".into());
    }
    Ok(value)
}
fn observation_identity(value: &WakeObservationV1) -> Result<String, String> {
    content_id(&json!({"domain":"af/wake-observation/1","task":value.task,"causal":value.causal,"revision":value.revision_id,"episode":value.episode,"kind":value.kind,"evidence":value.evidence})).map_err(|e|e.to_string())
}
fn delivery_identity(subscription: &str, observation: &str) -> Result<String, String> {
    content_id(&json!({"domain":"af/wake-delivery/1","subscription":subscription,"observation":observation})).map_err(|e|e.to_string())
}
fn payload_digest(value: &WakeEnvelopeV1) -> Result<String, String> {
    let mut payload = serde_json::to_value(value).map_err(|e| e.to_string())?;
    payload
        .as_object_mut()
        .ok_or("Invalid wake envelope")?
        .remove("payload_digest");
    content_id(&payload).map_err(|e| e.to_string())
}

/// Digest the exact requested configuration independently of allocated subscription identity.
pub fn configuration_digest(value: &WakeSubscriptionV1) -> Result<String, String> {
    value.validate()?;
    content_id(&json!({"domain":"af/wake-registration/1","task":value.task,"recipient_ref":value.recipient_ref,
        "registered_unix_ms":value.registered_unix_ms,"expires_unix_ms":value.expires_unix_ms,"max_observations":value.max_observations})).map_err(|e|e.to_string())
}

/// Pure registration comparison only; no persistence, allocation or route authority.
pub fn same_registration(
    existing: &WakeSubscriptionV1,
    requested: &WakeSubscriptionV1,
) -> Result<bool, String> {
    existing.validate()?;
    requested.validate()?;
    if existing.configuration_digest != configuration_digest(existing)?
        || requested.configuration_digest != configuration_digest(requested)?
    {
        return Err("Invalid wake configuration digest".into());
    }
    if existing.task.store_incarnation != requested.task.store_incarnation
        || existing.registration_key != requested.registration_key
    {
        return Ok(false);
    }
    if existing.configuration_digest != requested.configuration_digest {
        return Err("Wake registration key conflicts with configuration".into());
    }
    Ok(true)
}
