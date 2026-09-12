//! Additive exact-usage sidecar. Historical numeric columns remain readable.

use super::{AttemptUsage, AttemptWall, EventStore, StoreError, TaskAttemptWall};
use review_core::task::usage::{DecimalU64, TaskTokenUsageV1, TaskTokenUsageV2};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

impl From<&AttemptUsage> for TaskTokenUsageV1 {
    fn from(value: &AttemptUsage) -> Self {
        Self {
            input_tokens: value.input_tokens.map(Into::into),
            output_tokens: value.output_tokens.map(Into::into),
            cache_read_tokens: value.cache_read_tokens.map(Into::into),
            cache_write_tokens: value.cache_write_tokens.map(Into::into),
            reasoning_tokens: value.reasoning_tokens.map(Into::into),
            chargeable_tokens: value.chargeable_tokens.into(),
        }
    }
}

impl From<TaskTokenUsageV1> for AttemptUsage {
    fn from(value: TaskTokenUsageV1) -> Self {
        Self {
            input_tokens: value.input_tokens.map(DecimalU64::get),
            output_tokens: value.output_tokens.map(DecimalU64::get),
            cache_read_tokens: value.cache_read_tokens.map(DecimalU64::get),
            cache_write_tokens: value.cache_write_tokens.map(DecimalU64::get),
            reasoning_tokens: value.reasoning_tokens.map(DecimalU64::get),
            chargeable_tokens: value.chargeable_tokens.get(),
        }
    }
}

impl From<&AttemptUsage> for TaskTokenUsageV2 {
    fn from(value: &AttemptUsage) -> Self {
        TaskTokenUsageV1::from(value).into()
    }
}

impl TryFrom<TaskTokenUsageV2> for AttemptUsage {
    type Error = StoreError;

    fn try_from(value: TaskTokenUsageV2) -> Result<Self, Self::Error> {
        Ok(Self {
            input_tokens: value.input_tokens.map(DecimalU64::get),
            output_tokens: value.output_tokens.map(DecimalU64::get),
            cache_read_tokens: value.cache_read_tokens.map(DecimalU64::get),
            cache_write_tokens: value.cache_write_tokens.map(DecimalU64::get),
            reasoning_tokens: value.reasoning_tokens.map(DecimalU64::get),
            chargeable_tokens: value.chargeable_tokens.get().try_into().map_err(|_| {
                StoreError::Conflict("Task Attempt usage exceeds the legacy u64 range".into())
            })?,
        })
    }
}

fn has_usage_column(conn: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT count(*) FROM pragma_table_info('attempt_wall') WHERE name = ?1",
        [name],
        |row| row.get::<_, i64>(0).map(|count| count == 1),
    )
}

pub(super) fn migrate(conn: &Connection) -> Result<(), StoreError> {
    if has_usage_column(conn, "usage_v1_json")? && has_usage_column(conn, "usage_v2_json")? {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for name in ["usage_v1_json", "usage_v2_json"] {
        if !has_usage_column(&transaction, name)? {
            transaction
                .execute_batch(&format!("ALTER TABLE attempt_wall ADD COLUMN {name} TEXT"))?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn usage(row: &Row<'_>, offset: usize) -> Result<Option<TaskTokenUsageV2>, rusqlite::Error> {
    let error = |column, error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    // Presence selects the version, including on failure. A malformed newer payload must
    // never fall back to historical bytes or pretend that paid usage was absent.
    if let Some(exact) = row.get::<_, Option<String>>(offset + 7)? {
        return serde_json::from_str::<TaskTokenUsageV2>(&exact)
            .map(Some)
            .map_err(|e| error(offset + 7, e));
    }
    if let Some(exact) = row.get::<_, Option<String>>(offset + 6)? {
        return serde_json::from_str::<TaskTokenUsageV1>(&exact)
            .map(|value| Some(value.into()))
            .map_err(|e| error(offset + 6, e));
    }
    let chargeable: Option<i64> = row.get(offset + 5)?;
    let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
    let optional = |column| -> Result<Option<DecimalU64>, rusqlite::Error> {
        Ok(row
            .get::<_, Option<i64>>(column)?
            .map(|value| unsigned(value).into()))
    };
    chargeable
        .map(|chargeable| {
            Ok(TaskTokenUsageV2 {
                input_tokens: optional(offset)?,
                output_tokens: optional(offset + 1)?,
                cache_read_tokens: optional(offset + 2)?,
                cache_write_tokens: optional(offset + 3)?,
                reasoning_tokens: optional(offset + 4)?,
                chargeable_tokens: u128::from(unsigned(chargeable)).into(),
            })
        })
        .transpose()
}

fn merge_usage(current: &mut Option<TaskTokenUsageV2>, previous: Option<TaskTokenUsageV2>) {
    if let Some(previous) = previous {
        match current {
            Some(usage) => {
                usage.input_tokens = usage.input_tokens.max(previous.input_tokens);
                usage.output_tokens = usage.output_tokens.max(previous.output_tokens);
                usage.cache_read_tokens = usage.cache_read_tokens.max(previous.cache_read_tokens);
                usage.cache_write_tokens =
                    usage.cache_write_tokens.max(previous.cache_write_tokens);
                usage.reasoning_tokens = usage.reasoning_tokens.max(previous.reasoning_tokens);
                usage.chargeable_tokens = usage.chargeable_tokens.max(previous.chargeable_tokens);
            }
            None => *current = Some(previous),
        }
    }
}

impl EventStore {
    /// Preserve the legacy sidecar encoding. A row with wider Task usage is an explicit
    /// overflow error, never a narrowed counter or an absent observation.
    pub fn record_attempt_wall(&self, wall: &AttemptWall) -> Result<(), StoreError> {
        self.record_wall(
            &TaskAttemptWall {
                run_id: wall.run_id.clone(),
                attempt_id: wall.attempt_id.clone(),
                node_id: wall.node_id.clone(),
                round: wall.round,
                epoch: wall.epoch,
                started_unix_ms: wall.started_unix_ms,
                elapsed_ms: wall.elapsed_ms,
                usage: wall.usage.as_ref().map(Into::into),
            },
            true,
        )
    }

    /// Record an exact cumulative Task usage floor before fallible output publication.
    /// Native components remain u64; the charge can aggregate multiple Provider operations.
    pub fn record_task_attempt_wall(&self, wall: &TaskAttemptWall) -> Result<(), StoreError> {
        self.record_wall(wall, false)
    }

    fn record_wall(&self, wall: &TaskAttemptWall, legacy: bool) -> Result<(), StoreError> {
        fn bounded(value: u64, what: &str) -> Result<i64, StoreError> {
            i64::try_from(value).map_err(|_| {
                StoreError::Conflict(format!("attempt wall {what} exceeds SQLite range"))
            })
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, chargeable_tokens, usage_v1_json, usage_v2_json
             FROM attempt_wall WHERE run_id = ?1 AND attempt_id = ?2",
                rusqlite::params![wall.run_id, wall.attempt_id],
                |row| Ok((usage(row, 0)?, row.get::<_, Option<String>>(7)?.is_some())),
            )
            .optional()?;
        let has_v2 = previous.as_ref().is_some_and(|(_, has_v2)| *has_v2);
        let mut usage = wall.usage.clone();
        merge_usage(&mut usage, previous.and_then(|(usage, _)| usage));
        let exact = if !legacy || has_v2 {
            usage.as_ref().map(serde_json::to_string).transpose()?
        } else {
            None
        };
        let started = bounded(wall.started_unix_ms, "start")?;
        let elapsed = bounded(wall.elapsed_ms, "elapsed")?;
        if legacy {
            let usage = usage.map(AttemptUsage::try_from).transpose()?;
            let v1 = usage
                .as_ref()
                .map(|usage| serde_json::to_string(&TaskTokenUsageV1::from(usage)))
                .transpose()?;
            // The old numeric columns retain their original compatibility behavior.
            let integer = |value: Option<u64>| value.and_then(|value| i64::try_from(value).ok());
            transaction.execute(
                "INSERT OR REPLACE INTO attempt_wall (
                    run_id, attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens, chargeable_tokens, usage_v1_json, usage_v2_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    wall.run_id,
                    wall.attempt_id,
                    wall.node_id,
                    i64::from(wall.round),
                    i64::from(wall.epoch),
                    started,
                    elapsed,
                    integer(usage.as_ref().and_then(|u| u.input_tokens)),
                    integer(usage.as_ref().and_then(|u| u.output_tokens)),
                    integer(usage.as_ref().and_then(|u| u.cache_read_tokens)),
                    integer(usage.as_ref().and_then(|u| u.cache_write_tokens)),
                    integer(usage.as_ref().and_then(|u| u.reasoning_tokens)),
                    integer(usage.as_ref().map(|u| u.chargeable_tokens)),
                    v1,
                    exact
                ],
            )?;
        } else {
            // Upgrading a measurement preserves existing historical bytes and numeric columns.
            transaction.execute(
                "INSERT INTO attempt_wall (run_id, attempt_id, node_id, round, epoch,
                    started_unix_ms, elapsed_ms, usage_v2_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(run_id, attempt_id) DO UPDATE SET
                    node_id=excluded.node_id, round=excluded.round, epoch=excluded.epoch,
                    started_unix_ms=excluded.started_unix_ms, elapsed_ms=excluded.elapsed_ms,
                    usage_v2_json=excluded.usage_v2_json",
                rusqlite::params![
                    wall.run_id,
                    wall.attempt_id,
                    wall.node_id,
                    i64::from(wall.round),
                    i64::from(wall.epoch),
                    started,
                    elapsed,
                    exact
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Checked compatibility view. New-format presence selects that format even on failure.
    pub fn attempt_wall(&self, run_id: &str) -> Result<Vec<AttemptWall>, StoreError> {
        self.task_attempt_wall(run_id)?
            .into_iter()
            .map(|wall| {
                Ok(AttemptWall {
                    run_id: wall.run_id,
                    attempt_id: wall.attempt_id,
                    node_id: wall.node_id,
                    round: wall.round,
                    epoch: wall.epoch,
                    started_unix_ms: wall.started_unix_ms,
                    elapsed_ms: wall.elapsed_ms,
                    usage: wall.usage.map(AttemptUsage::try_from).transpose()?,
                })
            })
            .collect()
    }

    /// Read-only historical stores need no migration. Old rows widen without rewriting them;
    /// an absent table has no measurements.
    pub fn task_attempt_wall(&self, run_id: &str) -> Result<Vec<TaskAttemptWall>, StoreError> {
        let present: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'attempt_wall'",
            [],
            |row| row.get(0),
        )?;
        if present == 0 {
            return Ok(Vec::new());
        }
        let column = |name| -> Result<&str, rusqlite::Error> {
            Ok(if has_usage_column(&self.conn, name)? {
                name
            } else {
                "NULL"
            })
        };
        let v1 = column("usage_v1_json")?;
        let v2 = column("usage_v2_json")?;
        let mut statement = self.conn.prepare(&format!(
            "SELECT attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, chargeable_tokens, {v1}, {v2}
             FROM attempt_wall WHERE run_id = ?1 ORDER BY started_unix_ms, attempt_id"
        ))?;
        let rows = statement.query_map([run_id], |row| {
            let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
            Ok(TaskAttemptWall {
                run_id: run_id.to_string(),
                attempt_id: row.get(0)?,
                node_id: row.get(1)?,
                round: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                epoch: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                started_unix_ms: unsigned(row.get(4)?),
                elapsed_ms: unsigned(row.get(5)?),
                usage: usage(row, 6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Sqlite)
    }
}
