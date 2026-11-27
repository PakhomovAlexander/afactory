use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::fcntl::{FcntlArg, fcntl};
use nix::libc;

use super::{EventStore, StoreError};
use crate::local_fs::{PRIVATE_DIR_MODE, private_file_options};

mod allocation;

// The bundled SQLite Unix VFS uses these POSIX byte ranges for its shared database lock
// and the WAL write, checkpoint and recovery locks, respectively.
const DB_SHARED_FIRST: libc::off_t = 0x4000_0002;
const DB_SHARED_SIZE: libc::off_t = 510;
const WAL_WRITE_LOCK: libc::off_t = 120;
const WAL_MUTATION_LOCKS: libc::off_t = 3;

pub(super) struct SnapshotFiles {
    // Closing a separate handle to the index would drop SQLite's POSIX locks. Keep it
    // until after EventStore drops its SQLite connection.
    #[cfg(target_os = "linux")]
    _index: Option<File>,
    _directory: tempfile::TempDir,
}

impl EventStore {
    /// Read a native local Store from a private SQLite snapshot. The snapshot directory and
    /// copies are created with owner-only access. SQLite rebuilds any WAL index on the copy,
    /// and the original files are opened only for reading. Measure the copy allowance
    /// after creating its private directory, which may itself allocate a disk cluster.
    ///
    /// Use from a dedicated observation process with no other handles to the source
    /// database or its sidecars. POSIX locks are process-owned: closing another handle
    /// could drop these locks, and a writer in the same process would not conflict.
    pub fn open_observation_snapshot(
        path: &Path,
        runtime: &Path,
        allowance: impl FnOnce(&Path) -> Result<u64, StoreError>,
    ) -> Result<Self, StoreError> {
        let conflict = |error: io::Error| {
            StoreError::Conflict(format!("cannot observe {}: {error}", path.display()))
        };
        let directory = tempfile::Builder::new()
            .prefix("snapshot-")
            .permissions(fs::Permissions::from_mode(PRIVATE_DIR_MODE))
            .tempdir_in(runtime)
            .map_err(conflict)?;
        let max_bytes = allowance(directory.path())?;
        let database = directory.path().join("events.sqlite");
        let has_wal = copy_snapshot(path, &database, max_bytes).map_err(conflict)?;
        let mut store = if has_wal {
            Self::open_read_only(&database)?
        } else {
            Self::open_unchanged(&database)?
        };
        // Opening and checking the schema rebuilds the index. Settle Linux delayed
        // allocation before the caller releases the machine-wide admission lock.
        #[cfg(target_os = "linux")]
        let index = open_sidecar(&sidecar(&database, "-shm")).map_err(conflict)?;
        #[cfg(target_os = "linux")]
        if let Some(index) = &index {
            if let Err(error) = index.sync_data() {
                drop(store);
                return Err(conflict(error));
            }
        }
        store._observation_snapshot = Some(SnapshotFiles {
            #[cfg(target_os = "linux")]
            _index: index,
            _directory: directory,
        });
        Ok(store)
    }
}

fn copy_snapshot(path: &Path, destination: &Path, max_bytes: u64) -> io::Result<bool> {
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

    let wal_bytes = wal
        .as_ref()
        .map(|file| file.metadata().map(|meta| meta.len()))
        .transpose()?;
    let directory = destination
        .parent()
        .expect("snapshot has a private directory");
    let allocation = allocation::for_destination(directory)?;
    if snapshot_allocation(original.len(), wal_bytes, allocation)
        .is_none_or(|bytes| bytes > max_bytes)
    {
        return Err(io::Error::other(
            "observation snapshot exceeds the available storage budget",
        ));
    }

    let mut copy_options = private_file_options();
    copy_options.write(true).create_new(true);
    copy_exact(
        &mut database,
        &mut copy_options.open(destination)?,
        original.len(),
    )?;
    if let Some((wal, bytes)) = wal.as_mut().zip(wal_bytes) {
        copy_exact(
            wal,
            &mut copy_options.open(sidecar(destination, "-wal"))?,
            bytes,
        )?;
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

fn snapshot_allocation(
    database: u64,
    wal: Option<u64>,
    allocation: allocation::Allocation,
) -> Option<u64> {
    let database = allocation.file_bytes(database)?;
    let Some(wal) = wal else {
        return Some(database);
    };
    // WAL index entries are smaller than their corresponding WAL frames. Allow one WAL
    // length plus a region for SQLite to rebuild the index. Each file has its own mapping.
    let index = allocation.file_bytes(wal.checked_add(32_768)?)?;
    database
        .checked_add(allocation.file_bytes(wal)?)?
        .checked_add(index)
}

fn copy_exact(source: &mut File, destination: &mut File, bytes: u64) -> io::Result<()> {
    if io::copy(&mut source.take(bytes), destination)? != bytes || source.metadata()?.len() != bytes
    {
        return Err(io::Error::other(
            "observation source changed size during the copy",
        ));
    }
    #[cfg(target_os = "linux")]
    destination.sync_data()?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::process::Command;

    use serde_json::json;

    use super::super::{EventType, NewEvent};
    use super::*;
    use crate::Cas;

    #[test]
    fn observation_snapshot_reserves_each_file_and_refuses_overflow() {
        let unit = 4096;
        let allocation = allocation::Allocation::new(unit);
        assert_eq!(snapshot_allocation(1025, None, allocation), Some(unit));
        assert_eq!(
            snapshot_allocation(1025, Some(0), allocation),
            Some(9 * unit)
        );
        assert_eq!(
            snapshot_allocation(1025, Some(1025), allocation),
            Some(11 * unit)
        );
        assert_eq!(snapshot_allocation(u64::MAX, None, allocation), None);
        assert_eq!(snapshot_allocation(0, Some(u64::MAX), allocation), None);
        assert_eq!(
            snapshot_allocation(u64::MAX - 4095, Some(1), allocation),
            None
        );
    }

    #[test]
    fn observation_snapshot_rounds_db_wal_and_index_to_separate_clusters() {
        let cluster = 65_536;
        let allocation = allocation::Allocation::new(cluster);
        assert_eq!(snapshot_allocation(1024, None, allocation), Some(cluster));
        assert_eq!(
            snapshot_allocation(1024, Some(0), allocation),
            Some(2 * cluster)
        );
        assert_eq!(
            snapshot_allocation(1024, Some(1024), allocation),
            Some(3 * cluster)
        );
        assert_eq!(
            snapshot_allocation(1024, Some(32_769), allocation),
            Some(4 * cluster)
        );
        assert_eq!(
            snapshot_allocation(u64::MAX - cluster + 1, Some(1), allocation),
            None
        );
    }

    #[test]
    fn observation_snapshot_reserves_mapping_for_every_ext_file() {
        let block = 4096;
        let logical = 13 * block;
        assert_eq!(
            snapshot_allocation(logical, None, allocation::Allocation::ext(block, block)),
            Some(14 * block)
        );
        let cluster = 65_536;
        let allocation = allocation::Allocation::ext(block, cluster);
        // DB and WAL each need one data cluster and up to one mapping cluster.
        // The rebuilt index spans two data clusters and can need a mapping cluster.
        assert_eq!(
            snapshot_allocation(logical, Some(logical), allocation),
            Some(7 * cluster)
        );
        assert_eq!(snapshot_allocation(u64::MAX, None, allocation), None);
        assert_eq!(
            snapshot_allocation(u64::MAX - cluster + 1, None, allocation),
            None
        );
        assert_eq!(
            snapshot_allocation(logical, Some(u64::MAX), allocation),
            None
        );
    }

    #[test]
    fn observation_snapshot_is_private_under_permissive_umasks() {
        const SOURCE: &str = "AF_TEST_OBSERVATION_SOURCE";
        const TEST: &str = "store::observation_snapshot::tests::observation_snapshot_is_private_under_permissive_umasks";
        if let Some(source) = std::env::var_os(SOURCE) {
            let source = PathBuf::from(source);
            let source_files = [
                source.clone(),
                sidecar(&source, "-wal"),
                sidecar(&source, "-shm"),
            ];
            let source_state = || {
                source_files
                    .each_ref()
                    .map(|path| (fs::metadata(path).unwrap().mode(), fs::read(path).unwrap()))
            };
            let before = source_state();
            let reader =
                EventStore::open_observation_snapshot(&source, source.parent().unwrap(), |_| {
                    Ok(u64::MAX)
                })
                .unwrap();
            assert_eq!(reader.replay("run").unwrap().len(), 1);
            let database = reader.database_path().unwrap();
            let directory = database.parent().unwrap();
            let modes = [
                directory.to_path_buf(),
                database.clone(),
                sidecar(&database, "-wal"),
            ]
            .map(|path| format!("{:04o}", fs::metadata(path).unwrap().mode() & 0o777));
            assert_eq!(
                modes,
                ["0700", "0600", "0600"],
                "directory, database and WAL modes"
            );
            assert!(
                source_state() == before,
                "observation changed source bytes or permissions"
            );
            return;
        }

        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let source = root.path().join("events.sqlite");
        let mut writer = EventStore::open(&source).unwrap();
        writer
            .conn
            .execute_batch("PRAGMA wal_autocheckpoint=0; PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        let cas = Cas::open(root.path().join("cas")).unwrap();
        writer
            .append(
                "run",
                &cas,
                NewEvent::new(EventType::SourceCapturedV1, json!({"i": 1})),
            )
            .unwrap();
        assert!(fs::metadata(sidecar(&source, "-wal")).unwrap().len() > 0);

        // The writer stays open in this process and each reader inherits its own umask.
        for mask in ["000", "022"] {
            let output = Command::new("/bin/sh")
                .args([
                    "-c",
                    "umask \"$1\" && exec \"$2\" --exact \"$3\"",
                    "sh",
                    mask,
                ])
                .arg(std::env::current_exe().unwrap())
                .arg(TEST)
                .env(SOURCE, &source)
                .env("TMPDIR", root.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "umask {mask}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        }
    }
}
