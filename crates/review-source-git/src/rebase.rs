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
    Entry, EntryKind, Manifest, digest_bytes, digest_reader_with_buffer, encode_path,
};
use crate::materialize::{
    MaterializeError, checked_relative_path, materialize, refuse_symlink_ancestors,
};

/// The entries that differ between two manifests, matched by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ManifestChanges {
    /// Entries of `from` that `to` no longer has, as indexes into `from.entries`.
    deleted: Vec<usize>,
    /// Entries present in both with a different kind, content or size, as indexes into
    /// `to.entries`.
    modified: Vec<usize>,
    /// Entries `from` did not have, as indexes into `to.entries`.
    added: Vec<usize>,
}

impl ManifestChanges {
    /// Distinct paths written or removed when the changes are applied.
    fn touched(&self) -> u64 {
        (self.deleted.len() + self.modified.len() + self.added.len()) as u64
    }
}

/// The difference between two manifests. Both spell every path canonically, so equal paths are
/// equal raw bytes.
fn manifest_changes(from: &Manifest, to: &Manifest) -> ManifestChanges {
    let mut previous: BTreeMap<&str, usize> = from
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.path.as_str(), index))
        .collect();
    let mut changes = ManifestChanges::default();
    for (index, entry) in to.entries.iter().enumerate() {
        match previous.remove(entry.path.as_str()) {
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
/// Removals never follow a symlink: every parent component is opened `O_NOFOLLOW` and must be a
/// real directory, so a tree that drifted under its manifest cannot make the rebase reach
/// outside `root`. Nothing here verifies the result. A caller scans the tree beforehand, so the
/// tree holds `from`, and afterwards, comparing its content digest with the head's Tree Digest;
/// any disagreement means the tree is discarded.
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
            .map(|entry| checked_relative_path(&entry.path))
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
        .map(|index| &from.entries[*index])
        .chain(changes.modified.iter().map(|index| &to.entries[*index]));
    let mut parents: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in removals {
        let relative = checked_relative_path(&entry.path)?;
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
        prune_directory(root, directory);
    }
    let written: Vec<Entry> = changes
        .modified
        .iter()
        .chain(changes.added.iter())
        .map(|index| to.entries[*index].clone())
        .collect();
    if !written.is_empty() {
        let subset = Manifest::new(written)
            .map_err(|error| MaterializeError::Manifest(error.to_string()))?;
        materialize(&subset, cas, root)?;
    }
    Ok(changes.touched())
}

/// Unlink one entry of the tree being re-based without following any symlink on the way: the
/// root and every parent component are opened descriptor-relative with `O_NOFOLLOW` and must
/// be real directories, and the entry itself is unlinked relative to the last of them. A parent
/// that is a symlink, wherever it points, refuses the rebase instead of reaching through it.
fn remove_entry(root: &Path, relative: &Path, encoded: &str) -> Result<(), MaterializeError> {
    use nix::fcntl::AtFlags;
    use nix::sys::stat::{SFlag, fstatat};
    use nix::unistd::{UnlinkatFlags, unlinkat};

    let (directory, name) = open_parent_no_follow(root, relative, encoded)?;
    match fstatat(&directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) if SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR => {
            Err(MaterializeError::Manifest(format!(
                "entry `{encoded}` is a directory in the tree being re-based"
            )))
        }
        Ok(_) => unlinkat(&directory, name, UnlinkatFlags::NoRemoveDir)
            .map_err(|error| MaterializeError::Io(std::io::Error::from(error))),
        // The previous manifest names a path the tree lacks. The verification scan decides
        // whether what remains still equals the head.
        Err(nix::errno::Errno::ENOENT) => Ok(()),
        Err(error) => Err(MaterializeError::Io(std::io::Error::from(error))),
    }
}

/// Remove an emptied directory of the tree being re-based, relative to its no-follow parent.
/// Best effort: a directory that still holds entries, or a parent that is no longer a real
/// directory, leaves it in place for the verification scan to judge.
fn prune_directory(root: &Path, relative: &Path) {
    use nix::unistd::{UnlinkatFlags, unlinkat};

    if let Ok((directory, name)) = open_parent_no_follow(root, relative, "") {
        let _ = unlinkat(&directory, name, UnlinkatFlags::RemoveDir);
    }
}

/// Open every parent component of `relative` below `root` with `O_NOFOLLOW | O_DIRECTORY`,
/// returning the last directory and the entry's own name.
fn open_parent_no_follow<'a>(
    root: &Path,
    relative: &'a Path,
    encoded: &str,
) -> Result<(std::os::fd::OwnedFd, &'a std::ffi::OsStr), MaterializeError> {
    use nix::fcntl::{OFlag, open, openat};
    use nix::sys::stat::Mode;

    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY;
    let refused = |error: nix::errno::Errno| match error {
        nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR => MaterializeError::Manifest(
            format!("entry `{encoded}` lies below a symlink in the tree being re-based"),
        ),
        other => MaterializeError::Io(std::io::Error::from(other)),
    };
    let name = relative
        .file_name()
        .ok_or_else(|| MaterializeError::Manifest(format!("entry `{encoded}` has no name")))?;
    let mut directory = open(root, flags, Mode::empty()).map_err(refused)?;
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            directory =
                openat(&directory, component.as_os_str(), flags, Mode::empty()).map_err(refused)?;
        }
    }
    Ok((directory, name))
}

/// Read the tree at `root` back into a manifest, hashing every regular file and symlink target.
/// Directories contribute nothing: an empty directory is invisible to a manifest exactly as it is
/// to a capture. Anything that is neither a regular file, a directory nor a symlink is an error.
pub fn scan_tree(root: impl AsRef<Path>) -> Result<Manifest, std::io::Error> {
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
                path: encode_path(&path_bytes(relative)),
                kind,
                content,
                size,
            })
        },
    )?;
    Manifest::new(entries)
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

fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
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

        let scanned = scan_tree(&rebased).unwrap();
        assert_eq!(scanned, to);
        assert_eq!(scanned.content_digest(), to.content_digest());
        assert_eq!(scan_tree(&fresh).unwrap(), to);
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
        assert_eq!(manifest_changes(&from, &from), ManifestChanges::default());
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
            scan_tree(&root).unwrap().content_digest(),
            to.content_digest()
        );
        fs::write(root.join("new/stray.o"), b"left behind").unwrap();
        assert_ne!(
            scan_tree(&root).unwrap().content_digest(),
            to.content_digest()
        );
    }

    #[test]
    fn a_drifted_symlinked_parent_cannot_delete_outside_the_tree() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let from = Manifest::new(vec![
            entry("dir/victim", EntryKind::File, &cas, b"inside\n"),
            entry("keep.rs", EntryKind::File, &cas, b"keep\n"),
        ])
        .unwrap();
        let to = Manifest::new(vec![entry("keep.rs", EntryKind::File, &cas, b"keep\n")]).unwrap();
        let root = directory.path().join("tree");
        materialize(&from, &cas, &root).unwrap();
        // The tree drifted: `dir` is now a symlink to a directory outside the tree that holds
        // a file of the same name.
        let outside = directory.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("victim"), b"external\n").unwrap();
        fs::remove_dir_all(root.join("dir")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("dir")).unwrap();

        let error = apply_tree_diff(&from, &to, &cas, &root).unwrap_err();
        assert!(
            error.to_string().contains("below a symlink"),
            "the rebase refuses to reach through the symlink: {error}"
        );
        assert_eq!(
            fs::read(outside.join("victim")).unwrap(),
            b"external\n",
            "nothing outside the tree was touched"
        );
        assert!(
            fs::symlink_metadata(root.join("dir"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the drifted entry is left for the verification scan to judge"
        );
    }

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
