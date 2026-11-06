//! Attempt-local root vnode watch plus complete no-follow metadata observations.
//! Descendant ctime/mtime stamps retain mutation and ABA evidence without holding one
//! descriptor per path. Every directory lookup is anchored to the held root descriptor.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::dir::Dir;
use nix::fcntl::{OFlag, openat};
use nix::libc;
use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
use nix::sys::stat::Mode;

use super::{CaptureError, ChangeStamp};

type Observation = (ChangeStamp, u32, u32, u32, u64, u64);
type Observations = BTreeMap<PathBuf, Observation>;

pub(super) struct WorktreeMonitor {
    queue: Kqueue,
    root: File,
    before: Observations,
}

fn watch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Watch {
        detail: error.to_string(),
    }
}

fn observation(metadata: &std::fs::Metadata) -> Observation {
    (
        super::metadata_change_stamp(metadata),
        metadata.mode(),
        metadata.uid(),
        metadata.gid(),
        metadata.nlink(),
        metadata.len(),
    )
}

fn directory_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ObservePoint {
    BeforeComponent,
    OpenedDirectory,
    Entry,
}

fn observe(root: &File) -> Result<Observations, CaptureError> {
    observe_with(root, &mut |_, _| {})
}

fn observe_with(
    root: &File,
    hook: &mut impl FnMut(&Path, ObservePoint),
) -> Result<Observations, CaptureError> {
    let mut result = BTreeMap::new();
    result.insert(
        PathBuf::new(),
        observation(&root.metadata().map_err(watch_error)?),
    );
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        // Reopen one component at a time: neither path depth nor tree size increases
        // the live descriptor count. NOFOLLOW applies to *every* component.
        let mut parent = root.try_clone().map_err(watch_error)?;
        let mut prefix = PathBuf::new();
        for component in relative.components() {
            prefix.push(component.as_os_str());
            hook(&prefix, ObservePoint::BeforeComponent);
            parent = File::from(
                openat(
                    &parent,
                    Path::new(component.as_os_str()),
                    directory_flags(),
                    Mode::empty(),
                )
                .map_err(watch_error)?,
            );
        }
        // A directory replaced since it was queued cannot quietly change this scan.
        if result.get(&relative) != Some(&observation(&parent.metadata().map_err(watch_error)?)) {
            return Err(watch_error("directory changed during observation"));
        }
        hook(&relative, ObservePoint::OpenedDirectory);
        // Independent directory offset; cloning an fd would share readdir's offset.
        let mut entries = Dir::from_fd(
            openat(&parent, ".", directory_flags(), Mode::empty()).map_err(watch_error)?,
        )
        .map_err(watch_error)?;
        for entry in entries.iter() {
            let entry = entry.map_err(watch_error)?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let name = Path::new(std::ffi::OsStr::from_bytes(name));
            // Event-only opens cannot block on a FIFO; O_SYMLINK observes the link
            // itself, including dangling links, rather than its external target.
            let child = File::from(
                openat(
                    &parent,
                    name,
                    OFlag::from_bits_retain(libc::O_EVTONLY | libc::O_SYMLINK | libc::O_CLOEXEC),
                    Mode::empty(),
                )
                .map_err(watch_error)?,
            );
            let metadata = child.metadata().map_err(watch_error)?;
            drop(child);
            let path = relative.join(name);
            hook(&path, ObservePoint::Entry);
            result.insert(path.clone(), observation(&metadata));
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(result)
}

impl WorktreeMonitor {
    pub(super) fn start(root: &Path) -> Result<Self, CaptureError> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)
            .map_err(watch_error)?;
        let queue = Kqueue::new().map_err(watch_error)?;
        let change = KEvent::new(
            root.as_raw_fd() as usize,
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
        let count = queue
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
        let before = observe(&root)?;
        Ok(Self {
            queue,
            root,
            before,
        })
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
                break;
            }
            let event = events[0];
            if event.flags().contains(EvFlags::EV_ERROR) {
                return Err(watch_error(
                    nix::errno::Errno::from_raw(event.data() as i32),
                ));
            }
            if event.ident() != self.root.as_raw_fd() as usize {
                return Err(watch_error("event for an unregistered vnode"));
            }
            if event.fflags() != FilterFlag::NOTE_ATTRIB {
                return Ok(true);
            }
            if self.before.get(Path::new(""))
                != Some(&observation(&self.root.metadata().map_err(watch_error)?))
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Err(watch_error(
                    "vnode events did not settle within the boundary",
                ));
            }
        }
        Ok(observe(&self.root)? != self.before)
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
    fn a_transient_creation_survives_an_intermediate_observation() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let monitor = WorktreeMonitor::start(root.path()).unwrap();
        let transient = nested.join("transient");
        std::fs::write(&transient, b"never present in either scan").unwrap();
        std::fs::remove_file(&transient).unwrap();
        // A complete second observation retains the parent directory change stamp.
        observe(&monitor.root).unwrap();
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
    fn ignored_descendant_content_aba_is_detected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ignored")).unwrap();
        let file = root.path().join("ignored/file");
        std::fs::write(&file, b"before").unwrap();
        let monitor = WorktreeMonitor::start(root.path()).unwrap();
        std::fs::write(&file, b"during").unwrap();
        std::fs::write(&file, b"before").unwrap();
        assert!(monitor.changed().unwrap());
    }

    #[test]
    fn root_replacement_invalidates_the_held_root() {
        let container = tempfile::tempdir().unwrap();
        let root = container.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let monitor = WorktreeMonitor::start(&root).unwrap();
        std::fs::rename(&root, container.path().join("old")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(monitor.changed().unwrap());
    }

    #[test]
    fn replacement_after_directory_open_cannot_redirect_enumeration() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("nested/deeper")).unwrap();
        std::fs::write(root.path().join("nested/local-only"), b"local").unwrap();
        std::fs::write(outside.path().join("outside-only"), b"private").unwrap();
        let monitor = WorktreeMonitor::start(root.path()).unwrap();
        let mut replaced = false;
        let mut local_seen = false;
        let result = observe_with(&monitor.root, &mut |path, point| {
            assert_ne!(path.file_name(), Some(std::ffi::OsStr::new("outside-only")));
            if path == Path::new("nested") && point == ObservePoint::OpenedDirectory {
                std::fs::rename(root.path().join("nested"), root.path().join("saved")).unwrap();
                symlink(outside.path(), root.path().join("nested")).unwrap();
                replaced = true;
            }
            if path == Path::new("nested/local-only") && point == ObservePoint::Entry {
                local_seen = true;
            }
        });
        assert!(
            replaced && local_seen,
            "the held directory must actually be enumerated"
        );
        // Reopening nested/deeper must refuse the substituted intermediate symlink.
        assert!(matches!(result, Err(CaptureError::Watch { .. })));
        assert!(monitor.changed().unwrap());
    }

    #[test]
    fn replacement_during_component_resolution_cannot_follow_external_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("nested/deeper")).unwrap();
        std::fs::create_dir(outside.path().join("deeper")).unwrap();
        std::fs::write(outside.path().join("deeper/outside-only"), b"private").unwrap();
        let monitor = WorktreeMonitor::start(root.path()).unwrap();
        let mut nested_lookups = 0;
        let result = observe_with(&monitor.root, &mut |path, point| {
            assert_ne!(path.file_name(), Some(std::ffi::OsStr::new("outside-only")));
            if path == Path::new("nested") && point == ObservePoint::BeforeComponent {
                nested_lookups += 1;
                if nested_lookups == 2 {
                    std::fs::rename(root.path().join("nested"), root.path().join("saved")).unwrap();
                    symlink(outside.path(), root.path().join("nested")).unwrap();
                }
            }
        });
        assert_eq!(
            nested_lookups, 2,
            "exercise reopening an intermediate component"
        );
        assert!(matches!(result, Err(CaptureError::Watch { .. })));
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
        let mut held = Vec::new();
        while let Ok(file) = File::open(root.path()) {
            held.push(file);
        }
        assert!(matches!(
            WorktreeMonitor::start(root.path()),
            Err(CaptureError::Watch { .. })
        ));
        // Allow the root and kqueue to open, but exhaust the first observation's
        // lookup handle. A partial start must also fail closed and release both.
        drop(held.pop().unwrap());
        drop(held.pop().unwrap());
        assert!(matches!(
            WorktreeMonitor::start(root.path()),
            Err(CaptureError::Watch { .. })
        ));
        let released_root = File::open(root.path()).unwrap();
        let released_queue = File::open(root.path()).unwrap();
        assert!(File::open(root.path()).is_err());
        drop((released_root, released_queue));
        drop(held);
        let small = tempfile::tempdir().unwrap();
        assert!(
            !WorktreeMonitor::start(small.path())
                .unwrap()
                .changed()
                .unwrap()
        );
    }
}
