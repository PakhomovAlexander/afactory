//! One renewable writer lease for execution and bounded preparation. The caller and heartbeat
//! serialize mutations through the same Store connection whenever the heartbeat can take it.
//! The heartbeat also holds a second connection to the same Store file (ADR-0128): it observes
//! the exact writer through it on every tick, and renews through it only once the caller's
//! connection has stayed held into the lease's last reserve, so no single Store operation can
//! outlast a live writer's lease. That connection waits a bounded time for the database's write
//! lock, and the reserve is long enough to wait out a lock a Store operation holds.
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
const RESERVE_MS: u64 = 4_000;
/// How long one renewal through the own connection waits for the database's write lock.
const OWN_BUSY_TIMEOUT: Duration = Duration::from_secs(1);
/// No renewal through the own connection starts with less lease left than this: one that waits
/// its whole busy timeout still has time to commit, so the heartbeat decides before expiry.
const RESERVE_FLOOR_MS: u64 = OWN_BUSY_TIMEOUT.as_millis() as u64 + 500;
/// How often a due renewal retries the shared connection before its reserve is reached.
const RETRY: Duration = Duration::from_millis(10);
/// The first and the longest pause after a renewal through the own connection fails.
const BACKOFF: Duration = Duration::from_millis(20);
const MAX_BACKOFF: Duration = Duration::from_millis(200);

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

pub(super) fn renew_if_due(
    store: &mut EventStore,
    cas: &Cas,
    lease: &TaskLease,
) -> Result<(), String> {
    let lease_until = store.task_lease_state(lease).map_err(|e| e.to_string())?;
    if lease_until < now_ms()?.saturating_add(RENEW_BELOW_MS) {
        store
            .renew_task_lease(cas, lease, LEASE_MS)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// One renewal through the heartbeat's own connection. When it fails, whether on the write lock
/// or on the sequence race with an append the work committed meanwhile, a fresh read decides:
/// expiry, release or a successor fails at once, and otherwise the fresh expiry is returned so
/// the caller can retry.
fn renew_through_own(
    own: &mut EventStore,
    cas: &Cas,
    lease: &TaskLease,
) -> Result<Result<(), (String, u64)>, String> {
    if std::env::var_os("AF_REPORT_LOAD").is_some() {
        eprintln!("REPORT_OWN_RENEW_ATTEMPT");
    }
    match own.renew_task_lease(cas, lease, LEASE_MS) {
        Ok(_) => {
            if std::env::var_os("AF_REPORT_LOAD").is_some() {
                eprintln!("REPORT_OWN_RENEW_SUCCESS");
            }
            Ok(Ok(()))
        }
        Err(error) => {
            let lease_until = own.task_lease_state(lease).map_err(|observed| {
                format!("renewal failed: {error}; lease observation failed: {observed}")
            })?;
            Ok(Err((error.to_string(), lease_until)))
        }
    }
}

/// Stable host-cancellation diagnostic shared by the CLI interrupt adapter.
pub const CANCELLED: &str = "Task execution was cancelled by its host";

/// Proof of a scoped heartbeat owner. It grants no Store or effect authority and cannot
/// outlive the closure that owns and joins the heartbeat.
pub struct HeartbeatScope<'a, 'store> {
    store: &'a Mutex<&'store mut EventStore>,
    lease: &'a TaskLease,
    cancellation: &'a AtomicBool,
}

impl HeartbeatScope<'_, '_> {
    pub fn check(&self) -> Result<(), String> {
        super::control::check(Some(self.cancellation))
    }

    pub(crate) fn covers(
        &self,
        store: &Mutex<&mut EventStore>,
        lease: &TaskLease,
        cancellation: Option<&AtomicBool>,
    ) -> bool {
        std::ptr::eq(
            std::ptr::from_ref(self.store).cast::<()>(),
            std::ptr::from_ref(store).cast::<()>(),
        ) && self.lease.task_id() == lease.task_id()
            && self.lease.epoch() == lease.epoch()
            && cancellation.is_some_and(|flag| std::ptr::eq(self.cancellation, flag))
    }
}

pub fn with_lifecycle<'a, 'store, T>(
    store: &'a Mutex<&'store mut EventStore>,
    cas: &Cas,
    lease: &'a TaskLease,
    cancellation: &'a AtomicBool,
    work: impl FnOnce(&HeartbeatScope<'a, 'store>) -> Result<T, String>,
) -> Result<T, String> {
    with_heartbeat_controlled(store, cas, lease, Some(cancellation), || {
        let owner = HeartbeatScope {
            store,
            lease,
            cancellation,
        };
        owner.check()?;
        // No effect follows work: preserve an already-committed result if a host
        // interrupt arrives afterwards. Heartbeat failures still propagate on join.
        work(&owner)
    })
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
    super::control::check(cancellation)?;
    let own = {
        let mut shared = store.lock().expect("Task Store");
        // Enter with a full lease while no work can hold the Store yet. A lease that cannot be
        // read or renewed here is lost authority (ADR-0089): request cancellation and never
        // start the work, however quickly it would have finished before the first tick.
        if let Err(error) = renew_if_due(&mut shared, cas, lease) {
            if let Some(flag) = cancellation {
                flag.store(true, Ordering::Release);
            }
            return Err(error);
        }
        // A Store that cannot be reopened, such as one with no database file, keeps the shared
        // connection as its only path, exactly as before ADR-0128.
        shared.reopen(OWN_BUSY_TIMEOUT).ok()
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
                let mut lease_until = lease_until;
                let mut backoff = BACKOFF;
                let mut last_error = None;
                while lease_until < now_ms()?.saturating_add(RENEW_BELOW_MS) {
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
                    // The caller may have renewed at report entry since this retry loop
                    // began. Observe the exact current writer again before acting on reserve
                    // or expiry; this read grants no renewal or publication authority.
                    lease_until = own.task_lease_state(lease).map_err(|e| e.to_string())?;
                    let now = now_ms()?;
                    if lease_until >= now.saturating_add(RENEW_BELOW_MS) {
                        break;
                    }
                    if lease_until <= now.saturating_add(RESERVE_FLOOR_MS) {
                        // Too little lease is left for another wait on the write lock to commit
                        // before expiry: fail while this writer still holds authority.
                        return Err(last_error.unwrap_or_else(|| {
                            "Task writer lease could not be renewed before expiry".into()
                        }));
                    }
                    if lease_until > now.saturating_add(RESERVE_MS) {
                        if stopped_within(&stopped, RETRY) {
                            failure.0 = None;
                            return Ok(());
                        }
                        continue;
                    }
                    // The work's connection is still held. The Store validates and fences this
                    // renewal exactly as the shared connection's, so lost authority still mints
                    // nothing.
                    match renew_through_own(&mut own, cas, lease)? {
                        Ok(()) => break,
                        Err((error, fresh)) => {
                            last_error = Some(error);
                            lease_until = fresh;
                        }
                    }
                    if stopped_within(&stopped, backoff) {
                        failure.0 = None;
                        return Ok(());
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
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
