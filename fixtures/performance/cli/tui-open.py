#!/usr/bin/env python3
"""Measure bare af from launch to the first browser status-line frame."""

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
STATUS = re.compile(rb"\x1b\[30;[0-9]+H(?:\x1b\[[0-9;]*m|[^\x1b])*?NORMAL")


def measure(argv):
    if not argv:
        raise ValueError("expected child argv")
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    child = None
    try:
        environment = dict(os.environ, TERM="xterm-256color")
        def claim_tty():
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        started = time.monotonic_ns()
        child = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave,
                                 env=environment, start_new_session=True, close_fds=True,
                                 preexec_fn=claim_tty)
        os.close(slave)
        slave = -1
        first = None
        browser = None
        frame_start = None
        tail = b""
        deadline = started + DEADLINE_SECONDS * 1000000000
        while browser is None:
            remaining = (deadline - time.monotonic_ns()) / 1000000000
            if remaining <= 0:
                raise TimeoutError("browser status-line frame missing at 100-second deadline")
            ready, _, _ = select.select([master], [], [], min(remaining, 0.1))
            if not ready:
                if child.poll() is not None:
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
            # run() starts its first browser frame with Clear(2J), after splash::show.
            # Keep its last clear until the last-row status line proves that frame arrived.
            if b"\x1b[2J" in tail[-3:] + data:
                frame_start = observed
            tail = (tail + data)[-131072:]
            if STATUS.search(tail):
                browser = frame_start or observed
        os.write(master, b"q")
        try:
            code = child.wait(timeout=5)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("child did not exit after q") from error
        if code != 0:
            raise RuntimeError("child exited with status %d after browser frame" % code)
        def milliseconds(end):
            value = str(Decimal(end - started) / Decimal(1000000))
            return value.rstrip("0").rstrip(".") if "." in value else value

        return {"schema": SCHEMA, "browser_ms": milliseconds(browser),
                "splash_ms": milliseconds(first), "exit_code": code}
    finally:
        if child is not None and child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
        os.close(master)
        if slave >= 0:
            os.close(slave)


if __name__ == "__main__":
    try:
        print(json.dumps(measure(sys.argv[1:]), sort_keys=True, separators=(",", ":")))
    except (OSError, RuntimeError, TimeoutError, ValueError) as error:
        print("tui-open: %s" % error, file=sys.stderr)
        sys.exit(1)
