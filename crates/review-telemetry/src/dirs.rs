//! The directory registry (design §6): `$XDG_STATE_HOME/af/telemetry-dirs.v1.json`, an
//! `af/TelemetryDirs@1` document listing every telemetry directory a process ever wrote on this
//! machine, deduplicated by canonical path, so the storage inventory reaches every one of them
//! whatever the current level or `dir`.
//!
//! A process registers the canonical path of its telemetry directory before its first write; a
//! process that cannot register records one problem and writes no telemetry. Registration
//! refuses a root that is inside or contains another registered telemetry root, a registered
//! Store, the warm check cache, a release installation or any other directory the storage
//! inventory accounts on its own ([`Accounted`]): `af storage` measures a Store's whole allocated
//! directory, so a telemetry root inside one would be counted twice and removed with it.
//! [`overlap`] is the predicate the inventory rechecks every time.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The document type of the registry.
pub const SCHEMA: &str = "af/TelemetryDirs@1";
/// The registry's file name below `$XDG_STATE_HOME/af`.
pub const FILE: &str = "telemetry-dirs.v1.json";

/// The registry's location below a state home.
pub fn location(state_home: &Path) -> PathBuf {
    state_home.join("af").join(FILE)
}

/// One registered directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registered {
    /// Canonical, absolute.
    pub path: PathBuf,
    pub first_use_unix_ms: u64,
    pub last_use_unix_ms: u64,
}

/// `af/TelemetryDirs@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema: String,
    pub dirs: Vec<Registered>,
}

/// A directory the storage inventory accounts on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accounted {
    pub path: PathBuf,
    /// What it is, for the refusal: `a Task Store`, `the warm check cache`, …
    pub what: String,
    /// A telemetry root may lie inside it, though never contain it: the state root holds the
    /// default telemetry directory beside the Stores it accounts one by one.
    pub may_contain_telemetry: bool,
}

impl Accounted {
    pub fn new(path: impl Into<PathBuf>, what: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            what: what.into(),
            may_contain_telemetry: false,
        }
    }

    /// A root (the state root) a telemetry directory may lie inside but never contain.
    pub fn container(path: impl Into<PathBuf>, what: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            what: what.into(),
            may_contain_telemetry: true,
        }
    }
}

/// The canonical spelling of `path`, which need not exist yet: the deepest existing ancestor
/// canonicalized, the rest appended without `.` or `..`.
pub fn canonical(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut existing = path.to_path_buf();
    loop {
        if let Ok(resolved) = std::fs::canonicalize(&existing) {
            let mut out = resolved;
            for part in missing.iter().rev() {
                out.push(part);
            }
            return normalize(&out);
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return normalize(path),
        }
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// How a telemetry root overlaps something accounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub root: PathBuf,
    pub other: PathBuf,
    pub what: String,
    /// The root lies inside the other directory (else it contains it, or they are equal).
    pub inside: bool,
}

impl std::fmt::Display for Overlap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let relation = if self.root == self.other {
            "is"
        } else if self.inside {
            "lies inside"
        } else {
            "contains"
        };
        write!(
            f,
            "telemetry directory {} {relation} {} {}; choose a [telemetry] dir outside it",
            self.root.display(),
            self.what,
            self.other.display()
        )
    }
}

/// The first overlap of `root` with `telemetry` (other registered roots) or `accounted`, all
/// compared canonically; `None` when it stands alone.
pub fn overlap(root: &Path, telemetry: &[PathBuf], accounted: &[Accounted]) -> Option<Overlap> {
    let root = canonical(root);
    for entry in accounted {
        let other = canonical(&entry.path);
        let inside = root.starts_with(&other) && root != other;
        let contains = other.starts_with(&root);
        if contains || (inside && !entry.may_contain_telemetry) {
            return Some(Overlap {
                inside,
                root,
                other,
                what: entry.what.clone(),
            });
        }
    }
    for other in telemetry {
        let other = canonical(other);
        if other == root {
            continue;
        }
        if root.starts_with(&other) || other.starts_with(&root) {
            return Some(Overlap {
                inside: root.starts_with(&other),
                root,
                other,
                what: "another telemetry directory".into(),
            });
        }
    }
    None
}

/// Every registered directory. A missing registry is empty; one of another version or shape is
/// refused (ADR-0145).
pub fn read(location: &Path) -> Result<Vec<Registered>, String> {
    match std::fs::read(location) {
        Ok(bytes) => {
            let registry: Registry = serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", location.display()))?;
            if registry.schema != SCHEMA {
                return Err(format!(
                    "{}: schema `{}` is not {SCHEMA}",
                    location.display(),
                    registry.schema
                ));
            }
            Ok(registry.dirs)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("{}: {error}", location.display())),
    }
}

fn now_unix_ms() -> u64 {
    u64::try_from(crate::layout::unix_ns_now() / 1_000_000).unwrap_or(u64::MAX)
}

/// Register `root` in the registry at `location`, refusing an overlap. The refusal and every
/// failure is the one problem's reason; nothing under `root` is touched.
pub fn register(location: &Path, root: &Path, accounted: &[Accounted]) -> Result<(), String> {
    if !root.is_absolute() {
        return Err(format!(
            "telemetry directory {} is not absolute",
            root.display()
        ));
    }
    let root = canonical(root);
    let now = now_unix_ms();
    update(location, |dirs| {
        let others: Vec<PathBuf> = dirs.iter().map(|entry| entry.path.clone()).collect();
        if let Some(overlap) = overlap(&root, &others, accounted) {
            return Err(overlap.to_string());
        }
        match dirs.iter_mut().find(|entry| entry.path == root) {
            Some(entry) => entry.last_use_unix_ms = entry.last_use_unix_ms.max(now),
            None => dirs.push(Registered {
                path: root.clone(),
                first_use_unix_ms: now,
                last_use_unix_ms: now,
            }),
        }
        Ok(())
    })
}

/// Keep only the registered directories `keep` accepts; returns the dropped ones.
pub fn retain(
    location: &Path,
    keep: impl Fn(&Registered) -> bool,
) -> Result<Vec<Registered>, String> {
    let mut dropped = Vec::new();
    update(location, |dirs| {
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(dirs).into_iter().partition(&keep);
        *dirs = kept;
        dropped = gone;
        Ok(())
    })?;
    Ok(dropped)
}

/// Read, change and atomically replace the registry under its own lock file.
fn update(
    location: &Path,
    change: impl FnOnce(&mut Vec<Registered>) -> Result<(), String>,
) -> Result<(), String> {
    let parent = location
        .parent()
        .ok_or("the telemetry registry has no directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    let _held = crate::layout::exclusive_within(
        &parent.join("telemetry-dirs.lock"),
        crate::layout::LOCK_WAIT,
    )
    .map_err(|error| format!("locking the telemetry registry: {error}"))?
    .ok_or("the telemetry registry is locked by another process")?;
    let mut dirs = read(location)?;
    change(&mut dirs)?;
    let mut seen = std::collections::BTreeSet::new();
    dirs.retain(|entry| seen.insert(entry.path.clone()));
    dirs.sort_by(|a, b| a.path.cmp(&b.path));
    let mut text = serde_json::to_vec_pretty(&Registry {
        schema: SCHEMA.into(),
        dirs,
    })
    .map_err(|error| error.to_string())?;
    text.push(b'\n');
    let temp = parent.join(crate::layout::temp_name("dirs"));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| file.write_all(&text).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temp, location));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("writing {}: {error}", location.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_twice_keeps_one_entry() {
        let temp = tempfile::tempdir().unwrap();
        let location = location(temp.path());
        let root = temp.path().join("telemetry");
        register(&location, &root, &[]).unwrap();
        register(&location, &root.join("."), &[]).unwrap();
        let dirs = read(&location).unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].path, canonical(&root));
        assert!(
            !root.exists(),
            "registration touches nothing under the root"
        );
        let dropped = retain(&location, |_| false).unwrap();
        assert_eq!(dropped.len(), 1);
        assert!(read(&location).unwrap().is_empty());
    }

    #[test]
    fn the_state_root_may_contain_telemetry_but_not_the_reverse() {
        let state = PathBuf::from("/s/af");
        let accounted = [
            Accounted::container(&state, "the state root"),
            Accounted::new("/s/af/task/local", "the Task Store root"),
        ];
        assert_eq!(overlap(Path::new("/s/af/telemetry"), &[], &accounted), None);
        assert!(overlap(Path::new("/s"), &[], &accounted).is_some());
        assert!(overlap(Path::new("/s/af/task/local/x/t"), &[], &accounted).is_some());
    }
}
