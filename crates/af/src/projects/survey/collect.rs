//! Survey collection (ADR-0152 clause 13) and the bounded patterns of clause 14.
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
//! its lock, into what the plan needs (its Surveys, the deletions an interruption left, the files
//! it leaves), [`judge`] reads a Survey the plan picks, and the preview prints that plan while
//! the applied sweep runs each of its [`Job`]s with [`execute`], which reports what it found
//! instead of choosing again. Held is decided by one predicate, [`lock_available`]. Collection
//! appends `SurveyCollected@1` to the Project Store's log before the record leaves the CAS. The
//! tombstones are a run of their own, `survey-collection`, beside the run `project` the
//! Surveyor appends its chain to: a `SurveyRecorded@1` a tombstone names replays as collected,
//! and `previous_survey_id` is a name, never a reachability edge.
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
//! deleted. Everything else under `cas/removing`, a name that is not UTF-8 included, is walked
//! with its raw name and left. A link anywhere on the way, or a directory on another device than
//! the Store, refuses the Survey, which is reported and keeps its record and its place in the
//! log. So does a record the file system will not let af read, and one over
//! [`MAX_SURVEY_BYTES`], the ceiling the Surveyor writes within, which is left as `oversize`; a
//! record within it whose bytes are no Survey is collected with an `unreadable` tombstone, since
//! reclaiming corrupt bytes is the budget's job.
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
    MAX_SURVEY_BYTES, ProjectSurveyV1, SurveyCollectedHistoryV1, SurveyCollectedPayloadV1,
    SurveyCollectedTreeV1, SurveyCompletenessV1, SurveyReasonV1, SurveyRecordedPayloadV1,
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
/// The directory of a Project Store's `cas` that holds the passes' containers.
const REMOVING: &str = "removing";
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

/// Allocated bytes a `stat` reports.
#[allow(clippy::unnecessary_cast)]
fn allocated(stat: &nix::sys::stat::FileStat) -> u64 {
    (stat.st_blocks as u64).saturating_mul(512)
}

fn kind_of(stat: &nix::sys::stat::FileStat) -> nix::sys::stat::SFlag {
    use nix::sys::stat::SFlag;
    SFlag::from_bits_truncate(stat.st_mode as nix::libc::mode_t) & SFlag::S_IFMT
}

/// What `name` of `directory` is, not following it; `None` when there is nothing. The name is
/// taken raw, so one that is not UTF-8 is stated as it is.
fn stat_at(directory: &OwnedFd, name: &OsStr) -> Result<Option<nix::sys::stat::FileStat>, String> {
    use nix::fcntl::AtFlags;
    match nix::sys::stat::fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(errno) => Err(format!("inspecting its record: {errno}")),
    }
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

    /// Unlink `entry` only while it is `identity`: stated with `fstatat` relative to the
    /// container's own descriptor, without following it, immediately before `unlinkat` relative
    /// to the same descriptor. Returns the allocated bytes of that inode.
    fn unlink_checked(&self, entry: &OsStr, identity: Identity) -> Result<u64, Checked> {
        use nix::sys::stat::SFlag;
        use nix::unistd::{UnlinkatFlags, unlinkat};
        let stat = match stat_at(&self.directory, entry) {
            Ok(Some(stat)) => stat,
            Ok(None) => return Err(Checked::Gone),
            Err(why) => return Err(Checked::Failed(why)),
        };
        if kind_of(&stat) != SFlag::S_IFREG || Identity::of_stat(&stat) != identity {
            return Err(Checked::Replaced);
        }
        unlinkat(&self.directory, entry, UnlinkatFlags::NoRemoveDir)
            .map_err(|errno| Checked::Failed(errno.to_string()))?;
        Ok(allocated(&stat))
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
    use nix::sys::stat::SFlag;
    let (prefix_name, name) = split(survey_id)?;
    let cas = open_cas(store)?;
    let Some(prefix) = open_prefix(&cas.objects, prefix_name, cas.device)? else {
        return Ok(None);
    };
    let Some(stat) = stat_at(&prefix, OsStr::new(name))? else {
        return Ok(None);
    };
    match kind_of(&stat) {
        SFlag::S_IFREG => {}
        SFlag::S_IFLNK => {
            return Err(
                "its record is a symbolic link; af never reads or removes through one".into(),
            );
        }
        _ => return Err("its record is not a regular file".into()),
    }
    Ok(Some(Record {
        cas,
        prefix,
        name: name.to_string(),
        survey_id: survey_id.to_string(),
        object: object_path(store, survey_id),
        identity: Identity::of_stat(&stat),
        size: u64::try_from(stat.st_size).unwrap_or(0),
        bytes: allocated(&stat),
    }))
}

/// How a record of `size` bytes is refused: more than a Survey can hold.
fn oversize(size: u64) -> String {
    format!(
        "refused (oversize, {size} bytes): its record holds more than the {MAX_SURVEY_BYTES} \
         bytes a Survey can hold, so it is left and adds no tombstone"
    )
}

/// A debug build's fixture may replace a record between its identity read and its rename, as
/// another process could: `AF_TEST_SURVEY_REPLACE_BEFORE_RENAME=<directory>` moves the record
/// into that directory and puts a byte-identical copy, another inode, at its name. Release
/// binaries never read it.
fn replace_before_rename(object: &Path) {
    #[cfg(debug_assertions)]
    if let Some(aside) = std::env::var_os("AF_TEST_SURVEY_REPLACE_BEFORE_RENAME") {
        let (Ok(bytes), Some(name)) = (std::fs::read(object), object.file_name()) else {
            return;
        };
        if std::fs::rename(object, Path::new(&aside).join(name)).is_ok() {
            let _ = std::fs::write(object, bytes);
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = object;
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
        if Identity::of_descriptor(&file)? != self.identity {
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
    /// is [`REPLACED`].
    fn remove(self) -> Result<u64, String> {
        let object = self.object.clone();
        self.remove_with(|step, _| {
            if step == Before::Rename {
                replace_before_rename(&object);
            }
        })
    }

    fn remove_with(self, mut hook: impl FnMut(Before, &Path)) -> Result<u64, String> {
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
            // Gone since it was found: nothing of it is left to delete.
            Err(rustix::io::Errno::NOENT) => {
                container.close();
                return Ok(0);
            }
            Err(errno) => {
                container.close();
                return Err(format!("moving its record aside: {errno}"));
            }
        }
        hook(Before::Check, &inside);
        let removed = match container.unlink_checked(entry, self.identity) {
            Ok(bytes) => Ok(bytes),
            Err(Checked::Gone) => Ok(0),
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
    /// moved, or anything under `cas/removing` no tombstone names. Never deleted.
    Foreign,
    /// A record over [`MAX_SURVEY_BYTES`].
    Oversize,
    /// A name under `cas/removing` that is not UTF-8, so no Survey's.
    UnreadableName,
}

impl LeftReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            LeftReason::Held => "held",
            LeftReason::Foreign => "foreign",
            LeftReason::Oversize => "oversize",
            LeftReason::UnreadableName => "unreadable_name",
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
    /// Delete the record a tombstoned Survey still holds at its CAS name.
    AtName {
        store: PathBuf,
        survey_id: String,
        identity: Identity,
    },
    /// Delete a tombstoned Survey's record a dead pass left in its container.
    Moved {
        store: PathBuf,
        container: OsString,
        survey_id: String,
        identity: Identity,
    },
}

/// A deletion an interruption left: the plan's `recovery`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    /// The Survey's CAS object, as the removal names it.
    pub(crate) object: PathBuf,
    /// The allocated bytes it frees.
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
}

/// An uncollected Survey of the chain, oldest first: what `keep_surveys` counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Live {
    pub(crate) survey_id: String,
    pub(crate) cutoff_ms: u64,
    /// The newest complete Survey: never collected.
    pub(crate) protected: bool,
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
    /// The deletions an interruption left, in the order the tombstones were appended.
    pub(crate) recovery: Vec<Pending>,
    /// What it leaves: foreign files and unreadable names.
    pub(crate) left: Vec<Left>,
    /// What cannot be judged, with why: a failure of the sweep.
    pub(crate) refused: Vec<(PathBuf, String)>,
    /// Containers of dead passes that hold nothing: removed by the applied sweep.
    pub(crate) empty_containers: Vec<OsString>,
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
    identity: Identity,
    bytes: u64,
}

/// A tombstoned Survey's record a dead pass moved into its container.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Moved {
    container: OsString,
    identity: Identity,
    bytes: u64,
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
    use nix::sys::stat::SFlag;
    let Some(stat) = stat_at(directory, &name)? else {
        return Ok(None);
    };
    Ok(Some(Found {
        container: container.map(OsStr::to_os_string),
        kind: match kind_of(&stat) {
            SFlag::S_IFREG => FoundKind::File,
            SFlag::S_IFDIR => FoundKind::Directory,
            _ => FoundKind::Other,
        },
        identity: Identity::of_stat(&stat),
        bytes: allocated(&stat),
        name,
    }))
}

/// Walk `cas/removing` with raw names: every name in it, and in each directory below it (a
/// container) every name there, plus the containers that hold nothing. Reads only.
fn walk_removing(cas: &CasDirectories) -> Result<(Vec<Found>, Vec<OsString>), String> {
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
        if inside.is_empty() {
            empty.push(name.clone());
        }
        for entry in inside {
            if let Some(file) = found(&container, Some(&name), entry).map_err(listing)? {
                all.push(file);
            }
        }
    }
    Ok((all, empty))
}

/// Judge what [`walk_removing`] found, with raw names and nothing read: a regular file in a
/// container named by a tombstoned Survey is that Survey's record, moved; everything else is
/// left, a name that is not UTF-8 as `unreadable name` and the rest as `foreign`.
fn judge_leftovers(
    store: &Path,
    found: Vec<Found>,
    collected: &BTreeSet<String>,
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
        if file.kind != FoundKind::File {
            leave(LeftReason::Foreign, "it is not a regular file");
            continue;
        }
        if split(name).is_err() {
            leave(LeftReason::Foreign, "it names no Survey, so it is not af's");
            continue;
        }
        if !collected.contains(name) {
            leave(LeftReason::Foreign, "no tombstone names its Survey");
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
            },
        );
    }
    (moved, left)
}

/// Read the Project Store at `store` once, without its lock, for a sweep's plan and for `af
/// storage`: its Surveys at their CAS names (a record that cannot be reached without a link is
/// listed in use with nothing measured through the link, and so is one over
/// [`MAX_SURVEY_BYTES`]), whether its lock is held, its uncollected Surveys, and what an
/// interruption left: per tombstoned Survey, in tombstone order, its record moved into a dead
/// pass's container (a file that took its CAS name since is `foreign`), otherwise its record
/// still at its name, which is its record only when its bytes hash to its ID (any other file
/// there is `foreign`). Writes nothing but the lock file every holder of the Store creates.
pub(crate) fn look(store: &Path) -> Result<Look, String> {
    let events = EventStore::open_read_only(store.join("events.sqlite"))
        .map_err(|error| error.to_string())?;
    let log = Log::fold(&events.replay(RUN_ID).map_err(|error| error.to_string())?)?;
    let order: Vec<String> = tombstones(&events)?
        .into_iter()
        .map(|tombstone| tombstone.survey_id)
        .collect();
    let collected: BTreeSet<String> = order.iter().cloned().collect();
    let mut look = Look {
        held: lock_available(store).err(),
        ..Look::default()
    };
    let walked = open_cas(store).and_then(|cas| walk_removing(&cas));
    let (found, empty) = walked.unwrap_or_else(|why| {
        look.refused.push((
            store.join("cas").join(REMOVING),
            format!("what is under `cas/{REMOVING}` is left: {why}"),
        ));
        (Vec::new(), Vec::new())
    });
    look.empty_containers = empty;
    let (moved, left) = judge_leftovers(store, found, &collected);
    look.left = left;
    // What each collected Survey left, in the order the tombstones were appended: its record
    // moved into a dead pass's container, which is its bytes, or its record at its name. A file
    // at its name is its record only while none was moved and its bytes are the record's; any
    // other is somebody else's, durably: listed `foreign`, never the Survey's, never deleted.
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
            Ok(Some(record)) => match record.is_the_record() {
                Ok(true) => look.recovery.push(Pending {
                    object,
                    bytes: record.bytes,
                    counted: Counted::Survey,
                    job: Job::AtName {
                        store: store.to_path_buf(),
                        survey_id: survey_id.clone(),
                        identity: record.identity,
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
    let protected = newest_complete(&log);
    for recorded in oldest_first(&log) {
        let survey_id = recorded.survey_id.as_str();
        if moved.contains_key(survey_id) || not_records.contains(survey_id) {
            continue;
        }
        let (bytes, identity, refused) = match locate(store, survey_id) {
            Ok(None) => continue,
            Ok(Some(record)) => (record.bytes, Some(record.identity), record.oversize()),
            Err(reason) => (0, None, Some(format!("its record is left: {reason}"))),
        };
        let in_use = if refused.is_some() {
            refused
        } else if look.held.is_some() {
            look.held.clone()
        } else if !collected.contains(survey_id) && protected == Some(survey_id) {
            Some(NEWEST_COMPLETE.to_string())
        } else {
            None
        };
        look.surveys.push(Inventoried {
            object: object_path(store, survey_id),
            bytes,
            last_use_unix_ms: cutoff_ms(recorded),
            in_use,
            identity,
        });
    }
    // A Survey whose record is gone from the CAS still counts against `keep_surveys`.
    look.live = oldest_first(&log)
        .into_iter()
        .filter(|recorded| !collected.contains(&recorded.survey_id))
        .map(|recorded| Live {
            survey_id: recorded.survey_id.clone(),
            cutoff_ms: cutoff_ms(recorded),
            protected: protected == Some(recorded.survey_id.as_str()),
        })
        .collect();
    Ok(look)
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

/// Judge one Survey the plan picks from the Project Store at `store`, reading only: its record
/// found through descriptors and read as its tombstone will read it. A record that cannot be
/// reached without a link or that the file system will not let af read is refused; one over
/// [`MAX_SURVEY_BYTES`] is left `oversize`; one within it whose bytes are no Survey, or that is
/// gone from the CAS, is taken `unreadable`.
pub(crate) fn judge(store: &Path, survey_id: &str) -> Judged {
    let record = match locate(store, survey_id) {
        Ok(record) => record,
        Err(why) => return Judged::Refused(format!("refused: {why}")),
    };
    let Some(record) = record else {
        return Judged::Take {
            unreadable: Some("its record is not in the CAS".into()),
            identity: None,
        };
    };
    if let Some(why) = record.oversize() {
        return Judged::Left(LeftReason::Oversize, why);
    }
    match record.read() {
        Ok(bytes) if bytes.len() as u64 > MAX_SURVEY_BYTES => {
            Judged::Left(LeftReason::Oversize, oversize(bytes.len() as u64))
        }
        Ok(bytes) => Judged::Take {
            unreadable: totals(survey_id, &bytes).err(),
            identity: Some(record.identity),
        },
        Err(error) => Judged::Refused(format!(
            "refused: its record cannot be read, so it is kept: {error}"
        )),
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

/// What a job did: the allocated bytes of what it deleted, and, for a Survey it collected, why
/// its record's bytes were no Survey.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Executed {
    pub(crate) bytes: u64,
    pub(crate) unreadable: Option<String>,
}

/// Run one job of the plan under the Store's lock, taken without waiting. What differs from
/// what the plan found (the lock lost, the Survey protected or collected since, its record
/// replaced, gone or refused) is the error, beginning with the Survey it concerns: the job does
/// nothing more, and the next pass plans again.
pub(crate) fn execute(job: &Job) -> Result<Executed, String> {
    let (store, survey_id) = match job {
        Job::Take {
            store, survey_id, ..
        }
        | Job::AtName {
            store, survey_id, ..
        }
        | Job::Moved {
            store, survey_id, ..
        } => (store.as_path(), survey_id.as_str()),
    };
    let said = |why: String| format!("Survey {survey_id} is {why}");
    let mut held = match hold(store)? {
        Ok(held) => held,
        Err(reason) => {
            return Err(said(format!(
                "not collected: the lock was lost since the plan ({reason}); the next pass \
                 tries again"
            )));
        }
    };
    match job {
        Job::Take { identity, .. } => take(&mut held, survey_id, *identity).map_err(said),
        Job::AtName { identity, .. } => {
            let record = locate(store, survey_id).map_err(|why| said(format!("refused: {why}")))?;
            match record {
                None => Ok(Executed::default()),
                Some(record) if record.identity != *identity => Err(said(format!(
                    "{REPLACED}: another file holds its record's name since the plan, so \
                     nothing is deleted and the next pass plans again"
                ))),
                Some(record) if !record.is_the_record().unwrap_or(false) => Err(said(format!(
                    "{REPLACED}: its record's name holds bytes that are not its record's since \
                     the plan, so nothing is deleted"
                ))),
                Some(record) => record
                    .remove()
                    .map(|bytes| Executed {
                        bytes,
                        unreadable: None,
                    })
                    .map_err(|error| said(refusal(error))),
            }
        }
        Job::Moved {
            container,
            identity,
            ..
        } => settle(store, container, survey_id, *identity, |_| {})
            .map(|bytes| Executed {
                bytes,
                unreadable: None,
            })
            .map_err(|error| said(refusal(error))),
    }
}

/// Collect one live Survey of a held Store: its protection read again under the lock, its
/// record found again and compared with what the plan stated, its tombstone appended, then the
/// record deleted by [`Record::remove`].
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
                Ok(bytes) => totals(survey_id, &bytes),
                Err(error) => {
                    return Err(format!(
                        "refused: its record cannot be read, so it is kept: {error}"
                    ));
                }
            }
        }
    };
    let (tree, history, unreadable) = match read {
        Ok((tree, history)) => (tree, history, None),
        Err(why) => (None, None, Some(why)),
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
    tombstone
        .validate()
        .map_err(|why| format!("refused: {why}"))?;
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
        Some(record) => record.remove().map_err(|error| {
            format!(
                "{}; its tombstone is recorded, and the next pass deletes its record",
                refusal(error)
            )
        })?,
        None => 0,
    };
    Ok(Executed { bytes, unreadable })
}

/// A removal's error as a refusal: [`REPLACED`] already says so.
fn refusal(error: String) -> String {
    if error.starts_with(REPLACED) {
        error
    } else {
        format!("refused: {error}")
    }
}

/// Settle a record a dead pass left as `survey_id` in its `container`, the same sequence a
/// pass's own deletion follows: renamed into a container of this pass's own, stated there with
/// `fstatat` relative to that container and unlinked only when it is the inode the plan found.
/// Another file is set aside and nothing is deleted. The dead container goes once it is empty.
/// Returns the allocated bytes of the inode deleted.
fn settle(
    store: &Path,
    container: &OsStr,
    survey_id: &str,
    identity: Identity,
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
            match own.unlink_checked(entry, identity) {
                Ok(bytes) => Ok(bytes),
                Err(Checked::Gone) => Ok(0),
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
        Err(rustix::io::Errno::NOENT) => Ok(0),
        Err(errno) => Err(format!("moving it into this pass's container: {errno}")),
    };
    own.close();
    let _ = nix::unistd::unlinkat(&removing, container, nix::unistd::UnlinkatFlags::RemoveDir);
    settled
}

/// Remove, under the Store's lock, each container of a dead pass the plan found empty and that
/// still is. Nothing else is touched.
pub(crate) fn tidy(store: &Path, containers: &[OsString]) {
    let Ok(_lock) = take_lock(store) else {
        return;
    };
    let Ok(Some(removing)) = open_cas(store).and_then(|cas| open_removing(&cas, false)) else {
        return;
    };
    for container in containers {
        let _ = nix::unistd::unlinkat(
            &removing,
            container.as_os_str(),
            nix::unistd::UnlinkatFlags::RemoveDir,
        );
    }
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
            .remove_with(|step, inside| {
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
            .remove_with(|step, _| {
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
        assert!(record.remove().unwrap() > 0);
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
            .remove_with(|step, inside| {
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
            .remove_with(|step, _| match step {
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
        let (found, empty) = walk_removing(&open_cas(&store).unwrap()).unwrap();
        assert!(empty.is_empty());
        let (moved, left) = judge_leftovers(&store, found, &BTreeSet::from([survey_id.clone()]));
        assert!(moved.is_empty(), "{moved:?}");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].reason, LeftReason::Foreign);
        assert!(left[0].bytes > 0);
    }

    /// What a dead pass left in its container is settled by the next pass through a container of
    /// its own: renamed, checked relative to it and unlinked, and the dead container goes. A file
    /// that is not the inode the plan found is put back and nothing is deleted.
    #[test]
    fn a_dead_passs_container_is_settled_through_the_next_passs_own() {
        let (_root, store, survey_id) = store_with(b"one");
        let object = object_path(&store, &survey_id);
        let dead = store.join("cas/removing/dead");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::rename(&object, dead.join(&survey_id)).unwrap();
        let identity = Identity::of(&dead.join(&survey_id)).unwrap();
        let other = Identity {
            inode: identity.inode + 1,
            ..identity
        };
        let error = settle(&store, OsStr::new("dead"), &survey_id, other, |_| {}).unwrap_err();
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
        let error = settle(&store, OsStr::new("dead"), &survey_id, identity, |step| {
            if step == Before::Check {
                let (files, _) = under_removing(&store);
                let inside = store.join("cas/removing").join(
                    files
                        .iter()
                        .find(|file| !file.starts_with("dead/"))
                        .unwrap(),
                );
                std::fs::remove_file(&inside).unwrap();
                std::fs::write(&inside, b"one").unwrap();
            }
        })
        .unwrap_err();
        assert!(error.starts_with(REPLACED), "{error}");
        assert_eq!(std::fs::read(dead.join(&survey_id)).unwrap(), b"one");
        let identity = Identity::of(&dead.join(&survey_id)).unwrap();
        let bytes = settle(&store, OsStr::new("dead"), &survey_id, identity, |_| {}).unwrap();
        assert!(bytes > 0);
        assert_eq!(under_removing(&store), (Vec::new(), Vec::new()));
        assert!(!object.exists(), "its CAS name is not touched");
    }

    fn file(container: Option<&str>, name: &[u8], inode: u64) -> Found {
        Found {
            container: container.map(OsString::from),
            name: OsStr::from_bytes(name).to_os_string(),
            kind: FoundKind::File,
            identity: Identity { device: 1, inode },
            bytes: 4096,
        }
    }

    /// Leftovers are judged with their raw names: a regular file in a container named by a
    /// tombstoned Survey is its record, moved; a name that is not UTF-8 is `unreadable name`,
    /// and everything else is `foreign`, each left with its path kept byte for byte.
    #[test]
    fn leftovers_are_judged_by_their_raw_names() {
        let store = Path::new("/state/af/project/aa");
        let collected = format!("sha256:{}", "ab".repeat(32));
        let unnamed = format!("sha256:{}", "cd".repeat(32));
        let found = vec![
            file(Some("p1"), collected.as_bytes(), 1),
            file(Some("p2"), collected.as_bytes(), 2),
            file(Some("p1"), unnamed.as_bytes(), 3),
            file(Some("p1"), b"caf\xe9", 4),
            file(None, b"\xff", 5),
            file(None, collected.as_bytes(), 6),
            file(Some("p1"), format!("{collected}.replaced").as_bytes(), 7),
            Found {
                kind: FoundKind::Directory,
                ..file(Some("p1"), b"nested", 8)
            },
        ];
        let (moved, left) = judge_leftovers(store, found, &BTreeSet::from([collected.clone()]));
        assert_eq!(
            moved.get(&collected),
            Some(&Moved {
                container: "p1".into(),
                identity: Identity {
                    device: 1,
                    inode: 1
                },
                bytes: 4096,
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
                Foreign
            ]
        );
        let raw = &left[2].path;
        assert_eq!(
            raw.as_os_str().as_bytes(),
            b"/state/af/project/aa/cas/removing/p1/caf\xe9",
            "the name is kept raw"
        );
        assert!(left[2].detail.contains("caf\u{fffd}"), "{}", left[2].detail);
        assert_eq!(left[6].bytes, 0, "a directory is no file's bytes");
    }

    #[test]
    fn the_walk_finds_containers_their_files_and_empty_ones() {
        let (_root, store, survey_id) = store_with(b"one");
        let removing = store.join("cas/removing");
        std::fs::create_dir_all(removing.join("empty")).unwrap();
        std::fs::create_dir_all(removing.join("full")).unwrap();
        std::fs::write(removing.join("full").join(&survey_id), b"one").unwrap();
        std::fs::write(removing.join("loose"), b"x").unwrap();
        let (found, empty) = walk_removing(&open_cas(&store).unwrap()).unwrap();
        assert_eq!(empty, [OsString::from("empty")]);
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
                (Some("full".into()), survey_id.clone(), FoundKind::File),
                (None, "loose".into(), FoundKind::File),
            ]
        );
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
