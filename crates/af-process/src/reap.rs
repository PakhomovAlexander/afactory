//! Every reap of a child outside this crate's supervisor: the standard library's `wait`,
//! `try_wait`, `wait_with_output`, `output` and `status`, and a non-blocking `waitpid`, each
//! performed under the one process-wide lock the supervisor measures its reaps under
//! ([`crate::rusage`]).
//!
//! A supervised child's resource use is the change of the process's children totals across its
//! reap. That change is the one child's only when no other child of the process can be reaped
//! between the two readings, so the workspace's code reaps nothing directly: it reaps through
//! these, and a source test holds every other crate to it. A reap here never holds the lock
//! while it waits — it polls a non-blocking reap under the lock and sleeps between polls, as
//! the supervisor does — so a long-running child never stalls another one's reap.

use std::io::Read as _;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::Duration;

use crate::rusage::{self, Rusage};

const FIRST_POLL: Duration = Duration::from_millis(1);
const LONGEST_POLL: Duration = Duration::from_millis(20);

/// [`Child::wait`]: block until `child` exits and reap it.
pub fn wait(child: &mut Child) -> std::io::Result<ExitStatus> {
    wait_measured(child).0
}

/// [`wait`] with what the child used, measured as the supervisor measures its own reaps.
pub(crate) fn wait_measured(child: &mut Child) -> (std::io::Result<ExitStatus>, Option<Rusage>) {
    let mut delay = FIRST_POLL;
    loop {
        match rusage::measured(|| child.try_wait(), Option::is_some) {
            (Ok(Some(status)), usage) => return (Ok(status), usage),
            (Ok(None), _) => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(LONGEST_POLL);
            }
            (Err(error), _) => return (Err(error), None),
        }
    }
}

/// [`Child::try_wait`]: reap `child` when it has exited, without blocking.
pub fn try_wait(child: &mut Child) -> std::io::Result<Option<ExitStatus>> {
    rusage::measured(|| child.try_wait(), Option::is_some).0
}

/// [`Child::wait_with_output`]: close the child's stdin, read its stdout and stderr to their
/// ends (stderr on a thread of its own, so neither pipe can fill while the other is read),
/// then reap it.
pub fn wait_with_output(mut child: Child) -> std::io::Result<Output> {
    drop(child.stdin.take());
    let stderr = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        })
    });
    let mut stdout = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut stdout)?;
    }
    let stderr = match stderr {
        Some(reader) => reader
            .join()
            .map_err(|_| std::io::Error::other("reading the child's stderr panicked"))??,
        None => Vec::new(),
    };
    let status = wait(&mut child)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// [`Command::output`]: run `command` with stdin closed and its stdout and stderr captured —
/// the streams `Command::output` sets when the caller set none — and reap it.
pub fn output(command: &mut Command) -> std::io::Result<Output> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_with_output(child)
}

/// [`Command::status`]: run `command` with the streams it was given (inherited by default)
/// and reap it.
pub fn status(command: &mut Command) -> std::io::Result<ExitStatus> {
    wait(&mut command.spawn()?)
}

/// `waitpid(pid, flags | WNOHANG)`: one non-blocking look at `pid`, reaping it when it has
/// exited. `WaitStatus::StillAlive` while it runs; a caller that waits polls this.
pub fn wait_pid(
    pid: nix::unistd::Pid,
    flags: nix::sys::wait::WaitPidFlag,
) -> nix::Result<nix::sys::wait::WaitStatus> {
    use nix::sys::wait::{WaitPidFlag, WaitStatus};
    let (result, _) = rusage::measured(
        || {
            nix::sys::wait::waitpid(pid, Some(flags | WaitPidFlag::WNOHANG))
                .map_err(std::io::Error::from)
        },
        |status| matches!(status, WaitStatus::Exited(..) | WaitStatus::Signaled(..)),
    );
    result.map_err(|error| {
        nix::errno::Errno::from_raw(error.raw_os_error().unwrap_or(nix::libc::EIO))
    })
}

/// Run `reap`, a non-blocking reap the caller performs through an API of its own (a
/// pseudo-terminal child's `try_wait`), under the lock: never a blocking one, which would hold
/// the lock while it waits.
pub fn locked<T>(reap: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    rusage::measured(reap, |_| false).0
}

/// How long a polling caller of [`wait_pid`] or [`locked`] sleeps after its `attempt`-th look, from 1 ms up to
/// 20 ms, as the reaps of this crate poll.
pub fn poll_delay(attempt: u32) -> Duration {
    FIRST_POLL
        .saturating_mul(1 << attempt.min(5))
        .min(LONGEST_POLL)
}
