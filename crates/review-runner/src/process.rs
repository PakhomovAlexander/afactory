//! One bounded subprocess boundary shared by reviewers, checks, and sandbox providers.

use std::io::{Read, Write};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

const STDIN_EXIT_GRACE: Duration = Duration::from_millis(500);
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(5);

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
        }
    }
}

impl std::error::Error for SupervisedError {}

pub struct SupervisedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
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
    command.stdin(if input.is_some() {
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
    let stdin_result = input.map(|input| {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = match stdin.write_all(&input) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                Err(error) => Err(error),
            };
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
            return Err(SupervisedError::TimedOut {
                stdout: collect_after_kill(stdout),
                stderr: collect_after_kill(stderr),
            });
        }
        Err(error) => return Err(error),
    };
    if exit_policy == ExitPolicy::KillProcessGroup {
        kill_process_group(pid);
    }

    if let Some(receiver) = stdin_result {
        match receiver.recv_timeout(stdin_writer_wait(deadline)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(SupervisedError::Stdin(error)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                kill_process_group(pid);
                return Err(SupervisedError::TimedOut {
                    stdout: collect_after_kill(stdout),
                    stderr: collect_after_kill(stderr),
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(SupervisedError::Stdin(std::io::Error::other(
                    "input writer stopped without a result",
                )));
            }
        }
    }

    let stdout = collect_output(stdout, "stdout", pid)?;
    let stderr = collect_output(stderr, "stderr", pid)?;
    Ok(SupervisedOutput {
        status,
        stdout,
        stderr,
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
}
