//! How af's own state meets the local file system: private modes, and bounded reads.
//!
//! af never changes the process umask: it also writes into the user's checkout, which keeps the
//! user's. Instead every directory af creates for its own state (state roots, campaign and Task
//! Stores, `cas/`, `git-home/`) is created 0700 and every file there 0600, whatever the umask,
//! and a root af opens that its effective user owns is narrowed to 0700 when its group or others
//! may reach it.
//!
//! Files a repository supplies (authority policy, locks, Worker packages) are read through
//! [`read_bounded`]: never through a final link, never a FIFO or device, and never more than
//! the caller's bound, so a hostile file cannot make af read gigabytes.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// The mode of every directory af creates for its own state.
pub const PRIVATE_DIR_MODE: u32 = 0o700;
/// The mode of every file af creates in its own state.
pub const PRIVATE_FILE_MODE: u32 = 0o600;

/// Create `path` and every missing ancestor as private directories (0700). Directories that
/// already exist are left as they are; [`narrow_owned_dir`] tightens a root af owns.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(path)
}

/// Options that create a missing file private (0600); add the access the caller needs.
pub fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.mode(PRIVATE_FILE_MODE);
    options
}

/// Create the file `path` empty and private (0600) when it is absent; an existing file, or a
/// link in its place, is left untouched.
pub fn create_private_file(path: &Path) -> io::Result<()> {
    match private_file_options()
        .write(true)
        .create_new(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Narrow the directory `path` to 0700 when this process's effective user owns it and its
/// group or others have any access. Returns whether it changed the mode. A link, a non-directory
/// and another user's directory are left as they are. The mode is read and changed through one
/// descriptor opened without following a link, so a swapped path changes nothing else.
pub fn narrow_owned_dir(path: &Path) -> io::Result<bool> {
    use nix::fcntl::OFlag;
    use nix::sys::stat::{Mode, fchmod, fstat};
    let directory = match nix::fcntl::open(
        path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR) => return Ok(false),
        Err(errno) => return Err(errno.into()),
    };
    let stat = fstat(&directory)?;
    #[allow(clippy::unnecessary_cast)]
    let mode = stat.st_mode as u32;
    if stat.st_uid != nix::unistd::geteuid().as_raw() || mode & 0o077 == 0 {
        return Ok(false);
    }
    fchmod(&directory, Mode::S_IRWXU)?;
    Ok(true)
}

/// The names in the directory `directory` holds open, `.` and `..` excepted, listed through the
/// descriptor itself: whatever has become of its path since it was opened.
pub fn entry_names(directory: impl std::os::fd::AsFd) -> io::Result<Vec<std::ffi::OsString>> {
    use nix::fcntl::OFlag;
    use std::os::unix::ffi::OsStrExt as _;
    let mut listed = nix::dir::Dir::openat(
        &directory,
        ".",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )?;
    let mut names = Vec::new();
    for entry in listed.iter() {
        let name = entry?.file_name().to_bytes().to_vec();
        if name != b"." && name != b".." {
            names.push(std::ffi::OsStr::from_bytes(&name).to_os_string());
        }
    }
    names.sort();
    Ok(names)
}

/// The whole of the regular file `path` when it holds at most `limit` bytes. The file is opened
/// without following a final link and without blocking on a FIFO; anything but a regular file,
/// and a file over the bound, is refused with a one-line reason before more than `limit + 1`
/// bytes are read.
pub fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(nix::libc::ELOOP) => io::Error::new(
                io::ErrorKind::InvalidInput,
                "it is a symbolic link; af reads it only as a regular file",
            ),
            _ => error,
        })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it is not a regular file",
        ));
    }
    if metadata.len() > limit {
        return Err(too_large(metadata.len(), limit));
    }
    read_at_most(file, limit)
}

fn read_at_most(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(too_large(bytes.len() as u64, limit));
    }
    Ok(bytes)
}

fn too_large(bytes: u64, limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("it holds {bytes} bytes or more, over the bound of {limit} bytes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().mode() & 0o777
    }

    #[test]
    fn private_directories_and_files_ignore_a_permissive_umask() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("state/af/cas/objects");
        create_private_dir_all(&deep).unwrap();
        for directory in ["state", "state/af", "state/af/cas", "state/af/cas/objects"] {
            assert_eq!(mode(&root.path().join(directory)), 0o700, "{directory}");
        }
        let file = root.path().join("state/events.sqlite");
        create_private_file(&file).unwrap();
        assert_eq!(mode(&file), 0o600);
        // An existing file is left as it is.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        create_private_file(&file).unwrap();
        assert_eq!(mode(&file), 0o640);
    }

    #[test]
    fn an_owned_open_root_is_narrowed_and_a_link_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(narrow_owned_dir(&open).unwrap());
        assert_eq!(mode(&open), 0o700);
        assert!(!narrow_owned_dir(&open).unwrap());
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!narrow_owned_dir(&link).unwrap());
        assert_eq!(mode(&target), 0o755);
    }

    #[test]
    fn a_bounded_read_refuses_a_large_file_a_link_and_a_fifo() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("af.toml");
        std::fs::write(&file, vec![b'x'; 2048]).unwrap();
        assert_eq!(read_bounded(&file, 2048).unwrap().len(), 2048);
        let error = read_bounded(&file, 1024).unwrap_err().to_string();
        assert!(error.contains("over the bound of 1024 bytes"), "{error}");
        assert_eq!(error.lines().count(), 1);
        let link = root.path().join("link.toml");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let error = read_bounded(&link, 4096).unwrap_err().to_string();
        assert!(error.contains("symbolic link"), "{error}");
        let fifo = root.path().join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        let error = read_bounded(&fifo, 4096).unwrap_err().to_string();
        assert!(error.contains("not a regular file"), "{error}");
    }
}
