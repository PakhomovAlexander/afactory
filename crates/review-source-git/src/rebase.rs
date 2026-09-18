//! Re-basing a materialized tree from one manifest to the next (Worker warm layers, P3).
//!
//! A Warm Workspace keeps one template tree per node across Rounds. When the head advances, the
//! kernel applies the manifest-level difference to that tree instead of materializing the whole
//! head again: entries the new head no longer has are removed, entries it changed or added are
//! written from the CAS, and emptied directories are pruned. The apply step is deliberately
//! simple and the verification deliberately complete: [`scan_tree`] reads the result back into
//! a manifest whose content digest must equal the head's Tree Digest, and a rebase that cannot
//! be proven equal to a fresh materialization is discarded, never trusted.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use review_store::Cas;

use crate::manifest::{
    Entry, EntryKind, Manifest, PathEncoding, decode_path, digest_bytes, digest_reader_with_buffer,
    encode_path_for,
};
use crate::materialize::{
    MaterializeError, checked_relative_path, materialize, refuse_symlink_ancestors,
};

/// The entries that differ between two manifests. Paths are compared by their decoded bytes,
/// so two manifests with different path spellings describe the same tree the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestChanges {
    /// Entries of `from` that `to` no longer has, as indexes into `from.entries`.
    pub deleted: Vec<usize>,
    /// Entries present in both with a different kind, content or size, as indexes into
    /// `to.entries`.
    pub modified: Vec<usize>,
    /// Entries `from` did not have, as indexes into `to.entries`.
    pub added: Vec<usize>,
}

impl ManifestChanges {
    pub fn is_empty(&self) -> bool {
        self.deleted.is_empty() && self.modified.is_empty() && self.added.is_empty()
    }

    /// Distinct paths written or removed when the changes are applied.
    pub fn touched(&self) -> u64 {
        (self.deleted.len() + self.modified.len() + self.added.len()) as u64
    }
}

/// The difference between two manifests, by decoded path bytes.
pub fn manifest_changes(from: &Manifest, to: &Manifest) -> ManifestChanges {
    let mut previous: BTreeMap<Vec<u8>, usize> = from
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (decode_path(&entry.path), index))
        .collect();
    let mut changes = ManifestChanges::default();
    for (index, entry) in to.entries.iter().enumerate() {
        match previous.remove(&decode_path(&entry.path)) {
            None => changes.added.push(index),
            Some(before) => {
                let before = &from.entries[before];
                if before.kind != entry.kind
                    || before.content != entry.content
                    || before.size != entry.size
                {
                    changes.modified.push(index);
                }
            }
        }
    }
    changes.deleted = previous.into_values().collect();
    changes.deleted.sort_unstable();
    changes
}

/// Apply the difference between `from` and `to` to the tree at `root`, which must currently hold
/// `from`. Removed and modified entries are unlinked first, emptied directories pruned, then
/// modified and added entries are written from the CAS exactly as a materialization writes them.
/// Returns the number of distinct paths written or removed.
///
/// Nothing here verifies the result. A caller scans the tree afterwards and compares its content
/// digest with the head's Tree Digest; any disagreement means the tree is discarded.
pub fn apply_tree_diff(
    from: &Manifest,
    to: &Manifest,
    cas: &Cas,
    root: impl AsRef<Path>,
) -> Result<u64, MaterializeError> {
    let root = root.as_ref();
    for manifest in [from, to] {
        manifest
            .validate()
            .map_err(|error| MaterializeError::Manifest(error.to_string()))?;
        let decoded: Vec<PathBuf> = manifest
            .entries
            .iter()
            .map(|entry| checked_relative_path(&entry.path, manifest.path_encoding))
            .collect::<Result<_, _>>()?;
        refuse_symlink_ancestors(manifest, &decoded)?;
    }
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MaterializeError::Escape {
            path: root.display().to_string(),
        });
    }
    let changes = manifest_changes(from, to);
    let removals = changes
        .deleted
        .iter()
        .map(|index| (&from.entries[*index], from.path_encoding))
        .chain(
            changes
                .modified
                .iter()
                .map(|index| (&to.entries[*index], to.path_encoding)),
        );
    let mut parents: BTreeSet<PathBuf> = BTreeSet::new();
    for (entry, encoding) in removals {
        let relative = checked_relative_path(&entry.path, encoding)?;
        remove_entry(root, &relative, &entry.path)?;
        let mut ancestor = relative.parent();
        while let Some(directory) = ancestor {
            if directory.as_os_str().is_empty() {
                break;
            }
            parents.insert(directory.to_path_buf());
            ancestor = directory.parent();
        }
    }
    // Deepest first: an emptied directory goes only after everything below it did. A directory
    // that still holds entries refuses removal and stays; one the head needs again is recreated
    // by the materialization below.
    for directory in parents.iter().rev() {
        let _ = fs::remove_dir(root.join(directory));
    }
    let written: Vec<Entry> = changes
        .modified
        .iter()
        .chain(changes.added.iter())
        .map(|index| to.entries[*index].clone())
        .collect();
    if !written.is_empty() {
        let subset = Manifest::new_with_encoding(written, to.path_encoding)
            .map_err(|error| MaterializeError::Manifest(error.to_string()))?;
        materialize(&subset, cas, root)?;
    }
    Ok(changes.touched())
}

fn remove_entry(root: &Path, relative: &Path, encoded: &str) -> Result<(), MaterializeError> {
    let target = root.join(relative);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Err(MaterializeError::Manifest(format!(
                "entry `{encoded}` is a directory in the tree being re-based"
            )))
        }
        Ok(_) => fs::remove_file(&target).map_err(MaterializeError::Io),
        // The previous manifest names a path the tree lacks. The verification scan decides
        // whether what remains still equals the head.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MaterializeError::Io(error)),
    }
}

/// Read the tree at `root` back into a manifest with the given path spelling, hashing every
/// regular file and symlink target. Directories contribute nothing: an empty directory is
/// invisible to a manifest exactly as it is to a capture. Anything that is neither a regular
/// file, a directory nor a symlink is an error.
pub fn scan_tree(
    root: impl AsRef<Path>,
    path_encoding: PathEncoding,
) -> Result<Manifest, std::io::Error> {
    let root = root.as_ref();
    let mut level = vec![root.to_path_buf()];
    let mut found: Vec<(PathBuf, EntryKind)> = Vec::new();
    while !level.is_empty() {
        let scanned =
            review_parallel::try_map_owned(level, |directory| scan_directory(&directory))?;
        let mut next = Vec::new();
        for scan in scanned {
            next.extend(scan.directories);
            found.extend(scan.files);
        }
        level = next;
    }
    let entries = review_parallel::try_map_owned_with(
        found,
        || vec![0_u8; 64 * 1024],
        |buffer, (path, kind)| {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| std::io::Error::other("scanned path left its root"))?;
            let (content, size) = if kind == EntryKind::Symlink {
                let target = path_bytes(&fs::read_link(&path)?);
                (digest_bytes(&target), target.len() as u64)
            } else {
                digest_reader_with_buffer(fs::File::open(&path)?, buffer.as_mut_slice())?
            };
            Ok::<_, std::io::Error>(Entry {
                path: encode_path_for(path_encoding, &path_bytes(relative)),
                kind,
                content,
                size,
            })
        },
    )?;
    Manifest::new_with_encoding(entries, path_encoding)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

struct DirectoryScan {
    directories: Vec<PathBuf>,
    files: Vec<(PathBuf, EntryKind)>,
}

fn scan_directory(directory: &Path) -> Result<DirectoryScan, std::io::Error> {
    let mut scan = DirectoryScan {
        directories: Vec::new(),
        files: Vec::new(),
    };
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            scan.files.push((path, EntryKind::Symlink));
        } else if metadata.is_dir() {
            scan.directories.push(path);
        } else if metadata.is_file() {
            let kind = if is_executable(&metadata) {
                EntryKind::Executable
            } else {
                EntryKind::File
            };
            scan.files.push((path, kind));
        } else {
            return Err(std::io::Error::other(format!(
                "{} is neither a regular file, a directory nor a symlink",
                path.display()
            )));
        }
    }
    Ok(scan)
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: EntryKind, cas: &Cas, bytes: &[u8]) -> Entry {
        Entry {
            path: path.into(),
            kind,
            content: cas.put(bytes).unwrap(),
            size: bytes.len() as u64,
        }
    }

    /// Every file's bytes and every symlink's target below `root`, keyed by relative path.
    fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(at).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                let key = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if metadata.file_type().is_symlink() {
                    out.insert(
                        format!("{key} -> "),
                        path_bytes(&fs::read_link(&path).unwrap()),
                    );
                } else if metadata.is_dir() {
                    walk(root, &path, out);
                } else {
                    out.insert(key, fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn fixtures(cas: &Cas) -> (Manifest, Manifest) {
        let from = Manifest::new(vec![
            entry("a.rs", EntryKind::File, cas, b"one\n"),
            entry("gone/deep/b.rs", EntryKind::File, cas, b"two\n"),
            entry("kind", EntryKind::File, cas, b"was a file\n"),
            entry("was-a-link", EntryKind::Symlink, cas, b"a.rs"),
            entry("dir-to-file/inner", EntryKind::File, cas, b"inner\n"),
            entry("file-to-dir", EntryKind::File, cas, b"flat\n"),
            entry("run.sh", EntryKind::Executable, cas, b"#!/bin/sh\n"),
            entry("same.txt", EntryKind::File, cas, b"unchanged\n"),
        ])
        .unwrap();
        let to = Manifest::new(vec![
            entry("a.rs", EntryKind::File, cas, b"uno\n"),
            entry("kind", EntryKind::Symlink, cas, b"a.rs"),
            entry("was-a-link", EntryKind::File, cas, b"now a file\n"),
            entry("dir-to-file", EntryKind::File, cas, b"now flat\n"),
            entry("file-to-dir/inner", EntryKind::File, cas, b"now nested\n"),
            entry("run.sh", EntryKind::File, cas, b"#!/bin/sh\n"),
            entry("same.txt", EntryKind::File, cas, b"unchanged\n"),
            entry("new/c.rs", EntryKind::File, cas, b"three\n"),
        ])
        .unwrap();
        (from, to)
    }

    #[test]
    fn a_rebase_reproduces_a_full_materialization_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let (from, to) = fixtures(&cas);
        let changes = manifest_changes(&from, &to);
        assert_eq!(changes.deleted.len(), 3, "{changes:?}");
        assert_eq!(changes.modified.len(), 4, "{changes:?}");
        assert_eq!(changes.added.len(), 3, "{changes:?}");

        let rebased = dir.path().join("rebased");
        materialize(&from, &cas, &rebased).unwrap();
        let touched = apply_tree_diff(&from, &to, &cas, &rebased).unwrap();
        assert_eq!(touched, 10);
        let fresh = dir.path().join("fresh");
        materialize(&to, &cas, &fresh).unwrap();

        let scanned = scan_tree(&rebased, to.path_encoding).unwrap();
        assert_eq!(scanned, to);
        assert_eq!(scanned.content_digest(), to.content_digest());
        assert_eq!(scan_tree(&fresh, to.path_encoding).unwrap(), to);
        assert_eq!(tree_bytes(&rebased), tree_bytes(&fresh));
        assert!(
            !rebased.join("gone").exists(),
            "emptied directories are pruned"
        );
    }

    #[test]
    fn an_unchanged_manifest_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let (from, _) = fixtures(&cas);
        let root = dir.path().join("tree");
        materialize(&from, &cas, &root).unwrap();
        let before = tree_bytes(&root);
        assert!(manifest_changes(&from, &from).is_empty());
        assert_eq!(apply_tree_diff(&from, &from, &cas, &root).unwrap(), 0);
        assert_eq!(tree_bytes(&root), before);
    }

    #[test]
    fn a_directory_where_the_previous_manifest_names_a_file_refuses_the_rebase() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let from = Manifest::new(vec![entry("a", EntryKind::File, &cas, b"one\n")]).unwrap();
        let to = Manifest::new(vec![entry("a", EntryKind::File, &cas, b"two\n")]).unwrap();
        let root = dir.path().join("tree");
        materialize(&from, &cas, &root).unwrap();
        fs::remove_file(root.join("a")).unwrap();
        fs::create_dir(root.join("a")).unwrap();
        let error = apply_tree_diff(&from, &to, &cas, &root).unwrap_err();
        assert!(error.to_string().contains("is a directory"), "{error}");
    }

    #[test]
    fn a_stray_file_in_the_tree_fails_verification() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let (_, to) = fixtures(&cas);
        let root = dir.path().join("tree");
        materialize(&to, &cas, &root).unwrap();
        assert_eq!(
            scan_tree(&root, to.path_encoding).unwrap().content_digest(),
            to.content_digest()
        );
        fs::write(root.join("new/stray.o"), b"left behind").unwrap();
        assert_ne!(
            scan_tree(&root, to.path_encoding).unwrap().content_digest(),
            to.content_digest()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_root_is_refused_before_anything_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let (from, to) = fixtures(&cas);
        let real = dir.path().join("real");
        materialize(&from, &cas, &real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            apply_tree_diff(&from, &to, &cas, link),
            Err(MaterializeError::Escape { .. })
        ));
        assert_eq!(fs::read(real.join("a.rs")).unwrap(), b"one\n");
    }
}
