//! Survey CLI dispatch, terminal output, and application wiring.

use super::ProjectStore;
pub(crate) use af_project::survey::*;
use std::path::Path;
pub(crate) mod collect;

pub(crate) fn refresh(
    checkout: &super::Checkout,
    force: bool,
    register: bool,
) -> Result<Refreshed, String> {
    af_project::survey::refresh(&super::Application, checkout, force, register)
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// After `af onboard --apply` registered the Project and the Workspace: record the first Survey
/// and print its summary line, on stdout, or on stderr when stdout carries `--json`. A Survey
/// that fails prints one line on stderr; nothing changes onboarding's output or exit code.
pub(crate) fn after_onboard(repo: &Path, json: bool) {
    let Ok(super::Located::Checkout(checkout)) = super::locate(repo) else {
        return;
    };
    let say = |line: String| {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    match refresh(&checkout, false, false) {
        Ok(Refreshed::Recorded(recorded)) => {
            say(format!("af onboard: recorded {}", summary_line(&recorded)))
        }
        Ok(Refreshed::Unchanged(recorded)) => say(format!(
            "af onboard: {}; {}",
            unchanged_line(&recorded.event),
            summary_line(&recorded)
        )),
        Err(error) => eprintln!("af onboard: no Survey was recorded: {error}"),
    }
}

/// `af project`, as `collect::run` answers it, with what replay binds first: `af project
/// history` verifies every Survey event of the chain against its record or its tombstone before
/// any of it is printed, and `af project survey --section`, a text-only option, prints the named
/// sections of the newest Survey's summary. `--section` with `--json` never reaches here: it is
/// usage error 2.
pub(crate) fn run(command: crate::cli::ProjectCommand, sections: &[String]) -> Result<i32, String> {
    use crate::cli::ProjectCommand;
    match command {
        ProjectCommand::History { json } => history(json),
        ProjectCommand::Survey { json: false } if !sections.is_empty() => survey_sections(sections),
        ProjectCommand::Survey { json } => {
            // A collected newest Survey's event is bound to its tombstone before `collect::run`
            // names its head.
            if let Some(store) = existing_store_here()?
                && let Some(newest) = store.log()?.newest_survey()
            {
                store.collected(newest)?;
            }
            collect::run(ProjectCommand::Survey { json })
        }
        other => collect::run(other),
    }
}

/// `af project history`: every Survey event of the chain verified against its record or its
/// tombstone before any of it is printed, a Survey whose record is of a superseded version
/// listed as [`SUPERSEDED_LINE`] with none of its event's other fields.
pub(crate) fn history(json: bool) -> Result<i32, String> {
    let superseded = match existing_store_here()? {
        Some(store) => store.verify_chain()?,
        None => std::collections::BTreeSet::new(),
    };
    collect::history(json, &superseded)
}

/// The invoking checkout's Project Store, when it holds a log already; opening it creates
/// nothing a later `af project` command would not.
fn existing_store_here() -> Result<Option<ProjectStore>, String> {
    let checkout = super::require_checkout(Path::new("."))?;
    let state_home = crate::xdg_state_root()?;
    let directory = super::store_directory(&state_home, &checkout.project_id);
    if std::fs::symlink_metadata(directory.join("events.sqlite")).is_err() {
        return Ok(None);
    }
    ProjectStore::open(&super::Application, &state_home, &checkout.project_id).map(Some)
}

/// `af project survey --section NAME…`: the newest Survey's first lines and the named sections.
fn survey_sections(sections: &[String]) -> Result<i32, String> {
    let none = "this Project has no Survey yet; run `af project refresh` to record one";
    let store = existing_store_here()?.ok_or(none)?;
    let log = store.log()?;
    if let Some(newest) = log.newest_survey()
        && let Some(tombstone) = store.collected(newest)?
    {
        return Err(format!(
            "the newest Survey {} of {} was collected at {}; run `af project refresh` to \
             record one",
            tombstone.survey_id,
            short(&tombstone.head),
            af_contracts::task::collection::collected_time(tombstone.collected_unix_ms.get())
        ));
    }
    let recorded = newest(&store, &log)?.ok_or(none)?;
    for line in summary_of(&recorded, sections) {
        println!("{line}");
    }
    Ok(0)
}
