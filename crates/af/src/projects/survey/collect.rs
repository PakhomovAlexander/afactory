//! Survey collection (ADR-0159 clause 13) and the bounded patterns of clause 14.
//!
//! Every Survey is an entry of kind `survey` of the machine budget (ADR-0144 clause 4): its CAS
//! bytes, last used at its cutoff. The Storage Budget collects Surveys in its own passes only:
//! the collection pass takes the Surveys of a Project beyond the newest `[storage] keep_surveys`,
//! oldest first and idle at least `keep_days`, and the budget pass may take any Survey, oldest
//! first, while af is over `max_bytes`. Neither takes the newest complete Survey, a Survey of a
//! Project Store whose lock cannot be taken now (a running refresh, a plan, a lock file af may
//! not open) or one used within the hour, and `af project refresh` collects nothing.
//!
//! A sweep plans once (`crate::storage::plan`): [`look`] reads each Project Store once, without
//! its lock, into what the plan needs (its Surveys, the deletions an interruption left, the empty
//! containers of dead passes, the files it leaves), each object with the identity and bytes it
//! stated and each uncollected Survey's event verified against its record once, there and
//! nowhere else ([`Verdict`]); [`judge`] reads only that verdict and the record's name, refusing
//! a Survey whose name holds another inode than [`look`] stated, and the preview prints that
//! plan while the applied sweep runs each of its [`Job`]s with [`execute`], which compares what
//! it opens with the same identity and reports what it found instead of choosing again; an
//! object gone since the inventory is a failed entry, never a deletion of nothing. Held is decided by one predicate, [`lock_available`]. Collection
//! appends `SurveyCollected@1` to the Project Store's log before the record leaves the CAS. The
//! tombstones are a run of their own, `survey-collection`, beside the run `project` the
//! Surveyor appends its chain to: a `SurveyRecorded@1` a tombstone names replays as collected,
//! and `previous_survey_id` is a name, never a reachability edge.
//!
//! Two machine-local records beside the lock carry what the log cannot. `settled.json`
//! (`af/ProjectSurveysSettled@1`) lists the collected Surveys whose own records have left the
//! CAS, recorded under the lock once the inode off its name is checked to be the record and
//! before it is unlinked, so a file at such a name is `foreign` sweep after sweep, even when its
//! bytes hash to the Survey's ID. `pending-surveys/<64 hex>.json` (`af/survey-pending@1`,
//! [`super::pending`]) is the entry the Surveyor's writer keeps while it publishes: an entry
//! whose `SurveyRecorded@1` committed is cleared and nothing deleted, and otherwise only the
//! object it describes (its device, inode and length), at its Survey's CAS name or moved into a
//! dead pass's container by a recovery that was interrupted, is reclaimed, by recovery, before
//! the entry, which is cleared only once no file with that inode remains, whatever its length
//! (one changed in place is `foreign` and keeps the entry); any other object no event and no
//! entry names is `foreign` and never reclaimed. A settlement that cannot be made durable (the
//! directory's sync after the rename fails) puts the earlier `settled.json` back, synced, and
//! deletes nothing; when that fails too, the Survey is left an unverified settlement.
//!
//! [`look`] reads a Project Store's log under the Store discovery ceilings ([`Discovery`]), and
//! the inventory reads the Surveys of at most so many Project Stores.
//!
//! A record is reached, read and deleted through directory descriptors, ADR-0144's removal
//! boundary: the Project Store from `/`, then `cas`, `objects` and the digest-prefix directory,
//! each opened without following a link. The deletion names the inode it checks: every job
//! creates its own container `cas/removing/<pass-id>/` exclusively (mode 0700, a fresh random
//! ID), renames the record into it, and immediately before `unlinkat` states the name again with
//! `fstatat` relative to the container's descriptor; only the device and inode found before the
//! tombstone is unlinked, and the bytes reported are that inode's. Another file is moved back to
//! its name, or, when the name is taken again, renamed to `<survey-id>.replaced` in the
//! container, a name no pass ever deletes, and the Survey is `refused (replaced during
//! collection)`. A container left by a pass that died is settled by the next one the same way:
//! a file in it named by a tombstoned Survey is that Survey's record, moved, so it is renamed
//! into the new pass's container, checked and unlinked, and a file that took the Survey's CAS
//! name after the move is `foreign`: listed with its bytes, never the Survey's and never
//! deleted. A container's own name is checked as the names inside it are: only one named by a
//! pass ID is a dead pass's, and one that is not UTF-8 or no pass ID is left with everything in
//! it, even empty. Everything else under `cas/removing`, a name that is not UTF-8 included, is
//! walked with its raw name and left. A link anywhere on the way, or a directory on another device than
//! the Store, refuses the Survey, which is reported and keeps its record and its place in the
//! log. So does a record the file system will not let af read, and one over
//! [`MAX_SURVEY_BYTES`], the ceiling the Surveyor writes within, which is left as `oversize`; a
//! record within it whose bytes are no Survey is collected with an `unreadable` tombstone, since
//! reclaiming corrupt bytes is the budget's job. An interruption between such a tombstone and
//! the record's move leaves it at its name; the next sweep reclaims it by the object its
//! collection selected (device, inode, birth time and length) alone, since its bytes never hash
//! to its ID, and anything else at that name is `foreign`.
//!
//! The pattern rules of clause 14 live here too: a `[survey]` pattern is at most
//! [`MAX_PATTERN_BYTES`] and a list at most [`MAX_PATTERNS`], and the measurement's one pattern
//! loop checks the Survey's deadline before each match.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use review_core::EventType;
use review_core::project::{
    MAX_SURVEY_BYTES, MAX_SURVEYS_SETTLED_BYTES, PROJECT_SURVEYS_SETTLED_V1_SUPERSEDED,
    PROJECT_SURVEYS_SETTLED_V2, ProjectSurveyV2, ProjectSurveysSettledV2,
    SUPERSEDED_SURVEYS_SETTLED, SurveyCollectedHistoryV1, SurveyCollectedPayloadV1,
    SurveyCollectedTreeV1, SurveyCompletenessV1, SurveyPendingV2, SurveyReasonV1,
    SurveyRecordedPayloadV1, SurveySelectedV2,
};
use review_sandbox::removal::Kind;
use review_sandbox::{Identity, Stated};
use review_store::{Cas, EventStore, NewEvent};

use super::super::{Git, Log, ProjectStore, RUN_ID};
use super::measure::{Basis, Failure, Sections};
use super::policy::Policy;

/// The run of a Project Store's log that holds its Surveys' tombstones.
pub(crate) const COLLECTION_RUN: &str = "survey-collection";
/// The Store discovery ceilings (design note §3.3, ADR-0159 clause 13): where the inventory
/// walks Stores, it reads the Surveys of at most `stores` Project Stores, in path order, and
/// each Store's log through one cursor of at most `rows` rows of at most `row_bytes` bytes each
/// and `buffered_bytes` over all of them; beyond a ceiling the Store's Surveys are not read,
/// so they are kept, and the inventory reports the ceiling. The activity section's 64 MiB
/// per-Store CAS-read budget is PJ5's, with the section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Discovery {
    pub(crate) stores: usize,
    pub(crate) rows: u64,
    pub(crate) row_bytes: u64,
    pub(crate) buffered_bytes: u64,
}

/// The fixed discovery ceilings.
pub(crate) const DISCOVERY: Discovery = Discovery {
    stores: 1_000,
    rows: 100_000,
    row_bytes: 1 << 20,
    buffered_bytes: 16 << 20,
};

/// How a Project Store whose log is beyond a discovery ceiling is reported, before the ceiling.
const BEYOND_DISCOVERY: &str = "its log is beyond a Store discovery ceiling, so it is not read";

impl Discovery {
    /// The ceilings in force: [`DISCOVERY`], each of which a debug build's fixture may lower,
    /// never raise (`AF_TEST_SURVEY_DISCOVERY_STORES`, `_ROWS`, `_ROW_BYTES` and
    /// `_BUFFERED_BYTES`). Release binaries never read them.
    pub(crate) fn current() -> Self {
        #[allow(unused_mut)]
        let mut discovery = DISCOVERY;
        #[cfg(debug_assertions)]
        {
            let lowered = |name: &str, ceiling: u64| {
                std::env::var(format!("AF_TEST_SURVEY_DISCOVERY_{name}"))
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .map_or(ceiling, |value| value.min(ceiling))
            };
            discovery.stores = lowered("STORES", discovery.stores as u64) as usize;
            discovery.rows = lowered("ROWS", discovery.rows);
            discovery.row_bytes = lowered("ROW_BYTES", discovery.row_bytes);
            discovery.buffered_bytes = lowered("BUFFERED_BYTES", discovery.buffered_bytes);
        }
        discovery
    }

    /// One Store's read budget.
    fn budget(self) -> review_store::ReplayBudget {
        review_store::ReplayBudget::new(self.rows, self.row_bytes, self.buffered_bytes)
    }

    /// Why the Surveys of a Project Store beyond the first `stores` are not read.
    pub(crate) fn beyond_stores(self) -> String {
        format!(
            "its Surveys are not listed, so they are kept: the inventory reads the Surveys of at \
             most {} Project Stores (a Store discovery ceiling, max_stores)",
            self.stores
        )
    }
}

/// The lock every writer of a Project Store holds: a refresh while it reads and records, a plan
/// while it builds a Brief, and collection while it tombstones and deletes.
const LOCK: &str = "project.lock";
/// The directory of a Project Store's `cas` that holds the passes' containers.
const REMOVING: &str = "removing";
/// The record, beside the lock, of the collected Surveys whose records have left the CAS
/// (`af/ProjectSurveysSettled@1`).
pub(crate) const SETTLED: &str = "settled.json";
/// The directory, beside the lock, of the pending-publication entries the Surveyor's writer
/// keeps (`af/survey-pending@1`, ADR-0159 clause 2), one `<64 hex>.json` per Survey.
pub(crate) const PENDING: &str = super::pending::DIRECTORY;
/// Upper bound on one pending-publication entry collection reads: its Survey ID, two decimal
/// identities and a length fit many times over.
const MAX_PENDING_BYTES: u64 = 4096;
/// How an applied recovery refuses an object that is no longer the device, inode and length
/// the pending entry and the inventory stated: nothing is deleted.
const OBJECT_CHANGED: &str = "skipped: object changed";
const HOUR_MS: u64 = 3_600_000;
/// Upper bound on one `[survey]` pattern.
pub(crate) const MAX_PATTERN_BYTES: usize = 256;
/// Upper bound on the patterns of one `[survey]` list.
pub(crate) const MAX_PATTERNS: usize = 64;
/// Why the Surveys of a Project Store a refresh or a plan holds stay.
pub(crate) const HELD: &str = "a refresh or a plan holds its Project Store";
/// Why the Surveys of a Project Store whose lock af cannot open stay, before the error.
const LOCK_UNAVAILABLE: &str = "lock unavailable";
/// How a Survey whose record was replaced between its identity read and its unlink is refused.
const REPLACED: &str = "refused (replaced during collection)";
/// How a Survey whose record's name holds another file than inventory measured is refused: the
/// plan never adopts an inode inventory did not measure.
const REPLACED_SINCE_INVENTORY: &str = "refused (replaced since inventory)";
/// How a planned object that is not there any more fails: nothing of it is freed, and the
/// running total is unchanged by it.
const GONE: &str = "gone since the inventory";
/// What a file found in place of a record is renamed to in a container when its name is taken
/// again: a name that is no Survey's, so no pass ever deletes it.
const REPLACED_SUFFIX: &str = ".replaced";
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

/// A Project Store's `cas` and `objects` directories, reached through descriptors (ADR-0144
/// clause 3): the Store from `/`, then `cas` and `objects` below it, each opened
/// `O_NOFOLLOW | O_DIRECTORY` and on the Store's device.
struct CasDirectories {
    /// The Project Store itself: where `settled.json` and `pending/` live, beside the lock.
    store: OwnedFd,
    cas: OwnedFd,
    objects: OwnedFd,
    /// The device of the Project Store.
    device: u64,
}

fn device_of(descriptor: &OwnedFd) -> Result<u64, String> {
    Identity::of_descriptor(descriptor)
        .map(|identity| identity.device)
        .map_err(|error| format!("inspecting its CAS: {error}"))
}

fn open_cas(store: &Path) -> Result<CasDirectories, String> {
    let relative = store
        .strip_prefix("/")
        .map_err(|_| "its Project Store is not named by an absolute path".to_string())?;
    let root = review_sandbox::open_anchor(Path::new("/"))
        .map_err(|error| format!("opening `/`: {error}"))?;
    let directory = review_sandbox::open_beneath(&root, relative)
        .map_err(|error| format!("its Project Store cannot be reached without a link: {error}"))?;
    let cas = review_sandbox::open_beneath(&directory, Path::new("cas"))
        .map_err(|error| format!("its CAS objects directory is refused: {error}"))?;
    let objects = review_sandbox::open_beneath(&cas, Path::new("objects"))
        .map_err(|error| format!("its CAS objects directory is refused: {error}"))?;
    let device = device_of(&directory)?;
    if device_of(&cas)? != device || device_of(&objects)? != device {
        return Err("its CAS objects directory resolves outside its Project Store".into());
    }
    Ok(CasDirectories {
        store: directory,
        cas,
        objects,
        device,
    })
}

/// The hex of a Survey's ID split into its prefix directory and its object's name.
fn split(survey_id: &str) -> Result<(&str, &str), String> {
    survey_id
        .strip_prefix("sha256:")
        .filter(|_| review_core::is_digest(survey_id))
        .map(|hex| hex.split_at(2))
        .ok_or_else(|| format!("`{survey_id}` names no CAS object"))
}

/// The prefix directory `name` of `objects`, opened without following a link and on `device`;
/// `None` when there is none.
fn open_prefix(objects: &OwnedFd, name: &str, device: u64) -> Result<Option<OwnedFd>, String> {
    let prefix = match review_sandbox::open_beneath(objects, Path::new(name)) {
        Ok(prefix) => prefix,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("its CAS prefix directory is refused: {error}")),
    };
    if device_of(&prefix)? != device {
        return Err(format!(
            "its CAS prefix directory `{name}` resolves outside its Project Store"
        ));
    }
    Ok(Some(prefix))
}

fn kind_of(stat: &nix::sys::stat::FileStat) -> nix::sys::stat::SFlag {
    use nix::sys::stat::SFlag;
    SFlag::from_bits_truncate(stat.st_mode as nix::libc::mode_t) & SFlag::S_IFMT
}

/// What `name` of `directory` is, not following it, with its born identity (its device, inode
/// and birth time, `review_sandbox::born_at`); `None` when there is nothing. The name is taken
/// raw, so one that is not UTF-8 is stated as it is.
fn stat_at(directory: &OwnedFd, name: &OsStr) -> Result<Option<Stated>, String> {
    review_sandbox::born_at(directory, name)
        .map_err(|error| format!("inspecting its record: {error}"))
}

/// The born identity of `stated`, `shown` as what `directory` holds: refused, with one sentence
/// naming the file system, where that file system reports no birth time, since nothing could
/// then tell the file from one created later at a reused inode.
fn complete(stated: &Stated, shown: &str, directory: &OwnedFd) -> Result<Identity, String> {
    stated
        .complete(shown, directory)
        .map_err(|error| error.to_string())
}

/// Open `name` of `directory` to read, never following a link and never waiting on a pipe.
fn open_file(directory: &OwnedFd, name: &str) -> std::io::Result<std::fs::File> {
    use nix::fcntl::{OFlag, openat};
    openat(
        directory,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from)
}

/// At most one byte more than a Survey can hold of `file`.
fn read_bounded(file: std::fs::File) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_SURVEY_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Rename `from` of `from_directory` to `to` of `to_directory`, never over a name that is taken.
/// Both names are taken raw.
fn rename_free(
    from_directory: &OwnedFd,
    from: &OsStr,
    to_directory: &OwnedFd,
    to: &OsStr,
) -> Result<(), rustix::io::Errno> {
    rustix::fs::renameat_with(
        from_directory,
        from,
        to_directory,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
}

/// `cas/removing` of a Store, created when absent without following a link, opened
/// `O_NOFOLLOW | O_DIRECTORY` and on the Store's device; `None` when it is absent and `create`
/// is off.
fn open_removing(cas: &CasDirectories, create: bool) -> Result<Option<OwnedFd>, String> {
    if create {
        match nix::sys::stat::mkdirat(
            &cas.cas,
            REMOVING,
            nix::sys::stat::Mode::from_bits_truncate(0o700),
        ) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
            Err(errno) => return Err(format!("creating `cas/{REMOVING}`: {errno}")),
        }
    }
    let removing = match review_sandbox::open_beneath(&cas.cas, Path::new(REMOVING)) {
        Ok(removing) => removing,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "its CAS `{REMOVING}` directory is refused: {error}"
            ));
        }
    };
    if device_of(&removing)? != cas.device {
        return Err(format!(
            "its CAS `{REMOVING}` directory resolves outside its Project Store"
        ));
    }
    Ok(Some(removing))
}

/// A fresh random pass ID: 128 bits of `/dev/urandom` in hex.
fn pass_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("drawing a pass ID: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A regular file `name` of `directory`, opened without following a link and read up to `max`
/// bytes and one more; `None` when there is none. Anything else at the name is the error.
fn read_regular(directory: &OwnedFd, name: &str, max: u64) -> Result<Option<Vec<u8>>, String> {
    use nix::sys::stat::SFlag;
    let file = match open_file(directory, name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("`{name}` cannot be opened: {error}")),
    };
    let stat = nix::sys::stat::fstat(&file).map_err(|errno| format!("`{name}`: {errno}"))?;
    if kind_of(&stat) != SFlag::S_IFREG {
        return Err(format!("`{name}` is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("`{name}` cannot be read: {error}"))?;
    if bytes.len() as u64 > max {
        return Err(format!("`{name}` holds more than {max} bytes"));
    }
    Ok(Some(bytes))
}

/// What the Store's `settled.json` (`af/ProjectSurveysSettled@2`) says: the collected Surveys
/// whose own records have left the CAS, and, per collected Survey, the object its collection
/// selected before its tombstone, by identity and length.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Settled {
    survey_ids: BTreeSet<String>,
    selected: BTreeMap<String, (Identity, u64)>,
}

impl Settled {
    /// Whether `identity`, `size` bytes long, is the object the collection of `survey_id`
    /// selected: the one inode recovery may delete as its record.
    fn selects(&self, survey_id: &str, identity: Identity, size: u64) -> bool {
        self.selected.get(survey_id) == Some(&(identity, size))
    }
}

/// What the Store's `settled.json` says; nothing when there is no such file. A file at a settled
/// Survey's CAS name is foreign, whatever its bytes, and so is any file at a collected Survey's
/// name, or in a dead pass's container, that is not the object its collection selected. A
/// record another release wrote is refused by its label, naming the recovery.
fn read_settled(cas: &CasDirectories) -> Result<Settled, String> {
    let Some(bytes) = read_regular(&cas.store, SETTLED, MAX_SURVEYS_SETTLED_BYTES)? else {
        return Ok(Settled::default());
    };
    if review_core::project::schema_label(&bytes).as_deref()
        == Some(PROJECT_SURVEYS_SETTLED_V1_SUPERSEDED)
    {
        return Err(format!(
            "`{SETTLED}` is refused: {SUPERSEDED_SURVEYS_SETTLED}; delete it, and since no \
             collection's selection is then recorded, nothing at a collected Survey's name is \
             deleted"
        ));
    }
    let record: ProjectSurveysSettledV2 = serde_json::from_slice(&bytes)
        .map_err(|error| format!("`{SETTLED}` is no {PROJECT_SURVEYS_SETTLED_V2}: {error}"))?;
    record
        .validate()
        .map_err(|why| format!("`{SETTLED}`: {why}"))?;
    Ok(Settled {
        survey_ids: record.survey_ids.into_iter().collect(),
        selected: record
            .selected
            .into_iter()
            .map(|selected| {
                (
                    selected.survey_id,
                    (
                        super::pending::identity_of(&selected.object),
                        selected.bytes.get(),
                    ),
                )
            })
            .collect(),
    })
}

/// Record, under the Store's lock its caller holds, that the record of the collected Survey
/// `survey_id` has left its CAS name: `settled.json` rewritten whole,
/// through a fresh temporary file created exclusively beside it and renamed over it, so a reader
/// sees the old record or the new one, never part of one, and the Store's directory synced so
/// the rename is durable. Any failure is the error, and the caller then unlinks nothing: a sync
/// that fails after the rename puts the earlier record back (removes the new one, when there was
/// none) and syncs that too before it returns, so the Survey stays unsettled and the next sweep
/// tries again; when the put-back or its sync fails as well, both failures are the error and the
/// Survey is left [`UNVERIFIED_SETTLEMENT`]: its record is not deleted, and only a later sweep
/// that reads a consistent `settled.json` decides what holds its name.
fn record_settled(cas: &CasDirectories, survey_id: &str) -> Result<(), String> {
    record_settled_with(cas, survey_id, sync_store)
}

/// Record, under the Store's lock its caller holds and before the tombstone of `survey_id` is
/// appended, the object its collection selected at its CAS name, `identity` and `size` bytes
/// long: the only inode recovery deletes as its record from then on. Written as
/// [`record_settled`] writes; a failure, its sync's included (the earlier record is put back),
/// is the error, and the caller then appends no tombstone.
fn record_selected(
    cas: &CasDirectories,
    survey_id: &str,
    identity: Identity,
    size: u64,
) -> Result<(), String> {
    // A settlement's injected sync failure is a settlement's alone: the selection syncs as is.
    record_selected_with(cas, survey_id, identity, size, |cas, _| {
        nix::unistd::fsync(&cas.store).map_err(|errno| errno.to_string())
    })
}

fn record_selected_with(
    cas: &CasDirectories,
    survey_id: &str,
    identity: Identity,
    size: u64,
    sync: impl Fn(&CasDirectories, SyncOf) -> Result<(), String>,
) -> Result<(), String> {
    let changed = |settled: &mut Settled| {
        settled
            .selected
            .insert(survey_id.to_string(), (identity, size))
            != Some((identity, size))
    };
    update_settled(cas, changed, sync).map_err(|failure| match failure {
        Update::Failed(why) => format!("recording its selection: {why}"),
        Update::PutBack(why) => format!(
            "syncing `{SETTLED}`: {why}; the earlier record is put back and synced, so no \
             tombstone is appended and the next pass tries again"
        ),
        Update::Unverified(why, again) => format!(
            "syncing `{SETTLED}`: {why}; putting the earlier record back failed too ({again}), \
             so no tombstone is appended, and a later pass reads `{SETTLED}` again before it \
             collects the Survey"
        ),
    })
}

/// What a Survey whose settlement could neither be made durable nor durably undone is left as.
const UNVERIFIED_SETTLEMENT: &str = "unverified settlement";

/// Which sync of the Project Store's directory a settlement makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncOf {
    /// The one after the new `settled.json` was renamed into place.
    Install,
    /// The one after a failed install's put-back.
    Rollback,
}

/// Sync the Project Store's directory, which holds `settled.json`. A debug build's fixture may
/// make it fail, as a directory the file system will not sync does:
/// `AF_TEST_SURVEY_FAIL_SETTLED_SYNC=1` fails the install's sync alone, `=2` the put-back's
/// too. Release binaries never read it.
fn sync_store(cas: &CasDirectories, of: SyncOf) -> Result<(), String> {
    #[cfg(debug_assertions)]
    if let Some(value) = std::env::var_os("AF_TEST_SURVEY_FAIL_SETTLED_SYNC")
        && (value == "2" || (value == "1" && of == SyncOf::Install))
    {
        return Err("an injected failure (AF_TEST_SURVEY_FAIL_SETTLED_SYNC)".into());
    }
    #[cfg(not(debug_assertions))]
    let _ = of;
    nix::unistd::fsync(&cas.store).map_err(|errno| errno.to_string())
}

fn record_settled_with(
    cas: &CasDirectories,
    survey_id: &str,
    sync: impl Fn(&CasDirectories, SyncOf) -> Result<(), String>,
) -> Result<(), String> {
    let changed = |settled: &mut Settled| settled.survey_ids.insert(survey_id.to_string());
    update_settled(cas, changed, sync).map_err(|failure| match failure {
        Update::Failed(why) => why,
        Update::PutBack(why) => format!(
            "syncing `{SETTLED}`: {why}; the earlier record is put back and synced, so the Survey \
             stays unsettled and the next pass tries again"
        ),
        Update::Unverified(why, again) => format!(
            "syncing `{SETTLED}`: {why}; putting the earlier record back failed too ({again}), \
             so the Survey is left {UNVERIFIED_SETTLEMENT}: its record is not deleted, and a \
             later pass reads `{SETTLED}` again before anything at its name is"
        ),
    })
}

/// How an update of `settled.json` failed.
enum Update {
    /// Before or at its install: nothing changed.
    Failed(String),
    /// Its install's sync, after which the earlier record was put back and synced.
    PutBack(String),
    /// Its install's sync, and the put-back or its sync too.
    Unverified(String, String),
}

/// Apply `change` to `settled.json` and install the result durably, unless it changed nothing:
/// the new record written through a fresh temporary file renamed over the old one, then the
/// Store's directory synced; a sync that fails puts the earlier record back (removes the new one,
/// when there was none) and syncs that too.
fn update_settled(
    cas: &CasDirectories,
    change: impl FnOnce(&mut Settled) -> bool,
    sync: impl Fn(&CasDirectories, SyncOf) -> Result<(), String>,
) -> Result<(), Update> {
    let before = read_settled(cas).map_err(Update::Failed)?;
    let mut settled = before.clone();
    if !change(&mut settled) {
        return Ok(());
    }
    write_settled(cas, &settled).map_err(Update::Failed)?;
    let Err(why) = sync(cas, SyncOf::Install) else {
        return Ok(());
    };
    // The new record is in place but not durable: the earlier one goes back, durably, so no
    // reader takes the Survey for settled, or the object for selected, on a record that may
    // not survive.
    let restored = if before == Settled::default() {
        nix::unistd::unlinkat(&cas.store, SETTLED, nix::unistd::UnlinkatFlags::NoRemoveDir)
            .map_err(|errno| format!("removing the new record: {errno}"))
    } else {
        write_settled(cas, &before)
    }
    .and_then(|()| {
        sync(cas, SyncOf::Rollback).map_err(|again| format!("syncing the put-back: {again}"))
    });
    Err(match restored {
        Ok(()) => Update::PutBack(why),
        Err(again) => Update::Unverified(why, again),
    })
}

/// Replace `settled.json` with `settled`, through a temporary file synced and renamed over it.
fn write_settled(cas: &CasDirectories, settled: &Settled) -> Result<(), String> {
    use nix::fcntl::{OFlag, openat};
    use std::io::Write;
    let record = ProjectSurveysSettledV2 {
        schema: PROJECT_SURVEYS_SETTLED_V2.into(),
        survey_ids: settled.survey_ids.iter().cloned().collect(),
        selected: settled
            .selected
            .iter()
            .map(|(survey_id, &(identity, size))| {
                super::pending::object_of(identity)
                    .map(|object| SurveySelectedV2 {
                        survey_id: survey_id.clone(),
                        object,
                        bytes: size.into(),
                    })
                    .ok_or("a selection carries its object's birth time")
            })
            .collect::<Result<_, _>>()?,
    };
    record.validate()?;
    let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_SURVEYS_SETTLED_BYTES {
        return Err(format!(
            "`{SETTLED}` would hold more than {MAX_SURVEYS_SETTLED_BYTES} bytes"
        ));
    }
    let temporary = format!(".{SETTLED}.{}", pass_id()?);
    let written = openat(
        &cas.store,
        temporary.as_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .map_err(std::io::Error::from)
    .map(std::fs::File::from)
    .and_then(|mut file| {
        file.write_all(&bytes)?;
        file.sync_all()
    })
    .map_err(|error| error.to_string())
    .and_then(|()| {
        rustix::fs::renameat(&cas.store, temporary.as_str(), &cas.store, SETTLED)
            .map_err(|errno| errno.to_string())
    });
    if let Err(why) = written {
        let _ = nix::unistd::unlinkat(
            &cas.store,
            temporary.as_str(),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        );
        return Err(format!("writing `{SETTLED}`: {why}"));
    }
    Ok(())
}

/// The container one pass deletes in: `cas/removing/<pass-id>/`, created by this pass alone
/// (`mkdirat` with mode 0700 under a fresh random ID, refused when the name exists), opened
/// `O_NOFOLLOW | O_DIRECTORY` and on the Store's device. Only this pass renames into it, so
/// between the check before an unlink and the unlink only a process of the same user can
/// replace what it holds: the trust boundary delivery's rollback accepts (ADR-0031).
struct Container {
    /// `cas/removing`.
    removing: OwnedFd,
    /// The container.
    directory: OwnedFd,
    /// Its name under `cas/removing`.
    name: String,
}

/// What the check before an unlink found.
#[derive(Debug)]
enum Checked {
    /// Another inode holds the name.
    Replaced,
    /// Nothing holds it.
    Gone,
    /// The check or the unlink failed.
    Failed(String),
    /// The check found the inode, but what must precede its unlink failed: nothing was
    /// unlinked.
    Unready(String),
    /// What holds the name has no birth time to compare, its file system reporting none:
    /// nothing was unlinked.
    Incomplete(String),
}

impl Container {
    fn create(cas: &CasDirectories) -> Result<Self, String> {
        let removing = open_removing(cas, true)?.ok_or("no `cas/removing`")?;
        let name = pass_id()?;
        nix::sys::stat::mkdirat(
            &removing,
            name.as_str(),
            nix::sys::stat::Mode::from_bits_truncate(0o700),
        )
        .map_err(|errno| format!("creating its pass container `cas/{REMOVING}/{name}`: {errno}"))?;
        let directory = review_sandbox::open_beneath(&removing, Path::new(&name))
            .map_err(|error| format!("opening its pass container: {error}"))?;
        if device_of(&directory)? != cas.device {
            return Err("its pass container resolves outside its Project Store".into());
        }
        Ok(Self {
            removing,
            directory,
            name,
        })
    }

    /// How `entry` of the container is named in a message.
    fn label(&self, entry: &OsStr) -> String {
        format!("cas/{REMOVING}/{}/{}", self.name, entry.to_string_lossy())
    }

    /// Unlink `entry` only while it is `identity`, its device, inode and birth time: stated
    /// with `fstatat` relative to the container's own descriptor, without following it,
    /// immediately before `unlinkat` relative to the same descriptor, and only once `ready`
    /// succeeded after that check (a collected Survey's settlement is recorded there, once the
    /// inode off its name is known to be its record). A file born later at a reused inode is
    /// another file. Returns the allocated bytes of that inode.
    fn unlink_checked(
        &self,
        entry: &OsStr,
        identity: Identity,
        ready: impl FnOnce() -> Result<(), String>,
    ) -> Result<u64, Checked> {
        use nix::unistd::{UnlinkatFlags, unlinkat};
        let stated = match stat_at(&self.directory, entry) {
            Ok(Some(stated)) => stated,
            Ok(None) => return Err(Checked::Gone),
            Err(why) => return Err(Checked::Failed(why)),
        };
        if stated.kind != Kind::File {
            return Err(Checked::Replaced);
        }
        if complete(&stated, &self.label(entry), &self.directory).map_err(Checked::Incomplete)?
            != identity
        {
            return Err(Checked::Replaced);
        }
        ready().map_err(Checked::Unready)?;
        unlinkat(&self.directory, entry, UnlinkatFlags::NoRemoveDir)
            .map_err(|errno| Checked::Failed(errno.to_string()))?;
        Ok(stated.allocated)
    }

    /// Set aside `entry`, which the check found replaced: back to `name` of `directory` when that
    /// is free, otherwise renamed to `<entry>.replaced` here, which no pass deletes. Says where
    /// it went.
    fn set_aside(&self, entry: &OsStr, directory: &OwnedFd, name: &OsStr) -> String {
        if rename_free(&self.directory, entry, directory, name).is_ok() {
            return "that file is put back and nothing is deleted".into();
        }
        let mut aside = entry.to_os_string();
        aside.push(REPLACED_SUFFIX);
        match rename_free(&self.directory, entry, &self.directory, &aside) {
            Ok(()) => format!(
                "its name is taken again, so that file is left as `{}` and nothing is deleted",
                self.label(&aside)
            ),
            Err(errno) => format!(
                "its name is taken again ({errno}), so that file is left as `{}` and nothing is \
                 deleted",
                self.label(entry)
            ),
        }
    }

    /// Remove the container when it is empty; one that still holds a file stays for the next
    /// pass to report.
    fn close(self) {
        let _ = nix::unistd::unlinkat(
            &self.removing,
            self.name.as_str(),
            nix::unistd::UnlinkatFlags::RemoveDir,
        );
    }
}

/// Where in a removal a test may act: just before which step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// The rename, after the identity was read and the tombstone appended.
    Rename,
    /// The check that precedes the unlink, after the rename into the pass's container.
    Check,
    /// Setting aside the other file the check found.
    MoveBack,
}

/// One Survey's record, reached through directory descriptors (ADR-0144 clause 3) and stated
/// without following it: the identity its deletion is checked against. It is read and renamed
/// relative to its prefix directory, never through a path.
struct Record {
    cas: CasDirectories,
    /// The digest-prefix directory.
    prefix: OwnedFd,
    /// The object's name in it.
    name: String,
    /// The Survey whose record it is.
    survey_id: String,
    /// Its path, for a debug build's fixture.
    object: PathBuf,
    /// The object as it was stated.
    identity: Identity,
    /// Its length as it was stated.
    size: u64,
    /// Allocated bytes of the object.
    bytes: u64,
}

/// The record `survey_id` of the Project Store at `store`; `None` when the CAS does not hold it.
/// A symbolic link in place of the Store's path, `cas`, `objects`, the prefix directory or the
/// object, a directory on another device than the Store's, or an object that is no regular file
/// is the error: the record is never reached through it.
fn locate(store: &Path, survey_id: &str) -> Result<Option<Record>, String> {
    let (prefix_name, name) = split(survey_id)?;
    let cas = open_cas(store)?;
    let Some(prefix) = open_prefix(&cas.objects, prefix_name, cas.device)? else {
        return Ok(None);
    };
    let Some(stated) = stat_at(&prefix, OsStr::new(name))? else {
        return Ok(None);
    };
    match stated.kind {
        Kind::File => {}
        Kind::Symlink => {
            return Err(
                "its record is a symbolic link; af never reads or removes through one".into(),
            );
        }
        _ => return Err("its record is not a regular file".into()),
    }
    let object = object_path(store, survey_id);
    let identity = complete(
        &stated,
        &format!("its record {}", object.display()),
        &prefix,
    )?;
    Ok(Some(Record {
        cas,
        prefix,
        name: name.to_string(),
        survey_id: survey_id.to_string(),
        object,
        identity,
        size: stated.size,
        bytes: stated.allocated,
    }))
}

/// How a record of `size` bytes is refused: more than a Survey can hold.
fn oversize(size: u64) -> String {
    format!(
        "refused (oversize, {size} bytes): its record holds more than the {MAX_SURVEY_BYTES} \
         bytes a Survey can hold, so it is left and adds no tombstone"
    )
}

/// A debug build's fixture may replace a record as another process could, between its identity
/// read and its rename (`AF_TEST_SURVEY_REPLACE_BEFORE_RENAME=<directory>`) or between inventory
/// and the plan's judgement of it (`AF_TEST_SURVEY_REPLACE_BEFORE_JUDGE=<directory>`): the
/// record moves into that directory and a byte-identical copy, another inode, takes its name.
/// Release binaries never read either.
fn replace_object(variable: &str, object: &Path) {
    #[cfg(debug_assertions)]
    if let Some(aside) = std::env::var_os(variable) {
        let (Ok(bytes), Some(name)) = (std::fs::read(object), object.file_name()) else {
            return;
        };
        if std::fs::rename(object, Path::new(&aside).join(name)).is_ok() {
            let _ = std::fs::write(object, bytes);
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = (variable, object);
}

impl Record {
    /// How it is refused when its stated length is over what a Survey can hold.
    fn oversize(&self) -> Option<String> {
        (self.size > MAX_SURVEY_BYTES).then(|| oversize(self.size))
    }

    /// The record's bytes, read from the object opened relative to its prefix directory without
    /// following a link, the same file [`locate`] stated; at most one byte beyond
    /// [`MAX_SURVEY_BYTES`].
    fn read(&self) -> std::io::Result<Vec<u8>> {
        let file = open_file(&self.prefix, &self.name)?;
        if review_sandbox::born_of_descriptor(&file)?.identity != self.identity {
            return Err(std::io::Error::other(
                "the record changed since it was found",
            ));
        }
        read_bounded(file)
    }

    /// Whether its bytes are the record of its Survey: within [`MAX_SURVEY_BYTES`] and hashing
    /// to its ID. A collected Survey's CAS name holding anything else holds somebody else's file.
    fn is_the_record(&self) -> std::io::Result<bool> {
        if self.oversize().is_some() {
            return Ok(false);
        }
        let bytes = self.read()?;
        Ok(bytes.len() as u64 <= MAX_SURVEY_BYTES
            && review_store::canonical::blob_content_id(&bytes) == self.survey_id)
    }

    /// Delete the record [`locate`] stated, and nothing else: it is renamed into a container of
    /// this pass's own ([`Container`]) and unlinked there only after `fstatat` relative to the
    /// container finds the device and inode that was stated. Returns the allocated bytes of the
    /// inode it deleted. Another file is set aside ([`Container::set_aside`]) and the deletion
    /// is [`REPLACED`]; a record gone before its unlink is [`GONE`], never a deletion of nothing.
    /// With `settle`, the record of a collected Survey, the Store's `settled.json` records that
    /// it left its CAS name once it is in the container and before it is unlinked; when that
    /// cannot be recorded it is put back and nothing is deleted.
    fn remove(self, settle: bool) -> Result<u64, String> {
        let object = self.object.clone();
        self.remove_with(settle, |step, _| match step {
            Before::Rename => replace_object("AF_TEST_SURVEY_REPLACE_BEFORE_RENAME", &object),
            Before::Check => stop_before_unlink(),
            Before::MoveBack => {}
        })
    }

    fn remove_with(self, settle: bool, mut hook: impl FnMut(Before, &Path)) -> Result<u64, String> {
        let container = Container::create(&self.cas)?;
        let entry = OsStr::new(self.survey_id.as_str());
        let name = OsStr::new(self.name.as_str());
        let inside = self
            .object
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(|cas| cas.join(REMOVING).join(&container.name).join(entry))
            .unwrap_or_default();
        hook(Before::Rename, &inside);
        match rename_free(&self.prefix, name, &container.directory, entry) {
            Ok(()) => {}
            // Gone since it was found: nothing of it is freed, and the entry says so.
            Err(rustix::io::Errno::NOENT) => {
                container.close();
                return Err(format!("{GONE}, so nothing of it is freed"));
            }
            Err(errno) => {
                container.close();
                return Err(format!("moving its record aside: {errno}"));
            }
        }
        hook(Before::Check, &inside);
        // Once the inode off its name is known to be the record, and before it is unlinked,
        // the record has left the CAS: from then on whatever takes that name is somebody
        // else's, sweep after sweep.
        let ready = || {
            if settle {
                record_settled(&self.cas, &self.survey_id)
            } else {
                Ok(())
            }
        };
        let removed = match container.unlink_checked(entry, self.identity, ready) {
            Ok(bytes) => Ok(bytes),
            Err(Checked::Unready(why)) => Err(format!(
                "recording that its record left the CAS: {why}; {}",
                if rename_free(&container.directory, entry, &self.prefix, name).is_ok() {
                    "its record is put back and nothing is deleted"
                } else {
                    "its record stays in its pass's container for the next pass"
                }
            )),
            Err(Checked::Incomplete(why)) => Err(format!(
                "{why}; {}",
                if rename_free(&container.directory, entry, &self.prefix, name).is_ok() {
                    "its record is put back and nothing is deleted"
                } else {
                    "its record stays in its pass's container for the next pass"
                }
            )),
            Err(Checked::Gone) => Err(format!("{GONE}, so nothing of it is freed")),
            Err(Checked::Failed(why)) => Err(format!(
                "deleting its record from `{}`: {why}; the next pass finishes it",
                container.label(entry)
            )),
            Err(Checked::Replaced) => {
                hook(Before::MoveBack, &inside);
                Err(format!(
                    "{REPLACED}: another file took its record's name since it was found; {}",
                    container.set_aside(entry, &self.prefix, name)
                ))
            }
        };
        container.close();
        removed
    }
}

/// Why the plan leaves a file or a Survey it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeftReason {
    /// Its Project Store's lock cannot be taken now.
    Held,
    /// Not a Survey's record: a file at a collected Survey's CAS name after its record was
    /// moved, or anything under `cas/removing` no tombstone names or no pass container holds.
    /// Never deleted.
    Foreign,
    /// A record over [`MAX_SURVEY_BYTES`].
    Oversize,
    /// A name under `cas/removing` that is not UTF-8, a file's or its container's, so no
    /// Survey's and no pass's.
    UnreadableName,
    /// A directory nested deeper than the removal walk reaches
    /// (`review_sandbox::REMOVAL_MAX_DEPTH`): any removal of it would leave part of it, so the
    /// inventory leaves all of it, in the preview as in the applied sweep.
    TooDeep,
    /// A claim a dead process left that the inventory's walk could not measure: nothing could
    /// say what its removal freed, so every plan leaves it, with why.
    Unmeasurable,
}

impl LeftReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            LeftReason::Held => "held",
            LeftReason::Foreign => "foreign",
            LeftReason::Oversize => "oversize",
            LeftReason::UnreadableName => "unreadable_name",
            LeftReason::TooDeep => "too_deep",
            LeftReason::Unmeasurable => "unmeasurable",
        }
    }
}

/// One file or Survey the plan leaves where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Left {
    /// Its path, raw: a name under `cas/removing` may be no UTF-8.
    pub(crate) path: PathBuf,
    /// Its allocated bytes, counted in the Store's total and never as a Survey's.
    pub(crate) bytes: u64,
    pub(crate) reason: LeftReason,
    /// Why, in af's words.
    pub(crate) detail: String,
}

/// What the Store's total counts a pending deletion's bytes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Counted {
    /// The Survey's own entry, its record at its CAS name.
    Survey,
    /// The Project Store's entry: a record moved under `cas/removing`.
    Store,
}

/// One deletion or collection an applied sweep runs with [`execute`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Job {
    /// Tombstone a Survey and delete its record, which was stated as `identity` (`None`: the
    /// CAS did not hold it, so it is collected `unreadable`).
    Take {
        store: PathBuf,
        survey_id: String,
        identity: Option<Identity>,
    },
    /// Delete the record a tombstoned Survey still holds at its CAS name, the object its
    /// collection selected. `unreadable`: its tombstone says its bytes were no Survey, so they
    /// are never compared with its ID; otherwise they must still hash to it.
    AtName {
        store: PathBuf,
        survey_id: String,
        identity: Identity,
        unreadable: bool,
    },
    /// Delete a tombstoned Survey's record a dead pass left in its container.
    Moved {
        store: PathBuf,
        container: OsString,
        survey_id: String,
        identity: Identity,
    },
    /// Remove a container a dead pass left, stated as `identity`: one it left empty, or one
    /// whose every file is a moved record planned before it, once those are settled.
    Container {
        store: PathBuf,
        container: OsString,
        identity: Identity,
    },
    /// Delete the object at a Survey's CAS name whose `SurveyRecorded@1` never committed, stated
    /// as `identity` and `length`, the device, inode and length its pending-publication entry
    /// recorded.
    Unrecorded {
        store: PathBuf,
        survey_id: String,
        identity: Identity,
        length: u64,
    },
    /// Delete the object of a Survey whose `SurveyRecorded@1` never committed that a dead pass
    /// moved into its `container` before it could unlink it, stated as `identity` and `length`,
    /// the device, inode and length its pending-publication entry still records.
    MovedUnrecorded {
        store: PathBuf,
        container: OsString,
        survey_id: String,
        identity: Identity,
        length: u64,
    },
    /// Clear the pending-publication entry of a Survey, stated as `identity`: its event
    /// committed, or nothing the entry describes is left to reclaim.
    Publication {
        store: PathBuf,
        survey_id: String,
        identity: Identity,
    },
    /// Delete the object the writer staged for a Survey in its `pending-surveys`, stated as
    /// `identity` and `length`: bytes a crash left before or after the entry named them, never
    /// at their CAS name.
    Staged {
        store: PathBuf,
        survey_id: String,
        identity: Identity,
        length: u64,
    },
}

impl Job {
    /// The Project Store the job works in.
    pub(crate) fn store(&self) -> &Path {
        match self {
            Job::Take { store, .. }
            | Job::AtName { store, .. }
            | Job::Moved { store, .. }
            | Job::Container { store, .. }
            | Job::Unrecorded { store, .. }
            | Job::MovedUnrecorded { store, .. }
            | Job::Publication { store, .. }
            | Job::Staged { store, .. } => store,
        }
    }
}

/// A deletion an interruption left: the plan's `recovery`, with what inventory measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    /// The Survey's CAS object, as the removal names it; for [`Job::Container`], the container.
    pub(crate) object: PathBuf,
    /// The allocated bytes it frees: those of the inode inventory stated.
    pub(crate) bytes: u64,
    pub(crate) counted: Counted,
    pub(crate) job: Job,
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
    /// The record as it was stated: what its deletion is checked against.
    pub(crate) identity: Option<Identity>,
    /// An object its pending-publication entry describes whose `SurveyRecorded@1` never
    /// committed: in use, and reclaimed only by recovery.
    pub(crate) pending: bool,
}

/// What [`look`]'s one read of an uncollected Survey's record found: its `SurveyRecorded@1`
/// verified against it once, by the fold's own verifier
/// ([`super::TombstoneIdentity::of_record`]). The plan's judgement and the inventory read
/// nothing else of the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The event agrees with its record on every field clause 2 compares.
    Agrees,
    /// The event names a `field` its record does not record: refused, preview and pass alike,
    /// and kept.
    Disagrees(&'static str),
    /// No record vouches for anything (its bytes are no Survey, or the CAS does not hold it):
    /// collected `unreadable`, with why.
    Unreadable(String),
    /// A record over [`MAX_SURVEY_BYTES`]: left `oversize`, with why.
    Oversize(String),
    /// A record that cannot be reached without a link or that af may not read: refused, with
    /// why, beginning `refused`.
    Refused(String),
}

/// An uncollected Survey of the chain, oldest first: what `keep_surveys` counts, with what
/// [`look`] verified of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Live {
    pub(crate) survey_id: String,
    /// Its cutoff as its record vouches for it (0 when a record that disagrees with its event
    /// records none), never the time an event its record disproves names; the event's only
    /// when no record vouches for anything, as an `unreadable` tombstone takes it.
    pub(crate) cutoff_ms: u64,
    /// The newest complete Survey by the same verified fields: never collected.
    pub(crate) protected: bool,
    /// Its record as [`look`] stated it; `None` when the CAS does not hold it or it cannot be
    /// reached.
    pub(crate) identity: Option<Identity>,
    pub(crate) verdict: Verdict,
}

/// One Project Store as one read without its lock finds it: everything a sweep's plan needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Look {
    /// Its Surveys whose records its CAS holds at their names, oldest first; a tombstoned one
    /// only while no record of it was moved.
    pub(crate) surveys: Vec<Inventoried>,
    /// Why its lock cannot be taken now, by [`lock_available`].
    pub(crate) held: Option<String>,
    /// Its uncollected Surveys, oldest first.
    pub(crate) live: Vec<Live>,
    /// The deletions an interruption left, in the order the tombstones were appended, then the
    /// containers of dead passes whose every file is one of those moved records, then the ones
    /// that hold nothing.
    pub(crate) recovery: Vec<Pending>,
    /// What it leaves: foreign files and unreadable names.
    pub(crate) left: Vec<Left>,
    /// What cannot be judged, with why: a failure of the sweep.
    pub(crate) refused: Vec<(PathBuf, String)>,
}

/// What a file under `cas/removing` is, as stated without following it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoundKind {
    File,
    Directory,
    Other,
}

/// One name under `cas/removing`: in a container (`container` its raw name), or directly below.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    container: Option<OsString>,
    name: OsString,
    kind: FoundKind,
    /// Its device, inode and birth time; no birth time where its file system reports none, so
    /// it equals no identity af recorded.
    identity: Identity,
    /// Its allocated bytes as the Storage Budget counts them: a regular file's, nothing for a
    /// directory or anything else.
    bytes: u64,
    /// Its length.
    size: u64,
}

/// What a dead pass moved into its container: a tombstoned Survey's record, or (`pending`) the
/// object of a Survey whose `SurveyRecorded@1` never committed that its pending-publication
/// entry still describes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Moved {
    container: OsString,
    identity: Identity,
    bytes: u64,
    size: u64,
    pending: bool,
}

/// Every name `directory` holds, raw, sorted, `.` and `..` left out.
fn names_in(directory: &OwnedFd) -> Result<Vec<OsString>, String> {
    let mut names = Vec::new();
    let listing = rustix::fs::Dir::read_from(directory).map_err(|errno| errno.to_string())?;
    for entry in listing {
        let entry = entry.map_err(|errno| errno.to_string())?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        if name != "." && name != ".." {
            names.push(name.to_os_string());
        }
    }
    names.sort();
    Ok(names)
}

fn found(
    directory: &OwnedFd,
    container: Option<&OsStr>,
    name: OsString,
) -> Result<Option<Found>, String> {
    let Some(stated) = stat_at(directory, &name)? else {
        return Ok(None);
    };
    Ok(Some(Found {
        container: container.map(OsStr::to_os_string),
        kind: match stated.kind {
            Kind::File => FoundKind::File,
            Kind::Directory => FoundKind::Directory,
            Kind::Symlink | Kind::Other => FoundKind::Other,
        },
        identity: stated.identity,
        bytes: stated.counted(),
        size: stated.size,
        name,
    }))
}

/// Whether `name` is a pass container's: the 32 lowercase hex digits of [`pass_id`].
fn is_pass_id(name: &OsStr) -> bool {
    name.len() == 32
        && name
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

/// A container a dead pass left, as the walk stated it: its name, identity and allocated bytes,
/// and whether it holds nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PassContainer {
    name: OsString,
    identity: Identity,
    bytes: u64,
    empty: bool,
}

/// Walk `cas/removing` with raw names: every name in it, and in each directory below it (a
/// container) every name there, plus every pass container, with whether it holds nothing. A
/// directory whose own name is no pass ID (not UTF-8, or not [`pass_id`]'s pattern) is no
/// pass's container: what it holds is walked and judged like any leftover, and, when it holds
/// nothing, it is itself a leftover, never an empty container to remove. Reads only.
fn walk_removing(cas: &CasDirectories) -> Result<(Vec<Found>, Vec<PassContainer>), String> {
    let Some(removing) = open_removing(cas, false)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let listing = |error: String| format!("listing `cas/{REMOVING}`: {error}");
    let mut all = Vec::new();
    let mut empty = Vec::new();
    for name in names_in(&removing).map_err(listing)? {
        let Some(top) = found(&removing, None, name.clone()).map_err(listing)? else {
            continue;
        };
        if top.kind != FoundKind::Directory {
            all.push(top);
            continue;
        }
        let container = review_sandbox::open_beneath(&removing, Path::new(&name))
            .map_err(|error| listing(format!("opening a container: {error}")))?;
        if device_of(&container)? != cas.device {
            return Err(format!(
                "a container under `cas/{REMOVING}` resolves outside its Project Store"
            ));
        }
        let inside = names_in(&container).map_err(listing)?;
        if is_pass_id(&name) {
            empty.push(PassContainer {
                name: name.clone(),
                identity: top.identity,
                bytes: top.bytes,
                empty: inside.is_empty(),
            });
        } else if inside.is_empty() {
            all.push(top);
        }
        for entry in inside {
            if let Some(file) = found(&container, Some(&name), entry).map_err(listing)? {
                all.push(file);
            }
        }
    }
    Ok((all, empty))
}

/// Judge what [`walk_removing`] found, with raw names and nothing read: a regular file in a pass
/// container named by a tombstoned Survey is that Survey's record, moved, only while it is the
/// object its collection selected (`selected`: its device, inode, birth time and length); one
/// named by a Survey whose pending-publication entry describes it (`pending`, the same way) is
/// that entry's object, moved by a recovery that died before its unlink; everything else is
/// left, a byte-identical copy at another inode included. The container's own name is checked
/// as the file's is: a name of either that is not UTF-8 is `unreadable name`, and a container
/// whose name is no pass ID holds only `foreign` files, whatever they are named.
fn judge_leftovers(
    store: &Path,
    found: Vec<Found>,
    collected: &BTreeSet<String>,
    selected: &BTreeMap<String, (Identity, u64)>,
    pending: &BTreeMap<String, (Identity, u64)>,
) -> (BTreeMap<String, Moved>, Vec<Left>) {
    let mut moved = BTreeMap::new();
    let mut left = Vec::new();
    for file in found {
        let mut path = store.join("cas").join(REMOVING);
        if let Some(container) = &file.container {
            path.push(container);
        }
        path.push(&file.name);
        let mut leave = |reason: LeftReason, why: &str| {
            left.push(Left {
                path: path.clone(),
                bytes: if file.kind == FoundKind::File {
                    file.bytes
                } else {
                    0
                },
                reason,
                detail: format!(
                    "what is under `cas/{REMOVING}` as `{}` is left: {why}",
                    path.strip_prefix(store.join("cas").join(REMOVING))
                        .unwrap_or(&path)
                        .to_string_lossy()
                ),
            });
        };
        if file
            .container
            .as_ref()
            .is_some_and(|container| container.to_str().is_none())
        {
            leave(
                LeftReason::UnreadableName,
                "the name of its container is not UTF-8, so it is no pass's container",
            );
            continue;
        }
        let Some(name) = file.name.to_str() else {
            leave(
                LeftReason::UnreadableName,
                "its name is not UTF-8, so it names no Survey",
            );
            continue;
        };
        let Some(container) = file.container.clone() else {
            leave(LeftReason::Foreign, "it is no pass container");
            continue;
        };
        if !is_pass_id(&container) {
            leave(
                LeftReason::Foreign,
                "the name of its container is no pass ID, so it is no pass's container",
            );
            continue;
        }
        if file.kind != FoundKind::File {
            leave(LeftReason::Foreign, "it is not a regular file");
            continue;
        }
        if split(name).is_err() {
            leave(LeftReason::Foreign, "it names no Survey, so it is not af's");
            continue;
        }
        let is = |object: Option<&(Identity, u64)>| {
            object.is_some_and(|&(identity, size)| file.identity == identity && file.size == size)
        };
        let chosen = collected.contains(name) && is(selected.get(name));
        let described = is(pending.get(name));
        if !chosen && !described {
            leave(
                LeftReason::Foreign,
                if collected.contains(name) {
                    "it is not the object the collection of its Survey selected (another inode, \
                     birth time or length), so it is not that Survey's record"
                } else {
                    "no tombstone names its Survey and no pending-publication entry describes it"
                },
            );
            continue;
        }
        if moved.contains_key(name) {
            leave(
                LeftReason::Foreign,
                "an earlier container holds the record of its Survey",
            );
            continue;
        }
        moved.insert(
            name.to_string(),
            Moved {
                container,
                identity: file.identity,
                bytes: file.bytes,
                size: file.size,
                pending: !chosen,
            },
        );
    }
    (moved, left)
}

/// Read the Project Store at `store` once, without its lock, for a sweep's plan and for `af
/// storage`: its Surveys at their CAS names (a record that cannot be reached without a link is
/// listed in use with nothing measured through the link, and so is one over
/// [`MAX_SURVEY_BYTES`] and one whose event its record disproves), whether its lock is held, its
/// uncollected Surveys, each record read and its event verified against it once here
/// ([`verify`]), and what an interruption left: per tombstoned Survey, in tombstone order, its
/// record moved into a dead pass's container (a file that took its CAS name since is
/// `foreign`), otherwise its record still at its name, which is its record only when it is the
/// object its collection selected, its record has not left the CAS already ([`read_settled`])
/// and its bytes hash to its ID, or, for a Survey collected `unreadable`, by that selection alone
/// (any other file there is `foreign`); then the objects of unrecorded Surveys a dead pass moved into its
/// container that their pending-publication entries still describe ([`pending_entries`], read
/// before the leftovers are judged); then each dead pass's container whose every file is a
/// moved record or object planned before it, and each one that holds nothing; then the
/// pending-publication entries ([`pending_publications`]), and every CAS object nothing names,
/// `foreign` ([`unreferenced`]). Every one is recorded with the identity and bytes stated here, which
/// every later step compares against. Writes nothing but the lock file every holder of the
/// Store creates.
pub(crate) fn look(store: &Path) -> Result<Look, String> {
    look_within(store, Discovery::current())
}

/// [`look`] with its log read under `discovery`'s row ceilings, both runs through one budget:
/// a log beyond one is the error, naming the ceiling, and nothing of the Store is planned.
fn look_within(store: &Path, discovery: Discovery) -> Result<Look, String> {
    let events = EventStore::open_read_only(store.join("events.sqlite"))
        .map_err(|error| error.to_string())?;
    let mut budget = discovery.budget();
    let mut replay = |run: &str| {
        events
            .replay_within(run, &mut budget)
            .map_err(|error| error.to_string())?
            .map_err(|ceiling| format!("{BEYOND_DISCOVERY}: {ceiling}"))
    };
    let log = Log::fold(&replay(RUN_ID)?)?;
    let tombstoned = tombstones_of(replay(COLLECTION_RUN)?)?;
    // The Surveys collected `unreadable`: their records' bytes were no Survey, so they never
    // hash to their IDs, and their selection alone names them.
    let unreadable: BTreeSet<String> = tombstoned
        .iter()
        .filter(|tombstone| tombstone.unreadable.is_some())
        .map(|tombstone| tombstone.survey_id.clone())
        .collect();
    let order: Vec<String> = tombstoned
        .into_iter()
        .map(|tombstone| tombstone.survey_id)
        .collect();
    let collected: BTreeSet<String> = order.iter().cloned().collect();
    let mut look = Look {
        held: lock_available(store).err(),
        ..Look::default()
    };
    let cas = open_cas(store);
    // The collected Surveys whose records have left the CAS, and the object each collection
    // selected. One that cannot be read settles every collected Survey and selects nothing: a
    // file at such a name is then left, never deleted as a record.
    let Settled {
        survey_ids: settled,
        selected,
    } = match cas.as_ref().map_err(String::clone).and_then(read_settled) {
        Ok(settled) => settled,
        Err(why) => {
            look.refused.push((
                store.join(SETTLED),
                format!(
                    "the record of which collected Surveys left the CAS cannot be read, so no \
                     file at a collected Survey's name is deleted: {why}"
                ),
            ));
            Settled {
                survey_ids: collected.clone(),
                selected: BTreeMap::new(),
            }
        }
    };
    let walked = cas.as_ref().map_err(String::clone).and_then(walk_removing);
    let (found, containers) = walked.unwrap_or_else(|why| {
        look.refused.push((
            store.join("cas").join(REMOVING),
            format!("what is under `cas/{REMOVING}` is left: {why}"),
        ));
        (Vec::new(), Vec::new())
    });
    // A dead pass's container goes only when everything in it is a moved record the plan
    // settles before it; one that holds anything else stays with what it holds.
    let mut holding: BTreeMap<OsString, usize> = BTreeMap::new();
    for file in &found {
        if let Some(container) = &file.container {
            *holding.entry(container.clone()).or_default() += 1;
        }
    }
    // The writer's pending-publication entries, read before what is under `cas/removing` is
    // judged: an object an entry of an unrecorded Survey describes that a dead pass moved into
    // its container is that entry's object, recovered again, never foreign.
    let chain = oldest_first(&log);
    let recorded: BTreeSet<&str> = chain
        .iter()
        .map(|recorded| recorded.survey_id.as_str())
        .collect();
    let (entries, publications, staged) = match &cas {
        Ok(cas) => pending_entries(store, cas, &mut look),
        Err(_) => (Vec::new(), BTreeSet::new(), Vec::new()),
    };
    let unrecorded: BTreeMap<String, (Identity, u64)> = entries
        .iter()
        .filter(|pending| !recorded.contains(pending.survey_id.as_str()))
        .filter_map(|pending| {
            described(&pending.entry).map(|object| (pending.survey_id.clone(), object))
        })
        .collect();
    let (moved, left) = judge_leftovers(store, found, &collected, &selected, &unrecorded);
    look.left.extend(left);
    // What each collected Survey left, in the order the tombstones were appended: its record
    // moved into a dead pass's container, or its record at its name, each only while it is the
    // object its collection selected before the tombstone (its device, inode, birth time and
    // length) and, at its name, its bytes are the record's (or its tombstone says they were no
    // Survey, `unreadable`, and the selection alone names it). Any other file is somebody else's,
    // durably, even when its bytes hash to the Survey's ID: listed `foreign`, never the
    // Survey's, never deleted.
    let mut not_records = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for survey_id in &order {
        if !seen.insert(survey_id) {
            continue;
        }
        let object = object_path(store, survey_id);
        let foreign = |bytes: u64, why: String| Left {
            path: object.clone(),
            bytes,
            reason: LeftReason::Foreign,
            detail: format!(
                "a file at the CAS name of collected Survey {survey_id} {why}, so it is not the \
                 Survey's and is left"
            ),
        };
        let at_name = locate(store, survey_id);
        if let Some(record) = moved.get(survey_id) {
            look.recovery.push(Pending {
                object: object.clone(),
                bytes: record.bytes,
                counted: Counted::Store,
                job: Job::Moved {
                    store: store.to_path_buf(),
                    container: record.container.clone(),
                    survey_id: survey_id.clone(),
                    identity: record.identity,
                },
            });
            if let Ok(Some(file)) = at_name {
                not_records.insert(survey_id.clone());
                look.left.push(foreign(
                    file.bytes,
                    format!(
                        "took it after its record was moved into `cas/{REMOVING}/{}`",
                        record.container.to_string_lossy()
                    ),
                ));
            }
            continue;
        }
        match at_name {
            Ok(None) => {}
            // Its record left the CAS already: whatever holds its name since is somebody
            // else's, even when its bytes hash to the Survey's ID.
            Ok(Some(record)) if settled.contains(survey_id) => {
                not_records.insert(survey_id.clone());
                look.left.push(foreign(
                    record.bytes,
                    "took it after its record left the CAS".into(),
                ));
            }
            // Not the object its collection selected, or none was selected (its record was
            // gone when it was collected): another inode, whatever its bytes.
            Ok(Some(record)) if !selected_matches(&selected, survey_id, &record) => {
                not_records.insert(survey_id.clone());
                look.left.push(foreign(
                    record.bytes,
                    "is not the object the collection of that Survey selected (another inode, \
                     birth time or length)"
                        .into(),
                ));
            }
            // Collected `unreadable` (its bytes did not match its ID or did not parse): the
            // object its collection selected, by device, inode, birth time and length, is its
            // record whatever its bytes, since no digest of them could ever name it.
            Ok(Some(record)) if unreadable.contains(survey_id) => look.recovery.push(Pending {
                object,
                bytes: record.bytes,
                counted: Counted::Survey,
                job: Job::AtName {
                    store: store.to_path_buf(),
                    survey_id: survey_id.clone(),
                    identity: record.identity,
                    unreadable: true,
                },
            }),
            Ok(Some(record)) => match record.is_the_record() {
                Ok(true) => look.recovery.push(Pending {
                    object,
                    bytes: record.bytes,
                    counted: Counted::Survey,
                    job: Job::AtName {
                        store: store.to_path_buf(),
                        survey_id: survey_id.clone(),
                        identity: record.identity,
                        unreadable: false,
                    },
                }),
                Ok(false) => {
                    not_records.insert(survey_id.clone());
                    look.left.push(foreign(
                        record.bytes,
                        "holds bytes that are not its record's".into(),
                    ));
                }
                Err(error) => look.refused.push((
                    object,
                    format!(
                        "the record of collected Survey {survey_id} is left: refused: it cannot \
                         be read: {error}"
                    ),
                )),
            },
            Err(why) => look.refused.push((
                object,
                format!("the record of collected Survey {survey_id} is left: refused: {why}"),
            )),
        }
    }
    // The objects of unrecorded Surveys a dead pass moved before it could unlink them: each
    // still the device, inode and length its entry describes, recovered as a collected record
    // moved is, but never settled, since no tombstone names it.
    for (survey_id, record) in moved.iter().filter(|(_, record)| record.pending) {
        look.recovery.push(Pending {
            object: object_path(store, survey_id),
            bytes: record.bytes,
            counted: Counted::Store,
            job: Job::MovedUnrecorded {
                store: store.to_path_buf(),
                container: record.container.clone(),
                survey_id: survey_id.clone(),
                identity: record.identity,
                length: record.size,
            },
        });
    }
    // The containers of dead passes: planned like every other removal, after the records they
    // hold, never removed beside the plan. One that held records goes once they are settled;
    // one that holds nothing goes as it was stated.
    let mut planned_moves: BTreeMap<&OsStr, usize> = BTreeMap::new();
    for record in moved.values() {
        *planned_moves
            .entry(record.container.as_os_str())
            .or_default() += 1;
    }
    let (emptied, empty): (Vec<&PassContainer>, Vec<&PassContainer>) = containers
        .iter()
        .filter(|container| {
            container.empty
                || planned_moves.get(container.name.as_os_str()).copied()
                    == holding.get(&container.name).copied()
        })
        .partition(|container| !container.empty);
    for container in emptied.into_iter().chain(empty) {
        look.recovery.push(Pending {
            object: store.join("cas").join(REMOVING).join(&container.name),
            bytes: container.bytes,
            counted: Counted::Store,
            job: Job::Container {
                store: store.to_path_buf(),
                container: container.name.clone(),
                identity: container.identity,
            },
        });
    }
    // Every uncollected Survey once, its record read and its event verified against it here and
    // nowhere else; what the inventory and the plan know of it is what this read verified.
    pending_publications(store, entries, staged, &recorded, &mut look);
    let verified: Vec<(&SurveyRecordedPayloadV1, Verified)> = chain
        .iter()
        .filter(|recorded| !collected.contains(&recorded.survey_id))
        .map(|recorded| (*recorded, verify(store, recorded)))
        .collect();
    let protected = verified
        .iter()
        .rev()
        .find(|(recorded, verified)| {
            verified.completeness(recorded) == SurveyCompletenessV1::Complete
        })
        .map(|(recorded, _)| recorded.survey_id.clone());
    look.live = verified
        .iter()
        .map(|(recorded, verified)| Live {
            survey_id: recorded.survey_id.clone(),
            cutoff_ms: verified.cutoff_ms(recorded),
            protected: protected.as_deref() == Some(recorded.survey_id.as_str()),
            identity: verified.identity,
            verdict: verified.verdict.clone(),
        })
        .collect();
    let live: BTreeMap<&str, (&Live, &Verified)> = look
        .live
        .iter()
        .zip(verified.iter().map(|(_, verified)| verified))
        .map(|(live, verified)| (live.survey_id.as_str(), (live, verified)))
        .collect();
    for recorded in chain.iter().copied() {
        let survey_id = recorded.survey_id.as_str();
        if moved.contains_key(survey_id) || not_records.contains(survey_id) {
            continue;
        }
        let (bytes, identity, last_use_unix_ms, refused) = match live.get(survey_id) {
            // A collected Survey whose record is still at its name: its deletion is recovery.
            None => match locate(store, survey_id) {
                Ok(None) => continue,
                Ok(Some(record)) => (
                    record.bytes,
                    Some(record.identity),
                    cutoff_ms(recorded),
                    record.oversize(),
                ),
                Err(reason) => (0, None, 0, Some(format!("its record is left: {reason}"))),
            },
            Some((live, found)) => {
                if !found.present {
                    continue;
                }
                let refused = match &live.verdict {
                    Verdict::Agrees | Verdict::Unreadable(_) => None,
                    Verdict::Oversize(why) => Some(why.clone()),
                    Verdict::Refused(why) => Some(format!(
                        "its record is left: {}",
                        why.strip_prefix("refused: ").unwrap_or(why)
                    )),
                    Verdict::Disagrees(field) => Some(disagrees(field)),
                };
                (found.bytes, live.identity, live.cutoff_ms, refused)
            }
        };
        let in_use = if refused.is_some() {
            refused
        } else if look.held.is_some() {
            look.held.clone()
        } else if !collected.contains(survey_id) && protected.as_deref() == Some(survey_id) {
            Some(NEWEST_COMPLETE.to_string())
        } else {
            None
        };
        look.surveys.push(Inventoried {
            object: object_path(store, survey_id),
            bytes,
            last_use_unix_ms,
            in_use,
            identity,
            pending: false,
        });
    }
    let known: BTreeSet<&str> = chain
        .iter()
        .map(|recorded| recorded.survey_id.as_str())
        .chain(collected.iter().map(String::as_str))
        .chain(publications.iter().map(String::as_str))
        .collect();
    if let Ok(cas) = &cas {
        unreferenced(store, cas, &known, &mut look);
    }
    Ok(look)
}

/// Why an object its pending-publication entry describes is in use: only recovery takes it.
const PENDING_USE: &str =
    "pending: its SurveyRecorded@1 never committed, so recovery reclaims it through its entry";

/// Whether `record`, at a collected Survey's CAS name, is the object the collection of
/// `survey_id` selected before its tombstone: its device, inode, birth time and length.
fn selected_matches(
    selected: &BTreeMap<String, (Identity, u64)>,
    survey_id: &str,
    record: &Record,
) -> bool {
    selected.get(survey_id) == Some(&(record.identity, record.size))
}

/// The object a pending-publication entry describes, once the writer staged it: its device,
/// inode and birth time, and the length of the record's bytes.
fn described(entry: &SurveyPendingV2) -> Option<(Identity, u64)> {
    entry
        .object
        .map(|object| (super::pending::identity_of(&object), entry.bytes.get()))
}

/// A pending-publication entry `name` of `directory`, stated as `identity`: read without
/// following a link, from the inode stated, within [`MAX_PENDING_BYTES`], valid as the writer's
/// `af/survey-pending@2` (a superseded `@1` refused by its label) and naming the Survey its file
/// name names. Returns it with when it was last written, in Unix milliseconds.
fn read_publication(
    directory: &OwnedFd,
    name: &str,
    survey_id: &str,
    identity: Identity,
) -> Result<(SurveyPendingV2, u64), String> {
    let file = open_file(directory, name).map_err(|error| error.to_string())?;
    if review_sandbox::born_of_descriptor(&file)
        .map_err(|error| error.to_string())?
        .identity
        != identity
    {
        return Err("it changed since it was found".into());
    }
    let stat = nix::sys::stat::fstat(&file).map_err(|errno| errno.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_PENDING_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_PENDING_BYTES {
        return Err(format!("it holds more than {MAX_PENDING_BYTES} bytes"));
    }
    let entry = super::pending::parse(&bytes, Path::new(name))?;
    if entry.survey_id != survey_id {
        return Err(format!("it names Survey {}", entry.survey_id));
    }
    let written_unix_ms = u64::try_from(stat.st_mtime)
        .unwrap_or(0)
        .saturating_mul(1000)
        .saturating_add(u64::try_from(stat.st_mtime_nsec).unwrap_or(0) / 1_000_000);
    Ok((entry, written_unix_ms))
}

/// The Survey a pending-publication entry's file name names: `<64 hex>.json`.
fn publication_survey(name: &OsStr) -> Option<String> {
    let hex = name.to_str()?.strip_suffix(".json")?;
    let survey_id = format!("sha256:{hex}");
    review_core::is_digest(&survey_id).then_some(survey_id)
}

/// The Survey the name of an object the writer staged names: `<64 hex>.object`.
fn staged_survey(name: &OsStr) -> Option<String> {
    let hex = name.to_str()?.strip_suffix(super::pending::STAGED_SUFFIX)?;
    let survey_id = format!("sha256:{hex}");
    review_core::is_digest(&survey_id).then_some(survey_id)
}

/// One pending-publication entry as [`look`] read it.
struct PendingEntry {
    survey_id: String,
    /// Its file.
    path: PathBuf,
    /// Its file as it was stated, and its allocated bytes.
    identity: Identity,
    bytes: u64,
    /// When the writer last wrote it, in Unix milliseconds.
    written_unix_ms: u64,
    entry: SurveyPendingV2,
}

/// One object the writer staged, `pending-surveys/<64 hex>.object`, as [`look`] stated it.
struct StagedObject {
    survey_id: String,
    path: PathBuf,
    identity: Identity,
    size: u64,
    bytes: u64,
}

/// The pending-publication entries of the Project Store at `store` (ADR-0159 clauses 2 and
/// 13), read once with the rest of [`look`] from `pending-surveys/`: each readable one, the
/// Surveys every entry names, a readable one or not, and every object the writer staged there.
/// What is neither is left `foreign`, an entry that cannot be read is refused and left with what
/// it names, and a staged object whose file system reports no birth time is refused and left.
fn pending_entries(
    store: &Path,
    cas: &CasDirectories,
    look: &mut Look,
) -> (Vec<PendingEntry>, BTreeSet<String>, Vec<StagedObject>) {
    let mut entries = Vec::new();
    let mut named = BTreeSet::new();
    let mut staged = Vec::new();
    let directory = match review_sandbox::open_beneath(&cas.store, Path::new(PENDING)) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (entries, named, staged);
        }
        Err(error) => {
            look.refused.push((
                store.join(PENDING),
                format!("the pending-publication entries are left: refused: {error}"),
            ));
            return (entries, named, staged);
        }
    };
    let names = match device_of(&directory).and_then(|device| {
        if device == cas.device {
            names_in(&directory)
        } else {
            Err("it resolves outside its Project Store".into())
        }
    }) {
        Ok(names) => names,
        Err(why) => {
            look.refused.push((
                store.join(PENDING),
                format!("the pending-publication entries are left: refused: {why}"),
            ));
            return (entries, named, staged);
        }
    };
    for name in names {
        let path = store.join(PENDING).join(&name);
        let file = match found(&directory, None, name.clone()) {
            Ok(Some(file)) => file,
            Ok(None) => continue,
            Err(why) => {
                look.refused.push((path, format!("refused: {why}")));
                continue;
            }
        };
        if let Some(survey_id) = staged_survey(&name).filter(|_| file.kind == FoundKind::File) {
            if file.identity.born.is_none() {
                look.refused.push((
                    path.clone(),
                    format!(
                        "the object the writer staged for Survey {survey_id} is left: refused: \
                         it is on a file system that reports no birth time, so af cannot tell it \
                         from a file created later at the same inode"
                    ),
                ));
                continue;
            }
            named.insert(survey_id.clone());
            staged.push(StagedObject {
                survey_id,
                path,
                identity: file.identity,
                size: file.size,
                bytes: file.bytes,
            });
            continue;
        }
        let survey_id = match publication_survey(&name) {
            Some(survey_id) if file.kind == FoundKind::File => survey_id,
            _ => {
                look.left.push(Left {
                    bytes: if file.kind == FoundKind::File {
                        file.bytes
                    } else {
                        0
                    },
                    reason: LeftReason::Foreign,
                    detail: format!(
                        "what is under `{PENDING}` as `{}` is left: it is no pending-publication \
                         entry",
                        name.to_string_lossy()
                    ),
                    path,
                });
                continue;
            }
        };
        named.insert(survey_id.clone());
        match read_publication(
            &directory,
            name.to_str().unwrap_or_default(),
            &survey_id,
            file.identity,
        ) {
            Ok((entry, written_unix_ms)) => entries.push(PendingEntry {
                survey_id,
                path,
                identity: file.identity,
                bytes: file.bytes,
                written_unix_ms,
                entry,
            }),
            Err(why) => look.refused.push((
                path,
                format!(
                    "the pending-publication entry of Survey {survey_id} and what it names are \
                     left: refused: it cannot be read: {why}"
                ),
            )),
        }
    }
    (entries, named, staged)
}

/// Judge the pending-publication entries and the staged objects [`pending_entries`] read
/// (ADR-0159 clause 13). Every staged object is reclaimed first, as the writer's own leftover
/// (no writer holds the lock between a sweep's look and its jobs, which take it): the bytes a
/// crash staged before its entry named them, or the staged object an entry names whose CAS name
/// never took it. Then each entry whose `SurveyRecorded@1` committed is cleared, and nothing
/// deleted; for each other one, only the object at its Survey's CAS name that is the object it
/// describes (its device, inode, birth time and length) is counted, as a Survey `pending`, and
/// reclaimed by recovery before the entry is cleared, and any other file there is `foreign`. An
/// object it describes that a dead pass moved into its container was planned with the dead
/// passes' records (`moved`), so its entry too is cleared only after it. While the Store is held
/// every one is the writer's and is left `held`.
fn pending_publications(
    store: &Path,
    entries: Vec<PendingEntry>,
    staged: Vec<StagedObject>,
    recorded: &BTreeSet<&str>,
    look: &mut Look,
) {
    for object in staged {
        look.recovery.push(Pending {
            object: object.path,
            bytes: object.bytes,
            counted: Counted::Store,
            job: Job::Staged {
                store: store.to_path_buf(),
                survey_id: object.survey_id,
                identity: object.identity,
                length: object.size,
            },
        });
    }
    for pending in entries {
        let survey_id = pending.survey_id;
        let clear = Pending {
            object: pending.path,
            bytes: pending.bytes,
            counted: Counted::Store,
            job: Job::Publication {
                store: store.to_path_buf(),
                survey_id: survey_id.clone(),
                identity: pending.identity,
            },
        };
        if recorded.contains(survey_id.as_str()) {
            look.recovery.push(clear);
            continue;
        }
        let object = object_path(store, &survey_id);
        match locate(store, &survey_id) {
            Err(why) => {
                look.refused.push((
                    object,
                    format!("the unrecorded Survey {survey_id} is left: refused: {why}"),
                ));
                continue;
            }
            Ok(None) => {}
            Ok(Some(record)) => {
                if described(&pending.entry) == Some((record.identity, record.size))
                    || look.held.is_some()
                {
                    look.surveys.push(Inventoried {
                        object: object.clone(),
                        bytes: record.bytes,
                        last_use_unix_ms: pending.written_unix_ms,
                        in_use: Some(look.held.clone().unwrap_or_else(|| PENDING_USE.into())),
                        identity: Some(record.identity),
                        pending: true,
                    });
                    look.recovery.push(Pending {
                        object,
                        bytes: record.bytes,
                        counted: Counted::Survey,
                        job: Job::Unrecorded {
                            store: store.to_path_buf(),
                            survey_id: survey_id.clone(),
                            identity: record.identity,
                            length: record.size,
                        },
                    });
                } else {
                    look.left.push(Left {
                        path: object,
                        bytes: record.bytes,
                        reason: LeftReason::Foreign,
                        detail: format!(
                            "a file at the CAS name of unrecorded Survey {survey_id} is not the \
                             object its pending-publication entry describes, so it is left"
                        ),
                    });
                }
            }
        }
        look.recovery.push(clear);
    }
}

/// Every object of the Store's CAS no `SurveyRecorded@1`, tombstone or pending-publication
/// entry names (`known`): `foreign`, listed with its bytes, counted in the Store's total and
/// never reclaimed, whatever its bytes. Names that are no digest (a CAS write's temporary
/// file) are the CAS's own and are not listed.
fn unreferenced(store: &Path, cas: &CasDirectories, known: &BTreeSet<&str>, look: &mut Look) {
    let prefixes = match names_in(&cas.objects) {
        Ok(prefixes) => prefixes,
        Err(why) => {
            look.refused.push((
                store.join("cas").join("objects"),
                format!("its CAS objects cannot be listed: {why}"),
            ));
            return;
        }
    };
    for prefix in prefixes {
        let Some(prefix) = prefix.to_str().filter(|prefix| {
            prefix.len() == 2
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) else {
            continue;
        };
        let directory = match open_prefix(&cas.objects, prefix, cas.device) {
            Ok(Some(directory)) => directory,
            Ok(None) => continue,
            Err(why) => {
                look.refused
                    .push((store.join("cas").join("objects").join(prefix), why));
                continue;
            }
        };
        let Ok(names) = names_in(&directory) else {
            continue;
        };
        for name in names {
            let Some(survey_id) = name
                .to_str()
                .map(|rest| format!("sha256:{prefix}{rest}"))
                .filter(|survey_id| review_core::is_digest(survey_id))
            else {
                continue;
            };
            if known.contains(survey_id.as_str()) {
                continue;
            }
            let Ok(Some(file)) = found(&directory, None, name) else {
                continue;
            };
            look.left.push(Left {
                path: object_path(store, &survey_id),
                bytes: if file.kind == FoundKind::File {
                    file.bytes
                } else {
                    0
                },
                reason: LeftReason::Foreign,
                detail: format!(
                    "an object at the CAS name {survey_id}, which no SurveyRecorded@1, tombstone \
                     or pending-publication entry names, is left: it is no Survey af recorded"
                ),
            });
        }
    }
}

/// How a Survey whose event its record disproves is refused and kept, preview and pass alike.
fn disagrees(field: &str) -> String {
    format!(
        "refused: its SurveyRecorded@1 names a {field} its record does not record, so it is kept"
    )
}

/// What one read of an uncollected Survey's record found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Verified {
    /// Whether the CAS holds something at its name, a link included.
    present: bool,
    identity: Option<Identity>,
    bytes: u64,
    verdict: Verdict,
    /// The cutoff, in Unix seconds, and the completeness its record vouches for, when it can:
    /// its bytes are its record's and parse (the cutoff `None` when the record holds none).
    vouched: Option<(Option<u64>, SurveyCompletenessV1)>,
}

impl Verified {
    /// Its cutoff in Unix milliseconds, as a verified field says it.
    fn cutoff_ms(&self, recorded: &SurveyRecordedPayloadV1) -> u64 {
        match self.vouched {
            Some((cutoff_s, _)) => cutoff_s.unwrap_or(0).saturating_mul(1000),
            None => cutoff_ms(recorded),
        }
    }

    /// Its completeness, the same way.
    fn completeness(&self, recorded: &SurveyRecordedPayloadV1) -> SurveyCompletenessV1 {
        self.vouched
            .map_or(recorded.completeness, |(_, completeness)| completeness)
    }
}

/// Read the record of the uncollected Survey `recorded` names once, through descriptors, and
/// verify the event against it with the fold's own verifier: the one place the plan's
/// judgement and the inventory learn whether the event agrees with its record.
fn verify(store: &Path, recorded: &SurveyRecordedPayloadV1) -> Verified {
    let survey_id = recorded.survey_id.as_str();
    let mut verified = Verified {
        present: false,
        identity: None,
        bytes: 0,
        verdict: Verdict::Unreadable("its record is not in the CAS".into()),
        vouched: None,
    };
    let record = match locate(store, survey_id) {
        Ok(Some(record)) => record,
        Ok(None) => return verified,
        Err(why) => {
            verified.present = true;
            verified.verdict = Verdict::Refused(format!("refused: {why}"));
            return verified;
        }
    };
    verified.present = true;
    verified.identity = Some(record.identity);
    verified.bytes = record.bytes;
    if let Some(why) = record.oversize() {
        verified.verdict = Verdict::Oversize(why);
        return verified;
    }
    let bytes = match record.read() {
        Ok(bytes) if bytes.len() as u64 > MAX_SURVEY_BYTES => {
            verified.verdict = Verdict::Oversize(oversize(bytes.len() as u64));
            return verified;
        }
        Ok(bytes) => bytes,
        Err(error) => {
            verified.verdict = Verdict::Refused(format!(
                "refused: its record cannot be read, so it is kept: {error}"
            ));
            return verified;
        }
    };
    let survey = match parse_survey(survey_id, &bytes) {
        Ok(survey) => survey,
        Err(why) => {
            verified.verdict = Verdict::Unreadable(why);
            return verified;
        }
    };
    verified.verdict = match super::TombstoneIdentity::of_record(recorded, &bytes) {
        Ok(Ok(identity)) => {
            verified.vouched = Some((Some(identity.cutoff_unix_s.get()), identity.completeness));
            Verdict::Agrees
        }
        Ok(Err(field)) => {
            let cutoff_s = survey
                .identity
                .body
                .as_ref()
                .map(|identity| identity.cutoff_unix_s.get());
            verified.vouched = Some((cutoff_s, survey.completeness));
            Verdict::Disagrees(field)
        }
        Err(why) => Verdict::Unreadable(why),
    };
    verified
}

/// What the collection pass takes from one Project: the Surveys beyond the newest
/// `keep_surveys`, oldest first, idle at least `idle_ms` and not used within the hour, never the
/// newest complete one. `keep_surveys` is a target: a protected Survey beyond it stays.
pub(crate) fn choose_beyond(
    live: &[Live],
    keep_surveys: usize,
    idle_ms: u64,
    now_ms: u64,
) -> Vec<&Live> {
    let beyond = live.len().saturating_sub(keep_surveys);
    live.iter()
        .take(beyond)
        .filter(|survey| {
            !survey.protected
                && survey.cutoff_ms.saturating_add(idle_ms) <= now_ms
                && survey.cutoff_ms.saturating_add(HOUR_MS) <= now_ms
        })
        .collect()
}

/// [`choose_beyond`] over a log and its tombstones.
#[cfg(test)]
fn beyond_limit<'a>(
    log: &'a Log,
    collected: &BTreeSet<String>,
    keep_surveys: usize,
    idle_ms: u64,
    now_ms: u64,
) -> Vec<&'a SurveyRecordedPayloadV1> {
    let protected = newest_complete(log);
    let chain = oldest_first(log);
    let live: Vec<Live> = chain
        .iter()
        .filter(|recorded| !collected.contains(&recorded.survey_id))
        .map(|recorded| Live {
            survey_id: recorded.survey_id.clone(),
            cutoff_ms: cutoff_ms(recorded),
            protected: protected == Some(recorded.survey_id.as_str()),
            identity: None,
            verdict: Verdict::Agrees,
        })
        .collect();
    let chosen: BTreeSet<&str> = choose_beyond(&live, keep_surveys, idle_ms, now_ms)
        .into_iter()
        .map(|survey| survey.survey_id.as_str())
        .collect();
    chain
        .into_iter()
        .filter(|recorded| chosen.contains(recorded.survey_id.as_str()))
        .collect()
}

/// What the plan does with a Survey it picks, by [`judge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Judged {
    /// Taken: tombstoned and deleted, `unreadable` when its bytes are no Survey.
    Take {
        unreadable: Option<String>,
        identity: Option<Identity>,
    },
    /// Left where it is, and said: a record over the ceiling.
    Left(LeftReason, String),
    /// Refused, beginning `refused`: reached through a link, or unreadable to af.
    Refused(String),
}

/// Judge one Survey the plan picks from the Project Store at `store`, reading only what
/// [`look`] verified of it (`live`, its verdict) and its record's name: found through
/// descriptors and compared with `inventoried`, the identity inventory stated for it (`None`:
/// the CAS did not hold it). Another file at its name, or one where inventory found none, is
/// [`REPLACED_SINCE_INVENTORY`]: the plan never adopts an inode inventory did not measure, so
/// the bytes it plans are always the measured inode's. The event is never read again here: a
/// Survey whose event its record disproves is refused, one that cannot be reached without a
/// link or that af may not read is refused, one over [`MAX_SURVEY_BYTES`] is left `oversize`,
/// and one whose bytes are no Survey, or that is gone from the CAS, is taken `unreadable`.
pub(crate) fn judge(store: &Path, live: &Live, inventoried: Option<Identity>) -> Judged {
    replace_object(
        "AF_TEST_SURVEY_REPLACE_BEFORE_JUDGE",
        &object_path(store, &live.survey_id),
    );
    let record = match locate(store, &live.survey_id) {
        Ok(record) => record,
        Err(why) => return Judged::Refused(format!("refused: {why}")),
    };
    let found = record.as_ref().map(|record| record.identity);
    if found != inventoried || found != live.identity {
        return Judged::Refused(format!(
            "{REPLACED_SINCE_INVENTORY}: its record's name holds another file than inventory \
             measured, so nothing of it is taken and the next pass measures it again"
        ));
    }
    match &live.verdict {
        Verdict::Agrees => Judged::Take {
            unreadable: None,
            identity: found,
        },
        Verdict::Unreadable(why) => Judged::Take {
            unreadable: Some(why.clone()),
            identity: found,
        },
        Verdict::Oversize(why) => Judged::Left(LeftReason::Oversize, why.clone()),
        Verdict::Disagrees(field) => Judged::Refused(disagrees(field)),
        Verdict::Refused(why) => Judged::Refused(why.clone()),
    }
}

/// The lock of the Project Store at `store`, taken without waiting and opened with the flags
/// every holder opens it with. The error is why it cannot be taken now: [`HELD`] while a refresh
/// or a plan holds it, `lock unavailable: <error>` while af cannot open or lock the file.
fn take_lock(store: &Path) -> Result<nix::fcntl::Flock<std::fs::File>, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.join(LOCK))
        .map_err(|error| format!("{LOCK_UNAVAILABLE}: {error}"))?;
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(
        |(_, errno)| match errno {
            nix::errno::Errno::EWOULDBLOCK => HELD.to_string(),
            other => format!("{LOCK_UNAVAILABLE}: {other}"),
        },
    )
}

/// The one predicate by which inventory, the plan and an applied job decide that the Surveys
/// of the Project Store at `store` are held: its lock taken as an applied job takes it, and
/// released at once. The error is the reason each of them gives.
pub(crate) fn lock_available(store: &Path) -> Result<(), String> {
    take_lock(store).map(drop)
}

/// The Project Store at `store`, held without waiting: `Ok(Err(reason))` while its lock cannot
/// be taken now ([`lock_available`]'s reason).
fn hold(store: &Path) -> Result<Result<ProjectStore, String>, String> {
    let lock = match take_lock(store) {
        Ok(lock) => lock,
        Err(reason) => return Ok(Err(reason)),
    };
    let cas = Cas::open_existing(store.join("cas")).map_err(|error| error.to_string())?;
    let events =
        EventStore::open(store.join("events.sqlite")).map_err(|error| error.to_string())?;
    let name = store
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("a Project Store is named by its hex")?;
    Ok(Ok(ProjectStore {
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

/// A debug build's fixture may stop a removal after its rename into its pass's container and
/// before its unlink, as a crash would (`AF_TEST_SURVEY_STOP_BEFORE_UNLINK=1`), so the next
/// sweep is seen to finish it. Release binaries never read it.
fn stop_before_unlink() {
    #[cfg(debug_assertions)]
    if std::env::var_os("AF_TEST_SURVEY_STOP_BEFORE_UNLINK").is_some_and(|value| value == "1") {
        eprintln!("af: stopped before an unlink (AF_TEST_SURVEY_STOP_BEFORE_UNLINK)");
        std::process::exit(86);
    }
}

/// What a job did: the allocated bytes of what it deleted, and, for a Survey it collected, why
/// its record's bytes were no Survey.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Executed {
    pub(crate) bytes: u64,
    pub(crate) unreadable: Option<String>,
}

/// Run one job of the plan under the Store's lock, taken without waiting. What differs from
/// what inventory measured and the plan found (the lock lost, the Survey protected or collected
/// since, its record or a container replaced, gone or refused) is the error, beginning with what
/// it concerns: the job does nothing more, and the next pass plans again.
pub(crate) fn execute(job: &Job) -> Result<Executed, String> {
    let said = |why: String| match job {
        Job::Take { survey_id, .. }
        | Job::AtName { survey_id, .. }
        | Job::Moved { survey_id, .. }
        | Job::Unrecorded { survey_id, .. }
        | Job::MovedUnrecorded { survey_id, .. } => format!("Survey {survey_id} is {why}"),
        Job::Publication { survey_id, .. } => {
            format!("the pending-publication entry of Survey {survey_id} is {why}")
        }
        Job::Staged { survey_id, .. } => {
            format!("the object the writer staged for Survey {survey_id} is {why}")
        }
        Job::Container { container, .. } => format!(
            "the container `cas/{REMOVING}/{}` of a dead pass is {why}",
            container.to_string_lossy()
        ),
    };
    let lost = |reason: String| {
        said(format!(
            "not {}: the lock was lost since the plan ({reason}); the next pass tries again",
            if matches!(
                job,
                Job::Container { .. } | Job::Publication { .. } | Job::Staged { .. }
            ) {
                "removed"
            } else {
                "collected"
            }
        ))
    };
    let removed = |bytes: u64| Executed {
        bytes,
        unreadable: None,
    };
    match job {
        Job::Take {
            store,
            survey_id,
            identity,
        } => {
            let mut held = hold(store)?.map_err(lost)?;
            take(&mut held, survey_id, *identity).map_err(said)
        }
        Job::AtName {
            store,
            survey_id,
            identity,
            unreadable,
        } => {
            let _held = hold(store)?.map_err(lost)?;
            let settled = open_cas(store)
                .and_then(|cas| read_settled(&cas))
                .map_err(|why| said(format!("refused: {why}")))?;
            if settled.survey_ids.contains(survey_id) {
                return Err(said(format!(
                    "{REPLACED}: its record left the CAS since the plan, so what holds its name \
                     is foreign and nothing is deleted"
                )));
            }
            let record = locate(store, survey_id).map_err(|why| said(format!("refused: {why}")))?;
            match record {
                None => Err(said(format!("{GONE}, so nothing of it is freed"))),
                Some(record) if record.identity != *identity => Err(said(format!(
                    "{REPLACED}: another file holds its record's name since the plan, so \
                     nothing is deleted and the next pass plans again"
                ))),
                Some(record) if !settled.selects(survey_id, record.identity, record.size) => {
                    Err(said(format!(
                        "{REPLACED}: what holds its record's name is not the object its \
                         collection selected, so it is foreign and nothing is deleted"
                    )))
                }
                Some(record) if !*unreadable && !record.is_the_record().unwrap_or(false) => {
                    Err(said(format!(
                        "{REPLACED}: its record's name holds bytes that are not its record's \
                         since the plan, so nothing is deleted"
                    )))
                }
                Some(record) => record
                    .remove(true)
                    .map(removed)
                    .map_err(|error| said(refusal(error))),
            }
        }
        Job::Unrecorded {
            store,
            survey_id,
            identity,
            length,
        } => {
            let held = hold(store)?.map_err(lost)?;
            unrecorded(&held, survey_id, *identity, *length)
                .map(removed)
                .map_err(said)
        }
        Job::MovedUnrecorded {
            store,
            container,
            survey_id,
            identity,
            length,
        } => {
            let held = hold(store)?.map_err(lost)?;
            moved_unrecorded(&held, container, survey_id, *identity, *length)
                .map(removed)
                .map_err(said)
        }
        Job::Publication {
            store,
            survey_id,
            identity,
        } => {
            let held = hold(store)?.map_err(lost)?;
            clear_publication(&held, survey_id, *identity)
                .map(removed)
                .map_err(said)
        }
        Job::Staged {
            store,
            survey_id,
            identity,
            length,
        } => {
            let _lock = take_lock(store).map_err(lost)?;
            remove_staged(store, survey_id, *identity, *length)
                .map(removed)
                .map_err(said)
        }
        Job::Moved {
            store,
            container,
            survey_id,
            identity,
        } => {
            let _held = hold(store)?.map_err(lost)?;
            let settled = open_cas(store)
                .and_then(|cas| read_settled(&cas))
                .map_err(|why| said(format!("refused: {why}")))?;
            if settled
                .selected
                .get(survey_id.as_str())
                .is_none_or(|&(selected, _)| selected != *identity)
            {
                return Err(said(format!(
                    "{REPLACED}: what the dead pass left is not the object its collection \
                     selected, so nothing is deleted"
                )));
            }
            settle(store, container, survey_id, *identity, true, |_| {})
                .map(removed)
                .map_err(|error| said(refusal(error)))
        }
        Job::Container {
            store,
            container,
            identity,
        } => {
            let _lock = take_lock(store).map_err(lost)?;
            remove_container(store, container, *identity)
                .map(removed)
                .map_err(said)
        }
    }
}

/// Collect one live Survey of a held Store: its protection read again under the lock, its
/// record found again and compared with what the plan stated, the object selected recorded in
/// `settled.json` ([`record_selected`]), its tombstone appended, then the record deleted by
/// [`Record::remove`].
fn take(
    store: &mut ProjectStore,
    survey_id: &str,
    planned: Option<Identity>,
) -> Result<Executed, String> {
    let directory = store.directory.clone();
    let log = store.log()?;
    if collected_ids(&store.events)?.contains(survey_id) {
        return Err("not collected again: another pass collected it since the plan".into());
    }
    if newest_complete(&log) == Some(survey_id) {
        return Err(format!("{NEWEST_COMPLETE} now, so it is kept"));
    }
    let recorded = log
        .chain()
        .into_iter()
        .find(|recorded| recorded.survey_id == survey_id)
        .cloned()
        .ok_or("not recorded in its Project Store")?;
    let record = locate(&directory, survey_id).map_err(|why| format!("refused: {why}"))?;
    if record.as_ref().map(|record| record.identity) != planned {
        return Err(format!(
            "{REPLACED}: what its record's name holds changed since the plan, so nothing is \
             written or deleted and the next pass plans again"
        ));
    }
    let read = match &record {
        None => Err("its record is not in the CAS".to_string()),
        Some(record) => {
            if let Some(why) = record.oversize() {
                return Err(why);
            }
            match record.read() {
                Ok(bytes) if bytes.len() as u64 > MAX_SURVEY_BYTES => {
                    return Err(oversize(bytes.len() as u64));
                }
                Ok(bytes) => totals(survey_id, &bytes).and_then(|totals| {
                    super::TombstoneIdentity::of_record(&recorded, &bytes)
                        .map(|identity| (identity, totals))
                }),
                Err(error) => {
                    return Err(format!(
                        "refused: its record cannot be read, so it is kept: {error}"
                    ));
                }
            }
        }
    };
    // The tombstone binds the record's identity, copied after the event was verified against
    // the record; an event that disagrees with its record is refused and the Survey kept. A
    // record that is no Survey vouches for nothing, and its tombstone says `unreadable`.
    let (identity, tree, history, unreadable) = match read {
        Ok((Ok(identity), (tree, history))) => (identity, tree, history, None),
        Ok((Err(field), _)) => return Err(disagrees(field)),
        Err(why) => (
            super::TombstoneIdentity::of_event(&recorded),
            None,
            None,
            Some(why),
        ),
    };
    let tombstone = SurveyCollectedPayloadV1 {
        survey_id: survey_id.to_string(),
        tree,
        history,
        unreadable: unreadable.clone(),
        head: identity.head,
        cutoff_unix_s: identity.cutoff_unix_s,
        completeness: identity.completeness,
        refs_digest: identity.refs_digest,
        policy_digest: identity.policy_digest,
        surveyor_version: identity.surveyor_version,
        previous_survey_id: identity.previous_survey_id,
        collected_unix_ms: crate::storage::now_unix_ms().into(),
    };
    tombstone
        .validate()
        .map_err(|why| format!("refused: {why}"))?;
    // The object this collection takes, by device, inode, birth time and length, durable before
    // its tombstone: whatever recovery finds at its name or in a dead pass's container later is
    // its record only while it is that inode, never because its bytes hash to the Survey's ID.
    if let Some(record) = &record {
        open_cas(&directory)
            .and_then(|cas| record_selected(&cas, survey_id, record.identity, record.size))
            .map_err(|why| format!("refused: {why}"))?;
    }
    let event = NewEvent::new(
        EventType::SurveyCollectedV1,
        serde_json::to_value(&tombstone).map_err(|error| format!("refused: {error}"))?,
    )
    .at(review_core::task::collection::collected_time(
        crate::storage::now_unix_ms(),
    ));
    store
        .events
        .append(COLLECTION_RUN, &store.cas, event)
        .map_err(|error| format!("refused: recording SurveyCollected@1: {error}"))?;
    let bytes = match record {
        Some(_) if stop_after_tombstones() => 0,
        Some(record) => record.remove(true).map_err(|error| {
            if error.starts_with(GONE) {
                format!("{error}; its tombstone is recorded")
            } else {
                format!(
                    "{}; its tombstone is recorded, and the next pass deletes its record",
                    refusal(error)
                )
            }
        })?,
        // Gone from the CAS already: whatever takes its name from now on is foreign.
        None => {
            open_cas(&directory)
                .and_then(|cas| record_settled(&cas, survey_id))
                .map_err(|why| {
                    format!(
                        "refused: recording that its record left the CAS: {why}; its tombstone \
                         is recorded"
                    )
                })?;
            0
        }
    };
    Ok(Executed { bytes, unreadable })
}

/// Whether `survey_id` has a `SurveyRecorded@1` or a tombstone in the held Store's log now.
fn committed(store: &ProjectStore, survey_id: &str) -> Result<bool, String> {
    let log = store.log()?;
    Ok(log
        .chain()
        .iter()
        .any(|recorded| recorded.survey_id == survey_id)
        || collected_ids(&store.events)?.contains(survey_id))
}

/// The pending-publication entry of `survey_id` in the held Store, read again: its file's
/// identity and what it says; `None` when there is none.
fn publication_of(
    store: &ProjectStore,
    survey_id: &str,
) -> Result<Option<(Identity, SurveyPendingV2)>, String> {
    let cas = open_cas(&store.directory)?;
    let directory = match review_sandbox::open_beneath(&cas.store, Path::new(PENDING)) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("refused: its `{PENDING}` directory: {error}")),
    };
    let (_, hex) = survey_id.split_at("sha256:".len().min(survey_id.len()));
    let name = format!("{hex}.json");
    let Some(stated) = stat_at(&directory, OsStr::new(&name))? else {
        return Ok(None);
    };
    let identity = stated.identity;
    read_publication(&directory, &name, survey_id, identity)
        .map(|(entry, _)| Some((identity, entry)))
        .map_err(|why| format!("refused: its pending-publication entry cannot be read: {why}"))
}

/// Whether the held Store's pending-publication entry of `survey_id` still describes the object
/// the plan stated as `planned`, `length` bytes long: refused otherwise, nothing deleted.
fn still_described(
    store: &ProjectStore,
    survey_id: &str,
    planned: Identity,
    length: u64,
) -> Result<(), String> {
    if committed(store, survey_id)? {
        return Err("recorded since the plan, so its object is kept".into());
    }
    let entry = publication_of(store, survey_id)?.map(|(_, entry)| entry);
    if entry.as_ref().and_then(described) != Some((planned, length)) {
        return Err(
            "no longer described by its pending-publication entry, so its object is kept".into(),
        );
    }
    Ok(())
}

/// How an object that is no longer what inventory and the entry stated is skipped.
fn object_changed(identity: Identity, size: u64, planned: Identity, length: u64) -> String {
    let what = if identity == planned {
        format!("{size} bytes long where its pending-publication entry says {length}")
    } else {
        "another file than its pending-publication entry describes".into()
    };
    format!("{OBJECT_CHANGED}: what its CAS name holds is {what}, so nothing is deleted")
}

/// Reclaim, under the held Store's lock, the object at `survey_id`'s CAS name the plan stated
/// as `planned`, `length` bytes long: only while its `SurveyRecorded@1` has not committed, its
/// pending-publication entry still describes that device, inode and length, and the name still
/// holds that inode at that length; anything else is [`OBJECT_CHANGED`] or refused, and nothing
/// is deleted. Deleted through [`Record::remove`]; returns the allocated bytes of the inode.
fn unrecorded(
    store: &ProjectStore,
    survey_id: &str,
    planned: Identity,
    length: u64,
) -> Result<u64, String> {
    still_described(store, survey_id, planned, length)?;
    match locate(&store.directory, survey_id).map_err(|why| format!("refused: {why}"))? {
        None => Err(format!("{GONE}, so nothing of it is freed")),
        Some(record) if record.identity != planned || record.size != length => Err(object_changed(
            record.identity,
            record.size,
            planned,
            length,
        )),
        Some(record) => record.remove(false).map_err(refusal),
    }
}

/// Reclaim, under the held Store's lock, the object of the unrecorded Survey `survey_id` that a
/// dead pass moved into its `container` before it could unlink it, stated as `planned`,
/// `length` bytes long: only while its `SurveyRecorded@1` has not committed, its entry still
/// describes it and the container still holds that inode at that length; then settled as a
/// collected record moved is ([`settle`]), but never recorded in `settled.json`, since no
/// tombstone names the Survey. Returns the allocated bytes of the inode deleted.
fn moved_unrecorded(
    store: &ProjectStore,
    container: &OsStr,
    survey_id: &str,
    planned: Identity,
    length: u64,
) -> Result<u64, String> {
    still_described(store, survey_id, planned, length)?;
    let cas = open_cas(&store.directory)?;
    let removing =
        open_removing(&cas, false)?.ok_or_else(|| format!("{GONE}, so nothing of it is freed"))?;
    let dead = match review_sandbox::open_beneath(&removing, Path::new(container)) {
        Ok(dead) => dead,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{GONE}, so nothing of it is freed"));
        }
        Err(error) => {
            return Err(format!(
                "refused: opening the container it was left in: {error}"
            ));
        }
    };
    match stat_at(&dead, OsStr::new(survey_id)).map_err(|why| format!("refused: {why}"))? {
        None => Err(format!("{GONE}, so nothing of it is freed")),
        Some(stated)
            if stated.kind != Kind::File || stated.identity != planned || stated.size != length =>
        {
            Err(object_changed(
                stated.identity,
                stated.size,
                planned,
                length,
            ))
        }
        Some(_) => settle(
            &store.directory,
            container,
            survey_id,
            planned,
            false,
            |_| {},
        )
        .map_err(refusal),
    }
}

/// What of the object a pending-publication entry describes the held Store still holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Present {
    /// No file with its device and inode is at its Survey's CAS name or in a pass's container.
    Gone,
    /// It is there, with the length the entry records: recovery reclaims it first.
    Described,
    /// It is there, but no longer that length: truncated or grown in place, or moved so. It is
    /// listed `foreign`, never reclaimed, and the entry stays as long as it is there.
    Changed,
}

/// Whether the held Store still holds a file with the inode `entry` describes (its device, inode
/// and birth time), at its Survey's CAS name, at its staged name in `pending-surveys` or moved
/// into a pass's container under `cas/removing`, whatever its length now: the inode decides,
/// never the length, so an object grown or truncated in place is still the entry's. An entry is
/// cleared only once no such file remains.
fn still_holds(
    store: &ProjectStore,
    survey_id: &str,
    entry: &SurveyPendingV2,
) -> Result<Present, String> {
    let Some((identity, length)) = described(entry) else {
        return Ok(Present::Gone);
    };
    let judged = |size: u64| {
        if size == length {
            Present::Described
        } else {
            Present::Changed
        }
    };
    if let Some(record) = locate(&store.directory, survey_id)
        .map_err(|why| format!("refused: {why}"))?
        .filter(|record| record.identity == identity)
    {
        return Ok(judged(record.size));
    }
    let cas = open_cas(&store.directory)?;
    if let Some(staged) = staged_at(&cas, survey_id)?
        .filter(|staged| staged.kind == Kind::File && staged.identity == identity)
    {
        return Ok(judged(staged.size));
    }
    let (found, _) = walk_removing(&cas)
        .map_err(|why| format!("refused: what is under `cas/{REMOVING}` cannot be read: {why}"))?;
    Ok(found
        .iter()
        .find(|file| file.kind == FoundKind::File && file.identity == identity)
        .map_or(Present::Gone, |file| judged(file.size)))
}

/// How the entry of an object that is still there, but changed, is kept.
const STILL_PRESENT: &str = "kept: object changed, still present";

/// Clear, under the held Store's lock, the pending-publication entry of `survey_id` the plan
/// stated as `planned`: when its `SurveyRecorded@1` committed, or when no file with the inode it
/// describes is left, at its Survey's CAS name or in any pass's container under `cas/removing`;
/// an entry whose object is still there to reclaim is kept, so an interrupted reclamation keeps
/// its recovery, and so is one whose object changed length in place and stays ([`STILL_PRESENT`]). Returns the allocated bytes of the entry's
/// file.
fn clear_publication(
    store: &ProjectStore,
    survey_id: &str,
    planned: Identity,
) -> Result<u64, String> {
    let Some((identity, entry)) = publication_of(store, survey_id)? else {
        return Err(format!("{GONE}, so nothing of it is freed"));
    };
    if identity != planned {
        return Err(format!(
            "{REPLACED_SINCE_INVENTORY}: another file holds its name, so it is left"
        ));
    }
    if !committed(store, survey_id)? {
        match still_holds(store, survey_id, &entry)? {
            Present::Gone => {}
            Present::Described => {
                return Err(
                    "kept: the object it describes is still in its Project Store's CAS, so the \
                     next pass reclaims it first"
                        .into(),
                );
            }
            Present::Changed => {
                return Err(format!(
                    "{STILL_PRESENT}: a file with the inode it describes is still in its \
                     Project Store's CAS at another length, listed foreign and never \
                     reclaimed, so the entry stays until that file is gone"
                ));
            }
        }
    }
    let cas = open_cas(&store.directory)?;
    let directory = review_sandbox::open_beneath(&cas.store, Path::new(PENDING))
        .map_err(|error| format!("refused: its `{PENDING}` directory: {error}"))?;
    let (_, hex) = survey_id.split_at("sha256:".len());
    let name = format!("{hex}.json");
    let Some(stated) = stat_at(&directory, OsStr::new(&name))? else {
        return Err(format!("{GONE}, so nothing of it is freed"));
    };
    if stated.kind != Kind::File || stated.identity != planned {
        return Err(format!(
            "{REPLACED_SINCE_INVENTORY}: another file holds its name, so it is left"
        ));
    }
    nix::unistd::unlinkat(
        &directory,
        name.as_str(),
        nix::unistd::UnlinkatFlags::NoRemoveDir,
    )
    .map_err(|errno| format!("refused: removing it: {errno}"))?;
    Ok(stated.allocated)
}

/// What holds the staged name of `survey_id` in the Store's `pending-surveys`, stated without
/// following a link; `None` when nothing does or there is no such directory.
fn staged_at(cas: &CasDirectories, survey_id: &str) -> Result<Option<Stated>, String> {
    let directory = match review_sandbox::open_beneath(&cas.store, Path::new(PENDING)) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("refused: its `{PENDING}` directory: {error}")),
    };
    if device_of(&directory)? != cas.device {
        return Err(format!(
            "refused: its `{PENDING}` directory resolves outside its Project Store"
        ));
    }
    stat_at(
        &directory,
        OsStr::new(&super::pending::staged_name(survey_id)),
    )
}

/// Delete, under a Store lock the caller holds, the object the writer staged for `survey_id`
/// that the plan stated as `planned`, `length` bytes long: stated again relative to the Store's
/// `pending-surveys`, the writer's own directory, without following a link, and unlinked
/// relative to the same descriptor only while it is that regular file of that device, inode,
/// birth time and length; anything else is [`OBJECT_CHANGED`] and nothing is deleted. No writer
/// holds the lock meanwhile, so a staged object is no publication's in progress. Returns the
/// allocated bytes of the inode deleted.
fn remove_staged(
    store: &Path,
    survey_id: &str,
    planned: Identity,
    length: u64,
) -> Result<u64, String> {
    let cas = open_cas(store)?;
    let directory = match review_sandbox::open_beneath(&cas.store, Path::new(PENDING)) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{GONE}, so nothing of it is freed"));
        }
        Err(error) => return Err(format!("refused: its `{PENDING}` directory: {error}")),
    };
    if device_of(&directory)? != cas.device {
        return Err(format!(
            "refused: its `{PENDING}` directory resolves outside its Project Store"
        ));
    }
    let name = super::pending::staged_name(survey_id);
    let Some(stated) = stat_at(&directory, OsStr::new(&name))? else {
        return Err(format!("{GONE}, so nothing of it is freed"));
    };
    if stated.kind != Kind::File || stated.identity != planned || stated.size != length {
        return Err(format!(
            "{OBJECT_CHANGED}: what its staged name holds is not the object inventory stated, \
             so nothing is deleted"
        ));
    }
    nix::unistd::unlinkat(
        &directory,
        name.as_str(),
        nix::unistd::UnlinkatFlags::NoRemoveDir,
    )
    .map_err(|errno| format!("refused: removing it: {errno}"))?;
    Ok(stated.allocated)
}

/// A removal's error as a refusal: [`REPLACED`] already says so, and [`GONE`] is no refusal.
fn refusal(error: String) -> String {
    if error.starts_with(REPLACED) || error.starts_with(GONE) {
        error
    } else {
        format!("refused: {error}")
    }
}

/// Settle a record a dead pass left as `survey_id` in its `container`, the same sequence a
/// pass's own deletion follows: renamed into a container of this pass's own, stated there with
/// `fstatat` relative to that container and unlinked only when it is the inode the plan found.
/// Another file is set aside and nothing is deleted, and a record gone since inventory is
/// [`GONE`]. With `record` (a collected Survey's record), `settled.json` records that it left
/// the CAS before it is unlinked, and a failure to record it puts it back and deletes nothing.
/// The dead container stays: it goes only as an entry of its own the plan lists after the
/// records it holds ([`Job::Container`]). Returns the allocated bytes of the inode deleted.
fn settle(
    store: &Path,
    container: &OsStr,
    survey_id: &str,
    identity: Identity,
    record: bool,
    mut hook: impl FnMut(Before),
) -> Result<u64, String> {
    let cas = open_cas(store)?;
    let removing = open_removing(&cas, false)?.ok_or("its `cas/removing` is gone")?;
    let dead = review_sandbox::open_beneath(&removing, Path::new(container))
        .map_err(|error| format!("opening the container it was left in: {error}"))?;
    if device_of(&dead)? != cas.device {
        return Err("the container it was left in resolves outside its Project Store".into());
    }
    let own = Container::create(&cas)?;
    let entry = OsStr::new(survey_id);
    let settled = match rename_free(&dead, entry, &own.directory, entry) {
        Ok(()) => {
            hook(Before::Check);
            // Its record left its CAS name when the dead pass moved it: once the inode is known
            // to be that record, whatever holds the name is foreign, sweep after sweep.
            let ready = || {
                if record {
                    record_settled(&cas, survey_id)
                } else {
                    Ok(())
                }
            };
            match own.unlink_checked(entry, identity, ready) {
                Ok(bytes) => Ok(bytes),
                Err(Checked::Unready(why)) => Err(format!(
                    "recording that its record left the CAS: {why}; {}",
                    if rename_free(&own.directory, entry, &dead, entry).is_ok() {
                        "it is put back where the dead pass left it and nothing is deleted"
                    } else {
                        "it stays in this pass's container for the next pass"
                    }
                )),
                Err(Checked::Incomplete(why)) => Err(format!(
                    "{why}; {}",
                    if rename_free(&own.directory, entry, &dead, entry).is_ok() {
                        "it is put back where the dead pass left it and nothing is deleted"
                    } else {
                        "it stays in this pass's container for the next pass"
                    }
                )),
                Err(Checked::Gone) => Err(format!("{GONE}, so nothing of it is freed")),
                Err(Checked::Failed(why)) => Err(format!(
                    "deleting it from `{}`: {why}; the next pass finishes it",
                    own.label(entry)
                )),
                Err(Checked::Replaced) => Err(format!(
                    "{REPLACED}: what the dead pass left changed since the plan; {}",
                    own.set_aside(entry, &dead, entry)
                )),
            }
        }
        Err(rustix::io::Errno::NOENT) => Err(format!("{GONE}, so nothing of it is freed")),
        Err(errno) => Err(format!("moving it into this pass's container: {errno}")),
    };
    own.close();
    settled
}

/// Remove the container `container` of a dead pass that inventory stated as `identity`, empty
/// or holding only the moved records the plan settles before it, under a Store lock the caller
/// holds: stated again relative to `cas/removing` without following it, and removed only while
/// it is that directory; `rmdir` removes no container that still holds a file. A container gone
/// since inventory is [`GONE`]. Returns the bytes it freed as the Storage Budget counts them:
/// none, since a directory counts nothing.
fn remove_container(store: &Path, container: &OsStr, identity: Identity) -> Result<u64, String> {
    let cas = open_cas(store)?;
    let gone = || format!("{GONE}, so nothing of it is freed");
    let Some(removing) = open_removing(&cas, false)? else {
        return Err(gone());
    };
    let Some(stated) = stat_at(&removing, container)? else {
        return Err(gone());
    };
    if stated.kind != Kind::Directory || stated.identity != identity {
        return Err(format!(
            "{REPLACED_SINCE_INVENTORY}: another directory or file holds its name, so it is left"
        ));
    }
    nix::unistd::unlinkat(&removing, container, nix::unistd::UnlinkatFlags::RemoveDir)
        .map_err(|errno| format!("refused: removing it: {errno}; it is left"))?;
    // A directory holds no bytes the Storage Budget counts: only regular files do.
    Ok(stated.counted())
}

/// `text` cut at a character boundary to what an `unreadable` tombstone holds.
fn bounded(text: String) -> String {
    let mut end = text.len().min(MAX_UNREADABLE_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// What a Survey's record of at most [`MAX_SURVEY_BYTES`] says for its tombstone: the totals of
/// `tree` and `history`, or, when its bytes are no Survey, why, in af's own words, never the
/// bytes themselves.
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
    let survey = parse_survey(survey_id, bytes)?;
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

/// A Survey's record of at most [`MAX_SURVEY_BYTES`] parsed, when its bytes hash to its ID:
/// what it vouches for. Otherwise why its bytes are no Survey, in af's own words.
fn parse_survey(survey_id: &str, bytes: &[u8]) -> Result<ProjectSurveyV2, String> {
    if review_store::canonical::blob_content_id(bytes) != survey_id {
        return Err("its bytes do not match its digest".into());
    }
    serde_json::from_slice(bytes).map_err(|error| {
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
    })
}

/// The tombstones of a Project Store's log, in order.
pub(crate) fn tombstones(events: &EventStore) -> Result<Vec<SurveyCollectedPayloadV1>, String> {
    tombstones_of(
        events
            .replay(COLLECTION_RUN)
            .map_err(|error| format!("the Survey tombstones: {error}"))?,
    )
}

/// The tombstones among the events of the run `survey-collection`, in order.
fn tombstones_of(
    events: Vec<review_core::RunEvent>,
) -> Result<Vec<SurveyCollectedPayloadV1>, String> {
    events
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
        ProjectCommand::History { json } => super::history(json),
        other => super::super::commands::run(other),
    }
}

/// `af project history` once its chain is verified, `superseded` the Surveys whose records are
/// of a superseded version: in text, while any Survey is collected, the lines of
/// [`history_lines`]; otherwise `projects::commands`'s.
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

/// `af project history` in text: the chain newest first, a collected Survey answered from its
/// tombstone and marked `collected`, or `collected (unreadable)` when its record's bytes were no
/// Survey, and an uncollected one whose record is of a superseded version marked
/// `unreadable: superseded version`, with none of its event's fields but its ID and time.
fn history_lines(
    log: &Log,
    tombstones: &[SurveyCollectedPayloadV1],
    superseded: &BTreeSet<String>,
) -> Vec<String> {
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
            None if superseded.contains(&recorded.survey_id) => format!(
                "{}  {}  {}",
                recorded.survey_id,
                time(recorded.recorded_unix_ms.get()),
                super::SUPERSEDED_LINE
            ),
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
/// [`MAX_PATTERN_BYTES`], naming `key` (ADR-0159 clause 14). The error names no pattern, so a
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

/// Measure the head under one deadline from `started` (ADR-0159 clause 14), every pattern list
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
            Arc::clone(&matches),
        )
        .unwrap();
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

    /// Delete the file at `path` and create a byte-identical one there, as another process
    /// could, born later: a file system that hands the freed inode to the next file (ext4 does)
    /// gives it the same device and inode, so only its birth time tells it apart, and a test
    /// that means a replacement waits until that birth time differs.
    fn replace_born_later(path: &Path, bytes: &[u8]) {
        let before = Identity::born_of(path).unwrap();
        for _ in 0..100 {
            std::fs::remove_file(path).unwrap();
            std::thread::sleep(Duration::from_millis(5));
            std::fs::write(path, bytes).unwrap();
            if Identity::born_of(path).unwrap() != before {
                return;
            }
        }
        panic!("no file born later than the one at {}", path.display());
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

    /// Every file below `cas/removing` of a Store, by its path below it, and the containers.
    fn under_removing(store: &Path) -> (Vec<String>, Vec<String>) {
        let removing = store.join("cas/removing");
        let (mut files, mut containers) = (Vec::new(), Vec::new());
        for entry in std::fs::read_dir(&removing).into_iter().flatten() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().unwrap().is_dir() {
                for inner in std::fs::read_dir(entry.path()).unwrap() {
                    let inner = inner.unwrap().file_name().to_string_lossy().into_owned();
                    files.push(format!("{name}/{inner}"));
                }
                containers.push(name);
            } else {
                files.push(name);
            }
        }
        files.sort();
        containers.sort();
        (files, containers)
    }

    #[test]
    fn a_removal_deletes_the_inode_it_found_in_its_own_container_and_reports_its_bytes() {
        use std::os::unix::fs::MetadataExt;
        let (_root, store, survey_id) = store_with(&vec![b'x'; 10_000]);
        let object = object_path(&store, &survey_id);
        let record = locate(&store, &survey_id).unwrap().unwrap();
        let allocated = std::fs::metadata(&object).unwrap().blocks() * 512;
        let mut seen = None;
        let bytes = record
            .remove_with(false, |step, inside| {
                if step == Before::Check {
                    use std::os::unix::fs::PermissionsExt;
                    let container = inside.parent().unwrap();
                    let mode = std::fs::metadata(container).unwrap().permissions().mode();
                    seen = Some((inside.is_file(), mode & 0o777));
                }
            })
            .unwrap();
        assert_eq!(bytes, allocated);
        assert_eq!(
            seen,
            Some((true, 0o700)),
            "renamed into a private container"
        );
        assert!(!object.exists());
        assert_eq!(
            under_removing(&store),
            (Vec::new(), Vec::new()),
            "the pass removes its container"
        );
    }

    #[test]
    fn two_passes_never_share_a_container() {
        let a = pass_id().unwrap();
        let b = pass_id().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        let (_root, store, _) = store_with(b"one");
        let cas = open_cas(&store).unwrap();
        let first = Container::create(&cas).unwrap();
        // A name that exists is refused: a container is only ever created, never reused.
        let removing = open_removing(&cas, false).unwrap().unwrap();
        let taken = nix::sys::stat::mkdirat(
            &removing,
            first.name.as_str(),
            nix::sys::stat::Mode::from_bits_truncate(0o700),
        );
        assert_eq!(taken, Err(nix::errno::Errno::EEXIST));
        first.close();
        assert_eq!(under_removing(&store), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_record_replaced_before_its_rename_is_put_back_and_nothing_is_deleted() {
        let (root, store, survey_id) = store_with(b"one");
        let object = object_path(&store, &survey_id);
        let aside = root.path().join("aside");
        let record = locate(&store, &survey_id).unwrap().unwrap();
        // Between the identity read and the rename the record moves away and a byte-identical
        // copy, another inode, takes its name.
        let error = record
            .remove_with(false, |step, _| {
                if step == Before::Rename {
                    std::fs::rename(&object, &aside).unwrap();
                    std::fs::write(&object, b"one").unwrap();
                }
            })
            .unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert!(error.contains("put back"), "{error}");
        assert_eq!(std::fs::read(&object).unwrap(), b"one", "the copy is back");
        assert_eq!(
            std::fs::read(&aside).unwrap(),
            b"one",
            "the original is untouched"
        );
        assert_eq!(under_removing(&store), (Vec::new(), Vec::new()));
        // Found again, the copy is what its name holds, and it goes.
        let record = locate(&store, &survey_id).unwrap().unwrap();
        assert!(record.remove(false).unwrap() > 0);
        assert!(!object.exists());
        assert!(aside.is_file());
    }

    /// The check before the unlink is relative to the pass's own container: a file put in place
    /// of the record there, after the rename and before the check, is refused, put back, and
    /// nothing is deleted.
    #[test]
    fn a_replacement_in_the_container_before_the_check_is_refused() {
        let (root, store, survey_id) = store_with(b"one");
        let object = object_path(&store, &survey_id);
        let aside = root.path().join("aside");
        let record = locate(&store, &survey_id).unwrap().unwrap();
        let error = record
            .remove_with(false, |step, inside| {
                if step == Before::Check {
                    std::fs::rename(inside, &aside).unwrap();
                    std::fs::write(inside, b"someone else's").unwrap();
                }
            })
            .unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert!(error.contains("put back"), "{error}");
        assert_eq!(
            std::fs::read(&aside).unwrap(),
            b"one",
            "the record is untouched"
        );
        assert_eq!(
            std::fs::read(&object).unwrap(),
            b"someone else's",
            "the replacement is put back, not deleted"
        );
        assert_eq!(under_removing(&store), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_replaced_record_whose_name_is_taken_again_is_set_aside_under_a_name_no_pass_deletes() {
        let (root, store, survey_id) = store_with(b"one");
        let object = object_path(&store, &survey_id);
        let aside = root.path().join("aside");
        let record = locate(&store, &survey_id).unwrap().unwrap();
        let error = record
            .remove_with(false, |step, _| match step {
                Before::Rename => {
                    std::fs::rename(&object, &aside).unwrap();
                    std::fs::write(&object, b"two").unwrap();
                }
                Before::MoveBack => std::fs::write(&object, b"three").unwrap(),
                Before::Check => {}
            })
            .unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert!(error.contains(".replaced"), "{error}");
        assert_eq!(std::fs::read(&object).unwrap(), b"three");
        assert_eq!(std::fs::read(&aside).unwrap(), b"one");
        let (files, containers) = under_removing(&store);
        assert_eq!(files, [format!("{}/{survey_id}.replaced", containers[0])]);
        // The next pass finds it in a dead container under a name that is no Survey's: foreign,
        // left, even though a tombstone names the Survey.
        let (found, containers) = walk_removing(&open_cas(&store).unwrap()).unwrap();
        assert!(containers.iter().all(|container| !container.empty));
        let (moved, left) = judge_leftovers(
            &store,
            found,
            &BTreeSet::from([survey_id.clone()]),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(moved.is_empty(), "{moved:?}");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].reason, LeftReason::Foreign);
        assert!(left[0].bytes > 0);
    }

    /// What a dead pass left in its container is settled by the next pass through a container of
    /// its own: renamed, checked relative to it and unlinked. The dead container stays for the
    /// entry of its own the plan lists after it. A file that is not the inode the plan found is
    /// put back and nothing is deleted, and a record gone since is a failure.
    /// The Survey IDs the Store's `settled.json` lists.
    fn settled_in(store: &Path) -> Vec<String> {
        read_settled(&open_cas(store).unwrap())
            .unwrap()
            .survey_ids
            .into_iter()
            .collect()
    }

    /// A collected Survey's record is settled once the inode off its name is known to be its
    /// record, never for a replacement the check refuses; the record lists each Survey once,
    /// sorted.
    #[test]
    fn a_record_is_settled_only_once_its_inode_is_checked_and_stays_settled() {
        let (_root, store, survey_id) = store_with(b"settled");
        let object = object_path(&store, &survey_id);
        assert!(settled_in(&store).is_empty(), "no record yet");
        let record = locate(&store, &survey_id).unwrap().unwrap();
        let error = record
            .remove_with(true, |step, inside| {
                if step == Before::Check {
                    replace_born_later(inside, b"settled");
                }
            })
            .unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert!(
            settled_in(&store).is_empty(),
            "a replacement settles nothing"
        );
        let record = locate(&store, &survey_id).unwrap().unwrap();
        assert!(record.remove(true).unwrap() > 0);
        assert_eq!(settled_in(&store), [survey_id.clone()]);
        record_settled(&open_cas(&store).unwrap(), &survey_id).unwrap();
        let earlier = format!("sha256:{}", "0".repeat(64));
        record_settled(&open_cas(&store).unwrap(), &earlier).unwrap();
        assert_eq!(settled_in(&store), [earlier, survey_id]);
        assert!(!object.exists());
    }

    /// A settlement whose directory sync fails after the rename is a failure, and the earlier
    /// record is put back: the Survey stays unsettled, so its record is never unlinked on it and
    /// the next pass tries again. No temporary file is left.
    #[test]
    fn a_settlement_whose_sync_fails_puts_the_earlier_record_back() {
        let (_root, store, survey_id) = store_with(b"settled");
        let cas = open_cas(&store).unwrap();
        let fail = |cas: &CasDirectories, of: SyncOf| match of {
            SyncOf::Install => Err("injected".to_string()),
            SyncOf::Rollback => sync_store(cas, of),
        };
        let error = record_settled_with(&cas, &survey_id, fail).unwrap_err();
        assert!(
            error.starts_with("syncing `settled.json`: injected") && error.contains("put back"),
            "{error}"
        );
        assert!(!store.join(SETTLED).exists(), "no record, as before");
        let earlier = format!("sha256:{}", "0".repeat(64));
        record_settled(&cas, &earlier).unwrap();
        record_settled_with(&cas, &survey_id, fail).unwrap_err();
        assert_eq!(settled_in(&store), [earlier.clone()], "the earlier record");
        record_settled(&cas, &survey_id).unwrap();
        assert_eq!(settled_in(&store), [earlier, survey_id]);
        let names: Vec<String> = std::fs::read_dir(&store)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".settled"))
            .collect();
        assert!(names.is_empty(), "{names:?}");
    }

    /// A collection's selection is the record's identity and length, kept once its Survey is
    /// settled too; one whose sync fails is put back, the earlier record restored, and the error
    /// says no tombstone is appended.
    #[test]
    fn a_selection_is_recorded_durably_or_put_back() {
        let (_root, store, survey_id) = store_with(b"selected");
        let cas = open_cas(&store).unwrap();
        let record = locate(&store, &survey_id).unwrap().unwrap();
        let fail = |cas: &CasDirectories, of: SyncOf| match of {
            SyncOf::Install => Err("injected".to_string()),
            SyncOf::Rollback => sync_store(cas, of),
        };
        let error =
            record_selected_with(&cas, &survey_id, record.identity, record.size, fail).unwrap_err();
        assert!(
            error.contains("put back") && error.contains("no tombstone is appended"),
            "{error}"
        );
        assert!(!store.join(SETTLED).exists(), "no record, as before");
        record_selected(&cas, &survey_id, record.identity, record.size).unwrap();
        let settled = read_settled(&cas).unwrap();
        assert!(settled.selects(&survey_id, record.identity, record.size));
        let later = Identity {
            born: record.identity.born.map(|born| born + 1),
            ..record.identity
        };
        assert!(!settled.selects(&survey_id, later, record.size));
        assert!(!settled.selects(&survey_id, record.identity, record.size + 1));
        record_settled(&cas, &survey_id).unwrap();
        let settled = read_settled(&cas).unwrap();
        assert_eq!(settled.survey_ids, BTreeSet::from([survey_id.clone()]));
        assert!(
            settled.selects(&survey_id, record.identity, record.size),
            "the selection outlives the settlement"
        );
        // A record another release wrote is refused by its label, naming the recovery.
        std::fs::write(
            store.join(SETTLED),
            b"{\"schema\":\"af/ProjectSurveysSettled@1\",\"survey_ids\":[]}",
        )
        .unwrap();
        let error = read_settled(&cas).unwrap_err();
        assert!(
            error.contains("superseded version") && error.contains("delete it"),
            "{error}"
        );
    }

    /// A settlement whose install and put-back both fail to sync reports both failures and
    /// leaves the Survey an unverified settlement: the error says so, and nothing claims the
    /// Survey settled durably; the earlier record is what the directory holds.
    #[test]
    fn a_settlement_whose_rollback_sync_fails_too_is_an_unverified_settlement() {
        let (_root, store, survey_id) = store_with(b"settled");
        let cas = open_cas(&store).unwrap();
        let earlier = format!("sha256:{}", "0".repeat(64));
        record_settled(&cas, &earlier).unwrap();
        let both = |_: &CasDirectories, of: SyncOf| Err(format!("injected at {of:?}"));
        let error = record_settled_with(&cas, &survey_id, both).unwrap_err();
        assert!(
            error.starts_with("syncing `settled.json`: injected at Install")
                && error.contains("syncing the put-back: injected at Rollback")
                && error.contains(UNVERIFIED_SETTLEMENT)
                && error.contains("its record is not deleted"),
            "{error}"
        );
        assert_eq!(settled_in(&store), [earlier.clone()], "put back, unsynced");
        // Without an earlier record, the new one is removed and the same failure is reported.
        let (_root, store, survey_id) = store_with(b"unsettled");
        let cas = open_cas(&store).unwrap();
        let error = record_settled_with(&cas, &survey_id, both).unwrap_err();
        assert!(error.contains(UNVERIFIED_SETTLEMENT), "{error}");
        assert!(!store.join(SETTLED).exists());
    }

    /// A Project Store whose CAS holds `bytes` as the object of a Survey the writer published
    /// and never recorded: its log empty, its pending entry naming the object by device, inode
    /// and length, as the Surveyor writes it.
    fn unrecorded_store(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let store = std::fs::canonicalize(root.path()).unwrap().join("store");
        Cas::open(store.join("cas")).unwrap();
        EventStore::open(store.join("events.sqlite")).unwrap();
        let survey_id = review_store::canonical::blob_content_id(bytes);
        let kept = super::super::pending::begin(&store, &survey_id, bytes.len() as u64).unwrap();
        super::super::pending::publish(&store, &survey_id, bytes, kept).unwrap();
        (root, store, survey_id)
    }

    fn planned(look: &Look, wanted: fn(&Job) -> bool) -> Vec<&Job> {
        look.recovery
            .iter()
            .map(|pending| &pending.job)
            .filter(|job| wanted(job))
            .collect()
    }

    /// Apply checks again, under the lock, everything inventory required of a pending object:
    /// its device, its inode and its length. The object grown or truncated in place since the
    /// plan is `skipped: object changed` and nothing is deleted; its entry is kept while it
    /// describes an object still there. Back at its length, the planned job deletes it, and the
    /// entry is cleared after it.
    #[test]
    fn an_unrecorded_object_changed_in_place_since_the_plan_is_skipped_and_kept() {
        use std::io::Write as _;
        let (_root, store, survey_id) = unrecorded_store(&[b'p'; 100]);
        let object = object_path(&store, &survey_id);
        let entry = super::super::pending::location(&store, &survey_id);
        let looked = look(&store).unwrap();
        let unrecorded = planned(&looked, |job| matches!(job, Job::Unrecorded { .. }));
        let clear = planned(&looked, |job| matches!(job, Job::Publication { .. }));
        assert_eq!((unrecorded.len(), clear.len()), (1, 1), "{looked:?}");
        let (unrecorded, clear) = (unrecorded[0], clear[0]);
        assert!(matches!(unrecorded, Job::Unrecorded { length: 100, .. }));

        std::fs::OpenOptions::new()
            .append(true)
            .open(&object)
            .unwrap()
            .write_all(b"grown")
            .unwrap();
        let error = execute(unrecorded).unwrap_err();
        assert!(
            error.contains(&format!(
                "{OBJECT_CHANGED}: what its CAS name holds is 105 bytes"
            )),
            "{error}"
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(&object)
            .unwrap()
            .set_len(50)
            .unwrap();
        let error = execute(unrecorded).unwrap_err();
        assert!(error.contains(OBJECT_CHANGED), "{error}");
        assert!(object.is_file(), "nothing is deleted");
        assert!(entry.is_file());

        std::fs::OpenOptions::new()
            .write(true)
            .open(&object)
            .unwrap()
            .set_len(100)
            .unwrap();
        assert!(
            execute(clear).unwrap_err().contains("kept: "),
            "first the object"
        );
        assert!(execute(unrecorded).unwrap().bytes > 0);
        assert!(!object.exists());
        assert!(execute(clear).unwrap().bytes > 0);
        assert!(!entry.exists());
    }

    /// An entry is cleared only once no file with its inode is left: the inode decides, never
    /// the length. Truncated in place, grown in place and then moved into a pass's container
    /// at its new length, the object is still present, so the clear keeps the entry with
    /// [`STILL_PRESENT`] and the look lists the object `foreign`; once the operator deletes it,
    /// the clear goes through.
    #[test]
    fn an_entry_stays_while_its_inode_is_present_at_any_length() {
        let (_root, store, survey_id) = unrecorded_store(&[b'r'; 80]);
        let object = object_path(&store, &survey_id);
        let entry = super::super::pending::location(&store, &survey_id);
        let resize = |at: &Path, length: u64| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(at)
                .unwrap()
                .set_len(length)
                .unwrap();
        };
        let dead = store.join("cas/removing").join(P1);
        for (case, at) in [
            ("truncated", object.clone()),
            ("grown", object.clone()),
            ("moved", dead.join(&survey_id)),
        ] {
            match case {
                "truncated" => resize(&object, 40),
                "grown" => resize(&object, 120),
                _ => {
                    std::fs::create_dir_all(&dead).unwrap();
                    std::fs::rename(&object, &at).unwrap();
                }
            }
            let looked = look(&store).unwrap();
            let foreign: Vec<&Path> = looked
                .left
                .iter()
                .filter(|left| left.reason == LeftReason::Foreign)
                .map(|left| left.path.as_path())
                .collect();
            assert_eq!(foreign, [at.as_path()], "{case}: {looked:?}");
            let clear = planned(&looked, |job| matches!(job, Job::Publication { .. }));
            assert_eq!(clear.len(), 1, "{case}: {looked:?}");
            let error = execute(clear[0]).unwrap_err();
            assert!(error.contains(STILL_PRESENT), "{case}: {error}");
            assert!(
                entry.is_file() && at.is_file(),
                "{case}: nothing is removed"
            );
        }
        // The operator deletes it: no file with its inode is left, and the entry goes.
        std::fs::remove_file(dead.join(&survey_id)).unwrap();
        let looked = look(&store).unwrap();
        let clear = planned(&looked, |job| matches!(job, Job::Publication { .. }));
        assert!(execute(clear[0]).unwrap().bytes > 0);
        assert!(!entry.exists());
    }

    /// A pending object whose removal stopped after its rename into the pass's container and
    /// before its unlink keeps its recovery: its entry is not cleared while the object is in the
    /// container, the next look plans that object's recovery (never `foreign`) and then the
    /// container, and the entry is cleared only once the object is gone.
    #[test]
    fn an_interrupted_pending_removal_is_recovered_before_its_entry_is_cleared() {
        let (_root, store, survey_id) = unrecorded_store(&[b'q'; 64]);
        let object = object_path(&store, &survey_id);
        let entry = super::super::pending::location(&store, &survey_id);
        let looked = look(&store).unwrap();
        let clear = planned(&looked, |job| matches!(job, Job::Publication { .. }))[0].clone();
        // As a pass that died between its rename and its unlink leaves it.
        let dead = store.join("cas/removing").join(P1);
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::rename(&object, dead.join(&survey_id)).unwrap();
        let error = execute(&clear).unwrap_err();
        assert!(error.contains("kept: "), "{error}");
        assert!(entry.is_file(), "the entry outlives no object it describes");

        let again = look(&store).unwrap();
        assert!(again.left.is_empty(), "never foreign: {:?}", again.left);
        let jobs: Vec<&Job> = again.recovery.iter().map(|pending| &pending.job).collect();
        assert!(
            matches!(
                jobs.as_slice(),
                [
                    Job::MovedUnrecorded { length: 64, .. },
                    Job::Container { .. },
                    Job::Publication { .. }
                ]
            ),
            "{jobs:?}"
        );
        assert!(again.surveys.is_empty(), "its bytes stay the Store's");
        for job in jobs {
            assert!(execute(job).is_ok(), "{job:?}");
        }
        assert!(!dead.exists() && !entry.exists());
        assert!(
            settled_in(&store).is_empty(),
            "no tombstone, so nothing settled"
        );
        assert_eq!(look(&store).unwrap(), Look::default());
    }

    /// What a writer stopped before its staged object's identity, or after it and before its
    /// CAS name, leaves in `pending-surveys` is the writer's own: recovery reclaims the staged
    /// bytes, only while they are the file the look stated, and then clears the entry, which
    /// stays while the staged object it names is there. Nothing is ever at the CAS name.
    #[test]
    fn a_staged_object_is_reclaimed_before_its_entry_and_never_at_a_cas_name() {
        let bytes: &[u8] = b"{\"staged\":\"1\"}";
        for named in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = std::fs::canonicalize(root.path()).unwrap().join("store");
            Cas::open(store.join("cas")).unwrap();
            EventStore::open(store.join("events.sqlite")).unwrap();
            let survey_id = review_store::canonical::blob_content_id(bytes);
            let staged = super::super::pending::staged_location(&store, &survey_id);
            let entry = super::super::pending::location(&store, &survey_id);
            let object = object_path(&store, &survey_id);
            super::super::pending::begin(&store, &survey_id, bytes.len() as u64).unwrap();
            if named {
                // As a writer that stopped after the identity and before the CAS name leaves it.
                super::super::pending::publish(&store, &survey_id, bytes, None).unwrap();
                std::fs::rename(&object, &staged).unwrap();
            } else {
                // As a writer that stopped after the bytes and before their identity leaves it.
                std::fs::write(&staged, bytes).unwrap();
            }
            assert!(!object.exists(), "named {named}");
            let looked = look(&store).unwrap();
            let jobs: Vec<&Job> = looked.recovery.iter().map(|pending| &pending.job).collect();
            assert!(
                matches!(
                    jobs.as_slice(),
                    [Job::Staged { .. }, Job::Publication { .. }]
                ),
                "named {named}: {jobs:?}"
            );
            assert_eq!(
                looked.recovery[0].counted,
                Counted::Store,
                "its bytes are the Project Store's"
            );
            assert!(
                looked.surveys.is_empty() && looked.left.is_empty(),
                "{looked:?}"
            );
            if named {
                let kept = execute(jobs[1]).unwrap_err();
                assert!(kept.contains("kept: "), "first the staged object: {kept}");
            }
            // Replaced since the look, born later: skipped, nothing deleted.
            let stated = Identity::born_of(&staged).unwrap();
            replace_born_later(&staged, bytes);
            let skipped = execute(jobs[0]).unwrap_err();
            assert!(skipped.contains(OBJECT_CHANGED), "{skipped}");
            assert!(staged.is_file());
            let again = look(&store).unwrap();
            let jobs: Vec<&Job> = again.recovery.iter().map(|pending| &pending.job).collect();
            assert!(
                matches!(&jobs[0], Job::Staged { identity, .. } if *identity != stated),
                "{jobs:?}"
            );
            for job in jobs {
                assert!(execute(job).unwrap().bytes > 0, "named {named}: {job:?}");
            }
            assert!(!staged.exists() && !entry.exists() && !object.exists());
            assert_eq!(look(&store).unwrap(), Look::default(), "named {named}");
        }
    }

    /// A pending-publication entry is named by its Survey's hex and `.json`, nothing else.
    #[test]
    fn a_pending_entry_is_named_by_its_survey() {
        let hex = "a".repeat(64);
        assert_eq!(
            publication_survey(OsStr::new(&format!("{hex}.json"))),
            Some(format!("sha256:{hex}"))
        );
        for name in [hex.clone(), format!("{hex}.tmp"), "x.json".into()] {
            assert_eq!(publication_survey(OsStr::new(&name)), None, "{name}");
        }
    }

    #[test]
    fn a_dead_passs_container_is_settled_through_the_next_passs_own() {
        let (_root, store, survey_id) = store_with(b"one");
        let object = object_path(&store, &survey_id);
        let dead = store.join("cas/removing/dead");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::rename(&object, dead.join(&survey_id)).unwrap();
        let identity = Identity::born_of(&dead.join(&survey_id)).unwrap();
        let other = Identity {
            inode: identity.inode + 1,
            ..identity
        };
        let error =
            settle(&store, OsStr::new("dead"), &survey_id, other, true, |_| {}).unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert!(
            dead.join(&survey_id).is_file(),
            "put back where it was left"
        );
        let (files, containers) = under_removing(&store);
        assert_eq!(files, [format!("dead/{survey_id}")]);
        assert_eq!(
            containers,
            ["dead"],
            "no container of the refused pass is left"
        );
        // Replaced in the next pass's container before its check: refused the same way.
        let error = settle(
            &store,
            OsStr::new("dead"),
            &survey_id,
            identity,
            true,
            |step| {
                if step == Before::Check {
                    let (files, _) = under_removing(&store);
                    let inside = store.join("cas/removing").join(
                        files
                            .iter()
                            .find(|file| !file.starts_with("dead/"))
                            .unwrap(),
                    );
                    replace_born_later(&inside, b"one");
                }
            },
        )
        .unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert_eq!(std::fs::read(dead.join(&survey_id)).unwrap(), b"one");
        // The same device and inode born later is another file too, refused the same way.
        let identity = Identity::born_of(&dead.join(&survey_id)).unwrap();
        let later = Identity {
            born: identity.born.map(|born| born + 1),
            ..identity
        };
        let error =
            settle(&store, OsStr::new("dead"), &survey_id, later, true, |_| {}).unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert!(dead.join(&survey_id).is_file());
        let container = Identity::born_of(&dead).unwrap();
        let bytes = settle(
            &store,
            OsStr::new("dead"),
            &survey_id,
            identity,
            true,
            |_| {},
        )
        .unwrap();
        assert!(bytes > 0);
        assert_eq!(
            under_removing(&store),
            (Vec::new(), vec!["dead".to_string()]),
            "only the planned container entry removes the dead container"
        );
        assert!(!object.exists(), "its CAS name is not touched");
        // Settled again, the record is gone since the plan: failed, nothing freed.
        let error = settle(
            &store,
            OsStr::new("dead"),
            &survey_id,
            identity,
            true,
            |_| {},
        )
        .unwrap_err();
        assert!(error.starts_with(GONE), "{error}");
        remove_container(&store, OsStr::new("dead"), container).unwrap();
        assert_eq!(under_removing(&store), (Vec::new(), Vec::new()));
    }

    fn file(container: Option<&str>, name: &[u8], inode: u64) -> Found {
        Found {
            container: container.map(OsString::from),
            name: OsStr::from_bytes(name).to_os_string(),
            kind: FoundKind::File,
            identity: Identity {
                device: 1,
                inode,
                born: Some(1),
            },
            bytes: 4096,
            size: 10,
        }
    }

    /// Two pass containers' names, as [`pass_id`] draws them.
    const P1: &str = "0123456789abcdef0123456789abcdef";
    const P2: &str = "fedcba9876543210fedcba9876543210";

    /// Leftovers are judged with their raw names, the container's as the file's: a regular file
    /// in a pass container named by a tombstoned Survey is its record, moved; a name that is
    /// not UTF-8, a file's or its container's, is `unreadable name`, and everything else, a file
    /// in a container whose name is no pass ID included, is `foreign`, each left with its path
    /// kept byte for byte.
    #[test]
    fn leftovers_are_judged_by_their_raw_names() {
        let store = Path::new("/state/af/project/aa");
        let collected = format!("sha256:{}", "ab".repeat(32));
        let unnamed = format!("sha256:{}", "cd".repeat(32));
        let found = vec![
            file(Some(P1), collected.as_bytes(), 1),
            file(Some(P2), collected.as_bytes(), 2),
            file(Some(P1), unnamed.as_bytes(), 3),
            file(Some(P1), b"caf\xe9", 4),
            file(None, b"\xff", 5),
            file(None, collected.as_bytes(), 6),
            file(Some(P1), format!("{collected}.replaced").as_bytes(), 7),
            Found {
                kind: FoundKind::Directory,
                ..file(Some(P1), b"nested", 8)
            },
            // A tombstoned Survey's name in a container whose own name is not UTF-8.
            Found {
                container: Some(OsStr::from_bytes(b"\xffpass").to_os_string()),
                ..file(None, collected.as_bytes(), 9)
            },
            // And in one whose name is UTF-8 but no pass ID.
            file(Some("not-a-pass"), collected.as_bytes(), 10),
        ];
        let selected = BTreeMap::from([(
            collected.clone(),
            (
                Identity {
                    device: 1,
                    inode: 1,
                    born: Some(1),
                },
                10,
            ),
        )]);
        let (moved, left) = judge_leftovers(
            store,
            found.clone(),
            &BTreeSet::from([collected.clone()]),
            &selected,
            &BTreeMap::new(),
        );
        assert_eq!(
            moved.get(&collected),
            Some(&Moved {
                container: P1.into(),
                identity: Identity {
                    device: 1,
                    inode: 1,
                    born: Some(1),
                },
                bytes: 4096,
                size: 10,
                pending: false,
            })
        );
        assert_eq!(moved.len(), 1);
        let reasons: Vec<LeftReason> = left.iter().map(|left| left.reason).collect();
        use LeftReason::{Foreign, UnreadableName};
        assert_eq!(
            reasons,
            [
                Foreign,
                Foreign,
                UnreadableName,
                UnreadableName,
                Foreign,
                Foreign,
                Foreign,
                UnreadableName,
                Foreign
            ]
        );
        let mut raw = format!("/state/af/project/aa/cas/removing/{P1}/caf").into_bytes();
        raw.push(0xe9);
        assert_eq!(
            left[2].path.as_os_str().as_bytes(),
            raw,
            "the name is kept raw"
        );
        assert!(left[2].detail.contains("caf\u{fffd}"), "{}", left[2].detail);
        assert_eq!(left[6].bytes, 0, "a directory is no file's bytes");
        let mut raw = b"/state/af/project/aa/cas/removing/\xffpass/".to_vec();
        raw.extend_from_slice(collected.as_bytes());
        assert_eq!(left[7].path.as_os_str().as_bytes(), raw, "kept raw");
        assert!(
            left[7].detail.contains("its container is not UTF-8"),
            "{}",
            left[7].detail
        );
        assert_eq!(
            left[7].bytes, 4096,
            "its bytes are listed, never the Survey's"
        );
        assert!(left[8].detail.contains("no pass ID"), "{}", left[8].detail);
        // Without its collection's selection, or with the same device and inode born later,
        // the file is no record of the Survey: foreign, whatever its bytes.
        let later = BTreeMap::from([(
            collected.clone(),
            (
                Identity {
                    device: 1,
                    inode: 1,
                    born: Some(2),
                },
                10,
            ),
        )]);
        for selected in [BTreeMap::new(), later] {
            let (moved, left) = judge_leftovers(
                store,
                found.clone(),
                &BTreeSet::from([collected.clone()]),
                &selected,
                &BTreeMap::new(),
            );
            assert!(moved.is_empty(), "{moved:?}");
            let first = &left[0];
            assert!(
                first.path.ends_with(format!("{P1}/{collected}"))
                    && first.reason == LeftReason::Foreign
                    && first
                        .detail
                        .contains("not the object the collection of its Survey"),
                "{first:?}"
            );
        }
    }

    /// A file in a pass container named by an unrecorded Survey whose pending-publication entry
    /// describes it (its device, inode and length) is that entry's object, moved by a recovery
    /// that died before its unlink: recovered again, never foreign. Another inode, or the same
    /// inode at another length, is foreign, and so is the described one in a container whose
    /// name is no pass ID.
    #[test]
    fn a_pending_object_in_a_pass_container_is_its_entrys_never_foreign() {
        let store = Path::new("/state/af/project/aa");
        let unrecorded = format!("sha256:{}", "ef".repeat(32));
        let identity = Identity {
            device: 1,
            inode: 1,
            born: Some(1),
        };
        let pending = BTreeMap::from([(unrecorded.clone(), (identity, 10))]);
        let (moved, left) = judge_leftovers(
            store,
            vec![file(Some(P1), unrecorded.as_bytes(), 1)],
            &BTreeSet::new(),
            &BTreeMap::new(),
            &pending,
        );
        assert_eq!(
            moved.get(&unrecorded),
            Some(&Moved {
                container: P1.into(),
                identity,
                bytes: 4096,
                size: 10,
                pending: true,
            })
        );
        assert!(left.is_empty(), "{left:?}");
        for other in [
            file(Some(P1), unrecorded.as_bytes(), 2),
            Found {
                size: 11,
                ..file(Some(P1), unrecorded.as_bytes(), 1)
            },
            file(Some("not-a-pass"), unrecorded.as_bytes(), 1),
        ] {
            let (moved, left) = judge_leftovers(
                store,
                vec![other],
                &BTreeSet::new(),
                &BTreeMap::new(),
                &pending,
            );
            assert!(moved.is_empty(), "{moved:?}");
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].reason, LeftReason::Foreign);
        }
    }

    /// Only a container named by a pass ID that holds nothing is an empty container to remove:
    /// an empty directory of any other name is a leftover.
    #[test]
    fn the_walk_finds_containers_their_files_and_empty_ones() {
        let (_root, store, survey_id) = store_with(b"one");
        let removing = store.join("cas/removing");
        std::fs::create_dir_all(removing.join(P1)).unwrap();
        std::fs::create_dir_all(removing.join("empty")).unwrap();
        std::fs::create_dir_all(removing.join("full")).unwrap();
        std::fs::write(removing.join("full").join(&survey_id), b"one").unwrap();
        std::fs::write(removing.join("loose"), b"x").unwrap();
        let (found, empty) = walk_removing(&open_cas(&store).unwrap()).unwrap();
        let names: Vec<&OsStr> = empty.iter().map(|empty| empty.name.as_os_str()).collect();
        assert_eq!(names, [OsStr::new(P1)]);
        assert_eq!(
            empty[0].identity,
            Identity::born_of(&removing.join(P1)).unwrap()
        );
        let names: Vec<(Option<String>, String, FoundKind)> = found
            .iter()
            .map(|found| {
                (
                    found
                        .container
                        .as_ref()
                        .map(|name| name.to_string_lossy().into_owned()),
                    found.name.to_string_lossy().into_owned(),
                    found.kind,
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                (None, "empty".into(), FoundKind::Directory),
                (Some("full".into()), survey_id.clone(), FoundKind::File),
                (None, "loose".into(), FoundKind::File),
            ]
        );
        let (_, left) = judge_leftovers(
            &store,
            found,
            &BTreeSet::from([survey_id.clone()]),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(
            left.iter().all(|left| left.reason == LeftReason::Foreign),
            "{left:?}"
        );
        assert_eq!(left.len(), 3, "{left:?}");
    }

    /// The plan's judgement compares the record with what inventory stated: another inode at
    /// its name, or a file where inventory found none, is refused, and never adopted. What it
    /// takes or refuses beyond that is the verdict inventory's one read reached.
    #[test]
    fn a_record_replaced_since_inventory_is_refused_by_the_judgement() {
        let (_root, store, survey_id) = store_with(b"one");
        let object = object_path(&store, &survey_id);
        let recorded = SurveyRecordedPayloadV1 {
            survey_id: survey_id.clone(),
            ..recorded(1, SurveyCompletenessV1::Complete, NOW_S)
        };
        let verified = verify(&store, &recorded);
        let measured = verified.identity.unwrap();
        let live = |identity: Option<Identity>, verdict: Verdict| Live {
            survey_id: survey_id.clone(),
            cutoff_ms: verified.cutoff_ms(&recorded),
            protected: false,
            identity,
            verdict,
        };
        let unreadable = Verdict::Unreadable(
            "its bytes are not a Survey record: a syntax error at line 1, column 1".into(),
        );
        assert_eq!(verified.verdict, unreadable);
        assert_eq!(
            verified.cutoff_ms(&recorded),
            NOW_S * 1000,
            "no record vouches for a time"
        );
        let as_read = live(Some(measured), unreadable.clone());
        assert_eq!(
            judge(&store, &as_read, Some(measured)),
            Judged::Take {
                unreadable: Some(
                    "its bytes are not a Survey record: a syntax error at line 1, column 1".into()
                ),
                identity: Some(measured),
            }
        );
        // A verdict that its event disagrees with its record refuses it, reading nothing more.
        assert_eq!(
            judge(
                &store,
                &live(Some(measured), Verdict::Disagrees("cutoff_unix_s")),
                Some(measured)
            ),
            Judged::Refused(disagrees("cutoff_unix_s"))
        );
        // A byte-identical copy, another inode, takes the name.
        std::fs::rename(&object, object.with_extension("aside")).unwrap();
        std::fs::write(&object, b"one").unwrap();
        for inventoried in [Some(measured), None] {
            let Judged::Refused(why) =
                judge(&store, &live(inventoried, unreadable.clone()), inventoried)
            else {
                panic!("{inventoried:?} is refused");
            };
            assert!(why.starts_with(REPLACED_SINCE_INVENTORY), "{why}");
        }
        assert_eq!(
            std::fs::read(&object).unwrap(),
            b"one",
            "nothing is touched"
        );
        // Gone since inventory stated it: replaced by nothing, refused the same way.
        std::fs::remove_file(&object).unwrap();
        assert!(matches!(
            judge(&store, &as_read, Some(measured)),
            Judged::Refused(why) if why.starts_with(REPLACED_SINCE_INVENTORY)
        ));
        // Absent then and now: collected `unreadable`, as before.
        let absent = verify(&store, &recorded);
        assert_eq!(absent.identity, None);
        assert_eq!(
            judge(&store, &live(None, absent.verdict), None),
            Judged::Take {
                unreadable: Some("its record is not in the CAS".into()),
                identity: None,
            }
        );
    }

    /// An empty container of a dead pass is removed only while it is the directory the walk
    /// stated and still holds nothing.
    #[test]
    fn an_empty_container_is_removed_only_as_it_was_stated() {
        let (_root, store, survey_id) = store_with(b"one");
        let removing = store.join("cas/removing");
        std::fs::create_dir_all(removing.join(P1)).unwrap();
        let (_, empty) = walk_removing(&open_cas(&store).unwrap()).unwrap();
        let stated = empty[0].identity;
        // Another directory took its name.
        std::fs::rename(removing.join(P1), removing.join(P2)).unwrap();
        std::fs::create_dir(removing.join(P1)).unwrap();
        let error = remove_container(&store, OsStr::new(P1), stated).unwrap_err();
        assert!(error.starts_with(REPLACED_SINCE_INVENTORY), "{error}");
        assert!(removing.join(P1).is_dir());
        // A file arrived in it since: `rmdir` refuses, and it is left.
        std::fs::write(removing.join(P2).join(&survey_id), b"one").unwrap();
        let error = remove_container(&store, OsStr::new(P2), stated).unwrap_err();
        assert!(error.starts_with("refused: removing it"), "{error}");
        assert!(removing.join(P2).join(&survey_id).is_file());
        std::fs::remove_file(removing.join(P2).join(&survey_id)).unwrap();
        // As it was stated and empty: removed.
        remove_container(&store, OsStr::new(P2), stated).unwrap();
        assert!(!removing.join(P2).exists());
        // Gone since inventory: a failure that frees nothing, never a removal of nothing.
        let error = remove_container(&store, OsStr::new(P2), stated).unwrap_err();
        assert!(error.starts_with(GONE), "{error}");
    }

    #[test]
    fn writer_reader_and_collector_share_one_ceiling() {
        assert_eq!(MAX_SURVEY_BYTES, 1_048_576);
        assert_eq!(Policy::default().max_survey_bytes, MAX_SURVEY_BYTES);
    }

    #[test]
    fn a_record_over_the_ceiling_is_oversize_and_read_at_most_one_byte_beyond() {
        let ceiling = MAX_SURVEY_BYTES as usize;
        let (_root, store, survey_id) = store_with(&vec![b' '; ceiling]);
        let record = locate(&store, &survey_id).unwrap().unwrap();
        assert_eq!(record.oversize(), None);
        assert_eq!(record.read().unwrap().len(), ceiling);
        let (_root, store, survey_id) = store_with(&vec![b' '; 2 * ceiling]);
        let record = locate(&store, &survey_id).unwrap().unwrap();
        let why = record.oversize().unwrap();
        assert!(
            why.starts_with(&format!("refused (oversize, {} bytes)", 2 * ceiling)),
            "{why}"
        );
        assert_eq!(record.read().unwrap().len(), ceiling + 1);
    }

    #[test]
    fn the_lock_predicate_names_why_a_store_is_held() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, store, _) = store_with(b"one");
        assert_eq!(lock_available(&store), Ok(()));
        let lock = take_lock(&store).unwrap();
        assert_eq!(lock_available(&store), Err(HELD.to_string()));
        drop(lock);
        let path = store.join(LOCK);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        if std::fs::OpenOptions::new().write(true).open(&path).is_err() {
            let reason = lock_available(&store).unwrap_err();
            assert!(reason.starts_with("lock unavailable: "), "{reason}");
            assert_eq!(hold(&store).unwrap().err(), Some(reason));
        }
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
        let long = "é".repeat(200);
        let cut = bounded(long);
        assert!(cut.len() <= MAX_UNREADABLE_BYTES && cut.chars().all(|c| c == 'é'));
    }
}
