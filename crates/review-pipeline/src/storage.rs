//! The Storage Budget as the pipeline meets it (ADR-0144): the free-disk floor a check or a
//! Worker Attempt must clear before it starts, and the budget step a warm check runs before it
//! creates a new toolchain key.
//!
//! The policy, the sweep and every entry it may evict belong to the `af` binary, which installs
//! one [`StorageHost`] per process. A process that installs none — a library caller, a test of
//! this crate — has no floor and no budget step, exactly as before the budget existed.

use std::sync::{Arc, OnceLock};

/// The typed reason a check or a Worker Attempt refused below the free-disk floor carries, as
/// the prefix of its refusal.
pub const INSUFFICIENT_DISK: &str = "insufficient_disk";

/// The machine's Storage Budget, as the `af` binary implements it.
pub trait StorageHost: Send + Sync {
    /// `Ok` when the volumes af works on hold at least the free-disk floor, after one sweep
    /// when they did not; otherwise the refusal, starting with [`INSUFFICIENT_DISK`] and naming
    /// the free bytes, the floor, `af storage` and the knob.
    fn ensure_free_disk(&self) -> Result<(), String>;

    /// The budget step of the sweep, run before a warm check creates a toolchain key that does
    /// not exist yet. Best effort: a failure is the host's to report, never the check's.
    fn before_new_warm_key(&self);
}

static HOST: OnceLock<Arc<dyn StorageHost>> = OnceLock::new();

/// Install the process's Storage Budget. The first installation wins; `false` when one was
/// already installed.
pub fn install(host: Arc<dyn StorageHost>) -> bool {
    HOST.set(host).is_ok()
}

/// The installed floor, or `Ok` when no budget is installed.
pub fn ensure_free_disk() -> Result<(), String> {
    HOST.get().map_or(Ok(()), |host| host.ensure_free_disk())
}

/// Run the installed budget step, if any, before a new warm key.
pub(crate) fn before_new_warm_key() {
    if let Some(host) = HOST.get() {
        host.before_new_warm_key();
    }
}
