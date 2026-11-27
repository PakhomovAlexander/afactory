use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use nix::fcntl::{FcntlArg, fcntl};
use nix::libc;
use rusqlite::{Connection, MAIN_DB};

use super::{EventStore, StoreError, require_event_layout};

mod wal;

// The bundled SQLite Unix VFS uses these POSIX byte ranges for its shared database lock
// and the WAL write, checkpoint and recovery locks, respectively.
const DB_SHARED_FIRST: libc::off_t = 0x4000_0002;
const DB_SHARED_SIZE: libc::off_t = 510;
const WAL_WRITE_LOCK: libc::off_t = 120;
const WAL_MUTATION_LOCKS: libc::off_t = 3;

// Bound the database, WAL and any WAL-driven expansion using logical lengths, including
// holes. Once capture returns, the WAL is gone and deserialization may duplicate the
// database once. The caller serializes observations for the OS account.
const INPUT_LIMIT: u64 = 64 * 1024 * 1024;

impl EventStore {
    /// Capture committed bytes into a private, read-only in-memory database. The caller
    /// holds the account observation lock from admission until the Store is dropped. Use
    /// a dedicated reader process: closing another handle on a source inode can drop its
    /// POSIX locks, and a writer in the same process would not conflict with them.
    pub fn open_observation_snapshot(path: &Path) -> Result<Self, StoreError> {
        let image = capture(path).map_err(|error| {
            StoreError::Conflict(format!("cannot observe {}: {error}", path.display()))
        })?;
        let mut conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        // The source is an already complete buffer, so this read cannot fail partway.
        conn.deserialize_read_exact(MAIN_DB, image.as_slice(), image.len(), true)?;
        drop(image);
        conn.pragma_update(None, "query_only", true)?;
        require_event_layout(&conn)?;
        Ok(Self {
            conn,
            task_cache: std::cell::RefCell::new(None),
            commit_observer: None,
            observation_snapshot: true,
        })
    }
}

fn capture(path: &Path) -> io::Result<Vec<u8>> {
    let mut database = open_source(path)?;
    // Prevent rollback-mode writes to the main file and SQLite's last-close removal of
    // the WAL. A writer can still start WAL mode, so absence must be checked again.
    lock_shared(&database, DB_SHARED_FIRST, DB_SHARED_SIZE)?;
    let original = database.metadata()?;
    let journal = sidecar(path, "-journal");
    let wal_path = sidecar(path, "-wal");
    let shm_path = sidecar(path, "-shm");
    require_absent(&journal)?;

    let mut wal_file = open_sidecar(&wal_path)?;
    let shm = open_sidecar(&shm_path)?;
    if let Some(file) = &shm {
        lock_shared(file, WAL_WRITE_LOCK, WAL_MUTATION_LOCKS)?;
        require_same_file(&shm_path, &file.metadata()?, false)?;
    }

    let wal_meta = wal_file.as_ref().map(File::metadata).transpose()?;
    let wal_size = wal_meta.as_ref().map_or(0, Metadata::len);
    check_size(original.len(), wal_size)?;

    let mut header = [0; 100];
    database.read_exact(&mut header)?;
    let page_size = database_page_size(&header, original.len())?;
    let mut wal_bytes = zeroed(wal_size)?;
    if let Some(file) = &mut wal_file {
        file.read_exact(&mut wal_bytes)?;
    }
    if wal_size != 0 && header[18..20] != [2, 2] {
        return Err(io::Error::other(
            "SQLite WAL present for a rollback database",
        ));
    }
    let commit = wal::last_commit(&wal_bytes, page_size)?;
    let final_size = commit
        .as_ref()
        .map_or(original.len(), wal::Commit::image_size);
    let copy_size = final_size.max(original.len());
    check_size(copy_size, wal_size)?;
    let mut image = zeroed(copy_size)?;
    image[..header.len()].copy_from_slice(&header);
    database.read_exact(&mut image[header.len()..original.len() as usize])?;

    if let Some(meta) = &wal_meta {
        require_same_file(&wal_path, meta, true)?;
    } else {
        require_absent(&wal_path)?;
    }
    if let Some(file) = &shm {
        require_same_file(&shm_path, &file.metadata()?, false)?;
    } else {
        // A cold WAL can survive without an index. A native writer needs to create
        // the index to mutate the WAL, and the shared database lock prevents removal.
        require_absent(&shm_path)?;
    }
    require_absent(&journal)?;
    require_same_file(path, &original, true)?;

    image.truncate(final_size as usize);
    if let Some(commit) = commit {
        commit.apply(&mut image);
        let recovered = image[..100].try_into().expect("recovered SQLite header");
        if database_page_size(recovered, final_size)? != page_size {
            return Err(io::Error::other(
                "SQLite WAL changed the database page size",
            ));
        }
    }
    // Deserialization needs a standalone image. SQLite must not look for a WAL.
    image[18..20].fill(1);
    Ok(image)
}

fn database_page_size(header: &[u8; 100], bytes: u64) -> io::Result<usize> {
    let encoded = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if encoded == 1 {
        65_536
    } else {
        usize::from(encoded)
    };
    if &header[..16] != b"SQLite format 3\0"
        || !matches!(&header[18..20], [1, 1] | [2, 2])
        || !page_size.is_power_of_two()
        || page_size < 512
        || bytes % page_size as u64 != 0
    {
        return Err(io::Error::other(
            "unsupported SQLite database header or length",
        ));
    }
    Ok(page_size)
}

fn check_size(database: u64, wal: u64) -> io::Result<()> {
    if database
        .checked_add(wal)
        .is_none_or(|size| size > INPUT_LIMIT)
    {
        return Err(io::Error::other(
            "observation snapshot exceeds the in-memory input limit",
        ));
    }
    Ok(())
}

fn zeroed(size: u64) -> io::Result<Vec<u8>> {
    let size = usize::try_from(size).map_err(io::Error::other)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(io::Error::other)?;
    bytes.resize(size, 0);
    Ok(bytes)
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

fn require_same_file(path: &Path, original: &Metadata, check_size: bool) -> io::Result<()> {
    let current = fs::symlink_metadata(path)?;
    if (current.dev(), current.ino()) != (original.dev(), original.ino()) {
        return Err(io::Error::other(format!("{} was replaced", path.display())));
    }
    if check_size && current.len() != original.len() {
        return Err(io::Error::other(format!(
            "{} changed size during capture",
            path.display()
        )));
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
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use serde_json::json;

    use super::super::{EventType, NewEvent};
    use super::*;
    use crate::Cas;

    #[test]
    fn observation_snapshot_bounds_combined_logical_lengths_and_refuses_overflow() {
        assert!(check_size(INPUT_LIMIT - 1, 1).is_ok());
        assert!(check_size(INPUT_LIMIT, 1).is_err());
        assert!(check_size(u64::MAX, 1).is_err());
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
            let parent = source.parent().unwrap();
            let entries = || {
                fs::read_dir(parent)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<std::collections::BTreeSet<_>>()
            };
            let before_entries = entries();
            let reader = EventStore::open_observation_snapshot(&source).unwrap();
            assert_eq!(reader.replay("run").unwrap().len(), 1);
            assert!(reader.database_path().is_none());
            assert!(reader.reopen(std::time::Duration::ZERO).is_err());
            assert_eq!(entries(), before_entries);
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
