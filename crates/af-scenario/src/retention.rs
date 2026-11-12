//! Native Ant retention. The cursor is dispatch authority; expired receipts are not replay input.
use crate::{contract::*, schedule::*, state::*, store::World};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const RECENT: usize = 32;
const BUCKETS: usize = 64;
// Keep global transaction overhead outside all byte leases. Each Run owns its remaining
// lifecycle/response capacity; only that Run can consume it.
pub(crate) const ADMISSION_BYTES: u64 = MAX_BYTES - 512 * 1024;
const RUN_HEADROOM: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChargeBucket {
    pub settled_unix_ms: u64,
    pub tokens: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    /// Counts individual scheduled indices, not the number of range receipts.
    pub outcomes: BTreeMap<String, u64>,
    pub terminal_runs: u64,
    pub mode_changes: u64,
    /// Decimal u128: cumulative charges need not fit a JSON safe integer.
    pub charged_tokens: String,
    pub through_index: Option<i64>,
    /// Hash chain over exact expired receipt and Run identities/content before deletion.
    pub provenance: String,
    pub rolling_charges: Vec<ChargeBucket>,
}
impl Retention {
    pub fn new() -> Self {
        Self {
            outcomes: BTreeMap::new(),
            terminal_runs: 0,
            mode_changes: 0,
            charged_tokens: "0".into(),
            through_index: None,
            provenance: digest(b"af/ant-retention@1"),
            rolling_charges: Vec::new(),
        }
    }
    pub fn validate(&self, next: i64) -> Result<()> {
        let charged = self
            .charged_tokens
            .parse::<u128>()
            .map_err(|_| "Invalid retention accounting")?;
        require(
            charged.to_string() == self.charged_tokens
                && af_contracts::is_digest(&self.provenance)
                && self.through_index.is_none_or(|n| n < next)
                && self.outcomes.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "started"
                            | "skipped_busy"
                            | "skipped_missed"
                            | "skipped_paused"
                            | "skipped_disabled"
                            | "skipped_budget"
                            | "skipped_missing_time"
                            | "skipped_roadmap"
                            | "skipped_capacity"
                    )
                })
                && self.terminal_runs == self.outcomes.get("started").copied().unwrap_or(0)
                && self.rolling_charges.len() <= BUCKETS
                && self.rolling_charges.iter().all(|b| {
                    b.tokens
                        .parse::<u128>()
                        .is_ok_and(|n| n > 0 && n.to_string() == b.tokens)
                        && b.settled_unix_ms > 0
                        && b.settled_unix_ms <= MAX_TIME
                })
                && self
                    .rolling_charges
                    .windows(2)
                    .all(|w| w[0].settled_unix_ms <= w[1].settled_unix_ms),
            "Corrupt Ant retention summary",
        )
    }
    fn compact_buckets(&mut self) -> Result<()> {
        self.rolling_charges.sort_by_key(|b| b.settled_unix_ms);
        while self.rolling_charges.len() > BUCKETS {
            let first = self.rolling_charges.remove(0);
            let next = &mut self.rolling_charges[0];
            // Moving an older charge forward is conservative; never expire a charge early.
            next.tokens = next
                .tokens
                .parse::<u128>()
                .map_err(|_| "Invalid rolling charge")?
                .checked_add(
                    first
                        .tokens
                        .parse::<u128>()
                        .map_err(|_| "Invalid rolling charge")?,
                )
                .ok_or("Ant rolling accounting overflow")?
                .to_string();
        }
        Ok(())
    }
    pub fn rolling(&self, now: u64, window: u64) -> Result<u64> {
        let exact = self
            .rolling_charges
            .iter()
            .filter(|b| b.settled_unix_ms > now.saturating_sub(window))
            .try_fold(0u128, |n, b| {
                n.checked_add(
                    b.tokens
                        .parse::<u128>()
                        .map_err(|_| "Invalid rolling charge")?,
                )
                .ok_or("Ant rolling accounting overflow")
            })?;
        // Admission only needs an upper bound relative to a safe-integer budget. The
        // durable bucket keeps the exact decimal sum even after a native overrun.
        Ok(u64::try_from(exact).unwrap_or(u64::MAX))
    }
}
impl Default for Retention {
    fn default() -> Self {
        Self::new()
    }
}

/// Uncertainty, pending effects and native goal provenance never expire. In particular a
/// cancelled Run can still own an uncertain intent or an unconfirmed child cancellation.
pub fn protected(run: &Run) -> bool {
    lifecycle_open(run) || run.goal_claim.is_some()
}
fn lifecycle_open(run: &Run) -> bool {
    run.status.active()
        || run.waiting.is_some()
        || run.task_charges.values().any(TaskCharge::uncertain)
        || run
            .frames
            .values()
            .flat_map(|f| &f.steps)
            .flat_map(|p| &p.intents)
            .any(|i| matches!(i.phase, Phase::Prepared | Phase::Uncertain))
        || run.cancellation.as_ref().is_some_and(|c| {
            c.child_requests
                .values()
                .any(|r| r.state != CancellationState::Confirmed)
        })
}

fn planning_only_failure(run: &Run) -> bool {
    run.status == Status::Failed
        && run
            .frames
            .values()
            .flat_map(|f| &f.steps)
            .flat_map(|p| &p.intents)
            .any(|i| matches!(i.phase, Phase::Prepared | Phase::Uncertain))
        && run.frames.values().all(|frame| {
            let def = &run.pin.bundle.definitions[&frame.scenario];
            frame.steps.iter().zip(&def.steps).all(|(p, step)| {
                p.intents.iter().all(|intent| {
                    !matches!(intent.phase, Phase::Prepared | Phase::Uncertain)
                        || (matches!(step.operation, Kind::FreshTask { .. })
                            && !run.fresh_tasks.contains_key(&intent.id))
                })
            })
        })
}

pub fn compact(world: &mut World) -> Result<()> {
    for ant in world.ants.values_mut() {
        let recent_from = ant.occurrences.len().saturating_sub(RECENT);
        let latest = ant.occurrences.iter().rev().find_map(|o| match &o.outcome {
            OccurrenceOutcome::Started { run_id, .. } => Some(run_id.clone()),
            _ => None,
        });
        let mut retained = Vec::new();
        for (index, occurrence) in std::mem::take(&mut ant.occurrences).into_iter().enumerate() {
            let run_id = match &occurrence.outcome {
                OccurrenceOutcome::Started { run_id, .. } => Some(run_id),
                _ => None,
            };
            if index >= recent_from
                || run_id.is_some_and(|id| {
                    latest.as_ref() == Some(id) || world.runs.get(id).is_none_or(protected)
                })
            {
                retained.push(occurrence);
                continue;
            }
            let summary = &mut ant.retention;
            let run = run_id.and_then(|id| world.runs.remove(id));
            summary.provenance = hash(&(&summary.provenance, &occurrence, &run))?;
            summary.through_index =
                Some(summary.through_index.map_or(occurrence.through_index, |n| {
                    n.max(occurrence.through_index)
                }));
            let kind =
                serde_json::to_value(&occurrence.outcome).map_err(|e| e.to_string())?["kind"]
                    .as_str()
                    .ok_or("Invalid occurrence kind")?
                    .to_string();
            let count = u64::try_from(occurrence.through_index - occurrence.local_day + 1)
                .map_err(|_| "Invalid retained occurrence range")?;
            let total = summary.outcomes.entry(kind).or_default();
            *total = total.checked_add(count).ok_or("Retention count overflow")?;
            if let Some(run) = run {
                summary.terminal_runs = summary
                    .terminal_runs
                    .checked_add(1)
                    .ok_or("Retention count overflow")?;
                let tokens = run.task_charges.values().try_fold(0u128, |n, c| {
                    n.checked_add(u128::from(c.charged.unwrap_or(0)))
                        .ok_or("Retention charge overflow")
                })?;
                summary.charged_tokens = summary
                    .charged_tokens
                    .parse::<u128>()
                    .map_err(|_| "Invalid retention charge")?
                    .checked_add(tokens)
                    .ok_or("Retention charge overflow")?
                    .to_string();
                let tokens = run.accounted_tokens()?;
                if tokens > 0 {
                    summary.rolling_charges.push(ChargeBucket {
                        settled_unix_ms: run.updated_unix_ms,
                        tokens: tokens.to_string(),
                    });
                }
            }
        }
        ant.occurrences = retained;
        let next_due = ant.next_due_unix_ms()?;
        let summary = &mut ant.retention;
        summary.rolling_charges.retain(|b| {
            b.settled_unix_ms
                > ant
                    .last_tick_unix_ms
                    .saturating_sub(ant.pin.definition.execution.rolling_window_ms)
        });
        summary.compact_buckets()?;
        // Only transitions at/after the next due time can still affect future decisions.
        if let Some(next) = next_due {
            if let Some(last) = ant.changes.iter().rposition(|(t, _)| *t < next) {
                if last > 0 {
                    summary.provenance = hash(&(&summary.provenance, &ant.changes[..last]))?;
                    summary.mode_changes = summary
                        .mode_changes
                        .checked_add(last as u64)
                        .ok_or("Retention count overflow")?;
                    ant.changes.drain(..last);
                }
            }
        }
    }
    world
        .run_capacity
        .retain(|id, _| world.runs.contains_key(id));
    for (id, run) in &world.runs {
        // A failed fresh Task without a recorded target never reached Task execution:
        // planning can still be uncertain, so retain its intent/inputs and lifecycle lease,
        // but no full Task result can arrive through the stopped driver.
        if planning_only_failure(run) {
            let ceiling = run_bytes(run)?.saturating_add(RUN_HEADROOM);
            world
                .run_capacity
                .entry(id.clone())
                .and_modify(|old| *old = (*old).min(ceiling))
                .or_insert(ceiling);
        } else if !lifecycle_open(run) {
            let ceiling = run_bytes(run)?.saturating_add(terminal_tail(run));
            world
                .run_capacity
                .entry(id.clone())
                .and_modify(|old| *old = (*old).min(ceiling))
                .or_insert(ceiling);
        }
    }
    Ok(())
}

/// Capture only native open intents, not historical payloads or another Worker's context.
pub fn pending_intents(world: &World) -> BTreeSet<String> {
    world
        .runs
        .values()
        .flat_map(|r| r.frames.values())
        .flat_map(|f| &f.steps)
        .flat_map(|p| &p.intents)
        .filter(|i| matches!(i.phase, Phase::Prepared | Phase::Uncertain))
        .map(|i| i.id.clone())
        .collect()
}
/// A completed response body is eligible to expire if its full replay projection cannot fit
/// (for example, a large native receipt also copied into roadmap progress). Settle its exact
/// charges and Done intent, retain a content digest, stop future dispatch and say unavailable.
/// Never prune an open intent, its inputs, targets, cancellation, or accounting authority.
pub fn fit_completed_response(world: &mut World, pending: &BTreeSet<String>) -> Result<()> {
    if occupied(world).is_ok_and(|n| n <= MAX_BYTES) {
        return Ok(());
    }
    let mut expired = false;
    for run in world.runs.values_mut() {
        if !run.status.active() && run.status != Status::Cancelled {
            continue;
        }
        let mut pruned = false;
        for frame in run.frames.values_mut() {
            if let Some(progress) = frame.steps.last_mut() {
                if let Some(intent) = progress.intents.last_mut() {
                    if intent.phase == Phase::Done
                        && pending.contains(&intent.id)
                        && !progress.outputs.is_empty()
                    {
                        let receipt = hash(&progress.outputs)?;
                        intent.evidence.push(format!("Completed output payload pruned by native journal capacity: {receipt}; content unavailable"));
                        progress.outputs.clear();
                        progress.status = Status::Failed;
                        progress.diagnostic = Some("Completed output body unavailable: native capacity retention; exact charge and settled effect retained".into());
                        frame.outputs = None;
                        pruned = true;
                    }
                }
            }
        }
        if pruned {
            if run.status != Status::Cancelled {
                run.fail("Completed output payload intentionally pruned by native journal capacity; no further dispatch; exact charges and effect identities retained".into());
            }
            expired = true;
        }
    }
    if expired {
        world.reconcile()?;
    }
    Ok(())
}

fn run_bytes(run: &Run) -> Result<u64> {
    Ok(serde_json::to_vec(run).map_err(|e| e.to_string())?.len() as u64)
}
fn world_bytes(world: &World) -> Result<u64> {
    Ok(serde_json::to_vec(world).map_err(|e| e.to_string())?.len() as u64)
}
fn occupied(world: &World) -> Result<u64> {
    let mut bytes = world_bytes(world)?;
    for (id, run) in &world.runs {
        let used = run_bytes(run)?;
        let ceiling = *world
            .run_capacity
            .get(id)
            .ok_or("Run has no native journal capacity lease")?;
        require(
            ceiling >= used && ceiling <= MAX_BYTES,
            "Run exceeded its reserved native journal capacity",
        )?;
        bytes = bytes
            .checked_add(ceiling - used)
            .ok_or("Journal capacity overflow")?;
    }
    Ok(bytes)
}
/// Every current writer and replay observes the same allocation. Leases cannot be consumed
/// by another Run, another Ant's history, a roadmap, or a new admission.
pub fn validate_capacity(world: &World) -> Result<()> {
    require(
        world.runs.keys().eq(world.run_capacity.keys()),
        "Run capacity identities differ",
    )?;
    require(
        occupied(world)? <= MAX_BYTES,
        "Scenario admission refused: native journal lifecycle capacity is reserved",
    )
}
pub fn allocate_new(world: &mut World, old: &BTreeSet<String>) -> Result<()> {
    world
        .run_capacity
        .retain(|id, _| world.runs.contains_key(id));
    for (id, run) in &world.runs {
        if !old.contains(id) {
            world.run_capacity.insert(
                id.clone(),
                run_bytes(run)?
                    .checked_add(RUN_HEADROOM)
                    .ok_or("Journal capacity overflow")?,
            );
        }
    }
    if world.runs.keys().any(|id| !old.contains(id)) {
        require(
            occupied(world)? <= ADMISSION_BYTES,
            "Scenario admission refused: native journal lifecycle capacity is reserved",
        )?;
    }
    Ok(())
}
fn terminal_tail(run: &Run) -> u64 {
    // Known native charges may still be monotonically reconciled to an overrun. Keep
    // room for wider exact decimal counters and the deterministic failure diagnostic.
    1024 + run.task_charges.len() as u64 * 64
}
pub fn admission_fits(
    runs: &BTreeMap<String, Run>,
    candidate: &Run,
    capacity: Option<&BTreeMap<String, u64>>,
    metadata: u64,
) -> Result<bool> {
    let mut bytes = metadata
        .saturating_add(run_bytes(candidate)?)
        .saturating_add(RUN_HEADROOM);
    for (id, run) in runs {
        let used = run_bytes(run)?;
        bytes = bytes.saturating_add(
            capacity
                .and_then(|c| c.get(id).copied())
                .unwrap_or(used.saturating_add(if protected(run) { RUN_HEADROOM } else { 0 }))
                .max(used),
        );
    }
    Ok(bytes <= ADMISSION_BYTES)
}
/// Reserve a response before committing dispatch authority. The native Task CAS envelope is
/// bounded to 8 MiB. Adapter JSON is bounded to 64 KiB; allow sixfold JSON string escaping,
/// repeated evidence, the exact bound inputs copied into receipts, and lifecycle metadata.
fn response_bytes(dispatch: &Dispatch) -> Result<u64> {
    let inputs = serde_json::to_vec(&dispatch.inputs)
        .map_err(|e| e.to_string())?
        .len() as u64;
    Ok(match dispatch.definition.operation {
        Kind::Task { .. } | Kind::FreshTask { .. } => 8 * 1024 * 1024 + RUN_HEADROOM,
        _ => inputs.saturating_add(2 * 1024 * 1024),
    })
}
pub fn prepare_run(world: &mut World, id: &str, now: u64) -> Result<Option<Dispatch>> {
    let before = world.runs.get(id).ok_or("Scenario Run not found")?.clone();
    require(now >= before.updated_unix_ms, "Run clock moved backwards")?;
    let old_capacity = *world
        .run_capacity
        .get(id)
        .ok_or("Run capacity lease missing")?;
    let run = world.runs.get_mut(id).unwrap();
    let dispatch = match run.prepare(now) {
        Ok(d) => d,
        Err(why) => {
            run.fail(why);
            None
        }
    };
    let needed = run_bytes(run)?.saturating_add(match &dispatch {
        Some(d) => response_bytes(d)?,
        None => {
            if protected(run) {
                RUN_HEADROOM
            } else {
                0
            }
        }
    });
    world
        .run_capacity
        .insert(id.into(), old_capacity.max(needed));
    if occupied(world).is_ok_and(|n| n <= ADMISSION_BYTES) {
        return Ok(dispatch);
    }
    // No new effect was dispatched. Preserve all existing intents and reconciliation
    // authority. A known Run may terminate with an inspectable capacity refusal.
    world.run_capacity.insert(id.into(), old_capacity);
    let mut refused = before;
    if !refused
        .frames
        .values()
        .flat_map(|f| &f.steps)
        .flat_map(|p| &p.intents)
        .any(|i| matches!(i.phase, Phase::Prepared | Phase::Uncertain))
    {
        refused.updated_unix_ms = now;
        refused.fail("Native journal capacity cannot reserve the next transition/response; no new effect dispatched".into());
    } else if refused.status.active() && refused.waiting.is_none() {
        let (step, intent_id) = refused
            .frames
            .iter()
            .find_map(|(path, f)| {
                f.steps.iter().find_map(|p| {
                    p.intents
                        .last()
                        .filter(|i| matches!(i.phase, Phase::Prepared | Phase::Uncertain))
                        .map(|i| (format!("{path}/{}", p.step), i.id.clone()))
                })
            })
            .ok_or("Uncertain capacity refusal lost its intent")?;
        refused.status = Status::Waiting;
        refused.waiting = Some(Waiting::Reconcile {
            step,
            intent_id,
            diagnostic:
                "Native response capacity remains reserved; retry after capacity becomes available"
                    .into(),
        });
    }
    world.runs.insert(id.into(), refused);
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store::Store, tests::*};
    use std::sync::Arc;

    fn interval(id: &str, large: bool, task: bool) -> AntState {
        let (mut bundle, mut policy) = simple();
        let def = bundle.definitions.get_mut("main").unwrap();
        if task {
            let target = TaskTarget {
                repo: "/inert".into(),
                state: "/inert-state".into(),
                task_id: "native".into(),
                plan_id: digest(b"plan"),
                revision_id: digest(b"revision"),
                max_tokens: 100,
            };
            policy.task_plans.insert(target.plan_id.clone());
            def.authority.task_plans.insert(target.plan_id.clone());
            def.steps[0].inputs.clear();
            def.steps[0].operation = Kind::Task { target };
            def.steps[0].outputs = BTreeMap::from([("result".into(), Port::Task)]);
        } else {
            def.steps[0].when = Some(Binding::Literal {
                value: Box::new(Datum::Boolean(false)),
            });
        }
        if large {
            def.outputs.insert("report".into(), Port::Text);
            def.result.insert(
                "report".into(),
                Binding::Literal {
                    value: Box::new(Datum::Text("recorded native report\n".repeat(2600))),
                },
            );
        }
        let definition = Ant {
            schema: "af/ant@6".into(),
            id: id.into(),
            scenario: bundle,
            inputs: BTreeMap::new(),
            schedule: Schedule::Interval {
                seconds: 1,
                start_unix_ms: START,
            },
            owner: "owner".into(),
            notification: "local:inert".into(),
            execution: Execution {
                overlap: Skip::Skip,
                missed: Skip::Skip,
                lateness_ms: 999,
                per_run: limits(),
                rolling_tokens: 100_000,
                rolling_window_ms: 10_000,
            },
        };
        let mut ant = AntState::new(
            AntPin::new(&TestHost::default(), definition, policy).unwrap(),
            START,
        )
        .unwrap();
        ant.change(AntMode::Enabled, START).unwrap();
        ant
    }
    fn tick(world: &mut World, at: u64) -> Vec<String> {
        let (ids, refused) = tick_all_with_roadmaps(
            &TestHost::default(),
            &mut world.ants,
            &mut world.runs,
            &mut world.roadmaps,
            at,
        );
        assert!(refused.is_empty(), "{refused:?}");
        ids
    }

    #[test]
    fn sustained_native_retention_crosses_real_bound_and_keeps_latest_and_lifecycle() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::isolated(root.path()).unwrap();
        store
            .update(|w| {
                w.ants
                    .insert("second".into(), interval("second", true, false));
                Ok(())
            })
            .unwrap();
        let mut ids = BTreeSet::new();
        let mut first = None;
        let mut legacy_bytes = 0u64;
        let mut admitted = 0;
        let mut active = None;
        for index in 0..600u64 {
            let at = START + index * 2000;
            let (new, bytes) = store
                .update(|w| {
                    let new = tick(w, at);
                    assert!(tick(w, at).is_empty());
                    let mut bytes = serde_json::to_vec(w.ants["second"].occurrences.last().unwrap())
                        .unwrap()
                        .len() as u64;
                    if let Some(id) = new.first() {
                        active = Some(id.clone());
                    }
                    // Alternating busy, failed, cancelled and successful results. A late tick
                    // creates missed ranges; none of them is ever replayed as a catch-up burst.
                    if index % 3 != 0 {
                        if let Some(id) = active.take() {
                            let run = w.runs.get_mut(&id).unwrap();
                            match index % 5 {
                                0 => run.fail("recorded terminal failure".into()),
                                1 => {
                                    run.status = Status::Cancelled;
                                    run.cancellation = Some(Cancellation {
                                        requested_unix_ms: at,
                                        reason: "native cancellation".into(),
                                        child_requests: BTreeMap::new(),
                                    });
                                }
                                _ => {
                                    assert!(run.prepare(at).unwrap().is_none());
                                    assert!(run.prepare(at).unwrap().is_none());
                                    assert_eq!(run.status, Status::Succeeded);
                                }
                            }
                            run.updated_unix_ms = at;
                            bytes += serde_json::to_vec(run).unwrap().len() as u64;
                        }
                    }
                    Ok((new, bytes))
                })
                .unwrap();
            legacy_bytes += bytes;
            for id in new {
                assert!(ids.insert(id.clone()));
                first.get_or_insert(id);
                admitted += 1;
            }
            if index % 100 == 99 {
                let world = store.read().unwrap();
                assert_eq!(world, Store::isolated(root.path()).unwrap().read().unwrap());
                assert!(serde_json::to_vec(&world).unwrap().len() as u64 <= MAX_BYTES);
                valid("scenario-store-v9.json", &world);
            }
        }
        eprintln!(
            "retention mixed: legacy_bytes={legacy_bytes} world_bytes={} sqlite_bytes={}",
            world_bytes(&store.read().unwrap()).unwrap(),
            std::fs::metadata(root.path().join("runs/scenarios.sqlite"))
                .unwrap()
                .len()
        );
        assert!(
            legacy_bytes > MAX_BYTES * 2,
            "only {legacy_bytes} bytes of uncompacted history"
        );
        assert!(admitted > 300, "admissions stopped: {admitted}");
        let world = store.read().unwrap();
        let ant = &world.ants["second"];
        assert!(ant.retention.terminal_runs > 250);
        assert_eq!(ant.retention.charged_tokens, "0");
        assert!(ant.retention.outcomes["skipped_busy"] > 100);
        assert!(world.runs.len() <= RECENT + 1);
        assert!(
            store
                .read_output(false, first.as_ref().unwrap())
                .unwrap_err()
                .contains("pruned")
        );
        let latest = store.read_output(true, "second").unwrap();
        assert!(world.runs.contains_key(latest.run.as_ref().unwrap()));
        assert!(latest.outputs["report"].is_some());
        let c = rusqlite::Connection::open(root.path().join("runs/scenarios.sqlite")).unwrap();
        let indexed: i64 = c
            .query_row(
                "SELECT count(*) FROM result_views WHERE kind='run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, world.runs.len() as i64);
        // Lifecycle writes remain possible after more than two old journal capacities.
        let active = store
            .update(|w| Ok(tick(w, START + 1_200_000)))
            .unwrap()
            .pop()
            .unwrap();
        let cancelled = crate::runtime::cancel(
            &store,
            &active,
            "settle after sustained use".into(),
            START + 1_200_001,
        )
        .unwrap();
        assert_eq!(cancelled.status, Status::Cancelled);
        assert_eq!(
            cancelled.execution_id,
            store.read().unwrap().runs[&active].execution_id
        );
    }

    #[test]
    fn sustained_skips_and_human_hold_remain_bounded_and_replay_without_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::isolated(root.path()).unwrap();
        let mut ant = interval("held", false, false);
        // Restore the native human decision; the driver must retain its exact request.
        let (bundle, policy) = simple();
        ant.pin.definition.scenario = bundle;
        ant.pin = AntPin::new(&TestHost::default(), ant.pin.definition, policy).unwrap();
        store
            .update(|w| {
                w.ants.insert("held".into(), ant);
                Ok(())
            })
            .unwrap();
        let id = store.update(|w| Ok(tick(w, START))).unwrap().pop().unwrap();
        let held = crate::runtime::resume(&store, &id, Some(START)).unwrap();
        assert!(matches!(held.waiting, Some(Waiting::Decision { .. })));
        let mut legacy_bytes = 0u64;
        for batch in 0..400u64 {
            store
                .update(|w| {
                    for index in 1 + batch * 256..=(batch + 1) * 256 {
                        // Production updates compact after each tick. Batch only the
                        // SQLite durability checkpoints, retaining that same native policy.
                        compact(w)?;
                        assert!(tick(w, START + index * 1000).is_empty());
                        legacy_bytes +=
                            serde_json::to_vec(w.ants["held"].occurrences.last().unwrap())
                                .unwrap()
                                .len() as u64;
                    }
                    Ok(())
                })
                .unwrap();
        }
        eprintln!(
            "retention held: legacy_bytes={legacy_bytes} world_bytes={} sqlite_bytes={}",
            world_bytes(&store.read().unwrap()).unwrap(),
            std::fs::metadata(root.path().join("runs/scenarios.sqlite"))
                .unwrap()
                .len()
        );
        assert!(legacy_bytes > MAX_BYTES);
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        assert_eq!(world.runs[&id], held);
        assert_eq!(world.runs.len(), 1);
        assert_eq!(world.ants["held"].occurrences.len(), RECENT + 1);
        assert_eq!(
            world.ants["held"].retention.outcomes["skipped_busy"] + (RECENT as u64),
            102400
        );
        assert!(serde_json::to_vec(&world).unwrap().len() < 32 * 1024);
        let view = store.read_output(true, "held").unwrap();
        assert!(view.active);
        assert_eq!(view.outputs["ok"], Some(Datum::Boolean(true)));
        assert_eq!(
            crate::runtime::cancel(
                &store,
                &id,
                "operator stopped held work".into(),
                START + 102_400_001
            )
            .unwrap()
            .status,
            Status::Cancelled
        );
    }

    #[test]
    fn compact_charges_are_exact_conservative_and_unknown_work_never_expires() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(root.path()).unwrap();
        store
            .update(|w| {
                w.ants
                    .insert("charged".into(), interval("charged", false, true));
                Ok(())
            })
            .unwrap();
        let mut expected = 0u128;
        for index in 0..240u64 {
            store
                .update(|w| {
                    let id = tick(w, START + index * 1000).pop().unwrap();
                    let run = w.runs.get_mut(&id).unwrap();
                    run.prepare(START + index * 1000).unwrap().unwrap();
                    let charge = run.task_charges.values_mut().next().unwrap();
                    charge.charged = Some(index % 29);
                    expected += u128::from(index % 29);
                    run.frames.get_mut("root").unwrap().steps[0].intents[0].phase = Phase::Failed;
                    run.fail("native known failed Task".into());
                    Ok(())
                })
                .unwrap();
        }
        let world = store.read().unwrap();
        let retained: u128 = world
            .runs
            .values()
            .flat_map(|r| r.task_charges.values())
            .map(|c| u128::from(c.charged.unwrap()))
            .sum();
        let summary = &world.ants["charged"].retention;
        assert_eq!(
            summary.charged_tokens.parse::<u128>().unwrap() + retained,
            expected
        );
        assert_eq!(summary.rolling(START + 239_000, 10_000).unwrap(), 0); // older than the retained recent receipts
        let uncertain = store
            .update(|w| {
                let id = tick(w, START + 240_000).pop().unwrap();
                let run = w.runs.get_mut(&id).unwrap();
                run.prepare(START + 240_000).unwrap().unwrap();
                let charge = run.task_charges.values_mut().next().unwrap();
                charge.charged = Some(65);
                charge.unknown_usage = true;
                run.fail("native response lost".into());
                Ok(id)
            })
            .unwrap();
        store.host = Arc::new(TestHost {
            charge: Some((65, true)),
            ..TestHost::default()
        });
        crate::runtime::reconcile_pending(&store, START + 1_000_000).unwrap();
        store
            .update(|w| {
                assert!(!tick(w, START + 1_000_000).is_empty());
                Ok(())
            })
            .unwrap();
        for index in 1..=80 {
            store
                .update(|w| {
                    // Retire many later known/no-effect Runs around the unresolved hole.
                    for run in w.runs.values_mut().filter(|r| r.status.active()) {
                        run.fail("inert successor stopped before dispatch".into());
                    }
                    let id = tick(w, START + 1_000_000 + index * 1000).pop().unwrap();
                    w.runs
                        .get_mut(&id)
                        .unwrap()
                        .fail("inert successor stopped before dispatch".into());
                    Ok(())
                })
                .unwrap();
        }
        let world = store.read().unwrap();
        let run = &world.runs[&uncertain];
        assert_eq!(run.tokens().unwrap(), 100);
        assert_eq!(run.task_charges.values().next().unwrap().reserved(), 35);
        assert!(protected(run));
        // A native reconciliation retains the original execution and intent identities.
        let execution = run.execution_id.clone();
        let intent = run.frames["root"].steps[0].intents[0].id.clone();
        store.host = Arc::new(TestHost {
            charge: Some((73, false)),
            ..TestHost::default()
        });
        crate::runtime::reconcile_pending(&store, START + 1_080_001).unwrap();
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        assert_eq!(world.runs[&uncertain].tokens().unwrap(), 73);
        assert_eq!(world.runs[&uncertain].execution_id, execution);
        assert_eq!(
            world.runs[&uncertain].frames["root"].steps[0].intents[0].id,
            intent
        );
        assert_eq!(world.runs[&uncertain].dispatches, 1);
    }

    #[test]
    fn within_run_reconciliation_overflow_replays_skips_and_retires_exactly() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(root.path()).unwrap();
        let ant = interval("within", false, true);
        let mut definition = ant.pin.definition;
        definition.execution.rolling_window_ms = 100_000;
        let scenario = definition.scenario.definitions.get_mut("main").unwrap();
        let Kind::Task { target } = &mut scenario.steps[0].operation else {
            unreachable!()
        };
        target.max_tokens = 40;
        let mut second = scenario.steps[0].clone();
        second.id = "second".into();
        let Kind::Task { target } = &mut second.operation else {
            unreachable!()
        };
        target.task_id = "second".into();
        scenario.steps.push(second);
        let pin = AntPin::new(&TestHost::default(), definition, ant.pin.scenario.policy).unwrap();
        let mut ant = AntState::new(pin, START).unwrap();
        ant.change(AntMode::Enabled, START).unwrap();
        store
            .update(|w| {
                w.ants.insert("within".into(), ant);
                Ok(())
            })
            .unwrap();
        let receipt: TaskReceipt = serde_json::from_value(serde_json::json!({
            "task_id":"native", "plan_id":digest(b"plan"), "revision_id":digest(b"revision"),
            "result_id":digest(b"first-result"), "charged_tokens":17,
            "result":{"task_revision_id":digest(b"revision"),"execution":"completed","acceptance":"satisfied",
                "domain_conclusion":"verified", "outputs":{"report":{"artifact_ids":[digest(b"report")],"artifact_type":"af/Test@1","cardinality":"one"}},
                "evidence":[digest(b"native-evidence")],"missing_obligations":[]}
        })).unwrap();
        // Exercise durable dispatch and settlement, including the host's lost response.
        store.host = Arc::new(TestHost {
            task_settlements: BTreeMap::from([
                (
                    "native".into(),
                    Ok(crate::adapter::Settlement::TaskDone {
                        outputs: BTreeMap::from([("result".into(), Datum::Task(receipt))]),
                        unknown_usage: false,
                    }),
                ),
                ("second".into(), Err("Native response lost".into())),
            ]),
            ..TestHost::default()
        });
        let id = store.update(|w| Ok(tick(w, START))).unwrap().pop().unwrap();
        let lost = crate::runtime::resume(&store, &id, Some(START)).unwrap();
        assert_eq!(lost.status, Status::Failed);
        assert_eq!(lost.task_charges.len(), 2);
        assert_eq!(lost.tokens().unwrap(), 57);
        let second_key = lost
            .task_charges
            .iter()
            .find(|(_, c)| c.step == "root/second")
            .unwrap()
            .0
            .clone();
        assert_eq!(lost.task_charges[&second_key].charged, None);
        assert_eq!(lost.task_charges[&second_key].reserved(), 40);
        assert!(protected(&lost));
        let before = Store::isolated(root.path()).unwrap().read().unwrap();
        assert_eq!(before.runs[&id], lost);
        let execution = lost.execution_id.clone();
        let intents: Vec<_> = lost.frames["root"]
            .steps
            .iter()
            .map(|p| p.intents[0].id.clone())
            .collect();
        // A later admitted producer makes retirement possible without bypassing budgets.
        let successor = store
            .update(|w| {
                let next = tick(w, START + 1000).pop().unwrap();
                w.runs
                    .get_mut(&next)
                    .unwrap()
                    .fail("Stopped before dispatch".into());
                Ok(next)
            })
            .unwrap();
        for index in 2..=20 {
            store
                .update(|w| {
                    w.ants
                        .get_mut("within")
                        .unwrap()
                        .change(AntMode::Paused, START + index * 1000)?;
                    assert!(tick(w, START + index * 1000).is_empty());
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(store.read().unwrap().runs[&id], lost);
        let settled = START + 20_000;
        store.host = Arc::new(TestHost {
            task_charges: BTreeMap::from([
                ("native".into(), (17, false)),
                ("second".into(), (u64::MAX, false)),
            ]),
            ..TestHost::default()
        });
        crate::runtime::reconcile_pending(&store, settled).unwrap();
        let exact = u128::from(u64::MAX) + 17;
        let replay = Store::isolated(root.path()).unwrap();
        let world = replay.read().unwrap();
        let run = &world.runs[&id];
        run.validate(&TestHost::default()).unwrap();
        assert_eq!(run.status, Status::Failed);
        assert_eq!(run.tokens().unwrap(), exact);
        assert_eq!(run.accounted_tokens().unwrap(), exact);
        assert!(
            run.task_charges
                .values()
                .any(|c| c.step == "root/review" && c.charged == Some(17) && c.reserved() == 0)
        );
        assert_eq!(run.task_charges[&second_key].charged, Some(u64::MAX));
        let mut terminal = run.clone();
        assert!(terminal.prepare(settled).unwrap().is_none());
        assert_eq!(terminal.dispatches, 2);
        assert_eq!(run.task_charges[&second_key].reserved(), 0);
        assert!(!protected(run));
        assert_eq!(run.execution_id, execution);
        assert_eq!(run.dispatches, 2);
        assert_eq!(
            run.frames["root"]
                .steps
                .iter()
                .map(|p| p.intents[0].id.clone())
                .collect::<Vec<_>>(),
            intents
        );
        assert_eq!(run.frames["root"].steps[0].intents[0].phase, Phase::Done);
        assert_eq!(run.frames["root"].steps[1].intents[0].phase, Phase::Failed);
        // Use the actual Ant runtime tick after recovery. No Task can be dispatched.
        let at = START + 21_000;
        replay
            .update(|w| {
                w.ants
                    .get_mut("within")
                    .unwrap()
                    .change(AntMode::Enabled, at)
            })
            .unwrap();
        let result = crate::runtime::tick(&replay, at, Some(at)).unwrap();
        assert!(result.started.is_empty());
        assert!(result.refused.is_empty());
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        assert_eq!(world.ants["within"].next_day, 22);
        assert_eq!(world.ants["within"].last_tick_unix_ms, at);
        assert_eq!(
            world.ants["within"].occurrences.last().unwrap().outcome,
            OccurrenceOutcome::SkippedBudget
        );
        assert_eq!(world.runs[&id].task_charges, run.task_charges);
        assert!(
            crate::runtime::tick(&replay, at, Some(at))
                .unwrap()
                .started
                .is_empty()
        );
        // Age the settled body out while its exact rolling bucket remains live.
        let provenance = world.ants["within"].retention.provenance.clone();
        for index in 22..=60 {
            replay
                .update(|w| {
                    assert!(tick(w, START + index * 1000).is_empty());
                    Ok(())
                })
                .unwrap();
        }
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        assert!(!world.runs.contains_key(&id));
        assert!(world.runs.contains_key(&successor)); // latest producer remains inspectable
        let summary = &world.ants["within"].retention;
        assert_eq!(summary.charged_tokens, exact.to_string());
        assert_eq!(summary.terminal_runs, 1);
        assert_ne!(summary.provenance, provenance);
        assert_eq!(
            summary.rolling_charges,
            vec![ChargeBucket {
                settled_unix_ms: settled,
                tokens: exact.to_string(),
            }]
        );
        assert_eq!(summary.rolling(START + 60_000, 100_000).unwrap(), u64::MAX);
        let summary_before_expiry = summary.clone();
        replay
            .update(|w| {
                assert!(tick(w, settled + 99_000).is_empty());
                assert_eq!(
                    w.ants["within"].occurrences.last().unwrap().outcome,
                    OccurrenceOutcome::SkippedBudget
                );
                assert_eq!(tick(w, settled + 100_000).len(), 1);
                Ok(())
            })
            .unwrap();
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        let summary = &world.ants["within"].retention;
        assert_eq!(summary.charged_tokens, exact.to_string());
        assert_eq!(
            summary.terminal_runs,
            summary_before_expiry.terminal_runs + 1
        );
        assert!(summary.rolling_charges.is_empty());
    }

    #[test]
    fn rolling_admission_saturates_retained_and_retired_overruns_and_replays_exactly() {
        for retire in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let mut store = Store::isolated(root.path()).unwrap();
            let ant = interval("overflow", false, true);
            let mut definition = ant.pin.definition;
            definition.execution.rolling_window_ms = 100_000;
            let mut ant = AntState::new(
                AntPin::new(&TestHost::default(), definition, ant.pin.scenario.policy).unwrap(),
                START,
            )
            .unwrap();
            ant.change(AntMode::Enabled, START).unwrap();
            store
                .update(|w| {
                    w.ants.insert("overflow".into(), ant);
                    Ok(())
                })
                .unwrap();
            // Admit small reservations before native reconciliation discovers overruns.
            for index in 0..3 {
                store
                    .update(|w| {
                        let at = START + index * 1000;
                        let id = tick(w, at).pop().unwrap();
                        let run = w.runs.get_mut(&id).unwrap();
                        run.prepare(at).unwrap().unwrap();
                        run.frames.get_mut("root").unwrap().steps[0].intents[0].phase =
                            Phase::Failed;
                        run.fail("native response lost; usage unresolved".into());
                        Ok(())
                    })
                    .unwrap();
            }
            let settled = if retire {
                store
                    .update(|w| {
                        w.ants
                            .get_mut("overflow")
                            .unwrap()
                            .change(AntMode::Paused, START + 3000)?;
                        // Unknown Runs remain protected while their receipts age out of RECENT.
                        for index in 3..35 {
                            assert!(tick(w, START + index * 1000).is_empty());
                        }
                        Ok(())
                    })
                    .unwrap();
                START + 34_000
            } else {
                START + 2000
            };
            store.host = Arc::new(TestHost {
                charge: Some((u64::MAX, false)),
                ..TestHost::default()
            });
            crate::runtime::reconcile_pending(&store, settled).unwrap();
            let retired = if retire { 2 * u128::from(u64::MAX) } else { 0 };
            let exact = 3 * u128::from(u64::MAX);
            let assert_accounting = |w: &World| {
                let summary = &w.ants["overflow"].retention;
                let retired = summary.charged_tokens.parse::<u128>().unwrap();
                let live: u128 = w
                    .runs
                    .values()
                    .flat_map(|r| r.task_charges.values())
                    .map(|c| u128::from(c.charged.unwrap_or(0)))
                    .sum();
                assert_eq!(retired + live, exact);
            };
            let world = store.read().unwrap();
            assert_accounting(&world);
            assert_eq!(
                world.ants["overflow"].retention.charged_tokens,
                retired.to_string()
            );
            assert_eq!(
                world.ants["overflow"]
                    .retention
                    .rolling_charges
                    .iter()
                    .map(|b| b.tokens.parse::<u128>().unwrap())
                    .sum::<u128>(),
                retired
            );
            assert_eq!(
                world.ants["overflow"]
                    .retention
                    .rolling(settled, 100_000)
                    .unwrap(),
                if retire { u64::MAX } else { 0 }
            );
            let at = settled + 1000;
            store
                .update(|w| {
                    w.ants
                        .get_mut("overflow")
                        .unwrap()
                        .change(AntMode::Enabled, at)?;
                    assert!(tick(w, at).is_empty());
                    assert_eq!(
                        w.ants["overflow"].occurrences.last().unwrap().outcome,
                        OccurrenceOutcome::SkippedBudget
                    );
                    Ok(())
                })
                .unwrap();
            // Reopen the native SQLite Store: the skip and advanced cursor must be durable.
            let replay = Store::isolated(root.path()).unwrap();
            let world = replay.read().unwrap();
            let ant = &world.ants["overflow"];
            assert_eq!(ant.next_day, ((at - START) / 1000 + 1) as i64);
            assert_eq!(ant.last_tick_unix_ms, at);
            assert_eq!(
                ant.occurrences.last().unwrap().outcome,
                OccurrenceOutcome::SkippedBudget
            );
            assert_accounting(&world);
            replay
                .update(|w| {
                    assert!(tick(w, at).is_empty());
                    assert_eq!(w.ants["overflow"].next_day, ant.next_day);
                    assert!(tick(w, settled + 99_000).is_empty());
                    assert_eq!(
                        w.ants["overflow"].occurrences.last().unwrap().outcome,
                        OccurrenceOutcome::SkippedBudget
                    );
                    // Charges expire at the ordinary rolling boundary, never on overflow.
                    assert_eq!(tick(w, settled + 100_000).len(), 1);
                    Ok(())
                })
                .unwrap();
            assert_accounting(&Store::isolated(root.path()).unwrap().read().unwrap());
        }
    }

    #[test]
    fn reserved_large_native_response_settles_after_other_admissions_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(root.path()).unwrap();
        store.host = Arc::new(TestHost {
            task_settlement: Some(crate::adapter::Settlement::Waiting(
                "native Task still running".into(),
            )),
            ..TestHost::default()
        });
        let ant = interval("large", false, true);
        store
            .update(|w| {
                w.ants.insert("large".into(), ant);
                Ok(())
            })
            .unwrap();
        let id = store.update(|w| Ok(tick(w, START))).unwrap().pop().unwrap();
        let pending = crate::runtime::resume(&store, &id, Some(START)).unwrap();
        assert!(matches!(pending.waiting, Some(Waiting::Task { .. })));
        let intent = pending.frames["root"].steps[0].intents[0].id.clone();
        let lease = store.read().unwrap().run_capacity[&id];
        assert!(lease > 9 * 1024 * 1024);
        let (bundle, policy) = simple();
        let pin = Pin::new(&TestHost::default(), bundle, policy).unwrap();
        let mut held = Vec::new();
        for n in 0..16 {
            let label = format!("pressure-{n}");
            let run = Run::new(
                &TestHost::default(),
                label.clone(),
                pin.clone(),
                BTreeMap::new(),
                limits(),
                START,
                None,
            )
            .unwrap();
            if let Err(why) = store.update(|w| {
                w.admit_run(run)?;
                Ok(())
            }) {
                assert!(why.contains("capacity is reserved"));
                break;
            }
            held.push(label);
        }
        assert!(!held.is_empty() && held.len() < 16);
        let occupied_before = occupied(&store.read().unwrap()).unwrap();
        assert!(occupied_before > ADMISSION_BYTES - RUN_HEADROOM - 4096);
        // Capacity pressure is a recorded skip, and cannot borrow the in-flight lease.
        store
            .update(|w| {
                let mut other = interval("capacity", false, false);
                // Existing pins are small enough to fit the global metadata allowance.
                other.change(AntMode::Enabled, START).unwrap();
                w.ants.insert("capacity".into(), other);
                Ok(())
            })
            .unwrap();
        store
            .update(|w| {
                let (started, refused) = tick_all_with_capacity(
                    &TestHost::default(),
                    &mut w.ants,
                    &mut w.runs,
                    &mut w.roadmaps,
                    Some(&w.run_capacity),
                    START + 1000,
                );
                assert!(started.is_empty());
                assert!(refused.is_empty());
                assert_eq!(
                    w.ants["capacity"].occurrences.last().unwrap().outcome,
                    OccurrenceOutcome::SkippedCapacity
                );
                Ok(())
            })
            .unwrap();
        let receipt: TaskReceipt = serde_json::from_value(serde_json::json!({
            "task_id":"native", "plan_id":digest(b"plan"), "revision_id":digest(b"revision"),
            "result_id":digest(b"large-result"), "charged_tokens":73,
            "result":{"task_revision_id":digest(b"revision"),"execution":"completed","acceptance":"satisfied", "domain_conclusion":"x".repeat(7*1024*1024),
                "outputs":{"report":{"artifact_ids":[digest(b"report")],"artifact_type":"af/Test@1","cardinality":"one"}},
                "evidence":[digest(b"native-evidence")],"missing_obligations":[]}
        })).unwrap();
        store.host = Arc::new(TestHost {
            task_settlement: Some(crate::adapter::Settlement::TaskDone {
                outputs: BTreeMap::from([("result".into(), Datum::Task(receipt))]),
                unknown_usage: false,
            }),
            ..TestHost::default()
        });
        let settled = crate::runtime::resume(&store, &id, Some(START + 1001)).unwrap();
        assert_eq!(settled.status, Status::Succeeded);
        assert_eq!(settled.tokens().unwrap(), 73);
        assert_eq!(settled.frames["root"].steps[0].intents.len(), 1);
        assert_eq!(settled.frames["root"].steps[0].intents[0].id, intent);
        assert_eq!(
            settled.frames["root"].steps[0].intents[0].phase,
            Phase::Done
        );
        assert_eq!(settled.execution_id, pending.execution_id);
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        assert!(world_bytes(&world).unwrap() < MAX_BYTES);
        assert_eq!(
            world.run_capacity[&id],
            run_bytes(&settled).unwrap() + terminal_tail(&settled)
        );
        for label in held {
            assert_eq!(
                crate::runtime::cancel(
                    &store,
                    &label,
                    "stop while Store is under admission pressure".into(),
                    START + 1002
                )
                .unwrap()
                .status,
                Status::Cancelled
            );
        }
    }

    #[test]
    fn capacity_can_expire_a_completed_body_without_losing_goal_charge_or_effect_authority() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::isolated(root.path()).unwrap();
        let (roadmap, pin) = crate::admission_tests::fixture();
        store
            .update(|w| {
                w.roadmaps.insert(roadmap.id.clone(), roadmap);
                Ok(())
            })
            .unwrap();
        store
            .update(|w| {
                w.admit_run(Run::new(
                    &TestHost::default(),
                    "goal".into(),
                    pin,
                    BTreeMap::new(),
                    limits(),
                    START,
                    None,
                )?)?;
                Ok(())
            })
            .unwrap();
        let target = store
            .update(|w| {
                let d = prepare_run(w, "goal", START)?.unwrap();
                let Kind::FreshTask { template, .. } = &d.definition.operation else {
                    panic!("not fresh")
                };
                let target = TaskTarget {
                    repo: template.repo.clone(),
                    state: template.state.clone(),
                    task_id: crate::template::task_id(&d.intent.id),
                    plan_id: digest(b"plan"),
                    revision_id: digest(b"revision"),
                    max_tokens: template.max_tokens,
                };
                w.runs
                    .get_mut("goal")
                    .unwrap()
                    .fresh_tasks
                    .insert(d.intent.id, target.clone());
                Ok(target)
            })
            .unwrap();
        let (bundle, policy) = simple();
        let pin = Pin::new(&TestHost::default(), bundle, policy).unwrap();
        for n in 0..16 {
            let run = Run::new(
                &TestHost::default(),
                format!("other-{n}"),
                pin.clone(),
                BTreeMap::new(),
                limits(),
                START,
                None,
            )
            .unwrap();
            if store
                .update(|w| {
                    w.admit_run(run)?;
                    Ok(())
                })
                .is_err()
            {
                break;
            }
        }
        let result: af_contracts::task::TaskResultV1 = serde_json::from_value(serde_json::json!({
            "task_revision_id":target.revision_id,"execution":"completed","acceptance":"satisfied","domain_conclusion":"native body".repeat(700_000),
            "outputs":{"report":{"artifact_ids":[digest(b"report")],"artifact_type":"af/Test@1","cardinality":"one"}},"evidence":[digest(b"evidence")],"missing_obligations":[]
        })).unwrap();
        let receipt = TaskReceipt {
            task_id: target.task_id,
            plan_id: target.plan_id,
            revision_id: target.revision_id,
            result_id: digest(b"result"),
            charged_tokens: 17,
            result,
        };
        store
            .update(|w| {
                let r = w.runs.get_mut("goal").unwrap();
                r.task_charges.values_mut().next().unwrap().charged = Some(17);
                r.complete(
                    "root",
                    "review",
                    BTreeMap::from([("result".into(), Datum::Task(receipt))]),
                )?;
                Ok(())
            })
            .unwrap();
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        let run = &world.runs["goal"];
        assert_eq!(run.status, Status::Failed);
        assert_eq!(run.tokens().unwrap(), 17);
        assert!(
            run.failure
                .as_ref()
                .unwrap()
                .contains("intentionally pruned")
        );
        let progress = &run.frames["root"].steps[0];
        assert!(progress.outputs.is_empty());
        assert_eq!(progress.intents[0].phase, Phase::Done);
        assert!(
            progress.intents[0]
                .evidence
                .last()
                .unwrap()
                .contains("content unavailable")
        );
        assert_eq!(
            world.roadmaps["performance"].progress["store-replay"].actual_tokens,
            17
        );
        assert_eq!(
            world.roadmaps["performance"].progress["store-replay"].reserved_tokens,
            0
        );
        assert_eq!(
            world.roadmaps["performance"].progress["store-replay"].state,
            crate::roadmap::GoalState::Deferred
        );
        assert_eq!(run.dispatches, 1);
        assert!(occupied(&world).unwrap() <= MAX_BYTES);
        valid("scenario-store-v9.json", &world);
    }

    #[test]
    fn failed_fresh_planning_keeps_authority_and_unknown_tokens_without_a_result_lease() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::isolated(root.path()).unwrap();
        let (roadmap, pin) = crate::admission_tests::fixture();
        store
            .update(|w| {
                w.roadmaps.insert(roadmap.id.clone(), roadmap);
                Ok(())
            })
            .unwrap();
        store
            .update(|w| {
                w.admit_run(Run::new(
                    &TestHost::default(),
                    "planning".into(),
                    pin,
                    BTreeMap::new(),
                    limits(),
                    START,
                    None,
                )?)?;
                Ok(())
            })
            .unwrap();
        let intent = store
            .update(|w| Ok(prepare_run(w, "planning", START)?.unwrap().intent.id))
            .unwrap();
        assert!(store.read().unwrap().run_capacity["planning"] > 9 * 1024 * 1024);
        store
            .update(|w| {
                w.runs
                    .get_mut("planning")
                    .unwrap()
                    .fail("Planning response lost; no Task execution target recorded".into());
                Ok(())
            })
            .unwrap();
        let world = Store::isolated(root.path()).unwrap().read().unwrap();
        let run = &world.runs["planning"];
        assert_eq!(run.frames["root"].steps[0].intents[0].id, intent);
        assert_eq!(
            run.frames["root"].steps[0].intents[0].phase,
            Phase::Prepared
        );
        assert!(run.fresh_tasks.is_empty());
        assert_eq!(run.tokens().unwrap(), 20);
        assert!(run.task_charges.values().next().unwrap().charged.is_none());
        assert_eq!(
            world.roadmaps["performance"].progress["store-replay"].reserved_tokens,
            60
        );
        assert!(protected(run));
        assert_eq!(
            world.run_capacity["planning"],
            run_bytes(run).unwrap() + RUN_HEADROOM
        );
    }

    #[test]
    fn unknown_usage_outlives_the_rolling_window_and_current_shapes_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::isolated(root.path()).unwrap();
        let mut ant = interval("unknown", false, true);
        let mut definition = ant.pin.definition;
        definition.execution.rolling_tokens = 100;
        ant = AntState::new(
            AntPin::new(&TestHost::default(), definition, ant.pin.scenario.policy).unwrap(),
            START,
        )
        .unwrap();
        ant.change(AntMode::Enabled, START).unwrap();
        store
            .update(|w| {
                w.ants.insert("unknown".into(), ant);
                Ok(())
            })
            .unwrap();
        let id = store
            .update(|w| {
                let id = tick(w, START).pop().unwrap();
                let r = w.runs.get_mut(&id).unwrap();
                r.prepare(START).unwrap().unwrap();
                let c = r.task_charges.values_mut().next().unwrap();
                c.charged = Some(65);
                c.unknown_usage = true;
                r.fail("known partial tokens plus unresolved native usage".into());
                Ok(id)
            })
            .unwrap();
        store
            .update(|w| {
                assert!(tick(w, START + DAY).is_empty());
                assert_eq!(
                    w.ants["unknown"].occurrences.last().unwrap().outcome,
                    OccurrenceOutcome::SkippedBudget
                );
                Ok(())
            })
            .unwrap();
        let world = store.read().unwrap();
        assert_eq!(world.runs[&id].tokens().unwrap(), 100);
        assert_eq!(
            world.runs[&id]
                .task_charges
                .values()
                .next()
                .unwrap()
                .charged,
            Some(65)
        );
        assert_eq!(
            world.runs[&id]
                .task_charges
                .values()
                .next()
                .unwrap()
                .reserved(),
            35
        );
        let mut corrupt = world.clone();
        corrupt.run_capacity.remove(&id);
        assert!(corrupt.validate(&TestHost::default()).is_err());
        let mut corrupt = world.clone();
        corrupt.run_capacity.insert(id.clone(), 1);
        assert!(corrupt.validate(&TestHost::default()).is_err());
        let mut corrupt = world.clone();
        corrupt
            .ants
            .get_mut("unknown")
            .unwrap()
            .retention
            .charged_tokens = "00".into();
        assert!(corrupt.validate(&TestHost::default()).is_err());
        let mut previous = serde_json::to_value(&world).unwrap();
        previous["schema"] = serde_json::json!("af/scenario-store@8");
        previous.as_object_mut().unwrap().remove("run_capacity");
        assert!(serde_json::from_value::<World>(previous).is_err());
        let mut previous = world.ants["unknown"].clone();
        previous.schema = "af/ant-state@6".into();
        assert!(
            previous
                .validate(&TestHost::default(), &world.runs)
                .is_err()
        );
    }

    #[test]
    fn compact_rolling_buckets_never_release_tokens_early() {
        let mut summary = Retention::new();
        // Exercise the same merge policy against an exact uncompressed oracle.
        let exact: Vec<_> = (1..200)
            .map(|n| ChargeBucket {
                settled_unix_ms: START + n * 1000,
                tokens: (n % 11 + 1).to_string(),
            })
            .collect();
        summary.rolling_charges = exact.clone();
        summary.compact_buckets().unwrap();
        for now in (START..START + 400_000).step_by(1000) {
            let oracle: u64 = exact
                .iter()
                .filter(|b| b.settled_unix_ms > now.saturating_sub(100_000))
                .map(|b| b.tokens.parse::<u64>().unwrap())
                .sum();
            assert!(summary.rolling(now, 100_000).unwrap() >= oracle);
        }
        assert_eq!(summary.rolling(START + 400_000, 100_000).unwrap(), 0);
    }

    #[test]
    fn admission_refuses_before_lifecycle_reserve_and_existing_runs_can_cancel() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::isolated(root.path()).unwrap();
        let (bundle, policy) = simple();
        let pin = Pin::new(&TestHost::default(), bundle, policy).unwrap();
        let mut admitted = Vec::new();
        for index in 0..16 {
            let id = format!("capacity-{index}");
            let run = Run::new(
                &TestHost::default(),
                id.clone(),
                pin.clone(),
                BTreeMap::new(),
                limits(),
                START,
                None,
            )
            .unwrap();
            match store.update(|w| {
                w.admit_run(run)?;
                Ok(())
            }) {
                Ok(()) => admitted.push(id),
                Err(why) => {
                    assert!(why.contains("capacity is reserved"));
                    break;
                }
            }
        }
        assert!(!admitted.is_empty() && admitted.len() < 16);
        for id in admitted {
            assert_eq!(
                crate::runtime::cancel(
                    &store,
                    &id,
                    "capacity refusal does not prevent cancellation".into(),
                    START + 1
                )
                .unwrap()
                .status,
                Status::Cancelled
            );
        }
        store.read().unwrap();
    }
}
