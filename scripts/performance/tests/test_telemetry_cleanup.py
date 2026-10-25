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
import string
from unittest import mock

from perflab import contracts, hostprep, telemetryexec, telemetrylab
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

        real_host = hostprep.Host(run=run, statvfs=self.host.statvfs)
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

    def run_execution(self, owner, command=("/bin/true",), label="x"):
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

    def measure(self):
        for side in contracts.SIDES:
            binary = Path(self.tmp, side, "af")
            binary.parent.mkdir()
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
        os.mkdir(self.path("machine-lock"))

        def run(settings):
            settings.executor = self.executor
            self.executor.owner = settings.scratch_owner
            settings.lock_directory = self.path("machine-lock")
            settings.host_identity = lambda: fx.HOST
            return original(settings)

        with mock.patch.object(telemetrylab, "run", run), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
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

