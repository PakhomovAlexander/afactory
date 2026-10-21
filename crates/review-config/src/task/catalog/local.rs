//! One Task package directory on disk, digested and validated exactly as capture would admit it.
//! This is what `af catalog digest` prints; it reads files and nothing else.

use std::collections::BTreeMap;
use std::path::Path;

use super::{PackageBytes, TaskPlanCompiler, exact_version, safe_path};
use crate::lock::{LockError, PackageBounds, located_package_files, package_digest_from_files};
use review_core::task::is_package_name;

/// The most files and bytes one package may hold, the bounds capture enforces.
const MAX_FILES: usize = 4096;
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// The three manifests, exactly one of which a Task package holds.
const MANIFESTS: [&str; 3] = ["pipeline.toml", "worker.toml", "kind.toml"];

/// A local Task package as its catalog pin records it: the manifest's name and version and the
/// digest of every regular file under the package root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTaskPackage {
    pub name: String,
    pub version: String,
    pub digest: String,
}

impl TaskPlanCompiler {
    /// Digest the package directory `root` with the pin recipe and validate its bytes as
    /// `capture_package` does, so a digest is returned only for a package capture would admit
    /// under a pin carrying that digest. Reads files only; writes, runs and installs nothing.
    pub fn local_package(root: &Path) -> Result<LocalTaskPackage, String> {
        Self::local_package_with(root, &mut |_| {})
    }

    /// [`TaskPlanCompiler::local_package`], with `before_open` run between the root's check and
    /// each open of the walk, the root's included; only the race regressions give it a body.
    fn local_package_with(
        root: &Path,
        before_open: &mut dyn FnMut(&Path),
    ) -> Result<LocalTaskPackage, String> {
        use std::os::unix::fs::MetadataExt;
        let shown = root.display();
        // Trailing separators and `.` components are dropped before the check, which otherwise
        // would see through `link/` or `link/.` to the directory the link names.
        let lexical: std::path::PathBuf = root.components().collect();
        let located = match std::fs::symlink_metadata(&lexical) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "Task package {shown} is a symlink; name the package directory itself"
                ));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(format!("Task package {shown} is not a directory"));
            }
            Ok(meta) => (meta.dev(), meta.ino()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("No Task package directory at {shown}"));
            }
            Err(error) => return Err(format!("{shown}: {error}")),
        };
        let label = shown.to_string();
        // The walk opens every entry descriptor-relative and never follows a symlink, the root
        // included, and the root it opens must still be the directory checked above, by device
        // and inode, so a package swapped mid-read, for a symlink or for another directory, is
        // refused rather than read from elsewhere. It also stops at capture's bounds before
        // reading past them, so an oversized package is refused without being held in memory.
        let bounds = PackageBounds {
            files: MAX_FILES,
            bytes: MAX_BYTES,
            file_bytes: MAX_BYTES,
        };
        let files = located_package_files(&label, root, located, bounds, before_open).map_err(
            |error| match error {
            LockError::Symlink { path, .. } => format!(
                "Task package {shown} contains a symlink at {}; a package is regular files only",
                path.display()
            ),
            LockError::UnsupportedFileType { path, .. } => format!(
                "Task package {shown} contains a non-regular file at {}; a package is regular \
                 files only",
                path.display()
            ),
            LockError::UnsupportedPath { path, .. } => format!(
                "Task package {shown} contains a path that is not valid UTF-8 at {}",
                path.display()
            ),
            LockError::TooLarge { .. } => format!(
                "Task package {shown} exceeds the capture bounds of {MAX_FILES} files and \
                 {MAX_BYTES} bytes"
            ),
            other => other.to_string(),
            },
        )?;
        if files.is_empty() {
            return Err(format!("Task package {shown} has no files"));
        }
        if let Some(path) = files.keys().find(|path| !safe_path(path)) {
            return Err(format!(
                "Task package {shown} contains {path:?}, a path a catalog cannot capture"
            ));
        }
        let present: Vec<&str> = MANIFESTS
            .into_iter()
            .filter(|manifest| files.contains_key(*manifest))
            .collect();
        let [manifest] = present.as_slice() else {
            return Err(format!(
                "Task package {shown} must have exactly one pipeline.toml, worker.toml or \
                 kind.toml at its root; found {}",
                if present.is_empty() {
                    "none".to_string()
                } else {
                    present.join(", ")
                }
            ));
        };
        let (name, version) = identity(&files, manifest)
            .map_err(|error| format!("Task package {shown}: {manifest}: {error}"))?;
        if !is_package_name(&name) {
            return Err(format!(
                "Task package {shown}: {manifest} names `{name}`, which is not a package name"
            ));
        }
        if !exact_version(&version) {
            return Err(format!(
                "Task package {shown}: {manifest} version `{version}` is not one exact \
                 major.minor.patch release"
            ));
        }
        let bytes = PackageBytes {
            schema: "af.task-package/1".into(),
            name,
            version,
            digest: package_digest_from_files(&files),
            files,
        };
        Self::parse_package(&bytes).map_err(|error| format!("Task package {shown}: {error}"))?;
        Ok(LocalTaskPackage {
            name: bytes.name,
            version: bytes.version,
            digest: bytes.digest,
        })
    }
}

/// The `name` and `version` a manifest declares; the full parse that follows checks the rest.
fn identity(files: &BTreeMap<String, Vec<u8>>, manifest: &str) -> Result<(String, String), String> {
    let text = std::str::from_utf8(&files[manifest]).map_err(|e| e.to_string())?;
    let value: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let field = |key: &str| {
        value
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("declares no string `{key}`"))
    };
    Ok((field("name")?, field("version")?))
}

#[cfg(test)]
mod tests;
