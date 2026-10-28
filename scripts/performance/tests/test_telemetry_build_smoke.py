"""Smoke maintenance uses real CLI children/flocks; no Docker execution is claimed."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from unittest import mock

from perflab import hostlock
from tests import telemetry_build_smoke as smoke
from tests.support import ROOT, TempDir

LAB = os.path.join(ROOT, "scripts", "performance")
CHILD = r'''
import sys
sys.path.insert(0, %r)
from perflab import hostlock, telemetrybuild
from perflab.values import ContractError
from tests import telemetry_build_smoke as smoke
class TestLock(hostlock.MachineLock):
    def __init__(self):
        super().__init__(%r)
hostlock.MachineLock = TestLock
def build(*args):
    if not %r:
        raise ContractError("compile failed; no binary admitted")
telemetrybuild.run = build
smoke.fixture_child(%r, ["telemetry-contained-build", "--source", "unused",
    "--source-sha", "a" * 40, "--output", "unused", "--image", smoke.IMAGE,
    "--scratch-fs", "unused"])
'''


class FixtureMaintenance(TempDir):
    def setUp(self):
        super().setUp()
        self.locks = self.path("locks")
        os.mkdir(self.locks)
        self.lock = os.path.join(self.locks, hostlock.MACHINE_LOCK_NAME)
        self.fence = self.lock + ".fence"

    def child(self, root, success=False):
        receipt = root / "machine-guard.json"
        result = subprocess.run([sys.executable, "-I", "-B", "-c",
            CHILD % (LAB, self.locks, success, str(receipt))], capture_output=True, timeout=10)
        smoke.check_build_result(result, success)
        return result, json.loads(receipt.read_text())

    def probe(self):
        code = ("import sys; sys.path.insert(0,%r); from perflab import hostlock\n"
                "try: lock=hostlock.MachineLock(%r)\n"
                "except hostlock.Refused as error: print(error.reason)\n"
                "else: lock.release(); print('acquired')\n") % (LAB, self.locks)
        result = subprocess.run([sys.executable, "-I", "-B", "-c", code],
                                capture_output=True, timeout=10, check=True)
        return result.stdout.decode().strip()

    def maintain(self, receipt, fenced, cleanup):
        smoke.maintain_fixture(receipt, fenced, cleanup, directory=self.locks)

    def test_success_and_two_rejected_cli_children_settle_sequentially_on_same_inode(self):
        inodes = []
        for index, success in enumerate((True, False, False, True)):
            root = Path(self.path(str(index)))
            root.mkdir()
            result, receipt = self.child(root, success)
            self.assertEqual(result.returncode, 0 if success else 2)
            self.assertEqual(self.probe(), "acquired" if success else "coordination_unavailable")
            observed = []
            self.maintain(receipt, not success, lambda: observed.append(self.probe()))
            self.assertEqual(observed, ["concurrent_shard"])
            self.assertFalse(os.path.lexists(self.fence))
            self.assertEqual(self.probe(), "acquired")
            inodes.append(os.stat(self.lock).st_ino)
        self.assertEqual(len(set(inodes)), 1)

    def test_foreign_replacement_fence_is_retained_without_cleanup(self):
        _, receipt = self.child(Path(self.tmp))
        os.rename(self.fence, self.fence + ".original")
        os.mkdir(self.fence, 0o700)
        cleanup = mock.Mock()
        with self.assertRaisesRegex(AssertionError, "foreign fence"):
            self.maintain(receipt, True, cleanup)
        cleanup.assert_not_called()
        self.assertTrue(os.path.isdir(self.fence))
        self.assertEqual(self.probe(), "coordination_unavailable")

    def test_nonempty_fence_is_retained(self):
        _, receipt = self.child(Path(self.tmp))
        Path(self.fence, "unknown").write_text("uncertain")
        cleanup = mock.Mock()
        with self.assertRaises(AssertionError):
            self.maintain(receipt, True, cleanup)
        cleanup.assert_not_called()
        self.assertEqual(Path(self.fence, "unknown").read_text(), "uncertain")

    def test_foreign_owner_receipt_is_refused(self):
        _, receipt = self.child(Path(self.tmp))
        receipt["fence_uid"] += 1
        with self.assertRaisesRegex(AssertionError, "foreign fence owner"):
            self.maintain(receipt, True, lambda: self.fail("cleanup"))
        self.assertTrue(os.path.isdir(self.fence))

    def test_symlink_fence_is_not_followed(self):
        _, receipt = self.child(Path(self.tmp))
        os.rename(self.fence, self.fence + ".original")
        os.symlink(self.fence + ".original", self.fence)
        with self.assertRaises(OSError):
            self.maintain(receipt, True, lambda: self.fail("cleanup"))
        self.assertTrue(os.path.islink(self.fence))

    def test_changed_lock_inode_is_not_settled(self):
        _, receipt = self.child(Path(self.tmp))
        os.rename(self.lock, self.lock + ".original")
        Path(self.lock).touch(mode=0o644)
        with self.assertRaisesRegex(AssertionError, "machine lock changed"):
            self.maintain(receipt, True, lambda: self.fail("cleanup"))
        self.assertTrue(os.path.isdir(self.fence))

    def test_cleanup_failure_or_changed_fence_during_cleanup_retains_exclusion(self):
        _, receipt = self.child(Path(self.tmp))
        def incomplete():
            self.assertEqual(self.probe(), "concurrent_shard")
            raise OSError("unmount failed")
        with self.assertRaisesRegex(OSError, "unmount failed"):
            self.maintain(receipt, True, incomplete)
        self.assertEqual(self.probe(), "coordination_unavailable")
        def replaced():
            os.rename(self.fence, self.fence + ".original")
            os.mkdir(self.fence, 0o700)
        with self.assertRaisesRegex(AssertionError, "foreign fence"):
            self.maintain(receipt, True, replaced)
        self.assertTrue(os.path.isdir(self.fence))

    def test_maintenance_cannot_run_while_original_cli_lease_is_held(self):
        with hostlock.MachineLock(self.locks) as lease:
            lease.guard()
            path = self.path("guard.json")
            smoke.record_guard(lease, path)
            with open(path) as stream:
                receipt = json.load(stream)
            with self.assertRaises(BlockingIOError):
                self.maintain(receipt, True, lambda: self.fail("cleanup"))
            self.assertTrue(os.path.isdir(self.fence))

    def test_coordination_and_build_preflight_failures_cannot_pass_attack_assertions(self):
        for reason, status in (("coordination_unavailable", 4), ("concurrent_shard", 4),
                               ("invalid_original_plan", 2)):
            result = subprocess.CompletedProcess([], status, json.dumps({"reason": reason}).encode(), b"")
            with self.subTest(reason=reason), self.assertRaises(AssertionError):
                smoke.check_build_result(result, False)

    def test_compile_assertions_distinguish_preflight_and_missing_logs(self):
        mount = Path(self.path("scratch"))
        mount.mkdir()
        result = subprocess.CompletedProcess([], 2, json.dumps({
            "reason": "telemetry_build_failed", "detail": "scratch verification refused"}).encode(), b"")
        smoke.check_build_result(result, False)
        with self.assertRaises(AssertionError):
            smoke.check_compile_rejection(result, mount, "aggregate bound")
        result.stdout = b"compile failed; no binary admitted"
        with self.assertRaisesRegex(AssertionError, "compilation never started"):
            smoke.check_compile_rejection(result, mount, "aggregate bound")
        (mount / "logs").mkdir()
        (mount / "logs/compile.log").write_text("aggregate bound")
        smoke.check_compile_rejection(result, mount, "aggregate bound")

    def test_actual_child_checks_source_authority_before_importing_library(self):
        root = Path(self.path("control"))
        scripts = root / "scripts/performance"
        (scripts / "tests").mkdir(parents=True)
        (scripts / "perflab/__pycache__").mkdir(parents=True)
        shutil.copyfile(smoke.CLI, scripts / "cli.py")
        child = scripts / "tests/telemetry_build_smoke.py"
        shutil.copyfile(smoke.__file__, child)
        sentinel = root / "imported"
        (scripts / "perflab/__init__.py").write_text(
            "from pathlib import Path\nPath(%r).write_text('unsafe import')\n" % str(sentinel))
        result = subprocess.run([sys.executable, "-I", "-B", str(child), "--fixture-child",
            str(root / "receipt"), "telemetry-contained-build"], capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"measurement library bytecode cache refused", result.stderr)
        self.assertFalse(sentinel.exists())

    def scratch_run(self, live=b"", reaped=True, incomplete=False, changed_mount=False,
                    cleanup_lies=False):
        root = Path(self.path("fixture"))
        root.mkdir()
        def prepare(image, mount, size):
            with open(image, "xb") as stream:
                stream.truncate(size)
            os.mkdir(mount)
        def cleanup(image, mount):
            self.assertEqual(self.probe(), "concurrent_shard")
            if incomplete:
                return ["unmount failed"]
            if cleanup_lies:
                return []
            shutil.rmtree(mount)
            os.unlink(image)
            return []
        original = smoke.maintain_fixture
        def maintain(receipt, fenced, clean):
            return original(receipt, fenced, clean, directory=self.locks)
        def action(mount, fixture):
            result, _ = self.child(root)
            if reaped:
                fixture.child = result
            if changed_mount:
                mount.rename(root / "scratch.original")
                mount.mkdir()
        with mock.patch.object(smoke.hostprep, "prepare", side_effect=prepare), \
                mock.patch.object(smoke.hostprep, "cleanup", side_effect=cleanup) as clean, \
                mock.patch.object(smoke.subprocess, "check_output", return_value=live), \
                mock.patch.object(smoke, "maintain_fixture", side_effect=maintain):
            smoke.scratch(root, action)
        self.assertEqual(clean.call_count, 1)
        self.assertFalse((root / "scratch").exists())
        self.assertFalse((root / "scratch.img").exists())
        self.assertEqual(self.probe(), "acquired")

    def test_scratch_cleanup_after_reaped_rejection_settles_fence(self):
        self.scratch_run()

    def test_live_docker_descendant_retains_scratch_and_fence(self):
        with self.assertRaisesRegex(AssertionError, "container cleanup uncertain"):
            self.scratch_run(live=b"owned-container\n")
        self.assertTrue(Path(self.path("fixture/scratch.img")).exists())
        self.assertEqual(self.probe(), "coordination_unavailable")

    def test_missing_reaped_cli_proof_retains_scratch_and_fence(self):
        with self.assertRaisesRegex(AssertionError, "CLI cleanup uncertain"):
            self.scratch_run(reaped=False)
        self.assertTrue(Path(self.path("fixture/scratch.img")).exists())
        self.assertEqual(self.probe(), "coordination_unavailable")

    def test_incomplete_unmount_retains_scratch_and_fence(self):
        with self.assertRaisesRegex(AssertionError, "unmount failed"):
            self.scratch_run(incomplete=True)
        self.assertTrue(Path(self.path("fixture/scratch.img")).exists())
        self.assertEqual(self.probe(), "coordination_unavailable")

    def test_replaced_scratch_mount_retains_fence_without_cleanup(self):
        with self.assertRaisesRegex(AssertionError, "scratch mount changed"):
            self.scratch_run(changed_mount=True)
        self.assertTrue(Path(self.path("fixture/scratch.img")).exists())
        self.assertEqual(self.probe(), "coordination_unavailable")

    def test_cleanup_success_without_removal_cannot_settle_fence(self):
        with self.assertRaises(AssertionError):
            self.scratch_run(cleanup_lies=True)
        self.assertTrue(Path(self.path("fixture/scratch.img")).exists())
        self.assertEqual(self.probe(), "coordination_unavailable")

    def test_assertion_failure_retains_fixture_directory_for_inspection(self):
        with self.assertRaisesRegex(AssertionError, "attack failed"):
            with smoke.owned_root(self.tmp) as root:
                (root / "scratch").mkdir()
                raise AssertionError("attack failed")
        self.assertTrue((root / "scratch").is_dir())
