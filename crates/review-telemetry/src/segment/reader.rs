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
//! is interior corruption: the segment is renamed `.corrupt`, never read again. A `.jsonl.gz`
//! or `.refs` beside a `.jsonl` of the same id is stale staging (the publication's last step
//! removes the `.jsonl`): every reader treats the segment as unsealed ([`Listing::stale`]) and
//! recovery replaces those files, or, when it rejects the `.jsonl` (empty or corrupt), removes
//! them before it removes or renames the `.jsonl`, so they are never read as sealed.
//!
//! **Housekeeping** reads a `.jsonl` segment's valid prefix through [`structure`], never
//! [`scan`]: framing, envelopes and the prefix rules, and the full record only of the
//! structural kinds (design §3.1, ADR-0153).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use nix::fcntl::FlockArg;

use crate::layout::{self, Confined, EntryKind, FileIdentity, Layout, SegmentFile};
use crate::level::SEGMENT_MAX_BYTES_BOUND;
use crate::record::{
    Attributes, BlobState, RECORD_MAX_BYTES, Record, Seal, SealExit, SpanEnd, SpanStatus, schema,
};

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

/// The valid prefix of a live or unsealed segment as storage housekeeping reads it (design §3.1,
/// ADR-0153): the structural records only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Structure {
    /// The `segment`, `blob`, `blob_ref`, `problem` and `seal` records of the prefix, in order.
    pub records: Vec<Record>,
    /// Bytes of the complete, structurally valid lines kept, newlines included.
    pub kept_bytes: usize,
    /// Why the first line after the prefix was refused, when it is complete.
    pub invalid: Option<String>,
}

/// The record kinds housekeeping decodes in full; every other kind is judged by its envelope
/// alone.
const STRUCTURAL_KINDS: [&str; 5] = ["segment", "blob", "blob_ref", "problem", "seal"];

/// Every record kind with the versioned type it carries.
const KINDS: [(&str, &str); 12] = [
    ("segment", schema::SEGMENT),
    ("span_start", schema::SPAN),
    ("span_end", schema::SPAN),
    ("event", schema::EVENT),
    ("config", schema::CONFIG),
    ("node", schema::NODE),
    ("finding", schema::FINDING),
    ("demand", schema::DEMAND),
    ("blob", schema::BLOB),
    ("blob_ref", schema::BLOB_REF),
    ("problem", schema::PROBLEM),
    ("seal", schema::SEAL),
];

/// A line's envelope — its type and `seq` — read from the canonical prefix every writer of this
/// release begins a record with, `{"t":"<type>","seq":<seq>,"schema":"<schema>"`, and nothing
/// after it: the type must be a record kind, the `seq` a canonical decimal above zero and the
/// schema the one that kind carries (another version is refused, ADR-0145).
fn envelope(line: &[u8]) -> Result<(&'static str, u64), String> {
    if line.len() > RECORD_MAX_BYTES {
        return Err(format!(
            "a record of {} bytes exceeds the {RECORD_MAX_BYTES}-byte record bound",
            line.len()
        ));
    }
    let not = || "not a record envelope".to_string();
    let rest = line.strip_prefix(b"{\"t\":\"").ok_or_else(not)?;
    let (kind, expected) = KINDS
        .iter()
        .find(|(kind, _)| {
            rest.strip_prefix(kind.as_bytes())
                .is_some_and(|after| after.starts_with(b"\",\"seq\":"))
        })
        .copied()
        .ok_or_else(not)?;
    let rest = &rest[kind.len() + b"\",\"seq\":".len()..];
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let seq = std::str::from_utf8(&rest[..digits])
        .ok()
        .filter(|text| !text.is_empty() && (text.len() == 1 || !text.starts_with('0')))
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|seq| *seq > 0)
        .ok_or_else(|| format!("a `{kind}` record without a valid seq"))?;
    let rest = rest[digits..]
        .strip_prefix(b",\"schema\":\"")
        .ok_or_else(not)?;
    match rest.strip_prefix(expected.as_bytes()) {
        Some(after) if after.starts_with(b"\"") => Ok((kind, seq)),
        _ => Err(format!(
            "a `{kind}` record of another schema than `{expected}`"
        )),
    }
}

/// The structural scanner storage housekeeping reads a live or unsealed segment's valid prefix
/// with (design §3.1, ADR-0153): it validates the framing (complete lines within the record
/// bound), each line's envelope — type and `seq` ([`envelope`]) — and the prefix rules (the
/// header first and only first, `seq` strictly increasing, nothing after the seal), and decodes
/// the full record only for the `segment`, `blob`, `blob_ref`, `problem` and `seal` kinds. A
/// span, an event, a configuration part, a plan node, a Finding or a Demand is never decoded:
/// whatever its payload holds, only its envelope is read. Where [`scan`], the full reader,
/// refuses a line whose payload is invalid, this keeps it.
pub fn structure(bytes: &[u8]) -> Structure {
    let mut structure = Structure::default();
    let mut offset = 0;
    let mut last: Option<Position> = None;
    while let Some(newline) = bytes[offset..].iter().position(|byte| *byte == b'\n') {
        let line = &bytes[offset..offset + newline];
        let (kind, seq) = match envelope(line) {
            Ok(envelope) => envelope,
            Err(reason) => {
                structure.invalid = Some(reason);
                break;
            }
        };
        let ordered = match last {
            None => kind == "segment" && seq == 1,
            Some(Position { sealed: true, .. }) => false,
            Some(previous) => kind != "segment" && seq > previous.seq,
        };
        if !ordered {
            structure.invalid = Some(format!("a `{kind}` record at seq {seq} out of order"));
            break;
        }
        if STRUCTURAL_KINDS.contains(&kind) {
            match Record::parse_line(line) {
                Ok(record) if record.kind() == kind && record.seq() == seq => {
                    structure.records.push(record);
                }
                Ok(_) => {
                    structure.invalid = Some(format!("a `{kind}` record of another envelope"));
                    break;
                }
                Err(reason) => {
                    structure.invalid = Some(reason);
                    break;
                }
            }
        }
        last = Some(Position {
            seq,
            sealed: kind == "seal",
        });
        offset += newline + 1;
    }
    structure.kept_bytes = offset;
    structure
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

/// Open the file a caller names for reading, refusing a symbolic link as its last component.
/// The layout's own readers open segments relative to the opened `segments/` directory
/// ([`read_live_in`], [`read_sealed_in`]).
fn open_path(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(path)
}

/// The bytes of an opened `.jsonl` segment. Its length is judged before any byte is read: a
/// file longer than any segment this release writes ([`SEGMENT_MAX_BYTES_BOUND`]) is refused,
/// never loaded.
fn live_bytes(file: File) -> std::io::Result<Vec<u8>> {
    if file.metadata()?.len() > SEGMENT_MAX_BYTES_BOUND {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("longer than the {SEGMENT_MAX_BYTES_BOUND}-byte segment bound; never read"),
        ));
    }
    read_at_most(file, SEGMENT_MAX_BYTES_BOUND)
}

/// The bytes of the `.jsonl` segment at `path`, opened without following a link.
pub fn read_live_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    live_bytes(open_path(path)?)
}

/// The bytes of the `.jsonl` segment `segment_id`, opened relative to `segments`.
pub fn read_live_in(segments: &Confined, segment_id: &str) -> std::io::Result<Vec<u8>> {
    live_bytes(segments.open_file(&format!("{segment_id}{}", layout::LIVE))?)
}

/// The valid prefix of a live or unsealed segment, read without its lock.
pub fn read_live(path: &Path) -> Result<Prefix, String> {
    let bytes = read_live_bytes(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(scan(&bytes))
}

/// Every record of an opened sealed segment. Any invalid line, or a missing seal, refuses it.
fn sealed_records(file: File, path: &Path) -> Result<Vec<Record>, String> {
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

/// Every record of the sealed segment at `path`. Any invalid line, or a missing seal, refuses
/// it.
pub fn read_sealed(path: &Path) -> Result<Vec<Record>, String> {
    let file = open_path(path).map_err(|error| format!("{}: {error}", path.display()))?;
    sealed_records(file, path)
}

/// Every record of the sealed segment `segment_id`, opened relative to `segments`, with the
/// identity of the file read (its device, inode, size and modification time).
pub fn read_sealed_in(
    segments: &Confined,
    segment_id: &str,
) -> Result<(Vec<Record>, FileIdentity), String> {
    let path = segments.join(&format!("{segment_id}{}", layout::SEALED));
    let failed = |error: std::io::Error| format!("{}: {error}", path.display());
    let file = segments
        .open_file(&format!("{segment_id}{}", layout::SEALED))
        .map_err(failed)?;
    let identity = FileIdentity::of_file(&file).map_err(failed)?;
    Ok((sealed_records(file, &path)?, identity))
}

/// What housekeeping reads of a sealed segment (design §3.1, ADR-0153): its `segment` header
/// and its `seal` record, and nothing between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedFrame {
    /// The seal's `ended_unix_ns`.
    pub ended_unix_ns: u128,
    /// The identity of the file read (its device, inode, size and modification time).
    pub identity: FileIdentity,
}

/// The header and the seal of the sealed segment `segment_id`, opened relative to `segments`,
/// read structurally: the file is decompressed as a stream (up to the longest segment this
/// release writes) keeping only its first line and its last, and only those two are decoded —
/// never a span, an event, an attribute or a blob record, whatever they hold. The first must be
/// the `segment` header naming `segment_id` at `seq` 1 and the last the `seal` naming its
/// `.refs` manifest, the stream ending with that line's newline. What housekeeping (`af
/// storage`, the sweep, `af telemetry gc`) reads instead of [`read_sealed_in`], the full record
/// reader.
pub fn sealed_frame_in(segments: &Confined, segment_id: &str) -> Result<SealedFrame, String> {
    let name = format!("{segment_id}{}", layout::SEALED);
    let path = segments.join(&name);
    let failed = |error: std::io::Error| format!("{}: {error}", path.display());
    let refused = |why: &str| {
        format!(
            "{}: not a sealed segment of this release ({why})",
            path.display()
        )
    };
    let file = segments.open_file(&name).map_err(failed)?;
    let identity = FileIdentity::of_file(&file).map_err(failed)?;
    let mut stream = GzDecoder::new(file);
    let mut chunk = vec![0_u8; 64 * 1024];
    // The first complete line, the last, and the one being read; a line past the record bound
    // is kept as `None` (never a header or a seal), so nothing between is judged at all.
    let (mut first, mut last) = (None::<Option<Vec<u8>>>, None::<Vec<u8>>);
    let mut current = Some(Vec::new());
    let mut total: u64 = 0;
    loop {
        let read = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(failed(error)),
        };
        total += read as u64;
        if total > SEGMENT_MAX_BYTES_BOUND {
            return Err(refused(&format!(
                "longer than the {SEGMENT_MAX_BYTES_BOUND}-byte segment bound; never read"
            )));
        }
        for piece in chunk[..read].split_inclusive(|byte| *byte == b'\n') {
            let complete = piece.last() == Some(&b'\n');
            let piece = if complete {
                &piece[..piece.len() - 1]
            } else {
                piece
            };
            if let Some(line) = &mut current {
                if line.len() + piece.len() > RECORD_MAX_BYTES {
                    current = None;
                } else {
                    line.extend_from_slice(piece);
                }
            }
            if complete {
                let line = current.replace(Vec::new());
                if first.is_none() {
                    first = Some(line.clone());
                }
                last = line;
            }
        }
    }
    if current.is_none_or(|line| !line.is_empty()) {
        return Err(refused("bytes after the last complete line"));
    }
    let first = first
        .ok_or_else(|| refused("empty"))?
        .ok_or_else(|| refused("its first line is past the record bound"))?;
    let last = last.ok_or_else(|| refused("its last line is past the record bound"))?;
    match Record::parse_line(&first) {
        Ok(Record::Segment(header)) if header.seq == 1 && header.segment_id == segment_id => {}
        Ok(_) => return Err(refused("its first line is not its own segment header")),
        Err(reason) => return Err(refused(&format!("its header: {reason}"))),
    }
    let seal = match Record::parse_line(&last) {
        Ok(Record::Seal(seal)) => seal,
        Ok(_) => return Err(refused("its last line is not a seal")),
        Err(reason) => return Err(refused(&format!("its seal: {reason}"))),
    };
    if seal.seq <= 1 || seal.refs != format!("{segment_id}{}", layout::REFS) {
        return Err(refused("its seal is not its own"));
    }
    let ended_unix_ns = seal
        .ended_unix_ns
        .parse::<u128>()
        .map_err(|_| refused("its seal names no end time"))?;
    Ok(SealedFrame {
        ended_unix_ns,
        identity,
    })
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
    /// `.jsonl.gz` segments with no `.jsonl` of the same id beside them.
    pub sealed: Vec<String>,
    pub corrupt: Vec<String>,
    /// `.refs` manifests, by segment id, with no `.jsonl` of the same id beside them.
    pub refs: Vec<String>,
    /// Segment ids whose `.jsonl` exists beside a `.jsonl.gz` or `.refs` of the same id: the
    /// segment is the live or unsealed `.jsonl` (listed there), and those files are stale
    /// staging of a publication that never completed — never read as sealed, replaced by
    /// recovery and removed by the sweep after an hour.
    pub stale: Vec<String>,
    /// Temporary files under `segments/`.
    pub temporary: Vec<PathBuf>,
}

/// List `segments/` without reading any segment. Only regular files are listed, each name
/// matched exactly: a link, a directory or a stray name is never a segment or a temporary (it is
/// what `status` reports as unrecognized).
pub fn list(layout: &Layout) -> Result<Listing, String> {
    let directory = layout.segments();
    let failed = |error: std::io::Error| format!("{}: {error}", directory.display());
    match Confined::versioned(layout, "segments").map_err(failed)? {
        Some(segments) => list_in(&segments),
        None => Ok(Listing::default()),
    }
}

/// [`list`] of the opened `segments/` directory.
pub fn list_in(segments: &Confined) -> Result<Listing, String> {
    let failed = |error: std::io::Error| format!("{}: {error}", segments.path().display());
    let mut listing = Listing::default();
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
    // A `.jsonl` is the segment whatever else of its id lies beside it: the publication's last
    // step removes it, so a `.jsonl.gz` or `.refs` beside one is stale staging.
    let unsealed: BTreeSet<String> = listing
        .live
        .iter()
        .chain(&listing.unsealed)
        .cloned()
        .collect();
    let mut stale = BTreeSet::new();
    for list in [&mut listing.sealed, &mut listing.refs] {
        list.retain(|id| {
            let beside = unsealed.contains(id);
            if beside {
                stale.insert(id.clone());
            }
            !beside
        });
    }
    listing.stale = stale.into_iter().collect();
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
    /// Its writer sealed it after all: the `.jsonl` was gone by the time its lock was had.
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
    let failed = |error: std::io::Error| format!("{}: {error}", layout.segments().display());
    match Confined::versioned(layout, "segments").map_err(failed)? {
        Some(segments) => recover_in(&segments, segment_id),
        None => Ok(Recovery::AlreadySealed),
    }
}

/// [`recover`] in the opened `segments/` directory: the segment is opened, judged, rewritten,
/// renamed and removed relative to it, never through a path a link could redirect.
pub fn recover_in(segments: &Confined, segment_id: &str) -> Result<Recovery, String> {
    let name = format!("{segment_id}{}", layout::LIVE);
    let sealed_name = format!("{segment_id}{}", layout::SEALED);
    let live = segments.join(&name);
    let io = |error: std::io::Error| format!("{}: {error}", live.display());
    let file = match segments.open_read_write(&name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Recovery::AlreadySealed);
        }
        Err(error) => return Err(io(error)),
    };
    let Ok(mut held) = layout::try_lock(file, FlockArg::LockExclusiveNonblock) else {
        return Ok(Recovery::Live);
    };
    // The lock is on what we opened; the name must still name it (a writer that sealed in the
    // meantime unlinked it).
    let opened = FileIdentity::of_file(&held).map_err(io)?;
    match segments.identity(&name) {
        Some(current) if current.ino == opened.ino && current.dev == opened.dev => {}
        _ => return Ok(Recovery::AlreadySealed),
    }
    // A `.jsonl.gz` or `.refs` of the same id beside the `.jsonl` is stale staging of a
    // publication that never completed (its last step removes the `.jsonl`): the `.jsonl` is
    // the segment, and the files written below replace them.
    if opened.size > SEGMENT_MAX_BYTES_BOUND {
        return Err(format!(
            "{}: longer than the {SEGMENT_MAX_BYTES_BOUND}-byte segment bound; never read",
            live.display()
        ));
    }
    let bytes = read_at_most(&mut *held, SEGMENT_MAX_BYTES_BOUND).map_err(io)?;
    // A live segment rejected — empty, or corrupt — leaves no seal of its id readable: the
    // stale `.jsonl.gz` and `.refs` beside it go first, through the held directory, so no
    // listing ever takes them as sealed once the `.jsonl` is gone.
    let withdraw_stale = || -> Result<(), String> {
        for stale in [sealed_name.clone(), format!("{segment_id}{}", layout::REFS)] {
            match segments.unlink(&stale) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", segments.join(&stale).display())),
            }
        }
        Ok(())
    };
    if bytes.is_empty() {
        withdraw_stale()?;
        segments.unlink(&name).map_err(io)?;
        return Ok(Recovery::Empty);
    }
    let prefix = scan(&bytes);
    if prefix.interior_corruption || prefix.segment_id() != Some(segment_id) {
        withdraw_stale()?;
        segments
            .rename(&name, &format!("{segment_id}{}", layout::CORRUPT))
            .map_err(io)?;
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
    held.set_len(kept_bytes as u64).map_err(io)?;
    held.seek(SeekFrom::End(0)).map_err(io)?;
    held.write_all(&tail).map_err(io)?;
    held.sync_all().map_err(io)?;
    let mut kept = bytes[..kept_bytes].to_vec();
    kept.extend_from_slice(&tail);
    let refs = format!("{segment_id}{}", layout::REFS);
    segments
        .write_atomically(&refs, &super::writer::manifest(&referenced(records)))
        .map_err(|error| format!("{}: {error}", segments.join(&refs).display()))?;
    segments
        .write_atomically(&sealed_name, &crate::blob::compress(&kept))
        .map_err(|error| format!("{}: {error}", segments.join(&sealed_name).display()))?;
    segments.unlink(&name).map_err(io)?;
    drop(held);
    Ok(Recovery::Recovered {
        segment_id: segment_id.to_string(),
        discarded_bytes: prefix.discarded_bytes,
        unended_spans: unended,
    })
}

/// Recover every unsealed segment whose lock can be taken; returns what happened to each. The
/// root is opened once and everything beneath it reached through it ([`recover_all_in`]).
pub fn recover_all(layout: &Layout) -> Result<Vec<Recovery>, String> {
    let failed = |error: std::io::Error| format!("{}: {error}", layout.segments().display());
    match Confined::versioned(layout, "segments").map_err(failed)? {
        Some(segments) => recover_all_in(&segments),
        None => Ok(Vec::new()),
    }
}

/// [`recover_all`] in the opened `segments/` directory: every segment, manifest and lock
/// handle is derived from it, so a root swapped after it was opened recovers nothing and writes
/// nothing outside it.
pub fn recover_all_in(segments: &Confined) -> Result<Vec<Recovery>, String> {
    let listing = list_in(segments)?;
    listing
        .unsealed
        .iter()
        .chain(&listing.live)
        .map(|id| recover_in(segments, id))
        .collect()
}

/// The digests a sealed segment's `.refs` manifest names, read relative to the opened
/// `segments/` directory. Housekeeping's read: a missing manifest is refused, never replaced by
/// a read of the segment's records, so the references it would name are unknown.
pub fn manifest_digests_in(
    segments: &Confined,
    segment_id: &str,
) -> Result<BTreeSet<String>, String> {
    let name = format!("{segment_id}{}", layout::REFS);
    let mut text = String::new();
    match segments
        .open_file(&name)
        .and_then(|mut file| file.read_to_string(&mut text))
    {
        Ok(_) => Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "{}: missing; the blobs its segment references are unknown",
            segments.join(&name).display()
        )),
        Err(error) => Err(format!("{}: {error}", segments.join(&name).display())),
    }
}

/// Whether `name`, beside `v1/` under a telemetry root, is another version's directory.
pub fn is_other_version(name: &str) -> bool {
    name != layout::VERSION_DIR
        && name.len() > 1
        && name.starts_with('v')
        && name[1..].bytes().all(|byte| byte.is_ascii_digit())
}

/// The names of other version directories beside `v1/` in the opened root: listed
/// unreadable, never read.
pub fn other_versions(root: &Confined) -> Vec<String> {
    root.names()
        .unwrap_or_default()
        .into_iter()
        .filter(|name| is_other_version(name))
        .collect()
}
