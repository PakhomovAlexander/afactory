//! Attempt-local vnode watches: FSEvents can deliver pre-attempt activity after `SinceNow`.
//! Register parents before enumerating children, then keep every descriptor until the read
//! boundary closes. A creation, removal or replacement during enumeration dirties its parent;
//! no dynamic rewatching (and no asynchronous rewatch failure) is needed within one attempt.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use nix::libc;
use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};

use super::CaptureError;

pub(super) struct WorktreeMonitor {
    queue: Kqueue,
    files: std::collections::BTreeMap<usize, (File, std::fs::Metadata)>,
}

fn watch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Watch {
        detail: error.to_string(),
    }
}

impl WorktreeMonitor {
    pub(super) fn start(root: &Path) -> Result<Self, CaptureError> {
        let mut monitor = Self {
            queue: Kqueue::new().map_err(watch_error)?,
            files: std::collections::BTreeMap::new(),
        };
        monitor.register(root)?;
        Ok(monitor)
    }

    fn register(&mut self, path: &Path) -> Result<(), CaptureError> {
        // O_SYMLINK watches the link itself, including dangling/non-UTF-8 targets. O_EVTONLY
        // needs no read permission and cannot block opening a FIFO. Never follow links out
        // of the worktree while enumerating the watch set.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_EVTONLY | libc::O_SYMLINK)
            .open(path)
            .map_err(watch_error)?;
        let change = KEvent::new(
            file.as_raw_fd() as usize,
            EventFilter::EVFILT_VNODE,
            EvFlags::EV_ADD | EvFlags::EV_CLEAR | EvFlags::EV_RECEIPT,
            FilterFlag::NOTE_DELETE
                | FilterFlag::NOTE_WRITE
                | FilterFlag::NOTE_EXTEND
                | FilterFlag::NOTE_ATTRIB
                | FilterFlag::NOTE_LINK
                | FilterFlag::NOTE_RENAME
                | FilterFlag::NOTE_REVOKE,
            0,
            0,
        );
        let mut receipt = [change];
        let count = self
            .queue
            .kevent(
                &[change],
                &mut receipt,
                Some(libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            )
            .map_err(watch_error)?;
        if count != 1 || !receipt[0].flags().contains(EvFlags::EV_ERROR) {
            return Err(watch_error("missing vnode watch registration receipt"));
        }
        if receipt[0].data() != 0 {
            return Err(watch_error(nix::errno::Errno::from_raw(
                receipt[0].data() as i32
            )));
        }
        let metadata = file.metadata().map_err(watch_error)?;
        let directory = metadata.is_dir();
        self.files
            .insert(file.as_raw_fd() as usize, (file, metadata));
        if directory {
            for entry in std::fs::read_dir(path).map_err(watch_error)? {
                self.register(&entry.map_err(watch_error)?.path())?;
            }
        }
        Ok(())
    }

    pub(super) fn changed(&self) -> Result<bool, CaptureError> {
        let empty = KEvent::new(
            0,
            EventFilter::EVFILT_VNODE,
            EvFlags::empty(),
            FilterFlag::empty(),
            0,
            0,
        );
        let mut events = [empty];
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let count = self
                .queue
                .kevent(
                    &[],
                    &mut events,
                    Some(libc::timespec {
                        tv_sec: 0,
                        tv_nsec: remaining.as_nanos() as _,
                    }),
                )
                .map_err(watch_error)?;
            if count == 0 {
                return Ok(false);
            }
            let event = events[0];
            if event.flags().contains(EvFlags::EV_ERROR) {
                return Err(watch_error(
                    nix::errno::Errno::from_raw(event.data() as i32),
                ));
            }
            // macOS also emits NOTE_ATTRIB when git reads the index and updates its atime.
            // Access alone is not a mutation. Check the watched vnode's change stamp and
            // ownership/mode/link count before dismissing an attribute-only notification.
            // Every other flag, or any metadata disagreement, invalidates the attempt.
            if event.fflags() != FilterFlag::NOTE_ATTRIB {
                return Ok(true);
            }
            let Some((file, before)) = self.files.get(&event.ident()) else {
                return Err(watch_error("event for an unregistered vnode"));
            };
            let metadata = file.metadata().map_err(watch_error)?;
            if super::metadata_change_stamp(&metadata) != super::metadata_change_stamp(before)
                || (
                    metadata.mode(),
                    metadata.uid(),
                    metadata.gid(),
                    metadata.nlink(),
                    metadata.len(),
                ) != (
                    before.mode(),
                    before.uid(),
                    before.gid(),
                    before.nlink(),
                    before.len(),
                )
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                // An access storm cannot extend the boundary or hide queued mutations.
                return Err(watch_error(
                    "vnode events did not settle within the boundary",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn setup_and_read_access_do_not_dirty_a_new_boundary() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("index");
        std::fs::write(&path, b"before monitoring").unwrap();
        let monitor = WorktreeMonitor::start(root.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"before monitoring");
        assert!(!monitor.changed().unwrap());
    }

    #[test]
    fn a_transient_creation_survives_later_watch_registration() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let mut monitor = WorktreeMonitor::start(root.path()).unwrap();
        let transient = nested.join("transient");
        std::fs::write(&transient, b"never present in either scan").unwrap();
        std::fs::remove_file(&transient).unwrap();
        // Registration receipts must not consume an older directory mutation.
        monitor.register(&nested).unwrap();
        assert!(monitor.changed().unwrap());
        assert!(
            !WorktreeMonitor::start(root.path())
                .unwrap()
                .changed()
                .unwrap()
        );
    }

    #[test]
    fn metadata_aba_is_a_mutation_even_when_bytes_and_mode_agree() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"unchanged").unwrap();
        let original = std::fs::metadata(&path).unwrap().permissions();
        let monitor = WorktreeMonitor::start(root.path()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&path, original).unwrap();
        assert!(monitor.changed().unwrap());
    }

    #[test]
    fn links_are_watched_without_traversing_their_targets() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("link");
        symlink(outside.path(), &link).unwrap();
        symlink("absent", root.path().join("dangling")).unwrap();
        let monitor = WorktreeMonitor::start(root.path()).unwrap();
        std::fs::write(outside.path().join("private"), b"outside").unwrap();
        assert!(!monitor.changed().unwrap());
        std::fs::remove_file(&link).unwrap();
        symlink("other", &link).unwrap();
        assert!(monitor.changed().unwrap());
    }

    #[test]
    fn missing_watch_roots_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            WorktreeMonitor::start(&root.path().join("absent")),
            Err(CaptureError::Watch { .. })
        ));
    }

    #[test]
    fn descriptor_exhaustion_refuses_and_releases_partial_watches() {
        const CHILD: &str = "AF_CAPTURE_LOW_FD_TEST";
        if std::env::var_os(CHILD).is_none() {
            // Constrain only a fresh test process, never the parallel test runner.
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", "ulimit -n 64 || exit 90; exec \"$1\" --exact capture::macos_monitor::tests::descriptor_exhaustion_refuses_and_releases_partial_watches --nocapture", "sh"])
                .arg(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .status().unwrap();
            assert!(status.success(), "descriptor-limit child: {status}");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        for index in 0..128 {
            std::fs::write(root.path().join(index.to_string()), b"file").unwrap();
        }
        assert!(matches!(
            WorktreeMonitor::start(root.path()),
            Err(CaptureError::Watch { .. })
        ));
        let small = tempfile::tempdir().unwrap();
        assert!(
            !WorktreeMonitor::start(small.path())
                .unwrap()
                .changed()
                .unwrap()
        );
    }
}
