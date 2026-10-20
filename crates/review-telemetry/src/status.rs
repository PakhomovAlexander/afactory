//! `af telemetry status` (`af/telemetry-status@1`) and `af telemetry reindex`
//! (`af/telemetry-reindex@1`), as documents (design §10).
//!
//! `status` never recovers or removes anything: an unlocked `.jsonl` segment is counted
//! `unsealed` (recoverable) until a reader that owns its recovery seals it. It ingests new
//! segment bytes into the index, then reads the index. A directory of another version (`v0/`,
//! `v2/`) is listed unreadable and never read. `reindex` recovers every unsealed segment it can
//! lock, replaces the index under `index.lock` and verifies every blob file's digest under
//! `gc.lock` shared; without that lock within two seconds it opens no blob file and reports the
//! verification `deferred`. An index file of another schema version is refused (the command
//! exits 1), never read around; `reindex` replaces it.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dirs::{self, Registered};
use crate::index::{self, Index, Mode};
use crate::layout::{self, Confined, EntryKind, Layout};
use crate::level::Level;
use crate::segment::reader::{self, Recovery};
use crate::trace::sha256_digest;

/// `af/telemetry-status@1`.
pub const STATUS_SCHEMA: &str = "af/telemetry-status@1";
/// `af/telemetry-reindex@1`.
pub const REINDEX_SCHEMA: &str = "af/telemetry-reindex@1";

/// One accounted entry under the directory, by its allocated bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// `segment` (with its manifest), `blobs`, `index` (with WAL and shared memory),
    /// `temporary`, `locks` (with the `v1/` and `segments/` directories themselves), and
    /// `unrecognized`: whatever else lies beneath the root, counted and never read or removed.
    pub kind: String,
    /// The segment id, or the entry's name.
    pub name: String,
    /// For a segment: `live`, `unsealed`, `sealed` or `corrupt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// For a sealed segment the index read: its seal's `exit` (`ok`, `error`, `signal`,
    /// `recovered`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<String>,
    /// For a sealed segment the index read: `complete`, or why the capture is incomplete
    /// (`recovered, drops unknown`, `recovered, 3 dropped`, `12 dropped`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    /// For `unrecognized`: how many paths beneath the root match no known kind, each counted
    /// once with everything beneath it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// For `unrecognized`: the first [`UNRECOGNIZED_NAMES`] of those paths, relative to the
    /// root, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    pub bytes: u64,
}

impl Entry {
    fn new(kind: &str, name: &str, bytes: u64) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
            state: None,
            exit: None,
            capture: None,
            count: None,
            names: None,
            bytes,
        }
    }
}

/// The most unrecognized paths `status` names; its count and bytes cover every one.
pub const UNRECOGNIZED_NAMES: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bytes {
    pub total: u64,
    /// `[telemetry] max_bytes`.
    pub max_bytes: u64,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segments {
    pub live: u64,
    pub unsealed: u64,
    pub sealed: u64,
    pub corrupt: u64,
}

/// The index as this command found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexState {
    /// `fresh` (every new byte ingested now), `direct` (the segments were read without the
    /// index file) or `absent` (no telemetry was ever written here).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub ingested_segments: u64,
    pub ingested_records: u64,
    pub ingested_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProblemCount {
    pub what: String,
    pub reason: String,
    /// Segments that recorded it; 0 for the problem this command finds itself (`registry`: the
    /// accounted directories could not all be read).
    pub segments: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unreadable {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredDir {
    pub path: String,
    pub exists: bool,
    /// The directory this status reads.
    pub current: bool,
    pub first_use_unix_ms: u64,
    pub last_use_unix_ms: u64,
    /// The overlap check, rechecked now: `none`, `overlapping`, or `unknown` when what the
    /// storage inventory accounts could not be read whole (the Store registry is unreadable or
    /// of another version), so no directory is presented as standing alone.
    pub overlap: OverlapCheck,
    /// Why the directory overlaps other accounted state: the inventory counts it once, by the
    /// containing entry, and never evicts it as telemetry. Under `unknown`, an overlap found
    /// among what could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlapping: Option<String>,
}

/// The outcome of a registered directory's overlap check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapCheck {
    /// It overlaps nothing the inventory accounts, all of which was read.
    None,
    Overlapping,
    /// The accounted directories could not all be read: the check is incomplete.
    Unknown,
}

impl OverlapCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Overlapping => "overlapping",
            Self::Unknown => "unknown",
        }
    }
}

/// The `what` of the problem `status` names when the accounted directories could not all be
/// read.
pub const REGISTRY_PROBLEM: &str = "registry";

/// `af/telemetry-status@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub schema: String,
    pub level: Level,
    /// `content` keeps blobs and is not bounded by the overhead gate.
    pub perturbing: bool,
    pub directory: String,
    pub exists: bool,
    pub bytes: Bytes,
    pub segments: Segments,
    pub runs: u64,
    /// Attempts whose release decision withheld their pending content.
    pub withheld_attempts: u64,
    /// Stored occurrences whose blob file is missing.
    pub missing_blobs: u64,
    /// `verified`: blob files were looked at under `gc.lock` shared; `deferred`: the lock was
    /// not had within two seconds, no blob file was looked at and `missing_blobs` is 0.
    pub blob_verification: BlobVerification,
    pub index: IndexState,
    pub problems: Vec<ProblemCount>,
    pub unreadable: Vec<Unreadable>,
    pub registered: Vec<RegisteredDir>,
}

/// Whether blob files were read under `gc.lock` shared, or the read was deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobVerification {
    Verified,
    Deferred,
}

impl BlobVerification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Deferred => "deferred",
        }
    }
}

/// One recovered segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recovered {
    pub segment_id: String,
    pub discarded_bytes: u64,
    pub unended_spans: u64,
}

/// `af/telemetry-reindex@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reindex {
    pub schema: String,
    pub directory: String,
    pub recovered: Vec<Recovered>,
    /// Segments recovery found corrupt and renamed now.
    pub corrupt: Vec<String>,
    pub segments: u64,
    pub records: u64,
    pub spans: u64,
    pub events: u64,
    pub blobs: u64,
    pub blob_refs: u64,
    pub missing_blobs: u64,
    /// Blob files whose content does not match their digest.
    pub corrupt_blobs: Vec<String>,
    /// `deferred` when `gc.lock` was not had shared within two seconds: no blob file was
    /// opened, and `missing_blobs` and `corrupt_blobs` say nothing.
    pub blob_verification: BlobVerification,
    pub unreadable: Vec<Unreadable>,
}

/// The allocated bytes of a file (what eviction would free), never its apparent length.
fn allocated(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).map_or(0, |metadata| metadata.blocks() * 512)
}

fn allocated_tree(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let mut total = metadata.blocks() * 512;
    if metadata.is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.filter_map(Result::ok) {
            total += allocated_tree(&entry.path());
        }
    }
    total
}

fn registered(
    registry: Option<&Path>,
    root: &Path,
    accounted: &[dirs::Accounted],
    accounted_unknown: bool,
) -> Result<Vec<RegisteredDir>, String> {
    let Some(registry) = registry else {
        return Ok(Vec::new());
    };
    let current = dirs::canonical(root);
    let entries = dirs::read(registry)?;
    let paths: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
    Ok(entries
        .into_iter()
        .map(
            |Registered {
                 path,
                 first_use_unix_ms,
                 last_use_unix_ms,
             }| {
                let overlapping =
                    dirs::overlap(&path, &paths, accounted).map(|overlap| overlap.to_string());
                RegisteredDir {
                    exists: path.is_dir(),
                    current: path == current,
                    overlap: match (&overlapping, accounted_unknown) {
                        (_, true) => OverlapCheck::Unknown,
                        (Some(_), false) => OverlapCheck::Overlapping,
                        (None, false) => OverlapCheck::None,
                    },
                    overlapping,
                    path: path.display().to_string(),
                    first_use_unix_ms,
                    last_use_unix_ms,
                }
            },
        )
        .collect())
}

/// What `status` needs besides the directory.
#[derive(Debug, Clone)]
pub struct StatusInput {
    pub root: PathBuf,
    pub level: Level,
    pub max_bytes: u64,
    /// The registry, when the machine has a state home.
    pub registry: Option<PathBuf>,
    /// Every directory the storage inventory accounts on its own, for the overlap recheck.
    pub accounted: Vec<dirs::Accounted>,
    /// Why `accounted` is known incomplete (the Store registry could not be read): every
    /// overlap check is then `unknown` and the reason is one of the document's problems.
    pub accounted_unknown: Option<String>,
}

/// `af telemetry status`. `Err` when the directory cannot be read; the caller exits 1.
pub fn status(input: &StatusInput) -> Result<(Status, Option<String>), String> {
    let layout = Layout::new(&input.root);
    let root = layout.root();
    let mut status = Status {
        schema: STATUS_SCHEMA.into(),
        level: input.level,
        perturbing: input.level == Level::Content,
        directory: root.display().to_string(),
        exists: false,
        bytes: Bytes {
            total: 0,
            max_bytes: input.max_bytes,
            entries: Vec::new(),
        },
        segments: Segments::default(),
        runs: 0,
        withheld_attempts: 0,
        missing_blobs: 0,
        blob_verification: BlobVerification::Verified,
        index: IndexState {
            state: "absent".into(),
            reason: None,
            ingested_segments: 0,
            ingested_records: 0,
            ingested_bytes: 0,
        },
        problems: Vec::new(),
        unreadable: Vec::new(),
        registered: registered(
            input.registry.as_deref(),
            root,
            &input.accounted,
            input.accounted_unknown.is_some(),
        )?,
    };
    // Named first, and kept on every path out of this function: an unreadable registry never
    // hides an overlap silently.
    let registry_problem = input.accounted_unknown.as_ref().map(|reason| ProblemCount {
        what: REGISTRY_PROBLEM.into(),
        reason: format!("overlap checks are unknown: {reason}"),
        segments: 0,
    });
    status.problems.extend(registry_problem.clone());
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => status.exists = true,
        Ok(_) => return Err(format!("{} is not a directory", root.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((status, None));
        }
        Err(error) => return Err(format!("{}: {error}", root.display())),
    }
    std::fs::read_dir(root).map_err(|error| format!("{}: {error}", root.display()))?;
    for other in reader::other_versions(root) {
        status.unreadable.push(Unreadable {
            reason: "written under another telemetry version; never read".into(),
            path: other.display().to_string(),
        });
        status.bytes.total += allocated_tree(&other);
    }
    if !layout.has_versioned() {
        if let Some(entry) = unrecognized(&layout) {
            status.bytes.total += entry.bytes;
            status.bytes.entries.push(entry);
        }
        return Ok((status, None));
    }
    let listing = reader::list(&layout)?;
    status.segments = Segments {
        live: listing.live.len() as u64,
        unsealed: listing.unsealed.len() as u64,
        sealed: listing.sealed.len() as u64,
        corrupt: listing.corrupt.len() as u64,
    };
    let mut entries = Vec::new();
    for (ids, state, file) in [
        (&listing.live, "live", layout::LIVE),
        (&listing.unsealed, "unsealed", layout::LIVE),
        (&listing.sealed, "sealed", layout::SEALED),
        (&listing.corrupt, "corrupt", layout::CORRUPT),
    ] {
        for id in ids {
            let mut bytes = allocated(&layout.segments().join(format!("{id}{file}")));
            if state == "sealed" {
                bytes += allocated(&layout.refs(id));
            }
            entries.push(Entry {
                state: Some(state.into()),
                ..Entry::new("segment", id, bytes)
            });
        }
    }
    // A manifest whose segment is gone is reported with the temporaries it is like.
    let orphans: u64 = listing
        .refs
        .iter()
        .filter(|id| !listing.sealed.contains(id))
        .map(|id| allocated(&layout.refs(id)))
        .sum::<u64>()
        + listing
            .temporary
            .iter()
            .map(|path| allocated(path))
            .sum::<u64>();
    entries.push(Entry::new("blobs", "blobs", blob_bytes(&layout)));
    entries.push(Entry::new(
        "index",
        "index.sqlite",
        layout
            .index_files()
            .iter()
            .map(|path| allocated(path))
            .sum(),
    ));
    entries.push(Entry::new("temporary", "temporary", orphans));
    entries.push(Entry::new(
        "locks",
        "locks",
        allocated(&layout.gc_lock())
            + allocated(&layout.index_lock())
            + allocated(&layout.index_intent())
            + allocated(&layout.segments())
            + allocated(&layout.versioned()),
    ));
    let mut index = Index::open(&layout)?;
    let (ingested, refused) = index.ingest()?;
    status.index = IndexState {
        state: match index.mode() {
            Mode::Shared => "fresh".into(),
            Mode::Direct => "direct".into(),
        },
        reason: index.note.clone(),
        ingested_segments: ingested.segments,
        ingested_records: ingested.records,
        ingested_bytes: ingested.bytes,
    };
    for (path, reason) in refused {
        status.unreadable.push(Unreadable { path, reason });
    }
    status.runs = index.count("SELECT count(*) FROM runs")?;
    status.withheld_attempts = index.count(
        "SELECT count(*) FROM spans WHERE name = 'af.harness'
         AND json_extract(end_attributes, '$.\"af.stream.withheld\"') = 1",
    )?;
    match index.missing_blobs()? {
        Some(missing) => status.missing_blobs = missing.len() as u64,
        None => status.blob_verification = BlobVerification::Deferred,
    }
    for (segment_id, exit, capture) in index.seals()? {
        if let Some(entry) = entries.iter_mut().find(|entry| {
            entry.kind == "segment"
                && entry.state.as_deref() == Some("sealed")
                && entry.name == segment_id
        }) {
            entry.exit = Some(exit);
            entry.capture = Some(capture);
        }
    }
    status.problems = registry_problem
        .into_iter()
        .chain(
            index
                .problems()?
                .into_iter()
                .map(|(what, reason, segments)| ProblemCount {
                    what,
                    reason,
                    segments,
                }),
        )
        .collect();
    let note = index.note.clone();
    drop(index);
    // The index's own size after the ingest.
    if let Some(entry) = entries.iter_mut().find(|entry| entry.kind == "index") {
        entry.bytes = layout
            .index_files()
            .iter()
            .map(|path| allocated(path))
            .sum();
    }
    entries.extend(unrecognized(&layout));
    status.bytes.total += entries.iter().map(|entry| entry.bytes).sum::<u64>();
    status.bytes.entries = entries;
    Ok((status, note))
}

/// Where a path beneath the root lies, for [`unrecognized`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Root,
    Versioned,
    Segments,
    /// `v1/blobs/`: only two-hex prefix directories are known here.
    Blobs,
    /// `v1/blobs/<aa>/`: only the objects whose digest starts with `aa` and the publisher's
    /// temporary files are known here.
    BlobPrefix([u8; 2]),
    /// Inside a path no known kind names as a directory: nothing beneath it is known.
    Foreign,
}

/// How [`status`] accounts one path beneath the root.
enum Known {
    /// Counted by an entry with everything beneath it.
    Tree,
    /// Its own blocks counted by an entry; what lies beneath it is judged in `Place`.
    Own(Place),
    Unrecognized,
}

fn known(place: Place, name: &str, kind: EntryKind, layout: &Layout) -> Known {
    let is_dir = kind == EntryKind::Directory;
    let is_file = kind == EntryKind::File;
    match place {
        Place::Root if name == layout::VERSION_DIR && is_dir => Known::Own(Place::Versioned),
        Place::Root if reader::is_other_version(name) => Known::Tree,
        Place::Versioned if name == "segments" && is_dir => Known::Own(Place::Segments),
        Place::Versioned if name == "blobs" && is_dir => Known::Own(Place::Blobs),
        Place::Blobs if is_dir && layout::is_blob_prefix(name) => {
            let prefix = name.as_bytes();
            Known::Own(Place::BlobPrefix([prefix[0], prefix[1]]))
        }
        Place::BlobPrefix(prefix) if is_file && is_blob_file(prefix, name) => {
            Known::Own(Place::Foreign)
        }
        Place::Versioned
            if is_file
                && [layout.gc_lock(), layout.index_lock(), layout.index_intent()]
                    .iter()
                    .chain(layout.index_files().iter())
                    .any(|path| path.file_name().is_some_and(|known| known == name)) =>
        {
            Known::Own(Place::Foreign)
        }
        Place::Segments
            if is_file
                && (layout::is_temp_name(name, layout::WRITE_TEMP_LABEL)
                    || layout::parse_segment_name(name).is_some()) =>
        {
            Known::Own(Place::Foreign)
        }
        _ => Known::Unrecognized,
    }
}

/// A file a publisher leaves in `blobs/<aa>/`: the object `<64 hex>.gz` whose digest starts
/// with `aa`, or a temporary file the collector removes. A link or a stray name is neither.
fn is_blob_file(prefix: [u8; 2], name: &str) -> bool {
    let prefix = std::str::from_utf8(&prefix).unwrap_or_default();
    layout::is_temp_name(name, layout::BLOB_TEMP_LABEL)
        || layout::blob_object_digest(prefix, name).is_some()
}

/// What `path` is, without following a link.
fn entry_kind(path: &Path) -> EntryKind {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => EntryKind::Directory,
        Ok(metadata) if metadata.is_file() => EntryKind::File,
        _ => EntryKind::Other,
    }
}

/// The `blobs` entry: the `blobs/` directory itself, its prefix directories, the objects and
/// the temporaries in them; any other path beneath `blobs/` is [`unrecognized`].
fn blob_bytes(layout: &Layout) -> u64 {
    let blobs = layout.blobs();
    if !std::fs::symlink_metadata(&blobs).is_ok_and(|metadata| metadata.is_dir()) {
        return 0;
    }
    let mut bytes = allocated(&blobs);
    let mut pending = vec![(blobs, Place::Blobs)];
    while let Some((directory, place)) = pending.pop() {
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        for child in children.filter_map(Result::ok) {
            let path = child.path();
            let kind = entry_kind(&path);
            let is_dir = kind == EntryKind::Directory;
            match known(place, &child.file_name().to_string_lossy(), kind, layout) {
                Known::Tree => bytes += allocated_tree(&path),
                Known::Own(inner) => {
                    bytes += allocated(&path);
                    if is_dir {
                        pending.push((path, inner));
                    }
                }
                Known::Unrecognized => {}
            }
        }
    }
    bytes
}

/// Every path beneath the root that matches no known kind, counted once with everything
/// beneath it by allocated bytes, never read and never removed: with it the entries add up to
/// the physical total of the tree (ADR-0153 clause 8). `None` when there is none.
fn unrecognized(layout: &Layout) -> Option<Entry> {
    let mut count = 0;
    let mut bytes = 0;
    let mut names = Vec::new();
    let mut pending = vec![(layout.root().to_path_buf(), Place::Root)];
    while let Some((directory, place)) = pending.pop() {
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        for child in children.filter_map(Result::ok) {
            let path = child.path();
            let kind = entry_kind(&path);
            let is_dir = kind == EntryKind::Directory;
            match known(place, &child.file_name().to_string_lossy(), kind, layout) {
                Known::Tree => {}
                Known::Own(inner) => {
                    if is_dir {
                        pending.push((path, inner));
                    }
                }
                Known::Unrecognized => {
                    count += 1;
                    bytes += allocated_tree(&path);
                    let relative = path.strip_prefix(layout.root()).unwrap_or(&path);
                    names.push(relative.display().to_string());
                }
            }
        }
    }
    if count == 0 {
        return None;
    }
    names.sort();
    names.truncate(UNRECOGNIZED_NAMES);
    Some(Entry {
        count: Some(count),
        names: Some(names),
        ..Entry::new("unrecognized", "unrecognized", bytes)
    })
}

/// Verify every blob file's digest, holding `gc.lock` shared for the whole scan; returns the
/// digests that fail. `None` when the lock was not had within two seconds: then no blob file is
/// opened and the verification is deferred.
fn verify_blobs(layout: &Layout) -> Option<Vec<String>> {
    let _shared = layout::shared_within(&layout.gc_lock(), layout::LOCK_WAIT)
        .ok()
        .flatten()?;
    let mut corrupt = Vec::new();
    // The collector's grammar and confinement: only real prefix directories and regular files
    // named by their digest are opened, relative to the opened directory, never through a link.
    let Ok(Some(mut blobs)) = Confined::versioned(layout, "blobs") else {
        return Some(corrupt);
    };
    for prefix_name in blobs.names().unwrap_or_default() {
        if !layout::is_blob_prefix(&prefix_name) {
            continue;
        }
        let Ok(Some(mut prefix)) = blobs.child_dir(&prefix_name) else {
            continue;
        };
        for name in prefix.names().unwrap_or_default() {
            let Some(digest) = layout::blob_object_digest(&prefix_name, &name) else {
                continue;
            };
            if prefix.kind(&name) != Some(EntryKind::File) {
                continue;
            }
            let limit = crate::level::CONTENT_MAX_BLOB_BYTES_BOUND;
            let ok = prefix.open_file(&name).is_ok_and(|handle| {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(
                    &mut std::io::Read::take(flate2::read::GzDecoder::new(handle), limit + 1),
                    &mut bytes,
                )
                .is_ok()
                    && bytes.len() as u64 <= limit
                    && sha256_digest(&bytes) == digest
            });
            if !ok {
                corrupt.push(digest);
            }
        }
    }
    corrupt.sort();
    Some(corrupt)
}

/// `af telemetry reindex`. `Err` when the directory cannot be read or the index cannot be
/// replaced (another command holds it); the caller exits 1.
pub fn reindex(root: &Path) -> Result<Reindex, String> {
    let layout = Layout::new(root);
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(format!("{} is not a directory", root.display())),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    }
    let mut document = Reindex {
        schema: REINDEX_SCHEMA.into(),
        directory: root.display().to_string(),
        recovered: Vec::new(),
        corrupt: Vec::new(),
        segments: 0,
        records: 0,
        spans: 0,
        events: 0,
        blobs: 0,
        blob_refs: 0,
        missing_blobs: 0,
        corrupt_blobs: Vec::new(),
        blob_verification: BlobVerification::Verified,
        unreadable: reader::other_versions(root)
            .into_iter()
            .map(|path| Unreadable {
                path: path.display().to_string(),
                reason: "written under another telemetry version; never read".into(),
            })
            .collect(),
    };
    if !layout.has_versioned() {
        return Ok(document);
    }
    for recovery in reader::recover_all(&layout)? {
        match recovery {
            Recovery::Recovered {
                segment_id,
                discarded_bytes,
                unended_spans,
            } => document.recovered.push(Recovered {
                segment_id,
                discarded_bytes,
                unended_spans,
            }),
            Recovery::Corrupt { segment_id } => document.corrupt.push(segment_id),
            Recovery::Live | Recovery::AlreadySealed | Recovery::Empty => {}
        }
    }
    let replaced = index::replace(&layout)?;
    for (path, reason) in replaced.refused {
        document.unreadable.push(Unreadable { path, reason });
    }
    let index = Index::open(&layout)?;
    document.segments = index.count("SELECT count(*) FROM segments")?;
    document.records = replaced.ingested.records;
    document.spans = index.count("SELECT count(*) FROM spans")?;
    document.events = index.count("SELECT count(*) FROM events")?;
    document.blobs = index.count("SELECT count(*) FROM blobs")?;
    document.blob_refs = index.count("SELECT count(*) FROM blob_refs")?;
    let verified = match verify_blobs(&layout) {
        Some(corrupt) => index.missing_blobs()?.map(|missing| (missing, corrupt)),
        None => None,
    };
    match verified {
        Some((missing, corrupt)) => {
            document.missing_blobs = missing.len() as u64;
            document.corrupt_blobs = corrupt;
        }
        None => document.blob_verification = BlobVerification::Deferred,
    }
    Ok(document)
}
