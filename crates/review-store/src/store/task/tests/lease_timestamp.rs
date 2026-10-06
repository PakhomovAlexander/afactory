use super::*;

struct Clock;
impl Clock {
    fn at(time: u64) -> Self {
        TEST_CLOCK.with(|clock| clock.set(Some(time)));
        Self
    }
}
impl Drop for Clock {
    fn drop(&mut self) {
        TEST_CLOCK.with(|clock| clock.set(None));
        AFTER_PROJECTION.with(|hook| hook.borrow_mut().take());
    }
}

fn after_projection(time: u64) {
    AFTER_PROJECTION.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            TEST_CLOCK.with(|clock| clock.set(Some(time)));
        }));
    });
}

#[test]
fn renewal_duration_starts_after_current_cas_validation() {
    let _clock = Clock::at(1_000_000);
    let mut f = Fixture::new(false);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer-1", 15_000)
        .unwrap();
    f.propose(&lease);
    TEST_CLOCK.with(|clock| clock.set(Some(1_006_000)));
    // Validation consumes 8s, but the old lease is still live at completion.
    after_projection(1_014_000);
    PROJECTION_CALLS.with(|calls| calls.set(0));
    let event = f.store.renew_task_lease(&f.cas, &lease, 15_000).unwrap();
    assert_eq!(
        PROJECTION_CALLS.with(|calls| calls.get()),
        1,
        "validate once, not twice"
    );
    let transition = read_task_transition(&event).unwrap();
    assert_eq!(transition.now_unix_ms, 1_014_000);
    assert!(matches!(
        transition.change,
        TaskChangeV1::LeaseRenewed {
            lease_until_unix_ms: 1_029_000
        }
    ));
    assert_eq!(
        f.store.len(&task_run_id("task-1").unwrap()).unwrap(),
        3,
        "one renewal only"
    );
    assert_eq!(f.state().lease_until, 1_029_000, "replay agrees");
}

#[test]
fn validation_crossing_expiry_cannot_resurrect_ownership() {
    let _clock = Clock::at(1_000_000);
    let mut f = Fixture::new(false);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer-1", 15_000)
        .unwrap();
    f.propose(&lease);
    TEST_CLOCK.with(|clock| clock.set(Some(1_006_000)));
    after_projection(1_015_000);
    assert!(f.store.renew_task_lease(&f.cas, &lease, 15_000).is_err());
    assert_eq!(f.store.len(&task_run_id("task-1").unwrap()).unwrap(), 2);
}

#[test]
fn successor_during_validation_fences_the_old_owner() {
    let _clock = Clock::at(1_000_000);
    let mut f = Fixture::new(false);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer-1", 15_000)
        .unwrap();
    f.propose(&lease);
    let mut successor = f
        .store
        .reopen(std::time::Duration::from_millis(50))
        .unwrap();
    let cas = Cas::open(f._dir.path().join("cas")).unwrap();
    AFTER_PROJECTION.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            TEST_CLOCK.with(|clock| clock.set(Some(1_015_000)));
            successor
                .take_task_lease(&cas, "task-1", "writer-2", 15_000)
                .unwrap();
        }));
    });
    assert!(f.store.renew_task_lease(&f.cas, &lease, 15_000).is_err());
    assert_eq!(f.store.len(&task_run_id("task-1").unwrap()).unwrap(), 3);
    assert_eq!(f.state().writer, "writer-2");
}
