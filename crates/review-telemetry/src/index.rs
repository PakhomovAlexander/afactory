//! The index (design §6): `v1/index.sqlite`, a rebuildable projection over the segments, never
//! evidence of anything.
//!
//! It holds `segments`, `runs`, `executions`, `spans` (with `self_ns` computed at ingest),
//! `events`, `configs`, `nodes`, `findings`, `demands`, `blobs`, `blob_refs` and `problems`.
//! Ingest reads every sealed segment the index has not seen and the tail of each `.jsonl`
//! segment beyond its last ingested byte offset, in one transaction, idempotently by `span_id`,
//! segment id with `seq`, and digest. Exact counters stay `TEXT` and are aggregated in Rust
//! ([`sum_decimals`]).
//!
//! **Lifecycle.** Every connection holds `index.lock` shared for its lifetime. Replacing the
//! index ([`replace`]) takes it exclusively within two seconds, then checkpoints, closes, unlinks
//! the database with its WAL and shared-memory files, and recreates it, so no connection ever
//! sees a database replaced under it. A command that cannot have the shared lock within two
//! seconds reads the segments directly ([`Index::open`] returns an in-memory index) and says
//! so on stderr.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde_json::Value;

use crate::layout::{self, Held, Layout};
use crate::record::{Record, SealExit};
use crate::segment::reader::{self, Position};
use crate::trace;

/// The index's own shape; an index of another is refused and rebuilt by `reindex`.
pub const INDEX_SCHEMA: &str = "af/telemetry-index@1";

const SCHEMA_SQL: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE segments(
    segment_id TEXT PRIMARY KEY,
    state TEXT NOT NULL,
    ingested_offset INTEGER NOT NULL,
    last_seq INTEGER NOT NULL,
    sealed_last INTEGER NOT NULL,
    pid INTEGER NOT NULL,
    level TEXT NOT NULL,
    started_unix_ns INTEGER NOT NULL,
    ended_unix_ns INTEGER,
    exit TEXT,
    dropped INTEGER,
    discarded_bytes INTEGER,
    header TEXT NOT NULL
);
CREATE TABLE runs(
    trace_id TEXT PRIMARY KEY,
    store_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    kind TEXT
);
CREATE TABLE executions(
    span_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL,
    trace_id TEXT,
    ordinal INTEGER,
    store_sequence_at_start TEXT
);
CREATE TABLE spans(
    span_id TEXT PRIMARY KEY,
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    trace_id TEXT,
    parent_span_id TEXT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    start_unix_ns INTEGER NOT NULL,
    ended INTEGER NOT NULL DEFAULT 0,
    end_unix_ns INTEGER,
    status TEXT,
    attributes TEXT NOT NULL,
    end_attributes TEXT,
    self_ns INTEGER
);
CREATE INDEX spans_parent ON spans(parent_span_id);
CREATE INDEX spans_trace ON spans(trace_id);
CREATE TABLE events(
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    span_id TEXT NOT NULL,
    time_unix_ns INTEGER NOT NULL,
    name TEXT NOT NULL,
    attributes TEXT NOT NULL,
    blob TEXT,
    blob_media_type TEXT,
    PRIMARY KEY(segment_id, seq)
);
CREATE TABLE configs(
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    execution_id TEXT NOT NULL,
    part INTEGER NOT NULL,
    parts INTEGER NOT NULL,
    complete INTEGER NOT NULL,
    rows TEXT NOT NULL,
    PRIMARY KEY(segment_id, seq)
);
CREATE TABLE nodes(
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    execution_id TEXT NOT NULL,
    plan_digest TEXT NOT NULL,
    node_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    package TEXT,
    depends_on TEXT NOT NULL,
    expected_nodes INTEGER NOT NULL,
    PRIMARY KEY(segment_id, seq)
);
CREATE TABLE findings(
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    span_id TEXT NOT NULL,
    key TEXT NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY(segment_id, seq)
);
CREATE TABLE demands(
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    span_id TEXT NOT NULL,
    demand_id TEXT NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY(segment_id, seq)
);
CREATE TABLE blobs(
    digest TEXT PRIMARY KEY,
    bytes INTEGER NOT NULL,
    stored_bytes INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    redaction TEXT NOT NULL
);
CREATE TABLE blob_refs(
    occurrence_id TEXT PRIMARY KEY,
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    digest TEXT,
    span_id TEXT NOT NULL,
    event_seq INTEGER NOT NULL,
    source_digest TEXT NOT NULL,
    original_bytes INTEGER NOT NULL,
    truncated INTEGER NOT NULL,
    state TEXT NOT NULL
);
CREATE TABLE problems(
    segment_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    time_unix_ns INTEGER NOT NULL,
    what TEXT NOT NULL,
    reason TEXT NOT NULL,
    PRIMARY KEY(segment_id, seq)
);
";

/// The tables, in dump order, each with the columns that order its rows.
const TABLES: [(&str, &str); 13] = [
    ("segments", "segment_id"),
    ("runs", "trace_id"),
    ("executions", "span_id"),
    ("spans", "span_id"),
    ("events", "segment_id, seq"),
    ("configs", "segment_id, seq"),
    ("nodes", "segment_id, seq"),
    ("findings", "segment_id, seq"),
    ("demands", "segment_id, seq"),
    ("blobs", "digest"),
    ("blob_refs", "occurrence_id"),
    ("problems", "segment_id, seq"),
    ("meta", "key"),
];

/// How the index was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The shared index file, under `index.lock` shared.
    Shared,
    /// The lock was not had within two seconds: an in-memory index built from the segments
    /// directly.
    Direct,
}

/// An open index connection.
pub struct Index {
    connection: Connection,
    layout: Layout,
    mode: Mode,
    /// Why the shared index was not used, for stderr.
    pub note: Option<String>,
    _lock: Option<Held>,
}

/// What one ingest read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ingested {
    pub segments: u64,
    pub records: u64,
    pub bytes: u64,
}

fn sql(error: rusqlite::Error) -> String {
    format!("telemetry index: {error}")
}

fn ns(value: &str) -> i64 {
    value.parse::<i64>().unwrap_or(i64::MAX)
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).expect("a value serializes")
}

fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn create(connection: &Connection) -> Result<(), String> {
    connection.execute_batch(SCHEMA_SQL).map_err(sql)?;
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES ('schema', ?1)",
            params![INDEX_SCHEMA],
        )
        .map_err(sql)?;
    Ok(())
}

fn configure(connection: &Connection) -> Result<(), String> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sql)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(sql)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(sql)?;
    Ok(())
}

/// Open the index file, creating it when absent; refuse one of another shape or schema version
/// (ADR-0145: refused, never migrated), naming the file, both schemas and the command that
/// replaces it. A refused file is left as it was found.
fn open_file(layout: &Layout) -> Result<Connection, String> {
    let path = layout.index();
    let connection = Connection::open(&path).map_err(sql)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sql)?;
    let has_meta: bool = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sql)?
        > 0;
    if !has_meta {
        let tables: i64 = connection
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .map_err(sql)?;
        if tables > 0 {
            return Err(format!(
                "{}: the telemetry index is of another shape, not {INDEX_SCHEMA}; it is refused, \
                 never migrated: `af telemetry reindex` replaces it",
                path.display()
            ));
        }
        configure(&connection)?;
        create(&connection)?;
        return Ok(connection);
    }
    let schema: Option<String> = connection
        .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql)?;
    if schema.as_deref() != Some(INDEX_SCHEMA) {
        return Err(format!(
            "{}: the telemetry index is {}, not {INDEX_SCHEMA}; it is refused, never migrated: \
             `af telemetry reindex` replaces it",
            path.display(),
            schema.unwrap_or_else(|| "unversioned".into())
        ));
    }
    configure(&connection)?;
    Ok(connection)
}

impl Index {
    /// Open the directory's index under `index.lock` shared, waiting at most two seconds; when
    /// the lock is not had, an in-memory index of the segments read directly, with
    /// [`Self::note`] saying why. An index file of another shape or schema version is refused
    /// with `Err`, never read around: the reader fails closed and `reindex` replaces it.
    /// Nothing is ingested yet.
    pub fn open(layout: &Layout) -> Result<Self, String> {
        let deadline = std::time::Instant::now() + layout::LOCK_WAIT;
        let intent = layout::shared_within(&layout.index_intent(), layout::LOCK_WAIT)
            .map_err(|error| format!("{}: {error}", layout.index_intent().display()))?;
        let lock = match intent {
            Some(_) => layout::shared_within(
                &layout.index_lock(),
                deadline.saturating_duration_since(std::time::Instant::now()),
            )
            .map_err(|error| format!("{}: {error}", layout.index_lock().display()))?,
            None => None,
        };
        drop(intent);
        let Some(lock) = lock else {
            return Self::direct(
                layout,
                "the telemetry index is being replaced; reading the segments directly",
            );
        };
        Ok(Self {
            connection: open_file(layout)?,
            layout: layout.clone(),
            mode: Mode::Shared,
            note: None,
            _lock: Some(lock),
        })
    }

    /// An in-memory index over the segments, read without the index file.
    pub fn direct(layout: &Layout, note: &str) -> Result<Self, String> {
        let connection = Connection::open_in_memory().map_err(sql)?;
        create(&connection)?;
        Ok(Self {
            connection,
            layout: layout.clone(),
            mode: Mode::Direct,
            note: Some(note.to_string()),
            _lock: None,
        })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Ingest every new segment byte in one transaction: sealed segments not yet seen whole,
    /// and `.jsonl` tails beyond their ingested offset. Rows of segments that are gone or
    /// corrupt are removed, so a corrupt segment is excluded from every query. A segment that
    /// does not read is refused and listed; the others are ingested.
    pub fn ingest(&mut self) -> Result<(Ingested, Vec<(String, String)>), String> {
        let listing = reader::list(&self.layout)?;
        let transaction = self.connection.transaction().map_err(sql)?;
        let mut ingested = Ingested::default();
        let mut refused = Vec::new();
        let mut ended = BTreeSet::new();
        let present: BTreeSet<&String> = listing
            .sealed
            .iter()
            .chain(&listing.live)
            .chain(&listing.unsealed)
            .collect();
        let known: Vec<(String, String)> = {
            let mut statement = transaction
                .prepare("SELECT segment_id, state FROM segments")
                .map_err(sql)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?
        };
        for (segment_id, _) in &known {
            if !present.contains(segment_id) {
                forget(&transaction, segment_id)?;
            }
        }
        let states: BTreeMap<String, String> = known.into_iter().collect();
        for segment_id in &listing.sealed {
            if states.get(segment_id).map(String::as_str) == Some("sealed") {
                continue;
            }
            let path = self.layout.sealed_segment(segment_id);
            let records = match reader::read_sealed(&path) {
                Ok(records) => records,
                Err(reason) => {
                    forget(&transaction, segment_id)?;
                    refused.push((path.display().to_string(), reason));
                    continue;
                }
            };
            let savepoint = SegmentIngest::new(segment_id);
            match savepoint.apply(&transaction, &records, &mut ended) {
                Ok(()) => {
                    let bytes = reader::file_len(&path);
                    set_offset(&transaction, segment_id, bytes, &records, "sealed")?;
                    ingested.segments += 1;
                    ingested.records += records.len() as u64;
                    ingested.bytes += bytes;
                }
                Err(reason) => {
                    forget(&transaction, segment_id)?;
                    refused.push((path.display().to_string(), reason));
                }
            }
        }
        for segment_id in listing.live.iter().chain(&listing.unsealed) {
            let path = self.layout.live_segment(segment_id);
            let (offset, position) = transaction
                .query_row(
                    "SELECT ingested_offset, last_seq, sealed_last FROM segments WHERE segment_id = ?1",
                    params![segment_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql)?
                .map_or((0, None), |(offset, seq, sealed)| {
                    (
                        usize::try_from(offset).unwrap_or(0),
                        Some(Position {
                            seq: u64::try_from(seq).unwrap_or(0),
                            sealed: sealed != 0,
                        }),
                    )
                });
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                // Sealed or recovered between the listing and the read: the next ingest has it.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    refused.push((path.display().to_string(), error.to_string()));
                    continue;
                }
            };
            let (offset, position) = if offset > bytes.len() {
                (0, None)
            } else {
                (offset, position)
            };
            let prefix = reader::scan_from(&bytes, offset, position);
            if prefix.interior_corruption {
                // An invalid line before a valid one: the segment is unreadable now, exactly as
                // it is once recovery renames it `.corrupt`, so no query sees any of it.
                forget(&transaction, segment_id)?;
                refused.push((
                    path.display().to_string(),
                    format!(
                        "interior corruption: an invalid line ({}) precedes a valid one; \
                         unreadable",
                        prefix.invalid.as_deref().unwrap_or("not a record")
                    ),
                ));
                continue;
            }
            if prefix.records.is_empty() {
                continue;
            }
            match SegmentIngest::new(segment_id).apply(&transaction, &prefix.records, &mut ended) {
                Ok(()) => {
                    let last = prefix.records.last().expect("not empty");
                    transaction
                        .execute(
                            "UPDATE segments SET ingested_offset = ?2, last_seq = ?3, sealed_last = ?4
                             WHERE segment_id = ?1",
                            params![
                                segment_id,
                                as_i64(prefix.kept_bytes as u64),
                                as_i64(last.seq()),
                                i64::from(matches!(last, Record::Seal(_)))
                            ],
                        )
                        .map_err(sql)?;
                    ingested.records += prefix.records.len() as u64;
                    ingested.bytes += (prefix.kept_bytes - offset) as u64;
                }
                Err(reason) => {
                    forget(&transaction, segment_id)?;
                    refused.push((path.display().to_string(), reason));
                }
            }
        }
        for segment_id in &listing.corrupt {
            forget(&transaction, segment_id)?;
        }
        self_times(&transaction, &ended)?;
        transaction.commit().map_err(sql)?;
        Ok((ingested, refused))
    }

    /// Every table's rows in a fixed order, each row as its column texts: what any query of the
    /// index can see. Two indexes over the same segments dump identically.
    pub fn dump(&self) -> Result<BTreeMap<String, Vec<Vec<String>>>, String> {
        let mut dump = BTreeMap::new();
        for (table, order) in TABLES {
            if table == "segments" {
                // The byte offsets are bookkeeping of how ingest progressed, not content.
                let mut statement = self
                    .connection
                    .prepare(
                        "SELECT segment_id, state, last_seq, sealed_last, pid, level,
                                started_unix_ns, ended_unix_ns, exit, dropped, discarded_bytes,
                                header FROM segments ORDER BY segment_id",
                    )
                    .map_err(sql)?;
                dump.insert(table.to_string(), rows(&mut statement)?);
                continue;
            }
            let mut statement = self
                .connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
                .map_err(sql)?;
            dump.insert(table.to_string(), rows(&mut statement)?);
        }
        Ok(dump)
    }

    /// One count.
    pub fn count(&self, query: &str) -> Result<u64, String> {
        self.connection
            .query_row(query, [], |row| row.get::<_, i64>(0))
            .map(|count| u64::try_from(count).unwrap_or(0))
            .map_err(sql)
    }

    /// Distinct problems with how many segments recorded each.
    pub fn problems(&self) -> Result<Vec<(String, String, u64)>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT what, reason, count(*) FROM problems GROUP BY what, reason
                 ORDER BY what, reason",
            )
            .map_err(sql)?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                ))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)
    }

    /// Every sealed segment's seal as the index holds it: `(segment_id, exit, capture)`, the
    /// capture `complete` or why it is not ([`crate::record::capture_label`]): a recovered
    /// segment is never shown as a clean exit, and one whose drops are unknown says so.
    pub fn seals(&self) -> Result<Vec<(String, String, String)>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT segment_id, exit, dropped, discarded_bytes FROM segments
                 WHERE exit IS NOT NULL ORDER BY segment_id",
            )
            .map_err(sql)?;
        statement
            .query_map([], |row| {
                let exit: String = row.get(1)?;
                let dropped: Option<i64> = row.get(2)?;
                let discarded: Option<i64> = row.get(3)?;
                let capture = SealExit::parse(&exit).map_or_else(
                    || format!("unknown exit {exit}"),
                    |parsed| {
                        crate::record::capture_label(
                            parsed,
                            dropped.map(|value| u64::try_from(value).unwrap_or(0)),
                            discarded.map_or(0, |value| u64::try_from(value).unwrap_or(0)),
                        )
                    },
                );
                Ok((row.get(0)?, exit, capture))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)
    }

    /// Stored occurrences whose blob file is missing: `(occurrence_id, digest)`, looked for
    /// under `gc.lock` shared. `None` when the lock was not had within two seconds (a sweep
    /// holds it): the check is deferred, never answered while files may be collected.
    pub fn missing_blobs(&self) -> Result<Option<Vec<(String, String)>>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT occurrence_id, digest FROM blob_refs
                 WHERE state = 'stored' AND digest IS NOT NULL ORDER BY occurrence_id",
            )
            .map_err(sql)?;
        let all: Vec<(String, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        if all.is_empty() {
            return Ok(Some(all));
        }
        let Some(_shared) = layout::shared_within(&self.layout.gc_lock(), layout::LOCK_WAIT)
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        let mut present = BTreeMap::new();
        Ok(Some(
            all.into_iter()
                .filter(|(_, digest)| {
                    !*present
                        .entry(digest.clone())
                        .or_insert_with(|| crate::blob::reader::exists(&self.layout, digest))
                })
                .collect(),
        ))
    }

    /// The occurrence record of `occurrence_id`, as the index holds it.
    pub fn occurrence(
        &self,
        occurrence_id: &str,
    ) -> Result<Option<crate::record::BlobRef>, String> {
        self.connection
            .query_row(
                "SELECT occurrence_id, digest, span_id, event_seq, source_digest, original_bytes,
                        truncated, state, seq FROM blob_refs WHERE occurrence_id = ?1",
                params![occurrence_id],
                |row| {
                    let state: String = row.get(7)?;
                    Ok(crate::record::BlobRef {
                        seq: u64::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
                        schema: crate::record::schema::BLOB_REF.into(),
                        occurrence_id: row.get(0)?,
                        digest: row.get(1)?,
                        span_id: row.get(2)?,
                        event_seq: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                        source_digest: row.get(4)?,
                        original_bytes: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                        truncated: row.get::<_, i64>(6)? != 0,
                        state: if state == "stored" {
                            crate::record::BlobState::Stored
                        } else {
                            crate::record::BlobState::Dropped
                        },
                    })
                },
            )
            .optional()
            .map_err(sql)
    }
}

fn rows(statement: &mut rusqlite::Statement<'_>) -> Result<Vec<Vec<String>>, String> {
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|index| {
                    Ok(match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => "NULL".to_string(),
                        rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                        rusqlite::types::ValueRef::Real(value) => value.to_string(),
                        rusqlite::types::ValueRef::Text(text) => {
                            format!("'{}'", String::from_utf8_lossy(text))
                        }
                        rusqlite::types::ValueRef::Blob(bytes) => format!("x{}", trace::hex(bytes)),
                    })
                })
                .collect::<Result<Vec<String>, rusqlite::Error>>()
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

/// Remove every row a segment contributed.
fn forget(transaction: &Transaction<'_>, segment_id: &str) -> Result<(), String> {
    let spans: Vec<String> = {
        let mut statement = transaction
            .prepare("SELECT span_id FROM spans WHERE segment_id = ?1")
            .map_err(sql)?;
        statement
            .query_map(params![segment_id], |row| row.get(0))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    for span in &spans {
        transaction
            .execute("DELETE FROM executions WHERE span_id = ?1", params![span])
            .map_err(sql)?;
    }
    for table in [
        "spans",
        "events",
        "configs",
        "nodes",
        "findings",
        "demands",
        "blob_refs",
        "problems",
        "segments",
    ] {
        transaction
            .execute(
                &format!("DELETE FROM {table} WHERE segment_id = ?1"),
                params![segment_id],
            )
            .map_err(sql)?;
    }
    // Runs and blob objects no remaining record names.
    transaction
        .execute(
            "DELETE FROM runs WHERE trace_id NOT IN
                 (SELECT trace_id FROM executions WHERE trace_id IS NOT NULL)",
            [],
        )
        .map_err(sql)?;
    transaction
        .execute(
            "DELETE FROM blobs WHERE digest NOT IN
                 (SELECT digest FROM blob_refs WHERE digest IS NOT NULL)",
            [],
        )
        .map_err(sql)?;
    Ok(())
}

fn set_offset(
    transaction: &Transaction<'_>,
    segment_id: &str,
    bytes: u64,
    records: &[Record],
    state: &str,
) -> Result<(), String> {
    let last = records.last().map_or(0, Record::seq);
    transaction
        .execute(
            "UPDATE segments SET state = ?2, ingested_offset = ?3, last_seq = ?4, sealed_last = 1
             WHERE segment_id = ?1",
            params![segment_id, state, as_i64(bytes), as_i64(last)],
        )
        .map_err(sql)?;
    Ok(())
}

/// The ingest of one segment's records.
struct SegmentIngest<'a> {
    segment_id: &'a str,
}

impl<'a> SegmentIngest<'a> {
    fn new(segment_id: &'a str) -> Self {
        Self { segment_id }
    }

    fn apply(
        &self,
        transaction: &Transaction<'_>,
        records: &[Record],
        ended: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        let segment_id = self.segment_id;
        for record in records {
            let seq = as_i64(record.seq());
            match record {
                Record::Segment(header) => {
                    if header.segment_id != segment_id {
                        return Err(format!(
                            "the header names segment {}, not {segment_id}",
                            header.segment_id
                        ));
                    }
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO segments(segment_id, state, ingested_offset,
                                 last_seq, sealed_last, pid, level, started_unix_ns, header)
                             VALUES (?1, 'live', 0, 1, 0, ?2, ?3, ?4, ?5)",
                            params![
                                segment_id,
                                i64::from(header.pid),
                                header.level.as_str(),
                                ns(&header.started_unix_ns),
                                json(header)
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::SpanStart(start) => {
                    if start.span_id != trace::span_id(segment_id, start.seq) {
                        return Err(format!(
                            "span {} at seq {} is not the id its segment and seq derive",
                            start.span_id, start.seq
                        ));
                    }
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO spans(span_id, segment_id, seq, trace_id,
                                 parent_span_id, name, kind, start_unix_ns, attributes)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                            params![
                                start.span_id,
                                segment_id,
                                seq,
                                start.trace_id,
                                start.parent_span_id,
                                start.name,
                                start.kind.as_str(),
                                ns(&start.start_unix_ns),
                                json(&start.attributes)
                            ],
                        )
                        .map_err(sql)?;
                    if start.name == "af.run" {
                        let text = |key: &str| {
                            start
                                .attributes
                                .get(key)
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        };
                        if let (Some(store), Some(run)) = (text("af.store_id"), text("af.run_id")) {
                            let trace = trace::trace_id(&store, &run);
                            transaction
                                .execute(
                                    "INSERT OR IGNORE INTO runs(trace_id, store_id, run_id, kind)
                                     VALUES (?1, ?2, ?3, ?4)",
                                    params![trace, store, run, text("af.run.kind")],
                                )
                                .map_err(sql)?;
                            transaction
                                .execute(
                                    "INSERT OR IGNORE INTO executions(span_id, execution_id,
                                         trace_id, ordinal, store_sequence_at_start)
                                     VALUES (?1, ?2, ?3, ?4, ?5)",
                                    params![
                                        start.span_id,
                                        text("af.execution_id")
                                            .unwrap_or_else(|| segment_id.to_string()),
                                        trace,
                                        start
                                            .attributes
                                            .get("af.execution.ordinal")
                                            .and_then(Value::as_i64),
                                        text("af.store.sequence_at_start")
                                    ],
                                )
                                .map_err(sql)?;
                        }
                    }
                }
                Record::SpanEnd(end) => {
                    let changed = transaction
                        .execute(
                            "UPDATE spans SET ended = 1, end_unix_ns = ?2, status = ?3,
                                 end_attributes = ?4
                             WHERE span_id = ?1 AND ended = 0",
                            params![
                                end.span_id,
                                end.end_unix_ns.as_deref().map(ns),
                                end.status.as_str(),
                                json(&end.attributes)
                            ],
                        )
                        .map_err(sql)?;
                    if changed > 0 {
                        ended.insert(end.span_id.clone());
                    }
                }
                Record::Event(event) => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO events(segment_id, seq, span_id, time_unix_ns,
                                 name, attributes, blob, blob_media_type)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                segment_id,
                                seq,
                                event.span_id,
                                ns(&event.time_unix_ns),
                                event.name,
                                json(&event.attributes),
                                event.blob,
                                event.blob_media_type
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::Config(part) => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO configs(segment_id, seq, execution_id, part,
                                 parts, complete, rows)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                segment_id,
                                seq,
                                part.execution_id,
                                i64::from(part.part),
                                i64::from(part.parts),
                                i64::from(part.complete),
                                json(&part.rows)
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::Node(node) => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO nodes(segment_id, seq, execution_id,
                                 plan_digest, node_id, kind, package, depends_on, expected_nodes)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                            params![
                                segment_id,
                                seq,
                                node.execution_id,
                                node.plan_digest,
                                node.node_id,
                                node.kind,
                                node.package,
                                json(&node.depends_on),
                                i64::from(node.expected_nodes)
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::Finding(finding) => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO findings(segment_id, seq, span_id, key, record)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![segment_id, seq, finding.span_id, finding.key, json(finding)],
                        )
                        .map_err(sql)?;
                }
                Record::Demand(demand) => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO demands(segment_id, seq, span_id, demand_id,
                                 record)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                segment_id,
                                seq,
                                demand.span_id,
                                demand.demand_id,
                                json(demand)
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::Blob(object) => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO blobs(digest, bytes, stored_bytes, media_type,
                                 redaction)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                object.digest,
                                as_i64(object.bytes),
                                as_i64(object.stored_bytes),
                                object.media_type,
                                object.redaction
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::BlobRef(occurrence) => {
                    if occurrence.occurrence_id != trace::occurrence_id(segment_id, occurrence.seq)
                    {
                        return Err(format!(
                            "occurrence {} at seq {} is not the id its segment and seq derive",
                            occurrence.occurrence_id, occurrence.seq
                        ));
                    }
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO blob_refs(occurrence_id, segment_id, seq,
                                 digest, span_id, event_seq, source_digest, original_bytes,
                                 truncated, state)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                            params![
                                occurrence.occurrence_id,
                                segment_id,
                                seq,
                                occurrence.digest,
                                occurrence.span_id,
                                as_i64(occurrence.event_seq),
                                occurrence.source_digest,
                                as_i64(occurrence.original_bytes),
                                i64::from(occurrence.truncated),
                                occurrence.state.as_str()
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::Problem(problem) => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO problems(segment_id, seq, time_unix_ns, what,
                                 reason)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                segment_id,
                                seq,
                                ns(&problem.time_unix_ns),
                                problem.what.as_str(),
                                problem.reason
                            ],
                        )
                        .map_err(sql)?;
                }
                Record::Seal(seal) => {
                    transaction
                        .execute(
                            "UPDATE segments SET ended_unix_ns = ?2, exit = ?3, dropped = ?4,
                                 discarded_bytes = ?5
                             WHERE segment_id = ?1",
                            params![
                                segment_id,
                                ns(&seal.ended_unix_ns),
                                seal.exit.as_str(),
                                seal.dropped.map(as_i64),
                                as_i64(seal.discarded_bytes)
                            ],
                        )
                        .map_err(sql)?;
                }
            }
        }
        Ok(())
    }
}

/// Self time of every span that ended in this ingest and of each such span's parent: its
/// duration less the union of its ended children's intervals, clipped to it.
fn self_times(transaction: &Transaction<'_>, ended: &BTreeSet<String>) -> Result<(), String> {
    let mut affected = ended.clone();
    for span in ended {
        let parent: Option<String> = transaction
            .query_row(
                "SELECT parent_span_id FROM spans WHERE span_id = ?1",
                params![span],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?
            .flatten();
        if let Some(parent) = parent {
            affected.insert(parent);
        }
    }
    for span in affected {
        let interval: Option<(i64, Option<i64>)> = transaction
            .query_row(
                "SELECT start_unix_ns, end_unix_ns FROM spans WHERE span_id = ?1 AND ended = 1",
                params![span],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((start, Some(end))) = interval else {
            continue;
        };
        let mut children: Vec<(i64, i64)> = {
            let mut statement = transaction
                .prepare(
                    "SELECT start_unix_ns, end_unix_ns FROM spans
                     WHERE parent_span_id = ?1 AND ended = 1 AND end_unix_ns IS NOT NULL",
                )
                .map_err(sql)?;
            statement
                .query_map(params![span], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?
        };
        children.sort();
        let mut covered = 0_i64;
        let mut reach = start;
        for (child_start, child_end) in children {
            let from = child_start.max(reach).max(start);
            let to = child_end.min(end);
            if to > from {
                covered += to - from;
                reach = to;
            }
        }
        let own = (end - start).max(0) - covered;
        transaction
            .execute(
                "UPDATE spans SET self_ns = ?2 WHERE span_id = ?1",
                params![span, own.max(0)],
            )
            .map_err(sql)?;
    }
    Ok(())
}

/// What replacing the index did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replaced {
    pub ingested: Ingested,
    pub refused: Vec<(String, String)>,
}

/// Replace the index: take `index.lock` exclusively within two seconds, checkpoint, close,
/// unlink the database with its WAL and shared-memory files, recreate it and ingest every
/// segment. `Err` names why, the lock among the reasons; the old index is then untouched.
pub fn replace(layout: &Layout) -> Result<Replaced, String> {
    // New connections wait while this replacement holds the intent; the ones open finish.
    let _intent = layout::exclusive_within(&layout.index_intent(), layout::LOCK_WAIT)
        .map_err(|error| format!("{}: {error}", layout.index_intent().display()))?
        .ok_or("the telemetry index is in use by another command; try again")?;
    let lock = layout::exclusive_within(&layout.index_lock(), layout::LOCK_WAIT)
        .map_err(|error| format!("{}: {error}", layout.index_lock().display()))?
        .ok_or("the telemetry index is in use by another command; try again")?;
    if layout.index().exists() {
        if let Ok(connection) = Connection::open(layout.index()) {
            let _ = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            drop(connection);
        }
    }
    for path in layout.index_files() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    let connection = Connection::open(layout.index()).map_err(sql)?;
    configure(&connection)?;
    create(&connection)?;
    let mut index = Index {
        connection,
        layout: layout.clone(),
        mode: Mode::Shared,
        note: None,
        _lock: Some(lock),
    };
    let (ingested, refused) = index.ingest()?;
    Ok(Replaced { ingested, refused })
}

/// The exact sum of canonical decimal counters, or `None` on a value that is not one or a sum
/// beyond `u128` (ADR-0078: never a float, never an approximation).
pub fn sum_decimals<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut total: u128 = 0;
    for value in values {
        if !crate::redact::is_canonical_decimal(value) {
            return None;
        }
        total = total.checked_add(value.parse::<u128>().ok()?)?;
    }
    Some(total.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_sums_are_exact_or_refused() {
        assert_eq!(
            sum_decimals(["18446744073709551615", "1"]).as_deref(),
            Some("18446744073709551616")
        );
        assert_eq!(sum_decimals(["1", "x"]), None);
        assert_eq!(
            sum_decimals(["340282366920938463463374607431768211455", "1"]),
            None
        );
    }
}
