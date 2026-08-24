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

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

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
    let decoded_paths = if manifest
        .entries
        .iter()
        .any(|entry| entry.kind == EntryKind::Symlink)
    {
        let paths: Vec<PathBuf> = manifest
            .entries
            .iter()
            .map(|entry| checked_relative_path(&entry.path))
            .collect::<Result<_, _>>()?;
        let mut symlinks = HashSet::new();
        for (entry, path) in manifest.entries.iter().zip(&paths) {
            if path.ancestors().skip(1).any(|path| symlinks.contains(path)) {
                return Err(MaterializeError::Escape {
                    path: entry.path.clone(),
                });
            }
            if entry.kind == EntryKind::Symlink {
                symlinks.insert(path.clone());
            }
        }
        Some(paths)
    } else {
        None
    };

    // Preserve first-occurrence order while grouping in O(entries): the hash map is lookup only,
    // never an iteration authority. Sources and duplicates run as two non-nested executor phases,
    // so one heavily repeated digest can use the full worker budget without publishing bytes
    // before its source was verified.
    let mut group_by_content = HashMap::new();
    let mut groups: Vec<ContentGroup> = Vec::new();
    for (index, entry) in manifest.entries.iter().enumerate() {
        let group = if let Some(group) = group_by_content.get(entry.content.as_str()) {
            *group
        } else {
            let group = groups.len();
            group_by_content.insert(entry.content.as_str(), group);
            groups.push(ContentGroup::default());
            group
        };
        groups[group].indexes.push(index);
        if groups[group].regular_source.is_none() && entry.kind != EntryKind::Symlink {
            groups[group].regular_source = Some(index);
        }
    }
    if decoded_paths.is_none() {
        for group in &mut groups {
            if let Some(source) = group.regular_source {
                group.source_relative =
                    Some(checked_relative_path(&manifest.entries[source].path)?);
            }
        }
    }
    let prepared = PreparedDirectories::new(root);
    review_parallel::try_for_each(&groups, |group| {
        materialize_group_source(
            manifest,
            group,
            decoded_paths.as_deref(),
            cas,
            root,
            &prepared,
        )
    })?;
    let duplicates: Vec<(usize, usize, usize)> = groups
        .iter()
        .enumerate()
        .flat_map(|(group_index, group)| {
            group.regular_source.into_iter().flat_map(move |source| {
                group.indexes.iter().copied().filter_map(move |index| {
                    (index != source && manifest.entries[index].kind != EntryKind::Symlink)
                        .then_some((index, source, group_index))
                })
            })
        })
        .collect();
    review_parallel::try_for_each(&duplicates, |(index, source, group_index)| {
        let entry = &manifest.entries[*index];
        let relative = entry_relative(manifest, decoded_paths.as_deref(), *index)?;
        let target = prepare_relative(root, &relative, &entry.path, &prepared)?;
        let source_relative = match groups[*group_index].source_relative.as_deref() {
            Some(path) => path,
            None => decoded_paths
                .as_deref()
                .expect("symlink manifest paths are predecoded")[*source]
                .as_path(),
        };
        reflink_copy::reflink_or_copy(root.join(source_relative), &target)?;
        set_executable(&target, entry.kind == EntryKind::Executable)?;
        Ok::<_, MaterializeError>(())
    })?;
    Ok(())
}

#[derive(Default)]
struct ContentGroup {
    indexes: Vec<usize>,
    regular_source: Option<usize>,
    source_relative: Option<PathBuf>,
}

fn materialize_group_source(
    manifest: &Manifest,
    group: &ContentGroup,
    decoded_paths: Option<&[PathBuf]>,
    cas: &Cas,
    root: &Path,
    prepared: &PreparedDirectories,
) -> Result<(), MaterializeError> {
    let content = &manifest.entries[group.indexes[0]].content;
    let mut symlink_bytes = None;
    let actual_size = if let Some(index) = group.regular_source {
        let entry = &manifest.entries[index];
        let relative = group.source_relative.as_deref().unwrap_or_else(|| {
            decoded_paths.expect("symlink manifest paths are predecoded")[index].as_path()
        });
        let target = prepare_relative(root, relative, &entry.path, prepared)?;
        let size = cas
            .materialize_verified(content, &target)
            .map_err(|error| MaterializeError::Cas(error.to_string()))?;
        set_executable(&target, entry.kind == EntryKind::Executable)?;
        size
    } else {
        let bytes = cas
            .get_bounded(content, MAX_SYMLINK_TARGET_BYTES)
            .map_err(|error| MaterializeError::Cas(error.to_string()))?;
        let size = bytes.len() as u64;
        symlink_bytes = Some(bytes);
        size
    };

    for index in &group.indexes {
        let entry = &manifest.entries[*index];
        if entry.size != actual_size {
            return Err(MaterializeError::Manifest(format!(
                "entry `{}` declares {} bytes but CAS object {} has {}",
                entry.path, entry.size, entry.content, actual_size
            )));
        }
    }

    for index in &group.indexes {
        let entry = &manifest.entries[*index];
        if group.regular_source.as_ref() == Some(index) || entry.kind != EntryKind::Symlink {
            continue;
        }
        let relative = &decoded_paths.expect("symlink manifest paths are predecoded")[*index];
        let target = prepare_relative(root, relative, &entry.path, prepared)?;
        if symlink_bytes.is_none() {
            symlink_bytes = Some(
                cas.get_bounded(content, MAX_SYMLINK_TARGET_BYTES)
                    .map_err(|error| MaterializeError::Cas(error.to_string()))?,
            );
        }
        symlink(symlink_bytes.as_deref().expect("loaded above"), &target)?;
    }
    Ok(())
}

fn entry_relative<'a>(
    manifest: &'a Manifest,
    decoded_paths: Option<&'a [PathBuf]>,
    index: usize,
) -> Result<Cow<'a, Path>, MaterializeError> {
    decoded_paths.map_or_else(
        || checked_relative_path(&manifest.entries[index].path).map(Cow::Owned),
        |paths| Ok(Cow::Borrowed(paths[index].as_path())),
    )
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

struct PreparedDirectories {
    paths: Mutex<HashSet<PathBuf>>,
}

impl PreparedDirectories {
    fn new(root: &Path) -> Self {
        Self {
            paths: Mutex::new(HashSet::from([root.to_path_buf()])),
        }
    }
}

fn prepare_relative(
    root: &Path,
    relative: &Path,
    encoded: &str,
    prepared: &PreparedDirectories,
) -> Result<PathBuf, MaterializeError> {
    let mut parent = root.to_path_buf();
    if let Some(components) = relative.parent() {
        for component in components.components() {
            parent.push(component);
            if prepared
                .paths
                .lock()
                .expect("prepared directories")
                .contains(&parent)
            {
                continue;
            }
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
            prepared
                .paths
                .lock()
                .expect("prepared directories")
                .insert(parent.clone());
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
