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

/// Record this release as a writer of the Store, in `tx`, the transaction that appends this
/// release's events: every event a Store holds was written by a release its markers name, and
/// a Store this release only opened, read or refused gets no marker. The marker is part of the
/// append, so a marker that cannot be written fails the append as a whole and no event is
/// committed without it. A Store whose log holds events but no marker was written before
/// markers existed; this release does not refuse it here — replay is the release boundary
/// (ADR-0145) — and once this release appends to it, its marker names this release too.
pub(super) fn mark(tx: &Connection, now_unix_ms: u64) -> Result<(), StoreError> {
    let present = tx
        .query_row(
            "SELECT 1 FROM store_writers WHERE af_version = ?1",
            params![AF_VERSION],
            |_| Ok(()),
        )
        .optional()?;
    if present.is_some() {
        return Ok(());
    }
    let marker = StoreWriterV1::new(AF_VERSION, now_unix_ms);
    marker.validate().map_err(StoreError::Conflict)?;
    let record = serde_json::to_string(&marker)?;
    tx.execute(
        "INSERT INTO store_writers (af_version, record) VALUES (?1, ?2)",
        params![AF_VERSION, record],
    )?;
    Ok(())
}

/// Whether the log holds any event; a database without an events table holds none.
fn has_events(conn: &Connection) -> Result<bool, StoreError> {
    let table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'events'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table.is_none() {
        return Ok(false);
    }
    Ok(
        conn.query_row("SELECT EXISTS(SELECT 1 FROM events)", [], |row| {
            row.get::<_, bool>(0)
        })?,
    )
}

/// Who wrote the Store `conn` opened: a Store with neither a marker nor an event was written by
/// nobody yet, so it is this release's.
fn provenance_of(conn: &Connection) -> Result<StoreProvenance, StoreError> {
    let releases = releases(conn)?;
    if releases.is_empty() && !has_events(conn)? {
        return Ok(StoreProvenance::ThisRelease);
    }
    Ok(StoreProvenance::of(&releases, AF_VERSION))
}

/// Every release that appended to the Store, oldest first. A Store from before markers
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
    /// The releases that appended to this Store, oldest first. A read.
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
        provenance_of(&self.conn)
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
    provenance_of(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cas, NewEvent};
    use review_core::EventType;
    use serde_json::json;

    fn append(store: &mut EventStore, cas: &Cas) -> Result<(), StoreError> {
        store
            .append(
                "run",
                cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({ "i": 1 })),
            )
            .map(|_| ())
    }

    fn insert_event(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO events (run_id, sequence, event_id, type, occurred_at, artifact_refs,
                                 payload)
             VALUES ('other', 0, 'event', 'TaskTransition@9', '1970-01-01T00:00:00Z', '[]', '{}');",
        )
        .unwrap();
    }

    #[test]
    fn a_store_names_this_release_once_it_appended_and_never_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(&path).unwrap();
        // Opened and never written: no marker, and nobody else's Store either.
        assert!(store.writer_releases().unwrap().is_empty());
        assert_eq!(store.provenance().unwrap(), StoreProvenance::ThisRelease);
        assert_eq!(provenance_at(&path).unwrap(), StoreProvenance::ThisRelease);
        append(&mut store, &cas).unwrap();
        assert_eq!(
            store.writer_releases().unwrap(),
            vec![AF_VERSION.to_string()]
        );
        let marker = store.own_writer_marker().unwrap().unwrap();
        assert_eq!(marker.af_version, AF_VERSION);
        drop(store);
        // Opening and appending again records nothing new.
        let mut again = EventStore::open(&path).unwrap();
        append(&mut again, &cas).unwrap();
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
        // A Store from before markers: the log alone, with history.
        drop(EventStore::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE store_writers;").unwrap();
        insert_event(&conn);
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
        // A writing open of this release adds nothing until it appends.
        let store = EventStore::open(&path).unwrap();
        assert_eq!(store.writer_releases().unwrap(), vec!["99.0.0".to_string()]);
        assert_eq!(
            store.provenance().unwrap().writer(),
            Some("99.0.0"),
            "the fix still names the release that wrote it"
        );
    }

    /// An unmarked Store with history is not refused here; the append that this release makes
    /// to it records this release's marker with its event.
    #[test]
    fn an_append_to_an_unmarked_store_with_history_marks_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        drop(EventStore::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE store_writers;").unwrap();
        insert_event(&conn);
        drop(conn);
        let mut store = EventStore::open(&path).unwrap();
        assert!(store.writer_releases().unwrap().is_empty());
        assert_eq!(store.provenance().unwrap(), StoreProvenance::Unmarked);
        append(&mut store, &cas).unwrap();
        assert_eq!(
            store.writer_releases().unwrap(),
            vec![AF_VERSION.to_string()]
        );
    }

    /// A marker that cannot be written (here a trigger refuses it, as a busy or failing write
    /// would) fails the append whole: no event is committed without this release's marker.
    #[test]
    fn a_failed_marker_write_fails_the_append_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_marker BEFORE INSERT ON store_writers
             BEGIN SELECT RAISE(ABORT, 'database is locked'); END;",
        )
        .unwrap();
        let error = append(&mut store, &cas).unwrap_err();
        assert!(error.to_string().contains("database is locked"), "{error}");
        assert!(
            store.replay("run").unwrap().is_empty(),
            "no event without its marker"
        );
        assert!(store.writer_releases().unwrap().is_empty());
        // Once the marker can be written, the same append lands with it.
        conn.execute_batch("DROP TRIGGER refuse_marker;").unwrap();
        append(&mut store, &cas).unwrap();
        assert_eq!(store.replay("run").unwrap().len(), 1);
        assert_eq!(
            store.writer_releases().unwrap(),
            vec![AF_VERSION.to_string()]
        );
    }
}
