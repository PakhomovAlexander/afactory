//! Worker runners: supervised process transport, captured Task input composition and the
//! Task Worker adapter contract.
//!
//! A `command` Worker is deterministic — its output is a function of its input — so every
//! property of the Worker contract can be proved before a model is ever invoked. A model
//! adapter is then a *different runner behind the same contract*, and nothing above it has to
//! change.
//!
//! [`models`] owns Provider-neutral model/effort validation and binding diagnostics; native
//! Provider outcome interpretation stays in each adapter.

pub mod activity;
pub mod cli;
pub mod model;
pub mod models;
pub mod native_failure;
pub mod stream;
pub mod task;

pub use af_process::run_supervised;
pub use model::{ContextEntry, ContextManifest, ModelRunner, RunnerError, estimate_tokens};
