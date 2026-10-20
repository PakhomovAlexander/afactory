//! The telemetry kind of the Storage Budget (ADR-0144, design §6 "Budget"): what a telemetry
//! directory holds, measured as allocated bytes, as the entries the sweep evicts, and the
//! reclamation every sweep does. Part of the reader module (ADR-0153 clause 1): linked by
//! `af storage` and `af telemetry gc`.
//!
//! Housekeeping reads only structural metadata: file sizes and times, lock state, the `.refs`
//! manifest and the header and seal records of a sealed segment, the `blob_ref` records in the
//! valid prefix of a `.jsonl` one, and the index's size. It never reads a span, an event, an
//! attribute or a blob of a sealed segment.
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
//!
//! **Last use.** A sealed segment's last use is its seal record's `ended_unix_ns`, read from the
//! file the inventory identifies by a structural read that decodes its `segment` header and its
//! `seal` record and nothing between them ([`reader::sealed_frame_in`]; the full record reader
//! is never called here); a file's modification time serves only an entry with no seal to read
//! — a corrupt segment, another version's directory, or a sealed file whose header or seal
//! cannot be read, which is reported.
//!
//! **Identity-bound eviction** (ADR-0144). The inventory records where it found each entry —
//! the telemetry root's device and inode and those of every file the entry owns, the `.refs`
//! manifest included ([`Found`]) — and its last-use time; [`evict`] checks all of them again
//! before it unlinks any, through the held directory descriptors: the root it opens must be the
//! one inventoried, every file under its name a regular file and the one inventoried, none
//! modified within [`PROTECTED`], no writer may hold its lock, and the entry must not have been
//! used within [`PROTECTED`]. When any file of the entry changed, the whole entry is left in
//! place and reported as changed. A sealed segment's removal is staged so that it is whole or
//! nothing: each file is checked again immediately before its own step and renamed under a
//! staging name in the held `segments/`, a change found at the manifest's step restores the
//! compressed copy, and only once both files are staged, and checked there again, are they
//! unlinked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::blob::collect;
use crate::index::{self, Index};
use crate::layout::{self, Confined, EntryKind, FileIdentity, Layout, SegmentFile};
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
    /// Seal time: the `ended_unix_ns` of the sealed file's seal record, read structurally; for a
    /// corrupt segment, another version's directory, or a sealed file whose seal cannot be read,
    /// its modification time.
    pub last_use_unix_ms: u64,
    /// The blob digests it references.
    pub refs: BTreeSet<String>,
    /// The device and inode of its file (another version's: its directory) as inventoried.
    pub identity: Option<(u64, u64)>,
    /// The device and inode of a sealed segment's `.refs` manifest as inventoried; `None` when
    /// it had none.
    pub manifest: Option<(u64, u64)>,
}

/// Where the inventory found an entry: the telemetry root's device and inode and those of every
/// file the entry owns. An applied eviction removes nothing of an entry any of which changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub root: (u64, u64),
    pub file: (u64, u64),
    /// A sealed segment's `.refs` manifest; `None` when the inventory found none.
    pub manifest: Option<(u64, u64)>,
}

/// One telemetry directory as the budget sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub root: PathBuf,
    /// The root's device and inode as the inventory opened it.
    pub root_identity: Option<(u64, u64)>,
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
    /// Where the inventory found it, for an evictable segment: what [`evict`] checks again.
    pub found: Option<Found>,
}

/// The `in_use` reason of the `shared` entry.
pub const SHARED: &str = "the index, locks and blobs several segments share, orphaned blobs and \
                          temporary files: reclaimed by every sweep, never evicted whole";

fn modified_ms(directory: &Confined, name: &str) -> u64 {
    directory.modified_unix_ms(name).unwrap_or(0)
}

fn identity_of(directory: &Confined, name: &str) -> Option<(u64, u64)> {
    directory.entry_identity(name)
}

/// The seal time of the sealed segment `id`, in unix milliseconds — its seal record's
/// `ended_unix_ns` — with the device and inode of the file it was read from. Read structurally
/// ([`reader::sealed_frame_in`]): the header and the seal only, never a span, an event, an
/// attribute or a blob.
fn sealed_at(segments: &Confined, id: &str) -> Result<(u64, (u64, u64)), String> {
    let frame = reader::sealed_frame_in(segments, id)?;
    let ms = u64::try_from(frame.ended_unix_ns / 1_000_000).unwrap_or(u64::MAX);
    Ok((ms, (frame.identity.dev, frame.identity.ino)))
}

impl Directory {
    /// The eviction target of segment `id` as this inventory found it; `None` for an entry in
    /// use or one found without an identity.
    pub fn target(&self, id: &str) -> Option<Target> {
        let segment = self.segments.get(id)?;
        if !matches!(
            segment.state,
            State::Sealed | State::Corrupt | State::Unreadable
        ) {
            return None;
        }
        Some(Target {
            id: segment.id.clone(),
            state: segment.state,
            found: Found {
                root: self.root_identity?,
                file: segment.identity?,
                manifest: segment.manifest,
            },
            last_use_unix_ms: segment.last_use_unix_ms,
        })
    }

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
            root_identity: root_dir.own_identity().ok(),
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
                        identity: identity_of(&root_dir, &name),
                        manifest: None,
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
                            identity: identity_of(&segments, &name),
                            manifest: None,
                        },
                    );
                }
            }
            for id in &listing.sealed {
                let name = format!("{id}{}", layout::SEALED);
                let refs_name = format!("{id}{}", layout::REFS);
                // The manifest's identity before its digests are read: an eviction removes it only
                // while it is still the file these digests came from.
                let manifest = segments
                    .identity(&refs_name)
                    .map(|found| (found.dev, found.ino));
                let refs = match reader::manifest_digests_in(&segments, id) {
                    Ok(refs) => refs,
                    Err(problem) => {
                        directory.problems.push(problem);
                        BTreeSet::new()
                    }
                };
                // The last use is the seal time of the file read, bound to its identity.
                let (last_use_unix_ms, identity) = match sealed_at(&segments, id) {
                    Ok((sealed_ms, read)) => (sealed_ms, Some(read)),
                    Err(problem) => {
                        directory
                            .problems
                            .push(format!("{problem}; its last use is its modification time"));
                        (modified_ms(&segments, &name), identity_of(&segments, &name))
                    }
                };
                directory.segments.insert(
                    id.clone(),
                    Segment {
                        id: id.clone(),
                        state: State::Sealed,
                        path: segments.join(&name),
                        own_bytes: segments.allocated(&name) + segments.allocated(&refs_name),
                        last_use_unix_ms,
                        refs,
                        identity,
                        manifest,
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
                        identity: identity_of(&segments, &name),
                        manifest: None,
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
                found: match (segment.state, self.root_identity, segment.identity) {
                    (
                        State::Sealed | State::Corrupt | State::Unreadable,
                        Some(root),
                        Some(file),
                    ) => Some(Found {
                        root,
                        file,
                        manifest: segment.manifest,
                    }),
                    _ => None,
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
            found: None,
        });
        entries
    }
}

/// One entry to evict, as the inventory found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The segment id, or the other version's directory name.
    pub id: String,
    pub state: State,
    pub found: Found,
    /// Its last use as inventoried.
    pub last_use_unix_ms: u64,
}

/// What [`evict`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evicted {
    /// Removed: the allocated bytes it freed.
    Removed(u64),
    /// Changed since the inventory, and left in place: why.
    Changed(String),
}

fn mtime_ms(identity: &FileIdentity) -> u64 {
    u64::try_from(identity.mtime_ns.max(0) / 1_000_000).unwrap_or(u64::MAX)
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Remove the entry `target` of `root` as the inventory found it — a sealed segment with its
/// manifest, a corrupt one, or another version's directory — relative to the opened
/// directories, never through a link. Before it unlinks any file of the entry it checks again,
/// through the held descriptors, that the root is the directory inventoried, that every file
/// the entry owns (the `.refs` manifest included) is a regular file under its name with the
/// device and inode inventoried and was not modified within [`PROTECTED`] of `now`, that no
/// writer holds its lock (nor a live file of the segment's), and that its last use — a sealed
/// segment's seal time — is not within [`PROTECTED`] of `now`; an entry any file of which fails
/// one of these is left in place whole and returned as [`Evicted::Changed`].
///
/// A sealed segment's two files are removed whole or not at all: the compressed copy is
/// checked and renamed under a staging name in the held `segments/` (the first step), then the
/// manifest is checked and renamed the same way (the second); a file found changed before its
/// own step, or under its staging name, restores what was staged under its own name (never over
/// a file that took the name since) and the entry is [`Evicted::Changed`]. Only once both are
/// staged, and found there still the files inventoried immediately before the first unlink, are
/// they unlinked: [`Evicted::Removed`] is returned only when no file of the entry remains.
pub fn evict(root: &Path, target: &Target, now: SystemTime) -> Result<Evicted, String> {
    evict_with(root, target, now, &|| {})
}

/// [`evict`], running `midway` between the first and the second step of a sealed segment's
/// staged removal: fixtures replace a file there, deterministically.
pub fn evict_with(
    root: &Path,
    target: &Target,
    now: SystemTime,
    midway: &dyn Fn(),
) -> Result<Evicted, String> {
    let failed = |error: std::io::Error| format!("{}: {error}", root.display());
    let changed = |why: &str| Ok(Evicted::Changed(why.to_string()));
    let Some(root_dir) = Confined::open_root(root).map_err(failed)? else {
        return changed("its telemetry directory is gone");
    };
    if root_dir.own_identity().ok() != Some(target.found.root) {
        return changed("its telemetry directory is another directory than the one inventoried");
    }
    let protected_until = |last_use: u64| last_use.saturating_add(PROTECTED.as_millis() as u64);
    let now_ms = unix_ms(now);
    let id = target.id.as_str();
    if target.state == State::Unreadable {
        if !reader::is_other_version(id) {
            return Err(format!("{id} is not another telemetry version's directory"));
        }
        if root_dir.kind(id) != Some(EntryKind::Directory)
            || root_dir.entry_identity(id) != Some(target.found.file)
        {
            return changed("replaced since the inventory");
        }
        if protected_until(root_dir.newest_modified_unix_ms(id)) > now_ms {
            return changed("used within the last hour");
        }
        let bytes = root_dir.allocated_tree(id);
        root_dir.remove_tree(id).map_err(failed)?;
        return Ok(Evicted::Removed(bytes));
    }
    let Some(segments) = root_dir
        .child_dir(layout::VERSION_DIR)
        .map_err(failed)?
        .and_then(|versioned| versioned.child_dir("segments").ok().flatten())
    else {
        return changed("its segments directory is gone");
    };
    let (name, manifest) = match target.state {
        State::Sealed => (
            format!("{id}{}", layout::SEALED),
            Some(format!("{id}{}", layout::REFS)),
        ),
        State::Corrupt => (format!("{id}{}", layout::CORRUPT), None),
        State::Live | State::Unsealed | State::Unreadable => {
            return Err(format!(
                "segment {id} is {}: never evicted",
                target.state.as_str()
            ));
        }
    };
    for name in std::iter::once(&name).chain(manifest.as_ref()) {
        if !matches!(layout::parse_segment_name(name), Some((ref parsed, kind)) if parsed == id && kind != SegmentFile::Live)
        {
            return Err(format!("{name} is not a segment file"));
        }
    }
    // The segment came back under a writer, or was left unsealed: in use.
    let live = format!("{id}{}", layout::LIVE);
    if segments.kind(&live).is_some() {
        return changed("its live file is back: a writer holds it or it is unsealed");
    }
    // The file under the name, opened without following a link and held locked while it is
    // judged and unlinked.
    let file = match segments.open_file(&name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return changed("gone since the inventory");
        }
        Err(_) => return changed("replaced since the inventory by something not a file"),
    };
    let opened = FileIdentity::of_file(&file).map_err(failed)?;
    if (opened.dev, opened.ino) != target.found.file {
        return changed("replaced since the inventory");
    }
    let Ok(held) = layout::try_lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) else {
        return changed("locked: in use");
    };
    if protected_until(target.last_use_unix_ms) > now_ms {
        return changed("used within the last hour");
    }
    if protected_until(mtime_ms(&opened)) > now_ms {
        return changed("modified within the last hour");
    }
    let is_found = |name: &str, found: (u64, u64)| {
        segments
            .identity(name)
            .is_some_and(|current| (current.dev, current.ino) == found)
    };
    let unlink = |name: &str| {
        segments
            .unlink(name)
            .map_err(|error| format!("{}: {error}", segments.join(name).display()))
    };
    // A corrupt segment is one file: still the one inventoried at the moment it is unlinked.
    let Some(manifest) = manifest else {
        if !is_found(&name, target.found.file) {
            return changed("replaced since the inventory");
        }
        let bytes = segments.allocated(&name);
        unlink(&name)?;
        drop(held);
        return Ok(Evicted::Removed(bytes));
    };
    // Put a staged file back under its own name, never over a file that took the name since.
    let restore = |staged: &str, original: &str, why: &str| -> Result<Evicted, String> {
        match segments.rename_no_replace(staged, original) {
            Ok(()) => changed(why),
            Err(error) => changed(&format!(
                "{why}; its staged file {} could not be restored ({error}) and is left for the \
                 sweep's temporaries",
                segments.join(staged).display()
            )),
        }
    };
    // The first step: both files checked immediately before it, then the compressed copy
    // staged and found there still the file inventoried.
    if let Some(why) = manifest_changed(&segments, &manifest, target.found.manifest, now_ms) {
        return changed(why);
    }
    if !is_found(&name, target.found.file) {
        return changed("replaced since the inventory");
    }
    let staged_segment = layout::temp_name(layout::WRITE_TEMP_LABEL);
    segments
        .rename(&name, &staged_segment)
        .map_err(|error| format!("{}: {error}", segments.join(&name).display()))?;
    if !is_found(&staged_segment, target.found.file) {
        return restore(&staged_segment, &name, "replaced since the inventory");
    }
    midway();
    // The second step: the manifest checked immediately before it, then staged and found there
    // still the one inventoried; a change at either restores the compressed copy.
    if let Some(why) = manifest_changed(&segments, &manifest, target.found.manifest, now_ms) {
        return restore(&staged_segment, &name, why);
    }
    let staged_manifest = match target.found.manifest {
        Some(found) => {
            let staged = layout::temp_name(layout::WRITE_TEMP_LABEL);
            if let Err(error) = segments.rename(&manifest, &staged) {
                let why = format!("its manifest could not be staged ({error})");
                return restore(&staged_segment, &name, &why);
            }
            if !is_found(&staged, found) {
                let why = "its manifest was replaced since the inventory";
                let _ = restore(&staged, &manifest, why);
                return restore(&staged_segment, &name, why);
            }
            Some((staged, found))
        }
        None => None,
    };
    // Every file of the entry, checked under its staging name immediately before the first
    // unlink; then the unlinks.
    let whole = is_found(&staged_segment, target.found.file)
        && staged_manifest
            .as_ref()
            .is_none_or(|(staged, found)| is_found(staged, *found));
    if !whole {
        let why = "changed while it was staged for removal";
        if let Some((staged, _)) = &staged_manifest {
            let _ = restore(staged, &manifest, why);
        }
        return restore(&staged_segment, &name, why);
    }
    let mut bytes = segments.allocated(&staged_segment);
    if let Some((staged, _)) = &staged_manifest {
        bytes += segments.allocated(staged);
    }
    unlink(&staged_segment)?;
    if let Some((staged, _)) = &staged_manifest {
        unlink(staged)?;
    }
    drop(held);
    Ok(Evicted::Removed(bytes))
}

/// Why the manifest `name` is no longer what the inventory found (`found`: its device and
/// inode, or `None` for no manifest): another file under the name, something not a regular
/// file, one modified within [`PROTECTED`] of `now_ms`, or one where there was none. `None` when
/// it is unchanged.
fn manifest_changed(
    segments: &Confined,
    name: &str,
    found: Option<(u64, u64)>,
    now_ms: u64,
) -> Option<&'static str> {
    let present = segments.kind(name);
    let Some(found) = found else {
        return present
            .is_some()
            .then_some("its manifest appeared since the inventory");
    };
    if present.is_none() {
        return Some("its manifest is gone since the inventory");
    }
    let Some(current) = segments.identity(name) else {
        return Some("its manifest was replaced since the inventory by something not a file");
    };
    if (current.dev, current.ino) != found {
        return Some("its manifest was replaced since the inventory");
    }
    if mtime_ms(&current).saturating_add(PROTECTED.as_millis() as u64) > now_ms {
        return Some("its manifest was modified within the last hour");
    }
    None
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
            identity: None,
            manifest: None,
        }
    }

    #[test]
    fn a_shared_blob_is_marginal_only_to_the_last_segment_that_references_it() {
        let mut directory = Directory {
            root: PathBuf::from("/t"),
            root_identity: None,
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
