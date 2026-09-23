//! Check nodes: running a project's checks and recording each execution as its own artifact.
//!
//! Two properties this crate exists for:
//!
//! - **Nothing overwrites.** A gate that runs five times in one round leaves five records: every
//!   execution is an immutable `CheckResult@1` appended to the log, and earlier ones stay
//!   readable forever.
//! - **A check that could not run is not a pass.** `not_run` is a first-class status carrying a
//!   reason, and it fails a required gate exactly as a failure does. So does a gate with no
//!   required checks at all — a vacuous run is the most dangerous green there is.

pub mod gate;
pub mod runner;

// The exec vocabulary is kernel-wide, so it lives in review-core; re-exported here
// because a check command is still the canonical use.
pub use gate::{GateDecision, GateOutcome};
pub use review_core::exec::{Arg, Command};
pub use runner::{
    CheckDefinition, CheckExecution, CheckResult, CheckRunner, CheckStatus, check_event,
};
