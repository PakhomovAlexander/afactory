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
    pub frontiers: BTreeMap<String, PublicationProgress>,
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
    for definition in pin.bundle.definitions.values() {
        for step in &definition.steps {
            if matches!(&step.operation, Kind::Action { grant, .. }
                if grant.operation == Operation::PublishImplementationPrV1)
            {
                require(
                    poll_ms * 2 < step.timeout_ms.min(definition.limits.max_wall_ms),
                    &format!(
                        "Scenario driver poll-ms {poll_ms} is incompatible with publication step {} ({} ms); two polls must fit strictly inside its immutable deadline",
                        step.id, step.timeout_ms
                    ),
                )?;
            }
        }
    }
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
            schema: "af/scenario-driver@1".into(),
            execution_id: run.execution_id.clone(),
            pin_id: run.pin.revision.clone(),
            poll_ms,
            resumes: 0,
            next_unix_ms: run.created_unix_ms,
            acknowledged: BTreeSet::new(),
            blocker: None,
            frontiers: BTreeMap::new(),
        })
    }

    pub fn validate(&self, run: &Run) -> Result<()> {
        require(
            self.schema == "af/scenario-driver@1"
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
    once_inner(store, id, true)
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

fn once_inner(store: &Store, id: &str, preflight: bool) -> Result<Run> {
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
        if let Err(why) = crate::runtime::resume_admitted(store, id, None, preflight) {
            // Busy is a transient wait; the already consumed reservation stays consumed.
            if !why.starts_with("Scenario Run is already executing:") {
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
    let mut preflight = true;
    loop {
        let run = once_inner(store, id, preflight)?;
        preflight = false;
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
    let initial = store
        .read()?
        .runs
        .get(id)
        .ok_or("Scenario Run not found")?
        .clone();
    if initial.status.active() {
        if let Err(why) = crate::implementation::preflight(store.host.as_ref(), &initial.pin) {
            store.update(|world| {
                world
                    .runs
                    .get_mut(id)
                    .ok_or("Run disappeared")?
                    .fail(why.clone());
                world
                    .drivers
                    .get_mut(id)
                    .ok_or("Driver disappeared")?
                    .blocker = Some(Blocker::Boundary { diagnostic: why });
                Ok(())
            })?;
        }
    }
    loop {
        if stop() {
            return store
                .read()?
                .runs
                .get(id)
                .cloned()
                .ok_or("Scenario Run disappeared".into());
        }
        let run = once_inner(store, id, false)?;
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
        let mut delay = delay;
        while delay > 0 && !stop() {
            let chunk = delay.min(1_000);
            std::thread::sleep(std::time::Duration::from_millis(chunk));
            delay -= chunk;
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
        crate::tests::valid("scenario-driver-v1.json", &driver);
        let mut retired = serde_json::to_value(&driver).unwrap();
        retired["schema"] = serde_json::json!("af/scenario-driver@0");
        assert!(!crate::tests::schema("scenario-driver-v1.json").is_valid(&retired));
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
        crate::tests::valid("scenario-store-v14.json", &store.read().unwrap());
        let mut retired = serde_json::to_value(store.read().unwrap()).unwrap();
        retired["schema"] = serde_json::json!("af/scenario-store@13");
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
