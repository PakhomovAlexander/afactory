//! Exact-usage sidecar: one row per Attempt holding its wall-clock interval, its cumulative
//! `TaskTokenUsage@3` floor and its native usage observation.

use super::{EventStore, StoreError, TaskAttemptWall};
use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior};

fn text_error(
    column: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(error))
}

/// A malformed payload is an error, never an absent measurement.
fn usage(row: &Row<'_>, column: usize) -> Result<Option<TaskTokenUsageV3>, rusqlite::Error> {
    row.get::<_, Option<String>>(column)?
        .map(|text| serde_json::from_str(&text).map_err(|error| text_error(column, error)))
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
    /// Record an exact cumulative Task usage floor before fallible output publication.
    /// Native turn components and charge retain their full aggregate range.
    pub fn record_task_attempt_wall(&self, wall: &TaskAttemptWall) -> Result<(), StoreError> {
        self.record_wall(wall, None)
    }

    /// Persist native observation and its effective cumulative charge in the same transaction.
    /// Absence retains prior facts; incompleteness is sticky without a whole-Attempt replacement.
    pub fn record_task_attempt_wall_with_observation(
        &self,
        wall: &TaskAttemptWall,
        observation: &TaskUsageObservationV1,
    ) -> Result<(), StoreError> {
        observation.validate().map_err(StoreError::Conflict)?;
        self.record_wall(wall, Some(observation))
    }

    pub fn task_attempt_usage_observation(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<Option<TaskUsageObservationV1>, StoreError> {
        let text: Option<String> = self.conn.query_row(
            "SELECT usage_observation_v1_json FROM attempt_wall WHERE run_id = ?1 AND attempt_id = ?2",
            rusqlite::params![run_id, attempt_id], |row| row.get(0),
        ).optional()?.flatten();
        text.map(|text| {
            let observation: TaskUsageObservationV1 = serde_json::from_str(&text)?;
            observation.validate().map_err(StoreError::Conflict)?;
            Ok(observation)
        })
        .transpose()
    }

    fn record_wall(
        &self,
        wall: &TaskAttemptWall,
        observation: Option<&TaskUsageObservationV1>,
    ) -> Result<(), StoreError> {
        fn bounded(value: u64, what: &str) -> Result<i64, StoreError> {
            i64::try_from(value).map_err(|_| {
                StoreError::Conflict(format!("attempt wall {what} exceeds SQLite range"))
            })
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT usage_v3_json, usage_observation_v1_json
             FROM attempt_wall WHERE run_id = ?1 AND attempt_id = ?2",
                rusqlite::params![wall.run_id, wall.attempt_id],
                |row| Ok((usage(row, 0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        let previous_observation = previous
            .as_ref()
            .and_then(|(_, text)| text.as_deref())
            .map(serde_json::from_str::<TaskUsageObservationV1>)
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
        merge_usage(&mut usage, previous.and_then(|(usage, _)| usage));
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
        let exact = usage.as_ref().map(serde_json::to_string).transpose()?;
        let started = bounded(wall.started_unix_ms, "start")?;
        let elapsed = bounded(wall.elapsed_ms, "elapsed")?;
        transaction.execute(
            "INSERT INTO attempt_wall (run_id, attempt_id, node_id, round, epoch,
                started_unix_ms, elapsed_ms, usage_v3_json, usage_observation_v1_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(run_id, attempt_id) DO UPDATE SET
                node_id=excluded.node_id, round=excluded.round, epoch=excluded.epoch,
                started_unix_ms=excluded.started_unix_ms, elapsed_ms=excluded.elapsed_ms,
                usage_v3_json=excluded.usage_v3_json,
                usage_observation_v1_json=excluded.usage_observation_v1_json",
            rusqlite::params![
                wall.run_id,
                wall.attempt_id,
                wall.node_id,
                i64::from(wall.round),
                i64::from(wall.epoch),
                started,
                elapsed,
                exact,
                observation
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every row of one run, ordered by start. A malformed usage or observation payload is an
    /// error for the whole read.
    pub fn task_attempt_wall(&self, run_id: &str) -> Result<Vec<TaskAttemptWall>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT attempt_id, node_id, round, epoch, started_unix_ms, elapsed_ms,
                usage_v3_json, usage_observation_v1_json
             FROM attempt_wall WHERE run_id = ?1 ORDER BY started_unix_ms, attempt_id",
        )?;
        let rows = statement.query_map([run_id], |row| {
            if let Some(text) = row.get::<_, Option<String>>(7)? {
                let observation: TaskUsageObservationV1 =
                    serde_json::from_str(&text).map_err(|error| text_error(7, error))?;
                observation
                    .validate()
                    .map_err(|error| text_error(7, std::io::Error::other(error)))?;
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
