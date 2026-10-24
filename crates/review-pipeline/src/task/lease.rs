//! One renewable writer lease for execution and bounded preparation. The caller and heartbeat
//! serialize mutations through the same Store connection whenever the heartbeat can take it.
//! The heartbeat also holds a second connection to the same Store file (ADR-0128): it observes
//! the exact writer through it on every tick, and renews through it only once the caller's
//! connection has stayed held into the lease's last reserve, so no single Store operation can
//! outlast a live writer's lease. That connection waits a bounded time for the database's write
//! lock, and the reserve is long enough to wait out a lock a Store operation holds. The
//! heartbeat runs on its own thread by its own tick, whatever the Worker or the work does; it
//! renews the 60 s lease once a third of it has passed, and a heartbeat that loses the lease
//! says so first, so the work records nothing more and a successor settles it (ADR-0209).
use review_store::{Cas, EventStore, store::task::TaskLease};
use std::{
    sync::{
        Condvar, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// The lease a Task writer takes and its heartbeat renews (ADR-0209): 60 s. Renewed once a
/// third of it has passed, it survives 40 s in which the writer's process gets no CPU at all,
/// which covers a laptop's wake from a short sleep, App Nap's timer coalescing and a loaded
/// machine's scheduling delays. A longer lease only delays the takeover after a real crash,
/// which `af task run` waits out and says so; a lost writer is recovered without being charged
/// an Attempt either way.
pub const TASK_LEASE_MS: u64 = 60_000;
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

/// The first words of every error a failed heartbeat returns: the writer can no longer hold the
/// Task's lease, so it records nothing more and its successor settles its work (ADR-0209).
pub const WRITER_LOST: &str = "Task writer lost its lease (writer_lost)";

/// The heartbeat's cadence for one lease length: renewal once a third of the lease has passed
/// (two thirds remain), the own-connection reserve and the tick. `for_lease(15_000)` is the
/// cadence ADR-0128 measured; production runs [`LeaseTiming::PRODUCTION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseTiming {
    lease_ms: u64,
    renew_below_ms: u64,
    reserve_ms: u64,
    tick: Duration,
}

impl LeaseTiming {
    pub const PRODUCTION: Self = Self::for_lease(TASK_LEASE_MS);

    /// The cadence of a lease of `lease_ms`: renewal with two thirds left, a reserve of at most
    /// 4 s and a tick of at most 1 s. A lease shorter than 6 s has no room for the reserve
    /// above its floor, so it is lengthened to 6 s.
    pub const fn for_lease(lease_ms: u64) -> Self {
        let lease_ms = if lease_ms < 6_000 { 6_000 } else { lease_ms };
        let renew_below_ms = lease_ms / 3 * 2;
        let reserve_ms = if RESERVE_MS < renew_below_ms / 2 {
            RESERVE_MS
        } else {
            renew_below_ms / 2
        };
        let tick_ms = if lease_ms / 15 < 1_000 {
            lease_ms / 15
        } else {
            1_000
        };
        Self {
            lease_ms,
            renew_below_ms,
            reserve_ms,
            tick: Duration::from_millis(tick_ms),
        }
    }

    pub const fn lease_ms(&self) -> u64 {
        self.lease_ms
    }

    /// How long the lease has left when the heartbeat renews it.
    pub const fn renew_below_ms(&self) -> u64 {
        self.renew_below_ms
    }
}

/// What a heartbeat signals besides its own result: the cancellation it requests when it
/// fails, the flag that says the writer lost its lease, and its cadence.
#[derive(Debug, Clone, Copy)]
pub struct HeartbeatControl<'a> {
    pub cancellation: Option<&'a AtomicBool>,
    pub writer_lost: Option<&'a AtomicBool>,
    pub timing: LeaseTiming,
}

impl HeartbeatControl<'_> {
    /// The writer lost its lease: flag it, then request cancellation, so the work sees the loss
    /// before it sees the cancellation.
    fn lost(&self) {
        if let Some(flag) = self.writer_lost {
            flag.store(true, Ordering::Release);
        }
        if let Some(flag) = self.cancellation {
            flag.store(true, Ordering::Release);
        }
    }
}

fn lost(error: impl std::fmt::Display) -> String {
    format!("{WRITER_LOST}: {error}")
}

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

/// The Store's own clock: a lease's expiry is a Store time, so it is judged against the clock
/// the Store recorded it with.
fn now_ms() -> Result<u64, String> {
    review_store::store::task::now().map_err(|e| e.to_string())
}

fn renew_if_due(
    store: &mut EventStore,
    cas: &Cas,
    lease: &TaskLease,
    timing: &LeaseTiming,
) -> Result<(), String> {
    let lease_until = store.task_lease_state(lease).map_err(|e| e.to_string())?;
    if lease_until < now_ms()?.saturating_add(timing.renew_below_ms) {
        store
            .renew_task_lease(cas, lease, timing.lease_ms)
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
    timing: &LeaseTiming,
) -> Result<Result<(), (String, u64)>, String> {
    match own.renew_task_lease(cas, lease, timing.lease_ms) {
        Ok(_) => Ok(Ok(())),
        Err(error) => {
            let lease_until = own.task_lease_state(lease).map_err(|e| e.to_string())?;
            Ok(Err((error.to_string(), lease_until)))
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
    with_heartbeat_supervised(
        store,
        cas,
        lease,
        HeartbeatControl {
            cancellation,
            writer_lost: None,
            timing: LeaseTiming::PRODUCTION,
        },
        work,
    )
}

/// Run `work` under a heartbeat that renews `lease` on its own thread, by its own clock and
/// cadence, whatever the work does (ADR-0128, ADR-0209). A heartbeat that cannot keep the lease
/// flags `writer_lost`, requests cancellation, and returns an error that starts with
/// [`WRITER_LOST`] and states what the Store observed: its time and the lease's expiry, or the
/// writer that holds the lease now.
pub fn with_heartbeat_supervised<T>(
    store: &Mutex<&mut EventStore>,
    cas: &Cas,
    lease: &TaskLease,
    control: HeartbeatControl<'_>,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let timing = control.timing;
    let own = {
        let mut shared = store.lock().expect("Task Store");
        // Enter with a full lease while no work can hold the Store yet. A lease that cannot be
        // read or renewed here is lost authority (ADR-0089): request cancellation and never
        // start the work, however quickly it would have finished before the first tick.
        if let Err(error) = renew_if_due(&mut shared, cas, lease, &timing) {
            control.lost();
            return Err(lost(error));
        }
        // A Store that cannot be reopened, such as one with no database file, keeps the shared
        // connection as its only path, exactly as before ADR-0128.
        shared.reopen(OWN_BUSY_TIMEOUT).ok()
    };
    let stopped = (Mutex::new(false), Condvar::new());
    std::thread::scope(|scope| {
        let heartbeat = scope.spawn(|| -> Result<(), String> {
            // Unwinding is also a failed heartbeat. Flag the loss and request interruption
            // before joining work.
            struct CancelOnFailure<'a>(Option<HeartbeatControl<'a>>);
            impl Drop for CancelOnFailure<'_> {
                fn drop(&mut self) {
                    if let Some(control) = self.0 {
                        control.lost();
                    }
                }
            }
            let mut failure = CancelOnFailure(Some(control));
            let beat = || -> Result<(), String> {
                let Some(mut own) = own else {
                    loop {
                        if stopped_within(&stopped, timing.tick) {
                            return Ok(());
                        }
                        renew_if_due(&mut store.lock().expect("Task Store"), cas, lease, &timing)?;
                    }
                };
                beat_through_own(store, &mut own, cas, lease, &timing, &stopped)
            };
            let result = beat().map_err(lost);
            if result.is_ok() {
                failure.0 = None;
            }
            result
        });
        let stop = StopHeartbeat(&stopped);
        let result = work();
        drop(stop);
        heartbeat
            .join()
            .map_err(|_| lost("Task heartbeat panicked"))??;
        result
    })
}

/// The heartbeat loop with its own connection: observe every tick through it, renew through
/// the shared connection when due, and through the own one only in the lease's last reserve.
/// Returns once the owner stops the heartbeat; an error is lost authority.
fn beat_through_own(
    store: &Mutex<&mut EventStore>,
    own: &mut EventStore,
    cas: &Cas,
    lease: &TaskLease,
    timing: &LeaseTiming,
    stopped: &(Mutex<bool>, Condvar),
) -> Result<(), String> {
    loop {
        if stopped_within(stopped, timing.tick) {
            return Ok(());
        }
        // Observe the exact writer without waiting for the work's connection, so a
        // successor or an expiry is seen even while a Store operation is running.
        let lease_until = own.task_lease_state(lease).map_err(|e| e.to_string())?;
        if lease_until >= now_ms()?.saturating_add(timing.renew_below_ms) {
            continue;
        }
        let mut lease_until = lease_until;
        let mut backoff = BACKOFF;
        let mut last_error = None;
        while lease_until < now_ms()?.saturating_add(timing.renew_below_ms) {
            match store.try_lock() {
                Ok(mut shared) => {
                    renew_if_due(&mut shared, cas, lease, timing)?;
                    break;
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err("Task Store lock is poisoned".into());
                }
                Err(TryLockError::WouldBlock) => {}
            }
            let now = now_ms()?;
            if lease_until <= now.saturating_add(RESERVE_FLOOR_MS) {
                // Too little lease is left for another wait on the write lock to commit
                // before expiry: fail while this writer still holds authority.
                return Err(last_error.unwrap_or_else(|| {
                    "Task writer lease could not be renewed before expiry".into()
                }));
            }
            if lease_until > now.saturating_add(timing.reserve_ms) {
                if stopped_within(stopped, RETRY) {
                    return Ok(());
                }
                continue;
            }
            // The work's connection is still held. The Store validates and fences this
            // renewal exactly as the shared connection's, so lost authority still mints
            // nothing.
            match renew_through_own(own, cas, lease, timing)? {
                Ok(()) => break,
                Err((error, fresh)) => {
                    last_error = Some(error);
                    lease_until = fresh;
                }
            }
            if stopped_within(stopped, backoff) {
                return Ok(());
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }
}
