//! Read/link compatibility for pre-Task Campaigns and implementation `tasks.sqlite` logs.
//! A link is an index entry into the original authority, never a new execution or copied charge.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use review_core::RunEvent;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{EventStore, StoreError};
use crate::{Cas, content_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyExecutionKind {
    Implementation,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyTaskEvent {
    pub sequence: u64,
    pub event_type: String,
    pub artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LegacyExecutionHistory {
    Implementation { events: Vec<LegacyTaskEvent> },
    Review { events: Vec<RunEvent> },
}

impl LegacyExecutionHistory {
    fn through(&self) -> Option<u64> {
        match self {
            Self::Implementation { events } => events.last().map(|e| e.sequence),
            Self::Review { events } => events.last().map(|e| e.sequence),
        }
    }

    fn truncate(&mut self, through: u64) {
        match self {
            Self::Implementation { events } => events.retain(|e| e.sequence <= through),
            Self::Review { events } => events.retain(|e| e.sequence <= through),
        }
    }
}

/// The stable origin ID is retained when an archived Store is relocated. Path locators are
/// operational metadata; callers must explicitly retain the origin ID when moving archives.
#[derive(Debug, Clone)]
pub struct LegacyStoreOrigin {
    pub store_id: String,
    pub database: PathBuf,
    pub cas: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyTaskLink {
    pub schema: String,
    pub origin_store_id: String,
    pub database: PathBuf,
    pub cas: PathBuf,
    pub kind: LegacyExecutionKind,
    /// The original Task or Campaign ID is preserved, including its historical spelling.
    pub record_id: String,
    pub through_sequence: u64,
    pub history_id: String,
}

fn conflict(message: impl Into<String>) -> StoreError {
    StoreError::Conflict(message.into())
}

fn canonical_file(path: &Path) -> Result<PathBuf, StoreError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| conflict(e.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(conflict("Legacy Store must be a regular SQLite file"));
    }
    std::fs::canonicalize(path).map_err(|e| conflict(e.to_string()))
}

impl LegacyStoreOrigin {
    /// Assign an initial stable origin ID without writing anything into the historical Store.
    pub fn at(database: &Path, cas: &Path) -> Result<Self, StoreError> {
        let database = canonical_file(database)?;
        let cas = std::fs::canonicalize(cas).map_err(|e| conflict(e.to_string()))?;
        Cas::open_existing(&cas).map_err(|e| conflict(e.to_string()))?;
        let store_id = content_id(
            &json!({"namespace":"af/legacy-store-origin/1", "database":database, "cas":cas}),
        )
        .map_err(|e| conflict(e.to_string()))?;
        Ok(Self {
            store_id,
            database,
            cas,
        })
    }

    pub fn read(
        &self,
        kind: LegacyExecutionKind,
        record_id: &str,
    ) -> Result<LegacyExecutionHistory, StoreError> {
        if !review_core::is_digest(&self.store_id) || record_id.is_empty() || record_id.len() > 1024
        {
            return Err(conflict("Invalid legacy origin or record identity"));
        }
        canonical_file(&self.database)?;
        let cas = Cas::open_existing(&self.cas).map_err(|e| conflict(e.to_string()))?;
        let mut artifacts = BTreeSet::new();
        let history = match kind {
            LegacyExecutionKind::Implementation => {
                let connection =
                    Connection::open_with_flags(&self.database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                let mut statement = connection.prepare("SELECT sequence, event_type, artifact_id FROM task_events WHERE task_id = ?1 ORDER BY sequence")?;
                let events = statement
                    .query_map([record_id], |row| {
                        Ok(LegacyTaskEvent {
                            sequence: super::u64_column(row, 0)?,
                            event_type: row.get(1)?,
                            artifact_id: row.get(2)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                for (index, event) in events.iter().enumerate() {
                    if event.sequence != index as u64 + 1
                        || event.event_type.is_empty()
                        || !review_core::is_digest(&event.artifact_id)
                    {
                        return Err(conflict(
                            "Legacy implementation history has invalid identity or sequence",
                        ));
                    }
                    artifacts.insert(event.artifact_id.clone());
                }
                LegacyExecutionHistory::Implementation { events }
            }
            LegacyExecutionKind::Review => {
                let store = EventStore::open_read_only(&self.database)?;
                let events = store.replay(record_id)?;
                for (index, event) in events.iter().enumerate() {
                    if event.sequence != index as u64 {
                        return Err(conflict("Legacy Campaign history has a sequence gap"));
                    }
                    artifacts.extend(event.artifact_refs.iter().cloned());
                }
                LegacyExecutionHistory::Review { events }
            }
        };
        if history.through().is_none() {
            return Err(conflict("Legacy execution does not exist"));
        }
        for id in artifacts {
            cas.verify(&id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
        }
        Ok(history)
    }
}

impl EventStore {
    /// Preserve a historical origin in the common Store without importing its Attempt events.
    /// Idempotence is keyed by (origin, kind, native ID), so repeated links never add spend.
    pub fn link_legacy_task(
        &mut self,
        cas: &Cas,
        origin: &LegacyStoreOrigin,
        kind: LegacyExecutionKind,
        record_id: &str,
    ) -> Result<LegacyTaskLink, StoreError> {
        let history = origin.read(kind, record_id)?;
        let key = content_id(&json!({"namespace":"af/legacy-task-link/1", "origin":origin.store_id, "kind":kind, "record_id":record_id})).map_err(|e| conflict(e.to_string()))?;
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS task_legacy_links (link_id TEXT PRIMARY KEY NOT NULL, artifact_id TEXT NOT NULL)")?;
        if let Some(existing) = self
            .conn
            .query_row(
                "SELECT artifact_id FROM task_legacy_links WHERE link_id = ?1",
                [&key],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let link = read_link(cas, &existing)?;
            verify_prefix(cas, &link, history)?;
            return Ok(link);
        }
        let history_id = cas
            .put_json(&serde_json::to_value(&history)?)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        let link = LegacyTaskLink {
            schema: "af.legacy-task-link/1".into(),
            origin_store_id: origin.store_id.clone(),
            database: origin.database.clone(),
            cas: origin.cas.clone(),
            kind,
            record_id: record_id.into(),
            through_sequence: history.through().expect("nonempty history"),
            history_id,
        };
        let artifact_id = cas
            .put_json(&serde_json::to_value(&link)?)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        // The same CAS durability boundary precedes the common Store's immediate transaction.
        cas.flush()
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO task_legacy_links(link_id, artifact_id) VALUES (?1, ?2)",
            params![key, artifact_id],
        )?;
        let selected: String = tx.query_row(
            "SELECT artifact_id FROM task_legacy_links WHERE link_id = ?1",
            [&key],
            |row| row.get(0),
        )?;
        let selected = read_link(cas, &selected)?;
        verify_prefix(cas, &selected, history)?;
        tx.commit()?;
        Ok(selected)
    }

    pub fn legacy_task_links(&self, cas: &Cas) -> Result<Vec<LegacyTaskLink>, StoreError> {
        let exists: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'task_legacy_links')", [], |row| row.get(0))?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut statement = self
            .conn
            .prepare("SELECT artifact_id FROM task_legacy_links ORDER BY link_id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter().map(|id| read_link(cas, id)).collect()
    }
}

fn read_link(cas: &Cas, id: &str) -> Result<LegacyTaskLink, StoreError> {
    let value = cas
        .get_json(id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    let link: LegacyTaskLink = serde_json::from_value(value)?;
    if link.schema != "af.legacy-task-link/1"
        || !review_core::is_digest(&link.origin_store_id)
        || !review_core::is_digest(&link.history_id)
        || link.record_id.is_empty()
        || !link.database.is_absolute()
        || !link.cas.is_absolute()
    {
        return Err(conflict("Invalid legacy Task link"));
    }
    cas.verify(&link.history_id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    Ok(link)
}

fn verify_prefix(
    cas: &Cas,
    link: &LegacyTaskLink,
    mut current: LegacyExecutionHistory,
) -> Result<(), StoreError> {
    if current
        .through()
        .is_none_or(|last| last < link.through_sequence)
    {
        return Err(conflict("Linked legacy history was truncated"));
    }
    current.truncate(link.through_sequence);
    let original = cas
        .get_json(&link.history_id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    if serde_json::to_value(current)? != original {
        return Err(conflict("Linked legacy history was rewritten"));
    }
    Ok(())
}

impl LegacyTaskLink {
    /// Continue to inspect new events from the original compatible runner, while checking the
    /// prefix originally linked. No execution permission is derived from this read adapter.
    pub fn read(&self, cas: &Cas) -> Result<LegacyExecutionHistory, StoreError> {
        let origin = LegacyStoreOrigin {
            store_id: self.origin_store_id.clone(),
            database: self.database.clone(),
            cas: self.cas.clone(),
        };
        let history = origin.read(self.kind, &self.record_id)?;
        verify_prefix(cas, self, history.clone())?;
        Ok(history)
    }
}

#[cfg(test)]
mod tests;
