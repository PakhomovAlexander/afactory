//! One bounded subprocess boundary shared by reviewers, checks, and sandbox providers.

use std::io::{Read, Write};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

const STDIN_EXIT_GRACE: Duration = Duration::from_millis(500);
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum SupervisedError {
    Cancelled,
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
}

impl std::fmt::Display for SupervisedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "process was cancelled"),
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
    run_supervised_duplex_inner(command, timeout, exit_policy, None, writer, reader)
}

/// Cancelled work terminates the same owned process group as deadline expiration. Existing
/// callers retain the non-polling wait; only cancellable calls observe this flag every 20 ms.
pub fn run_supervised_duplex_cancellable<I, R, F, G>(
    command: &mut std::process::Command,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: &std::sync::atomic::AtomicBool,
    writer: F,
    reader: G,
) -> Result<SupervisedDuplexOutput<I, R>, SupervisedError>
where
    I: Send,
    R: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), I> + Send,
    G: FnOnce(&mut dyn Read) -> R + Send,
{
    run_supervised_duplex_inner(
        command,
        timeout,
        exit_policy,
        Some(cancellation),
        writer,
        reader,
    )
}

fn run_supervised_duplex_inner<I, R, F, G>(
    command: &mut std::process::Command,
    timeout: Duration,
    exit_policy: ExitPolicy,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
    writer: F,
    reader: G,
) -> Result<SupervisedDuplexOutput<I, R>, SupervisedError>
where
    I: Send,
    R: Send,
    F: FnOnce(&mut dyn Write) -> Result<(), I> + Send,
    G: FnOnce(&mut dyn Read) -> R + Send,
{
    if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
        return Err(SupervisedError::Cancelled);
    }
    command.stdin(Stdio::piped());
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
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (input_send, input_receive) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer(&mut stdin)));
            let _ = input_send.send(result);
        });

        let mut stdout = child.stdout.take().expect("stdout was piped");
        let (output_send, output_receive) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader(&mut stdout)));
            let _ = output_send.send(result);
        });

        let stderr = drain_async(child.stderr.take().expect("stderr was piped"));
        let deadline = Instant::now() + timeout;
        let status = match wait_exact_cancellable(child, deadline, cancellation) {
            Ok(status) => status,
            Err(error) => return Err(error),
        };
        if exit_policy == ExitPolicy::KillProcessGroup {
            kill_process_group(pid);
        }

        let input = match input_receive.recv_timeout(stdin_writer_wait(deadline)) {
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

fn wait_exact(
    child: std::process::Child,
    deadline: Instant,
) -> Result<ExitStatus, SupervisedError> {
    wait_exact_cancellable(child, deadline, None)
}

#[cfg(unix)]
fn wait_exact_cancellable(
    mut child: std::process::Child,
    deadline: Instant,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ExitStatus, SupervisedError> {
    let pid = child.id();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(child.wait());
    });
    loop {
        let cancelled =
            cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire));
        if cancelled {
            kill_process_group(pid);
            let _ = receive.recv_timeout(OUTPUT_DRAIN_GRACE);
            return Err(SupervisedError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let delay = if cancellation.is_some() {
            remaining.min(Duration::from_millis(20))
        } else {
            remaining
        };
        match receive.recv_timeout(delay) {
            Ok(Ok(status)) => return Ok(status),
            Ok(Err(error)) => {
                kill_process_group(pid);
                return Err(SupervisedError::Wait(error));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    kill_process_group(pid);
                    let _ = receive.recv_timeout(OUTPUT_DRAIN_GRACE);
                    return Err(SupervisedError::TimedOut {
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    });
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                kill_process_group(pid);
                return Err(SupervisedError::Wait(std::io::Error::other(
                    "process waiter stopped without a result",
                )));
            }
        }
    }
}

#[cfg(not(unix))]
fn wait_exact_cancellable(
    mut child: std::process::Child,
    deadline: Instant,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ExitStatus, SupervisedError> {
    let mut delay = Duration::from_millis(1);
    loop {
        let cancelled =
            cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire));
        match child.try_wait().map_err(SupervisedError::Wait)? {
            Some(status) => return Ok(status),
            None if Instant::now() < deadline && !cancelled => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(20));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if cancelled {
                    SupervisedError::Cancelled
                } else {
                    SupervisedError::TimedOut {
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    }
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
