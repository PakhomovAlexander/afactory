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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use review_source_git::{
    Entry, EntryKind, Manifest, digest_bytes, digest_reader_with_buffer, fs_path,
};
use review_store::Cas;

use crate::{Sandbox, ensure_directory_mode, restore_writable_dirs};

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
    pub baseline: Arc<Manifest>,
    /// The tree as it stood at seal time.
    pub final_manifest: Manifest,
    pub mutations: MutationSet,
    _cleanup: CleanupDir,
}

impl SealedSandbox {
    /// Whether the node left the sandbox as it found it. A read-only node that mutated anything
    /// is a contract violation by the node, and worth surfacing rather than tolerating.
    pub fn unchanged(&self) -> bool {
        self.mutations.is_empty()
    }

    /// Publish every byte in the sealed tree and return its complete materializable Manifest.
    ///
    /// Ordinary review sealing deliberately does not hash added build output. An implement Task
    /// has a different boundary: its derived Snapshot is the output, so every entry must name a
    /// verified CAS object before the Snapshot can be recorded.
    pub fn capture_snapshot(&self, cas: &Cas) -> Result<Manifest, std::io::Error> {
        let mut entries = Vec::with_capacity(self.final_manifest.entries.len());
        let mut buffer = vec![0_u8; 64 * 1024];
        for expected in &self.final_manifest.entries {
            let path = self.root.join(fs_path(&expected.path));
            let metadata = std::fs::symlink_metadata(&path)?;
            let kind = if metadata.file_type().is_symlink() {
                EntryKind::Symlink
            } else if is_executable(&metadata) {
                EntryKind::Executable
            } else {
                EntryKind::File
            };
            if kind != expected.kind {
                return Err(std::io::Error::other(format!(
                    "sealed path {} changed kind during Snapshot capture",
                    expected.path
                )));
            }
            let (content, size) = if kind == EntryKind::Symlink {
                let target = std::fs::read_link(&path)?;
                let bytes = path_bytes(&target);
                (
                    cas.put(bytes).map_err(std::io::Error::other)?,
                    bytes.len() as u64,
                )
            } else {
                cas.put_reader_with_buffer(&mut std::fs::File::open(&path)?, &mut buffer)
                    .map_err(std::io::Error::other)?
            };
            entries.push(Entry {
                path: expected.path.clone(),
                kind,
                content,
                size,
            });
        }
        Manifest::new_with_encoding(entries, self.final_manifest.path_encoding)
            .map_err(std::io::Error::other)
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
        // into the tree concurrently and any per-path removal failure without losing cleanup.
        let _ = std::fs::remove_dir_all(&temp_root);
    }
}

pub(crate) fn seal(sandbox: Sandbox) -> Result<SealedSandbox, std::io::Error> {
    let (root, baseline, dir) = sandbox.into_parts();
    let (final_manifest, mutations) = match scan_and_diff(&root, baseline.as_ref()) {
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
    let mut entries = Vec::new();
    let mut mutations = MutationSet::default();
    let mut matched = vec![false; baseline.entries.len()];
    let mut level = vec![root.to_path_buf()];
    let mut pending_candidates = Vec::new();
    while !level.is_empty() {
        let (scanned, hashed) = review_parallel::try_join(
            || {
                review_parallel::try_map_owned(level, |directory| {
                    scan_directory(root, baseline, directory)
                })
            },
            || hash_baseline_candidates(pending_candidates),
        )?;
        append_hashed_candidates(hashed, &mut entries, &mut mutations);

        let mut next_level = Vec::new();
        let mut next_candidates = Vec::new();
        for scan in scanned {
            next_level.extend(scan.directories);
            mutations.added.extend(scan.added_paths);
            entries.extend(scan.added_entries);
            for (position, candidate) in scan.baseline_candidates {
                matched[position] = true;
                next_candidates.push(candidate);
            }
        }
        level = next_level;
        pending_candidates = next_candidates;
    }
    let hashed = hash_baseline_candidates(pending_candidates)?;
    append_hashed_candidates(hashed, &mut entries, &mut mutations);
    for (entry, was_matched) in baseline.entries.iter().zip(matched) {
        if !was_matched {
            mutations.deleted.push(entry.path.clone());
        }
    }
    mutations.added.sort();
    mutations.modified.sort();
    mutations.deleted.sort();
    let manifest = Manifest::new_with_encoding(entries, baseline.path_encoding)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok((manifest, mutations))
}

struct DirectoryScan<'a> {
    directories: Vec<PathBuf>,
    added_entries: Vec<Entry>,
    added_paths: Vec<String>,
    baseline_candidates: Vec<(usize, BaselineCandidate<'a>)>,
}

fn scan_directory<'a>(
    root: &Path,
    baseline: &'a Manifest,
    directory: PathBuf,
) -> Result<DirectoryScan<'a>, std::io::Error> {
    ensure_directory_mode(&directory, 0o500)?;
    let paths = std::fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    let classified = review_parallel::try_map_owned(paths, |path| {
        classify_directory_entry(root, baseline, path)
    })?;
    let mut scan = DirectoryScan {
        directories: Vec::new(),
        added_entries: Vec::new(),
        added_paths: Vec::new(),
        baseline_candidates: Vec::new(),
    };
    for entry in classified {
        match entry {
            ClassifiedEntry::Directory(path) => scan.directories.push(path),
            ClassifiedEntry::Added(entry) => {
                scan.added_paths.push(entry.path.clone());
                scan.added_entries.push(entry);
            }
            ClassifiedEntry::Baseline(position, candidate) => {
                scan.baseline_candidates.push((position, candidate))
            }
        }
    }
    Ok(scan)
}

enum ClassifiedEntry<'a> {
    Directory(PathBuf),
    Added(Entry),
    Baseline(usize, BaselineCandidate<'a>),
}

fn classify_directory_entry<'a>(
    root: &Path,
    baseline: &'a Manifest,
    path: PathBuf,
) -> Result<ClassifiedEntry<'a>, std::io::Error> {
    // Seal is a containment boundary: never follow a path that could have become a symlink
    // between directory enumeration and metadata lookup.
    let meta = std::fs::symlink_metadata(&path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        return Ok(ClassifiedEntry::Directory(path));
    }
    let relative_path = path.strip_prefix(root).expect("walked path is under root");
    let relative = baseline.encode_key(path_bytes(relative_path));
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else if is_executable(&meta) {
        EntryKind::Executable
    } else {
        EntryKind::File
    };
    match baseline
        .entries
        .binary_search_by(|entry| entry.path.as_str().cmp(&relative))
    {
        Err(_) => Ok(ClassifiedEntry::Added(Entry {
            path: relative,
            kind,
            content: String::new(),
            size: meta.len(),
        })),
        Ok(position) => Ok(ClassifiedEntry::Baseline(
            position,
            BaselineCandidate {
                path,
                relative,
                kind,
                previous: &baseline.entries[position],
            },
        )),
    }
}

fn append_hashed_candidates(
    candidates: Vec<(Entry, bool)>,
    entries: &mut Vec<Entry>,
    mutations: &mut MutationSet,
) {
    for (entry, modified) in candidates {
        if modified {
            mutations.modified.push(entry.path.clone());
        }
        entries.push(entry);
    }
}

fn remove_tree_parallel(root: &Path) {
    let mut level = vec![root.to_path_buf()];
    let mut directories = vec![root.to_path_buf()];
    let mut pending_files = Vec::new();
    while !level.is_empty() {
        let joined = review_parallel::try_join(
            || review_parallel::try_map_owned(level, scan_removal_directory),
            || remove_file_batch(pending_files),
        );
        let Ok((scanned, ())) = joined else {
            return;
        };
        let mut next_level = Vec::new();
        let mut next_files = Vec::new();
        for scan in scanned {
            directories.extend(scan.directories.iter().cloned());
            next_level.extend(scan.directories);
            next_files.extend(scan.files);
        }
        level = next_level;
        pending_files = next_files;
    }
    let _ = remove_file_batch(pending_files);
    // The level walk records every child after its parent, so reverse is deepest-first.
    for directory in directories.into_iter().rev() {
        let _ = std::fs::remove_dir(directory);
    }
}

struct RemovalScan {
    directories: Vec<PathBuf>,
    files: Vec<PathBuf>,
}

fn scan_removal_directory(directory: PathBuf) -> Result<RemovalScan, ()> {
    let _ = ensure_directory_mode(&directory, 0o700);
    let mut scan = RemovalScan {
        directories: Vec::new(),
        files: Vec::new(),
    };
    if let Ok(entries) = std::fs::read_dir(&directory) {
        let paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        if let Ok(classified) = review_parallel::try_map_owned(paths, |path| {
            let is_directory = std::fs::symlink_metadata(&path)
                .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
                .unwrap_or(false);
            Ok::<_, ()>((path, is_directory))
        }) {
            for (path, is_directory) in classified {
                if is_directory {
                    scan.directories.push(path);
                } else {
                    scan.files.push(path);
                }
            }
        }
    }
    Ok(scan)
}

fn remove_file_batch(files: Vec<PathBuf>) -> Result<(), ()> {
    review_parallel::try_for_each_owned(files, |path| {
        let _ = std::fs::remove_file(path);
        Ok::<_, ()>(())
    })
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
