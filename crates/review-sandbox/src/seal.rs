//! Sealing: freezing a sandbox and computing what changed in it.
//!
//! A reviewer's own account of what it edited is a claim. The mutation set here is a *derivation*
//! — the sandbox's tree is rescanned and diffed against the manifest it was materialized from,
//! so a patch proposal can be checked against what actually happened rather than against what
//! was reported.
//!
//! This is what makes the design's rule enforceable: an auto-appliable patch must equal the
//! kernel-computed final sandbox diff byte for byte, and diagnostic mutations must be reverted
//! before completion. Neither is checkable without computing the diff independently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use review_source_git::{Entry, EntryKind, Manifest, digest_bytes, encode_path};
use review_store::canonical::blob_content_id_reader_with_buffer;

use crate::{Mode, Sandbox, restore_known_dirs, restore_writable_dirs};

/// What a node changed in its sandbox, relative to the snapshot it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationSet {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
}

impl MutationSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    /// Every path touched, in one sorted list — the declared path set of a patch proposal must
    /// equal this exactly.
    pub fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .added
            .iter()
            .chain(&self.modified)
            .chain(&self.deleted)
            .cloned()
            .collect();
        paths.sort();
        paths
    }
}

/// A sandbox after it has been frozen. There is no way back to a writable handle.
pub struct SealedSandbox {
    root: PathBuf,
    pub mode: Mode,
    pub baseline: Manifest,
    /// The tree as it stood at seal time.
    pub final_manifest: Manifest,
    pub mutations: MutationSet,
    _cleanup: CleanupDir,
}

impl SealedSandbox {
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the node left the sandbox as it found it. A read-only node that mutated anything
    /// is a contract violation by the node, and worth surfacing rather than tolerating.
    pub fn unchanged(&self) -> bool {
        self.mutations.is_empty()
    }
}

struct CleanupDir {
    plan: CleanupPlan,
    dir: Option<tempfile::TempDir>,
}

impl Drop for CleanupDir {
    fn drop(&mut self) {
        let Some(dir) = self.dir.take() else { return };
        let temp_root = dir.keep();
        remove_tree_parallel(&self.plan);
        // The recorded plan is exact at seal time. This fallback handles a caller that writes
        // through `root()` afterwards and any per-path removal failure without losing cleanup.
        let _ = std::fs::remove_dir_all(temp_root);
    }
}

pub(crate) fn seal(sandbox: Sandbox) -> Result<SealedSandbox, std::io::Error> {
    let (root, baseline, mode, dir) = sandbox.into_parts();
    let (final_manifest, mutations, directories, cleanup) =
        match scan_and_diff(&root, &baseline, mode, review_parallel::worker_limit()) {
            Ok(result) => result,
            Err(error) => {
                // A mutable reviewer may remove directory permissions. Restore best-effort before
                // returning so TempDir cleanup still has a chance to remove the hostile tree.
                restore_writable_dirs(&root);
                return Err(error);
            }
        };
    restore_known_dirs(&directories);
    Ok(SealedSandbox {
        root,
        mode,
        baseline,
        final_manifest,
        mutations,
        _cleanup: CleanupDir {
            plan: cleanup,
            dir: Some(dir),
        },
    })
}

/// Walk the sealed tree and diff it against the baseline in one pass — reading and hashing
/// **only** the paths the baseline also has.
///
/// The point of the single pass: a file absent from the baseline is `added` whatever its bytes
/// are, so hashing it cannot change the answer. A reviewer that verified a claim by building
/// leaves a whole `target/` behind — on this workspace ~10k files and 1 GiB — and a plain
/// scan spent seconds SHA-256-ing exactly that population for nothing. Baseline-present paths
/// are still read and hashed, because that is the only way to tell a modification from an
/// untouched file.
fn scan_and_diff(
    root: &Path,
    baseline: &Manifest,
    mode: Mode,
    worker_budget: usize,
) -> Result<(Manifest, MutationSet, Vec<PathBuf>, CleanupPlan), std::io::Error> {
    let index: BTreeMap<&str, &Entry> = baseline
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e))
        .collect();

    let mut entries = Vec::new();
    let mut mutations = MutationSet::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut baseline_candidates = Vec::new();
    let mut directories = Vec::new();
    let mut cleanup = CleanupPlan::default();

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        cleanup.directories.push(dir.clone());
        if mode == Mode::ReadOnly || directory_needs_restore(&dir)? {
            directories.push(dir.clone());
        }
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() && !meta.file_type().is_symlink() {
                stack.push(path);
                continue;
            }
            cleanup.files.push(path.clone());
            // The manifest key must be capture's *encoding* of the raw path bytes, not
            // `to_string_lossy`, which collapses two distinct non-UTF-8 names to one key.
            let relative_path = path.strip_prefix(root).expect("walked path is under root");
            let relative = encode_path(path_bytes(relative_path));
            let kind = if meta.file_type().is_symlink() {
                EntryKind::Symlink
            } else if is_executable(&meta) {
                EntryKind::Executable
            } else {
                EntryKind::File
            };
            seen.insert(relative.clone());

            match index.get(relative.as_str()) {
                None => {
                    // Added: presence is the whole fact. Record it with its size from the stat
                    // we already have, and no content hash — the bytes are never read.
                    mutations.added.push(relative.clone());
                    entries.push(Entry {
                        path: relative,
                        kind,
                        content: String::new(),
                        size: meta.len(),
                    });
                }
                Some(previous) => {
                    baseline_candidates.push(BaselineCandidate {
                        path,
                        relative,
                        kind,
                        previous_content: previous.content.clone(),
                        previous_kind: previous.kind,
                    });
                }
            }
        }
    }
    for (entry, modified) in hash_baseline_candidates(&baseline_candidates, worker_budget)? {
        if modified {
            mutations.modified.push(entry.path.clone());
        }
        entries.push(entry);
    }
    for path in index.keys() {
        if !seen.contains(*path) {
            mutations.deleted.push((*path).to_string());
        }
    }
    mutations.added.sort();
    mutations.modified.sort();
    mutations.deleted.sort();
    Ok((Manifest::new(entries), mutations, directories, cleanup))
}

#[derive(Default)]
struct CleanupPlan {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

fn remove_tree_parallel(plan: &CleanupPlan) {
    let workers = review_parallel::worker_limit().min(plan.files.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(path) = plan.files.get(index) else {
                        return;
                    };
                    let _permit = review_parallel::acquire_worker_permit();
                    let _ = std::fs::remove_file(path);
                }
            });
        }
    });
    let mut directories: Vec<&PathBuf> = plan.directories.iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        let _ = std::fs::remove_dir(directory);
    }
}

#[cfg(unix)]
fn directory_needs_restore(path: &Path) -> Result<bool, std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::symlink_metadata(path)?.permissions().mode() & 0o200 == 0)
}

#[cfg(not(unix))]
fn directory_needs_restore(_path: &Path) -> Result<bool, std::io::Error> {
    Ok(false)
}

struct BaselineCandidate {
    path: PathBuf,
    relative: String,
    kind: EntryKind,
    previous_content: String,
    previous_kind: EntryKind,
}

fn hash_baseline_candidates(
    candidates: &[BaselineCandidate],
    worker_budget: usize,
) -> Result<Vec<(Entry, bool)>, std::io::Error> {
    let workers = worker_budget.max(1).min(candidates.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| -> Result<Vec<(Entry, bool)>, std::io::Error> {
                    let mut hashed = Vec::new();
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(candidate) = candidates.get(index) else {
                            return Ok(hashed);
                        };
                        let _permit = review_parallel::acquire_worker_permit();
                        let (content, size) = if candidate.kind == EntryKind::Symlink {
                            let target = std::fs::read_link(&candidate.path)?;
                            let bytes = path_bytes(&target);
                            (digest_bytes(bytes), bytes.len() as u64)
                        } else {
                            blob_content_id_reader_with_buffer(
                                std::fs::File::open(&candidate.path)?,
                                &mut buffer,
                            )?
                        };
                        let modified = candidate.previous_content != content
                            || candidate.previous_kind != candidate.kind;
                        hashed.push((
                            Entry {
                                path: candidate.relative.clone(),
                                kind: candidate.kind,
                                content,
                                size,
                            },
                            modified,
                        ));
                    }
                })
            })
            .collect();
        let mut hashed = Vec::with_capacity(candidates.len());
        for handle in handles {
            hashed.extend(handle.join().expect("sandbox hash worker")?);
        }
        Ok(hashed)
    })
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

/// The raw bytes of a path, for lossless encoding. Unix: the OS bytes; elsewhere, a best-effort
/// UTF-8 view (the byte-exact model does not apply off-unix).
#[cfg(unix)]
fn path_bytes(path: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().to_str().map(str::as_bytes).unwrap_or(b"")
}
