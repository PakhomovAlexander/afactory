//! Shared worker permits for filesystem-heavy infrastructure.
//!
//! The limit may be initialized once by an embedder before any work starts. Otherwise the first
//! use adopts `available_parallelism`. Permits are re-entrant per thread: nested infrastructure
//! work does not deadlock a one-core process or consume a second slot.

use std::cell::Cell;
use std::sync::{Condvar, Mutex, OnceLock};

struct WorkerPool {
    available: Mutex<usize>,
    changed: Condvar,
    limit: usize,
}

static POOL: OnceLock<WorkerPool> = OnceLock::new();

thread_local! {
    static HELD_PERMITS: Cell<usize> = const { Cell::new(0) };
}

fn default_limit() -> usize {
    std::thread::available_parallelism()
        .map(|workers| workers.get())
        .unwrap_or(1)
}

fn configured_pool(limit: usize) -> WorkerPool {
    let limit = limit.max(1);
    WorkerPool {
        available: Mutex::new(limit),
        changed: Condvar::new(),
        limit,
    }
}

fn pool() -> &'static WorkerPool {
    POOL.get_or_init(|| configured_pool(default_limit()))
}

/// Returned when an embedder tries to configure the pool after its first use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyInitialized;

impl std::fmt::Display for AlreadyInitialized {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("worker limit is already initialized")
    }
}

impl std::error::Error for AlreadyInitialized {}

/// Set the process-wide worker capacity before first use.
pub fn init_worker_limit(limit: usize) -> Result<(), AlreadyInitialized> {
    POOL.set(configured_pool(limit))
        .map_err(|_| AlreadyInitialized)
}

/// Maximum active worker items across participating infrastructure phases.
pub fn worker_limit() -> usize {
    pool().limit
}

/// A permit for one active work item.
pub struct WorkerPermit {
    pool: &'static WorkerPool,
}

/// Wait for one process-wide worker slot.
///
/// Re-entrant acquisition on a thread that already holds a permit is a no-op against global
/// capacity. The slot is returned when the last permit held by that thread is dropped.
pub fn acquire_worker_permit() -> WorkerPermit {
    let pool = pool();
    let already_held = HELD_PERMITS.with(|held| {
        let count = held.get();
        held.set(count + 1);
        count > 0
    });
    if !already_held {
        let mut available = pool.available.lock().expect("worker permit pool");
        while *available == 0 {
            available = pool.changed.wait(available).expect("worker permit pool");
        }
        *available -= 1;
    }
    WorkerPermit { pool }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        let release = HELD_PERMITS.with(|held| {
            let count = held.get();
            debug_assert!(count > 0, "dropping an unheld worker permit");
            held.set(count.saturating_sub(1));
            count == 1
        });
        if release {
            let mut available = self.pool.available.lock().expect("worker permit pool");
            *available += 1;
            self.pool.changed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nested_acquisition_on_one_thread_is_reentrant() {
        let outer = super::acquire_worker_permit();
        let inner = super::acquire_worker_permit();
        drop(outer);
        drop(inner);
        let _again = super::acquire_worker_permit();
    }
}
