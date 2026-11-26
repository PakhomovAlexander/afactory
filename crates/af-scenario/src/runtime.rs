//! Drive one recorded Run. A dispatch is committed before leaving the transaction; completion
//! is committed independently, so writer loss leaves an intent that resume must reconcile.
use super::adapter::{self, Settlement};
use super::contract::*;
use super::host::{TaskCancellationObservation, TemplatePlanError};
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
    resume_admitted(store, id, clock, true)
}

pub(crate) fn resume_admitted(
    store: &Store,
    id: &str,
    clock: Option<u64>,
    preflight: bool,
) -> Result<Run> {
    let existing = store
        .read()?
        .runs
        .get(id)
        .cloned()
        .ok_or("Scenario Run not found")?;
    let scoped = existing.pin.policy.implementation_startup.is_some();
    let execution_time = || {
        if scoped {
            store.host.authorization_time()
        } else {
            clock.map_or_else(now, Ok)
        }
    };
    reconcile_native_charges(store, id, execution_time()?)?;
    if let Some(cancellation) = existing.cancellation {
        // Cancellation handoff has its own lock: the original driver may still hold the Run
        // while a process that recorded cancellation died before contacting its active Task.
        return cancel(store, id, cancellation.reason, execution_time()?);
    }
    let _lock = store.lock_run(id)?;
    // Admit the complete immutable pool once per drive. Per-dispatch native binding,
    // expiry and cancellation checks and final adapter authorization remain live.
    let admitted = store.read()?;
    let admitted = admitted.runs.get(id).ok_or("Scenario Run not found")?;
    if preflight && admitted.status.active() {
        super::implementation::preflight(store.host.as_ref(), &admitted.pin)?;
    }
    if admitted.status.active() {
        store.host.check_run_dispatch(store.path(), admitted)?;
    }
    loop {
        let at = execution_time()?;
        let mut cancellation_guard = None;
        let prepared = store.update(|world| {
            crate::retention::prepare_run(
                store.host.as_ref(),
                world,
                id,
                at,
                &mut cancellation_guard,
            )
        })?;
        drop(cancellation_guard);
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
                        let mut cancellation_guard = None;
                        store.update(|world| {
                            let run = world.runs.get_mut(id).ok_or("Run disappeared")?;
                            let at = execution_time()?;
                            if run.status.active()
                                && !run
                                    .reconcile_task_cancellation(
                                        store.host.as_ref(),
                                        at,
                                        &mut cancellation_guard,
                                    )?
                                    .cancelled()
                            {
                                match why {
                                    TemplatePlanError::StorageCapacity(diagnostic)
                                        if dispatch.intent.phase == Phase::Prepared
                                            && !run
                                                .fresh_tasks
                                                .contains_key(&dispatch.intent.id) =>
                                    {
                                        run.wait_for_template_capacity(&dispatch, diagnostic, at)?;
                                    }
                                    TemplatePlanError::StorageCapacity(diagnostic)
                                    | TemplatePlanError::Refused(diagnostic) => {
                                        run.fail(diagnostic)
                                    }
                                }
                            }
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
                    execution_time()?,
                );
            }
            // Materialization itself uses the original Scenario and step allowance.
            dispatch.timeout_ms = dispatch
                .timeout_ms
                .saturating_sub(execution_time()?.saturating_sub(at));
            if dispatch.timeout_ms == 0 {
                continue;
            }
        }
        if let Some(scope) = &run.pin.policy.implementation_startup {
            let remaining = scope
                .expires_unix_ms
                .checked_sub(store.host.authorization_time()?)
                .filter(|n| *n > 0)
                .ok_or("Implementation startup scope expired")?;
            dispatch.timeout_ms = dispatch.timeout_ms.min(remaining);
        }
        store.host.check_run_dispatch(store.path(), &run)?;
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let task_dispatch = matches!(
            dispatch.definition.operation,
            Kind::Task { .. } | Kind::CiDiagnosis { .. } | Kind::FreshTask { .. }
        );
        let full_step = format!("{}/{}", dispatch.frame, dispatch.step);
        let result = std::thread::scope(|scope| {
            let (finished, wait) = std::sync::mpsc::channel();
            let cancellation_ref = &cancellation;
            let startup = run.pin.policy.implementation_startup.as_ref();
            let watcher = scope.spawn(move || {
                loop {
                    match wait.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let expired = startup.is_some_and(|authority| {
                                authority.live(store.host.as_ref()).is_err()
                            });
                            if expired
                                || store.read().map_or(true, |world| {
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
                                })
                            {
                                cancellation_ref.store(true, std::sync::atomic::Ordering::Release);
                                break;
                            }
                        }
                    }
                }
            });
            let result = adapter::dispatch(store.host.as_ref(), &run, &dispatch, &cancellation);
            let _ = finished.send(());
            let _ = watcher.join();
            result
        });
        let result = result.and_then(|settlement| match settlement {
            Settlement::TaskTerminal(outcome) => {
                let outcome = *outcome;
                outcome.validate()?;
                let unknown_usage = outcome.unknown_usage;
                if dispatch.definition.outputs.get("result") == Some(&Port::TaskOutcome) {
                    Ok(Settlement::TaskDone {
                        outputs: BTreeMap::from([("result".into(), Datum::TaskOutcome(outcome))]),
                        unknown_usage,
                    })
                } else if outcome.accepted() {
                    Ok(Settlement::TaskDone {
                        outputs: BTreeMap::from([("result".into(), Datum::Task(outcome.receipt))]),
                        unknown_usage,
                    })
                } else {
                    Ok(Settlement::TaskFailed {
                        diagnostic: format!(
                            "Native Task ended: {:?} ({:?})",
                            outcome.outcome, outcome.cause
                        ),
                        charged_tokens: outcome.receipt.charged_tokens,
                        unknown_usage,
                    })
                }
            }
            other => Ok(other),
        });
        let settled_at = execution_time()?;
        let mut cancellation_guard = None;
        let stop = store.update(|world| {
            let run = world.runs.get_mut(id).ok_or("Scenario Run disappeared")?;
            let cancelled = run.status == Status::Cancelled;
            if task_dispatch && !run.status.active() && !cancelled {
                return Ok(true);
            }
            require(
                settled_at >= run.updated_unix_ms,
                "Completion clock moved backwards",
            )?;
            run.updated_unix_ms = settled_at;
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
            // A response can have been read from a finished Task whose writer still held
            // its lease. Reobserve under the publication fence and keep it through commit.
            let target = match &dispatch.definition.operation {
                Kind::Task { target } | Kind::CiDiagnosis { target } => Some(target),
                _ => dispatch.target.as_ref(),
            };
            let observation = if let Some(target) = target {
                let (observation, guard) = store
                    .host
                    .task_cancellation_for_settlement(target, &dispatch.intent.id)?;
                cancellation_guard = guard;
                observation
            } else {
                TaskCancellationObservation::Observed(None)
            };
            let result = match &observation {
                TaskCancellationObservation::Observed(Some(stop)) => Ok(Settlement::TaskCancelled(stop.clone())),
                _ => result,
            };
            let exact_charge = match &result {
                Ok(Settlement::TaskFailed {
                    charged_tokens,
                    unknown_usage,
                    ..
                })
                | Ok(Settlement::CiDiagnosisDone {
                    charged_tokens,
                    unknown_usage,
                    ..
                }) => Some((*charged_tokens, *unknown_usage)),
                Ok(Settlement::TaskDone {
                    outputs,
                    unknown_usage,
                }) => outputs.values().find_map(|v| match v {
                    Datum::Task(t) => Some((t.charged_tokens, *unknown_usage)),
                    Datum::TaskOutcome(t) => Some((t.receipt.charged_tokens, *unknown_usage)),
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
                charge.unknown_usage = unknown_usage
                    || !matches!(observation, TaskCancellationObservation::Observed(_));
                charge_breached =
                    actual > charge.maximum || run.tokens()? > u128::from(run.token_allowance());
            }
            let result = if matches!(result, Ok(Settlement::TaskCancelled(_))) {
                result
            } else {
                match observation {
                    TaskCancellationObservation::Unavailable(diagnostic) => {
                        if let Err(why) | Ok(Settlement::Uncertain(why)) = result {
                            run.record_task_read_failure(&dispatch.frame, &why, true)?;
                        }
                        run.record_task_read_failure(&dispatch.frame, &diagnostic, true)?;
                        if charge_breached && !cancelled {
                            run.fail("Task exceeded its Scenario token reservation; exact charge retained".into());
                        } else {
                            run.wait_for_task_read(
                                &dispatch.frame,
                                target.ok_or("Unavailable Task target disappeared")?.task_id.clone(),
                            );
                        }
                        return Ok(true);
                    }
                    TaskCancellationObservation::Missing(diagnostic) => {
                        result.and(Err(diagnostic))
                    }
                    TaskCancellationObservation::Observed(_) => result,
                }
            };
            match result {
                Ok(Settlement::TaskCancelled(stop)) => {
                    let key = super::template::charge_key(&dispatch)?;
                    run.settle_task_cancellation(&dispatch.frame, &key, stop)?;
                    Ok(true)
                }
                Ok(Settlement::TaskTerminal(_)) => {
                    unreachable!("terminal outcomes normalized above")
                }
                Ok(Settlement::Done(ref outputs))
                | Ok(Settlement::TaskDone { ref outputs, .. })
                | Ok(Settlement::CiDiagnosisDone { ref outputs, .. }) => {
                    for datum in outputs.values() {
                        datum.validate()?;
                    }
                    run.complete(
                        store.host.as_ref(),
                        &dispatch.frame,
                        &dispatch.step,
                        outputs.clone(),
                    )?;
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
                Ok(Settlement::Progress { ref evidence, .. })
                | Ok(Settlement::Waiting(ref evidence))
                | Ok(Settlement::Uncertain(ref evidence)) => {
                    if let Ok(Settlement::Progress { checkpoint, .. }) = &result {
                        world
                            .drivers
                            .get_mut(id)
                            .ok_or("Native driver receipt missing")?
                            .progress(&dispatch, *checkpoint)?;
                    }
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
                            Kind::Task { target } | Kind::CiDiagnosis { target } => Waiting::Task {
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
        drop(cancellation_guard);
        if stop {
            return Ok(store.read()?.runs[id].clone());
        }
    }
}
pub fn cancel(store: &Store, id: &str, reason: String, at: u64) -> Result<Run> {
    text(&reason)?;
    let mut cancellation_guard = None;
    let targets: BTreeMap<String,(TaskTarget,Option<u64>)> = store.update(|world| {
        let run = world.runs.get_mut(id).ok_or("Scenario Run not found")?;
        require(at >= run.updated_unix_ms,"Cancellation clock moved backwards")?;
        let repeating = run.status == Status::Cancelled;
        require(repeating || run.status.active(),"A terminal Run cannot be cancelled")?;
        let task = run.reconcile_task_cancellation(store.host.as_ref(), at, &mut cancellation_guard)?;
        if task.cancelled() {
            return Ok(BTreeMap::new());
        }
        let mut targets = BTreeMap::new();
        for (path,frame) in &run.frames {
            let def = &run.pin.bundle.definitions[&frame.scenario];
            for (p,step) in frame.steps.iter().zip(&def.steps) {
                let full = format!("{path}/{}",step.id);
                let previous = run.cancellation.as_ref().and_then(|c| c.child_requests.get(&full));
                let pending = previous.is_some_and(|r| r.state != CancellationState::Confirmed);
                if let Some(target) = super::template::targets(run,p,step) && ((!repeating && !p.complete() && !p.intents.is_empty()) || pending) {
                    let recorded = if repeating {
                        previous.and_then(|p| p.run)
                    } else if matches!(&task, TaskReconciliation::Clear) {
                        store.host.active_task_run(&target)
                    } else {
                        None
                    };
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
    drop(cancellation_guard);
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
pub(crate) fn reconcile_native_charges(store: &Store, id: &str, at: u64) -> Result<()> {
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
            Kind::Task { target } | Kind::CiDiagnosis { target } => {
                vec![(target, progress.intents.last())]
            }
            Kind::FreshTask { template, .. } => progress
                .intents
                .iter()
                .filter_map(|intent| {
                    let candidate =
                        hash(&(&template.state, super::template::task_id(&intent.id))).ok()?;
                    (candidate == *key)
                        .then(|| {
                            run.fresh_tasks
                                .get(&intent.id)
                                .map(|target| (target, Some(intent)))
                        })
                        .flatten()
                })
                .collect(),
            _ => Vec::new(),
        };
        for (target, intent) in targets {
            // Read any terminal result first. A cancellation followed by a later manual
            // completion must be visible to the intent-scoped observation below.
            let terminal = store.host.task_charge(target);
            let cancellation = match intent {
                Some(intent) => store.host.task_cancellation(target, &intent.id)?,
                None => TaskCancellationObservation::Observed(None),
            };
            let observed = match cancellation {
                TaskCancellationObservation::Missing(_)
                | TaskCancellationObservation::Unavailable(_) => continue,
                // The driver settles the intent and its charge together.
                TaskCancellationObservation::Observed(Some(_)) if run.status.active() => continue,
                // A request published after a definitive settlement does not change it.
                TaskCancellationObservation::Observed(Some(ref stop))
                    if intent.is_some_and(|i| {
                        i.phase == Phase::Done
                            || (i.phase == Phase::Failed && !i.evidence.contains(&stop.evidence))
                    }) =>
                {
                    continue;
                }
                TaskCancellationObservation::Observed(Some(stop)) => {
                    Some((stop.charged_tokens, stop.unknown_usage))
                }
                TaskCancellationObservation::Observed(None) => terminal?,
            };
            if let Some((actual, unknown)) = observed
                && (charge.charged != Some(actual) || charge.unknown_usage != unknown)
            {
                settlements.insert(key.clone(), (actual, unknown));
            }
        }
    }
    if settlements.is_empty() {
        return Ok(());
    }
    let mut cancellation_guard = None;
    let observed_run = run;
    store.update(|world| {
        let run = world.runs.get_mut(id).ok_or("Scenario Run disappeared")?;
        // Charge observations made outside the transaction cannot replace a settlement
        // that committed while they were being read.
        if run != observed_run
            || !matches!(
                run.reconcile_task_cancellation(store.host.as_ref(), at, &mut cancellation_guard)?,
                TaskReconciliation::Clear
            )
        {
            return Ok(());
        }
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
            && (run.status.active() || run.status == Status::Succeeded)
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
        let scoped = store.read()?.runs[id]
            .pin
            .policy
            .implementation_startup
            .is_some();
        let result = if scoped {
            crate::driver::once_until_idle(store, id)
        } else {
            resume(store, id, clock)
        };
        match result {
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
    fn fresh_task_capacity_wait_reopens_and_reuses_the_original_dispatch() {
        let first = "Artifact Store write hit ENOSPC on first object";
        let later = "Artifact Store write hit ENOSPC on another object";
        for (last_failure, deadline) in [
            (None, false),
            (Some("Pinned authority changed"), false),
            (None, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (roadmap, pin) = crate::admission_tests::fixture();
            let failures =
                [first, later].map(|reason| TemplatePlanError::StorageCapacity(reason.into()));
            let host = Arc::new(TestHost {
                planned_tasks: Some(Default::default()),
                template_failures: std::sync::Mutex::new(
                    failures
                        .into_iter()
                        .chain(last_failure.map(|reason| TemplatePlanError::Refused(reason.into())))
                        .map(Some)
                        .collect(),
                ),
                ..TestHost::default()
            });
            let mut store = Store::isolated(root.path()).unwrap();
            store.host = host.clone();
            store
                .update(|world| {
                    world.roadmaps.insert(roadmap.id.clone(), roadmap);
                    Ok(())
                })
                .unwrap();
            store
                .update(|world| {
                    world.admit_run(Run::new(
                        host.as_ref(),
                        "capacity".into(),
                        pin,
                        BTreeMap::new(),
                        limits(),
                        START,
                        None,
                    )?)?;
                    Ok(())
                })
                .unwrap();
            let waiting = resume(&store, "capacity", Some(START)).unwrap();
            let step = &waiting.frames["root"].steps[0];
            let intent = &step.intents[0];
            assert_eq!(waiting.status, Status::Waiting);
            assert_eq!(step.status, Status::Waiting);
            assert_eq!(intent.phase, Phase::Prepared);
            assert_eq!(intent.evidence, [first]);
            assert_eq!(
                waiting.waiting,
                Some(Waiting::Reconcile {
                    step: format!("root/{}", step.step),
                    intent_id: intent.id.clone(),
                    diagnostic: first.into(),
                })
            );
            assert!(waiting.fresh_tasks.is_empty());
            assert_eq!(waiting.dispatches, 1);
            assert_eq!(waiting.task_charges.values().next().unwrap().charged, None);
            assert_eq!(
                host.native_dispatches
                    .load(std::sync::atomic::Ordering::Relaxed),
                0
            );

            drop(store);
            let mut store = Store::isolated(root.path()).unwrap();
            store.host = host.clone();
            assert_eq!(store.read().unwrap().runs["capacity"], waiting);
            crate::tests::valid("scenario-store-v18.json", &store.read().unwrap());
            let repeated = resume(&store, "capacity", Some(START + 1)).unwrap();
            assert_eq!(repeated.frames, waiting.frames);
            assert_eq!(repeated.waiting, waiting.waiting);
            assert_eq!(repeated.task_charges, waiting.task_charges);
            assert_eq!(repeated.dispatches, waiting.dispatches);

            let at = if deadline {
                step.deadline_unix_ms
            } else {
                START + 2
            };
            let settled = resume(&store, "capacity", Some(at)).unwrap();
            assert_eq!(settled.execution_id, waiting.execution_id);
            let after = &settled.frames["root"].steps[0];
            assert_eq!(after.deadline_unix_ms, step.deadline_unix_ms);
            assert_eq!(after.intents.len(), 1);
            assert_eq!(after.intents[0].id, intent.id);
            assert_eq!(after.intents[0].input_digest, intent.input_digest);
            assert_eq!(after.intents[0].evidence[0], first);
            assert_eq!(settled.dispatches, waiting.dispatches);
            let planned = host.planned_tasks.as_ref().unwrap().lock().unwrap();
            if deadline || last_failure.is_some() {
                assert_eq!(settled.status, Status::Failed);
                assert!(
                    settled
                        .failure
                        .as_deref()
                        .unwrap()
                        .contains(last_failure.unwrap_or("deadline reached"))
                );
                assert!(settled.fresh_tasks.is_empty());
                assert!(planned.is_empty());
                assert_eq!(
                    host.native_dispatches
                        .load(std::sync::atomic::Ordering::Relaxed),
                    0
                );
            } else {
                assert_eq!(settled.status, Status::Succeeded);
                assert_eq!(settled.fresh_tasks.len(), 1);
                assert_eq!(
                    settled.fresh_tasks[&intent.id].task_id,
                    crate::template::task_id(&intent.id)
                );
                assert_eq!(
                    planned.as_slice(),
                    &[settled.fresh_tasks[&intent.id].clone()]
                );
                assert_eq!(
                    host.native_dispatches
                        .load(std::sync::atomic::Ordering::Relaxed),
                    1
                );
                assert_eq!(
                    settled.task_charges.values().next().unwrap().charged,
                    Some(17)
                );
            }
        }
    }

    #[test]
    fn fresh_task_retry_reserves_dispatch_and_reports_its_own_capacity_failure() {
        let previous = "First native Task failed";
        let capacity = "Second Task artifact Store hit ENOSPC";
        for max_dispatches in [1, 2] {
            let root = tempfile::tempdir().unwrap();
            let (_, original) = crate::admission_tests::fixture();
            let mut bundle = original.bundle;
            let frame = bundle.definitions.get_mut("main").unwrap();
            frame.admission = None;
            frame.steps[0].max_attempts = 2;
            let host = Arc::new(TestHost {
                planned_tasks: Some(Default::default()),
                task_settlement: Some(Settlement::TaskFailed {
                    diagnostic: previous.into(),
                    charged_tokens: 17,
                    unknown_usage: false,
                }),
                template_failures: std::sync::Mutex::new(
                    [
                        None,
                        Some(TemplatePlanError::StorageCapacity(capacity.into())),
                    ]
                    .into(),
                ),
                ..TestHost::default()
            });
            let pin = Pin::new(host.as_ref(), bundle, original.policy).unwrap();
            let mut store = Store::isolated(root.path()).unwrap();
            store.host = host.clone();
            store
                .update(|world| {
                    world.admit_run(Run::new(
                        host.as_ref(),
                        "retry".into(),
                        pin,
                        BTreeMap::new(),
                        Limits {
                            max_dispatches,
                            ..limits()
                        },
                        START,
                        None,
                    )?)?;
                    Ok(())
                })
                .unwrap();

            let run = resume(&store, "retry", Some(START)).unwrap();
            let step = &run.frames["root"].steps[0];
            assert_eq!(run.dispatches, max_dispatches);
            assert_eq!(run.frames["root"].dispatches, max_dispatches);
            assert_eq!(run.fresh_tasks.len(), 1);
            assert_eq!(run.task_charges.len(), max_dispatches as usize);
            assert_eq!(
                host.planned_tasks.as_ref().unwrap().lock().unwrap().len(),
                1
            );
            assert_eq!(
                host.native_dispatches
                    .load(std::sync::atomic::Ordering::Relaxed),
                1
            );
            assert_eq!(step.intents[0].phase, Phase::Failed);
            assert_eq!(step.intents[0].evidence, [previous]);
            if max_dispatches == 1 {
                assert_eq!(run.status, Status::Failed);
                assert_eq!(
                    run.failure.as_deref(),
                    Some("Scenario dispatch allowance exhausted")
                );
                assert_eq!(step.intents.len(), 1);
            } else {
                assert_eq!(run.status, Status::Waiting);
                assert_eq!(step.intents.len(), 2);
                let retry = &step.intents[1];
                assert_eq!(retry.phase, Phase::Prepared);
                assert_eq!(retry.evidence, [capacity]);
                assert_eq!(step.diagnostic.as_deref(), Some(capacity));
                assert_eq!(
                    run.waiting,
                    Some(Waiting::Reconcile {
                        step: format!("root/{}", step.step),
                        intent_id: retry.id.clone(),
                        diagnostic: capacity.into(),
                    })
                );
            }
        }
    }

    #[test]
    fn nonaccepted_native_task_settles_nested_steps_with_recorded_accounting() {
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
        let mut child = root.clone();
        child.steps[0].operation = Kind::Task {
            target: target.clone(),
        };
        child.steps[0].inputs.clear();
        child.steps[0].outputs = BTreeMap::from([("result".into(), Port::Task)]);
        root.steps[0].operation = Kind::Scenario {
            scenario: "child".into(),
        };
        root.steps[0].inputs.clear();
        root.steps[0].outputs = child.outputs.clone();
        bundle.definitions.insert("child".into(), child);

        let host = TestHost::default();
        let pin = Pin::new(&host, bundle, policy).unwrap();
        for unknown_usage in [false, true] {
            let outcome = serde_json::from_value(serde_json::json!({
                "schema":"af/scenario-task-outcome@2",
                "receipt":{"task_id":target.task_id,"plan_id":target.plan_id,
                    "revision_id":target.revision_id,"result_id":digest(b"result"),"charged_tokens":17,
                    "result":{"task_revision_id":target.revision_id,"execution":"incomplete",
                        "acceptance":"unsatisfied","domain_conclusion":"gate_failed",
                        "outputs":{},"evidence":[digest(b"check")],"missing_obligations":["goal"]}},
                "outcome":"gate_failed","cause":"check_failed","unknown_usage":unknown_usage,
                "candidate_snapshot_id":null,"checks_id":null,"repair":null
            })).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("state");
            let mut store = Store::isolated(&path).unwrap();
            let host = Arc::new(TestHost {
                task_settlement: Some(Settlement::TaskTerminal(Box::new(outcome))),
                ..TestHost::default()
            });
            store.host = host.clone();
            let run = Run::new(
                host.as_ref(),
                "failed".into(),
                pin.clone(),
                BTreeMap::new(),
                limits(),
                START,
                None,
            )
            .unwrap();
            store.update(|world| world.admit_run(run)).unwrap();
            let failed = resume(&store, "failed", Some(START)).unwrap();
            assert_eq!(failed.status, Status::Failed);
            assert!(failed.waiting.is_none());
            let parent = &failed.frames["root"].steps[0];
            assert_eq!(parent.status, Status::Failed);
            assert_eq!(parent.child.as_deref(), Some("root/review"));
            assert!(parent.outputs.is_empty());
            let task = &failed.frames["root/review"].steps[0];
            assert_eq!(task.status, Status::Failed);
            assert!(task.outputs.is_empty());
            assert_eq!(task.diagnostic, failed.failure);
            let [intent] = task.intents.as_slice() else {
                panic!("expected one native intent")
            };
            assert_eq!(intent.phase, Phase::Failed);
            assert_eq!(
                intent.evidence,
                ["Native Task ended: GateFailed (Some(CheckFailed))"]
            );
            let charge = failed.task_charges.values().next().unwrap();
            assert_eq!(charge.charged, Some(17));
            assert_eq!(charge.unknown_usage, unknown_usage);
            assert_eq!(charge.reserved(), if unknown_usage { 83 } else { 0 });
            assert_eq!(
                host.native_dispatches
                    .load(std::sync::atomic::Ordering::Relaxed),
                1
            );
            drop(store);
            let reopened = Store::isolated(&path).unwrap();
            assert_eq!(resume(&reopened, "failed", Some(START)).unwrap(), failed);
        }
    }

    #[test]
    fn cancelled_native_intent_replays_without_dispatch_and_retains_unknown_usage() {
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
        root.steps[0].max_attempts = 2;
        let host = TestHost::default();
        let pin = Pin::new(&host, bundle, policy).unwrap();
        let mut original = Run::new(
            &host,
            "cancel-replay".into(),
            pin,
            BTreeMap::new(),
            limits(),
            START,
            None,
        )
        .unwrap();
        let dispatch = original.prepare(&host, START).unwrap().unwrap();
        let mut recovered: Run =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        let evidence = "Operator cancelled Task native (run 1, epoch 2)";
        let after_deadline = recovered.frames["root"].steps[0].deadline_unix_ms + 1;
        let observation = |intent_id: String| TestHost {
            startup_time: after_deadline.into(),
            task_cancel: Some((
                intent_id,
                adapter::TaskCancellation {
                    evidence: evidence.into(),
                    charged_tokens: 17,
                    unknown_usage: true,
                },
            )),
            ..TestHost::default()
        };
        let unrelated = observation(digest(b"other-intent"));
        assert!(
            !recovered
                .reconcile_task_cancellation(&unrelated, START + 1, &mut None)
                .unwrap()
                .cancelled()
        );
        assert_eq!(recovered, original);
        let host = Arc::new(observation(dispatch.intent.id.clone()));
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(&directory.path().join("state")).unwrap();
        store.host = host.clone();
        store.update(|world| world.admit_run(recovered)).unwrap();
        let recovered = crate::driver::once(&store, "cancel-replay").unwrap();
        assert_eq!(recovered.failure.as_deref(), Some(evidence));
        assert_eq!(store.read().unwrap().drivers["cancel-replay"].resumes, 0);
        recovered.validate(host.as_ref()).unwrap();
        assert_eq!(recovered.status, Status::Failed);
        assert_eq!(recovered.execution_id, original.execution_id);
        assert_eq!(recovered.dispatches, original.dispatches);
        let step = &recovered.frames["root"].steps[0];
        assert_eq!(step.status, Status::Failed);
        assert!(step.outputs.is_empty());
        assert_eq!(step.intents.len(), 1);
        assert_eq!(step.intents[0].id, dispatch.intent.id);
        assert_eq!(step.intents[0].phase, Phase::Failed);
        assert_eq!(step.intents[0].evidence, [evidence]);
        let charge = recovered.task_charges.values().next().unwrap();
        assert_eq!(charge.charged, Some(17));
        assert_eq!(charge.reserved(), 83);
        assert_eq!(
            crate::driver::once(&store, "cancel-replay").unwrap(),
            recovered
        );
    }

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
        root.steps[0].timeout_ms = 1;
        let host = TestHost {
            task_settlement: Some(Settlement::Waiting("Native response lost".into())),
            ..TestHost::default()
        };
        let pin = Pin::new(&host, bundle, policy).unwrap();
        let run = Run::new(
            &host,
            "recovery".into(),
            pin,
            BTreeMap::new(),
            limits(),
            START,
            None,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(&directory.path().join("state")).unwrap();
        store.host = Arc::new(host);
        store
            .update(|world| {
                world.admit_run(run)?;
                Ok(())
            })
            .unwrap();
        let waiting = resume(&store, "recovery", Some(START)).unwrap();
        assert_eq!(waiting.status, Status::Waiting);
        let intent = &waiting.frames["root"].steps[0].intents[0];
        assert_eq!(intent.phase, Phase::Uncertain);
        let expired = resume(&store, "recovery", Some(START + 1)).unwrap();
        assert_eq!(expired.status, Status::Failed);
        assert_eq!(expired.frames["root"].steps[0].status, Status::Failed);
        assert_eq!(expired.frames["root"].steps[0].intents, [intent.clone()]);
        assert_eq!(expired.task_charges, waiting.task_charges);
        assert_eq!(expired.dispatches, waiting.dispatches);
        store.host = Arc::new(TestHost {
            charge: Some((65, true)),
            ..TestHost::default()
        });
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
        assert!(
            run.prepare(&TestHost::default(), START + 2)
                .unwrap()
                .is_none()
        );
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
