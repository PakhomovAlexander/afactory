#!/usr/bin/env python3
"""Bounded native gate ownership regressions; all subreapers are disposable children.

Launch by absolute filename from any cwd, with no inherited PYTHONPATH. Evidence reads
only known fixture PIDs, never host process command lines or environments.
"""
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.dont_write_bytecode = True
OWNER = Path(__file__).resolve().with_name('gate-owner.py')
SELF = Path(__file__).resolve()


def state(pid):
    try:
        raw = Path(f'/proc/{pid}/stat').read_bytes()
    except FileNotFoundError:
        return None
    fields = raw[raw.rindex(b')') + 1:].split()
    return [pid, fields[0].decode(), int(fields[1]), int(fields[2]), int(fields[3])]


def until(predicate, seconds=5):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(0.01)
    raise AssertionError('fixture condition missed its bounded deadline')


def load_owner():
    spec = importlib.util.spec_from_file_location('gate_owner', OWNER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def gate_fixture(root, mode):
    if mode == 'zombies':
        # Each intermediate parent owns/reaps its direct child. Its grandchild is
        # orphaned and exits under the nearest subreaper while this leader stays alive.
        pids = []
        for _ in range(24):
            read, write = os.pipe()
            parent = os.fork()
            if parent == 0:
                os.close(read)
                orphan = os.fork()
                if orphan == 0:
                    os.close(write)
                    time.sleep(0.03)
                    os._exit(0)
                os.write(write, str(orphan).encode())
                os._exit(0)
            os.close(write)
            pids.append(int(os.read(read, 32)))
            os.close(read)
            assert os.waitpid(parent, 0)[1] == 0  # Owner must not steal this status.
        (root / 'pids').write_text(json.dumps(pids))
        until(lambda: (root / 'release').exists())
    elif mode in ('live', 'cancel', 'drain', 'thread'):
        parent = os.fork()
        if parent == 0:
            os.setsid()
            if mode == 'thread':
                import threading

                def worker():
                    until(lambda: state(os.getpid())[1] == 'Z')
                    (root / 'live').write_text(str(os.getpid()))
                    until(lambda: (root / 'finish').exists(), seconds=60)
                    (root / 'finished').write_text('natural exit')
                    os._exit(0)

                threading.Thread(target=worker).start()
                ctypes.CDLL(None).pthread_exit(None)
            if mode == 'cancel':
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                # One more escaped generation, exposed only after its parent is killed.
                nested = os.fork()
                if nested == 0:
                    os.setsid()
                    (root / 'nested').write_text(str(os.getpid()))
                    time.sleep(60)
                    os._exit(0)
            (root / 'live').write_text(str(os.getpid()))
            until(lambda: (root / 'finish').exists(), seconds=60)
            (root / 'finished').write_text('natural exit')
            os._exit(0)
        until(lambda: (root / 'live').exists())
        (root / 'leader').write_text(str(os.getpid()))
        if mode == 'cancel':
            time.sleep(60)
        # Early leader exit: owner must drain live detached work without signalling it.


def outer(root, repaired):
    libc = ctypes.CDLL(None, use_errno=True)
    assert libc.prctl(36, 1, 0, 0, 0) == 0
    command = [sys.executable, str(SELF), '--gate', str(root), 'zombies']
    if repaired:
        command = [sys.executable, str(OWNER), *command]
    child = subprocess.Popen(command)
    sibling = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(.3); exit(37)'])
    pids = []
    try:
        until(lambda: (root / 'pids').exists())
        pids = json.loads((root / 'pids').read_text())
        if repaired:
            until(lambda: all(state(pid) is None for pid in pids))
            evidence = [state(pid) for pid in pids]
        else:
            until(lambda: all((s := state(pid)) and s[1] == 'Z'
                              and s[2] == os.getpid() for pid in pids))
            evidence = [state(pid) for pid in pids]
        assert child.poll() is None, 'gate must still be alive when orphans are observed'
        assert sibling.wait(timeout=2) == 37, 'never steal another direct child status'
        print(json.dumps({'repaired': repaired, 'outer': os.getpid(),
                          'gate_alive': True, 'children': evidence}), flush=True)
        (root / 'release').touch()
        assert child.wait(timeout=3) == 0
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=3)
        if sibling.poll() is None:
            sibling.kill()
        sibling.wait(timeout=3)
        # The predecessor's 24 known dead adopted children are ours; release all slots.
        for pid in pids:
            if (observed := state(pid)) and observed[2] == os.getpid():
                assert observed[1] == 'Z'
                os.waitpid(pid, 0)


def under_outer(command):
    """Run focused original Rust tests beneath the same retaining outer-subreaper model."""
    libc = ctypes.CDLL(None, use_errno=True)
    assert libc.prctl(36, 1, 0, 0, 0) == 0
    result = subprocess.call([sys.executable, str(OWNER), *command])
    remaining = load_owner().children()
    if remaining:
        raise AssertionError(f'outer adopted gate children: {[state(pid) for pid in remaining]}')
    return result


@unittest.skipUnless(sys.platform == 'linux', 'actual Linux subreaper and /proc required')
class Ownership(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='af-gate-owner-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.env = {'PATH': os.environ['PATH'], 'HOME': self.tmp.name,
                    **{f'XDG_{name}_HOME': self.tmp.name for name in
                       ('CONFIG', 'CACHE', 'STATE', 'DATA')}, 'PYTHONDONTWRITEBYTECODE': '1'}

    def command(self, *args):
        return [sys.executable, str(OWNER), *args]

    def test_bounded_outer_subreaper_reproduction(self):
        for repaired in (False, True):
            root = self.root / str(repaired)
            root.mkdir()
            result = subprocess.run([sys.executable, str(SELF), '--outer', str(root),
                                     str(int(repaired))], cwd='/', env=self.env,
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            evidence = json.loads(result.stdout)
            self.assertTrue(evidence['gate_alive'])
            self.assertEqual(len(evidence['children']), 24)
            print('outer-subreaper evidence:', result.stdout.strip(), flush=True)

    def test_status_streams_spawn_and_signal(self):
        for code in (0, 23):
            result = subprocess.run(self.command(sys.executable, '-c',
                f'import sys; print("out"); print(sys.stdin.readline(), end=""); '
                f'print("err",file=sys.stderr); exit({code})'),
                env=self.env, input='in\n', capture_output=True, text=True, timeout=5)
            self.assertEqual((result.returncode, result.stdout, result.stderr),
                             (code, 'out\nin\n', 'err\n'))
        result = subprocess.run(self.command('/no/such/af-gate-fixture'),
                                env=self.env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 127)
        denied = self.root / 'not-executable'
        denied.write_text('no executable bit')
        result = subprocess.run(self.command(str(denied)), env=self.env,
                                capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 126)
        for sig in (signal.SIGTERM, signal.SIGKILL):
            result = subprocess.run(self.command(sys.executable, '-c',
                f'import os; os.kill(os.getpid(), {sig})'), env=self.env, timeout=5)
            self.assertEqual(result.returncode, -sig)

    def start(self, root, mode):
        child = subprocess.Popen(self.command(sys.executable, str(SELF), '--gate',
                                              str(root), mode), env=self.env, cwd='/')
        self.addCleanup(self.stop, child)
        until(lambda: (root / 'live').exists())
        return child

    @staticmethod
    def stop(child):
        if child.poll() is None:
            child.terminate()
        child.wait(timeout=7)

    def test_live_detached_drain_and_concurrent_gate_isolation(self):
        other = self.root / 'other'
        other.mkdir()
        a, b = self.start(self.root, 'live'), self.start(other, 'live')
        pids = [int((root / 'live').read_text()) for root in (self.root, other)]
        until(lambda: all(state(pid)[2] == owner.pid for pid, owner in zip(pids, (a, b))))
        for pid in pids:
            self.assertEqual(state(pid)[4], pid, 'live orphan escaped into its own session')
        self.assertIsNone(a.poll())
        self.assertIsNone(b.poll())
        (self.root / 'finish').touch()
        self.assertEqual(a.wait(timeout=3), 0)
        self.assertEqual((self.root / 'finished').read_text(), 'natural exit')
        self.assertIsNone(b.poll())
        self.assertNotEqual(state(pids[1])[1], 'Z')
        (other / 'finish').touch()
        self.assertEqual(b.wait(timeout=3), 0)
        self.assertTrue(all(state(pid) is None for pid in pids))

    def test_cancellation_kills_owned_escaped_nested_tree(self):
        child = self.start(self.root, 'cancel')
        sibling_root = self.root / 'sibling'
        sibling_root.mkdir()
        sibling = self.start(sibling_root, 'live')
        sibling_pid = int((sibling_root / 'live').read_text())
        until(lambda: (self.root / 'nested').exists() and (self.root / 'leader').exists())
        pids = [int((self.root / name).read_text()) for name in ('live', 'nested', 'leader')]
        for pid in pids:
            self.assertNotEqual(state(pid)[1], 'Z')
        start = time.monotonic()
        child.terminate()
        self.assertEqual(child.wait(timeout=7), -signal.SIGTERM)
        self.assertLess(time.monotonic() - start, 6)
        self.assertTrue(all(state(pid) is None for pid in pids))
        self.assertIsNone(sibling.poll())
        self.assertNotEqual(state(sibling_pid)[1], 'Z')
        (sibling_root / 'finish').touch()
        self.assertEqual(sibling.wait(timeout=3), 0)
        self.assertIsNone(state(sibling_pid))

    def test_live_orphan_drain_expiry_fails_and_cleans_up(self):
        child = self.start(self.root, 'drain')
        pid = int((self.root / 'live').read_text())
        start = time.monotonic()
        self.assertEqual(child.wait(timeout=15), 125)
        self.assertGreater(time.monotonic() - start, 9)
        self.assertIsNone(state(pid))
        self.assertFalse((self.root / 'finished').exists())

    def test_zombie_thread_leader_with_live_worker_is_not_false_completion(self):
        child = self.start(self.root, 'thread')
        pid = int((self.root / 'live').read_text())
        until(lambda: state(pid)[2] == child.pid)
        self.assertEqual(state(pid)[1], 'Z')
        self.assertIsNone(child.poll(), 'live worker still owns unfinished work')
        (self.root / 'finish').touch()
        self.assertEqual(child.wait(timeout=3), 0)
        self.assertEqual((self.root / 'finished').read_text(), 'natural exit')
        self.assertIsNone(state(pid))

    def test_cancellation_forwards_each_supported_signal(self):
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT):
            root = self.root / str(sig)
            root.mkdir()
            source = '''
import os, signal, sys, time
from pathlib import Path
root, sig = Path(sys.argv[1]), int(sys.argv[2])
def stopped(sent, frame):
    (root / 'received').write_text(str(sent))
    sys.exit(17)
signal.signal(sig, stopped)
(root / 'ready').write_text(str(os.getpid()))
time.sleep(60)
'''
            child = subprocess.Popen(self.command(sys.executable, '-c', source,
                                                  str(root), str(sig)), env=self.env)
            self.addCleanup(self.stop, child)
            until(lambda: (root / 'ready').exists())
            pid = int((root / 'ready').read_text())
            os.kill(child.pid, sig)
            self.assertEqual(child.wait(timeout=5), -sig)
            self.assertEqual((root / 'received').read_text(), str(sig))
            self.assertIsNone(state(pid))

    def test_observation_error_is_failure_and_tears_down_owned_children(self):
        source = '''
import importlib.util, sys, time
from pathlib import Path
spec = importlib.util.spec_from_file_location('gate_owner', sys.argv[1])
owner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owner)
root, fixture = Path(sys.argv[2]), sys.argv[3]
read, first = owner.children, [True]
def fail_once():
    if first:
        deadline = time.monotonic() + 5
        while not (root / 'live').exists():
            assert time.monotonic() < deadline
            time.sleep(.01)
        first.clear()
        raise OSError('injected child observation failure')
    return read()
owner.children = fail_once
sys.argv = [str(owner.__file__), sys.executable, fixture, '--gate', str(root), 'live']
sys.exit(owner.main())
'''
        result = subprocess.run([sys.executable, '-c', source, str(OWNER),
                                 str(self.root), str(SELF)], env=self.env,
                                capture_output=True, text=True, timeout=8)
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertIn('injected child observation failure', result.stderr)
        self.assertIn('emergency teardown', result.stderr)
        self.assertIsNone(state(int((self.root / 'live').read_text())))


class Fallback(unittest.TestCase):
    def test_only_unreaped_owned_pids_are_signalled_and_leader_is_reserved(self):
        owner = load_owner()
        with mock.patch.object(owner.os, 'waitid', side_effect=ChildProcessError), \
                mock.patch.object(owner.os, 'kill') as kill:
            with self.assertRaises(ChildProcessError):
                owner.send_child(1234, signal.SIGTERM)
            kill.assert_not_called()
        with mock.patch.object(owner, 'children', return_value=[1234, 5678]), \
                mock.patch.object(owner.os, 'waitpid', return_value=(5678, 0)) as wait:
            self.assertEqual(owner.sweep(1234), [])
            wait.assert_called_once_with(5678, os.WNOHANG)

    def test_non_linux_executes_original_without_prctl(self):
        owner = load_owner()
        with mock.patch.object(owner.sys, 'platform', 'darwin'), \
                mock.patch.object(owner.ctypes, 'CDLL') as libc, \
                mock.patch.object(owner.os, 'execvp', side_effect=SystemExit) as execute:
            with self.assertRaises(SystemExit):
                owner.run(['bash', '/source/verify-body.sh'])
            execute.assert_called_once_with('bash', ['bash', '/source/verify-body.sh'])
            libc.assert_not_called()


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--gate':
        gate_fixture(Path(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) > 1 and sys.argv[1] == '--outer':
        outer(Path(sys.argv[2]), bool(int(sys.argv[3])))
    elif len(sys.argv) > 1 and sys.argv[1] == '--under-outer':
        sys.exit(under_outer(sys.argv[2:]))
    else:
        unittest.main()
