//! Resuming a Task whose run was lost or interrupted (ADR-0197). `af task run` waits out a
//! dead run's lease instead of refusing it, says on its first line what it does with the
//! interrupted node or why it cannot continue, and `af task show` and `af task list` name such a
//! Task `abandoned` with its cause and the command that continues it.

use review_graph::task::CompiledOperator;
use review_pipeline::task::lease::{LeaseTiming, TASK_LEASE_MS, WRITER_LOST};
use review_store::store::task::execution::{TaskAbandonCause, TaskNodeRecovery};
use review_store::store::task::{TaskLease, TaskProjection};
use review_store::{Cas, EventStore};
use std::collections::BTreeSet;
use std::time::Duration;

use super::preview;

/// The lease `af task run` and `af task start` take for a Task's execution. Debug builds read
/// `AF_TEST_TASK_LEASE_MS` (6,000 ms at least), so a fixture that kills a run need not wait out
/// the production lease; production release binaries never read it.
pub(crate) fn lease_ms() -> u64 {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("AF_TEST_TASK_LEASE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|ms| *ms >= 6_000)
    {
        return ms;
    }
    TASK_LEASE_MS
}

/// The heartbeat cadence of [`lease_ms`].
pub(crate) fn timing() -> LeaseTiming {
    LeaseTiming::for_lease(lease_ms())
}

/// A Unix time in milliseconds as its UTC time of day, `17:41:03Z`.
fn clock_of(unix_ms: u64) -> String {
    let seconds = unix_ms / 1_000 % 86_400;
    format!(
        "{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn times(count: u32) -> String {
    if count == 1 {
        "1 time".into()
    } else {
        format!("{count} times")
    }
}

/// Take the Task's lease for `writer`. A lease its previous run still holds is waited out, at
/// most one lease length, with one line saying so: a run killed without releasing it (a closed
/// lid, SIGKILL, a reboot) holds it until it expires. A lease renewed while waiting belongs to a
/// run that is still working, which is refused with what to do.
pub(crate) fn take_lease(
    cas: &Cas,
    store: &mut EventStore,
    id: &str,
    writer: &str,
) -> Result<TaskLease, String> {
    let lease_ms = lease_ms();
    let mut seen: Option<(String, u64, u64)> = None;
    loop {
        let state = store
            .task_projection(cas, id)
            .map_err(|e| e.to_string())?
            .ok_or("Unknown Task")?;
        let now = super::clock()?;
        if !state.lease_held_at(now) {
            return store
                .take_task_lease(cas, id, writer, lease_ms)
                .map_err(|e| e.to_string());
        }
        let (holder, epoch) = state.lease_writer();
        let until = state.lease_until_unix_ms();
        match &seen {
            None => {
                eprintln!(
                    "resuming Task {}: its previous run's lease (writer {}) is held until {}; \
                     waiting {} s for it to expire",
                    preview::text(id),
                    preview::text(holder),
                    clock_of(until),
                    until.saturating_sub(now).div_ceil(1_000)
                );
                seen = Some((holder.to_owned(), epoch, until));
            }
            Some((seen_holder, seen_epoch, seen_until))
                if seen_holder != holder || *seen_epoch != epoch || until > *seen_until =>
            {
                return Err(format!(
                    "cannot continue Task {id}: another run is working on it (writer {holder} \
                     renewed its lease until {}); let that run finish or stop it, then run \
                     `af task run {id}`",
                    clock_of(until),
                    id = preview::text(id),
                    holder = preview::text(holder),
                ));
            }
            Some(_) => {}
        }
        if crate::interrupt::received().is_some() {
            return Err(crate::interrupt::INTERRUPTED.into());
        }
        std::thread::sleep(Duration::from_millis(
            until.saturating_sub(now).clamp(1, 250),
        ));
    }
}

/// The phrase for a node's abandoned Attempts: `lost its writer 2 times`, `was interrupted 1
/// time`, or both.
fn losses(recovery: &TaskNodeRecovery) -> String {
    match (recovery.writer_lost, recovery.host_cancelled) {
        (lost, 0) => format!("lost its writer {}", times(lost)),
        (0, cancelled) => format!("was interrupted {}", times(cancelled)),
        (lost, cancelled) => format!(
            "lost its writer {} and was interrupted {}",
            times(lost),
            times(cancelled)
        ),
    }
}

/// The local name of the slot `node` runs, for the `[slots.<name>]` a fix names.
fn slot_of(state: &TaskProjection, node: &str) -> Option<String> {
    let execution = state.execution.as_ref()?;
    let CompiledOperator::Primitive { operator, .. } = &execution.graph.nodes.get(node)?.operator
    else {
        return None;
    };
    use review_core::task::pipeline::TaskOperatorV1::{FixVerify, Verify, Worker};
    let (Worker { slot } | Verify { slot } | FixVerify { slot }) = operator else {
        return None;
    };
    Some(slot.rsplit(".slots.").next().unwrap_or(slot).to_owned())
}

/// What a Task whose node used up its recovery allowance needs: a new Task, or a pipeline that
/// allows more recoveries.
fn exhausted_fix(state: &TaskProjection, recovery: &TaskNodeRecovery) -> String {
    let slot = slot_of(state, &recovery.node).unwrap_or_else(|| "<slot>".into());
    format!(
        "start it again with `af task start --file TASK_FILE` (the Task file it was started \
         from), or allow more with `[slots.{}] recovery_attempts` in its pipeline \
         (af.pipeline/3)",
        preview::text(&slot)
    )
}

/// Every node without an output whose abandoned Attempts used up its recovery allowance.
fn exhausted(state: &TaskProjection) -> Vec<TaskNodeRecovery> {
    let Some(execution) = state.execution.as_ref() else {
        return Vec::new();
    };
    execution
        .recoveries()
        .into_iter()
        .filter(|recovery| recovery.exhausted() && !execution.outputs.contains_key(&recovery.node))
        .collect()
}

/// The nodes of `state` whose latest Attempt was abandoned and which have no output yet: the
/// ones a resumed run dispatches again.
fn interrupted(state: &TaskProjection) -> Vec<TaskNodeRecovery> {
    let Some(execution) = state.execution.as_ref() else {
        return Vec::new();
    };
    let accounting = execution.attempt_accounting();
    execution
        .recoveries()
        .into_iter()
        .filter(|recovery| {
            !execution.outputs.contains_key(&recovery.node)
                && accounting
                    .iter()
                    .filter(|row| row.started && row.reservation.node == recovery.node)
                    .max_by_key(|row| row.started_unix_ms)
                    .is_some_and(|row| row.abandoned.is_some())
        })
        .collect()
}

/// Refuse to continue a Task one of whose nodes used up its recovery allowance, or print the
/// line saying which interrupted node a resumed run dispatches again. `before` is the Task
/// before its new writer settled the lost writer's Attempts, `after` the Task after.
pub(super) fn announce(
    id: &str,
    before: &TaskProjection,
    after: &TaskProjection,
) -> Result<(), String> {
    if let Some(recovery) = exhausted(after).first() {
        return Err(format!(
            "cannot continue Task {}: node {} {} (recovery allowance {}); fix: {}",
            preview::text(id),
            preview::text(&recovery.node),
            losses(recovery),
            recovery.allowance,
            exhausted_fix(after, recovery)
        ));
    }
    let orphaned: BTreeSet<String> = before
        .execution
        .iter()
        .flat_map(|execution| execution.orphaned_attempts())
        .map(|(node, _)| node)
        .collect();
    for recovery in interrupted(after) {
        let at = recovery
            .last_abandoned_unix_ms
            .map_or_else(|| "an unknown time".into(), clock_of);
        let count = format!(
            "{} recovery of {}",
            recovery.abandoned(),
            recovery.allowance
        );
        if orphaned.contains(&recovery.node) {
            eprintln!(
                "resuming Task {}: node {} was interrupted at {at} (its writer was lost); \
                 settling the lost Attempt and dispatching a new one ({count})",
                preview::text(id),
                preview::text(&recovery.node),
            );
        } else {
            eprintln!(
                "resuming Task {}: node {} was interrupted at {at}; dispatching a new Attempt \
                 ({count})",
                preview::text(id),
                preview::text(&recovery.node),
            );
        }
    }
    Ok(())
}

/// A Task whose last run ended without finishing it, with nothing working on it now: what
/// `af task show` and `af task list` print as `phase abandoned`.
pub(crate) struct Abandoned {
    pub cause: TaskAbandonCause,
    pub node: String,
    pub at_unix_ms: Option<u64>,
    pub next_action: String,
}

/// Whether `state` is abandoned at `now_unix_ms`: unfinished, its lease neither held nor
/// renewed, and either a started Attempt nobody settled (its writer was lost) or a node whose
/// latest Attempt was abandoned and that has no output.
pub(crate) fn abandoned(state: &TaskProjection, now_unix_ms: u64) -> Option<Abandoned> {
    if matches!(state.phase, review_core::task::TaskPhaseV1::Finished { .. })
        || state.lease_held_at(now_unix_ms)
    {
        return None;
    }
    let execution = state.execution.as_ref()?;
    let id = preview::text(&state.task_id);
    let next = format!("af task run {id}");
    if let Some((node, _)) = execution.orphaned_attempts().into_iter().next() {
        return Some(Abandoned {
            cause: TaskAbandonCause::WriterLost,
            node,
            at_unix_ms: Some(state.last_recorded_unix_ms()),
            next_action: next,
        });
    }
    let recovery = interrupted(state)
        .into_iter()
        .max_by_key(|recovery| recovery.last_abandoned_unix_ms)?;
    let cause = execution
        .attempt_accounting()
        .into_iter()
        .filter(|row| row.started && row.reservation.node == recovery.node)
        .max_by_key(|row| row.started_unix_ms)
        .and_then(|row| row.abandoned)?;
    Some(Abandoned {
        cause,
        node: recovery.node.clone(),
        at_unix_ms: recovery.last_abandoned_unix_ms,
        next_action: if recovery.exhausted() {
            "af task start --file TASK_FILE".into()
        } else {
            next
        },
    })
}

impl Abandoned {
    /// `af task show`'s lines for it.
    pub(crate) fn lines(&self) -> [String; 2] {
        [
            format!(
                "phase abandoned (cause {}): node {} stopped at {}",
                self.cause.as_str(),
                preview::text(&self.node),
                self.at_unix_ms
                    .map_or_else(|| "an unknown time".into(), clock_of)
            ),
            format!("next: {}", self.next_action),
        ]
    }

    /// The fields `af/task-inspection@12` and `af/task-list-entry@3` carry for it.
    pub(crate) fn record(&self, document: &mut serde_json::Value) {
        document["cause"] = self.cause.as_str().into();
        document["next_action"] = self.next_action.clone().into();
    }
}

/// The message of a run whose heartbeat lost the writer lease, or None for any other error.
pub(super) fn writer_lost(id: &str, error: &str) -> Option<String> {
    let detail = error.strip_prefix(WRITER_LOST)?;
    let detail = detail.trim_start_matches(':').trim();
    Some(format!(
        "Task {id} lost its writer lease (writer_lost: {detail}); its Worker processes were \
         stopped and nothing more was recorded. To continue, run `af task run {id}`: it waits \
         for the lease to expire (at most {} s), settles the lost Attempt without charging its \
         reservation, and dispatches it again",
        lease_ms().div_ceil(1_000),
        id = preview::text(id),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_of_is_the_utc_time_of_day() {
        assert_eq!(clock_of(0), "00:00:00Z");
        assert_eq!(clock_of(63_663_000), "17:41:03Z");
        assert_eq!(clock_of(86_400_000 + 1_000), "00:00:01Z");
    }

    fn recovery(writer_lost: u32, host_cancelled: u32) -> TaskNodeRecovery {
        TaskNodeRecovery {
            node: "root.nodes.review.bugs".into(),
            chargeable: 0,
            writer_lost,
            host_cancelled,
            allowance: 1,
            last_abandoned_unix_ms: None,
        }
    }

    #[test]
    fn losses_name_each_cause_and_count() {
        assert_eq!(losses(&recovery(2, 0)), "lost its writer 2 times");
        assert_eq!(losses(&recovery(0, 1)), "was interrupted 1 time");
        assert_eq!(
            losses(&recovery(1, 1)),
            "lost its writer 1 time and was interrupted 1 time"
        );
    }

    #[test]
    fn a_lost_writer_error_says_what_happened_and_what_to_run() {
        let message = writer_lost(
            "pagination-cli",
            &format!(
                "{WRITER_LOST}: Task writer lease is expired or fenced: observed at 2, expiry 1, \
                 last recorded 1"
            ),
        )
        .unwrap();
        assert!(
            message.starts_with(
                "Task pagination-cli lost its writer lease (writer_lost: Task writer lease is \
                 expired or fenced: observed at 2, expiry 1, last recorded 1)"
            ),
            "{message}"
        );
        assert!(
            message.contains("run `af task run pagination-cli`"),
            "{message}"
        );
        assert!(writer_lost("x", "Task execution was interrupted").is_none());
    }

    #[test]
    fn the_production_lease_is_the_heartbeats() {
        if std::env::var_os("AF_TEST_TASK_LEASE_MS").is_none() {
            assert_eq!(lease_ms(), TASK_LEASE_MS);
            assert_eq!(timing(), LeaseTiming::PRODUCTION);
        }
    }
}
