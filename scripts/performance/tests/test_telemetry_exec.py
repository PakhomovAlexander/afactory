"""Where a telemetry experiment's script runs and what bounds it (ADR-0214).

The trusted local executor runs real short-lived processes here: a script that leaves a process
behind in its group, one that outlives the run's deadline and one that writes past its room.
The contained executor runs through a stand-in `docker` client that executes the command on
this host and records its arguments; what is exercised is the container's declared boundary
(only the execution's own scratch directory writable, nothing else of the controller or the
other side bound, network none, read-only root, limits) and how an unconfirmed removal or a
changed scratch filesystem is an uncertain cleanup. No measurement is made.
"""
import errno
import io
import json
import os
import subprocess
import sys
import time
import unittest
from types import SimpleNamespace
from unittest import mock

from perflab import builds, limits, supervise, telemetryexec, telemetrylab
from perflab.values import ContractError
from tests import telemetry_fixtures as fx
from tests.support import TempDir
from tests.telemetry_subreaper import state, thread_states

DOCKER = r'''
import json, os, signal, subprocess, sys
here = os.path.dirname(os.path.abspath(sys.argv[0]))
args = sys.argv[1:]
with open(os.path.join(here, "docker-calls.jsonl"), "a") as log:
    log.write(json.dumps(args) + "\n")
if args[0] == "rm":
    name = args[-1]
    marker = os.path.join(here, name + ".pid")
    if os.path.exists(marker):
        try:
            os.killpg(int(open(marker).read()), signal.SIGKILL)
        except OSError:
            pass
    sys.exit(0)
if args[0] == "ps":
    # A removal the runtime cannot confirm: the container is still listed.
    if os.path.exists(os.path.join(here, "keep")):
        sys.stdout.write("0123456789ab\n")
    sys.exit(0)
assert args[0] == "run", args
name = args[args.index("--name") + 1]
workdir = args[args.index("--workdir") + 1]
rest = args[args.index("--entrypoint") + 3:]
assert rest[0] == "-i"
env, index = {}, 1
while "=" in rest[index] and not rest[index].startswith("/"):
    key, value = rest[index].split("=", 1)
    env[key] = value
    index += 1
child = subprocess.Popen(rest[index:], env=env, cwd=workdir, start_new_session=True)
with open(os.path.join(here, name + ".pid"), "w") as stream:
    stream.write(str(child.pid))
sys.exit(child.wait())
'''


class Image(object):
    reference = "registry.example/af-sample@sha256:" + "a" * 64

    def record(self):
        return {"reference": self.reference, "id": "sha256:" + "b" * 64,
                "platform": "linux/amd64"}


def execution(test, argv, room=64 << 20, seconds=30, label="x1", cwd=None, readable=()):
    cwd = cwd or test.path("run")
    if not os.path.isdir(cwd):
        os.makedirs(cwd, 0o700)
    capture = test.path("capture")
    if not os.path.isdir(capture):
        os.mkdir(capture, 0o700)
    env = {"PATH": "/usr/bin:/bin", "HOME": cwd, "TMPDIR": cwd,
           "XDG_CONFIG_HOME": cwd, "XDG_CACHE_HOME": cwd,
           "XDG_STATE_HOME": cwd, "XDG_DATA_HOME": cwd}
    return telemetryexec.Execution(argv, env, cwd, [cwd], list(readable), capture, label, room,
                                   time.monotonic() + seconds, 1 << 30)


def alive(pid):
    if sys.platform == "linux":
        observed = state(pid)
        return observed is not None and (observed[1] not in ("Z", "X", "x")
                                         or any(status not in ("Z", "X", "x")
                                                for _, status in thread_states(pid)))
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class TrustedLocal(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.executor = telemetryexec.TrustedLocal(sys.executable)

    def test_a_clean_script_s_stdout_and_stderr_are_kept_whole_and_tailed(self):
        outcome = self.executor.run(execution(self, [
            sys.executable, "-c", "import sys; print('{\"a\": 1}'); "
            "sys.stderr.write('x' * 300000)"]))
        self.assertIsNone(outcome.failure())
        self.assertEqual((outcome.code, outcome.stdout), (0, b'{"a": 1}\n'))
        self.assertEqual(outcome.stderr_bytes, 300000)
        self.assertEqual(len(outcome.stderr), telemetryexec.LOG_BYTES_MAX)
        self.assertEqual(outcome.cleanup, "confirmed")

    def test_a_process_left_behind_in_its_group_fails_the_execution_and_is_killed(self):
        marker = self.path("left.pid")
        outcome = self.executor.run(execution(self, [
            sys.executable, "-c", "import subprocess, sys; "
            "p = subprocess.Popen(['sleep', '60']); "
            "open(sys.argv[1], 'w').write(str(p.pid)); print('{}')", marker]))
        self.assertEqual(outcome.code, 0)
        self.assertIn("outlived it", outcome.failure())
        self.assertEqual(outcome.cleanup, "confirmed")
        with open(marker) as stream:
            pid = int(stream.read())
        deadline = time.monotonic() + 5
        while alive(pid) and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertFalse(alive(pid), "the process left behind was killed")

    def test_the_deadline_stops_the_whole_group(self):
        started = time.monotonic()
        outcome = self.executor.run(execution(self, [
            "/bin/sh", "-c", "sleep 60 & sleep 60"], seconds=1))
        self.assertTrue(outcome.timed_out)
        self.assertIsNone(outcome.code)
        self.assertIn("deadline", outcome.failure())
        self.assertLess(time.monotonic() - started, 20)

    def test_writes_past_the_room_are_stopped_while_they_are_written(self):
        target = self.path("run", "fill")
        outcome = self.executor.run(execution(self, [
            sys.executable, "-c", "import sys, time\n"
            "with open(sys.argv[1], 'wb') as s:\n"
            "    for _ in range(400):\n"
            "        s.write(b'x' * (1 << 20)); s.flush(); time.sleep(0.02)\n", target],
            room=8 << 20))
        self.assertIsNotNone(outcome.exceeded)
        self.assertIn("stopped while writing", outcome.failure())
        self.assertLess(os.path.getsize(target), 400 << 20)

    def test_a_deadline_already_spent_starts_nothing(self):
        doc = execution(self, [sys.executable, "-c", "print(1)"], seconds=-1)
        outcome = self.executor.run(doc)
        self.assertIn("no time", outcome.failure())
        self.assertFalse(os.path.exists(doc.outputs()[0]))

    def test_a_long_telemetry_deadline_does_not_raise_the_verdict_sample_bound(self):
        outcome = self.executor.run(execution(self, ["/bin/true"], seconds=600))
        self.assertIsNone(outcome.failure(), outcome.cleanup_detail)
        self.assertEqual(limits.SAMPLE_TIMEOUT_SECONDS_MAX, 120)

    def test_an_exit_observed_after_the_absolute_deadline_is_still_a_timeout(self):
        doc = execution(self, ["/bin/true"])
        wait = telemetryexec._LocalProcess.wait
        def observe_late(process, timeout):
            wait(process, timeout)
            self.executor.clock = lambda: doc.deadline + 1
        with mock.patch.object(telemetryexec._LocalProcess, "wait", observe_late):
            outcome = self.executor.run(doc)
        self.assertTrue(outcome.timed_out)
        self.assertIsNone(outcome.code)
        self.assertIn("deadline", outcome.failure())
        self.assertEqual(outcome.cleanup, "confirmed", outcome.cleanup_detail)

    def test_a_spawn_failure_starts_nothing(self):
        outcome = self.executor.run(execution(self, [self.path("absent")]))
        self.assertIn("could not start", outcome.failure())
        self.assertEqual(outcome.cleanup, "confirmed")

    def test_the_file_size_limit_still_bounds_an_individual_file(self):
        doc = execution(self, [sys.executable, "-c",
                              "open('fill', 'wb').write(b'x' * (4 << 20))"])
        doc.file_bytes = 1 << 20
        outcome = self.executor.run(doc)
        self.assertNotEqual(outcome.code, 0)
        self.assertEqual(os.path.getsize(self.path("run", "fill")), 1 << 20)
        self.assertEqual(outcome.cleanup, "confirmed", outcome.cleanup_detail)

    def test_an_overflow_is_counted_and_never_a_complete_stdout_document(self):
        outcome = self.executor.run(execution(self, [sys.executable, "-c",
            "import sys; sys.stdout.write('x' * (17 << 20))"]))
        self.assertTrue(outcome.stdout_overflow)
        self.assertEqual(len(outcome.stdout), telemetryexec.DOCUMENT_BYTES_MAX)
        self.assertIsNotNone(outcome.failure())
        self.assertEqual(outcome.cleanup, "confirmed", outcome.cleanup_detail)

    def test_cancellation_and_active_check_errors_end_only_the_owned_group(self):
        for index, error in enumerate((KeyboardInterrupt(), OSError(errno.EIO, "room check"))):
            doc = execution(self, [sys.executable, "-c", "import time; time.sleep(60)"],
                            label="cancel%d" % index)
            other = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                     env=doc.env, start_new_session=True)
            launched = []
            launch = self.executor.launch
            def record(*args):
                owned = launch(*args)
                launched.append(owned)
                return owned
            try:
                with mock.patch.object(self.executor, "launch", side_effect=record), \
                        mock.patch.object(self.executor, "_exceeded", side_effect=error):
                    with self.assertRaises(type(error)):
                        self.executor.run(doc)
                self.assertEqual(len(launched), 1)
                self.assertIsNotNone(launched[0].returncode)
                self.assertIsNone(other.poll(), "another live execution must be untouched")
            finally:
                other.kill()
                other.wait(timeout=5)


@unittest.skipUnless(sys.platform == "linux", "Linux complete process-state evidence")
class LocalEvidence(TempDir):
    setUp = TrustedLocal.setUp

    def test_fixture_proc_read_handles_only_vanished_processes_as_absent(self):
        # Linux may report ESRCH after /proc/stat was opened but before its read.
        for error in (FileNotFoundError(errno.ENOENT, "vanished"),
                      ProcessLookupError(errno.ESRCH, "vanished")):
            stream = mock.MagicMock()
            stream.__enter__.return_value.read.side_effect = error
            with mock.patch("builtins.open", return_value=stream):
                self.assertIsNone(state(os.getpid()))
        for error in (PermissionError(errno.EACCES, "unknown"), OSError(errno.EIO, "unknown")):
            stream = mock.MagicMock()
            stream.__enter__.return_value.read.side_effect = error
            with mock.patch("builtins.open", return_value=stream):
                with self.assertRaises(type(error)):
                    state(os.getpid())

    def test_signals_only_use_the_still_reserved_owned_leader(self):
        signal_group = os.killpg
        observed = []
        def check(pgid, sent):
            leader = state(pgid)
            self.assertIsNotNone(leader)
            self.assertEqual(leader[2:], [os.getpid(), pgid, pgid])
            observed.append(leader)
            return signal_group(pgid, sent)
        with mock.patch.object(telemetryexec.os, "killpg", side_effect=check):
            outcome = self.executor.run(execution(self, ["/bin/true"]))
        self.assertIsNone(outcome.failure(), outcome.cleanup_detail)
        self.assertTrue(observed)
        self.assertTrue(all(leader[1] == "Z" for leader in observed))
        self.assertIsNone(state(observed[0][0]), "leader is reaped only after the last signal")

    def test_unknown_evidence_is_sticky_even_when_the_next_scan_is_complete(self):
        scan = telemetryexec._group_alive
        for index, error in enumerate((PermissionError(errno.EACCES, "process listing"),
                                       OSError(errno.EIO, "process iterator"),
                                       ValueError("malformed process status"))):
            calls = []
            def fail_once(*args):
                calls.append(True)
                if len(calls) == 1:
                    raise error
                return scan(*args)
            with mock.patch.object(telemetryexec, "_group_alive", side_effect=fail_once):
                outcome = self.executor.run(execution(self, ["/bin/true"],
                                                     label="unknown%d" % index))
            self.assertEqual(outcome.cleanup, "uncertain", outcome.cleanup_detail)
            self.assertIn(str(error), outcome.cleanup_detail)
            self.assertIn("cleanup was not confirmed", outcome.failure())
            self.assertGreater(len(calls), 1)

    def test_an_unreaped_observation_error_is_not_a_clean_execution(self):
        observe = os.waitid
        calls = []
        def fail_once(*args):
            calls.append(True)
            if len(calls) == 1:
                raise OSError(errno.EIO, "unreaped observation")
            return observe(*args)
        with mock.patch.object(telemetryexec.os, "waitid", side_effect=fail_once):
            outcome = self.executor.run(execution(self, ["/bin/sleep", "60"]))
        self.assertEqual(outcome.cleanup, "uncertain", outcome.cleanup_detail)
        self.assertIn("unreaped observation", outcome.failure())

    def test_a_failed_signal_never_establishes_confirmed_cleanup(self):
        killpg = os.killpg
        calls = []
        def fail_once(*args):
            calls.append(True)
            if len(calls) == 1:
                raise PermissionError(errno.EPERM, "group signal denied")
            return killpg(*args)
        with mock.patch.object(telemetryexec.os, "killpg", side_effect=fail_once):
            outcome = self.executor.run(execution(self, ["/bin/true"]))
        self.assertEqual(outcome.cleanup, "uncertain")
        self.assertIn("group signal denied", outcome.failure())

    def test_lost_child_ownership_never_signals_a_released_group(self):
        # ECHILD means another reaper released the PID. Reap the real exited fixture first,
        # then inject that condition, and assert no subsequent group signal can escape.
        observe, launch, children = os.waitid, self.executor.launch, []
        statuses = []
        def record(*args):
            process = launch(*args)
            children.append(process.child)
            return process
        def lost(idtype, pid, flags):
            until = time.monotonic() + 5
            while observe(idtype, pid, flags) is None:
                if time.monotonic() >= until:
                    raise TimeoutError("true did not exit")
                time.sleep(0.01)
            reaped, status = os.waitpid(pid, os.WNOHANG)
            self.assertEqual(reaped, pid)
            statuses.append(os.waitstatus_to_exitcode(status))
            raise ChildProcessError(errno.ECHILD, "lost ownership")
        with mock.patch.object(telemetryexec.os, "waitid", side_effect=lost), \
                mock.patch.object(self.executor, "launch", side_effect=record), \
                mock.patch.object(telemetryexec.os, "killpg") as sent:
            outcome = self.executor.run(execution(self, ["/bin/true"]))
        sent.assert_not_called()
        self.assertEqual(outcome.cleanup, "uncertain")
        self.assertIn("lost ownership", outcome.failure())
        self.assertIsNone(outcome.code, "an externally reaped status is unknown to the executor")
        # This test's external reaper knows the status; tell Popen after asserting the
        # executor did not invent it, so its destructor does not warn about the dead fixture.
        children[0].returncode = statuses[0]

    def test_a_partial_or_unparseable_scan_is_never_empty(self):
        def broken_iterator():
            yield SimpleNamespace(name="not-a-pid")
            raise OSError(errno.EIO, "partial enumeration")
        with mock.patch.object(telemetryexec.os, "scandir") as listing:
            listing.return_value.__enter__.return_value = broken_iterator()
            with self.assertRaises(OSError):
                telemetryexec._group_alive(123, time.monotonic() + 1)
        for raw in (b"123 (x) Z 1 123 123", b"malformed", b"x" * 4097):
            with mock.patch.object(telemetryexec.os, "scandir") as listing, \
                    mock.patch("builtins.open", return_value=io.BytesIO(raw)):
                listing.return_value.__enter__.return_value = [SimpleNamespace(name="123")]
                with self.assertRaises(ValueError):
                    telemetryexec._group_alive(123, time.monotonic() + 1)

    def test_scan_bounds_and_missing_leader_are_unknown(self):
        with self.assertRaisesRegex(ValueError, "bound"):
            telemetryexec._group_alive(123, time.monotonic() - 1)
        with mock.patch.object(telemetryexec.os, "scandir") as listing:
            listing.return_value.__enter__.return_value = []
            with self.assertRaisesRegex(ValueError, "leader is absent"):
                telemetryexec._group_alive(123, time.monotonic() + 1)

    def test_a_missing_or_malformed_thread_count_is_unknown_in_the_real_executor(self):
        original, failed = open, []
        def corrupt_once(path, *args, **kwargs):
            if str(path).startswith("/proc/") and str(path).endswith("/stat") and not failed:
                with original(path, *args, **kwargs) as stream:
                    prefix, rest = stream.read().rsplit(b")", 1)
                fields = rest.split()
                fields[17] = b"unknown"
                failed.append(path)
                return io.BytesIO(prefix + b") " + b" ".join(fields) + b"\n")
            return original(path, *args, **kwargs)
        with mock.patch("builtins.open", side_effect=corrupt_once):
            outcome = self.executor.run(execution(self, ["/bin/true"]))
        self.assertTrue(failed)
        self.assertEqual(outcome.cleanup, "uncertain")
        self.assertIn("process status", outcome.failure())


@unittest.skipUnless(sys.platform == "linux", "Linux ancestor subreaper regression")
class AncestorSubreaper(TempDir):
    def run_helper(self, mode):
        helper = os.path.join(os.path.dirname(os.path.abspath(__file__)), "telemetry_subreaper.py")
        env = dict(os.environ, HOME=self.tmp, XDG_CONFIG_HOME=self.tmp,
                   XDG_CACHE_HOME=self.tmp, XDG_STATE_HOME=self.tmp, XDG_DATA_HOME=self.tmp)
        # Exercise both helper launches without the test runner's import path or cwd.
        env.pop("PYTHONPATH", None)
        started = time.monotonic()
        result = subprocess.run([sys.executable, helper, "ancestor", self.tmp, mode],
                                env=env, cwd=self.tmp, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertLess(len(result.stdout), 8192)
        return json.loads(result.stdout), time.monotonic() - started

    def check_execution(self, mode):
        evidence, elapsed = self.run_helper(mode)
        self.assertEqual(evidence["cleanup"], "confirmed", evidence)
        leader, descendant = evidence["pids"]
        final_leader, final_child = evidence["final"]
        self.assertIsNone(final_leader, evidence)
        self.assertEqual(final_child, [descendant, "Z", evidence["ancestor"], leader, leader],
                         evidence)
        self.assertTrue(any(snapshot[1] and snapshot[1][1] not in ("Z", "X", "x")
                            and snapshot[1][3:] == [leader, leader]
                            for snapshot in evidence["transitions"]), evidence)
        self.assertLess(elapsed, 20)
        return evidence

    def test_leftover_under_retaining_ancestor_subreaper(self):
        evidence = self.check_execution("leftover")
        self.assertEqual(evidence["code"], 0)
        self.assertIn("outlived it", evidence["failure"])
        self.assertFalse(evidence["timed_out"])

    def test_deadline_under_retaining_ancestor_subreaper(self):
        evidence = self.check_execution("deadline")
        self.assertIsNone(evidence["code"])
        self.assertTrue(evidence["timed_out"])
        self.assertIn("deadline", evidence["failure"])

    def test_real_sigint_cancellation_cleans_up_before_propagating(self):
        evidence = self.check_execution("cancel")
        self.assertTrue(evidence["cancelled"])
        self.assertIsNone(evidence["code"])
        self.assertFalse(evidence["timed_out"])

    def test_a_member_missed_by_one_signal_is_killed_by_the_next(self):
        evidence = self.check_execution("repeat_kill")
        self.assertIn("outlived it", evidence["failure"])

    def test_a_genuinely_live_survivor_is_uncertain_and_cannot_admit_stdout(self):
        evidence, elapsed = self.run_helper("survivor")
        self.assertEqual(evidence["cleanup"], "uncertain", evidence)
        self.assertIn("cleanup was not confirmed", evidence["failure"])
        self.assertIsNotNone(evidence["final"][1], evidence)
        self.assertNotIn(evidence["final"][1][1], ("Z", "X", "x"), evidence)
        self.assertEqual(evidence["final"][1][2], evidence["ancestor"])
        self.assertLess(elapsed, 20)

    def test_a_zombie_leader_with_a_live_thread_is_not_confirmed_cleanup(self):
        evidence, elapsed = self.run_helper("thread_survivor")
        self.assertEqual(evidence["cleanup"], "uncertain", evidence)
        self.assertEqual(evidence["final"][1][1], "Z", evidence)
        self.assertTrue(any(status not in ("Z", "X", "x")
                            for _, status in evidence["final_threads"][1]), evidence)
        self.assertIn("cleanup was not confirmed", evidence["failure"])
        self.assertLess(elapsed, 20)


class ContainedBase(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.docker_dir = self.path("docker-bin")
        os.mkdir(self.docker_dir)
        self.docker = os.path.join(self.docker_dir, "docker")
        with open(self.docker, "w") as stream:
            stream.write("#!%s -I\n%s" % (sys.executable, DOCKER))
        os.chmod(self.docker, 0o755)
        self.scratch = self.path("scratch")
        os.mkdir(self.scratch, 0o700)
        self.executor = telemetryexec.Contained(
            self.docker, Image(), {"logical_cpus": 2, "memory_limit_bytes": 8 << 30,
                                   "pid_limit": 1024, "scratch_limit_bytes": 4 << 30},
            self.scratch, os.getuid(), os.getgid(), [0, 1], "tl_test", "linux stand-in")
        os.mkdir(self.executor.directory, 0o700)
        self.run_dir = os.path.join(self.executor.directory, "runs", "overhead-0-candidate")
        os.makedirs(self.run_dir, 0o700)

    def calls(self):
        with open(os.path.join(self.docker_dir, "docker-calls.jsonl")) as stream:
            return [json.loads(line) for line in stream]


class Contained(ContainedBase):

    def test_only_the_execution_s_own_scratch_directory_is_writable(self):
        candidate = self.path("candidate", "af")
        os.makedirs(os.path.dirname(candidate))
        with open(candidate, "w") as stream:
            stream.write("#!/bin/sh\n")
        script = self.path("authority", "scripts", "telemetry-overhead.py")
        doc = execution(self, ["/usr/bin/python3", "-I", "-B", script], cwd=self.run_dir,
                        readable=[script, candidate])
        argv = self.executor.argv("af-perf-telemetry-x", doc)
        joined = " ".join(argv)
        for flag in ("--network none", "--read-only", "--cap-drop ALL",
                     "--security-opt no-new-privileges", "--pids-limit 1024",
                     "--memory 8589934592", "--memory-swap 8589934592", "--cpuset-cpus 0,1",
                     "--cgroupns private", "--ipc private", "--init", "--pull never",
                     "--ulimit fsize=1073741824:1073741824"):
            self.assertIn(flag, joined)
        mounts = [argv[index + 1] for index, item in enumerate(argv) if item == "--mount"]
        writable = [item for item in mounts if not item.endswith(",readonly")]
        self.assertEqual(writable, ["type=bind,src=%s,dst=%s" % (self.run_dir, self.run_dir)])
        self.assertEqual(sorted(item.split(",")[1][4:] for item in mounts
                                if item.endswith(",readonly")), sorted([candidate, script]))
        self.assertNotIn(self.path("capture"), joined, "the capture files are never bound")
        self.assertEqual(argv[argv.index("--entrypoint") + 1:argv.index("--entrypoint") + 4],
                         ["/usr/bin/env", Image.reference, "-i"])
        self.assertEqual(argv[argv.index("--user") + 1], "%d:%d" % (os.getuid(), os.getgid()))
        # Nothing outside the result's own scratch directory is ever writable.
        for outside in (self.path("result"), self.scratch, self.path("candidate")):
            hostile = execution(self, ["/bin/true"], cwd=self.run_dir)
            hostile.writable.append(outside)
            with self.assertRaises(ContractError):
                self.executor.argv("x", hostile)

    def test_a_contained_execution_runs_and_its_container_is_removed(self):
        outcome = self.executor.run(execution(self, [
            "/bin/sh", "-c", "echo '{}'; echo note >&2"], cwd=self.run_dir))
        self.assertIsNone(outcome.failure(), outcome.cleanup_detail)
        self.assertEqual((outcome.code, outcome.stdout, outcome.stderr), (0, b"{}\n", b"note\n"))
        verbs = [call[0] for call in self.calls()]
        self.assertEqual(verbs, ["run", "rm", "ps"])
        self.assertTrue(os.path.isdir(os.path.join(self.scratch, supervise.SAMPLE_DIRECTORY)))

    def test_an_unconfirmed_removal_is_an_uncertain_cleanup(self):
        with open(os.path.join(self.docker_dir, "keep"), "w"):
            pass
        outcome = self.executor.run(execution(self, ["/bin/sh", "-c", "echo '{}'"],
                                              cwd=self.run_dir))
        self.assertEqual(outcome.cleanup, "uncertain")
        self.assertIn("removal unconfirmed", outcome.cleanup_detail)
        self.assertIn("cleanup was not confirmed", outcome.failure())

    def test_a_changed_scratch_filesystem_is_an_uncertain_cleanup(self):
        planted = os.path.join(self.scratch, "planted")
        outcome = self.executor.run(execution(self, [
            "/bin/sh", "-c", "mkdir '%s'; echo '{}'" % planted], cwd=self.run_dir))
        self.assertEqual(outcome.cleanup, "uncertain")
        self.assertIn("planted appeared at the scratch filesystem's root",
                      outcome.cleanup_detail)

    def test_a_contained_deadline_removes_the_container(self):
        outcome = self.executor.run(execution(self, ["/bin/sh", "-c", "sleep 60"],
                                              cwd=self.run_dir, seconds=1))
        self.assertTrue(outcome.timed_out)
        self.assertEqual(outcome.cleanup, "confirmed", outcome.cleanup_detail)
        self.assertEqual([call[0] for call in self.calls()], ["run", "rm", "ps"])

    def test_the_image_is_pinned_by_digest(self):
        class Loose(Image):
            reference = "registry.example/af-sample:latest"
        with self.assertRaises(ContractError):
            telemetryexec.Contained(self.docker, Loose(), {}, self.scratch, 0, 0, [0], "x", "")
        self.assertTrue(builds.pinned_image(Image.reference))


class ContainedRun(ContainedBase):
    """A whole contained result through the stand-in client: the authority's real corpus
    generator writes the master corpus in its own scratch directory, and each side's read runs
    on its own copy. The stand-in binaries answer only `--version`, so every read fails; what
    is checked is where everything ran and what each execution could write."""

    def test_every_execution_of_a_result_is_contained_in_its_own_directory(self):
        binaries = {}
        for side in ("baseline", "candidate"):
            path = self.path("%s-af" % side)
            with open(path, "w") as stream:
                stream.write('#!/bin/sh\n[ "$1" = --version ] && { echo "af 0.0.0"; exit 0; }\n'
                             'exit 9\n')
            os.chmod(path, 0o755)
            binaries[side] = path
        self.binaries = binaries
        os.rmdir(self.run_dir)
        os.rmdir(os.path.dirname(self.run_dir))
        os.rmdir(self.executor.directory)
        self.executor.python = sys.executable
        # A contained run's scratch filesystem is its work bound: what the corpus took from
        # it never stops the reads.
        result = telemetrylab.run(fx.settings(self, self.executor, "result",
                                              experiments=["read_latency"], rounds=1,
                                              free_bytes=lambda path: 1))
        self.assertEqual((result["execution"]["mode"], result["execution"]["cleanup"]),
                         ("linux_container", "confirmed"))
        self.assertEqual(result["execution"]["container"]["image"]["reference"],
                         Image.reference)
        self.assertEqual(result["binaries"]["candidate"]["version"], "af 0.0.0")
        self.assertEqual(result["corpus"]["status"], "ok", result["corpus"]["detail"])
        self.assertEqual(result["corpus"]["facts"]["segments"], 20)
        self.assertTrue(all(run["status"] == "failed" for run in result["runs"]))
        self.assertEqual(result["status"], "failed")
        own = self.executor.directory + "/"
        runs = [call for call in self.calls() if call[0] == "run"]
        self.assertEqual(len(runs), 5)
        for call in runs:
            mounts = [call[index + 1] for index, item in enumerate(call) if item == "--mount"]
            writable = [item.split(",")[1][4:] for item in mounts
                        if not item.endswith(",readonly")]
            readonly = [item.split(",")[1][4:] for item in mounts if item.endswith(",readonly")]
            self.assertTrue(writable and all(path.startswith(own) for path in writable))
            self.assertNotIn(own + "corpus", readonly, "the master corpus is never bound")
            self.assertFalse([path for path in readonly if path.startswith(self.path("result"))])
            self.assertLessEqual(len([path for path in readonly
                                      if path in binaries.values()]), 1)
            if "generate" in call:
                self.assertIn(own + "corpus", writable)
            else:
                self.assertNotIn(own + "corpus", writable)
        self.assertFalse(os.path.exists(self.executor.directory), "the work is removed")
        telemetrylab.verify(self.path("result"))
