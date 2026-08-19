//! Check nodes: running a project's checks and recording each execution as its own artifact.
//!
//! Two properties the shell harness could not offer:
//!
//! - **Nothing overwrites.** `checks.sh` truncated `checks.tsv` at the start of every run, so a
//!   gate that ran five times in one round left evidence of one. Here every execution is an
//!   immutable `CheckResult@1` appended to the log; earlier ones stay readable forever.
//! - **A check that could not run is not a pass.** `not_run` is a first-class status carrying a
//!   reason, and it fails a required gate exactly as a failure does. So does a gate with no
//!   required checks at all — a vacuous run is the most dangerous green there is.

pub mod gate;
pub mod runner;

// The exec vocabulary is kernel-wide, so it lives in review-core; re-exported here
// because a check command is still the canonical use.
pub use gate::{GateDecision, GateOutcome};
pub use review_core::exec::{Arg, ArgError, Command, Provenance};
pub use runner::{CheckDefinition, CheckResult, CheckRunner, CheckStatus, check_event};
