//! Private, bounded copies of an explicitly selected installed Rust toolchain.
//! This protects the supplied roots from aliasing; `trusted_local` is not OS isolation.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use nix::dir::Dir;
use nix::fcntl::OFlag;
use nix::sys::stat::{Mode, SFlag, fstat};
use review_source_git::{digest_bytes, digest_reader_with_buffer, encode_path};

use crate::cache::normalize_materialized_metadata;

pub const MAX_TOOLCHAIN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_TOOLCHAIN_ENTRIES: u64 = 100_000;

#[derive(Debug, Clone, Copy)]
pub struct ToolchainLimits {
    pub max_bytes: u64,
    pub max_entries: u64,
    pub max_copy_bytes: u64,
}

impl ToolchainLimits {
    pub fn validate(self) -> Result<(), String> {
        if self.max_bytes == 0
            || self.max_bytes > MAX_TOOLCHAIN_BYTES
            || self.max_entries == 0
            || self.max_entries > MAX_TOOLCHAIN_ENTRIES
            || self.max_copy_bytes == 0
            || self.max_copy_bytes > self.max_bytes
        {
            return Err("Rust toolchain limits are invalid or exceed kernel ceilings".into());
        }
        Ok(())
    }
}

struct FilePlan {
    handle: std::fs::File,
    path: PathBuf,
    size: u64,
    executable: bool,
}

/// The digest is over the sorted relative path, private byte digest, size, and executable bit.
/// `target` must be a newly absent path under a private runtime directory. A failed copy is
/// removed, and callers must keep the directory alive for the check's whole lifetime.
pub fn snapshot_toolchain(
    source: &Path,
    target: &Path,
    limits: ToolchainLimits,
    expected_digest: Option<&str>,
) -> Result<String, String> {
    snapshot_toolchain_inner(source, target, limits, expected_digest, || {})
}

fn snapshot_toolchain_inner(
    source: &Path,
    target: &Path,
    limits: ToolchainLimits,
    expected_digest: Option<&str>,
    after_preflight: impl FnOnce(),
) -> Result<String, String> {
    limits.validate()?;
    if !source.is_absolute() || !target.is_absolute() || target.exists() {
        return Err(
            "Rust toolchain source and target must be absolute; target must be absent".into(),
        );
    }
    let root = open_absolute_directory(source)?;
    let mut pending = vec![(root, PathBuf::new(), 0_u32)];
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut total = 0_u64;
    let mut entries = 0_u64;
    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
    while let Some((mut directory, parent, depth)) = pending.pop() {
        if depth > 128 {
            return Err("Rust toolchain exceeds directory depth limit".into());
        }
        let mut names = Vec::new();
        for entry in directory.iter() {
            let entry = entry.map_err(|e| format!("reading Rust toolchain directory: {e}"))?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            entries = entries
                .checked_add(1)
                .ok_or("Rust toolchain entry count overflow")?;
            if entries > limits.max_entries {
                return Err("Rust toolchain exceeds entry limit".into());
            }
            names.push(
                std::str::from_utf8(bytes)
                    .map_err(|_| "Rust toolchain path is not UTF-8")?
                    .to_owned(),
            );
        }
        names.sort();
        for name in names.into_iter().rev() {
            let path = parent.join(&name);
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "credentials"
                    | "credentials.toml"
                    | "credentials.json"
                    | "config"
                    | "config.toml"
                    | ".netrc"
                    | ".cargo"
                    | ".git-credentials"
            ) {
                return Err("Rust toolchain contains credential/config-shaped content".into());
            }
            if path.as_os_str().as_encoded_bytes().len() > 4096 {
                return Err("Rust toolchain path exceeds length limit".into());
            }
            let handle = nix::fcntl::openat(&directory, name.as_str(), flags, Mode::empty())
                .map_err(|e| format!("Rust toolchain contains unsafe entry: {e}"))?;
            let stat =
                fstat(&handle).map_err(|e| format!("inspecting Rust toolchain entry: {e}"))?;
            let kind = SFlag::from_bits_truncate(stat.st_mode);
            if kind == SFlag::S_IFDIR {
                directories.push(path.clone());
                pending.push((
                    Dir::from_fd(handle).map_err(|e| e.to_string())?,
                    path,
                    depth + 1,
                ));
            } else if kind == SFlag::S_IFREG {
                let size =
                    u64::try_from(stat.st_size).map_err(|_| "negative Rust toolchain file size")?;
                total = total
                    .checked_add(size)
                    .ok_or("Rust toolchain byte count overflow")?;
                if total > limits.max_bytes {
                    return Err("Rust toolchain exceeds byte limit".into());
                }
                files.push(FilePlan {
                    handle: handle.into(),
                    path,
                    size,
                    executable: stat.st_mode & 0o111 != 0,
                });
            } else {
                return Err("Rust toolchain contains a link or special file".into());
            }
        }
    }
    if files.is_empty() {
        return Err("Rust toolchain source is empty".into());
    }
    if total > limits.max_copy_bytes {
        return Err("Rust toolchain exceeds explicit plain-copy limit".into());
    }
    after_preflight();
    directories.sort();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    std::fs::create_dir(target).map_err(|e| format!("creating private Rust toolchain: {e}"))?;
    let result = (|| {
        normalize_materialized_metadata(target, true, 0o700).map_err(|e| e.to_string())?;
        for path in directories {
            let destination = target.join(path);
            std::fs::create_dir(&destination).map_err(|e| e.to_string())?;
            normalize_materialized_metadata(&destination, true, 0o700)
                .map_err(|e| e.to_string())?;
        }
        let mut manifest = Vec::new();
        for mut file in files {
            if file.handle.metadata().map_err(|e| e.to_string())?.len() != file.size {
                return Err("Rust toolchain source changed before copy".into());
            }
            file.handle
                .seek(SeekFrom::Start(0))
                .map_err(|e| e.to_string())?;
            let destination = target.join(&file.path);
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|e| e.to_string())?;
            let copied = std::io::copy(
                &mut (&mut file.handle).take(file.size.saturating_add(1)),
                &mut output,
            )
            .map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
            if copied != file.size
                || file.handle.metadata().map_err(|e| e.to_string())?.len() != file.size
            {
                return Err("Rust toolchain source changed during copy".into());
            }
            normalize_materialized_metadata(
                &destination,
                false,
                if file.executable { 0o700 } else { 0o600 },
            )
            .map_err(|e| e.to_string())?;
            let mut output = std::fs::File::open(&destination).map_err(|e| e.to_string())?;
            let (digest, size) = digest_reader_with_buffer(
                &mut (&mut output).take(file.size.saturating_add(1)),
                &mut [0_u8; 64 * 1024],
            )
            .map_err(|e| e.to_string())?;
            if size != file.size {
                return Err("private Rust toolchain changed during hashing".into());
            }
            manifest.push((
                encode_path(file.path.as_os_str().as_encoded_bytes()),
                digest,
                size,
                file.executable,
            ));
        }
        let digest = digest_bytes(&serde_json::to_vec(&manifest).map_err(|e| e.to_string())?);
        if expected_digest.is_some_and(|expected| expected != digest) {
            return Err("Rust toolchain content digest differs from machine mapping".into());
        }
        Ok(digest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(target);
    }
    result
}

fn open_absolute_directory(path: &Path) -> Result<Dir, String> {
    if !path.is_absolute() {
        return Err("Rust toolchain source must be absolute".into());
    }
    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY;
    let mut directory =
        Dir::open(Path::new("/"), flags, Mode::empty()).map_err(|e| e.to_string())?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                let handle = nix::fcntl::openat(&directory, name, flags, Mode::empty())
                    .map_err(|e| format!("Rust toolchain source has unsafe ancestor: {e}"))?;
                directory = Dir::from_fd(handle).map_err(|e| e.to_string())?;
            }
            _ => return Err("Rust toolchain source contains traversal".into()),
        }
    }
    Ok(directory)
}

/// Read a bounded configuration/declaration through no-follow ancestor descriptors.
pub fn read_toolchain_declaration(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let parent = path.parent().ok_or("toolchain declaration lacks parent")?;
    let directory = open_absolute_directory(parent)?;
    let name = path
        .file_name()
        .ok_or("toolchain declaration lacks filename")?;
    let handle = nix::fcntl::openat(
        &directory,
        name,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| format!("opening toolchain declaration: {e}"))?;
    let stat = fstat(&handle).map_err(|e| e.to_string())?;
    if SFlag::from_bits_truncate(stat.st_mode) != SFlag::S_IFREG {
        return Err("toolchain declaration must be a regular file".into());
    }
    let file = std::fs::File::from(handle);
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("toolchain declaration exceeds byte limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> ToolchainLimits {
        ToolchainLimits {
            max_bytes: 1024,
            max_entries: 20,
            max_copy_bytes: 1024,
        }
    }
    #[test]
    fn replacement_after_preflight_uses_retained_descriptor_not_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("tool"), b"trusted").unwrap();
        let expected = snapshot_toolchain(&source, &root.join("initial"), limits(), None).unwrap();
        std::fs::write(root.join("outside"), b"untrusted").unwrap();
        let target = root.join("copy");
        let actual = snapshot_toolchain_inner(&source, &target, limits(), Some(&expected), || {
            std::fs::rename(source.join("tool"), root.join("held")).unwrap();
            std::os::unix::fs::symlink(root.join("outside"), source.join("tool")).unwrap();
        })
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(std::fs::read(target.join("tool")).unwrap(), b"trusted");
    }
    #[test]
    fn same_length_change_after_admission_fails_content_pin_and_removes_partial() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("tool"), b"good").unwrap();
        let expected = snapshot_toolchain(&source, &root.join("initial"), limits(), None).unwrap();
        let target = root.join("copy");
        assert!(
            snapshot_toolchain_inner(&source, &target, limits(), Some(&expected), || {
                std::fs::write(source.join("tool"), b"evil").unwrap();
            })
            .is_err()
        );
        assert!(!target.exists());
    }
}
