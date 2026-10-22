use crate::control::{ReceiveError, cancelled, receive};
use crate::drain::{Drain, Drained, collect_after_kill_until, collect_cancellable, drain_async};
use crate::{
    ExitPolicy, Leader, MAX_CAPTURED_STREAM_BYTES, Rusage, SupervisedError, SupervisedOutput,
    SupervisedStreamError, Truncation, stdin_writer_wait, wait_exact_cancellable,
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
    /// The streams that wrote more than their cap, with what each wrote in total. A truncated
    /// stream is a prefix: the caller decides what that means, and must not read it as whole.
    pub truncation: Truncation,
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
                truncation: self.truncation,
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

/// How many bytes of each stream a run keeps resident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    pub stdout: usize,
    pub stderr: usize,
}

impl Caps {
    /// Both streams at one cap: a captured run's output is evidence, never protocol data.
    pub(crate) fn both(cap: usize) -> Self {
        Self {
            stdout: cap,
            stderr: cap,
        }
    }

    /// A buffered run whose stdout is a trusted tool's whole answer (git plumbing, a version
    /// probe), parsed as one document: stdout is kept whole and only stderr is capped.
    ///
    /// Deliberately uncapped. [`MAX_CAPTURED_STREAM_BYTES`] bounds reviewer, Worker and check
    /// output, which a hostile repository or model can make endless; git plumbing output
    /// (`ls-files`, `ls-tree`) grows with the repository's own tree, and truncating it would
    /// break large repositories rather than bound an adversary.
    pub(crate) const PROTOCOL: Self = Self {
        stdout: usize::MAX,
        stderr: MAX_CAPTURED_STREAM_BYTES,
    };

    pub(crate) fn truncation(self, stdout_total: u64, stderr_total: u64) -> Truncation {
        let over = |total: u64, cap: usize| (total > cap as u64).then_some(total);
        let stdout = over(stdout_total, self.stdout);
        let stderr = over(stderr_total, self.stderr);
        Truncation {
            stdout,
            stderr,
            cap: if stdout.is_some() {
                self.stdout
            } else {
                self.stderr
            },
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
    captured_inner(
        command,
        input,
        timeout,
        exit_policy,
        None,
        Caps::both(MAX_CAPTURED_STREAM_BYTES),
    )
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
        Caps::both(MAX_CAPTURED_STREAM_BYTES),
    )
}

/// A captured run with a caller's own cap on each stream instead of
/// [`MAX_CAPTURED_STREAM_BYTES`], for a caller that needs less (or a test that proves the cap
/// without writing 64 MiB). Cancellation is optional, as everywhere else.
pub fn run_supervised_captured_bounded(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&AtomicBool>,
    cap: usize,
) -> SupervisedCapture {
    captured_inner(
        command,
        input,
        timeout,
        exit_policy,
        cancellation,
        Caps::both(cap),
    )
}

/// A captured run whose stdin the caller streams, so a large input is never one buffer: the
/// writer runs on the supervision's scoped thread and may borrow caller state. A writer that
/// meets a closed pipe stops quietly, as a buffered input does; the exit status then speaks.
pub fn run_supervised_captured_streaming<F>(
    command: &mut Command,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&AtomicBool>,
    writer: F,
) -> SupervisedCapture
where
    F: FnOnce(&mut dyn Write) -> std::io::Result<()> + Send,
{
    let writer = move |stdin: &mut dyn Write| match writer(stdin) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    };
    let capture = run_supervised_controlled(
        command,
        Some(writer),
        timeout,
        exit_policy,
        cancellation,
        Caps::both(MAX_CAPTURED_STREAM_BYTES),
    );
    capture.into_capture()
}

pub(crate) fn captured_inner(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&AtomicBool>,
    caps: Caps,
) -> SupervisedCapture {
    let writer = input.map(|input| {
        move |stdin: &mut dyn Write| match stdin.write_all(&input) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        }
    });
    run_supervised_controlled(command, writer, timeout, exit_policy, cancellation, caps)
        .into_capture()
}

pub(crate) struct StreamCapture<E> {
    status: Result<ExitStatus, SupervisedStreamError<E>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stderr_held: bool,
    pid: Option<u32>,
    rusage: Option<Rusage>,
    truncation: Truncation,
}

impl StreamCapture<std::io::Error> {
    fn into_capture(self) -> SupervisedCapture {
        SupervisedCapture {
            status: self.status.map_err(|error| match error {
                SupervisedStreamError::Process(error) => error,
                SupervisedStreamError::Input(error) => SupervisedError::Stdin(error),
            }),
            stdout: self.stdout,
            stderr: self.stderr,
            stderr_held: self.stderr_held,
            pid: self.pid,
            rusage: self.rusage,
            truncation: self.truncation,
        }
    }
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
            truncation: Truncation::default(),
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
                truncation: self.truncation,
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
    caps: Caps,
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
        truncation: caps.truncation(stdout.total, stderr.total),
        stdout: stdout.bytes,
        stderr: stderr.bytes,
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
    caps: Caps,
) -> StreamCapture<E> {
    let stderr_held = matches!(stderr.status, Ok(true));
    let truncation = caps.truncation(stdout.total, stderr.total);
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
        truncation,
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
    run_supervised_controlled(command, writer, timeout, exit_policy, None, Caps::PROTOCOL)
}

fn run_supervised_controlled<E, F>(
    command: &mut Command,
    writer: Option<F>,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&AtomicBool>,
    caps: Caps,
) -> StreamCapture<E>
where
    E: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), E> + Send,
{
    if cancelled(cancellation) {
        return StreamCapture::empty(SupervisedError::Cancelled);
    }
    // Marks every process of the run's tree, so that its descendants are told from any other
    // run's however they leave its process group.
    let tree =
        (exit_policy == ExitPolicy::KillDescendants).then(|| crate::reaper::Tree::new(command));
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
        let stdout = drain_async(stdout_pipe, caps.stdout);
        let stderr = drain_async(stderr_pipe, caps.stderr);
        let deadline = Instant::now() + timeout;
        let waited =
            wait_exact_cancellable(deadline, cancellation, &leader, exit_policy.kills_group());
        if let Some(tree) = &tree {
            // Before draining: an escaped descendant may hold a pipe, and must not outlive the
            // run that started it. The leader is still unreaped, so its group id is exact.
            tree.end_escaped(leader.pid());
        }
        if let Err(error) = waited {
            return failed(
                SupervisedStreamError::Process(error),
                stdout,
                stderr,
                &leader,
                caps,
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
                return failed(error, stdout, stderr, &leader, caps);
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
            (Ok(status), rusage) => completed(status, stdout, stderr, leader.pid(), rusage, caps),
            (Err(error), rusage) => StreamCapture {
                status: Err(SupervisedStreamError::Process(SupervisedError::Wait(error))),
                truncation: caps.truncation(stdout.total, stderr.total),
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
                total: 14,
                status: Ok(false),
            },
            Drained {
                bytes: b"diagnostic prefix".to_vec(),
                total: 17,
                status: Err(SupervisedError::OutputRead {
                    stream: "stderr",
                    source: std::io::Error::other("read failed"),
                }),
            },
            1,
            None,
            Caps::both(MAX_CAPTURED_STREAM_BYTES),
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
