"""The PTY driver and its private registry fixture."""

import contextlib
import errno
import importlib.util
import io
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

# A stand-in for the browser. `tcdrain` returns once what was written left the terminal's
# output queue, so every sequence reaches the driver in the order written; it does not say the
# driver read one before the next was written, so what needs two reads apart is proved on the
# driver's `Screen` with chunks of the test's own. `ready` writes the pids to the FIFO the
# test reads, the test's only synchronization; `record` names the session leaders the test
# releases itself when the driver's own cleanup could not.
BROWSER = (
    "import fcntl, os, signal, struct, sys, termios, time, tty\n"
    "mode = sys.argv[1]\n"
    "def record(*pids):\n"
    "    with open(sys.argv[0] + '.pids', 'a') as kept:\n"
    "        kept.write(''.join('%d\\n' % pid for pid in pids))\n"
    "record(os.getpid())\n"
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
    "if mode not in ('late', 'escape-first'):\n"
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
    "elif mode == 'erased-split':\n"
    "    paint(b'\\x1b[30;1HNORMAL')\n"
    "    paint(b'\\x1b[2J')\n"
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
    "elif mode in ('escape', 'hidden', 'escape-first'):\n"
    "    done_r, done_w = os.pipe()\n"
    "    escaped = os.fork()\n"
    "    if escaped == 0:\n"
    "        os.setsid()\n"
    "        os.closerange(0, done_w)\n"
    "        os.closerange(done_w + 1, 1024)\n"
    "        if mode == 'hidden':\n"
    "            import ctypes\n"
    "            assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0\n"
    "        os.write(done_w, b'x')\n"
    "        os.close(done_w)\n"
    "        time.sleep(30)\n"
    "        os._exit(0)\n"
    "    os.close(done_w)\n"
    "    assert os.read(done_r, 1) == b'x'\n"
    "    os.close(done_r)\n"
    "    assert os.getsid(escaped) != os.getsid(0)\n"
    "    record(escaped)\n"
    "    ready(os.getpid(), escaped)\n"
    "    paint(b'\\x1b[2J\\x1b[30;1HNORMAL')\n"
    "elif mode == 'orphan':\n"
    "    stray = os.fork()\n"
    "    if stray == 0:\n"
    "        os.setsid()\n"
    "        os.execve('/bin/sleep', ['sleep', '30'], {})\n"
    "    record(stray)\n"
    "    sys.exit(1)\n"
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
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.root], timeout=30))
        self.addCleanup(self.release_recorded)
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
        """Whatever a failed step left of the driver: asked to end its session (SIGTERM, which
        its cleanup answers within its bounds), killed with its group when it led one if it
        did not, reaped, its pipes closed."""
        if process.poll() is None:
            self.signal(process, session, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.signal(process, session, signal.SIGKILL)
                process.wait(timeout=10)
        for stream in (process.stdout, process.stderr):
            stream.close()

    def signal(self, process, session, number):
        try:
            if session:
                os.killpg(process.pid, number)
            else:
                process.send_signal(number)
        except ProcessLookupError:
            pass

    def release(self, pid):
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def reap_within(self, pid, seconds):
        """Reap child `pid`, polling without blocking, within `seconds`; one that still runs
        at the deadline is killed, and one not reaped within another such bound fails the
        test rather than hold it."""
        for killed in (False, True):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if os.waitpid(pid, os.WNOHANG)[0]:
                    return
                time.sleep(0.01)
            if killed:
                self.fail("pid %d was not reaped within %s s of its kill" % (pid, seconds))
            self.release(pid)

    def recorded(self):
        """The pids the browser recorded, in order: itself first, then what it started that
        leads a session of its own."""
        try:
            with open(self.browser + ".pids", encoding="utf-8") as kept:
                return [int(line) for line in kept.read().split()]
        except OSError:
            return []

    def release_recorded(self):
        """Whatever the browser recorded that still leads a session, with its group: the
        fallback for a driver that could not end the session itself."""
        for pid in self.recorded():
            try:
                leader = os.getsid(pid) == pid
            except OSError:
                continue
            if leader:
                try:
                    os.killpg(pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass

    def stderr_so_far(self, process, seconds=1):
        """What the driver wrote to its stderr so far, read for at most `seconds` so that a
        process left holding the pipe, or writing to it without end, cannot hold the test."""
        source = process.stderr.fileno()
        chunks = []
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([source], [], [], max(deadline - time.monotonic(), 0))
            if not ready:
                break
            data = os.read(source, 65536)
            if not data:
                break
            chunks.append(data)
        return b"".join(chunks).decode("utf-8", "replace")

    def read_to_end(self, source, seconds):
        """Everything descriptor `source` delivers until its end, which must come within
        `seconds`: a pipe a process holds open past that fails the test rather than hold it."""
        chunks = []
        deadline = time.monotonic() + seconds
        while True:
            remaining = deadline - time.monotonic()
            ready, _, _ = select.select([source], [], [], max(remaining, 0))
            if not ready or remaining <= 0:
                self.fail("the pipe did not end within %s s" % seconds)
            data = os.read(source, 65536)
            if not data:
                return b"".join(chunks).decode("utf-8", "replace")
            chunks.append(data)

    def consume(self, number):
        """A pending `number` a test left blocked, taken before its handler is restored."""
        if number in signal.sigpending():
            signal.sigwait({number})

    def run_driver(self, mode):
        process = self.drive(mode)
        stdout, stderr = process.communicate(timeout=10)
        return process.returncode, stdout, stderr

    def pids(self, process):
        """The pids the browser wrote to the readiness FIFO, within 10 s; each is killed at
        cleanup if it is still there. A driver that exited fails the wait only when a read of
        the FIFO made after its exit still holds no complete line: the browser may have written
        its pids just before the driver ended."""
        fifo = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        line = b""
        deadline = time.monotonic() + 10
        try:
            while True:
                exited = process.poll() is not None
                ready, _, _ = select.select([fifo], [], [], 0 if exited else 0.05)
                data = os.read(fifo, 4096) if ready else b""
                line += data
                if b"\n" in line:
                    break
                if exited:
                    self.fail("the driver exited %d before the browser was ready: %s"
                              % (process.returncode, self.stderr_so_far(process)))
                if time.monotonic() >= deadline:
                    self.fail("the browser was not ready within 10 s")
                if not data:
                    time.sleep(0.02)  # no writer has opened the FIFO yet
        finally:
            os.close(fifo)
        pids = [int(pid) for pid in line.split()]
        for pid in pids:
            self.addCleanup(self.release, pid)
        return pids

    PS = ["ps", "-o", "stat=", "-p"]

    def state(self, pid, seconds=10):
        """`pid`'s process state as `ps` prints it; empty once it is gone. `ps` has `seconds`
        to answer: one that does not fails the test rather than hold it."""
        try:
            return subprocess.run(self.PS + [str(pid)], stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True,
                                  timeout=seconds).stdout.strip()
        except subprocess.TimeoutExpired:
            self.fail("ps did not report pid %d within %s s" % (pid, seconds))

    def assert_not_running(self, pid):
        """`pid` is gone or a zombie its parent has yet to reap; within 2 s, since the
        reparenting and reaping of a killed orphan are the init's to time."""
        deadline = time.monotonic() + 2
        while True:
            state = self.state(pid, max(deadline - time.monotonic(), 0.1))
            if state == "" or state.startswith("Z"):
                return
            if time.monotonic() >= deadline:
                self.fail("pid %d still runs in state %r" % (pid, state))
            time.sleep(0.02)

    def test_a_blocked_process_query_cannot_outlive_its_bound(self):
        # `state` gives `ps` a bound: one that never answers fails the test within it, so
        # `assert_not_running`'s own deadline holds whatever `ps` does.
        self.PS = ["/bin/sh", "-c", "exec sleep 30", "sh"]
        started = time.monotonic()
        with self.assertRaisesRegex(AssertionError, "did not report"):
            self.state(os.getpid(), 0.2)
        self.assertLess(time.monotonic() - started, 5)

    def test_a_pipe_written_without_end_cannot_hold_the_stderr_read(self):
        # `stderr_so_far` reads for its bound and no longer, whatever keeps writing; a pipe
        # that never ends fails `read_to_end` within its bound.
        writer = subprocess.Popen(["/bin/sh", "-c", "while :; do echo x >&2; done"],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  start_new_session=True)
        self.addCleanup(self.end, writer, True)
        started = time.monotonic()
        self.assertIn("x", self.stderr_so_far(writer, 0.3))
        self.assertLess(time.monotonic() - started, 3)
        started = time.monotonic()
        with self.assertRaisesRegex(AssertionError, "did not end"):
            self.read_to_end(writer.stderr.fileno(), 0.3)
        self.assertLess(time.monotonic() - started, 3)

    def test_first_status_line_and_missing_frame(self):
        code, stdout, stderr = self.run_driver("paint")
        self.assertEqual(code, 0, stderr)
        result = json.loads(stdout)
        self.assertEqual(result["schema"], "af.perf-tui-open/1")
        self.assertEqual(result["exit_code"], 0)
        # The splash and the frame may reach one read: the terminal keeps no write apart.
        self.assertGreaterEqual(float(result["browser_ms"]), float(result["splash_ms"]))
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
        # The only NORMAL precedes the clear, in one write: no browser frame is on the screen,
        # whether the driver read the two together or apart.
        code, stdout, stderr = self.run_driver("erased")
        self.assertEqual(code, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertRegex(stderr, "before browser status-line frame|frame erased before exit")

    def test_a_status_line_a_clear_in_a_later_read_erased_never_counts(self):
        # The status line and the clear that erases it are two writes, so the driver may well
        # read them apart: it reads the terminal through the browser's exit and refuses the
        # sample either way.
        code, stdout, stderr = self.run_driver("erased-split")
        self.assertEqual(code, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertRegex(stderr, "before browser status-line frame|frame erased before exit")

    def test_a_process_listing_that_fails_leaves_the_cleanup_unconfirmed(self):
        # A `/proc` of the test's own that cannot be listed stands in for Linux's: no
        # settlement can be confirmed, so the driver exits 3 with no document, the exit the
        # runner keeps a sample's scratch for (`DRIVER_UNCONFIRMED_EXIT`).
        if os.geteuid() == 0:
            self.skipTest("root lists every directory")
        driver = self.loaded()
        proc = os.path.join(self.root, "proc")
        os.makedirs(os.path.join(proc, "self", "task"))
        os.chmod(proc, 0o100)
        self.addCleanup(os.chmod, proc, 0o700)
        driver.PROC = proc
        code, stdout, stderr = self.run_main(driver, "paint")
        self.assertEqual(code, clicases.DRIVER_UNCONFIRMED_EXIT, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("the process listing failed", stderr)

    def loaded(self):
        """The driver module for a run of `main` in this process, its signal handlers restored
        after the test; a test patches what it needs on it."""
        driver = load_driver()
        handlers = {number: signal.getsignal(number) for number in driver.SIGNALS}
        self.addCleanup(lambda: [signal.signal(n, h) for n, h in handlers.items()])
        return driver

    def run_main(self, driver, mode):
        """`main` of `driver` on the browser in `mode`, in this process: (exit code, stdout,
        stderr)."""
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = driver.main(self.argv(mode))
        return code, stdout.getvalue(), stderr.getvalue()

    def test_a_member_the_settlement_finds_refuses_the_sample_whatever_a_probe_before_said(self):
        # A member can be born between two listings of the group, its parent gone before the
        # first read it: the settlement's own probes decide, not one probe before it. The first
        # probe after the browser's exit finds a member here, the kill's does not; the sample
        # is refused after the kill, with no document.
        driver = self.loaded()
        actual = driver.live_members
        probes = []

        def live_members(group):
            probes.append(group)
            return len(probes) == 1 or actual(group)

        driver.live_members = live_members
        code, stdout, stderr = self.run_main(driver, "paint")
        self.assertEqual(code, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("outlived it; it was killed", stderr)
        self.assertGreaterEqual(len(probes), 2)

    def test_a_session_whose_exit_cannot_be_watched_is_still_ended_and_cleared(self):
        # The watcher of the leader's exit is refused (a kqueue the process limit denies, say):
        # the session is killed all the same, its group settled with the leader among the live
        # members until it exits, the leader reaped, and what left the session found by its
        # token and killed. The refusal is the ordinary failure, with no document, and nothing
        # of the browser's is left. The refusal waits for the browser's readiness, within 10 s
        # and while the browser runs (the real watcher says whether it exited), so that the
        # descendant that left its session exists when the cleanup begins; the browser paints
        # nothing before it (`escape-first`), since `tcdrain` on this platform's terminal
        # waits for a read of the driver's that comes after the watcher.
        self.assert_refused_watcher_fails_and_clears()

    @unittest.skipUnless(os.path.isdir("/proc/self/task"),
                         "an undumpable process keeps its environment from its user on Linux")
    def test_a_refused_watcher_still_knows_the_browsers_start_for_its_cleanup(self):
        # A process of this user's that started before the browser and made itself undumpable,
        # so that its environment cannot be read: the browser's start, recorded before the
        # exit watcher, tells the cleanup it is not the browser's doing, so the refused watcher
        # ends in the ordinary failure, not an unconfirmed cleanup (issue #477).
        if os.geteuid() == 0:
            self.skipTest("root reads every environment")
        older = subprocess.Popen(
            [sys.executable, "-I", "-c",
             "import ctypes, sys, time\n"
             "assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0\n"
             "sys.stdout.write('x\\n')\n"
             "sys.stdout.flush()\n"
             "time.sleep(60)\n"],
            stdout=subprocess.PIPE)
        self.addCleanup(older.stdout.close)
        self.addCleanup(self.reap_within, older.pid, 10)
        self.addCleanup(self.release, older.pid)
        self.assertEqual(self.read_line(older.stdout.fileno(), 10), b"x\n")
        with self.assertRaises(PermissionError):
            open("/proc/%d/environ" % older.pid, "rb").close()
        # The browser's start must fall on a later clock tick than this process's.
        with open("/proc/%d/stat" % older.pid, "rb") as source:
            stat = source.read()
        start = int(stat[stat.rindex(b")") + 2:].split()[19])
        ticks = os.sysconf("SC_CLK_TCK")
        deadline = time.monotonic() + 10
        while time.clock_gettime(time.CLOCK_BOOTTIME) * ticks < start + 1:
            self.assertLess(time.monotonic(), deadline, "the clock did not pass a tick")
            time.sleep(0.001)
        self.assert_refused_watcher_fails_and_clears()

    def read_line(self, source, seconds):
        """One line from descriptor `source`, which must come within `seconds`."""
        line = b""
        deadline = time.monotonic() + seconds
        while b"\n" not in line:
            remaining = deadline - time.monotonic()
            ready, _, _ = select.select([source], [], [], max(remaining, 0))
            if not ready or remaining <= 0:
                self.fail("no line within %s s" % seconds)
            data = os.read(source, 4096)
            if not data:
                self.fail("the pipe ended before a line")
            line += data
        return line

    def assert_refused_watcher_fails_and_clears(self):
        """The browser in `escape-first` mode under an `Exit` that is refused once the browser
        is ready: the ordinary failure, no document, and neither the browser nor the process
        that left its session left running."""
        driver = self.loaded()
        pids = []
        fifo = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, fifo)

        class Refused(driver.Exit):
            def __init__(self, pid):
                super().__init__(pid)
                line = b""
                deadline = time.monotonic() + 10
                while b"\n" not in line:
                    if self.seen():
                        raise RuntimeError("the browser exited before it was ready")
                    if time.monotonic() >= deadline:
                        raise RuntimeError("the browser was not ready within 10 s")
                    ready, _, _ = select.select([fifo], [], [], 0.05)
                    line += os.read(fifo, 4096) if ready else b""
                pids.extend(int(word) for word in line.split())
                raise OSError(errno.EMFILE, "too many open files")

        driver.Exit = Refused
        code, stdout, stderr = self.run_main(driver, "escape-first")
        self.assertEqual(code, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("too many open files", stderr)
        self.assertEqual(len(pids), 2, pids)
        browser, escaped = pids
        self.assert_not_running(escaped)
        self.assert_not_running(browser)

    def test_a_watcher_that_fails_once_the_browser_ran_still_ends_and_clears_the_session(self):
        # The watcher registered, and fails on every read once the browser painted its frame
        # (its kqueue or waitid refuses): the quit wait fails, and the cleanup gives the
        # watcher up rather than fail on it again, settles the group, reaps the leader and
        # finds what left the session by its token. The ordinary failure, no document, and
        # nothing of the browser's left.
        driver = self.loaded()
        fifo = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, fifo)

        class Failing(driver.Exit):
            def seen(self):
                raise OSError(errno.EIO, "the exit watcher cannot be read")

        driver.Exit = Failing
        code, stdout, stderr = self.run_main(driver, "escape")
        self.assertEqual(code, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("the exit watcher cannot be read", stderr)
        browser, escaped = [int(pid) for pid in os.read(fifo, 4096).split()]
        self.assert_not_running(escaped)
        self.assert_not_running(browser)

    def test_the_interval_ends_when_the_status_line_arrives_not_at_an_earlier_clear(self):
        # The first output is a clear and a splash; the status line follows without another
        # clear. Timed from that clear, the browser would equal the splash; it ends when the
        # status line reaches the terminal. On the terminal the two writes may reach one
        # read, so the arrivals apart are proved on the screen below with chunks of the
        # test's own: a clear and a splash at 5 and the status line at 12 end at 12.
        code, stdout, stderr = self.run_driver("late")
        self.assertEqual(code, 0, stderr)
        browser, splash = clicases.tui_report(stdout.encode())
        self.assertGreaterEqual(browser, splash)
        driver = load_driver()
        status = b"\x1b[30;1H\x1b[0;7mNORMAL  browser"
        for name, chunks, first, frame in (
                ("apart", [(b"splash", 5), (driver.CLEAR, 7), (status, 12)], 5, 12),
                ("late clear", [(driver.CLEAR + b"splash", 5), (status, 12)], 5, 12),
                ("one read", [(b"splash" + driver.CLEAR + status, 9)], 9, 9),
                ("split line", [(status[:10], 3), (status[10:], 8)], 3, 8),
                ("erased", [(status + driver.CLEAR, 4), (b"more", 6)], 4, None),
                ("erased in a later read", [(status, 11), (driver.CLEAR, 12)], 11, None),
                ("repainted after a clear", [(status, 4), (driver.CLEAR, 6), (status, 8)],
                 4, 8)):
            with self.subTest(name):
                screen = driver.Screen()
                for data, at in chunks:
                    screen.feed(data, at)
                self.assertEqual((screen.first, screen.browser), (first, frame))

    def test_the_proc_listing_skips_the_gone_and_other_users_and_refuses_what_it_cannot_read(
            self):
        # A `/proc` of the test's own: one environment carries the token, one another's, one
        # is root's to read (another user's process), one this user's that started before the
        # browser, one is gone. A process of this user's that started no earlier than the
        # browser and whose environment cannot be read leaves the listing unconfirmed.
        if os.geteuid() == 0:
            self.skipTest("root reads every environment")
        driver = load_driver()
        proc = os.path.join(self.root, "proc")
        uid = os.getuid()

        def process(pid, environ, owner, started, readable=True, state=b"S", vsize=4096):
            os.makedirs(os.path.join(proc, str(pid)))
            with open(os.path.join(proc, str(pid), "status"), "wb") as output:
                output.write(b"Name:\tx\nUid:\t%d\t%d\t%d\t%d\n" % ((owner,) * 4))
            with open(os.path.join(proc, str(pid), "stat"), "wb") as output:
                output.write(b"%d (x y) %s 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 %d %d 0\n"
                             % (pid, state, started, vsize))
            if environ is not None:
                with open(os.path.join(proc, str(pid), "environ"), "wb") as output:
                    output.write(environ)
                if not readable:
                    os.chmod(os.path.join(proc, str(pid), "environ"), 0)

        marker = b"AF_PERF_TUI_OPEN_SESSION=t0ken"
        process(101, b"A=1\0" + marker + b"\0", uid, 500)
        process(102, b"A=1\0AF_PERF_TUI_OPEN_SESSION=other\0", uid, 500)
        process(103, b"", 0, 500, readable=False)
        process(104, b"", uid, 300, readable=False)
        process(105, None, uid, 500)
        self.assertEqual(driver.proc_listed(marker, set(), 400, proc), {101})
        self.assertEqual(driver.proc_listed(marker, {101}, 400, proc), set())
        # One that exited, awaiting its parent's reap, and one on its way out without an
        # address space any more: Linux owns their environments as root's, and they run
        # nothing, so neither is suspect whenever it started.
        process(110, b"", uid, 500, readable=False, state=b"Z", vsize=0)
        process(111, b"", uid, 500, readable=False, state=b"R", vsize=0)
        self.assertEqual(driver.proc_listed(marker, set(), 400, proc), {101})
        process(106, b"", uid, 400, readable=False)
        with self.assertRaisesRegex(driver.Unconfirmed, "process 106, this user's, cannot"):
            driver.proc_listed(marker, set(), 400, proc)
        # Without the browser's start, what this user's started before it is suspect too.
        with self.assertRaisesRegex(driver.Unconfirmed, "process 10[46], this user's, cannot"):
            driver.proc_listed(marker, set(), None, proc)

        def drop(pid):
            for name in os.listdir(os.path.join(proc, str(pid))):
                os.chmod(os.path.join(proc, str(pid), name), 0o600)
            subprocess.run(["rm", "-r", os.path.join(proc, str(pid))], check=True, timeout=30)

        # An owner or a start that cannot be read, and a listing that cannot be made, leave
        # the cleanup unconfirmed; so does a status that names no uid.
        drop(106)
        process(107, b"", uid, 500, readable=False)
        os.chmod(os.path.join(proc, "107", "stat"), 0)
        with self.assertRaisesRegex(driver.Unconfirmed, "stat of process 107"):
            driver.proc_listed(marker, set(), 400, proc)
        drop(107)
        process(108, b"", uid, 500, readable=False)
        os.chmod(os.path.join(proc, "108", "status"), 0)
        with self.assertRaisesRegex(driver.Unconfirmed, "owner of process 108"):
            driver.proc_listed(marker, set(), 400, proc)
        drop(108)
        process(109, b"", uid, 500, readable=False)
        with open(os.path.join(proc, "109", "status"), "wb") as output:
            output.write(b"Name:\tx\n")
        with self.assertRaisesRegex(driver.Unconfirmed, "owner of process 109 is not named"):
            driver.proc_listed(marker, set(), 400, proc)
        drop(109)
        self.assertEqual(driver.proc_listed(marker, set(), 400, proc), {101})
        os.chmod(proc, 0o100)
        self.addCleanup(os.chmod, proc, 0o700)
        with self.assertRaisesRegex(driver.Unconfirmed, "the process listing failed"):
            driver.proc_listed(marker, set(), 400, proc)

    def test_the_proc_listing_skips_a_zombie_whose_environment_linux_withholds(self):
        # A child that exited and is not reaped yet: Linux owns its `environ` as root's (a
        # process without an address space), so the read fails as an undumpable process's
        # would, and the listing must know the difference, as it does when it re-lists right
        # after killing what it found. (The kernel's `task_dump_owner`.)
        if not os.path.isdir("/proc/self/task"):
            self.skipTest("a Linux /proc")
        driver = load_driver()
        zombie = os.fork()
        if zombie == 0:
            os._exit(0)
        self.addCleanup(self.reap_within, zombie, 10)
        deadline = time.monotonic() + 10
        while not self.state(zombie).startswith("Z"):
            self.assertLess(time.monotonic(), deadline, "the child did not become a zombie")
            time.sleep(0.01)
        try:
            with open("/proc/%d/environ" % zombie, "rb") as source:
                self.assertEqual(source.read(), b"")
        except PermissionError:
            pass  # this kernel owns a zombie's environment as root's
        # The listing reads the real `/proc`, of the child alone: another process of this
        # user's that is live, undumpable and unreadable is not what this proves.
        driver.proc_names = lambda proc: [str(zombie)]
        self.assertEqual(driver.proc_listed(b"AF_PERF_TUI_OPEN_SESSION=t0ken", set(), None,
                                            driver.PROC), set())

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

    @unittest.skipUnless(sys.platform.startswith("linux"),
                         "an undumpable process keeps its environment from its user on Linux")
    def test_a_process_of_this_user_whose_environment_cannot_be_read_is_unconfirmed(self):
        # The escaped process made itself undumpable, so its `/proc` environment is root's to
        # read; it started after the browser, so it may be the browser's doing: the cleanup
        # is unconfirmed, no document is printed, and the test releases it.
        if os.geteuid() == 0:
            self.skipTest("root reads every environment")
        process = self.drive("hidden")
        browser, hidden = self.pids(process)
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 3, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("process %d, this user's, cannot be read" % hidden, stderr)
        self.assertNotIn(self.state(hidden), ("", "Z"))
        self.release(hidden)
        self.assert_not_running(hidden)
        self.assert_not_running(browser)

    def test_a_cancellation_during_the_settlement_waits_for_it_and_ends_the_sample(self):
        # The leader is reaped, and only then does the cancellation land: it stays pending
        # while the process that left the session is found and killed, and ends the sample
        # after that, as a cancellation, not a document.
        driver = load_driver()
        reap = driver.reap

        def reap_then_cancel(child, wait, deadline):
            code = reap(child, wait, deadline)
            os.kill(os.getpid(), signal.SIGTERM)
            return code

        driver.reap = reap_then_cancel
        self.addCleanup(signal.signal, signal.SIGTERM,
                        signal.signal(signal.SIGTERM, driver.cancelled))
        self.addCleanup(self.consume, signal.SIGTERM)
        fifo = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, fifo)
        with self.assertRaises(driver.Cancelled) as caught:
            driver.measure(self.argv("escape"))
        self.assertEqual(caught.exception.args[0], signal.SIGTERM)
        self.assertNotIn(signal.SIGTERM, signal.sigpending())
        browser, escaped = [int(pid) for pid in os.read(fifo, 4096).split()]
        self.assert_not_running(escaped)
        self.assert_not_running(browser)

    def test_a_driver_that_exits_before_readiness_fails_within_its_bound_and_releases(self):
        # The browser exits before it is ready, leaving a process of a session of its own, with
        # an environment of its own, holding the driver's stderr: the driver reports the exit
        # without finding it, the readiness wait fails without reading that pipe to its end,
        # and the browser's record is what releases the process.
        process = self.drive("orphan")
        started = time.monotonic()
        with self.assertRaises(AssertionError) as caught:
            self.pids(process)
        self.assertLess(time.monotonic() - started, 5)
        self.assertIn("before browser status-line frame", str(caught.exception))
        stray = self.recorded()[-1]
        self.assertNotIn(self.state(stray), ("", "Z"))
        self.release_recorded()
        self.assert_not_running(stray)

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
        self.assertEqual(self.read_to_end(process.stderr.fileno(), 10), "")
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
                                env=environment, capture_output=True, text=True, timeout=120)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(os.path.exists(os.path.join(scratch, "home/state/af/stores.toml")))
        result = subprocess.run(["/bin/sh", SETUP, "registry_stores_20x10000", scratch, af,
                                 "-", seed], env=environment, capture_output=True, text=True,
                                timeout=120)
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
