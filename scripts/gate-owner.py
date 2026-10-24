#!/usr/bin/env python3
"""A disposable Linux child owner for one complete native gate.

Only this single-threaded process enables subreaping and waits for its own children.
The command's direct children still belong to the command. This is lifecycle management,
not containment: an uncatchable kill of this owner requires outer-supervisor cleanup.
"""
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

POLL_SECONDS = 0.01
DRAIN_SECONDS = 10
TERM_SECONDS = 2
KILL_SECONDS = 2


def children():
    # Single-threaded owner: its complete direct-child set, without a host-wide scan.
    return [int(pid) for pid in Path(
        f'/proc/self/task/{os.getpid()}/children').read_text().split()]


def exited(pid):
    return os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None


def sweep(leader):
    remaining = []
    for pid in children():
        if pid == leader:
            continue  # Reserve the leader's status and process-group ID until final wait.
        if os.waitpid(pid, os.WNOHANG)[0] == 0:
            remaining.append(pid)
    return remaining


def send_child(pid, sig):
    # No other thread/handler reaps. Successful waitid, even for a live child, proves
    # this is our unreaped child and pins its PID until our next explicit wait.
    os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
    os.kill(pid, sig)


def send_group(leader, sig):
    try:
        os.killpg(leader, sig)
    except ProcessLookupError:
        pass  # No member remains in the still-reserved group; adopted children are separate.


def finish(code):
    if code < 0:
        sig = -code
        if sig not in (signal.SIGKILL, signal.SIGSTOP):
            signal.signal(sig, signal.SIG_DFL)
        os.kill(os.getpid(), sig)
    return code


def launch_failure(error):
    print(f'gate-owner: cannot start command: {error}', file=sys.stderr)
    return 127 if isinstance(error, FileNotFoundError) else 126


def run(command):
    if sys.platform != 'linux':
        # Preserve command, descriptors and signal semantics without unsupported prctl.
        try:
            os.execvp(command[0], command)
        except OSError as error:
            return launch_failure(error)

    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'PR_SET_CHILD_SUBREAPER')

    cancelled = []

    def cancel(sig, _frame):
        if not cancelled:
            cancelled.append(sig)

    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT):
        signal.signal(sig, cancel)
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    try:
        leader = subprocess.Popen(command, start_new_session=True)
    except OSError as error:
        return launch_failure(error)
    drain_until = None
    stopping = None
    kill_at = None
    stop_until = None
    failed = False
    term_sent = set()
    try:
        while True:
            done = exited(leader.pid)
            remaining = sweep(leader.pid)
            now = time.monotonic()
            if cancelled and stopping is None:
                stopping = cancelled[0]
            if done and drain_until is None:
                drain_until = now + DRAIN_SECONDS
            if done and not remaining and children() == [leader.pid]:
                # Adoption can race this scan while the leader is live. An exited
                # leader with no other children cannot leave another adopter behind.
                # Re-read after sweeping: a parent can exit during that sweep and
                # expose grandchildren absent from its initial children snapshot.
                break
            if stopping is None and drain_until is not None and now >= drain_until:
                print('gate-owner: live descendants exceeded the normal drain bound',
                      file=sys.stderr, flush=True)
                failed = True
                stopping = signal.SIGTERM
            if stopping is not None:
                if kill_at is None:
                    # The unreaped leader reserves the group ID against PID reuse.
                    send_group(leader.pid, stopping)
                    kill_at = now + TERM_SECONDS
                    stop_until = kill_at + KILL_SECONDS
                # Killing direct parents exposes escaped/nested descendants by adoption.
                # Normal execution and normal draining never signal live orphan work.
                for pid in remaining:
                    if now >= kill_at:
                        send_child(pid, signal.SIGKILL)
                    elif pid not in term_sent:
                        send_child(pid, stopping)
                        term_sent.add(pid)
                term_sent.intersection_update(remaining)
                if now >= kill_at:
                    send_group(leader.pid, signal.SIGKILL)
                if now >= stop_until:
                    raise TimeoutError('gate-owned children did not exit after termination')
            time.sleep(POLL_SECONDS)
        code = leader.wait()  # Observed exited, so cannot block; exact leader status.
        if cancelled:
            return -cancelled[0]
        return 125 if failed and code == 0 else code
    finally:
        # Unexpected observation errors fail; they never claim clean completion.
        if leader.returncode is None:
            print('gate-owner: emergency teardown after supervision failure', file=sys.stderr)
            until = stop_until if stop_until is not None else time.monotonic() + KILL_SECONDS
            try:
                while True:
                    send_group(leader.pid, signal.SIGKILL)
                    for pid in sweep(leader.pid):
                        send_child(pid, signal.SIGKILL)
                    if (exited(leader.pid) and not sweep(leader.pid)
                            and children() == [leader.pid]):
                        leader.wait()
                        break
                    if time.monotonic() >= until:
                        print('gate-owner: child cleanup unconfirmed', file=sys.stderr)
                        break
                    time.sleep(POLL_SECONDS)
            except OSError as error:
                print(f'gate-owner: child cleanup unconfirmed: {error}', file=sys.stderr)


def main():
    if len(sys.argv) < 2:
        print('usage: gate-owner.py COMMAND [ARGS...]', file=sys.stderr)
        return 2
    try:
        code = run(sys.argv[1:])
    except (OSError, TimeoutError) as error:
        print(f'gate-owner: {error}', file=sys.stderr)
        return 125
    return finish(code)


if __name__ == '__main__':
    sys.exit(main())
