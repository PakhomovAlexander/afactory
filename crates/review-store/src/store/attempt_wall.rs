//! Additive exact-usage sidecar. Historical numeric columns remain readable.

use super::{AttemptUsage, AttemptWall, EventStore, StoreError};
use review_core::task::usage::{DecimalU64, TaskTokenUsageV1};
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

fn has_exact_column(conn: &Connection) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT count(*) FROM pragma_table_info('attempt_wall') WHERE name = 'usage_v1_json'",
        [],
        |row| row.get::<_, i64>(0).map(|count| count == 1),
    )
}

pub(super) fn migrate(conn: &Connection) -> Result<(), StoreError> {
    if has_exact_column(conn)? {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if !has_exact_column(&transaction)? {
        transaction.execute_batch("ALTER TABLE attempt_wall ADD COLUMN usage_v1_json TEXT")?;
    }
    transaction.commit()?;
    Ok(())
}

fn usage(row: &Row<'_>, offset: usize) -> Result<Option<AttemptUsage>, rusqlite::Error> {
    let exact: Option<String> = row.get(offset + 6)?;
    if let Some(exact) = exact {
        // Presence selects the new format, including on failure: malformed exact data may
        // never silently fall back to old numeric fields or invent an absent observation.
        return serde_json::from_str::<TaskTokenUsageV1>(&exact)
            .map(|usage| Some(usage.into()))
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    offset + 6,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            });
    }
    let chargeable: Option<i64> = row.get(offset + 5)?;
    let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
    let optional = |column| -> Result<Option<u64>, rusqlite::Error> {
        Ok(row.get::<_, Option<i64>>(column)?.map(unsigned))
    };
    chargeable
        .map(|chargeable| {
            Ok(AttemptUsage {
                input_tokens: optional(offset)?,
                output_tokens: optional(offset + 1)?,
                cache_read_tokens: optional(offset + 2)?,
                cache_write_tokens: optional(offset + 3)?,
                reasoning_tokens: optional(offset + 4)?,
                chargeable_tokens: unsigned(chargeable),
            })
        })
        .transpose()
}

impl EventStore {
    /// Write elapsed time and a monotonic usage floor before fallible output publication.
    /// Exact decimal counters live in a TEXT column; SQLite numeric affinity cannot round them.
    pub fn record_attempt_wall(&self, wall: &AttemptWall) -> Result<(), StoreError> {
        fn bounded(value: u64, what: &str) -> Result<i64, StoreError> {
            i64::try_from(value).map_err(|_| {
                StoreError::Conflict(format!("attempt wall {what} exceeds SQLite range"))
            })
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens, chargeable_tokens, usage_v1_json
             FROM attempt_wall WHERE run_id = ?1 AND attempt_id = ?2",
                rusqlite::params![wall.run_id, wall.attempt_id],
                |row| usage(row, 0),
            )
            .optional()?
            .flatten();
        let mut usage = wall.usage.clone();
        if let Some(previous) = previous {
            match &mut usage {
                Some(usage) => {
                    usage.input_tokens = usage.input_tokens.max(previous.input_tokens);
                    usage.output_tokens = usage.output_tokens.max(previous.output_tokens);
                    usage.cache_read_tokens =
                        usage.cache_read_tokens.max(previous.cache_read_tokens);
                    usage.cache_write_tokens =
                        usage.cache_write_tokens.max(previous.cache_write_tokens);
                    usage.reasoning_tokens = usage.reasoning_tokens.max(previous.reasoning_tokens);
                    usage.chargeable_tokens =
                        usage.chargeable_tokens.max(previous.chargeable_tokens);
                }
                None => usage = Some(previous),
            }
        }
        let exact = usage
            .as_ref()
            .map(|usage| serde_json::to_string(&TaskTokenUsageV1::from(usage)))
            .transpose()?;
        // The old columns are a compatibility view only. Values outside their exact range
        // stay absent there; every new reader prefers the complete versioned TEXT payload.
        let integer = |value: Option<u64>| value.and_then(|value| i64::try_from(value).ok());
        transaction.execute(
            "INSERT OR REPLACE INTO attempt_wall (
                 run_id, attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                 reasoning_tokens, chargeable_tokens, usage_v1_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                wall.run_id,
                wall.attempt_id,
                wall.node_id,
                i64::from(wall.round),
                i64::from(wall.epoch),
                bounded(wall.started_unix_ms, "start")?,
                bounded(wall.elapsed_ms, "elapsed")?,
                integer(usage.as_ref().and_then(|u| u.input_tokens)),
                integer(usage.as_ref().and_then(|u| u.output_tokens)),
                integer(usage.as_ref().and_then(|u| u.cache_read_tokens)),
                integer(usage.as_ref().and_then(|u| u.cache_write_tokens)),
                integer(usage.as_ref().and_then(|u| u.reasoning_tokens)),
                integer(usage.as_ref().map(|u| u.chargeable_tokens)),
                exact,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Read-only historical stores require no migration; an absent table has no measurements.
    pub fn attempt_wall(&self, run_id: &str) -> Result<Vec<AttemptWall>, StoreError> {
        let present: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'attempt_wall'",
            [],
            |row| row.get(0),
        )?;
        if present == 0 {
            return Ok(Vec::new());
        }
        let exact = if has_exact_column(&self.conn)? {
            "usage_v1_json"
        } else {
            "NULL"
        };
        let mut statement = self.conn.prepare(&format!(
            "SELECT attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens, chargeable_tokens, {exact}
             FROM attempt_wall WHERE run_id = ?1 ORDER BY started_unix_ms, attempt_id"
        ))?;
        let rows = statement.query_map([run_id], |row| {
            let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
            Ok(AttemptWall {
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
