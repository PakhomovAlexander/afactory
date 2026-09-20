//! Build Cache capture and clone: the explicitly unsafe carry of a Gate's candidate-built
//! output into Worker sandboxes (Worker warm layers, package P2).
//!
//! This is not a second cache mechanism. A Build Cache lives below the same reserved
//! `.af-cache` root as an administrator-approved Cache Snapshot, is traversed with the same
//! descriptor-relative no-follow discipline, gets the same fixed private modes and stripped
//! extended attributes and ACLs, and is removed by the same `remove_materialized_caches` before
//! any seal. What differs is trust: the bytes were produced by candidate code, so the layout is
//! closed harder (regular files only, entry, depth, path and byte limits, no credential-shaped
//! path) and the kernel admits the carry only under the trusted-local policy.
//!
//! Captured bytes are CAS objects addressed by a source `Manifest`, exactly as a Snapshot is,
//! so a resumed Round re-materializes the same cache instead of trusting a directory that a
//! previous process left behind.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use review_core::{BuildCacheKindV1, BuildCacheLimitsV1, validate_build_cache_path_v1};
use review_source_git::{
    Entry, EntryKind, Manifest, decode_path, encode_path, fs_path, materialize,
};
use review_store::Cas;

use crate::Sandbox;
use crate::cache::{
    CACHE_ROOT, CacheEnvironment, CacheError, CacheErrorKind, cache_error, ensure_cache_root,
    normalize_materialized_metadata,
};

fn relative_root(kind: BuildCacheKindV1) -> PathBuf {
    Path::new(CACHE_ROOT).join(kind.as_str())
}

/// The one environment variable a kind sets, pointed at the sandbox-local clone. The local
/// value is the absolute host path of a trusted-local sandbox; the container value is the fixed
/// `/work` path, recorded for symmetry with Cache Snapshots even though a container Gate can
/// never declare a build cache.
pub fn build_cache_environment(kind: BuildCacheKindV1, sandbox_root: &Path) -> CacheEnvironment {
    CacheEnvironment {
        local: vec![(
            kind.environment_variable().into(),
            sandbox_root.join(relative_root(kind)).display().to_string(),
        )],
        container: vec![(
            kind.environment_variable().into(),
            format!("/work/{CACHE_ROOT}/{}", kind.as_str()),
        )],
    }
}

/// Create the empty, private directory a Gate check will build into. The Gate's environment
/// points the build tool at it; whatever the check leaves there is what capture inspects.
pub fn prepare_build_cache_root(
    kind: BuildCacheKindV1,
    sandbox: &Sandbox,
) -> Result<PathBuf, CacheError> {
    ensure_cache_root(sandbox)?;
    let target = sandbox.root().join(relative_root(kind));
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(cache_error(
            CacheErrorKind::MaterializationFailed,
            format!(
                "build cache path `{}` already exists",
                relative_root(kind).display()
            ),
        ));
    }
    std::fs::create_dir(&target).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("creating build cache root: {error}"),
        )
    })?;
    normalize_materialized_metadata(&target, true, 0o700).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("normalizing build cache root metadata: {error}"),
        )
    })?;
    Ok(target)
}

/// One captured Build Cache: its content manifest published to the CAS, and how long the
/// capture took. The manifest names only `file` and `executable` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedBuildCache {
    pub kind: BuildCacheKindV1,
    pub manifest: Manifest,
    pub manifest_id: String,
    pub content_digest: String,
    /// Regular files in the manifest.
    pub entries: u64,
    pub bytes: u64,
    pub started_unix_ms: u64,
    /// Host-observed time spent traversing, hashing and publishing the tree.
    pub capture_ms: u64,
}

/// One cloned Build Cache inside a Worker sandbox, with the host-observed time it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedBuildCache {
    pub kind: BuildCacheKindV1,
    pub entries: u64,
    pub bytes: u64,
    pub started_unix_ms: u64,
    pub materialization_ms: u64,
}

struct PlannedFile {
    source: std::fs::File,
    encoded: String,
    size: u64,
    executable: bool,
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

/// A closed-layout refusal names either a bound or unsafe content.
fn layout_error(message: String) -> CacheError {
    let bound = message.contains("limit");
    cache_error(
        if bound {
            CacheErrorKind::LimitExceeded
        } else {
            CacheErrorKind::UnsafeContent
        },
        message,
    )
}

/// Capture the Gate's declared cache directory after its checks ran. Every entry is opened
/// descriptor-relative without following links; a symlink, FIFO, socket, device or any other
/// non-regular entry refuses the whole capture, as does any bound the tree exceeds. Regular
/// files stream into the CAS through a fixed buffer and are re-checked against their opened
/// size, so a file that grows or is replaced during capture is refused rather than filed.
pub fn capture_build_cache(
    kind: BuildCacheKindV1,
    sandbox: &Sandbox,
    limits: &BuildCacheLimitsV1,
    cas: &Cas,
) -> Result<CapturedBuildCache, CacheError> {
    limits
        .validate()
        .map_err(|error| cache_error(CacheErrorKind::LimitExceeded, error))?;
    let started_unix_ms = unix_now_ms();
    let clock = Instant::now();
    let source = sandbox.root().join(relative_root(kind));
    let (files, bytes) = walk(kind, &source, limits)?;
    if files.is_empty() {
        return Err(cache_error(
            CacheErrorKind::SourceUnavailable,
            format!("{kind} build cache contains no files"),
        ));
    }
    let entries = review_parallel::try_map_owned(files, |file| publish(file, cas))?;
    let manifest = Manifest::new(entries)
        .map_err(|error| cache_error(CacheErrorKind::UnsafeContent, error.to_string()))?;
    let manifest_id = cas
        .put_json(&serde_json::to_value(&manifest).map_err(|error| {
            cache_error(CacheErrorKind::MaterializationFailed, error.to_string())
        })?)
        .map_err(|error| cache_error(CacheErrorKind::MaterializationFailed, error.to_string()))?;
    let entries = u64::try_from(manifest.entries.len())
        .map_err(|_| cache_error(CacheErrorKind::LimitExceeded, "build cache entry overflow"))?;
    Ok(CapturedBuildCache {
        kind,
        content_digest: manifest.content_digest(),
        manifest,
        manifest_id,
        entries,
        bytes,
        started_unix_ms,
        capture_ms: clock.elapsed().as_millis() as u64,
    })
}

#[cfg(unix)]
fn walk(
    kind: BuildCacheKindV1,
    source: &Path,
    limits: &BuildCacheLimitsV1,
) -> Result<(Vec<PlannedFile>, u64), CacheError> {
    use nix::dir::Dir;
    use nix::fcntl::OFlag;
    use nix::sys::stat::{Mode as NixMode, SFlag, fstat};

    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
    let root =
        Dir::open(source, flags | OFlag::O_DIRECTORY, NixMode::empty()).map_err(|error| {
            cache_error(
                CacheErrorKind::SourceUnavailable,
                format!(
                    "{kind} build cache root {} must be an accessible real directory: {error}",
                    source.display()
                ),
            )
        })?;
    let mut files = Vec::new();
    let mut bytes = 0_u64;
    let mut entry_count = 0_u64;
    let mut level = vec![(root, PathBuf::new(), 0_u32)];
    while let Some((mut directory, parent, depth)) = level.pop() {
        let mut names = Vec::new();
        for entry in directory.iter() {
            let entry = entry.map_err(|error| {
                cache_error(
                    CacheErrorKind::SourceUnavailable,
                    format!("reading build cache directory: {error}"),
                )
            })?;
            let raw = entry.file_name().to_bytes();
            if matches!(raw, b"." | b"..") {
                continue;
            }
            entry_count = entry_count.checked_add(1).ok_or_else(|| {
                cache_error(
                    CacheErrorKind::LimitExceeded,
                    "build cache entry count overflow",
                )
            })?;
            if entry_count > limits.max_entries {
                return Err(cache_error(
                    CacheErrorKind::LimitExceeded,
                    format!(
                        "{kind} build cache exceeds its {} filesystem-entry limit",
                        limits.max_entries
                    ),
                ));
            }
            names.push(
                std::str::from_utf8(raw)
                    .map_err(|_| {
                        cache_error(
                            CacheErrorKind::UnsafeContent,
                            format!("{kind} build cache paths must be portable UTF-8"),
                        )
                    })?
                    .to_string(),
            );
        }
        names.sort();
        for name in names.into_iter().rev() {
            let relative = parent.join(&name);
            let relative_bytes = relative.as_os_str().as_encoded_bytes().to_vec();
            if relative_bytes.len() as u64 > limits.max_path_bytes {
                return Err(cache_error(
                    CacheErrorKind::LimitExceeded,
                    format!(
                        "{kind} build cache path exceeds its {} byte limit",
                        limits.max_path_bytes
                    ),
                ));
            }
            if depth.saturating_add(1) > limits.max_depth {
                return Err(cache_error(
                    CacheErrorKind::LimitExceeded,
                    format!(
                        "{kind} build cache tree exceeds its {} directory depth limit",
                        limits.max_depth
                    ),
                ));
            }
            validate_build_cache_path_v1(kind, &relative_bytes, limits).map_err(layout_error)?;
            let descriptor = nix::fcntl::openat(&directory, name.as_str(), flags, NixMode::empty())
                .map_err(|error| {
                    cache_error(
                        CacheErrorKind::UnsafeContent,
                        format!(
                            "opening build cache path {} without following links or special files: {error}",
                            relative.display()
                        ),
                    )
                })?;
            let stat = fstat(&descriptor).map_err(|error| {
                cache_error(
                    CacheErrorKind::SourceUnavailable,
                    format!(
                        "inspecting build cache path {}: {error}",
                        relative.display()
                    ),
                )
            })?;
            let file_type = SFlag::from_bits_truncate(stat.st_mode);
            if file_type == SFlag::S_IFDIR {
                let child = Dir::from_fd(descriptor).map_err(|error| {
                    cache_error(
                        CacheErrorKind::SourceUnavailable,
                        format!("opening build cache directory: {error}"),
                    )
                })?;
                level.push((child, relative, depth.saturating_add(1)));
                continue;
            }
            if file_type != SFlag::S_IFREG {
                return Err(cache_error(
                    CacheErrorKind::UnsafeContent,
                    format!(
                        "build cache path {} is not a regular file or directory; links and special files are refused",
                        relative.display()
                    ),
                ));
            }
            let size = u64::try_from(stat.st_size).map_err(|_| {
                cache_error(
                    CacheErrorKind::UnsafeContent,
                    "build cache file has negative size",
                )
            })?;
            bytes = bytes.checked_add(size).ok_or_else(|| {
                cache_error(
                    CacheErrorKind::LimitExceeded,
                    "build cache byte count overflow",
                )
            })?;
            if bytes > limits.max_bytes {
                return Err(cache_error(
                    CacheErrorKind::LimitExceeded,
                    format!(
                        "{kind} build cache exceeds its {} byte limit",
                        limits.max_bytes
                    ),
                ));
            }
            let executable = NixMode::from_bits_truncate(stat.st_mode)
                .intersects(NixMode::S_IXUSR | NixMode::S_IXGRP | NixMode::S_IXOTH);
            files.push(PlannedFile {
                source: std::fs::File::from(descriptor),
                encoded: encode_path(&relative_bytes),
                size,
                executable,
            });
        }
    }
    Ok((files, bytes))
}

#[cfg(not(unix))]
fn walk(
    _kind: BuildCacheKindV1,
    _source: &Path,
    _limits: &BuildCacheLimitsV1,
) -> Result<(Vec<PlannedFile>, u64), CacheError> {
    Err(cache_error(
        CacheErrorKind::MaterializationFailed,
        "Build Cache capture requires descriptor-relative no-follow filesystem APIs",
    ))
}

/// Stream one opened regular file into the CAS and file it under the size it was opened with.
/// Every pass the CAS takes over the descriptor is bounded to one byte more than that size, so
/// a file that grows during capture costs at most one extra byte per pass, is refused as a
/// concurrent change, and is never filed under the size the walk admitted.
fn publish(file: PlannedFile, cas: &Cas) -> Result<Entry, CacheError> {
    let PlannedFile {
        source,
        encoded,
        size,
        executable,
    } = file;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bounded = BoundedSource::new(source, size.saturating_add(1));
    let (content, published) = cas
        .put_reader_with_buffer(&mut bounded, &mut buffer)
        .map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("publishing build cache file {encoded}: {error}"),
            )
        })?;
    let metadata = bounded.source.metadata().map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("inspecting open build cache file {encoded}: {error}"),
        )
    })?;
    if published != size || !metadata.is_file() || metadata.len() != size {
        return Err(cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("build cache file {encoded} changed during capture"),
        ));
    }
    Ok(Entry {
        path: encoded,
        kind: if executable {
            EntryKind::Executable
        } else {
            EntryKind::File
        },
        content,
        size,
    })
}

/// An opened descriptor the CAS may read and rewind, never past its admitted bound. The bound
/// holds on every pass: hashing, publication and any re-read after a rewind.
struct BoundedSource {
    source: std::fs::File,
    limit: u64,
    position: u64,
}

impl BoundedSource {
    fn new(source: std::fs::File, limit: u64) -> Self {
        Self {
            source,
            limit,
            position: 0,
        }
    }
}

impl std::io::Read for BoundedSource {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.position);
        if remaining == 0 || buffer.is_empty() {
            return Ok(0);
        }
        let window = usize::try_from(remaining)
            .map_or(buffer.len(), |remaining| buffer.len().min(remaining));
        let read = self.source.read(&mut buffer[..window])?;
        self.position = self.position.saturating_add(read as u64);
        Ok(read)
    }
}

impl std::io::Seek for BoundedSource {
    fn seek(&mut self, target: std::io::SeekFrom) -> std::io::Result<u64> {
        // The CAS only rewinds between its passes; any other movement would let a caller
        // read past the bound by seeking around it.
        match target {
            std::io::SeekFrom::Start(0) => {
                self.source.seek(std::io::SeekFrom::Start(0))?;
                self.position = 0;
                Ok(0)
            }
            other => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("bounded build cache source only rewinds, not {other:?}"),
            )),
        }
    }
}

/// Clone one captured Build Cache into a Worker sandbox from the CAS. Every manifest entry is
/// re-checked against the closed layout and the limits before a byte is written: the manifest
/// is candidate-derived data, and a stored artifact earns no more trust than a fresh capture.
pub fn materialize_build_cache(
    kind: BuildCacheKindV1,
    manifest: &Manifest,
    sandbox: &Sandbox,
    limits: &BuildCacheLimitsV1,
    cas: &Cas,
) -> Result<MaterializedBuildCache, CacheError> {
    limits
        .validate()
        .map_err(|error| cache_error(CacheErrorKind::LimitExceeded, error))?;
    manifest
        .validate()
        .map_err(|error| cache_error(CacheErrorKind::UnsafeContent, error.to_string()))?;
    if manifest.entries.is_empty() {
        return Err(cache_error(
            CacheErrorKind::SourceUnavailable,
            format!("{kind} build cache manifest names no files"),
        ));
    }
    let entries = u64::try_from(manifest.entries.len())
        .map_err(|_| cache_error(CacheErrorKind::LimitExceeded, "build cache entry overflow"))?;
    if entries > limits.max_entries {
        return Err(cache_error(
            CacheErrorKind::LimitExceeded,
            format!(
                "{kind} build cache exceeds its {} filesystem-entry limit",
                limits.max_entries
            ),
        ));
    }
    let mut bytes = 0_u64;
    for entry in &manifest.entries {
        if entry.kind == EntryKind::Symlink {
            return Err(cache_error(
                CacheErrorKind::UnsafeContent,
                format!("build cache manifest names a symlink at {}", entry.path),
            ));
        }
        if !review_core::is_digest(&entry.content) {
            return Err(cache_error(
                CacheErrorKind::UnsafeContent,
                format!(
                    "build cache manifest entry {} has no content identity",
                    entry.path
                ),
            ));
        }
        validate_build_cache_path_v1(kind, &decode_path(&entry.path), limits)
            .map_err(layout_error)?;
        bytes = bytes.checked_add(entry.size).ok_or_else(|| {
            cache_error(
                CacheErrorKind::LimitExceeded,
                "build cache byte count overflow",
            )
        })?;
        if bytes > limits.max_bytes {
            return Err(cache_error(
                CacheErrorKind::LimitExceeded,
                format!(
                    "{kind} build cache exceeds its {} byte limit",
                    limits.max_bytes
                ),
            ));
        }
    }
    ensure_cache_root(sandbox)?;
    let target = sandbox.root().join(relative_root(kind));
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(cache_error(
            CacheErrorKind::MaterializationFailed,
            format!(
                "build cache path `{}` already exists",
                relative_root(kind).display()
            ),
        ));
    }
    let started_unix_ms = unix_now_ms();
    let clock = Instant::now();
    let result = materialize(manifest, cas, &target)
        .map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("cloning build cache from the CAS: {error}"),
            )
        })
        .and_then(|()| normalize_tree(&target, manifest));
    if let Err(error) = result {
        crate::restore_writable_dirs(&target);
        let _ = std::fs::remove_dir_all(&target);
        return Err(error);
    }
    Ok(MaterializedBuildCache {
        kind,
        entries,
        bytes,
        started_unix_ms,
        materialization_ms: clock.elapsed().as_millis() as u64,
    })
}

/// Fixed modes and stripped metadata on every cloned object: directories `0700`, files `0600`
/// or `0700` by the manifest's executable bit, extended attributes and ACLs removed.
fn normalize_tree(target: &Path, manifest: &Manifest) -> Result<(), CacheError> {
    let mut directories = BTreeSet::from([target.to_path_buf()]);
    let mut files = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        let relative = fs_path(&entry.path);
        let mut parent = relative.parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            directories.insert(target.join(directory));
            parent = directory.parent();
        }
        let mode = if entry.kind == EntryKind::Executable {
            0o700
        } else {
            0o600
        };
        files.push((target.join(relative), mode));
    }
    review_parallel::try_for_each_owned(files, |(path, mode)| {
        normalize_materialized_metadata(&path, false, mode).map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!(
                    "normalizing build cache file metadata for {}: {error}",
                    path.display()
                ),
            )
        })
    })?;
    for directory in directories.iter().rev() {
        normalize_materialized_metadata(directory, true, 0o700).map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!(
                    "normalizing build cache directory metadata for {}: {error}",
                    directory.display()
                ),
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_source_git::digest_reader_with_buffer;
    use std::io::{Read, Seek, SeekFrom};

    fn planned(directory: &Path, bytes: &[u8], admitted: u64) -> PlannedFile {
        let path = directory.join("debug/deps/libfixture.rlib");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        PlannedFile {
            source: std::fs::File::open(&path).unwrap(),
            encoded: "debug/deps/libfixture.rlib".into(),
            size: admitted,
            executable: false,
        }
    }

    fn digest_of(bytes: &[u8]) -> String {
        let mut buffer = vec![0_u8; 64];
        digest_reader_with_buffer(&mut std::io::Cursor::new(bytes), &mut buffer)
            .unwrap()
            .0
    }

    #[test]
    fn a_file_that_grew_after_the_walk_is_refused_and_never_filed_under_its_full_content() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let grown = b"compiled-and-then-some-more";
        let error = publish(planned(directory.path(), grown, 8), &cas).unwrap_err();
        assert_eq!(error.kind(), CacheErrorKind::ConcurrentChange);
        assert!(
            !cas.contains(&digest_of(grown)),
            "the grown content must not be published"
        );
        assert!(
            !cas.contains(&digest_of(&grown[..8])),
            "the admitted prefix is not the file either"
        );
    }

    #[test]
    fn a_file_that_shrank_after_the_walk_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let error = publish(planned(directory.path(), b"short", 64), &cas).unwrap_err();
        assert_eq!(error.kind(), CacheErrorKind::ConcurrentChange);
        // The CAS files what it actually read under that content's own identity, as the safe
        // cache path does; the refusal is what keeps it out of the manifest.
        assert!(cas.contains(&digest_of(b"short")));
    }

    #[test]
    fn a_stable_file_is_filed_under_its_opened_size() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let entry = publish(planned(directory.path(), b"compiled", 8), &cas).unwrap();
        assert_eq!(entry.size, 8);
        assert_eq!(entry.content, digest_of(b"compiled"));
        assert!(cas.contains(&entry.content));
    }

    #[test]
    fn the_bounded_source_never_reads_past_its_limit_even_after_a_rewind() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file");
        std::fs::write(&path, b"0123456789").unwrap();
        let mut bounded = BoundedSource::new(std::fs::File::open(&path).unwrap(), 4);
        let mut first = Vec::new();
        bounded.read_to_end(&mut first).unwrap();
        assert_eq!(first, b"0123");
        bounded.rewind().unwrap();
        let mut second = Vec::new();
        bounded.read_to_end(&mut second).unwrap();
        assert_eq!(second, b"0123");
        assert!(bounded.seek(SeekFrom::End(0)).is_err());
        assert!(bounded.seek(SeekFrom::Start(1)).is_err());
    }
}
