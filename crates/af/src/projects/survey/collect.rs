//! Survey collection (ADR-0152 clause 13) and the bounded patterns of clause 14.
//!
//! Every Survey is an entry of kind `survey` of the machine budget (ADR-0144 clause 4): its CAS
//! bytes, last used at its cutoff. The Storage Budget collects Surveys in its own passes only:
//! the collection pass takes the Surveys of a Project beyond the newest `[storage] keep_surveys`,
//! oldest first and idle at least `keep_days`, and the budget pass may take any Survey, oldest
//! first, while af is over `max_bytes`. Neither takes the newest complete Survey, a Survey the
//! Project Store's lock holds (a running refresh, a plan) or one used within the hour, and
//! `af project refresh` collects nothing. Collection appends `SurveyCollected@1` to the Project
//! Store's log before the record leaves the CAS; a pass that finds a tombstone whose record is
//! still there finishes the deletion. The tombstones are a run of their own, `survey-collection`,
//! beside the run `project` the Surveyor appends its chain to: a `SurveyRecorded@1` a tombstone
//! names replays as collected, and `previous_survey_id` is a name, never a reachability edge.
//!
//! The pattern rules of clause 14 live here too: a `[survey]` pattern is at most
//! [`MAX_PATTERN_BYTES`] and a list at most [`MAX_PATTERNS`], and the loop that applies patterns
//! checks the Survey's Clock between matches.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use review_core::EventType;
use review_core::project::{
    ProjectSurveyV1, SurveyCollectedHistoryV1, SurveyCollectedPayloadV1, SurveyCollectedTreeV1,
    SurveyCompletenessV1, SurveyRecordedPayloadV1,
};
use review_store::{Cas, EventStore, NewEvent};

use super::super::{Log, ProjectStore, RUN_ID};
use super::measure::Clock;

/// The run of a Project Store's log that holds its Surveys' tombstones.
pub(crate) const COLLECTION_RUN: &str = "survey-collection";
/// The lock every writer of a Project Store holds: a refresh while it reads and records, a plan
/// while it builds a Brief, and collection while it tombstones and deletes.
const LOCK: &str = "project.lock";
const HOUR_MS: u64 = 3_600_000;
/// Upper bound on one `[survey]` pattern.
pub(crate) const MAX_PATTERN_BYTES: usize = 256;
/// Upper bound on the patterns of one `[survey]` list.
pub(crate) const MAX_PATTERNS: usize = 64;
/// Why a held Project Store's Surveys stay.
const HELD: &str = "a refresh or a plan holds its Project Store";
/// Why the newest complete Survey stays.
const NEWEST_COMPLETE: &str = "the newest complete Survey of its Project";

/// Where the CAS of the Project Store at `store` keeps the record `survey_id`.
pub(crate) fn object_path(store: &Path, survey_id: &str) -> PathBuf {
    let hex = survey_id.strip_prefix("sha256:").unwrap_or(survey_id);
    let (prefix, rest) = hex.split_at(2.min(hex.len()));
    store.join("cas").join("objects").join(prefix).join(rest)
}

/// The Project Store and the Survey a [`object_path`] names.
pub(crate) fn survey_of_object(object: &Path) -> Option<(PathBuf, String)> {
    let rest = object.file_name()?.to_str()?;
    let prefix_directory = object.parent()?;
    let prefix = prefix_directory.file_name()?.to_str()?;
    let objects = prefix_directory.parent()?;
    let cas = objects.parent()?;
    let store = cas.parent()?;
    let survey_id = format!("sha256:{prefix}{rest}");
    (review_core::is_digest(&survey_id) && object_path(store, &survey_id) == object)
        .then(|| (store.to_path_buf(), survey_id))
}

/// The tombstones of a Project Store's log, in order.
pub(crate) fn tombstones(events: &EventStore) -> Result<Vec<SurveyCollectedPayloadV1>, String> {
    events
        .replay(COLLECTION_RUN)
        .map_err(|error| format!("the Survey tombstones: {error}"))?
        .into_iter()
        .map(|event| match event.event_type {
            EventType::SurveyCollectedV1 => serde_json::from_value(event.payload)
                .map_err(|error| format!("SurveyCollected@1: {error}")),
            other => Err(format!("the Survey tombstones do not hold {other} events")),
        })
        .collect()
}

/// The Surveys a tombstone names.
pub(crate) fn collected_ids(events: &EventStore) -> Result<BTreeSet<String>, String> {
    Ok(tombstones(events)?
        .into_iter()
        .map(|tombstone| tombstone.survey_id)
        .collect())
}

/// The newest complete Survey of the chain: never collected.
fn newest_complete(log: &Log) -> Option<&str> {
    log.chain()
        .into_iter()
        .find(|recorded| recorded.completeness == SurveyCompletenessV1::Complete)
        .map(|recorded| recorded.survey_id.as_str())
}

/// Every recorded Survey once, oldest first.
fn oldest_first(log: &Log) -> Vec<&SurveyRecordedPayloadV1> {
    let mut seen = BTreeSet::new();
    log.chain()
        .into_iter()
        .rev()
        .filter(|recorded| seen.insert(recorded.survey_id.as_str()))
        .collect()
}

fn cutoff_ms(recorded: &SurveyRecordedPayloadV1) -> u64 {
    recorded.cutoff_unix_s.get().saturating_mul(1000)
}

fn lock_held(store: &Path) -> bool {
    crate::storage::lock_held(&store.join(LOCK))
}

/// One Survey as the machine budget sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inventoried {
    /// The CAS object of the record: the unit the budget evicts.
    pub(crate) object: PathBuf,
    /// Allocated bytes of the object.
    pub(crate) bytes: u64,
    /// The Survey's cutoff, in Unix milliseconds.
    pub(crate) last_use_unix_ms: u64,
    /// Why it is never collected now, when it is not.
    pub(crate) in_use: Option<String>,
}

/// The Surveys whose records the Project Store at `store` still holds, oldest first: a Survey
/// a tombstone names is among them only while an interrupted collection left its bytes. Reads
/// only.
pub(crate) fn inventory(store: &Path) -> Result<Vec<Inventoried>, String> {
    let events = EventStore::open_read_only(store.join("events.sqlite"))
        .map_err(|error| error.to_string())?;
    let log = Log::fold(&events.replay(RUN_ID).map_err(|error| error.to_string())?)?;
    let collected = collected_ids(&events)?;
    let held = lock_held(store);
    let protected = newest_complete(&log);
    let mut surveys = Vec::new();
    for recorded in oldest_first(&log) {
        let object = object_path(store, &recorded.survey_id);
        if std::fs::symlink_metadata(&object).is_err() {
            continue;
        }
        let bytes = review_sandbox::storage::allocated_bytes(&object)
            .map_err(|error| format!("measuring Survey {}: {error}", recorded.survey_id))?;
        let in_use = if held {
            Some(HELD.to_string())
        } else if !collected.contains(&recorded.survey_id)
            && protected == Some(recorded.survey_id.as_str())
        {
            Some(NEWEST_COMPLETE.to_string())
        } else {
            None
        };
        surveys.push(Inventoried {
            object,
            bytes,
            last_use_unix_ms: cutoff_ms(recorded),
            in_use,
        });
    }
    Ok(surveys)
}

/// What the collection pass takes from one Project: the Surveys beyond the newest
/// `keep_surveys`, oldest first, idle at least `idle_ms` and not used within the hour, never the
/// newest complete one. `keep_surveys` is a target: a protected Survey beyond it stays.
fn beyond_limit<'a>(
    log: &'a Log,
    collected: &BTreeSet<String>,
    keep_surveys: usize,
    idle_ms: u64,
    now_ms: u64,
) -> Vec<&'a SurveyRecordedPayloadV1> {
    let protected = newest_complete(log);
    let live: Vec<&SurveyRecordedPayloadV1> = oldest_first(log)
        .into_iter()
        .filter(|recorded| !collected.contains(&recorded.survey_id))
        .collect();
    let beyond = live.len().saturating_sub(keep_surveys);
    live.into_iter()
        .take(beyond)
        .filter(|recorded| {
            protected != Some(recorded.survey_id.as_str())
                && cutoff_ms(recorded).saturating_add(idle_ms) <= now_ms
                && cutoff_ms(recorded).saturating_add(HOUR_MS) <= now_ms
        })
        .collect()
}

/// The Project Store at `store`, held for collection without waiting: a Store a refresh or a
/// plan holds is left for the next pass.
fn hold(store: &Path) -> Result<ProjectStore, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.join(LOCK))
        .map_err(|error| format!("opening its lock: {error}"))?;
    let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|_| format!("{HELD} now; the next pass tries again"))?;
    let cas = Cas::open_existing(store.join("cas")).map_err(|error| error.to_string())?;
    let events =
        EventStore::open(store.join("events.sqlite")).map_err(|error| error.to_string())?;
    let name = store
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("a Project Store is named by its hex")?;
    Ok(ProjectStore {
        directory: store.to_path_buf(),
        project_id: format!("sha256:{name}"),
        cas,
        events,
        _lock: lock,
    })
}

/// A debug build's fixture may stop a collection after its tombstones, as an interruption
/// would, so the next pass is seen to finish it. Release binaries never read it.
fn stop_after_tombstones() -> bool {
    #[cfg(debug_assertions)]
    {
        std::env::var_os("AF_TEST_SURVEY_STOP_AFTER_TOMBSTONES").is_some_and(|value| value == "1")
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

/// The tombstone of `recorded`, with the totals its record holds when it can be read.
fn tombstone_of(
    store: &ProjectStore,
    recorded: &SurveyRecordedPayloadV1,
) -> SurveyCollectedPayloadV1 {
    let survey: Option<ProjectSurveyV1> = store
        .cas
        .get(&recorded.survey_id)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let tree = survey
        .as_ref()
        .and_then(|survey| survey.tree.body.as_ref())
        .map(|tree| SurveyCollectedTreeV1 {
            files: tree.files,
            bytes: tree.bytes,
            lines: tree.lines,
        });
    let history = survey
        .as_ref()
        .and_then(|survey| survey.history.body.as_ref())
        .map(|history| SurveyCollectedHistoryV1 {
            commits: history.commits,
            merges: history.merges,
        });
    SurveyCollectedPayloadV1 {
        survey_id: recorded.survey_id.clone(),
        head: recorded.head.clone(),
        cutoff_unix_s: recorded.cutoff_unix_s,
        completeness: recorded.completeness,
        tree,
        history,
        collected_unix_ms: crate::storage::now_unix_ms().into(),
    }
}

/// Delete the record `survey_id` from the CAS of a held Store; the bytes it held, 0 when it was
/// gone already.
fn delete(store: &ProjectStore, survey_id: &str) -> Result<u64, String> {
    let object = object_path(&store.directory, survey_id);
    let bytes = review_sandbox::storage::allocated_bytes(&object).unwrap_or(0);
    match store.cas.remove_unreachable(survey_id) {
        Ok(_) => Ok(bytes),
        Err(review_store::CasError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(0)
        }
        Err(error) => Err(format!("deleting Survey {survey_id}: {error}")),
    }
}

/// Tombstone, then delete, each of `chosen` in a held Store: the tombstone is in the log before
/// the bytes go, so an interruption between the two leaves what the next pass finishes. Returns
/// each record's object and the bytes deleted.
fn collect_held(
    store: &mut ProjectStore,
    chosen: &[&SurveyRecordedPayloadV1],
) -> Result<Vec<(PathBuf, u64)>, String> {
    let mut taken = Vec::new();
    for recorded in chosen {
        let tombstone = tombstone_of(store, recorded);
        tombstone.validate()?;
        let event = NewEvent::new(
            EventType::SurveyCollectedV1,
            serde_json::to_value(&tombstone).map_err(|error| error.to_string())?,
        )
        .at(review_core::task::collection::collected_time(
            crate::storage::now_unix_ms(),
        ));
        store
            .events
            .append(COLLECTION_RUN, &store.cas, event)
            .map_err(|error| format!("recording SurveyCollected@1: {error}"))?;
        let object = object_path(&store.directory, &recorded.survey_id);
        let bytes = if stop_after_tombstones() {
            0
        } else {
            delete(store, &recorded.survey_id)?
        };
        taken.push((object, bytes));
    }
    Ok(taken)
}

/// The log and tombstones of a held Store.
fn read_held(store: &ProjectStore) -> Result<(Log, BTreeSet<String>), String> {
    Ok((store.log()?, collected_ids(&store.events)?))
}

/// The collection pass's Surveys of the Project Store at `store`, as [`beyond_limit`] chooses
/// them from a read-only look: previewed when `apply` is off, otherwise chosen again and
/// collected under the Store's lock, which is taken only when the look chose any. Returns each
/// record's object and its bytes.
pub(crate) fn collect_beyond_limit(
    store: &Path,
    keep_surveys: usize,
    idle_ms: u64,
    now_ms: u64,
    apply: bool,
) -> Result<Vec<(PathBuf, u64)>, String> {
    let planned: Vec<(PathBuf, u64)> = {
        let events = EventStore::open_read_only(store.join("events.sqlite"))
            .map_err(|error| error.to_string())?;
        let log = Log::fold(&events.replay(RUN_ID).map_err(|error| error.to_string())?)?;
        let collected = collected_ids(&events)?;
        beyond_limit(&log, &collected, keep_surveys, idle_ms, now_ms)
            .into_iter()
            .map(|recorded| {
                let object = object_path(store, &recorded.survey_id);
                let bytes = review_sandbox::storage::allocated_bytes(&object).unwrap_or(0);
                (object, bytes)
            })
            .collect()
    };
    if !apply || planned.is_empty() {
        return Ok(planned);
    }
    let mut held = hold(store)?;
    let (log, collected) = read_held(&held)?;
    let chosen = beyond_limit(&log, &collected, keep_surveys, idle_ms, now_ms);
    collect_held(&mut held, &chosen)
}

/// The budget's eviction of one Survey: under the Store's lock, its protection read again (a
/// newer complete Survey may have been recorded since inventory, this one may have become the
/// newest complete one), then tombstoned and deleted. A Survey already tombstoned has its
/// bytes deleted. Returns the bytes deleted.
pub(crate) fn collect_one(store: &Path, survey_id: &str) -> Result<u64, String> {
    let mut held = hold(store)?;
    let (log, collected) = read_held(&held)?;
    if collected.contains(survey_id) {
        return delete(&held, survey_id);
    }
    if newest_complete(&log) == Some(survey_id) {
        return Err(format!(
            "Survey {survey_id} is {NEWEST_COMPLETE} now, so it is kept"
        ));
    }
    let recorded = log
        .chain()
        .into_iter()
        .find(|recorded| recorded.survey_id == survey_id)
        .cloned()
        .ok_or_else(|| format!("its Project Store records no Survey {survey_id}"))?;
    let taken = collect_held(&mut held, &[&recorded])?;
    Ok(taken.into_iter().map(|(_, bytes)| bytes).sum())
}

/// Finish what an interrupted collection left: the records of tombstoned Surveys still in the
/// CAS of the Project Store at `store` are deleted. A Store a refresh or a plan holds is left
/// for the next pass. Returns each record's object and the bytes deleted.
pub(crate) fn finish_collected(store: &Path) -> Result<Vec<(PathBuf, u64)>, String> {
    let left: Vec<String> = {
        let events = EventStore::open_read_only(store.join("events.sqlite"))
            .map_err(|error| error.to_string())?;
        // In the order the tombstones were appended.
        tombstones(&events)?
            .into_iter()
            .map(|tombstone| tombstone.survey_id)
            .filter(|survey_id| std::fs::symlink_metadata(object_path(store, survey_id)).is_ok())
            .collect()
    };
    if left.is_empty() || lock_held(store) {
        return Ok(Vec::new());
    }
    let held = match hold(store) {
        Ok(held) => held,
        Err(_) => return Ok(Vec::new()),
    };
    let collected = collected_ids(&held.events)?;
    let mut finished = Vec::new();
    for survey_id in left
        .iter()
        .filter(|survey_id| collected.contains(*survey_id))
    {
        let bytes = delete(&held, survey_id)?;
        if bytes > 0 {
            finished.push((object_path(store, survey_id), bytes));
        }
    }
    Ok(finished)
}

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
    review_core::task::collection::collected_time(unix_ms)
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// `af project`, with the two answers collection changes: `af project survey` says that a
/// collected newest Survey was collected, and `af project history` marks every Survey a
/// tombstone names `collected`, from the tombstone. Every other command, and these two while
/// no Survey is collected, is `projects::commands`'s.
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
        ProjectCommand::History { json: false } => match read_here()? {
            Some((log, tombstones)) if !tombstones.is_empty() => {
                for line in history_lines(&log, &tombstones) {
                    println!("{line}");
                }
                Ok(0)
            }
            _ => super::super::commands::run(ProjectCommand::History { json: false }),
        },
        other => super::super::commands::run(other),
    }
}

/// `af project history` in text: the chain newest first, a collected Survey answered from its
/// tombstone and marked `collected`.
fn history_lines(log: &Log, tombstones: &[SurveyCollectedPayloadV1]) -> Vec<String> {
    let chain = log.chain();
    let mut lines = vec![format!(
        "{} Survey{}",
        chain.len(),
        if chain.len() == 1 { "" } else { "s" }
    )];
    for recorded in chain {
        let tombstone = tombstones
            .iter()
            .find(|tombstone| tombstone.survey_id == recorded.survey_id);
        let line = match tombstone {
            None => format!(
                "{}  {}  head {}  cutoff {}  {}",
                recorded.survey_id,
                time(recorded.recorded_unix_ms.get()),
                short(&recorded.head),
                time(recorded.cutoff_unix_s.get().saturating_mul(1000)),
                recorded.completeness.as_str()
            ),
            Some(tombstone) => {
                let totals: Vec<String> = tombstone
                    .tree
                    .map(|tree| format!("{} files", tree.files.get()))
                    .into_iter()
                    .chain(
                        tombstone
                            .history
                            .map(|history| format!("{} commits", history.commits.get())),
                    )
                    .collect();
                format!(
                    "{}  {}  head {}  cutoff {}  {}  collected {}{}",
                    tombstone.survey_id,
                    time(recorded.recorded_unix_ms.get()),
                    short(&tombstone.head),
                    time(tombstone.cutoff_unix_s.get().saturating_mul(1000)),
                    tombstone.completeness.as_str(),
                    time(tombstone.collected_unix_ms.get()),
                    if totals.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", totals.join(", "))
                    }
                )
            }
        };
        lines.push(line);
    }
    lines
}

/// Refuse a `[survey]` pattern list over [`MAX_PATTERNS`] entries or holding a pattern over
/// [`MAX_PATTERN_BYTES`], naming `key` (ADR-0152 clause 14). The error names no pattern, so a
/// `SurveyFailed@1` may carry it.
pub(crate) fn check_patterns(key: &str, patterns: &[String]) -> Result<(), String> {
    if patterns.len() > MAX_PATTERNS {
        return Err(format!(
            "the [survey] policy does not load: {key} holds {} patterns, more than \
             {MAX_PATTERNS}",
            patterns.len()
        ));
    }
    if let Some(long) = patterns
        .iter()
        .find(|pattern| pattern.len() > MAX_PATTERN_BYTES)
    {
        return Err(format!(
            "the [survey] policy does not load: {key} holds a pattern of {} bytes, more than \
             {MAX_PATTERN_BYTES}",
            long.len()
        ));
    }
    Ok(())
}

/// Whether any of `patterns` matches `text`, checking the Survey's Clock before each match:
/// `None` when the deadline passed first. Each match is bounded by
/// `crate::project::glob_matches`'s cells, so the wall time past the deadline is one match.
/// The measurement's own pattern loop calls it once `measure.rs` is next changed; until then
/// only its tests do, and the expectation below fails the build the day it is called.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "measure.rs adopts it in the next Surveyor round")
)]
pub(super) fn any_match_within(patterns: &[String], text: &str, clock: &Clock) -> Option<bool> {
    for pattern in patterns {
        if clock.item() {
            return None;
        }
        if crate::project::glob_matches(pattern, text) {
            return Some(true);
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use review_core::project::SurveyCompletenessV1;
    use review_core::task::usage::DecimalU64;

    use super::*;

    fn recorded(
        n: u8,
        completeness: SurveyCompletenessV1,
        cutoff_s: u64,
    ) -> SurveyRecordedPayloadV1 {
        SurveyRecordedPayloadV1 {
            survey_id: format!("sha256:{}", format!("{n:02x}").repeat(32)),
            head: "1".repeat(40),
            cutoff_unix_s: cutoff_s.into(),
            completeness,
            policy_digest: format!("sha256:{}", "b".repeat(64)),
            surveyor_version: DecimalU64::from(2),
            refs_digest: format!("sha256:{}", "c".repeat(64)),
            previous_survey_id: None,
            recorded_unix_ms: (cutoff_s * 1000).into(),
        }
    }

    fn log(surveys: &[SurveyRecordedPayloadV1]) -> Log {
        Log {
            surveys: surveys
                .iter()
                .cloned()
                .map(super::super::Outcome::Recorded)
                .collect(),
            ..Log::default()
        }
    }

    const DAY_S: u64 = 86_400;
    const NOW_S: u64 = 1_800_000_000;

    fn ids(chosen: &[&SurveyRecordedPayloadV1]) -> Vec<String> {
        chosen
            .iter()
            .map(|recorded| recorded.survey_id.clone())
            .collect()
    }

    #[test]
    fn collection_takes_the_oldest_beyond_the_limit_and_never_the_newest_complete_one() {
        use SurveyCompletenessV1::{Complete, Partial};
        let old = NOW_S - 30 * DAY_S;
        let surveys = [
            recorded(1, Complete, old),
            recorded(2, Complete, old + 1),
            recorded(3, Partial, old + 2),
            recorded(4, Partial, old + 3),
            recorded(5, Partial, old + 4),
        ];
        let log = log(&surveys);
        let idle = 14 * DAY_S * 1000;
        let now = NOW_S * 1000;
        // Two kept, the newest; three beyond, of which the second is the newest complete one.
        let chosen = beyond_limit(&log, &BTreeSet::new(), 2, idle, now);
        assert_eq!(
            ids(&chosen),
            vec![surveys[0].survey_id.clone(), surveys[2].survey_id.clone()]
        );
        // What a tombstone names is no longer counted.
        let collected = BTreeSet::from([surveys[0].survey_id.clone()]);
        let chosen = beyond_limit(&log, &collected, 2, idle, now);
        assert_eq!(ids(&chosen), vec![surveys[2].survey_id.clone()]);
        // A limit the Project is within takes nothing.
        assert!(beyond_limit(&log, &BTreeSet::new(), 5, idle, now).is_empty());
        // Retention age and recent use protect whatever the limit says.
        assert!(beyond_limit(&log, &BTreeSet::new(), 0, 31 * DAY_S * 1000, now).is_empty());
        let fresh = [
            recorded(6, Partial, NOW_S - 60),
            recorded(7, Partial, NOW_S - 30),
        ];
        assert!(beyond_limit(&self::log(&fresh), &BTreeSet::new(), 0, 0, now).is_empty());
    }

    #[test]
    fn a_complete_survey_followed_by_partial_ones_is_the_one_kept() {
        use SurveyCompletenessV1::{Complete, Partial};
        let old = NOW_S - 30 * DAY_S;
        let surveys = [
            recorded(1, Complete, old),
            recorded(2, Partial, old + 1),
            recorded(3, Partial, old + 2),
        ];
        let log = log(&surveys);
        let chosen = beyond_limit(&log, &BTreeSet::new(), 0, 0, NOW_S * 1000);
        assert_eq!(
            ids(&chosen),
            vec![surveys[1].survey_id.clone(), surveys[2].survey_id.clone()]
        );
    }

    #[test]
    fn an_object_path_names_its_store_and_survey() {
        let store = Path::new("/state/af/project/abc");
        let id = format!("sha256:{}", "ab".repeat(32));
        let object = object_path(store, &id);
        assert!(object.ends_with(format!("cas/objects/ab/{}", "ab".repeat(31))));
        assert_eq!(survey_of_object(&object), Some((store.to_path_buf(), id)));
        assert_eq!(
            survey_of_object(Path::new("/state/af/project/abc/events.sqlite")),
            None
        );
    }

    #[test]
    fn a_pattern_list_is_bounded_and_the_refusal_names_its_key() {
        let one = |bytes: usize| vec!["a".repeat(bytes)];
        assert!(check_patterns("exclude", &one(256)).is_ok());
        let error = check_patterns("exclude", &one(257)).unwrap_err();
        assert!(
            error.contains("exclude holds a pattern of 257 bytes"),
            "{error}"
        );
        let sixty_four: Vec<String> = (0..64).map(|n| format!("p{n}/**")).collect();
        assert!(check_patterns("tests", &sixty_four).is_ok());
        let mut sixty_five = sixty_four.clone();
        sixty_five.push("one/more".into());
        let error = check_patterns("agent_trailers", &sixty_five).unwrap_err();
        assert!(
            error.contains("agent_trailers holds 65 patterns, more than 64"),
            "{error}"
        );
        assert!(!error.contains('@'));
    }

    #[test]
    fn patterns_are_applied_between_clock_checks() {
        let (failing, text) = (format!("{}b", "*a".repeat(30)), "a".repeat(30));
        let patterns = vec![failing, "*a".repeat(30)];
        let clock = Clock::new(Instant::now(), 60_000);
        assert_eq!(any_match_within(&patterns[..1], &text, &clock), Some(false));
        assert_eq!(any_match_within(&patterns, &text, &clock), Some(true));
        let passed = Clock::new(Instant::now() - Duration::from_secs(2), 1);
        assert_eq!(any_match_within(&patterns, &text, &passed), None);
    }
}
