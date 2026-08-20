//! Capturing a tree as an immutable, content-identified snapshot.
//!
//! Two providers, and the difference between them is the whole problem:
//!
//! - A **committed** capture reads objects. They cannot change under us, so one pass is enough.
//! - A **dirty** capture reads a live worktree, which someone may be editing *right now*. There
//!   is no atomic read of a directory tree, so the boundary has to be established rather than
//!   assumed: fingerprint the index, take two complete passes, fingerprint the index again, and
//!   admit the result only if all three agree. Any disagreement is retried, and a bounded number
//!   of failures fails closed.
//!
//! The failure this prevents is a torn tree: half the files from before an edit, half from
//! after, digested as though it were a state that existed. Every reviewer would then agree they
//! reviewed snapshot X, and no such X was ever on disk.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Stdio;

use review_store::Cas;
use sha2::{Digest, Sha256};

use crate::git::{GitError, Repo, split_nul};
use crate::manifest::{Entry, EntryKind, Manifest, digest_bytes, encode_path};

#[derive(Debug)]
pub enum CaptureError {
    Git(GitError),
    Io(std::io::Error),
    Cas(String),
    /// The worktree kept changing while it was being read.
    Unstable {
        attempts: u32,
    },
    /// A path git reported that we could not read as a file, symlink or executable.
    UnsupportedEntry {
        path: String,
    },
    /// An object the tree references that git could not hand over. Refused, never invented:
    /// a snapshot with substituted content still digests, verifies and materializes cleanly,
    /// so this is the last point where the shortfall is detectable at all.
    ObjectUnproducible {
        oid: String,
        detail: String,
    },
    /// `git cat-file --batch` answered outside its own protocol.
    Batch {
        detail: String,
    },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Git(e) => write!(f, "{e}"),
            CaptureError::Io(e) => write!(f, "capture io: {e}"),
            CaptureError::Cas(e) => write!(f, "capture cas: {e}"),
            CaptureError::Unstable { attempts } => write!(
                f,
                "worktree changed during capture ({attempts} attempts); refusing to admit a torn tree"
            ),
            CaptureError::UnsupportedEntry { path } => {
                write!(f, "unsupported entry kind at {path}")
            }
            CaptureError::ObjectUnproducible { oid, detail } => write!(
                f,
                "git could not produce object {oid} ({detail}); refusing to capture an invented tree"
            ),
            CaptureError::Batch { detail } => {
                write!(f, "git cat-file --batch: {detail}")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<GitError> for CaptureError {
    fn from(e: GitError) -> Self {
        CaptureError::Git(e)
    }
}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e)
    }
}

/// A captured snapshot: its manifest, its identity, and where it came from.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub manifest: Manifest,
    pub content_digest: String,
    pub repository_id: String,
    /// The commit this content corresponds to, when one exists. Provenance, not identity.
    pub source_revision: Option<String>,
    pub dirty: bool,
    /// Passes consumed before the read boundary held. Always 1 for a committed capture.
    pub attempts: u32,
}

impl Snapshot {
    /// The `SourceSnapshot@1` payload for this capture.
    pub fn to_payload(&self, tree_id: &str, manifest_artifact: Option<&str>) -> serde_json::Value {
        let capture = if self.dirty {
            serde_json::json!({
                "kind": "synthetic_worktree",
                "tree_id": tree_id,
                "boundary": "revalidated",
                "attempts": self.attempts,
            })
        } else {
            serde_json::json!({ "kind": "committed", "tree_id": tree_id })
        };
        let mut payload = serde_json::json!({
            "repository_id": self.repository_id,
            "vcs": "git",
            "capture": capture,
            "content_digest": self.content_digest,
        });
        if let Some(revision) = &self.source_revision {
            payload["source_revision"] = serde_json::json!(revision);
        }
        if let Some(manifest_id) = manifest_artifact {
            payload["artifact_manifest"] = serde_json::json!(manifest_id);
        }
        payload
    }
}

/// A seam for proving the read boundary works.
///
/// Real captures use [`NoObserver`]. A test implements this to mutate the worktree *between*
/// the two passes, which is the only way to demonstrate that the revalidation catches what it
/// claims to catch.
pub trait CaptureObserver {
    fn between_passes(&self, _attempt: u32) {}
}

pub struct NoObserver;
impl CaptureObserver for NoObserver {}

/// Where a streamed object lands. Capture hands each blob here exactly once, as it arrives.
type ObjectSink<'a> = dyn FnMut(&str, &[u8]) -> Result<(), CaptureError> + 'a;

pub struct Capture<'a> {
    repo: &'a Repo,
    cas: &'a Cas,
    /// How many times a changing worktree may be retried before the capture fails closed.
    pub max_attempts: u32,
}

impl<'a> Capture<'a> {
    pub fn new(repo: &'a Repo, cas: &'a Cas) -> Self {
        Self {
            repo,
            cas,
            max_attempts: 3,
        }
    }

    /// Capture a committed tree. Objects are immutable, so this needs no read boundary.
    pub fn committed(&self, rev: &str) -> Result<Snapshot, CaptureError> {
        let commit = self.repo.rev_parse(rev)?;
        let listing = self
            .repo
            .bytes(&["ls-tree", "-r", "-z", "--full-tree", &commit])?;

        let mut oids: Vec<(String, EntryKind, String)> = Vec::new();
        for record in split_nul(&listing) {
            // "<mode> SP <type> SP <oid> TAB <path>"
            let Some(tab) = record.iter().position(|b| *b == b'\t') else {
                continue;
            };
            let (meta, path) = record.split_at(tab);
            let path = encode_path(&path[1..]);
            let meta = String::from_utf8_lossy(meta);
            let mut fields = meta.split_whitespace();
            let (Some(mode), Some(kind), Some(oid)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // Gitlinks (submodule commits) are recorded by reference in the snapshot's
            // submodule list, never fetched — implicit recursion is exactly what capture must
            // not do.
            if kind != "blob" {
                continue;
            }
            let entry_kind = EntryKind::from_mode(mode)
                .ok_or_else(|| CaptureError::UnsupportedEntry { path: path.clone() })?;
            oids.push((path, entry_kind, oid.to_string()));
        }

        // Each distinct object is read once, stored as it arrives, and reduced to its digest —
        // one blob resident at a time, not the tree.
        let mut unique: Vec<String> = oids.iter().map(|(_, _, o)| o.clone()).collect();
        unique.sort();
        unique.dedup();
        let mut stored: BTreeMap<String, (String, u64)> = BTreeMap::new();
        self.read_objects(&unique, &mut |oid, bytes| {
            let digest = self.store(bytes)?;
            stored.insert(oid.to_string(), (digest, bytes.len() as u64));
            Ok(())
        })?;

        let mut entries = Vec::with_capacity(oids.len());
        for (path, kind, oid) in &oids {
            // An object the tree references but git never delivered is a refusal, not an empty
            // file: the snapshot must be the commit's content or no snapshot at all.
            let (digest, size) =
                stored
                    .get(oid)
                    .ok_or_else(|| CaptureError::ObjectUnproducible {
                        oid: oid.clone(),
                        detail: "not returned by git cat-file".to_string(),
                    })?;
            entries.push(Entry {
                path: path.clone(),
                kind: *kind,
                content: digest.clone(),
                size: *size,
            });
        }

        let manifest = Manifest::new(entries);
        Ok(Snapshot {
            content_digest: manifest.content_digest(),
            manifest,
            repository_id: self.repo.repository_id()?,
            source_revision: Some(commit),
            dirty: false,
            attempts: 1,
        })
    }

    /// Capture the live worktree behind a revalidated read boundary.
    pub fn dirty(&self) -> Result<Snapshot, CaptureError> {
        self.dirty_observed(&NoObserver)
    }

    pub fn dirty_observed(&self, observer: &dyn CaptureObserver) -> Result<Snapshot, CaptureError> {
        for attempt in 1..=self.max_attempts {
            let index_before = self.index_fingerprint()?;
            let first = self.scan_worktree(false)?;
            observer.between_passes(attempt);
            // The second pass publishes as it reads: if the boundary holds these are exactly
            // the snapshot's bytes, and if it does not, unreferenced CAS objects are inert.
            let second = self.scan_worktree(true)?;
            let index_after = self.index_fingerprint()?;

            if index_before == index_after && first == second {
                let mut entries = Vec::with_capacity(second.len());
                for (path, (kind, digest, size)) in second {
                    entries.push(Entry {
                        path,
                        kind,
                        content: digest,
                        size,
                    });
                }
                let manifest = Manifest::new(entries);
                return Ok(Snapshot {
                    content_digest: manifest.content_digest(),
                    manifest,
                    repository_id: self.repo.repository_id()?,
                    source_revision: self.repo.rev_parse("HEAD").ok(),
                    dirty: true,
                    attempts: attempt,
                });
            }
        }
        Err(CaptureError::Unstable {
            attempts: self.max_attempts,
        })
    }

    /// A fingerprint of the index as git reports it. Cheap, and it catches a staged change that
    /// leaves file bytes untouched.
    fn index_fingerprint(&self) -> Result<String, CaptureError> {
        let listing = self.repo.bytes(&["ls-files", "-s", "-z"])?;
        let mut hasher = Sha256::new();
        hasher.update(&listing);
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// One complete pass over every path under review — tracked plus untracked-not-ignored —
    /// reduced to fingerprints: kind, content digest, size. One file's bytes are resident at a
    /// time; the stability comparison needs 32 bytes per path, never the tree twice.
    ///
    /// Bytes are read from the filesystem, never through git, so no clean filter or textconv can
    /// interpose. `.gitattributes` has nothing to act on. With `publish` set, each file's bytes
    /// are stored to the CAS as they are read, so the pass that is admitted needs no re-read.
    fn scan_worktree(
        &self,
        publish: bool,
    ) -> Result<BTreeMap<String, (EntryKind, String, u64)>, CaptureError> {
        let mut paths: Vec<String> = Vec::new();
        for args in [
            vec!["ls-files", "-z", "--cached"],
            vec!["ls-files", "-z", "--others", "--exclude-standard"],
        ] {
            for record in split_nul(&self.repo.bytes(&args)?) {
                paths.push(encode_path(record));
            }
        }
        paths.sort();
        paths.dedup();

        let mut out = BTreeMap::new();
        for path in paths {
            // `path` is the encoded manifest key; the filesystem read needs the raw bytes.
            let full = self.repo.workdir().join(crate::manifest::fs_path(&path));
            let Ok(meta) = std::fs::symlink_metadata(&full) else {
                // Tracked but deleted from the worktree: it is not part of what is there.
                continue;
            };
            let (kind, bytes) = if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&full)?;
                (
                    EntryKind::Symlink,
                    target.to_string_lossy().into_owned().into_bytes(),
                )
            } else if meta.is_file() {
                let bytes = std::fs::read(&full)?;
                (
                    if is_executable(&meta) {
                        EntryKind::Executable
                    } else {
                        EntryKind::File
                    },
                    bytes,
                )
            } else {
                continue;
            };
            let digest = if publish {
                self.store(&bytes)?
            } else {
                digest_bytes(&bytes)
            };
            out.insert(path, (kind, digest, bytes.len() as u64));
        }
        Ok(out)
    }

    /// Publish the bytes and return the digest they are filed under — the same value the
    /// manifest records, so a manifest entry is always a working lookup key.
    fn store(&self, bytes: &[u8]) -> Result<String, CaptureError> {
        let digest = self
            .cas
            .put(bytes)
            .map_err(|e| CaptureError::Cas(e.to_string()))?;
        debug_assert_eq!(digest, digest_bytes(bytes));
        Ok(digest)
    }

    /// Read every object with one `cat-file --batch`, handing each to `sink` as it arrives.
    /// One object is resident at a time; nothing accumulates here.
    ///
    /// A single process for the whole tree: the oids are written on a dedicated thread while
    /// stdout is drained here, so neither pipe can fill and block the other. Spawning one git
    /// per 128 objects paid a process-and-pack-open on a path whose whole point is to avoid
    /// per-object overhead — measured 23x slower on a 5,000-blob repo.
    ///
    /// Every way git can fall short is surfaced: a `missing` answer, a malformed or truncated
    /// stream, a non-zero exit. Anything less and a shortfall becomes invented snapshot content.
    fn read_objects(&self, oids: &[String], sink: &mut ObjectSink<'_>) -> Result<(), CaptureError> {
        if oids.is_empty() {
            return Ok(());
        }
        let mut child = self
            .repo
            .streaming(&["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CaptureError::Git(GitError::Spawn(e)))?;

        // Feed the request on its own thread: with the whole tree's oids, writing them all
        // before reading any output is the deadlock the two threads exist to prevent.
        let mut stdin = child.stdin.take().expect("stdin piped");
        let requested: Vec<String> = oids.to_vec();
        let writer = std::thread::spawn(move || {
            for oid in &requested {
                if writeln!(stdin, "{oid}").is_err() {
                    break; // git went away (killed on our early exit); nothing left to ask.
                }
            }
        });
        let mut stderr_pipe = child.stderr.take().expect("stderr piped");
        let stderr_thread = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buffer);
            buffer
        });

        let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
        let streamed = parse_batch_stream(&mut stdout, sink);
        if streamed.is_err() {
            // Stop reading before we finish asking: kill git so the writer's next `writeln`
            // fails rather than blocking on a pipe nobody is draining.
            let _ = child.kill();
        }
        let _ = writer.join();
        let stderr = stderr_thread.join().unwrap_or_default();
        let status = child.wait()?;

        let git_failed = || {
            CaptureError::Git(GitError::Failed {
                args: vec!["cat-file".to_string(), "--batch".to_string()],
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        };
        match streamed {
            // The sink's failure (a CAS refusal) is the precise cause; the abandoned pipe makes
            // git's own exit uninformative noise after it.
            Err(StreamError::Sink(error)) => Err(error),
            // A valid `missing` answer is authoritative even when git also emitted an unrelated
            // environment warning on stderr. The non-zero exit is our kill after that answer.
            Err(StreamError::Protocol(error @ CaptureError::ObjectUnproducible { .. })) => {
                Err(error)
            }
            // For malformed/truncated protocol, stderr can carry the more precise git failure.
            Err(StreamError::Protocol(error)) => {
                if stderr.is_empty() {
                    Err(error)
                } else {
                    Err(git_failed())
                }
            }
            // A clean stream but a git that died: git's stderr is the whole story.
            Ok(()) if !status.success() => Err(git_failed()),
            Ok(()) => Ok(()),
        }
    }
}

/// Why streaming stopped: the sink refused bytes, or git broke its own protocol. Kept apart
/// because the caller reports them differently once the child's exit status is known.
enum StreamError {
    Sink(CaptureError),
    Protocol(CaptureError),
}

/// `<oid> SP <type> SP <size>\n<content>\n`, repeated — or `<oid> SP missing\n`, which is a
/// refusal, not a skip.
fn parse_batch_stream(
    reader: &mut impl BufRead,
    sink: &mut ObjectSink<'_>,
) -> Result<(), StreamError> {
    let io = |e: std::io::Error| StreamError::Protocol(CaptureError::Io(e));
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header).map_err(io)? == 0 {
            return Ok(());
        }
        let fields: Vec<&str> = header.split_whitespace().collect();
        match fields.as_slice() {
            [oid, "missing"] => {
                return Err(StreamError::Protocol(CaptureError::ObjectUnproducible {
                    oid: (*oid).to_string(),
                    detail: "git reports it missing".to_string(),
                }));
            }
            [oid, _kind, size] => {
                let size: usize = size.parse().map_err(|_| {
                    StreamError::Protocol(CaptureError::Batch {
                        detail: format!("unparseable size in header {:?}", header.trim_end()),
                    })
                })?;
                let mut bytes = vec![0u8; size];
                reader.read_exact(&mut bytes).map_err(io)?;
                let mut newline = [0u8; 1];
                reader.read_exact(&mut newline).map_err(io)?;
                sink(oid, &bytes).map_err(StreamError::Sink)?;
            }
            _ => {
                return Err(StreamError::Protocol(CaptureError::Batch {
                    detail: format!("unparseable header {:?}", header.trim_end()),
                }));
            }
        }
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Whether a capture left the checkout as it found it. Used by tests, and worth having in the
/// API: "read-only" is a claim that should be checkable, not a comment.
///
/// Built from the index listing, HEAD, and a direct filesystem walk — deliberately **not** from
/// `git status`, which hashes worktree files and would therefore run the candidate's own clean
/// filter. Checking that nothing ran must not itself run something.
pub fn worktree_state(repo: &Repo) -> Result<String, CaptureError> {
    let mut hasher = Sha256::new();
    hasher.update(repo.bytes(&["ls-files", "-s", "-z"])?);
    hasher.update(repo.bytes(&["ls-files", "-z", "--others", "--exclude-standard"])?);
    hasher.update(repo.line(&["rev-parse", "HEAD"])?.as_bytes());

    // File bytes and modes, read directly. A change git has not noticed yet still shows up.
    let mut paths: Vec<String> = Vec::new();
    for args in [
        vec!["ls-files", "-z", "--cached"],
        vec!["ls-files", "-z", "--others", "--exclude-standard"],
    ] {
        for record in split_nul(&repo.bytes(&args)?) {
            paths.push(encode_path(record));
        }
    }
    paths.sort();
    paths.dedup();
    for path in paths {
        hasher.update(path.as_bytes());
        let full = repo.workdir().join(crate::manifest::fs_path(&path));
        match std::fs::symlink_metadata(&full) {
            Err(_) => hasher.update(b"<absent>"),
            Ok(meta) if meta.file_type().is_symlink() => {
                hasher.update(std::fs::read_link(&full)?.to_string_lossy().as_bytes());
            }
            Ok(meta) => {
                hasher.update(std::fs::read(&full)?);
                hasher.update([u8::from(is_executable(&meta))]);
            }
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}
