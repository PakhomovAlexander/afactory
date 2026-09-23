//! One bounded subprocess boundary shared by reviewers, checks, and sandbox providers.

use std::io::{Read, Write};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

mod capture;
mod control;
mod drain;
mod spawn;

use capture::run_supervised_inner;
pub use capture::{
    SupervisedCapture, run_supervised_captured, run_supervised_captured_cancellable,
    run_supervised_captured_cancellable_with_policy, run_supervised_captured_with_policy,
};
use drain::{collect_after_kill, collect_stderr, drain_async};
pub use spawn::spawn;

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
    run_supervised_captured_with_policy(command, input, timeout, exit_policy).into_result()
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
    run_supervised_inner(command, Some(writer), timeout, exit_policy).into_result()
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

    let mut child = spawn::spawn(command).map_err(SupervisedError::Spawn)?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    let leader = Leader::new(child);
    std::thread::scope(|scope| {
        let (input_send, input_receive) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer(&mut stdin)));
            let _ = input_send.send(result);
        });

        let (output_send, output_receive) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader(&mut stdout)));
            let _ = output_send.send(result);
        });

        let stderr = drain_async(stderr_pipe);
        let deadline = Instant::now() + timeout;
        let kill_group_on_exit = exit_policy == ExitPolicy::KillProcessGroup;
        let outcome = (|| {
            wait_exact_cancellable(deadline, cancellation, &leader, kill_group_on_exit)?;

            let input = match input_receive.recv_timeout(stdin_writer_wait(deadline)) {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => {
                    return Err(SupervisedError::Stdin(std::io::Error::other(
                        "input writer panicked",
                    )));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    leader.kill_group();
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
                    leader.kill_group();
                    return Err(SupervisedError::OutputHeld("stdout"));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(SupervisedError::OutputRead {
                        stream: "stdout",
                        source: std::io::Error::other("output reader stopped without a result"),
                    });
                }
            };
            let (stderr, stderr_held) = collect_stderr(stderr, &leader)?;
            Ok((input, output, stderr, stderr_held))
        })();
        // Reaping is the final operation: every kill above ran while the pid was reserved.
        let status = leader.reap();
        let (input, output, stderr, stderr_held) = outcome?;
        Ok(SupervisedDuplexOutput {
            status: status.map_err(SupervisedError::Wait)?,
            input,
            output,
            stderr,
            stderr_held,
        })
    })
}

fn stdin_writer_wait(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(STDIN_EXIT_GRACE)
}

/// Wait for the leader to exit, killing its process group on the way out when asked.
///
/// The group is killed *between* the leader's exit and its reaping. A reaped pid is free for
/// reuse, and a process group id is only reserved while the group has a member, so a
/// `killpg` issued after `wait` can land on an unrelated process that was just spawned into
/// its own group under the recycled id — on a loaded machine running thousands of short git
/// commands, that was a `git rev-parse` dying with `SIGKILL` and an empty stderr. `waitid`
/// with `WNOWAIT` observes the exit while the leader is still a zombie, so its pid and group
/// id remain reserved while the group's other members are killed.
/// Wait for the leader to exit, killing its process group on the way out when asked. Returns
/// once the exit has been observed; the leader is reaped by the caller, last. On a deadline or
/// a cancellation the group is killed while the leader is alive and its id reserved.
fn wait_exact_cancellable(
    deadline: Instant,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
    leader: &std::sync::Arc<Leader>,
    kill_group_on_exit: bool,
) -> Result<(), SupervisedError> {
    let (send, receive) = std::sync::mpsc::channel();
    let waiter = std::sync::Arc::clone(leader);
    std::thread::spawn(move || {
        waiter.observe_exit();
        if kill_group_on_exit {
            waiter.kill_group();
        }
        let _ = send.send(());
    });
    loop {
        let cancelled =
            cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire));
        if cancelled {
            leader.kill_group();
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
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    leader.kill_group();
                    let _ = receive.recv_timeout(OUTPUT_DRAIN_GRACE);
                    return Err(SupervisedError::TimedOut {
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    });
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                leader.kill_group();
                return Err(SupervisedError::Wait(std::io::Error::other(
                    "process waiter stopped without a result",
                )));
            }
        }
    }
}

/// A non-blocking view of whether a child has exited, taken without reaping it.
///
/// For a caller that polls a child while draining its pipes and wants to end the child's
/// process group when the leader exits: `try_wait` reaps, and a group kill after the reap can
/// land on a stranger that recycled the pid, so the kill has to come first. Create the watch
/// right after spawning, then poll [`try_reap_killing_group`] instead of `try_wait`.
pub struct ExitWatch {
    pid: u32,
    seen: bool,
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    queue: Option<nix::sys::event::Kqueue>,
}

impl ExitWatch {
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            seen: false,
            #[cfg(any(
                target_os = "macos",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd",
                target_os = "dragonfly"
            ))]
            queue: register_exit(pid),
        }
    }

    /// `Some(true)` once the child has exited (it stays unreaped until the caller reaps it),
    /// `Some(false)` while it runs, `None` when the platform cannot say without reaping.
    pub fn exited(&mut self) -> Option<bool> {
        if self.seen {
            return Some(true);
        }
        let exited = exited_without_reaping(self)?;
        if exited {
            self.seen = true;
        }
        Some(exited)
    }
}

/// `try_wait` that ends the child's process group *before* reaping the child, so the kill can
/// never reach a recycled pid. When the platform cannot observe an exit without reaping, this
/// reaps like `try_wait` and kills nothing: a surviving grandchild is the lesser harm.
pub fn try_reap_killing_group(
    child: &mut std::process::Child,
    watch: &mut ExitWatch,
) -> std::io::Result<Option<ExitStatus>> {
    match watch.exited() {
        Some(true) => {
            kill_process_group(watch.pid);
            child.wait().map(Some)
        }
        Some(false) => Ok(None),
        None => child.try_wait(),
    }
}

#[cfg(all(target_os = "linux", not(target_env = "uclibc")))]
fn exited_without_reaping(watch: &ExitWatch) -> Option<bool> {
    use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};
    let id = nix::unistd::Pid::from_raw(watch.pid as i32);
    loop {
        let flags = WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT;
        match waitid(Id::Pid(id), flags) {
            Ok(WaitStatus::StillAlive) => return Some(false),
            Ok(_) => return Some(true),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return None,
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn register_exit(pid: u32) -> Option<nix::sys::event::Kqueue> {
    use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
    let queue = Kqueue::new().ok()?;
    let exit = KEvent::new(
        pid as usize,
        EventFilter::EVFILT_PROC,
        EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
        FilterFlag::NOTE_EXIT,
        0,
        0,
    );
    let zero = nix::sys::time::TimeSpec::new(0, 0);
    queue.kevent(&[exit], &mut [], Some(*zero.as_ref())).ok()?;
    Some(queue)
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn exited_without_reaping(watch: &ExitWatch) -> Option<bool> {
    use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent};
    let queue = watch.queue.as_ref()?;
    let mut events = [KEvent::new(
        0,
        EventFilter::EVFILT_PROC,
        EvFlags::empty(),
        FilterFlag::empty(),
        0,
        0,
    )];
    let zero = nix::sys::time::TimeSpec::new(0, 0);
    loop {
        match queue.kevent(&[], &mut events, Some(*zero.as_ref())) {
            Ok(count) => return Some(count > 0),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return None,
        }
    }
}

#[cfg(not(any(
    all(target_os = "linux", not(target_env = "uclibc")),
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
fn exited_without_reaping(_watch: &ExitWatch) -> Option<bool> {
    None
}

/// The supervised leader, owned until it is reaped.
///
/// Every process-group kill goes through here, and reaping is the final operation of every
/// supervision path. A reaped pid is free for reuse and a process-group id is reserved only
/// while the group has a member, so a `killpg` issued after the reap could land on a stranger
/// spawned into its own group under the recycled id. While the leader is unreaped — running,
/// or a zombie whose exit was observed without reaping — its pid and group id stay reserved and
/// every kill is exact. Once [`Leader::reap`] has run, [`Leader::kill_group`] is a no-op; the
/// reap holds the lock, so a kill cannot slip in between the check and the signal.
pub(crate) struct Leader {
    pid: u32,
    state: std::sync::Mutex<LeaderState>,
}

enum LeaderState {
    /// Not yet reaped: running, or exited and observed without reaping.
    Unreaped(std::process::Child),
    /// Reaped; the pid may already belong to someone else.
    Reaped(std::io::Result<ExitStatus>),
    /// Taken by [`Leader::reap`].
    Gone,
}

impl Leader {
    pub(crate) fn new(child: std::process::Child) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            pid: child.id(),
            state: std::sync::Mutex::new(LeaderState::Unreaped(child)),
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LeaderState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Kill the leader's process group unless the leader has already been reaped.
    pub(crate) fn kill_group(&self) {
        let state = self.lock();
        if matches!(*state, LeaderState::Unreaped(_)) {
            kill_process_group(self.pid);
        }
    }

    /// Block until the leader has exited. Where the platform can observe an exit without
    /// reaping, the leader stays a zombie and its pid reserved until [`Leader::reap`]; where it
    /// cannot, this polls and reaps under the lock, so no later kill can target the pid.
    fn observe_exit(&self) {
        if wait_without_reaping(self.pid) {
            return;
        }
        let mut delay = Duration::from_millis(1);
        loop {
            {
                let mut state = self.lock();
                let LeaderState::Unreaped(child) = &mut *state else {
                    return;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        *state = LeaderState::Reaped(Ok(status));
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        *state = LeaderState::Reaped(Err(error));
                        return;
                    }
                }
            }
            std::thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_millis(20));
        }
    }

    /// Reap the leader and return its exit status; after this no group kill is ever issued for
    /// its pid. Blocks until the leader has exited, which every caller has already ensured by
    /// observing the exit or killing the group.
    pub(crate) fn reap(&self) -> std::io::Result<ExitStatus> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, LeaderState::Gone) {
            LeaderState::Unreaped(mut child) => child.wait(),
            LeaderState::Reaped(status) => status,
            LeaderState::Gone => Err(std::io::Error::other("process was already reaped")),
        }
    }
}

/// Block until the child has exited without reaping it, so its pid stays reserved. `false`
/// when the platform could not answer; the caller then reaps without touching the group,
/// which can leave a grandchild behind but can never kill a stranger.
#[cfg(all(target_os = "linux", not(target_env = "uclibc")))]
fn wait_without_reaping(pid: u32) -> bool {
    use nix::sys::wait::{Id, WaitPidFlag, waitid};
    let id = nix::unistd::Pid::from_raw(pid as i32);
    loop {
        match waitid(Id::Pid(id), WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT) {
            Ok(_) => return true,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return false,
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn wait_without_reaping(pid: u32) -> bool {
    use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
    let Ok(queue) = Kqueue::new() else {
        return false;
    };
    let exit = KEvent::new(
        pid as usize,
        EventFilter::EVFILT_PROC,
        EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
        FilterFlag::NOTE_EXIT,
        0,
        0,
    );
    let mut events = [exit];
    loop {
        match queue.kevent(&[exit], &mut events, None) {
            Ok(count) => return count > 0,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return false,
        }
    }
}

#[cfg(not(any(
    all(target_os = "linux", not(target_env = "uclibc")),
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
fn wait_without_reaping(_pid: u32) -> bool {
    false
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
