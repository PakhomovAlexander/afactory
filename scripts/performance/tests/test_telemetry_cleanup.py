"""Workflow argv -> real CLI -> executor -> final cleanup, over owned real files.

Host mount commands and Docker are recorded stand-ins, not live containment evidence.
No supplied result document participates in the cleanup decision.
"""
import contextlib
import importlib.util
import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import string
import sys
import time
from unittest import mock

from perflab import builds, budget, capability, contracts, hostlock, hostprep, telemetryexec, telemetrylab
from perflab.values import ContractError
from tests import telemetry_fixtures as fx
from tests import test_workflow as workflow
from tests.test_continuous import StandInHost
from tests.test_telemetry_exec import ContainedBase, execution
from tests.support import CLI, ROOT, read

spec = importlib.util.spec_from_file_location("telemetry_cleanup_cli", CLI)
cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli)


class WorkflowCleanup(ContainedBase):
    def setUp(self):
        super().setUp()
        # ContainedBase's directories are used by the fake Docker executor; ci-scratch
        # prepares this distinct mount through the actual workflow/CLI preparation path.
        self.image = self.path("scratch.img")
        self.mount = self.path("scratch-fs")
        self.owner_path = self.path("telemetry-owner")
        self.host = StandInHost(100 << 30)
        recorded_run = self.host.run

        def run(argv, **kwargs):
            result = recorded_run(argv, **kwargs)
            if argv[0] == "truncate" and result[0] == 0:
                Path(argv[-1]).write_bytes(b"x" * 65536)
            if argv[:3] == ["sudo", "-n", "umount"] and result[0] == 0:
                # Model the visible empty underlying mountpoint after a real unmount.
                for path in Path(argv[-1]).iterdir():
                    if path.is_dir(): shutil.rmtree(path)
                    else: path.unlink()
            return result

        # Model the Linux runner just as we model its mount commands; the real
        # host platform refusal is covered separately by HostedScratch.
        real_host = hostprep.Host(run=run, statvfs=self.host.statvfs, system="linux")
        self.patch = mock.patch.object(hostprep, "Host", return_value=real_host)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.steps = workflow.steps(workflow.jobs(read(os.path.join(
            ROOT, ".github/workflows/performance-telemetry.yml")).decode())["measure"])
        self.values = dict(RUNNER_TEMP=self.tmp, WORK_BYTES="65536", IMAGE="unused",
                           PROFILE="manual", TRIGGER="manual", BASELINE=fx.SHA["baseline"],
                           CANDIDATE=fx.SHA["candidate"], CONTROL=fx.SHA["authority"],
                           GITHUB_RUN_ID="123", GITHUB_RUN_ATTEMPT="1")
        self.assertEqual(self.invoke("ci-scratch", cleanup=False), 0)
        self.executor.scratch_root = self.mount
        self.scratch = self.mount
        os.makedirs(self.executor.directory)
        self.run_dir = os.path.join(self.executor.directory, "one")
        os.mkdir(self.run_dir)

    def argv(self, command, cleanup=None):
        for step in self.steps:
            body = workflow.block(step, "run")
            if body is None:
                body = "\n".join(step).split("run: ", 1)[-1]
            for line in body.replace("\\\n", " ").splitlines():
                if "cli.py" in line and command in line:
                    if cleanup is not None and ("--cleanup" in line) != cleanup:
                        continue
                    args = shlex.split(string.Template(line).safe_substitute(self.values))
                    index = next(i for i, arg in enumerate(args) if arg.endswith("cli.py"))
                    return [arg for arg in args[index + 1:] if arg != "$@"]
        self.fail("workflow command missing: " + command)

    def invoke(self, command, cleanup=None):
        args = cli.parser().parse_args(self.argv(command, cleanup))
        self.stderr = io.StringIO()
        self.stdout = io.TextIOWrapper(io.BytesIO(), write_through=True)
        with contextlib.redirect_stderr(self.stderr), contextlib.redirect_stdout(self.stdout):
            if command == "ci-scratch":
                # A simulated host gets its own actual inode/fence. The AF sandbox's /tmp
                # deliberately is not a root-owned system directory; production refuses it.
                directory = getattr(self, "machine_lock_directory", self.path("machine-lock"))
                os.makedirs(directory, exist_ok=True)
                return cli.cmd_ci_scratch(args, lock_directory=directory)
            return args.handler(args)

    def retained(self):
        before = len(self.host.argv)
        self.assertNotEqual(self.invoke("ci-scratch", cleanup=True), 0)
        self.assertEqual(self.host.argv[before:], [], "unsafe cleanup issued host commands")
        self.assertTrue(os.path.isfile(self.image))
        self.assertTrue(os.path.isdir(self.mount))
        evidence = self.stdout.buffer.getvalue().decode() + self.stderr.getvalue()
        self.assertIn("retained recovery paths", evidence)
        self.assertIn(self.image, evidence)
        self.assertIn(self.mount, evidence)

    def owner(self):
        return hostprep.TelemetryOwner(self.owner_path, self.mount)

    def run_execution(self, owner, command=(sys.executable, "-I", "-c", "pass"), label="x"):
        self.executor.owner = owner
        return self.executor.run(execution(self, command, cwd=self.run_dir, label=label))

    def test_stopped_execution_cleans_even_without_a_result_boolean(self):
        with self.owner() as owner:
            result = self.run_execution(owner)
            self.assertEqual(result.cleanup, "confirmed")
            self.assertEqual(owner.doc["phase"], "stopped")
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)
        self.assertFalse(os.path.exists(self.image))
        self.assertEqual(self.host.argv[-3:], [["sudo", "-n", "umount", self.mount],
                                             ["rmdir", self.mount], ["rm", "-f", self.image]])

    def test_uncertain_removal_retains_and_stops_the_next_execution(self):
        Path(self.docker_dir, "keep").touch()
        with self.owner() as owner:
            self.assertEqual(self.run_execution(owner).cleanup, "uncertain")
            before = len(self.calls())
            self.assertEqual(self.run_execution(owner, label="next").cleanup, "uncertain")
            self.assertEqual(len(self.calls()), before)
            self.assertEqual(owner.doc["phase"], "uncertain")
        # Candidate-controlled successful result data is irrelevant to this authority.
        Path(self.mount, "result.json").write_text('{"cleanup":"confirmed"}')
        self.retained()
        self.assertTrue(Path(self.mount, "result.json").exists())
        self.assertNotEqual(self.invoke("telemetry-run"), 0)
        self.assertIn("unsettled launch", self.stderr.getvalue())

    def launch_exception(self, exception):
        # A signal/error can arrive after the daemon accepted Popen. Pending evidence
        # is durable before that call; no result exists on this path.
        with self.owner() as owner:
            with mock.patch.object(telemetryexec.subprocess, "Popen", side_effect=exception):
                if isinstance(exception, OSError):
                    self.assertEqual(self.run_execution(owner).cleanup, "uncertain")
                else:
                    with self.assertRaises(type(exception)):
                        self.run_execution(owner)
            self.assertEqual(owner._read()["phase"], "active")
        self.retained()

    def test_cancellation_at_launch_retains_before_result_publication(self):
        self.launch_exception(KeyboardInterrupt())

    def test_exception_at_launch_retains_before_result_publication(self):
        self.launch_exception(RuntimeError("post-launch exception"))

    def test_oserror_at_launch_retains_and_stops_the_next_execution(self):
        self.launch_exception(OSError("Popen ownership unknown"))
        before = len(self.calls()) if Path(self.docker_dir, "docker-calls.jsonl").exists() else 0
        result = self.run_execution(None, label="next")
        self.assertEqual(result.cleanup, "uncertain")
        self.assertEqual(self.executor.names, 1)
        self.assertEqual(before, 0)

    def test_cleanup_during_active_controller_refuses_the_lock(self):
        with self.owner() as owner:
            owner.begin("af-perf-telemetry-held")
            self.retained()
        self.retained()  # Controller exit without a settlement remains active.

    def test_missing_corrupt_foreign_and_changed_filesystem_evidence_retain(self):
        with self.owner() as owner:
            self.assertEqual(self.run_execution(owner).cleanup, "confirmed")
        state = Path(self.owner_path, "state.json")
        original = state.read_bytes()
        for change in ("missing", "corrupt", "foreign", "oversized", "version", "image",
                       "mount", "uid", "symlink", "fifo"):
            with self.subTest(change=change):
                doc = json.loads(original)
                state.unlink()
                if change == "corrupt":
                    state.write_bytes(b"{")
                elif change == "oversized":
                    state.write_bytes(b" " * 8193)
                elif change == "symlink":
                    target = Path(self.path("candidate-evidence"))
                    target.write_bytes(original)
                    os.chmod(target, 0o600)
                    state.symlink_to(target)
                elif change == "fifo":
                    os.mkfifo(state, 0o600)
                elif change != "missing":
                    if change == "foreign": doc["control"][1] += 1
                    if change == "version": doc["schema"] = "af.perf-telemetry-owner/0"
                    if change == "image": doc["image_identity"][1] += 1
                    if change == "mount": doc["mount_identity"][1] += 1
                    if change == "uid": doc["owner_uid"] += 1
                    state.write_text(json.dumps(doc))
                if state.exists() and not state.is_symlink(): os.chmod(state, 0o600)
                self.retained()
                if state.is_symlink() or state.exists(): state.unlink()
                state.write_bytes(original)
                os.chmod(state, 0o600)

    def test_foreign_cleanup_arguments_and_copied_owner_refuse(self):
        with self.owner() as owner:
            with self.assertRaisesRegex(ContractError, "cleanup unconfirmed"):
                owner.cleanup(self.path("foreign.img"))
        copied = self.path("copied-owner")
        shutil.copytree(self.owner_path, copied)
        with self.assertRaisesRegex(ContractError, "foreign scratch"):
            with hostprep.TelemetryOwner(copied, self.mount):
                pass
        self.assertEqual(self.host.argv[-1][-2:], ["0700", self.mount])

    def test_safe_prelaunch_refusal_cleans_from_workflow_commands(self):
        # Real telemetry-run CLI refuses a nonexistent binary before any possible launch.
        self.assertNotEqual(self.invoke("telemetry-run"), 0)
        with self.owner() as owner:
            self.assertEqual((owner.doc["phase"], owner.doc["launches"]), ("ready", 0))
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)
        self.assertFalse(os.path.exists(self.image))

    def measure(self, configure=None):
        for side in contracts.SIDES:
            binary = Path(self.tmp, side, "af")
            binary.parent.mkdir(exist_ok=True)
            binary.write_text("#!/bin/sh\necho 'af 0.0.0'\n")
            binary.chmod(0o755)
            record = {"schema": contracts.TELEMETRY_BUILD_SCHEMA, "side": side,
                      "source_sha": fx.SHA[side],
                      "binary_sha256": "sha256:" + hashlib.sha256(binary.read_bytes()).hexdigest(),
                      "image": self.executor.image.reference, "rustc": "fixture rustc",
                      "cargo": "fixture cargo", "command": "fixture build"}
            binary.with_name("af.build.json").write_bytes(contracts.write_bytes(record, "telemetry-build"))
        args = cli.parser().parse_args(self.argv("telemetry-run") + ["--experiment", "overhead",
            "--rounds", "1", "--pairs", "1", "--scale", "smoke", "--corpus-runs", "10"])
        original = telemetrylab.run
        lock_directory = getattr(self, "machine_lock_directory", self.path("machine-lock"))
        os.makedirs(lock_directory, exist_ok=True)

        def run(settings):
            settings.executor = self.executor
            self.executor.owner = settings.scratch_owner
            settings.lock_directory = lock_directory
            settings.host_identity = lambda: fx.HOST
            if configure is not None:
                configure(settings)
            return original(settings)

        with mock.patch.object(telemetrylab, "run", run), \
                contextlib.redirect_stdout(io.TextIOWrapper(io.BytesIO(), write_through=True)), \
                contextlib.redirect_stderr(io.StringIO()):
            return args.handler(args)

    def test_workflow_cli_cancellation_before_result_keeps_actual_work(self):
        # The CLI's parsed workflow argv carries the actual owner into real _run_locked
        # and Contained.launch. Only the host/runtime are stand-ins.
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        with mock.patch.object(telemetryexec.subprocess, "Popen", side_effect=KeyboardInterrupt()):
            with self.assertRaises(KeyboardInterrupt):
                self.measure()
        self.assertTrue(os.path.isdir(self.executor.directory))
        self.assertFalse(Path(self.tmp, "telemetry-result", telemetrylab.RESULT_JSON).exists())
        self.retained()
        self.assertNotEqual(self.invoke("telemetry-run"), 0)
        self.assertIn("unsettled launch", self.stderr.getvalue())

    def real_preparation(self, settings):
        settings.executor = None
        settings.mode = "linux_container"
        settings.image = "sha256:" + "0" * 64

    def image_inspection(self, available):
        def inspect(argv, *args, **kwargs):
            self.assertEqual(argv[1:3], ["image", "inspect"])
            with self.owner_state() as doc:
                self.assertTrue(doc["filesystem_pending"], "inspection must follow fencing")
            return type("Reply", (), {"returncode": 0 if available else 1,
                "stdout": (b'"sha256:' + b'0' * 64 + b'"\t"linux"\t"amd64"\t[]\t[]'),
                "stderr": b"image unavailable"})()
        return inspect

    @contextlib.contextmanager
    def owner_state(self):
        # Read the actual durable record while its controller holds the owner lock.
        yield json.loads(Path(self.owner_path, "state.json").read_bytes())

    def test_fenced_image_and_inert_host_refusals_allow_next_controller_and_workflow_cleanup(self):
        for boundary in ("image", "host"):
            with self.subTest(boundary=boundary):
                with mock.patch.object(builds, "_run", self.image_inspection(boundary == "host")), \
                        mock.patch.object(capability, "docker_info", return_value={
                            "unavailable": "runtime unavailable"}), \
                        mock.patch.object(capability, "scratch_capability") as probe:
                    self.assertEqual(self.measure(self.real_preparation), cli.EXIT_UNAVAILABLE)
                    probe.assert_not_called()
                with self.owner() as owner:
                    self.assertEqual(owner.doc["launches"], 0)
                    self.assertFalse(owner.doc["filesystem_pending"])
                    owner.available()  # A new controller can acquire the real persisted owner.
                self.assertEqual(self.executor.names, 0)
                self.assertFalse(Path(self.tmp, "telemetry-result").exists())
        # The next actual controller runs normally; the workflow's cleanup then admits it.
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        self.assertEqual(self.measure(), cli.EXIT_UNAVAILABLE)
        self.assertEqual(self.executor.names, 4)
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)

    def test_fenced_safe_image_refusal_admits_real_ci_scratch_without_candidate_launch(self):
        with mock.patch.object(builds, "_run", self.image_inspection(False)):
            self.assertEqual(self.measure(self.real_preparation), cli.EXIT_UNAVAILABLE)
        self.assertEqual(self.executor.names, 0)
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)
        self.assertFalse(os.path.exists(self.image))

    def test_mutating_probe_and_opaque_preparation_interruptions_keep_persisted_fence(self):
        for boundary in ("probe", "image_hook", "host_hook", "executor"):
            with self.subTest(boundary=boundary):
                # Each injected interruption is a distinct fixture machine. Never erase a
                # prior scenario's durable global fence just to run the next boundary.
                self.machine_lock_directory = self.path("machine-lock-" + boundary)
                failure = {"probe": OSError("partially mutating probe"),
                           "image_hook": telemetrylab.Unavailable("refused", "opaque image hook"),
                           "host_hook": KeyboardInterrupt("preparation interrupted"),
                           "executor": RuntimeError("opaque executor preparation")}[boundary]
                def mutate(*args, **kwargs):
                    Path(self.mount, "preparation-data").write_bytes(b"possibly still owned")
                    raise failure
                def configure(settings):
                    self.real_preparation(settings)
                    if boundary == "image_hook": settings.image_identity = mutate
                    if boundary == "host_hook": settings.contained_host = mutate
                with mock.patch.object(builds, "_run", self.image_inspection(True)), \
                        mock.patch.object(capability, "docker_info", return_value={
                            "unavailable": None, "identity": "linux fixture cgroup2"}), \
                        mock.patch.object(capability, "scratch_capability", mutate), \
                        (mock.patch.object(telemetrylab, "_executor", mutate)
                         if boundary == "executor" else contextlib.nullcontext()):
                    if isinstance(failure, (KeyboardInterrupt, RuntimeError)):
                        with self.assertRaises(type(failure)):
                            self.measure(configure)
                    else:
                        self.assertEqual(self.measure(configure),
                            cli.EXIT_INVALID if isinstance(failure, OSError) else cli.EXIT_UNAVAILABLE)
                with self.owner() as owner:
                    self.assertTrue(owner.doc["filesystem_pending"])
                    self.assertEqual(owner.doc["launches"], 0)
                    with self.assertRaises(ContractError): owner.available()
                self.retained()
                self.assertEqual(self.executor.names, 0)
                self.assertEqual(Path(self.mount, "preparation-data").read_bytes(),
                                 b"possibly still owned")
                with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment fence"):
                    hostlock.MachineLock(self.machine_lock_directory)
                # Test-owned recovery, never product compensation. The machine fence stays.
                with self.owner() as owner:
                    owner.doc["filesystem_pending"] = False
                    owner._write()

    def assert_unused_fence_settlement(self, boundary, preparation_delay=0):
        # Every injected failed cleanup remains fenced. Later fault scenarios are separate
        # fixture machines; no automatic or test recovery removes an uncertain machine fence.
        self.machine_lock_directory = self.path("machine-lock-settlement-" + boundary)
        offset = [0.0]
        actual_sync = telemetrylab._sync
        calls, settlements, failures = [], [], []
        def sync(allowance, step, function, *args, **kwargs):
            if step != "settling unused telemetry work":
                return actual_sync(allowance, step, function, *args, **kwargs)
            offset[0] = allowance.deadline - time.monotonic() + (
                .01 if boundary == "expired" else -.08)
            start = time.monotonic()
            try:
                return actual_sync(allowance, step, function, *args, **kwargs)
            except BaseException as error:
                failures.append(error)
                raise
            finally:
                # Include deadline refusal and interrupted/failed settlement returns.
                settlements.append(time.monotonic() - start)
        actual_write = hostprep.TelemetryOwner._write
        def write(owner):
            if not owner.doc["filesystem_pending"]:
                calls.append(time.monotonic())
                if boundary == "error": raise OSError("settlement write failed")
                time.sleep(30)
            return actual_write(owner)
        actual_inspect = self.image_inspection(False)
        def inspect_image(*args, **kwargs):
            # Unrelated preparation precedes the settlement observation, inside the
            # real fenced executor route, without replacing its persisted ownership.
            if preparation_delay:
                time.sleep(preparation_delay)
            return actual_inspect(*args, **kwargs)
        def configure(settings):
            self.real_preparation(settings)
            settings.clock = lambda: time.monotonic() + offset[0]
        start = time.monotonic()
        with mock.patch.object(builds, "_run", inspect_image), \
                mock.patch.object(telemetrylab, "_sync", sync), \
                mock.patch.object(hostprep.TelemetryOwner, "_write", write):
            self.assertEqual(self.measure(configure), cli.EXIT_UNAVAILABLE)
        total_elapsed = time.monotonic() - start
        self.assertEqual(len(settlements), 1, "unused-work settlement boundary was not reached once")
        self.assertLess(settlements[0], 1)
        self.assertEqual(len(failures), 1)
        self.assertIsInstance(failures[0], {"error": OSError, "stall": budget.Interrupted,
                                          "expired": budget.Expired}[boundary])
        self.assertEqual(len(calls), 0 if boundary == "expired" else 1)
        self.assertEqual(self.executor.names, 0)
        with self.owner() as owner:
            self.assertTrue(owner.doc["filesystem_pending"])
        self.retained()
        with self.owner() as owner:
            owner.doc["filesystem_pending"] = False  # Test-owned recovery.
            owner._write()
        return total_elapsed

    def test_failed_or_expired_unused_fence_settlement_is_bounded_and_conservative(self):
        for boundary in ("error", "stall", "expired"):
            with self.subTest(boundary=boundary):
                self.assert_unused_fence_settlement(boundary)

    def test_slow_preparation_does_not_consume_unused_fence_settlement_measurement(self):
        for boundary in ("stall", "expired"):
            with self.subTest(boundary=boundary):
                elapsed = self.assert_unused_fence_settlement(boundary, preparation_delay=1.05)
                # This deliberately fails the old total-setup bound, while the shared
                # assertions still check real interruption/expiry and durable retention.
                self.assertGreaterEqual(elapsed, 1.05)

    def test_workflow_cli_stopped_runs_allow_cleanup_after_result_publication(self):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        # Fixture binaries deliberately provide no measurement protocol. Their lifecycle
        # still settles; failed/admitted measurements are never cleanup authority.
        self.assertEqual(self.measure(), cli.EXIT_UNAVAILABLE)
        self.assertTrue(Path(self.tmp, "telemetry-result", telemetrylab.RESULT_JSON).exists())
        with self.owner() as owner:
            self.assertEqual(owner.doc["phase"], "stopped")
            self.assertEqual(owner.doc["launches"], 4)
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)

    def test_workflow_cli_uncertainty_publishes_retained_state_and_never_launches_again(self):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        Path(self.docker_dir, "keep").touch()
        self.assertEqual(self.measure(), cli.EXIT_UNAVAILABLE)
        result = json.loads(Path(self.tmp, "telemetry-result", telemetrylab.RESULT_JSON).read_bytes())
        self.assertEqual(result["execution"]["cleanup"], "uncertain")
        self.assertEqual(result["execution"]["retained"], self.executor.directory)
        self.assertEqual(len([call for call in self.calls() if call[0] == "run"]), 1)
        self.retained()
        self.assertTrue(Path(self.executor.directory).exists())

    def test_workflow_cli_publication_exception_after_stopping_is_safe_to_clean(self):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        with mock.patch.object(telemetrylab, "write_bundle", side_effect=KeyboardInterrupt()):
            with self.assertRaises(KeyboardInterrupt):
                self.measure()
        with self.owner() as owner:
            self.assertEqual(owner.doc["phase"], "stopped")
        self.assertFalse(Path(self.tmp, "telemetry-result", telemetrylab.RESULT_JSON).exists())
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)

    def test_actual_untraversable_execution_invalidates_stopped_owner_and_workflow_cleanup(self):
        self.assertNotEqual(os.geteuid(), 0, "requires actual unprivileged deletion")
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        original = self.executor.run
        hostile = []
        def run(execution):
            outcome = original(execution)
            Path(execution.cwd, "candidate-file").write_bytes(b"owned data")
            os.chmod(execution.cwd, 0)
            hostile.append(execution.cwd)
            return outcome
        try:
            with mock.patch.object(self.executor, "run", run):
                self.assertEqual(self.measure(), cli.EXIT_UNAVAILABLE)
            result = json.loads(Path(self.tmp, "telemetry-result", telemetrylab.RESULT_JSON).read_bytes())
            self.assertEqual(result["execution"]["cleanup"], "uncertain")
            self.assertNotEqual(result["status"], "complete")
            self.assertEqual(len(hostile), 1)
            with self.owner() as owner:
                self.assertEqual(owner.doc["phase"], "uncertain")
                self.assertEqual(owner.doc["launches"], 1)
            self.retained()
            self.assertNotEqual(self.invoke("telemetry-run"), 0)
            self.assertIn("unsettled launch", self.stderr.getvalue())
            self.assertEqual(os.stat(hostile[0]).st_mode & 0o777, 0)
        finally:
            for path in hostile:
                os.chmod(path, 0o700)  # Recovery belongs to this test, after refusal assertions.

    def test_final_filesystem_deletion_error_invalidates_hosted_cleanup_authority(self):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        original = shutil.rmtree
        def remove(path, *args, **kwargs):
            if path == self.executor.directory:
                raise PermissionError("injected work-root deletion failure")
            return original(path, *args, **kwargs)
        with mock.patch.object(telemetrylab.shutil, "rmtree", remove):
            self.assertEqual(self.measure(), cli.EXIT_UNAVAILABLE)
        with self.owner() as owner:
            self.assertEqual(owner.doc["phase"], "uncertain")
        self.retained()
        self.assertTrue(os.path.isdir(self.executor.directory))

    def test_missing_control_directory_after_launch_preserves_recovery_state(self):
        with self.owner() as owner:
            owner.begin("af-perf-telemetry-possible-launch")
        renamed = self.owner_path + "-retained"
        os.rename(self.owner_path, renamed)
        self.retained()
        self.assertEqual(json.loads(Path(renamed, "state.json").read_bytes())["phase"], "active")

    def test_replaced_image_inode_after_launch_never_authorizes_unmount(self):
        with self.owner() as owner:
            self.assertEqual(self.run_execution(owner).cleanup, "confirmed")
        os.rename(self.image, self.image + "-retained")
        Path(self.image).write_bytes(b"x" * 65536)
        self.retained()
        self.assertTrue(Path(self.image + "-retained").exists())

    def test_persisted_work_fence_survives_interrupted_cleanup_boundaries(self):
        # Use actual owner records, the real timer and the workflow's cleanup admission.
        # The last stopped record must be unsafe even if uncertainty publication fails.
        for boundary in ("read", "write", "replace", "file_sync", "directory_sync",
                         "delete", "settle_read", "settle_write"):
            with self.subTest(boundary=boundary):
                if not os.path.isdir(self.run_dir):
                    os.mkdir(self.run_dir)
                Path(self.run_dir, "candidate-data").write_bytes(b"retained")
                with self.owner() as owner:
                    # Each subcase's explicit test-owned recovery restores the initial state.
                    owner.prepare_work()
                    self.executor.pending = False
                    self.assertEqual(self.run_execution(owner).cleanup, "confirmed")
                    persisted = json.loads(Path(self.owner_path, "state.json").read_bytes())
                    self.assertEqual(persisted["phase"], "stopped")
                    self.assertTrue(persisted["filesystem_pending"])
                    state = {"problems": []}
                    real_read, real_write = owner._read, owner._write
                    real_sync, real_replace = os.fsync, os.replace
                    def stall():
                        self.assertGreater(signal.getitimer(signal.ITIMER_REAL)[0], 0)
                        time.sleep(30)
                    def read_owner():
                        if boundary == "read" or (boundary == "settle_read"
                                                  and not os.path.exists(self.run_dir)):
                            stall()
                        return real_read()
                    def write_owner():
                        if boundary == "write" or (boundary == "settle_write"
                                                   and not os.path.exists(self.run_dir)):
                            stall()
                        return real_write()
                    def sync(fd):
                        if ((boundary == "directory_sync" and fd == owner.directory)
                                or (boundary == "file_sync" and fd != owner.directory)):
                            stall()
                        return real_sync(fd)
                    def replace(*args, **kwargs):
                        if boundary == "replace": stall()
                        return real_replace(*args, **kwargs)
                    start = time.monotonic()
                    with mock.patch.object(owner, "_read", read_owner), \
                            mock.patch.object(owner, "_write", write_owner), \
                            mock.patch.object(os, "fsync", sync), \
                            mock.patch.object(os, "replace", replace), \
                            mock.patch.object(telemetrylab.shutil, "rmtree",
                                side_effect=lambda *a, **k: stall()) if boundary == "delete" else contextlib.nullcontext():
                        self.assertFalse(telemetrylab._remove_owned(self.run_dir,
                            type("Settings", (), {"scratch_owner": owner})(), self.executor,
                            state, budget.Budget(start + .08, time.monotonic, 0),
                            self.executor.directory))
                    self.assertLess(time.monotonic() - start, 1)
                    self.assertEqual(state["cleanup"], "uncertain")
                    self.assertTrue(self.executor.pending)
                    self.assertTrue(Path(self.executor.directory).is_dir())
                    self.assertTrue(real_read()["filesystem_pending"])
                    before = len(self.calls())
                    self.assertEqual(self.run_execution(owner, label="refused").cleanup,
                                     "uncertain")
                    self.assertEqual(len(self.calls()), before)
                self.retained()
                with self.owner() as reopened:
                    with self.assertRaisesRegex(ContractError, "unsettled launch"):
                        reopened.available()
                    # Explicit fixture recovery only, after verifying product refusal.
                    reopened.doc.update(phase="stopped", filesystem_pending=False)
                    reopened._write()

    def test_exception_after_container_removal_preserves_work_and_original_failure(self):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        original = hostprep.TelemetryOwner.finish
        def stopped(owner, *args):
            original(owner, *args)
            raise RuntimeError("after confirmed container removal")
        with mock.patch.object(hostprep.TelemetryOwner, "finish", stopped):
            with self.assertRaisesRegex(RuntimeError, "after confirmed container removal"):
                self.measure()
        with self.owner() as owner:
            self.assertEqual(owner.doc["phase"], "stopped")
            self.assertTrue(owner.doc["filesystem_pending"])
        self.retained()
        self.assertEqual(len([call for call in self.calls() if call[0] == "run"]), 1)
        self.assertTrue(Path(self.executor.directory).is_dir())
        self.assertFalse(Path(self.tmp, "telemetry-result", "manifest.json").exists())
        self.assertFalse(Path(self.tmp, "telemetry-result", "result.json").exists())

    def test_failed_fence_before_setup_never_starts_candidate_work(self):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        original = hostprep.TelemetryOwner._write
        def write(owner):
            if owner.doc["filesystem_pending"]:
                raise OSError("initial fence publication failed")
            return original(owner)
        with mock.patch.object(hostprep.TelemetryOwner, "_write", write):
            self.assertNotEqual(self.measure(), 0)
        self.assertEqual(self.executor.names, 0)
        self.assertFalse(Path(self.executor.directory).exists())
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)

    def assert_stalled_owner_fence(self, preparation_delay=0):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        offset = [0.0]
        actual_sync, actual_write = telemetrylab._sync, hostprep.TelemetryOwner._write
        writes, interruptions, boundaries = [], [], []
        selected = []
        enforcement, active = [], []

        @contextlib.contextmanager
        def preparation(step):
            self.assertEqual(step, "removing telemetry scratch")
            self.assertFalse(enforcement, "production enforcement must enter once")
            enforcement.append("enter")
            active.append(True)
            try:
                yield  # The selected ownership write below arms the real timer.
                self.fail("the stalled ownership write returned")
            finally:
                active.clear()
                enforcement.append("exit")

        def sync(allowance, step, function, *args, **kwargs):
            if step != "removing telemetry scratch":
                return actual_sync(allowance, step, function, *args, **kwargs)
            self.assertFalse(selected, "cleanup must not retry")
            boundaries.append(step)
            selected.append((allowance, allowance.enforce))
            # Real controller preparation and owner reads precede the measured write.
            time.sleep(preparation_delay)
            with mock.patch.object(allowance, "enforce", preparation):
                return actual_sync(allowance, step, function, *args, **kwargs)

        def write(owner):
            if owner.doc["phase"] == "uncertain":
                self.assertEqual((enforcement, active), (["enter"], [True]),
                                 "production enforcement must precede the blocked ownership write")
                enforcement.append("blocked")
                self.assertEqual(len(selected), 1)
                allowance, enforce = selected[0]
                self.assertEqual(allowance.reserve, 0)
                writes.append(time.monotonic())
                offset[0] = allowance.deadline - writes[0] - .08
                try:
                    with enforce("removing telemetry scratch"):
                        self.assertGreater(signal.getitimer(signal.ITIMER_REAL)[0], 0)
                        time.sleep(30)
                        self.fail("the real deadline failed to interrupt ownership I/O")
                except budget.Interrupted as error:
                    interruptions.append(str(error))
                    raise
            return actual_write(owner)

        start = time.monotonic()
        with mock.patch.object(telemetrylab, "_sync", sync), \
                mock.patch.object(hostprep.TelemetryOwner, "_write", write):
            self.assertEqual(self.measure(lambda settings: setattr(settings, "clock",
                lambda: time.monotonic() + offset[0])), cli.EXIT_UNAVAILABLE)
        elapsed = time.monotonic() - start
        self.assertEqual(enforcement, ["enter", "blocked", "exit"])
        self.assertFalse(active)
        self.assertEqual(len(boundaries), 1, "cleanup boundary must be reached")
        self.assertEqual(len(writes), 1, "failure handlers must not retry ownership I/O")
        self.assertLess(time.monotonic() - writes[0], 1)
        self.assertEqual(len(interruptions), 1)
        self.assertIn("total deadline ended removing telemetry scratch", interruptions[0])
        with self.owner() as owner:
            self.assertEqual(owner.doc["phase"], "stopped")
            self.assertTrue(owner.doc["filesystem_pending"])
        self.assertEqual(len([call for call in self.calls() if call[0] == "run"]), 1)
        self.assertTrue(Path(self.executor.directory).is_dir())
        self.assertFalse(Path(self.tmp, "telemetry-result", "manifest.json").exists())
        self.assertFalse(Path(self.tmp, "telemetry-result", "result.json").exists())
        self.retained()
        self.assertNotEqual(self.invoke("telemetry-run"), 0)
        self.assertIn("unsettled launch", self.stderr.getvalue())
        return elapsed

    def test_public_run_stalled_owner_fence_does_not_retry_after_expiry(self):
        self.assert_stalled_owner_fence()

    def test_slow_preparation_still_reaches_and_interrupts_public_run_owner_fence(self):
        self.assertGreaterEqual(self.assert_stalled_owner_fence(preparation_delay=1.05), 1.05)

    def assert_final_work_settlement_stall(self, boundary, preparation_delay=0):
        entered, interruptions = [], []
        enforcement, active = [], []
        def clock():
            return time.monotonic() - entered[0] if entered else 0.0
        allowance = budget.Budget(.08, clock, 0)
        enforce = allowance.enforce

        @contextlib.contextmanager
        def preparation(step):
            self.assertEqual(step, "removing telemetry scratch")
            self.assertFalse(entered)
            self.assertFalse(enforcement, "production enforcement must enter once")
            enforcement.append("enter")
            active.append(True)
            try:
                yield  # Execute real fencing and deletion before the measured operation.
                self.fail("the stalled settlement returned")
            finally:
                active.clear()
                enforcement.append("exit")

        def stall():
            self.assertEqual((enforcement, active), (["enter"], [True]),
                             "production enforcement must precede the blocked settlement I/O")
            enforcement.append("blocked")
            self.assertFalse(entered, "settlement I/O must not retry")
            self.assertEqual(clock(), 0)
            self.assertEqual((allowance.deadline, allowance.reserve), (.08, 0))
            entered.append(time.monotonic())
            try:
                with enforce("removing telemetry scratch"):
                    self.assertGreater(signal.getitimer(signal.ITIMER_REAL)[0], 0)
                    time.sleep(30)
                    self.fail("the real deadline failed to interrupt settlement I/O")
            except budget.Interrupted as error:
                interruptions.append(str(error))
                raise

        actual_remove = shutil.rmtree
        def remove(*args, **kwargs):
            time.sleep(preparation_delay)
            return actual_remove(*args, **kwargs)

        start = time.monotonic()
        with self.owner() as owner:
            owner.prepare_work()
            self.executor.pending = False
            self.assertEqual(self.run_execution(owner).cleanup, "confirmed")
            original = owner.work_cleaned
            def cleaned():
                with mock.patch.object(owner, "_" + boundary, side_effect=stall):
                    original()
            state = {"problems": []}
            with mock.patch.object(owner, "work_cleaned", cleaned), \
                    mock.patch.object(allowance, "enforce", preparation), \
                    mock.patch.object(telemetrylab.shutil, "rmtree", remove):
                self.assertFalse(telemetrylab._remove_owned(self.executor.directory,
                    type("Settings", (), {"scratch_owner": owner})(), self.executor,
                    state, allowance, self.executor.directory))
            elapsed = time.monotonic() - start
            self.assertEqual(enforcement, ["enter", "blocked", "exit"])
            self.assertFalse(active)
            self.assertEqual(len(entered), 1, "settlement boundary must be reached")
            self.assertLess(time.monotonic() - entered[0], 1)
            self.assertEqual(len(interruptions), 1)
            self.assertIn(interruptions[0], state["stopped"])
            self.assertFalse(os.path.exists(self.executor.directory))
            self.assertEqual(state["retained"], self.mount)
            self.assertIn("directory removed", state["stopped"])
            self.assertTrue(owner._read()["filesystem_pending"])
        self.retained()
        return elapsed

    def test_final_work_settlement_stall_retains_the_actual_mount_recovery_path(self):
        self.assert_final_work_settlement_stall("read")

    def test_final_work_settlement_write_stall_retains_the_actual_mount_recovery_path(self):
        self.assert_final_work_settlement_stall("write")

    def test_slow_deletion_still_reaches_and_interrupts_final_work_settlement_read(self):
        self.assertGreaterEqual(self.assert_final_work_settlement_stall("read", 1.05), 1.05)

    def test_slow_deletion_still_reaches_and_interrupts_final_work_settlement_write(self):
        self.assertGreaterEqual(self.assert_final_work_settlement_stall("write", 1.05), 1.05)

    @contextlib.contextmanager
    def bypass_cleanup_enforcement(self):
        original = telemetrylab._sync
        def bypass(allowance, step, function, *args, **kwargs):
            if step == "removing telemetry scratch":
                # Mutation control: remove only production's enforcement context.
                return function(*args, **kwargs)
            return original(allowance, step, function, *args, **kwargs)
        with mock.patch.object(telemetrylab, "_sync", bypass):
            yield

    def test_owner_fence_fixture_refuses_bypassed_production_enforcement(self):
        with self.assertRaisesRegex(AssertionError, "production enforcement must precede"):
            with self.bypass_cleanup_enforcement():
                self.assert_stalled_owner_fence()

    def test_settlement_read_fixture_refuses_bypassed_production_enforcement(self):
        with self.assertRaisesRegex(AssertionError, "production enforcement must precede"):
            with self.bypass_cleanup_enforcement():
                self.assert_final_work_settlement_stall("read")

    def test_settlement_write_fixture_refuses_bypassed_production_enforcement(self):
        with self.assertRaisesRegex(AssertionError, "production enforcement must precede"):
            with self.bypass_cleanup_enforcement():
                self.assert_final_work_settlement_stall("write")

    def test_controller_loss_before_launch_preserves_the_setup_fence(self):
        with self.owner() as owner:
            owner.prepare_work()
            self.assertEqual(owner.doc["phase"], "ready")
        self.retained()
        with self.owner() as owner:
            with self.assertRaisesRegex(ContractError, "unsettled launch"):
                owner.available()

    def test_successful_multiple_executions_allow_a_fresh_controller_launch(self):
        os.rmdir(self.run_dir)
        os.rmdir(self.executor.directory)
        self.assertEqual(self.measure(), cli.EXIT_UNAVAILABLE)
        with self.owner() as owner:
            self.assertEqual(owner.doc["launches"], 4)
            self.assertFalse(owner.doc["filesystem_pending"])
            owner.available()
        os.rename(self.path("telemetry-result"), self.path("first-result"))
        self.assertEqual(self.measure(), cli.EXIT_UNAVAILABLE)
        with self.owner() as owner:
            self.assertEqual(owner.doc["launches"], 8)
            self.assertFalse(owner.doc["filesystem_pending"])
        self.assertEqual(self.invoke("ci-scratch", cleanup=True), 0)

