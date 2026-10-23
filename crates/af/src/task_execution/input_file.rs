//! Bounded local configuration reads before a Task runtime or deadline supervisor exists.
use std::io::Read;
use std::path::Path;

/// The bytes of the local input `path`, named `what` in every refusal (`cannot open Task file
/// ticket.json: No such file or directory (os error 2)`), at most `max` of them.
pub(super) fn read(path: &Path, max: u64, what: &str) -> Result<Vec<u8>, String> {
    use rustix::fs::{Mode, OFlags, open};
    let limit = max.checked_add(1).ok_or("Invalid Task input byte bound")?;
    let shown = path.display();
    // Explicit local paths retain their existing symlink behavior. Inspect the opened
    // object: a symlink to a FIFO must not block before a Task can be admitted.
    let file: std::fs::File = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        let error = std::io::Error::from(error);
        // Spelled out for the Task file, the input people name most, as the troubleshooting
        // table in docs/README.md quotes it.
        if what == "Task file" {
            format!("cannot open Task file {shown}: {error}")
        } else {
            format!("cannot open {what} {shown}: {error}")
        }
    })?
    .into();
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {what} {shown}: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{what} {shown} must be a regular file"));
    }
    if metadata.len() > max {
        return Err(format!(
            "{what} {shown} exceeds its byte bound of {max} bytes"
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {what} {shown}: {error}"))?;
    if bytes.len() as u64 > max {
        return Err(format!(
            "{what} {shown} exceeds its byte bound of {max} bytes"
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_input_checks_the_open_file_and_original_byte_bound() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("input");
        std::fs::write(&file, b"exact").unwrap();
        assert_eq!(read(&file, 5, "Task file").unwrap(), b"exact");
        assert!(
            read(&file, 4, "Task file")
                .unwrap_err()
                .contains("byte bound")
        );
        assert!(
            read(root.path(), 100, "Task file")
                .unwrap_err()
                .contains("regular file")
        );
        assert!(read(&file, u64::MAX, "Task file").is_err());
        {
            let link = root.path().join("explicit-link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert_eq!(read(&link, 5, "Task file").unwrap(), b"exact");
        }
    }

    #[test]
    fn fifo_child() {
        let Some(path) = std::env::var_os("AF_TEST_TASK_INPUT_FIFO") else {
            return;
        };
        assert!(
            read(Path::new(&path), 64, "Task file")
                .unwrap_err()
                .contains("regular file")
        );
        std::fs::write(Path::new(&path).with_extension("rejected"), b"refused").unwrap();
    }

    #[test]
    fn fifo_and_its_explicit_symlink_refuse_without_a_writer() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("input.fifo");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let link = root.path().join("fifo-link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        for path in [&fifo, &link] {
            // Bound the regression itself: the old blocking open hangs without a writer.
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "task_execution::input_file::tests::fifo_child"])
                .env("AF_TEST_TASK_INPUT_FIFO", path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    assert_eq!(
                        std::fs::read(path.with_extension("rejected")).unwrap(),
                        b"refused"
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("Task input reader blocked on a FIFO");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
