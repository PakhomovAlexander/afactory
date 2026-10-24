"""Where a telemetry experiment's script runs and what bounds it (ADR-0200).

The trusted local executor runs real short-lived processes here: a script that leaves a process
behind in its group, one that outlives the run's deadline and one that writes past its room.
The contained executor runs through a stand-in `docker` client that executes the command on
this host and records its arguments; what is exercised is the container's declared boundary
(only the execution's own scratch directory writable, nothing else of the controller or the
other side bound, network none, read-only root, limits) and how an unconfirmed removal or a
changed scratch filesystem is an uncertain cleanup. No measurement is made.
"""
import json
import os
import sys
import time

from perflab import builds, supervise, telemetryexec, telemetrylab
from perflab.values import ContractError
from tests import telemetry_fixtures as fx
from tests.support import TempDir

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
    env = {"PATH": "/usr/bin:/bin", "HOME": cwd, "TMPDIR": cwd}
    return telemetryexec.Execution(argv, env, cwd, [cwd], list(readable), capture, label, room,
                                   time.monotonic() + seconds, 1 << 30)


def alive(pid):
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
        pid = int(open(marker).read())
        deadline = time.monotonic() + 5
        while alive(pid) and time.monotonic() < deadline:
            time.sleep(0.05)
        try:
            os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            pass
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
