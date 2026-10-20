//! The content digest of a committed tree without capturing it (ADR-0152 clause 9).
//!
//! [`crate::Capture::committed`] stores every blob in a CAS and holds the whole manifest before
//! it digests it. A reader that wants only the digest needs neither: this path writes no blob
//! anywhere, streams each blob through the hash in one fixed buffer, and holds the manifest in at
//! most [`Ceilings::manifest_bytes`] of memory. [`crate::Manifest::content_digest`] hashes the
//! entry count first and then the entries in the byte order of their encoded paths, while
//! `ls-tree` lists them in git's tree order; so the listing is read once into sorted runs, each
//! run beyond the memory ceiling spilled to temporary storage of at most
//! [`Ceilings::temp_bytes`], and the runs are merged into one `cat-file --batch` in canonical
//! order, whose blobs are hashed as they stream. The digest equals the committed capture's for
//! the same tree, by the one framing [`ManifestDigest`] both use.
//!
//! A ceiling or the caller's deadline stops it with a typed reason ([`Stopped`]) and no digest,
//! never the digest of a part, and the temporary storage is removed on every path out.

use std::collections::BinaryHeap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::Instant;

use crate::capture::CaptureError;
use crate::git::{GitError, Repo};
use crate::manifest::{EntryKind, ManifestDigest, digest_reader_with_buffer, encode_path};

/// The name prefix of the directory that holds one digest's spilled runs:
/// `af-tree-digest-<pid>-<random>` in the temporary root, so the crash sweep of
/// `review_sandbox::stale` attributes a directory a killed process left to that process and
/// removes it once no process has that pid, under the rules of the sandbox and check runtime
/// directories.
pub const SPILL_PREFIX: &str = "af-tree-digest-";
/// Memory the manifest may take before a sorted run is spilled: 16 MiB.
pub const MANIFEST_BUFFER_BYTES: u64 = 16 * 1024 * 1024;
/// Temporary storage the spilled runs may take: 256 MiB.
pub const TEMP_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
/// The longest `ls-tree` record read, as every streamed record of the Surveyor.
pub const MAX_LISTING_RECORD_BYTES: usize = 64 * 1024;
/// The one buffer every blob streams through.
const BLOB_BUFFER_BYTES: usize = 64 * 1024;
/// The buffer through which the merge reads each spilled run.
const RUN_BUFFER_BYTES: usize = 8 * 1024;
/// Entries the merge may run ahead of the hash.
const AHEAD: usize = 1024;

/// The bounds of one digest. Production uses [`Ceilings::FIXED`]; a test may lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ceilings {
    pub manifest_bytes: u64,
    pub temp_bytes: u64,
}

impl Ceilings {
    /// The fixed ceilings: no project raises them.
    pub const FIXED: Ceilings = Ceilings {
        manifest_bytes: MANIFEST_BUFFER_BYTES,
        temp_bytes: TEMP_MANIFEST_BYTES,
    };
}

/// Why no digest was computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The caller's deadline passed first.
    Deadline,
    /// An `ls-tree` record is longer than [`MAX_LISTING_RECORD_BYTES`].
    RecordBytes,
    /// One manifest record does not fit [`Ceilings::manifest_bytes`].
    ManifestBytes,
    /// The spilled runs would exceed [`Ceilings::temp_bytes`].
    TempBytes,
}

/// One manifest entry before its blob is read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    path: String,
    kind: EntryKind,
    oid: String,
}

impl Pending {
    /// What the entry costs held or spilled: the bytes of its run record.
    fn bytes(&self) -> u64 {
        (4 + self.path.len() + 1 + 2 + self.oid.len()) as u64
    }

    fn write(&self, out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(&(self.path.len() as u32).to_be_bytes())?;
        out.write_all(self.path.as_bytes())?;
        out.write_all(&[match self.kind {
            EntryKind::File => 0,
            EntryKind::Executable => 1,
            EntryKind::Symlink => 2,
        }])?;
        out.write_all(&(self.oid.len() as u16).to_be_bytes())?;
        out.write_all(self.oid.as_bytes())
    }

    /// The next record of a run; `None` at its end.
    fn read(input: &mut impl Read) -> std::io::Result<Option<Pending>> {
        let mut length = [0_u8; 4];
        match input.read_exact(&mut length) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let mut path = vec![0_u8; u32::from_be_bytes(length) as usize];
        input.read_exact(&mut path)?;
        let mut kind = [0_u8; 1];
        input.read_exact(&mut kind)?;
        let mut length = [0_u8; 2];
        input.read_exact(&mut length)?;
        let mut oid = vec![0_u8; u16::from_be_bytes(length) as usize];
        input.read_exact(&mut oid)?;
        let text = |bytes: Vec<u8>| {
            String::from_utf8(bytes).map_err(|_| std::io::Error::other("a run is not UTF-8"))
        };
        Ok(Some(Pending {
            path: text(path)?,
            kind: match kind[0] {
                0 => EntryKind::File,
                1 => EntryKind::Executable,
                2 => EntryKind::Symlink,
                _ => return Err(std::io::Error::other("a run names no entry kind")),
            },
            oid: text(oid)?,
        }))
    }
}

/// Why reading stopped inside a stream.
enum Halt {
    Stopped(Stopped),
    Failed(CaptureError),
}

impl From<CaptureError> for Halt {
    fn from(error: CaptureError) -> Self {
        Halt::Failed(error)
    }
}

impl From<std::io::Error> for Halt {
    fn from(error: std::io::Error) -> Self {
        Halt::Failed(CaptureError::Io(error))
    }
}

/// The manifest as the listing yields it: at most `manifest_bytes` held, each run beyond it
/// sorted and spilled under `temp_parent`, at most `temp_bytes` in all.
struct Runs<'a> {
    ceilings: Ceilings,
    temp_parent: &'a Path,
    held: Vec<Pending>,
    held_bytes: u64,
    spilled_bytes: u64,
    entries: u64,
    directory: Option<tempfile::TempDir>,
    runs: Vec<std::path::PathBuf>,
}

impl<'a> Runs<'a> {
    fn new(ceilings: Ceilings, temp_parent: &'a Path) -> Self {
        Self {
            ceilings,
            temp_parent,
            held: Vec::new(),
            held_bytes: 0,
            spilled_bytes: 0,
            entries: 0,
            directory: None,
            runs: Vec::new(),
        }
    }

    fn add(&mut self, entry: Pending) -> Result<(), Halt> {
        let bytes = entry.bytes();
        if bytes > self.ceilings.manifest_bytes {
            return Err(Halt::Stopped(Stopped::ManifestBytes));
        }
        if self.held_bytes + bytes > self.ceilings.manifest_bytes {
            self.spill()?;
        }
        self.held_bytes += bytes;
        self.entries += 1;
        self.held.push(entry);
        Ok(())
    }

    /// Sort what is held and write it as one run.
    fn spill(&mut self) -> Result<(), Halt> {
        if self.held.is_empty() {
            return Ok(());
        }
        if self.spilled_bytes + self.held_bytes > self.ceilings.temp_bytes {
            return Err(Halt::Stopped(Stopped::TempBytes));
        }
        if self.directory.is_none() {
            self.directory = Some(
                tempfile::Builder::new()
                    .prefix(&format!("{SPILL_PREFIX}{}-", std::process::id()))
                    .tempdir_in(self.temp_parent)?,
            );
        }
        let directory = self.directory.as_ref().expect("created above");
        let path = directory.path().join(format!("run-{}", self.runs.len()));
        self.held
            .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let mut out = BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?,
        );
        for entry in self.held.drain(..) {
            entry.write(&mut out)?;
        }
        out.flush()?;
        self.spilled_bytes += self.held_bytes;
        self.held_bytes = 0;
        self.runs.push(path);
        Ok(())
    }

    /// Every entry in canonical order: the held ones alone, or every run merged.
    fn into_order(mut self) -> Result<(Order, Option<tempfile::TempDir>), Halt> {
        if self.runs.is_empty() {
            self.held
                .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
            return Ok((Order::Held(self.held.into_iter()), None));
        }
        self.spill()?;
        let mut readers = Vec::with_capacity(self.runs.len());
        let mut heads = BinaryHeap::new();
        for (index, path) in self.runs.iter().enumerate() {
            let mut reader = BufReader::with_capacity(RUN_BUFFER_BYTES, std::fs::File::open(path)?);
            if let Some(entry) = Pending::read(&mut reader)? {
                heads.push(Head { entry, index });
            }
            readers.push(reader);
        }
        Ok((Order::Merged { readers, heads }, self.directory))
    }
}

/// The smallest path first.
struct Head {
    entry: Pending,
    index: usize,
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.entry.path == other.entry.path
    }
}

impl Eq for Head {}

impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Head {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.entry.path.as_bytes().cmp(self.entry.path.as_bytes())
    }
}

/// The manifest's entries in canonical order.
enum Order {
    Held(std::vec::IntoIter<Pending>),
    Merged {
        readers: Vec<BufReader<std::fs::File>>,
        heads: BinaryHeap<Head>,
    },
}

impl Order {
    fn next(&mut self) -> std::io::Result<Option<Pending>> {
        match self {
            Order::Held(entries) => Ok(entries.next()),
            Order::Merged { readers, heads } => {
                let Some(Head { entry, index }) = heads.pop() else {
                    return Ok(None);
                };
                if let Some(next) = Pending::read(&mut readers[index])? {
                    heads.push(Head { entry: next, index });
                }
                Ok(Some(entry))
            }
        }
    }
}

/// Read one NUL-ended record of at most [`MAX_LISTING_RECORD_BYTES`]; `None` at the end.
fn listing_record(input: &mut dyn BufRead, record: &mut Vec<u8>) -> Result<Option<()>, Halt> {
    record.clear();
    let limit = MAX_LISTING_RECORD_BYTES as u64 + 1;
    let read = (&mut *input).take(limit).read_until(0, record)?;
    if read == 0 {
        return Ok(None);
    }
    if record.last() == Some(&0) {
        record.pop();
    } else if record.len() as u64 >= limit {
        return Err(Halt::Stopped(Stopped::RecordBytes));
    }
    Ok(Some(()))
}

/// The manifest entry of one `ls-tree -r -z` record, `None` for a gitlink or another non-blob,
/// which a committed capture leaves out of its manifest too.
fn listed(record: &[u8]) -> Result<Option<Pending>, CaptureError> {
    let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
        return Ok(None);
    };
    let (meta, path) = record.split_at(tab);
    let path = encode_path(&path[1..]);
    let meta = String::from_utf8_lossy(meta);
    let mut fields = meta.split_whitespace();
    let (Some(mode), Some(kind), Some(oid)) = (fields.next(), fields.next(), fields.next()) else {
        return Ok(None);
    };
    if kind != "blob" {
        return Ok(None);
    }
    let kind = EntryKind::from_mode(mode)
        .ok_or_else(|| CaptureError::UnsupportedEntry { path: path.clone() })?;
    Ok(Some(Pending {
        path,
        kind,
        oid: oid.to_string(),
    }))
}

/// The content digest [`crate::Capture::committed`] would give `rev`'s tree, computed without
/// writing a blob anywhere: `Ok(Err(reason))` when a ceiling or `repo`'s deadline stopped it,
/// with nothing left under `temp_parent`.
pub fn committed_digest(
    repo: &Repo,
    rev: &str,
    ceilings: Ceilings,
    temp_parent: &Path,
) -> Result<Result<String, Stopped>, CaptureError> {
    let deadline = repo.deadline();
    let past = || deadline.is_some_and(|deadline| Instant::now() >= deadline);
    // A git the deadline stopped is the deadline, never a failure.
    let stopped_or = |error: GitError| {
        if past() {
            Ok(Err(Stopped::Deadline))
        } else {
            Err(CaptureError::Git(error))
        }
    };
    let tree = match repo.resolve_tree(rev) {
        Ok(tree) => tree,
        Err(error) => return stopped_or(error),
    };

    let mut runs = Runs::new(ceilings, temp_parent);
    let listing = repo.streaming(
        &["ls-tree", "-r", "-z", "--full-tree", tree.as_str()],
        |_| Ok::<_, std::convert::Infallible>(()),
        |stdout| -> Result<(), Halt> {
            let mut input = BufReader::with_capacity(BLOB_BUFFER_BYTES, stdout);
            let mut record = Vec::new();
            while listing_record(&mut input, &mut record)?.is_some() {
                if past() {
                    return Err(Halt::Stopped(Stopped::Deadline));
                }
                if let Some(entry) = listed(&record)? {
                    runs.add(entry)?;
                }
            }
            Ok(())
        },
    );
    let listing = match listing {
        Ok(listing) => listing,
        Err(error) => return stopped_or(error),
    };
    match listing.output {
        Err(Halt::Stopped(reason)) => return Ok(Err(reason)),
        Err(Halt::Failed(error)) => return Err(error),
        Ok(()) if !listing.status.success() => {
            return stopped_or(GitError::Failed {
                args: vec!["ls-tree".into()],
                stderr: String::from_utf8_lossy(&listing.stderr).into_owned(),
            });
        }
        Ok(()) => {}
    }
    let entries = runs.entries;
    // Held until the digest is finished: dropping it removes every run.
    let (mut order, _spilled) = match runs.into_order() {
        Ok(order) => order,
        Err(Halt::Stopped(reason)) => return Ok(Err(reason)),
        Err(Halt::Failed(error)) => return Err(error),
    };
    let digest = ManifestDigest::new(entries);
    if entries == 0 {
        return Ok(Ok(digest.finish()));
    }

    let (ahead, behind) = std::sync::mpsc::sync_channel::<Pending>(AHEAD);
    let hashed = repo.streaming(
        &["cat-file", "--batch"],
        move |stdin| -> Result<(), CaptureError> {
            while let Some(entry) = order.next().map_err(CaptureError::Io)? {
                let line = format!("{}\n", entry.oid);
                // The hash stopped early: nothing more is asked.
                if ahead.send(entry).is_err() || stdin.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
            Ok(())
        },
        move |stdout| -> Result<(u64, ManifestDigest), Halt> {
            let mut digest = digest;
            let mut input = BufReader::with_capacity(BLOB_BUFFER_BYTES, stdout);
            let mut buffer = vec![0_u8; BLOB_BUFFER_BYTES];
            let mut header = String::new();
            let mut hashed = 0_u64;
            while let Ok(entry) = behind.recv() {
                if past() {
                    return Err(Halt::Stopped(Stopped::Deadline));
                }
                header.clear();
                (&mut input)
                    .take(MAX_LISTING_RECORD_BYTES as u64)
                    .read_line(&mut header)?;
                let fields: Vec<&str> = header.split_whitespace().collect();
                let size = match fields.as_slice() {
                    [oid, "blob", size] if *oid == entry.oid => size.parse::<u64>().ok(),
                    _ => None,
                };
                let Some(size) = size else {
                    return Err(Halt::Failed(CaptureError::ObjectUnproducible {
                        oid: entry.oid,
                        detail: format!("git cat-file answered {:?}", header.trim_end()),
                    }));
                };
                let (content, read) =
                    digest_reader_with_buffer((&mut input).take(size), &mut buffer)?;
                let mut newline = [0_u8; 1];
                input.read_exact(&mut newline)?;
                if read != size || newline != *b"\n" {
                    return Err(Halt::Failed(CaptureError::Batch {
                        detail: format!("object {} ends before its {size} bytes", entry.oid),
                    }));
                }
                digest.entry(&entry.path, entry.kind, &content, size);
                hashed += 1;
            }
            Ok((hashed, digest))
        },
    );
    let hashed = match hashed {
        Ok(hashed) => hashed,
        Err(error) => return stopped_or(error),
    };
    match (hashed.output, hashed.input) {
        (Err(Halt::Stopped(reason)), _) => Ok(Err(reason)),
        (Err(Halt::Failed(error)), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(_), Ok(())) if !hashed.status.success() => stopped_or(GitError::Failed {
            args: vec!["cat-file".into(), "--batch".into()],
            stderr: String::from_utf8_lossy(&hashed.stderr).into_owned(),
        }),
        (Ok((count, _)), Ok(())) if count != entries => Err(CaptureError::Batch {
            detail: format!("{count} of {entries} objects were read"),
        }),
        (Ok((_, digest)), Ok(())) => Ok(Ok(digest.finish())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(path: &str) -> Pending {
        Pending {
            path: path.into(),
            kind: EntryKind::File,
            oid: "a".repeat(40),
        }
    }

    fn drained(order: &mut Order) -> Vec<String> {
        let mut paths = Vec::new();
        while let Some(entry) = order.next().unwrap() {
            paths.push(entry.path);
        }
        paths
    }

    #[test]
    fn spilled_runs_merge_into_canonical_order_and_leave_nothing_behind() {
        let parent = tempfile::tempdir().unwrap();
        let one = pending("a").bytes();
        let ceilings = Ceilings {
            manifest_bytes: one * 2,
            temp_bytes: one * 10,
        };
        let mut runs = Runs::new(ceilings, parent.path());
        for path in ["e", "b", "a%20b", "a/c", "a-b", "d"] {
            runs.add(pending(path)).ok().unwrap();
        }
        assert!(
            runs.runs.len() >= 2,
            "runs spilled, the last one still held"
        );
        // The runs live in one directory named for this process, which the crash sweep knows.
        let names: Vec<String> = std::fs::read_dir(parent.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        let owned = format!("{SPILL_PREFIX}{}-", std::process::id());
        assert!(
            names.len() == 1 && names[0].starts_with(&owned) && names[0].len() > owned.len(),
            "{names:?}"
        );
        let (mut order, spilled) = runs.into_order().ok().unwrap();
        assert_eq!(drained(&mut order), ["a%20b", "a-b", "a/c", "b", "d", "e"]);
        drop(order);
        drop(spilled);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_ceiling_stops_the_manifest_with_its_reason() {
        let parent = tempfile::tempdir().unwrap();
        let one = pending("a").bytes();
        let mut runs = Runs::new(
            Ceilings {
                manifest_bytes: one,
                temp_bytes: one,
            },
            parent.path(),
        );
        runs.add(pending("a")).ok().unwrap();
        runs.add(pending("b")).ok().unwrap();
        assert!(matches!(
            runs.add(pending("c")),
            Err(Halt::Stopped(Stopped::TempBytes))
        ));
        assert!(matches!(
            runs.add(pending("long-path")),
            Err(Halt::Stopped(Stopped::ManifestBytes))
        ));
        drop(runs);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }
}
