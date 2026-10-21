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
//!
//! **Confinement.** An index opens `v1/` once, without following a link, and keeps that
//! descriptor for the connection's lifetime, in direct mode too: `index.intent`, `index.lock`
//! and `gc.lock` are taken, the segments read, the blob files looked for, and the database with
//! its WAL and shared-memory files opened, created and removed, through it. SQLite opens files
//! only by path, so it is given a path that names the held directory itself
//! ([`Confined::anchored_path`]: the inode path on macOS, the descriptor's own entry on Linux),
//! never `layout.index()` or any other pathname; where there is none the open is refused.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::fcntl::FlockArg;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, Transaction, params};
use serde_json::Value;

use crate::layout::{self, Confined, EntryKind, FileIdentity, Held, Layout};
use crate::record::{Record, SealExit};
use crate::segment::reader::{self, Position};
use crate::trace;

/// How every blob file is stored: the `blobs` row, keyed by digest, holds only what is intrinsic
/// to the retained bytes (their digest, their sizes and this codec); media type and redaction
/// are the occurrence's.
pub const BLOB_CODEC: &str = "gzip";

/// The index's own shape; an index of another is refused and rebuilt by `reindex`.
pub const INDEX_SCHEMA: &str = "af/telemetry-index@2";

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
    identity TEXT,
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
    run_id TEXT NOT NULL,
    run_span_id TEXT NOT NULL,
    section TEXT NOT NULL,
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
    codec TEXT NOT NULL
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
    state TEXT NOT NULL,
    media_type TEXT NOT NULL,
    redaction TEXT NOT NULL
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
    /// The `v1/` the connection was opened through, held until after the connection closes.
    versioned: Option<Confined>,
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

/// The file names of the database with its WAL and shared-memory files, in `v1/`.
fn index_names(layout: &Layout) -> Vec<String> {
    layout
        .index_files()
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .collect()
}

thread_local! {
    static CONNECT_HOOK: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// Run `body` with `hook` called on this thread between the checks of the held `v1/` and the
/// open of the database, where a test renames and replaces the directory.
#[doc(hidden)]
pub fn with_connect_hook<R>(hook: impl FnMut() + 'static, body: impl FnOnce() -> R) -> R {
    CONNECT_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let result = body();
    CONNECT_HOOK.with(|slot| *slot.borrow_mut() = None);
    result
}

/// The telemetry root's `v1/`, created when absent, opened without following a link.
fn open_versioned(layout: &Layout) -> Result<Confined, String> {
    Confined::create_root(layout.root())
        .and_then(|root| root.create_child_dir(layout::VERSION_DIR))
        .map_err(|error| format!("{}: {error}", layout.versioned().display()))
}

/// How SQLite opens the anchored database path. On macOS the inode path has no link in it, so
/// `SQLITE_OPEN_NOFOLLOW` refuses any link SQLite meets. On Linux the path is the descriptor's
/// own `/proc/self/fd/<fd>` entry, which SQLite's unix VFS treats as a link (it `lstat`s every
/// component and expands each link itself, refusing all of them under `SQLITE_OPEN_NOFOLLOW`),
/// so the flag cannot be set there: SQLite expands the descriptor's entry inside its own open,
/// and [`connect`] then compares what it expanded with where the descriptor is.
fn anchored_flags() -> OpenFlags {
    if cfg!(target_os = "linux") {
        OpenFlags::default()
    } else {
        OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW
    }
}

/// Whether the database `connection` opened at `anchored` is in the held `v1/` `versioned`,
/// judged without looking any pathname up: the filename SQLite resolved is the anchored path
/// itself (macOS: the inode path has no link to expand), or, on Linux, what the kernel says the
/// held descriptor names now, so a `v1/` moved while SQLite expanded its entry is refused.
fn opened_in_held(connection: &Connection, anchored: &Path, versioned: &Confined) -> bool {
    connection.path().is_some_and(|opened| {
        expected_database(anchored, versioned).is_some_and(|expected| expected == Path::new(opened))
    })
}

/// The filename SQLite resolves for the database of the held `v1/` (see [`opened_in_held`]).
#[cfg(target_os = "linux")]
fn expected_database(_anchored: &Path, versioned: &Confined) -> Option<PathBuf> {
    versioned
        .descriptor_path()
        .ok()
        .map(|directory| directory.join(layout::INDEX))
}

/// The filename SQLite resolves for the database of the held `v1/` (see [`opened_in_held`]).
#[cfg(not(target_os = "linux"))]
fn expected_database(anchored: &Path, _versioned: &Confined) -> Option<PathBuf> {
    Some(anchored.to_path_buf())
}

/// Open the index file of the held `v1/` `versioned`, never through a link and never through
/// its pathname: a database, WAL or shared-memory file that is not a regular file in its place
/// is refused (`reindex` removes the link itself, never what it points to). SQLite is given a
/// path that names the held directory itself ([`Confined::anchored_path`]; where the file system
/// offers none the open is refused, a named reader error), so a `v1/` renamed and replaced by a
/// link or by another directory since it was opened leads SQLite nowhere outside it. The WAL
/// and shared-memory files are opened at once beside the database, and the database is checked
/// to be the held directory's own regular file.
fn connect(layout: &Layout, versioned: &Confined) -> Result<Connection, String> {
    let path = layout.index();
    for name in index_names(layout) {
        if versioned
            .kind(&name)
            .is_some_and(|kind| kind != EntryKind::File)
        {
            return Err(format!(
                "{}: not a regular file; the telemetry index is never opened through it",
                versioned.join(&name).display()
            ));
        }
    }
    let anchored: PathBuf = versioned
        .anchored_path()
        .map_err(|error| format!("{}: {error}", path.display()))?
        .join(layout::INDEX);
    CONNECT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook();
        }
    });
    let connection = Connection::open_with_flags(&anchored, anchored_flags())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sql)?;
    // The first read opens the WAL and shared-memory files of a WAL database, now, beside the
    // database just opened.
    connection
        .query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !opened_in_held(&connection, &anchored, versioned)
        || versioned.kind(layout::INDEX) != Some(EntryKind::File)
    {
        return Err(format!(
            "{}: the opened database is not the telemetry directory's own; it is never used",
            path.display()
        ));
    }
    Ok(connection)
}

/// Open the index file, creating it when absent; refuse one of another shape or schema version
/// (ADR-0145: refused, never migrated), naming the file, both schemas and the command that
/// replaces it. A refused file is left as it was found.
fn open_file(layout: &Layout, versioned: &Confined) -> Result<Connection, String> {
    let path = layout.index();
    let connection = connect(layout, versioned)?;
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
        Self::open_in(layout, open_versioned(layout)?)
    }

    /// [`Self::open`] through `versioned`, the telemetry root's `v1/` the caller opened: the
    /// locks are taken and the database opened through it, and it is held for the connection's
    /// lifetime.
    pub fn open_in(layout: &Layout, versioned: Confined) -> Result<Self, String> {
        let deadline = std::time::Instant::now() + layout::LOCK_WAIT;
        let shared = FlockArg::LockSharedNonblock;
        let intent = layout::lock_in(&versioned, layout::INDEX_INTENT, layout::LOCK_WAIT, shared)
            .map_err(|error| format!("{}: {error}", layout.index_intent().display()))?;
        let lock = match intent {
            Some(_) => layout::lock_in(
                &versioned,
                layout::INDEX_LOCK,
                deadline.saturating_duration_since(std::time::Instant::now()),
                shared,
            )
            .map_err(|error| format!("{}: {error}", layout.index_lock().display()))?,
            None => None,
        };
        drop(intent);
        let Some(lock) = lock else {
            return Self::direct_in(
                layout,
                Some(versioned),
                "the telemetry index is being replaced; reading the segments directly",
            );
        };
        Ok(Self {
            connection: open_file(layout, &versioned)?,
            layout: layout.clone(),
            mode: Mode::Shared,
            note: None,
            _lock: Some(lock),
            versioned: Some(versioned),
        })
    }

    /// An in-memory index over the segments, read without the index file: the telemetry root
    /// is opened once, and its `v1/`, when it is a real directory, is held and read through.
    pub fn direct(layout: &Layout, note: &str) -> Result<Self, String> {
        Self::direct_in(layout, layout.open_versioned(), note)
    }

    /// [`Self::direct`] through `versioned`, the `v1/` the caller holds (`None`: there is none,
    /// and nothing is read): every segment and blob file is reached through it, whatever has
    /// since become of the root's path.
    pub fn direct_in(
        layout: &Layout,
        versioned: Option<Confined>,
        note: &str,
    ) -> Result<Self, String> {
        let connection = Connection::open_in_memory().map_err(sql)?;
        create(&connection)?;
        Ok(Self {
            connection,
            layout: layout.clone(),
            mode: Mode::Direct,
            note: Some(note.to_string()),
            _lock: None,
            versioned,
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
    ///
    /// A sealed segment's file is recorded with its identity (device, inode, size and
    /// modification time) and compared with the file under its name on every ingest: a sealed
    /// file replaced or rewritten since is read and validated again, its earlier rows
    /// forgotten, and on refusal it is listed unreadable, exactly as a rebuild would answer. A
    /// segment ingested as sealed whose `.jsonl` the listing selects again (the compressed copy
    /// beside it is then stale staging) has its rows forgotten and its live prefix ingested from
    /// offset zero, as a rebuild would.
    pub fn ingest(&mut self) -> Result<(Ingested, Vec<(String, String)>), String> {
        // The segments of the `v1/` the index holds, never looked up by pathname again.
        let directory = match &self.versioned {
            Some(versioned) => versioned
                .child_dir("segments")
                .map_err(|error| format!("{}: {error}", self.layout.segments().display()))?,
            None => None,
        };
        let listing = match &directory {
            Some(directory) => reader::list_in(directory)?,
            None => reader::Listing::default(),
        };
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
        let known: Vec<(String, String, Option<String>)> = {
            let mut statement = transaction
                .prepare("SELECT segment_id, state, identity FROM segments")
                .map_err(sql)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?
        };
        for (segment_id, _, _) in &known {
            if !present.contains(segment_id) {
                forget(&transaction, segment_id)?;
            }
        }
        let states: BTreeMap<String, (String, Option<String>)> = known
            .into_iter()
            .map(|(segment_id, state, identity)| (segment_id, (state, identity)))
            .collect();
        for segment_id in &listing.sealed {
            let directory = directory
                .as_ref()
                .expect("a listing comes from a directory");
            let name = format!("{segment_id}{}", layout::SEALED);
            let path = directory.join(&name);
            // Read once whole; read again only when the file under its name is no longer the
            // one read.
            if let Some((state, Some(identity))) = states.get(segment_id)
                && state == "sealed"
                && directory
                    .identity(&name)
                    .map(|current| current.text())
                    .as_ref()
                    == Some(identity)
            {
                continue;
            }
            // Whatever rows the segment left (a live tail, an earlier file of this name) are
            // forgotten: the sealed file is ingested whole or not at all.
            if states.contains_key(segment_id) {
                forget(&transaction, segment_id)?;
            }
            let (records, identity) = match reader::read_sealed_in(directory, segment_id) {
                Ok(read) => read,
                Err(reason) => {
                    refused.push((path.display().to_string(), reason));
                    continue;
                }
            };
            let savepoint = SegmentIngest::new(segment_id);
            match savepoint.apply(&transaction, &records, &mut ended) {
                Ok(()) => {
                    set_sealed(&transaction, segment_id, &identity, &records)?;
                    ingested.segments += 1;
                    ingested.records += records.len() as u64;
                    ingested.bytes += identity.size;
                }
                Err(reason) => {
                    forget(&transaction, segment_id)?;
                    refused.push((path.display().to_string(), reason));
                }
            }
        }
        for segment_id in listing.live.iter().chain(&listing.unsealed) {
            let directory = directory
                .as_ref()
                .expect("a listing comes from a directory");
            let path = directory.join(&format!("{segment_id}{}", layout::LIVE));
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
            let bytes = match reader::read_live_in(directory, segment_id) {
                Ok(bytes) => bytes,
                // Sealed or recovered between the listing and the read: the next ingest has it.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    refused.push((path.display().to_string(), error.to_string()));
                    continue;
                }
            };
            // A segment ingested as sealed whose live file the listing now selects (a stale
            // compressed copy beside it): its rows are forgotten and the live prefix ingested
            // from offset zero, so no view shows a record of the stale copy.
            let stale_seal =
                matches!(states.get(segment_id), Some((state, _)) if state == "sealed");
            if stale_seal {
                forget(&transaction, segment_id)?;
            }
            let (offset, position) = if stale_seal || offset > bytes.len() {
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
    /// under `gc.lock` shared, the lock and every blob directory reached through the `v1/` this
    /// index holds, never through the saved pathname. `None` when the lock was not had within
    /// two seconds (a sweep holds it): the check is deferred, never answered while files may be
    /// collected.
    ///
    /// Presence is resolved per prefix directory, not per occurrence: the distinct digests are
    /// grouped by their `blobs/<aa>/` prefix, each prefix directory is opened once (without
    /// following a link) and each object judged in it by `fstatat`, so a corpus of many
    /// occurrences costs at most one directory open per prefix.
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
        // Without a held `v1/` there is no blob directory: every referenced file is missing.
        let Some(versioned) = self.versioned.as_ref().filter(|_| !all.is_empty()) else {
            return Ok(Some(all));
        };
        let Some(_shared) = layout::lock_in(
            versioned,
            layout::GC_LOCK,
            layout::LOCK_WAIT,
            FlockArg::LockSharedNonblock,
        )
        .ok()
        .flatten() else {
            return Ok(None);
        };
        let blobs = versioned.child_dir("blobs").ok().flatten();
        let digests: BTreeSet<&str> = all.iter().map(|(_, digest)| digest.as_str()).collect();
        let present: BTreeSet<String> = match &blobs {
            Some(blobs) => crate::blob::reader::present_in(blobs, digests)
                .into_iter()
                .map(str::to_string)
                .collect(),
            None => BTreeSet::new(),
        };
        Ok(Some(
            all.into_iter()
                .filter(|(_, digest)| !present.contains(digest.as_str()))
                .collect(),
        ))
    }

    /// The `v1/` this index holds, for the checks that follow an ingest (blob verification).
    pub fn versioned(&self) -> Option<&Confined> {
        self.versioned.as_ref()
    }

    /// The occurrence record of `occurrence_id`, as the index holds it.
    pub fn occurrence(
        &self,
        occurrence_id: &str,
    ) -> Result<Option<crate::record::BlobRef>, String> {
        self.connection
            .query_row(
                "SELECT occurrence_id, digest, span_id, event_seq, source_digest, original_bytes,
                        truncated, state, seq, media_type, redaction FROM blob_refs
                 WHERE occurrence_id = ?1",
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
                        media_type: row.get(9)?,
                        redaction: row.get(10)?,
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

/// Mark a segment ingested whole from its sealed file, recording the file's identity.
fn set_sealed(
    transaction: &Transaction<'_>,
    segment_id: &str,
    identity: &FileIdentity,
    records: &[Record],
) -> Result<(), String> {
    let last = records.last().map_or(0, Record::seq);
    transaction
        .execute(
            "UPDATE segments SET state = 'sealed', ingested_offset = ?2, last_seq = ?3,
                 sealed_last = 1, identity = ?4
             WHERE segment_id = ?1",
            params![
                segment_id,
                as_i64(identity.size),
                as_i64(last),
                identity.text()
            ],
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
                            "INSERT OR IGNORE INTO configs(segment_id, seq, execution_id, run_id,
                                 run_span_id, section, part, parts, complete, rows)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                            params![
                                segment_id,
                                seq,
                                part.execution_id,
                                part.run_id,
                                part.run_span_id,
                                part.section,
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
                            "INSERT OR IGNORE INTO blobs(digest, bytes, stored_bytes, codec)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![
                                object.digest,
                                as_i64(object.bytes),
                                as_i64(object.stored_bytes),
                                BLOB_CODEC
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
                                 truncated, state, media_type, redaction)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                                occurrence.state.as_str(),
                                occurrence.media_type,
                                occurrence.redaction
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

impl Index {
    /// Drop every row the segments `segment_ids` contributed, in one transaction, without
    /// reading any segment: the sweep's step after it evicts them. Returns how many of them the
    /// index held.
    pub fn forget_segments(&mut self, segment_ids: &[String]) -> Result<u64, String> {
        let transaction = self.connection.transaction().map_err(sql)?;
        let mut held = 0;
        for segment_id in segment_ids {
            let known: i64 = transaction
                .query_row(
                    "SELECT count(*) FROM segments WHERE segment_id = ?1",
                    params![segment_id],
                    |row| row.get(0),
                )
                .map_err(sql)?;
            held += u64::try_from(known).unwrap_or(0);
            forget(&transaction, segment_id)?;
        }
        transaction.commit().map_err(sql)?;
        Ok(held)
    }

    /// The database's free pages and page size (`PRAGMA freelist_count`, `page_size`).
    pub fn free_pages(&self) -> Result<(u64, u64), String> {
        let pages: i64 = self
            .connection
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .map_err(sql)?;
        let size: i64 = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(sql)?;
        Ok((
            u64::try_from(pages).unwrap_or(0),
            u64::try_from(size).unwrap_or(0),
        ))
    }

    /// `VACUUM` the database, then truncate its write-ahead log. `Err` carries whether SQLite
    /// failed for lack of space (`SQLITE_FULL`), when the caller replaces the index instead.
    pub fn vacuum(&self) -> Result<(), (bool, String)> {
        match self.connection.execute_batch("VACUUM") {
            Ok(()) => {
                let _ =
                    self.connection
                        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
                Ok(())
            }
            Err(error) => {
                let full = matches!(
                    &error,
                    rusqlite::Error::SqliteFailure(failure, _)
                        if failure.code == rusqlite::ErrorCode::DiskFull
                );
                Err((full, sql(error)))
            }
        }
    }
}

/// Delete the index under `index.lock` held exclusively (within two seconds), leaving none: the
/// next reader rebuilds it from the segments. `Ok(false)` when the lock was not had — the
/// deletion is deferred. Every file is removed relative to the held `v1/`.
pub fn delete_in(layout: &Layout, versioned: &Confined) -> Result<bool, String> {
    let exclusive = FlockArg::LockExclusiveNonblock;
    let Some(_intent) = layout::lock_in(
        versioned,
        layout::INDEX_INTENT,
        layout::LOCK_WAIT,
        exclusive,
    )
    .map_err(|error| format!("{}: {error}", layout.index_intent().display()))?
    else {
        return Ok(false);
    };
    let Some(_lock) = layout::lock_in(versioned, layout::INDEX_LOCK, layout::LOCK_WAIT, exclusive)
        .map_err(|error| format!("{}: {error}", layout.index_lock().display()))?
    else {
        return Ok(false);
    };
    for name in index_names(layout) {
        match versioned.unlink(&name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", versioned.join(&name).display())),
        }
    }
    Ok(true)
}

/// The file names of the database with its WAL and shared-memory files, for the budget.
pub fn file_names(layout: &Layout) -> Vec<String> {
    index_names(layout)
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
    replace_in(layout, open_versioned(layout)?)
}

/// [`replace`] through `versioned`, the telemetry root's `v1/` the caller opened: the locks are
/// taken, the files removed and the database recreated and ingested through it.
pub fn replace_in(layout: &Layout, versioned: Confined) -> Result<Replaced, String> {
    let exclusive = FlockArg::LockExclusiveNonblock;
    // New connections wait while this replacement holds the intent; the ones open finish.
    let _intent = layout::lock_in(
        &versioned,
        layout::INDEX_INTENT,
        layout::LOCK_WAIT,
        exclusive,
    )
    .map_err(|error| format!("{}: {error}", layout.index_intent().display()))?
    .ok_or("the telemetry index is in use by another command; try again")?;
    let lock = layout::lock_in(&versioned, layout::INDEX_LOCK, layout::LOCK_WAIT, exclusive)
        .map_err(|error| format!("{}: {error}", layout.index_lock().display()))?
        .ok_or("the telemetry index is in use by another command; try again")?;
    let names = index_names(layout);
    if versioned.kind(&names[0]) == Some(EntryKind::File)
        && let Ok(connection) = connect(layout, &versioned)
    {
        let _ = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        drop(connection);
    }
    // Removed relative to the opened `v1/`: a link in a file's place goes itself, never what
    // it points to.
    for name in &names {
        match versioned.unlink(name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", versioned.join(name).display())),
        }
    }
    let connection = connect(layout, &versioned)?;
    configure(&connection)?;
    create(&connection)?;
    let mut index = Index {
        connection,
        layout: layout.clone(),
        mode: Mode::Shared,
        note: None,
        _lock: Some(lock),
        versioned: Some(versioned),
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

    /// Design §6 and the first measurement of `docs/telemetry.md` § Performance: an unchanged
    /// `status` resolves the retained blobs' presence with one open per prefix directory, never
    /// one per stored occurrence: many occurrences of one blob, and a few other blobs, cost one
    /// open of `blobs/` and one of each prefix, and a missing object is still reported for
    /// every occurrence of it.
    #[test]
    fn missing_blobs_opens_each_prefix_once() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout::new(directory.path().join("telemetry"));
        let index = Index::open(&layout).unwrap();
        let blobs = index
            .versioned()
            .unwrap()
            .create_child_dir("blobs")
            .unwrap();
        let digest = |byte: &str| format!("sha256:{}", byte.repeat(32));
        let stored = [digest("aa"), digest("ab"), digest("cd")];
        for digest in &stored {
            let hex = digest.strip_prefix("sha256:").unwrap();
            let prefix = blobs.create_child_dir(&hex[..2]).unwrap();
            std::fs::write(prefix.join(&format!("{hex}.gz")), b"x").unwrap();
        }
        let gone = digest("ef");
        let insert = |occurrence: String, digest: &str| {
            index
                .connection()
                .execute(
                    "INSERT INTO blob_refs(occurrence_id, segment_id, seq, digest, span_id,
                     event_seq, source_digest, original_bytes, truncated, state, media_type,
                     redaction) VALUES (?1, 's', 1, ?2, 'x', 1, ?2, 1, 0, 'stored', 'text/plain',
                     'af-redact/1')",
                    params![occurrence, digest],
                )
                .unwrap();
        };
        for occurrence in 0..2_000 {
            insert(format!("s:{occurrence}"), &stored[0]);
        }
        insert("t:1".into(), &stored[1]);
        insert("t:2".into(), &stored[2]);
        insert("u:1".into(), &gone);
        insert("u:2".into(), &gone);
        layout::reset_child_dir_opens();
        let missing = index.missing_blobs().unwrap().unwrap();
        assert_eq!(
            missing,
            [("u:1".to_string(), gone.clone()), ("u:2".to_string(), gone)]
        );
        // `blobs/`, then the prefixes `aa`, `ab` and `cd` once each; `ef` is absent, never
        // opened.
        assert_eq!(layout::child_dir_opens(), 4);
    }

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
