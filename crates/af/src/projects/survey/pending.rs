//! The pending-publication entries of a Project Store (ADR-0152 clause 2).
//!
//! A Survey is published in two steps: its record goes into the Project Store's CAS, then
//! `SurveyRecorded@1` into its log. A process that dies between the two leaves a record no event
//! references, which no inventory that starts from the log would see. So the Surveyor, holding
//! the Project Store's lock throughout, publishes in this order, which collection's reader
//! (`collect.rs`) relies on:
//!
//! 1. [`begin`]: one machine-local entry, `af/survey-pending@1` at `pending-surveys/<64
//!    hex>.json` beside `project.lock`, naming the Survey and the length of its bytes, written
//!    through a synced temporary file renamed into place and the directory synced, before any
//!    byte of the record enters the CAS;
//! 2. the record linked at its CAS name and made durable there;
//! 3. [`written`]: the identity (device and inode) of the file at the Survey's CAS name, stated
//!    without following a link and checked to hold the record's length, recorded the same way,
//!    before the event is appended;
//! 4. `SurveyRecorded@1` committed, and only then [`clear`].
//!
//! An entry is at most [`MAX_ENTRY_BYTES`] and names at most `MAX_SURVEY_BYTES` of record, and
//! is never in the repository or the log. A refresh that finds the entry of the Survey it is
//! publishing (a crash left it, and the same bytes came again) finishes that publication and
//! clears it, but never discards the identity it holds: an entry that already names an object
//! is kept as it is, and the publication goes on only while the CAS name holds that object;
//! any other entry is left for collection to settle. Every entry is written, renamed and
//! removed relative to `pending-surveys` opened beneath the Project Store without following a
//! link, as collection's reader opens it, and only while it is a real directory of the Store,
//! so nothing is ever written through a link. A debug build stops at the two crash points a
//! fixture needs: [`stop_before_identity`] after step 2, and [`stop_before_event`] after step 3,
//! and fails step 2 on request ([`fail_put`]).

use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use review_core::project::{SURVEY_PENDING_V1, SurveyObjectIdentityV1, SurveyPendingV1};

/// The directory of the entries, beside `project.lock`.
pub(crate) const DIRECTORY: &str = "pending-surveys";

/// The largest entry the writer writes and collection reads: an entry holds a digest and three
/// decimal numbers, a few hundred bytes.
pub(crate) const MAX_ENTRY_BYTES: usize = 4096;

/// A debug build's fixture may stop a refresh after its record's bytes and its identity and
/// before its event, as a crash would. Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_STOP_BEFORE_EVENT: &str = "AF_TEST_SURVEY_STOP_BEFORE_EVENT";
/// A debug build's fixture may stop a refresh after its record's bytes are linked at their CAS
/// name and before the entry records their identity, as a crash would. Release binaries never
/// read it.
#[cfg(debug_assertions)]
const TEST_STOP_BEFORE_IDENTITY: &str = "AF_TEST_SURVEY_STOP_BEFORE_IDENTITY";
/// A debug build's fixture may make a refresh's write of its record into the CAS fail.
/// Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_FAIL_PUT: &str = "AF_TEST_SURVEY_FAIL_PUT";

/// Where the Project Store at `store` keeps the entry of `survey_id`.
#[cfg(test)]
pub(crate) fn location(store: &Path, survey_id: &str) -> PathBuf {
    let hex = survey_id.strip_prefix("sha256:").unwrap_or(survey_id);
    store.join(DIRECTORY).join(format!("{hex}.json"))
}

/// The Project Store at `store` and its `pending-surveys` directory, held open: the Store as
/// its path names it, the directory beneath it opened without following a link, as collection's
/// reader opens it (`review_sandbox::open_beneath`), and refused unless it is a real directory on
/// the Store's device owned by the Store's owner. With `create`, an absent directory is made
/// (mode 0700) and the Store's directory synced; without it, an absent one is `None`.
struct Entries {
    store: std::os::fd::OwnedFd,
    directory: std::os::fd::OwnedFd,
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
        Ok(Some(Self {
            store: held,
            directory,
            path,
        }))
    }

    /// The entry `name`, read without following a link, within [`MAX_ENTRY_BYTES`]; `None`
    /// when there is none.
    fn read(&self, name: &str) -> Result<Option<SurveyPendingV1>, String> {
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
        let entry: SurveyPendingV1 = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "{} is no {SURVEY_PENDING_V1}: {error}",
                self.path.join(name).display()
            )
        })?;
        entry.validate()?;
        Ok(Some(entry))
    }

    /// Replace the entry `name` whole through a fresh temporary file created exclusively beside
    /// it, synced, renamed over it and the directory synced, every step relative to the
    /// directory held open, so nothing is ever written through a link.
    fn replace(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        use nix::fcntl::{OFlag, openat};
        let temporary = format!(".{name}.{}-{}", std::process::id(), unique());
        let written = openat(
            &self.directory,
            temporary.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .map_err(std::io::Error::from)
        .map(std::fs::File::from)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .map_err(|error| error.to_string())
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
fn sync(directory: &std::os::fd::OwnedFd) -> Result<(), String> {
    nix::unistd::fsync(directory).map_err(|errno| errno.to_string())
}

/// The file name of `survey_id`'s entry.
fn file_name(survey_id: &str) -> String {
    let hex = survey_id.strip_prefix("sha256:").unwrap_or(survey_id);
    format!("{hex}.json")
}

/// Replace the entry whole, durably on return, through the Store's own `pending-surveys`
/// ([`Entries`]); the Project Store's directory is synced too when that directory is new.
fn write(store: &Path, entry: &SurveyPendingV1) -> Result<(), String> {
    entry.validate()?;
    let value = serde_json::to_value(entry).map_err(|error| error.to_string())?;
    let bytes = review_store::canonicalize(&value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "a pending entry of {} bytes is more than {MAX_ENTRY_BYTES}",
            bytes.len()
        ));
    }
    let entries = Entries::open(store, true)?.ok_or("the pending entries cannot be opened")?;
    entries.replace(&file_name(&entry.survey_id), &bytes)
}

/// The identity of the regular file at `object`, stated without following a link, when it is
/// one of exactly `bytes`; `None` otherwise or when there is none.
fn regular_of(object: &Path, bytes: u64) -> Option<SurveyObjectIdentityV1> {
    let stated = std::fs::symlink_metadata(object).ok()?;
    (stated.file_type().is_file() && stated.len() == bytes).then(|| SurveyObjectIdentityV1 {
        device: stated.dev().into(),
        inode: stated.ino().into(),
    })
}

/// Record that `survey_id`, `bytes` long, is about to be written to the CAS at `object`: before
/// any byte is, under the Project Store's lock. An entry a crash left for the same Survey is
/// this publication's, which finishes it, but never at the cost of the identity it holds: one
/// that already names an object keeps it, unrewritten, and the object at the CAS name must be
/// that one (a regular file of that device, inode and length), or the publication is refused
/// and the entry left for collection, which reclaims that object. Returns the identity kept, for
/// [`written`] to check the bytes it finds against.
pub(crate) fn begin(
    store: &Path,
    survey_id: &str,
    bytes: u64,
    object: &Path,
) -> Result<Option<SurveyObjectIdentityV1>, String> {
    let failed =
        |error: String| format!("recording the publication of Survey {survey_id}: {error}");
    let entry = SurveyPendingV1 {
        schema: SURVEY_PENDING_V1.into(),
        survey_id: survey_id.into(),
        bytes: bytes.into(),
        object: None,
    };
    entry.validate().map_err(failed)?;
    let earlier = match Entries::open(store, false).map_err(failed)? {
        Some(entries) => entries.read(&file_name(survey_id)).map_err(|error| {
            failed(format!(
                "{error}; it is left for `af storage prune --apply` to settle"
            ))
        })?,
        None => None,
    };
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
        if earlier.bytes.get() != bytes || regular_of(object, bytes) != Some(named) {
            return Err(failed(format!(
                "its pending-publication entry names the object {}:{} of {} bytes, which its CAS \
                 name no longer holds, so the entry is kept for `af storage prune --apply` to \
                 settle that object; run `af project refresh` again after it",
                named.device.get(),
                named.inode.get(),
                earlier.bytes.get()
            )));
        }
        return Ok(Some(named));
    }
    write(store, &entry).map_err(failed)?;
    Ok(None)
}

/// Record the identity of the file at `object`, the Survey's CAS name, that now holds
/// `survey_id`'s `bytes` durably: stated without following a link, and refused unless it is a
/// regular file of exactly that length, and, when [`begin`] kept an earlier identity (`kept`),
/// unless it is that same file, so the entry never describes another object and an identity it
/// held is never replaced.
pub(crate) fn written(
    store: &Path,
    survey_id: &str,
    bytes: u64,
    object: &Path,
    kept: Option<SurveyObjectIdentityV1>,
) -> Result<SurveyObjectIdentityV1, String> {
    let identity = regular_of(object, bytes).ok_or_else(|| {
        format!("the CAS name of Survey {survey_id} holds no regular file of its {bytes} bytes")
    })?;
    if kept.is_some_and(|kept| kept != identity) {
        return Err(format!(
            "the CAS name of Survey {survey_id} holds another file than its pending-publication \
             entry names, so the entry is kept for `af storage prune --apply`"
        ));
    }
    write(
        store,
        &SurveyPendingV1 {
            schema: SURVEY_PENDING_V1.into(),
            survey_id: survey_id.into(),
            bytes: bytes.into(),
            object: Some(identity),
        },
    )
    .map_err(|error| format!("recording the publication of Survey {survey_id}: {error}"))?;
    Ok(identity)
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

/// Fail a refresh's write of its record into the CAS when a debug build's fixture asks
/// (`AF_TEST_SURVEY_FAIL_PUT=1`), as a CAS that refuses the bytes would. Release binaries never
/// read it.
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
pub(crate) fn read(store: &Path, survey_id: &str) -> Result<Option<SurveyPendingV1>, String> {
    let bytes = match std::fs::read(location(store, survey_id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let entry: SurveyPendingV1 =
        serde_json::from_slice(&bytes).map_err(|error| format!("{SURVEY_PENDING_V1}: {error}"))?;
    entry.validate()?;
    if entry.survey_id != survey_id {
        return Err(format!(
            "the pending entry of Survey {survey_id} names Survey {}",
            entry.survey_id
        ));
    }
    Ok(Some(entry))
}

/// Stop here, as a crash between the record's bytes and its event would, once the entry
/// records the object's identity, when a debug build's fixture asks.
pub(crate) fn stop_before_event() {
    #[cfg(debug_assertions)]
    stop_if(TEST_STOP_BEFORE_EVENT, "SurveyRecorded@1");
}

/// Stop here, as a crash between the record's bytes and its identity would, once the bytes are
/// linked at their CAS name and before the entry names them, when a debug build's fixture asks.
pub(crate) fn stop_before_identity() {
    #[cfg(debug_assertions)]
    stop_if(TEST_STOP_BEFORE_IDENTITY, "the object's identity");
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

    const SURVEY: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn an_entry_is_begun_given_its_object_and_cleared() {
        let store = tempfile::tempdir().unwrap();
        let object = store.path().join("object");
        assert_eq!(read(store.path(), SURVEY), Ok(None));

        assert_eq!(begin(store.path(), SURVEY, 2, &object), Ok(None));
        let begun = read(store.path(), SURVEY).unwrap().unwrap();
        assert_eq!(begun.bytes.get(), 2);
        assert_eq!(
            begun.object, None,
            "no identity before the bytes are written"
        );

        std::fs::write(&object, b"{}").unwrap();
        let identity = written(store.path(), SURVEY, 2, &object, None).unwrap();
        let stated = std::fs::symlink_metadata(&object).unwrap();
        assert_eq!(
            (identity.device.get(), identity.inode.get()),
            (stated.dev(), stated.ino())
        );
        assert_eq!(
            read(store.path(), SURVEY).unwrap().unwrap().object,
            Some(identity)
        );
        // Canonical JSON, every number decimal text.
        let text = std::fs::read_to_string(location(store.path(), SURVEY)).unwrap();
        assert!(
            text.starts_with("{\"bytes\":\"2\",\"object\":{\"device\":\""),
            "{text}"
        );

        // A crash left it: the same Survey begins again keeping the object it names, unrewritten.
        let before = std::fs::read(location(store.path(), SURVEY)).unwrap();
        assert_eq!(begin(store.path(), SURVEY, 2, &object), Ok(Some(identity)));
        assert_eq!(
            std::fs::read(location(store.path(), SURVEY)).unwrap(),
            before
        );
        clear(store.path(), SURVEY).unwrap();
        assert_eq!(read(store.path(), SURVEY), Ok(None));
        assert!(
            !store.path().join(DIRECTORY).exists(),
            "the empty directory goes"
        );
        clear(store.path(), SURVEY).unwrap();
    }

    /// A retry never discards the identity an earlier attempt of the same Survey recorded: the
    /// entry keeps naming that object while the bytes at the CAS name are that object, and the
    /// publication is refused, the entry untouched, when they are not (another inode, another
    /// length, nothing there) or when the bytes the retry finds are another file.
    #[test]
    fn a_retry_keeps_the_object_an_earlier_attempt_recorded() {
        let store = tempfile::tempdir().unwrap();
        let object = store.path().join("object");
        std::fs::write(&object, b"{}").unwrap();
        begin(store.path(), SURVEY, 2, &object).unwrap();
        let first = written(store.path(), SURVEY, 2, &object, None).unwrap();
        let recorded = std::fs::read(location(store.path(), SURVEY)).unwrap();

        // The retry's `put` fails after `begin`: the entry still names the first object.
        assert_eq!(begin(store.path(), SURVEY, 2, &object), Ok(Some(first)));
        assert_eq!(
            std::fs::read(location(store.path(), SURVEY)).unwrap(),
            recorded
        );

        // Another file at the CAS name, of the same bytes: refused, the entry as it was.
        let aside = store.path().join("aside");
        std::fs::rename(&object, &aside).unwrap();
        std::fs::write(&object, b"{}").unwrap();
        let refused = begin(store.path(), SURVEY, 2, &object).unwrap_err();
        assert!(refused.contains("no longer holds"), "{refused}");
        assert!(written(store.path(), SURVEY, 2, &object, Some(first)).is_err());
        assert_eq!(
            std::fs::read(location(store.path(), SURVEY)).unwrap(),
            recorded
        );
        // Nothing at the name: refused the same way.
        std::fs::remove_file(&object).unwrap();
        assert!(begin(store.path(), SURVEY, 2, &object).is_err());
        // The first object back at its name: the publication goes on, naming it still.
        std::fs::rename(&aside, &object).unwrap();
        assert_eq!(begin(store.path(), SURVEY, 2, &object), Ok(Some(first)));
        assert_eq!(
            written(store.path(), SURVEY, 2, &object, Some(first)),
            Ok(first)
        );
    }

    /// The writer opens `pending-surveys` as collection's reader does, never through a link:
    /// one replaced by a symbolic link to a writable directory, or by a file, is refused naming
    /// its path, and nothing is written where it points.
    #[test]
    fn a_pending_directory_that_is_a_link_is_refused_and_nothing_is_written_through_it() {
        let store = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let object = store.path().join("object");
        std::os::unix::fs::symlink(outside.path(), store.path().join(DIRECTORY)).unwrap();
        for refused in [
            begin(store.path(), SURVEY, 2, &object).unwrap_err(),
            clear(store.path(), SURVEY).unwrap_err(),
        ] {
            assert!(
                refused.contains(&format!(
                    "{} is refused: it is a symbolic link or not a directory",
                    store.path().join(DIRECTORY).display()
                )),
                "{refused}"
            );
        }
        std::fs::write(&object, b"{}").unwrap();
        assert!(written(store.path(), SURVEY, 2, &object, None).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);

        std::fs::remove_file(store.path().join(DIRECTORY)).unwrap();
        std::fs::write(store.path().join(DIRECTORY), b"").unwrap();
        assert!(begin(store.path(), SURVEY, 2, &object).is_err());
    }

    #[test]
    fn the_identity_is_only_ever_the_records_own_regular_file_and_an_entry_stays_small() {
        let store = tempfile::tempdir().unwrap();
        let object = store.path().join("object");
        std::fs::write(&object, b"{}").unwrap();
        begin(store.path(), SURVEY, 2, &object).unwrap();
        // A length other than the record's, a link at the CAS name, or nothing there: refused,
        // and the entry keeps no identity.
        assert!(written(store.path(), SURVEY, 3, &object, None).is_err());
        let link = store.path().join("link");
        std::os::unix::fs::symlink(&object, &link).unwrap();
        assert!(written(store.path(), SURVEY, 2, &link, None).is_err());
        assert!(written(store.path(), SURVEY, 2, &store.path().join("gone"), None).is_err());
        assert_eq!(read(store.path(), SURVEY).unwrap().unwrap().object, None);
        // The largest entry there can be, a record of the ceiling's length with the largest
        // identity, is well within the bound collection reads.
        let largest = SurveyPendingV1 {
            schema: SURVEY_PENDING_V1.into(),
            survey_id: SURVEY.into(),
            bytes: review_core::project::MAX_SURVEY_BYTES.into(),
            object: Some(SurveyObjectIdentityV1 {
                device: u64::MAX.into(),
                inode: u64::MAX.into(),
            }),
        };
        write(store.path(), &largest).unwrap();
        let written = std::fs::read(location(store.path(), SURVEY)).unwrap();
        assert!(written.len() < MAX_ENTRY_BYTES, "{}", written.len());
        // A record over the ceiling has no entry.
        let other = SURVEY.replace('1', "3");
        assert!(
            begin(
                store.path(),
                &other,
                review_core::project::MAX_SURVEY_BYTES + 1,
                &object
            )
            .is_err()
        );
        assert_eq!(read(store.path(), &other), Ok(None));
    }

    #[test]
    fn another_surveys_entry_keeps_the_directory_and_a_wrong_entry_is_refused() {
        let store = tempfile::tempdir().unwrap();
        let object = store.path().join("object");
        let other = SURVEY.replace('1', "2");
        begin(store.path(), SURVEY, 2, &object).unwrap();
        begin(store.path(), &other, 3, &object).unwrap();
        clear(store.path(), SURVEY).unwrap();
        assert_eq!(read(store.path(), &other).unwrap().unwrap().bytes.get(), 3);
        assert!(begin(store.path(), "not-a-digest", 2, &object).is_err());
        assert!(begin(store.path(), SURVEY, 0, &object).is_err());
        std::fs::copy(
            location(store.path(), &other),
            location(store.path(), SURVEY),
        )
        .unwrap();
        assert!(
            read(store.path(), SURVEY).is_err(),
            "an entry names its own Survey"
        );
        assert!(
            begin(store.path(), SURVEY, 2, &object).is_err(),
            "and the writer never replaces one that names another"
        );
    }
}
