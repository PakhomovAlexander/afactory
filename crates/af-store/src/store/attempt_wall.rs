//! Exact-usage sidecar: one row per Attempt holding its wall-clock interval, its cumulative
//! `TaskTokenUsage@3` floor and its native usage observation; and, beside it, the
//! `af/TaskAttemptWallEnd@1` of an Attempt its wall ended, kept before settlement so a lost
//! writer's successor settles that Attempt from it (ADR-0257).

use super::{EventStore, StoreError, TaskAttemptWall};
use af_contracts::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior};

use super::task::execution::PreparedTaskAttempt;
use crate::Cas;

struct SendResponse<'a> {
    cas: &'a Cas,
    attempt: &'a PreparedTaskAttempt,
    intent: &'a str,
    raw: &'a str,
    usage_known: bool,
}

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
        self.record_wall(wall, None, None)
    }

    /// Persist native observation and its effective cumulative charge in the same transaction.
    /// Absence retains prior facts; incompleteness is sticky without a whole-Attempt replacement.
    pub fn record_task_attempt_wall_with_observation(
        &self,
        wall: &TaskAttemptWall,
        observation: &TaskUsageObservationV1,
    ) -> Result<(), StoreError> {
        observation.validate().map_err(StoreError::Conflict)?;
        self.record_wall(wall, Some(observation), None)
    }

    /// Index an immutable send intent before the transport may contact the Provider.
    pub fn record_task_attempt_send(
        &self,
        cas: &Cas,
        attempt: &PreparedTaskAttempt,
        ordinal: u32,
        intent: &str,
        artifact: &str,
    ) -> Result<(), StoreError> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.check_send_attempt(cas, attempt)?;
        if super::task::now()? >= attempt.reservation().deadline_unix_ms {
            return Err(StoreError::Conflict("Send Attempt deadline elapsed".into()));
        }
        let run_id = super::task::task_run_id(attempt.task_id())?;
        super::ensure_send_layout(&transaction)?;
        let previous: Option<(String, u32, Option<bool>)> = transaction
            .query_row(
                "SELECT attempt_id, ordinal, usage_known FROM task_attempt_send_v1
             WHERE run_id=?1 AND node_id=?2 ORDER BY ordinal DESC LIMIT 1",
                [&run_id, attempt.node()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let admissible = match previous {
            None => ordinal == 1,
            Some((owner, last, known)) => {
                owner == attempt.id() && last.checked_add(1) == Some(ordinal) && known == Some(true)
            }
        };
        if !admissible {
            return Err(StoreError::Conflict(
                "Send does not follow a known response of this Attempt".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO task_attempt_send_v1
             (run_id, attempt_id, node_id, ordinal, intent_id, intent_artifact_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                run_id,
                attempt.id(),
                attempt.node(),
                ordinal,
                intent,
                artifact
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn check_send_attempt(
        &self,
        cas: &Cas,
        attempt: &PreparedTaskAttempt,
    ) -> Result<(), StoreError> {
        let task = self
            .task_projection(cas, attempt.task_id())?
            .ok_or_else(|| StoreError::Conflict("Send has no Task".into()))?;
        let now = super::task::now()?;
        if task.lease_epoch() != attempt.writer_epoch()
            || !task.lease_held_at(now)
            || !task.execution.as_ref().is_some_and(|run| {
                run.attempt_accounting().iter().any(|row| {
                    row.attempt_id == attempt.id()
                        && row.started
                        && !row.released
                        && row.result.is_none()
                        && row.context_id.as_deref() == Some(attempt.context_id())
                        && &row.reservation == attempt.reservation()
                })
            })
        {
            return Err(StoreError::Conflict(
                "Send has no current started Attempt".into(),
            ));
        }
        Ok(())
    }

    /// Index the drained response and its cumulative accounting floor in one commit.
    pub fn record_task_attempt_send_response(
        &self,
        cas: &Cas,
        attempt: &PreparedTaskAttempt,
        wall: &TaskAttemptWall,
        intent: &str,
        raw: &str,
        usage_known: bool,
    ) -> Result<(), StoreError> {
        if wall.run_id != super::task::task_run_id(attempt.task_id())?
            || wall.attempt_id != attempt.id()
            || wall.node_id != attempt.node()
        {
            return Err(StoreError::Conflict(
                "Response wall differs from its Attempt".into(),
            ));
        }
        self.record_wall(
            wall,
            None,
            Some(SendResponse {
                cas,
                attempt,
                intent,
                raw,
                usage_known,
            }),
        )
    }

    pub fn task_attempt_sends(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<Vec<(String, Option<String>)>, StoreError> {
        if !super::has_send_layout(&self.conn)? {
            return Ok(Vec::new());
        }
        let mut statement = self.conn.prepare(
            "SELECT intent_artifact_id, raw_artifact_id FROM task_attempt_send_v1
             WHERE run_id=?1 AND attempt_id=?2 ORDER BY ordinal",
        )?;
        Ok(statement
            .query_map([run_id, attempt_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    pub(super) fn task_attempt_has_unknown_send(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<bool, StoreError> {
        if !super::has_send_layout(&self.conn)? {
            return Ok(false);
        }
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_attempt_send_v1 WHERE run_id=?1 AND attempt_id=?2
             AND COALESCE(usage_known, 0)=0)",
            [run_id, attempt_id],
            |row| row.get(0),
        )?)
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
        response: Option<SendResponse<'_>>,
    ) -> Result<(), StoreError> {
        fn bounded(value: u64, what: &str) -> Result<i64, StoreError> {
            i64::try_from(value).map_err(|_| {
                StoreError::Conflict(format!("attempt wall {what} exceeds SQLite range"))
            })
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if let Some(response) = &response {
            self.check_send_attempt(response.cas, response.attempt)?;
        }
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
        let observation_text = observation
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
                observation_text
            ],
        )?;
        if let Some(SendResponse {
            intent,
            raw,
            usage_known,
            ..
        }) = response
        {
            let changed = transaction.execute(
                "UPDATE task_attempt_send_v1 SET raw_artifact_id=?1, usage_known=?2
                 WHERE run_id=?3 AND attempt_id=?4 AND intent_id=?5 AND raw_artifact_id IS NULL",
                rusqlite::params![raw, usage_known, wall.run_id, wall.attempt_id, intent],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "Response lacks an unsettled send intent".into(),
                ));
            }
        }
        transaction.commit()?;
        let merged = TaskAttemptWall {
            usage,
            ..wall.clone()
        };
        self.wall_committed(&merged, observation.as_ref());
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

    /// Keep the `af/TaskAttemptWallEnd@1` an Attempt's wall left before anything else of the
    /// Attempt is published (ADR-0257): a writer lost before settlement leaves its successor the
    /// wall end and the partial tree it names, not only an unsettled Attempt. The artifact must
    /// already be durable in the CAS. One Attempt keeps one wall end; recording another is a
    /// conflict.
    pub fn record_task_attempt_wall_end(
        &self,
        run_id: &str,
        attempt_id: &str,
        wall_end_id: &str,
    ) -> Result<(), StoreError> {
        let changed = self.conn.execute(
            "INSERT INTO attempt_wall_end (run_id, attempt_id, wall_end_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(run_id, attempt_id) DO NOTHING",
            rusqlite::params![run_id, attempt_id, wall_end_id],
        )?;
        if changed == 0
            && self.task_attempt_wall_end(run_id, attempt_id)?.as_deref() != Some(wall_end_id)
        {
            return Err(StoreError::Conflict(
                "Task Attempt already recorded another wall end".into(),
            ));
        }
        Ok(())
    }

    /// The wall end recorded for one Attempt, when its wall ended it.
    pub fn task_attempt_wall_end(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT wall_end_id FROM attempt_wall_end WHERE run_id = ?1 AND attempt_id = ?2",
                rusqlite::params![run_id, attempt_id],
                |row| row.get(0),
            )
            .optional()?)
    }
}
