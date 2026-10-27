"""Executable admission and configuration regressions for the hosted build boundary."""
import os
import time
from unittest import mock

from perflab import budget, telemetrybuild
from perflab.values import ContractError
from tests.support import TempDir, read


class BuildBounds(TempDir):
    def setUp(self):
        super().setUp()
        self.state = self.path("state")
        os.mkdir(self.state)
        for name in telemetrybuild.STATE_NAMES:
            os.mkdir(os.path.join(self.state, name))
        os.mkdir(os.path.join(self.state, "target/release"))
        self.binary = os.path.join(self.state, "target/release/af")
        self.output = self.path("output")
        os.mkdir(self.output)
        self.timing = budget.Budget(time.monotonic() + 30, time.monotonic, 0)
        self.sentinel = self.path("sentinel")
        with open(self.sentinel, "wb") as stream:
            stream.write(b"operator sentinel")

    def test_regular_binary_is_copied_and_hashed_then_mutation_refuses(self):
        with open(self.binary, "wb") as stream:
            stream.write(b"real admitted bytes")
        digest = telemetrybuild.admit_binary(self.state, self.output, self.timing)
        self.assertEqual(read(os.path.join(self.output, "af")), b"real admitted bytes")
        self.assertTrue(digest.startswith("sha256:"))
        os.unlink(os.path.join(self.output, "af"))
        original = telemetrybuild.admission.copy_fd
        def changed(*args, **kwargs):
            result = original(*args, **kwargs)
            with open(self.binary, "wb") as stream:
                stream.write(b"changed same length")
            return result
        with mock.patch.object(telemetrybuild.admission, "copy_fd", changed):
            with self.assertRaisesRegex(ContractError, "mutated"):
                telemetrybuild.admit_binary(self.state, self.output, self.timing)

    def test_metadata_and_binary_and_ancestor_links_and_logical_oversize_refuse(self):
        os.symlink(self.sentinel, os.path.join(self.state, "af-rustc"))
        with self.assertRaisesRegex(ContractError, "foreign state"):
            telemetrybuild.admit_binary(self.state, self.output, self.timing)
        os.unlink(os.path.join(self.state, "af-rustc"))
        os.symlink(self.sentinel, self.binary)
        with self.assertRaisesRegex(ContractError, "symlink"):
            telemetrybuild.admit_binary(self.state, self.output, self.timing)
        os.unlink(self.binary)
        release = os.path.dirname(self.binary)
        os.rename(release, release + "-real")
        os.symlink(release + "-real", release)
        with self.assertRaisesRegex(ContractError, "symlink"):
            telemetrybuild.admit_binary(self.state, self.output, self.timing)
        os.unlink(release)
        os.rename(release + "-real", release)
        with open(self.binary, "wb") as stream:
            stream.truncate(telemetrybuild.BINARY_BYTES + 1)
        with self.assertRaisesRegex(ContractError, "binary byte bound"):
            telemetrybuild.admit_binary(self.state, self.output, self.timing)
        self.assertEqual(read(self.sentinel), b"operator sentinel")
        self.assertEqual(os.listdir(self.output), [])

    def test_configured_commands_and_credential_providers_refused_before_fetch(self):
        os.mkdir(self.path(".cargo"))
        for value in ('[build]\nrustc-wrapper="/payload"\n',
                      '[registry]\nglobal-credential-providers=["/payload"]\n',
                      '[alias]\nfetch="!/payload"\n'):
            with open(self.path(".cargo/config.toml"), "w") as stream:
                stream.write(value)
            with self.assertRaisesRegex(ContractError, "authority"):
                telemetrybuild.check_configs(self.tmp)

    def controller(self, available=True, metadata_attack=False):
        from types import SimpleNamespace
        from tests.support import ROOT
        source, scratch, output = self.path("source"), self.path("scratch"), self.path("published")
        os.mkdir(source)
        os.mkdir(scratch)
        with open(os.path.join(source, "rust-toolchain.toml"), "wb") as stream:
            stream.write(read(os.path.join(ROOT, "rust-toolchain.toml")))
        calls = []
        def prepare(source, target, sha, timing):
            calls.append("provenance")
            os.mkdir(target)
        def stage(image, tool, source, state, logs, git, phase, timing):
            calls.append(phase)
            if phase == "compile":
                os.makedirs(os.path.join(state, "target/release"))
                with open(os.path.join(state, "target/release/af"), "wb") as stream:
                    stream.write(b"fixture binary")
                if metadata_attack:
                    os.symlink(self.sentinel, os.path.join(state, "af-rustc"))
        tool = SimpleNamespace(rustc_identity="rustc 1.88.0 fixture", cargo_identity="cargo 1.88.0 fixture")
        checked = {"capability": {"status": "available" if available else "unavailable",
                                   "reasons": [] if available else ["aggregate bound unavailable"]}}
        with mock.patch.object(telemetrybuild.capability, "scratch_capability", return_value=checked), \
                mock.patch.object(telemetrybuild, "provenance", prepare), \
                mock.patch.object(telemetrybuild, "stage", stage), \
                mock.patch.object(telemetrybuild.builds, "BuildImage") as image, \
                mock.patch.object(telemetrybuild.builds, "ContainerToolchain", return_value=tool):
            if available and not metadata_attack:
                telemetrybuild.run(source, output, "unused fixture image", scratch, "a" * 40, 60)
                self.assertEqual(read(os.path.join(output, "af")), b"fixture binary")
                self.assertEqual(read(os.path.join(output, "af-rustc")), b"rustc 1.88.0 fixture\n")
                self.assertEqual(os.listdir(os.path.join(scratch, "sample")), [])
                self.assertEqual(calls, ["provenance", "fetch", "compile"])
            else:
                with self.assertRaisesRegex(ContractError, "foreign state" if available else "aggregate bound"):
                    telemetrybuild.run(source, output, "unused fixture image", scratch, "a" * 40, 60)
                self.assertFalse(os.path.lexists(output))
                self.assertEqual(read(self.sentinel), b"operator sentinel")
                if not available:
                    image.assert_not_called()
                    self.assertEqual(calls, [])

    def test_controller_requires_verified_aggregate_bound_before_any_execution(self):
        self.controller(available=False)

    def test_controller_publishes_only_after_admission_and_inventory_restoration(self):
        self.controller()

    def test_controller_metadata_attack_never_writes_host_sentinel_or_publishes(self):
        self.controller(metadata_attack=True)

    def test_implicit_writable_image_volumes_are_refused(self):
        from types import SimpleNamespace
        image = telemetrybuild.builds.BuildImage.__new__(telemetrybuild.builds.BuildImage)
        image.docker, image.reference, image.budget = "docker", "fixture", self.timing
        for declaration, accepted in ((b"null", True), (b"{}", True), (b'{"/escape":{}}', False)):
            with mock.patch.object(telemetrybuild.builds, "_run", return_value=SimpleNamespace(stdout=declaration)):
                if accepted:
                    image.refuse_implicit_volumes()
                else:
                    with self.assertRaisesRegex(ContractError, "implicit writable"):
                        image.refuse_implicit_volumes()

    def test_shared_step_preallocates_and_caps_logs_and_settles_timer_interruption(self):
        # The hosted build uses Linux posix_fallocate. On macOS exercise the same
        # allocation-before-launch boundary with real dense writes, never sparse truncate
        # or a success-only mock. Production preallocation remains mandatory.
        native_allocate = getattr(os, "posix_fallocate", None)
        allocations = []
        def allocate(fd, offset, count):
            if native_allocate is not None:
                native_allocate(fd, offset, count)
            else:
                written = 0
                while written < count:
                    written += os.pwrite(fd, b"x" * min(65536, count - written), offset + written)
            info = os.fstat(fd)
            self.assertEqual(info.st_size, offset + count)
            self.assertGreaterEqual(info.st_blocks * 512, count)
            allocations.append(count)
        patch = mock.patch.object(os, "posix_fallocate", allocate, create=True)
        patch.start()
        self.addCleanup(patch.stop)
        import sys
        from types import SimpleNamespace
        logs = self.path("logs")
        os.mkdir(logs)
        layout = SimpleNamespace(logs=logs, recorded=lambda value: value)
        step_budget = SimpleNamespace(run_deadline=time.monotonic() + 20, deadline_seconds=20,
            log_bytes=4 << 20, preallocate_logs=True, exceeded=lambda: None)
        steps = telemetrybuild.builds.Steps(layout, step_budget)
        result = steps.run("bounded", [sys.executable, "-B", "-c",
            "import os; [os.write(1,b'x'*65536) for _ in range(96)]"], self.tmp,
            {"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"})
        self.assertEqual(result["status"], "succeeded")
        self.assertEqual(result["log"]["bytes"], 4 << 20)
        self.assertEqual(result["log"]["truncated_bytes"], 2 << 20)
        process = telemetrybuild.subprocess.Popen
        children = []
        def started(*args, **kwargs):
            child = process(*args, **kwargs)
            children.append(child)
            return child
        timer = budget.Budget(time.monotonic() + .2, time.monotonic, 0)
        with mock.patch.object(telemetrybuild.subprocess, "Popen", started):
            with self.assertRaises(budget.Interrupted), timer.enforce("fixture build cancellation"):
                steps.run("interrupted", [sys.executable, "-B", "-c", "import time; time.sleep(30)"],
                          self.tmp, {"PYTHONDONTWRITEBYTECODE": "1"})
        self.assertEqual(allocations, [4 << 20, 4 << 20])
        self.assertEqual(len(children), 1)
        self.assertIsNotNone(children[0].returncode, "build client survived the total deadline")

    def test_failed_log_preallocation_refuses_before_launch(self):
        from types import SimpleNamespace
        logs = self.path("logs")
        os.mkdir(logs)
        layout = SimpleNamespace(logs=logs, recorded=lambda value: value)
        step_budget = SimpleNamespace(run_deadline=time.monotonic() + 20, deadline_seconds=20,
            log_bytes=4 << 20, preallocate_logs=True, exceeded=lambda: None)
        steps = telemetrybuild.builds.Steps(layout, step_budget)
        with mock.patch.object(os, "posix_fallocate", side_effect=OSError("allocation refused"),
                               create=True) as allocate, \
                mock.patch.object(telemetrybuild.subprocess, "Popen") as launch:
            with self.assertRaisesRegex(OSError, "allocation refused"):
                steps.run("refused", ["unused"], self.tmp, {})
        self.assertEqual(allocate.call_args.args[1:], (0, 4 << 20))
        launch.assert_not_called()
