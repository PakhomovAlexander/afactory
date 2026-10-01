//! One integration-test binary for this crate. Every subject file is a module here, so
//! the crate links its test dependencies once instead of once per file (ADR-0124).
//! Add a new subject as `tests/it/<subject>.rs` plus a `mod` line below.

// Shared fixtures, declared once for every subject module.
mod common;
#[path = "support/review_memo.rs"]
mod review_memo;
#[path = "support/schemas.rs"]
mod schemas;
#[path = "support/task_cli.rs"]
mod task_cli;

mod af_layout;
mod build_metadata;
mod campaign_loop;
mod cli_surface;
mod common_review_summaries;
mod lock_af_version;
mod onboard;
mod provider_onboarding;
mod provider_registry;
mod render;
mod routing;
mod self_managed;
mod self_optimizer;
mod task_acceptance;
mod task_catalog;
mod task_cli_fixture;
mod task_delivery;
mod task_developer;
mod task_document;
mod task_file;
mod task_heavy;
mod task_input_bindings;
mod task_interrupt;
mod task_issues;
mod task_planning;
mod task_preview;
mod task_public_schemas;
mod task_refresh;
mod task_repair;
mod task_selection;
mod task_starters;
mod tui;
mod undeclared_af_paths;
