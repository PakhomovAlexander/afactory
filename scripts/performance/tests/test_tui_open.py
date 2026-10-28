"""The PTY driver and its private registry fixture."""

import importlib.util
import json
import os
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest

from perflab import clicases
from perflab.values import ContractError
from tests.support import ROOT

DRIVER = os.path.join(ROOT, "fixtures/performance/cli/tui-open.py")
SETUP = os.path.join(ROOT, "fixtures/performance/cli/setup.sh")

# A stand-in for the browser. `tcdrain` returns once the driver has read what was written, so
# every sequence reaches the driver in the order written without a sleep. `ready` writes the
# pids to the FIFO the test reads, the test's only synchronization.
BROWSER = (
    "import fcntl, os, signal, struct, sys, termios, time, tty\n"
    "mode = sys.argv[1]\n"
    "assert os.isatty(0) and os.isatty(1)\n"
    "size = fcntl.ioctl(0, termios.TIOCGWINSZ, b'\\x00' * 8)\n"
    "assert struct.unpack('HHHH', size)[:2] == (30, 100)\n"
    "tty.setraw(0)\n"
    "def paint(data):\n"
    "    os.write(1, data)\n"
    "    termios.tcdrain(1)\n"
    "def ready(*pids):\n"
    "    with open(sys.argv[2], 'w') as fifo:\n"
    "        fifo.write(' '.join(str(pid) for pid in pids) + '\\n')\n"
    "if mode != 'late':\n"
    "    paint(b'splash')\n"
    "if mode == 'paint':\n"
    "    paint(b'\\x1b[2J')\n"
    "    paint(b'\\x1b[30;1H\\x1b[0;7mNORMAL  browser')\n"
    "elif mode == 'late':\n"
    "    paint(b'\\x1b[2J\\x1b[1;1Hsplash')\n"
    "    paint(b'\\x1b[30;1H\\x1b[0;7mNORMAL  browser')\n"
    "elif mode == 'erased':\n"
    "    paint(b'\\x1b[30;1HNORMAL\\x1b[2J')\n"
    "    sys.exit(0)\n"
    "elif mode == 'stray':\n"
    "    if os.fork() == 0:\n"
    "        signal.signal(signal.SIGHUP, signal.SIG_IGN)\n"
    "        time.sleep(30)\n"
    "        os._exit(0)\n"
    "    paint(b'\\x1b[2J\\x1b[30;1HNORMAL')\n"
    "elif mode == 'hold':\n"
    "    stray = os.fork()\n"
    "    if stray == 0:\n"
    "        signal.signal(signal.SIGHUP, signal.SIG_IGN)\n"
    "        time.sleep(30)\n"
    "        os._exit(0)\n"
    "    ready(os.getpid(), stray)\n"
    "    time.sleep(30)\n"
    "elif mode == 'escape':\n"
    "    escaped = os.fork()\n"
    "    if escaped == 0:\n"
    "        os.setsid()\n"
    "        os.closerange(0, 1024)\n"
    "        time.sleep(30)\n"
    "        os._exit(0)\n"
    "    ready(os.getpid(), escaped)\n"
    "    paint(b'\\x1b[2J\\x1b[30;1HNORMAL')\n"
    "elif mode == 'deaf':\n"
    "    signal.signal(signal.SIGHUP, signal.SIG_IGN)\n"
    "    ready(os.getpid())\n"
    "    end = time.monotonic() + 60\n"
    "    while not os.path.exists(sys.argv[2] + '.release') and time.monotonic() < end:\n"
    "        time.sleep(0.02)\n"
    "    sys.exit(0)\n"
    "if mode == 'missing':\n"
    "    sys.exit(0)\n"
    "if mode == 'hang':\n"
    "    time.sleep(1)\n"
    "os.read(0, 1)\n")


def load_driver():
    spec = importlib.util.spec_from_file_location("tui_open_driver", DRIVER)
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)
    return driver


class TuiOpen(unittest.TestCase):
    def setUp(self):
        # An in-process measure() returns with the cancellation signals blocked.
        self.addCleanup(signal.pthread_sigmask, signal.SIG_SETMASK,
                        signal.pthread_sigmask(signal.SIG_BLOCK, ()))
        self.root = tempfile.mkdtemp(prefix="af-tui-open-")
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.root]))
        self.browser = os.path.join(self.root, "browser.py")
        with open(self.browser, "w", encoding="utf-8") as output:
            output.write(BROWSER)
        self.fifo = os.path.join(self.root, "ready")
        os.mkfifo(self.fifo)

    def argv(self, mode):
        return [sys.executable, "-I", self.browser, mode, self.fifo]

    def drive(self, mode, **options):
        process = subprocess.Popen([sys.executable, "-I", DRIVER] + self.argv(mode),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                   **options)
        self.addCleanup(self.end, process, options.get("start_new_session", False))
        return process

    def end(self, process, session):
        """Whatever a failed step left of the driver: killed, with its group when it led one,
        reaped, its pipes closed."""
        if process.poll() is None:
            try:
                if session:
                    os.killpg(process.pid, signal.SIGKILL)
                else:
                    process.kill()
            except ProcessLookupError:
                pass
            process.wait(timeout=10)
        for stream in (process.stdout, process.stderr):
            stream.close()

    def release(self, pid):
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def run_driver(self, mode):
        process = self.drive(mode)
        stdout, stderr = process.communicate(timeout=10)
        return process.returncode, stdout, stderr

    def pids(self, process):
        """The pids the browser wrote to the readiness FIFO, within 10 s and while the driver
        runs; each is killed at cleanup if it is still there."""
        fifo = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        line = b""
        deadline = time.monotonic() + 10
        try:
            while b"\n" not in line:
                if process.poll() is not None:
                    self.fail("the driver exited %d before the browser was ready: %s"
                              % (process.returncode, process.stderr.read()))
                if time.monotonic() >= deadline:
                    self.fail("the browser was not ready within 10 s")
                ready, _, _ = select.select([fifo], [], [], 0.05)
                data = os.read(fifo, 4096) if ready else b""
                if data:
                    line += data
                else:
                    time.sleep(0.02)  # no writer has opened the FIFO yet
        finally:
            os.close(fifo)
        pids = [int(pid) for pid in line.split()]
        for pid in pids:
            self.addCleanup(self.release, pid)
        return pids

    def assert_not_running(self, pid):
        """`pid` is gone or a zombie its parent has yet to reap; within 2 s, since the
        reparenting and reaping of a killed orphan are the init's to time."""
        deadline = time.monotonic() + 2
        while True:
            state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   text=True).stdout.strip()
            if state == "" or state.startswith("Z"):
                return
            if time.monotonic() >= deadline:
                self.fail("pid %d still runs in state %r" % (pid, state))
            time.sleep(0.02)

    def test_first_status_line_and_missing_frame(self):
        code, stdout, stderr = self.run_driver("paint")
        self.assertEqual(code, 0, stderr)
        result = json.loads(stdout)
        self.assertEqual(result["schema"], "af.perf-tui-open/1")
        self.assertEqual(result["exit_code"], 0)
        self.assertGreater(float(result["browser_ms"]), float(result["splash_ms"]))
        browser, splash = clicases.tui_report(stdout.encode())
        self.assertEqual((float(browser), float(splash)),
                         (float(result["browser_ms"]), float(result["splash_ms"])))
        code, stdout, stderr = self.run_driver("missing")
        self.assertNotEqual(code, 0)
        self.assertEqual(stdout, "")
        self.assertIn("before browser status-line frame", stderr)
        driver = load_driver()
        driver.DEADLINE_SECONDS = 0.05
        with self.assertRaisesRegex(TimeoutError, "deadline"):
            driver.measure(self.argv("hang"))

    def test_a_status_line_a_later_clear_erased_never_counts(self):
        # The only NORMAL precedes the clear: no browser frame is on the screen.
        code, stdout, stderr = self.run_driver("erased")
        self.assertEqual(code, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("before browser status-line frame", stderr)

    def test_the_interval_ends_when_the_status_line_arrives_not_at_an_earlier_clear(self):
        # The first output is a clear and a splash; the status line arrives only after the
        # driver read them, without another clear. Timed from that clear, the browser would
        # equal the splash; it ends when the status line reaches the terminal.
        code, stdout, stderr = self.run_driver("late")
        self.assertEqual(code, 0, stderr)
        browser, splash = clicases.tui_report(stdout.encode())
        self.assertGreater(browser, splash)

    def test_a_descendant_that_outlives_the_browser_is_killed_and_fails_the_sample(self):
        # The stray sleeps for 30 s holding the driver's stderr: the pipe ends within the
        # bound only because the driver killed it before reaping the browser.
        code, stdout, stderr = self.run_driver("stray")
        self.assertEqual(code, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("outlived it; it was killed", stderr)

    def test_a_descendant_that_left_the_session_and_closed_everything_is_found_and_killed(self):
        # The escaped process is in a session of its own and holds neither the terminal nor
        # the driver's stderr; the token in its environment is what finds it.
        process = self.drive("escape")
        browser, escaped = self.pids(process)
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("outlived it; it was killed", stderr)
        self.assert_not_running(escaped)
        self.assert_not_running(browser)

    def test_cancellation_while_the_browser_runs_ends_its_whole_session(self):
        # As the trusted-local supervisor does: the driver leads its own process group, and
        # the cancellation signals that group, which the browser's session left.
        process = self.drive("hold", start_new_session=True)
        browser, stray = self.pids(process)
        self.assertNotEqual(os.getpgid(browser), process.pid)
        os.killpg(process.pid, signal.SIGTERM)
        # Both held the driver's stderr; it ends only once neither runs.
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 128 + signal.SIGTERM, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("cancelled by signal %d" % signal.SIGTERM, stderr)

    def test_a_killed_driver_leaves_its_session_holding_the_supervisors_pipe(self):
        # A driver killed outright cannot end the session; the browser still holds the
        # driver's stderr, so the supervisor cannot drain it and the cleanup stays uncertain.
        process = self.drive("deaf", start_new_session=True)
        self.addCleanup(lambda: open(self.fifo + ".release", "w").close())
        self.pids(process)
        process.kill()
        process.wait(timeout=10)
        readable, _, _ = select.select([process.stderr], [], [], 0)
        self.assertEqual(readable, [])
        open(self.fifo + ".release", "w").close()
        self.assertEqual(process.stderr.read(), "")
        process.stdout.close()
        process.stderr.close()

    def test_a_session_that_cannot_be_settled_is_unconfirmed(self):
        # A group whose members never stop: the driver reports it unconfirmed and leaves its
        # leader unreaped, so the group's id cannot come to name another group meanwhile.
        driver = load_driver()
        spawned = []

        class Recorded(subprocess.Popen):
            def __init__(self, *args, **kwargs):
                subprocess.Popen.__init__(self, *args, **kwargs)
                spawned.append(self)

        driver.subprocess = type("Subprocess", (), {"Popen": Recorded})
        driver.live_members = lambda group: True
        driver.CLEANUP_SECONDS = 0
        try:
            with self.assertRaises(driver.Unconfirmed):
                driver.measure(self.argv("paint"))
            self.assertEqual([child.returncode for child in spawned], [None])
        finally:
            for child in spawned:
                child.wait(timeout=10)
        self.assertEqual(driver.UNCONFIRMED_EXIT, clicases.DRIVER_UNCONFIRMED_EXIT)

    def test_reported_interval_rejects_untrusted_fields(self):
        valid = {"schema": "af.perf-tui-open/1", "browser_ms": "42.125",
                 "splash_ms": "5", "exit_code": 0}
        self.assertEqual([str(value) for value in clicases.tui_report(
            json.dumps(valid).encode())], ["337/8", "5"])
        for edit in ({"extra": 1}, {"browser_ms": "-1"}, {"splash_ms": "50"},
                     {"exit_code": 1}):
            doc = dict(valid, **edit)
            with self.subTest(edit=edit), self.assertRaises(ContractError):
                clicases.tui_report(json.dumps(doc).encode())

    def test_registry_setup_records_twenty_stores_through_the_sides_af(self):
        scratch = os.path.join(self.root, "scratch")
        seed = os.path.join(self.root, "seed")
        os.makedirs(os.path.join(seed, "probe-work"))
        with open(os.path.join(seed, "probe-work", "events.sqlite"), "wb") as output:
            output.write(b"seed")
        os.mkdir(scratch)
        af = os.path.join(self.root, "af")
        with open(af, "w", encoding="utf-8") as output:
            output.write("#!/bin/sh\n"
                         "test \"$1 $2 $3 $4 $5 $6 $7\" = "
                         "\"task gc --older-than 36500 --keep 1000 --state\" || exit 2\n"
                         "echo \"$8\" >>\"$XDG_STATE_HOME/calls\"\n"
                         "mkdir -p \"$XDG_STATE_HOME/af\"\n"
                         "printf '[[store]]\\n' >>\"$XDG_STATE_HOME/af/stores.toml\"\n")
        os.chmod(af, 0o700)
        environment = dict(os.environ, **clicases.tui_home_environment(scratch))
        self.assertEqual(environment["XDG_STATE_HOME"], os.path.join(scratch, "home/state"))
        result = subprocess.run(["/bin/sh", SETUP, "registry_empty", scratch, af, "-", seed],
                                env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(os.path.exists(os.path.join(scratch, "home/state/af/stores.toml")))
        result = subprocess.run(["/bin/sh", SETUP, "registry_stores_20x10000", scratch, af,
                                 "-", seed], env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        with open(os.path.join(scratch, "home/state/calls"), encoding="utf-8") as source:
            calls = source.read().splitlines()
        self.assertEqual(len(calls), 20)
        self.assertEqual(len(set(calls)), 20)
        for path in calls:
            with open(os.path.join(path, "events.sqlite"), "rb") as source:
                self.assertEqual(source.read(), b"seed")


if __name__ == "__main__":
    unittest.main()
