"""Actual unprivileged deletion failures and interruptible terminal-boundary work."""
import contextlib
import io
import os
from pathlib import Path
import time
import unittest
from unittest import mock

from perflab import admission, budget, telemetryexec, telemetrylab
from perflab.values import ContractError
from tests import telemetry_fixtures as fx
from tests.support import TempDir
from tests.test_telemetry_cleanup import cli


class Terminal(TempDir):
    def setUp(self):
        super().setUp()
        self.binaries = fx.binaries(self)
        self.executor = fx.StandIn(self.binaries)

    def settings(self, **kwargs):
        return fx.settings(self, self.executor, experiments=["overhead"], rounds=1, **kwargs)

    @unittest.skipIf(os.geteuid() == 0, "requires an actual unprivileged filesystem owner")
    def test_untraversable_owned_directory_stops_versions_corpus_and_measurements(self):
        for target in ("version-baseline", "corpus", "overhead-0-baseline", "read_latency-0-baseline"):
            with self.subTest(target=target):
                label = target.replace("-", "_")
                executor = fx.StandIn(self.binaries)
                original = executor.run
                hostile = []

                def run(execution):
                    outcome = original(execution)
                    if os.path.basename(execution.cwd) == target:
                        Path(execution.cwd, "owned-file").write_bytes(b"candidate data")
                        os.chmod(execution.cwd, 0)
                        hostile.append(execution.cwd)
                    return outcome

                executor.run = run
                settings = fx.settings(self, executor, output=label, rounds=1,
                    experiments=["read_latency"] if target in ("corpus", "read_latency-0-baseline")
                    else ["overhead"])
                try:
                    result = telemetrylab.run(settings)
                    self.assertEqual(result["execution"]["cleanup"], "uncertain")
                    self.assertNotEqual(result["status"], "complete")
                    self.assertEqual(result["execution"]["retained"], settings.output + ".work")
                    self.assertEqual(executor.calls[-1].cwd, hostile[0])
                    self.assertTrue(os.path.lexists(hostile[0]))
                    before = len(executor.calls)
                    retry = fx.settings(self, executor, output=label + "_retry", work=settings.output + ".work")
                    with self.assertRaisesRegex(ContractError, "work directory"):
                        telemetrylab.run(retry)
                    self.assertEqual(len(executor.calls), before)
                finally:
                    for path in hostile:
                        os.chmod(path, 0o700)  # Explicit test-owned recovery after assertions.

    def test_final_work_deletion_error_is_not_a_complete_result(self):
        real = telemetrylab.shutil.rmtree
        def remove(path, *args, **kwargs):
            if path == self.path("result.work"):
                raise PermissionError("injected final directory deletion failure")
            return real(path, *args, **kwargs)
        with mock.patch.object(telemetrylab.shutil, "rmtree", remove):
            result = telemetrylab.run(self.settings())
        self.assertEqual(result["execution"]["cleanup"], "uncertain")
        self.assertEqual(result["status"], "partial")
        self.assertTrue(Path(result["execution"]["retained"]).is_dir())

    def test_binary_and_authority_admission_use_time_since_api_entry(self):
        for name in ("_file_sha256", "authority_digest"):
            now = [0.0]
            real = getattr(telemetrylab, name)
            def elapsed(*args, **kwargs):
                result = real(*args, **kwargs)
                now[0] += 14401
                return result
            with self.subTest(name=name), mock.patch.object(telemetrylab, name, elapsed), \
                    mock.patch.object(telemetrylab, "_executor") as launch:
                with self.assertRaises(budget.Expired):
                    telemetrylab.run(self.settings(clock=lambda: now[0]))
                launch.assert_not_called()
                self.assertEqual(self.executor.calls, [])

    def test_real_stalled_descriptor_read_is_interrupted_before_any_launch(self):
        real = admission.read_bounded
        def stalled(*args, **kwargs):
            if not str(args[3]).endswith("telemetry.toml"):
                time.sleep(30)
            return real(*args, **kwargs)
        offset = [0.0]
        configuration = telemetrylab.configuration
        def elapsed(*args):
            result = configuration(*args)
            offset[0] = 240 * 60 - 90 - .2
            return result
        start = time.monotonic()
        with mock.patch.object(telemetrylab, "configuration", elapsed), \
                mock.patch.object(admission, "read_bounded", stalled):
            with self.assertRaises(budget.Interrupted):
                telemetrylab.run(self.settings(clock=lambda: time.monotonic() + offset[0]))
        self.assertLess(time.monotonic() - start, 2)
        self.assertEqual(self.executor.calls, [])
        self.assertFalse(Path(self.path("result")).exists())

    def test_cli_build_record_admission_is_in_the_same_interruptible_budget(self):
        args = cli.parser().parse_args(["telemetry-run", "--profile", "manual", "--trigger", "manual", "--mode", "trusted_local",
            "--baseline-af", self.binaries["baseline"], "--candidate-af", self.binaries["candidate"],
            "--baseline-sha", fx.SHA["baseline"], "--candidate-sha", fx.SHA["candidate"],
            "--authority-sha", fx.SHA["authority"], "--baseline-build", self.path("record"),
            "--output", self.path("result")])
        real_run = telemetrylab.run
        def run(settings):
            settings.clock = lambda: time.monotonic() + offset[0]
            settings.executor = self.executor
            settings.lock_directory = self.tmp
            return real_run(settings)
        offset = [0.0]
        configuration = telemetrylab.configuration
        def elapsed(*args):
            result = configuration(*args)
            offset[0] = 240 * 60 - 90 - .3
            return result
        start = time.monotonic()
        with mock.patch.object(telemetrylab, "configuration", elapsed), \
                mock.patch.object(telemetrylab, "run", run), \
                mock.patch.object(telemetrylab, "read_build", side_effect=lambda *args: time.sleep(30)), \
                contextlib.redirect_stdout(io.TextIOWrapper(io.BytesIO())), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(args.handler(args), cli.EXIT_UNAVAILABLE)
        self.assertLess(time.monotonic() - start, 2)
        self.assertEqual(self.executor.calls, [])

    def test_stalled_corpus_copy_and_integrity_are_really_interrupted(self):
        for operation in ("copy", "integrity"):
            with self.subTest(operation=operation):
                offset = [0.0]
                executor = fx.StandIn(self.binaries)
                original = executor.run
                def run(execution):
                    result = original(execution)
                    if ((operation == "copy" and "generate" in execution.argv) or
                            (operation == "integrity" and len(executor.calls) == 1)):
                        offset[0] = 240 * 60 - 90 - 1.0
                    return result
                executor.run = run
                settings = fx.settings(self, executor, output=operation, rounds=1,
                    experiments=["read_latency"], clock=lambda: time.monotonic() + offset[0])
                patch = (mock.patch.object(admission, "copy_tree", side_effect=lambda *a, **k: time.sleep(30))
                         if operation == "copy" else
                         mock.patch.object(telemetrylab._Guard, "problems", side_effect=lambda *a: time.sleep(30)))
                start = time.monotonic()
                with patch:
                    result = telemetrylab.run(settings)
                self.assertLess(time.monotonic() - start, 3)
                self.assertNotEqual(result["status"], "complete")
                self.assertEqual(len(executor.calls), 3 if operation == "copy" else 1)
                self.assertTrue(all(item["status"] != "ok" for item in result["runs"]))
                self.assertEqual(result["execution"]["cleanup"], "confirmed")
                self.assertFalse(Path(settings.output + ".work").exists())

    def test_real_final_cleanup_and_publication_stalls_cannot_publish_complete_bundles(self):
        for operation in ("cleanup", "publication"):
            with self.subTest(operation=operation):
                offset = [0.0]
                executor = fx.StandIn(self.binaries)
                settings = fx.settings(self, executor, output=operation, rounds=1,
                    experiments=["overhead"], clock=lambda: time.monotonic() + offset[0])
                remove_owned = telemetrylab._remove_owned
                remove_tree = telemetrylab.shutil.rmtree
                def final_remove(path, *args, **kwargs):
                    if path == settings.output + ".work":
                        offset[0] = 240 * 60 - (1.2 if operation == "publication" else .2)
                    return remove_owned(path, *args, **kwargs)
                def stalled_remove(path, *args, **kwargs):
                    if path == settings.output + ".work":
                        time.sleep(30)
                    return remove_tree(path, *args, **kwargs)
                def stalled_manifest(output, *args):
                    Path(output, "manifest.json").write_bytes(b"{")
                    time.sleep(30)
                patch = (mock.patch.object(telemetrylab.shutil, "rmtree", stalled_remove)
                         if operation == "cleanup" else
                         mock.patch.object(telemetrylab.bundle, "write_manifest", stalled_manifest))
                start = time.monotonic()
                with mock.patch.object(telemetrylab, "_remove_owned", final_remove), patch, \
                        self.assertRaises((budget.Expired, budget.Interrupted)):
                    telemetrylab.run(settings)
                self.assertLess(time.monotonic() - start, 3)
                self.assertFalse(Path(settings.output, "manifest.json").exists())
                self.assertFalse(Path(settings.output, "result.json").exists())
                self.assertEqual(Path(settings.output + ".work").exists(), operation == "cleanup")
                if operation == "cleanup":
                    self.assertTrue(executor.pending)
                else:
                    self.assertIn("did not complete", Path(settings.output, "failure.txt").read_text())

    def test_binary_bytes_and_authority_traversal_are_bounded_before_reads(self):
        path = self.path("oversize")
        with open(path, "wb") as stream:
            stream.truncate(telemetrylab.BINARY_BYTES_MAX + 1)
        with mock.patch.object(admission, "read_bounded") as read:
            with self.assertRaisesRegex(ContractError, "binary byte bound"):
                telemetrylab._file_sha256(path)
            read.assert_not_called()
        authority = Path(self.path("authority"))
        authority.mkdir()
        for i in range(telemetrylab.AUTHORITY_FILES_MAX + 1):
            (authority / str(i)).mkdir()
        with mock.patch.object(telemetrylab, "authority_names", return_value=["authority"]):
            with self.assertRaisesRegex(ContractError, "entries"):
                telemetrylab.authority_files({}, self.tmp)

    def test_binary_special_files_links_and_replacements_are_refused(self):
        fifo = self.path("fifo")
        os.mkfifo(fifo)
        link = self.path("link")
        os.symlink(self.binaries["baseline"], link)
        for path in (fifo, link, "/dev/null"):
            with self.assertRaises(ContractError):
                telemetrylab._file_sha256(path)
        real = admission.read_bounded
        def replaced(fd, *args, **kwargs):
            result = real(fd, *args, **kwargs)
            path = self.binaries["baseline"]
            os.rename(path, path + ".old")
            Path(path).write_bytes(Path(path + ".old").read_bytes())
            return result
        with mock.patch.object(admission, "read_bounded", replaced):
            with self.assertRaisesRegex(ContractError, "changed"):
                telemetrylab._file_sha256(self.binaries["baseline"])
