//! One measured child process: the timing and lifecycle core of the performance supervisor.
//!
//! The performance laboratory's authority-built `perf-supervisor` example measures both sides
//! with this module (ADR-0166); nothing here is specific to a benchmark. A sample runs one
//! command with an exact environment, writes its stdin, drains stdout and stderr concurrently
//! into capped buffers that count what they drop, and brackets child launch through the
//! observed exit with a monotonic clock. The exit is observed without reaping (`waitid` with
//! `WNOWAIT` on Linux, a kqueue `NOTE_EXIT` on macOS), so the child's pid and process-group id
//! stay reserved while the lifecycle checks run. Those checks, and every kill and reap, happen
//! after the timed interval and are timed separately.
//!
//! Every wait is bounded (ADR-0167). When the unreaped observation fails, the child is polled
//! without blocking inside the same deadline and cancellation loop, and its interval is then
//! not established; when it cannot be polled either, the deadline still ends the sample and the
//! exit is reported unobserved. A failed observation, reap or process enumeration is never a
//! clean lifecycle: it is `cleanup_uncertain`, with the original error.
//!
//! Two containments differ in what they can establish, and the outcome says which one ran:
//!
//! * [`Containment::PidNamespace`] is for a supervisor that is PID 1 of its own Linux PID
//!   namespace (a sample container); [`run_sample`] refuses it anywhere else before anything is
//!   spawned or signalled. Every process in the namespace other than the supervisor descends
//!   from the child, including ones that started a new session; at the child's exit they are
//!   enumerated, `kill(-1, SIGKILL)` ends all of them and the namespace is scanned again until
//!   only the supervisor remains. Termination is verified, not assumed.
//! * [`Containment::ProcessGroup`] puts the child in its own process group and kills that group
//!   while the child is unreaped. On Linux the group's live members are enumerated before and
//!   after; on macOS they are not observable. A descendant that started a new session leaves
//!   the group: only a pipe it still holds reveals it. This is not namespace containment, and
//!   the outcome never claims that it is. One group kill can miss a process a member was
//!   forking at that moment (macOS signals the parent, not the new child, which stays in the
//!   group with no signal pending and holds the inherited pipes); the group is therefore
//!   signalled again, while its leader's ID still names only it, until its pipes are closed and
//!   no member but the zombie leader runs, or the cleanup grace ends.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(20);
/// How often a child whose exit cannot be observed without reaping is polled instead.
const FALLBACK_POLL: Duration = Duration::from_millis(1);
const READ_CHUNK: usize = 64 * 1024;
/// How often a killed process group is signalled again while it settles.
const SETTLE_POLL: Duration = Duration::from_millis(5);

/// Why `pid_namespace` containment is refused outside a namespace's init.
pub const NOT_NAMESPACE_INIT: &str = "pid_namespace containment requires this supervisor to be \
                                      PID 1 of its own Linux PID namespace";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    ProcessGroup,
    PidNamespace,
}

impl Containment {
    pub fn name(self) -> &'static str {
        match self {
            Self::ProcessGroup => "process_group",
            Self::PidNamespace => "pid_namespace",
        }
    }
}

/// Whether this process is PID 1 of a Linux PID namespace: the one place where `kill(-1, …)`
/// reaches exactly the processes the sample started. Anywhere else it reaches every process of
/// the user, so `pid_namespace` containment is refused there.
pub fn namespace_init() -> bool {
    cfg!(target_os = "linux") && std::process::id() == 1
}

/// Everything one sample executes. Nothing of the caller's environment, signal mask or
/// signal dispositions is inherited; the child starts in the caller's current directory,
/// which a one-sample supervisor sets before it runs the sample.
pub struct SampleSpec {
    pub program: std::path::PathBuf,
    pub args: Vec<std::ffi::OsString>,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub stdout_cap: usize,
    pub stderr_cap: usize,
    pub containment: Containment,
    /// How long cleanup may wait for a forwarded signal, the kill, and the pipes to close.
    pub cleanup_grace: Duration,
    /// Byte patterns counted over the child's entire stdout, past its cap too, so an oracle
    /// can check a count in output larger than any retained log; empty counts nothing.
    pub stdout_patterns: Vec<Vec<u8>>,
    /// Selectors of declared fields in the child's stdout as one JSON document, observed over
    /// the entire stdout, past its cap too (`crate::json_stream`, ADR-0172): an oracle can
    /// judge the selected semantics of output larger than any retained log without freezing its
    /// formatting. Empty observes nothing.
    pub stdout_json: Vec<crate::json_stream::Selector>,
}

/// Cancellation from outside the sample: the first signal number admitted wins.
///
/// Admission and a launch are ordered at one gate (ADR-0173). A launch passes the gate only
/// while no cancellation is admitted, and holds it until its spawn has returned, so a
/// cancellation admitted before the gate means nothing is spawned. A cancellation that arrives
/// while a launch holds the gate is recorded then and admitted when the gate opens, once the
/// child exists; like any later one, it cancels that child through its containment. So
/// [`Control::cancelled`] never reports a cancellation that a launch in progress did not see.
#[derive(Default)]
pub struct Control {
    /// The first signal recorded (0 while none is) in the low 32 bits, and `LAUNCHING` while
    /// a launch holds the gate.
    state: AtomicU64,
}

const SIGNAL_BITS: u64 = 0xffff_ffff;
const LAUNCHING: u64 = 1 << 32;

impl Control {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record `signal` unless one already was. It is admitted at once, or, while a launch
    /// holds the gate, when that launch has spawned. It never waits.
    pub fn cancel(&self, signal: i32) {
        let mut state = self.state.load(Ordering::Acquire);
        while state & SIGNAL_BITS == 0 {
            match self.state.compare_exchange_weak(
                state,
                state | u64::from(signal as u32),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => state = current,
            }
        }
    }

    /// The admitted cancellation's signal.
    pub fn cancelled(&self) -> Option<i32> {
        let state = self.state.load(Ordering::Acquire);
        if state & LAUNCHING != 0 {
            return None;
        }
        match (state & SIGNAL_BITS) as u32 as i32 {
            0 => None,
            signal => Some(signal),
        }
    }

    /// Run `spawn` inside the launch gate, or, when a cancellation was admitted before the
    /// gate, run nothing and return its signal. One launch holds the gate at a time.
    fn launch<T>(&self, spawn: impl FnOnce() -> T) -> Result<T, i32> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & LAUNCHING != 0 {
                std::thread::yield_now();
                state = self.state.load(Ordering::Acquire);
                continue;
            }
            match (state & SIGNAL_BITS) as u32 as i32 {
                0 => {}
                signal => return Err(signal),
            }
            match self.state.compare_exchange_weak(
                state,
                state | LAUNCHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => state = current,
            }
        }
        // The gate opens however the spawn ends, and with it any cancellation recorded
        // meanwhile is admitted.
        struct Open<'a>(&'a AtomicU64);
        impl Drop for Open<'_> {
            fn drop(&mut self) {
                self.0.fetch_and(!LAUNCHING, Ordering::AcqRel);
            }
        }
        let _open = Open(&self.state);
        Ok(spawn())
    }
}

/// Block SIGTERM, SIGINT and SIGHUP in the calling thread and forward each one received to
/// `control`. Call it before any other thread exists: threads inherit the blocked mask. A
/// child inherits a blocked mask through the standard library's spawn, so `run_sample` spawns
/// with an explicit empty mask and default dispositions instead.
#[cfg(unix)]
pub fn forward_termination_signals(control: Arc<Control>) -> std::io::Result<()> {
    use nix::sys::signal::{SigSet, Signal};
    let mut set = SigSet::empty();
    for signal in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        set.add(signal);
    }
    set.thread_block().map_err(std::io::Error::from)?;
    std::thread::spawn(move || {
        while let Ok(signal) = set.wait() {
            control.cancel(signal as i32);
        }
    });
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    Exited {
        code: i32,
    },
    Signalled {
        signal: i32,
    },
    TimedOut,
    Cancelled {
        signal: i32,
    },
    LaunchFailed {
        detail: String,
    },
    /// The child's exit was observed, but reaping it failed, so its status is not known.
    StatusUnknown {
        detail: String,
    },
}

/// How the end of the timed interval was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitObservation {
    /// Seen without reaping: the pid and process group stayed reserved for the lifecycle.
    Unreaped,
    /// The unreaped observation failed and the child was reaped by polling: its interval is
    /// not established, and its process group cannot be killed after that reap.
    ReapedByPolling,
    /// No exit was seen before the deadline (or the cancellation) and the cleanup grace ended;
    /// the child was signalled but is never waited for.
    Unobserved,
    /// Nothing was launched.
    NothingLaunched,
}

impl ExitObservation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unreaped => "unreaped",
            Self::ReapedByPolling => "reaped_by_polling",
            Self::Unobserved => "unobserved",
            Self::NothingLaunched => "none",
        }
    }
}

/// One drained pipe: the retained prefix and every byte read, so truncation is exact.
#[derive(Debug, Default, Clone)]
pub struct Stream {
    pub retained: Vec<u8>,
    pub total: u64,
    /// For each of the spec's patterns, its occurrences in every byte read, in order.
    pub pattern_counts: Vec<u64>,
    /// The spec's JSON selectors' observation of every byte read, when it declared any; until
    /// the pipe's end was read, its document is never complete.
    pub json: Option<crate::json_stream::Observation>,
}

impl Stream {
    pub fn truncated(&self) -> u64 {
        self.total - self.retained.len() as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// Every process of the PID namespace was enumerated.
    PidNamespaceScan,
    /// The live members of the child's process group were enumerated.
    ProcessGroupScan,
    /// Nothing could be enumerated; only the pipes reveal a descendant.
    PipesOnly,
}

impl Observation {
    pub fn name(self) -> &'static str {
        match self {
            Self::PidNamespaceScan => "pid_namespace_scan",
            Self::ProcessGroupScan => "process_group_scan",
            Self::PipesOnly => "pipes_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStatus {
    /// No descendant outlived the child and the cleanup was verified.
    Clean,
    /// None was observed, but the containment cannot enumerate every descendant.
    NoObservedDescendants,
    /// Descendants outlived the child; they were killed and their termination verified.
    UnexpectedDescendants,
    /// Termination of everything the sample started could not be verified.
    CleanupUncertain,
}

impl LifecycleStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::NoObservedDescendants => "no_observed_descendants",
            Self::UnexpectedDescendants => "unexpected_descendants",
            Self::CleanupUncertain => "cleanup_uncertain",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Lifecycle {
    pub observation: Observation,
    /// Live descendants when the child's exit was observed; `None` when they were not counted:
    /// not observable, the enumeration failed, or the child did not exit by itself.
    pub descendants_at_exit: Option<u32>,
    /// Processes of the sample still present after cleanup; `None` when not observable or the
    /// enumeration failed.
    pub remaining_after_cleanup: Option<u32>,
    pub pipes_held: bool,
    pub status: LifecycleStatus,
    /// For an uncertain cleanup every reason, the first one first, each with its original
    /// error.
    pub detail: String,
}

/// OS-accounted usage of the reaped child and the descendants it waited for, from
/// `getrusage(RUSAGE_CHILDREN)` before the launch and after the reap. It is exact when the
/// calling process reaps no other child in between, as the one-sample supervisor guarantees.
/// `maxrss_bytes` is the largest resident set of any child the process has reaped, converted
/// from kilobytes on Linux; it is not a sum and not a cgroup figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rusage {
    pub user_cpu_ns: u128,
    pub system_cpu_ns: u128,
    pub maxrss_bytes: u64,
    pub minor_faults: u64,
    pub major_faults: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
}

pub struct SampleOutcome {
    pub termination: Termination,
    /// Monotonic nanoseconds from immediately before the launch to the exit observed without
    /// reaping; absent for anything else: a launch failure, a timeout, a cancellation, an
    /// unknown status, or an exit that was reaped by polling or never observed.
    pub child_wall_ns: Option<u128>,
    pub exit_observation: ExitObservation,
    pub launched: Instant,
    /// When the exit (or the kill) was observed; the end of the timed interval. For an
    /// unobserved exit, when the observation was given up.
    pub ended: Instant,
    /// Nanoseconds spent after `ended` on lifecycle checks, kills, reaps and pipe closure.
    pub cleanup_ns: u128,
    pub stdout: Stream,
    pub stderr: Stream,
    pub lifecycle: Lifecycle,
    pub rusage: Result<Rusage, String>,
}

/// The operating-system calls a sample's observation rests on. Production uses [`NATIVE`]; the
/// tests replace one with a deterministic failure, to prove that no failure is taken for an
/// exit or a verified cleanup and that none of them unbounds a wait.
#[derive(Clone, Copy)]
struct Os {
    /// Block until the child has exited, without reaping it.
    wait_unreaped: fn(u32) -> Result<(), String>,
    /// Reap the child if it has exited; `Ok(None)` while it runs.
    try_reap: fn(u32) -> Result<Option<Termination>, String>,
    /// The processes the containment can enumerate; `Ok(None)` when it can enumerate none.
    enumerate: fn(Containment, u32) -> Listing,
    /// Spawn the child, inside the launch gate.
    spawn: fn(&SampleSpec) -> std::io::Result<Spawned>,
}

const NATIVE: Os = Os {
    wait_unreaped,
    try_reap,
    enumerate: processes,
    spawn: spawn_clean,
};

/// Run one sample. Every outcome, including a launch failure and a refused containment, is
/// returned, never an error, and no wait in it outlasts the deadline and two cleanup graces.
pub fn run_sample(spec: &SampleSpec, control: &Control) -> SampleOutcome {
    run_sample_on(spec, control, NATIVE)
}

fn run_sample_on(spec: &SampleSpec, control: &Control, os: Os) -> SampleOutcome {
    // The containment's precondition is the shared leaf's, not each caller's: outside a
    // namespace's init, `kill(-1)` would reach every process of the user.
    if spec.containment == Containment::PidNamespace && !namespace_init() {
        return SampleOutcome::refused(spec.containment, NOT_NAMESPACE_INIT.into());
    }
    let before = children_usage();
    let launched = Instant::now();
    // A cancellation admitted before the launch gate launches nothing (ADR-0171, ADR-0173): a
    // setup the host cancelled is followed by no measured child, and the decision and the
    // spawn are one step, so no cancellation is admitted between them.
    let spawned = match control.launch(|| (os.spawn)(spec)) {
        Err(signal) => {
            return SampleOutcome::refused(
                spec.containment,
                format!("cancelled by signal {signal} before the launch"),
            );
        }
        Ok(Ok(spawned)) => spawned,
        Ok(Err(error)) => return launch_failed(spec, launched, error),
    };
    let pid = spawned.pid;
    let mut watch = Watch::start(pid, os);
    let mut stdout = drain(
        spawned.stdout,
        spec.stdout_cap,
        spec.stdout_patterns.clone(),
        spec.stdout_json.clone(),
    );
    let mut stderr = drain(spawned.stderr, spec.stderr_cap, Vec::new(), Vec::new());
    let input = spec.stdin.clone();
    let stdin = spawned.stdin;
    std::thread::spawn(move || write_input(stdin, input));

    let deadline = launched + spec.timeout;
    let mut termination = None;
    let mut notes = Vec::new();
    let observed = loop {
        let wait = deadline.saturating_duration_since(Instant::now()).min(POLL);
        if let Some(exit) = watch.wait(wait) {
            break Some(exit);
        }
        if let Some(signal) = control.cancelled() {
            termination = Some(Termination::Cancelled { signal });
            note(&mut notes, signal_all(spec.containment, pid, signal));
            if let Some(exit) = watch.wait_until(Instant::now() + spec.cleanup_grace) {
                break Some(exit);
            }
            note(&mut notes, signal_all(spec.containment, pid, SIGKILL));
            break watch.wait_until(Instant::now() + spec.cleanup_grace);
        }
        if Instant::now() >= deadline {
            termination = Some(Termination::TimedOut);
            note(&mut notes, signal_all(spec.containment, pid, SIGKILL));
            break watch.wait_until(Instant::now() + spec.cleanup_grace);
        }
    };
    let ended = observed.as_ref().map_or_else(Instant::now, |exit| exit.at);
    let exit_observation = observed
        .as_ref()
        .map_or(ExitObservation::Unobserved, |exit| exit.observation);
    let normal_exit = termination.is_none();

    // The timed interval is over; everything below is lifecycle work.
    let descendants_at_exit = if normal_exit && exit_observation == ExitObservation::Unreaped {
        live_descendants(os, spec.containment, pid)
    } else {
        Ok(None)
    };
    // A namespace's init may signal it at any time; a process group only while its leader is
    // unreaped, because a reaped leader's id may come to name another group.
    if spec.containment == Containment::PidNamespace
        || exit_observation != ExitObservation::ReapedByPolling
    {
        note(&mut notes, signal_all(spec.containment, pid, SIGKILL));
    }
    // One cleanup grace bounds settling the group, verifying it gone and draining the pipes.
    let cleanup_deadline = Instant::now() + spec.cleanup_grace;
    if spec.containment == Containment::ProcessGroup
        && exit_observation != ExitObservation::ReapedByPolling
    {
        settle_group(
            os,
            pid,
            [&mut stdout, &mut stderr],
            cleanup_deadline,
            &mut notes,
        );
    }
    let reaped = match &observed {
        Some(Exit {
            reaped: Some(status),
            ..
        }) => Ok(Some(status.clone())),
        Some(_) => reap(pid)
            .map(Some)
            .map_err(|error| format!("reaping the child failed: {error}")),
        // Never seen to exit, so never waited for: nothing below can block on it.
        None => Ok(None),
    };
    let after = children_usage();
    let rusage = match &reaped {
        Ok(Some(_)) => usage_delta(before, after),
        Ok(None) => Err("the child was never reaped, so its usage is unknown".into()),
        Err(error) => Err(error.clone()),
    };
    if let Err(error) = &reaped {
        notes.push(error.clone());
    }
    let remaining_after_cleanup = verify_gone(os, spec.containment, pid, cleanup_deadline);
    let stdout = stdout.finish(cleanup_deadline);
    let stderr = stderr.finish(cleanup_deadline);
    let pipes_held = stdout.1 || stderr.1;
    let cleanup_ns = ended.elapsed().as_nanos();

    let termination = termination.unwrap_or_else(|| match &reaped {
        Ok(Some(status)) => status.clone(),
        Ok(None) => Termination::StatusUnknown {
            detail: "the child's exit was not observed".into(),
        },
        Err(error) => Termination::StatusUnknown {
            detail: error.clone(),
        },
    });
    let timed = exit_observation == ExitObservation::Unreaped
        && matches!(
            termination,
            Termination::Exited { .. } | Termination::Signalled { .. }
        );
    let lifecycle = lifecycle(
        spec.containment,
        Found {
            exit: exit_observation,
            observer_failure: watch.failure.take(),
            poll_failure: watch.lost.take(),
            at_exit: descendants_at_exit,
            remaining: remaining_after_cleanup,
            pipes_held,
            notes,
        },
    );
    SampleOutcome {
        child_wall_ns: timed.then(|| ended.duration_since(launched).as_nanos()),
        termination,
        exit_observation,
        launched,
        ended,
        cleanup_ns,
        stdout: stdout.0,
        stderr: stderr.0,
        lifecycle,
        rusage,
    }
}

/// Keep a cleanup step's failure as a reason the cleanup is uncertain, once.
fn note(notes: &mut Vec<String>, result: Result<(), String>) {
    if let Err(error) = result
        && !notes.contains(&error)
    {
        notes.push(error);
    }
}

struct Spawned {
    pid: u32,
    stdin: std::fs::File,
    stdout: std::fs::File,
    stderr: std::fs::File,
}

/// `posix_spawn` the child in its own process group with an empty signal mask and default
/// dispositions for every signal the supervisor handles, its three pipes and nothing else.
fn spawn_clean(spec: &SampleSpec) -> std::io::Result<Spawned> {
    use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn};
    use nix::sys::signal::{SigSet, Signal};
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let text = |bytes: &[u8]| {
        CString::new(bytes).map_err(|_| std::io::Error::other("an argument holds a NUL byte"))
    };
    let program = text(spec.program.as_os_str().as_bytes())?;
    let mut argv = vec![program.clone()];
    for arg in &spec.args {
        argv.push(text(arg.as_bytes())?);
    }
    let mut envp = Vec::new();
    for (key, value) in &spec.env {
        let mut entry = key.as_bytes().to_vec();
        entry.push(b'=');
        entry.extend_from_slice(value.as_bytes());
        envp.push(text(&entry)?);
    }
    let mut attr = PosixSpawnAttr::init()?;
    attr.set_flags(
        PosixSpawnFlags::POSIX_SPAWN_SETPGROUP
            | PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK
            | PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF,
    )?;
    attr.set_pgroup(nix::unistd::Pid::from_raw(0))?;
    attr.set_sigmask(&SigSet::empty())?;
    let mut defaults = SigSet::empty();
    for signal in [
        Signal::SIGTERM,
        Signal::SIGINT,
        Signal::SIGHUP,
        Signal::SIGPIPE,
    ] {
        defaults.add(signal);
    }
    attr.set_sigdefault(&defaults)?;

    let _creation = crate::spawn::creation();
    let (stdin_read, stdin_write) = cloexec_pipe()?;
    let (stdout_read, stdout_write) = cloexec_pipe()?;
    let (stderr_read, stderr_write) = cloexec_pipe()?;
    let mut actions = PosixSpawnFileActions::init()?;
    // dup2 clears close-on-exec on the target; every other pipe end closes at exec.
    actions.add_dup2(stdin_read.as_raw_fd(), 0)?;
    actions.add_dup2(stdout_write.as_raw_fd(), 1)?;
    actions.add_dup2(stderr_write.as_raw_fd(), 2)?;
    let mut busy = 0;
    let pid = loop {
        match posix_spawn(program.as_c_str(), &actions, &attr, &argv, &envp) {
            Err(nix::errno::Errno::ETXTBSY) if busy < 100 => {
                busy += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            result => break result?,
        }
    };
    Ok(Spawned {
        pid: pid.as_raw() as u32,
        stdin: stdin_write.into(),
        stdout: stdout_read.into(),
        stderr: stderr_read.into(),
    })
}

fn cloexec_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    let (read, write) = nix::unistd::pipe()?;
    for fd in [&read, &write] {
        fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    }
    Ok((read, write))
}

/// Reap a child whose exit was already observed, and translate its status.
fn reap(pid: u32) -> std::io::Result<Termination> {
    use nix::sys::wait::{WaitStatus, waitpid};
    let pid = nix::unistd::Pid::from_raw(pid as i32);
    loop {
        match waitpid(pid, None) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(Termination::Exited { code }),
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                return Ok(Termination::Signalled {
                    signal: signal as i32,
                });
            }
            Ok(_) => continue,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn launch_failed(spec: &SampleSpec, launched: Instant, error: std::io::Error) -> SampleOutcome {
    let mut outcome = SampleOutcome::refused(
        spec.containment,
        format!("starting {}: {error}", spec.program.display()),
    );
    outcome.launched = launched;
    outcome
}

impl SampleOutcome {
    /// The outcome of a sample that launched nothing, for `detail`.
    pub fn refused(containment: Containment, detail: String) -> Self {
        let now = Instant::now();
        Self {
            termination: Termination::LaunchFailed { detail },
            child_wall_ns: None,
            exit_observation: ExitObservation::NothingLaunched,
            launched: now,
            ended: now,
            cleanup_ns: 0,
            stdout: Stream::default(),
            stderr: Stream::default(),
            lifecycle: Lifecycle {
                observation: observation(containment),
                descendants_at_exit: Some(0),
                remaining_after_cleanup: Some(0),
                pipes_held: false,
                status: LifecycleStatus::Clean,
                detail: "nothing was launched".into(),
            },
            rusage: Err("nothing was launched".into()),
        }
    }
}

/// What the lifecycle checks found, for [`lifecycle`] to judge.
struct Found {
    exit: ExitObservation,
    observer_failure: Option<String>,
    poll_failure: Option<String>,
    at_exit: Result<Option<u32>, String>,
    remaining: Result<Option<u32>, String>,
    pipes_held: bool,
    notes: Vec<String>,
}

fn lifecycle(containment: Containment, found: Found) -> Lifecycle {
    let observation = observation(containment);
    let observer = found
        .observer_failure
        .unwrap_or_else(|| "the observer gave no result".into());
    let mut uncertain = Vec::new();
    match found.exit {
        ExitObservation::Unreaped | ExitObservation::NothingLaunched => {}
        ExitObservation::ReapedByPolling => uncertain.push(format!(
            "the child's exit could not be observed without reaping it ({observer}); it was \
             reaped by polling, so its interval is not established{}",
            if containment == Containment::ProcessGroup {
                " and its process group could not be killed after that reap"
            } else {
                ""
            }
        )),
        ExitObservation::Unobserved => uncertain.push(format!(
            "the child's exit was not observed ({observer}; polling: {}); it was signalled but \
             never reaped",
            found
                .poll_failure
                .as_deref()
                .unwrap_or("no exit before the cleanup grace ended")
        )),
    }
    if let Err(error) = &found.at_exit {
        uncertain.push(format!(
            "enumerating the sample's processes at the child's exit failed: {error}"
        ));
    }
    match &found.remaining {
        Err(error) => uncertain.push(format!(
            "enumerating the sample's processes after cleanup failed: {error}"
        )),
        Ok(Some(count)) if *count > 0 => uncertain.push(format!(
            "{count} process(es) of the sample were still present after cleanup"
        )),
        Ok(_) => {}
    }
    if found.pipes_held {
        uncertain.push(
            "a process outside the killed scope still held the child's stdout or stderr".into(),
        );
    }
    uncertain.extend(found.notes);
    let at_exit = found.at_exit.unwrap_or(None);
    let (status, detail) = if !uncertain.is_empty() {
        (LifecycleStatus::CleanupUncertain, uncertain.join("; "))
    } else if at_exit.is_some_and(|count| count > 0) {
        (
            LifecycleStatus::UnexpectedDescendants,
            format!(
                "{} live descendant(s) outlived the child; they were killed and their \
                 termination verified",
                at_exit.unwrap_or(0)
            ),
        )
    } else if observation == Observation::PipesOnly {
        (
            LifecycleStatus::NoObservedDescendants,
            "the process group was killed and the pipes closed; descendants that left the group \
             are not observable on this platform"
                .to_string(),
        )
    } else {
        (
            LifecycleStatus::Clean,
            format!("verified by {}", observation.name()),
        )
    };
    Lifecycle {
        observation,
        descendants_at_exit: at_exit,
        remaining_after_cleanup: found.remaining.unwrap_or(None),
        pipes_held: found.pipes_held,
        status,
        detail,
    }
}

fn observation(containment: Containment) -> Observation {
    match containment {
        Containment::PidNamespace => Observation::PidNamespaceScan,
        Containment::ProcessGroup if cfg!(target_os = "linux") => Observation::ProcessGroupScan,
        Containment::ProcessGroup => Observation::PipesOnly,
    }
}

fn write_input(mut stdin: std::fs::File, input: Vec<u8>) {
    // A child that stops reading is judged by its exit and output, not by this write.
    let _ = stdin.write_all(&input);
}

struct Drain {
    stream: Arc<Mutex<Stream>>,
    done: mpsc::Receiver<()>,
    /// The pipe was read to its end.
    ended: bool,
}

impl Drain {
    /// Whether the pipe has been read to its end, without waiting.
    fn ended(&mut self) -> bool {
        if !self.ended {
            self.ended = self.done.try_recv().is_ok();
        }
        self.ended
    }

    /// The stream and whether its pipe was still held open at `deadline`.
    fn finish(self, deadline: Instant) -> (Stream, bool) {
        let held = !self.ended
            && self
                .done
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err();
        let stream = self
            .stream
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        (stream, held)
    }
}

/// Read a pipe to its end, keeping at most `cap` bytes and counting and observing every byte.
fn drain(
    mut pipe: impl Read + Send + 'static,
    cap: usize,
    patterns: Vec<Vec<u8>>,
    selectors: Vec<crate::json_stream::Selector>,
) -> Drain {
    let mut observer =
        (!selectors.is_empty()).then(|| crate::json_stream::Observer::new(selectors));
    let stream = Arc::new(Mutex::new(Stream {
        pattern_counts: vec![0; patterns.len()],
        json: observer
            .as_ref()
            .map(crate::json_stream::Observer::observation),
        ..Stream::default()
    }));
    let shared = Arc::clone(&stream);
    let (send, done) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = vec![0; READ_CHUNK];
        let mut counter = PatternCounter::new(patterns);
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    let counts = counter.feed(&chunk[..count]);
                    if let Some(observer) = observer.as_mut() {
                        observer.feed(&chunk[..count]);
                    }
                    let mut stream = shared.lock().unwrap_or_else(|error| error.into_inner());
                    let room = cap.saturating_sub(stream.retained.len()).min(count);
                    stream.retained.extend_from_slice(&chunk[..room]);
                    stream.total += count as u64;
                    for (total, found) in stream.pattern_counts.iter_mut().zip(counts) {
                        *total += found;
                    }
                    stream.json = observer
                        .as_ref()
                        .map(crate::json_stream::Observer::observation);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // The document, and a number it ended in, is complete only now.
        let mut stream = shared.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(observer) = observer.as_mut() {
            observer.finish();
            stream.json = Some(observer.observation());
        }
        drop(stream);
        let _ = send.send(());
    });
    Drain {
        stream,
        done,
        ended: false,
    }
}

/// Counts every occurrence of each pattern in a byte stream read in arbitrary chunks: the last
/// `longest - 1` bytes carry over, and a match is counted in the chunk where it ends.
struct PatternCounter {
    patterns: Vec<Vec<u8>>,
    carry: Vec<u8>,
    keep: usize,
}

impl PatternCounter {
    fn new(patterns: Vec<Vec<u8>>) -> Self {
        let keep = patterns
            .iter()
            .map(|pattern| pattern.len().saturating_sub(1))
            .max()
            .unwrap_or(0);
        Self {
            patterns,
            carry: Vec::new(),
            keep,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Vec<u64> {
        if self.patterns.is_empty() {
            return Vec::new();
        }
        let carried = self.carry.len();
        let mut window = std::mem::take(&mut self.carry);
        window.extend_from_slice(chunk);
        let counts = self
            .patterns
            .iter()
            .map(|pattern| {
                if pattern.is_empty() || window.len() < pattern.len() {
                    return 0;
                }
                window
                    .windows(pattern.len())
                    .enumerate()
                    .filter(|(start, bytes)| {
                        start + pattern.len() > carried && *bytes == pattern.as_slice()
                    })
                    .count() as u64
            })
            .collect();
        let from = window.len().saturating_sub(self.keep);
        self.carry = window[from..].to_vec();
        counts
    }
}

struct Exit {
    at: Instant,
    observation: ExitObservation,
    reaped: Option<Termination>,
}

/// The child's exit as far as it can be seen, every wait bounded by its caller.
///
/// A thread blocks in the unreaped observation and stamps the instant it returns. When that
/// observation fails, the child is polled without blocking from then on; when polling fails
/// too, nothing can see the exit, and only the caller's deadline or cancellation ends a wait.
struct Watch {
    pid: u32,
    os: Os,
    unreaped: Option<mpsc::Receiver<Result<Instant, String>>>,
    /// Why the unreaped observation failed, verbatim.
    failure: Option<String>,
    /// Why polling failed too, verbatim.
    lost: Option<String>,
}

impl Watch {
    fn start(pid: u32, os: Os) -> Self {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send((os.wait_unreaped)(pid).map(|()| Instant::now()));
        });
        Self {
            pid,
            os,
            unreaped: Some(receive),
            failure: None,
            lost: None,
        }
    }

    /// The exit, if it is seen within `timeout`.
    fn wait(&mut self, timeout: Duration) -> Option<Exit> {
        let until = Instant::now() + timeout;
        if let Some(receive) = &self.unreaped {
            match receive.recv_timeout(timeout) {
                Ok(Ok(at)) => {
                    return Some(Exit {
                        at,
                        observation: ExitObservation::Unreaped,
                        reaped: None,
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return None,
                Ok(Err(reason)) => self.failure = Some(reason),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.failure = Some("the exit observer ended without a result".into());
                }
            }
            self.unreaped = None;
        }
        loop {
            if self.lost.is_none() {
                match (self.os.try_reap)(self.pid) {
                    Ok(Some(status)) => {
                        return Some(Exit {
                            at: Instant::now(),
                            observation: ExitObservation::ReapedByPolling,
                            reaped: Some(status),
                        });
                    }
                    Ok(None) => {}
                    Err(reason) => self.lost = Some(reason),
                }
            }
            let now = Instant::now();
            if now >= until {
                return None;
            }
            std::thread::sleep((until - now).min(FALLBACK_POLL));
        }
    }

    /// The exit, if it is seen before `deadline`.
    fn wait_until(&mut self, deadline: Instant) -> Option<Exit> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Some(exit) = self.wait(left.min(POLL)) {
                return Some(exit);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }
}

/// Reap the child if it has exited, without blocking.
fn try_reap(pid: u32) -> Result<Option<Termination>, String> {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    let id = nix::unistd::Pid::from_raw(pid as i32);
    match waitpid(id, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(_, code)) => Ok(Some(Termination::Exited { code })),
        Ok(WaitStatus::Signaled(_, signal, _)) => Ok(Some(Termination::Signalled {
            signal: signal as i32,
        })),
        Ok(_) | Err(nix::errno::Errno::EINTR) => Ok(None),
        Err(error) => Err(format!("waitpid(WNOHANG): {error}")),
    }
}

#[cfg(all(target_os = "linux", not(target_env = "uclibc")))]
fn wait_unreaped(pid: u32) -> Result<(), String> {
    use nix::sys::wait::{Id, WaitPidFlag, waitid};
    let id = nix::unistd::Pid::from_raw(pid as i32);
    loop {
        match waitid(Id::Pid(id), WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT) {
            Ok(_) => return Ok(()),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(format!("waitid(WNOWAIT): {error}")),
        }
    }
}

#[cfg(target_os = "macos")]
fn wait_unreaped(pid: u32) -> Result<(), String> {
    use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
    let queue = Kqueue::new().map_err(|error| format!("kqueue: {error}"))?;
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
            Ok(count) if count > 0 && events[0].flags().contains(EvFlags::EV_ERROR) => {
                // The child is ours and unreaped, so it exists: an attach error of ESRCH means
                // it had already exited and is a zombie, which is the exit observed now.
                let errno = nix::errno::Errno::from_raw(events[0].data() as i32);
                return match errno {
                    nix::errno::Errno::ESRCH => Ok(()),
                    errno => Err(format!("kevent(NOTE_EXIT): {errno}")),
                };
            }
            Ok(0) => return Err("kevent(NOTE_EXIT) returned no event".into()),
            Ok(_) => return Ok(()),
            Err(nix::errno::Errno::EINTR) => continue,
            // Same reasoning for an attach refused by the call itself.
            Err(nix::errno::Errno::ESRCH) => return Ok(()),
            Err(error) => return Err(format!("kevent(NOTE_EXIT): {error}")),
        }
    }
}

#[cfg(not(any(
    all(target_os = "linux", not(target_env = "uclibc")),
    target_os = "macos"
)))]
fn wait_unreaped(_pid: u32) -> Result<(), String> {
    Err("this platform cannot observe an exit without reaping".into())
}

const SIGKILL: i32 = 9;

/// Send `signal` (0 only checks) to everything the sample may have started: the whole PID
/// namespace from its PID 1, or the child's process group while the child is unreaped. A
/// namespace broadcast from anything but a namespace's init is refused before any signal is
/// sent. `ESRCH`, nothing left to signal, is success. So is macOS's `EPERM` for a process
/// group: XNU skips zombies and answers `EPERM` when it found no member to signal, which is
/// the group of an exited leader; a macOS group is never enumerated, so its lifecycle claims at
/// most `no_observed_descendants` either way.
fn signal_all(containment: Containment, pid: u32, signal: i32) -> Result<(), String> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;
    if containment == Containment::PidNamespace && !namespace_init() {
        return Err(format!(
            "refused to signal every process: {NOT_NAMESPACE_INIT}"
        ));
    }
    let signal = match signal {
        0 => None,
        number => {
            Some(Signal::try_from(number).map_err(|error| format!("signal {number}: {error}"))?)
        }
    };
    let (sent, scope) = match containment {
        Containment::PidNamespace => (kill(Pid::from_raw(-1), signal), "namespace"),
        Containment::ProcessGroup => (killpg(Pid::from_raw(pid as i32), signal), "process group"),
    };
    match sent {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(Errno::EPERM)
            if cfg!(target_vendor = "apple") && containment == Containment::ProcessGroup =>
        {
            Ok(())
        }
        Err(error) => Err(format!("signalling the sample's {scope}: {error}")),
    }
}

/// Signal the child's killed process group again until both pipes are read to their end and no
/// member but the zombie leader runs, or until `deadline`. The leader is unreaped, so the
/// group's ID names only the sample's processes. A group kill can miss a process a member was
/// forking as it arrived: the new child stays in the group with no signal pending and holds
/// the inherited pipes, and only another kill ends it. Nothing is judged here; a pipe still
/// held or a member still live at the deadline remains uncertain afterwards.
fn settle_group(
    os: Os,
    pid: u32,
    drains: [&mut Drain; 2],
    deadline: Instant,
    notes: &mut Vec<String>,
) {
    let [stdout, stderr] = drains;
    loop {
        let ended = stdout.ended() && stderr.ended();
        if (ended && !group_member_alive(os, pid)) || Instant::now() >= deadline {
            return;
        }
        note(notes, signal_all(Containment::ProcessGroup, pid, SIGKILL));
        std::thread::sleep(SETTLE_POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Whether a member of the child's process group other than its unreaped leader may still
/// run. Linux enumerates the group's live members. macOS refuses a signal to a group whose
/// members are all zombies (`EPERM`) or gone (`ESRCH`); any other answer means a member still
/// accepts one. Where neither is available, only the pipes tell, and this is false.
fn group_member_alive(os: Os, pid: u32) -> bool {
    if cfg!(target_vendor = "apple") {
        use nix::errno::Errno;
        use nix::sys::signal::killpg;
        use nix::unistd::Pid;
        return !matches!(
            killpg(Pid::from_raw(pid as i32), None),
            Err(Errno::EPERM | Errno::ESRCH)
        );
    }
    matches!(
        live_descendants(os, Containment::ProcessGroup, pid),
        Ok(Some(count)) if count > 0
    )
}

/// Live processes other than the child (and, in a namespace, its PID 1); `Ok(None)` when the
/// containment cannot enumerate them.
fn live_descendants(os: Os, containment: Containment, pid: u32) -> Result<Option<u32>, String> {
    Ok((os.enumerate)(containment, pid)?.map(|list| {
        list.iter()
            .filter(|process| process.pid != pid && process.live)
            .count() as u32
    }))
}

/// After the kill, wait until no process of the sample remains (zombies in a namespace are
/// reaped here, as its init), or until `deadline`: the count left, `Ok(None)` when nothing is
/// observable, and the enumeration's own error when it fails.
fn verify_gone(
    os: Os,
    containment: Containment,
    pid: u32,
    deadline: Instant,
) -> Result<Option<u32>, String> {
    loop {
        if containment == Containment::PidNamespace {
            reap_orphans();
        }
        let Some(list) = (os.enumerate)(containment, pid)? else {
            return Ok(None);
        };
        let remaining = match containment {
            // Every process but this init belongs to the sample, zombies included.
            Containment::PidNamespace => list.len() as u32,
            Containment::ProcessGroup => list.iter().filter(|process| process.live).count() as u32,
        };
        if remaining == 0 || Instant::now() >= deadline {
            return Ok(Some(remaining));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn reap_orphans() {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    while let Ok(status) = waitpid(None, Some(WaitPidFlag::WNOHANG)) {
        if status == WaitStatus::StillAlive {
            break;
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_orphans() {}

struct Process {
    pid: u32,
    live: bool,
}

/// The processes a containment enumerated; `Ok(None)` when it can enumerate none, and the
/// enumeration's own error when it failed.
type Listing = Result<Option<Vec<Process>>, String>;

/// The processes the containment can enumerate: every process but this one in a namespace, or
/// the child's process group on Linux. A process that vanished between the listing and its read
/// is gone; any other failure to list `/proc`, or to read or parse an entry, is an error, never
/// an absent process.
#[cfg(target_os = "linux")]
fn processes(containment: Containment, pid: u32) -> Listing {
    let me = std::process::id();
    let entries = std::fs::read_dir("/proc").map_err(|error| format!("listing /proc: {error}"))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("listing /proc: {error}"))?;
        let Some(other) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if other == me {
            continue;
        }
        let Some((state, group)) = stat_fields(other)? else {
            continue;
        };
        if containment == Containment::ProcessGroup && group != pid {
            continue;
        }
        found.push(Process {
            pid: other,
            live: !matches!(state, 'Z' | 'X' | 'x'),
        });
    }
    Ok(Some(found))
}

#[cfg(not(target_os = "linux"))]
fn processes(containment: Containment, _pid: u32) -> Listing {
    match containment {
        Containment::ProcessGroup => Ok(None),
        Containment::PidNamespace => Err("a PID namespace is enumerated only on Linux".into()),
    }
}

/// State and process group from `/proc/<pid>/stat`, or `None` when the process is gone.
#[cfg(target_os = "linux")]
fn stat_fields(pid: u32) -> Result<Option<(char, u32)>, String> {
    let path = format!("/proc/{pid}/stat");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(format!("reading {path}: {error}")),
    };
    parse_stat(&bytes).map(Some).ok_or_else(|| {
        format!(
            "{path} is not a process status: {:?}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(256)])
        )
    })
}

/// The state and process group of one `/proc/<pid>/stat` line. The command name may hold any
/// byte, so the fields are read after its last `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_stat(bytes: &[u8]) -> Option<(char, u32)> {
    let close = bytes.iter().rposition(|byte| *byte == b')')?;
    let rest = std::str::from_utf8(&bytes[close + 1..]).ok()?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent = fields.next()?;
    let group = fields.next()?.parse().ok()?;
    Some((state, group))
}

#[cfg(unix)]
fn children_usage() -> Result<nix::sys::resource::Usage, String> {
    nix::sys::resource::getrusage(nix::sys::resource::UsageWho::RUSAGE_CHILDREN)
        .map_err(|error| format!("getrusage(RUSAGE_CHILDREN) failed: {error}"))
}

#[cfg(unix)]
fn usage_delta(
    before: Result<nix::sys::resource::Usage, String>,
    after: Result<nix::sys::resource::Usage, String>,
) -> Result<Rusage, String> {
    let (before, after) = (before?, after?);
    let nanos = |time: nix::sys::time::TimeVal| {
        time.tv_sec() as i128 * 1_000_000_000 + time.tv_usec() as i128 * 1_000
    };
    let cpu = |after: nix::sys::time::TimeVal, before: nix::sys::time::TimeVal| {
        u128::try_from(nanos(after) - nanos(before)).map_err(|_| "child CPU time went backwards")
    };
    let count = |after: std::ffi::c_long, before: std::ffi::c_long| {
        u64::try_from(after - before).map_err(|_| "a child usage counter went backwards")
    };
    let maxrss = u64::try_from(after.max_rss()).map_err(|_| "negative maximum resident set")?;
    Ok(Rusage {
        user_cpu_ns: cpu(after.user_time(), before.user_time())?,
        system_cpu_ns: cpu(after.system_time(), before.system_time())?,
        maxrss_bytes: if cfg!(target_vendor = "apple") {
            maxrss
        } else {
            maxrss * 1024
        },
        minor_faults: count(after.minor_page_faults(), before.minor_page_faults())?,
        major_faults: count(after.major_page_faults(), before.major_page_faults())?,
        voluntary_switches: count(
            after.voluntary_context_switches(),
            before.voluntary_context_switches(),
        )?,
        involuntary_switches: count(
            after.involuntary_context_switches(),
            before.involuntary_context_switches(),
        )?,
    })
}

#[cfg(not(unix))]
fn children_usage() -> Result<(), String> {
    Err("rusage is unavailable on this platform".into())
}

#[cfg(not(unix))]
fn usage_delta(_before: Result<(), String>, _after: Result<(), String>) -> Result<Rusage, String> {
    Err("rusage is unavailable on this platform".into())
}

/// One observed file's name and its bounded text, or why it could not be read.
pub type Observed = (String, Result<String, String>);

/// Bounded raw text of the cgroup v2 files, CPU affinity and SMT siblings a Linux sample
/// container reports; the trusted driver parses them. Each value is at most 1 KiB; a missing
/// file is listed with its reason, never as an invented zero.
pub fn linux_observations() -> Result<Vec<Observed>, String> {
    if !cfg!(target_os = "linux") {
        return Err("cgroup v2 and /proc observations exist only on Linux".into());
    }
    let mut observed = Vec::new();
    for name in CGROUP_FILES {
        observed.push((
            format!("cgroup/{name}"),
            read_bounded(&format!("/sys/fs/cgroup/{name}"), 1024),
        ));
    }
    // The status file is longer than one value; only the extracted list is reported.
    let allowed = read_bounded("/proc/self/status", 16 * 1024).map(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .map(|value| value.trim().to_string())
            .unwrap_or_default()
    });
    if let Ok(list) = &allowed {
        for cpu in expand_cpu_list(list).into_iter().take(8) {
            observed.push((
                format!("cpu{cpu}/thread_siblings_list"),
                read_bounded(
                    &format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"),
                    1024,
                ),
            ));
        }
    }
    observed.push(("proc/cpus_allowed_list".into(), allowed));
    Ok(observed)
}

const CGROUP_FILES: [&str; 12] = [
    "cpu.max",
    "cpu.stat",
    "cpu.pressure",
    "cpuset.cpus.effective",
    "memory.max",
    "memory.swap.max",
    "memory.current",
    "memory.peak",
    "memory.events",
    "memory.pressure",
    "pids.max",
    "pids.current",
];

fn read_bounded(path: &str, limit: u64) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{path}: {error}"))?;
    let mut text = String::new();
    file.take(limit)
        .read_to_string(&mut text)
        .map_err(|error| format!("{path}: {error}"))?;
    Ok(text)
}

/// `0-2,5` as `[0, 1, 2, 5]`; anything malformed contributes nothing.
pub fn expand_cpu_list(list: &str) -> Vec<u32> {
    let mut cpus = Vec::new();
    for part in list
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let range = match part.split_once('-') {
            Some((low, high)) => low.parse::<u32>().ok().zip(high.parse::<u32>().ok()),
            None => part.parse::<u32>().ok().map(|cpu| (cpu, cpu)),
        };
        if let Some((low, high)) = range
            && low <= high
            && high - low < 4096
        {
            cpus.extend(low..=high);
        }
    }
    cpus
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str, timeout: Duration) -> SampleSpec {
        SampleSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            stdin: b"request\n".to_vec(),
            timeout,
            stdout_cap: 1024,
            stderr_cap: 1024,
            containment: Containment::ProcessGroup,
            cleanup_grace: Duration::from_secs(5),
            stdout_patterns: Vec::new(),
            stdout_json: Vec::new(),
        }
    }

    std::thread_local! {
        /// The spawns a test's injected spawn reached on this thread.
        static SPAWNS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        /// The control a test's injected spawn cancels, on the thread that launches.
        static GATED: std::cell::RefCell<Option<Arc<Control>>> =
            const { std::cell::RefCell::new(None) };
    }

    fn counted_spawn(spec: &SampleSpec) -> std::io::Result<Spawned> {
        SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
        spawn_clean(spec)
    }

    /// The spawn of a cancellation that arrives at the OS spawn boundary: another thread
    /// cancels and has returned before `posix_spawn` is called, and no cancellation is
    /// admitted at that moment; then the real spawn.
    fn cancelled_at_the_spawn(spec: &SampleSpec) -> std::io::Result<Spawned> {
        let control = GATED.with(|gated| gated.borrow().clone()).unwrap();
        let cancel = Arc::clone(&control);
        std::thread::spawn(move || cancel.cancel(15))
            .join()
            .unwrap();
        assert_eq!(control.cancelled(), None, "admitted inside the gate");
        counted_spawn(spec)
    }

    #[test]
    fn a_cancellation_before_the_launch_launches_nothing() {
        let marker = scratch_path("never-launched");
        let control = Control::new();
        control.cancel(15);
        let os = Os {
            spawn: counted_spawn,
            ..NATIVE
        };
        let outcome = run_sample_on(
            &shell(&format!("touch '{}'", marker.display()), WALL),
            &control,
            os,
        );
        assert!(
            matches!(&outcome.termination, Termination::LaunchFailed { detail }
                if detail == "cancelled by signal 15 before the launch"),
            "{:?}",
            outcome.termination
        );
        assert_eq!(outcome.exit_observation, ExitObservation::NothingLaunched);
        assert_eq!(
            SPAWNS.with(std::cell::Cell::get),
            0,
            "the spawn was reached"
        );
        assert!(!marker.exists());
    }

    #[test]
    fn the_launch_gate_orders_every_cancellation_before_or_after_the_spawn() {
        // Admitted before the gate: nothing inside it runs.
        let control = Control::new();
        control.cancel(15);
        assert_eq!(control.launch(|| unreachable!("spawned")), Err::<(), _>(15));

        // The root's boundary injection (ADR-0173): a cancellation made inside the spawn, on
        // the launching thread, just before the OS spawn. The gate already decided, so it is
        // admitted only when the gate opens, never while the spawn could still miss it.
        let control = Control::new();
        let seen = control.launch(|| {
            control.cancel(15);
            control.cancelled()
        });
        assert_eq!(seen, Ok(None));
        assert_eq!(control.cancelled(), Some(15));

        // From another thread, completed while the gate is held: the same.
        let control = Control::new();
        let seen = control.launch(|| {
            let cancel = Arc::clone(&control);
            std::thread::spawn(move || cancel.cancel(2)).join().unwrap();
            control.cancelled()
        });
        assert_eq!(seen, Ok(None));
        assert_eq!(control.cancelled(), Some(2));
        // The first signal recorded wins, inside the gate too.
        control.cancel(1);
        assert_eq!(control.cancelled(), Some(2));
    }

    #[test]
    fn a_cancellation_racing_the_gate_never_meets_a_spawn_that_missed_it() {
        // Both sides released together by a barrier, many times: whichever comes first, a
        // spawn never runs once a cancellation is admitted, and one that ran saw none.
        for _ in 0..2000 {
            let control = Control::new();
            let start = Arc::new(std::sync::Barrier::new(2));
            let (cancel, release) = (Arc::clone(&control), Arc::clone(&start));
            let canceller = std::thread::spawn(move || {
                release.wait();
                cancel.cancel(15);
            });
            start.wait();
            let launched = control.launch(|| control.cancelled());
            canceller.join().unwrap();
            assert!(
                matches!(launched, Ok(None) | Err(15)),
                "a spawn ran with a cancellation admitted: {launched:?}"
            );
            assert_eq!(control.cancelled(), Some(15));
        }
    }

    #[test]
    fn a_cancellation_arriving_at_the_spawn_cancels_the_launched_child() {
        // The cancellation completes at the OS spawn boundary; the child is launched and the
        // admitted cancellation then ends it through its group, never as a clean sample.
        let control = Control::new();
        GATED.with(|gated| *gated.borrow_mut() = Some(Arc::clone(&control)));
        let os = Os {
            spawn: cancelled_at_the_spawn,
            ..NATIVE
        };
        let started = Instant::now();
        let outcome = run_sample_on(
            &shell(
                "trap 'echo forwarded; exit 0' TERM; while :; do sleep 0.05; done",
                WALL,
            ),
            &control,
            os,
        );
        GATED.with(|gated| *gated.borrow_mut() = None);
        assert!(started.elapsed() < NEVER, "{:?}", started.elapsed());
        assert_eq!(SPAWNS.with(std::cell::Cell::get), 1);
        assert_eq!(outcome.termination, Termination::Cancelled { signal: 15 });
        assert_eq!(outcome.child_wall_ns, None);
    }

    #[test]
    fn patterns_are_counted_across_chunks_and_past_the_cap() {
        let mut counter = PatternCounter::new(vec![b"{\"seq\":".to_vec(), b"aa".to_vec()]);
        let text = b"x{\"seq\":1}{\"seq\":2}aaa";
        let mut counts = vec![0_u64; 2];
        // Every split point, so no occurrence is lost or counted twice at a boundary.
        for split in 0..=text.len() {
            let mut counter = PatternCounter::new(vec![b"{\"seq\":".to_vec(), b"aa".to_vec()]);
            let mut found = vec![0_u64; 2];
            for part in [&text[..split], &text[split..]] {
                for (total, count) in found.iter_mut().zip(counter.feed(part)) {
                    *total += count;
                }
            }
            assert_eq!(found, vec![2, 2], "split at {split}");
        }
        for byte in text {
            for (total, count) in counts
                .iter_mut()
                .zip(counter.feed(std::slice::from_ref(byte)))
            {
                *total += count;
            }
        }
        assert_eq!(counts, vec![2, 2], "one byte at a time");
        let mut spec = shell(
            "printf '{\"items\":['; i=0; while [ $i -lt 500 ]; do \
             [ $i -gt 0 ] && printf ','; printf '{\"seq\":%d}' $i; i=$((i+1)); done; \
             printf ']}\n'",
            WALL,
        );
        spec.stdout_cap = 16;
        spec.stdout_patterns = vec![b"{\"seq\":".to_vec(), b"{\"seq\":499}".to_vec()];
        spec.stdout_json = vec![
            crate::json_stream::Selector::parse(
                "items.*.seq",
                crate::json_stream::Kind::Integer,
                Vec::new(),
            )
            .unwrap(),
        ];
        let outcome = run_sample(&spec, &Control::new());
        assert_eq!(outcome.stdout.retained.len(), 16);
        assert!(outcome.stdout.total > 16);
        assert_eq!(outcome.stdout.pattern_counts, vec![500, 1]);
        assert!(outcome.stderr.pattern_counts.is_empty());
        // The document is observed whole, far past the 16 retained bytes.
        let json = outcome
            .stdout
            .json
            .expect("the declared selectors were observed");
        assert_eq!(json.document, crate::json_stream::Document::Complete);
        assert_eq!(
            json.selectors,
            vec![crate::json_stream::Selected {
                holders: 500,
                count: 500,
                first: Some(0),
                last: Some(499),
                ..crate::json_stream::Selected::default()
            }]
        );
        assert!(outcome.stderr.json.is_none());
    }

    const WALL: Duration = Duration::from_secs(120);
    /// Far beyond any deadline under test: a sample that returns sooner was ended by its
    /// deadline or cancellation, not by its child.
    const NEVER: Duration = Duration::from_secs(30);

    /// A fresh path below the temporary directory, unique to this process and call.
    fn scratch_path(name: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "af-sample-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// The pid a fixture wrote to `path`, waiting up to `NEVER` for it.
    fn written_pid(path: &std::path::Path) -> i32 {
        let deadline = Instant::now() + NEVER;
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse()
            {
                let _ = std::fs::remove_file(path);
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "{} was never written",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn injected_observer(_pid: u32) -> Result<(), String> {
        Err("waitid(WNOWAIT): EIO: I/O error (injected)".into())
    }

    fn injected_reap(_pid: u32) -> Result<Option<Termination>, String> {
        Err("waitpid(WNOHANG): EIO: I/O error (injected)".into())
    }

    fn injected_enumeration(_: Containment, _pid: u32) -> Listing {
        Err("listing /proc: Input/output error (os error 5) (injected)".into())
    }

    #[test]
    fn a_finished_child_is_timed_and_its_streams_are_complete() {
        let outcome = run_sample(
            &shell("cat; printf err >&2; exit 3", WALL),
            &Control::default(),
        );
        assert_eq!(outcome.termination, Termination::Exited { code: 3 });
        assert_eq!(outcome.stdout.retained, b"request\n");
        assert_eq!(outcome.stderr.retained, b"err");
        assert!(outcome.child_wall_ns.is_some_and(|nanos| nanos > 0));
        assert_eq!(outcome.exit_observation, ExitObservation::Unreaped);
        assert!(!outcome.lifecycle.pipes_held);
        assert!(
            matches!(
                outcome.lifecycle.status,
                LifecycleStatus::Clean | LifecycleStatus::NoObservedDescendants
            ),
            "{}",
            outcome.lifecycle.detail
        );
        assert!(outcome.rusage.is_ok());
    }

    #[test]
    fn the_environment_is_exactly_the_declared_one() {
        let mut spec = shell("env", WALL);
        spec.env.push(("ONLY".into(), "this".into()));
        let outcome = run_sample(&spec, &Control::default());
        let text = String::from_utf8(outcome.stdout.retained).unwrap();
        let mut keys: Vec<&str> = text
            .lines()
            .filter_map(|line| line.split('=').next())
            .filter(|key| !matches!(*key, "PWD" | "SHLVL" | "_" | "OLDPWD"))
            .collect();
        keys.sort();
        assert_eq!(keys, ["ONLY", "PATH"]);
    }

    #[test]
    fn output_beyond_the_caps_is_drained_and_counted() {
        let outcome = run_sample(
            &shell(
                "head -c 100000 /dev/zero; head -c 70000 /dev/zero >&2",
                WALL,
            ),
            &Control::default(),
        );
        assert_eq!(outcome.termination, Termination::Exited { code: 0 });
        assert_eq!(outcome.stdout.retained.len(), 1024);
        assert_eq!(outcome.stdout.total, 100_000);
        assert_eq!(outcome.stdout.truncated(), 100_000 - 1024);
        assert_eq!(outcome.stderr.truncated(), 70_000 - 1024);
    }

    #[test]
    fn a_descendant_forked_as_the_group_is_killed_is_killed_too() {
        // The child exits at once while a member forks a long sleep that holds the inherited
        // pipes. A single group kill misses a process forked as it arrives (on macOS about one
        // run in four), which would hold the pipes past the grace; the group is signalled
        // again until its pipes close, so no run here keeps a live member or a held pipe, and
        // none is uncertain.
        for _ in 0..20 {
            let mut spec = shell("(sleep 10; :) & exit 0", WALL);
            spec.cleanup_grace = Duration::from_secs(2);
            let outcome = run_sample(&spec, &Control::default());
            assert_eq!(outcome.termination, Termination::Exited { code: 0 });
            assert!(
                !outcome.lifecycle.pipes_held,
                "{}",
                outcome.lifecycle.detail
            );
            assert_ne!(
                outcome.lifecycle.status,
                LifecycleStatus::CleanupUncertain,
                "{}",
                outcome.lifecycle.detail
            );
        }
    }

    #[test]
    fn a_deadline_kills_the_group_and_reports_a_timeout() {
        let started = Instant::now();
        let outcome = run_sample(
            &shell("sleep 60 & exec sleep 60", Duration::from_millis(300)),
            &Control::default(),
        );
        assert_eq!(outcome.termination, Termination::TimedOut);
        assert_eq!(outcome.child_wall_ns, None);
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(!outcome.lifecycle.pipes_held);
    }

    #[test]
    fn a_launch_failure_is_an_outcome() {
        let mut spec = shell("true", WALL);
        spec.program = "/nonexistent/perf-sample-program".into();
        let outcome = run_sample(&spec, &Control::default());
        assert!(matches!(
            outcome.termination,
            Termination::LaunchFailed { .. }
        ));
        assert_eq!(outcome.exit_observation, ExitObservation::NothingLaunched);
        assert!(outcome.rusage.is_err());
    }

    #[test]
    fn a_cancellation_forwards_its_signal_to_the_group() {
        let control = Control::new();
        let cancel = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel(15);
        });
        let outcome = run_sample(
            &shell(
                "trap 'echo forwarded; exit 0' TERM; while :; do sleep 1; done",
                WALL,
            ),
            &control,
        );
        assert_eq!(outcome.termination, Termination::Cancelled { signal: 15 });
        assert_eq!(outcome.child_wall_ns, None);
        assert_eq!(outcome.stdout.retained, b"forwarded\n");
    }

    /// A child whose descendant leaves the process group for a new session and, from inside
    /// that session and while it holds the child's stdout and stderr, writes its pid to
    /// `marker`; the child exits only after that handshake, so cleanup cannot catch the
    /// descendant before it escaped.
    fn escape_script(marker: &std::path::Path) -> String {
        format!(
            "M='{}'; \
             if command -v setsid >/dev/null 2>&1; then \
               setsid sh -c 'echo $$ > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"; exec sleep 30' \"$M\" & \
             else \
               perl -e 'use POSIX; fork and exit; POSIX::setsid() or die; \
                 open(my $f, \">\", \"$ARGV[0].tmp\") or die; print $f \"$$\\n\"; close $f; \
                 rename(\"$ARGV[0].tmp\", $ARGV[0]) or die; exec \"sleep\", \"30\"' \"$M\"; \
             fi; \
             i=0; while [ ! -f \"$M\" ] && [ $i -lt 3000 ]; do sleep 0.01; i=$((i+1)); done; \
             [ -f \"$M\" ] || exit 9; exit 0",
            marker.display()
        )
    }

    /// End an escaped fixture process, which is no longer anyone's child here, and wait until
    /// it is gone.
    fn end_escaped(pid: i32) {
        use nix::sys::signal::{Signal, kill};
        let pid = nix::unistd::Pid::from_raw(pid);
        let _ = kill(pid, Signal::SIGKILL);
        let deadline = Instant::now() + NEVER;
        while kill(pid, None).is_ok() {
            assert!(Instant::now() < deadline, "escaped fixture {pid} survived");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_descendant_holding_a_pipe_outside_the_group_makes_cleanup_uncertain() {
        // A new session leaves the process group; only the pipe it holds reveals it.
        let marker = scratch_path("escaped");
        let mut spec = shell(&escape_script(&marker), WALL);
        spec.cleanup_grace = Duration::from_millis(500);
        let outcome = run_sample(&spec, &Control::default());
        let escaped = written_pid(&marker);
        // The expected path: the descendant leads a session of its own and outlived cleanup.
        let session = nix::unistd::getsid(Some(nix::unistd::Pid::from_raw(escaped)));
        end_escaped(escaped);
        assert_eq!(session.map(|session| session.as_raw()), Ok(escaped));
        assert_eq!(outcome.termination, Termination::Exited { code: 0 });
        assert!(outcome.lifecycle.pipes_held);
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
    }

    #[test]
    fn a_failed_exit_observation_keeps_the_deadline_and_is_never_timed() {
        let os = Os {
            wait_unreaped: injected_observer,
            ..NATIVE
        };
        let started = Instant::now();
        let outcome = run_sample_on(
            &shell("exec sleep 30", Duration::from_millis(100)),
            &Control::default(),
            os,
        );
        assert!(started.elapsed() < NEVER / 3, "{:?}", started.elapsed());
        assert_eq!(outcome.termination, Termination::TimedOut);
        assert_eq!(outcome.exit_observation, ExitObservation::ReapedByPolling);
        assert_eq!(outcome.child_wall_ns, None);
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
        assert!(
            outcome
                .lifecycle
                .detail
                .contains("waitid(WNOWAIT): EIO: I/O error (injected)"),
            "{}",
            outcome.lifecycle.detail
        );
        assert!(outcome.rusage.is_ok(), "the killed child was reaped");
    }

    #[test]
    fn a_failed_exit_observation_keeps_cancellation() {
        let os = Os {
            wait_unreaped: injected_observer,
            ..NATIVE
        };
        let control = Control::new();
        let cancel = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.cancel(15);
        });
        let started = Instant::now();
        let outcome = run_sample_on(
            &shell(
                "trap 'echo forwarded; exit 0' TERM; while :; do sleep 0.05; done",
                WALL,
            ),
            &control,
            os,
        );
        assert!(started.elapsed() < NEVER, "{:?}", started.elapsed());
        assert_eq!(outcome.termination, Termination::Cancelled { signal: 15 });
        assert_eq!(outcome.exit_observation, ExitObservation::ReapedByPolling);
        assert_eq!(outcome.stdout.retained, b"forwarded\n");
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
    }

    #[test]
    fn an_exit_reaped_by_polling_keeps_its_status_but_establishes_no_interval() {
        let os = Os {
            wait_unreaped: injected_observer,
            ..NATIVE
        };
        let outcome = run_sample_on(&shell("exit 4", WALL), &Control::default(), os);
        assert_eq!(outcome.termination, Termination::Exited { code: 4 });
        assert_eq!(outcome.exit_observation, ExitObservation::ReapedByPolling);
        assert_eq!(outcome.child_wall_ns, None);
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
        assert!(
            outcome.lifecycle.detail.starts_with(
                "the child's exit could not be observed without reaping it (waitid(WNOWAIT): \
                 EIO: I/O error (injected)); it was reaped by polling, so its interval is not \
                 established"
            ),
            "{}",
            outcome.lifecycle.detail
        );
    }

    #[test]
    fn an_exit_nothing_can_observe_ends_at_the_deadline_and_is_never_waited_for() {
        let os = Os {
            wait_unreaped: injected_observer,
            try_reap: injected_reap,
            ..NATIVE
        };
        let pidfile = scratch_path("pid");
        let mut spec = shell(
            &format!("echo $$ > '{}'; exec sleep 30", pidfile.display()),
            Duration::from_secs(1),
        );
        spec.cleanup_grace = Duration::from_millis(300);
        let started = Instant::now();
        let outcome = run_sample_on(&spec, &Control::default(), os);
        let elapsed = started.elapsed();
        // The fixture's child was killed but, by construction, never reaped: reap it here.
        let child = written_pid(&pidfile);
        let status = nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(child), None);
        assert!(elapsed < NEVER / 3, "{elapsed:?}");
        assert!(
            matches!(
                status,
                Ok(nix::sys::wait::WaitStatus::Signaled(
                    _,
                    nix::sys::signal::Signal::SIGKILL,
                    _
                ))
            ),
            "{status:?}"
        );
        assert_eq!(outcome.termination, Termination::TimedOut);
        assert_eq!(outcome.exit_observation, ExitObservation::Unobserved);
        assert_eq!(outcome.child_wall_ns, None);
        assert!(
            outcome
                .rusage
                .as_ref()
                .is_err_and(|reason| reason.contains("never reaped"))
        );
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
        for injected in [
            "waitid(WNOWAIT): EIO: I/O error (injected)",
            "waitpid(WNOHANG): EIO: I/O error (injected)",
        ] {
            assert!(
                outcome.lifecycle.detail.contains(injected),
                "{}",
                outcome.lifecycle.detail
            );
        }
    }

    #[test]
    fn a_failed_enumeration_is_never_a_clean_lifecycle() {
        let os = Os {
            enumerate: injected_enumeration,
            ..NATIVE
        };
        let outcome = run_sample_on(&shell("exit 0", WALL), &Control::default(), os);
        assert_eq!(outcome.termination, Termination::Exited { code: 0 });
        assert_eq!(outcome.exit_observation, ExitObservation::Unreaped);
        assert!(outcome.child_wall_ns.is_some());
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
        assert_eq!(outcome.lifecycle.descendants_at_exit, None);
        assert_eq!(outcome.lifecycle.remaining_after_cleanup, None);
        // The original error comes first, verbatim, and the cleanup's own failure follows.
        assert!(
            outcome.lifecycle.detail.starts_with(
                "enumerating the sample's processes at the child's exit failed: listing /proc: \
                 Input/output error (os error 5) (injected)"
            ),
            "{}",
            outcome.lifecycle.detail
        );
        assert!(outcome.lifecycle.detail.contains("after cleanup failed"));
    }

    #[test]
    fn namespace_containment_is_refused_before_anything_is_spawned_or_signalled() {
        if namespace_init() {
            // This test process is a namespace's init: the containment is genuine here.
            return;
        }
        let marker = scratch_path("launched");
        let mut spec = shell(&format!("touch '{}'", marker.display()), WALL);
        spec.containment = Containment::PidNamespace;
        let outcome = run_sample(&spec, &Control::default());
        assert_eq!(
            outcome.termination,
            Termination::LaunchFailed {
                detail: NOT_NAMESPACE_INIT.into()
            }
        );
        assert_eq!(outcome.exit_observation, ExitObservation::NothingLaunched);
        assert!(!marker.exists());
        // Defence in depth: the broadcast refuses by itself. Signal 0 only checks permissions,
        // so even a broken guard would deliver nothing here.
        let refused = signal_all(Containment::PidNamespace, 0, 0).unwrap_err();
        assert!(refused.contains("PID 1"), "{refused}");
    }

    /// Run as PID 1 of a disposable PID namespace only: a container whose init is this test
    /// binary, started with `--ignored --exact <name>`. Anywhere else `kill(-1)` would reach
    /// every process of the user, so each one asserts that it is a namespace's init first.
    fn namespace_spec(script: &str, timeout: Duration) -> SampleSpec {
        assert!(
            namespace_init(),
            "run only as PID 1 of a disposable PID namespace"
        );
        let mut spec = shell(script, timeout);
        spec.containment = Containment::PidNamespace;
        spec
    }

    #[test]
    #[ignore = "PID 1 of a disposable PID namespace only"]
    fn a_namespace_init_kills_and_verifies_a_new_session_escape() {
        let spec = namespace_spec("setsid sleep 30 & sleep 0.2; exit 0", WALL);
        let outcome = run_sample(&spec, &Control::default());
        assert_eq!(outcome.termination, Termination::Exited { code: 0 });
        assert_eq!(outcome.lifecycle.observation, Observation::PidNamespaceScan);
        assert_eq!(outcome.lifecycle.descendants_at_exit, Some(1));
        assert_eq!(outcome.lifecycle.remaining_after_cleanup, Some(0));
        assert!(!outcome.lifecycle.pipes_held);
        assert_eq!(
            outcome.lifecycle.status,
            LifecycleStatus::UnexpectedDescendants
        );
    }

    #[test]
    #[ignore = "PID 1 of a disposable PID namespace only"]
    fn a_failed_namespace_enumeration_is_never_clean() {
        let os = Os {
            enumerate: injected_enumeration,
            ..NATIVE
        };
        let outcome = run_sample_on(&namespace_spec("exit 0", WALL), &Control::default(), os);
        assert_eq!(outcome.termination, Termination::Exited { code: 0 });
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
        assert_eq!(outcome.lifecycle.remaining_after_cleanup, None);
        assert!(
            outcome.lifecycle.detail.starts_with(
                "enumerating the sample's processes at the child's exit failed: listing /proc: \
                 Input/output error (os error 5) (injected)"
            ),
            "{}",
            outcome.lifecycle.detail
        );
    }

    #[test]
    #[ignore = "PID 1 of a disposable PID namespace only"]
    fn a_failed_exit_observation_in_a_namespace_keeps_the_deadline() {
        let os = Os {
            wait_unreaped: injected_observer,
            ..NATIVE
        };
        let started = Instant::now();
        let outcome = run_sample_on(
            &namespace_spec("sleep 30 & exec sleep 30", Duration::from_millis(100)),
            &Control::default(),
            os,
        );
        assert!(started.elapsed() < NEVER / 3, "{:?}", started.elapsed());
        assert_eq!(outcome.termination, Termination::TimedOut);
        assert_eq!(outcome.exit_observation, ExitObservation::ReapedByPolling);
        assert_eq!(outcome.lifecycle.status, LifecycleStatus::CleanupUncertain);
        // The namespace is still killed and scanned after a polled reap.
        assert_eq!(outcome.lifecycle.remaining_after_cleanup, Some(0));
        assert!(outcome.lifecycle.detail.contains("(injected)"));
    }

    #[test]
    fn a_process_status_is_read_after_the_last_parenthesis_and_malformed_ones_are_refused() {
        assert_eq!(parse_stat(b"42 (a) b) c) S 1 42 42 0"), Some(('S', 42)));
        assert_eq!(parse_stat(b"42 (sh) Z 1 7 7 0\n"), Some(('Z', 7)));
        assert_eq!(parse_stat(b"42 (sh) S 1"), None);
        assert_eq!(parse_stat(b"42 (sh) S 1 x"), None);
        assert_eq!(parse_stat(b"garbage"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_live_group_member_at_exit_is_an_unexpected_descendant_on_linux() {
        let outcome = run_sample(
            &shell("sleep 30 >/dev/null 2>&1 </dev/null & exit 0", WALL),
            &Control::default(),
        );
        assert_eq!(outcome.lifecycle.observation, Observation::ProcessGroupScan);
        assert_eq!(outcome.lifecycle.descendants_at_exit, Some(1));
        assert_eq!(outcome.lifecycle.remaining_after_cleanup, Some(0));
        assert_eq!(
            outcome.lifecycle.status,
            LifecycleStatus::UnexpectedDescendants
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reports_descendants_as_unobservable_not_absent() {
        let outcome = run_sample(
            &shell("sleep 30 >/dev/null 2>&1 </dev/null & exit 0", WALL),
            &Control::default(),
        );
        assert_eq!(outcome.lifecycle.observation, Observation::PipesOnly);
        assert_eq!(outcome.lifecycle.descendants_at_exit, None);
        assert_eq!(
            outcome.lifecycle.status,
            LifecycleStatus::NoObservedDescendants
        );
    }

    #[test]
    fn cpu_lists_expand_and_malformed_parts_contribute_nothing() {
        assert_eq!(expand_cpu_list("0-2,5"), [0, 1, 2, 5]);
        assert_eq!(expand_cpu_list(" 3 \n"), [3]);
        assert_eq!(expand_cpu_list("4-2,x,7"), [7]);
    }
}
