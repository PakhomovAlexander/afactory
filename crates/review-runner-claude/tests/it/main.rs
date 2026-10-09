//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

// The shared load-safe fixture wall (ADR-0114), declared once for every subject module.
#[path = "../../../review-runner/tests/it/support/load_safe_wall.rs"]
mod load_safe_wall;

mod fake_claude;
mod session;
mod task_auth_failure;
mod task_cancellation;
mod task_capture;
mod task_model_usage;
mod task_structured;
mod task_worker;
