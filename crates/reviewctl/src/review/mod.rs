//! The `af review` subcommands, one module per surface. Argument parsing, state resolution, and
//! the shared option types stay in `main.rs`.

pub(crate) mod campaigns;
pub(crate) mod evidence;
pub(crate) mod ledger;
pub(crate) mod report;
pub(crate) mod run;
