//! Durable Scenario orchestration and Ant admission, independent of the CLI.
//!
//! Machine locations and native Task operations are supplied explicitly through [`Host`].
//! The domain retains pinned authority checks and bounded external adapter dispatch; a host
//! supplies no additional publication authority.
pub mod adapter;
pub mod checks;
pub mod contract;
pub mod host;
pub mod performance;
pub mod presentation;
pub mod retention;
pub mod roadmap;
pub mod runtime;
pub mod schedule;
pub mod source;
pub mod state;
pub mod store;
pub mod template;
pub mod workspace;
pub use host::Host;

#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod interval_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
use {
    contract::*, schedule::*, state::*, std::collections::BTreeMap, std::path::PathBuf,
    store::Store,
};
