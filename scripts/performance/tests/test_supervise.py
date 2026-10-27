"""The supervisor's framed reply, the Linux sample container and host capabilities (ADR-0166).

Results here are constructed by the tests; the real supervisor's processes are exercised by the
Rust integration test `crates/review-process/tests/perf_supervisor.rs`.
"""
import collections
import copy
import errno
import io
import json
import os
import signal
import subprocess
import sys
import threading
import time
from unittest import mock

from perflab import capability, contracts, supervise
from perflab import suite as suite_module
from perflab.values import ContractError
from tests.support import (CLI, FAKE_SUPERVISOR_EXECUTE, ROOT, SUITE, TempDir, fake_supervisor,
                           held_child_argv, read, release_recorded)

FIXTURES = os.path.join(ROOT, "fixtures/performance/schema")


def fixture(name):
    return json.loads(read(os.path.join(FIXTURES, name)))


def probe_request(**changes):
    doc = {"argv": ["/perf/bin/perf-probe"], "environment": {"PATH": "/usr/bin:/bin"},
           "cwd": "/perf/scratch", "stdin": "{}", "timeout_ms": 30000,
           "stdout_cap": 65536, "log_cap": 262144, "containment": "pid_namespace",
           "scratch": "/perf/scratch"}
    doc.update(changes)
    return supervise.request(doc["argv"], doc["environment"], doc["cwd"], doc["stdin"],
                             doc["timeout_ms"], doc["stdout_cap"], doc["log_cap"],
                             doc["containment"], doc["scratch"])


class Framing(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.result = fixture("supervisor-result/valid-namespace.json")
        self.request = probe_request()

    def reply(self, result, payload):
        return json.dumps(result).encode("utf-8") + b"\n" + payload

    def test_a_reply_is_one_result_line_and_exactly_its_declared_bytes(self):
        result, out, err = supervise.read_reply(self.reply(self.result, b"hello\n"),
                                                self.request)
        self.assertEqual((out, err), (b"hello\n", b""))
        self.assertEqual(result["lifecycle"]["status"], "unexpected_descendants")
        for payload in (b"hello", b"hello\n!"):
            with self.subTest(payload=payload):
                with self.assertRaises(ContractError):
                    supervise.read_reply(self.reply(self.result, payload), self.request)

    def test_a_reply_beyond_the_requested_caps_or_containment_is_refused(self):
        small = probe_request(stdout_cap=4, log_cap=8)
        with self.assertRaises(ContractError) as caught:
            supervise.read_reply(self.reply(self.result, b"hello\n"), small)
        self.assertIn("caps", str(caught.exception))
        local = probe_request(containment="process_group", scratch=None)
        with self.assertRaises(ContractError):
            supervise.read_reply(self.reply(self.result, b"hello\n"), local)

    def test_a_result_line_must_be_bounded_and_valid(self):
        broken = copy.deepcopy(self.result)
        broken["lifecycle"]["status"] = "probably_fine"
        with self.assertRaises(ContractError):
            supervise.read_reply(self.reply(broken, b"hello\n"), self.request)
        with self.assertRaises(ContractError):
            supervise.read_reply(b"x" * (64 * 1024 + 2), self.request)

    def test_the_fake_supervisor_round_trips_through_launch(self):
        supervisor = fake_supervisor(self.path("supervisor"))
        order = supervise.request(["/bin/sh", "-c", "cat; echo err >&2"],
                                  {"PATH": "/usr/bin:/bin"}, self.tmp, "in\n", 10000, 16, 64,
                                  "process_group", None)
        supervised = supervise.launch([supervisor], order)
        self.assertEqual((supervised.stdout, supervised.stderr), (b"in\n", b"err\n"))
        self.assertEqual(supervise.outcome(supervised, "process_group"), (None, None, False))
        with self.assertRaises(supervise.SupervisorFailure):
            supervise.launch(["/bin/sh", "-c", "cat >/dev/null; echo nonsense"], order)

    def test_an_exit_not_observed_unreaped_stops_the_shard(self):
        result = fixture("supervisor-result/valid-unobserved.json")
        supervised = supervise.Supervised(result, b"", b"", 1, b"")
        found, detail, halt = supervise.outcome(supervised, "pid_namespace")
        self.assertEqual((found, halt), ("lifecycle_violation", True))
        self.assertIn("waitid(WNOWAIT): EIO", detail)

    def test_a_failed_enumeration_is_refused_as_clean_and_stops_the_shard_when_reported(self):
        # Root-reproduced: opendir(/proc) EIO used to come back clean with remaining null.
        claimed = copy.deepcopy(self.result)
        claimed["lifecycle"].update(status="clean", descendants_at_exit=None,
                                    remaining_after_cleanup=None)
        claimed["scratch"] = None
        with self.assertRaises(ContractError):
            contracts.validate_supervisor_result(claimed)
        reported = copy.deepcopy(claimed)
        reported["lifecycle"].update(status="cleanup_uncertain", detail=(
            "enumerating the sample's processes at the child's exit failed: listing /proc: "
            "Input/output error (os error 5)"))
        contracts.validate_supervisor_result(reported)
        found, detail, halt = supervise.outcome(
            supervise.Supervised(reported, b"", b"", 1, b""), "pid_namespace")
        self.assertEqual((found, halt), ("lifecycle_violation", True))
        self.assertIn("listing /proc: Input/output error", detail)

    def test_lifecycle_counts_must_support_their_status(self):
        cases = (
            ("held pipe", {"pipes_held": True}, "no pipe was held"),
            ("remainder", {"status": "unexpected_descendants", "remaining_after_cleanup": 2},
             "verified only when nothing remained"),
            ("uncounted", {"status": "unexpected_descendants", "descendants_at_exit": 0},
             "unexpected descendants are counted ones"),
            ("clean with live ones", {"status": "clean"}, "clean only without live"),
        )
        for name, change, error in cases:
            with self.subTest(name):
                broken = copy.deepcopy(self.result)
                broken["lifecycle"].update(change)
                broken["scratch"] = None
                with self.assertRaises(ContractError) as caught:
                    contracts.validate_supervisor_result(broken)
                self.assertIn(error, str(caught.exception))

    def test_cancellation_and_launch_failures_are_never_judged_as_output(self):
        for kind, signal, failure, stop in (("cancelled", 15, "lifecycle_violation", True),
                                            ("launch_failed", None, "launch_failure", False),
                                            ("timeout", None, "timeout", False)):
            with self.subTest(kind=kind):
                result = copy.deepcopy(self.result)
                result["termination"] = {"kind": kind, "code": None, "signal": signal,
                                         "detail": "constructed"}
                result["timing"]["child_wall_ns"] = None
                result["lifecycle"].update(status="clean", descendants_at_exit=None)
                if kind == "launch_failed":
                    result["timing"]["exit_observation"] = "none"
                    result["lifecycle"]["descendants_at_exit"] = 0
                contracts.validate_supervisor_result(result)
                supervised = supervise.Supervised(result, b"", b"", 1, b"")
                found, _, halt = supervise.outcome(supervised, "pid_namespace")
                self.assertEqual((found, halt), (failure, stop))


class HostBackstop(TempDir):
    """The host backstop of a supervised request (ADR-0170): it covers a declared setup and
    the measured child in sequence, each with its deadline and cleanup grace, and never reaches
    past the run's total deadline. Real processes: a stand-in supervisor that takes the given
    time before replying through the fake supervisor's framed protocol."""

    def request(self, setup_ms=3000, child_ms=3000, grace_ms=100):
        return supervise.request(["/bin/sh", "-c", "exit 0"], {"PATH": "/usr/bin:/bin"},
                                 self.tmp, "", child_ms, 4096, 8192, grace_ms=grace_ms,
                                 setup=(["/bin/sh", "-c", "exit 0"], {"PATH": "/usr/bin:/bin"},
                                        self.tmp, setup_ms))

    def test_exception_after_popen_preserves_cause_and_only_valid_cleanup_evidence_settles(self):
        request = self.request()
        reply = fixture("supervisor-result/valid-trusted-local.json")
        payload = json.dumps(reply).encode() + b"\n" + b"hello\n"
        # A lost initial communication can still settle via a valid cancellation reply.
        for confirmed in (True, False):
            with self.subTest(confirmed=confirmed):
                original = OSError("lost pipe after launch")
                process = mock.Mock(returncode=0)
                process.communicate.side_effect = [original, (payload, b"")] if confirmed else [
                    original, OSError("lost cancellation reply")]
                with mock.patch.object(supervise.subprocess, "Popen", return_value=process):
                    with self.assertRaises(supervise.SupervisorFailure) as caught:
                        supervise.launch(["stand-in"], request)
                failure = caught.exception
                self.assertTrue(failure.launched)
                self.assertIs(failure.__cause__, original)
                self.assertEqual(failure.uncertain, not confirmed)
                process.send_signal.assert_called_once()

    def test_callback_failure_is_owned_even_if_it_throws_a_typed_prelaunch_refusal(self):
        request = self.request()
        process = mock.Mock(returncode=None)
        process.communicate.side_effect = OSError("no reply")
        original = supervise.SupervisorFailure("callback refused", launched=False)
        with mock.patch.object(supervise.subprocess, "Popen", return_value=process):
            with self.assertRaises(supervise.SupervisorFailure) as caught:
                supervise.launch(["stand-in"], request, started=mock.Mock(side_effect=original))
        self.assertTrue(caught.exception.launched)
        self.assertTrue(caught.exception.uncertain)
        self.assertIs(caught.exception.__cause__, original)
        process.send_signal.assert_called_once()

    def test_cancellation_keeps_its_type_and_carries_typed_ownership_evidence(self):
        from perflab.budget import Interrupted
        for kind in (KeyboardInterrupt, Interrupted):
            with self.subTest(kind=kind):
                original = kind("cancelled after Popen")
                process = mock.Mock(returncode=None)
                process.communicate.side_effect = OSError("cancellation reply lost")
                with mock.patch.object(supervise.subprocess, "Popen", return_value=process):
                    with self.assertRaises(kind) as caught:
                        supervise.launch(["stand-in"], self.request(),
                                         started=mock.Mock(side_effect=original))
                self.assertIs(caught.exception, original)
                self.assertTrue(original.supervisor_failure.uncertain)
                self.assertTrue(original.supervisor_failure.launched)
                process.send_signal.assert_called_once()

    def test_exceptional_cancellation_never_waits_past_the_original_deadline(self):
        now = [0.0]
        process = mock.Mock(returncode=None)
        timeouts = []
        def communicate(*args, **kwargs):
            timeouts.append(kwargs["timeout"])
            if len(timeouts) == 1:
                now[0] = 19.98
                raise OSError("pipe failed near deadline")
            now[0] += kwargs["timeout"]
            raise subprocess.TimeoutExpired("stand-in", kwargs["timeout"])
        process.communicate.side_effect = communicate
        process.wait.side_effect = subprocess.TimeoutExpired("stand-in", 0)
        with mock.patch.object(supervise.subprocess, "Popen", return_value=process):
            with self.assertRaises(supervise.SupervisorFailure) as caught:
                supervise.launch(["stand-in"], self.request(), deadline=20,
                                 clock=lambda: now[0], cancel_margin=0)
        self.assertTrue(caught.exception.uncertain)
        self.assertLessEqual(now[0], 20)
        self.assertLess(timeouts[1], .021)
        process.wait.assert_called_once_with(timeout=0)

    def test_interruption_during_exceptional_cancellation_keeps_both_causes_and_ownership(self):
        from perflab.budget import Interrupted
        original, interruption = OSError("pipe failed"), Interrupted("cleanup deadline")
        process = mock.Mock(returncode=None)
        process.communicate.side_effect = [original, interruption]
        with mock.patch.object(supervise.subprocess, "Popen", return_value=process):
            with self.assertRaises(Interrupted) as caught:
                supervise.launch(["stand-in"], self.request())
        self.assertIs(caught.exception, interruption)
        self.assertIs(interruption.__cause__, original)
        self.assertTrue(interruption.supervisor_failure.uncertain)
        # Python 3.9 has no `add_note` (ADR-0162); the note is carried the same way there.
        self.assertEqual(original.__notes__,
                         ["Supervisor cancellation unconfirmed: cleanup deadline"])

    def slow(self, seconds):
        fake = fake_supervisor(self.path("fake-supervisor"))
        marker = self.path("launched")
        path = self.path("slow-supervisor")
        with open(path, "w") as stream:
            stream.write("#!%s -I\nimport os, time\nopen(%r, 'w').close()\ntime.sleep(%r)\n"
                         "os.execv(%r, [%r])\n" % (sys.executable, marker, seconds, fake, fake))
        os.chmod(path, 0o755)
        return path, marker

    def latch(self, marker, bounds=None):
        """A `started` hook for `launch`: it holds the bounded wait back until the stand-in has
        started (it wrote `marker`), so the bound measures the supervisor, never how long a
        loaded host takes to start an interpreter, and records each timeout `launch` waits
        with in `bounds`. Returns the hook and a list that gets the instant the latch opened."""
        opened = []

        def started(process):
            deadline = time.monotonic() + 120
            while not os.path.exists(marker):
                self.assertIsNone(process.poll(), "the stand-in ended before it started")
                self.assertLess(time.monotonic(), deadline, "the stand-in never started")
                time.sleep(0.01)
            opened.append(time.monotonic())
            if bounds is not None:
                communicate = process.communicate

                def bounded(input=None, timeout=None):
                    bounds.append(timeout)
                    return communicate(input, timeout=timeout)

                process.communicate = bounded

        return started, opened

    def test_the_backstop_covers_the_setup_and_the_child_with_their_cleanup(self):
        request = self.request(setup_ms=110000, child_ms=80000, grace_ms=5000)
        # The predecessor counted only the child: 85 s + margin, before a valid seed could
        # finish its 110 s setup and 80 s publication.
        self.assertEqual(supervise.backstop_seconds(request, margin=60),
                         110 + 5 + 80 + 5 + 60)
        alone = supervise.request([sys.executable, "-I", "-c", "pass"], {}, self.tmp, "", 80000, 1, 1, grace_ms=5000)
        self.assertEqual(supervise.backstop_seconds(alone, margin=60), 80 + 5 + 60)

    def test_a_setup_and_child_near_their_limits_are_never_cut_off(self):
        # 30.1 s each of setup and child time with their grace, and a 1 s margin: the supervisor
        # is given 61.2 s; counting the child alone would have cut it off at 31.1 s. The bound is
        # read where `launch` waits with it, after a latch on the stand-in's start, so a loaded
        # host's interpreter start never eats into it, and the stand-in replies at once, well
        # inside it.
        supervisor, marker = self.slow(0)
        bounds = []
        started, _ = self.latch(marker, bounds)
        request = self.request(setup_ms=30000, child_ms=30000)
        supervised = supervise.launch([supervisor], request, margin=1, started=started)
        self.assertEqual(supervised.result["termination"]["kind"], "exited")
        self.assertIsNotNone(supervised.result["setup"])
        self.assertEqual(len(bounds), 1)
        self.assertAlmostEqual(bounds[0], 30.1 + 30.1 + 1, places=6)
        self.assertGreater(bounds[0], 30.1 + 1)

    def test_a_supervisor_past_the_backstop_is_killed_and_reported(self):
        supervisor, _ = self.slow(30)
        with self.assertRaises(supervise.SupervisorFailure) as caught:
            supervise.launch([supervisor], self.request(), margin=1)
        self.assertIn("within 7.200 s, its backstop", str(caught.exception))

    def test_the_run_s_total_deadline_dominates_the_backstop(self):
        # 3 s remain and the cancellation reserve is 0.5 s: the bound is 2.5 s, not 67.2 s. The
        # stand-in dies of the SIGTERM before it took the signal over, so it launched nothing.
        # The stand-in has started (a latch on its marker) before the bound runs, so it is the
        # bound that ends it, whatever the host's load.
        supervisor, marker = self.slow(30)
        bounds = []
        started, opened = self.latch(marker, bounds)
        with self.assertRaises(supervise.SupervisorFailure) as caught:
            supervise.launch([supervisor], self.request(), deadline=103.0,
                             clock=lambda: 100.0, margin=60, cancel_margin=0.2,
                             started=started)
        self.assertIn("within 2.500 s, the run's total deadline", str(caught.exception))
        self.assertTrue((caught.exception.launched, caught.exception.deadline))
        self.assertFalse(caught.exception.uncertain)
        self.assertEqual(bounds[0], 2.5)
        # Ended by the bound and its signal, not after the 67.2 s backstop: a load-safe margin.
        self.assertLess(time.monotonic() - opened[0], 30)
        self.assertTrue(os.path.exists(marker))

    def test_nothing_launches_without_the_time_a_cancellation_takes(self):
        # Root-reproduced: a 0.5 s total deadline used to launch the supervisor and kill only
        # it, leaving its child running. The default reserve is three graces and 10 s.
        supervisor, marker = self.slow(0)
        for remaining in (0.5, 0.0, -5.0):
            with self.subTest(remaining=remaining):
                with self.assertRaises(supervise.SupervisorFailure) as caught:
                    supervise.launch([supervisor], self.request(grace_ms=5000),
                                     deadline=100.0 + remaining, clock=lambda: 100.0)
                failure = caught.exception
                self.assertIn("nothing was launched", str(failure))
                self.assertEqual((failure.launched, failure.deadline, failure.uncertain),
                                 (False, True, False))
                self.assertFalse(os.path.exists(marker))
        self.assertEqual(supervise.cancel_reserve_seconds(self.request(grace_ms=5000)), 25.0)

    def test_a_supervisor_that_does_not_end_its_sample_leaves_the_cleanup_uncertain(self):
        # A stand-in that took SIGTERM over, launched an owned nonspawning child in its own
        # session and never replies: it is killed after its reserve, and its child is not
        # known to have ended. The test then ends the child it owns.
        child_marker = self.path("child")
        path = self.path("deaf-supervisor")
        with open(path, "w") as stream:
            stream.write("#!%s -I\nimport signal, subprocess, sys, time\n"
                         "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                         "sys.stdin.buffer.read()\n"
                         "subprocess.Popen(%r, start_new_session=True)\n"
                         "time.sleep(120)\n" % (sys.executable, held_child_argv(child_marker)))
        os.chmod(path, 0o755)
        started = time.monotonic()
        with self.assertRaises(supervise.SupervisorFailure) as caught:
            supervise.launch([path], self.request(), margin=1, cancel_margin=0.5)
        failure = caught.exception
        try:
            self.assertLess(time.monotonic() - started, 30)
            self.assertTrue(failure.uncertain and failure.launched)
            self.assertFalse(failure.deadline)
            self.assertIn("gave no reply within 0.800 s and was killed", str(failure))
            self.assertIn("no reply confirms that its sample ended", str(failure))
            child = int(read(child_marker))
            os.kill(child, 0)
        finally:
            release_recorded(child_marker)

    def test_a_contained_launch_keeps_its_removal_time_before_the_deadline(self):
        # The cancellation reserve is 0.5 s; with a container, its removal's allowance is kept
        # free too, so 30 s left launches nothing and the bound leaves both before the deadline.
        supervisor, marker = self.slow(0)
        with self.assertRaises(supervise.SupervisorFailure) as caught:
            supervise.launch([supervisor], self.request(), deadline=130.0,
                             clock=lambda: 100.0, cancel_margin=0.2, removal_seconds=60)
        self.assertIn("cancellation and its container's removal takes; nothing was launched",
                      str(caught.exception))
        self.assertFalse(caught.exception.launched)
        self.assertFalse(os.path.exists(marker))
        supervisor, marker = self.slow(30)
        bounds = []
        started, _ = self.latch(marker, bounds)
        with self.assertRaises(supervise.SupervisorFailure) as caught:
            supervise.launch([supervisor], self.request(), deadline=163.0,
                             clock=lambda: 100.0, cancel_margin=0.2, removal_seconds=60,
                             started=started)
        self.assertIn("within 2.500 s, the run's total deadline", str(caught.exception))
        self.assertEqual(bounds[0], 2.5)
        # Whether anything may be live is the removal's to say, never assumed here.
        self.assertNotIn("no reply confirms", str(caught.exception))

    def test_a_cancelled_reply_decides_by_its_own_lifecycle(self):
        # A reply after the host's SIGTERM is never judged; whether the sample's end was
        # confirmed is its lifecycle's to say.
        result = fixture("supervisor-result/valid-trusted-local.json")
        self.assertTrue(supervise.cleanup_confirmed(result))
        uncertain = copy.deepcopy(result)
        uncertain["lifecycle"]["status"] = "cleanup_uncertain"
        self.assertFalse(supervise.cleanup_confirmed(uncertain))


class RecordedChild(TempDir):
    """A fixture's child that this test does not hold is ended by asking it, never by a signal
    to the ID its marker names: that ID may name another test's process by then (#331)."""

    def test_a_released_child_ends_by_itself_and_nothing_is_signalled(self):
        marker = self.path("marker")
        # Started by another process, as a fixture's is: launchd or init reaps it.
        subprocess.run([sys.executable, "-c", "import subprocess, sys; subprocess.Popen("
                        "sys.argv[1:], start_new_session=True)"] + held_child_argv(marker),
                       check=True)
        deadline = time.monotonic() + 30
        while not os.path.exists(marker) and time.monotonic() < deadline:
            time.sleep(0.01)
        with mock.patch.object(os, "kill") as kill, mock.patch.object(os, "killpg") as killpg:
            release_recorded(marker)
        self.assertEqual((kill.call_count, killpg.call_count), (0, 0))

    def test_a_marker_naming_a_live_process_never_gets_it_signalled(self):
        # The marker's ID names a live process this test holds, as a recycled ID would name
        # another test's: it is left running, reported, and only its holder ends it.
        held = subprocess.Popen(["sleep", "30"], start_new_session=True)
        self.addCleanup(held.wait)
        self.addCleanup(held.kill)
        marker = self.path("marker")
        with open(marker, "w") as stream:
            stream.write(str(held.pid))
        with mock.patch.object(sys, "stderr", io.StringIO()) as said:
            self.assertFalse(release_recorded(marker, timeout=0.2))
        self.assertIn("is not signalled", said.getvalue())
        self.assertIsNone(held.poll(), "the process under the marker's ID still runs")
        # A child that never recorded itself has nothing to end.
        self.assertFalse(release_recorded(self.path("never-written")))


class FakeSupervisorCancel(TempDir):
    """The fake supervisor forwards a SIGTERM to its child's group only while it holds that
    child unreaped: after the reap the group ID is free for another test's group (#331)."""

    def setUp(self):
        TempDir.setUp(self)
        self.fake = {"os": os, "signal": signal, "subprocess": subprocess,
                     "threading": threading, "time": time}
        exec(FAKE_SUPERVISOR_EXECUTE, self.fake)
        previous = signal.signal(signal.SIGTERM, self.fake["cancel"])
        self.addCleanup(signal.signal, signal.SIGTERM, previous)
        self.addCleanup(signal.pthread_sigmask, signal.SIG_UNBLOCK, {signal.SIGTERM})

    def execute(self, code):
        return self.fake["execute"]([sys.executable, "-I", "-c", code], b"",
                                    {"PATH": "/usr/bin:/bin"}, self.path(), 30000)

    def test_a_cancel_after_the_reap_signals_nothing(self):
        termination = self.execute("pass")[0]
        self.assertEqual(termination["kind"], "exited")
        self.assertIsNone(self.fake["running"][0])
        with mock.patch.object(os, "killpg") as sent:
            self.fake["cancel"](signal.SIGTERM, None)
        self.assertEqual(sent.call_count, 0)

    def test_a_sigterm_raised_during_the_reap_runs_after_the_child_is_released(self):
        reap, seen = os.waitpid, []

        def reaped_then_cancelled(pid, options):
            found = reap(pid, options)
            if found[0] == pid:
                # Pending until the reap is recorded, however it is scheduled.
                signal.pthread_kill(threading.main_thread().ident, signal.SIGTERM)
            return found

        with mock.patch.object(os, "waitpid", side_effect=reaped_then_cancelled), \
                mock.patch.object(os, "killpg", side_effect=lambda *a: seen.append(a)):
            termination = self.execute("pass")[0]
        self.assertTrue(self.fake["cancelled"][0], "the SIGTERM was handled")
        self.assertEqual(termination["kind"], "cancelled")
        self.assertEqual(seen, [], "a reaped child's group is never signalled")

    def test_a_cancel_while_the_child_runs_ends_its_group(self):
        main = threading.main_thread().ident
        timer = threading.Timer(0.3, signal.pthread_kill, (main, signal.SIGTERM))
        timer.start()
        self.addCleanup(timer.cancel)
        started = time.monotonic()
        termination = self.execute("import time; time.sleep(30)")[0]
        self.assertEqual(termination["kind"], "cancelled")
        self.assertLess(time.monotonic() - started, 20)
        self.assertIsNone(self.fake["running"][0])


class Container(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.env = suite_module.load(SUITE).doc["environments"]["linux_x86_64_gnu_hosted"]

    def test_the_sample_container_carries_every_declared_constraint(self):
        argv = supervise.container_argv(
            "docker", "af-perf-run-0-1", "registry.invalid/af-perf@sha256:" + "0" * 64,
            self.env, "/run/build/artifacts/supervisor/perf-supervisor",
            {"af": "/run/build/artifacts/baseline/af"}, "/srv/perf-scratch/sample", 1000, 1000,
            [0, 2], readonly={"/perf/fixtures": "/run/work/fixtures"})
        text = " ".join(argv)
        for expected in ("--network none", "--read-only", "--cap-drop ALL",
                         "--security-opt no-new-privileges", "--pids-limit 256",
                         "--memory 4294967296", "--memory-swap 4294967296",
                         "--cpuset-cpus 0,2", "--user 1000:1000", "--cgroupns private",
                         "--entrypoint /perf/bin/perf-supervisor",
                         "dst=/perf/bin/perf-supervisor,readonly",
                         "dst=/perf/bin/af,readonly",
                         "src=/run/work/fixtures,dst=/perf/fixtures,readonly",
                         "src=/srv/perf-scratch/sample,dst=/perf/scratch"):
            self.assertIn(expected, text)
        # The only writable bind is the per-sample directory: the scratch filesystem's root
        # (and its lost+found) is never exposed, so the child cannot write beside the sample
        # or rename the mount root (ADR-0168).
        writable = [item for item in argv if item.startswith("type=bind")
                    and not item.endswith(",readonly")]
        self.assertEqual(writable, ["type=bind,src=/srv/perf-scratch/sample,dst=/perf/scratch"])
        self.assertNotIn("src=/srv/perf-scratch,", text)
        with self.assertRaises(ContractError):
            supervise.container_argv("docker", "n", "sha256:" + "0" * 64, self.env, "/s", {},
                                     "/srv/perf-scratch", 1, 1, [0, 1])
        with self.assertRaises(ContractError):
            supervise.container_argv("docker", "n", "sha256:" + "0" * 64, self.env, "/s", {},
                                     "/srv/perf-scratch/sample", 1, 1, [0, 1],
                                     readonly={"/perf/scratch/x": "/srv"})
        for absent in ("--rm", "--cpus", "--cpu-quota", "--privileged", "--init", "tmpfs"):
            self.assertNotIn(absent, argv)
        self.assertNotIn("--cpu-period", text)
        with self.assertRaises(ContractError):
            supervise.container_argv("docker", "n", "af-perf:latest", self.env, "/s", {},
                                     "/srv/sample", 1, 1, [0, 1])
        with self.assertRaises(ContractError):
            supervise.container_argv("docker", "n", "sha256:" + "0" * 64, self.env, "/s", {},
                                     "/srv/sample", 1, 1, [0])

    def test_a_seed_writer_binds_only_its_seed_directory_and_a_sample_reads_it_read_only(self):
        # ADR-0169: a seed's writer may also write its own new directory in the same verified
        # filesystem's seed store, at /perf/seed-out; a sample binds a sealed seed read-only.
        argv = supervise.container_argv(
            "docker", "n", "sha256:" + "0" * 64, self.env, "/s", {}, "/srv/perf-scratch/sample",
            1000, 1000, [0, 1], readonly={"/perf/fixtures": "/run/work/fixtures"},
            seed_output="/srv/perf-scratch/seeds/baseline-task_state")
        writable = [item for item in argv if item.startswith("type=bind")
                    and not item.endswith(",readonly")]
        self.assertEqual(writable, [
            "type=bind,src=/srv/perf-scratch/sample,dst=/perf/scratch",
            "type=bind,src=/srv/perf-scratch/seeds/baseline-task_state,dst=/perf/seed-out"])
        for elsewhere in ("/srv/perf-scratch", "/srv/other/seeds/x",
                          "/srv/perf-scratch/seeds", "/srv/perf-scratch/sample/x"):
            with self.subTest(seed_output=elsewhere):
                with self.assertRaises(ContractError):
                    supervise.container_argv("docker", "n", "sha256:" + "0" * 64, self.env,
                                             "/s", {}, "/srv/perf-scratch/sample", 1, 1, [0, 1],
                                             seed_output=elsewhere)
        argv = supervise.container_argv(
            "docker", "n", "sha256:" + "0" * 64, self.env, "/s", {}, "/srv/perf-scratch/sample",
            1, 1, [0, 1], readonly={"/perf/seed": "/srv/perf-scratch/seeds/baseline-task_state"})
        self.assertIn("type=bind,src=/srv/perf-scratch/seeds/baseline-task_state,"
                      "dst=/perf/seed,readonly", argv)
        with self.assertRaises(ContractError):
            supervise.container_argv("docker", "n", "sha256:" + "0" * 64, self.env, "/s", {},
                                     "/srv/perf-scratch/sample", 1, 1, [0, 1],
                                     readonly={"/perf/seed-out": "/srv/elsewhere"})

    def test_cpus_prefer_separate_physical_cores_and_say_when_they_cannot(self):
        siblings = {0: {0, 4}, 4: {0, 4}, 1: {1, 5}, 5: {1, 5}}
        self.assertEqual(supervise.choose_cpus([0, 1, 4, 5], siblings, 2), ([0, 1], False))
        self.assertEqual(supervise.choose_cpus([0, 4], siblings, 2), ([0, 4], True))
        with self.assertRaises(ContractError):
            supervise.choose_cpus([3], {}, 2)

    def test_observed_limits_are_verified_never_assumed(self):
        result = fixture("supervisor-result/valid-namespace.json")
        problems = supervise.check_container_observations(result, self.env)
        # The fixture came from a 256-MiB, 64-PID functional probe, not the declared profile.
        self.assertIn("cgroup/memory.max is 268435456, not 4294967296", problems)
        self.assertIn("cgroup/pids.max is 64, not 256", problems)
        self.assertFalse(any("swap" in problem for problem in problems))
        observed = result["observations"]["before"]
        observed["cgroup/memory.max"] = {"value": "4294967296\n"}
        observed["cgroup/pids.max"] = {"value": "256\n"}
        self.assertEqual(supervise.check_container_observations(result, self.env), [])
        observed["cgroup/memory.swap.max"] = {"value": "max\n"}
        observed["cgroup/cpu.max"] = {"value": "200000 100000\n"}
        problems = supervise.check_container_observations(result, self.env)
        self.assertEqual(len(problems), 2, problems)
        del observed["cgroup/pids.max"]
        self.assertIn("cgroup/pids.max is unobservable, not 256",
                      supervise.check_container_observations(result, self.env))


MOUNTINFO = """22 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw
40 22 0:35 / /perf-tmpfs rw - tmpfs tmpfs rw,size=2097152k
41 22 7:0 / /srv/perf-scratch rw,nosuid - ext4 /dev/loop0 rw
42 22 7:1 / /srv/perf\\040huge rw - xfs /dev/loop1 rw
"""


class Capabilities(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.mountinfo = self.path("mountinfo")
        with open(self.mountinfo, "w") as stream:
            stream.write(MOUNTINFO)

    def scratch(self, path, system="linux", verify=False):
        return capability.scratch_capability(path, 2 << 30, verify, system, self.mountinfo)

    def test_no_backend_and_non_linux_backends_are_unavailable(self):
        record = self.scratch(None)
        self.assertEqual(record["capability"]["status"], "unavailable")
        self.assertIn("tmpfs", record["capability"]["reasons"][0])
        record = self.scratch("/srv/perf-scratch", system="macos")
        self.assertEqual(record["enforcement"], "refused")
        self.assertIn("cannot be verified", record["capability"]["reasons"][0])

    def test_tmpfs_and_paths_that_are_not_mount_points_are_refused(self):
        self.assertEqual(capability.mount_of("/perf-tmpfs/x", self.mountinfo),
                         ("/perf-tmpfs", "tmpfs"))
        self.assertEqual(capability.mount_of("/srv/perf huge/a", self.mountinfo),
                         ("/srv/perf huge", "xfs"))
        record = self.scratch("/perf-tmpfs")
        self.assertIn("not a disk-backed block filesystem", record["capability"]["reasons"][0])
        record = self.scratch("/srv")
        self.assertIn("is not itself a mount point", record["capability"]["reasons"][0])

    def test_the_fill_stops_at_the_limit_and_removes_its_file(self):
        record = capability.fill(self.tmp, 3 << 20)
        self.assertEqual(record["stopped"], "limit_exceeded")
        self.assertGreater(record["written_bytes"], 3 << 20)
        self.assertIsNone(record["cleanup"])
        self.assertEqual(os.listdir(self.tmp), ["mountinfo"])
        self.assertIn("without the filesystem refusing", capability.fill_refusal(record, 3 << 20))

    def verify(self, host, limit=8 << 20, deadline=None):
        """The scratch capability of a mounted ext4 filesystem whose calls `host` answers."""
        return capability.scratch_capability("/srv/perf-scratch", limit, True, "linux",
                                              self.mountinfo, host=host, deadline=deadline)

    def test_io_and_permission_errors_never_verify_a_bound(self):
        # Root-reproduced: EIO or EACCES on the first write used to verify enforcement with
        # zero bytes written.
        for code in (errno.EIO, errno.EACCES):
            with self.subTest(errno=errno.errorcode[code]):
                record = self.verify(FakeHost(fail={"write": (1, code)}))
                self.assertEqual(record["enforcement"], "refused")
                self.assertEqual(record["capability"]["status"], "unavailable")
                self.assertEqual((record["fill"]["stopped"], record["fill"]["errno"],
                                  record["fill"]["written_bytes"]),
                                 ("error", errno.errorcode[code], 0))
                self.assertIn("not a capacity refusal", record["capability"]["reasons"][0])
                self.assertIn(errno.errorcode[code], record["capability"]["reasons"][0])
        record = self.verify(FakeHost(fail={"open": (1, errno.EACCES)}))
        self.assertEqual((record["enforcement"], record["fill"]["stage"]), ("refused", "open"))

    def test_a_failed_sync_keeps_the_bytes_its_write_accepted(self):
        host = FakeHost(fail={"fsync": (3, errno.EIO)})
        record = self.verify(host)
        self.assertEqual(record["enforcement"], "refused")
        self.assertEqual((record["fill"]["stage"], record["fill"]["errno"]), ("fsync", "EIO"))
        self.assertEqual(record["fill"]["written_bytes"], 3 * capability.FILL_CHUNK)
        self.assertEqual(host.unlinked, 1)

    def test_a_capacity_refusal_without_progress_or_a_full_filesystem_proves_nothing(self):
        record = self.verify(FakeHost(fail={"write": (1, errno.ENOSPC)}, free=0))
        self.assertIn("refused the first write", record["capability"]["reasons"][0])
        # ENOSPC while the filesystem still reports free space is not its capacity.
        record = self.verify(FakeHost(fail={"write": (4, errno.ENOSPC)}, free=5 << 20))
        self.assertEqual(record["enforcement"], "refused")
        self.assertIn("5242880 bytes free", record["capability"]["reasons"][0])
        record = self.verify(FakeHost(fail={"write": (4, errno.ENOSPC),
                                            "statvfs": (2, errno.EIO)}))
        self.assertIn("statvfs: EIO", record["capability"]["reasons"][0])

    def test_no_progress_and_the_time_bound_stop_the_fill(self):
        record = self.verify(FakeHost(zero_after=2))
        self.assertEqual(record["fill"]["stopped"], "no_progress")
        self.assertEqual(record["fill"]["written_bytes"], 2 * capability.FILL_CHUNK)
        self.assertEqual(record["enforcement"], "refused")
        # Every clock read costs 40 s of a 600 s check, its cleanup allowance kept: the fill
        # stops at its bound after a few writes, never by a refusal.
        host = FakeHost(tick=40.0)
        record = self.verify(host, limit=1 << 40)
        self.assertEqual(record["fill"]["stopped"], "deadline")
        self.assertLessEqual(host.writes, 6)
        self.assertEqual(host.unlinked, 1)
        self.assertIn("time bound", record["capability"]["reasons"][0])
        self.assertTrue(capability.ended_by_deadline(record))

    def test_a_capacity_answer_after_the_fill_s_bound_is_the_deadline_s(self):
        # Root-reproduced (ADR-0173): with one second to write, the first MiB was accepted and
        # the next write took five seconds to return ENOSPC; the fill used to report a refusal
        # after 5000 ms and the capacity check found nothing wrong with it.
        host = FakeHost(fail={"write": (2, errno.ENOSPC)}, free=0, delay={"write": (2, 5.0)})
        deadline = 1.0 + capability.FILL_CLEANUP_SECONDS
        record = self.verify(host, deadline=deadline)
        fill = record["fill"]
        self.assertEqual((fill["stopped"], fill["errno"], fill["stage"]),
                         ("deadline", None, None))
        self.assertEqual((fill["written_bytes"], fill["elapsed_ms"]),
                         (capability.FILL_CHUNK, 5000))
        self.assertEqual(record["enforcement"], "refused")
        self.assertIn("did not finish within its time bound", record["capability"]["reasons"][0])
        self.assertIsNotNone(capability.fill_refusal(fill, 8 << 20))
        # Its file was still removed within the deadline, which kept time for that.
        self.assertEqual((host.unlinked, fill["cleanup"]), (1, None))
        # A sync, a refusal at open and the free space answered late are the deadline's too.
        for late in ({"fsync": (1, 5.0)}, {"statvfs": (2, 40.0)}):
            with self.subTest(late=late):
                record = self.verify(FakeHost(fail={"write": (3, errno.ENOSPC)}, free=0,
                                              delay=late), deadline=deadline)
                self.assertEqual(record["fill"]["stopped"], "deadline", record["fill"])
                self.assertEqual(record["enforcement"], "refused")
        # The same answer in time verifies the bound.
        record = self.verify(FakeHost(fail={"write": (2, errno.ENOSPC)}, free=0),
                             deadline=deadline)
        self.assertEqual(record["enforcement"], "verified", record)

    def test_a_fill_blocked_at_the_deadline_is_abandoned_and_its_file_kept(self):
        # A write that never returns while the caller waits: the caller's wait ends at the
        # deadline (here at once), and the fill is refused without waiting for that write.
        host = FakeHost(block={"write": 2}, deadline_reached=True)
        record = self.verify(host)
        fill = record["fill"]
        self.assertEqual((fill["stopped"], fill["written_bytes"]),
                         ("deadline", capability.FILL_CHUNK))
        self.assertIn("not confirmed: its write call had not returned at the deadline",
                      fill["cleanup"])
        self.assertIn(".af-perf-enforcement-", fill["cleanup"])
        self.assertEqual(record["enforcement"], "refused")
        self.assertIn("not removed cleanly", record["capability"]["reasons"][0])
        self.assertTrue(capability.ended_by_deadline(record))
        doc = fixture("capabilities/valid-verified-scratch.json")
        doc["linux_container"]["scratch"].update(
            enforcement="refused", fill=fill,
            capability={"status": "unavailable", "reasons": record["capability"]["reasons"]})
        doc["linux_container"]["capability"] = {"status": "unavailable", "reasons": ["x"]}
        contracts.validate_capabilities(doc)
        # When the blocked write finally returns, the abandoned worker writes nothing more and
        # only closes its descriptor: the file stays for the operator.
        host.release.set()
        self.assertTrue(host.closed.wait(30))
        self.assertEqual((host.writes, host.calls["fsync"], host.unlinked), (2, 1, 0))

    def test_a_slow_check_before_the_fill_starts_no_fill(self):
        # Root-reproduced (ADR-0173): one second was left; finding the mount took two, and the
        # fill used to start after the deadline with its own second. One deadline, fixed before
        # the first step, now bounds every step.
        host = FakeHost(free=0, delay={"mount_of": (1, 2.0)})
        record = self.verify(host, deadline=1.0)
        self.assertIsNone(record["fill"])
        self.assertEqual(host.calls["open"], 0)
        self.assertEqual(record["enforcement"], "refused")
        self.assertEqual(record["capability"]["reasons"],
                         [capability.DEADLINE_REASON + "reading its capacity; it was not "
                          "started"])
        self.assertTrue(capability.ended_by_deadline(record))
        # Time for the metadata but not for a fill and its cleanup: no fill file is created.
        host = FakeHost(free=0, delay={"listdir": (1, 2.0)})
        record = self.verify(host, deadline=capability.FILL_CLEANUP_SECONDS + 1.0)
        self.assertIsNone(record["fill"])
        self.assertEqual(host.calls["open"], 0)
        self.assertTrue(record["capability"]["reasons"][0].startswith(
            capability.DEADLINE_REASON + "filling it"), record["capability"]["reasons"])

    def test_only_a_capacity_refusal_of_a_full_filesystem_verifies_and_cleanup_is_kept(self):
        for code in (errno.ENOSPC, errno.EDQUOT):
            with self.subTest(errno=errno.errorcode[code]):
                record = self.verify(FakeHost(fail={"write": (6, code)}, free=4096))
                self.assertEqual(record["enforcement"], "verified", record)
                self.assertEqual(record["capability"], {"status": "available", "reasons": []})
                self.assertEqual(record["fill"]["written_bytes"], 5 * capability.FILL_CHUNK)
        record = self.verify(FakeHost(fail={"write": (6, errno.ENOSPC),
                                            "unlink": (1, errno.EIO)}, free=0))
        self.assertEqual(record["enforcement"], "refused")
        self.assertIn("unlink: EIO", record["fill"]["cleanup"])
        self.assertIn("not removed cleanly", record["capability"]["reasons"][0])
        doc = fixture("capabilities/valid-verified-scratch.json")
        doc["linux_container"]["scratch"]["fill"]["cleanup"] = "unlink: EIO: I/O error"
        with self.assertRaises(ContractError):
            contracts.validate_capabilities(doc)

    def test_the_report_names_every_gap_and_validates(self):
        doc = capability.report(suite_module.load(SUITE).doc, docker="/nonexistent/docker")
        self.assertEqual(doc["linux_container"]["capability"]["status"], "unavailable")
        self.assertTrue(any("no container runtime" in reason for reason in
                            doc["linux_container"]["capability"]["reasons"]))
        self.assertIsNotNone(doc["tools"]["docker_server"]["unavailable"])
        self.assertEqual(doc["trusted_local"]["same_user_isolation"], "not_provided")
        for env_id, record in doc["environments"].items():
            if env_id == "macos_arm64_local":
                # Executable since p3b (ADR-0168), exactly where trusted local mode is: on a
                # macOS arm64 host; anywhere else it names the host it is not.
                local = (capability.host_platform() == "macos" and doc["host"]["arch"]
                         == "aarch64")
                self.assertEqual(record["status"], "available" if local else "unavailable")
                self.assertFalse(any(reason.startswith("registry: ")
                                     for reason in record["reasons"]))
                continue
            # The contained Linux environments are executable (ADR-0169): without a container
            # runtime and a verified scratch this host refuses them with its own reasons,
            # never through a registry state.
            self.assertEqual(record["status"], "unavailable", env_id)
            self.assertFalse(any(reason.startswith("registry: ")
                                 for reason in record["reasons"]))
            self.assertTrue(any("no container runtime" in reason
                                for reason in record["reasons"]), record["reasons"])
            self.assertTrue(any("no verified disk-bounded scratch" in reason
                                for reason in record["reasons"]), record["reasons"])
        result = subprocess.run([sys.executable, "-I", CLI, "capabilities", "--docker",
                                 "/nonexistent/docker"], cwd=ROOT, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stderr)
        contracts.validate_capabilities(json.loads(result.stdout))


StatVfs = collections.namedtuple("StatVfs", "f_blocks f_frsize f_bavail")


class FakeHost(capability.Host):
    """Filesystem calls of an 8-MiB ext4 scratch with deterministic failures: `fail` maps a
    call to (the 1-based call that fails, errno); later calls of it succeed. `free` is what
    `statvfs` reports available, all of it unless given. `delay` maps a call to (the 1-based
    call that takes long, seconds of the controlled clock); `block` maps a call to the 1-based
    call that blocks until `release` is set, and `deadline_reached` makes the caller's wait
    for the fill end at once, as a deadline would."""

    def __init__(self, fail=None, free=None, zero_after=None, tick=0.0, delay=None,
                 block=None, deadline_reached=False):
        self.fail, self.free, self.zero_after, self.tick = fail or {}, free, zero_after, tick
        self.delay, self.block = delay or {}, block or {}
        self.deadline_reached = deadline_reached
        self.calls = collections.Counter()
        self.now, self.writes, self.unlinked = 0.0, 0, 0
        self.blocked, self.release, self.closed = (threading.Event(), threading.Event(),
                                                   threading.Event())

    def check(self, call):
        self.calls[call] += 1
        at, seconds = self.delay.get(call, (None, None))
        if self.calls[call] == at:
            self.now += seconds
        if self.calls[call] == self.block.get(call):
            self.blocked.set()
            self.release.wait(60)
        at, code = self.fail.get(call, (None, None))
        if self.calls[call] == at:
            raise OSError(code, os.strerror(code))

    def open(self, path, flags, mode):
        self.check("open")
        return 99

    def write(self, fd, data):
        self.writes += 1
        self.check("write")
        if self.zero_after is not None and self.writes > self.zero_after:
            return 0
        return len(data)

    def fsync(self, fd):
        self.check("fsync")

    def close(self, fd):
        try:
            self.check("close")
        finally:
            self.closed.set()

    def unlink(self, path):
        self.check("unlink")
        self.unlinked += 1

    def statvfs(self, path):
        self.check("statvfs")
        blocks = (8 << 20) // 4096
        free = blocks * 4096 if self.free is None else self.free
        return StatVfs(blocks, 4096, free // 4096)

    def listdir(self, path):
        self.check("listdir")
        return ["lost+found"]

    def realpath(self, path):
        return path

    def mount_of(self, path, mountinfo):
        self.check("mount_of")
        return capability.mount_of(path, mountinfo)

    def clock(self):
        self.now += self.tick
        return self.now

    def wait(self, event, seconds):
        # The controlled clock is not real time: wait for the worker, or, when the deadline is
        # to be reached while a call blocks, until that call blocks.
        if self.deadline_reached:
            self.blocked.wait(60)
            return False
        return event.wait(60)


FAKE_DOCKER = r'''
import json, os, sys
log = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "calls")
with open(log, "a") as stream:
    stream.write(json.dumps(sys.argv[1:]) + "\n")
command = sys.argv[1]
here = os.path.dirname(log)
if command == "run":
    request = json.loads(sys.stdin.buffer.read())
    sample = [arg.split(",")[1][4:] for arg in sys.argv if arg.startswith("type=bind,src=")
              and arg.endswith(",dst=/perf/scratch")][0]
    # What a candidate could do if more than the per-sample directory were exposed, or what
    # a supervisor that did not clear would leave behind: the host check must see both.
    if os.path.exists(os.path.join(here, "escape")):
        with open(os.path.join(os.path.dirname(sample), "escaped-scratch-file"), "w") as out:
            out.write("outside the sample\n")
    if os.path.exists(os.path.join(here, "leftover")):
        with open(os.path.join(sample, "left-behind"), "w") as out:
            out.write("not cleared\n")
    if os.path.exists(os.path.join(here, "rename")):
        os.rename(sample, sample + "-moved")
        os.mkdir(sample, 0o700)
    if os.path.exists(os.path.join(here, "fail")):
        sys.stderr.write("owned fake launch failure\n")
        sys.exit(1)
    with open(os.path.join(os.path.dirname(log), "result.json"), "rb") as stream:
        result = stream.read().strip()
    sys.stdout.buffer.write(result + b"\n" + b"hello\n")
elif command == "ps":
    if os.path.exists(os.path.join(os.path.dirname(log), "stuck")):
        print("0123456789ab")
'''


class ContainedSample(TempDir):
    """The container's lifetime around one supervised sample, with a stand-in `docker`."""

    def setUp(self):
        TempDir.setUp(self)
        self.env = suite_module.load(SUITE).doc["environments"]["linux_x86_64_gnu_hosted"]
        os.mkdir(self.path("bin"))
        self.docker = self.path("bin", "docker")
        with open(self.docker, "w") as stream:
            stream.write("#!%s -I\n%s" % (sys.executable, FAKE_DOCKER))
        os.chmod(self.docker, 0o755)
        with open(self.path("bin", "result.json"), "w") as stream:
            stream.write(json.dumps(fixture("supervisor-result/valid-namespace.json")))

    def run_one(self, deadline=None, clock=time.monotonic):
        order = probe_request()
        os.makedirs(self.path("scratch-fs", "lost+found"), exist_ok=True)
        return supervise.run_contained(
            self.docker, "af-perf-run-0-1", "sha256:" + "1" * 64, self.env, "/s/perf-supervisor",
            {"perf-probe": "/a/perf-probe"}, self.path("scratch-fs"), os.getuid(), os.getgid(),
            [0, 1], order, deadline=deadline, clock=clock)

    def calls(self):
        with open(self.path("bin", "calls")) as stream:
            return [json.loads(line) for line in stream]

    def test_interrupted_removal_preserves_the_original_exception_and_uncertain_ownership(self):
        from perflab.budget import Interrupted
        original, interruption = OSError("launch pipe failed"), Interrupted("removal deadline")
        with mock.patch.object(supervise, "launch", side_effect=original), \
                mock.patch.object(supervise, "remove_container", side_effect=interruption) as remove:
            with self.assertRaises(Interrupted) as caught:
                self.run_one()
        self.assertIs(caught.exception, interruption)
        self.assertIs(interruption.__cause__, original)
        self.assertTrue(interruption.supervisor_failure.uncertain)
        self.assertTrue(interruption.supervisor_failure.launched)
        remove.assert_called_once()

    def test_the_container_is_removed_and_its_limits_checked_after_the_sample(self):
        contained = self.run_one()
        self.assertEqual(contained.supervised.stdout, b"hello\n")
        self.assertTrue(contained.removed, contained.removal)
        self.assertEqual([call[0] for call in self.calls()], ["run", "rm", "ps"])
        self.assertIn("--network", self.calls()[0])
        self.assertNotIn("--rm", self.calls()[0])
        # The canned result came from a 256-MiB functional probe: its limits are not the
        # environment's, so the environment is unverified rather than assumed.
        self.assertTrue(any("memory.max" in problem for problem in contained.limit_problems))
        self.assertIsNotNone(contained.container_overhead_ns)
        self.assertEqual(contained.scratch_problems, [])
        bind = [arg for arg in self.calls()[0] if arg.endswith(",dst=/perf/scratch")]
        self.assertEqual(bind, ["type=bind,src=%s,dst=/perf/scratch"
                                % os.path.join(self.path("scratch-fs"), "sample")])

    def test_writes_beside_the_sample_and_a_renamed_sample_root_are_never_clean(self):
        # The defect ADR-0168 closes: with the whole filesystem bound, a child's write to
        # ../escaped-scratch-file survived a cleanup reported clean. Now only sample/ is bound,
        # and the host compares the filesystem's root and the sample directory's identity
        # before and after, so anything outside sample/, a sample/ renamed away and replaced,
        # or a sample/ not emptied, is reported for the shard to stop.
        for marker, expected in (("escape", "escaped-scratch-file appeared"),
                                 ("rename", "sample was replaced"),
                                 ("leftover", "still holds left-behind")):
            with self.subTest(marker=marker):
                root = self.path("scratch-fs")
                if os.path.exists(root):
                    for current, dirs, files in os.walk(root, topdown=False):
                        for name in files:
                            os.unlink(os.path.join(current, name))
                        for name in dirs:
                            os.rmdir(os.path.join(current, name))
                    os.rmdir(root)
                open(self.path("bin", marker), "w").close()
                try:
                    contained = self.run_one()
                finally:
                    os.unlink(self.path("bin", marker))
                self.assertTrue(any(expected in problem
                                    for problem in contained.scratch_problems),
                                contained.scratch_problems)
                self.assertTrue(os.path.isdir(os.path.join(root, "lost+found")),
                                "the protected lost+found is never touched")

    def test_a_scratch_filesystem_that_is_not_fresh_is_refused_before_any_container(self):
        root = self.path("scratch-fs")
        os.makedirs(os.path.join(root, "sample"))
        os.chmod(os.path.join(root, "sample"), 0o700)
        with open(os.path.join(root, "stale"), "w") as stream:
            stream.write("from an earlier sample\n")
        with self.assertRaises(ContractError) as caught:
            self.run_one()
        self.assertIn("holds stale", str(caught.exception))
        os.unlink(os.path.join(root, "stale"))
        with open(os.path.join(root, "sample", "x"), "w") as stream:
            stream.write("x")
        with self.assertRaises(ContractError) as caught:
            self.run_one()
        self.assertIn("not empty before the sample", str(caught.exception))
        self.assertFalse(os.path.exists(self.path("bin", "calls")))

    def test_the_shard_s_seed_store_may_sit_beside_the_sample_directory(self):
        root = self.path("scratch-fs")
        os.makedirs(os.path.join(root, "seeds", "baseline-task_state"))
        inventory = supervise.prepare_scratch(root, os.getuid(), os.getgid())
        self.assertEqual(sorted(inventory), ["sample", "seeds"])
        self.assertEqual(supervise.scratch_problems(root, inventory), [])
        os.rename(os.path.join(root, "seeds"), os.path.join(root, "moved"))
        os.mkdir(os.path.join(root, "seeds"))
        problems = supervise.scratch_problems(root, inventory)
        self.assertTrue(any("seeds was replaced" in p or "moved appeared" in p
                            for p in problems), problems)

    def test_the_container_user_must_be_one_the_host_can_verify(self):
        root = self.path("scratch-fs")
        os.makedirs(root)
        if os.geteuid() == 0:
            self.skipTest("root may hand the sample directory to any user")
        with self.assertRaises(ContractError) as caught:
            supervise.prepare_scratch(root, os.getuid() + 1, os.getgid())
        self.assertIn("the container user must be this user", str(caught.exception))
        self.assertEqual(os.listdir(root), [])

    def test_an_unconfirmed_removal_is_reported_for_the_shard_to_stop(self):
        open(self.path("bin", "stuck"), "w").close()
        contained = self.run_one()
        self.assertFalse(contained.removed)
        self.assertIn("removal unconfirmed", contained.removal)

    def test_a_failed_launch_is_resolved_by_one_removal(self):
        # Root-reproduced (ADR-0173): a failed launch used to remove its container in the
        # launch and again after it. Now the one owner removes it once, and only a confirmed
        # removal makes the failure certain.
        open(self.path("bin", "fail"), "w").close()
        for stuck, uncertain in ((False, False), (True, True)):
            with self.subTest(stuck=stuck):
                if stuck:
                    open(self.path("bin", "stuck"), "w").close()
                if os.path.exists(self.path("bin", "calls")):
                    os.unlink(self.path("bin", "calls"))
                with self.assertRaises(supervise.SupervisorFailure) as caught:
                    self.run_one()
                failure = caught.exception
                self.assertEqual([call[0] for call in self.calls()], ["run", "rm", "ps"])
                self.assertEqual((failure.launched, failure.uncertain), (True, uncertain))
                self.assertIn("the supervisor exited 1: owned fake launch failure",
                              str(failure))
                self.assertIn("removal unconfirmed" if stuck else "af-perf-run-0-1 removed",
                              str(failure))
                self.assertNotIn("no reply confirms", str(failure))

    def test_the_removal_ends_by_the_run_s_deadline(self):
        # Root-reproduced (ADR-0173): with a 60 s caller deadline, a failed launch's removals
        # ran rm and ps twice, each with its own 60 s, 236 s of controlled time in all. Each
        # docker call here takes 59 s of the controlled clock unless its timeout ends it first.
        open(self.path("bin", "fail"), "w").close()
        now = [0.0]
        calls = []
        real_run = subprocess.run

        def docker_call(argv, timeout=None, **options):
            if argv[0] != self.docker:
                return real_run(argv, timeout=timeout, **options)
            calls.append((argv[1], now[0], timeout))
            if timeout < 59.0:
                now[0] += timeout
                raise subprocess.TimeoutExpired(argv[:2], timeout)
            now[0] += 59.0
            return subprocess.CompletedProcess(argv, 0, b"", b"")

        with mock.patch.object(supervise.subprocess, "run", docker_call):
            # 90 s left: the cancellation reserve (25 s), the removal allowance and the start fit.
            with self.assertRaises(supervise.SupervisorFailure) as caught:
                self.run_one(deadline=90.0, clock=lambda: now[0])
        self.assertEqual([(command, started) for command, started, _ in calls],
                         [("rm", 0.0), ("ps", 59.0)])
        self.assertEqual([timeout for _, _, timeout in calls], [60.0, 1.0])
        self.assertLessEqual(now[0], 60.0)
        self.assertTrue(caught.exception.uncertain)
        self.assertIn("removal unconfirmed", str(caught.exception))
        # A removal with less time left than its allowance ends by the deadline; one with
        # none left starts nothing and is unconfirmed.
        del calls[:]
        now[0] = 0.0
        with mock.patch.object(supervise.subprocess, "run", docker_call):
            removal = supervise.remove_container(self.docker, "c", deadline=10.0,
                                                 clock=lambda: now[0])
        self.assertEqual(calls, [("rm", 0.0, 10.0)])
        self.assertEqual(now[0], 10.0)
        self.assertIn("container c removal unconfirmed", removal)
        del calls[:]
        with mock.patch.object(supervise.subprocess, "run", docker_call):
            removal = supervise.remove_container(self.docker, "c", deadline=5.0,
                                                 clock=lambda: now[0])
        self.assertEqual(calls, [])
        self.assertIn("no time was left for its removal", removal)

    def test_a_trusted_local_request_is_never_run_in_a_container(self):
        order = probe_request(containment="process_group", scratch=None)
        with self.assertRaises(ContractError):
            supervise.run_contained(self.docker, "n", "sha256:" + "1" * 64, self.env, "/s",
                                    {}, self.path("scratch-fs"), 1, 1, [0, 1], order)
        self.assertFalse(os.path.exists(self.path("bin", "calls")))


class OptionalMetrics(TempDir):
    WANTED = ["user_cpu_ms", "system_cpu_ms", "page_faults", "context_switches",
              "wait4_maxrss_bytes", "cgroup_peak_memory_bytes", "setup_ms"]

    def test_rusage_and_cgroup_values_are_exact_and_labeled_by_their_source(self):
        result = fixture("supervisor-result/valid-namespace.json")
        metrics, unavailable = supervise.optional_metrics(result, self.WANTED)
        self.assertEqual(metrics["user_cpu_ms"], {"value": "3.139", "unit": "ms"})
        self.assertEqual(metrics["system_cpu_ms"], {"value": "3.382", "unit": "ms"})
        self.assertEqual(metrics["page_faults"], {"value": "246", "unit": "count"})
        self.assertEqual(metrics["context_switches"], {"value": "52", "unit": "count"})
        self.assertEqual(metrics["wait4_maxrss_bytes"], {"value": "1531904", "unit": "bytes"})
        self.assertEqual(metrics["cgroup_peak_memory_bytes"], {"value": "6909952",
                                                               "unit": "bytes"})
        # The probe's own metric is not the supervisor's to report or to call missing.
        self.assertNotIn("setup_ms", metrics)
        self.assertEqual(unavailable, {})

    def test_missing_sources_are_unavailable_with_reasons_never_zero(self):
        result = fixture("supervisor-result/valid-trusted-local.json")
        result["rusage"] = None
        result["rusage_unavailable"] = "getrusage failed"
        metrics, unavailable = supervise.optional_metrics(result, self.WANTED)
        self.assertEqual(metrics, {})
        self.assertIn("getrusage failed", unavailable["user_cpu_ms"])
        self.assertIn("only on Linux", unavailable["cgroup_peak_memory_bytes"])
