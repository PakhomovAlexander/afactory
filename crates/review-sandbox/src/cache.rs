//! Bounded, credential-free package cache snapshots for writable Gate clones.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use review_core::{
    CacheManifestEntryV1, CacheManifestV1, CachePathEncodingV1, RunCacheKindV5,
    validate_cache_path_v1,
};
use review_source_git::{digest_reader_with_buffer, encode_path};
use review_store::Cas;

use crate::Sandbox;

use review_core::{
    MAX_CACHE_BYTES_V1 as MAX_CACHE_BYTES, MAX_CACHE_COPY_BYTES_V1 as MAX_CACHE_COPY_BYTES,
    MAX_CACHE_ENTRIES_V1 as MAX_CACHE_FILES,
};

/// The one reserved sandbox path every cache kind lives below: administrator-approved Cache
/// Snapshots and explicitly unsafe Build Caches alike, so one removal before seal covers both.
pub(crate) const CACHE_ROOT: &str = ".af-cache";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheErrorKind {
    PolicyUnavailable,
    SourceUnavailable,
    UnsafeContent,
    LimitExceeded,
    CopyLimitExceeded,
    ConcurrentChange,
    MaterializationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheError {
    kind: CacheErrorKind,
    detail: String,
}

impl CacheError {
    pub fn new(kind: CacheErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub const fn kind(&self) -> CacheErrorKind {
        self.kind
    }

    /// Operator detail is deliberately separate from Display: it may contain a machine-local
    /// source path and must never be copied into a durable RunReport.
    pub fn operator_detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self.kind {
            CacheErrorKind::PolicyUnavailable => "cache policy unavailable",
            CacheErrorKind::SourceUnavailable => "cache source unavailable",
            CacheErrorKind::UnsafeContent => "cache source refused by safety policy",
            CacheErrorKind::LimitExceeded => "cache source exceeds configured limits",
            CacheErrorKind::CopyLimitExceeded => {
                "cache source cannot be materialized within its copy limit"
            }
            CacheErrorKind::ConcurrentChange => "cache source changed during snapshot",
            CacheErrorKind::MaterializationFailed => "cache materialization failed",
        })
    }
}

impl std::error::Error for CacheError {}

pub(crate) fn cache_error(kind: CacheErrorKind, detail: impl Into<String>) -> CacheError {
    CacheError::new(kind, detail)
}

/// The reserved cache root of one sandbox, created on first use with the fixed private mode.
/// A Subject that already contains the reserved path is refused, exactly as `materialize_cache`
/// refuses it, so candidate content can never pre-seed a cache directory.
pub(crate) fn ensure_cache_root(sandbox: &Sandbox) -> Result<PathBuf, CacheError> {
    let cache_root = sandbox.root().join(CACHE_ROOT);
    match std::fs::symlink_metadata(&cache_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if sandbox.baseline().entries.iter().any(|entry| {
                entry.path == CACHE_ROOT || entry.path.starts_with(&format!("{CACHE_ROOT}/"))
            }) {
                return Err(cache_error(
                    CacheErrorKind::MaterializationFailed,
                    format!("Subject already contains reserved cache path `{CACHE_ROOT}`"),
                ));
            }
            return Ok(cache_root);
        }
        Ok(_) => {
            return Err(cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("Subject already contains reserved cache path `{CACHE_ROOT}`"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("inspecting sandbox cache root: {error}"),
            ));
        }
    }
    std::fs::create_dir(&cache_root).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("creating sandbox cache root: {error}"),
        )
    })?;
    normalize_materialized_metadata(&cache_root, true, 0o700).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("normalizing sandbox cache root metadata: {error}"),
        )
    })?;
    Ok(cache_root)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CacheKind {
    Cargo,
}

impl CacheKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
        }
    }

    fn relative_root(self) -> PathBuf {
        Path::new(CACHE_ROOT).join(self.name())
    }

    fn validate_relative(self, relative: &Path, is_file: bool) -> Result<(), CacheError> {
        let relative = relative.to_str().ok_or_else(|| {
            cache_error(
                CacheErrorKind::UnsafeContent,
                "Cargo cache paths must be portable UTF-8",
            )
        })?;
        validate_cache_path_v1(manifest_kind(self), relative.as_bytes(), is_file)
            .map_err(|error| cache_error(CacheErrorKind::UnsafeContent, error))
    }

    pub fn environment(self, sandbox_root: &Path) -> CacheEnvironment {
        match self {
            Self::Cargo => CacheEnvironment {
                local: vec![
                    (
                        "CARGO_HOME".into(),
                        sandbox_root
                            .join(self.relative_root())
                            .display()
                            .to_string(),
                    ),
                    ("CARGO_NET_OFFLINE".into(), "true".into()),
                ],
                container: vec![
                    ("CARGO_HOME".into(), "/work/.af-cache/cargo".into()),
                    ("CARGO_NET_OFFLINE".into(), "true".into()),
                ],
            },
        }
    }
}

fn manifest_kind(kind: CacheKind) -> RunCacheKindV5 {
    match kind {
        CacheKind::Cargo => RunCacheKindV5::Cargo,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    pub max_bytes: u64,
    pub max_files: u64,
    pub max_copy_bytes: u64,
}

impl CacheLimits {
    pub fn validate(self) -> Result<(), String> {
        if self.max_bytes == 0
            || self.max_files == 0
            || self.max_copy_bytes == 0
            || self.max_copy_bytes > self.max_bytes
            || self.max_bytes > MAX_CACHE_BYTES
            || self.max_files > MAX_CACHE_FILES
            || self.max_copy_bytes > MAX_CACHE_COPY_BYTES
        {
            return Err(format!(
                "cache limits must be positive and bounded by {MAX_CACHE_BYTES} bytes, {MAX_CACHE_FILES} filesystem entries, and {MAX_CACHE_COPY_BYTES} copied bytes"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSource {
    pub kind: CacheKind,
    pub source: PathBuf,
    pub limits: CacheLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMaterialization {
    Reflink,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSnapshot {
    pub kind: CacheKind,
    pub source_digest: String,
    pub bytes: u64,
    pub files: u64,
    pub materialization: CacheMaterialization,
    pub started_unix_ms: u64,
    /// Host-observed time spent resolving and hashing the bounded source tree.
    pub lookup_ms: u64,
    /// Host-observed time spent making the admitted bytes available in the sandbox.
    pub materialization_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEnvironment {
    pub local: Vec<(String, String)>,
    pub container: Vec<(String, String)>,
}

#[derive(Clone)]
struct PlannedFile {
    source: Arc<std::fs::File>,
    relative: PathBuf,
    encoded: String,
    size: u64,
}

struct Preflight {
    directories: Vec<PathBuf>,
    files: Vec<PlannedFile>,
    bytes: u64,
}

/// Materialize one requested cache inside the Gate clone after provider admission.
pub fn materialize_cache(
    source: &CacheSource,
    sandbox: &Sandbox,
    cas: &Cas,
) -> Result<CacheSnapshot, CacheError> {
    let started_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    let lookup_started = std::time::Instant::now();
    source
        .limits
        .validate()
        .map_err(|error| cache_error(CacheErrorKind::LimitExceeded, error))?;
    if !source.source.is_absolute() {
        return Err(cache_error(
            CacheErrorKind::SourceUnavailable,
            "cache source must resolve to an absolute directory",
        ));
    }
    let preflight = preflight(source.kind, &source.source, source.limits)?;
    let lookup_ms = lookup_started.elapsed().as_millis() as u64;
    let cache_root = sandbox.root().join(CACHE_ROOT);
    if std::fs::symlink_metadata(&cache_root).is_ok() {
        return Err(cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("Subject already contains reserved cache path `{CACHE_ROOT}`"),
        ));
    }
    std::fs::create_dir(&cache_root).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("creating sandbox cache root: {error}"),
        )
    })?;
    normalize_materialized_metadata(&cache_root, true, 0o700).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("normalizing sandbox cache root metadata: {error}"),
        )
    })?;
    let target = sandbox.root().join(source.kind.relative_root());

    let materialization_started = std::time::Instant::now();
    let result = materialize_preflight(source, &preflight, &target, cas).map(|mut snapshot| {
        snapshot.lookup_ms = lookup_ms;
        snapshot.materialization_ms = materialization_started.elapsed().as_millis() as u64;
        snapshot.started_unix_ms = started_unix_ms;
        snapshot
    });
    if result.is_err() {
        let _ = remove_cache_root(&cache_root);
    }
    result
}

#[cfg(unix)]
fn preflight(kind: CacheKind, source: &Path, limits: CacheLimits) -> Result<Preflight, CacheError> {
    use nix::dir::Dir;
    use nix::fcntl::OFlag;
    use nix::sys::stat::{Mode as NixMode, SFlag, fstat};

    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut bytes = 0_u64;
    let mut entry_count = 0_u64;
    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
    let root =
        Dir::open(source, flags | OFlag::O_DIRECTORY, NixMode::empty()).map_err(|error| {
            cache_error(
                CacheErrorKind::SourceUnavailable,
                format!(
                    "{} cache source {} must be an accessible real directory: {error}",
                    kind.name(),
                    source.display()
                ),
            )
        })?;
    let mut level = vec![(root, PathBuf::new(), 0_u32)];
    while let Some((mut directory, parent, depth)) = level.pop() {
        if depth > 128 {
            return Err(cache_error(
                CacheErrorKind::LimitExceeded,
                "cache tree exceeds the 128-directory depth limit",
            ));
        }
        let mut names = Vec::new();
        for entry in directory.iter() {
            let entry = entry.map_err(|error| {
                cache_error(
                    CacheErrorKind::SourceUnavailable,
                    format!("reading cache directory: {error}"),
                )
            })?;
            let raw = entry.file_name().to_bytes();
            if matches!(raw, b"." | b"..") {
                continue;
            }
            entry_count = entry_count.checked_add(1).ok_or_else(|| {
                cache_error(CacheErrorKind::LimitExceeded, "cache entry count overflow")
            })?;
            if entry_count > limits.max_files {
                return Err(cache_error(
                    CacheErrorKind::LimitExceeded,
                    format!(
                        "{} cache exceeds its {} filesystem-entry limit",
                        kind.name(),
                        limits.max_files
                    ),
                ));
            }
            names.push(
                std::str::from_utf8(raw)
                    .map_err(|_| {
                        cache_error(
                            CacheErrorKind::UnsafeContent,
                            "Cargo cache paths must be portable UTF-8",
                        )
                    })?
                    .to_string(),
            );
        }
        names.sort();
        for name in names.into_iter().rev() {
            let relative = parent.join(&name);
            if relative.as_os_str().as_encoded_bytes().len() > 4096 {
                return Err(cache_error(
                    CacheErrorKind::LimitExceeded,
                    "cache path exceeds the 4096-byte portability limit",
                ));
            }
            kind.validate_relative(&relative, false)?;
            let descriptor = nix::fcntl::openat(&directory, name.as_str(), flags, NixMode::empty())
                .map_err(|error| {
                    cache_error(
                        CacheErrorKind::UnsafeContent,
                        format!(
                            "opening cache path {} without following links: {error}",
                            relative.display()
                        ),
                    )
                })?;
            let stat = fstat(&descriptor).map_err(|error| {
                cache_error(
                    CacheErrorKind::SourceUnavailable,
                    format!("inspecting cache path {}: {error}", relative.display()),
                )
            })?;
            let file_type = SFlag::from_bits_truncate(stat.st_mode);
            if file_type == SFlag::S_IFDIR {
                directories.push(relative);
                let child = Dir::from_fd(descriptor).map_err(|error| {
                    cache_error(
                        CacheErrorKind::SourceUnavailable,
                        format!("opening cache directory: {error}"),
                    )
                })?;
                level.push((child, parent.join(name), depth + 1));
                continue;
            }
            if file_type != SFlag::S_IFREG {
                return Err(cache_error(
                    CacheErrorKind::UnsafeContent,
                    format!(
                        "cache path {} is not a regular file or directory; links are never followed",
                        relative.display()
                    ),
                ));
            }
            kind.validate_relative(&relative, true)?;
            let size = u64::try_from(stat.st_size).map_err(|_| {
                cache_error(
                    CacheErrorKind::UnsafeContent,
                    "cache file has negative size",
                )
            })?;
            bytes = bytes.checked_add(size).ok_or_else(|| {
                cache_error(CacheErrorKind::LimitExceeded, "cache byte count overflow")
            })?;
            if bytes > limits.max_bytes {
                return Err(cache_error(
                    CacheErrorKind::LimitExceeded,
                    format!(
                        "{} cache exceeds its {} byte limit",
                        kind.name(),
                        limits.max_bytes
                    ),
                ));
            }
            let relative_text = relative.to_str().ok_or_else(|| {
                cache_error(
                    CacheErrorKind::UnsafeContent,
                    "Cargo cache paths must be portable UTF-8",
                )
            })?;
            files.push(PlannedFile {
                source: Arc::new(std::fs::File::from(descriptor)),
                relative: relative.clone(),
                encoded: encode_path(relative_text.as_bytes()),
                size,
            });
        }
    }
    if files.is_empty() {
        return Err(cache_error(
            CacheErrorKind::SourceUnavailable,
            format!("{} cache source contains no files", kind.name()),
        ));
    }
    directories.sort();
    directories.dedup();
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(Preflight {
        directories,
        files,
        bytes,
    })
}

#[cfg(not(unix))]
fn preflight(
    _kind: CacheKind,
    _source: &Path,
    _limits: CacheLimits,
) -> Result<Preflight, CacheError> {
    Err(cache_error(
        CacheErrorKind::MaterializationFailed,
        "Cache Snapshots require descriptor-relative no-follow filesystem APIs",
    ))
}

fn materialize_preflight(
    source: &CacheSource,
    preflight: &Preflight,
    target: &Path,
    cas: &Cas,
) -> Result<CacheSnapshot, CacheError> {
    let parent = target.parent().ok_or_else(|| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            "sandbox cache target has no parent",
        )
    })?;
    let probe = parent.join(".reflink-probe");
    let materialization = match reflink_from_handle(&preflight.files[0].source, &probe) {
        Ok(()) => {
            std::fs::remove_file(&probe).map_err(|error| {
                cache_error(
                    CacheErrorKind::MaterializationFailed,
                    format!("removing cache reflink probe: {error}"),
                )
            })?;
            CacheMaterialization::Reflink
        }
        Err(_) if preflight.bytes <= source.limits.max_copy_bytes => {
            let _ = std::fs::remove_file(&probe);
            CacheMaterialization::Copy
        }
        Err(error) => {
            let _ = std::fs::remove_file(&probe);
            return Err(cache_error(
                CacheErrorKind::CopyLimitExceeded,
                format!(
                    "{} cache needs a {} byte plain copy after reflink preflight failed ({error}), exceeding its {} byte copy limit",
                    source.kind.name(),
                    preflight.bytes,
                    source.limits.max_copy_bytes
                ),
            ));
        }
    };
    std::fs::create_dir(target).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("creating sandbox cache target: {error}"),
        )
    })?;
    normalize_materialized_metadata(target, true, 0o700).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("normalizing sandbox cache target metadata: {error}"),
        )
    })?;
    for relative in &preflight.directories {
        let directory = target.join(relative);
        std::fs::create_dir_all(&directory).map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("creating cache directory {}: {error}", directory.display()),
            )
        })?;
        normalize_materialized_metadata(&directory, true, 0o700).map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("normalizing cache directory metadata: {error}"),
            )
        })?;
    }

    let mut entries = review_parallel::try_map_owned(preflight.files.clone(), |file| {
        materialize_file(file, target.to_path_buf(), materialization)
    })?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = CacheManifestV1 {
        kind: manifest_kind(source.kind),
        path_encoding: CachePathEncodingV1::PercentV2,
        entries,
    };
    manifest.validate().map_err(|error| {
        cache_error(
            CacheErrorKind::UnsafeContent,
            format!("validating materialized CacheManifest@1: {error}"),
        )
    })?;
    let source_digest = cas
        .put_json(&serde_json::to_value(&manifest).map_err(|error| {
            cache_error(CacheErrorKind::MaterializationFailed, error.to_string())
        })?)
        .map_err(|error| cache_error(CacheErrorKind::MaterializationFailed, error.to_string()))?;
    Ok(CacheSnapshot {
        kind: source.kind,
        source_digest,
        bytes: preflight.bytes,
        files: u64::try_from(preflight.files.len())
            .map_err(|_| cache_error(CacheErrorKind::LimitExceeded, "cache file count overflow"))?,
        materialization,
        started_unix_ms: 0,
        lookup_ms: 0,
        materialization_ms: 0,
    })
}

fn materialize_file(
    file: PlannedFile,
    target_root: PathBuf,
    materialization: CacheMaterialization,
) -> Result<CacheManifestEntryV1, CacheError> {
    let target = target_root.join(&file.relative);
    if stable_size(&file.source)? != file.size {
        return Err(cache_error(
            CacheErrorKind::ConcurrentChange,
            format!(
                "cache source {} changed before materialization",
                file.relative.display()
            ),
        ));
    }
    match materialization {
        CacheMaterialization::Reflink => {
            reflink_from_handle(&file.source, &target).map_err(|error| {
                cache_error(
                    CacheErrorKind::MaterializationFailed,
                    format!("reflinking {}: {error}", file.relative.display()),
                )
            })?;
        }
        CacheMaterialization::Copy => {
            copy_exact_bounded(&file.source, &target, file.size)?;
        }
    }
    normalize_materialized_metadata(&target, false, 0o600).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!(
                "normalizing cache file metadata for {}: {error}",
                file.relative.display()
            ),
        )
    })?;
    let mut source_buffer = vec![0_u8; 64 * 1024];
    let mut target_buffer = vec![0_u8; 64 * 1024];
    let (source_digest, source_size) =
        digest_stable_file(&file.source, file.size, &mut source_buffer)?;
    let mut target_file = std::fs::File::open(&target).map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("opening materialized cache file: {error}"),
        )
    })?;
    let mut bounded_target = (&mut target_file).take(file.size.saturating_add(1));
    let (target_digest, target_size) =
        digest_reader_with_buffer(&mut bounded_target, &mut target_buffer).map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("hashing materialized cache file: {error}"),
            )
        })?;
    if source_size != file.size || target_size != file.size || source_digest != target_digest {
        return Err(cache_error(
            CacheErrorKind::ConcurrentChange,
            format!(
                "cache source {} changed during materialization",
                file.relative.display()
            ),
        ));
    }
    Ok(CacheManifestEntryV1 {
        path: file.encoded,
        content: target_digest,
        size: target_size,
    })
}

#[cfg(target_os = "linux")]
fn reflink_from_handle(file: &std::fs::File, target: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    reflink_copy::reflink(
        PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd())),
        target,
    )
}

#[cfg(target_os = "macos")]
fn reflink_from_handle(file: &std::fs::File, target: &Path) -> std::io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::other("cache reflink target has no parent"))?;
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::other("cache reflink target has no file name"))?;
    let directory = nix::fcntl::open(
        parent,
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_CLOEXEC
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(std::io::Error::other)?;
    rustix::fs::fclonefileat(file, &directory, name, rustix::fs::CloneFlags::NOOWNERCOPY)
        .map_err(std::io::Error::from)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn reflink_from_handle(file: &std::fs::File, target: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    reflink_copy::reflink(
        PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd())),
        target,
    )
}

#[cfg(not(unix))]
fn reflink_from_handle(_file: &std::fs::File, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "Cache Snapshots require stable descriptor paths",
    ))
}

fn stable_size(file: &std::fs::File) -> Result<u64, CacheError> {
    let metadata = file.metadata().map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("inspecting open cache file: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(cache_error(
            CacheErrorKind::ConcurrentChange,
            "open cache descriptor changed away from a regular file",
        ));
    }
    Ok(metadata.len())
}

fn copy_exact_bounded(
    source: &std::fs::File,
    target: &Path,
    expected: u64,
) -> Result<(), CacheError> {
    let mut reader = source.try_clone().map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("cloning cache source descriptor: {error}"),
        )
    })?;
    reader.seek(SeekFrom::Start(0)).map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("rewinding cache source: {error}"),
        )
    })?;
    let mut reader = reader.take(expected.saturating_add(1));
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|error| {
            cache_error(
                CacheErrorKind::MaterializationFailed,
                format!("creating cache target {}: {error}", target.display()),
            )
        })?;
    let copied = std::io::copy(&mut reader, &mut writer).map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("copying bounded cache file: {error}"),
        )
    })?;
    writer.flush().map_err(|error| {
        cache_error(
            CacheErrorKind::MaterializationFailed,
            format!("flushing cache target: {error}"),
        )
    })?;
    if copied != expected {
        return Err(cache_error(
            CacheErrorKind::ConcurrentChange,
            format!(
                "cache source changed size during bounded copy: expected {expected}, read {copied}"
            ),
        ));
    }
    Ok(())
}

fn digest_stable_file(
    source: &std::fs::File,
    expected: u64,
    buffer: &mut [u8],
) -> Result<(String, u64), CacheError> {
    let mut reader = source.try_clone().map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("cloning cache source descriptor: {error}"),
        )
    })?;
    reader.seek(SeekFrom::Start(0)).map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("rewinding cache source: {error}"),
        )
    })?;
    let mut bounded = reader.take(expected.saturating_add(1));
    let result = digest_reader_with_buffer(&mut bounded, buffer).map_err(|error| {
        cache_error(
            CacheErrorKind::ConcurrentChange,
            format!("hashing bounded cache source: {error}"),
        )
    })?;
    if result.1 != expected || stable_size(source)? != expected {
        return Err(cache_error(
            CacheErrorKind::ConcurrentChange,
            "cache source changed during bounded hashing",
        ));
    }
    Ok(result)
}

#[cfg(unix)]
pub(crate) fn normalize_materialized_metadata(
    path: &Path,
    directory: bool,
    mode: u32,
) -> std::io::Result<()> {
    use nix::fcntl::OFlag;
    use nix::sys::stat::{Mode as NixMode, fchmod};

    let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
    if directory {
        flags |= OFlag::O_DIRECTORY;
    }
    let descriptor =
        nix::fcntl::open(path, flags, NixMode::empty()).map_err(std::io::Error::other)?;
    let file = std::fs::File::from(descriptor);

    normalize_macos_metadata(&file, directory)?;
    fchmod(&file, NixMode::from_bits_truncate(mode as _)).map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn normalize_macos_metadata(file: &std::fs::File, directory: bool) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    use xattr::FileExt;

    if !directory {
        let attributes: Vec<_> = file.list_xattr()?.collect();
        for attribute in attributes {
            let _ = file.remove_xattr(&attribute);
        }
        for attribute in file.list_xattr()? {
            let value = file.get_xattr(&attribute)?;
            // macOS attaches an immutable, kernel-owned provenance marker to files created by a
            // provenance-tracked process and refuses its removal. Admit only its closed bounded
            // shape; every source-controlled xattr and named fork is removed or fails closed.
            let admitted_provenance = attribute == std::ffi::OsStr::new("com.apple.provenance")
                && value
                    .as_ref()
                    .is_some_and(|value| value.len() <= 64 && value.starts_with(&[1, 2]));
            if !admitted_provenance {
                return Err(std::io::Error::other(format!(
                    "materialized cache object retained extended attribute {attribute:?}"
                )));
            }
        }
    }

    // The cache tree is private and no worker has started, and `/dev/fd` binds the ACL operation
    // to the no-follow descriptor above rather than resolving the candidate path a second time.
    let descriptor_path = PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()));
    exacl::setfacl(&[&descriptor_path], &[], None)?;
    if !exacl::getfacl(&descriptor_path, None)?.is_empty() {
        return Err(std::io::Error::other(
            "materialized cache object retained an extended ACL",
        ));
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn normalize_macos_metadata(_file: &std::fs::File, _directory: bool) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn normalize_materialized_metadata(
    path: &Path,
    _directory: bool,
    _mode: u32,
) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)
}

/// Remove seeded and Gate-mutated cache bytes before the ordinary Subject seal. Every cache
/// kind lives below the one reserved root, so this also removes a cloned Build Cache before a
/// Worker sandbox is sealed: its bytes never enter a candidate tree, a Proposal or a delivered
/// worktree.
pub fn remove_materialized_caches(sandbox: &Sandbox) -> Result<(), String> {
    let root = sandbox.root().join(CACHE_ROOT);
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            remove_cache_root(&root)?;
        }
        Ok(_) => std::fs::remove_file(&root)
            .map_err(|error| format!("removing replaced sandbox cache path: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspecting sandbox cache path: {error}")),
    }
    if std::fs::symlink_metadata(&root).is_ok() {
        return Err("sandbox cache cleanup left the reserved cache root behind".into());
    }
    Ok(())
}

fn remove_cache_root(root: &Path) -> Result<(), String> {
    crate::restore_writable_dirs(root);
    std::fs::remove_dir_all(root)
        .map_err(|error| format!("removing sandbox cache root {}: {error}", root.display()))
}

#[cfg(all(test, unix))]
mod race_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn limits() -> CacheLimits {
        CacheLimits {
            max_bytes: 1024,
            max_files: 16,
            max_copy_bytes: 1024,
        }
    }

    #[test]
    fn a_path_replaced_by_a_symlink_after_preflight_cannot_change_the_open_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("registry/cache")).unwrap();
        let path = source.join("registry/cache/a.crate");
        std::fs::write(&path, b"admitted").unwrap();
        let planned = preflight(CacheKind::Cargo, &source, limits()).unwrap();

        let original = root.path().join("original");
        std::fs::rename(&path, &original).unwrap();
        let secret = root.path().join("secret");
        std::fs::write(&secret, b"credential").unwrap();
        symlink(&secret, &path).unwrap();
        let target = root.path().join("target");
        std::fs::create_dir_all(target.join("registry/cache")).unwrap();
        let entry = materialize_file(
            planned.files[0].clone(),
            target.clone(),
            CacheMaterialization::Copy,
        )
        .unwrap();
        assert_eq!(entry.size, 8);
        assert_eq!(
            std::fs::read(target.join("registry/cache/a.crate")).unwrap(),
            b"admitted"
        );
    }

    #[test]
    fn growth_after_preflight_reads_at_most_one_extra_byte_and_fails() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("registry/cache")).unwrap();
        let path = source.join("registry/cache/a.crate");
        std::fs::write(&path, b"small").unwrap();
        let planned = preflight(CacheKind::Cargo, &source, limits()).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&vec![b'x'; 512])
            .unwrap();
        let target = root.path().join("target");
        std::fs::create_dir_all(target.join("registry/cache")).unwrap();
        assert!(
            materialize_file(
                planned.files[0].clone(),
                target.clone(),
                CacheMaterialization::Copy,
            )
            .unwrap_err()
            .operator_detail()
            .contains("changed")
        );
        assert!(
            std::fs::metadata(target.join("registry/cache/a.crate"))
                .map(|metadata| metadata.len() <= 6)
                .unwrap_or(true)
        );
    }
}
