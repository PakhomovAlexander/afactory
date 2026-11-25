use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use nix::fcntl::{FcntlArg, fcntl};
use nix::libc;

use super::{EventStore, StoreError};

// The bundled SQLite Unix VFS uses these POSIX byte ranges for its shared database lock
// and the WAL write, checkpoint and recovery locks, respectively.
const DB_SHARED_FIRST: libc::off_t = 0x4000_0002;
const DB_SHARED_SIZE: libc::off_t = 510;
const WAL_WRITE_LOCK: libc::off_t = 120;
const WAL_MUTATION_LOCKS: libc::off_t = 3;

impl EventStore {
    /// Read a native local Store from a private SQLite snapshot. The original files are
    /// opened only for reading, even when SQLite must rebuild the WAL index on the copy.
    ///
    /// Use from a dedicated observation process with no other handles to the source
    /// database or its sidecars. POSIX locks are process-owned: closing another handle
    /// could drop these locks, and a writer in the same process would not conflict.
    pub fn open_observation_snapshot(path: &Path) -> Result<Self, StoreError> {
        let result = (|| {
            let directory = tempfile::Builder::new()
                .prefix("af-observation-")
                .tempdir()?;
            let database = directory.path().join("events.sqlite");
            let has_wal = copy_snapshot(path, &database)?;
            Ok::<_, io::Error>((directory, database, has_wal))
        })();
        let (directory, database, has_wal) = result.map_err(|error| {
            StoreError::Conflict(format!("cannot observe {}: {error}", path.display()))
        })?;
        let mut store = if has_wal {
            Self::open_read_only(database)?
        } else {
            Self::open_unchanged(database)?
        };
        store._observation_snapshot = Some(directory);
        Ok(store)
    }
}

fn copy_snapshot(path: &Path, destination: &Path) -> io::Result<bool> {
    let mut database = open_source(path)?;
    // Prevent rollback-mode writes to the main file and SQLite's last-close removal of
    // the WAL. A writer can still start WAL mode, so absence must be checked again.
    lock_shared(&database, DB_SHARED_FIRST, DB_SHARED_SIZE)?;
    let original = database.metadata()?;
    let journal = sidecar(path, "-journal");
    let wal_path = sidecar(path, "-wal");
    let shm_path = sidecar(path, "-shm");
    require_absent(&journal)?;

    let mut wal = open_sidecar(&wal_path)?;
    let shm = open_sidecar(&shm_path)?;
    if let Some(file) = &shm {
        // Writers, checkpoints and WAL recovery take exclusive locks here. Lock all
        // three together before copying either data file. The index itself is transient
        // and SQLite reconstructs it from the WAL in the private directory.
        lock_shared(file, WAL_WRITE_LOCK, WAL_MUTATION_LOCKS)?;
        require_same_file(&shm_path, &file.metadata()?)?;
    }

    io::copy(&mut database, &mut File::create(destination)?)?;
    if let Some(wal) = &mut wal {
        io::copy(wal, &mut File::create(sidecar(destination, "-wal"))?)?;
        require_same_file(&wal_path, &wal.metadata()?)?;
    } else {
        require_absent(&wal_path)?;
    }
    if let Some(file) = &shm {
        require_same_file(&shm_path, &file.metadata()?)?;
    } else {
        // A cold WAL can survive without an index. A native writer would have to
        // create the index to modify the WAL, and our main lock prevents its removal.
        require_absent(&shm_path)?;
    }
    require_absent(&journal)?;
    require_same_file(path, &original)?;
    Ok(wal.is_some())
}

fn open_source(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

fn open_sidecar(path: &Path) -> io::Result<Option<File>> {
    match open_source(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn lock_shared(file: &File, start: libc::off_t, len: libc::off_t) -> io::Result<()> {
    let lock = libc::flock {
        l_type: libc::F_RDLCK as _,
        l_whence: libc::SEEK_SET as _,
        l_start: start,
        l_len: len,
        l_pid: 0,
    };
    fcntl(file, FcntlArg::F_SETLK(&lock)).map_err(io::Error::from)?;
    Ok(())
}

fn require_same_file(path: &Path, original: &Metadata) -> io::Result<()> {
    let current = fs::symlink_metadata(path)?;
    if (current.dev(), current.ino()) != (original.dev(), original.ino()) {
        return Err(io::Error::other(format!("{} was replaced", path.display())));
    }
    Ok(())
}

fn require_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::other(format!("{} is present", path.display()))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}
