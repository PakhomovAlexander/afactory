//! Writing a captured snapshot into a sandbox.
//!
//! Materialization is where a snapshot stops being a digest and becomes files a reviewer can
//! read — so it is also where a hostile path gets its one chance to escape. Every path is
//! resolved against the root and refused if it leaves: absolute paths, `..` components, and
//! symlinked parent directories all fail closed rather than being sanitized into something
//! plausible.
//!
//! Nothing here consults git. A materialized tree is a function of the manifest and the CAS,
//! which is what makes it reproducible on a machine that has never seen the repository.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use review_store::Cas;

use crate::manifest::{EntryKind, Manifest};

const MAX_SYMLINK_TARGET_BYTES: u64 = 16 * 1024;

#[derive(Debug)]
pub enum MaterializeError {
    Io(std::io::Error),
    Cas(String),
    Manifest(String),
    /// A path that would leave the sandbox root.
    Escape {
        path: String,
    },
}

impl std::fmt::Display for MaterializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaterializeError::Io(e) => write!(f, "materialize io: {e}"),
            MaterializeError::Cas(e) => write!(f, "materialize cas: {e}"),
            MaterializeError::Manifest(e) => write!(f, "materialize manifest: {e}"),
            MaterializeError::Escape { path } => {
                write!(f, "refusing to materialize outside the sandbox: {path}")
            }
        }
    }
}

impl std::error::Error for MaterializeError {}

impl From<std::io::Error> for MaterializeError {
    fn from(e: std::io::Error) -> Self {
        MaterializeError::Io(e)
    }
}

/// Write every entry of `manifest` under `root`.
pub fn materialize(
    manifest: &Manifest,
    cas: &Cas,
    root: impl AsRef<Path>,
) -> Result<(), MaterializeError> {
    let root = root.as_ref();
    manifest
        .validate()
        .map_err(|error| MaterializeError::Manifest(error.to_string()))?;
    fs::create_dir_all(root)?;
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(MaterializeError::Escape {
            path: root.display().to_string(),
        });
    }

    // A manifest-declared symlink must never become the parent of another entry. Decode this
    // preflight only when the tree actually contains symlinks; ordinary manifests decode every
    // path exactly once, in `prepare_target` immediately before the write.
    if manifest
        .entries
        .iter()
        .any(|entry| entry.kind == EntryKind::Symlink)
    {
        let mut symlinks = HashSet::new();
        for entry in &manifest.entries {
            let path = checked_relative_path(&entry.path)?;
            if path.ancestors().skip(1).any(|path| symlinks.contains(path)) {
                return Err(MaterializeError::Escape {
                    path: entry.path.clone(),
                });
            }
            if entry.kind == EntryKind::Symlink {
                symlinks.insert(path);
            }
        }
    }

    // Sort compact entry indexes into digest groups. One bounded executor pass overlaps groups;
    // each group streams one verified regular file from CAS and reflinks/copies its duplicates.
    // No worker waits on another worker and resident content is O(workers × 64 KiB), independent
    // of object size and duplicate count.
    let mut order: Vec<usize> = (0..manifest.entries.len()).collect();
    order.sort_by(|left, right| {
        manifest.entries[*left]
            .content
            .cmp(&manifest.entries[*right].content)
            .then_with(|| left.cmp(right))
    });
    let mut groups = Vec::new();
    let mut start = 0;
    while start < order.len() {
        let content = &manifest.entries[order[start]].content;
        let mut end = start + 1;
        while end < order.len() && manifest.entries[order[end]].content == *content {
            end += 1;
        }
        groups.push(start..end);
        start = end;
    }
    review_parallel::try_for_each(&groups, |range| {
        let indexes = &order[range.clone()];
        let content = &manifest.entries[indexes[0]].content;
        materialize_group(manifest, indexes, content, cas, root)
    })?;
    Ok(())
}

fn materialize_group(
    manifest: &Manifest,
    indexes: &[usize],
    content: &str,
    cas: &Cas,
    root: &Path,
) -> Result<(), MaterializeError> {
    let regular = indexes
        .iter()
        .copied()
        .find(|index| manifest.entries[*index].kind != EntryKind::Symlink);
    let mut source = None;
    let mut symlink_bytes = None;
    let actual_size = if let Some(index) = regular {
        let entry = &manifest.entries[index];
        let target = prepare_target(root, &entry.path)?;
        let mut file = fs::File::create(&target)?;
        let size = cas
            .copy_verified_to(content, &mut file)
            .map_err(|error| MaterializeError::Cas(error.to_string()))?;
        set_executable(&target, entry.kind == EntryKind::Executable)?;
        source = Some((index, target));
        size
    } else {
        let bytes = cas
            .get_bounded(content, MAX_SYMLINK_TARGET_BYTES)
            .map_err(|error| MaterializeError::Cas(error.to_string()))?;
        let size = bytes.len() as u64;
        symlink_bytes = Some(bytes);
        size
    };

    for index in indexes {
        let entry = &manifest.entries[*index];
        if entry.size != actual_size {
            return Err(MaterializeError::Manifest(format!(
                "entry `{}` declares {} bytes but CAS object {} has {}",
                entry.path, entry.size, entry.content, actual_size
            )));
        }
    }

    for index in indexes {
        let entry = &manifest.entries[*index];
        if source.as_ref().is_some_and(|(source, _)| source == index) {
            continue;
        }
        let target = prepare_target(root, &entry.path)?;
        match entry.kind {
            EntryKind::Symlink => {
                if symlink_bytes.is_none() {
                    symlink_bytes = Some(
                        cas.get_bounded(content, MAX_SYMLINK_TARGET_BYTES)
                            .map_err(|error| MaterializeError::Cas(error.to_string()))?,
                    );
                }
                symlink(symlink_bytes.as_deref().expect("loaded above"), &target)?;
            }
            EntryKind::File | EntryKind::Executable => {
                let (_, source) = source.as_ref().expect("regular source exists");
                reflink_copy::reflink_or_copy(source, &target)?;
                set_executable(&target, entry.kind == EntryKind::Executable)?;
            }
        }
    }
    Ok(())
}

fn checked_relative_path(encoded: &str) -> Result<PathBuf, MaterializeError> {
    let decoded = crate::manifest::decode_path(encoded);
    let raw = crate::manifest::fs_path_bytes(&decoded);
    let escapes = raw.components().any(|component| {
        matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    let noncanonical = crate::manifest::encode_path(&decoded) != encoded
        || decoded
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || matches!(component, b"." | b".."));
    if escapes || noncanonical || encoded.is_empty() {
        return Err(MaterializeError::Escape {
            path: encoded.to_string(),
        });
    }
    Ok(raw)
}

fn prepare_target(root: &Path, encoded: &str) -> Result<PathBuf, MaterializeError> {
    let relative = checked_relative_path(encoded)?;
    let mut parent = root.to_path_buf();
    if let Some(components) = relative.parent() {
        for component in components.components() {
            parent.push(component);
            loop {
                match fs::symlink_metadata(&parent) {
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                        break;
                    }
                    Ok(_) => {
                        return Err(MaterializeError::Escape {
                            path: encoded.to_string(),
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        match fs::create_dir(&parent) {
                            Ok(()) => break,
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                continue;
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(root.join(relative))
}

#[cfg(unix)]
fn symlink(target: &[u8], at: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), at)
}

#[cfg(not(unix))]
fn symlink(target: &[u8], at: &Path) -> std::io::Result<()> {
    fs::write(at, target)
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_leave_the_root_are_refused() {
        for path in [
            "../escape",
            "a/../../escape",
            "/etc/passwd",
            "a/./duplicate",
            "a//duplicate",
            "a%2Fduplicate",
            "",
        ] {
            assert!(
                matches!(
                    checked_relative_path(path),
                    Err(MaterializeError::Escape { .. })
                ),
                "{path} was not refused"
            );
        }
        assert!(checked_relative_path("a/b/c.rs").is_ok());
        // A path that merely *contains* dots is fine; only a real parent component escapes.
        assert!(checked_relative_path("a/..b/c").is_ok());
    }

    #[test]
    fn a_manifest_entry_below_a_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let link = cas.put(b"../outside").unwrap();
        let file = cas.put(b"content").unwrap();
        let manifest = Manifest::new(vec![
            crate::manifest::Entry {
                path: "link".into(),
                kind: EntryKind::Symlink,
                content: link,
                size: 10,
            },
            crate::manifest::Entry {
                path: "link/escape".into(),
                kind: EntryKind::File,
                content: file,
                size: 7,
            },
        ])
        .unwrap();

        assert!(matches!(
            materialize(&manifest, &cas, dir.path().join("tree")),
            Err(MaterializeError::Escape { path }) if path == "link/escape"
        ));
    }

    #[test]
    fn duplicate_targets_are_refused_before_any_write_can_race() {
        let dir = tempfile::tempdir().unwrap();
        let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
        let one = cas.put(b"one").unwrap();
        let two = cas.put(b"two").unwrap();
        let manifest = Manifest {
            entries: vec![
                crate::Entry {
                    path: "same".into(),
                    kind: EntryKind::File,
                    content: one,
                    size: 3,
                },
                crate::Entry {
                    path: "same".into(),
                    kind: EntryKind::File,
                    content: two,
                    size: 3,
                },
            ],
        };

        let error = materialize(&manifest, &cas, dir.path().join("tree")).unwrap_err();
        assert!(error.to_string().contains("repeats path `same`"));
        assert!(!dir.path().join("tree/same").exists());
    }

    #[test]
    fn a_large_repeated_digest_materializes_without_nested_executor_work() {
        let dir = tempfile::tempdir().unwrap();
        let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
        let repeated = cas.put(b"repeat").unwrap();
        let distinct = cas.put(b"distinct").unwrap();
        let mut entries: Vec<crate::Entry> = (0..review_parallel::worker_limit())
            .map(|index| crate::Entry {
                path: format!("repeated/{index:04}"),
                kind: EntryKind::File,
                content: repeated.clone(),
                size: 6,
            })
            .collect();
        entries.push(crate::Entry {
            path: "ordinary".into(),
            kind: EntryKind::File,
            content: distinct,
            size: 8,
        });
        let manifest = Manifest::new(entries).unwrap();
        let root = dir.path().join("tree");

        materialize(&manifest, &cas, &root).unwrap();

        assert_eq!(std::fs::read(root.join("ordinary")).unwrap(), b"distinct");
        assert_eq!(
            std::fs::read(root.join("repeated/0000")).unwrap(),
            b"repeat"
        );
    }

    #[test]
    fn oversized_symlink_targets_are_refused_before_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
        let bytes = vec![b'x'; MAX_SYMLINK_TARGET_BYTES as usize + 1];
        let content = cas.put(&bytes).unwrap();
        let manifest = Manifest::new(vec![crate::Entry {
            path: "link".into(),
            kind: EntryKind::Symlink,
            content,
            size: bytes.len() as u64,
        }])
        .unwrap();

        let error = materialize(&manifest, &cas, dir.path().join("tree")).unwrap_err();
        assert!(error.to_string().contains("limit is 16384"));
    }
}
