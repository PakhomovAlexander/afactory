//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

// Shared fixtures, declared once for every subject module.
#[path = "support/render.rs"]
mod render_support;

mod change_wide_prior;
mod command_inputs;
mod model_supervision;
mod prior_bound;
mod session_render;
mod task_command_process_group;
mod task_contexts;
mod task_model_transport;
mod task_usage;
mod warm_render;
