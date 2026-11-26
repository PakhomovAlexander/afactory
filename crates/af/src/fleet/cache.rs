use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nix::fcntl::{Flock, FlockArg};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::context::{Binding, write_record};
use super::transport::MAX_REPLY_BYTES;

pub(crate) const STAGING: &str = ".staging";
const SCHEMA: &str = "af/fleet-cache@2";
pub(crate) const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cached<T> {
    schema: String,
    visibility: Visibility,
    binding_id: String,
    pub source: String,
    pub subject: String,
    pub observed_unix_ms: u64,
    pub data: T,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Visibility {
    Infrastructure,
}

pub(super) struct Cache {
    root: PathBuf,
    binding_id: String,
    scope: String,
}

impl Cache {
    pub(super) fn new(alias: &str, binding: &Binding) -> Option<Self> {
        let bytes = serde_json::to_vec(&(alias, binding)).ok()?;
        Some(Self {
            root: crate::config::state_home()
                .ok()?
                .join("af/fleet-cache")
                .join(alias),
            binding_id: af_telemetry::trace::sha256_digest(&bytes),
            scope: binding.descriptor.scope.clone(),
        })
    }

    pub(super) fn remember<T: Serialize>(&self, subject: &str, now: u64, data: &T) {
        let save = || -> Result<(), String> {
            let roots = crate::storage::Roots::current()?;
            let policy = crate::storage::policy()?;
            let cache_root = &roots.fleet_cache;
            af_store::local_fs::create_private_dir_all(cache_root).map_err(|e| e.to_string())?;
            let held = lock_root(cache_root).map_err(|e| e.to_string())?;
            let staging = cache_root.join(STAGING);
            match af_sandbox::Identity::of(&staging) {
                Ok(identity) => {
                    af_sandbox::remove_tree_at(
                        &*held,
                        std::ffi::OsStr::new(STAGING),
                        Some(identity),
                    )
                    .map_err(|e| e.to_string())?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            let old_bytes = match self.open_directory(&held) {
                Ok(_) => {
                    for subject in ["pools", "allocations"] {
                        let path = self.root.join(format!("{subject}.json"));
                        if read_record::<serde::de::IgnoredAny>(&path)
                            .is_some_and(|record| expired(record.observed_unix_ms, now))
                        {
                            let _ = fs::remove_file(path);
                        }
                    }
                    fs::symlink_metadata(self.root.join(format!("{subject}.json")))
                        .ok()
                        .filter(|metadata| metadata.is_file() && metadata.nlink() == 1)
                        .map_or(0, |metadata| metadata.blocks().saturating_mul(512))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
                Err(error) => return Err(error.to_string()),
            };
            let inventory = crate::storage::inventory(&roots, crate::storage::Detail::Stores);
            let used = inventory.total().saturating_sub(old_bytes);
            if !inventory.problems.is_empty() || used >= policy.max_bytes {
                return Ok(());
            }
            let cached = Cached {
                schema: SCHEMA.into(),
                visibility: Visibility::Infrastructure,
                binding_id: self.binding_id.clone(),
                source: self.scope.clone(),
                subject: subject.into(),
                observed_unix_ms: now,
                data,
            };
            af_store::local_fs::create_private_dir_all(&staging).map_err(|e| e.to_string())?;
            af_sandbox::open_beneath(&*held, Path::new(STAGING)).map_err(|e| e.to_string())?;
            // Keep at most one staged reply on disk across interrupted writers. It is
            // inventoried until the next write or sweep clears it.
            let staged = write_record(&staging, &cached)?;
            let staged_bytes = staged
                .as_file()
                .metadata()
                .map_err(|e| e.to_string())?
                .blocks()
                .saturating_mul(512);
            if used.saturating_add(staged_bytes) <= policy.max_bytes {
                af_store::local_fs::create_private_dir_all(&self.root)
                    .map_err(|e| e.to_string())?;
                self.open_directory(&held).map_err(|e| e.to_string())?;
                staged
                    .persist(self.root.join(format!("{subject}.json")))
                    .map_err(|e| e.error.to_string())?;
            } else {
                drop(staged);
            }
            let _ = fs::remove_dir(staging);
            Ok(())
        };
        // A failed local cache write does not erase a live observation.
        let _ = save();
    }

    pub(super) fn last<T: DeserializeOwned>(&self, subject: &str, now: u64) -> Option<Cached<T>> {
        let cache_root = self.root.parent()?;
        let directory = open_root(cache_root).ok()?;
        self.open_directory(&directory).ok()?;
        let path = self.root.join(format!("{subject}.json"));
        let record: Cached<T> = read_record(&path)?;
        if now.checked_sub(record.observed_unix_ms)? > MAX_AGE_MS {
            // An atomic refresh may have replaced the first read. Recheck under the
            // same lock as writers and the storage sweep before removing the file.
            if let Ok(_held) = lock_root(cache_root)
                && read_record::<serde::de::IgnoredAny>(&path)
                    .is_some_and(|current| expired(current.observed_unix_ms, now))
            {
                let _ = fs::remove_file(path);
            }
            return None;
        }
        (record.schema == SCHEMA
            && record.binding_id == self.binding_id
            && record.source == self.scope
            && record.subject == subject)
            .then_some(record)
    }
    fn open_directory(&self, root: &OwnedFd) -> io::Result<OwnedFd> {
        let name = self
            .root
            .file_name()
            .ok_or_else(|| io::Error::other("missing cache alias"))?;
        af_sandbox::open_beneath(root, Path::new(name))
    }
}

fn read_record<T: DeserializeOwned>(path: &Path) -> Option<Cached<T>> {
    let bytes = af_store::local_fs::read_bounded(path, (MAX_REPLY_BYTES * 2) as u64).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn expired(at: u64, now: u64) -> bool {
    now.saturating_sub(at) > MAX_AGE_MS
}

fn open_root(root: &Path) -> io::Result<OwnedFd> {
    let state_home = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("missing cache state home"))?;
    let anchor = af_sandbox::open_anchor(state_home)?;
    af_sandbox::open_beneath(&anchor, Path::new("af/fleet-cache"))
}

pub(crate) fn lock_root(root: &Path) -> io::Result<Flock<OwnedFd>> {
    Flock::lock(open_root(root)?, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, error)| error.into())
}
