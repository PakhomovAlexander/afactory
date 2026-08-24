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
    let workers = std::thread::available_parallelism()
        .map(|workers| workers.get())
        .unwrap_or(1);
    materialize_with_workers(manifest, cas, root, workers)
}

/// Materialize with an explicit worker budget supplied by the owning scheduler.
pub fn materialize_with_workers(
    manifest: &Manifest,
    cas: &Cas,
    root: impl AsRef<Path>,
    workers: usize,
) -> Result<(), MaterializeError> {
    let root = root.as_ref();
    fs::create_dir_all(root)?;

    // Each distinct parent is prepared once: `create_dir_all` stats every component, so doing
    // it per entry costs O(files × depth) syscalls where O(directories × depth) suffices.
    let mut prepared: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
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

    let symlinks: Vec<&Path> = targets
        .iter()
        .filter(|(_, entry)| entry.kind == EntryKind::Symlink)
        .map(|(target, _)| target.as_path())
        .collect();
    if let Some((_, entry)) = targets.iter().find(|(target, _)| {
        symlinks
            .iter()
            .any(|symlink| target != symlink && target.starts_with(symlink))
    }) {
        return Err(MaterializeError::Escape {
            path: entry.path.clone(),
        });
    }

    // One verified CAS read per distinct digest. Distinct content groups run concurrently;
    // repeated content is then written to every occurrence without being re-read or re-hashed.
    let mut grouped: std::collections::BTreeMap<_, Vec<_>> = std::collections::BTreeMap::new();
    for (target, entry) in targets {
        grouped
            .entry(entry.content.as_str())
            .or_default()
            .push((target, entry.kind));
    }
    let groups: Vec<_> = grouped.into_iter().collect();
    let workers = workers.max(1).min(groups.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| -> Result<(), MaterializeError> {
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((content, targets)) = groups.get(index) else {
                            return Ok(());
                        };

                        let bytes = cas
                            .get(content)
                            .map_err(|error| MaterializeError::Cas(error.to_string()))?;

                        for (target, kind) in targets {
                            match kind {
                                EntryKind::Symlink => symlink(&bytes, target)?,
                                EntryKind::File | EntryKind::Executable => {
                                    fs::write(target, &bytes)?;
                                    set_executable(target, *kind == EntryKind::Executable)?;
                                }
                            }
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("materialize worker")?;
        }
        Ok(())
    })
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
}
