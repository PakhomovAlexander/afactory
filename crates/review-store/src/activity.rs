//! Disposable Task activity, never execution authority. One fixed ring per admitted Attempt.
//! Slots and the head are padded JSON documents; a torn/concurrent read is explicitly refused
//! and retried on the next poll. No CAS object or execution event refers to this sidecar.
use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "af/task-activity/1";
pub const CAPACITY: usize = 128;
pub const SLOT_BYTES: usize = 1024;
pub const MAX_BYTES: u64 = ((CAPACITY + 1) * SLOT_BYTES) as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Worker,
    Command,
    FileChange,
    Tool,
    Observation,
}
impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Worker => "Worker",
            Self::Command => "Running command",
            Self::FileChange => "Changing files",
            Self::Tool => "Using tool",
            Self::Observation => "Activity gap",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Started,
    Completed,
    Failed,
    Dropped,
    Truncated,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Dropped => "observer overflow (notifications lost)",
            Self::Truncated => "oversized/incomplete frame discarded",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: String,
    pub task_id: String,
    pub attempt_id: String,
    pub sequence: u64,
    pub unix_ms: u64,
    pub operation: Operation,
    pub phase: Phase,
}

/// Host-owned attribution. Provider data never supplies paths or these identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    root: PathBuf,
    task_id: String,
    attempt_id: String,
}
impl Binding {
    pub fn new(root: &Path, task_id: &str, attempt_id: &str) -> Result<Self, Error> {
        if !review_core::task::is_name(task_id)
            || attempt_id.is_empty()
            || attempt_id.len() > 256
            || !attempt_id.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            root: root.into(),
            task_id: task_id.into(),
            attempt_id: attempt_id.into(),
        })
    }

    fn names(&self) -> (String, String) {
        let digest = |s: &str| {
            crate::content_id(&serde_json::json!(s))
                .expect("string identity")
                .replace(':', "-")
        };
        (digest(&self.task_id), digest(&self.attempt_id))
    }

    // Hold every component below the trusted Store: no symlink, FIFO, device or candidate path.
    fn directory(&self, create: bool) -> Result<File, Error> {
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let mut dir = open(&self.root, flags, Mode::empty()).map_err(io_error)?;
        for name in ["activity".to_owned(), self.names().0] {
            if create {
                match nix::sys::stat::mkdirat(&dir, name.as_str(), Mode::S_IRWXU) {
                    Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
                    Err(e) => return Err(io_error(e)),
                }
            }
            dir = openat(&dir, name.as_str(), flags, Mode::empty()).map_err(io_error)?;
        }
        Ok(dir.into())
    }

    fn accepts(&self, r: &Record) -> bool {
        r.schema == SCHEMA
            && r.task_id == self.task_id
            && r.attempt_id == self.attempt_id
            && r.sequence > 0
            && r.sequence <= review_core::json::SAFE_INTEGER_MAX as u64
            && r.unix_ms <= review_core::json::SAFE_INTEGER_MAX as u64
            && (r.operation == Operation::Observation)
                == matches!(r.phase, Phase::Dropped | Phase::Truncated)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Missing,
    Unreadable,
    Invalid,
    Changed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Missing => "activity missing (not yet published or no longer retained)",
            Self::Unreadable => "activity unreadable",
            Self::Invalid => "activity invalid, truncated, or unsupported version",
            Self::Changed => "activity changed during read; retrying",
        })
    }
}
fn io_error(e: impl Into<io::Error>) -> Error {
    if e.into().kind() == io::ErrorKind::NotFound {
        Error::Missing
    } else {
        Error::Unreadable
    }
}

pub struct Writer {
    binding: Binding,
    file: File,
    sequence: u64,
}
impl Writer {
    pub fn create(binding: Binding) -> Result<Self, Error> {
        let dir = binding.directory(true)?;
        let fd = openat(
            &dir,
            binding.names().1.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(io_error)?;
        let file = File::from(fd);
        file.set_len(MAX_BYTES).map_err(io_error)?;
        Ok(Self {
            binding,
            file,
            sequence: 0,
        })
    }
    /// Only enums, clock and construction-time identities can cross this boundary.
    pub fn publish(
        &mut self,
        operation: Operation,
        phase: Phase,
        unix_ms: u64,
    ) -> Result<(), Error> {
        let record = Record {
            schema: SCHEMA.into(),
            task_id: self.binding.task_id.clone(),
            attempt_id: self.binding.attempt_id.clone(),
            sequence: self.sequence.saturating_add(1),
            unix_ms,
            operation,
            phase,
        };
        if !self.binding.accepts(&record) {
            return Err(Error::Invalid);
        }
        let bytes = serde_json::to_vec(&record).map_err(|_| Error::Invalid)?;
        if bytes.len() >= SLOT_BYTES {
            return Err(Error::Invalid);
        }
        let mut slot = [b' '; SLOT_BYTES];
        slot[..bytes.len()].copy_from_slice(&bytes);
        slot[SLOT_BYTES - 1] = b'\n';
        let index = (record.sequence - 1) % CAPACITY as u64 + 1;
        self.file
            .write_all_at(&slot, index * SLOT_BYTES as u64)
            .map_err(io_error)?;
        // Publish the head last. Readers check every slot's exact sequence and identity.
        self.file.write_all_at(&slot, 0).map_err(io_error)?;
        self.sequence = record.sequence;
        Ok(())
    }
}

pub struct Reader {
    binding: Binding,
    pub records: VecDeque<Record>,
    pub sequence: u64,
    pub lost: u64,
}
impl Reader {
    pub fn new(binding: Binding) -> Self {
        Self {
            binding,
            records: VecDeque::new(),
            sequence: 0,
            lost: 0,
        }
    }
    /// Read only new slots, with fixed allocation and at most one ring's worth of IO.
    /// Call on a background reader, never on the terminal's key loop.
    pub fn poll(&mut self) -> Result<(), Error> {
        let dir = self.binding.directory(false)?;
        let file = File::from(
            openat(
                &dir,
                self.binding.names().1.as_str(),
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(io_error)?,
        );
        let meta = file.metadata().map_err(io_error)?;
        if !meta.is_file() || meta.len() != MAX_BYTES {
            return Err(Error::Invalid);
        }
        let read = |offset| -> Result<Record, Error> {
            let mut slot = [0; SLOT_BYTES];
            file.read_exact_at(&mut slot, offset).map_err(io_error)?;
            if offset == 0 && slot.iter().all(|byte| *byte == 0) {
                return Err(Error::Missing);
            }
            let record: Record = serde_json::from_slice(&slot).map_err(|_| Error::Invalid)?;
            if !self.binding.accepts(&record) {
                return Err(Error::Invalid);
            }
            Ok(record)
        };
        let head = read(0)?;
        if head.sequence < self.sequence {
            return Err(Error::Invalid);
        }
        let first = self
            .sequence
            .saturating_add(1)
            .max(head.sequence.saturating_sub(CAPACITY as u64 - 1));
        let mut fresh = Vec::new();
        for seq in first..=head.sequence {
            let record = read(((seq - 1) % CAPACITY as u64 + 1) * SLOT_BYTES as u64)?;
            if record.sequence != seq {
                return Err(Error::Changed);
            }
            fresh.push(record);
        }
        if read(0)?.sequence != head.sequence {
            return Err(Error::Changed);
        }
        self.lost = head.sequence.saturating_sub(CAPACITY as u64);
        for record in fresh {
            if self.records.len() == CAPACITY {
                self.records.pop_front();
            }
            self.records.push_back(record);
        }
        self.sequence = head.sequence;
        Ok(())
    }
}

/// Collection removes only this Task's disposable channels through held directories. Links
/// are unlinked, never followed; an unexpected directory keeps the enclosing directory alive.
pub fn forget_task(root: &Path, task_id: &str) -> Result<(), Error> {
    let binding = Binding::new(root, task_id, "collection")?;
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let root = open(root, flags, Mode::empty()).map_err(io_error)?;
    let activity = match openat(&root, "activity", flags, Mode::empty()) {
        Ok(dir) => dir,
        Err(nix::errno::Errno::ENOENT) => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    let name = binding.names().0;
    let dir = match openat(&activity, name.as_str(), flags, Mode::empty()) {
        Ok(dir) => dir,
        Err(nix::errno::Errno::ENOENT) => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    let mut entries = nix::dir::Dir::openat(&dir, ".", flags, Mode::empty()).map_err(io_error)?;
    for entry in entries.iter() {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        nix::unistd::unlinkat(&dir, name, nix::unistd::UnlinkatFlags::NoRemoveDir)
            .map_err(io_error)?;
    }
    nix::unistd::unlinkat(
        &activity,
        name.as_str(),
        nix::unistd::UnlinkatFlags::RemoveDir,
    )
    .map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn ring_is_bounded_incremental_and_reports_overwritten_history() {
        let root = tempfile::tempdir().unwrap();
        let binding = Binding::new(root.path(), "task", "attempt").unwrap();
        let mut writer = Writer::create(binding.clone()).unwrap();
        let mut reader = Reader::new(binding);
        writer
            .publish(Operation::Command, Phase::Started, 1)
            .unwrap();
        reader.poll().unwrap();
        assert_eq!(reader.sequence, 1);
        reader.poll().unwrap();
        assert_eq!(
            reader.records.len(),
            1,
            "an unchanged head never duplicates rows"
        );
        for n in 2..=CAPACITY as u64 * 3 {
            writer
                .publish(Operation::Tool, Phase::Completed, n)
                .unwrap();
        }
        reader.poll().unwrap();
        assert_eq!(reader.sequence, CAPACITY as u64 * 3);
        assert_eq!(reader.records.len(), CAPACITY);
        assert_eq!(
            reader.records.front().unwrap().sequence,
            CAPACITY as u64 * 2 + 1
        );
        assert_eq!(reader.lost, CAPACITY as u64 * 2);
        assert_eq!(writer.file.metadata().unwrap().len(), MAX_BYTES);
        assert_eq!(writer.file.metadata().unwrap().mode() & 0o777, 0o600);
        assert!(
            Reader::new(Binding::new(root.path(), "other", "attempt").unwrap()).poll()
                == Err(Error::Missing)
        );
        assert!(
            Reader::new(Binding::new(root.path(), "task", "other").unwrap()).poll()
                == Err(Error::Missing)
        );
        let other_binding = Binding::new(root.path(), "other", "attempt").unwrap();
        let mut other_writer = Writer::create(other_binding.clone()).unwrap();
        other_writer
            .publish(Operation::Tool, Phase::Started, 1)
            .unwrap();
        forget_task(root.path(), "task").unwrap();
        assert_eq!(reader.poll(), Err(Error::Missing));
        assert!(
            Reader::new(other_binding).poll().is_ok(),
            "collection preserves a sibling Task"
        );
        let other_store = tempfile::tempdir().unwrap();
        assert!(
            Reader::new(Binding::new(other_store.path(), "task", "attempt").unwrap()).poll()
                == Err(Error::Missing)
        );
    }

    #[test]
    fn reader_refuses_wrong_identity_version_torn_slots_and_links() {
        let root = tempfile::tempdir().unwrap();
        let binding = Binding::new(root.path(), "task", "attempt").unwrap();
        let mut writer = Writer::create(binding.clone()).unwrap();
        writer
            .publish(Operation::Command, Phase::Started, 1)
            .unwrap();
        let mut reader = Reader::new(binding.clone());
        reader.poll().unwrap();
        let mut slot = [b' '; SLOT_BYTES];
        let mut record = reader.records[0].clone();
        for field in ["version", "identity", "sequence"] {
            let mut record = record.clone();
            match field {
                "version" => record.schema = "af/task-activity/0".into(),
                "identity" => record.attempt_id = "other".into(),
                _ => record.sequence = 0,
            }
            let bytes = serde_json::to_vec(&record).unwrap();
            slot.fill(b' ');
            slot[..bytes.len()].copy_from_slice(&bytes);
            writer.file.write_all_at(&slot, 0).unwrap();
            assert_eq!(reader.poll(), Err(Error::Invalid));
            assert_eq!(
                reader.records.len(),
                1,
                "bad sidecars do not replace observed data"
            );
        }
        record.sequence = 2;
        let bytes = serde_json::to_vec(&record).unwrap();
        slot.fill(b' ');
        slot[..bytes.len()].copy_from_slice(&bytes);
        writer.file.write_all_at(&slot, 0).unwrap();
        writer
            .file
            .write_all_at(&slot, 2 * SLOT_BYTES as u64)
            .unwrap();
        // Header says seq 2, but a partial slot isn't a complete document.
        writer
            .file
            .write_all_at(b"broken", 2 * SLOT_BYTES as u64)
            .unwrap();
        assert_eq!(reader.poll(), Err(Error::Invalid));
        writer.file.set_len(MAX_BYTES + 1).unwrap();
        assert_eq!(reader.poll(), Err(Error::Invalid));
        let names = binding.names();
        let path = root.path().join("activity").join(names.0).join(names.1);
        std::fs::remove_file(&path).unwrap();
        symlink("/dev/zero", &path).unwrap();
        assert_eq!(reader.poll(), Err(Error::Unreadable));
        let other = tempfile::tempdir().unwrap();
        symlink(root.path().join("activity"), other.path().join("activity")).unwrap();
        assert_eq!(
            Reader::new(Binding::new(other.path(), "task", "attempt").unwrap()).poll(),
            Err(Error::Unreadable)
        );
    }
}
