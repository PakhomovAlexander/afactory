use crate::control::{ReceiveError, cancelled, receive};
use crate::drain::{Drain, Drained, collect_after_kill_until, collect_cancellable, drain_observed};
use crate::observe::{ObservedStream, OutputObserver, Tap};
use crate::{
    ExitPolicy, Leader, Rusage, SupervisedError, SupervisedOutput, SupervisedStreamError,
    stdin_writer_wait, wait_exact_cancellable,
};
use std::io::Write;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Process outcome and bytes are independent: failed transport cannot erase reported usage.
/// The process id and resource use survive every outcome a process was started for — a timed
/// out, killed or cancelled one included.
pub struct SupervisedCapture {
    pub status: Result<ExitStatus, SupervisedError>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stderr_held: bool,
    /// The leader's process id; `None` when nothing was started.
    pub pid: Option<u32>,
    /// What the reaped leader used; `None` when nothing was started or the platform delivered
    /// nothing ([`Rusage`]).
    pub rusage: Option<Rusage>,
}

impl SupervisedCapture {
    pub fn into_result(self) -> Result<SupervisedOutput, SupervisedError> {
        match self.status {
            Ok(status) => Ok(SupervisedOutput {
                status,
                stdout: self.stdout,
                stderr: self.stderr,
                stderr_held: self.stderr_held,
                pid: self.pid,
                rusage: self.rusage,
            }),
            Err(SupervisedError::TimedOut { .. }) => Err(SupervisedError::TimedOut {
                stdout: self.stdout,
                stderr: self.stderr,
                pid: self.pid,
                rusage: self.rusage,
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
    captured_inner(command, input, timeout, exit_policy, None, None)
}

/// Cancellation closes and reaps the same owned group as a deadline, retaining both streams.
pub fn run_supervised_captured_cancellable(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    cancellation: &AtomicBool,
) -> SupervisedCapture {
    run_supervised_captured_cancellable_with_policy(
        command,
        input,
        timeout,
        ExitPolicy::PreserveProcessGroup,
        cancellation,
    )
}

pub fn run_supervised_captured_cancellable_with_policy(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: &AtomicBool,
) -> SupervisedCapture {
    captured_inner(
        command,
        input,
        timeout,
        exit_policy,
        Some(cancellation),
        None,
    )
}

/// [`run_supervised_captured_with_policy`], cancellable when `cancellation` is given, whose
/// drains also offer every chunk they read to `observer` (design §7.3). The offer never
/// blocks and a full queue drops the chunk, so the capture — its bytes, status, deadline, held
/// output and resource use — is the one the same command gives without an observer.
pub fn run_supervised_captured_observed(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&AtomicBool>,
    observer: &OutputObserver,
) -> SupervisedCapture {
    captured_inner(
        command,
        input,
        timeout,
        exit_policy,
        cancellation,
        Some(observer),
    )
}

fn captured_inner(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&AtomicBool>,
    observer: Option<&OutputObserver>,
) -> SupervisedCapture {
    let writer = input.map(|input| {
        move |stdin: &mut dyn Write| match stdin.write_all(&input) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        }
    });
    let capture = run_supervised_controlled(
        command,
        writer,
        timeout,
        exit_policy,
        cancellation,
        observer,
    );
    SupervisedCapture {
        status: capture.status.map_err(|error| match error {
            SupervisedStreamError::Process(error) => error,
            SupervisedStreamError::Input(error) => SupervisedError::Stdin(error),
        }),
        stdout: capture.stdout,
        stderr: capture.stderr,
        stderr_held: capture.stderr_held,
        pid: capture.pid,
        rusage: capture.rusage,
    }
}

pub(crate) struct StreamCapture<E> {
    status: Result<ExitStatus, SupervisedStreamError<E>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stderr_held: bool,
    pid: Option<u32>,
    rusage: Option<Rusage>,
}

impl<E> StreamCapture<E> {
    fn empty(error: SupervisedError) -> Self {
        Self {
            status: Err(SupervisedStreamError::Process(error)),
            stdout: vec![],
            stderr: vec![],
            stderr_held: false,
            pid: None,
            rusage: None,
        }
    }

    pub(crate) fn into_result(self) -> Result<SupervisedOutput, SupervisedStreamError<E>> {
        match self.status {
            Ok(status) => Ok(SupervisedOutput {
                status,
                stdout: self.stdout,
                stderr: self.stderr,
                stderr_held: self.stderr_held,
                pid: self.pid,
                rusage: self.rusage,
            }),
            Err(SupervisedStreamError::Process(SupervisedError::TimedOut { .. })) => {
                Err(SupervisedStreamError::Process(SupervisedError::TimedOut {
                    stdout: self.stdout,
                    stderr: self.stderr,
                    pid: self.pid,
                    rusage: self.rusage,
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
    leader: &Leader,
) -> StreamCapture<E> {
    // Close the entire owned group before waiting for either drain or the scoped writer, then
    // reap the leader last, once nothing will signal its pid again.
    let deadline = Instant::now() + crate::OUTPUT_DRAIN_GRACE;
    leader.kill_group();
    let stdout = collect_after_kill_until(stdout, deadline);
    let stderr = collect_after_kill_until(stderr, deadline);
    let (_, rusage) = leader.reap();
    StreamCapture {
        status: Err(error),
        stdout,
        stderr,
        stderr_held: false,
        pid: Some(leader.pid()),
        rusage,
    }
}

fn completed<E>(
    status: ExitStatus,
    stdout: Drained,
    stderr: Drained,
    pid: u32,
    rusage: Option<Rusage>,
) -> StreamCapture<E> {
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
        pid: Some(pid),
        rusage,
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
    run_supervised_controlled(command, writer, timeout, exit_policy, None, None)
}

fn run_supervised_controlled<E, F>(
    command: &mut Command,
    writer: Option<F>,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&AtomicBool>,
    observer: Option<&OutputObserver>,
) -> StreamCapture<E>
where
    E: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), E> + Send,
{
    if cancelled(cancellation) {
        return StreamCapture::empty(SupervisedError::Cancelled);
    }
    command.stdin(if writer.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match crate::spawn::spawn(command) {
        Ok(child) => child,
        Err(error) => return StreamCapture::empty(SupervisedError::Spawn(error)),
    };
    let stdin_pipe = child.stdin.take();
    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    let leader = Leader::new(child);
    std::thread::scope(|scope| {
        let stdin_result = writer.map(|writer| {
            let mut stdin = stdin_pipe.expect("stdin was piped");
            let (send, receive) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer(&mut stdin)));
                let _ = send.send(result);
            });
            receive
        });
        let tap = |stream| {
            observer.map(|observer| Tap {
                observer: observer.clone(),
                process: leader.pid(),
                stream,
            })
        };
        let stdout = drain_observed(stdout_pipe, tap(ObservedStream::Stdout));
        let stderr = drain_observed(stderr_pipe, tap(ObservedStream::Stderr));
        let deadline = Instant::now() + timeout;
        let kill_group_on_exit = exit_policy == ExitPolicy::KillProcessGroup;
        if let Err(error) =
            wait_exact_cancellable(deadline, cancellation, &leader, kill_group_on_exit)
        {
            return failed(
                SupervisedStreamError::Process(error),
                stdout,
                stderr,
                &leader,
            );
        }
        if let Some(receiver) = stdin_result {
            let error = match receive(&receiver, stdin_writer_wait(deadline), cancellation) {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(error))) => Some(SupervisedStreamError::Input(error)),
                Ok(Err(_)) => Some(SupervisedStreamError::Process(SupervisedError::Stdin(
                    std::io::Error::other("input writer panicked"),
                ))),
                Err(ReceiveError::Cancelled) => {
                    Some(SupervisedStreamError::Process(SupervisedError::Cancelled))
                }
                Err(ReceiveError::Timeout) => {
                    Some(SupervisedStreamError::Process(SupervisedError::TimedOut {
                        stdout: vec![],
                        stderr: vec![],
                        pid: None,
                        rusage: None,
                    }))
                }
                Err(ReceiveError::Disconnected) => {
                    Some(SupervisedStreamError::Process(SupervisedError::Stdin(
                        std::io::Error::other("input writer stopped without a result"),
                    )))
                }
            };
            if let Some(error) = error {
                return failed(error, stdout, stderr, &leader);
            }
        }
        let mut cleanup = None;
        let stdout = collect_cancellable(stdout, "stdout", &leader, &mut cleanup, cancellation);
        if stdout.status.is_err() {
            cleanup.get_or_insert_with(|| Instant::now() + crate::OUTPUT_DRAIN_GRACE);
            leader.kill_group();
        }
        let stderr = collect_cancellable(stderr, "stderr", &leader, &mut cleanup, cancellation);
        if stderr.status.is_err() {
            leader.kill_group();
        }
        let cancelled_now = cancelled(cancellation);
        if cancelled_now {
            leader.kill_group();
        }
        // Reaping is the final operation: every kill above ran while the pid was reserved.
        let mut capture = match leader.reap() {
            (Ok(status), rusage) => completed(status, stdout, stderr, leader.pid(), rusage),
            (Err(error), rusage) => StreamCapture {
                status: Err(SupervisedStreamError::Process(SupervisedError::Wait(error))),
                stdout: stdout.bytes,
                stderr: stderr.bytes,
                stderr_held: false,
                pid: Some(leader.pid()),
                rusage,
            },
        };
        if capture.status.is_ok() && cancelled_now {
            capture.status = Err(SupervisedStreamError::Process(SupervisedError::Cancelled));
        }
        capture
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> ExitStatus {
        crate::reap::status(std::process::Command::new("/bin/sh").args(["-c", "exit 0"])).unwrap()
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
            1,
            None,
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
            crate::tests::LOAD_SAFE_WALL,
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
        let capture = run_supervised_captured(&mut command, None, crate::tests::LOAD_SAFE_WALL);
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
