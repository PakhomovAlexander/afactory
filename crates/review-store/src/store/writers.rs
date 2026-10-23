//! The releases that wrote a Store (`af/store-writer@1`), in a table beside the event log.
//!
//! Its two columns are the part every release reads: the release, and the versioned record
//! that release wrote. A release reads another's markers by the release column alone, so a
//! later marker shape never hides who wrote the Store; it reads its own record whole.

use std::path::Path;

use review_core::store_writer::{StoreProvenance, StoreWriterV1, is_release};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::{EventStore, StoreError};

/// The release this build is, as `af --version` prints it.
pub const AF_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(super) const CREATE: &str = "CREATE TABLE IF NOT EXISTS store_writers (
     af_version TEXT PRIMARY KEY NOT NULL,
     record     TEXT NOT NULL
 );";

/// Record this release as a writer of the Store `conn` opened, once: when it created the Store,
/// and on the first writing open of each later release. A Store whose log holds events but no
/// marker was written before markers existed and stays unmarked, so it is still named as an
/// earlier release's rather than this one's. A Store that cannot take the row now (another
/// writer holds it, or the file is read-only) keeps the markers it has: the marker names a
/// writer, and no event, replay or budget depends on it.
pub(super) fn record(conn: &Connection, now_unix_ms: u64) {
    let present = conn
        .query_row(
            "SELECT 1 FROM store_writers WHERE af_version = ?1",
            params![AF_VERSION],
            |_| Ok(()),
        )
        .optional();
    if !matches!(present, Ok(None)) {
        return;
    }
    let unmarked_history = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events) AND NOT EXISTS(SELECT 1 FROM store_writers)",
        [],
        |row| row.get::<_, bool>(0),
    );
    if !matches!(unmarked_history, Ok(false)) {
        return;
    }
    let marker = StoreWriterV1::new(AF_VERSION, now_unix_ms);
    let Ok(record) = serde_json::to_string(&marker) else {
        return;
    };
    let _ = conn.execute(
        "INSERT OR IGNORE INTO store_writers (af_version, record) VALUES (?1, ?2)",
        params![AF_VERSION, record],
    );
}

/// Every release that opened the Store for writing, oldest first. A Store from before markers
/// has no table, and has none; a release column that does not spell a release is skipped.
fn releases(conn: &Connection) -> Result<Vec<String>, StoreError> {
    let table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'store_writers'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table.is_none() {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare("SELECT af_version FROM store_writers ORDER BY rowid ASC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut releases = Vec::new();
    for release in rows {
        let release = release?;
        if is_release(&release) {
            releases.push(release);
        }
    }
    Ok(releases)
}

impl EventStore {
    /// The releases that opened this Store for writing, oldest first. A read.
    pub fn writer_releases(&self) -> Result<Vec<String>, StoreError> {
        releases(&self.conn)
    }

    /// This release's own marker, read whole and validated, when it wrote one. A read.
    pub fn own_writer_marker(&self) -> Result<Option<StoreWriterV1>, StoreError> {
        if releases(&self.conn)?
            .iter()
            .all(|release| release != AF_VERSION)
        {
            return Ok(None);
        }
        let record: String = self.conn.query_row(
            "SELECT record FROM store_writers WHERE af_version = ?1",
            params![AF_VERSION],
            |row| row.get(0),
        )?;
        let marker: StoreWriterV1 = serde_json::from_str(&record)?;
        marker.validate().map_err(StoreError::Conflict)?;
        if marker.af_version != AF_VERSION {
            return Err(StoreError::Conflict(
                "a Store writer marker names another release than its row".into(),
            ));
        }
        Ok(Some(marker))
    }

    /// Who wrote this Store, judged by this release. A read.
    pub fn provenance(&self) -> Result<StoreProvenance, StoreError> {
        Ok(StoreProvenance::of(&self.writer_releases()?, AF_VERSION))
    }
}

/// Who wrote the Store whose database is `database`, read without creating, changing or
/// locking anything: a Store this release cannot replay is still one it can name.
pub fn provenance_at(database: &Path) -> Result<StoreProvenance, StoreError> {
    if !std::fs::metadata(database).is_ok_and(|metadata| metadata.is_file()) {
        return Err(StoreError::Conflict(format!(
            "no database file at {}",
            database.display()
        )));
    }
    let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(StoreProvenance::of(&releases(&conn)?, AF_VERSION))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_store_names_this_release_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let store = EventStore::open(&path).unwrap();
        assert_eq!(
            store.writer_releases().unwrap(),
            vec![AF_VERSION.to_string()]
        );
        let marker = store.own_writer_marker().unwrap().unwrap();
        assert_eq!(marker.af_version, AF_VERSION);
        drop(store);
        // Opening again records nothing new.
        let again = EventStore::open(&path).unwrap();
        assert_eq!(
            again.writer_releases().unwrap(),
            vec![AF_VERSION.to_string()]
        );
        assert_eq!(again.provenance().unwrap(), StoreProvenance::ThisRelease);
        assert_eq!(provenance_at(&path).unwrap(), StoreProvenance::ThisRelease);
    }

    #[test]
    fn another_release_and_an_unmarked_store_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        // A Store from before markers: the log alone.
        drop(EventStore::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE store_writers;").unwrap();
        assert_eq!(provenance_at(&path).unwrap(), StoreProvenance::Unmarked);
        conn.execute_batch(CREATE).unwrap();
        conn.execute(
            "INSERT INTO store_writers (af_version, record) VALUES ('99.0.0', '{\"schema\":\"af/store-writer@9\"}'), ('not a release', '{}')",
            [],
        )
        .unwrap();
        drop(conn);
        assert_eq!(
            provenance_at(&path).unwrap(),
            StoreProvenance::OtherReleases(vec!["99.0.0".into()])
        );
        // A writing open of this release adds its own marker beside the other one.
        let store = EventStore::open(&path).unwrap();
        assert_eq!(
            store.writer_releases().unwrap(),
            vec!["99.0.0".to_string(), AF_VERSION.to_string()]
        );
        assert_eq!(
            store.provenance().unwrap().writer(),
            Some("99.0.0"),
            "the fix still names the release that wrote it"
        );
    }

    #[test]
    fn a_store_with_history_and_no_marker_stays_an_earlier_releases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        drop(EventStore::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE store_writers;
             INSERT INTO events (run_id, sequence, event_id, type, occurred_at, artifact_refs,
                                 payload)
             VALUES ('run', 0, 'event', 'TaskTransition@1', '1970-01-01T00:00:00Z', '[]', '{}');",
        )
        .unwrap();
        drop(conn);
        let store = EventStore::open(&path).unwrap();
        assert!(store.writer_releases().unwrap().is_empty());
        assert_eq!(store.provenance().unwrap(), StoreProvenance::Unmarked);
    }
}
