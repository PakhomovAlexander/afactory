use crate::drain::{Drain, Drained, collect, collect_after_kill_until, drain_async};
use crate::{
    ExitPolicy, SupervisedError, SupervisedOutput, SupervisedStreamError, kill_process_group,
    stdin_writer_wait, wait_exact,
};
use std::io::Write;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Process outcome and bytes are independent: failed transport cannot erase reported usage.
pub struct SupervisedCapture {
    pub status: Result<ExitStatus, SupervisedError>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stderr_held: bool,
}

impl SupervisedCapture {
    pub fn into_result(self) -> Result<SupervisedOutput, SupervisedError> {
        match self.status {
            Ok(status) => Ok(SupervisedOutput {
                status,
                stdout: self.stdout,
                stderr: self.stderr,
                stderr_held: self.stderr_held,
            }),
            Err(SupervisedError::TimedOut { .. }) => Err(SupervisedError::TimedOut {
                stdout: self.stdout,
                stderr: self.stderr,
            }),
            Err(error) => Err(error),
        }
    }
}

pub fn run_supervised_captured(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> SupervisedCapture {
    run_supervised_captured_with_policy(command, input, timeout, ExitPolicy::PreserveProcessGroup)
}

pub fn run_supervised_captured_with_policy(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    exit_policy: ExitPolicy,
) -> SupervisedCapture {
    let writer = input.map(|input| {
        move |stdin: &mut dyn Write| match stdin.write_all(&input) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        }
    });
    let capture = run_supervised_inner(command, writer, timeout, exit_policy);
    SupervisedCapture {
        status: capture.status.map_err(|error| match error {
            SupervisedStreamError::Process(error) => error,
            SupervisedStreamError::Input(error) => SupervisedError::Stdin(error),
        }),
        stdout: capture.stdout,
        stderr: capture.stderr,
        stderr_held: capture.stderr_held,
    }
}

pub(crate) struct StreamCapture<E> {
    status: Result<ExitStatus, SupervisedStreamError<E>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stderr_held: bool,
}

impl<E> StreamCapture<E> {
    fn empty(error: SupervisedError) -> Self {
        Self {
            status: Err(SupervisedStreamError::Process(error)),
            stdout: vec![],
            stderr: vec![],
            stderr_held: false,
        }
    }

    pub(crate) fn into_result(self) -> Result<SupervisedOutput, SupervisedStreamError<E>> {
        match self.status {
            Ok(status) => Ok(SupervisedOutput {
                status,
                stdout: self.stdout,
                stderr: self.stderr,
                stderr_held: self.stderr_held,
            }),
            Err(SupervisedStreamError::Process(SupervisedError::TimedOut { .. })) => {
                Err(SupervisedStreamError::Process(SupervisedError::TimedOut {
                    stdout: self.stdout,
                    stderr: self.stderr,
                }))
            }
            Err(error) => Err(error),
        }
    }
}

fn failed<E>(
    error: SupervisedStreamError<E>,
    stdout: Drain,
    stderr: Drain,
    pid: u32,
) -> StreamCapture<E> {
    // Close the entire owned group before waiting for either drain or the scoped writer.
    let deadline = Instant::now() + crate::OUTPUT_DRAIN_GRACE;
    kill_process_group(pid);
    StreamCapture {
        status: Err(error),
        stdout: collect_after_kill_until(stdout, deadline),
        stderr: collect_after_kill_until(stderr, deadline),
        stderr_held: false,
    }
}

fn completed<E>(status: ExitStatus, stdout: Drained, stderr: Drained) -> StreamCapture<E> {
    let stderr_held = matches!(stderr.status, Ok(true));
    let status = stdout
        .status
        .and(stderr.status)
        .map(|_| status)
        .map_err(SupervisedStreamError::Process);
    StreamCapture {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stderr_held,
    }
}

pub(crate) fn run_supervised_inner<E, F>(
    command: &mut Command,
    writer: Option<F>,
    timeout: Duration,
    exit_policy: ExitPolicy,
) -> StreamCapture<E>
where
    E: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), E> + Send,
{
    command.stdin(if writer.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return StreamCapture::empty(SupervisedError::Spawn(error)),
    };
    let pid = child.id();
    std::thread::scope(|scope| {
        let stdin_result = writer.map(|writer| {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            let (send, receive) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer(&mut stdin)));
                let _ = send.send(result);
            });
            receive
        });
        let stdout = drain_async(child.stdout.take().expect("stdout was piped"));
        let stderr = drain_async(child.stderr.take().expect("stderr was piped"));
        let deadline = Instant::now() + timeout;
        let status = match wait_exact(child, deadline) {
            Ok(status) => status,
            Err(error) => {
                return failed(SupervisedStreamError::Process(error), stdout, stderr, pid);
            }
        };
        if exit_policy == ExitPolicy::KillProcessGroup {
            kill_process_group(pid);
        }
        if let Some(receiver) = stdin_result {
            let error = match receiver.recv_timeout(stdin_writer_wait(deadline)) {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(error))) => Some(SupervisedStreamError::Input(error)),
                Ok(Err(_)) => Some(SupervisedStreamError::Process(SupervisedError::Stdin(
                    std::io::Error::other("input writer panicked"),
                ))),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    Some(SupervisedStreamError::Process(SupervisedError::TimedOut {
                        stdout: vec![],
                        stderr: vec![],
                    }))
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    Some(SupervisedStreamError::Process(SupervisedError::Stdin(
                        std::io::Error::other("input writer stopped without a result"),
                    )))
                }
            };
            if let Some(error) = error {
                return failed(error, stdout, stderr, pid);
            }
        }
        let mut cleanup = None;
        let stdout = collect(stdout, "stdout", pid, &mut cleanup);
        if stdout.status.is_err() {
            cleanup.get_or_insert_with(|| Instant::now() + crate::OUTPUT_DRAIN_GRACE);
            kill_process_group(pid);
        }
        let stderr = collect(stderr, "stderr", pid, &mut cleanup);
        if stderr.status.is_err() {
            kill_process_group(pid);
        }
        completed(status, stdout, stderr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> ExitStatus {
        std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .status()
            .unwrap()
    }

    #[test]
    fn stderr_read_failure_preserves_completed_stdout_and_both_prefixes() {
        let capture: StreamCapture<()> = completed(
            status(),
            Drained {
                bytes: b"reported usage".to_vec(),
                status: Ok(false),
            },
            Drained {
                bytes: b"diagnostic prefix".to_vec(),
                status: Err(SupervisedError::OutputRead {
                    stream: "stderr",
                    source: std::io::Error::other("read failed"),
                }),
            },
        );
        assert!(matches!(
            capture.status,
            Err(SupervisedStreamError::Process(
                SupervisedError::OutputRead {
                    stream: "stderr",
                    ..
                }
            ))
        ));
        assert_eq!(capture.stdout, b"reported usage");
        assert_eq!(capture.stderr, b"diagnostic prefix");
    }

    #[test]
    fn stream_input_failure_keeps_both_outputs_and_its_error_type() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf output; printf diagnostic >&2"]);
        let capture = run_supervised_inner(
            &mut command,
            Some(|_: &mut dyn Write| Err("original typed input failure")),
            Duration::from_secs(5),
            ExitPolicy::PreserveProcessGroup,
        );
        assert_eq!(capture.stdout, b"output");
        assert_eq!(capture.stderr, b"diagnostic");
        assert!(matches!(
            capture.into_result(),
            Err(SupervisedStreamError::Input("original typed input failure"))
        ));
    }

    #[test]
    fn held_stdout_capture_and_compatibility_wrapper_keep_the_same_failure() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf output; printf diagnostic >&2; sleep 30 &"]);
        let started = Instant::now();
        let capture = run_supervised_captured(&mut command, None, Duration::from_secs(1));
        assert_eq!(capture.stdout, b"output");
        assert_eq!(capture.stderr, b"diagnostic");
        assert!(matches!(
            capture.status,
            Err(SupervisedError::OutputHeld("stdout"))
        ));
        assert!(matches!(
            capture.into_result(),
            Err(SupervisedError::OutputHeld("stdout"))
        ));
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn spawn_failure_does_not_invent_output() {
        let capture = run_supervised_captured(
            &mut Command::new("/nonexistent/af-process-fixture"),
            None,
            Duration::from_millis(1),
        );
        assert!(matches!(capture.status, Err(SupervisedError::Spawn(_))));
        assert!(capture.stdout.is_empty() && capture.stderr.is_empty());
    }
}
