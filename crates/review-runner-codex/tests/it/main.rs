//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

mod fake_codex;
mod task_cancellation;
mod task_capture;
mod task_final_message;
mod task_worker;
