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

use crate::{Mode, Sandbox};

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
    _dir: tempfile::TempDir,
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

pub(crate) fn seal(sandbox: Sandbox) -> Result<SealedSandbox, std::io::Error> {
    let (root, baseline, mode, dir) = sandbox.into_parts();
    let (final_manifest, mutations) = scan_and_diff(&root, &baseline)?;
    Ok(SealedSandbox {
        root,
        mode,
        baseline,
        final_manifest,
        mutations,
        _dir: dir,
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
    let index: BTreeMap<&str, &Entry> = baseline
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e))
        .collect();

    let mut entries = Vec::new();
    let mut mutations = MutationSet::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
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
                    // Present in the baseline: read and hash, the only way to detect a change.
                    let bytes = if kind == EntryKind::Symlink {
                        std::fs::read_link(&path)?
                            .to_string_lossy()
                            .into_owned()
                            .into_bytes()
                    } else {
                        std::fs::read(&path)?
                    };
                    let content = digest_bytes(&bytes);
                    // Kind is part of identity: a file replaced by a symlink to the same bytes
                    // is a change, and a reviewer that made one has not left the tree alone.
                    if previous.content != content || previous.kind != kind {
                        mutations.modified.push(relative.clone());
                    }
                    entries.push(Entry {
                        path: relative,
                        kind,
                        content,
                        size: bytes.len() as u64,
                    });
                }
            }
        }
    }
    for path in index.keys() {
        if !seen.contains(*path) {
            mutations.deleted.push((*path).to_string());
        }
    }
    mutations.added.sort();
    mutations.modified.sort();
    mutations.deleted.sort();
    Ok((Manifest::new(entries), mutations))
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
