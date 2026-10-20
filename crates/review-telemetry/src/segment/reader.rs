//! Reading segments: listing, the valid prefix of a live or unsealed segment, sealed segments,
//! and recovery (design §6). Telemetry fails closed for the reader: a record of another schema
//! version, a sealed segment with an invalid line, or a directory of another version is refused,
//! never migrated (ADR-0145).
//!
//! **Recovery** happens only under the segment's lock. A reader that can take the exclusive lock
//! of a `.jsonl` segment owns its recovery: it keeps the longest prefix of complete, valid lines,
//! appends a `span_end` with `end_unix_ns = null` for every span the prefix never ended and a
//! `seal` with `exit = "recovered"` and the discarded suffix as `discarded_bytes`, writes the
//! `.refs` manifest from the kept records, then compresses and renames. A prefix that already
//! ends with the writer's seal (the writer died before compressing) has that seal superseded by
//! the recovered one, which keeps the writer's counts; a prefix without it gets a recovered seal
//! whose `dropped` and `dropped_chunks` are `null`, unknown. An invalid line before a valid one
//! is interior corruption: the segment is renamed `.corrupt`, never read again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use nix::fcntl::FlockArg;

use crate::layout::{self, Confined, EntryKind, Layout, SegmentFile};
use crate::level::SEGMENT_MAX_BYTES_BOUND;
use crate::record::{Attributes, BlobState, Record, Seal, SealExit, SpanEnd, SpanStatus, schema};

/// The valid prefix of a segment's bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prefix {
    pub records: Vec<Record>,
    /// Bytes of the complete, valid lines kept, newlines included.
    pub kept_bytes: usize,
    /// Bytes after them.
    pub discarded_bytes: u64,
    /// An invalid line precedes a valid one.
    pub interior_corruption: bool,
    /// Why the first line after the prefix was refused, when it is complete.
    pub invalid: Option<String>,
}

impl Prefix {
    /// The segment id its header names.
    pub fn segment_id(&self) -> Option<&str> {
        match self.records.first() {
            Some(Record::Segment(header)) => Some(&header.segment_id),
            _ => None,
        }
    }

    pub fn sealed(&self) -> bool {
        matches!(self.records.last(), Some(Record::Seal(_)))
    }
}

/// Where a scan stands: the last valid record's `seq`, and whether it was the seal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub seq: u64,
    pub sealed: bool,
}

/// Whether `record` may follow `previous` in a segment: the header first and only first, `seq`
/// strictly increasing, nothing after the seal.
fn follows(previous: Option<Position>, record: &Record) -> bool {
    match previous {
        None => matches!(record, Record::Segment(header) if header.seq == 1),
        Some(Position { sealed: true, .. }) => false,
        Some(previous) => !matches!(record, Record::Segment(_)) && record.seq() > previous.seq,
    }
}

/// Split `bytes` into its longest prefix of complete, valid lines. `start` is a byte offset at
/// a line boundary already known valid (`previous` where it stood), for a live tail.
pub fn scan_from(bytes: &[u8], start: usize, previous: Option<Position>) -> Prefix {
    let mut prefix = Prefix::default();
    let start = start.min(bytes.len());
    let mut offset = start;
    let mut last = previous;
    while let Some(newline) = bytes[offset..].iter().position(|byte| *byte == b'\n') {
        let line = &bytes[offset..offset + newline];
        match Record::parse_line(line) {
            Ok(record) if follows(last, &record) => {
                last = Some(Position {
                    seq: record.seq(),
                    sealed: matches!(record, Record::Seal(_)),
                });
                prefix.records.push(record);
                offset += newline + 1;
            }
            Ok(record) => {
                prefix.invalid = Some(format!(
                    "a `{}` record at seq {} out of order",
                    record.kind(),
                    record.seq()
                ));
                break;
            }
            Err(reason) => {
                prefix.invalid = Some(reason);
                break;
            }
        }
    }
    prefix.kept_bytes = offset;
    prefix.discarded_bytes = (bytes.len() - offset) as u64;
    if offset < bytes.len() {
        // Anything valid after the first invalid line is interior corruption.
        let rest = &bytes[offset..];
        let mut lines = rest.split(|byte| *byte == b'\n');
        lines.next();
        let complete = rest.iter().filter(|byte| **byte == b'\n').count();
        prefix.interior_corruption = lines
            .take(complete.saturating_sub(1))
            .any(|line| Record::parse_line(line).is_ok());
    }
    prefix
}

/// [`scan_from`] from the start of a segment.
pub fn scan(bytes: &[u8]) -> Prefix {
    scan_from(bytes, 0, None)
}

/// Read `reader` whole when it yields at most `limit` bytes; past it nothing more is read and
/// the read is refused.
fn read_at_most(reader: impl std::io::Read, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("longer than the {limit}-byte segment bound; never read"),
        ));
    }
    Ok(bytes)
}

/// The bytes of a `.jsonl` segment, opened without following a link. Its length is judged
/// before any byte is read: a file longer than any segment this release writes
/// ([`SEGMENT_MAX_BYTES_BOUND`]) is refused, never loaded.
pub fn read_live_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = layout::open_nofollow(path)?;
    if file.metadata()?.len() > SEGMENT_MAX_BYTES_BOUND {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("longer than the {SEGMENT_MAX_BYTES_BOUND}-byte segment bound; never read"),
        ));
    }
    read_at_most(file, SEGMENT_MAX_BYTES_BOUND)
}

/// The valid prefix of a live or unsealed segment, read without its lock.
pub fn read_live(path: &Path) -> Result<Prefix, String> {
    let bytes = read_live_bytes(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(scan(&bytes))
}

/// Every record of a sealed segment. Any invalid line, or a missing seal, refuses it.
pub fn read_sealed(path: &Path) -> Result<Vec<Record>, String> {
    let file =
        layout::open_nofollow(path).map_err(|error| format!("{}: {error}", path.display()))?;
    // Decompressed only up to the longest segment this release writes: a larger stream is
    // refused before it is held.
    let bytes = read_at_most(GzDecoder::new(file), SEGMENT_MAX_BYTES_BOUND)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let prefix = scan(&bytes);
    if prefix.discarded_bytes > 0 || !prefix.sealed() {
        let reason = prefix
            .invalid
            .as_deref()
            .map(|reason| format!(": line {}: {reason}", prefix.records.len() + 1))
            .unwrap_or_default();
        return Err(format!(
            "{}: not a sealed segment of this release ({} bytes after the last valid line{reason})",
            path.display(),
            prefix.discarded_bytes
        ));
    }
    Ok(prefix.records)
}

/// The digests of every stored occurrence in `records`, sorted.
pub fn referenced(records: &[Record]) -> BTreeSet<String> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) if occurrence.state == BlobState::Stored => {
                occurrence.digest.clone()
            }
            _ => None,
        })
        .collect()
}

/// The segment files of a directory, by kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// `.jsonl` segments whose writer holds the lock.
    pub live: Vec<String>,
    /// `.jsonl` segments nobody holds: a crashed or abandoned writer's, recoverable.
    pub unsealed: Vec<String>,
    pub sealed: Vec<String>,
    pub corrupt: Vec<String>,
    /// `.refs` manifests, by segment id.
    pub refs: Vec<String>,
    /// Temporary files under `segments/`.
    pub temporary: Vec<PathBuf>,
}

/// List `segments/` without reading any segment. Only regular files are listed, each name
/// matched exactly: a link, a directory or a stray name is never a segment or a temporary (it is
/// what `status` reports as unrecognized).
pub fn list(layout: &Layout) -> Result<Listing, String> {
    let directory = layout.segments();
    let failed = |error: std::io::Error| format!("{}: {error}", directory.display());
    let mut listing = Listing::default();
    let Some(mut segments) = Confined::versioned(layout, "segments").map_err(failed)? else {
        return Ok(listing);
    };
    for name in segments.names().map_err(failed)? {
        if segments.kind(&name) != Some(EntryKind::File) {
            continue;
        }
        if layout::is_temp_name(&name, layout::WRITE_TEMP_LABEL) {
            listing.temporary.push(segments.join(&name));
            continue;
        }
        match layout::parse_segment_name(&name) {
            Some((id, SegmentFile::Live)) => {
                let locked = segments.open_file(&name).is_ok_and(|file| {
                    layout::try_lock(file, FlockArg::LockSharedNonblock).is_err()
                });
                if locked {
                    listing.live.push(id);
                } else {
                    listing.unsealed.push(id);
                }
            }
            Some((id, SegmentFile::Sealed)) => listing.sealed.push(id),
            Some((id, SegmentFile::Corrupt)) => listing.corrupt.push(id),
            Some((id, SegmentFile::Refs)) => listing.refs.push(id),
            None => {}
        }
    }
    for list in [
        &mut listing.live,
        &mut listing.unsealed,
        &mut listing.sealed,
        &mut listing.corrupt,
        &mut listing.refs,
    ] {
        list.sort();
    }
    listing.temporary.sort();
    Ok(listing)
}

/// What recovery did with one `.jsonl` segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Its writer holds the lock: it is live, and nothing was touched.
    Live,
    /// Sealed with `exit = "recovered"`.
    Recovered {
        segment_id: String,
        discarded_bytes: u64,
        unended_spans: u64,
    },
    /// Interior corruption, or no valid header: renamed `.corrupt`.
    Corrupt { segment_id: String },
    /// Its writer sealed it after all (the `.jsonl.gz` exists); the leftover was removed.
    AlreadySealed,
    /// An empty file a writer created and never wrote: removed.
    Empty,
}

fn record_time(record: &Record) -> Option<&str> {
    match record {
        Record::Segment(header) => Some(&header.started_unix_ns),
        Record::SpanStart(start) => Some(&start.start_unix_ns),
        Record::SpanEnd(end) => end.end_unix_ns.as_deref(),
        Record::Event(event) => Some(&event.time_unix_ns),
        Record::Problem(problem) => Some(&problem.time_unix_ns),
        Record::Seal(seal) => Some(&seal.ended_unix_ns),
        _ => None,
    }
}

/// Recover the `.jsonl` segment `segment_id` if its lock can be taken now.
pub fn recover(layout: &Layout, segment_id: &str) -> Result<Recovery, String> {
    let live = layout.live_segment(segment_id);
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(&live)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Recovery::AlreadySealed);
        }
        Err(error) => return Err(format!("{}: {error}", live.display())),
    };
    let Ok(mut held) = layout::try_lock(file, FlockArg::LockExclusiveNonblock) else {
        return Ok(Recovery::Live);
    };
    // The lock is on what we opened; the path must still name it (a writer that sealed in the
    // meantime unlinked it).
    let opened = held
        .metadata()
        .map_err(|error| format!("{}: {error}", live.display()))?;
    match std::fs::metadata(&live) {
        Ok(current) if current.ino() == opened.ino() && current.dev() == opened.dev() => {}
        _ => return Ok(Recovery::AlreadySealed),
    }
    if layout.sealed_segment(segment_id).exists() {
        std::fs::remove_file(&live).map_err(|error| format!("{}: {error}", live.display()))?;
        return Ok(Recovery::AlreadySealed);
    }
    let length = held
        .metadata()
        .map_err(|error| format!("{}: {error}", live.display()))?
        .len();
    if length > SEGMENT_MAX_BYTES_BOUND {
        return Err(format!(
            "{}: longer than the {SEGMENT_MAX_BYTES_BOUND}-byte segment bound; never read",
            live.display()
        ));
    }
    let bytes = read_at_most(&mut *held, SEGMENT_MAX_BYTES_BOUND)
        .map_err(|error| format!("{}: {error}", live.display()))?;
    if bytes.is_empty() {
        std::fs::remove_file(&live).map_err(|error| format!("{}: {error}", live.display()))?;
        return Ok(Recovery::Empty);
    }
    let prefix = scan(&bytes);
    if prefix.interior_corruption || prefix.segment_id() != Some(segment_id) {
        let corrupt = layout.corrupt_segment(segment_id);
        std::fs::rename(&live, &corrupt).map_err(|error| format!("{}: {error}", live.display()))?;
        return Ok(Recovery::Corrupt {
            segment_id: segment_id.to_string(),
        });
    }
    // A writer that died between its seal flush and the compression left its seal as the last
    // valid line: it is superseded, so the segment ends with exactly one seal, the recovered
    // one, carrying the writer's counts (known in this case).
    let (records, kept_bytes, stranded) = match prefix.records.split_last() {
        Some((Record::Seal(seal), rest)) => {
            let seal_line = bytes[..prefix.kept_bytes - 1]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |at| at + 1);
            (rest, seal_line, Some(seal.clone()))
        }
        _ => (prefix.records.as_slice(), prefix.kept_bytes, None),
    };
    let mut additions = Vec::new();
    let mut unended = 0;
    let mut open: BTreeMap<u64, String> = BTreeMap::new();
    let (mut spans, mut events, mut blobs) = (0, 0, 0);
    for record in records {
        match record {
            Record::SpanStart(start) => {
                open.insert(start.seq, start.span_id.clone());
                spans += 1;
            }
            Record::SpanEnd(end) => open.retain(|_, id| *id != end.span_id),
            Record::Event(_) => events += 1,
            Record::Blob(_) => blobs += 1,
            _ => {}
        }
    }
    let mut seq = records.last().map_or(1, Record::seq);
    for span_id in open.into_values() {
        seq += 1;
        unended += 1;
        additions.push(Record::SpanEnd(SpanEnd {
            seq,
            schema: schema::SPAN.into(),
            span_id,
            end_unix_ns: None,
            status: SpanStatus::Unset,
            attributes: Attributes::new(),
        }));
    }
    let seal = match stranded {
        Some(writer) => Seal {
            seq: seq + 1,
            exit: SealExit::Recovered,
            discarded_bytes: prefix.discarded_bytes,
            refs: format!("{segment_id}{}", layout::REFS),
            ..writer
        },
        // Without the writer's seal its in-memory drop counters are lost: unknown, never 0,
        // even when the prefix holds the exhaustion `problem`.
        None => Seal {
            seq: seq + 1,
            schema: schema::SEAL.into(),
            ended_unix_ns: records
                .iter()
                .rev()
                .find_map(record_time)
                .unwrap_or("0")
                .to_string(),
            exit: SealExit::Recovered,
            spans,
            events,
            blobs,
            dropped: None,
            dropped_chunks: None,
            discarded_bytes: prefix.discarded_bytes,
            refs: format!("{segment_id}{}", layout::REFS),
        },
    };
    additions.push(Record::Seal(seal));
    let mut tail = Vec::new();
    for record in &additions {
        tail.extend(serde_json::to_vec(record).expect("a record serializes"));
        tail.push(b'\n');
    }
    let io = |error: std::io::Error| format!("{}: {error}", live.display());
    held.set_len(kept_bytes as u64).map_err(io)?;
    held.seek(SeekFrom::End(0)).map_err(io)?;
    held.write_all(&tail).map_err(io)?;
    held.sync_all().map_err(io)?;
    let mut kept = bytes[..kept_bytes].to_vec();
    kept.extend_from_slice(&tail);
    let refs = layout.refs(segment_id);
    super::writer::write_atomically(&refs, &super::writer::manifest(&referenced(records)))
        .map_err(|error| format!("{}: {error}", refs.display()))?;
    let sealed = layout.sealed_segment(segment_id);
    super::writer::write_atomically(&sealed, &crate::blob::compress(&kept))
        .map_err(|error| format!("{}: {error}", sealed.display()))?;
    std::fs::remove_file(&live).map_err(io)?;
    drop(held);
    Ok(Recovery::Recovered {
        segment_id: segment_id.to_string(),
        discarded_bytes: prefix.discarded_bytes,
        unended_spans: unended,
    })
}

/// Recover every unsealed segment whose lock can be taken; returns what happened to each.
pub fn recover_all(layout: &Layout) -> Result<Vec<Recovery>, String> {
    let listing = list(layout)?;
    listing
        .unsealed
        .iter()
        .chain(&listing.live)
        .map(|id| recover(layout, id))
        .collect()
}

/// The digests a sealed segment's `.refs` manifest names; a missing manifest falls back to the
/// segment's `blob_ref` records.
pub fn manifest_digests(layout: &Layout, segment_id: &str) -> Result<BTreeSet<String>, String> {
    let path = layout.refs(segment_id);
    let mut text = String::new();
    match layout::open_nofollow(&path).and_then(|mut file| file.read_to_string(&mut text)) {
        Ok(_) => Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(referenced(&read_sealed(
            &layout.sealed_segment(segment_id),
        )?)),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Whether `name`, beside `v1/` under a telemetry root, is another version's directory.
pub fn is_other_version(name: &str) -> bool {
    name != layout::VERSION_DIR
        && name.len() > 1
        && name.starts_with('v')
        && name[1..].bytes().all(|byte| byte.is_ascii_digit())
}

/// Other version directories beside `v1/`: listed unreadable, never read.
pub fn other_versions(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut other: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| is_other_version(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    other.sort();
    other
}

/// The bytes of the file at `path`, for a reader that must not create it.
pub fn file_len(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).map_or(0, |metadata| metadata.len())
}

/// Open `path` for reading, for the index's tail reads, never through a link.
pub fn open(path: &Path) -> std::io::Result<File> {
    layout::open_nofollow(path)
}
