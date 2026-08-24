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

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use review_store::Cas;

use crate::manifest::{EntryKind, Manifest};

#[derive(Debug)]
pub enum MaterializeError {
    Io(std::io::Error),
    Cas(String),
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

/// Resolve a manifest path under `root`, refusing anything that escapes.
fn safe_join(root: &Path, encoded: &str) -> Result<PathBuf, MaterializeError> {
    // Decode first: the manifest path is a JSON-safe display form, and joining it verbatim
    // would write `docs/50%-off.md` to `docs/50%25-off.md`. The escape check runs on the real
    // components — `..`, an absolute root, a Windows prefix are all ASCII, so decoding does not
    // hide them.
    let raw = crate::manifest::fs_path(encoded);
    let escapes = raw.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    if escapes || encoded.is_empty() {
        return Err(MaterializeError::Escape {
            path: encoded.to_string(),
        });
    }
    Ok(root.join(&raw))
}

/// Write every entry of `manifest` under `root`.
pub fn materialize(
    manifest: &Manifest,
    cas: &Cas,
    root: impl AsRef<Path>,
) -> Result<(), MaterializeError> {
    let root = root.as_ref();
    fs::create_dir_all(root)?;

    // Each distinct parent is prepared once: `create_dir_all` stats every component, so doing
    // it per entry costs O(files × depth) syscalls where O(directories × depth) suffices.
    let mut prepared: BTreeSet<PathBuf> = BTreeSet::new();
    let mut targets = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        let target = safe_join(root, &entry.path)?;
        if let Some(parent) = target.parent()
            && !prepared.contains(parent)
        {
            fs::create_dir_all(parent)?;
            // A parent that is a symlink would place the write outside the root even though
            // every component looked innocent.
            if fs::symlink_metadata(parent)?.file_type().is_symlink() {
                return Err(MaterializeError::Escape {
                    path: entry.path.clone(),
                });
            }
            prepared.insert(parent.to_path_buf());
        }

        targets.push((target, entry));
    }

    let symlinks: HashSet<&Path> = targets
        .iter()
        .filter(|(_, entry)| entry.kind == EntryKind::Symlink)
        .map(|(target, _)| target.as_path())
        .collect();
    if !symlinks.is_empty() {
        if let Some((_, entry)) = targets.iter().find(|(target, _)| {
            target
                .ancestors()
                .skip(1)
                .take_while(|ancestor| *ancestor != root)
                .any(|ancestor| symlinks.contains(ancestor))
        }) {
            return Err(MaterializeError::Escape {
                path: entry.path.clone(),
            });
        }
    }

    // One verified CAS read per distinct digest. Load at most one group per executor worker,
    // then fan every occurrence in that bounded window out through the executor. This retains
    // occurrence-level concurrency for a single heavily repeated blob while bounding resident
    // content to worker_limit × largest group blob rather than the sum of candidate-controlled
    // distinct content.
    let mut grouped: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (target, entry) in targets {
        grouped
            .entry(entry.content.as_str())
            .or_default()
            .push((target, entry.kind));
    }
    let groups: Vec<_> = grouped.into_iter().collect();
    for window in groups.chunks(review_parallel::worker_limit()) {
        let loads: Vec<_> = window
            .iter()
            .map(|(content, targets)| (*content, targets))
            .collect();
        let loaded = review_parallel::try_map_owned(loads, |(content, targets)| {
            cas.get(content)
                .map(|bytes| (bytes, targets))
                .map_err(|error| MaterializeError::Cas(error.to_string()))
        })?;
        let occurrences: Vec<_> = loaded
            .iter()
            .flat_map(|(bytes, targets)| {
                targets
                    .iter()
                    .map(move |(target, kind)| (bytes.as_slice(), target, *kind))
            })
            .collect();
        review_parallel::try_for_each(&occurrences, |(bytes, target, kind)| {
            write_entry(bytes, target, *kind)?;
            Ok::<_, MaterializeError>(())
        })?;
    }
    Ok(())
}

fn write_entry(bytes: &[u8], target: &Path, kind: EntryKind) -> std::io::Result<()> {
    match kind {
        EntryKind::Symlink => symlink(bytes, target),
        EntryKind::File | EntryKind::Executable => {
            fs::write(target, bytes)?;
            set_executable(target, kind == EntryKind::Executable)
        }
    }
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
        let root = Path::new("/tmp/sandbox");
        for path in ["../escape", "a/../../escape", "/etc/passwd", ""] {
            assert!(
                matches!(safe_join(root, path), Err(MaterializeError::Escape { .. })),
                "{path} was not refused"
            );
        }
        assert!(safe_join(root, "a/b/c.rs").is_ok());
        // A path that merely *contains* dots is fine; only a real parent component escapes.
        assert!(safe_join(root, "a/..b/c").is_ok());
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
        ]);

        assert!(matches!(
            materialize(&manifest, &cas, dir.path().join("tree")),
            Err(MaterializeError::Escape { path }) if path == "link/escape"
        ));
    }
}
