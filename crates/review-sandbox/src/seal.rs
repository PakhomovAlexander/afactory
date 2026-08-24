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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use review_source_git::{
    Entry, EntryKind, Manifest, digest_bytes, digest_reader_with_buffer, encode_path,
};

use crate::{Mode, Sandbox, restore_writable_dirs};

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
    dir: Option<tempfile::TempDir>,
}

impl Drop for CleanupDir {
    fn drop(&mut self) {
        let Some(dir) = self.dir.take() else { return };
        let temp_root = dir.keep();
        remove_tree_parallel(&temp_root);
        // The drop-time walk is an optimization. This fallback handles a caller that writes
        // through `root()` concurrently and any per-path removal failure without losing cleanup.
        let _ = std::fs::remove_dir_all(&temp_root);
    }
}

pub(crate) fn seal(sandbox: Sandbox) -> Result<SealedSandbox, std::io::Error> {
    let (root, baseline, mode, dir) = sandbox.into_parts();
    let (final_manifest, mutations) = match scan_and_diff(&root, &baseline) {
        Ok(result) => result,
        Err(error) => {
            // A mutable reviewer may remove directory permissions. Restore best-effort before
            // returning so TempDir cleanup still has a chance to remove the hostile tree.
            restore_writable_dirs(&root);
            return Err(error);
        }
    };
    Ok(SealedSandbox {
        root,
        mode,
        baseline,
        final_manifest,
        mutations,
        _cleanup: CleanupDir { dir: Some(dir) },
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
) -> Result<(Manifest, MutationSet), std::io::Error> {
    baseline
        .validate()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    const TASKS_PER_WORKER: usize = 64;
    let index: BTreeMap<&str, (usize, &Entry)> = baseline
        .entries
        .iter()
        .enumerate()
        .map(|(position, entry)| (entry.path.as_str(), (position, entry)))
        .collect();

    let mut entries = Vec::new();
    let mut mutations = MutationSet::default();
    let mut matched = vec![false; baseline.entries.len()];
    let mut baseline_candidates = Vec::new();

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        restore_directory_for_scan(&dir)?;
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() && !meta.file_type().is_symlink() {
                stack.push(path);
                continue;
            }
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
                Some((position, previous)) => {
                    matched[*position] = true;
                    baseline_candidates.push(BaselineCandidate {
                        path,
                        relative,
                        kind,
                        previous,
                    });
                    if baseline_candidates.len()
                        >= review_parallel::worker_limit() * TASKS_PER_WORKER
                    {
                        append_baseline_candidates(
                            std::mem::take(&mut baseline_candidates),
                            &mut entries,
                            &mut mutations,
                        )?;
                    }
                }
            }
        }
    }
    append_baseline_candidates(baseline_candidates, &mut entries, &mut mutations)?;
    for (entry, was_matched) in baseline.entries.iter().zip(matched) {
        if !was_matched {
            mutations.deleted.push(entry.path.clone());
        }
    }
    mutations.added.sort();
    mutations.modified.sort();
    mutations.deleted.sort();
    Ok((Manifest::new(entries), mutations))
}

fn append_baseline_candidates(
    candidates: Vec<BaselineCandidate<'_>>,
    entries: &mut Vec<Entry>,
    mutations: &mut MutationSet,
) -> Result<(), std::io::Error> {
    for (entry, modified) in hash_baseline_candidates(candidates)? {
        if modified {
            mutations.modified.push(entry.path.clone());
        }
        entries.push(entry);
    }
    Ok(())
}

fn remove_tree_parallel(root: &Path) {
    const TASKS_PER_WORKER: usize = 64;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        restore_directory_for_cleanup(&directory);
        directories.push(directory.clone());
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry
                .file_type()
                .map(|kind| kind.is_dir() && !kind.is_symlink())
                .unwrap_or(false)
            {
                stack.push(path);
            } else {
                files.push(path);
                if files.len() >= review_parallel::worker_limit() * TASKS_PER_WORKER {
                    remove_file_batch(std::mem::take(&mut files));
                }
            }
        }
    }
    remove_file_batch(files);
    // The DFS records every child after its parent, so reverse order is already deepest-first.
    for directory in directories.into_iter().rev() {
        let _ = std::fs::remove_dir(directory);
    }
}

#[cfg(unix)]
fn restore_directory_for_cleanup(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(not(unix))]
fn restore_directory_for_cleanup(_path: &Path) {}

fn remove_file_batch(files: Vec<PathBuf>) {
    let _ = review_parallel::try_for_each_owned(files, |path| {
        let _ = std::fs::remove_file(path);
        Ok::<_, ()>(())
    });
}

#[cfg(unix)]
fn restore_directory_for_scan(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::symlink_metadata(path)?.permissions().mode();
    if mode & 0o500 != 0o500 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

#[cfg(not(unix))]
fn restore_directory_for_scan(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

struct BaselineCandidate<'a> {
    path: PathBuf,
    relative: String,
    kind: EntryKind,
    previous: &'a Entry,
}

fn hash_baseline_candidates(
    candidates: Vec<BaselineCandidate<'_>>,
) -> Result<Vec<(Entry, bool)>, std::io::Error> {
    review_parallel::try_map_owned_with(
        candidates,
        || vec![0u8; 64 * 1024],
        |buffer, candidate| {
            let (content, size) = if candidate.kind == EntryKind::Symlink {
                let target = std::fs::read_link(&candidate.path)?;
                let bytes = path_bytes(&target);
                (digest_bytes(bytes), bytes.len() as u64)
            } else {
                digest_reader_with_buffer(
                    std::fs::File::open(&candidate.path)?,
                    buffer.as_mut_slice(),
                )?
            };
            let modified =
                candidate.previous.content != content || candidate.previous.kind != candidate.kind;
            Ok((
                Entry {
                    path: candidate.relative,
                    kind: candidate.kind,
                    content,
                    size,
                },
                modified,
            ))
        },
    )
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
