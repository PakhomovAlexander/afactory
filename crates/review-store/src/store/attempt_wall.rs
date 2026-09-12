//! Additive exact-usage sidecar. Historical numeric columns remain readable.

use super::{AttemptUsage, AttemptWall, EventStore, StoreError, TaskAttemptWall};
use review_core::task::usage::{DecimalU64, TaskTokenUsageV1, TaskTokenUsageV2, TaskTokenUsageV3};
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

impl From<&AttemptUsage> for TaskTokenUsageV3 {
    fn from(value: &AttemptUsage) -> Self {
        TaskTokenUsageV1::from(value).into()
    }
}
impl TryFrom<TaskTokenUsageV3> for AttemptUsage {
    type Error = StoreError;
    fn try_from(value: TaskTokenUsageV3) -> Result<Self, Self::Error> {
        TaskTokenUsageV1::try_from(&value)
            .map(Into::into)
            .map_err(|error| {
                StoreError::Conflict(format!(
                    "Task Attempt usage exceeds the legacy u64 range: {error}"
                ))
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
    if has_usage_column(conn, "usage_v1_json")?
        && has_usage_column(conn, "usage_v2_json")?
        && has_usage_column(conn, "usage_v3_json")?
        && has_usage_column(conn, "usage_observation_v1_json")?
    {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for name in [
        "usage_v1_json",
        "usage_v2_json",
        "usage_v3_json",
        "usage_observation_v1_json",
    ] {
        if !has_usage_column(&transaction, name)? {
            transaction
                .execute_batch(&format!("ALTER TABLE attempt_wall ADD COLUMN {name} TEXT"))?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn usage(row: &Row<'_>, offset: usize) -> Result<Option<TaskTokenUsageV3>, rusqlite::Error> {
    let error = |column, error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    // Presence selects the version, including on failure. A malformed newer payload must
    // never fall back to historical bytes or pretend that paid usage was absent.
    if let Some(exact) = row.get::<_, Option<String>>(offset + 8)? {
        return serde_json::from_str::<TaskTokenUsageV3>(&exact)
            .map(Some)
            .map_err(|e| error(offset + 8, e));
    }
    if let Some(exact) = row.get::<_, Option<String>>(offset + 7)? {
        return serde_json::from_str::<TaskTokenUsageV2>(&exact)
            .map(|value| Some(value.into()))
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
            }
            .into())
        })
        .transpose()
}

fn merge_usage(current: &mut Option<TaskTokenUsageV3>, previous: Option<TaskTokenUsageV3>) {
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
            None,
        )
    }

    /// Record an exact cumulative Task usage floor before fallible output publication.
    /// Native turn components and charge retain their full aggregate range.
    pub fn record_task_attempt_wall(&self, wall: &TaskAttemptWall) -> Result<(), StoreError> {
        self.record_wall(wall, false, None)
    }

    /// Persist native observation and its effective cumulative charge in the same transaction.
    /// Absence retains prior facts; incompleteness is sticky without a whole-Attempt replacement.
    pub fn record_task_attempt_wall_with_observation(
        &self,
        wall: &TaskAttemptWall,
        observation: &review_core::task::usage::TaskUsageObservationV1,
    ) -> Result<(), StoreError> {
        observation.validate().map_err(StoreError::Conflict)?;
        self.record_wall(wall, false, Some(observation))
    }

    pub fn task_attempt_usage_observation(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<Option<review_core::task::usage::TaskUsageObservationV1>, StoreError> {
        if !has_usage_column(&self.conn, "usage_observation_v1_json")? {
            return Ok(None);
        }
        let text: Option<String> = self.conn.query_row(
            "SELECT usage_observation_v1_json FROM attempt_wall WHERE run_id = ?1 AND attempt_id = ?2",
            rusqlite::params![run_id, attempt_id], |row| row.get(0),
        ).optional()?.flatten();
        text.map(|text| {
            let observation: review_core::task::usage::TaskUsageObservationV1 =
                serde_json::from_str(&text)?;
            observation.validate().map_err(StoreError::Conflict)?;
            Ok(observation)
        })
        .transpose()
    }

    fn record_wall(
        &self,
        wall: &TaskAttemptWall,
        legacy: bool,
        observation: Option<&review_core::task::usage::TaskUsageObservationV1>,
    ) -> Result<(), StoreError> {
        fn bounded(value: u64, what: &str) -> Result<i64, StoreError> {
            i64::try_from(value).map_err(|_| {
                StoreError::Conflict(format!("attempt wall {what} exceeds SQLite range"))
            })
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, chargeable_tokens, usage_v1_json, usage_v2_json, usage_v3_json, usage_observation_v1_json
             FROM attempt_wall WHERE run_id = ?1 AND attempt_id = ?2",
                rusqlite::params![wall.run_id, wall.attempt_id],
                |row| {
                    Ok((
                        usage(row, 0)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?.is_some(),
                        row.get::<_, Option<String>>(9)?,
                    ))
                },
            )
            .optional()?;
        let prior_v2 = previous.as_ref().and_then(|(_, v2, _, _)| v2.clone());
        let has_v3 = previous.as_ref().is_some_and(|(_, _, v3, _)| *v3);
        let previous_observation = previous
            .as_ref()
            .and_then(|(_, _, _, text)| text.as_deref())
            .map(serde_json::from_str::<review_core::task::usage::TaskUsageObservationV1>)
            .transpose()?;
        if let Some(previous) = &previous_observation {
            previous.validate().map_err(StoreError::Conflict)?;
        }
        let mut observation = observation.cloned();
        if let Some(previous) = previous_observation {
            match &mut observation {
                Some(current) => current.merge_previous(&previous),
                None => observation = Some(previous),
            }
        }
        let mut usage = wall.usage.clone();
        merge_usage(&mut usage, previous.and_then(|(usage, _, _, _)| usage));
        if let Some(reported) = observation
            .as_ref()
            .and_then(|value| value.reported_usage.as_ref())
            && usage
                .as_ref()
                .is_none_or(|value| value.chargeable_tokens < reported.chargeable_tokens)
        {
            return Err(StoreError::Conflict(
                "Task effective charge is below its native observation".into(),
            ));
        }
        let observation = observation
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let wide = has_v3
            || usage
                .as_ref()
                .is_some_and(|u| TaskTokenUsageV2::try_from(u).is_err());
        let exact = usage.as_ref().map(serde_json::to_string).transpose()?;
        // Upgrading a row never rewrites the frozen older-version text.
        let v2 = if wide {
            prior_v2
        } else if !legacy || prior_v2.is_some() {
            exact.clone()
        } else {
            None
        };
        let v3 = if wide { exact.clone() } else { None };
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
                    reasoning_tokens, chargeable_tokens, usage_v1_json, usage_v2_json, usage_v3_json, usage_observation_v1_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
                    v2,
                    v3,
                    observation
                ],
            )?;
        } else {
            // Upgrading a measurement preserves existing historical bytes and numeric columns.
            transaction.execute(
                "INSERT INTO attempt_wall (run_id, attempt_id, node_id, round, epoch,
                    started_unix_ms, elapsed_ms, usage_v2_json, usage_v3_json, usage_observation_v1_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(run_id, attempt_id) DO UPDATE SET
                    node_id=excluded.node_id, round=excluded.round, epoch=excluded.epoch,
                    started_unix_ms=excluded.started_unix_ms, elapsed_ms=excluded.elapsed_ms,
                    usage_v2_json=excluded.usage_v2_json, usage_v3_json=excluded.usage_v3_json,
                    usage_observation_v1_json=excluded.usage_observation_v1_json",
                rusqlite::params![
                    wall.run_id,
                    wall.attempt_id,
                    wall.node_id,
                    i64::from(wall.round),
                    i64::from(wall.epoch),
                    started,
                    elapsed,
                    v2,
                    v3,
                    observation
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
        let v3 = column("usage_v3_json")?;
        let observation = column("usage_observation_v1_json")?;
        let mut statement = self.conn.prepare(&format!(
            "SELECT attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, chargeable_tokens, {v1}, {v2}, {v3}, {observation}
             FROM attempt_wall WHERE run_id = ?1 ORDER BY started_unix_ms, attempt_id"
        ))?;
        let rows = statement.query_map([run_id], |row| {
            if let Some(text) = row.get::<_, Option<String>>(15)? {
                let observation: review_core::task::usage::TaskUsageObservationV1 =
                    serde_json::from_str(&text).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            15,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                observation.validate().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        15,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other(error)),
                    )
                })?;
            }
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
