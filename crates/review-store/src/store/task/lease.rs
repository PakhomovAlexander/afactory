//! Fresh writer liveness for heartbeat polling, without loading execution authority.
use super::*;

impl EventStore {
    /// Read the exact durable writer/epoch, expiry and latest policy clock in one SQLite
    /// snapshot. This grants no dispatch, publication, renewal or recovery authority. All of
    /// those operations still validate the complete Task projection and its current CAS refs.
    /// No lease fact or integrity result is retained between calls.
    pub fn task_lease_state(&self, lease: &TaskLease) -> Result<u64, StoreError> {
        let run_id = task_run_id(lease.task_id())?;
        // Lease changes are frozen TaskTransition@1 records. Include the latest transition
        // of any generation and the stream tail (which may be a late Broker receipt), so a
        // changed writer or future policy clock cannot hide behind an earlier lease row.
        let mut query = self.conn.prepare(
            "WITH lease AS (
                SELECT sequence FROM events WHERE run_id=?1 AND type='TaskTransition@1'
                AND json_extract(payload,'$.change.kind') IN
                    ('opened','lease_taken','lease_renewed','lease_released')
                ORDER BY sequence DESC LIMIT 1
             ), transition AS (
                SELECT sequence FROM events WHERE run_id=?1 AND type IN
                    ('TaskTransition@1','TaskTransition@2','TaskTransition@3','TaskTransition@4',
                     'TaskTransition@5')
                ORDER BY sequence DESC LIMIT 1
             ), tail AS (
                SELECT sequence FROM events WHERE run_id=?1 ORDER BY sequence DESC LIMIT 1
             )
             SELECT sequence,type,payload,node_id,attempt_id,causation_id,correlation_id
             FROM events WHERE run_id=?1 AND sequence IN
                (0,(SELECT sequence FROM lease),(SELECT sequence FROM transition),
                   (SELECT sequence FROM tail)) ORDER BY sequence",
        )?;
        let rows = query.query_map([&run_id], |row| {
            Ok((
                crate::store::u64_column(row, 0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                (3..7).all(|column| {
                    row.get::<_, Option<String>>(column)
                        .is_ok_and(|v| v.is_none())
                }),
            ))
        })?;
        let mut expiry = None;
        let mut clock = 0;
        let mut writer = None;
        let mut genesis = false;
        for row in rows {
            let (sequence, kind, raw, plain) = row?;
            if !plain {
                return Err(conflict(
                    "Task lease observation has foreign event metadata",
                ));
            }
            let event_type = kind
                .parse::<EventType>()
                .map_err(|e| conflict(e.to_string()))?;
            let payload = serde_json::from_str(&raw)?;
            let time = if event_type == EventType::TaskBrokerTransitionV1 {
                let value: task::broker::TaskBrokerTransitionV1 = serde_json::from_value(payload)?;
                value.validate().map_err(conflict)?;
                value.now_unix_ms
            } else {
                let event = RunEvent {
                    event_id: String::new(),
                    run_id: run_id.clone(),
                    sequence,
                    event_type,
                    occurred_at: String::new(),
                    node_id: None,
                    attempt_id: None,
                    causation_id: None,
                    correlation_id: None,
                    artifact_refs: vec![],
                    payload,
                };
                let value = read_task_transition(&event)?;
                if sequence == 0 {
                    if !matches!(value.change, TaskChangeV1::Opened { .. }) || value.epoch != 1 {
                        return Err(conflict("Invalid Task lease genesis"));
                    }
                    genesis = true;
                }
                let owner = (value.writer, value.epoch);
                match value.change {
                    TaskChangeV1::Opened {
                        lease_until_unix_ms,
                        ..
                    }
                    | TaskChangeV1::LeaseTaken {
                        lease_until_unix_ms,
                    }
                    | TaskChangeV1::LeaseRenewed {
                        lease_until_unix_ms,
                    } => {
                        expiry = Some(lease_until_unix_ms);
                        writer = Some(owner);
                    }
                    TaskChangeV1::LeaseReleased {} => {
                        expiry = Some(value.now_unix_ms);
                        writer = Some(owner);
                    }
                    _ if writer.as_ref() != Some(&owner) => {
                        return Err(conflict("Task writer lease is expired or fenced"));
                    }
                    _ => {}
                }
                value.now_unix_ms
            };
            if time < clock {
                return Err(conflict("Task lease observation clock moved backwards"));
            }
            clock = time;
        }
        let until = expiry
            .filter(|_| genesis)
            .ok_or_else(|| conflict("Unknown Task lease"))?;
        let time = now()?;
        if writer.as_ref() != Some(&(lease.writer.clone(), lease.epoch))
            || time >= until
            || time < clock
        {
            return Err(conflict("Task writer lease is expired or fenced"));
        }
        Ok(until)
    }
}
