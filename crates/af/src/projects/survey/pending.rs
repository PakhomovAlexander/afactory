//! The pending-publication entries of a Project Store (ADR-0152 clause 2).
//!
//! A Survey is published in two steps: its record goes into the Project Store's CAS, then
//! `SurveyRecorded@1` into its log. A process that dies between the two leaves a record no event
//! references, which no inventory that starts from the log would see. So the Surveyor, under the
//! Project Store's lock, first writes one machine-local entry, `af/survey-pending@1` at
//! `pending-surveys/<64 hex>.json` beside `project.lock`, naming the Survey and the length of
//! its bytes; adds the identity (device and inode) of the CAS object once the bytes are written;
//! and removes the entry once the event committed. An entry is never in the repository or the
//! log. A refresh that finds the entry of the Survey it is publishing (a crash left it, and the
//! same bytes came again) finishes that publication and clears it; any other entry is left for
//! collection to settle.

use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use review_core::project::{SURVEY_PENDING_V1, SurveyObjectIdentityV1, SurveyPendingV1};

/// The directory of the entries, beside `project.lock`.
pub(crate) const DIRECTORY: &str = "pending-surveys";

/// A debug build's fixture may stop a refresh after its record's bytes and before its event,
/// as a crash would. Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_STOP_BEFORE_EVENT: &str = "AF_TEST_SURVEY_STOP_BEFORE_EVENT";

/// Where the Project Store at `store` keeps the entry of `survey_id`.
pub(crate) fn location(store: &Path, survey_id: &str) -> PathBuf {
    let hex = survey_id.strip_prefix("sha256:").unwrap_or(survey_id);
    store.join(DIRECTORY).join(format!("{hex}.json"))
}

/// Replace the entry whole through a temporary file, synced, and a rename.
fn write(store: &Path, entry: &SurveyPendingV1) -> Result<(), String> {
    entry.validate()?;
    let file = location(store, &entry.survey_id);
    let directory = file.parent().ok_or("a pending entry has no directory")?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let value = serde_json::to_value(entry).map_err(|error| error.to_string())?;
    let bytes = review_store::canonicalize(&value).map_err(|error| error.to_string())?;
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
    Ok(())
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

/// Record the identity of the CAS object at `object` that now holds `survey_id`'s bytes,
/// stated without following a link.
pub(crate) fn written(
    store: &Path,
    survey_id: &str,
    bytes: u64,
    object: &Path,
) -> Result<SurveyObjectIdentityV1, String> {
    let stated = std::fs::symlink_metadata(object)
        .map_err(|error| format!("stating the record of Survey {survey_id}: {error}"))?;
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

/// Stop here, as a crash between the record's bytes and its event would, when a debug build's
/// fixture asks.
pub(crate) fn stop_before_event() {
    #[cfg(debug_assertions)]
    if std::env::var_os(TEST_STOP_BEFORE_EVENT).is_some_and(|value| value == "1") {
        eprintln!("af: stopped before SurveyRecorded@1 ({TEST_STOP_BEFORE_EVENT})");
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
