//! One process-wide CPU permit pool for bounded filesystem work.
//!
//! Materialization, sandbox cloning, sealing, and CAS durability may overlap. Sizing each phase
//! independently from `available_parallelism` oversubscribes the host, while statically dividing
//! every phase by the scheduler width underutilizes a lone phase. A shared permit lets a lone
//! phase use the machine and makes concurrent phases share the same fixed capacity.

use std::sync::{Condvar, Mutex, OnceLock};

struct WorkerPool {
    available: Mutex<usize>,
    changed: Condvar,
    limit: usize,
}

fn pool() -> &'static WorkerPool {
    static POOL: OnceLock<WorkerPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let limit = std::thread::available_parallelism()
            .map(|workers| workers.get())
            .unwrap_or(1);
        WorkerPool {
            available: Mutex::new(limit),
            changed: Condvar::new(),
            limit,
        }
    })
}

/// Maximum number of CPU/file workers that may be active across participating phases.
pub fn worker_limit() -> usize {
    pool().limit
}

/// A permit for one active filesystem worker. Dropping it returns capacity to the process.
pub struct WorkerPermit {
    pool: &'static WorkerPool,
}

/// Wait for one process-wide worker slot.
pub fn acquire_worker_permit() -> WorkerPermit {
    let pool = pool();
    let mut available = pool.available.lock().expect("worker permit pool");
    while *available == 0 {
        available = pool.changed.wait(available).expect("worker permit pool");
    }
    *available -= 1;
    WorkerPermit { pool }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        let mut available = self.pool.available.lock().expect("worker permit pool");
        *available += 1;
        self.pool.changed.notify_one();
    }
}
