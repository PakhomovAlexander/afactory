use super::*;
type Hook = Box<dyn FnOnce()>;
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
    static CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
pub(super) fn during_context() {
    CALLS.with(|calls| calls.set(calls.get() + 1));
    if let Some(hook) = HOOK.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

#[test]
fn renewal_during_context_callback_never_redispatches_it_and_takeover_is_closed() {
    for takeover in [false, true] {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let context = f.cas.put(b"exact context").unwrap();
        let reserved = f
            .store
            .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
            .unwrap();
        let mut other = f.store.reopen(std::time::Duration::from_secs(1)).unwrap();
        let cas = Cas::open_existing(f._dir.path().join("cas")).unwrap();
        let old = lease.clone();
        let reserved_for_release = reserved.clone();
        HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                if takeover {
                    other
                        .release_reserved_task_attempt(
                            &cas,
                            &old,
                            &reserved_for_release,
                            "callback takeover fixture",
                        )
                        .unwrap();
                    other.release_task_lease(&cas, &old).unwrap();
                    other
                        .take_task_lease(&cas, old.task_id(), "successor", 15_000)
                        .unwrap();
                } else {
                    other.renew_task_lease(&cas, &old, 2_000_000).unwrap();
                }
            }))
        });
        CALLS.with(|calls| calls.set(0));
        let result =
            f.store
                .bind_task_attempt_context(&f.cas, &lease, &reserved, &context, &f.authority);
        assert_eq!(
            CALLS.with(|calls| calls.get()),
            1,
            "callback is not redispatched"
        );
        assert_eq!(
            result.is_err(),
            takeover,
            "same-owner renewal continues; successor refuses: {result:?}"
        );
        assert_eq!(
            f.state().execution.unwrap().budget.begun_attempts(),
            0,
            "binding never dispatches paid work"
        );
    }
}
