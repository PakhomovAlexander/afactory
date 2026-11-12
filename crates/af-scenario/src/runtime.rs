//! Drive one recorded Run. A dispatch is committed before leaving the transaction; completion
//! is committed independently, so writer loss leaves an intent that resume must reconcile.
use super::adapter::{self, Settlement};
use super::contract::*;
use super::state::*;
use super::store::Store;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis(),
    )
    .map_err(|e| e.to_string())
}
pub fn resume(store: &Store, id: &str, clock: Option<u64>) -> Result<Run> {
    reconcile_native_charges(store, id, clock.map_or_else(now, Ok)?)?;
    let existing = store
        .read()?
        .runs
        .get(id)
        .cloned()
        .ok_or("Scenario Run not found")?;
    if let Some(cancellation) = existing.cancellation {
        // Cancellation handoff has its own lock: the original driver may still hold the Run
        // while a process that recorded cancellation died before contacting its active Task.
        return cancel(store, id, cancellation.reason, clock.map_or_else(now, Ok)?);
    }
    let _lock = store.lock_run(id)?;
    loop {
        let at = clock.map_or_else(now, Ok)?;
        let prepared = store.update(|world| crate::retention::prepare_run(world, id, at))?;
        let run = store
            .read()?
            .runs
            .get(id)
            .cloned()
            .ok_or("Scenario Run not found")?;
        if !run.status.active() || run.status == Status::Waiting {
            return Ok(run);
        }
        let Some(mut dispatch) = prepared else {
            continue;
        };
        // Intent admission precedes dispatch. Cancellation after this point requests stopping
        // this active child; it cannot make an external effect that already happened disappear.
        if store.read()?.runs[id].status == Status::Cancelled {
            return Ok(store.read()?.runs[id].clone());
        }
        if matches!(dispatch.definition.operation, Kind::FreshTask { .. }) {
            let world = store.read()?;
            let goal = run
                .goal_claim
                .as_ref()
                .map(|c| {
                    let admission = run.pin.bundle.definitions[&run.pin.bundle.root]
                        .admission
                        .as_ref()
                        .ok_or("Claim lacks admission")?;
                    world.roadmaps[&admission.roadmap]
                        .head()
                        .proposal
                        .goals
                        .iter()
                        .find(|g| g.id == c.goal_id)
                        .cloned()
                        .ok_or("Claim goal missing".to_string())
                })
                .transpose()?;
            let target =
                match super::template::instantiate(store.host.as_ref(), &dispatch, goal.as_ref()) {
                    Ok(target) => target,
                    Err(why) => {
                        store.update(|world| {
                            world.runs.get_mut(id).ok_or("Run disappeared")?.fail(why);
                            Ok(())
                        })?;
                        return Ok(store.read()?.runs[id].clone());
                    }
                };
            store.update(|world| {
                let run = world.runs.get_mut(id).ok_or("Run disappeared")?;
                if let Some(old) = run.fresh_tasks.get(&dispatch.intent.id) {
                    require(
                        *old == target,
                        "Fresh native Task plan changed on reconciliation",
                    )?;
                }
                run.fresh_tasks
                    .insert(dispatch.intent.id.clone(), target.clone());
                Ok(())
            })?;
            dispatch.target = Some(target);
            if store.read()?.runs[id].status == Status::Cancelled {
                return cancel(
                    store,
                    id,
                    store.read()?.runs[id]
                        .cancellation
                        .as_ref()
                        .ok_or("Missing cancellation")?
                        .reason
                        .clone(),
                    clock.map_or_else(now, Ok)?,
                );
            }
        }
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let task_dispatch = matches!(
            dispatch.definition.operation,
            Kind::Task { .. } | Kind::FreshTask { .. }
        );
        let full_step = format!("{}/{}", dispatch.frame, dispatch.step);
        let result = std::thread::scope(|scope| {
            let (finished, wait) = std::sync::mpsc::channel();
            let cancellation_ref = &cancellation;
            let watcher = scope.spawn(move || {
                loop {
                    match wait.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if store.read().map_or(true, |world| {
                                world.runs.get(id).is_none_or(|r| {
                                    r.status == Status::Cancelled
                                        && (!task_dispatch
                                            || r.cancellation
                                                .as_ref()
                                                .and_then(|c| c.child_requests.get(&full_step))
                                                .is_some_and(|s| {
                                                    s.state != CancellationState::Requested
                                                }))
                                })
                            }) {
                                cancellation_ref.store(true, std::sync::atomic::Ordering::Release);
                                break;
                            }
                        }
                    }
                }
            });
            let result = adapter::dispatch(
                store.host.as_ref(),
                &run.pin.policy,
                &dispatch,
                &cancellation,
            );
            let _ = finished.send(());
            let _ = watcher.join();
            result
        });
        let settled_at = clock.map_or_else(now, Ok)?;
        let stop = store.update(|world| {
            let run = world.runs.get_mut(id).ok_or("Scenario Run disappeared")?;
            require(
                settled_at >= run.updated_unix_ms,
                "Completion clock moved backwards",
            )?;
            run.updated_unix_ms = settled_at;
            let cancelled = run.status == Status::Cancelled;
            let progress = run
                .frames
                .get_mut(&dispatch.frame)
                .and_then(|f| f.steps.last_mut())
                .ok_or("Dispatch step disappeared")?;
            require(
                progress.step == dispatch.step
                    && progress
                        .intents
                        .last()
                        .is_some_and(|i| i.id == dispatch.intent.id),
                "Stale dispatch completion",
            )?;
            let exact_charge = match &result {
                Ok(Settlement::TaskFailed {
                    charged_tokens,
                    unknown_usage,
                    ..
                }) => Some((*charged_tokens, *unknown_usage)),
                Ok(Settlement::TaskDone {
                    outputs,
                    unknown_usage,
                }) => outputs.values().find_map(|v| match v {
                    Datum::Task(t) => Some((t.charged_tokens, *unknown_usage)),
                    _ => None,
                }),
                _ => None,
            };
            let mut charge_breached = false;
            if let Some((actual, unknown_usage)) = exact_charge {
                let key = super::template::charge_key(&dispatch)?;
                let charge = run
                    .task_charges
                    .get_mut(&key)
                    .ok_or("Task was not reserved")?;
                charge.charged = Some(actual);
                charge.unknown_usage = unknown_usage;
                charge_breached =
                    actual > charge.maximum || run.tokens()? > u128::from(run.token_allowance());
            }
            match result {
                Ok(Settlement::Done(ref outputs))
                | Ok(Settlement::TaskDone { ref outputs, .. }) => {
                    for datum in outputs.values() {
                        datum.validate()?;
                    }
                    run.complete(&dispatch.frame, &dispatch.step, outputs.clone())?;
                    if !cancelled {
                        run.status = Status::Running;
                        run.waiting = None;
                    }
                    if charge_breached && !cancelled {
                        run.fail(
                            "Task exceeded its Scenario token reservation; exact charge retained"
                                .into(),
                        );
                    }
                    Ok(cancelled || charge_breached)
                }
                Ok(Settlement::Waiting(ref evidence)) | Ok(Settlement::Uncertain(ref evidence)) => {
                    text(evidence)?;
                    let progress = run
                        .frames
                        .get_mut(&dispatch.frame)
                        .unwrap()
                        .steps
                        .last_mut()
                        .unwrap();
                    let intent = progress.intents.last_mut().unwrap();
                    intent.phase = Phase::Uncertain;
                    intent.evidence.push(evidence.clone());
                    progress.status = Status::Waiting;
                    if !cancelled {
                        run.status = Status::Waiting;
                        let step = format!("{}/{}", dispatch.frame, dispatch.step);
                        run.waiting = Some(match &dispatch.definition.operation {
                            Kind::Wait { .. } => Waiting::Condition {
                                step,
                                deadline_unix_ms: progress.deadline_unix_ms,
                                evidence: evidence.clone(),
                            },
                            Kind::Task { target } => Waiting::Task {
                                step,
                                task_id: target.task_id.clone(),
                            },
                            Kind::FreshTask { .. } => Waiting::Task {
                                step,
                                task_id: super::template::task_id(&dispatch.intent.id),
                            },
                            _ => Waiting::Reconcile {
                                step,
                                intent_id: dispatch.intent.id.clone(),
                                diagnostic: evidence.clone(),
                            },
                        });
                    }
                    Ok(true)
                }
                Ok(Settlement::Failed(ref why))
                | Ok(Settlement::TaskFailed {
                    diagnostic: ref why,
                    ..
                })
                | Err(ref why) => {
                    let progress = run
                        .frames
                        .get_mut(&dispatch.frame)
                        .unwrap()
                        .steps
                        .last_mut()
                        .unwrap();
                    let intent = progress.intents.last_mut().unwrap();
                    intent.phase = Phase::Failed;
                    intent.evidence.push(why.clone());
                    progress.diagnostic = Some(why.clone());
                    if !cancelled {
                        // An admission or integrity error cannot become retry authority. A
                        // definitive adapter failure may use the step's finite attempt bound.
                        if result.is_err()
                            || charge_breached
                            || progress.intents.len()
                                >= usize::from(dispatch.definition.max_attempts)
                        {
                            run.fail(why.clone());
                        }
                    }
                    Ok(cancelled || !run.status.active())
                }
            }
        })?;
        if stop {
            return Ok(store.read()?.runs[id].clone());
        }
    }
}
pub fn cancel(store: &Store, id: &str, reason: String, at: u64) -> Result<Run> {
    text(&reason)?;
    let targets: BTreeMap<String,(TaskTarget,Option<u64>)> = store.update(|world| {
        let run = world.runs.get_mut(id).ok_or("Scenario Run not found")?;
        require(at >= run.updated_unix_ms,"Cancellation clock moved backwards")?;
        let repeating = run.status == Status::Cancelled;
        require(repeating || run.status.active(),"A terminal Run cannot be cancelled")?;
        let mut targets = BTreeMap::new();
        for (path,frame) in &run.frames {
            let def = &run.pin.bundle.definitions[&frame.scenario];
            for (p,step) in frame.steps.iter().zip(&def.steps) {
                let full = format!("{path}/{}",step.id);
                let previous = run.cancellation.as_ref().and_then(|c| c.child_requests.get(&full));
                let pending = previous.is_some_and(|r| r.state != CancellationState::Confirmed);
                if let Some(target) = super::template::targets(run,p,step) && ((!repeating && !p.complete() && !p.intents.is_empty()) || pending) {
                    let recorded = if repeating { previous.and_then(|p| p.run) } else { store.host.active_task_run(&target) };
                    targets.insert(full,(target.clone(),recorded));
                }
            }
        }
        if !repeating {
            run.status = Status::Cancelled; run.waiting = None; run.updated_unix_ms = at;
            run.cancellation = Some(Cancellation { requested_unix_ms: at,reason,child_requests: targets.iter().map(|(k,(_,recorded))| (k.clone(),ChildCancellation { run: *recorded,state: CancellationState::Requested,evidence: "Stop requested for the recorded Task run; completed effects remain; no rollback".into() })).collect() });
        }
        Ok(targets)
    })?;
    if targets.is_empty() {
        return Ok(store.read()?.runs[id].clone());
    }
    let _delivery = match store.lock_cancel(id) {
        Ok(lock) => lock,
        Err(why) if why.starts_with("Scenario Run is already executing:") => {
            return Ok(store.read()?.runs[id].clone());
        }
        Err(why) => return Err(why),
    };
    for (path, (target, recorded)) in targets {
        let needed = store.update(|world| {
            let run = world.runs.get_mut(id).ok_or("Scenario Run disappeared")?;
            let request = run
                .cancellation
                .as_mut()
                .and_then(|c| c.child_requests.get_mut(&path))
                .ok_or("Cancellation disappeared")?;
            if request.state == CancellationState::Confirmed {
                return Ok(false);
            }
            request.state = CancellationState::Requested;
            run.updated_unix_ms = run.updated_unix_ms.max(at);
            Ok(true)
        })?;
        if !needed {
            continue;
        }
        let receipt = match recorded {
            Some(expected) if store.host.active_task_run(&target) != Some(expected) => ChildCancellation { run: recorded,state: CancellationState::Confirmed,evidence: "Recorded run no longer owns this Task; no later run was signalled; no rollback".into() },
            Some(expected) => match store.host.cancel_task_run(&target,expected) {
                Ok(_) => ChildCancellation { run: recorded,state: CancellationState::Confirmed,evidence: "Recorded Task run cancellation confirmed; Task receipts retained".into() },
                Err(why) => ChildCancellation { run: recorded,state: CancellationState::Unconfirmed,evidence: format!("Task cancellation not confirmed: {why}; no rollback claimed") },
            },
            None => ChildCancellation { run: None,state: CancellationState::Unconfirmed,evidence: "No active Task run was recorded at cancellation; no unowned run was signalled; the original driver stops only its own child".into() },
        };
        store.update(|world| {
            world
                .runs
                .get_mut(id)
                .and_then(|r| r.cancellation.as_mut())
                .ok_or("Cancellation disappeared")?
                .child_requests
                .insert(path, receipt);
            Ok(())
        })?;
    }
    Ok(store
        .read()?
        .runs
        .get(id)
        .cloned()
        .ok_or("Scenario Run not found")?)
}

/// Recover a native result whose response was lost, including after cancellation or deadline.
/// Missing, running or unreadable Tasks retain the full reservation; no caller supplies usage.
fn reconcile_native_charges(store: &Store, id: &str, at: u64) -> Result<()> {
    let world = store.read()?;
    let run = world.runs.get(id).ok_or("Scenario Run not found")?;
    require(at >= run.updated_unix_ms, "Run clock moved backwards")?;
    let mut settlements = BTreeMap::new();
    for (key, charge) in &run.task_charges {
        if charge.charged.is_some() && !charge.unknown_usage && run.status.active() {
            continue;
        }
        let Some((path, step_id)) = charge.step.rsplit_once('/') else {
            continue;
        };
        let Some(frame) = run.frames.get(path) else {
            continue;
        };
        let Some(step) = run.pin.bundle.definitions[&frame.scenario]
            .steps
            .iter()
            .find(|s| s.id == step_id)
        else {
            continue;
        };
        let Some(progress) = frame.steps.iter().find(|p| p.step == step_id) else {
            continue;
        };
        let targets: Vec<_> = match &step.operation {
            Kind::Task { target } => vec![target.clone()],
            Kind::FreshTask { template, .. } => progress
                .intents
                .iter()
                .filter_map(|intent| {
                    let candidate =
                        hash(&(&template.state, super::template::task_id(&intent.id))).ok()?;
                    (candidate == *key)
                        .then(|| run.fresh_tasks.get(&intent.id).cloned())
                        .flatten()
                })
                .collect(),
            _ => Vec::new(),
        };
        for target in targets {
            if let Some((actual, unknown)) = store.host.task_charge(&target)?
                && (charge.charged != Some(actual) || charge.unknown_usage != unknown)
            {
                settlements.insert(key.clone(), (actual, unknown));
            }
        }
    }
    if settlements.is_empty() {
        return Ok(());
    }
    store.update(|world| {
        let run = world.runs.get_mut(id).ok_or("Scenario Run disappeared")?;
        let mut breached = false;
        for (key, (actual, unknown)) in settlements {
            let charge = run
                .task_charges
                .get_mut(&key)
                .ok_or("Native reservation disappeared")?;
            require(
                charge.charged.is_none_or(|old| old <= actual),
                "Native charge changed after settlement",
            )?;
            charge.charged = Some(actual);
            charge.unknown_usage = unknown;
            breached |= actual > charge.maximum;
        }
        run.updated_unix_ms = run.updated_unix_ms.max(at);
        if (breached || run.tokens()? > u128::from(run.token_allowance()))
            && run.status != Status::Cancelled
        {
            run.fail(
                "Task exceeded its Scenario token reservation; exact recovered charge retained"
                    .into(),
            );
        }
        Ok(())
    })
}

/// Admission never releases uncertain work merely because a date changed. It may recover
/// native terminal usage before considering a new Run, including a previously failed Run.
pub fn reconcile_pending(store: &Store, at: u64) -> Result<()> {
    let ids: Vec<String> = store
        .read()?
        .runs
        .values()
        .filter(|r| r.updated_unix_ms <= at && r.task_charges.values().any(TaskCharge::uncertain))
        .map(|r| r.id.clone())
        .collect();
    for id in ids {
        reconcile_native_charges(store, &id, at)?;
    }
    Ok(())
}

/// One scheduler drive. These fields are rendered by the CLI's existing Ant tick document.
pub struct Tick {
    pub started: Vec<String>,
    pub refused: BTreeMap<String, String>,
    pub busy: Vec<String>,
    pub runs: BTreeMap<String, Run>,
}

/// Admit unique due occurrences, then recover their active Runs and poll existing waits.
/// Admission and execution remain separate durable transitions across driver loss.
pub fn tick(store: &Store, now: u64, clock: Option<u64>) -> Result<Tick> {
    reconcile_pending(store, now)?;
    let (started, refused) = store.update(|world| {
        Ok(crate::schedule::tick_all_with_capacity(
            store.host.as_ref(),
            &mut world.ants,
            &mut world.runs,
            &mut world.roadmaps,
            Some(&world.run_capacity),
            now,
        ))
    })?;
    // Admission and execution are separate durable transitions. Recover an occurrence
    // admitted by a process that died before driving it, and poll existing waits too.
    let active: Vec<String> = store
        .read()?
        .runs
        .values()
        .filter(|r| r.ant.is_some() && r.status.active() && r.updated_unix_ms <= now)
        .map(|r| r.id.clone())
        .collect();
    let mut results = BTreeMap::new();
    let mut busy = Vec::new();
    for id in &active {
        match resume(store, id, clock) {
            Ok(run) => {
                results.insert(id.clone(), run);
            }
            Err(why) if why.starts_with("Scenario Run is already executing:") => {
                busy.push(id.clone())
            }
            Err(why) => return Err(why),
        }
    }

    Ok(Tick {
        started,
        refused,
        busy,
        runs: results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{START, TestHost, limits, simple};
    use std::sync::Arc;

    #[test]
    fn recovered_host_accounting_retains_unknown_reservations_exact_overruns_and_monotonicity() {
        let (mut bundle, mut policy) = simple();
        let target = TaskTarget {
            repo: "/repo".into(),
            state: "/task-state".into(),
            task_id: "native".into(),
            plan_id: digest(b"plan"),
            revision_id: digest(b"revision"),
            max_tokens: 100,
        };
        policy.task_plans.insert(target.plan_id.clone());
        let root = bundle.definitions.get_mut("main").unwrap();
        root.authority.task_plans.insert(target.plan_id.clone());
        root.steps[0].operation = Kind::Task { target };
        root.steps[0].inputs.clear();
        root.steps[0].outputs = BTreeMap::from([("result".into(), Port::Task)]);
        let host = TestHost {
            charge: Some((65, true)),
            ..TestHost::default()
        };
        let pin = Pin::new(&host, bundle, policy).unwrap();
        let mut run = Run::new(
            &host,
            "recovery".into(),
            pin,
            BTreeMap::new(),
            limits(),
            START,
            None,
        )
        .unwrap();
        run.prepare(START).unwrap().unwrap();
        run.fail("Native response lost".into());
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(&directory.path().join("state")).unwrap();
        store.host = Arc::new(host);
        store
            .update(|world| {
                world.admit_run(run)?;
                Ok(())
            })
            .unwrap();
        reconcile_pending(&store, START + 1).unwrap();
        let run = store.read().unwrap().runs.remove("recovery").unwrap();
        // Unknown native usage retains the unmeasured 35 tokens beside the exact 65.
        assert_eq!(run.tokens().unwrap(), 100);
        assert_eq!(run.accounted_tokens().unwrap(), 100);
        assert_eq!(run.task_charges.values().next().unwrap().reserved(), 35);
        assert!(
            run.task_charges
                .values()
                .all(|c| c.charged == Some(65) && c.unknown_usage)
        );
        store.host = Arc::new(TestHost {
            charge: Some((101, false)),
            ..TestHost::default()
        });
        reconcile_pending(&store, START + 2).unwrap();
        let mut run = store.read().unwrap().runs.remove("recovery").unwrap();
        assert_eq!(run.status, Status::Failed);
        assert_eq!(run.tokens().unwrap(), 101);
        assert_eq!(run.accounted_tokens().unwrap(), 101);
        assert!(
            run.task_charges
                .values()
                .all(|c| c.charged == Some(101) && !c.unknown_usage)
        );
        let dispatches = run.dispatches;
        assert!(run.prepare(START + 2).unwrap().is_none());
        assert_eq!(run.dispatches, dispatches);
        store.host = Arc::new(TestHost {
            charge: Some((60, false)),
            ..TestHost::default()
        });
        assert_eq!(
            reconcile_native_charges(&store, "recovery", START + 3).unwrap_err(),
            "Native charge changed after settlement"
        );
        assert_eq!(
            store.read().unwrap().runs["recovery"].tokens().unwrap(),
            101
        );
    }
}
