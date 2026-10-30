"""Diagnostic tools and candidates under the authority supervisor (package p5, ADR-0175).

Nothing here forks process supervision. Every diagnostic tool and every candidate execution is
one `af.perf-supervisor-request/4` to the authority-built `perf-supervisor`, the same hashed
bytes that bracket verdict samples, so its deadline, process-group or PID-namespace
containment, bounded stdin, concurrent bounded drains, descendant scan and cleanup are
`review_process::sample`'s (ADR-0026, ADR-0166). This module only builds the request, launches
it through `supervise.launch` and reads the reply:

- **One absolute deadline.** A run is given the time left before the caller's monotonic
  deadline, less the supervisor's cancellation reserve; nothing launches without it. A reply
  that comes after the deadline is a failure whatever it says, never a success.
- **Bounded streams.** Stdin is at most one 64-KiB record; stdout and stderr are retained up to
  their caps and counted beyond them. A truncated stdout is never parsed as if it were whole.
  Output larger than a stream cap goes to a new file (`stdout_path`), written by the tool
  itself under the byte room the caller grants (`RLIMIT_FSIZE`), and is admitted afterwards
  like any other file a tool wrote.
- **Lifecycle.** A descendant that outlived its tool, an exit that was not observed, an
  uncertain cleanup or a cancellation fails the run, and an uncertain cleanup is reported so the
  caller keeps everything the run could reach.
- **Cancellation.** `start` returns a handle whose `cancel` sends the supervisor SIGTERM, which
  it forwards to the run through the same supervision (ADR-0171).
- **All outputs together.** A `watch` asks, while the run lasts, whether everything it wrote
  outgrew its caller's room, and cancels it when it did (ADR-0176).
- **Contained.** `Contained` runs the same supervisor as PID 1 of one sample container per
  request through `supervise.run_contained`, the container a shard's sample gets, with the
  request's binds at their own paths and only the run's own directory of the verified scratch
  filesystem writable.

Nothing here retries.
"""
import os
import signal
import threading
import time

from . import limits, supervise
from .values import ContractError

STDERR_CAP_BYTES = 64 * 1024
GRACE_MS = 1000
# A run whose request needs a longer bound than the supervisor accepts is refused, not split.
TIMEOUT_MS_MAX = limits.SAMPLE_TIMEOUT_SECONDS_MAX * 1000
# The redirection that sends a tool's stdout to a new file: `set -C` refuses an existing path,
# and the tool is executed in the shell's place, so the supervisor's child is the tool itself.
REDIRECT = 'set -C; exec "$@" >"$0"'
SHELL = "/bin/sh"
# How often a running tool's written bytes are measured against its room.
WATCH_SECONDS = 0.05


class Supervisor(object):
    """The verified authority supervisor every diagnostic execution goes through, and the
    containment it runs them in. `margin` and `cancel_margin` are the host backstop's margin
    and the cancellation reserve's (`supervise.launch`)."""

    def __init__(self, path, containment="process_group", margin=supervise.HOST_MARGIN_SECONDS,
                 cancel_margin=None, grace_ms=GRACE_MS):
        self.path = path
        self.containment = containment
        self.margin = margin
        self.cancel_margin = cancel_margin
        self.grace_ms = grace_ms

    scratch = None
    # The name of the container a contained request runs in while it runs, for the trusted
    # host observer's identity check (`hostobserver.inspect`); a host request has none.
    active = None

    def reserve(self):
        """Seconds a request keeps before the deadline for its cancellation and start."""
        return (3 * self.grace_ms / 1000.0
                + (supervise.CANCEL_MARGIN_SECONDS if self.cancel_margin is None
                   else self.cancel_margin)
                + supervise.START_SECONDS)

    def launch(self, request_doc, deadline, clock, started, file_bytes, binds):
        """Run one request; its `supervise.Supervised` reply, or `supervise.SupervisorFailure`.
        Every process it starts inherits `RLIMIT_FSIZE` at `file_bytes`; `binds` name nothing
        here, where the run sees the host as the driver does."""
        return supervise.launch([self.path], request_doc, deadline=deadline, clock=clock,
                                margin=self.margin, cancel_margin=self.cancel_margin,
                                started=started, file_bytes=file_bytes)


class Contained(Supervisor):
    """The same authority supervisor as PID 1 of one Linux sample container per request
    (`supervise.run_contained`, ADR-0168, ADR-0176): network none, read-only root, every
    capability dropped, no new privileges, the environment's PID, memory and CPU set with swap
    disabled and no CPU quota, as the declared container user, with only the run's own directory
    of the verified disk-bounded scratch filesystem writable. Each request's container is
    removed and its removal confirmed; an unconfirmed removal, a scratch filesystem that is not
    exactly as it was, or a resource limit the supervisor could not observe as declared fails
    the run, and an unconfirmed removal leaves what it could reach for the operator."""

    containment = "pid_namespace"
    scratch = supervise.SCRATCH

    def __init__(self, path, docker, image, environment, scratch_root, uid, gid, cpus, label,
                 grace_ms=GRACE_MS):
        Supervisor.__init__(self, path, containment="pid_namespace", grace_ms=grace_ms)
        self.docker = docker
        self.image = image
        self.environment = environment
        # Resolved once: every writable bind is compared with the directories below it.
        self.scratch_root = os.path.realpath(scratch_root)
        self.uid = uid
        self.gid = gid
        self.cpus = cpus
        self.label = label
        self.requests = 0

    def reserve(self):
        return Supervisor.reserve(self) + supervise.REMOVAL_SECONDS

    def launch(self, request_doc, deadline, clock, started, file_bytes, binds):
        from . import builds
        from .budget import Budget
        # The same image admission as shard/diagnostic preflight, also for direct users.
        try:
            builds.refuse_implicit_volumes(self.docker, self.image, Budget(
                deadline if deadline is not None else clock() + supervise.HOST_MARGIN_SECONDS,
                clock, supervise.REMOVAL_SECONDS if deadline is not None else 0))
        except (OSError, ContractError) as error:
            raise supervise.SupervisorFailure(str(error), launched=False) from error
        self.requests += 1
        name = "af-perf-diag-%s-%d-%s" % (self.label[-12:], self.requests,
                                          os.urandom(4).hex())
        self.active = name
        try:
            contained = supervise.run_contained(
                self.docker, name, self.image, self.environment, self.path, {},
                self.scratch_root, self.uid, self.gid, self.cpus, request_doc,
                deadline=deadline, clock=clock, binds=binds, file_bytes=file_bytes,
                started=started, extra=(supervise.DIAGNOSTIC_DIRECTORY,))
        finally:
            self.active = None
        problems = []
        if not contained.removed:
            problems.append(contained.removal)
        if contained.scratch_problems:
            problems.append("the scratch filesystem is not as it was: "
                            + "; ".join(contained.scratch_problems))
        if contained.limit_problems:
            problems.append("the container's limits are unverified: "
                            + "; ".join(contained.limit_problems))
        if problems:
            raise supervise.SupervisorFailure("; ".join(problems)[:4096],
                                              uncertain=not contained.removed,
                                              supervised=contained.supervised)
        return contained.supervised


class Result(object):
    def __init__(self, argv, code=None, stdout=b"", stdout_bytes=0, stderr=b"", stderr_bytes=0,
                 timed_out=False, elapsed_ms=0, launch_error=None, lifecycle=None,
                 uncertain=False, late=False, cancelled=False, supervised=None):
        self.argv = argv
        self.code = code
        self.stdout = stdout
        self.stdout_bytes = stdout_bytes
        self.stderr = stderr
        self.stderr_bytes = stderr_bytes
        self.timed_out = timed_out
        self.elapsed_ms = elapsed_ms
        self.launch_error = launch_error
        # Why the supervised lifecycle is not acceptable, or None.
        self.lifecycle = lifecycle
        # Whether a process this run started may still be live.
        self.uncertain = uncertain
        # Whether the reply came after the caller's deadline.
        self.late = late
        self.cancelled = cancelled
        self.supervised = supervised
        # Why the files the run wrote outgrew the room its caller gave them, or None.
        self.exceeded = None

    @property
    def stdout_truncated(self):
        return self.stdout_bytes > len(self.stdout)

    @property
    def ok(self):
        """Exited 0 within its bound and the deadline, with its whole stdout and a clean
        lifecycle."""
        return self.failure() is None

    def failure(self):
        """Why this result cannot be used, or None."""
        supervised = self.supervision_failure()
        if supervised is not None:
            return supervised
        if self.code != 0:
            name = self.argv[0] if self.argv else "the tool"
            tail = self.stderr.decode("utf-8", "replace").strip()[-512:]
            return "%s exited %s%s" % (name, self.code, (": " + tail) if tail else "")
        return None

    def supervision_failure(self, observed_stream=False):
        """Why the supervised run cannot be used whatever its exit code, or None: a launch
        refusal, a late reply, a cancellation, a timeout, a refused lifecycle, an unconfirmed
        cleanup, outputs over the step's room or a truncated stdout. Only the exit code is left
        to the caller's oracle; a stdout prefix that parses is never a whole one (ADR-0176).
        With `observed_stream`, a CLI command's stdout beyond its retained log is not itself a
        failure: the supervisor counted and observed all of it, and the command's oracle judges
        that observation and refuses any rule that needs the retained document whole."""
        name = self.argv[0] if self.argv else "the tool"
        if self.launch_error is not None:
            return "cannot run %s: %s" % (name, self.launch_error)
        if self.late:
            return "%s replied after the diagnostic deadline, so nothing of it is used" % name
        if self.exceeded is not None:
            return "%s: %s" % (name, self.exceeded)
        if self.cancelled:
            return "%s was cancelled" % name
        if self.timed_out:
            return "%s did not finish within its bound and was killed" % name
        if self.lifecycle is not None:
            return "%s: %s" % (name, self.lifecycle)
        if self.uncertain:
            return "%s: its supervisor did not confirm that everything it started ended" % name
        if self.stdout_truncated and not observed_stream:
            return "%s wrote %d bytes, over its %d-byte output cap" % (
                name, self.stdout_bytes, len(self.stdout))
        return None


def command(argv, stdout_path=None):
    """The argv the supervisor runs: `argv` itself, or its stdout redirected to a new file."""
    if stdout_path is None:
        return list(argv)
    return [SHELL, "-c", REDIRECT, stdout_path] + list(argv)


class Running(object):
    """One supervised run in progress: `done()` while it runs, `cancel()` through the
    supervisor, `wait()` for its Result.

    `watch`, when given, is asked every `WATCH_SECONDS` while the run lasts whether what it
    has written so far outgrew the room its caller gave it, all its files together: a reason
    cancels the run through its supervisor and makes its Result fail with that reason
    (`exceeded`). `RLIMIT_FSIZE` bounds each file; this bounds their sum while they are
    written, never only afterwards (ADR-0176)."""

    def __init__(self, supervisor, argv, request_doc, deadline, clock, file_bytes, watch=None,
                 binds=()):
        self.argv = argv
        self.deadline = deadline
        self.clock = clock
        self._process = None
        self._cancelled = False
        self._exceeded = None
        self._lock = threading.Lock()
        self._outcome = None
        self._started = clock()
        self._thread = threading.Thread(target=self._launch, args=(
            supervisor, request_doc, file_bytes, binds))
        self._thread.daemon = True
        self._thread.start()
        if watch is not None:
            watcher = threading.Thread(target=self._watch, args=(watch,))
            watcher.daemon = True
            watcher.start()

    def _watch(self, watch):
        while True:
            self._thread.join(WATCH_SECONDS)
            if not self._thread.is_alive():
                return
            reason = watch()
            if reason is not None:
                with self._lock:
                    self._exceeded = reason
                self.cancel()
                return

    def _launch(self, supervisor, request_doc, file_bytes, binds):
        try:
            self._outcome = ("supervised", supervisor.launch(
                request_doc, self.deadline, self.clock, self._started_process, file_bytes,
                binds))
        except supervise.SupervisorFailure as failure:
            self._outcome = ("failure", failure)
        except BaseException as error:
            # An opaque adapter's ordinary exception is not proof of no launch. Built-in
            # supervisors supply typed pre-launch refusals and post-launch settlement.
            failure = getattr(error, "supervisor_failure", None)
            if failure is None:
                failure = supervise.SupervisorFailure(str(error), uncertain=True)
                failure.__cause__ = error
            self._outcome = ("failure", failure)

    def _started_process(self, process):
        with self._lock:
            self._process = process
            if self._cancelled:
                _terminate(process)

    def done(self):
        return not self._thread.is_alive()

    def cancel(self):
        """Cancel the run through its supervisor; its reply still says how it ended."""
        with self._lock:
            self._cancelled = True
            if self._process is not None:
                _terminate(self._process)

    def wait(self, timeout=None):
        self._thread.join(timeout)
        if self._thread.is_alive():
            return None
        result = _result(self.argv, self._outcome, self.deadline, self.clock, self._started,
                         self._cancelled)
        result.exceeded = self._exceeded
        return result


def _terminate(process):
    try:
        process.send_signal(signal.SIGTERM)
    except OSError:
        pass


def _result(argv, outcome, deadline, clock, started, cancelled):
    now = clock()
    elapsed_ms = int(max(now - started, 0) * 1000)
    late = deadline is not None and now > deadline
    kind, value = outcome
    if kind == "failure":
        supervised = value.supervised
        result = Result(argv, elapsed_ms=elapsed_ms, uncertain=value.uncertain,
                        late=late or value.deadline, cancelled=cancelled,
                        supervised=supervised)
        if not value.launched:
            result.launch_error = str(value)
        else:
            result.lifecycle = str(value)[:4096]
            if supervised is not None:
                _fill(result, supervised)
        return result
    result = Result(argv, elapsed_ms=elapsed_ms, late=late, cancelled=cancelled,
                    supervised=value)
    _fill(result, value)
    return result


def _fill(result, supervised):
    reply = supervised.result
    termination = reply["termination"]
    result.stdout, result.stderr = supervised.stdout, supervised.stderr
    result.stdout_bytes = reply["stdout"]["total"]
    result.stderr_bytes = reply["stderr"]["total"]
    if termination["kind"] == "exited":
        result.code = termination["code"]
    elif termination["kind"] == "signalled":
        result.code = -termination["signal"]
    elif termination["kind"] == "timeout":
        result.timed_out = True
    elif termination["kind"] == "cancelled":
        result.cancelled = True
    elif termination["kind"] == "launch_failed":
        result.launch_error = termination["detail"] or "the supervisor launched nothing"
    if result.lifecycle is None and termination["kind"] in ("exited", "signalled",
                                                             "setup_failed", "status_unknown"):
        failure, detail, _ = supervise.outcome(supervised, reply["supervisor"]["containment"],
                                               control_record=False)
        if failure is not None:
            result.lifecycle = "%s: %s" % (failure, detail)
    if not supervise.cleanup_confirmed(reply):
        result.uncertain = True


def request(supervisor, argv, env, timeout_seconds, output_cap, stdin=b"", cwd=None,
            deadline=None, clock=time.monotonic, stdout_path=None, setup=None, patterns=(),
            selectors=(), log_cap=None):
    """The supervisor request of one run, bounded by `timeout_seconds` and by the time left
    before `deadline` less the supervisor's reserve; None when no time is left. `setup`,
    `patterns` and `selectors` are a CLI command's untimed fixture step, its stdout patterns and
    its streamed JSON observation (`supervise.request`). The retained log is `output_cap` of
    stdout and `STDERR_CAP_BYTES` of stderr, or, with `log_cap`, exactly that total: a CLI
    command's sample log, whose `output_cap` already leaves its stderr reserve
    (`clicases.stdout_bytes`)."""
    seconds = timeout_seconds
    if deadline is not None:
        seconds = min(seconds, deadline - clock() - supervisor.reserve())
    timeout_ms = int(seconds * 1000)
    if timeout_ms < 1:
        return None
    if timeout_ms > TIMEOUT_MS_MAX:
        raise ContractError("a supervised run is bounded by at most %d s" % (
            TIMEOUT_MS_MAX // 1000))
    if isinstance(stdin, bytes):
        stdin = stdin.decode("utf-8")
    if log_cap is None:
        stdout_cap = max(1, min(output_cap, limits.SAMPLE_LOG_BYTES_MAX - STDERR_CAP_BYTES))
        log_cap = stdout_cap + STDERR_CAP_BYTES
    else:
        if not 1 <= output_cap < log_cap <= limits.SAMPLE_LOG_BYTES_MAX:
            raise ContractError("a supervised run's %d-byte stdout must leave stderr room within "
                                "its total log of %d of at most %d bytes" % (
                                    output_cap, log_cap, limits.SAMPLE_LOG_BYTES_MAX))
        stdout_cap = output_cap
    if setup is not None:
        setup_argv, setup_env, setup_cwd, setup_seconds = setup
        setup_ms = int(min(setup_seconds, seconds) * 1000)
        if setup_ms < 1:
            return None
        setup = (setup_argv, setup_env, setup_cwd, setup_ms)
    return supervise.request(
        command(argv, stdout_path), env, cwd or "/", stdin, timeout_ms, stdout_cap,
        log_cap, containment=supervisor.containment,
        scratch=supervisor.scratch, grace_ms=supervisor.grace_ms, setup=setup,
        patterns=patterns, selectors=selectors)


def start(supervisor, argv, env, timeout_seconds, output_cap, stdin=b"", cwd=None,
          deadline=None, clock=time.monotonic, stdout_path=None, file_bytes=None, watch=None,
          binds=(), setup=None, patterns=(), selectors=(), log_cap=None):
    """Start one supervised run; a `Running`, or a launch-refused `Result` when the deadline
    leaves no time for it. `binds` are the (host path, writable) pairs a contained supervisor
    binds into the run's container."""
    argv = list(argv)
    try:
        doc = request(supervisor, argv, env, timeout_seconds, output_cap, stdin, cwd,
                      deadline, clock, stdout_path, setup, patterns, selectors, log_cap)
    except ContractError as error:
        return Result(argv, launch_error=str(error))
    if doc is None:
        return Result(argv, launch_error="the diagnostic deadline leaves no time for it beyond "
                      "the supervisor's %.0f-s cancellation reserve" % supervisor.reserve(),
                      late=True)
    if stdout_path is not None and os.path.lexists(stdout_path):
        return Result(argv, launch_error="its output %s already exists" % stdout_path)
    return Running(supervisor, argv, doc, deadline, clock, file_bytes, watch, binds)


def run(supervisor, argv, env, timeout_seconds, output_cap, stdin=b"", cwd=None,
        deadline=None, clock=time.monotonic, stdout_path=None, file_bytes=None, watch=None,
        binds=(), setup=None, patterns=(), selectors=(), log_cap=None):
    """Run `argv` once under `supervisor`, within `timeout_seconds` and before `deadline`."""
    started = start(supervisor, argv, env, timeout_seconds, output_cap, stdin, cwd, deadline,
                    clock, stdout_path, file_bytes, watch, binds, setup, patterns, selectors,
                    log_cap)
    if isinstance(started, Result):
        return started
    return started.wait()
