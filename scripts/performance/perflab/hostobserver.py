"""A trusted host CPU observer of one owned, contained process (ADR-0240).

The candidate keeps its unchanged sample container: credential-free, network none, every
capability dropped, no new privileges. The operator's perf (`diagnostic.perf`, given
`CAP_PERFMON` by the operator, never bound into a candidate container) runs on the host as the
compute account under the authority supervisor's process-group containment (`toolrun`), the
same bounded runner, deadline and cleanup every diagnostic tool gets; nothing here forks its
own supervision.

The contained command is started behind a gate: `/bin/sh -c GATE GATE_FIFO ARGV...` blocks
reading the private gate FIFO before it executes the command in its own place. The observer

1. finds the one process of the owned container whose command line is exactly that gate (its
   cgroup names the container's full ID, its ancestry reaches the container's init process, it
   runs as this account) and freezes its identity: host PID and its start time in clock ticks,
   so a recycled PID is never mistaken for it;
2. starts `perf record -p PID` with recording disabled (`--delay=-1`) and acknowledged control,
   enables it (whole command) and waits for perf's acknowledgement: the events are open;
3. checks the frozen identity again, then opens the gate: the command is executed in the
   attached task, so a CLI command that exits in milliseconds is recorded from its first
   instruction and no Docker client or waiting shell time is attributed to AF;
4. after the command exits, waits for perf to end of its own accord (perf leaves when its
   target does) and confirms the observer stopped through its supervisor's reply. An observer
   whose end is not confirmed is `cleanup_uncertain`: the caller keeps the common fence.

A supported native probe keeps its acknowledged region: the recording stays disabled until the
probe's region begins and is switched off when it ends, through the same control FIFOs.
"""
import os
import re
import signal
import subprocess
import tempfile
import time

from . import perfcapture, toolrun
from .perfcapture import CaptureError
from .values import ContractError, require, sha256_bytes

SCHEMA = "af.perf-host-observer/1"
SHELL = toolrun.SHELL
# The gate: blocks until the trusted observer opens the FIFO, then executes the command in
# this process's place. A gate that cannot be read never runs the command.
GATE = 'read _ <"$0" || exit 125; exec "$@"'
PROC = "/proc"
PROC_ENTRIES_MAX = 65536
CMDLINE_BYTES_MAX = 64 * 1024
ANCESTRY_MAX = 8
POLL_SECONDS = 0.005
# A target is looked for at most this long after its run started (it waits on the gate).
FIND_SECONDS = 30.0
ACK_SECONDS = 10.0
# Bounded wait for perf to end after its target exited, before it is cancelled.
STOP_SECONDS = 30.0


def gated(argv, gate):
    """The contained command behind the gate FIFO `gate`."""
    return [SHELL, "-c", GATE, gate] + list(argv)


def is_gated(argv):
    return list(argv[:3]) == [SHELL, "-c", GATE]


def attach_argv(perf, event, frequency, unwind, output, control, pid, max_bytes=None):
    """`perf record` attached to `pid`, disabled until enabled through `control`; with
    `max_bytes` perf ends its own session at that room (as `perfcapture.record_argv`)."""
    graph = "fp" if unwind == "frame_pointer" else "dwarf,%d" % 32768
    limit = [] if max_bytes is None else ["--max-size=%dK" % max(1, max_bytes // 1024)]
    return [perf, "record", "--no-buildid-cache", "-e", event, "-F", str(frequency),
            "--call-graph", graph, "--delay=-1", control.argument()] + limit + [
            "-o", output, "-p", str(pid)]


class Target(object):
    """The frozen identity of one observed process."""

    def __init__(self, pid, start_ticks, uid, cmdline_sha256, container_id, init_pid):
        self.pid = pid
        self.start_ticks = start_ticks
        self.uid = uid
        self.cmdline_sha256 = cmdline_sha256
        self.container_id = container_id
        self.init_pid = init_pid

    def record(self):
        return {"pid": self.pid, "start_ticks": self.start_ticks, "uid": self.uid,
                "cmdline_sha256": self.cmdline_sha256, "container_id": self.container_id,
                "init_pid": self.init_pid}


def _read(path, limit):
    with open(path, "rb") as stream:
        return stream.read(limit + 1)


def process(pid, proc=PROC):
    """(ppid, start ticks, uid, cmdline argv, cgroup text) of `pid`, or None when it is gone."""
    base = os.path.join(proc, str(pid))
    try:
        stat = _read(os.path.join(base, "stat"), 4096).decode("ascii", "replace")
        raw = _read(os.path.join(base, "cmdline"), CMDLINE_BYTES_MAX)
        status = _read(os.path.join(base, "status"), 65536).decode("utf-8", "replace")
        cgroup = _read(os.path.join(base, "cgroup"), 65536).decode("utf-8", "replace")
    except (FileNotFoundError, ProcessLookupError, NotADirectoryError):
        return None
    # The command name may hold any byte: the fields follow its last `)`.
    fields = stat[stat.rindex(")") + 2:].split()
    uid = re.search(r"^Uid:\s+(\d+)", status, re.M)
    require(len(fields) > 19 and uid is not None and len(raw) <= CMDLINE_BYTES_MAX,
            "unreadable process record %s" % base)
    argv = [part.decode("utf-8", "surrogateescape") for part in raw.split(b"\0")[:-1]]
    return int(fields[1]), int(fields[19]), int(uid.group(1)), argv, raw, cgroup


def find(gate_argv, container=None, proc=PROC, uid=None):
    """The one owned process waiting at `gate_argv`'s gate, frozen, or None while there is none
    yet. `container` is (full ID, init host PID) of the owned container: the process's cgroup
    must name that ID and its ancestry reach that init. More than one match refuses."""
    uid = os.geteuid() if uid is None else uid
    wanted = list(gate_argv[:4])
    names = [name for name in os.listdir(proc) if name.isdigit()]
    require(len(names) <= PROC_ENTRIES_MAX, "process table exceeds its bound")
    found = []
    for name in names:
        info = process(int(name), proc)
        if info is None or info[3][:4] != wanted:
            continue
        ppid, ticks, owner, argv, raw, cgroup = info
        require(owner == uid, "the gated process %s runs as uid %d, not this account" % (name, owner))
        if container is not None:
            identity, init = container
            require(identity in cgroup, "the gated process %s is outside container %s"
                    % (name, identity[:12]))
            parent, hops = ppid, 0
            while parent != init:
                hops += 1
                above = process(parent, proc) if parent > 1 else None
                require(above is not None and hops < ANCESTRY_MAX,
                        "the gated process %s does not descend from the container's init %d"
                        % (name, init))
                parent = above[0]
        found.append(Target(int(name), ticks, owner, sha256_bytes(raw),
                            container[0] if container else None,
                            container[1] if container else None))
    require(len(found) <= 1, "more than one process waits at one private gate")
    return found[0] if found else None


def unchanged(target, proc=PROC):
    """Whether `target` is still the very process that was frozen (no recycled PID)."""
    info = process(target.pid, proc)
    return (info is not None and info[1] == target.start_ticks and info[2] == target.uid
            and sha256_bytes(info[4]) == target.cmdline_sha256)


def inspect(docker, name, deadline, clock=time.monotonic):
    """(full container ID, its init's host PID) of the running container `name`, or None."""
    seconds = min(10.0, deadline - clock())
    if seconds <= 0:
        return None
    try:
        completed = subprocess.run([docker, "inspect", "--type", "container", "--format",
                                    "{{.Id}} {{.State.Pid}} {{.State.Running}}", name],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, timeout=seconds)
    except subprocess.TimeoutExpired:
        return None
    fields = completed.stdout.decode("ascii", "replace").split()
    if completed.returncode != 0 or len(fields) != 3 or fields[2] != "true":
        return None
    require(re.match(r"^[0-9a-f]{64}$", fields[0]) and fields[1].isdigit()
            and int(fields[1]) > 1, "docker inspect returned no container identity")
    return fields[0], int(fields[1])


def supervised_start(supervisor, env, cwd, clock=time.monotonic):
    """The production starter: perf as one request to the authority supervisor on the host."""
    def start(argv, seconds, deadline, file_bytes, cap=64 * 1024):
        return toolrun.start(supervisor, argv, env, seconds, cap, cwd=cwd,
                             deadline=deadline, clock=clock, file_bytes=file_bytes)
    return start


class Bounded(object):
    """One trusted operator-tool process of the pre-build capability check, which runs before
    any build exists and so before an authority-built supervisor exists: its own session,
    bounded by `seconds`, stdout and stderr in private files read within `cap`, and its whole
    process group ended and reaped, its end confirmed by the group being gone. It runs only
    the configured perf and the probe's own shell loop, never candidate code; every capture
    runs under the authority supervisor (`supervised_start`)."""

    def __init__(self, argv, env, cwd, seconds, cap, clock=time.monotonic):
        self.argv = list(argv)
        self.cap = cap
        self.clock = clock
        self.deadline = clock() + seconds
        self.code = None
        self.timed_out = False
        self.cancelled = False
        self.uncertain = False
        self.late = False
        self.stdout = self.stderr = b""
        self.supervised = None
        self.exceeded = None
        self._out = tempfile.TemporaryFile()
        self._err = tempfile.TemporaryFile()
        self.process = subprocess.Popen(self.argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                                        stdout=self._out, stderr=self._err,
                                        start_new_session=True)

    def done(self):
        return self.process.poll() is not None

    def _end_group(self, signum):
        try:
            os.killpg(self.process.pid, signum)
        except ProcessLookupError:
            pass
        except PermissionError:
            # Some kernels refuse to signal a group whose only member is the exited, unreaped
            # leader; a live member is still found by the survivor check after reaping.
            if self.process.poll() is None:
                raise

    def cancel(self):
        self.cancelled = True
        self._end_group(signal.SIGTERM)

    def wait(self, timeout=None):
        limit = self.deadline - self.clock() if timeout is None else min(
            timeout, self.deadline - self.clock())
        try:
            self.process.wait(max(0.0, limit))
        except subprocess.TimeoutExpired:
            if timeout is not None and self.clock() < self.deadline:
                return None
            self.timed_out = True
            self._end_group(signal.SIGKILL)
            try:
                self.process.wait(5)
            except subprocess.TimeoutExpired:
                self.uncertain = True
        self.code = self.process.returncode
        # Every descendant of its session must be gone too: a survivor is uncertain.
        try:
            os.killpg(self.process.pid, 0)
            self._end_group(signal.SIGKILL)
            self.uncertain = True
        except ProcessLookupError:
            pass
        except PermissionError:
            self.uncertain = True
        for stream, name in ((self._out, "stdout"), (self._err, "stderr")):
            stream.seek(0)
            setattr(self, name, stream.read(self.cap + 1))
            stream.close()
        return self

    def failure(self):
        if self.uncertain:
            return "%s: its end is not confirmed" % self.argv[0]
        if self.timed_out:
            return "%s timed out" % self.argv[0]
        if len(self.stdout) > self.cap:
            return "%s wrote more than %d bytes" % (self.argv[0], self.cap)
        if self.code != 0:
            tail = self.stderr.decode("utf-8", "replace").strip()[-512:]
            return "%s exited %s%s" % (self.argv[0], self.code, (": " + tail) if tail else "")
        return None


def bounded_start(env, cwd, clock=time.monotonic):
    """The starter of the pre-build capability check (`Bounded`)."""
    def start(argv, seconds, deadline, file_bytes, cap=64 * 1024):
        seconds = min(seconds, deadline - clock())
        if seconds <= 0:
            return toolrun.Result(list(argv), launch_error="no time is left", late=True)
        return Bounded(argv, env, cwd, seconds, cap, clock)
    return start


class Observer(object):
    """One host perf recording of one gated, owned process.

    `start(argv, seconds, deadline, file_bytes)` runs perf under the authority supervisor
    (`supervised_start`) and returns a `toolrun.Running` or a launch-refused `toolrun.Result`.
    `container()` names the owned container's (ID, init PID) once it runs, or None; it is
    None for a host process (the capability probe), whose boundary is its gate and account.
    """

    def __init__(self, start, perf, event, frequency, unwind, output, control_dir, gate,
                 deadline, whole, container=None, file_bytes=None, proc=PROC,
                 clock=time.monotonic):
        self.start = start
        self.perf = perf
        self.event = event
        self.frequency = frequency
        self.unwind = unwind
        self.output = output
        self.control_dir = control_dir
        self.gate = gate
        self.deadline = deadline
        self.whole = whole
        self.container = container
        self.file_bytes = file_bytes
        self.proc = proc
        self.clock = clock
        self.control = None
        self.running = None
        self.target = None
        self.identity = None
        self.attached_ms = None
        self.released = False
        self.result = None
        self.detail = ""

    def make_gate(self):
        os.mkfifo(self.gate, 0o600)

    def attach(self, alive):
        """Find and freeze the gated target, attach perf, confirm its acknowledgement and the
        unchanged target, then open the gate. `alive()` is whether the contained run still
        runs; a run that ended first refuses. Raises CaptureError; `stop` settles perf."""
        began = self.clock()
        until = min(self.deadline, began + FIND_SECONDS)
        gate_argv = [SHELL, "-c", GATE, self.gate]
        while self.target is None:
            if not alive():
                raise CaptureError("observer_target_missing", "the contained run ended before "
                                   "its gated process was observed")
            if self.clock() >= until:
                raise CaptureError("observer_target_missing", "no owned process waited at "
                                   "the gate within %.0f s" % (until - began))
            if self.container is not None and self.identity is None:
                self.identity = self.container()
            if self.container is None or self.identity is not None:
                try:
                    self.target = find(gate_argv, self.identity, self.proc)
                except ContractError as error:
                    raise CaptureError("observer_target_refused", str(error))
                except OSError as error:
                    raise CaptureError("observer_unavailable", "cannot read the process table "
                                       "%s: %s" % (self.proc, error))
            if self.target is None:
                time.sleep(POLL_SECONDS)
        self.control = perfcapture.PerfControl(self.control_dir)
        argv = attach_argv(self.perf, self.event, self.frequency, self.unwind, self.output,
                           self.control, self.target.pid, self.file_bytes)
        seconds = max(0.0, self.deadline - self.clock())
        running = self.start(argv, seconds, self.deadline, self.file_bytes)
        if isinstance(running, toolrun.Result):
            raise CaptureError("observer_refused", "host perf did not start: %s"
                               % running.failure())
        self.running = running
        ack = min(self.deadline, self.clock() + ACK_SECONDS)
        if self.whole:
            self.send(b"enable\n", ack)
        else:
            # Disabled until the region: one acknowledged round trip proves perf attached.
            self.send(b"disable\n", ack)
        if not unchanged(self.target, self.proc):
            raise CaptureError("observer_target_changed", "process %d is no longer the frozen "
                               "gated process (exited or a recycled PID); nothing is "
                               "attributed to it" % self.target.pid)
        self.attached_ms = int((self.clock() - began) * 1000)
        self.release()

    def send(self, command, deadline):
        """Send one control command and wait for perf's acknowledgement, in short slices so
        a perf that ended (refused its events) is reported at once, not at the deadline."""
        if command not in perfcapture.CONTROL_COMMANDS:
            raise ValueError(command)
        try:
            os.write(self.control.control, command)
        except OSError as error:
            raise CaptureError("control_closed", "cannot send %r to perf: %s" % (command, error))
        while True:
            if self.running is not None and self.running.done():
                raise CaptureError("observer_exited", "host perf ended before %r was "
                                   "acknowledged" % command)
            try:
                held = perfcapture.read_ack(self.control.ack, min(deadline, self.clock() + 0.05),
                                            self.clock)
            except CaptureError as error:
                if error.reason != "control_timeout" or self.clock() >= deadline:
                    raise
                continue
            self.control.acknowledgements.append(held)
            return held

    def release(self):
        """Open the gate: the command is executed in the attached process."""
        try:
            fd = os.open(self.gate, os.O_WRONLY | os.O_NONBLOCK)
        except OSError as error:
            raise CaptureError("observer_gate", "cannot open the gate %s: %s" % (self.gate, error))
        try:
            os.write(fd, b"go\n")
        finally:
            os.close(fd)
        self.released = True

    def stop(self, target_ended=True):
        """Settle perf: after its target ended it leaves of its own accord within
        `STOP_SECONDS`; otherwise (or when the target did not end) it is cancelled through its
        supervisor. Returns the Result; raises `cleanup_uncertain` when its end is unconfirmed."""
        try:
            if self.running is None:
                return None
            if target_ended:
                self.result = self.running.wait(max(0.0, min(STOP_SECONDS,
                                                             self.deadline - self.clock())))
            if self.result is None:
                self.running.cancel()
                self.result = self.running.wait()
            if self.result.uncertain:
                raise CaptureError("cleanup_uncertain", "the host observer's end is not "
                                   "confirmed: %s" % self.result.failure())
            return self.result
        finally:
            if self.control is not None:
                self.control.close()
            for path in (self.gate,):
                if os.path.lexists(path):
                    os.unlink(path)

    def failure(self):
        """Why the recording cannot be used, or None (after `stop`)."""
        if self.result is None:
            return "host perf was never started"
        return self.result.failure()

    def document(self, perf_sha256, region):
        """The `af.perf-host-observer/1` record kept beside the capture."""
        result = self.result
        return {"schema": SCHEMA, "observer": "host_perf", "perf_sha256": perf_sha256,
                "event": self.event, "region": region,
                "target": None if self.target is None else self.target.record(),
                "attached_before_release": self.attached_ms is not None and self.released,
                "attach_ms": self.attached_ms,
                "exit_code": None if result is None else result.code,
                "stopped_confirmed": result is not None and not result.uncertain,
                "detail": (self.detail or (result.failure() if result is not None
                                           and result.failure() else ""))[:4096]}


def probe(start, perf, event, frequency, workdir, deadline, clock=time.monotonic, proc=PROC):
    """The actual host-observer capability: attach this host's perf to an owned process this
    account started behind the same gate (a bounded shell loop), record it whole and decode at
    least one `event` sample. Returns (available, reason, detail). No container is needed: the
    attach and decode path is the one a contained target gets; Docker is checked by the run."""
    gate = os.path.join(workdir, "observer-gate")
    data = os.path.join(workdir, "observer.data")
    observer = Observer(start, perf, event, frequency, "frame_pointer", data, workdir, gate,
                        deadline, True, proc=proc, clock=clock)
    observer.make_gate()
    loop = gated(["/bin/sh", "-c", "i=0; while [ $i -lt 400000 ]; do i=$((i+1)); done"], gate)
    # The owned target is itself one supervised request, never a bare child of the driver.
    target = start(loop, max(0.0, deadline - clock()), deadline, None)
    if isinstance(target, toolrun.Result):
        return False, "observer_refused", "the owned probe loop did not start: %s" % (
            target.failure())
    ended = refusal = None
    try:
        observer.attach(lambda: not target.done())
        ended = target.wait(max(0.0, deadline - clock()))
        if ended is None:
            refusal = ("observer_timeout", "the owned probe loop outlived its deadline")
    except CaptureError as error:
        refusal = (error.reason, error.detail)
    finally:
        if ended is None:
            # A target still waiting at its gate, or over its deadline, is cancelled through
            # its own runner and its end settled before anything is reported.
            target.cancel()
            ended = target.wait()
    try:
        observer.stop(refusal is None)
    except CaptureError as error:
        refusal = (error.reason, error.detail)
    if ended is None or ended.uncertain:
        return False, "cleanup_uncertain", "the owned probe loop's end is not confirmed"
    if refusal is not None:
        return (False,) + refusal
    if observer.failure() is not None:
        return False, "perf_refused", "host perf record -p: " + observer.failure()
    script = start([perf, "script", "-i", data, "-F", perfcapture.SCRIPT_FIELDS],
                   max(0.0, deadline - clock()), deadline, None,
                   perfcapture.CAPABILITY_SCRIPT_BYTES)
    result = script if isinstance(script, toolrun.Result) else script.wait()
    if result.failure() is not None:
        return False, "perf_script_failed", result.failure()
    try:
        stats = perfcapture.parse_script(result.stdout.decode("utf-8", "replace"))
        if stats.samples == 0:
            return False, "event_unsupported", ("host perf attached but recorded no %s sample "
                                                "of the owned process" % event)
        perfcapture.check_event(stats, event)
    except CaptureError as error:
        return False, error.reason, error.detail
    return True, None, ("%d %s samples of an owned gated process decoded by the host observer"
                        % (stats.samples, event))
