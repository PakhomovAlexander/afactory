//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

// Shared fixtures, declared once for every subject module.
mod support;

mod attempt_wall;
mod campaign_authority;
mod canonical_identity;
mod crash_replay;
mod ledger_convergence;
mod optimization_economics;
mod report_scope;
mod task_contract_identity;
