"""The trusted host CPU observer: frozen target identity, attachment before the gate opens.

Real processes throughout: a gated shell waits at its FIFO, a stand-in `perf` (this file's
`FAKE_PERF`) speaks perf's acknowledged control protocol and leaves when its target does, and
both run through the observer's bounded runner. The process table is a fixture directory with
the real target's PID, since `/proc` and a container are what the operator's run provides.
"""
import json
import os
import shutil
import stat
import subprocess
import sys

from perflab import contracts, hostobserver, perfcapture, toolrun
from perflab.perfcapture import CaptureError
from tests.support import TempDir

CONTAINER = "c" * 64
# perf's control protocol, `record -p` ending with its target, and a `script` of one sample.
FAKE_PERF = r'''#!/usr/bin/env python3
import os, select, sys, time
args = sys.argv[1:]
log = os.environ["OBSERVER_LOG"]
def note(text):
    fd = os.open(log, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    os.write(fd, (text + "\n").encode())
    os.close(fd)
if args[0] == "script":
    note("script " + " ".join(args[1:]))
    if "--no-inline" not in args:
        sys.exit(65)
    data = open(args[args.index("-i") + 1]).read()
    sys.stdout.write("sh 7/7 1.0: %s: \n\t    1000 loop (/bin/sh)\n\n" % data.strip())
    sys.exit(0)
control, ack = [a for a in args if a.startswith("--control=")][0][len("--control=fifo:"):].split(",")
pid = int(args[args.index("-p") + 1])
event = args[args.index("-e") + 1]
output = args[args.index("-o") + 1]
fd = os.open(control, os.O_RDONLY | os.O_NONBLOCK)
out = os.open(ack, os.O_WRONLY)
until = time.monotonic() + 20
while time.monotonic() < until:
    ready, _, _ = select.select([fd], [], [], 0.01)
    if ready:
        command = os.read(fd, 64)
        if command:
            if os.environ.get("RECYCLE"):
                with open(os.environ["RECYCLE"], "w") as stat:
                    stat.write("%d (sh) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 99999 0 0\n" % pid)
            note("ack " + command.decode().strip())
            os.write(out, b"ack\n\0")
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        break
with open(output, "w") as stream:
    stream.write(event)
note("perf ended")
'''


def proc_entry(root, pid, argv, ppid, ticks=4242, uid=None, cgroup=CONTAINER):
    path = os.path.join(root, str(pid))
    os.makedirs(path, exist_ok=True)
    with open(os.path.join(path, "stat"), "w") as stream:
        stream.write("%d (sh) S %d %d %d 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 %d 0 0\n"
                     % (pid, ppid, pid, pid, ticks))
    with open(os.path.join(path, "cmdline"), "wb") as stream:
        stream.write(b"".join(part.encode() + b"\0" for part in argv))
    with open(os.path.join(path, "status"), "w") as stream:
        stream.write("Name:\tsh\nUid:\t%d\t%d\t%d\t%d\n" % ((os.geteuid() if uid is None else uid,) * 4))
    with open(os.path.join(path, "cgroup"), "w") as stream:
        stream.write("0::/system.slice/docker-%s.scope\n" % cgroup)
    return path


class HostObserver(TempDir):
    def setUp(self):
        super().setUp()
        self.proc = self.path("proc")
        os.mkdir(self.proc)
        self.perf = self.path("perf")
        with open(self.perf, "w") as stream:
            stream.write(FAKE_PERF.replace("/usr/bin/env python3", sys.executable))
        os.chmod(self.perf, stat.S_IRWXU)
        self.log = self.path("log")
        self.env = {"PATH": "/usr/bin:/bin", "OBSERVER_LOG": self.log}
        self.init = 1000

    def lines(self):
        with open(self.log) as stream:
            return stream.read().splitlines()

    def observer(self, whole=True, container=True, env=None):
        clock = hostobserver.time.monotonic
        observer = hostobserver.Observer(
            hostobserver.bounded_start(env or self.env, self.tmp), self.perf, "task-clock:u", 99,
            "frame_pointer", self.path("rep.data"), self.tmp,
            self.path("gate-%s" % os.urandom(4).hex()), clock() + 30,
            whole, container=(lambda: (CONTAINER, self.init)) if container else None,
            proc=self.proc)
        observer.make_gate()
        return observer

    def target(self, observer, ticks=4242, cgroup=CONTAINER):
        """The contained command behind its gate, entered in the fixture process table."""
        command = ["/bin/sh", "-c", 'echo ran >>"$0"', self.log]
        # An ended process leaves the process table, as it does under /proc.
        shutil.rmtree(self.proc)
        os.mkdir(self.proc)
        process = subprocess.Popen(hostobserver.gated(command, observer.gate),
                                   stdin=subprocess.DEVNULL)
        self.addCleanup(lambda: process.poll() is None and (process.kill(), process.wait()))
        proc_entry(self.proc, self.init, ["perf-supervisor"], 0, cgroup=cgroup)
        proc_entry(self.proc, process.pid, hostobserver.gated(command, observer.gate), self.init,
                   ticks=ticks, cgroup=cgroup)
        return process

    def test_attaches_and_acknowledges_before_the_gate_lets_the_command_execute(self):
        observer = self.observer()
        process = self.target(observer)
        observer.attach(lambda: process.poll() is None)
        self.assertEqual(process.wait(10), 0)
        result = observer.stop(True)
        self.assertIsNone(result.failure())
        # perf's acknowledgement precedes the command's first instruction, and perf ended
        # of its own accord after its target, recording the declared event.
        self.assertEqual(self.lines(), ["ack enable", "ran", "perf ended"])
        with open(self.path("rep.data")) as stream:
            self.assertEqual(stream.read(), "task-clock:u")
        doc = contracts.CONTRACTS["host-observer"][2](
            observer.document("sha256:" + "a" * 64, "whole_command"))
        self.assertTrue(doc["attached_before_release"] and doc["stopped_confirmed"])
        self.assertEqual((doc["target"]["pid"], doc["target"]["start_ticks"],
                          doc["target"]["container_id"], doc["target"]["init_pid"]),
                         (process.pid, 4242, CONTAINER, self.init))
        self.assertFalse(os.path.lexists(observer.gate), "the gate FIFO is removed")
        contracts.read_json(contracts.write_bytes(doc, "host-observer"), "host-observer")

    def test_region_probe_stays_disabled_until_its_region(self):
        observer = self.observer(whole=False)
        process = self.target(observer)
        observer.attach(lambda: process.poll() is None)
        process.wait(10)
        self.assertIsNone(observer.stop(True).failure())
        # Attached and acknowledged disabled before the probe ran; its region enables it.
        self.assertEqual(self.lines(), ["ack disable", "ran", "perf ended"])

    def test_a_recycled_pid_is_refused_and_the_gate_never_opens(self):
        observer = self.observer()
        process = self.target(observer)
        observer.start = hostobserver.bounded_start(
            dict(self.env, RECYCLE=os.path.join(self.proc, str(process.pid), "stat")), self.tmp)
        with self.assertRaises(CaptureError) as caught:
            observer.attach(lambda: process.poll() is None)
        self.assertEqual(caught.exception.reason, "observer_target_changed")
        self.assertIsNone(process.poll(), "the command still waits at its closed gate")
        process.kill()
        process.wait()
        result = observer.stop(True)
        self.assertFalse(result.uncertain)
        self.assertNotIn("ran", self.lines())

    def test_identity_outside_the_owned_container_or_account_is_refused(self):
        for cgroup, uid, why in ((("d" * 64), None, "outside container"),
                                 (CONTAINER, os.geteuid() + 1, "not this account")):
            with self.subTest(why=why):
                observer = self.observer()
                process = self.target(observer, cgroup=cgroup)
                if uid is not None:
                    proc_entry(self.proc, process.pid, hostobserver.gated(
                        ["/bin/sh", "-c", 'echo ran >>"$0"', self.log], observer.gate),
                        self.init, uid=uid)
                with self.assertRaises(CaptureError) as caught:
                    observer.attach(lambda: process.poll() is None)
                self.assertEqual(caught.exception.reason, "observer_target_refused")
                self.assertIn(why, caught.exception.detail)
                self.assertIsNone(observer.stop(True))
                process.kill()
                process.wait()

    def test_a_run_that_ends_before_its_target_is_observed_refuses(self):
        observer = self.observer()
        with self.assertRaises(CaptureError) as caught:
            observer.attach(lambda: False)
        self.assertEqual(caught.exception.reason, "observer_target_missing")

    def test_probe_attaches_records_and_decodes_an_owned_gated_process(self):
        """The actual capability: the configured perf attached by PID behind the same gate."""
        bounded = hostobserver.bounded_start(self.env, self.tmp)

        def start(argv, seconds, deadline, file_bytes, cap=64 * 1024):
            running = bounded(argv, seconds, deadline, file_bytes, cap)
            if hostobserver.is_gated(argv):
                shutil.rmtree(self.proc)
                os.mkdir(self.proc)
                proc_entry(self.proc, running.process.pid, argv, 1)
            return running
        available, reason, detail = hostobserver.probe(
            start, self.perf, "task-clock:u", 99, self.tmp,
            hostobserver.time.monotonic() + 30, proc=self.proc)
        self.assertEqual((available, reason), (True, None), detail)
        self.assertIn("1 task-clock:u samples", detail)
        # The proof decodes as an accepted capture does: function-level, the same fields.
        scripts = [line for line in self.lines() if line.startswith("script ")]
        self.assertEqual(len(scripts), 1, scripts)
        self.assertEqual(scripts[0].split()[1:], perfcapture.script_argv(
            "perf", os.path.join(self.tmp, "observer.data"))[2:])
        # A perf that cannot record is the refusal, never a guess from a path or sysctl.
        with open(self.perf, "w") as stream:
            stream.write("#!/bin/sh\necho 'perf_event_open: Operation not permitted' >&2\nexit 1\n")
        os.mkdir(self.path("again"))
        available, reason, detail = hostobserver.probe(
            start, self.perf, "task-clock:u", 99, self.path("again"),
            hostobserver.time.monotonic() + 30, proc=self.proc)
        self.assertFalse(available)
        self.assertIn(reason, ("control_closed", "observer_exited", "control_timeout"))

    def test_bounded_runner_ends_its_whole_group_at_its_deadline(self):
        running = hostobserver.bounded_start(self.env, self.tmp)(
            ["/bin/sh", "-c", "sleep 30 & sleep 30"], 0.2,
            hostobserver.time.monotonic() + 0.2, None)
        result = running.wait()
        self.assertTrue(result.timed_out)
        self.assertFalse(result.uncertain)
        self.assertIn("timed out", result.failure())
        late = hostobserver.bounded_start(self.env, self.tmp)(
            ["/bin/true"], 1, hostobserver.time.monotonic() - 1, None)
        self.assertIsInstance(late, toolrun.Result)

    def control_dir(self, tag):
        path = self.path("control-%s" % tag)
        os.mkdir(path)
        return path

    def test_a_long_diagnostic_window_admits_perf_for_the_commands_own_bounded_lifetime(self):
        # The real supervisor request admission (`toolrun.start`) with 1404 s of the diagnostic
        # window left: perf is admitted for the command's bound plus its allowances, never the
        # whole window, which the 120-s per-run hard cap refuses.
        from unittest import mock
        admitted = []
        clock = hostobserver.time.monotonic

        def running(supervisor, argv, doc, deadline, clock, file_bytes, watch, binds):
            admitted.append(doc["timeout_ms"])
            return hostobserver.Bounded(argv, self.env, self.tmp, doc["timeout_ms"] / 1000.0,
                                        64 * 1024, clock)
        supervisor = toolrun.Supervisor(self.path("no-supervisor"))
        for seconds, expected in ((None, None), (hostobserver.lifetime(10), 50000)):
            observer = hostobserver.Observer(
                hostobserver.supervised_start(supervisor, self.env, self.tmp), self.perf,
                "task-clock:u", 99, "frame_pointer", self.path("rep-%s.data" % expected),
                self.control_dir(expected), self.path("gate-%s" % os.urandom(4).hex()), clock() + 1404, True,
                container=lambda: (CONTAINER, self.init), proc=self.proc, seconds=seconds)
            observer.make_gate()
            process = self.target(observer)
            with mock.patch.object(toolrun, "Running", running):
                if expected is None:
                    # Before the per-command lifetime: the whole window is refused.
                    with self.assertRaises(CaptureError) as caught:
                        observer.attach(lambda: process.poll() is None)
                    self.assertEqual(caught.exception.reason, "observer_refused")
                    self.assertIn("at most 120 s", caught.exception.detail)
                    self.assertIn("at most 120 s", observer.document("sha256:" + "a" * 64,
                                                                     "whole_command")["detail"])
                    process.kill()
                    process.wait()
                    observer.stop(False)
                    continue
                observer.attach(lambda: process.poll() is None)
            self.assertEqual(process.wait(10), 0)
            self.assertIsNone(observer.stop(True).failure())
        self.assertEqual(len(admitted), 1)
        self.assertLessEqual(admitted[0], expected)
        self.assertGreater(admitted[0], 49000)
        # The hard cap holds whatever the command's bound.
        self.assertEqual(hostobserver.lifetime(120), 120.0)

    def test_attach_argv_records_the_frozen_pid_disabled_until_enabled(self):
        control = perfcapture.PerfControl(self.tmp)
        try:
            argv = hostobserver.attach_argv("/opt/perf", "task-clock:u", 99, "dwarf",
                                            "/out", control, 4321, max_bytes=8192)
        finally:
            control.close()
        self.assertEqual(argv[-2:], ["-p", "4321"])
        self.assertIn("--delay=-1", argv)
        self.assertIn("dwarf,32768", argv)
        self.assertIn("--max-size=8K", argv)
        self.assertNotIn("--", argv)
        self.assertEqual(json.loads(json.dumps(argv)), argv)


class HostObservedProfile(TempDir):
    """The diagnostic driver in host-observer mode: the trusted tools run on the host under the
    host supervisor and bind nothing; the gated candidate command binds the image's own shell
    never, and perf is never bound into the candidate's container."""

    def profile(self):
        from perflab import diagnostics
        profile = object.__new__(diagnostics.Profile)
        profile.tools = {"perf": ({"executable_sha256": "sha256:" + "a" * 64}, "/opt/perf"),
                         "inferno_collapse_perf": ({}, "/opt/inferno/inferno-collapse-perf")}
        profile.execution = {"mode": diagnostics.LINUX_CONTAINER}
        profile.supervisor, profile.host = "contained", "host"
        profile.budget = type("B", (), {"deadline": None})()
        profile.clock = hostobserver.time.monotonic
        profile.retained, profile.work, profile.stage = [], self.tmp, self.tmp
        profile.output = self.tmp
        return profile

    def test_trusted_tools_run_on_the_host_and_the_candidate_stays_contained(self):
        from unittest import mock
        from perflab import diagnostics
        profile = self.profile()
        seen = []

        def run(supervisor, argv, env, timeout, cap, **kw):
            seen.append((supervisor, argv[0], kw["binds"]))
            return toolrun.Result(argv, code=0)
        with mock.patch.object(diagnostics.toolrun, "run", run):
            profile.tool_run(["/opt/perf", "script", "-i", "x"], 1, 64, self.tmp)
            profile.tool_run([toolrun.SHELL, "-c", "loop", "/opt/perf", self.tmp], 1, 64, self.tmp)
            profile.tool_run(["/opt/inferno/inferno-collapse-perf", "x"], 1, 64, self.tmp)
            profile.tool_run(hostobserver.gated(["/af", "--help"], self.path("gate")), 1, 64,
                             self.tmp, reads=("/af",), writes=(self.tmp,))
        self.assertEqual([(s, b) for s, _, b in seen[:3]], [("host", ())] * 3)
        supervisor, program, binds = seen[3]
        self.assertEqual((supervisor, program), ("contained", toolrun.SHELL))
        bound = [path for path, _ in binds]
        self.assertIn("/af", bound)
        self.assertNotIn(toolrun.SHELL, bound, "the image's own shell is never replaced")
        self.assertNotIn("/opt/perf", bound, "perf is never bound into the candidate container")


class ObserverCallbackFailure(TempDir):
    """`Profile.tool_run` when the observer callback fails after the contained run started: the
    run is cancelled through its supervisor and its settled Result decides what is kept."""

    def profile(self):
        from perflab import diagnostics
        profile = object.__new__(diagnostics.Profile)
        profile.tools = {"perf": ({"executable_sha256": "sha256:" + "a" * 64}, "/opt/perf")}
        profile.execution = {"mode": diagnostics.TRUSTED_LOCAL}
        profile.supervisor, profile.host = "contained", None
        profile.budget = type("B", (), {"deadline": None})()
        profile.clock = hostobserver.time.monotonic
        profile.retained, profile.work, profile.stage = [], self.tmp, self.tmp
        profile.output = self.tmp
        return profile

    def failed(self, profile, result, wait_error=None, refusal=None):
        from unittest import mock
        from perflab import diagnostics
        cancelled = []

        class Running(object):
            def done(self):
                return False

            def cancel(self):
                cancelled.append(True)

            def wait(self, timeout=None):
                if wait_error is not None:
                    raise wait_error
                return result

        def observe(running):
            raise refusal or CaptureError("observer_refused", "host perf did not start: a "
                                          "supervised run is bounded by at most 120 s")
        with mock.patch.object(diagnostics.toolrun, "start", lambda *a, **k: Running()):
            try:
                profile.tool_run(["/af", "--help"], 1, 64, self.tmp, observe=observe)
            except BaseException as error:
                self.assertEqual(cancelled, [True], "the contained run is cancelled")
                return error
        self.fail("the observer failure is raised")

    def test_a_refused_observer_with_confirmed_cancellation_keeps_its_cause_and_result(self):
        profile = self.profile()
        settled = toolrun.Result(["/af"], code=-15)
        error = self.failed(profile, settled)
        self.assertEqual(error.reason, "observer_refused")
        self.assertIs(error.settled, settled, "the confirmed Result reaches the caller")
        self.assertEqual(profile.retained, [], "a confirmed end keeps nothing for recovery")

    def test_an_unconfirmed_cancellation_fences_and_keeps_the_primary_cause_first(self):
        profile = self.profile()
        error = self.failed(profile, toolrun.Result(["/af"], code=-9, uncertain=True))
        self.assertEqual(error.reason, "cleanup_uncertain")
        self.assertTrue(error.detail.startswith("observer_refused: host perf did not start"),
                        error.detail)
        self.assertIn("tool cleanup unconfirmed", error.detail)
        self.assertEqual(profile.retained, [os.path.abspath(self.tmp)])

    def test_an_opaque_failure_is_never_proof_of_cleanup(self):
        profile = self.profile()
        error = self.failed(profile, None, wait_error=RuntimeError("supervisor reply lost"))
        self.assertEqual(error.reason, "cleanup_uncertain")
        # The callback's cause stands first; the end the supervisor never confirmed fences.
        self.assertTrue(error.detail.startswith("observer_refused: "), error.detail)
        self.assertIn("tool cleanup unconfirmed", error.detail)
        self.assertEqual(profile.retained, [os.path.abspath(self.tmp)])

    def test_a_cleanup_after_a_failed_repetition_keeps_the_primary_cause(self):
        from unittest import mock
        profile = self.profile()
        primary = CaptureError("observer_refused", "host perf did not start")
        uncertain = CaptureError("cleanup_uncertain", "scratch kept for recovery")
        with mock.patch.object(type(profile), "collect", side_effect=uncertain):
            with self.assertRaises(CaptureError) as caught:
                profile.settle_repetition(None, "captures/x", primary)
        self.assertEqual(caught.exception.reason, "cleanup_uncertain")
        self.assertEqual(caught.exception.detail, "observer_refused: host perf did not start; "
                         "cleanup after it: scratch kept for recovery")
        refused = CaptureError("capture_unadmitted", "late")
        with mock.patch.object(type(profile), "collect", side_effect=refused):
            # A secondary refusal that is not an unconfirmed cleanup leaves the primary standing.
            profile.settle_repetition(None, "captures/x", primary)
        with mock.patch.object(type(profile), "collect", side_effect=uncertain):
            with self.assertRaises(CaptureError) as caught:
                profile.settle_repetition(None, "captures/x", None)
        self.assertIs(caught.exception, uncertain)
