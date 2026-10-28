#!/usr/bin/env python3
"""Measure bare af from launch to the first browser status-line frame (ADR-0258).

`python3 -I tui-open.py AF [ARG...]` runs the argv in a 100x30 pseudo-terminal as the leader of
a new session (the browser needs a controlling terminal), and prints one
`af.perf-tui-open/1` line: `splash_ms`, launch to the first byte of any output, and
`browser_ms`, launch to the read that completes the last-row `NORMAL` status line of the screen
after its last clear. A pseudo-terminal carries no write boundaries, so the instant the status
line reaches the terminal is the earliest the browser frame is shown on it; a status line that
an earlier clear erased never counts.

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
the driver, as beyond any process-group sample. SIGTERM, SIGINT and SIGHUP, the supervisor's
cancellation, end the session the same way before the driver exits. A cleanup that cannot be
confirmed, a listing included, exits 3, and the processes left still hold the supervisor's pipe.
"""

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


def live_members(group):
    """Whether a process of `group` other than its exited, unreaped leader still runs. Linux
    lists the group's live members; XNU refuses a signal to a group of zombies with EPERM."""
    if os.path.isdir("/proc/self/task"):
        for name in os.listdir("/proc"):
            if not name.isdigit():
                continue
            try:
                with open("/proc/%s/stat" % name, "rb") as source:
                    stat = source.read()
            except OSError:
                continue
            fields = stat[stat.rindex(b")") + 2:].split()
            if int(fields[2]) == group and fields[0] not in (b"Z", b"X", b"x"):
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


def settle(group, deadline):
    """Kill the group's live members until none runs, or until `deadline`; whether none runs.
    Its leader is unreaped, so the group's id names nothing else."""
    while live_members(group):
        if time.monotonic() >= deadline:
            return False
        kill_group(group)
        time.sleep(0.005)
    return True


def listed(token, exclude):
    """The pids, `exclude` apart, of every live process whose environment carries `token` in
    `SESSION_VARIABLE`: the browser's doing, whatever session, group or descriptors it has now,
    unless it replaced its environment before `exec`. Linux reads `/proc`; elsewhere `ps -E`
    lists the user's processes with their environments."""
    marker = "%s=%s" % (SESSION_VARIABLE, token)
    found = set()
    if os.path.isdir("/proc/self/task"):
        for name in os.listdir("/proc"):
            if not name.isdigit() or int(name) in exclude:
                continue
            try:
                with open("/proc/%s/environ" % name, "rb") as source:
                    environment = source.read()
            except OSError:
                continue
            if marker.encode("utf-8") in environment.split(b"\0"):
                found.add(int(name))
        return found
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


def settle_listed(token, exclude, deadline):
    """Kill every process `listed` names until none is, or until `deadline`: whether one was
    found, and whether none is left."""
    found = False
    while True:
        pids = listed(token, exclude)
        if not pids:
            return found, True
        found = True
        if time.monotonic() >= deadline:
            return found, False
        for pid in pids:
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        time.sleep(0.005)


def reap(child):
    _, status = os.waitpid(child.pid, 0)
    child.returncode = os.waitstatus_to_exitcode(status) if hasattr(
        os, "waitstatus_to_exitcode") else (os.WEXITSTATUS(status) if os.WIFEXITED(status)
                                            else -os.WTERMSIG(status))
    return child.returncode


def end(child, watch, token, deadline):
    """Kill the running session, observe its leader's exit, settle its group, reap the leader
    and clear what left the group; whether every step was confirmed before `deadline`."""
    kill_group(child.pid)
    while not watch.seen():
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.005)
    if not settle(child.pid, deadline):
        return False
    reap(child)
    return settle_listed(token, {child.pid}, deadline)[1]


def drain(master, timeout):
    """Read and discard what the terminal holds; False once it ended."""
    ready, _, _ = select.select([master], [], [], timeout)
    if not ready:
        return True
    try:
        return bool(os.read(master, 65536))
    except OSError:
        return False


def milliseconds(nanoseconds):
    value = str(Decimal(nanoseconds) / Decimal(1000000))
    return value.rstrip("0").rstrip(".") if "." in value else value


def measure(argv):
    """The driver's document for one run of `argv`; it returns, or raises, with SIGTERM, SIGINT
    and SIGHUP blocked once the session's cleanup began."""
    if not argv:
        raise ValueError("expected child argv")
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    witness = os.dup(2)
    child = watch = None
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
        first = None
        browser = None
        screen = b""
        deadline = started + DEADLINE_SECONDS * 1000000000
        while browser is None:
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
            observed = time.monotonic_ns()
            if first is None:
                first = observed
            # Only what the last clear left on the screen counts: an earlier status line is
            # gone, and the line must arrive for the frame that paints it to be shown.
            screen += data
            clear = screen.rfind(CLEAR)
            if clear >= 0:
                screen = screen[clear + len(CLEAR):]
            screen = screen[-SCREEN_MAX:]
            if STATUS.search(screen):
                browser = observed
        os.write(master, b"q")
        quit_by = time.monotonic() + QUIT_SECONDS
        open_terminal = True
        while not watch.seen():
            if time.monotonic() >= quit_by:
                raise RuntimeError("child did not exit after q")
            if open_terminal:
                open_terminal = drain(master, 0.02)
            else:
                time.sleep(0.005)
        cleanup_by = time.monotonic() + CLEANUP_SECONDS
        stray = live_members(child.pid)
        if not settle(child.pid, cleanup_by):
            raise Unconfirmed("a process of the browser's session survived its kill")
        code = reap(child)
        escaped, cleared = settle_listed(token, {child.pid},
                                         time.monotonic() + CLEANUP_SECONDS)
        if not cleared:
            raise Unconfirmed("a process the browser started survived its kill")
        if stray or escaped:
            raise RuntimeError("a process the browser started outlived it; it was killed")
        while open_terminal:
            if time.monotonic() >= cleanup_by:
                raise Unconfirmed("a process outside the browser's group still holds its "
                                  "terminal")
            open_terminal = drain(master, 0.02)
        if code != 0:
            raise RuntimeError("child exited with status %d after browser frame" % code)
        return {"schema": SCHEMA, "browser_ms": milliseconds(browser - started),
                "splash_ms": milliseconds(first - started), "exit_code": code}
    finally:
        # From here on a cancellation stays pending: the cleanup is what it would ask for, and
        # it never replaces the outcome the cleanup reached.
        signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
        try:
            if child is not None and child.returncode is None:
                if not end(child, watch, token, time.monotonic() + CLEANUP_SECONDS):
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
