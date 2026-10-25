"""Where one telemetry experiment script runs, and what bounds it (ADR-0215).

A telemetry experiment script runs one side's AF binary, which may be a pull request's
candidate: it is untrusted code. Each execution (a `--version` query, the corpus generator, every
bench script run) goes through one `Executor`, the same for both sides of a result:

- **`Contained`** (`linux_container`, the only mode a pull request is measured in) runs it in
  its own Linux container of the pinned sample image: network none, a read-only root, every
  capability dropped, no new privileges, the declared PID, memory (swap disabled) and CPU set,
  a file size limit, the container user, a private cgroup and IPC namespace, and
  `docker-init` as PID 1. Bound read-only at their own paths are only the measurement
  authority's inputs and this side's binary; the only writable bind is the run's own new
  directory on the verified disk-bounded scratch filesystem (`supervise.TELEMETRY_DIRECTORY`),
  checked before and after every execution with `supervise.prepare_scratch` and
  `supervise.scratch_problems`. The controller, its result directory, the other side's binary,
  the master corpus and every credential are never bound. Every process of the execution lives
  in the container's PID namespace and ends with it: the container is removed once, however
  the execution ended, by `supervise.remove_container`, and an unconfirmed removal or a scratch
  that is not as it was is an uncertain cleanup.
- **`TrustedLocal`** (`trusted_local`) runs it on this host in its own session, for source the
  operator trusts. It is recorded as such: nothing confines what it writes or reaches, and a
  process that leaves its session cannot be observed. A process of its group that outlives the
  script fails the execution, and is killed.

The shared `perf-supervisor` is not the PID 1 here: its request bounds a child at the verdict
sample's 120 seconds (`limits.SAMPLE_TIMEOUT_SECONDS_MAX`), and a bench script runs for minutes.
Raising that protected bound would change the verdict path; the container's PID namespace is
the boundary that ends every descendant instead.

Both modes share one discipline. The script's stdout and stderr go to two new files of the
controller's capture directory, which the execution cannot reach when contained; no pipe is
drained. While it runs, one loop on the caller's thread checks the caller's one monotonic
deadline and, about once a second, everything the execution may write (its writable state and
both capture files) against the room the caller gives it; either ends the execution. Each file
it writes is bounded by `RLIMIT_FSIZE`. Nothing here retries.
"""
import errno
import os
import resource
import select
import secrets
import signal
import subprocess
import sys
import time

from . import builds, supervise
from .values import ContractError, require

POLL_SECONDS = 0.05
ROOM_SECONDS = 1.0
KILL_GRACE_SECONDS = 10
# Bound a complete Linux process scan, including each status read. An exceeded bound is
# unknown evidence, never an empty group.
PROCESS_ENTRIES_MAX = 65536
PROCESS_STAT_BYTES_MAX = 4096
# A script's document is at most this; a larger stdout is never parsed as a document.
DOCUMENT_BYTES_MAX = 16 * 1024 * 1024
# The retained tail of a script's stderr; the whole of it may hold at most STDERR_FILE_MAX.
LOG_BYTES_MAX = 256 * 1024
STDERR_FILE_MAX = 16 * 1024 * 1024
CONTAINER_PYTHON = "/usr/bin/python3"
CONTAINER_PATH = "/usr/local/bin:/usr/bin:/bin"
MODES = ("trusted_local", "linux_container")
TRUSTED_LOCAL_LIMITATIONS = (
    "trusted local mode: the scripts and both binaries ran on this host as the operator's "
    "user; nothing confined what they wrote or reached, and a process that left its session "
    "could not be observed. Only source the operator trusts is measured this way.")
CONTAINED_LIMITATIONS = (
    "each execution ran in its own container of the pinned image with network none, a "
    "read-only root and only its own scratch directory writable; the limits are the ones the "
    "container was started with, not observed in its cgroup. Within one execution the script "
    "and the binary it measures share one user, so a binary can still falsify its own run's "
    "document, never the other side's or the controller's.")


class Outcome(object):
    """How one execution ended. `cleanup` is `confirmed` when nothing it started can still run
    and its writable state is as the caller expects, `uncertain` otherwise (the caller keeps
    everything the execution could reach). `lifecycle` says why a clean exit is still not
    usable (a process outlived it); `exceeded` why it was stopped for its room."""

    def __init__(self, code=None, stdout=b"", stdout_bytes=0, stderr=b"", stderr_bytes=0,
                 wall_ns=0, timed_out=False, error=None, exceeded=None, lifecycle=None,
                 cleanup="confirmed", cleanup_detail=""):
        self.code = code
        self.stdout = stdout
        self.stdout_bytes = stdout_bytes
        self.stderr = stderr
        self.stderr_bytes = stderr_bytes
        self.wall_ns = wall_ns
        self.timed_out = timed_out
        self.error = error
        self.exceeded = exceeded
        self.lifecycle = lifecycle
        self.cleanup = cleanup
        self.cleanup_detail = cleanup_detail

    @property
    def stdout_overflow(self):
        return self.stdout_bytes > DOCUMENT_BYTES_MAX

    def failure(self):
        """Why nothing of this execution's stdout may be admitted, its exit code aside."""
        if self.error is not None:
            return "could not start: %s" % self.error
        if self.cleanup != "confirmed":
            return "its cleanup was not confirmed: %s" % self.cleanup_detail
        if self.exceeded is not None:
            return self.exceeded
        if self.timed_out:
            return "it did not end before the run's deadline and was stopped"
        if self.lifecycle is not None:
            return self.lifecycle
        if self.stdout_overflow:
            return "its stdout held %d bytes, over the %d-byte document bound" % (
                self.stdout_bytes, DOCUMENT_BYTES_MAX)
        return None


def allocated(paths):
    """Bytes allocated below `paths` (each a file or a directory), by lstat; nothing is
    followed and a path that vanished counts nothing."""
    total = 0
    for root in paths:
        try:
            info = os.lstat(root)
        except OSError:
            continue
        total += getattr(info, "st_blocks", 0) * 512 or info.st_size
        if not os.path.isdir(root) or os.path.islink(root):
            continue
        for directory, subdirs, files in os.walk(root):
            for name in files + subdirs:
                try:
                    entry = os.lstat(os.path.join(directory, name))
                except OSError:
                    continue
                total += getattr(entry, "st_blocks", 0) * 512 or entry.st_size
    return total


def _limit_files(file_bytes):
    def apply():
        os.setsid()
        resource.setrlimit(resource.RLIMIT_FSIZE, (file_bytes, file_bytes))
    return apply


def _read_capture(path, head, limit):
    """(retained bytes, total size) of a capture file: its first `limit` bytes, or with
    `head` false its last."""
    try:
        size = os.lstat(path).st_size
        with open(path, "rb") as stream:
            if not head and size > limit:
                stream.seek(size - limit)
            return stream.read(limit), size
    except OSError:
        return b"", 0


class Execution(object):
    """What one execution needs: its argv (the program as the execution sees it), its
    environment and working directory, the paths it may write (`writable`, its own new
    directories), the paths it may only read (`readable`), the capture directory its
    stdout and stderr go to (the controller's own), and its room."""

    def __init__(self, argv, env, cwd, writable, readable, capture, label, room_bytes,
                 deadline, file_bytes):
        self.argv = list(argv)
        self.env = dict(env)
        self.cwd = cwd
        self.writable = list(writable)
        self.readable = list(readable)
        self.capture = capture
        self.label = label
        self.room_bytes = room_bytes
        self.deadline = deadline
        self.file_bytes = file_bytes

    def outputs(self):
        return (os.path.join(self.capture, self.label + ".stdout"),
                os.path.join(self.capture, self.label + ".stderr"))


class Executor(object):
    """The one way a result's executions run; `TrustedLocal` and `Contained` below."""

    mode = None
    containment = None
    python = None
    limitations = ""

    def __init__(self, clock=time.monotonic):
        self.clock = clock

    def record(self):
        return None

    def prepare(self, execution):
        """Check this executor can run `execution` now; a reason it cannot, or None."""
        return None

    def launch(self, execution, stdout, stderr):
        raise NotImplementedError

    def stop(self, process, execution):
        raise NotImplementedError

    def finish(self, process, execution, stopped):
        """(lifecycle problem or None, cleanup state, cleanup detail) after the process
        ended."""
        raise NotImplementedError

    def run(self, execution):
        """Run `execution` once within its deadline and room; its Outcome."""
        stdout_path, stderr_path = execution.outputs()
        if execution.deadline - self.clock() <= 0:
            return Outcome(error="the run's deadline leaves no time for it")
        refused = self.prepare(execution)
        if refused is not None:
            return Outcome(error=refused)
        started = self.clock()
        stdout = stderr = None
        try:
            stdout = open(stdout_path, "xb")
            stderr = open(stderr_path, "xb")
            process = self.launch(execution, stdout, stderr)
        except (OSError, ContractError) as error:
            return Outcome(error=str(error)[:1024])
        finally:
            for stream in (stdout, stderr):
                if stream is not None:
                    stream.close()
        try:
            timed_out, exceeded = False, None
            watched = execution.writable + [stdout_path, stderr_path]
            checked = started
            while True:
                remaining = execution.deadline - self.clock()
                if remaining <= 0:
                    timed_out = True
                    self.stop(process, execution)
                    break
                try:
                    process.wait(timeout=min(POLL_SECONDS, remaining))
                    if self.clock() >= execution.deadline:
                        timed_out = True
                        self.stop(process, execution)
                    break
                except subprocess.TimeoutExpired:
                    pass
                except OSError:
                    if not isinstance(process, _LocalProcess):
                        raise
                    # TrustedLocal keeps the observation error and its ownership status. End
                    # the owned group, but never turn a failed observation into a clean run.
                    self.stop(process, execution)
                    break
                now = self.clock()
                if now >= execution.deadline:
                    timed_out = True
                elif now - checked >= ROOM_SECONDS:
                    checked = now
                    exceeded = self._exceeded(execution, watched, stdout_path, stderr_path)
                if timed_out or exceeded is not None:
                    self.stop(process, execution)
                    break
            if exceeded is None and not timed_out:
                # What it wrote in its last second is held to the same room.
                exceeded = self._exceeded(execution, watched, stdout_path, stderr_path)
            wall_ns = int((self.clock() - started) * 1e9)
        except BaseException:
            # Cancellation or an error in the active room check must still close the owned
            # execution. The original exception propagates; no document is returned.
            self.stop(process, execution)
            self.finish(process, execution, True)
            raise
        lifecycle, cleanup, detail = self.finish(process, execution, timed_out or exceeded)
        out, out_bytes = _read_capture(stdout_path, True, DOCUMENT_BYTES_MAX)
        err, err_bytes = _read_capture(stderr_path, False, LOG_BYTES_MAX)
        code = process.returncode
        return Outcome(code=code if not (timed_out or exceeded) else None, stdout=out,
                       stdout_bytes=out_bytes, stderr=err, stderr_bytes=err_bytes,
                       wall_ns=wall_ns, timed_out=timed_out, exceeded=exceeded,
                       lifecycle=lifecycle, cleanup=cleanup, cleanup_detail=detail)

    def _exceeded(self, execution, watched, stdout_path, stderr_path):
        used = allocated(watched)
        if used > execution.room_bytes:
            return ("it wrote %d bytes, over the %d bytes of room its profile gives it; it was "
                    "stopped while writing" % (used, execution.room_bytes))
        if allocated([stdout_path]) > DOCUMENT_BYTES_MAX + 65536:
            return "its stdout outgrew the %d-byte document bound" % DOCUMENT_BYTES_MAX
        if allocated([stderr_path]) > STDERR_FILE_MAX + 65536:
            return "its stderr outgrew %d bytes" % STDERR_FILE_MAX
        return None


class TrustedLocal(Executor):
    """This host, the execution in its own session (see the module's limits)."""

    mode = "trusted_local"
    containment = "process_group"
    limitations = TRUSTED_LOCAL_LIMITATIONS

    def __init__(self, python, clock=time.monotonic):
        Executor.__init__(self, clock)
        self.python = python

    def launch(self, execution, stdout, stderr):
        return _LocalProcess(subprocess.Popen(
            execution.argv, cwd=execution.cwd, env=execution.env, stdin=subprocess.DEVNULL,
            stdout=stdout, stderr=stderr, preexec_fn=_limit_files(execution.file_bytes),
            close_fds=True))

    def prepare(self, execution):
        if sys.platform != "linux" and sys.platform != "darwin":
            return "trusted local lifecycle observation supports only Linux and macOS"
        return None

    def stop(self, process, execution):
        for sent in (signal.SIGTERM, signal.SIGKILL):
            process.signal_group(sent)
            try:
                process.wait(timeout=KILL_GRACE_SECONDS)
                return
            except subprocess.TimeoutExpired:
                continue
            except OSError:
                # No polling reap here: the leader reserves the group through finish.
                continue

    def finish(self, process, execution, stopped):
        lifecycle = None
        until = time.monotonic() + KILL_GRACE_SECONDS
        notes = []
        try:
            if not stopped and _group_alive(process.pid, until):
                lifecycle = ("a process it started outlived it in its process group; it was "
                             "killed, and nothing of the run is admitted")
        except (OSError, ValueError) as error:
            notes.append(str(error))
        # As in review_process::sample, keep the leader unreaped while repeatedly killing
        # and scanning: a child forked during one group signal may need the next signal.
        gone = False
        while process.owned:
            process.signal_group(signal.SIGKILL)
            try:
                process.wait(timeout=min(POLL_SECONDS, max(0, until - time.monotonic())))
            except (subprocess.TimeoutExpired, OSError):
                pass
            try:
                gone = not _group_alive(process.pid, until)
            except (OSError, ValueError) as error:
                if not notes:
                    notes.append(str(error))
            if gone or time.monotonic() >= until:
                break
            time.sleep(min(POLL_SECONDS, max(0, until - time.monotonic())))
        # This is the only reap. No signal or scan of the group follows it.
        process.reap()
        if process.problem:
            notes.insert(0, process.problem)
        if not gone:
            notes.append("a live group member remains or its end could not be established")
        if process.returncode is None:
            notes.append("the script's exit could not be reaped")
        if notes:
            return lifecycle, "uncertain", "; ".join(notes)[:4096]
        return lifecycle, "confirmed", (
            "no executing process of its group remains (zombies cannot execute or hold open "
            "files); a process that left its session cannot be observed in trusted local mode")


class _LocalProcess(object):
    """Reserve the leader until cleanup, following review-process's unreaped-exit discipline.

    This file-backed, long-duration executor cannot use the verdict supervisor's 120-second
    request. Use the same OS observations as its shared leaf: waitid(WNOWAIT) on Linux and
    kqueue NOTE_EXIT on macOS. Never use Popen.poll/wait while a group may still be signalled.
    """

    def __init__(self, child):
        self.child, self.pid = child, child.pid
        self.owned, self.exited, self.problem = True, False, None
        self.queue = None

    @property
    def returncode(self):
        return self.child.returncode

    def wait(self, timeout):
        until = time.monotonic() + timeout
        while not self.exited:
            try:
                if sys.platform == "linux":
                    self.exited = os.waitid(os.P_PID, self.pid,
                                            os.WEXITED | os.WNOWAIT | os.WNOHANG) is not None
                else:
                    if self.queue is None:
                        self.queue = select.kqueue()
                    event = select.kevent(self.pid, filter=select.KQ_FILTER_PROC,
                                         flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                         fflags=select.KQ_NOTE_EXIT)
                    events = self.queue.control([event], 1, 0)
                    if events and events[0].flags & select.KQ_EV_ERROR:
                        raise OSError(events[0].data, "kqueue NOTE_EXIT")
                    self.exited = bool(events)
            except OSError as error:
                if sys.platform == "darwin" and error.errno == errno.ESRCH:
                    # An attach to our unreaped, already exited child (shared leaf semantics).
                    self.exited = True
                else:
                    self.problem = self.problem or "observing the unreaped script failed: %s" % error
                    if error.errno == errno.ECHILD:
                        self.owned = False
                    raise
            if self.exited:
                return
            if time.monotonic() >= until:
                raise subprocess.TimeoutExpired(self.child.args, timeout)
            time.sleep(min(POLL_SECONDS, max(0, until - time.monotonic())))

    def signal_group(self, sent):
        if not self.owned:
            return
        try:
            os.killpg(self.pid, sent)
        except OSError as error:
            if error.errno == errno.ESRCH or (sys.platform == "darwin"
                                            and error.errno == errno.EPERM):
                return
            self.problem = self.problem or "signalling the owned process group failed: %s" % error

    def reap(self):
        # Nonblocking even when exit observation failed. The group reservation ends here.
        try:
            if self.owned:
                self.child.wait(timeout=0)
        except (subprocess.TimeoutExpired, OSError) as error:
            self.problem = self.problem or "reaping the script failed: %s" % error
        self.owned = False
        if self.queue is not None:
            self.queue.close()


def _group_alive(pgid, deadline):
    """Any executing group member, or an error for unknown evidence; leader still reserved.

    Follow review_process::sample's process-group evidence: Linux excludes terminal tasks
    Z/X/x only after also checking the thread count in that same stat read; macOS excludes
    zombie-only groups. Linux scans every numeric entry,
    fails closed on enumeration/read/parse/bound errors and tolerates only vanished entries.
    """
    if sys.platform == "linux":
        live, leader = False, False
        with os.scandir("/proc") as entries:
            for count, entry in enumerate(entries):
                if count >= PROCESS_ENTRIES_MAX or time.monotonic() >= deadline:
                    raise ValueError("process scan exceeded its entry or time bound")
                if not entry.name.isascii() or not entry.name.isdecimal():
                    continue
                try:
                    with open(os.path.join("/proc", entry.name, "stat"), "rb") as stream:
                        raw = stream.read(PROCESS_STAT_BYTES_MAX + 1)
                except OSError as error:
                    if error.errno in (errno.ENOENT, errno.ESRCH):
                        continue
                    raise
                try:
                    prefix, rest = raw.rsplit(b")", 1)
                    fields = rest.split()
                    state, parent, group, session = fields[:4]
                    pid = int(prefix.split(b" ", 1)[0])
                    parent, group, session = int(parent), int(group), int(session)
                    # stat field 20 (index 17 after comm) counts the whole thread group.
                    # A zombie leader can still have executing threads, so its state alone
                    # cannot establish that the process is terminal. One read binds both.
                    threads = int(fields[17])
                    if (len(raw) > PROCESS_STAT_BYTES_MAX or len(fields) < 20
                            or not raw.endswith(b"\n") or not fields[19].isdigit()
                            or pid != int(entry.name) or threads < 1
                            or min(parent, group, session) < 0
                            or not prefix.startswith(str(pid).encode("ascii") + b" (")
                            or state not in (b"R", b"S", b"D", b"Z", b"T", b"t", b"X",
                                             b"x", b"K", b"W", b"P", b"I", b"N")):
                        raise ValueError()
                except (ValueError, IndexError):
                    raise ValueError("unreadable process status for pid %s" % entry.name)
                if pid == pgid:
                    if parent != os.getpid() or group != pgid or session != pgid:
                        raise ValueError("the reserved group leader's ownership changed")
                    leader = True
                if group == pgid and (state not in (b"Z", b"X", b"x") or threads > 1):
                    live = True
        if time.monotonic() >= deadline:
            raise ValueError("process scan exceeded its time bound")
        if not leader:
            raise ValueError("the reserved group leader is absent from the process scan")
        return live
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        if sys.platform == "darwin":
            return False
        raise
    return True


class Contained(Executor):
    """One Linux container of the pinned image per execution (see the module)."""

    mode = "linux_container"
    containment = "pid_namespace"
    python = CONTAINER_PYTHON
    limitations = CONTAINED_LIMITATIONS

    def __init__(self, docker, image, environment, scratch_root, uid, gid, cpus, label,
                 runtime, clock=time.monotonic):
        Executor.__init__(self, clock)
        require(builds.pinned_image(image.reference), "the sample image is pinned by digest")
        self.docker = docker
        self.image = image
        self.environment = environment
        self.scratch_root = os.path.realpath(scratch_root)
        self.uid = uid
        self.gid = gid
        self.cpus = list(cpus)
        self.label = label
        self.runtime = runtime
        self.names = 0
        self.inventory = None
        self.current = None

    @property
    def directory(self):
        """The result's own root entry of the scratch filesystem: every writable path."""
        return os.path.join(self.scratch_root, supervise.TELEMETRY_DIRECTORY)

    def record(self):
        return {"image": self.image.record(), "runtime": self.runtime[:256],
                "cpus": list(self.cpus),
                "memory_limit_bytes": self.environment["memory_limit_bytes"],
                "pid_limit": self.environment["pid_limit"],
                "scratch_limit_bytes": self.environment["scratch_limit_bytes"]}

    def argv(self, name, execution):
        """`docker run` of one execution; refused unless every writable path lies in this
        result's own directory of the verified scratch filesystem."""
        environment = self.environment
        memory = environment["memory_limit_bytes"]
        argv = [self.docker, "run", "--name", name, "--pull", "never", "--init",
                "--network", "none", "--read-only", "--cap-drop", "ALL",
                "--security-opt", "no-new-privileges",
                "--pids-limit", str(environment["pid_limit"]),
                "--memory", str(memory), "--memory-swap", str(memory),
                "--cpuset-cpus", ",".join(str(cpu) for cpu in self.cpus),
                "--user", "%d:%d" % (self.uid, self.gid), "--cgroupns", "private",
                "--ipc", "private",
                "--ulimit", "fsize=%d:%d" % (execution.file_bytes, execution.file_bytes),
                "--workdir", execution.cwd]
        own = self.directory + os.sep
        binds = sorted(set([(path, True) for path in execution.writable]
                           + [(path, False) for path in execution.readable]))
        for path, writable in binds:
            require(os.path.isabs(path) and os.path.normpath(path) == path and "," not in path
                    and "=" not in path, "a bind is a plain absolute path: %s" % path)
            if writable:
                require(path.startswith(own), "a writable bind lies in the result's own "
                        "directory of the verified scratch filesystem: %s" % path)
            else:
                require(not path.startswith(own) or path == execution.cwd,
                        "a read-only bind lies outside the writable scratch: %s" % path)
            argv += ["--mount", "type=bind,src=%s,dst=%s%s" % (
                path, path, "" if writable else ",readonly")]
        require(execution.cwd in execution.writable, "the working directory is writable state")
        argv += ["--entrypoint", "/usr/bin/env", self.image.reference, "-i"]
        argv += ["%s=%s" % item for item in sorted(execution.env.items())]
        return argv + execution.argv

    def prepare(self, execution):
        try:
            self.inventory = supervise.prepare_scratch(
                self.scratch_root, self.uid, self.gid, extra=(supervise.TELEMETRY_DIRECTORY,))
        except (ContractError, OSError) as error:
            return "the scratch filesystem is not ready: %s" % error
        return None

    def launch(self, execution, stdout, stderr):
        self.names += 1
        name = "af-perf-telemetry-%s-%d-%s" % (self.label[-16:].lower().replace("_", "-"),
                                               self.names, secrets.token_hex(4))
        argv = self.argv(name, execution)
        self.current = name
        return subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                                env=builds.docker_client_environment(),
                                preexec_fn=_limit_files(execution.file_bytes), close_fds=True)

    def _remove(self, execution):
        # The container's one removal; it ends every process in its PID namespace.
        deadline = max(execution.deadline, self.clock()) + supervise.REMOVAL_SECONDS
        return supervise.remove_container(self.docker, self.current, deadline, self.clock)

    def stop(self, process, execution):
        self.removal = self._remove(execution)
        try:
            process.wait(timeout=KILL_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            process.kill()
            try:
                process.wait(timeout=KILL_GRACE_SECONDS)
            except subprocess.TimeoutExpired:
                pass

    removal = None

    def finish(self, process, execution, stopped):
        removal = self.removal if stopped and self.removal else self._remove(execution)
        self.removal = None
        if process.returncode is None:
            process.kill()
            try:
                process.wait(timeout=KILL_GRACE_SECONDS)
            except subprocess.TimeoutExpired:
                pass
        problems = []
        if not removal.endswith(" removed"):
            problems.append(removal)
        if process.returncode is None:
            problems.append("the container client did not end")
        if self.inventory is not None:
            problems += supervise.scratch_problems(self.scratch_root, self.inventory)
        if problems:
            return None, "uncertain", "; ".join(problems)[:4096]
        return None, "confirmed", removal
