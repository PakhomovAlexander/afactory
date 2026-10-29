#!/usr/bin/env python3
"""Measure bare af from launch to the first browser status-line frame (ADR-0260).

`python3 -I tui-open.py AF [ARG...]` runs the argv in a 100x30 pseudo-terminal as the leader of
a new session (the browser needs a controlling terminal), and prints one
`af.perf-tui-open/1` line: `splash_ms`, launch to the first byte of any output, and
`browser_ms`, launch to the read that completes the last-row `NORMAL` status line of the screen
after its last clear. A pseudo-terminal carries no write boundaries, so the instant the status
line reaches the terminal is the earliest the browser frame is shown on it; a status line that
a clear erased never counts, whether the clear came before it or after it: the terminal is
read through the browser's exit, a clear after the line that nothing repaints refuses the
sample, and a line repainted after a clear is the frame. Every wait of the cleanup reads the
terminal too, the reap included, since on macOS an exiting session leader's close of its
terminal waits until its pending output is read.

The session is this driver's to end. The child holds a duplicate of the driver's stderr, the
supervisor's pipe, so a process of the session that keeps it cannot outlive the driver unseen by
the supervisor.
The child's exit is observed without reaping it, so its process-group id names only the session
while every member left is killed; the child is reaped only after that, and a member that
outlived the browser fails the sample. A process of the browser's doing that left its group,
by `setsid`, and closed every descriptor it inherited is found all the same: the browser runs
with a token of this run in `AF_PERF_TUI_OPEN_SESSION`, and once its session is settled every
live process whose environment carries the token (`/proc` on Linux, `ps -E` elsewhere) is
killed and fails the sample; one that also replaced its environment before `exec` is beyond
the driver, as beyond any process-group sample. On Linux a process of this user's whose
environment cannot be read, one that made itself undumpable, and that started no earlier than
the browser leaves the listing unconfirmed, as does a `/proc` that cannot be listed or a
process whose `status` or `stat` cannot be read; one that has exited or given up its address
space, awaiting its parent's reap (Linux owns its environment as root's then), runs nothing
and is skipped. SIGTERM, SIGINT and SIGHUP, the supervisor's
cancellation, end the session the same way before the driver exits; one that lands once the
session is already being settled stays pending until that settlement is confirmed, and ends the
sample then. A cleanup that cannot be confirmed, a listing included, exits 3; a process it
could not find may hold nothing of the supervisor's, so that exit is the caller's to read as an
unconfirmed cleanup, whatever its pipes settled.
"""

import errno
import fcntl
from decimal import Decimal
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

SCHEMA = "af.perf-tui-open/1"
DEADLINE_SECONDS = 100
QUIT_SECONDS = 5
CLEANUP_SECONDS = 2
LISTING_SECONDS = 10
UNCONFIRMED_EXIT = 3
SESSION_VARIABLE = "AF_PERF_TUI_OPEN_SESSION"
PROC = "/proc"
# The `stat` states of a process that has exited: a zombie awaiting its parent's reap, or dead.
EXITED = (b"Z", b"X", b"x")
CLEAR = b"\x1b[2J"
STATUS = re.compile(rb"\x1b\[30;[0-9]+H(?:\x1b\[[0-9;]*m|[^\x1b])*?NORMAL")
SCREEN_MAX = 131072
SIGNALS = (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)


class Cancelled(Exception):
    """The supervisor cancelled the sample with this signal."""


class Unconfirmed(Exception):
    """A process of the session may still run."""


def cancelled(number, frame):
    raise Cancelled(number)


class Exit:
    """The child's exit, observed without reaping it: a kqueue NOTE_EXIT on macOS, waitid with
    WNOWAIT elsewhere."""

    def __init__(self, pid):
        self.pid = pid
        self.done = False
        self.queue = None
        if hasattr(select, "kqueue"):
            self.queue = select.kqueue()
            try:
                self.queue.control([select.kevent(pid, select.KQ_FILTER_PROC,
                                                  select.KQ_EV_ADD, select.KQ_NOTE_EXIT)], 0)
            except ProcessLookupError:
                self.done = True

    def seen(self):
        if not self.done:
            if self.queue is not None:
                self.done = bool(self.queue.control(None, 1, 0))
            else:
                self.done = os.waitid(os.P_PID, self.pid,
                                      os.WEXITED | os.WNOWAIT | os.WNOHANG) is not None
        return self.done


def gone(error):
    """Whether `error`, from reading a process's `/proc` entry, says the process is gone."""
    return isinstance(error, FileNotFoundError) or error.errno == errno.ESRCH


def proc_names(proc):
    """The process ids `proc` lists; a listing that fails leaves the cleanup unconfirmed."""
    try:
        return [name for name in os.listdir(proc) if name.isdigit()]
    except OSError as error:
        raise Unconfirmed("the process listing failed: %s" % error) from error


def live_members(group):
    """Whether a process of `group` other than its exited, unreaped leader still runs. Linux
    lists the group's live members (a `stat` that cannot be read, or a `/proc` that cannot be
    listed, leaves the cleanup unconfirmed); XNU refuses a signal to a group of zombies with
    EPERM."""
    if os.path.isdir(PROC + "/self/task"):
        for name in proc_names(PROC):
            try:
                with open("%s/%s/stat" % (PROC, name), "rb") as source:
                    stat = source.read()
            except OSError as error:
                if gone(error):
                    continue
                raise Unconfirmed("process %s cannot be read: %s" % (name, error)) from error
            fields = stat[stat.rindex(b")") + 2:].split()
            if int(fields[2]) == group and fields[0] not in EXITED:
                return True
        return False
    try:
        os.killpg(group, 0)
    except (ProcessLookupError, PermissionError):
        return False
    return True


def kill_group(group):
    try:
        os.killpg(group, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def settle(group, deadline, wait):
    """Kill the group's live members until none runs, or until `deadline`: whether none runs,
    and whether any ran. Its leader is unreaped, so the group's id names nothing else. The
    settlement's own probes say what ran: a member born between two listings, whose parent
    exited before the first read it, is one the kill finds, and it counts. `wait` is one step
    of waiting (`Drain.step`), reading the terminal on the way."""
    found = False
    while live_members(group):
        found = True
        if time.monotonic() >= deadline:
            return False, found
        kill_group(group)
        wait()
    return True, found


def proc_stat(name, proc):
    """The fields of process `name`'s `stat` under `proc` after its command name (its state
    first, its start in clock ticks since boot twentieth, its address space's size twenty-first),
    readable whatever the process did to itself; None once it is gone. A `stat` that cannot be
    read otherwise leaves the cleanup unconfirmed."""
    try:
        with open("%s/%s/stat" % (proc, name), "rb") as source:
            stat = source.read()
    except OSError as error:
        if gone(error):
            return None
        raise Unconfirmed("the stat of process %s cannot be read: %s" % (name, error)) from error
    return stat[stat.rindex(b")") + 2:].split()


def proc_runs(fields):
    """Whether a process whose `stat` fields these are still runs anything: one that has exited
    (a zombie awaiting its parent's reap) or given up its address space on its way out does
    not, and Linux owns its `environ` as root's from then on, so that it cannot be read."""
    return fields[0] not in EXITED and int(fields[20]) != 0


def proc_owned(name, proc):
    """Whether process `name` under `proc` runs as this user: its `status`, readable by anyone,
    names its uids. One that is gone is nobody's; one whose status cannot be read, or names no
    uid, leaves the cleanup unconfirmed."""
    try:
        with open("%s/%s/status" % (proc, name), "rb") as source:
            status = source.read()
    except OSError as error:
        if gone(error):
            return False
        raise Unconfirmed("the owner of process %s cannot be read: %s" % (name, error)) from error
    for line in status.splitlines():
        if line.startswith(b"Uid:"):
            return str(os.getuid()).encode("utf-8") in line.split()[1:]
    raise Unconfirmed("the owner of process %s is not named by its status" % name)


def proc_listed(marker, exclude, born, proc):
    """`listed` on Linux: the environments under `proc`. A process that is gone is skipped, and
    so is one whose environment another user's permissions keep, since the browser runs as
    this user, one of this user's that started before the browser did (`born`, its start in
    clock ticks; None when unknown), and one that runs nothing any more (`proc_runs`). One of
    this user's that started no earlier, still runs and whose environment cannot be read, as
    a process that made itself undumpable, may be the browser's doing: the listing is
    unconfirmed, as it is when `proc` cannot be listed or a process's owner or `stat` cannot be
    read."""
    found = set()
    for name in proc_names(proc):
        if int(name) in exclude:
            continue
        try:
            with open("%s/%s/environ" % (proc, name), "rb") as source:
                environment = source.read()
        except OSError as error:
            if gone(error) or not proc_owned(name, proc):
                continue
            fields = proc_stat(name, proc)
            if fields is None or not proc_runs(fields):
                continue
            if born is not None and int(fields[19]) < born:
                continue
            raise Unconfirmed("the environment of process %s, this user's, cannot be read"
                              % name) from error
        if marker in environment.split(b"\0"):
            found.add(int(name))
    return found


def listed(token, exclude, born):
    """The pids, `exclude` apart, of every live process whose environment carries `token` in
    `SESSION_VARIABLE`: the browser's doing, whatever session, group or descriptors it has now,
    unless it replaced its environment before `exec`. Linux reads `/proc` (`born`, the
    browser's start there); elsewhere `ps -E` lists the user's processes with their
    environments."""
    marker = "%s=%s" % (SESSION_VARIABLE, token)
    found = set()
    if os.path.isdir(PROC + "/self/task"):
        return proc_listed(marker.encode("utf-8"), exclude, born, PROC)
    try:
        listing = subprocess.run(["ps", "-E", "-ww", "-ax", "-o", "pid=", "-o", "command="],
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                 timeout=LISTING_SECONDS, check=True)
    except (OSError, subprocess.SubprocessError) as error:
        raise Unconfirmed("the process listing failed: %s" % error) from error
    for line in listing.stdout.decode("utf-8", "replace").splitlines():
        fields = line.split()
        if len(fields) > 1 and fields[0].isdigit() and marker in fields[1:]:
            if int(fields[0]) not in exclude:
                found.add(int(fields[0]))
    return found


def settle_listed(token, exclude, seconds, born, wait):
    """Kill every process `listed` names until none is: whether one was found, and whether none
    is left within `seconds` of the first kill. The listing has a bound of its own; `wait` is
    one step of waiting (`Drain.step`)."""
    found = False
    deadline = None
    while True:
        pids = listed(token, exclude, born)
        if not pids:
            return found, True
        found = True
        if deadline is None:
            deadline = time.monotonic() + seconds
        elif time.monotonic() >= deadline:
            return found, False
        for pid in pids:
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        wait()


def reap(child, wait, deadline):
    """Reap the exited leader: its status, or None when it is not reaped by `deadline`. The
    reap waits with `wait` (`Drain.step`): on macOS an exiting session leader's close of its
    terminal waits until its pending output is read, so a reap that only waited for the exit
    could wait for itself."""
    while True:
        pid, status = os.waitpid(child.pid, os.WNOHANG)
        if pid:
            break
        if time.monotonic() >= deadline:
            return None
        wait()
    child.returncode = os.waitstatus_to_exitcode(status) if hasattr(
        os, "waitstatus_to_exitcode") else (os.WEXITSTATUS(status) if os.WIFEXITED(status)
                                            else -os.WTERMSIG(status))
    return child.returncode


def end(child, watch, drain, token, deadline, born):
    """Kill the running session, observe its leader's exit, settle its group, reap the leader,
    and, whether or not the leader was reaped before, clear what left the group; whether every
    step was confirmed, the group's before `deadline` and what left it within CLEANUP_SECONDS
    of its first kill. `watch` is None when the leader's exit could not be watched (its
    `Exit` was refused), and a watch that fails here is given up the same way: the settlement
    observes the exit then, since the leader is a live member of its group until it exits,
    and the reap confirms it. Every wait reads the terminal (`drain`, a `Drain`): on macOS an
    exiting session leader's close of its terminal waits until its pending output is read."""
    if child.returncode is None:
        kill_group(child.pid)
        while watch is not None:
            try:
                if watch.seen():
                    break
            except OSError:
                watch = None  # the settlement observes the exit instead
                break
            if time.monotonic() >= deadline:
                return False
            drain.step()
        if not settle(child.pid, deadline, drain.step)[0]:
            return False
        if reap(child, drain.step, deadline) is None:
            return False
    return settle_listed(token, {child.pid}, CLEANUP_SECONDS, born, drain.step)[1]


def drain(master, screen, timeout):
    """Read what the terminal holds into `screen`; False once it ended."""
    ready, _, _ = select.select([master], [], [], timeout)
    if not ready:
        return True
    try:
        data = os.read(master, 65536)
    except OSError:
        return False
    if not data:
        return False
    screen.feed(data, time.monotonic_ns())
    return True


class Drain:
    """The terminal's reader through the cleanup: `step` is one short wait that reads what the
    terminal holds into `screen` while the terminal is open, and sleeps once it ended. On
    macOS a killed process's exit waits until its pending terminal output is read, so no wait
    of the cleanup leaves the terminal unread."""

    def __init__(self, master, screen):
        self.master = master
        self.screen = screen
        self.open = True

    def step(self):
        if self.open:
            self.open = drain(self.master, self.screen, 0.005)
        else:
            time.sleep(0.005)


def milliseconds(nanoseconds):
    value = str(Decimal(nanoseconds) / Decimal(1000000))
    return value.rstrip("0").rstrip(".") if "." in value else value


class Screen:
    """What the terminal shows since its last clear, and when: `first`, the arrival of the
    first byte of any output, and `browser`, the arrival of the read that completed the status
    line on the screen the last clear left, None while no line stands after the last clear. A
    status line a clear erased never counts, whether the clear came before it or in a later
    read; a line repainted after a clear is the frame, at its own arrival. A read that holds
    the splash and the frame at once gives both one instant, since nothing separates them on
    the terminal either."""

    def __init__(self):
        self.shown = b""
        self.first = None
        self.browser = None

    def feed(self, data, observed):
        """Take the bytes read at `observed`; whether the status line is on the screen now."""
        if self.first is None:
            self.first = observed
        self.shown += data
        clear = self.shown.rfind(CLEAR)
        if clear >= 0:
            self.shown = self.shown[clear + len(CLEAR):]
            self.browser = None
        self.shown = self.shown[-SCREEN_MAX:]
        if self.browser is None and STATUS.search(self.shown):
            self.browser = observed
        return self.browser is not None


def measure(argv):
    """The driver's document for one run of `argv`; it returns, or raises, with SIGTERM, SIGINT
    and SIGHUP blocked once the session's cleanup began."""
    if not argv:
        raise ValueError("expected child argv")
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    witness = os.dup(2)
    child = watch = born = None
    screen = Screen()
    reader = Drain(master, screen)
    settled = False
    token = os.urandom(16).hex()
    mask = signal.pthread_sigmask(signal.SIG_BLOCK, ())
    try:
        environment = dict(os.environ, TERM="xterm-256color")
        environment[SESSION_VARIABLE] = token

        def enter_session():
            for number in SIGNALS:
                signal.signal(number, signal.SIG_DFL)
            signal.pthread_sigmask(signal.SIG_UNBLOCK, SIGNALS)
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        # No cancellation lands between the spawn and the record of its pid.
        signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
        try:
            started = time.monotonic_ns()
            child = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave,
                                     env=environment, start_new_session=True, close_fds=True,
                                     pass_fds=(witness,), preexec_fn=enter_session)
            watch = Exit(child.pid)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        os.close(slave)
        slave = -1
        if os.path.isdir(PROC + "/self/task"):
            fields = proc_stat(child.pid, PROC)
            born = None if fields is None else int(fields[19])
        deadline = started + DEADLINE_SECONDS * 1000000000
        while screen.browser is None:
            remaining = (deadline - time.monotonic_ns()) / 1000000000
            if remaining <= 0:
                raise TimeoutError("browser status-line frame missing at 100-second deadline")
            ready, _, _ = select.select([master], [], [], min(remaining, 0.1))
            if not ready:
                if watch.seen():
                    raise RuntimeError("child exited before browser status-line frame")
                continue
            try:
                data = os.read(master, 65536)
            except OSError as error:
                raise RuntimeError("PTY closed before browser status-line frame") from error
            if not data:
                raise RuntimeError("PTY ended before browser status-line frame")
            screen.feed(data, time.monotonic_ns())
        try:
            os.write(master, b"q")
        except OSError:
            pass  # the browser is gone already: its exit is observed below
        quit_by = time.monotonic() + QUIT_SECONDS
        open_terminal = True
        while not watch.seen():
            if time.monotonic() >= quit_by:
                raise RuntimeError("child did not exit after q")
            if open_terminal:
                open_terminal = drain(master, screen, 0.02)
            else:
                time.sleep(0.005)
        # The session is ending: a cancellation from here on asks for the settlement that
        # follows, so it stays pending until that is confirmed, and ends the sample then.
        signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
        cleanup_by = time.monotonic() + CLEANUP_SECONDS
        reader.open = open_terminal
        settled_group, stray = settle(child.pid, cleanup_by, reader.step)
        if not settled_group:
            raise Unconfirmed("a process of the browser's session survived its kill")
        code = reap(child, reader.step, cleanup_by)
        if code is None:
            raise Unconfirmed("the browser was not reaped after its exit")
        escaped, cleared = settle_listed(token, {child.pid}, CLEANUP_SECONDS, born,
                                         reader.step)
        settled = True
        open_terminal = reader.open
        if not cleared:
            raise Unconfirmed("a process the browser started survived its kill")
        held_by = time.monotonic() + CLEANUP_SECONDS
        while open_terminal:
            if time.monotonic() >= held_by:
                raise Unconfirmed("a process outside the browser's group still holds its "
                                  "terminal")
            open_terminal = drain(master, screen, 0.02)
        # The settlement is confirmed: a cancellation that waited for it ends the sample now.
        pending = signal.sigpending() & set(SIGNALS)
        if pending:
            raise Cancelled(int(signal.sigwait(pending)))
        if screen.browser is None:
            raise RuntimeError("browser status-line frame erased before exit")
        if stray or escaped:
            raise RuntimeError("a process the browser started outlived it; it was killed")
        if code != 0:
            raise RuntimeError("child exited with status %d after browser frame" % code)
        return {"schema": SCHEMA, "browser_ms": milliseconds(screen.browser - started),
                "splash_ms": milliseconds(screen.first - started), "exit_code": code}
    finally:
        # From here on a cancellation stays pending: the cleanup is what it would ask for, and
        # it never replaces the outcome the cleanup reached.
        signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
        try:
            if child is not None and not settled:
                if not end(child, watch, reader, token, time.monotonic() + CLEANUP_SECONDS,
                           born):
                    raise Unconfirmed("the browser's session did not end after its kill")
        finally:
            os.close(master)
            os.close(witness)
            if slave >= 0:
                os.close(slave)


def main(argv):
    for number in SIGNALS:
        signal.signal(number, cancelled)
    try:
        print(json.dumps(measure(argv), sort_keys=True, separators=(",", ":")))
        return 0
    except Unconfirmed as error:
        print("tui-open: cleanup unconfirmed: %s" % error, file=sys.stderr)
        return UNCONFIRMED_EXIT
    except Cancelled as error:
        print("tui-open: cancelled by signal %d; the browser's session was ended" % error.args[0],
              file=sys.stderr)
        return 128 + error.args[0]
    except (OSError, RuntimeError, TimeoutError, ValueError) as error:
        print("tui-open: %s" % error, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
