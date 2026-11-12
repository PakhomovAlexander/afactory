use crate::{schedule::*, store::Store, tests::*};
use std::collections::BTreeMap;
fn interval(seconds: u64) -> AntState {
    let mut def = ant("UTC", 12, 0).pin.definition;
    def.schedule = Schedule::Interval {
        seconds,
        start_unix_ms: START,
    };
    let (_, policy) = simple();
    let mut state = AntState::new(
        AntPin::new(&TestHost::default(), def, policy).unwrap(),
        START,
    )
    .unwrap();
    state.change(AntMode::Enabled, START).unwrap();
    state
}
#[test]
fn native_interval_boundaries_replay_overlap_misfire_and_clock_recovery() {
    for seconds in [1, 5, 60, 900, 9_007_199_254_740_991] {
        let mut ant = interval(seconds);
        let mut runs = BTreeMap::new();
        assert_eq!(ant.next_due_unix_ms().unwrap(), Some(START));
        assert_eq!(
            ant.tick(&TestHost::default(), START, &mut runs)
                .unwrap()
                .len(),
            1
        );
        assert!(
            ant.tick(&TestHost::default(), START, &mut runs)
                .unwrap()
                .is_empty()
        );
        ant.validate(&TestHost::default(), &runs).unwrap();
        valid("ant-state-v7.json", &ant);
        let text = serde_json::to_string(&ant).unwrap();
        ant = serde_json::from_str(&text).unwrap();
        if seconds > 900 {
            assert_eq!(ant.next_due_unix_ms().unwrap(), None);
            continue;
        }
        let next = START + seconds * 1000;
        assert!(
            ant.tick(&TestHost::default(), next - 1, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert!(
            ant.tick(&TestHost::default(), next, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            ant.occurrences.last().unwrap().outcome,
            OccurrenceOutcome::SkippedBusy
        );
        let before = ant.clone();
        assert!(ant.tick(&TestHost::default(), START, &mut runs).is_err());
        assert_eq!(before, ant);
        let later = next + seconds * 1000 * 1_000_000;
        assert!(
            ant.tick(&TestHost::default(), later, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert_eq!(ant.occurrences.len(), 4); // first, busy, one compact range, current busy
        assert_eq!(ant.occurrences[2].through_index, 1_000_000);
        assert_eq!(ant.occurrences[2].outcome, OccurrenceOutcome::SkippedMissed);
        ant.validate(&TestHost::default(), &runs).unwrap();
        valid("ant-state-v7.json", &ant);
        assert!(
            ant.tick(&TestHost::default(), later + 1, &mut runs)
                .unwrap()
                .is_empty()
        );
        for run in runs.values_mut() {
            run.fail("fixture settled".into());
        }
        let next = later + seconds * 1000;
        assert_eq!(
            ant.tick(&TestHost::default(), next, &mut runs)
                .unwrap()
                .len(),
            1
        );
        ant.validate(&TestHost::default(), &runs).unwrap();
        ant.change(AntMode::Paused, next).unwrap();
        ant.tick(&TestHost::default(), next + seconds * 1000, &mut runs)
            .unwrap();
        assert_eq!(
            ant.occurrences.last().unwrap().outcome,
            OccurrenceOutcome::SkippedPaused
        );
        assert_eq!(runs.values().filter(|r| r.status.active()).count(), 1);
    }
}
#[test]
fn interval_pause_disable_and_invalid_intervals_keep_authority_closed() {
    let mut ant = interval(1);
    ant.change(AntMode::Paused, START).unwrap();
    let mut runs = BTreeMap::new();
    ant.tick(&TestHost::default(), START, &mut runs).unwrap();
    assert_eq!(ant.occurrences[0].outcome, OccurrenceOutcome::SkippedPaused);
    ant.change(AntMode::Disabled, START + 1).unwrap();
    ant.tick(&TestHost::default(), START + 1000, &mut runs)
        .unwrap();
    assert_eq!(
        ant.occurrences[1].outcome,
        OccurrenceOutcome::SkippedDisabled
    );
    for seconds in [0, u64::MAX] {
        ant.pin.definition.schedule = Schedule::Interval {
            seconds,
            start_unix_ms: START,
        };
        assert!(ant.pin.definition.validate(&TestHost::default()).is_err());
    }
}
#[test]
fn scheduler_lease_stop_crash_status_and_output_index_replay() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::isolated(temp.path()).unwrap();
    assert_eq!(store.scheduler_status().unwrap()["running"], false);
    assert!(store.scheduler_start(0).is_err());
    assert!(store.scheduler_start(u64::MAX).is_err());
    let lease = store.scheduler_start(START).unwrap();
    assert!(store.scheduler_start(START).is_err());
    assert_eq!(store.scheduler_status().unwrap()["running"], true);
    assert!(!store.scheduler_heartbeat(START, None).unwrap());
    store.scheduler_stop().unwrap();
    assert!(
        store
            .scheduler_heartbeat(START + 1000, Some("fixture"))
            .unwrap()
    );
    drop(lease);
    assert_eq!(store.scheduler_status().unwrap()["running"], false);
    let _restart = store.scheduler_start(START + 2000).unwrap();
    assert!(!store.scheduler_heartbeat(START + 2000, None).unwrap());
    let ant = interval(1);
    store
        .update(|world| {
            world.ants.insert("daily".into(), ant);
            Ok(())
        })
        .unwrap();
    let view = store.read_output(true, "daily").unwrap();
    assert!(view.run.is_none());
    assert!(view.outputs.contains_key("ok"));
    store.read().unwrap();
}

#[test]
fn interval_serde_refusals_and_old_generations_fail_closed() {
    let ant = interval(1);
    for value in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("1"),
        serde_json::json!(null),
    ] {
        let mut wire = serde_json::to_value(&ant.pin.definition).unwrap();
        wire["schedule"]["seconds"] = value;
        assert!(serde_json::from_value::<Ant>(wire).is_err());
    }
    let mut old = ant.clone();
    old.schema = "af/ant-state@5".into();
    assert!(
        old.validate(&TestHost::default(), &BTreeMap::new())
            .is_err()
    );
    let mut old = ant.pin.clone();
    old.schema = "af/ant-pin@5".into();
    assert!(old.validate(&TestHost::default()).is_err());
    let mut old = ant.pin.definition;
    old.schema = "af/ant@5".into();
    assert!(old.validate(&TestHost::default()).is_err());
    let root = tempfile::tempdir().unwrap();
    let store = Store::isolated(root.path()).unwrap();
    let mut world = store.read().unwrap();
    world.schema = "af/scenario-store@6".into();
    assert!(world.validate(&TestHost::default()).is_err());
    std::fs::write(
        root.path().join("runs/scenario-owner"),
        b"af/scenario-store@6\n",
    )
    .unwrap();
    assert!(Store::isolated(root.path()).is_err());
}

#[test]
fn concurrent_interval_admission_has_one_receipt_and_current_store_replays() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::isolated(root.path()).unwrap();
    store
        .update(|world| {
            world.ants.insert("daily".into(), interval(1));
            Ok(())
        })
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .update(|world| {
                        let (started, refused) = crate::schedule::tick_all(
                            &TestHost::default(),
                            &mut world.ants,
                            &mut world.runs,
                            START,
                        );
                        assert!(refused.is_empty());
                        Ok(started.len())
                    })
                    .unwrap()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum::<usize>(),
        1
    );
    let world = store.read().unwrap();
    assert_eq!(world.runs.len(), 1);
    assert_eq!(world.ants["daily"].occurrences.len(), 1);
    assert_eq!(
        store.read_output(true, "daily").unwrap().run.as_ref(),
        world.runs.keys().next()
    );
    assert_eq!(Store::isolated(root.path()).unwrap().read().unwrap(), world);
}
