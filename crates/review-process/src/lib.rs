//! One bounded subprocess boundary shared by reviewers, checks, and sandbox providers.

use std::io::{Read, Write};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

const STDIN_EXIT_GRACE: Duration = Duration::from_millis(500);
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// The most stderr bytes one duplex-supervised process's diagnostics are kept for.
///
/// Stderr is diagnostics, never the answer: callers quote its last line and store nothing else,
/// so by construction it is orders of magnitude smaller than the stdout a bounded reader is
/// draining beside it. 1 MiB holds thousands of log lines — far more than any excerpt needs —
/// and closes the hole a stdout ceiling alone leaves: a producer that writes its runaway output
/// to fd 2 instead of fd 1 (`yes >&2`, or a CLI whose progress and tracing go to stderr) grows
/// this process's memory without limit while the stdout reader sits idle and never aborts.
/// Past the ceiling the tail is read and discarded rather than left unread — an unread pipe
/// blocks the producer instead of ending it — and the kept bytes carry a truncation marker, so
/// a cut is never mistaken for the whole stream.
pub const MAX_STDERR_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub enum SupervisedError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
    TimedOut {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Stdin(std::io::Error),
    OutputRead {
        stream: &'static str,
        source: std::io::Error,
    },
    OutputHeld(&'static str),
    /// The caller's stdout reader refused to read further (see [`AbortSignal`]); the process
    /// group was ended. What the reader kept is the caller's; stderr up to the kill is here.
    OutputRefused {
        stderr: Vec<u8>,
    },
}

impl std::fmt::Display for SupervisedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "starting process: {error}"),
            Self::Wait(error) => write!(f, "waiting for process: {error}"),
            Self::TimedOut { .. } => write!(f, "process exceeded its deadline"),
            Self::Stdin(error) => write!(f, "delivering process input: {error}"),
            Self::OutputRead { stream, source } => {
                write!(f, "reading process {stream}: {source}")
            }
            Self::OutputHeld(stream) => {
                write!(f, "process {stream} pipe was still held after 5 seconds")
            }
            Self::OutputRefused { .. } => {
                write!(
                    f,
                    "process stdout was refused by its reader and the process was ended"
                )
            }
        }
    }
}

impl std::error::Error for SupervisedError {}

pub struct SupervisedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// A descendant kept stderr open after the leader exited. Stdout remains complete evidence;
    /// callers may surface this flag as a diagnostic without discarding the answer.
    pub stderr_held: bool,
}

#[derive(Debug)]
pub enum SupervisedStreamError<E> {
    Process(SupervisedError),
    Input(E),
}

pub struct SupervisedDuplexOutput<I, R> {
    pub status: ExitStatus,
    pub input: Result<(), I>,
    pub output: R,
    pub stderr: Vec<u8>,
    pub stderr_held: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitPolicy {
    /// Preserve descendants after the leader exits; a held pipe becomes an observable failure.
    PreserveProcessGroup,
    /// End the entire process group when the leader exits, closing pipes held by background work.
    KillProcessGroup,
}

/// Run one command in its own process group with an exact deadline and bounded pipe lifetimes.
pub fn run_supervised(
    command: &mut std::process::Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<SupervisedOutput, SupervisedError> {
    run_supervised_with_policy(command, input, timeout, ExitPolicy::PreserveProcessGroup)
}

pub fn run_supervised_with_policy(
    command: &mut std::process::Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    exit_policy: ExitPolicy,
) -> Result<SupervisedOutput, SupervisedError> {
    let writer = input.map(|input| {
        move |stdin: &mut dyn Write| match stdin.write_all(&input) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        }
    });
    run_supervised_inner(command, writer, timeout, exit_policy).map_err(|error| match error {
        SupervisedStreamError::Process(error) => error,
        SupervisedStreamError::Input(error) => SupervisedError::Stdin(error),
    })
}

/// Run a bounded process while a caller streams its stdin without first materializing one large
/// input buffer. The writer may borrow caller state; process-group termination closes the pipe
/// before this function returns on any deadline or lifecycle failure.
pub fn run_supervised_streaming<E, F>(
    command: &mut std::process::Command,
    timeout: Duration,
    exit_policy: ExitPolicy,
    writer: F,
) -> Result<SupervisedOutput, SupervisedStreamError<E>>
where
    E: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), E> + Send,
{
    run_supervised_inner(command, Some(writer), timeout, exit_policy)
}

/// What the exact child wait can learn: the leader exited, or the stdout reader asked for the
/// process group to end.
enum Waited {
    Exited(std::io::Result<ExitStatus>),
    Abort,
}

/// Handed to a duplex stdout reader. `abort` ends the whole process group as soon as the
/// supervisor observes it — a reader that has hit a byte ceiling stops the producer instead of
/// letting it run to the deadline or block on a full pipe. Sending twice is harmless.
pub struct AbortSignal {
    sender: std::sync::mpsc::Sender<Waited>,
}

impl AbortSignal {
    pub fn abort(&self) {
        let _ = self.sender.send(Waited::Abort);
    }
}

/// Run a bounded process with caller-defined streaming on both stdin and stdout. The callbacks
/// execute on the shared process boundary's scoped threads, so a protocol parser can keep one
/// object resident at a time while deadline and process-group policy remain centralized here.
pub fn run_supervised_duplex<I, R, F, G>(
    command: &mut std::process::Command,
    timeout: Duration,
    exit_policy: ExitPolicy,
    writer: F,
    reader: G,
) -> Result<SupervisedDuplexOutput<I, R>, SupervisedError>
where
    I: Send,
    R: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), I> + Send,
    G: FnOnce(&mut dyn Read) -> R + Send,
{
    run_supervised_duplex_with_abort(command, timeout, exit_policy, Some(writer), |stdout, _| {
        reader(stdout)
    })
}

/// [`run_supervised_duplex`] whose stdout reader may end the process early through an
/// [`AbortSignal`]. On abort the process group is killed, its exit reaped, and the call returns
/// [`SupervisedError::OutputRefused`]; whatever the reader retained through captured state is
/// still the caller's, exactly as on a deadline.
///
/// `writer` decides what fd 0 is, exactly as it does for [`run_supervised`]: `Some` gives the
/// child a pipe, `None` gives it `/dev/null`. Which one a child sees is observable — CLIs that
/// read a prompt from stdin branch on it — so a caller with no input must not hand one a pipe.
pub fn run_supervised_duplex_with_abort<I, R, F, G>(
    command: &mut std::process::Command,
    timeout: Duration,
    exit_policy: ExitPolicy,
    writer: Option<F>,
    reader: G,
) -> Result<SupervisedDuplexOutput<I, R>, SupervisedError>
where
    I: Send,
    R: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), I> + Send,
    G: FnOnce(&mut dyn Read, AbortSignal) -> R + Send,
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

    let mut child = command.spawn().map_err(SupervisedError::Spawn)?;
    let pid = child.id();
    std::thread::scope(|scope| {
        let input_receive = writer.map(|writer| {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            let (input_send, input_receive) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer(&mut stdin)));
                let _ = input_send.send(result);
            });
            input_receive
        });

        let (waited_send, waited_receive) = std::sync::mpsc::channel();
        let abort = AbortSignal {
            sender: waited_send.clone(),
        };
        let mut stdout = child.stdout.take().expect("stdout was piped");
        let (output_send, output_receive) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reader(&mut stdout, abort)
            }));
            let _ = output_send.send(result);
        });

        let stderr = drain_async_bounded(
            child.stderr.take().expect("stderr was piped"),
            MAX_STDERR_BYTES,
        );
        let deadline = Instant::now() + timeout;
        let status = match wait_exact_or_abort(child, deadline, waited_send, waited_receive) {
            Ok(Waited::Exited(Ok(status))) => status,
            Ok(Waited::Exited(Err(error))) => return Err(SupervisedError::Wait(error)),
            Ok(Waited::Abort) => {
                return Err(SupervisedError::OutputRefused {
                    stderr: collect_after_kill(stderr),
                });
            }
            Err(error) => return Err(error),
        };
        if exit_policy == ExitPolicy::KillProcessGroup {
            kill_process_group(pid);
        }

        let input = match input_receive {
            // No writer, so nothing was delivered and nothing can have failed: the child was
            // given `/dev/null` on fd 0 and never had a pipe to wait on.
            None => Ok(()),
            Some(receiver) => match receiver.recv_timeout(stdin_writer_wait(deadline)) {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => {
                    return Err(SupervisedError::Stdin(std::io::Error::other(
                        "input writer panicked",
                    )));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    kill_process_group(pid);
                    return Err(SupervisedError::TimedOut {
                        stdout: Vec::new(),
                        stderr: collect_after_kill(stderr),
                    });
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(SupervisedError::Stdin(std::io::Error::other(
                        "input writer stopped without a result",
                    )));
                }
            },
        };
        let output = match output_receive.recv_timeout(OUTPUT_DRAIN_GRACE) {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => {
                return Err(SupervisedError::OutputRead {
                    stream: "stdout",
                    source: std::io::Error::other("output reader panicked"),
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                kill_process_group(pid);
                return Err(SupervisedError::OutputHeld("stdout"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(SupervisedError::OutputRead {
                    stream: "stdout",
                    source: std::io::Error::other("output reader stopped without a result"),
                });
            }
        };
        let (stderr, stderr_held) = collect_stderr(stderr, pid)?;
        Ok(SupervisedDuplexOutput {
            status,
            input,
            output,
            stderr,
            stderr_held,
        })
    })
}

fn run_supervised_inner<E, F>(
    command: &mut std::process::Command,
    writer: Option<F>,
    timeout: Duration,
    exit_policy: ExitPolicy,
) -> Result<SupervisedOutput, SupervisedStreamError<E>>
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

    let mut child = command
        .spawn()
        .map_err(SupervisedError::Spawn)
        .map_err(SupervisedStreamError::Process)?;
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
            Err(SupervisedError::TimedOut { .. }) => {
                return Err(SupervisedStreamError::Process(SupervisedError::TimedOut {
                    stdout: collect_after_kill(stdout),
                    stderr: collect_after_kill(stderr),
                }));
            }
            Err(error) => return Err(SupervisedStreamError::Process(error)),
        };
        if exit_policy == ExitPolicy::KillProcessGroup {
            kill_process_group(pid);
        }

        if let Some(receiver) = stdin_result {
            match receiver.recv_timeout(stdin_writer_wait(deadline)) {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => return Err(SupervisedStreamError::Input(error)),
                Ok(Err(_)) => {
                    return Err(SupervisedStreamError::Process(SupervisedError::Stdin(
                        std::io::Error::other("input writer panicked"),
                    )));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    kill_process_group(pid);
                    return Err(SupervisedStreamError::Process(SupervisedError::TimedOut {
                        stdout: collect_after_kill(stdout),
                        stderr: collect_after_kill(stderr),
                    }));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(SupervisedStreamError::Process(SupervisedError::Stdin(
                        std::io::Error::other("input writer stopped without a result"),
                    )));
                }
            }
        }

        let stdout =
            collect_output(stdout, "stdout", pid).map_err(SupervisedStreamError::Process)?;
        let (stderr, stderr_held) =
            collect_stderr(stderr, pid).map_err(SupervisedStreamError::Process)?;
        Ok(SupervisedOutput {
            status,
            stdout,
            stderr,
            stderr_held,
        })
    })
}

fn collect_after_kill(receiver: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>) -> Vec<u8> {
    receiver
        .recv_timeout(OUTPUT_DRAIN_GRACE)
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

fn stdin_writer_wait(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(STDIN_EXIT_GRACE)
}

fn drain_async(
    mut pipe: impl Read + Send + 'static,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    receive
}

/// [`drain_async`] under a byte ceiling: at most `limit` bytes are retained, the rest is read
/// and thrown away so the producer is never blocked on a full pipe, and a truncation marker is
/// appended to what is kept. See [`MAX_STDERR_BYTES`].
fn drain_async_bounded(
    mut pipe: impl Read + Send + 'static,
    limit: usize,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut truncated = false;
        let result = loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break Ok(()),
                Ok(read) => {
                    let kept = read.min(limit.saturating_sub(bytes.len()));
                    bytes.extend_from_slice(&buffer[..kept]);
                    truncated |= read > kept;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        if truncated {
            bytes.extend_from_slice(
                format!("\n[stderr exceeded {limit} bytes; the rest was discarded]\n").as_bytes(),
            );
        }
        let _ = send.send(result.map(|()| bytes));
    });
    receive
}

fn collect_output(
    receiver: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    stream: &'static str,
    pid: u32,
) -> Result<Vec<u8>, SupervisedError> {
    match receiver.recv_timeout(OUTPUT_DRAIN_GRACE) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(source)) => Err(SupervisedError::OutputRead { stream, source }),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_process_group(pid);
            Err(SupervisedError::OutputHeld(stream))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(SupervisedError::OutputRead {
            stream,
            source: std::io::Error::other("output reader stopped without a result"),
        }),
    }
}

fn collect_stderr(
    receiver: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    pid: u32,
) -> Result<(Vec<u8>, bool), SupervisedError> {
    match receiver.recv_timeout(OUTPUT_DRAIN_GRACE) {
        Ok(Ok(bytes)) => Ok((bytes, false)),
        Ok(Err(source)) => Err(SupervisedError::OutputRead {
            stream: "stderr",
            source,
        }),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_process_group(pid);
            Ok((Vec::new(), true))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(SupervisedError::OutputRead {
            stream: "stderr",
            source: std::io::Error::other("output reader stopped without a result"),
        }),
    }
}

/// [`wait_exact`] that also listens for an [`AbortSignal`]: an abort kills the process group,
/// reaps the leader within the drain grace, and reports `Waited::Abort`.
#[cfg(unix)]
fn wait_exact_or_abort(
    mut child: std::process::Child,
    deadline: Instant,
    send: std::sync::mpsc::Sender<Waited>,
    receive: std::sync::mpsc::Receiver<Waited>,
) -> Result<Waited, SupervisedError> {
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = send.send(Waited::Exited(child.wait()));
    });
    match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Waited::Exited(Ok(status))) => Ok(Waited::Exited(Ok(status))),
        Ok(Waited::Exited(Err(error))) => {
            kill_process_group(pid);
            Err(SupervisedError::Wait(error))
        }
        Ok(Waited::Abort) => {
            kill_process_group(pid);
            // Reap the leader so no zombie outlives the refusal; the waiter thread owns it.
            let _ = receive.recv_timeout(OUTPUT_DRAIN_GRACE);
            Ok(Waited::Abort)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_process_group(pid);
            let _ = receive.recv_timeout(OUTPUT_DRAIN_GRACE);
            Err(SupervisedError::TimedOut {
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            kill_process_group(pid);
            Err(SupervisedError::Wait(std::io::Error::other(
                "process waiter stopped without a result",
            )))
        }
    }
}

#[cfg(not(unix))]
fn wait_exact_or_abort(
    mut child: std::process::Child,
    deadline: Instant,
    _send: std::sync::mpsc::Sender<Waited>,
    receive: std::sync::mpsc::Receiver<Waited>,
) -> Result<Waited, SupervisedError> {
    let mut delay = Duration::from_millis(1);
    loop {
        if let Ok(Waited::Abort) = receive.try_recv() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Waited::Abort);
        }
        match child.try_wait().map_err(SupervisedError::Wait)? {
            Some(status) => return Ok(Waited::Exited(Ok(status))),
            None if Instant::now() < deadline => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(20));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SupervisedError::TimedOut {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                });
            }
        }
    }
}

#[cfg(unix)]
fn wait_exact(
    mut child: std::process::Child,
    deadline: Instant,
) -> Result<ExitStatus, SupervisedError> {
    let pid = child.id();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(child.wait());
    });
    match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => {
            kill_process_group(pid);
            Err(SupervisedError::Wait(error))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_process_group(pid);
            let _ = receive.recv_timeout(OUTPUT_DRAIN_GRACE);
            Err(SupervisedError::TimedOut {
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            kill_process_group(pid);
            Err(SupervisedError::Wait(std::io::Error::other(
                "process waiter stopped without a result",
            )))
        }
    }
}

#[cfg(not(unix))]
fn wait_exact(
    mut child: std::process::Child,
    deadline: Instant,
) -> Result<ExitStatus, SupervisedError> {
    let mut delay = Duration::from_millis(1);
    loop {
        match child.try_wait().map_err(SupervisedError::Wait)? {
            Some(status) => return Ok(status),
            None if Instant::now() < deadline => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(20));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SupervisedError::TimedOut {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                });
            }
        }
    }
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_child_deadline_still_gets_stdin_writer_grace() {
        let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(stdin_writer_wait(expired), STDIN_EXIT_GRACE);
    }

    /// A reader that refuses further output ends the process at once: an endless producer is
    /// gone long before its deadline, and what the reader kept is still in the caller's hands.
    #[test]
    fn an_aborting_reader_ends_the_process_group_before_the_deadline() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "yes"]);
        let mut kept = Vec::new();
        let started = Instant::now();
        let outcome = run_supervised_duplex_with_abort(
            &mut command,
            Duration::from_secs(30),
            ExitPolicy::PreserveProcessGroup,
            None::<fn(&mut dyn Write) -> Result<(), ()>>,
            |stdout: &mut dyn Read, abort: AbortSignal| {
                let mut buffer = [0_u8; 1024];
                while kept.len() < 4096 {
                    let read = stdout.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    kept.extend_from_slice(&buffer[..read]);
                }
                abort.abort();
            },
        );
        let Err(error) = outcome else {
            panic!("an aborted process completed normally");
        };

        assert!(
            matches!(error, SupervisedError::OutputRefused { .. }),
            "{error}"
        );
        assert!(kept.len() >= 4096);
        assert!(kept.starts_with(b"y\ny\n"));
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_descendant_holding_only_stderr_does_not_destroy_complete_stdout() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 30 >&2 & printf complete"]);

        let started = Instant::now();
        let output = run_supervised(&mut command, None, Duration::from_secs(1)).unwrap();

        assert_eq!(output.stdout, b"complete");
        assert!(output.stderr_held);
        assert!(started.elapsed() < Duration::from_secs(8));
    }
}
