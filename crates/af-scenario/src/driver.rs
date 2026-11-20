//! Bounded foreground driving. Reservations and checkpoint acknowledgements share the
//! Scenario journal transaction; a lost process never gets its consumed reservation back.
use crate::{contract::*, state::*, store::Store};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const DEFAULT_POLL_MS: u64 = 30_000;
pub const MAX_RESUMES: u32 = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Driver {
    pub schema: String,
    pub execution_id: String,
    pub pin_id: String,
    pub poll_ms: u64,
    pub resumes: u32,
    pub next_unix_ms: u64,
    #[serde(deserialize_with = "crate::contract::unique")]
    pub acknowledged: BTreeSet<String>,
    pub blocker: Option<Blocker>,
    pub preflight_wait: Option<PreflightWait>,
    pub frontiers: BTreeMap<String, PublicationProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightWait {
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Blocker {
    Exhausted,
    Boundary { diagnostic: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkpoint {
    NativeProofReady,
    PublicationPrepared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationProgress {
    pub input_digest: String,
    pub checkpoint: Checkpoint,
}

/// Validate the whole effective tree, including currently unselected branches, before
/// native planning or execution. Two ordinary observations must fit a publication step.
pub fn validate_schedule(pin: &Pin, poll_ms: u64) -> Result<()> {
    require(
        (1_000..=3_600_000).contains(&poll_ms),
        "Scenario driver poll-ms must be between 1000 and 3600000",
    )?;
    fn visit(pin: &Pin, name: &str, enclosing: u64, poll_ms: u64) -> Result<()> {
        let definition = &pin.bundle.definitions[name];
        let bound = enclosing.min(definition.limits.max_wall_ms);
        for step in &definition.steps {
            let bound = bound.min(step.timeout_ms);
            match &step.operation {
                Kind::Scenario { scenario } => visit(pin, scenario, bound, poll_ms)?,
                Kind::Action { grant, .. }
                    if grant.operation == Operation::PublishImplementationPrV1 =>
                {
                    require(
                        poll_ms * 2 < bound,
                        &format!(
                            "Scenario driver poll-ms {poll_ms} is incompatible with publication step {} (effective enclosing bound {bound} ms); two polls must fit strictly inside its immutable deadline",
                            step.id
                        ),
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    // Do not deduplicate by Scenario identity: a shared child has a distinct effective
    // deadline on each invocation path. Bundle admission already refuses cycles.
    visit(pin, &pin.bundle.root, u64::MAX, poll_ms)?;
    Ok(())
}

pub fn validate_run_schedule(pin: &Pin, limits: &Limits, poll_ms: u64) -> Result<()> {
    validate_schedule(pin, poll_ms)?;
    let publishes = pin.bundle.definitions.values().any(|d| {
        d.steps.iter().any(|s| {
            matches!(&s.operation, Kind::Action { grant, .. }
            if grant.operation == Operation::PublishImplementationPrV1)
        })
    });
    require(
        !publishes || poll_ms * 2 < limits.max_wall_ms,
        "Scenario driver schedule is incompatible with the effective Run elapsed-time allowance",
    )
}

impl Driver {
    pub fn new(run: &Run, poll_ms: u64) -> Result<Self> {
        validate_run_schedule(&run.pin, &run.limits, poll_ms)?;
        Ok(Self {
            schema: "af/scenario-driver@2".into(),
            execution_id: run.execution_id.clone(),
            pin_id: run.pin.revision.clone(),
            poll_ms,
            resumes: 0,
            next_unix_ms: run.created_unix_ms,
            acknowledged: BTreeSet::new(),
            blocker: None,
            preflight_wait: None,
            frontiers: BTreeMap::new(),
        })
    }

    pub fn validate(&self, run: &Run) -> Result<()> {
        require(
            self.schema == "af/scenario-driver@2"
                && self.execution_id == run.execution_id
                && self.pin_id == run.pin.revision
                && self.resumes <= MAX_RESUMES
                && self.acknowledged.len() <= MAX_RESUMES as usize
                && self
                    .acknowledged
                    .iter()
                    .all(|id| af_contracts::is_digest(id))
                && self.next_unix_ms >= run.created_unix_ms
                && self.next_unix_ms <= MAX_TIME,
            "Corrupt Scenario driver identity or reservation",
        )?;
        validate_run_schedule(&run.pin, &run.limits, self.poll_ms)?;
        require(
            self.frontiers.len() <= MAX_RESUMES as usize,
            "Driver progress bound exhausted",
        )?;
        for (id, frontier) in &self.frontiers {
            require(
                run.frames.values().any(|frame| {
                    frame.steps.iter().any(|p| {
                        p.intents
                            .iter()
                            .any(|i| i.id == *id && i.input_digest == frontier.input_digest)
                            && run.pin.bundle.definitions[&frame.scenario]
                                .steps
                                .iter()
                                .any(|s| {
                                    s.id == p.step
                                        && matches!(&s.operation, Kind::Action { grant, .. }
                        if grant.operation == Operation::PublishImplementationPrV1)
                                })
                    })
                }),
                "Driver progress lost its exact scoped publication intent",
            )?;
        }
        if let Some(Blocker::Boundary { diagnostic }) = &self.blocker {
            text(diagnostic)?;
        }
        require(
            self.blocker.is_none() || !run.status.active(),
            "Blocked Scenario driver has an active Run",
        )
    }

    pub(crate) fn validate_revision_of(&self, old: &Self) -> Result<()> {
        require(
            self.schema == old.schema
                && self.execution_id == old.execution_id
                && self.pin_id == old.pin_id
                && self.poll_ms == old.poll_ms
                && self.resumes >= old.resumes
                && self.resumes <= old.resumes.saturating_add(1)
                && self.next_unix_ms >= old.next_unix_ms
                && (self.resumes != old.resumes || self.next_unix_ms == old.next_unix_ms)
                && old.acknowledged.is_subset(&self.acknowledged)
                && self.acknowledged.len()
                    <= old.acknowledged.len() + usize::from(self.resumes != old.resumes)
                && (old.blocker.is_none() || self.blocker == old.blocker),
            "Scenario driver schedule, consumed reservation or acknowledgement changed",
        )?;
        for (id, frontier) in &old.frontiers {
            require(
                self.frontiers.get(id).is_some_and(|new| {
                    new.input_digest == frontier.input_digest
                        && new.checkpoint >= frontier.checkpoint
                }),
                "Scenario driver lost retained native progress",
            )?;
        }
        Ok(())
    }

    fn credit(&self, run: &Run) -> Result<Option<String>> {
        // Only the two closed, native-proof-backed checkpoints admitted by the pinned
        // publication adapter earn credits. Uncertainty and pending prose never do.
        if self.preflight_wait.is_some() {
            return Ok(None);
        }
        let Some(Waiting::Reconcile { intent_id, .. }) = &run.waiting else {
            return Ok(None);
        };
        let Some(frontier) = self.frontiers.get(intent_id) else {
            return Ok(None);
        };
        let credit = hash(&(
            &run.execution_id,
            &run.pin.revision,
            intent_id,
            &frontier.input_digest,
            frontier.checkpoint,
        ))?;
        Ok((!self.acknowledged.contains(&credit)).then_some(credit))
    }

    pub(crate) fn progress(&mut self, dispatch: &Dispatch, checkpoint: Checkpoint) -> Result<()> {
        require(
            matches!(&dispatch.definition.operation, Kind::Action { grant, .. }
            if grant.operation == Operation::PublishImplementationPrV1),
            "Progress outside scoped publication",
        )?;
        if let Some(old) = self.frontiers.get(&dispatch.intent.id) {
            require(
                old.input_digest == dispatch.intent.input_digest && checkpoint >= old.checkpoint,
                "Publication progress regressed or changed inputs",
            )?;
        }
        self.frontiers.insert(
            dispatch.intent.id.clone(),
            PublicationProgress {
                input_digest: dispatch.intent.input_digest.clone(),
                checkpoint,
            },
        );
        Ok(())
    }

    fn reserve(&mut self, run: &Run, at: u64) -> Result<bool> {
        self.validate(run)?;
        if !run.status.active() || self.blocker.is_some() {
            return Ok(false);
        }
        let credit = self.credit(run)?;
        if at < self.next_unix_ms && credit.is_none() {
            return Ok(false);
        }
        if self.resumes == MAX_RESUMES {
            self.blocker = Some(Blocker::Exhausted);
            return Ok(false);
        }
        // Acknowledge even when already due: restart cannot save up a replayed receipt.
        if let Some(credit) = credit {
            self.acknowledged.insert(credit);
        }
        self.resumes += 1;
        self.next_unix_ms = at
            .checked_add(self.poll_ms)
            .filter(|n| *n <= MAX_TIME)
            .ok_or("Scenario driver reservation time overflow")?;
        Ok(true)
    }
}

/// One durable reservation, useful to service supervisors as well as the foreground CLI.
/// No supplied clock can backdate authorization or make a reservation due.
pub fn once(store: &Store, id: &str) -> Result<Run> {
    once_inner(store, id)
}

/// Completed child frames retain evidence, not a deadline on subsequent parent work.
/// Use the same frontier for expiry and sleeping so an expired completed child cannot
/// either stop the Run or turn the service's next wake into an immediate busy loop.
fn deadline(run: &Run) -> u64 {
    let mut due = run.created_unix_ms + run.limits.max_wall_ms;
    if let Some(scope) = &run.pin.policy.implementation_startup {
        due = due.min(scope.expires_unix_ms);
    }
    for frame in run.frames.values().filter(|frame| frame.outputs.is_none()) {
        due = due.min(
            frame.started_unix_ms
                + run.pin.bundle.definitions[&frame.scenario]
                    .limits
                    .max_wall_ms,
        );
        for progress in frame.steps.iter().filter(|p| p.status.active()) {
            due = due.min(progress.deadline_unix_ms);
        }
    }
    due
}

fn once_inner(store: &Store, id: &str) -> Result<Run> {
    let _driver = store.lock_driver(id)?;
    let at = store.host.authorization_time()?;
    crate::runtime::reconcile_native_charges(store, id, at)?;
    if store
        .read()?
        .runs
        .get(id)
        .ok_or("Scenario Run not found")?
        .cancellation
        .is_some()
    {
        // Recover the exact retained cancellation handoff, even after the driver stopped.
        return crate::runtime::resume_admitted(store, id, None, false);
    }
    let reserved = store.update(|world| {
        let run = world.runs.get_mut(id).ok_or("Scenario Run not found")?;
        let driver = world
            .drivers
            .get_mut(id)
            .ok_or("Scenario driver was not admitted at startup")?;
        let expired = deadline(run) <= at;
        if run.status.active() && expired {
            let diagnostic =
                "Scenario driver reached immutable elapsed-time or startup expiry boundary"
                    .to_string();
            run.updated_unix_ms = run.updated_unix_ms.max(at);
            run.fail(diagnostic.clone());
            driver.blocker = Some(Blocker::Boundary { diagnostic });
        }
        let reserved = driver.reserve(run, at)?;
        if driver.blocker == Some(Blocker::Exhausted) && run.status.active() {
            run.fail("Scenario driver exhausted its immutable 512 resume reservations".into());
        }
        Ok(reserved)
    })?;
    if reserved {
        let pin = store.read()?.runs[id].pin.clone();
        let preflight = crate::implementation::preflight(store.host.as_ref(), &pin);
        store.update(|world| {
            world
                .drivers
                .get_mut(id)
                .ok_or("Driver disappeared")?
                .preflight_wait = matches!(
                &preflight,
                Err(crate::implementation::PreflightError::Transport(_))
            )
            .then_some(PreflightWait::Transport);
            Ok(())
        })?;
        let (result, runtime) = match preflight {
            Ok(()) => (
                crate::runtime::resume_admitted(store, id, None, false),
                true,
            ),
            Err(crate::implementation::PreflightError::Transport(_)) => {
                // No runtime dispatch is allowed until complete preflight succeeds.
                // This already consumed reservation retains cadence across process loss.
                return Ok(store.read()?.runs[id].clone());
            }
            Err(error) => (Err(String::from(error)), false),
        };
        if let Err(why) = result {
            // Busy is a transient wait; the already consumed reservation stays consumed.
            if !runtime || !why.starts_with("Scenario Run is already executing:") {
                store.update(|world| {
                    let run = world.runs.get_mut(id).ok_or("Scenario Run disappeared")?;
                    if run.status.active() {
                        run.fail(why.clone());
                        world
                            .drivers
                            .get_mut(id)
                            .ok_or("Driver disappeared")?
                            .blocker = Some(Blocker::Boundary { diagnostic: why });
                    }
                    Ok(())
                })?;
            }
        }
    }
    store.update(|world| {
        let run = world.runs.get(id).ok_or("Run disappeared")?;
        let driver = world.drivers.get_mut(id).ok_or("Driver disappeared")?;
        if driver.blocker.is_none() && run.status == Status::Failed {
            driver.blocker = Some(Blocker::Boundary {
                diagnostic: run.failure.clone().ok_or("Missing failure")?,
            });
        }
        Ok(())
    })?;
    Ok(store
        .read()?
        .runs
        .get(id)
        .ok_or("Scenario Run disappeared")?
        .clone())
}

/// Drain only newly observed finite progress. A pending observation returns immediately.
pub fn once_until_idle(store: &Store, id: &str) -> Result<Run> {
    loop {
        let run = once_inner(store, id)?;
        let world = store.read()?;
        if !run.status.active() || world.drivers[id].credit(&run)?.is_none() {
            return Ok(run);
        }
    }
}

/// Foreground service: killing it preserves the reservation and original Run deadlines.
/// An external supervisor restarts this same command against the same Store and Run ID.
pub fn drive(store: &Store, id: &str) -> Result<Run> {
    drive_until(store, id, || false)
}

pub fn drive_until(store: &Store, id: &str, stop: impl Fn() -> bool) -> Result<Run> {
    loop {
        if stop() {
            return store
                .read()?
                .runs
                .get(id)
                .cloned()
                .ok_or("Scenario Run disappeared".into());
        }
        let run = once_inner(store, id)?;
        if !run.status.active() {
            return Ok(run);
        }
        let world = store.read()?;
        let driver = world.drivers.get(id).ok_or("Scenario driver disappeared")?;
        if driver.credit(&run)?.is_some() {
            continue;
        }
        let now = store.host.authorization_time()?;
        let due = driver.next_unix_ms.min(deadline(&run));
        let delay = due.saturating_sub(now).min(30_000);
        let wake = std::time::Instant::now() + std::time::Duration::from_millis(delay);
        while !stop() {
            let remaining = wake.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(remaining.min(std::time::Duration::from_secs(1)));
            if stop() {
                break;
            }
            // Cancellation is a checked handoff, not another dispatch reservation.
            // Re-enter once_inner to reconcile it; never return an unchecked status.
            // One elapsed deadline includes journal reads without extending idle cadence.
            if std::time::Instant::now() < wake
                && store
                    .read()?
                    .runs
                    .get(id)
                    .ok_or("Scenario Run disappeared")?
                    .cancellation
                    .is_some()
            {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{DAY, START, TestHost, limits, simple};

    fn run() -> Run {
        let (bundle, policy) = simple();
        Run::new(
            &TestHost::default(),
            "driver".into(),
            Pin::new(&TestHost::default(), bundle, policy).unwrap(),
            BTreeMap::new(),
            limits(),
            START,
            None,
        )
        .unwrap()
    }

    #[test]
    fn cancellation_wakes_idle_driver_without_refunding_its_due_reservation() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        };
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(&directory.path().join("state")).unwrap();
        let at = crate::runtime::now().unwrap();
        store.host = Arc::new(TestHost {
            live_clock: true,
            ..TestHost::default()
        });
        let (bundle, policy) = simple();
        let pin = Pin::new(store.host.as_ref(), bundle, policy).unwrap();
        let run = Run::new(
            store.host.as_ref(),
            "cancel-wait".into(),
            pin,
            BTreeMap::new(),
            limits(),
            at,
            None,
        )
        .unwrap();
        store.update(|world| world.admit_run(run)).unwrap();
        let waiting = once(&store, "cancel-wait").unwrap();
        assert_eq!(waiting.status, Status::Waiting);
        let before = store.read().unwrap().drivers["cancel-wait"].clone();
        assert_eq!(before.resumes, 1);
        assert!(before.next_unix_ms >= at + DEFAULT_POLL_MS);

        let stop = Arc::new(AtomicBool::new(false));
        let (sleeping_tx, sleeping_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let owned_store = store.clone();
        let owned_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let checks = AtomicUsize::new(0);
            let result = drive_until(&owned_store, "cancel-wait", || {
                // The first check starts the loop; the second enters its retained wait.
                if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                    let _ = sleeping_tx.send(());
                }
                owned_stop.load(Ordering::SeqCst)
            });
            let _ = finished_tx.send(result);
        });
        let outcome = (|| -> Result<Run> {
            sleeping_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|why| {
                    let result = match finished_rx.try_recv() {
                        Ok(Err(error)) => error,
                        Ok(Ok(run)) => format!("Driver stopped with {:?}", run.status),
                        Err(error) => error.to_string(),
                    };
                    format!("Driver did not enter its retained wait: {why}; {result}")
                })?;
            crate::runtime::cancel(
                &store,
                "cancel-wait",
                "Stop the recorded Run".into(),
                crate::runtime::now()?,
            )?;
            finished_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| "Cancelled driver slept until its next dispatch reservation")?
        })();
        // Even the unfixed driver's failure path releases its thread promptly.
        stop.store(true, Ordering::SeqCst);
        worker.join().unwrap();
        let cancelled = outcome.unwrap();
        assert_eq!(cancelled.status, Status::Cancelled);
        assert!(cancelled.cancellation.is_some());
        let after = store.read().unwrap();
        assert_eq!(after.drivers["cancel-wait"], before);
        assert_eq!(after.runs["cancel-wait"].dispatches, waiting.dispatches);
        assert_eq!(after.runs["cancel-wait"].task_charges, waiting.task_charges);
    }

    #[test]
    fn reservation_replays_without_clock_or_idle_credits_and_exhausts_exactly() {
        let run = run();
        let mut driver = Driver::new(&run, DEFAULT_POLL_MS).unwrap();
        assert!(driver.reserve(&run, START).unwrap());
        let encoded = serde_json::to_vec(&driver).unwrap();
        let mut replay: Driver = serde_json::from_slice(&encoded).unwrap();
        for at in [START, START + 1, START + DEFAULT_POLL_MS - 1] {
            assert!(!replay.reserve(&run, at).unwrap());
            assert_eq!(replay, driver);
        }
        for n in 1..MAX_RESUMES {
            assert!(
                replay
                    .reserve(&run, START + u64::from(n) * DEFAULT_POLL_MS)
                    .unwrap()
            );
        }
        assert!(
            !replay
                .reserve(&run, START + u64::from(MAX_RESUMES) * DEFAULT_POLL_MS)
                .unwrap()
        );
        assert_eq!(replay.resumes, MAX_RESUMES);
        assert_eq!(replay.blocker, Some(Blocker::Exhausted));
        crate::tests::valid("scenario-driver-v2.json", &driver);
        let mut retired = serde_json::to_value(&driver).unwrap();
        retired["schema"] = serde_json::json!("af/scenario-driver@1");
        retired.as_object_mut().unwrap().remove("preflight_wait");
        assert!(!crate::tests::schema("scenario-driver-v2.json").is_valid(&retired));
        driver.execution_id = digest(b"foreign");
        assert!(driver.validate(&run).is_err());
    }

    #[test]
    fn retained_schedule_and_consumed_reservations_cannot_be_rewritten() {
        let run = run();
        let mut driver = Driver::new(&run, DEFAULT_POLL_MS).unwrap();
        driver.reserve(&run, START).unwrap();
        for change in [0, 1, 2, 3] {
            let mut wrong = driver.clone();
            match change {
                0 => wrong.poll_ms = 1_000,
                1 => wrong.resumes = 0,
                2 => wrong.next_unix_ms -= 1,
                _ => {
                    wrong.acknowledged.insert(digest(b"caller sequence"));
                }
            }
            assert!(wrong.validate_revision_of(&driver).is_err());
        }
        assert!(driver.validate_revision_of(&driver).is_ok());
    }

    #[test]
    fn complete_schedule_checks_unselected_publication_before_any_dispatch() {
        let mut run = run();
        let definition = run.pin.bundle.definitions.get_mut("main").unwrap();
        definition.steps[0].operation = Kind::Action {
            grant: Grant {
                adapter: "workspace".into(),
                operation: Operation::PublishImplementationPrV1,
                destination: "github:owner/project".into(),
            },
            requires_decision: false,
            required_condition: None,
        };
        definition.steps[0].timeout_ms = 120_000;
        definition.limits.max_wall_ms = 600_000;
        assert!(validate_schedule(&run.pin, DEFAULT_POLL_MS).is_ok());
        for poll in [0, 999, 60_000, 120_000, u64::MAX] {
            assert!(validate_schedule(&run.pin, poll).is_err(), "{poll}");
        }
        assert_eq!(run.dispatches, 0);
        assert!(run.task_charges.is_empty());
    }

    #[test]
    fn publication_schedule_checks_every_ancestor_and_shared_invocation_path() {
        let mut run = run();
        let mut child = run.pin.bundle.definitions["main"].clone();
        child.steps[0].operation = Kind::Action {
            grant: Grant {
                adapter: "workspace".into(),
                operation: Operation::PublishImplementationPrV1,
                destination: "github:owner/project".into(),
            },
            requires_decision: false,
            required_condition: None,
        };
        child.steps[0].timeout_ms = 120_000;
        child.limits.max_wall_ms = 180_000;
        let mut parent = child.clone();
        parent.steps[0].operation = Kind::Scenario {
            scenario: "publication".into(),
        };
        parent.steps[0].timeout_ms = 90_000;
        parent.limits.max_wall_ms = 150_000;
        run.pin
            .bundle
            .definitions
            .insert("publication".into(), child);
        run.pin
            .bundle
            .definitions
            .insert("other-parent".into(), parent.clone());
        run.pin.bundle.definitions.insert("parent".into(), parent);
        let root = run.pin.bundle.definitions.get_mut("main").unwrap();
        root.steps[0].operation = Kind::Scenario {
            scenario: "parent".into(),
        };
        root.steps[0].timeout_ms = 100_000;
        root.limits.max_wall_ms = 200_000;
        assert!(validate_run_schedule(&run.pin, &run.limits, 30_000).is_ok());
        // A second invocation of the same child must not reuse the first path's admission.
        let root = run.pin.bundle.definitions.get_mut("main").unwrap();
        let mut second = root.steps[0].clone();
        second.id = "second".into();
        second.operation = Kind::Scenario {
            scenario: "other-parent".into(),
        };
        second.timeout_ms = 45_000;
        second.when = Some(Binding::Literal {
            value: Box::new(Datum::Boolean(false)),
        });
        root.steps.push(second);
        assert!(validate_schedule(&run.pin, 30_000).is_err());
        for bound in [59_999, 60_000, 60_001] {
            run.pin.bundle.definitions.get_mut("main").unwrap().steps[1].timeout_ms = bound;
            assert_eq!(validate_schedule(&run.pin, 30_000).is_ok(), bound > 60_000);
        }
        run.pin
            .bundle
            .definitions
            .get_mut("parent")
            .unwrap()
            .limits
            .max_wall_ms = 60_000;
        assert!(validate_schedule(&run.pin, 30_000).is_err());
        run.pin
            .bundle
            .definitions
            .get_mut("parent")
            .unwrap()
            .limits
            .max_wall_ms = 60_001;
        assert!(validate_schedule(&run.pin, 30_000).is_ok());
        let mut limits = run.limits.clone();
        limits.max_wall_ms = 60_000;
        assert!(validate_run_schedule(&run.pin, &limits, 30_000).is_err());
        limits.max_wall_ms += 1;
        assert!(validate_run_schedule(&run.pin, &limits, 30_000).is_ok());
        assert_eq!(run.dispatches, 0);
        assert!(run.task_charges.is_empty());
    }

    #[test]
    fn persisted_expiry_and_exhaustion_are_terminal_without_native_dispatch() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::isolated(&directory.path().join("state")).unwrap();
        let mut run = run();
        run.created_unix_ms = START - run.limits.max_wall_ms;
        run.frames.get_mut("root").unwrap().started_unix_ms = run.created_unix_ms;
        store
            .update(|w| {
                w.admit_run(run)?;
                Ok(())
            })
            .unwrap();
        let stopped = once(&store, "driver").unwrap();
        assert_eq!(stopped.status, Status::Failed);
        assert_eq!(stopped.dispatches, 0);
        assert!(matches!(
            store.read().unwrap().drivers["driver"].blocker,
            Some(Blocker::Boundary { .. })
        ));
        assert_eq!(once(&store, "driver").unwrap(), stopped);
        crate::tests::valid("scenario-store-v17.json", &store.read().unwrap());
        let mut retired = serde_json::to_value(store.read().unwrap()).unwrap();
        retired["schema"] = serde_json::json!("af/scenario-store@15");
        retired.as_object_mut().unwrap().remove("drivers");
        assert!(serde_json::from_value::<crate::store::World>(retired).is_err());
    }

    #[test]
    fn completed_child_deadline_does_not_expire_parent_wait_after_restart() {
        let at = crate::runtime::now().unwrap();
        let started = at - 2 * DAY;
        let (mut bundle, policy) = simple();
        let mut child = bundle.definitions["main"].clone();
        child.limits.max_wall_ms = DAY;
        child.steps[0].timeout_ms = DAY;
        child.steps[0].when = Some(Binding::Literal {
            value: Box::new(Datum::Boolean(false)),
        });
        let parent = bundle.definitions.get_mut("main").unwrap();
        let mut invoke = parent.steps[0].clone();
        invoke.id = "child".into();
        invoke.inputs.clear();
        invoke.outputs = child.outputs.clone();
        invoke.timeout_ms = DAY;
        invoke.operation = Kind::Scenario {
            scenario: "child".into(),
        };
        parent.steps.insert(0, invoke);
        bundle.definitions.insert("child".into(), child);
        let host = std::sync::Arc::new(TestHost::default());
        host.startup_time
            .store(at, std::sync::atomic::Ordering::Relaxed);
        let run = Run::new(
            host.as_ref(),
            "parent".into(),
            Pin::new(host.as_ref(), bundle, policy).unwrap(),
            BTreeMap::new(),
            limits(),
            started,
            None,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state");
        let mut store = Store::isolated(&path).unwrap();
        store.host = host.clone();
        store.update(|w| w.admit_run(run)).unwrap();
        let waiting = crate::runtime::resume(&store, "parent", Some(started)).unwrap();
        assert_eq!(waiting.status, Status::Waiting);
        assert!(waiting.frames["root/child"].outputs.is_some());
        assert_eq!(deadline(&waiting), started + 9 * DAY);
        let original_child = waiting.frames["root/child"].clone();
        drop(store);

        let mut store = Store::isolated(&path).unwrap();
        store.host = host.clone();
        let resumed = once(&store, "parent").unwrap();
        assert_eq!(resumed.status, Status::Waiting);
        assert_eq!(resumed.frames["root/child"], original_child);
        assert_eq!(resumed.dispatches, waiting.dispatches);
        assert_eq!(resumed.task_charges, waiting.task_charges);
        assert!(store.read().unwrap().drivers["parent"].blocker.is_none());

        // The active parent's own immutable deadline still stops the Run exactly.
        host.startup_time
            .store(started + 9 * DAY, std::sync::atomic::Ordering::Relaxed);
        let stopped = once(&store, "parent").unwrap();
        assert_eq!(stopped.status, Status::Failed);
        assert!(matches!(
            store.read().unwrap().drivers["parent"].blocker,
            Some(Blocker::Boundary { .. })
        ));
    }
}
