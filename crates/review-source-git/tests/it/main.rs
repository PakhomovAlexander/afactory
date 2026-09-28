//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

// Shared fixtures, declared once for every subject module.
mod common;

mod capture;
mod git_deadline;
mod hostile_git_config;
mod task_identity;
mod tree_diff;
