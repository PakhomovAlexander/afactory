//! Survey collection's CLI presentation. Inventory, judgement and deletion live in af-project.

use super::super::Log;
use af_contracts::project::SurveyCollectedPayloadV1;
pub(crate) use af_project::survey::collect::*;
use af_store::EventStore;
use std::collections::BTreeSet;
use std::path::Path;
const RUN_ID: &str = "project";

/// The log and the tombstones of the invoking checkout's Project Store, read-only; `None` when
/// the Project Store holds no log yet.
fn read_here() -> Result<Option<(Log, Vec<SurveyCollectedPayloadV1>)>, String> {
    let checkout = super::super::require_checkout(Path::new("."))?;
    let store = super::super::store_directory(&crate::xdg_state_root()?, &checkout.project_id);
    if std::fs::symlink_metadata(store.join("events.sqlite")).is_err() {
        return Ok(None);
    }
    let events = EventStore::open_read_only(store.join("events.sqlite"))
        .map_err(|error| format!("the Project Store {}: {error}", store.display()))?;
    let log = Log::fold(
        &events
            .replay(RUN_ID)
            .map_err(|error| format!("the Project Store {}: {error}", store.display()))?,
    )?;
    Ok(Some((log, tombstones(&events)?)))
}

fn time(unix_ms: u64) -> String {
    af_contracts::task::collection::collected_time(unix_ms)
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// `af project`, with the two answers collection changes: `af project survey` says that a
/// collected newest Survey was collected, and `af project history` marks every Survey a
/// tombstone names `collected`, from the tombstone. Every other command, and these two while
/// no Survey is collected, is `project::commands`'s.
pub(crate) fn run(command: crate::cli::ProjectCommand) -> Result<i32, String> {
    use crate::cli::ProjectCommand;
    match command {
        ProjectCommand::Survey { json } => {
            if let Some((log, tombstones)) = read_here()?
                && let Some(newest) = log.newest_survey()
                && let Some(tombstone) = tombstones
                    .iter()
                    .find(|tombstone| tombstone.survey_id == newest.survey_id)
            {
                return Err(format!(
                    "the newest Survey {} of {} was collected at {}; run `af project refresh` to \
                     record one",
                    newest.survey_id,
                    short(&newest.head),
                    time(tombstone.collected_unix_ms.get())
                ));
            }
            super::super::commands::run(ProjectCommand::Survey { json })
        }
        ProjectCommand::History { json } => super::history(json),
        other => super::super::commands::run(other),
    }
}

/// `af project history` once its chain is verified, `superseded` the Surveys whose records are
/// of a superseded version: in text, while any Survey is collected, the lines of
/// [`history_lines`]; otherwise `project::commands`'s.
pub(crate) fn history(json: bool, superseded: &BTreeSet<String>) -> Result<i32, String> {
    match read_here()? {
        Some((log, tombstones)) if !json && !tombstones.is_empty() => {
            for line in history_lines(&log, &tombstones, superseded) {
                println!("{line}");
            }
            Ok(0)
        }
        _ => super::super::commands::history(json, superseded),
    }
}
