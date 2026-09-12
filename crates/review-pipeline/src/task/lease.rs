//! One renewable writer lease for execution and bounded preparation. The caller and heartbeat
//! serialize mutations through the same Store connection; no second event writer is created.
use review_store::{Cas, EventStore, store::task::TaskLease};
use std::{
    sync::{Condvar, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct StopHeartbeat<'a>(&'a (Mutex<bool>, Condvar));
impl Drop for StopHeartbeat<'_> {
    fn drop(&mut self) {
        *self.0.0.lock().expect("Task heartbeat") = true;
        self.0.1.notify_all();
    }
}

pub fn with_heartbeat<T>(
    store: &Mutex<&mut EventStore>,
    cas: &Cas,
    lease: &TaskLease,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let stopped = (Mutex::new(false), Condvar::new());
    std::thread::scope(|scope| {
        let heartbeat = scope.spawn(|| -> Result<(), String> {
            loop {
                let (done, _) = stopped
                    .1
                    .wait_timeout_while(
                        stopped.0.lock().expect("Task heartbeat"),
                        Duration::from_secs(1),
                        |done| !*done,
                    )
                    .expect("Task heartbeat");
                if *done {
                    return Ok(());
                }
                drop(done);
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_millis() as u64;
                let mut store = store.lock().expect("Task Store");
                let projection = store
                    .task_projection(cas, lease.task_id())
                    .map_err(|e| e.to_string())?
                    .ok_or("Unknown Task")?;
                if projection.lease_until_unix_ms() < now.saturating_add(10_000) {
                    store
                        .renew_task_lease(cas, lease, 15_000)
                        .map_err(|e| e.to_string())?;
                }
            }
        });
        let stop = StopHeartbeat(&stopped);
        let result = work();
        drop(stop);
        heartbeat
            .join()
            .map_err(|_| "Task heartbeat panicked".to_string())??;
        result
    })
}
