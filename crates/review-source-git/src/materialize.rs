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

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Condvar, Mutex};

use review_store::Cas;

use crate::manifest::{EntryKind, Manifest};

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
    manifest
        .validate()
        .map_err(|error| MaterializeError::Manifest(error.to_string()))?;
    fs::create_dir_all(root)?;

    // Each distinct parent is prepared once: `create_dir_all` stats every component, so doing
    // it per entry costs O(files × depth) syscalls where O(directories × depth) suffices.
    let mut prepared: BTreeSet<PathBuf> = BTreeSet::new();
    let mut symlinks: HashSet<&str> = HashSet::new();
    for entry in &manifest.entries {
        if has_symlink_ancestor(&entry.path, &symlinks) {
            return Err(MaterializeError::Escape {
                path: entry.path.clone(),
            });
        }
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
        if entry.kind == EntryKind::Symlink {
            symlinks.insert(&entry.path);
        }
    }

    // Sort compact entry indexes into contiguous digest groups. One executor pass overlaps reads
    // and writes across groups; an explicit byte budget, admitted from the CAS object's actual
    // on-disk length before allocation, bounds resident content independently of CPU count.
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
    let resident = ResidentBudget::new(64 * 1024 * 1024);
    review_parallel::try_for_each(&groups, |range| {
        // Large occurrence groups are handled below from the caller thread. Keeping nested
        // executor work out of this byte-budgeted fan-out ensures a worker waiting for memory
        // can never be stolen by the permit holder itself.
        if range.len() >= review_parallel::worker_limit() {
            return Ok(());
        }
        let indexes = &order[range.clone()];
        let content = &manifest.entries[indexes[0]].content;
        let (bytes, _resident) = cas
            .get_admitted(content, |length| resident.acquire(length))
            .map_err(|error| MaterializeError::Cas(error.to_string()))?;
        for index in indexes {
            let entry = &manifest.entries[*index];
            if entry.size != bytes.len() as u64 {
                return Err(MaterializeError::Manifest(format!(
                    "entry `{}` declares {} bytes but CAS object {} has {}",
                    entry.path,
                    entry.size,
                    entry.content,
                    bytes.len()
                )));
            }
        }
        indexes.iter().try_for_each(|index| {
            let entry = &manifest.entries[*index];
            let target = safe_join(root, &entry.path)?;
            write_entry(&bytes, &target, entry.kind)?;
            Ok::<_, MaterializeError>(())
        })
    })?;

    // A heavily repeated digest still writes occurrences in parallel, but only after the outer
    // byte-budgeted pass has drained. The resident permit is therefore never held across a
    // re-entrant executor call that can steal another memory-waiting materialization task.
    for range in groups
        .iter()
        .filter(|range| range.len() >= review_parallel::worker_limit())
    {
        let indexes = &order[range.clone()];
        let content = &manifest.entries[indexes[0]].content;
        let (bytes, _resident) = cas
            .get_admitted(content, |length| resident.acquire(length))
            .map_err(|error| MaterializeError::Cas(error.to_string()))?;
        for index in indexes {
            let entry = &manifest.entries[*index];
            if entry.size != bytes.len() as u64 {
                return Err(MaterializeError::Manifest(format!(
                    "entry `{}` declares {} bytes but CAS object {} has {}",
                    entry.path,
                    entry.size,
                    entry.content,
                    bytes.len()
                )));
            }
        }
        review_parallel::try_for_each(indexes, |index| {
            let entry = &manifest.entries[*index];
            let target = safe_join(root, &entry.path)?;
            write_entry(&bytes, &target, entry.kind)?;
            Ok::<_, MaterializeError>(())
        })?;
    }
    Ok(())
}

fn has_symlink_ancestor<'a>(path: &'a str, symlinks: &HashSet<&'a str>) -> bool {
    let mut prefix = path;
    while let Some((parent, _)) = prefix.rsplit_once('/') {
        if symlinks.contains(parent) {
            return true;
        }
        prefix = parent;
    }
    false
}

struct ResidentBudget {
    limit: u64,
    used: Mutex<u64>,
    changed: Condvar,
}

impl ResidentBudget {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            used: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    fn acquire(&self, bytes: u64) -> ResidentPermit<'_> {
        // An oversized object must still make progress, alone. Its charge fills the budget, so
        // the bound is max(limit, largest object), never CPU workers × largest object.
        let charge = bytes.min(self.limit);
        let mut used = self.used.lock().expect("resident materialization budget");
        while *used > self.limit - charge {
            used = self
                .changed
                .wait(used)
                .expect("resident materialization budget");
        }
        *used += charge;
        ResidentPermit {
            budget: self,
            charge,
        }
    }
}

struct ResidentPermit<'a> {
    budget: &'a ResidentBudget,
    charge: u64,
}

impl Drop for ResidentPermit<'_> {
    fn drop(&mut self) {
        let mut used = self
            .budget
            .used
            .lock()
            .expect("resident materialization budget");
        *used -= self.charge;
        self.budget.changed.notify_all();
    }
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
    fn a_large_repeated_group_drains_outside_the_budgeted_outer_fanout() {
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
}
