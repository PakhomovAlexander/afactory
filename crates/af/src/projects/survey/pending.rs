//! The pending-publication entries of a Project Store (ADR-0158 clause 2).
//!
//! A Survey is published in two steps: its record goes into the Project Store's CAS, then
//! `SurveyRecorded@1` into its log. A process that dies between the two leaves a record no event
//! references, which no inventory that starts from the log would see. So the Surveyor, holding
//! the Project Store's lock throughout, publishes in this order, which collection's reader
//! (`collect.rs`) relies on:
//!
//! 1. [`begin`]: one machine-local entry, `af/survey-pending@2` at `pending-surveys/<64
//!    hex>.json` beside `project.lock`, naming the Survey and the length of its bytes, written
//!    through a synced temporary file renamed into place and the directory synced, before any
//!    byte of the record is written anywhere;
//! 2. [`publish`]: the record's bytes staged in the writer's own `pending-surveys/<64
//!    hex>.object`, created exclusively, synced and its directory synced; then the identity of
//!    that staged object (its device, inode and birth time, stated without following a link and
//!    checked to hold the record's length) recorded in the entry the same way; and only then
//!    that inode renamed to its CAS name, never over a name that is taken, and the directories
//!    synced;
//! 3. `SurveyRecorded@1` committed, and only then [`clear`].
//!
//! Every state between them has its recovery, collection's: staged bytes whose entry names no
//! object (a crash before the identity) are the writer's own leftovers and are reclaimed; an
//! entry naming the staged object while the CAS name is empty has that staged object reclaimed;
//! an entry naming the object at the CAS name has that object reclaimed; and the entry goes once
//! no file with its inode is left. Nothing is ever at the CAS name before the entry names it.
//!
//! An entry is at most [`MAX_ENTRY_BYTES`] and names at most `MAX_SURVEY_BYTES` of record, and
//! is never in the repository or the log. A refresh that finds the entry of the Survey it is
//! publishing (a crash left it, and the same bytes came again) finishes that publication and
//! clears it, but never discards the identity it holds: an entry that already names an object
//! is kept as it is, and the publication goes on only while that object is the staged file or
//! the file at the CAS name; any other entry is left for collection to settle. Every entry and
//! staged object is written, renamed and removed relative to `pending-surveys` opened beneath
//! the Project Store without following a link, as collection's reader opens it, and only while
//! it is a real directory of the Store, so nothing is ever written through a link. A debug
//! build stops at the three crash points a fixture needs: [`stop_before_identity`] after the
//! bytes are staged, [`stop_before_link`] after the entry names them, and
//! [`stop_before_event`] after they took their CAS name; and fails the staging on request
//! ([`fail_put`]).

use std::ffi::OsStr;
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use review_core::project::{
    SUPERSEDED_SURVEY_PENDING, SURVEY_PENDING_V1_SUPERSEDED, SURVEY_PENDING_V2,
    SurveyObjectIdentityV2, SurveyPendingV2,
};
use review_sandbox::{Identity, Stated, removal::Kind};

/// The directory of the entries and the staged objects, beside `project.lock`.
pub(crate) const DIRECTORY: &str = "pending-surveys";

/// What follows a Survey's hex in the name of the object the writer stages.
pub(crate) const STAGED_SUFFIX: &str = ".object";

/// The largest entry the writer writes and collection reads: an entry holds a digest and four
/// decimal numbers, a few hundred bytes.
pub(crate) const MAX_ENTRY_BYTES: usize = 4096;

/// A debug build's fixture may stop a refresh after its record's bytes took their CAS name and
/// before its event, as a crash would. Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_STOP_BEFORE_EVENT: &str = "AF_TEST_SURVEY_STOP_BEFORE_EVENT";
/// A debug build's fixture may stop a refresh after its record's bytes are staged and before
/// the entry records their identity, as a crash would. Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_STOP_BEFORE_IDENTITY: &str = "AF_TEST_SURVEY_STOP_BEFORE_IDENTITY";
/// A debug build's fixture may stop a refresh after the entry records the staged object's
/// identity and before that object takes its CAS name, as a crash would. Release binaries never
/// read it.
#[cfg(debug_assertions)]
const TEST_STOP_BEFORE_LINK: &str = "AF_TEST_SURVEY_STOP_BEFORE_LINK";
/// A debug build's fixture may make a refresh's write of its record fail. Release binaries
/// never read it.
#[cfg(debug_assertions)]
const TEST_FAIL_PUT: &str = "AF_TEST_SURVEY_FAIL_PUT";

/// Where the Project Store at `store` keeps the entry of `survey_id`.
#[cfg(test)]
pub(crate) fn location(store: &Path, survey_id: &str) -> PathBuf {
    store.join(DIRECTORY).join(file_name(survey_id))
}

/// Where the Project Store at `store` stages the record of `survey_id`.
#[cfg(test)]
pub(crate) fn staged_location(store: &Path, survey_id: &str) -> PathBuf {
    store.join(DIRECTORY).join(staged_name(survey_id))
}

/// The identity a persisted object identity names.
pub(crate) fn identity_of(object: &SurveyObjectIdentityV2) -> Identity {
    Identity {
        device: object.device.get(),
        inode: object.inode.get(),
        born: Some(object.born_unix_ns.get()),
    }
}

/// The persisted form of a born identity; `None` for one without a birth time, which is never
/// recorded.
pub(crate) fn object_of(identity: Identity) -> Option<SurveyObjectIdentityV2> {
    identity.born.map(|born| SurveyObjectIdentityV2 {
        device: identity.device.into(),
        inode: identity.inode.into(),
        born_unix_ns: born.into(),
    })
}

/// The entry `bytes` hold, read as the writer and collection read it: a superseded
/// `af/survey-pending@1` is refused by its label before any field is decoded, naming the
/// recovery.
pub(crate) fn parse(bytes: &[u8], shown: &Path) -> Result<SurveyPendingV2, String> {
    if review_core::project::schema_label(bytes).as_deref() == Some(SURVEY_PENDING_V1_SUPERSEDED) {
        return Err(format!(
            "{} is refused: {SUPERSEDED_SURVEY_PENDING}; delete it, and what it names is then \
             listed foreign and kept",
            shown.display()
        ));
    }
    let entry: SurveyPendingV2 = serde_json::from_slice(bytes)
        .map_err(|error| format!("{} is no {SURVEY_PENDING_V2}: {error}", shown.display()))?;
    entry.validate()?;
    Ok(entry)
}

/// The Project Store at `store` and its `pending-surveys` directory, held open: the Store as
/// its path names it, the directory beneath it opened without following a link, as collection's
/// reader opens it (`review_sandbox::open_beneath`), and refused unless it is a real directory on
/// the Store's device owned by the Store's owner. With `create`, an absent directory is made
/// (mode 0700) and the Store's directory synced; without it, an absent one is `None`.
struct Entries {
    store: OwnedFd,
    directory: OwnedFd,
    /// The device of the Store.
    device: u64,
    /// Its path, for what the writer reports.
    path: PathBuf,
}

impl Entries {
    fn open(store: &Path, create: bool) -> Result<Option<Self>, String> {
        let path = store.join(DIRECTORY);
        let held = review_sandbox::open_anchor(store)
            .map_err(|error| format!("opening the Project Store {}: {error}", store.display()))?;
        let mut made = false;
        if create {
            match nix::sys::stat::mkdirat(
                &held,
                DIRECTORY,
                nix::sys::stat::Mode::from_bits_truncate(0o700),
            ) {
                Ok(()) => made = true,
                Err(nix::errno::Errno::EEXIST) => {}
                Err(errno) => return Err(format!("creating {}: {errno}", path.display())),
            }
        }
        let directory = match review_sandbox::open_beneath(&held, Path::new(DIRECTORY)) {
            Ok(directory) => directory,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(_) => return Err(refused(&path, "it is a symbolic link or not a directory")),
        };
        let owner = nix::sys::stat::fstat(&held).map_err(|errno| errno.to_string())?;
        let stated = nix::sys::stat::fstat(&directory).map_err(|errno| errno.to_string())?;
        if stated.st_dev != owner.st_dev {
            return Err(refused(
                &path,
                "it is on another device than its Project Store",
            ));
        }
        if stated.st_uid != owner.st_uid {
            return Err(refused(
                &path,
                "another user than its Project Store's owns it",
            ));
        }
        if made {
            sync(&held)?;
        }
        #[allow(clippy::unnecessary_cast)]
        let device = owner.st_dev as u64;
        Ok(Some(Self {
            store: held,
            directory,
            device,
            path,
        }))
    }

    /// The entry `name`, read without following a link, within [`MAX_ENTRY_BYTES`]; `None`
    /// when there is none.
    fn read(&self, name: &str) -> Result<Option<SurveyPendingV2>, String> {
        use nix::fcntl::{OFlag, openat};
        let file = match openat(
            &self.directory,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        ) {
            Ok(file) => std::fs::File::from(file),
            Err(nix::errno::Errno::ENOENT) => return Ok(None),
            Err(errno) => return Err(format!("{}: {errno}", self.path.join(name).display())),
        };
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .file_type()
            .is_file()
        {
            return Err(format!(
                "{} is not a regular file",
                self.path.join(name).display()
            ));
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_ENTRY_BYTES as u64 + 1),
            &mut bytes,
        )
        .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err(format!(
                "{} holds more than {MAX_ENTRY_BYTES} bytes",
                self.path.join(name).display()
            ));
        }
        parse(&bytes, &self.path.join(name)).map(Some)
    }

    /// Replace the entry `name` whole through a fresh temporary file created exclusively beside
    /// it, synced, renamed over it and the directory synced, every step relative to the
    /// directory held open, so nothing is ever written through a link.
    fn replace(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let temporary = format!(".{name}.{}-{}", std::process::id(), unique());
        let written = create(
            &self.directory,
            &temporary,
            bytes,
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .and_then(|()| {
            rustix::fs::renameat(&self.directory, temporary.as_str(), &self.directory, name)
                .map_err(|errno| errno.to_string())
        });
        if let Err(why) = written {
            let _ = nix::unistd::unlinkat(
                &self.directory,
                temporary.as_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            );
            return Err(why);
        }
        sync(&self.directory)
    }

    /// What `name` of the directory is, stated without following a link.
    fn stat(&self, name: &str) -> Result<Option<Stated>, String> {
        review_sandbox::born_at(&self.directory, OsStr::new(name))
            .map_err(|error| format!("{}: {error}", self.path.join(name).display()))
    }

    /// The CAS `objects` directory of the Store, reached beneath it without following a link and
    /// on its device.
    fn objects(&self) -> Result<OwnedFd, String> {
        let objects = review_sandbox::open_beneath(&self.store, Path::new("cas/objects"))
            .map_err(|error| format!("its CAS objects directory is refused: {error}"))?;
        self.on_device(&objects, "its CAS objects directory")?;
        Ok(objects)
    }

    fn on_device(&self, directory: &OwnedFd, what: &str) -> Result<(), String> {
        let device = review_sandbox::Identity::of_descriptor(directory)
            .map_err(|error| format!("inspecting {what}: {error}"))?
            .device;
        if device == self.device {
            Ok(())
        } else {
            Err(format!("{what} resolves outside its Project Store"))
        }
    }

    /// What holds `survey_id`'s CAS name, stated without following a link; `None` when nothing
    /// does or its prefix directory is absent.
    fn at_cas_name(&self, survey_id: &str) -> Result<Option<Stated>, String> {
        let (prefix, rest) = split(survey_id)?;
        let objects = self.objects()?;
        let prefix = match review_sandbox::open_beneath(&objects, Path::new(prefix)) {
            Ok(prefix) => prefix,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("its CAS prefix directory is refused: {error}")),
        };
        self.on_device(&prefix, "its CAS prefix directory")?;
        review_sandbox::born_at(&prefix, OsStr::new(rest))
            .map_err(|error| format!("the CAS name of Survey {survey_id}: {error}"))
    }
}

/// Create `name` of `directory` exclusively, without following a link, holding `bytes`, synced.
fn create(
    directory: &OwnedFd,
    name: &str,
    bytes: &[u8],
    mode: nix::sys::stat::Mode,
) -> Result<(), String> {
    use nix::fcntl::{OFlag, openat};
    openat(
        directory,
        name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        mode,
    )
    .map_err(std::io::Error::from)
    .map(std::fs::File::from)
    .and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    })
    .map_err(|error| error.to_string())
}

/// How a `pending-surveys` that is no real directory of the Store is refused: nothing is
/// written there.
fn refused(path: &Path, why: &str) -> String {
    format!(
        "{} is refused: {why}, and af writes a pending-publication entry only into a directory \
         of its own Project Store; remove it and run `af project refresh` again",
        path.display()
    )
}

/// A number no other temporary name of this process holds.
fn unique() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Make what `directory` holds durable: the directory itself synced.
fn sync(directory: &OwnedFd) -> Result<(), String> {
    nix::unistd::fsync(directory).map_err(|errno| errno.to_string())
}

/// The hex of a Survey's ID split into its CAS prefix directory and its object's name.
fn split(survey_id: &str) -> Result<(&str, &str), String> {
    survey_id
        .strip_prefix("sha256:")
        .filter(|_| review_core::is_digest(survey_id))
        .map(|hex| hex.split_at(2))
        .ok_or_else(|| format!("`{survey_id}` names no CAS object"))
}

/// The file name of `survey_id`'s entry.
fn file_name(survey_id: &str) -> String {
    let hex = survey_id.strip_prefix("sha256:").unwrap_or(survey_id);
    format!("{hex}.json")
}

/// The file name of `survey_id`'s staged object.
pub(crate) fn staged_name(survey_id: &str) -> String {
    let hex = survey_id.strip_prefix("sha256:").unwrap_or(survey_id);
    format!("{hex}{STAGED_SUFFIX}")
}

/// Replace the entry whole, durably on return, through the Store's own `pending-surveys`
/// ([`Entries`]); the Project Store's directory is synced too when that directory is new.
fn write(entries: &Entries, entry: &SurveyPendingV2) -> Result<(), String> {
    entry.validate()?;
    let value = serde_json::to_value(entry).map_err(|error| error.to_string())?;
    let bytes = review_store::canonicalize(&value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "a pending entry of {} bytes is more than {MAX_ENTRY_BYTES}",
            bytes.len()
        ));
    }
    entries.replace(&file_name(&entry.survey_id), &bytes)
}

/// The born identity of `stated` when it is a regular file of exactly `bytes`; `None` otherwise.
fn regular(stated: Option<Stated>, bytes: u64) -> Option<Identity> {
    stated
        .filter(|stated| stated.kind == Kind::File && stated.size == bytes)
        .map(|stated| stated.identity)
        .filter(|identity| identity.born.is_some())
}

/// Where the object an earlier attempt of the same Survey recorded is now: still staged, or at
/// its CAS name already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kept {
    Staged(Identity),
    Linked(Identity),
}

/// Record that `survey_id`, `bytes` long, is about to be published: before any byte of it is
/// written, under the Project Store's lock. An entry a crash left for the same Survey is this
/// publication's, which finishes it, but never at the cost of the identity it holds: one that
/// already names an object keeps it, unrewritten, and that object must be the staged file or
/// the file at the CAS name (a regular file of that device, inode, birth time and length), or
/// the publication is refused and the entry left for collection, which reclaims that object.
/// Returns where the kept object is, for [`publish`] to finish from.
pub(crate) fn begin(store: &Path, survey_id: &str, bytes: u64) -> Result<Option<Kept>, String> {
    let failed =
        |error: String| format!("recording the publication of Survey {survey_id}: {error}");
    let entry = SurveyPendingV2 {
        schema: SURVEY_PENDING_V2.into(),
        survey_id: survey_id.into(),
        bytes: bytes.into(),
        object: None,
    };
    entry.validate().map_err(failed)?;
    let entries = Entries::open(store, true)
        .map_err(failed)?
        .ok_or_else(|| failed("the pending entries cannot be opened".into()))?;
    let earlier = entries.read(&file_name(survey_id)).map_err(|error| {
        failed(format!(
            "{error}; it is left for `af storage prune --apply` to settle"
        ))
    })?;
    if let Some(earlier) = &earlier
        && earlier.survey_id != survey_id
    {
        return Err(failed(format!(
            "its pending-publication entry names Survey {}, so it is left for `af storage prune \
             --apply` to settle",
            earlier.survey_id
        )));
    }
    if let Some((earlier, named)) =
        earlier.and_then(|earlier| earlier.object.map(|named| (earlier, named)))
    {
        let named = identity_of(&named);
        let length = earlier.bytes.get();
        if length == bytes {
            if regular(entries.at_cas_name(survey_id).map_err(failed)?, bytes) == Some(named) {
                return Ok(Some(Kept::Linked(named)));
            }
            if regular(
                entries.stat(&staged_name(survey_id)).map_err(failed)?,
                bytes,
            ) == Some(named)
            {
                return Ok(Some(Kept::Staged(named)));
            }
        }
        return Err(failed(format!(
            "its pending-publication entry names the object {}:{} of {length} bytes, which \
             neither its staged name nor its CAS name holds, so the entry is kept for `af storage \
             prune --apply` to settle that object; run `af project refresh` again after it",
            named.device, named.inode
        )));
    }
    write(&entries, &entry).map_err(failed)?;
    Ok(None)
}

/// Publish `record`, the bytes of `survey_id`, whose entry [`begin`] wrote: staged in the
/// writer's own `pending-surveys/<64 hex>.object` (created exclusively, synced, its directory
/// synced), the staged object's identity recorded in the entry (stated without following a
/// link, refused unless it is a regular file of the record's length with a birth time), and only
/// then that inode renamed to its CAS name, never over a name that is taken, and the directories
/// synced. A staged file an earlier attempt left without an identity is that attempt's own
/// leftover and is replaced; one whose identity `begin` kept (`Kept::Staged`) is checked to hold
/// the record's bytes and then renamed; one already at its CAS name (`Kept::Linked`) is kept.
/// Returns the object's identity.
pub(crate) fn publish(
    store: &Path,
    survey_id: &str,
    record: &[u8],
    kept: Option<Kept>,
) -> Result<Identity, String> {
    let failed = |error: String| format!("publishing Survey {survey_id}: {error}");
    let entries = Entries::open(store, false)
        .map_err(failed)?
        .ok_or_else(|| failed("its pending-publication entry is gone".into()))?;
    let length = record.len() as u64;
    let staged = staged_name(survey_id);
    let identity = match kept {
        Some(Kept::Linked(identity)) => return Ok(identity),
        Some(Kept::Staged(identity)) => {
            if read_staged(&entries, &staged, record.len()).map_err(failed)? != record {
                return Err(failed(format!(
                    "its staged object {} holds other bytes than the record, so it is kept for \
                     `af storage prune --apply` to settle",
                    entries.path.join(&staged).display()
                )));
            }
            identity
        }
        None => {
            // Nothing names a staged file yet: what is there is an earlier attempt's leftover,
            // its bytes perhaps only part written, and this writer's own under the lock.
            match nix::unistd::unlinkat(
                &entries.directory,
                staged.as_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            ) {
                Ok(()) | Err(nix::errno::Errno::ENOENT) => {}
                Err(errno) => {
                    return Err(failed(format!(
                        "removing the leftover {}: {errno}",
                        entries.path.join(&staged).display()
                    )));
                }
            }
            create(
                &entries.directory,
                &staged,
                record,
                nix::sys::stat::Mode::from_bits_truncate(0o644),
            )
            .and_then(|()| sync(&entries.directory))
            .map_err(|why| {
                failed(format!(
                    "staging its record in {}: {why}",
                    entries.path.join(&staged).display()
                ))
            })?;
            stop_before_identity();
            let stated = entries.stat(&staged).map_err(failed)?;
            let identity = match stated {
                Some(stated) if stated.kind == Kind::File && stated.size == length => stated
                    .complete(
                        &entries.path.join(&staged).display().to_string(),
                        &entries.directory,
                    )
                    .map_err(|error| failed(error.to_string()))?,
                _ => {
                    return Err(failed(format!(
                        "{} holds no regular file of its {length} bytes",
                        entries.path.join(&staged).display()
                    )));
                }
            };
            write(
                &entries,
                &SurveyPendingV2 {
                    schema: SURVEY_PENDING_V2.into(),
                    survey_id: survey_id.into(),
                    bytes: length.into(),
                    object: object_of(identity),
                },
            )
            .map_err(failed)?;
            identity
        }
    };
    stop_before_link();
    link(&entries, &staged, survey_id, identity, length).map_err(failed)?;
    Ok(identity)
}

/// The staged object's bytes, at most `length` and one more, read without following a link.
fn read_staged(entries: &Entries, staged: &str, length: usize) -> Result<Vec<u8>, String> {
    use nix::fcntl::{OFlag, openat};
    let file = openat(
        &entries.directory,
        staged,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(|errno| format!("{}: {errno}", entries.path.join(staged).display()))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, length as u64 + 1),
        &mut bytes,
    )
    .map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// Rename the staged object `staged`, which must still be `identity` of `length` bytes, to
/// `survey_id`'s CAS name, never over a name that is taken, and make the rename durable: the
/// prefix directory synced, `objects` too when the prefix directory is new, and
/// `pending-surveys`.
fn link(
    entries: &Entries,
    staged: &str,
    survey_id: &str,
    identity: Identity,
    length: u64,
) -> Result<(), String> {
    let (prefix_name, rest) = split(survey_id)?;
    if regular(entries.stat(staged)?, length) != Some(identity) {
        return Err(format!(
            "{} is not the staged object its pending-publication entry names, so the entry is \
             kept for `af storage prune --apply`",
            entries.path.join(staged).display()
        ));
    }
    let objects = entries.objects()?;
    let made = match nix::sys::stat::mkdirat(
        &objects,
        prefix_name,
        nix::sys::stat::Mode::from_bits_truncate(0o755),
    ) {
        Ok(()) => true,
        Err(nix::errno::Errno::EEXIST) => false,
        Err(errno) => return Err(format!("creating its CAS prefix directory: {errno}")),
    };
    let prefix = review_sandbox::open_beneath(&objects, Path::new(prefix_name))
        .map_err(|error| format!("its CAS prefix directory is refused: {error}"))?;
    entries.on_device(&prefix, "its CAS prefix directory")?;
    match rustix::fs::renameat_with(
        &entries.directory,
        staged,
        &prefix,
        rest,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {}
        Err(rustix::io::Errno::EXIST) => {
            return Err(format!(
                "another file holds the CAS name of Survey {survey_id}, which this writer did \
                 not put there and never replaces: its staged object and its pending-publication \
                 entry are kept for `af storage prune --apply`, which lists that file foreign, \
                 and a refresh records the Survey once that file is removed"
            ));
        }
        Err(errno) => return Err(format!("moving its staged object to its CAS name: {errno}")),
    }
    sync(&prefix)?;
    if made {
        sync(&objects)?;
    }
    sync(&entries.directory)
}

/// Remove the entry of `survey_id` once its event committed, and the directory once it is
/// empty, through the Store's own `pending-surveys` ([`Entries`]): an entry already gone, or no
/// directory, is no error.
pub(crate) fn clear(store: &Path, survey_id: &str) -> Result<(), String> {
    let failed = |error: String| format!("clearing the publication of Survey {survey_id}: {error}");
    let Some(entries) = Entries::open(store, false).map_err(failed)? else {
        return Ok(());
    };
    match nix::unistd::unlinkat(
        &entries.directory,
        file_name(survey_id).as_str(),
        nix::unistd::UnlinkatFlags::NoRemoveDir,
    ) {
        Ok(()) | Err(nix::errno::Errno::ENOENT) => {}
        Err(errno) => return Err(failed(errno.to_string())),
    }
    // Another entry keeps the directory.
    let _ = nix::unistd::unlinkat(
        &entries.store,
        DIRECTORY,
        nix::unistd::UnlinkatFlags::RemoveDir,
    );
    Ok(())
}

/// Fail a refresh's write of its record when a debug build's fixture asks
/// (`AF_TEST_SURVEY_FAIL_PUT=1`), after the entry and before any byte of the record, as a file
/// system that refuses the bytes would. Release binaries never read it.
pub(crate) fn fail_put() -> Result<(), String> {
    #[cfg(debug_assertions)]
    if std::env::var_os(TEST_FAIL_PUT).is_some_and(|value| value == "1") {
        return Err(format!("an injected failure ({TEST_FAIL_PUT})"));
    }
    Ok(())
}

/// The entry of `survey_id`, when there is one: what collection reads for inventory and
/// recovery in its next round.
#[cfg(test)]
pub(crate) fn read(store: &Path, survey_id: &str) -> Result<Option<SurveyPendingV2>, String> {
    let path = location(store, survey_id);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let entry = parse(&bytes, &path)?;
    if entry.survey_id != survey_id {
        return Err(format!(
            "the pending entry of Survey {survey_id} names Survey {}",
            entry.survey_id
        ));
    }
    Ok(Some(entry))
}

/// Stop here, as a crash between the record's CAS name and its event would, when a debug
/// build's fixture asks.
pub(crate) fn stop_before_event() {
    #[cfg(debug_assertions)]
    stop_if(TEST_STOP_BEFORE_EVENT, "SurveyRecorded@1");
}

/// Stop here, as a crash between the staged bytes and their identity would, when a debug
/// build's fixture asks: nothing is at the CAS name yet.
fn stop_before_identity() {
    #[cfg(debug_assertions)]
    stop_if(TEST_STOP_BEFORE_IDENTITY, "the staged object's identity");
}

/// Stop here, as a crash between the recorded identity and the CAS name would, when a debug
/// build's fixture asks.
fn stop_before_link() {
    #[cfg(debug_assertions)]
    stop_if(TEST_STOP_BEFORE_LINK, "the staged object's CAS name");
}

/// Exit 86, as a crash would leave the Project Store, when `variable` is `1`.
#[cfg(debug_assertions)]
fn stop_if(variable: &str, before: &str) {
    if std::env::var_os(variable).is_some_and(|value| value == "1") {
        eprintln!("af: stopped before {before} ({variable})");
        std::process::exit(86);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Project Store with an empty CAS, as a refresh finds one.
    fn store() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let store = std::fs::canonicalize(root.path()).unwrap().join("store");
        review_store::Cas::open(store.join("cas")).unwrap();
        (root, store)
    }

    fn at_name(store: &Path, survey_id: &str) -> PathBuf {
        super::super::collect::object_path(store, survey_id)
    }

    const RECORD: &[u8] = b"{\"a\":\"1\"}";

    fn survey() -> String {
        review_store::canonical::blob_content_id(RECORD)
    }

    #[test]
    fn an_entry_is_begun_staged_linked_and_cleared() {
        let (_root, store) = store();
        let survey = survey();
        let length = RECORD.len() as u64;
        assert_eq!(read(&store, &survey), Ok(None));
        assert_eq!(begin(&store, &survey, length), Ok(None));
        let begun = read(&store, &survey).unwrap().unwrap();
        assert_eq!(begun.bytes.get(), length);
        assert_eq!(
            begun.object, None,
            "no identity before the bytes are staged"
        );
        assert!(!at_name(&store, &survey).exists());

        let identity = publish(&store, &survey, RECORD, None).unwrap();
        assert_eq!(std::fs::read(at_name(&store, &survey)).unwrap(), RECORD);
        assert!(
            !staged_location(&store, &survey).exists(),
            "the staged object took its CAS name"
        );
        assert_eq!(
            Identity::born_of(&at_name(&store, &survey)).unwrap(),
            identity,
            "the inode the entry names, birth time and all"
        );
        assert_eq!(
            read(&store, &survey).unwrap().unwrap().object,
            object_of(identity)
        );
        // Canonical JSON, every number decimal text.
        let text = std::fs::read_to_string(location(&store, &survey)).unwrap();
        assert!(
            text.starts_with(&format!(
                "{{\"bytes\":\"{length}\",\"object\":{{\"born_unix_ns\":\""
            )),
            "{text}"
        );

        // A crash left it: the same Survey begins again keeping the object it names, unrewritten.
        let before = std::fs::read(location(&store, &survey)).unwrap();
        assert_eq!(
            begin(&store, &survey, length),
            Ok(Some(Kept::Linked(identity)))
        );
        assert_eq!(
            publish(&store, &survey, RECORD, Some(Kept::Linked(identity))),
            Ok(identity)
        );
        assert_eq!(std::fs::read(location(&store, &survey)).unwrap(), before);
        clear(&store, &survey).unwrap();
        assert_eq!(read(&store, &survey), Ok(None));
        assert!(!store.join(DIRECTORY).exists(), "the empty directory goes");
        clear(&store, &survey).unwrap();
    }

    /// Nothing is at the CAS name before the entry names the staged object: bytes staged by an
    /// attempt that stopped before their identity are that attempt's leftover, replaced by the
    /// next attempt, and an entry naming a staged object that never took its CAS name is
    /// finished from that object.
    #[test]
    fn a_staged_object_takes_its_cas_name_only_once_the_entry_names_it() {
        let (_root, store) = store();
        let survey = survey();
        let length = RECORD.len() as u64;
        begin(&store, &survey, length).unwrap();
        // As an attempt that died between its staged bytes and its identity leaves them.
        std::fs::write(staged_location(&store, &survey), &RECORD[..3]).unwrap();
        assert_eq!(read(&store, &survey).unwrap().unwrap().object, None);
        assert!(!at_name(&store, &survey).exists());
        assert_eq!(begin(&store, &survey, length), Ok(None));
        let identity = publish(&store, &survey, RECORD, None).unwrap();
        assert_eq!(std::fs::read(at_name(&store, &survey)).unwrap(), RECORD);

        // As an attempt that died between its identity and the CAS name leaves it.
        std::fs::rename(at_name(&store, &survey), staged_location(&store, &survey)).unwrap();
        assert_eq!(
            begin(&store, &survey, length),
            Ok(Some(Kept::Staged(identity)))
        );
        assert_eq!(
            publish(&store, &survey, RECORD, Some(Kept::Staged(identity))),
            Ok(identity)
        );
        assert_eq!(
            Identity::born_of(&at_name(&store, &survey)).unwrap(),
            identity
        );
        assert!(!staged_location(&store, &survey).exists());
    }

    /// A retry never discards the identity an earlier attempt of the same Survey recorded: the
    /// entry keeps naming that object while it is the staged file or the file at the CAS name,
    /// and the publication is refused, the entry untouched, when neither holds it (another
    /// inode with the same bytes, another length, nothing there).
    #[test]
    fn a_retry_keeps_the_object_an_earlier_attempt_recorded() {
        let (root, store) = store();
        let survey = survey();
        let length = RECORD.len() as u64;
        begin(&store, &survey, length).unwrap();
        let first = publish(&store, &survey, RECORD, None).unwrap();
        let recorded = std::fs::read(location(&store, &survey)).unwrap();
        let object = at_name(&store, &survey);

        // Another file at the CAS name, of the same bytes, born later: refused, the entry as it
        // was.
        let aside = root.path().join("aside");
        std::fs::rename(&object, &aside).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&object, RECORD).unwrap();
        let refused = begin(&store, &survey, length).unwrap_err();
        assert!(refused.contains("neither its staged name nor"), "{refused}");
        assert_eq!(std::fs::read(location(&store, &survey)).unwrap(), recorded);
        // Nothing at the name: refused the same way.
        std::fs::remove_file(&object).unwrap();
        assert!(begin(&store, &survey, length).is_err());
        // Another length: refused.
        assert!(begin(&store, &survey, length + 1).is_err());
        // The first object back at its name: the publication goes on, naming it still.
        std::fs::rename(&aside, &object).unwrap();
        assert_eq!(
            begin(&store, &survey, length),
            Ok(Some(Kept::Linked(first)))
        );
        assert_eq!(std::fs::read(location(&store, &survey)).unwrap(), recorded);
    }

    /// A staged object whose bytes are not the record's, or a CAS name another file took, keeps
    /// the staged object and the entry for collection, and nothing is replaced.
    #[test]
    fn a_staged_object_never_replaces_a_taken_cas_name() {
        let (_root, store) = store();
        let survey = survey();
        let length = RECORD.len() as u64;
        begin(&store, &survey, length).unwrap();
        let object = at_name(&store, &survey);
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&object, b"someone else's").unwrap();
        let refused = publish(&store, &survey, RECORD, None).unwrap_err();
        assert!(
            refused.contains("another file holds the CAS name"),
            "{refused}"
        );
        assert_eq!(std::fs::read(&object).unwrap(), b"someone else's");
        assert_eq!(
            std::fs::read(staged_location(&store, &survey)).unwrap(),
            RECORD
        );
        let named = read(&store, &survey).unwrap().unwrap().object.unwrap();
        assert_eq!(
            identity_of(&named),
            Identity::born_of(&staged_location(&store, &survey)).unwrap()
        );
        // The retry finds the staged object its entry names, but its bytes changed: refused.
        std::fs::remove_file(&object).unwrap();
        let kept = begin(&store, &survey, length).unwrap();
        assert!(matches!(kept, Some(Kept::Staged(_))), "{kept:?}");
        let other = b"{\"a\":\"2\"}";
        let refused = publish(&store, &survey, other, kept).unwrap_err();
        assert!(refused.contains("holds other bytes"), "{refused}");
        assert!(!object.exists());
    }

    /// The writer opens `pending-surveys` as collection's reader does, never through a link:
    /// one replaced by a symbolic link to a writable directory, or by a file, is refused naming
    /// its path, and nothing is written where it points.
    #[test]
    fn a_pending_directory_that_is_a_link_is_refused_and_nothing_is_written_through_it() {
        let (_root, store) = store();
        let outside = tempfile::tempdir().unwrap();
        let survey = survey();
        std::os::unix::fs::symlink(outside.path(), store.join(DIRECTORY)).unwrap();
        for refused in [
            begin(&store, &survey, 2).unwrap_err(),
            publish(&store, &survey, RECORD, None).unwrap_err(),
            clear(&store, &survey).unwrap_err(),
        ] {
            assert!(
                refused.contains(&format!(
                    "{} is refused: it is a symbolic link or not a directory",
                    store.join(DIRECTORY).display()
                )),
                "{refused}"
            );
        }
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        assert!(!at_name(&store, &survey).exists());

        std::fs::remove_file(store.join(DIRECTORY)).unwrap();
        std::fs::write(store.join(DIRECTORY), b"").unwrap();
        assert!(begin(&store, &survey, 2).is_err());
    }

    #[test]
    fn an_entry_stays_small_and_a_superseded_one_is_refused_by_its_label() {
        let (_root, store) = store();
        let survey = survey();
        // The largest entry there can be, a record of the ceiling's length with the largest
        // identity, is well within the bound collection reads.
        let largest = SurveyPendingV2 {
            schema: SURVEY_PENDING_V2.into(),
            survey_id: survey.clone(),
            bytes: review_core::project::MAX_SURVEY_BYTES.into(),
            object: Some(SurveyObjectIdentityV2 {
                device: u64::MAX.into(),
                inode: u64::MAX.into(),
                born_unix_ns: u64::MAX.into(),
            }),
        };
        let entries = Entries::open(&store, true).unwrap().unwrap();
        write(&entries, &largest).unwrap();
        let written = std::fs::read(location(&store, &survey)).unwrap();
        assert!(written.len() < MAX_ENTRY_BYTES, "{}", written.len());
        // A record over the ceiling has no entry.
        let other = format!("sha256:{}", "3".repeat(64));
        assert!(begin(&store, &other, review_core::project::MAX_SURVEY_BYTES + 1).is_err());
        assert_eq!(read(&store, &other), Ok(None));
        // An entry another release wrote is refused by its label, naming the version, and the
        // writer never replaces it.
        let superseded = format!(
            "{{\"bytes\":\"9\",\"object\":{{\"device\":\"1\",\"inode\":\"2\"}},\
             \"schema\":\"af/survey-pending@1\",\"survey_id\":\"{survey}\"}}"
        );
        std::fs::write(location(&store, &survey), &superseded).unwrap();
        let refused = begin(&store, &survey, 9).unwrap_err();
        assert!(refused.contains(SUPERSEDED_SURVEY_PENDING), "{refused}");
        assert!(read(&store, &survey).is_err());
        assert_eq!(
            std::fs::read_to_string(location(&store, &survey)).unwrap(),
            superseded
        );
    }

    #[test]
    fn another_surveys_entry_keeps_the_directory_and_a_wrong_entry_is_refused() {
        let (_root, store) = store();
        let survey = format!("sha256:{}", "1".repeat(64));
        let other = survey.replace('1', "2");
        begin(&store, &survey, 2).unwrap();
        begin(&store, &other, 3).unwrap();
        clear(&store, &survey).unwrap();
        assert_eq!(read(&store, &other).unwrap().unwrap().bytes.get(), 3);
        assert!(begin(&store, "not-a-digest", 2).is_err());
        assert!(begin(&store, &survey, 0).is_err());
        std::fs::copy(location(&store, &other), location(&store, &survey)).unwrap();
        assert!(
            read(&store, &survey).is_err(),
            "an entry names its own Survey"
        );
        assert!(
            begin(&store, &survey, 2).is_err(),
            "and the writer never replaces one that names another"
        );
    }
}
