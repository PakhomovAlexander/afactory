//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

mod captured_registry;
mod captured_review;
mod definition;
mod lock_af_version;
mod lockfile;
mod node_budget;
mod optimization_package;
mod task_selection_schema;
