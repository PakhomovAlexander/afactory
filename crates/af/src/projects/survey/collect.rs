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
//! A record is reached, read and deleted through directory descriptors, ADR-0144's removal
//! boundary: the Project Store from `/`, then `cas`, `objects` and the digest-prefix directory,
//! each opened without following a link, the object opened and unlinked relative to its prefix
//! directory. A link anywhere on that way, or a directory on another device than the Store,
//! refuses the Survey, which is reported and keeps its record and its place in the log. So does a
//! record the file system will not let af read; a record whose bytes were read and are no Survey
//! is collected with an `unreadable` tombstone, since reclaiming corrupt bytes is the budget's
//! job.
//!
//! The pattern rules of clause 14 live here too: a `[survey]` pattern is at most
//! [`MAX_PATTERN_BYTES`] and a list at most [`MAX_PATTERNS`], and the measurement's one pattern
//! loop checks the Survey's deadline before each match.

use std::collections::BTreeSet;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use review_core::EventType;
use review_core::project::{
    ProjectSurveyV1, SurveyCollectedHistoryV1, SurveyCollectedPayloadV1, SurveyCollectedTreeV1,
    SurveyCompletenessV1, SurveyReasonV1, SurveyRecordedPayloadV1,
};
use review_sandbox::Identity;
use review_store::{Cas, EventStore, NewEvent};

use super::super::{Git, Log, ProjectStore, RUN_ID};
use super::measure::{Basis, Failure, Sections};
use super::policy::Policy;

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
pub(crate) const HELD: &str = "a refresh or a plan holds its Project Store";
/// Why the newest complete Survey stays.
const NEWEST_COMPLETE: &str = "the newest complete Survey of its Project";
/// Upper bound on the reason an `unreadable` tombstone carries.
const MAX_UNREADABLE_BYTES: usize = review_core::project::MAX_SURVEY_TEXT_BYTES;

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

/// One Survey's record, reached through directory descriptors (ADR-0144 clause 3): the Project
/// Store from `/` and `cas`, `objects` and the digest-prefix directory below it, each opened
/// `O_NOFOLLOW | O_DIRECTORY`, and the object stated without following it. It is read and
/// unlinked relative to `prefix`, never through a path.
struct Record {
    /// The digest-prefix directory.
    prefix: OwnedFd,
    /// The object's name in it.
    name: String,
    /// The object as it was stated.
    identity: Identity,
    /// Allocated bytes of the object.
    bytes: u64,
}

/// The record `survey_id` of the Project Store at `store`; `None` when the CAS does not hold it.
/// A symbolic link in place of the Store's path, `cas`, `objects`, the prefix directory or the
/// object, a directory on another device than the Store's, or an object that is no regular file
/// is the error: the record is never reached through it.
fn locate(store: &Path, survey_id: &str) -> Result<Option<Record>, String> {
    use nix::fcntl::AtFlags;
    use nix::sys::stat::{SFlag, fstatat};
    let hex = survey_id
        .strip_prefix("sha256:")
        .filter(|_| review_core::is_digest(survey_id))
        .ok_or_else(|| format!("`{survey_id}` names no CAS object"))?;
    let (prefix_name, name) = hex.split_at(2);
    let relative = store
        .strip_prefix("/")
        .map_err(|_| "its Project Store is not named by an absolute path".to_string())?;
    let root = review_sandbox::open_anchor(Path::new("/"))
        .map_err(|error| format!("opening `/`: {error}"))?;
    let directory = review_sandbox::open_beneath(&root, relative)
        .map_err(|error| format!("its Project Store cannot be reached without a link: {error}"))?;
    let objects = review_sandbox::open_beneath(&directory, Path::new("cas/objects"))
        .map_err(|error| format!("its CAS objects directory is refused: {error}"))?;
    let prefix = match review_sandbox::open_beneath(&objects, Path::new(prefix_name)) {
        Ok(prefix) => prefix,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("its CAS prefix directory is refused: {error}")),
    };
    let device = |descriptor: &OwnedFd| {
        Identity::of_descriptor(descriptor)
            .map(|identity| identity.device)
            .map_err(|error| format!("inspecting its CAS: {error}"))
    };
    let store_device = device(&directory)?;
    if device(&objects)? != store_device || device(&prefix)? != store_device {
        return Err(format!(
            "its CAS prefix directory `{prefix_name}` resolves outside its Project Store"
        ));
    }
    let stat = match fstatat(&prefix, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(errno) => return Err(format!("inspecting its record: {errno}")),
    };
    let kind = SFlag::from_bits_truncate(stat.st_mode as nix::libc::mode_t) & SFlag::S_IFMT;
    if kind == SFlag::S_IFLNK {
        return Err("its record is a symbolic link; af never reads or removes through one".into());
    }
    if kind != SFlag::S_IFREG {
        return Err("its record is not a regular file".into());
    }
    #[allow(clippy::unnecessary_cast)]
    let bytes = (stat.st_blocks as u64).saturating_mul(512);
    Ok(Some(Record {
        prefix,
        name: name.to_string(),
        identity: Identity::of_stat(&stat),
        bytes,
    }))
}

impl Record {
    /// The record's bytes, read from the object opened relative to its prefix directory without
    /// following a link, the same file [`locate`] stated; at most one byte beyond the largest
    /// record a Survey can be.
    fn read(&self) -> std::io::Result<Vec<u8>> {
        use nix::fcntl::{OFlag, openat};
        let descriptor = openat(
            &self.prefix,
            self.name.as_str(),
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )?;
        let file = std::fs::File::from(descriptor);
        if Identity::of_descriptor(&file)? != self.identity {
            return Err(std::io::Error::other(
                "the record changed since it was found",
            ));
        }
        let mut bytes = Vec::new();
        file.take(largest_record().saturating_add(1))
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Unlink the object relative to its prefix directory, only while its name still holds the
    /// file [`locate`] stated. Returns its allocated bytes.
    fn unlink(self) -> Result<u64, String> {
        use nix::fcntl::AtFlags;
        use nix::unistd::{UnlinkatFlags, unlinkat};
        let now = nix::sys::stat::fstatat(
            &self.prefix,
            self.name.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|errno| format!("inspecting its record: {errno}"))?;
        if Identity::of_stat(&now) != self.identity {
            return Err("its record changed since it was found, so it is left".into());
        }
        unlinkat(&self.prefix, self.name.as_str(), UnlinkatFlags::NoRemoveDir)
            .map_err(|errno| format!("deleting its record: {errno}"))?;
        Ok(self.bytes)
    }
}

/// The most bytes a Survey's record can hold: the ceiling of `max_survey_bytes`.
fn largest_record() -> u64 {
    Policy::default().max_survey_bytes
}

/// `text` cut at a character boundary to what an `unreadable` tombstone holds.
fn bounded(text: String) -> String {
    let mut end = text.len().min(MAX_UNREADABLE_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// What a Survey's record says for its tombstone: the totals of `tree` and `history`, or, when
/// its bytes are no Survey, why, in af's own words, never the bytes themselves.
fn totals(
    survey_id: &str,
    bytes: &[u8],
) -> Result<
    (
        Option<SurveyCollectedTreeV1>,
        Option<SurveyCollectedHistoryV1>,
    ),
    String,
> {
    if bytes.len() as u64 > largest_record() {
        return Err(bounded(format!(
            "its record holds more than {} bytes, more than any Survey",
            largest_record()
        )));
    }
    if review_store::canonical::blob_content_id(bytes) != survey_id {
        return Err("its bytes do not match its digest".into());
    }
    let survey: ProjectSurveyV1 = serde_json::from_slice(bytes).map_err(|error| {
        let category = match error.classify() {
            serde_json::error::Category::Io => "a read",
            serde_json::error::Category::Syntax => "a syntax",
            serde_json::error::Category::Data => "a data",
            serde_json::error::Category::Eof => "an end-of-input",
        };
        bounded(format!(
            "its bytes are not a Survey record: {category} error at line {}, column {}",
            error.line(),
            error.column()
        ))
    })?;
    let tree = survey.tree.body.as_ref().map(|tree| SurveyCollectedTreeV1 {
        files: tree.files,
        bytes: tree.bytes,
        lines: tree.lines,
    });
    let history = survey
        .history
        .body
        .as_ref()
        .map(|history| SurveyCollectedHistoryV1 {
            commits: history.commits,
            merges: history.merges,
        });
    Ok((tree, history))
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
/// a tombstone names is among them only while an interrupted collection left its bytes. A
/// record that cannot be reached without a link is listed in use, with nothing measured through
/// the link. Reads only.
pub(crate) fn inventory(store: &Path) -> Result<Vec<Inventoried>, String> {
    let events = EventStore::open_read_only(store.join("events.sqlite"))
        .map_err(|error| error.to_string())?;
    let log = Log::fold(&events.replay(RUN_ID).map_err(|error| error.to_string())?)?;
    let collected = collected_ids(&events)?;
    let held = lock_held(store);
    let protected = newest_complete(&log);
    let mut surveys = Vec::new();
    for recorded in oldest_first(&log) {
        let (bytes, refused) = match locate(store, &recorded.survey_id) {
            Ok(None) => continue,
            Ok(Some(record)) => (record.bytes, None),
            Err(reason) => (0, Some(format!("its record is left: {reason}"))),
        };
        let in_use = if refused.is_some() {
            refused
        } else if held {
            Some(HELD.to_string())
        } else if !collected.contains(&recorded.survey_id)
            && protected == Some(recorded.survey_id.as_str())
        {
            Some(NEWEST_COMPLETE.to_string())
        } else {
            None
        };
        surveys.push(Inventoried {
            object: object_path(store, &recorded.survey_id),
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

/// The Project Store at `store`, held for collection without waiting; `None` while a refresh or
/// a plan holds it, so it is left for the next pass.
fn hold(store: &Path) -> Result<Option<ProjectStore>, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.join(LOCK))
        .map_err(|error| format!("opening its lock: {error}"))?;
    let Ok(lock) = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
    else {
        return Ok(None);
    };
    let cas = Cas::open_existing(store.join("cas")).map_err(|error| error.to_string())?;
    let events =
        EventStore::open(store.join("events.sqlite")).map_err(|error| error.to_string())?;
    let name = store
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("a Project Store is named by its hex")?;
    Ok(Some(ProjectStore {
        directory: store.to_path_buf(),
        project_id: format!("sha256:{name}"),
        cas,
        events,
        _lock: lock,
    }))
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

/// One Survey a collection took, or would take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Taken {
    /// The CAS object of its record.
    pub(crate) object: PathBuf,
    /// The allocated bytes its record held: what its deletion frees.
    pub(crate) bytes: u64,
    /// Why its record's bytes are no Survey, when it was collected `unreadable`.
    pub(crate) unreadable: Option<String>,
}

/// What a collection did, or would do, with the Surveys it chose in one Project Store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Pass {
    pub(crate) taken: Vec<Taken>,
    /// Chosen and left because a refresh or a plan holds the Store: each record's object and
    /// its bytes. Nothing was written.
    pub(crate) held: Vec<(PathBuf, u64)>,
    /// Chosen and refused: each record's object and why. Its Survey keeps its record, and the
    /// log gains no tombstone for it, unless the reason says one was recorded.
    pub(crate) refused: Vec<(PathBuf, String)>,
}

/// Take one Survey of the Project Store at `directory`: its record found through descriptors and
/// read for its tombstone, then, when `store` holds the Store (an applied pass), the tombstone
/// appended and the record unlinked. A record that cannot be reached without a link, or that the
/// file system will not let af read, refuses the Survey with nothing written; one whose bytes are
/// no Survey, or that is gone from the CAS, is collected with `unreadable`.
fn take(
    directory: &Path,
    store: Option<&mut ProjectStore>,
    recorded: &SurveyRecordedPayloadV1,
) -> Result<Taken, String> {
    let survey_id = recorded.survey_id.as_str();
    let record = locate(directory, survey_id)?;
    let read = match &record {
        None => Err("its record is not in the CAS".to_string()),
        Some(record) => match record.read() {
            Ok(bytes) => totals(survey_id, &bytes),
            Err(error) => {
                return Err(format!("its record cannot be read, so it is kept: {error}"));
            }
        },
    };
    let bytes = record.as_ref().map_or(0, |record| record.bytes);
    let (tree, history, unreadable) = match read {
        Ok((tree, history)) => (tree, history, None),
        Err(why) => (None, None, Some(why)),
    };
    let object = object_path(directory, survey_id);
    let Some(store) = store else {
        return Ok(Taken {
            object,
            bytes,
            unreadable,
        });
    };
    let tombstone = SurveyCollectedPayloadV1 {
        survey_id: survey_id.to_string(),
        head: recorded.head.clone(),
        cutoff_unix_s: recorded.cutoff_unix_s,
        completeness: recorded.completeness,
        tree,
        history,
        unreadable: unreadable.clone(),
        collected_unix_ms: crate::storage::now_unix_ms().into(),
    };
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
    let bytes = match record {
        Some(_) if stop_after_tombstones() => 0,
        Some(record) => record.unlink().map_err(|error| {
            format!("{error}; its tombstone is recorded, and the next pass deletes it")
        })?,
        None => 0,
    };
    Ok(Taken {
        object,
        bytes,
        unreadable,
    })
}

/// [`take`] each of `chosen`, one at a time: a refused Survey is reported and the rest go on.
fn take_each(
    directory: &Path,
    mut store: Option<&mut ProjectStore>,
    chosen: &[&SurveyRecordedPayloadV1],
) -> Pass {
    let mut pass = Pass::default();
    for recorded in chosen {
        match take(directory, store.as_deref_mut(), recorded) {
            Ok(taken) => pass.taken.push(taken),
            Err(why) => pass.refused.push((
                object_path(directory, &recorded.survey_id),
                format!("Survey {} is refused: {why}", recorded.survey_id),
            )),
        }
    }
    pass
}

/// The log and tombstones of a held Store.
fn read_held(store: &ProjectStore) -> Result<(Log, BTreeSet<String>), String> {
    Ok((store.log()?, collected_ids(&store.events)?))
}

/// The collection pass in the Project Store at `store`: the Surveys [`beyond_limit`] chooses from
/// a read-only look, previewed when `apply` is off, otherwise chosen again and taken under the
/// Store's lock, which is taken only when the look chose any. Preview and pass apply the same
/// lock check: while a refresh or a plan holds the Store, every Survey the look chose is listed
/// held and nothing else is done.
pub(crate) fn collect_beyond_limit(
    store: &Path,
    keep_surveys: usize,
    idle_ms: u64,
    now_ms: u64,
    apply: bool,
) -> Result<Pass, String> {
    let chosen: Vec<SurveyRecordedPayloadV1> = {
        let events = EventStore::open_read_only(store.join("events.sqlite"))
            .map_err(|error| error.to_string())?;
        let log = Log::fold(&events.replay(RUN_ID).map_err(|error| error.to_string())?)?;
        let collected = collected_ids(&events)?;
        beyond_limit(&log, &collected, keep_surveys, idle_ms, now_ms)
            .into_iter()
            .cloned()
            .collect()
    };
    if chosen.is_empty() {
        return Ok(Pass::default());
    }
    let all_held = || Pass {
        held: chosen
            .iter()
            .map(|recorded| {
                let bytes = locate(store, &recorded.survey_id)
                    .ok()
                    .flatten()
                    .map_or(0, |record| record.bytes);
                (object_path(store, &recorded.survey_id), bytes)
            })
            .collect(),
        ..Pass::default()
    };
    if !apply {
        if lock_held(store) {
            return Ok(all_held());
        }
        return Ok(take_each(store, None, &chosen.iter().collect::<Vec<_>>()));
    }
    let Some(mut held) = hold(store)? else {
        return Ok(all_held());
    };
    let (log, collected) = read_held(&held)?;
    let chosen = beyond_limit(&log, &collected, keep_surveys, idle_ms, now_ms);
    Ok(take_each(store, Some(&mut held), &chosen))
}

/// The budget's eviction of one Survey, or its preview when `apply` is off: under the Store's
/// lock, its protection read again (a newer complete Survey may have been recorded since
/// inventory, this one may have become the newest complete one), then [`take`]n. A Survey
/// already tombstoned has its record deleted.
pub(crate) fn collect_one(store: &Path, survey_id: &str, apply: bool) -> Result<Taken, String> {
    if !apply {
        let events = EventStore::open_read_only(store.join("events.sqlite"))
            .map_err(|error| error.to_string())?;
        let log = Log::fold(&events.replay(RUN_ID).map_err(|error| error.to_string())?)?;
        let recorded = log
            .chain()
            .into_iter()
            .find(|recorded| recorded.survey_id == survey_id)
            .cloned()
            .ok_or_else(|| format!("its Project Store records no Survey {survey_id}"))?;
        return take(store, None, &recorded);
    }
    let Some(mut held) = hold(store)? else {
        return Err(format!("{HELD} now; the next pass tries again"));
    };
    let (log, collected) = read_held(&held)?;
    if collected.contains(survey_id) {
        let bytes = match locate(store, survey_id)? {
            Some(record) => record.unlink()?,
            None => 0,
        };
        return Ok(Taken {
            object: object_path(store, survey_id),
            bytes,
            unreadable: None,
        });
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
    take(store, Some(&mut held), &recorded)
}

/// Finish what an interrupted collection left: the records of tombstoned Surveys still in the
/// CAS of the Project Store at `store` are deleted through descriptors, and one that cannot be
/// reached without a link is refused. A Store a refresh or a plan holds is left for the next
/// pass.
pub(crate) fn finish_collected(store: &Path) -> Result<Pass, String> {
    let left: Vec<String> = {
        let events = EventStore::open_read_only(store.join("events.sqlite"))
            .map_err(|error| error.to_string())?;
        // In the order the tombstones were appended.
        tombstones(&events)?
            .into_iter()
            .map(|tombstone| tombstone.survey_id)
            .filter(|survey_id| !matches!(locate(store, survey_id), Ok(None)))
            .collect()
    };
    if left.is_empty() || lock_held(store) {
        return Ok(Pass::default());
    }
    let Some(held) = hold(store)? else {
        return Ok(Pass::default());
    };
    let collected = collected_ids(&held.events)?;
    let mut pass = Pass::default();
    for survey_id in left
        .iter()
        .filter(|survey_id| collected.contains(*survey_id))
    {
        let object = object_path(store, survey_id);
        match locate(store, survey_id).and_then(|record| record.map(Record::unlink).transpose()) {
            Ok(None) => {}
            Ok(Some(bytes)) => pass.taken.push(Taken {
                object,
                bytes,
                unreadable: None,
            }),
            Err(why) => pass.refused.push((
                object,
                format!("the record of collected Survey {survey_id} is left: {why}"),
            )),
        }
    }
    Ok(pass)
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
/// tombstone and marked `collected`, or `collected (unreadable)` when its record's bytes were no
/// Survey.
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
                    "{}  {}  head {}  cutoff {}  {}  collected {}{}{}",
                    tombstone.survey_id,
                    time(recorded.recorded_unix_ms.get()),
                    short(&tombstone.head),
                    time(tombstone.cutoff_unix_s.get().saturating_mul(1000)),
                    tombstone.completeness.as_str(),
                    if tombstone.unreadable.is_some() {
                        "(unreadable) "
                    } else {
                        ""
                    },
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

/// A test's hook, run as each match begins with how many have begun.
#[cfg(test)]
type BeforeMatch = Box<dyn FnMut(u64) + Send>;

/// What the pattern matches of one Survey did: how many patterns were matched, and whether the
/// deadline cut a list short.
#[derive(Default)]
pub(super) struct Matches {
    matched: AtomicU64,
    cut: AtomicBool,
    #[cfg(test)]
    before_match: Mutex<Option<BeforeMatch>>,
}

impl Matches {
    fn begin(&self) {
        let _begun = self.matched.fetch_add(1, Ordering::Relaxed) + 1;
        #[cfg(test)]
        if let Some(hook) = self
            .before_match
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            hook(_begun);
        }
    }

    /// Patterns matched so far.
    #[cfg(test)]
    fn matched(&self) -> u64 {
        self.matched.load(Ordering::Relaxed)
    }

    /// Whether the deadline cut a pattern list short.
    fn cut(&self) -> bool {
        self.cut.load(Ordering::Relaxed)
    }
}

/// Whether any of `patterns` matches `text`, checking `deadline`, the Survey's one deadline,
/// before each match: `None` when it passed first. Each match is bounded by
/// `crate::project::glob_matches`'s cells, so the wall time past the deadline is one match.
pub(super) fn any_match_within(
    patterns: &[String],
    text: &str,
    deadline: Instant,
    matches: &Matches,
) -> Option<bool> {
    for pattern in patterns {
        if Instant::now() >= deadline {
            return None;
        }
        matches.begin();
        if crate::project::glob_matches(pattern, text) {
            return Some(true);
        }
    }
    Some(false)
}

/// One Survey being measured, known by the pattern lists of its policy: the measurement hands
/// its one pattern loop a list and nothing else, and its streams are read on threads of their
/// own, so the list is what names the Survey's deadline.
struct Measuring {
    /// The address of each non-empty pattern list of the policy.
    lists: Vec<usize>,
    deadline: Instant,
    matches: Arc<Matches>,
}

/// Every Survey this process measures now.
static MEASURING: Mutex<Vec<Measuring>> = Mutex::new(Vec::new());

/// The pattern lists of one policy under a Survey's deadline, from now until the value drops.
struct UnderDeadline<'a> {
    matches: Arc<Matches>,
    /// The lists are known by address: the policy is borrowed, so none moves meanwhile.
    _policy: &'a Policy,
}

impl<'a> UnderDeadline<'a> {
    fn new(policy: &'a Policy, deadline: Instant, matches: Arc<Matches>) -> Self {
        let lists = [
            &policy.exclude,
            &policy.tests,
            &policy.agent_authors,
            &policy.agent_trailers,
        ]
        .into_iter()
        .filter(|list| !list.is_empty())
        .map(|list| list.as_ptr() as usize)
        .collect();
        MEASURING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Measuring {
                lists,
                deadline,
                matches: Arc::clone(&matches),
            });
        Self {
            matches,
            _policy: policy,
        }
    }
}

impl Drop for UnderDeadline<'_> {
    fn drop(&mut self) {
        MEASURING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|measuring| !Arc::ptr_eq(&measuring.matches, &self.matches));
    }
}

/// The measurement's one pattern loop (`measure.rs`'s `any_match` delegates here): whether any
/// of `patterns` matches `text`. A list of a policy being measured is applied through
/// [`any_match_within`] under its Survey's deadline; once the deadline has passed it matches
/// nothing more and says so to the Survey, so a pattern list overruns the deadline by at most
/// the one match in progress. Any other list, a test's, matches unbounded.
pub(super) fn any_match(patterns: &[String], text: &str) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let address = patterns.as_ptr() as usize;
    let measuring = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|measuring| measuring.lists.contains(&address))
        .map(|measuring| (measuring.deadline, Arc::clone(&measuring.matches)));
    match measuring {
        None => patterns
            .iter()
            .any(|pattern| crate::project::glob_matches(pattern, text)),
        Some((deadline, matches)) => any_match_within(patterns, text, deadline, &matches)
            .unwrap_or_else(|| {
                matches.cut.store(true, Ordering::Relaxed);
                false
            }),
    }
}

/// Measure the head under one deadline from `started` (ADR-0152 clause 14), every pattern list
/// checking it before each match.
pub(super) fn measure(
    git: &Git,
    basis: &Basis,
    started: Instant,
    clock: super::measure::Clock,
) -> Result<Sections, Failure> {
    measure_counted(git, basis, started, clock, Arc::default())
}

/// [`measure`], counting into `matches`. The listing stops at the item after a match the
/// deadline cut, and `history` checks the deadline again before its later reads, so a section
/// a cut list reached is partial already, except `tree` when the deadline passed while its
/// `tests` were matched after its last read: then `history` and everything after it is
/// unavailable, and a `tree` that says complete is partial with reason `max_wall_ms`. (A cut in
/// the first commit of `history`, before any commit was counted, also leaves `history`
/// unavailable; `tree` is then marked partial although it was whole, which only measures it
/// again.)
fn measure_counted(
    git: &Git,
    basis: &Basis,
    started: Instant,
    clock: super::measure::Clock,
    matches: Arc<Matches>,
) -> Result<Sections, Failure> {
    let deadline = started + Duration::from_millis(basis.policy.max_wall_ms);
    let under = UnderDeadline::new(basis.policy, deadline, Arc::clone(&matches));
    let measured = super::measure::measure(git, basis, clock);
    drop(under);
    let mut sections = measured?;
    if matches.cut()
        && sections.tree.completeness == SurveyCompletenessV1::Complete
        && sections.history.completeness == SurveyCompletenessV1::Unavailable
    {
        sections.tree.completeness = SurveyCompletenessV1::Partial;
        sections.tree.reason = Some(SurveyReasonV1::MaxWallMs);
    }
    Ok(sections)
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
    fn patterns_are_applied_between_deadline_checks() {
        let (failing, text) = (format!("{}b", "*a".repeat(30)), "a".repeat(30));
        let patterns = vec![failing, "*a".repeat(30)];
        let matches = Matches::default();
        let later = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            any_match_within(&patterns[..1], &text, later, &matches),
            Some(false)
        );
        assert_eq!(
            any_match_within(&patterns, &text, later, &matches),
            Some(true)
        );
        assert_eq!(matches.matched(), 3);
        let passed = Instant::now() - Duration::from_secs(2);
        assert_eq!(any_match_within(&patterns, &text, passed, &matches), None);
        assert_eq!(matches.matched(), 3, "nothing is matched past the deadline");
    }

    /// `?` is one byte of the canonical encoded spelling, `review_core::encode_path`'s: a
    /// two-byte `é` takes two, a percent-encoded byte three. The same cases run through routing
    /// in `crate::project`'s tests and through the Surveyor in `tests/it/project_collect.rs`.
    const BYTE_CASES: &[(&str, &[u8], bool)] = &[
        ("caf?.rs", "café.rs".as_bytes(), false),
        ("caf??.rs", "café.rs".as_bytes(), true),
        ("caf?.rs", b"caf\xe9.rs", false),
        ("caf???.rs", b"caf\xe9.rs", true),
        ("50?.txt", b"50%.txt", false),
        ("50???.txt", b"50%.txt", true),
        ("caf*.rs", "café.rs".as_bytes(), true),
    ];

    #[test]
    fn a_question_mark_is_one_byte_of_the_encoded_spelling_in_the_surveyors_lists() {
        let policy = Policy {
            exclude: BYTE_CASES
                .iter()
                .map(|(pattern, _, _)| pattern.to_string())
                .collect(),
            ..Policy::default()
        };
        let _under = UnderDeadline::new(
            &policy,
            Instant::now() + Duration::from_secs(60),
            Arc::default(),
        );
        for (index, (pattern, raw, matches)) in BYTE_CASES.iter().enumerate() {
            let path = review_core::encode_path(raw);
            assert_eq!(
                any_match(&policy.exclude[index..=index], &path),
                *matches,
                "`{pattern}` against `{path}`"
            );
        }
        assert_eq!(review_core::encode_path(b"caf\xe9.rs"), "caf%E9.rs");
        assert_eq!(review_core::encode_path(b"50%.txt"), "50%25.txt");
    }

    #[test]
    fn a_measured_list_matches_nothing_past_its_deadline_and_says_so() {
        let policy = Policy {
            exclude: vec!["vendor/**".into()],
            ..Policy::default()
        };
        // A list no Survey measures matches unbounded.
        assert!(any_match(&policy.exclude, "vendor/a.c"));
        let matches = Arc::new(Matches::default());
        {
            let _under = UnderDeadline::new(
                &policy,
                Instant::now() - Duration::from_millis(1),
                Arc::clone(&matches),
            );
            assert!(!any_match(&policy.exclude, "vendor/a.c"));
            // An equal list of another policy is not this Survey's.
            let other = policy.exclude.clone();
            assert!(any_match(&other, "vendor/a.c"));
        }
        assert!(matches.cut());
        assert_eq!(matches.matched(), 0);
        assert!(
            any_match(&policy.exclude, "vendor/a.c"),
            "dropped, unbounded"
        );
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Ada")
            .env("GIT_AUTHOR_EMAIL", "ada@collect.invalid")
            .env("GIT_COMMITTER_NAME", "Ada")
            .env("GIT_COMMITTER_EMAIL", "ada@collect.invalid")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository of eight files in one commit.
    fn repository() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        for n in 0..8 {
            let path = repo.path().join(format!("src/f{n}.rs"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("fn f{n}() {{}}\n")).unwrap();
        }
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "Eight"]);
        repo
    }

    /// Measure `repo` under `policy` with a deadline `budget_ms` away, the deadline passing while
    /// the first pattern is matched: the hook sleeps past it. `None` when the deadline passed
    /// before any pattern was matched (a slow machine measured `identity` that long).
    fn measured_past_the_first_match(
        repo: &Path,
        policy: &Policy,
        budget_ms: u64,
    ) -> Option<(Sections, Arc<Matches>)> {
        let policy = &Policy {
            max_wall_ms: budget_ms,
            ..policy.clone()
        };
        let git = Git::open(repo).unwrap();
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let digest = format!("sha256:{}", "a".repeat(64));
        let basis = Basis {
            project_id: &digest,
            repository_id: "git:root:0000000000000000000000000000000000000000",
            head: &head,
            refs_digest: &digest,
            policy,
            policy_digest: &digest,
            cutoff: 1_800_000_000,
            surveyor_version: super::super::SURVEYOR_VERSION,
            git_version: "git version 2",
        };
        let started = Instant::now();
        let deadline = started + Duration::from_millis(budget_ms);
        let matches = Arc::new(Matches::default());
        *matches.before_match.lock().unwrap() = Some(Box::new(move |begun| {
            if begun == 1 {
                std::thread::sleep(
                    deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
                );
            }
        }));
        let sections = measure_counted(
            &git,
            &basis,
            started,
            super::super::measure::Clock::new(started, basis.policy.max_wall_ms),
            Arc::clone(&matches)).unwrap();
        (matches.matched() > 0).then_some((sections, matches))
    }

    /// The deadline passes during the first of 64 `exclude` patterns: the Surveyor matches no
    /// other pattern after it, `tree` is partial with reason `max_wall_ms` and every later
    /// section is unavailable.
    #[test]
    fn a_deadline_during_the_first_exclude_match_stops_the_list_and_leaves_tree_partial() {
        let repo = repository();
        let policy = Policy {
            exclude: (0..64).map(|n| format!("nothing{n}/**")).collect(),
            ..Policy::default()
        };
        let (sections, matches) = [1_500, 6_000, 30_000]
            .into_iter()
            .find_map(|budget| measured_past_the_first_match(repo.path(), &policy, budget))
            .expect("a deadline that passes during a match");
        assert_eq!(matches.matched(), 1, "one match in progress, none after");
        assert!(matches.cut());
        assert_eq!(sections.tree.completeness, SurveyCompletenessV1::Partial);
        assert_eq!(sections.tree.reason, Some(SurveyReasonV1::MaxWallMs));
        for (name, completeness) in [
            ("history", sections.history.completeness),
            ("contributors", sections.contributors.completeness),
            ("decisions", sections.decisions.completeness),
            ("vocabulary", sections.vocabulary.completeness),
            ("checks", sections.checks.completeness),
        ] {
            assert_eq!(completeness, SurveyCompletenessV1::Unavailable, "{name}");
        }
    }

    /// The deadline passes while `tests` are matched, after `tree` read its last item: `tree`
    /// is partial, never complete with a test split the deadline cut.
    #[test]
    fn a_deadline_during_the_tests_split_leaves_tree_partial() {
        let repo = repository();
        let policy = Policy {
            tests: (0..64).map(|n| format!("nothing{n}/**")).collect(),
            ..Policy::default()
        };
        let (sections, matches) = [1_500, 6_000, 30_000]
            .into_iter()
            .find_map(|budget| measured_past_the_first_match(repo.path(), &policy, budget))
            .expect("a deadline that passes during a match");
        assert_eq!(matches.matched(), 1, "one match in progress, none after");
        assert_eq!(sections.tree.completeness, SurveyCompletenessV1::Partial);
        assert_eq!(sections.tree.reason, Some(SurveyReasonV1::MaxWallMs));
        assert_eq!(
            sections.history.completeness,
            SurveyCompletenessV1::Unavailable
        );
    }

    /// A Project Store's CAS holding `bytes` as one object: the Store and the object's digest.
    fn store_with(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let store = std::fs::canonicalize(root.path()).unwrap().join("store");
        let survey_id = Cas::open(store.join("cas")).unwrap().put(bytes).unwrap();
        (root, store, survey_id)
    }

    #[test]
    fn a_record_is_reached_only_without_a_link() {
        let (root, store, survey_id) = store_with(b"{}");
        let record = locate(&store, &survey_id).unwrap().unwrap();
        assert!(record.bytes > 0);
        assert_eq!(record.read().unwrap(), b"{}");
        assert!(
            locate(&store, &format!("sha256:{}", "0".repeat(64)))
                .unwrap()
                .is_none()
        );
        // The prefix directory moved outside the Store, a link in its place.
        let object = object_path(&store, &survey_id);
        let prefix = object.parent().unwrap();
        let outside = std::fs::canonicalize(root.path()).unwrap().join("outside");
        std::fs::rename(prefix, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, prefix).unwrap();
        let error = locate(&store, &survey_id).err().unwrap();
        assert!(error.contains("prefix directory is refused"), "{error}");
        std::fs::remove_file(prefix).unwrap();
        std::fs::rename(&outside, prefix).unwrap();
        // The object itself a link.
        let moved = outside.with_extension("object");
        std::fs::rename(&object, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &object).unwrap();
        let error = locate(&store, &survey_id).err().unwrap();
        assert!(error.contains("symbolic link"), "{error}");
        assert!(moved.is_file());
        // `objects` a link.
        std::fs::remove_file(&object).unwrap();
        std::fs::rename(&moved, &object).unwrap();
        let objects = store.join("cas/objects");
        let away = outside.with_extension("objects");
        std::fs::rename(&objects, &away).unwrap();
        std::os::unix::fs::symlink(&away, &objects).unwrap();
        let error = locate(&store, &survey_id).err().unwrap();
        assert!(error.contains("objects directory is refused"), "{error}");
    }

    #[test]
    fn an_unlink_takes_only_the_file_that_was_found() {
        let (_root, store, survey_id) = store_with(b"one");
        let object = object_path(&store, &survey_id);
        let record = locate(&store, &survey_id).unwrap().unwrap();
        // Another file took the name since.
        std::fs::remove_file(&object).unwrap();
        std::fs::write(&object, b"another").unwrap();
        let error = record.unlink().unwrap_err();
        assert!(error.contains("changed since"), "{error}");
        assert!(object.is_file());
        let record = locate(&store, &survey_id).unwrap().unwrap();
        let bytes = record.bytes;
        assert_eq!(record.unlink().unwrap(), bytes);
        assert!(!object.exists());
    }

    #[test]
    fn corrupt_bytes_are_unreadable_with_a_bounded_reason_and_a_record_keeps_its_totals() {
        let digest = |bytes: &[u8]| review_store::canonical::blob_content_id(bytes);
        let garbage = b"not json at all";
        let error = totals(&digest(garbage), garbage).unwrap_err();
        assert!(error.starts_with("its bytes are not a Survey record: a syntax error"));
        assert!(error.len() <= MAX_UNREADABLE_BYTES);
        let error = totals(&digest(b"{}"), b"{}").unwrap_err();
        assert!(error.contains("a data error"), "{error}");
        assert!(!error.contains('`'), "{error}: no content of the record");
        let error = totals(&digest(b"{}"), b"{ }").unwrap_err();
        assert_eq!(error, "its bytes do not match its digest");
        let huge = vec![b' '; largest_record() as usize + 1];
        let error = totals(&digest(&huge), &huge).unwrap_err();
        assert!(error.contains("more than any Survey"), "{error}");
        let long = "é".repeat(200);
        let cut = bounded(long);
        assert!(cut.len() <= MAX_UNREADABLE_BYTES && cut.chars().all(|c| c == 'é'));
    }
}
