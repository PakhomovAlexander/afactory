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
//! clears it; any other entry is left for collection to settle. A debug build stops at the two
//! crash points a fixture needs: [`stop_before_identity`] after step 2, and [`stop_before_event`]
//! after step 3.

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

/// Where the Project Store at `store` keeps the entry of `survey_id`.
pub(crate) fn location(store: &Path, survey_id: &str) -> PathBuf {
    let hex = survey_id.strip_prefix("sha256:").unwrap_or(survey_id);
    store.join(DIRECTORY).join(format!("{hex}.json"))
}

/// Make the entries of `directory` durable: its own file synced.
fn sync_directory(directory: &Path) -> Result<(), String> {
    std::fs::File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

/// Replace the entry whole through a temporary file, synced, a rename and the directory synced
/// (and the Project Store's, when the directory is new), so the entry is durable on return.
fn write(store: &Path, entry: &SurveyPendingV1) -> Result<(), String> {
    entry.validate()?;
    let file = location(store, &entry.survey_id);
    let directory = file.parent().ok_or("a pending entry has no directory")?;
    if !directory.is_dir() {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        sync_directory(store)?;
    }
    let value = serde_json::to_value(entry).map_err(|error| error.to_string())?;
    let bytes = review_store::canonicalize(&value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "a pending entry of {} bytes is more than {MAX_ENTRY_BYTES}",
            bytes.len()
        ));
    }
    let mut staged =
        tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    staged
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    staged
        .persist(&file)
        .map_err(|error| error.error.to_string())?;
    sync_directory(directory)
}

/// Record that `survey_id`, `bytes` long, is about to be written to the CAS: before any byte
/// is, under the Project Store's lock. An entry a crash left for the same Survey is replaced,
/// since this publication finishes it.
pub(crate) fn begin(store: &Path, survey_id: &str, bytes: u64) -> Result<(), String> {
    write(
        store,
        &SurveyPendingV1 {
            schema: SURVEY_PENDING_V1.into(),
            survey_id: survey_id.into(),
            bytes: bytes.into(),
            object: None,
        },
    )
    .map_err(|error| format!("recording the publication of Survey {survey_id}: {error}"))
}

/// Record the identity of the file at `object`, the Survey's CAS name, that now holds
/// `survey_id`'s `bytes` durably: stated without following a link, and refused unless it is a
/// regular file of exactly that length, so the entry never describes another object.
pub(crate) fn written(
    store: &Path,
    survey_id: &str,
    bytes: u64,
    object: &Path,
) -> Result<SurveyObjectIdentityV1, String> {
    let stated = std::fs::symlink_metadata(object)
        .map_err(|error| format!("stating the record of Survey {survey_id}: {error}"))?;
    if !stated.file_type().is_file() || stated.len() != bytes {
        return Err(format!(
            "the CAS name of Survey {survey_id} holds no regular file of its {bytes} bytes"
        ));
    }
    let identity = SurveyObjectIdentityV1 {
        device: stated.dev().into(),
        inode: stated.ino().into(),
    };
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
/// empty. An entry already gone is no error.
pub(crate) fn clear(store: &Path, survey_id: &str) -> Result<(), String> {
    match std::fs::remove_file(location(store, survey_id)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "clearing the publication of Survey {survey_id}: {error}"
            ));
        }
    }
    // Another entry keeps the directory.
    let _ = std::fs::remove_dir(store.join(DIRECTORY));
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
        std::fs::write(&object, b"{}").unwrap();
        assert_eq!(read(store.path(), SURVEY), Ok(None));

        begin(store.path(), SURVEY, 2).unwrap();
        let begun = read(store.path(), SURVEY).unwrap().unwrap();
        assert_eq!(begun.bytes.get(), 2);
        assert_eq!(
            begun.object, None,
            "no identity before the bytes are written"
        );

        let identity = written(store.path(), SURVEY, 2, &object).unwrap();
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

        // A crash left it: the same Survey begins again and is cleared.
        begin(store.path(), SURVEY, 2).unwrap();
        assert_eq!(read(store.path(), SURVEY).unwrap().unwrap().object, None);
        clear(store.path(), SURVEY).unwrap();
        assert_eq!(read(store.path(), SURVEY), Ok(None));
        assert!(
            !store.path().join(DIRECTORY).exists(),
            "the empty directory goes"
        );
        clear(store.path(), SURVEY).unwrap();
    }

    #[test]
    fn the_identity_is_only_ever_the_records_own_regular_file_and_an_entry_stays_small() {
        let store = tempfile::tempdir().unwrap();
        let object = store.path().join("object");
        std::fs::write(&object, b"{}").unwrap();
        begin(store.path(), SURVEY, 2).unwrap();
        // A length other than the record's, a link at the CAS name, or nothing there: refused,
        // and the entry keeps no identity.
        assert!(written(store.path(), SURVEY, 3, &object).is_err());
        let link = store.path().join("link");
        std::os::unix::fs::symlink(&object, &link).unwrap();
        assert!(written(store.path(), SURVEY, 2, &link).is_err());
        assert!(written(store.path(), SURVEY, 2, &store.path().join("gone")).is_err());
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
        assert!(
            begin(
                store.path(),
                SURVEY,
                review_core::project::MAX_SURVEY_BYTES + 1
            )
            .is_err()
        );
    }

    #[test]
    fn another_surveys_entry_keeps_the_directory_and_a_wrong_entry_is_refused() {
        let store = tempfile::tempdir().unwrap();
        let other = SURVEY.replace('1', "2");
        begin(store.path(), SURVEY, 2).unwrap();
        begin(store.path(), &other, 3).unwrap();
        clear(store.path(), SURVEY).unwrap();
        assert_eq!(read(store.path(), &other).unwrap().unwrap().bytes.get(), 3);
        assert!(begin(store.path(), "not-a-digest", 2).is_err());
        assert!(begin(store.path(), SURVEY, 0).is_err());
        std::fs::copy(
            location(store.path(), &other),
            location(store.path(), SURVEY),
        )
        .unwrap();
        assert!(
            read(store.path(), SURVEY).is_err(),
            "an entry names its own Survey"
        );
    }
}
