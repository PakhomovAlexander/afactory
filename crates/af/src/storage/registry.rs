//! The Store registry (ADR-0144): `$XDG_STATE_HOME/af/stores.toml`, every Task and review
//! Store af opened or created outside its default roots (`--state`, `--state-root`), so the
//! Storage Budget's sweep reaches Stores nothing else knows.
//!
//! Recording is best effort and never fails a command: a registry that cannot be read or
//! written is reported once on stderr and left as it is. The sweep visits every registered
//! Store and drops the entries whose path no longer holds one.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a registered path holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StoreKind {
    /// A Task Store (`af task … --state DIR`).
    Task,
    /// One review campaign's Store (`af review run --state DIR`).
    Review,
    /// A directory of review campaign Stores (`af review … --state-root DIR`).
    ReviewRoot,
}

/// One registered Store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registered {
    pub(crate) path: PathBuf,
    pub(crate) kind: StoreKind,
    pub(crate) first_use_unix_ms: u64,
    pub(crate) last_use_unix_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    #[serde(default, rename = "store")]
    stores: Vec<Registered>,
}

const VERSION: u32 = 1;

/// The registry's location below a state home.
pub(crate) fn location(state_home: &Path) -> PathBuf {
    state_home.join("af").join("stores.toml")
}

/// Whether `path` lies below one of af's default Store roots, where the sweep finds it anyway.
fn under_default_roots(state_home: &Path, path: &Path) -> bool {
    let af = state_home.join("af");
    [
        af.join("task").join("local"),
        af.join("review").join("campaigns"),
        af.join("review").join("local"),
    ]
    .iter()
    .any(|root| path.starts_with(root))
}

/// Record that af used the Store at `path` now. Best effort: nothing here fails the command.
pub(crate) fn record(path: &Path, kind: StoreKind) {
    let Ok(state_home) = crate::config::state_home() else {
        return;
    };
    if !path.is_absolute() || under_default_roots(&state_home, path) {
        return;
    }
    let now = super::now_unix_ms();
    if let Err(error) = update(&location(&state_home), |stores| {
        match stores
            .iter_mut()
            .find(|entry| entry.path == path && entry.kind == kind)
        {
            Some(entry) => entry.last_use_unix_ms = entry.last_use_unix_ms.max(now),
            None => stores.push(Registered {
                path: path.to_path_buf(),
                kind,
                first_use_unix_ms: now,
                last_use_unix_ms: now,
            }),
        }
    }) {
        eprintln!("af: the Store registry was not updated: {error}");
    }
}

/// Every registered Store, in path order. A missing registry is empty; an unreadable one is an
/// error the sweep reports.
pub(crate) fn read(location: &Path) -> Result<Vec<Registered>, String> {
    match std::fs::read_to_string(location) {
        Ok(text) => parse(&text).map_err(|error| format!("{}: {error}", location.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("{}: {error}", location.display())),
    }
}

fn parse(text: &str) -> Result<Vec<Registered>, String> {
    let file: RegistryFile = toml::from_str(text).map_err(|error| error.to_string())?;
    if file.version != VERSION {
        return Err(format!("version {} is not {VERSION}", file.version));
    }
    Ok(file.stores)
}

/// Keep only the registered Stores `keep` accepts; returns the dropped ones.
pub(crate) fn retain(
    location: &Path,
    keep: impl Fn(&Registered) -> bool,
) -> Result<Vec<Registered>, String> {
    let mut dropped = Vec::new();
    update(location, |stores| {
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(stores).into_iter().partition(&keep);
        *stores = kept;
        dropped = gone;
    })?;
    Ok(dropped)
}

/// Read, change and atomically replace the registry under its own lock file, so concurrent
/// commands never lose each other's entries.
fn update(location: &Path, change: impl FnOnce(&mut Vec<Registered>)) -> Result<(), String> {
    let parent = location
        .parent()
        .ok_or("the Store registry has no directory")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join("stores.lock"))
        .map_err(|error| error.to_string())?;
    let _held = nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, errno)| format!("locking the Store registry: {errno}"))?;
    let mut stores = read(location)?;
    change(&mut stores);
    stores.sort_by(|a, b| a.path.cmp(&b.path).then(a.kind.cmp(&b.kind)));
    let text = toml::to_string(&RegistryFile {
        version: VERSION,
        stores,
    })
    .map_err(|error| error.to_string())?;
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    staged
        .write_all(
            format!(
                "# Stores af used outside its default roots; `af storage` reads this (ADR-0144).\n{text}"
            )
            .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
    staged
        .persist(location)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_twice_keeps_one_entry_and_dropping_rewrites_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let location = location(directory.path());
        let store = PathBuf::from("/elsewhere/state");
        for now in [10, 20] {
            update(&location, |stores| {
                match stores.iter_mut().find(|entry| entry.path == store) {
                    Some(entry) => entry.last_use_unix_ms = now,
                    None => stores.push(Registered {
                        path: store.clone(),
                        kind: StoreKind::Task,
                        first_use_unix_ms: now,
                        last_use_unix_ms: now,
                    }),
                }
            })
            .unwrap();
        }
        let stores = read(&location).unwrap();
        assert_eq!(stores.len(), 1);
        assert_eq!(
            (stores[0].first_use_unix_ms, stores[0].last_use_unix_ms),
            (10, 20)
        );
        let dropped = retain(&location, |_| false).unwrap();
        assert_eq!(dropped.len(), 1);
        assert!(read(&location).unwrap().is_empty());
    }

    #[test]
    fn default_roots_are_never_registered_and_a_foreign_version_is_refused() {
        let home = Path::new("/home/me/.local/state");
        assert!(under_default_roots(
            home,
            Path::new("/home/me/.local/state/af/task/local/0123")
        ));
        assert!(!under_default_roots(home, Path::new("/tmp/state")));
        assert!(parse("version = 2\n").is_err());
        assert!(parse("version = 1\n").unwrap().is_empty());
    }
}
