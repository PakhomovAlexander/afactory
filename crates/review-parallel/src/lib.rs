//! One bounded executor shared by filesystem-heavy Review Kernel infrastructure.
//!
//! The CLI initializes the executor once from the host's available parallelism. Library-only
//! embedders may do the same before first use; otherwise the first operation adopts that default.
//! Concurrent and nested phases submit work to the same worker threads, so scheduler concurrency
//! does not multiply OS threads and no transferable RAII permit can corrupt capacity accounting.

use std::sync::OnceLock;

use rayon::prelude::*;

static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

fn default_limit() -> usize {
    std::thread::available_parallelism()
        .map(|workers| workers.get())
        .unwrap_or(1)
}

fn configured_pool(limit: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(limit.max(1))
        .stack_size(2 * 1024 * 1024)
        .thread_name(|index| format!("review-worker-{index}"))
        .build()
        .expect("a positive Review Kernel worker limit builds")
}

fn pool() -> &'static rayon::ThreadPool {
    POOL.get_or_init(|| configured_pool(default_limit()))
}

/// Returned when an embedder tries to configure the executor after its first use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyInitialized;

impl std::fmt::Display for AlreadyInitialized {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("worker executor is already initialized")
    }
}

impl std::error::Error for AlreadyInitialized {}

/// Set the process-wide worker capacity before first use.
pub fn init_worker_limit(limit: usize) -> Result<(), AlreadyInitialized> {
    POOL.set(configured_pool(limit))
        .map_err(|_| AlreadyInitialized)
}

/// Maximum active worker tasks across all participating infrastructure phases.
pub fn worker_limit() -> usize {
    pool().current_num_threads()
}

/// Apply a fallible operation to borrowed items on the shared executor.
pub fn try_for_each<T, E, F>(items: &[T], operation: F) -> Result<(), E>
where
    T: Sync,
    E: Send,
    F: Fn(&T) -> Result<(), E> + Send + Sync,
{
    pool().install(|| items.par_iter().try_for_each(operation))
}

/// Apply a fallible operation to owned items on the shared executor.
pub fn try_for_each_owned<T, E, F>(items: Vec<T>, operation: F) -> Result<(), E>
where
    T: Send,
    E: Send,
    F: Fn(T) -> Result<(), E> + Send + Sync,
{
    pool().install(|| items.into_par_iter().try_for_each(operation))
}

/// Transform owned items on the shared executor, retaining indexed input order.
pub fn try_map_owned<T, R, E, F>(items: Vec<T>, operation: F) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Send + Sync,
{
    pool().install(|| items.into_par_iter().map(operation).collect())
}

/// Transform owned items with worker-local reusable state on the shared executor.
pub fn try_map_owned_with<T, S, R, E, I, F>(
    items: Vec<T>,
    initialize: I,
    operation: F,
) -> Result<Vec<R>, E>
where
    T: Send,
    S: Send,
    R: Send,
    E: Send,
    I: Fn() -> S + Send + Sync,
    F: Fn(&mut S, T) -> Result<R, E> + Send + Sync,
{
    pool().install(|| {
        items
            .into_par_iter()
            .map_init(initialize, operation)
            .collect()
    })
}

/// Run two fallible infrastructure phases concurrently on the shared executor.
///
/// This is the bounded pipeline primitive for work such as walking the next directory level
/// while hashing the candidates discovered at the previous one. Nested parallel operations in
/// either arm still reuse this same pool.
pub fn try_join<A, B, E, FA, FB>(left: FA, right: FB) -> Result<(A, B), E>
where
    A: Send,
    B: Send,
    E: Send,
    FA: FnOnce() -> Result<A, E> + Send,
    FB: FnOnce() -> Result<B, E> + Send,
{
    pool().install(|| {
        let (left, right) = rayon::join(left, right);
        Ok((left?, right?))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    #[test]
    fn nested_work_reuses_the_same_executor() {
        let outer = vec![1, 2, 3];
        super::try_for_each(&outer, |_| {
            super::try_for_each(&[4, 5], |_| Ok::<_, ()>(()))
        })
        .unwrap();
        assert_eq!(super::worker_limit(), super::pool().current_num_threads());
    }

    #[test]
    fn concurrent_phases_use_only_the_shared_worker_threads() {
        let threads = Mutex::new(HashSet::new());
        let items = vec![(); super::worker_limit() * 8];
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    super::try_for_each(&items, |_| {
                        threads.lock().unwrap().insert(std::thread::current().id());
                        std::thread::yield_now();
                        Ok::<_, ()>(())
                    })
                    .unwrap();
                });
            }
        });
        assert!(threads.into_inner().unwrap().len() <= super::worker_limit());
    }

    #[test]
    fn joined_phases_reuse_the_shared_executor() {
        let (left, right) = super::try_join(
            || super::try_map_owned(vec![1, 2], |value| Ok::<_, ()>(value + 1)),
            || super::try_map_owned(vec![3, 4], |value| Ok::<_, ()>(value + 1)),
        )
        .unwrap();
        assert_eq!(left, vec![2, 3]);
        assert_eq!(right, vec![4, 5]);
    }
}
