"""Isolated Linux ancestor subreaper for the real telemetry executor regression.

Only this disposable helper adopts children. It deliberately retains them until the inner
executor returns, just as the native check driver does. Evidence is limited to known fixture
PIDs' state/parent/group/session, never command lines or environments.
"""
import ctypes
import json
import os
import signal
import subprocess
import sys
import time
from unittest import mock

from perflab import telemetryexec


def state(pid):
    try:
        with open("/proc/%d/stat" % pid, "rb") as stream:
            raw = stream.read(4096)
    except FileNotFoundError:
        return None
    fields = raw[raw.rindex(b")") + 1:].split()
    return [pid, fields[0].decode("ascii"), int(fields[1]), int(fields[2]), int(fields[3])]


def thread_states(pid):
    """Only a known fixture's threads, including a zombie leader with a live worker."""
    try:
        names = os.listdir("/proc/%d/task" % pid)
    except FileNotFoundError:
        return []
    if len(names) > 8:
        raise AssertionError("fixture thread evidence exceeds its bound")
    result = []
    for name in sorted(names):
        try:
            with open("/proc/%d/task/%s/stat" % (pid, name), "rb") as stream:
                raw = stream.read(4096)
        except FileNotFoundError:
            continue
        result.append([int(name), raw[raw.rindex(b")") + 1:].split()[0].decode("ascii")])
    return result


SCRIPT = r'''
import json, os, subprocess, sys, time
source = "import os,sys,time; open('ready', 'w').write(str(os.getpid())); time.sleep(60)"
if sys.argv[1] == "thread_survivor":
    source = """
import ctypes, os, threading, time
def worker():
    while True:
        with open('/proc/self/stat', 'rb') as stream:
            raw = stream.read(4096)
        if raw[raw.rindex(b')') + 1:].split()[0] == b'Z':
            break
        time.sleep(0.005)
    open('ready', 'w').write(str(os.getpid()))
    time.sleep(60)
threading.Thread(target=worker).start()
ctypes.CDLL(None).pthread_exit(None)
"""
child = subprocess.Popen([sys.executable, "-c", source])
while not os.path.exists("ready"):
    time.sleep(0.005)
with open("pids.json", "w") as stream:
    json.dump([os.getpid(), child.pid], stream)
until = time.monotonic() + 5
while not os.path.exists("observed"):
    if time.monotonic() >= until:
        raise TimeoutError("ancestor did not observe the live fixture")
    time.sleep(0.005)
print('{}', flush=True)
if sys.argv[1] in ("deadline", "cancel"):
    time.sleep(60)
'''


def inner(root, mode):
    env = {"PATH": "/usr/bin:/bin", "HOME": root, "TMPDIR": root,
           "XDG_CONFIG_HOME": root, "XDG_CACHE_HOME": root, "XDG_STATE_HOME": root,
           "XDG_DATA_HOME": root}
    execution = telemetryexec.Execution(
        [sys.executable, "-c", SCRIPT, mode], env, root, [root], [], root, "capture",
        64 << 20, time.monotonic() + (1 if mode == "deadline" else 30), 1 << 30)
    executor = telemetryexec.TrustedLocal(sys.executable)
    if mode == "cancel":
        finished, finish = [], executor.finish
        def record(*args):
            value = finish(*args)
            finished.append(value)
            return value
        with mock.patch.object(executor, "finish", side_effect=record):
            try:
                executor.run(execution)
            except KeyboardInterrupt:
                with open(os.path.join(root, "outcome.json"), "w") as stream:
                    cleanup = finished[-1] if finished else (None, "uncertain", "no cleanup")
                    json.dump({"code": None, "timed_out": False, "cancelled": True,
                               "cleanup": cleanup[1], "detail": cleanup[2],
                               "failure": "cancelled by SIGINT"}, stream)
                return
        raise AssertionError("SIGINT must propagate after real executor cleanup")
    if mode in ("survivor", "thread_survivor"):
        # Model a signal that cannot end a genuinely live member. The real executor still
        # performs its complete scans and uses its unchanged ten-second cleanup bound.
        with mock.patch.object(telemetryexec._LocalProcess, "signal_group"):
            result = executor.run(execution)
    elif mode == "repeat_kill":
        signals, send = [], telemetryexec._LocalProcess.signal_group
        def miss_once(process, sent):
            signals.append(sent)
            if len(signals) > 1:
                send(process, sent)
        with mock.patch.object(telemetryexec._LocalProcess, "signal_group", miss_once):
            result = executor.run(execution)
        if len(signals) < 2:
            raise AssertionError("a still-live member requires another owned group signal")
    else:
        result = executor.run(execution)
    with open(os.path.join(root, "outcome.json"), "w") as stream:
        json.dump({"code": result.code, "timed_out": result.timed_out,
                   "cleanup": result.cleanup, "detail": result.cleanup_detail,
                   "failure": result.failure()}, stream)


def ancestor(root, mode):
    # prctl is confined to this helper; no process-global change reaches unittest.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER")
    child = subprocess.Popen([sys.executable, __file__, "inner", root, mode])
    pids, transitions, interrupted = [], [], False
    until = time.monotonic() + 25
    try:
        while child.poll() is None:
            if not pids:
                try:
                    with open(os.path.join(root, "pids.json")) as stream:
                        pids = json.load(stream)
                except (FileNotFoundError, ValueError):
                    pass
            snapshot = [state(pid) for pid in pids]
            if snapshot and (not transitions or snapshot != transitions[-1]):
                # Keep only changed process states, with a hard evidence bound.
                if len(transitions) < 16:
                    transitions.append(snapshot)
            if (snapshot and snapshot[1]
                    and (snapshot[1][1] not in ("Z", "X", "x")
                         or mode == "thread_survivor")
                    and snapshot[1][2] == pids[0]):
                with open(os.path.join(root, "observed"), "w"):
                    pass
                if mode == "cancel" and not interrupted:
                    os.kill(child.pid, signal.SIGINT)
                    interrupted = True
            if time.monotonic() >= until:
                raise TimeoutError("inner executor did not finish within helper bound")
            time.sleep(0.01)
        if child.returncode:
            raise RuntimeError("inner executor exited %d" % child.returncode)
        with open(os.path.join(root, "outcome.json")) as stream:
            result = json.load(stream)
        result.update(ancestor=os.getpid(), inner=child.pid, pids=pids,
                      transitions=transitions, final=[state(pid) for pid in pids],
                      final_threads=[thread_states(pid) for pid in pids])
        print(json.dumps(result), flush=True)
    finally:
        # These PIDs are signalled only while they are our unreaped children. Never reap
        # globally or signal a released group ID, including on the helper's error path.
        if child.returncode is None:
            child.kill()
        child.wait(timeout=2)
        # This helper runs exactly one fixture. Its remaining children are only that
        # fixture's adopted descendants, including a leader not yet named by its marker.
        until = time.monotonic() + 2
        while True:
            with open("/proc/self/task/%d/children" % os.getpid()) as stream:
                adopted = [int(value) for value in stream.read(4096).split()]
            if not adopted:
                break
            if len(adopted) > 8 or time.monotonic() >= until:
                raise TimeoutError("fixture children could not be ended within helper bound")
            for pid in adopted:
                try:
                    os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
                except ChildProcessError:
                    continue
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, os.WNOHANG)
            time.sleep(0.01)


if __name__ == "__main__":
    (inner if sys.argv[1] == "inner" else ancestor)(sys.argv[2], sys.argv[3])
