//! `af task gc` and `af task list --sizes` (ADR-0135): Store hygiene over the common Task Store.
//!
//! The Store owns every rule — reachability, the collection plan, the tombstone and the sweep;
//! this module resolves the state directory and prints what the Store decided. A preview opens
//! the Store read-only and writes nothing.

use std::path::Path;

use review_core::task::collection::collected_time;
use review_store::store::task::collection::{
    CollectedTask, CollectionCandidate, CollectionDisposition, CollectionOutcome, CollectionPlan,
    StoreTotals, TaskFootprint,
};
use review_store::{Cas, EventStore};
use serde_json::{Value, json};

use super::{state_path, store_present};

const DAY_MS: u64 = 86_400_000;

/// `af task gc --older-than DAYS --keep N [--apply]`.
pub(crate) fn gc(
    older_than_days: u64,
    keep: usize,
    apply: bool,
    repo: &Path,
    state: Option<&Path>,
    json_output: bool,
) -> Result<i32, String> {
    let (_, state) = state_path(repo, state)?;
    if !store_present(&state)? {
        return Err(format!("Task state {} holds no Store", state.display()));
    }
    let older_than_ms = older_than_days
        .checked_mul(DAY_MS)
        .ok_or("--older-than is too large")?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let (plan, outcome) = if apply {
        let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        let outcome = store
            .apply_task_collection(&cas, older_than_ms, keep, stop_after_tombstones())
            .map_err(|e| e.to_string())?;
        (outcome.plan.clone(), Some(outcome))
    } else {
        let store =
            EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        let plan = store
            .plan_task_collection(&cas, older_than_ms, keep)
            .map_err(|e| e.to_string())?;
        (plan, None)
    };
    if json_output {
        let document = document(&plan, older_than_days, outcome.as_ref());
        println!(
            "{}",
            serde_json::to_string(&document).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", text(&plan, outcome.as_ref()));
    }
    Ok(0)
}

/// Deterministic command-path fixtures stop `--apply` between the tombstones and the sweep, as
/// a process ended there would. Production release binaries never read this setting.
fn stop_after_tombstones() -> bool {
    #[cfg(debug_assertions)]
    {
        std::env::var_os("AF_TEST_GC_STOP_AFTER_TOMBSTONES").is_some_and(|value| value == "1")
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

fn totals(totals: &StoreTotals) -> Value {
    json!({
        "objects": totals.objects,
        "bytes": totals.bytes,
        "unreachable_objects": totals.unreachable_objects,
        "unreachable_bytes": totals.unreachable_bytes,
    })
}

pub(super) fn footprint(footprint: &TaskFootprint) -> Value {
    json!({
        "exclusive_objects": footprint.exclusive_objects,
        "exclusive_bytes": footprint.exclusive_bytes,
        "shared_objects": footprint.shared_objects,
        "shared_bytes": footprint.shared_bytes,
    })
}

fn candidate(task: &CollectionCandidate) -> Value {
    let mut candidate = json!({
        "task_id": task.task_id,
        "kind": task.kind,
        "outcome": task.outcome,
        "chargeable_tokens": task.chargeable_tokens,
        "last_event_unix_ms": task.last_event_unix_ms,
        "disposition": task.disposition,
        "sizes": footprint(&task.footprint),
        "collected_bytes": task.collected_bytes,
    });
    // Unknown usage is never shown as spend (ADR-0143); this is the count a tombstone keeps.
    if task.unknown_usage_attempts > 0 {
        candidate["unknown_usage_attempts"] = json!(task.unknown_usage_attempts);
    }
    candidate
}

/// The `af/task-gc@1` document.
fn document(
    plan: &CollectionPlan,
    older_than_days: u64,
    outcome: Option<&CollectionOutcome>,
) -> Value {
    json!({
        "schema": "af/task-gc@1",
        "apply": outcome.is_some(),
        "older_than_days": older_than_days,
        "keep": plan.keep,
        "now_unix_ms": plan.now_unix_ms,
        "store": totals(&plan.totals),
        "reclaimable_objects": plan.reclaimable_objects,
        "reclaimable_bytes": plan.reclaimable_bytes,
        "live_writers": plan
            .live_writers
            .iter()
            .map(|(task_id, until)| json!({"task_id": task_id, "until_unix_ms": until}))
            .collect::<Vec<_>>(),
        "tasks": plan.tasks.iter().map(candidate).collect::<Vec<_>>(),
        "collected": plan
            .collected
            .iter()
            .map(|task| &task.collected)
            .collect::<Vec<_>>(),
        "applied": outcome.map(|outcome| json!({
            "tombstoned": outcome.tombstoned,
            "swept": outcome.swept,
            "removed_objects": outcome.removed_objects,
            "removed_bytes": outcome.removed_bytes,
        })),
    })
}

fn disposition(value: &CollectionDisposition, keep: usize, older_than_ms: u64) -> String {
    match value {
        CollectionDisposition::Collect => "collect".into(),
        CollectionDisposition::KeptNewest => format!("kept: among the newest {keep}"),
        CollectionDisposition::KeptRecent => {
            format!("kept: last event within {} days", older_than_ms / DAY_MS)
        }
        CollectionDisposition::Running => "never collected: running".into(),
        CollectionDisposition::Unfinished => "never collected: not finished".into(),
        CollectionDisposition::WriterLease { until_unix_ms } => format!(
            "never collected: writer lease until {}",
            collected_time(*until_unix_ms)
        ),
        CollectionDisposition::BoundBy { tasks } => {
            format!("never collected: bound by {}", tasks.join(", "))
        }
    }
}

fn text(plan: &CollectionPlan, outcome: Option<&CollectionOutcome>) -> String {
    let mut out = format!(
        "Store: {} objects, {} bytes ({} bytes unreachable)\n",
        plan.totals.objects, plan.totals.bytes, plan.totals.unreachable_bytes
    );
    for task in &plan.tasks {
        let bytes = if task.disposition.collects() {
            format!("  {} bytes", task.collected_bytes)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{}  {}  last event {}{bytes}\n",
            task.task_id,
            disposition(&task.disposition, plan.keep, plan.older_than_ms),
            collected_time(task.last_event_unix_ms),
        ));
    }
    for task in &plan.collected {
        out.push_str(&format!(
            "{}  collected {}\n",
            task.collected.task_id,
            collected_time(task.collected.collected_unix_ms)
        ));
    }
    for (task_id, until) in &plan.live_writers {
        out.push_str(&format!(
            "live writer: {task_id} holds its lease until {}; --apply is refused until it ends\n",
            collected_time(*until)
        ));
    }
    match outcome {
        None => out.push_str(&format!(
            "Would reclaim {} objects, {} bytes. Nothing was written; run with --apply to collect.\n",
            plan.reclaimable_objects, plan.reclaimable_bytes
        )),
        Some(outcome) => {
            if outcome.tombstoned.is_empty() {
                out.push_str("Collected no Task.\n");
            } else {
                out.push_str(&format!("Collected {}.\n", outcome.tombstoned.join(", ")));
            }
            if outcome.swept {
                out.push_str(&format!(
                    "Removed {} objects, {} bytes.\n",
                    outcome.removed_objects, outcome.removed_bytes
                ));
            } else {
                out.push_str("The sweep did not run; the next `af task gc --apply` finishes it.\n");
            }
        }
    }
    out
}

/// The `af/task-list-entry@2` of a collected Task: its retained summary, the result its log
/// named, and `collected`; nothing artifact-backed. The unknown-usage count is the tombstone's,
/// so the collected Task still lists `(+N unknown)` (ADR-0143).
pub(super) fn list_entry(task: &CollectedTask) -> Value {
    let mut entry = json!({
        "schema": "af/task-list-entry@2",
        "task_id": task.collected.task_id,
        "kind": task.collected.kind,
        "phase": {"kind": "finished", "result_id": task.result_id},
        "outcome": task.collected.outcome,
        "chargeable_tokens": task.collected.chargeable_tokens,
        "derived_snapshot_id": null,
        "delivery": null,
        "collected": task.collected,
    });
    if task.collected.unknown_usage_attempts > 0 {
        entry["unknown_usage_attempts"] = json!(task.collected.unknown_usage_attempts);
    }
    entry
}

/// The `af/task-collected-inspection@1` document `af task show --json` prints for a collected
/// Task in place of the artifact-backed inspection.
pub(super) fn inspection(task: &CollectedTask) -> Value {
    json!({
        "schema": "af/task-collected-inspection@1",
        "task_id": task.collected.task_id,
        "phase": {"kind": "finished", "result_id": task.result_id},
        "collected": task.collected,
    })
}

/// `af task show` of a collected Task.
pub(super) fn present(task: &CollectedTask, json_output: bool) -> Result<(), String> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&inspection(task)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let collected = &task.collected;
    println!(
        "Task {}: collected {}",
        collected.task_id,
        collected_time(collected.collected_unix_ms)
    );
    println!("  kind: {}", collected.kind);
    println!("  revision: {}", collected.revision_id);
    println!("  result: {}", task.result_id);
    println!("  outcome: {}", collected.outcome);
    // Unknown usage is never shown as spend, even after collection (ADR-0143).
    let unknown = match collected.unknown_usage_attempts {
        0 => String::new(),
        n => format!(" (+{n} unknown)"),
    };
    println!(
        "  chargeable tokens: {}{unknown}",
        collected.chargeable_tokens
    );
    println!(
        "  last event: {}",
        collected_time(collected.last_event_unix_ms)
    );
    println!("  bytes collected: {}", collected.collected_bytes);
    Ok(())
}
