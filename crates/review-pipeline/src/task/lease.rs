//! One renewable writer lease for execution and bounded preparation. The caller and heartbeat
//! serialize mutations through the same Store connection whenever the heartbeat can take it.
//! The heartbeat also holds a second connection to the same Store file (ADR-0125): it observes
//! the exact writer through it on every tick, and renews through it only once the caller's
//! connection has stayed held into the lease's last reserve, so no single Store operation can
//! outlast a live writer's lease.
use review_store::{Cas, EventStore, store::task::TaskLease};
use std::{
    sync::{
        Condvar, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The ADR-0089 heartbeat tick, renewal threshold and lease duration, unchanged.
const TICK: Duration = Duration::from_secs(1);
const RENEW_BELOW_MS: u64 = 10_000;
const LEASE_MS: u64 = 15_000;
/// A due renewal that still cannot take the shared connection with this much lease left renews
/// through the heartbeat's own connection instead. Until then it keeps waiting, because a
/// renewal through the own connection can overtake an append the work is about to make.
const RESERVE_MS: u64 = 2_000;
/// How often a due renewal retries the shared connection before its reserve is reached.
const RETRY: Duration = Duration::from_millis(10);
/// Reserve renewals that may lose the sequence race to the work's own append before failing.
const RESERVE_ATTEMPTS: usize = 3;

struct StopHeartbeat<'a>(&'a (Mutex<bool>, Condvar));
impl Drop for StopHeartbeat<'_> {
    fn drop(&mut self) {
        *self.0.0.lock().expect("Task heartbeat") = true;
        self.0.1.notify_all();
    }
}

/// Wait up to `timeout` for the owner to stop the heartbeat; true once it has.
fn stopped_within(stopped: &(Mutex<bool>, Condvar), timeout: Duration) -> bool {
    let (done, _) = stopped
        .1
        .wait_timeout_while(stopped.0.lock().expect("Task heartbeat"), timeout, |done| {
            !*done
        })
        .expect("Task heartbeat");
    *done
}

fn now_ms() -> Result<u64, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64)
}

fn renew_if_due(store: &mut EventStore, cas: &Cas, lease: &TaskLease) -> Result<(), String> {
    let lease_until = store.task_lease_state(lease).map_err(|e| e.to_string())?;
    if lease_until < now_ms()?.saturating_add(RENEW_BELOW_MS) {
        store
            .renew_task_lease(cas, lease, LEASE_MS)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Renew through the heartbeat's own connection. An attempt that loses the sequence race to an
/// append the work committed meanwhile is retried only while a fresh read still shows this
/// exact writer live; expiry, release or a successor fails at once.
fn renew_in_reserve(own: &mut EventStore, cas: &Cas, lease: &TaskLease) -> Result<(), String> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match own.renew_task_lease(cas, lease, LEASE_MS) {
            Ok(_) => return Ok(()),
            Err(error) => {
                own.task_lease_state(lease).map_err(|e| e.to_string())?;
                if attempts == RESERVE_ATTEMPTS {
                    return Err(error.to_string());
                }
            }
        }
    }
}

pub fn with_heartbeat<T>(
    store: &Mutex<&mut EventStore>,
    cas: &Cas,
    lease: &TaskLease,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_heartbeat_controlled(store, cas, lease, None, work)
}

pub fn with_heartbeat_controlled<T>(
    store: &Mutex<&mut EventStore>,
    cas: &Cas,
    lease: &TaskLease,
    cancellation: Option<&AtomicBool>,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let own = {
        let mut shared = store.lock().expect("Task Store");
        // Enter with a full lease while no work can hold the Store yet. A lease that cannot be
        // read or renewed here fails on the first tick, as before, and requests cancellation.
        let _ = renew_if_due(&mut shared, cas, lease);
        // A Store that cannot be reopened, such as one with no database file, keeps the shared
        // connection as its only path, exactly as before ADR-0125.
        shared.reopen().ok()
    };
    let stopped = (Mutex::new(false), Condvar::new());
    std::thread::scope(|scope| {
        let heartbeat = scope.spawn(|| -> Result<(), String> {
            // Unwinding is also a failed heartbeat. Request interruption before joining work.
            struct CancelOnFailure<'a>(Option<&'a AtomicBool>);
            impl Drop for CancelOnFailure<'_> {
                fn drop(&mut self) {
                    if let Some(flag) = self.0 {
                        flag.store(true, Ordering::Release);
                    }
                }
            }
            let mut failure = CancelOnFailure(cancellation);
            let Some(mut own) = own else {
                loop {
                    if stopped_within(&stopped, TICK) {
                        failure.0 = None;
                        return Ok(());
                    }
                    renew_if_due(&mut store.lock().expect("Task Store"), cas, lease)?;
                }
            };
            loop {
                if stopped_within(&stopped, TICK) {
                    failure.0 = None;
                    return Ok(());
                }
                // Observe the exact writer without waiting for the work's connection, so a
                // successor or an expiry is seen even while a Store operation is running.
                let lease_until = own.task_lease_state(lease).map_err(|e| e.to_string())?;
                if lease_until >= now_ms()?.saturating_add(RENEW_BELOW_MS) {
                    continue;
                }
                loop {
                    match store.try_lock() {
                        Ok(mut shared) => {
                            renew_if_due(&mut shared, cas, lease)?;
                            break;
                        }
                        Err(TryLockError::Poisoned(_)) => {
                            return Err("Task Store lock is poisoned".into());
                        }
                        Err(TryLockError::WouldBlock) => {}
                    }
                    if lease_until <= now_ms()?.saturating_add(RESERVE_MS) {
                        // The work's connection is still held. The Store validates and fences
                        // this renewal exactly as the shared connection's, so lost authority
                        // still mints nothing.
                        renew_in_reserve(&mut own, cas, lease)?;
                        break;
                    }
                    if stopped_within(&stopped, RETRY) {
                        failure.0 = None;
                        return Ok(());
                    }
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
