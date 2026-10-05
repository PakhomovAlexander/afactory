//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

// Shared fixtures, declared once for every subject module.
mod support;

mod code_policy_schema;
mod optimization_producers;
mod receipt_authority;
mod remote_checks;
mod task_campaign_review;
mod task_implementation;
mod task_review;
mod task_review_execute_checks;
mod task_runtime;
