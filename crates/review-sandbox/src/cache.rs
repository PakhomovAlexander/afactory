//! Bounded, credential-free package cache snapshots for writable Gate clones.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use review_core::{CacheManifestEntryV1, CacheManifestV1, CachePathEncodingV1, RunCacheKindV5};
use review_source_git::{digest_reader_with_buffer, encode_path};
use review_store::Cas;

use crate::{Sandbox, ensure_directory_mode};

pub const MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_CACHE_COPY_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_CACHE_FILES: u64 = 250_000;
const CACHE_ROOT: &str = ".af-cache";

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

    fn validate_relative(self, relative: &Path) -> Result<(), String> {
        let mut components = relative.components();
        let Some(Component::Normal(first)) = components.next() else {
            return Err("cache entry has no normal relative path".into());
        };
        let first = first
            .to_str()
            .ok_or("Cargo cache paths must be portable UTF-8")?;
        if first != "registry" {
            return Err(format!(
                "Cargo cache path `{}` is outside the credential-free registry/ allowlist; git dependency caches are not admitted in CacheManifest@1",
                relative.display()
            ));
        }
        if let Some(Component::Normal(second)) = components.next() {
            let second = second
                .to_str()
                .ok_or("Cargo cache paths must be portable UTF-8")?;
            if !matches!(second, "cache" | "index") {
                return Err(format!(
                    "Cargo cache path `{}` is outside the admitted registry/cache and sparse registry/index layouts",
                    relative.display()
                ));
            }
        }
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(format!(
                    "cache path `{}` has a non-normal component",
                    relative.display()
                ));
            };
            let component = component
                .to_str()
                .ok_or("Cargo cache paths must be portable UTF-8")?
                .to_ascii_lowercase();
            if matches!(
                component.as_str(),
                "credentials"
                    | "credentials.toml"
                    | "credentials.json"
                    | ".git-credentials"
                    | "config"
                    | "config.toml"
                    | ".netrc"
                    | ".npmrc"
                    | "token"
                    | "tokens"
            ) {
                return Err(format!(
                    "cache path `{}` has credential-shaped component `{component}`",
                    relative.display()
                ));
            }
        }
        Ok(())
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
) -> Result<CacheSnapshot, String> {
    source.limits.validate()?;
    if !source.source.is_absolute() {
        return Err("cache source must resolve to an absolute directory".into());
    }
    let preflight = preflight(source.kind, &source.source, source.limits)?;
    let cache_root = sandbox.root().join(CACHE_ROOT);
    if std::fs::symlink_metadata(&cache_root).is_ok() {
        return Err(format!(
            "Subject already contains reserved cache path `{CACHE_ROOT}`"
        ));
    }
    std::fs::create_dir(&cache_root)
        .map_err(|error| format!("creating sandbox cache root: {error}"))?;
    ensure_directory_mode(&cache_root, 0o700)
        .map_err(|error| format!("restricting sandbox cache root: {error}"))?;
    let target = sandbox.root().join(source.kind.relative_root());

    let result = materialize_preflight(source, &preflight, &target, cas);
    if result.is_err() {
        let _ = remove_cache_root(&cache_root);
    }
    result
}

#[cfg(unix)]
fn preflight(kind: CacheKind, source: &Path, limits: CacheLimits) -> Result<Preflight, String> {
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
            format!(
                "{} cache source {} must be an accessible real directory: {error}",
                kind.name(),
                source.display()
            )
        })?;
    let mut level = vec![(root, PathBuf::new(), 0_u32)];
    while let Some((mut directory, parent, depth)) = level.pop() {
        if depth > 128 {
            return Err("cache tree exceeds the 128-directory depth limit".into());
        }
        let mut names = Vec::new();
        for entry in directory.iter() {
            let entry = entry.map_err(|error| format!("reading cache directory: {error}"))?;
            let raw = entry.file_name().to_bytes();
            if matches!(raw, b"." | b"..") {
                continue;
            }
            entry_count = entry_count
                .checked_add(1)
                .ok_or("cache entry count overflow")?;
            if entry_count > limits.max_files {
                return Err(format!(
                    "{} cache exceeds its {} filesystem-entry limit",
                    kind.name(),
                    limits.max_files
                ));
            }
            names.push(
                std::str::from_utf8(raw)
                    .map_err(|_| "Cargo cache paths must be portable UTF-8")?
                    .to_string(),
            );
        }
        names.sort();
        for name in names.into_iter().rev() {
            let relative = parent.join(&name);
            if relative.as_os_str().as_encoded_bytes().len() > 4096 {
                return Err("cache path exceeds the 4096-byte portability limit".into());
            }
            kind.validate_relative(&relative)?;
            let descriptor = nix::fcntl::openat(&directory, name.as_str(), flags, NixMode::empty())
                .map_err(|error| {
                    format!(
                        "opening cache path {} without following links: {error}",
                        relative.display()
                    )
                })?;
            let stat = fstat(&descriptor).map_err(|error| {
                format!("inspecting cache path {}: {error}", relative.display())
            })?;
            let file_type = SFlag::from_bits_truncate(stat.st_mode);
            if file_type == SFlag::S_IFDIR {
                directories.push(relative);
                let child = Dir::from_fd(descriptor)
                    .map_err(|error| format!("opening cache directory: {error}"))?;
                level.push((child, parent.join(name), depth + 1));
                continue;
            }
            if file_type != SFlag::S_IFREG {
                return Err(format!(
                    "cache path {} is not a regular file or directory; links are never followed",
                    relative.display()
                ));
            }
            if relative.components().count() < 3 {
                return Err(format!(
                    "Cargo cache file `{}` is outside the admitted registry/cache and sparse registry/index layouts",
                    relative.display()
                ));
            }
            let size = u64::try_from(stat.st_size).map_err(|_| "cache file has negative size")?;
            bytes = bytes.checked_add(size).ok_or("cache byte count overflow")?;
            if bytes > limits.max_bytes {
                return Err(format!(
                    "{} cache exceeds its {} byte limit",
                    kind.name(),
                    limits.max_bytes
                ));
            }
            let relative_text = relative
                .to_str()
                .ok_or("Cargo cache paths must be portable UTF-8")?;
            files.push(PlannedFile {
                source: Arc::new(std::fs::File::from(descriptor)),
                relative: relative.clone(),
                encoded: encode_path(relative_text.as_bytes()),
                size,
            });
        }
    }
    if files.is_empty() {
        return Err(format!("{} cache source contains no files", kind.name()));
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
fn preflight(_kind: CacheKind, _source: &Path, _limits: CacheLimits) -> Result<Preflight, String> {
    Err("Cache Snapshots require descriptor-relative no-follow filesystem APIs".into())
}

fn materialize_preflight(
    source: &CacheSource,
    preflight: &Preflight,
    target: &Path,
    cas: &Cas,
) -> Result<CacheSnapshot, String> {
    let parent = target
        .parent()
        .ok_or("sandbox cache target has no parent")?;
    let probe = parent.join(".reflink-probe");
    let probe_source = source_handle_path(&preflight.files[0].source)?;
    let materialization = match reflink_copy::reflink(&probe_source, &probe) {
        Ok(()) => {
            std::fs::remove_file(&probe)
                .map_err(|error| format!("removing cache reflink probe: {error}"))?;
            CacheMaterialization::Reflink
        }
        Err(_) if preflight.bytes <= source.limits.max_copy_bytes => {
            let _ = std::fs::remove_file(&probe);
            CacheMaterialization::Copy
        }
        Err(error) => {
            let _ = std::fs::remove_file(&probe);
            return Err(format!(
                "{} cache needs a {} byte plain copy after reflink preflight failed ({error}), exceeding its {} byte copy limit",
                source.kind.name(),
                preflight.bytes,
                source.limits.max_copy_bytes
            ));
        }
    };
    std::fs::create_dir(target)
        .map_err(|error| format!("creating sandbox cache target: {error}"))?;
    ensure_directory_mode(target, 0o700)
        .map_err(|error| format!("restricting sandbox cache target: {error}"))?;
    for relative in &preflight.directories {
        let directory = target.join(relative);
        std::fs::create_dir_all(&directory).map_err(|error| {
            format!("creating cache directory {}: {error}", directory.display())
        })?;
        ensure_directory_mode(&directory, 0o700)
            .map_err(|error| format!("making cache directory writable: {error}"))?;
    }

    let mut entries = review_parallel::try_map_owned(preflight.files.clone(), |file| {
        materialize_file(file, target.to_path_buf(), materialization)
    })
    .map_err(|error| format!("materializing {} cache: {error}", source.kind.name()))?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = CacheManifestV1 {
        kind: match source.kind {
            CacheKind::Cargo => RunCacheKindV5::Cargo,
        },
        path_encoding: CachePathEncodingV1::PercentV2,
        entries,
    };
    manifest.validate()?;
    let source_digest = cas
        .put_json(&serde_json::to_value(&manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    Ok(CacheSnapshot {
        kind: source.kind,
        source_digest,
        bytes: preflight.bytes,
        files: u64::try_from(preflight.files.len()).map_err(|_| "cache file count overflow")?,
        materialization,
    })
}

fn materialize_file(
    file: PlannedFile,
    target_root: PathBuf,
    materialization: CacheMaterialization,
) -> Result<CacheManifestEntryV1, String> {
    let target = target_root.join(&file.relative);
    if stable_size(&file.source)? != file.size {
        return Err(format!(
            "cache source {} changed before materialization",
            file.relative.display()
        ));
    }
    match materialization {
        CacheMaterialization::Reflink => {
            let source = source_handle_path(&file.source)?;
            reflink_copy::reflink(&source, &target)
                .map_err(|error| format!("reflinking {}: {error}", file.relative.display()))?;
        }
        CacheMaterialization::Copy => {
            copy_exact_bounded(&file.source, &target, file.size)?;
        }
    }
    make_file_writable(&target)
        .map_err(|error| format!("making cache file {} writable: {error}", target.display()))?;
    let mut source_buffer = vec![0_u8; 64 * 1024];
    let mut target_buffer = vec![0_u8; 64 * 1024];
    let (source_digest, source_size) =
        digest_stable_file(&file.source, file.size, &mut source_buffer)
            .map_err(|error| format!("hashing cache source: {error}"))?;
    let mut target_file = std::fs::File::open(&target)
        .map_err(|error| format!("opening materialized cache file: {error}"))?;
    let mut bounded_target = (&mut target_file).take(file.size.saturating_add(1));
    let (target_digest, target_size) =
        digest_reader_with_buffer(&mut bounded_target, &mut target_buffer)
            .map_err(|error| format!("hashing materialized cache file: {error}"))?;
    if source_size != file.size || target_size != file.size || source_digest != target_digest {
        return Err(format!(
            "cache source {} changed during materialization",
            file.relative.display()
        ));
    }
    Ok(CacheManifestEntryV1 {
        path: file.encoded,
        content: target_digest,
        size: target_size,
    })
}

#[cfg(target_os = "linux")]
fn source_handle_path(file: &std::fs::File) -> Result<PathBuf, String> {
    use std::os::fd::AsRawFd;
    Ok(PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd())))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn source_handle_path(file: &std::fs::File) -> Result<PathBuf, String> {
    use std::os::fd::AsRawFd;
    Ok(PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd())))
}

#[cfg(not(unix))]
fn source_handle_path(_file: &std::fs::File) -> Result<PathBuf, String> {
    Err("Cache Snapshots require stable descriptor paths".into())
}

fn stable_size(file: &std::fs::File) -> Result<u64, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspecting open cache file: {error}"))?;
    if !metadata.is_file() {
        return Err("open cache descriptor changed away from a regular file".into());
    }
    Ok(metadata.len())
}

fn copy_exact_bounded(source: &std::fs::File, target: &Path, expected: u64) -> Result<(), String> {
    let mut reader = source
        .try_clone()
        .map_err(|error| format!("cloning cache source descriptor: {error}"))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewinding cache source: {error}"))?;
    let mut reader = reader.take(expected.saturating_add(1));
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|error| format!("creating cache target {}: {error}", target.display()))?;
    let copied = std::io::copy(&mut reader, &mut writer)
        .map_err(|error| format!("copying bounded cache file: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("flushing cache target: {error}"))?;
    if copied != expected {
        return Err(format!(
            "cache source changed size during bounded copy: expected {expected}, read {copied}"
        ));
    }
    Ok(())
}

fn digest_stable_file(
    source: &std::fs::File,
    expected: u64,
    buffer: &mut [u8],
) -> Result<(String, u64), String> {
    let mut reader = source
        .try_clone()
        .map_err(|error| format!("cloning cache source descriptor: {error}"))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewinding cache source: {error}"))?;
    let mut bounded = reader.take(expected.saturating_add(1));
    let result =
        digest_reader_with_buffer(&mut bounded, buffer).map_err(|error| error.to_string())?;
    if result.1 != expected || stable_size(source)? != expected {
        return Err("cache source changed during bounded hashing".into());
    }
    Ok(result)
}

#[cfg(unix)]
fn make_file_writable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path)?;
    let mode = metadata.permissions().mode() | 0o600;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn make_file_writable(path: &Path) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)
}

/// Remove seeded and Gate-mutated cache bytes before the ordinary Subject seal.
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
            .contains("changed")
        );
        assert!(
            std::fs::metadata(target.join("registry/cache/a.crate"))
                .map(|metadata| metadata.len() <= 6)
                .unwrap_or(true)
        );
    }
}
