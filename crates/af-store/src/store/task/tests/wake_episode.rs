use super::*;

#[test]
fn wake_episodes_follow_resume_and_recovered_execution_but_not_maintenance_leases() {
    super::super::test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false);
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.store.begin_task_execution(&f.cas, &lease).unwrap();
        let first = f.state().lifecycle_episode();
        let wait = f
            .store
            .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
            .unwrap();
        f.store.resume_task(&f.cas, &lease, &f.authority).unwrap();
        assert_eq!(f.state().lifecycle_episode(), first + 1);
        assert_eq!(
            f.store
                .task_projection_at_sequence(&f.cas, lease.task_id(), wait.sequence)
                .unwrap()
                .unwrap()
                .lifecycle_episode(),
            first
        );
        clock.set(f.state().lease_until_unix_ms() + 1);
        let recovered = f
            .store
            .take_task_lease(&f.cas, lease.task_id(), "recovery", 60_000)
            .unwrap();
        // Taking the lease is insufficient: no business execution or resume has started.
        assert_eq!(f.state().lifecycle_episode(), first + 1);
        f.store.begin_task_execution(&f.cas, &recovered).unwrap();
        assert_eq!(f.state().lifecycle_episode(), first + 2);
        f.store.release_task_lease(&f.cas, &recovered).unwrap();
        let maintenance = f
            .store
            .take_task_lease(&f.cas, lease.task_id(), "maintenance", 60_000)
            .unwrap();
        f.store.release_task_lease(&f.cas, &maintenance).unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        assert_eq!(f.state().lifecycle_episode(), first + 2);
    });
}

#[test]
fn wake_plan_review_waiting_is_derived_from_the_checked_plan() {
    super::super::test_clock::with_time(1_000_000, |_| {
        let mut f = Fixture::new(true);
        let lease = f.open();
        f.propose(&lease);
        let before = f.state();
        assert_eq!(
            before.phase,
            TaskPhaseV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsPlanReview
            }
        );
        assert!(
            !f.store
                .replay(&task_run_id(lease.task_id()).unwrap())
                .unwrap()
                .iter()
                .any(|e| read_task_transition(e)
                    .is_ok_and(|t| matches!(t.change, TaskChangeV1::Waiting { .. })))
        );
        f.decide(&lease, PlanDecisionKindV1::Approved);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        assert!(matches!(f.state().phase, TaskPhaseV1::Running {}));
        assert_eq!(f.state().lifecycle_episode(), before.lifecycle_episode());
    });
}
