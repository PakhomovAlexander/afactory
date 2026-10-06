//! One heartbeat spans CLI preparation, runtime construction and result publication.
use super::*;
use review_pipeline::task::lease::{HeartbeatScope, with_lifecycle};
use review_store::SharedEventStore;
use std::sync::atomic::AtomicBool;

pub(super) fn run<'store, T>(
    cas: &'store Cas,
    store: &'store mut EventStore,
    lease: &TaskLease,
    work: impl FnOnce(
        &SharedEventStore<'store>,
        &HeartbeatScope<'_, 'store>,
        &AtomicBool,
    ) -> Result<T, String>,
) -> Result<T, String> {
    let shared = SharedEventStore::new(store);
    let cancellation = AtomicBool::new(false);
    crate::interrupt::note_task(lease.task_id());
    crate::interrupt::forwarding(&cancellation, || {
        crate::interrupt::check()?;
        with_lifecycle(&shared, cas, lease, &cancellation, |owner| {
            crate::interrupt::check()?;
            #[cfg(test)]
            let _marker = TestOwner::new();
            work(&shared, owner, &cancellation)
        })
    })
    .map_err(|error| {
        if error == review_pipeline::task::lease::CANCELLED
            && crate::interrupt::received().is_some()
        {
            crate::interrupt::INTERRUPTED.into()
        } else {
            error
        }
    })
}

#[cfg(test)]
struct TestOwner;
#[cfg(test)]
impl TestOwner {
    fn new() -> Self {
        lease_lifecycle_tests::LIFECYCLE_ACTIVE.with(|value| assert!(!value.replace(true)));
        Self
    }
}
#[cfg(test)]
impl Drop for TestOwner {
    fn drop(&mut self) {
        lease_lifecycle_tests::LIFECYCLE_ACTIVE.with(|value| value.set(false));
    }
}
