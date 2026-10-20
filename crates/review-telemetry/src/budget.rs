//! The telemetry kind of the Storage Budget (ADR-0144, design §6 "Budget"): what a telemetry
//! directory holds, measured as allocated bytes, as the entries the sweep evicts, and the
//! reclamation every sweep does. Part of the reader module (ADR-0153 clause 1): linked by
//! `af storage` and `af telemetry gc`.
//!
//! Housekeeping reads only structural metadata: file sizes and times, lock state, the `.refs`
//! manifest of a sealed segment and the `blob_ref` records in the valid prefix of a `.jsonl`
//! one, and the index's size. It never reads a span, an event, an attribute or a blob.
//!
//! **Physical union.** A directory's bytes are every allocated byte beneath it, counted once.
//! Each sealed or corrupt segment is an entry whose bytes are its **marginal** bytes — what its
//! eviction frees: the segment, its manifest, and the blobs no other retained segment references
//! — recomputed after every removal ([`Directory::remove`]), so a blob two segments share is
//! freed only with the second. A live or unsealed segment is an entry in use. What no segment
//! owns alone — the index with its WAL and shared memory, the locks, blobs several segments
//! share, orphaned blobs and temporary files — is one `shared` entry, never evicted whole and
//! reclaimed by every sweep ([`reclaim`]). A directory of another telemetry version (`v0/`,
//! `v2/`) is one `unreadable` entry, evicted whole like a segment and never read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::blob::collect;
use crate::index::{self, Index};
use crate::layout::{self, Confined, EntryKind, Layout, SegmentFile};
use crate::segment::reader;

/// Nothing used within this long is evicted, and temporaries and orphaned blobs younger than
/// this are kept (ADR-0144).
pub const PROTECTED: Duration = Duration::from_secs(3_600);
/// The index is vacuumed when its free pages exceed this and a quarter of the file.
pub const VACUUM_FREE_BYTES: u64 = 16 * 1024 * 1024;

/// What a segment is, for the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// A writer holds its lock.
    Live,
    /// A `.jsonl` nobody holds: recovered by the next applied sweep before anything is
    /// collected.
    Unsealed,
    Sealed,
    Corrupt,
    /// Another telemetry version's directory, never read.
    Unreadable,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Unsealed => "unsealed",
            Self::Sealed => "sealed",
            Self::Corrupt => "corrupt",
            Self::Unreadable => "unreadable",
        }
    }
}

/// One segment (or another version's directory) of a telemetry directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The segment id, or the other version's directory name.
    pub id: String,
    pub state: State,
    /// The file's (or directory's) path, for reports.
    pub path: PathBuf,
    /// Allocated bytes of the segment's own files (the segment and its manifest).
    pub own_bytes: u64,
    /// Seal time: when the sealed file was written; for a corrupt segment or another version's
    /// directory its modification time.
    pub last_use_unix_ms: u64,
    /// The blob digests it references.
    pub refs: BTreeSet<String>,
}

/// One telemetry directory as the budget sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub root: PathBuf,
    /// Every allocated byte beneath the root.
    pub total: u64,
    pub segments: BTreeMap<String, Segment>,
    /// Allocated bytes of every blob object, by digest.
    pub blobs: BTreeMap<String, u64>,
    /// What could not be read: reported, never guessed.
    pub problems: Vec<String>,
}

/// One entry of the telemetry kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `None` for the directory's `shared` entry.
    pub segment_id: Option<String>,
    /// `live`, `unsealed`, `sealed`, `corrupt`, `unreadable` or `shared`.
    pub state: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
    pub last_use_unix_ms: u64,
    /// Why it is never evicted now.
    pub in_use: Option<String>,
}

/// The `in_use` reason of the `shared` entry.
pub const SHARED: &str = "the index, locks and blobs several segments share, orphaned blobs and \
                          temporary files: reclaimed by every sweep, never evicted whole";

fn modified_ms(directory: &Confined, name: &str) -> u64 {
    directory.modified_unix_ms(name).unwrap_or(0)
}

impl Directory {
    /// Measure the telemetry directory `root`; `None` when it does not exist. With `recover`
    /// (an applied sweep, before it collects anything) every unsealed segment whose lock can be
    /// taken is recovered first, and the flushed prefix of every one that cannot be locked is
    /// read for its references, in that order. Reads only structural metadata.
    pub fn measure(root: &Path, recover: bool) -> Result<Option<Self>, String> {
        let failed = |error: std::io::Error| format!("{}: {error}", root.display());
        let Some(root_dir) = Confined::open_root(root).map_err(failed)? else {
            return Ok(None);
        };
        let mut directory = Self {
            root: root.to_path_buf(),
            total: root_dir.own_allocated(),
            segments: BTreeMap::new(),
            blobs: BTreeMap::new(),
            problems: Vec::new(),
        };
        for name in root_dir.names().map_err(failed)? {
            directory.total += root_dir.allocated_tree(&name);
            if reader::is_other_version(&name) && root_dir.kind(&name) == Some(EntryKind::Directory)
            {
                directory.segments.insert(
                    name.clone(),
                    Segment {
                        id: name.clone(),
                        state: State::Unreadable,
                        path: root_dir.join(&name),
                        own_bytes: root_dir.allocated_tree(&name),
                        last_use_unix_ms: root_dir.newest_modified_unix_ms(&name),
                        refs: BTreeSet::new(),
                    },
                );
            }
        }
        let Some(versioned) = root_dir.child_dir(layout::VERSION_DIR).map_err(|error| {
            format!("{}: {error}", root_dir.join(layout::VERSION_DIR).display())
        })?
        else {
            return Ok(Some(directory));
        };
        if let Some(segments) = versioned
            .child_dir("segments")
            .map_err(|error| format!("{}: {error}", versioned.join("segments").display()))?
        {
            if recover && let Err(problem) = reader::recover_all_in(&segments) {
                directory.problems.push(problem);
            }
            let listing = reader::list_in(&segments)?;
            for (ids, state) in [
                (&listing.live, State::Live),
                (&listing.unsealed, State::Unsealed),
            ] {
                for id in ids {
                    let name = format!("{id}{}", layout::LIVE);
                    let refs = match reader::read_live_in(&segments, id) {
                        Ok(bytes) => reader::referenced(&reader::scan(&bytes).records),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            directory
                                .problems
                                .push(format!("{}: {error}", segments.join(&name).display()));
                            BTreeSet::new()
                        }
                    };
                    directory.segments.insert(
                        id.clone(),
                        Segment {
                            id: id.clone(),
                            state,
                            path: segments.join(&name),
                            own_bytes: segments.allocated(&name),
                            last_use_unix_ms: modified_ms(&segments, &name),
                            refs,
                        },
                    );
                }
            }
            for id in &listing.sealed {
                let name = format!("{id}{}", layout::SEALED);
                let refs_name = format!("{id}{}", layout::REFS);
                let refs = match reader::manifest_digests_in(&segments, id) {
                    Ok(refs) => refs,
                    Err(problem) => {
                        directory.problems.push(problem);
                        BTreeSet::new()
                    }
                };
                directory.segments.insert(
                    id.clone(),
                    Segment {
                        id: id.clone(),
                        state: State::Sealed,
                        path: segments.join(&name),
                        own_bytes: segments.allocated(&name) + segments.allocated(&refs_name),
                        last_use_unix_ms: modified_ms(&segments, &name),
                        refs,
                    },
                );
            }
            for id in &listing.corrupt {
                let name = format!("{id}{}", layout::CORRUPT);
                directory.segments.insert(
                    id.clone(),
                    Segment {
                        id: id.clone(),
                        state: State::Corrupt,
                        path: segments.join(&name),
                        own_bytes: segments.allocated(&name),
                        last_use_unix_ms: modified_ms(&segments, &name),
                        refs: BTreeSet::new(),
                    },
                );
            }
        }
        if let Ok(Some(blobs)) = versioned.child_dir("blobs") {
            for prefix_name in blobs.names().unwrap_or_default() {
                if !layout::is_blob_prefix(&prefix_name) {
                    continue;
                }
                let Ok(Some(prefix)) = blobs.child_dir(&prefix_name) else {
                    continue;
                };
                for name in prefix.names().unwrap_or_default() {
                    if let Some(digest) = layout::blob_object_digest(&prefix_name, &name)
                        && prefix.kind(&name) == Some(EntryKind::File)
                    {
                        directory.blobs.insert(digest, prefix.allocated(&name));
                    }
                }
            }
        }
        Ok(Some(directory))
    }

    /// The bytes evicting `id` frees now: its own files and the blobs no other retained segment
    /// references.
    pub fn marginal(&self, id: &str) -> u64 {
        let Some(segment) = self.segments.get(id) else {
            return 0;
        };
        let shared: BTreeSet<&String> = self
            .segments
            .values()
            .filter(|other| other.id != id)
            .flat_map(|other| other.refs.iter())
            .collect();
        segment.own_bytes
            + segment
                .refs
                .iter()
                .filter(|digest| !shared.contains(digest))
                .filter_map(|digest| self.blobs.get(digest))
                .sum::<u64>()
    }

    /// Take `id` out of the model: the bytes it frees ([`Self::marginal`]), the blobs only it
    /// referenced going with it.
    pub fn remove(&mut self, id: &str) -> u64 {
        let freed = self.marginal(id);
        let Some(segment) = self.segments.remove(id) else {
            return 0;
        };
        let remaining: BTreeSet<String> = self
            .segments
            .values()
            .flat_map(|other| other.refs.iter().cloned())
            .collect();
        for digest in segment.refs {
            if !remaining.contains(&digest) {
                self.blobs.remove(&digest);
            }
        }
        self.total = self.total.saturating_sub(freed);
        freed
    }

    /// The entries of the telemetry kind: one per segment (marginal bytes) and the `shared`
    /// remainder, adding up to [`Self::total`].
    pub fn entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .segments
            .values()
            .map(|segment| Entry {
                segment_id: Some(segment.id.clone()),
                state: segment.state.as_str(),
                path: segment.path.clone(),
                bytes: self.marginal(&segment.id),
                last_use_unix_ms: segment.last_use_unix_ms,
                in_use: match segment.state {
                    State::Live => Some("a writer holds its lock".into()),
                    State::Unsealed => {
                        Some("unsealed: an applied sweep recovers it before it may go".into())
                    }
                    State::Sealed | State::Corrupt | State::Unreadable => None,
                },
            })
            .collect();
        let owned: u64 = entries.iter().map(|entry| entry.bytes).sum();
        entries.push(Entry {
            segment_id: None,
            state: "shared",
            path: self.root.clone(),
            bytes: self.total.saturating_sub(owned),
            last_use_unix_ms: self
                .segments
                .values()
                .map(|segment| segment.last_use_unix_ms)
                .max()
                .unwrap_or(0),
            in_use: Some(SHARED.into()),
        });
        entries
    }
}

/// Remove the segment `id` of `root` as `state` names it — a sealed segment with its manifest,
/// a corrupt one, or another version's directory — relative to the opened directories, never
/// through a link. A segment that is live now (its `.jsonl` came back under a writer's lock) is
/// refused. Returns the allocated bytes removed.
pub fn evict(root: &Path, id: &str, state: State) -> Result<u64, String> {
    let failed = |error: std::io::Error| format!("{}: {error}", root.display());
    let root_dir = Confined::open_root(root)
        .map_err(failed)?
        .ok_or_else(|| format!("{} is gone", root.display()))?;
    if state == State::Unreadable {
        if !reader::is_other_version(id) {
            return Err(format!("{id} is not another telemetry version's directory"));
        }
        let bytes = root_dir.allocated_tree(id);
        root_dir.remove_tree(id).map_err(failed)?;
        return Ok(bytes);
    }
    let segments = root_dir
        .child_dir(layout::VERSION_DIR)
        .map_err(failed)?
        .and_then(|versioned| versioned.child_dir("segments").ok().flatten())
        .ok_or_else(|| format!("{} has no segments", root.display()))?;
    let names: Vec<String> = match state {
        State::Sealed => vec![
            format!("{id}{}", layout::SEALED),
            format!("{id}{}", layout::REFS),
        ],
        State::Corrupt => vec![format!("{id}{}", layout::CORRUPT)],
        State::Live | State::Unsealed | State::Unreadable => {
            return Err(format!("segment {id} is {}: never evicted", state.as_str()));
        }
    };
    let mut bytes = 0;
    for name in names {
        if !matches!(layout::parse_segment_name(&name), Some((ref parsed, kind)) if parsed == id && kind != SegmentFile::Live)
        {
            return Err(format!("{name} is not a segment file"));
        }
        let allocated = segments.allocated(&name);
        match segments.remove_file(&name) {
            Ok(()) => bytes += allocated,
            Err(_) if segments.kind(&name).is_none() => {}
            Err(error) => return Err(format!("{}: {error}", segments.join(&name).display())),
        }
    }
    Ok(bytes)
}

/// What one sweep reclaimed in a directory beyond its evictions, as allocated bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reclaimed {
    pub temporary_bytes: u64,
    pub blob_bytes: u64,
    /// The index's allocated bytes before less after (vacuum or deletion).
    pub index_bytes: u64,
    /// `vacuumed`, `deleted` (no room to vacuum), `deferred` (`index.lock` not had), `kept`
    /// (free pages under the threshold) or `absent`.
    pub index: String,
    /// What was skipped or failed: never a run failure.
    pub notes: Vec<String>,
}

/// Every sweep's reclamation in the telemetry directory `root`: temporary files older than
/// [`PROTECTED`] without a lock; blobs no remaining segment references, under `gc.lock` held
/// exclusively (skipped when it is not had within two seconds); then the index rows of the
/// segments `evicted` are dropped and, when its free pages exceed [`VACUUM_FREE_BYTES`] and a
/// quarter of the file, it is vacuumed — and deleted under `index.lock` when the vacuum fails
/// for lack of space, deferred without the lock. Never reads a span.
pub fn reclaim(root: &Path, evicted: &[String], now: SystemTime) -> Result<Reclaimed, String> {
    let layout = Layout::new(root);
    let mut reclaimed = Reclaimed {
        index: "absent".into(),
        ..Reclaimed::default()
    };
    let failed = |error: std::io::Error| format!("{}: {error}", root.display());
    let Some(versioned) = Confined::open_root(root)
        .map_err(failed)?
        .and_then(|root_dir| root_dir.child_dir(layout::VERSION_DIR).ok().flatten())
    else {
        return Ok(reclaimed);
    };
    let collected = collect::collect_in(&versioned, PROTECTED, now)?;
    reclaimed.temporary_bytes = collected.temporary_bytes;
    reclaimed.blob_bytes = collected.blob_bytes;
    if let Some(skipped) = collected.skipped {
        reclaimed
            .notes
            .push(format!("blob collection skipped: {skipped}"));
    }
    let names = index::file_names(&layout);
    if versioned.kind(&names[0]) != Some(EntryKind::File) {
        return Ok(reclaimed);
    }
    let allocated =
        |versioned: &Confined| -> u64 { names.iter().map(|name| versioned.allocated(name)).sum() };
    let before = allocated(&versioned);
    let index = Index::open_in(
        &layout,
        versioned
            .try_clone()
            .map_err(|error| format!("{}: {error}", layout.versioned().display()))?,
    );
    let index = match index {
        Ok(index) if index.mode() == index::Mode::Shared => index,
        Ok(_) => {
            reclaimed.index = "deferred".into();
            reclaimed
                .notes
                .push("the index is being replaced; its reclamation is deferred".into());
            return Ok(reclaimed);
        }
        Err(problem) => {
            reclaimed.index = "deferred".into();
            reclaimed.notes.push(problem);
            return Ok(reclaimed);
        }
    };
    let mut index = index;
    if !evicted.is_empty() {
        index.forget_segments(evicted)?;
    }
    let (free_pages, page_size) = index.free_pages()?;
    let file_bytes = versioned
        .identity(&names[0])
        .map_or(0, |identity| identity.size);
    let free = free_pages.saturating_mul(page_size);
    if free <= VACUUM_FREE_BYTES || free.saturating_mul(4) <= file_bytes {
        reclaimed.index = "kept".into();
        drop(index);
        reclaimed.index_bytes = before.saturating_sub(allocated(&versioned));
        return Ok(reclaimed);
    }
    let vacuumed = index.vacuum();
    // Closed before it is measured: an open connection keeps its shared-memory file.
    drop(index);
    match vacuumed {
        Ok(()) => reclaimed.index = "vacuumed".into(),
        Err((true, why)) => {
            reclaimed.index = if index::delete_in(&layout, &versioned)? {
                "deleted".into()
            } else {
                "deferred".into()
            };
            reclaimed.notes.push(format!(
                "the index could not be vacuumed for lack of space ({why})"
            ));
        }
        Err((false, why)) => {
            reclaimed.index = "deferred".into();
            reclaimed.notes.push(why);
        }
    }
    reclaimed.index_bytes = before.saturating_sub(allocated(&versioned));
    Ok(reclaimed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(id: &str, own: u64, refs: &[&str]) -> Segment {
        Segment {
            id: id.into(),
            state: State::Sealed,
            path: PathBuf::from(id),
            own_bytes: own,
            last_use_unix_ms: 0,
            refs: refs.iter().map(|digest| digest.to_string()).collect(),
        }
    }

    #[test]
    fn a_shared_blob_is_marginal_only_to_the_last_segment_that_references_it() {
        let mut directory = Directory {
            root: PathBuf::from("/t"),
            total: 10 + 20 + 1000 + 7 + 3,
            segments: [
                ("a".to_string(), segment("a", 10, &["blob"])),
                ("b".to_string(), segment("b", 20, &["blob", "own"])),
            ]
            .into_iter()
            .collect(),
            blobs: [("blob".to_string(), 1000), ("own".to_string(), 7)]
                .into_iter()
                .collect(),
            problems: Vec::new(),
        };
        assert_eq!(directory.marginal("a"), 10);
        assert_eq!(directory.marginal("b"), 27);
        let entries = directory.entries();
        assert_eq!(
            entries.iter().map(|entry| entry.bytes).sum::<u64>(),
            directory.total
        );
        assert_eq!(entries.last().unwrap().bytes, 1003);
        assert_eq!(directory.remove("a"), 10);
        assert_eq!(directory.marginal("b"), 1027);
        assert_eq!(directory.remove("b"), 1027);
        assert_eq!(directory.total, 3);
    }
}
