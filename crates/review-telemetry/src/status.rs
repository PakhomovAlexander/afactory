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
use crate::layout::{self, Layout};
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
    /// `temporary`, `locks`.
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
    pub bytes: u64,
}

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
    /// Segments that recorded it.
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
    /// Why the directory overlaps other accounted state, rechecked now: the inventory counts it
    /// once, by the containing entry, and never evicts it as telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlapping: Option<String>,
}

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
             }| RegisteredDir {
                exists: path.is_dir(),
                current: path == current,
                overlapping: dirs::overlap(&path, &paths, accounted)
                    .map(|overlap| overlap.to_string()),
                path: path.display().to_string(),
                first_use_unix_ms,
                last_use_unix_ms,
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
        registered: registered(input.registry.as_deref(), root, &input.accounted)?,
    };
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
    if !layout.versioned().is_dir() {
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
                kind: "segment".into(),
                name: id.clone(),
                state: Some(state.into()),
                exit: None,
                capture: None,
                bytes,
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
    entries.push(Entry {
        kind: "blobs".into(),
        name: "blobs".into(),
        state: None,
        exit: None,
        capture: None,
        bytes: allocated_tree(&layout.blobs()),
    });
    entries.push(Entry {
        kind: "index".into(),
        name: "index.sqlite".into(),
        state: None,
        exit: None,
        capture: None,
        bytes: layout
            .index_files()
            .iter()
            .map(|path| allocated(path))
            .sum(),
    });
    entries.push(Entry {
        kind: "temporary".into(),
        name: "temporary".into(),
        state: None,
        exit: None,
        capture: None,
        bytes: orphans,
    });
    entries.push(Entry {
        kind: "locks".into(),
        name: "locks".into(),
        state: None,
        exit: None,
        capture: None,
        bytes: allocated(&layout.gc_lock())
            + allocated(&layout.index_lock())
            + allocated(&layout.index_intent())
            + allocated(&layout.segments())
            + allocated(&layout.versioned()),
    });
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
    status.problems = index
        .problems()?
        .into_iter()
        .map(|(what, reason, segments)| ProblemCount {
            what,
            reason,
            segments,
        })
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
    status.bytes.total += entries.iter().map(|entry| entry.bytes).sum::<u64>();
    status.bytes.entries = entries;
    Ok((status, note))
}

/// Verify every blob file's digest, holding `gc.lock` shared for the whole scan; returns the
/// digests that fail. `None` when the lock was not had within two seconds: then no blob file is
/// opened and the verification is deferred.
fn verify_blobs(layout: &Layout) -> Option<Vec<String>> {
    let _shared = layout::shared_within(&layout.gc_lock(), layout::LOCK_WAIT)
        .ok()
        .flatten()?;
    let mut corrupt = Vec::new();
    let Ok(prefixes) = std::fs::read_dir(layout.blobs()) else {
        return Some(corrupt);
    };
    for prefix in prefixes.filter_map(Result::ok) {
        let Ok(files) = std::fs::read_dir(prefix.path()) else {
            continue;
        };
        for file in files.filter_map(Result::ok) {
            let name = file.file_name().to_string_lossy().to_string();
            let Some(hex) = name.strip_suffix(".gz") else {
                continue;
            };
            if name.starts_with(layout::TEMP_PREFIX) {
                continue;
            }
            let digest = format!("sha256:{hex}");
            let ok = std::fs::File::open(file.path()).is_ok_and(|handle| {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(handle), &mut bytes)
                    .is_ok()
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
    if !layout.versioned().is_dir() {
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
