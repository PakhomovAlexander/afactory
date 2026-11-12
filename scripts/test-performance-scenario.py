#!/usr/bin/env python3
"""Hermetic functional tests of Scenario submission/evidence, never benchmarks or models."""
import copy
import errno
import json
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts/performance"))

from perflab import contracts, executor, host_lock, json_subset, research_evidence, scenario_adapter, scenario_source  # noqa: E402
from perflab.values import ContractError, canonical_digest, document_bytes, sha256_bytes  # noqa: E402
from tests.test_executor import ExecutorFixture, PRODUCER, receipt  # noqa: E402

D = "sha256:" + "1" * 64
BASE = "sha256:" + "2" * 64
HEAD = "sha256:" + "3" * 64


def datum(value):
    return dict(type="text", value=value)


class SourceCapture(unittest.TestCase):
    """Native process-boundary units; adapter tests below cover the common fence lifecycle."""
    def setUp(self):
        self.home = tempfile.TemporaryDirectory()
        self.addCleanup(self.home.cleanup)
        self.path = self.home.name
        self.binary = pathlib.Path(self.path) / "af"
        self.request = dict(baseline_snapshot_id=None, candidate_snapshot_id=HEAD)
        scenario_source.save_request(os.path.join(self.path, "source-request.json"), self.request)
        self.config = dict(af=str(self.binary), task_repo=self.path, task_state=self.path)

    def program(self, body):
        self.binary.write_text("#!/usr/bin/python3\n" + body + "\n")
        self.binary.chmod(0o700)
        self.config["af_digest"] = sha256_bytes(self.binary.read_bytes())

    def capture(self):
        return scenario_source.capture(self.config, self.path, canonical_digest(self.request))

    def matching_receipt(self):
        receipt = dict(schema="af/scenario-experiment-source@1",
            request_digest=canonical_digest(self.request), baseline_snapshot_id=BASE,
            candidate_snapshot_id=self.request["candidate_snapshot_id"],
            baseline_content_digest=BASE, candidate_content_digest=HEAD,
            baseline_origin_id=BASE, candidate_origin_id=HEAD, repository_id="hermetic-native-source",
            baseline_sha="2" * 40, candidate_sha="3" * 40,
            repository=os.path.join(self.path, "source/repository.git"))
        schema = json.loads((ROOT / "schemas/scenario-experiment-source-v1.json").read_text())
        scenario = json.loads((ROOT / "schemas/scenario-v5.json").read_text())
        # The existing subset validator accepts local refs: inline the shipped digest
        # definition without changing any receipt assertions or requiring a new dependency.
        schema["$defs"] = dict(digest=scenario["$defs"]["digest"])
        for node in schema["properties"].values():
            if "$ref" in node:
                self.assertEqual(node["$ref"], "urn:af:schema:scenario:5#/$defs/digest")
                node["$ref"] = "#/$defs/digest"
        return json_subset.Schema(schema, "scenario-experiment-source-v1.json").validate(receipt)

    def test_success_without_descendants_accepts_matching_receipt(self):
        receipt = self.matching_receipt()
        self.program("import json; print(json.dumps(%r))" % receipt)
        self.assertEqual(self.capture(), receipt)

    def successful_leader_with_child(self):
        group_file = os.path.join(self.path, "group")
        receipt = self.matching_receipt()
        self.program("""import json, os, time
with open(%r, 'w') as stream:
    stream.write(str(os.getpid()))
if os.fork() == 0:
    time.sleep(30)
    os._exit(0)
print(json.dumps(%r))
""" % (group_file, receipt))
        return group_file

    def test_successful_leader_with_descendant_is_not_clean(self):
        group_file = self.successful_leader_with_child()
        try:
            with self.assertRaisesRegex(ContractError, "process group remains") as caught:
                self.capture()
            self.assertNotIsInstance(caught.exception, scenario_source.CleanCaptureRefusal)
        finally:
            if os.path.exists(group_file):
                os.killpg(int(pathlib.Path(group_file).read_text()), signal.SIGKILL)

    def test_reaped_refusal_without_capture_is_clean(self):
        self.program("import sys; sys.exit(1)")
        with self.assertRaises(scenario_source.CleanCaptureRefusal):
            self.capture()

    def test_preflight_refusal_is_clean_only_without_existing_capture(self):
        self.program("raise AssertionError('must not execute')")
        self.config["af_digest"] = D
        with self.assertRaises(scenario_source.CleanCaptureRefusal):
            self.capture()
        os.mkdir(os.path.join(self.path, "source"))
        with self.assertRaises(ContractError) as caught:
            self.capture()
        self.assertNotIsInstance(caught.exception, scenario_source.CleanCaptureRefusal)

    def test_setup_failure_is_clean_only_without_capture(self):
        self.program("raise AssertionError('must not execute')")
        for resource, failures in (("TemporaryDirectory", [OSError("home unavailable")]),
                                   ("TemporaryFile", [OSError("output unavailable")]),
                                   ("TemporaryFile", [None, OSError("stderr unavailable")])):
            for existing in (False, True):
                with self.subTest(resource=resource, failures=len(failures), existing=existing):
                    source = os.path.join(self.path, "source")
                    if existing:
                        os.mkdir(source)
                    original = getattr(scenario_source.tempfile, resource)
                    calls = iter(failures)
                    def setup(*args, **kwargs):
                        failure = next(calls, None)
                        if failure is not None:
                            raise failure
                        return original(*args, **kwargs)
                    with mock.patch.object(scenario_source.tempfile, resource, side_effect=setup), \
                            mock.patch.object(scenario_source.subprocess, "Popen") as launch:
                        with self.assertRaises(OSError if existing else scenario_source.CleanCaptureRefusal):
                            self.capture()
                        launch.assert_not_called()
                    if existing:
                        os.rmdir(source)

    def test_launch_refusal_with_existing_capture_is_not_clean(self):
        self.program("raise AssertionError('must not execute')")
        self.binary.chmod(0o600)
        os.mkdir(os.path.join(self.path, "source"))
        with self.assertRaises(OSError):
            self.capture()

    def test_missing_interpreter_is_clean(self):
        self.program("raise AssertionError('must not execute')")
        self.binary.write_text("#!/nonexistent-af-test-interpreter\n")
        self.config["af_digest"] = sha256_bytes(self.binary.read_bytes())
        with self.assertRaises(scenario_source.CleanCaptureRefusal):
            self.capture()

    def test_interrupted_launch_is_not_clean(self):
        self.program("raise AssertionError('must not execute')")
        with mock.patch.object(scenario_source.subprocess, "Popen", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.capture()

    def test_ambiguous_oserror_launch_is_not_clean(self):
        self.program("raise AssertionError('must not execute')")
        for failure in (InterruptedError(), InterruptedError(errno.EINTR, "interrupted exec wait"),
                        OSError(errno.EIO, "unknown launch outcome"), OSError("unknown launch outcome")):
            with self.subTest(failure=repr(failure)), \
                    mock.patch.object(scenario_source.subprocess, "Popen", side_effect=failure):
                with self.assertRaises(OSError) as caught:
                    self.capture()
                self.assertIs(caught.exception, failure)

    def test_invalid_executable_is_clean(self):
        self.program("raise AssertionError('must not execute')")
        self.binary.write_text("invalid executable\n")
        self.config["af_digest"] = sha256_bytes(self.binary.read_bytes())
        with self.assertRaises(scenario_source.CleanCaptureRefusal):
            self.capture()

    def test_prelaunch_cleanup_failure_is_not_clean(self):
        self.program("raise AssertionError('must not execute')")
        with mock.patch.object(scenario_source.tempfile.TemporaryDirectory, "__exit__",
                               side_effect=OSError("cleanup unconfirmed")), \
                mock.patch.object(scenario_source.tempfile, "TemporaryFile", side_effect=OSError("setup refused")):
            with self.assertRaisesRegex(OSError, "cleanup unconfirmed"):
                self.capture()

    def test_signal_exit_and_failed_group_probe_are_not_clean(self):
        self.program("import os, signal; os.kill(os.getpid(), signal.SIGTERM)")
        with self.assertRaises(ContractError) as caught:
            self.capture()
        self.assertNotIsInstance(caught.exception, scenario_source.CleanCaptureRefusal)
        self.program("import sys; sys.exit(1)")
        with mock.patch.object(scenario_source.os, "killpg", side_effect=PermissionError("probe refused")):
            with self.assertRaises(PermissionError):
                self.capture()

    def test_torn_capture_refusal_is_not_clean(self):
        self.program("import os, sys; os.mkdir(sys.argv[sys.argv.index('--into')+1]); sys.exit(1)")
        with self.assertRaises(ContractError) as caught:
            self.capture()
        self.assertNotIsInstance(caught.exception, scenario_source.CleanCaptureRefusal)

    def test_timeout_is_not_clean(self):
        self.program("import time; time.sleep(30)")
        with mock.patch.object(scenario_source, "CAPTURE_SECONDS", 0.05):
            with self.assertRaises(ContractError) as caught:
                self.capture()
        self.assertNotIsInstance(caught.exception, scenario_source.CleanCaptureRefusal)

    def test_surviving_process_group_is_not_clean(self):
        self.program("import sys; sys.exit(1)")
        # A successful signal probe means a descendant remains even after its leader exits.
        with mock.patch.object(scenario_source.os, "killpg", return_value=None):
            with self.assertRaises(ContractError) as caught:
                self.capture()
        self.assertNotIsInstance(caught.exception, scenario_source.CleanCaptureRefusal)


class SourceCaptureFence(ExecutorFixture):
    """Run the real capture inside the Journal's real common exclusion."""
    def setUp(self):
        super().setUp()
        self.source = SourceCapture()
        self.source.setUp()
        self.addCleanup(self.source.doCleanups)
        lock_patch = mock.patch.object(executor.Journal, "lock_directory", self.locks)
        lock_patch.start()
        self.addCleanup(lock_patch.stop)

    def handoff(self):
        try:
            return self.source.capture(), None, False
        except scenario_source.CleanCaptureRefusal as error:
            return None, str(error), False

    def assert_clean_refusal_and_later_work(self, journal, inode):
        result, refusal = journal.exclusive("source capture", self.handoff)
        self.assertIsNone(result)
        self.assertIsNotNone(refusal)
        lock = os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME)
        self.assertFalse(os.path.lexists(lock + ".fence"))
        self.assertEqual(journal.exclusive("unrelated work", lambda: (True, None, False)), (True, None))
        self.assertEqual(os.stat(lock).st_ino, inode)

    def lock_inode(self):
        with host_lock.MachineLock(self.locks):
            return os.stat(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME)).st_ino

    def test_nonexecutable_pinned_file_settles_fence_and_allows_unrelated_work(self):
        self.source.program("raise AssertionError('must not execute')")
        self.source.binary.chmod(0o600)
        inode = self.lock_inode()
        with executor.Journal(self.c) as journal:
            self.assert_clean_refusal_and_later_work(journal, inode)

    def test_setup_failure_settles_fence_and_allows_unrelated_work(self):
        self.source.program("raise AssertionError('must not execute')")
        inode = self.lock_inode()
        with executor.Journal(self.c) as journal:
            with mock.patch.object(scenario_source.tempfile, "TemporaryFile", side_effect=OSError("setup refused")):
                self.assert_clean_refusal_and_later_work(journal, inode)

    def assert_uncertain_launch_retains_fence(self, failure):
        self.source.program("raise AssertionError('must not execute')")
        inode = self.lock_inode()
        with executor.Journal(self.c) as journal:
            with mock.patch.object(scenario_source.subprocess, "Popen", side_effect=failure):
                with self.assertRaises(type(failure)) as caught:
                    journal.exclusive("source capture", self.handoff)
                self.assertIs(caught.exception, failure)
            lock = os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME)
            self.assertTrue(os.path.isdir(lock + ".fence"))
            with self.assertRaises(ContractError):
                journal.exclusive("unrelated work", lambda: (True, None, False))
            self.assertEqual(os.stat(lock).st_ino, inode)

    def test_eintr_launch_retains_fence_and_blocks_unrelated_work(self):
        self.assert_uncertain_launch_retains_fence(InterruptedError(errno.EINTR, "interrupted exec wait"))

    def test_unknown_launch_failure_retains_fence_and_blocks_unrelated_work(self):
        self.assert_uncertain_launch_retains_fence(OSError(errno.EIO, "unknown launch outcome"))

    def test_keyboard_interrupt_launch_retains_fence_and_blocks_unrelated_work(self):
        self.assert_uncertain_launch_retains_fence(KeyboardInterrupt())

    def test_successful_leader_with_child_retains_fence_and_blocks_unrelated_work(self):
        group_file = self.source.successful_leader_with_child()
        inode = self.lock_inode()
        try:
            with executor.Journal(self.c) as journal:
                with self.assertRaisesRegex(ContractError, "process group remains"):
                    journal.exclusive("source capture", self.handoff)
                lock = os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME)
                self.assertTrue(os.path.isdir(lock + ".fence"))
                with self.assertRaises(ContractError):
                    journal.exclusive("unrelated work", lambda: (True, None, False))
                self.assertEqual(os.stat(lock).st_ino, inode)
        finally:
            if os.path.exists(group_file):
                os.killpg(int(pathlib.Path(group_file).read_text()), signal.SIGKILL)


class ScenarioLab(ExecutorFixture):
    def setUp(self):
        super().setUp()
        os.mkdir(self.c["root"], 0o700)
        self.spool = os.path.join(self.c["root"], "scenario-spool")
        self.evidence = os.path.join(self.c["root"], "scenario-evidence")
        os.mkdir(self.spool, 0o700)
        os.mkdir(self.evidence, 0o700)
        index = research_evidence.rebuild([])
        index_path = research_evidence.retain(self.evidence, index, "evidence-index")
        self.config = dict(schema="af.perf-scenario-adapter/1", root=self.spool,
            executor=self.path("executor.toml"), af=self.path("af"), af_digest=D,
            task_repo=self.repo.path, task_state=self.path("task-state"),
            destination="native:claw", max_intents=8, max_bytes=16 * 1024 ** 3,
            evidence_index=index_path, evidence_index_digest=canonical_digest(index),
            evidence_root=self.evidence, evidence_max_age_ms=86400000,
            evidence_max_items=32, evidence_max_bytes=24576)
        patch = mock.patch.object(executor, "load_config", return_value=self.c)
        patch.start()
        self.addCleanup(patch.stop)
        lock_patch = mock.patch.object(executor.Journal, "lock_directory", self.locks)
        lock_patch.start()
        self.addCleanup(lock_patch.stop)
        self.adapter = scenario_adapter.Adapter(self.config)
        self.experiment = dict(schema="af.perf-scenario-experiment/1", work="core",
            configuration_digest=canonical_digest(self.c), producer_digest=PRODUCER)
        self.request = dict(schema="af/scenario-adapter-request@4", method="execute", intent_id=D,
            input_digest=BASE, operation="experiment_submit_v1", destination="native:claw",
            inputs=dict(subject=datum(D), revision=datum(HEAD),
                        body=datum(document_bytes(self.experiment).decode()),
                        candidate=dict(type="task", value={"task_id": "fake-native-result-for-adapter-test",
                            "result_id": D, "result": {"outputs": {"snapshot": {"snapshot_id": HEAD}}}})))

    def capture(self, config, path, expected):
        # Only the native handoff boundary is replaced. Journal submission, preflight,
        # idempotency, locks and fences are real. The native CLI handoff has a Rust test.
        source = dict(schema="af/scenario-experiment-source@1", request_digest=expected,
            baseline_snapshot_id=BASE, candidate_snapshot_id=HEAD,
            repository=self.repo.path, baseline_sha=self.base, candidate_sha=self.head)
        os.makedirs(os.path.join(path, "source"), exist_ok=True)
        with open(os.path.join(path, "source/source.json"), "w") as stream:
            json.dump(source, stream)
        return source

    def submit_intent(self):
        self.assertEqual(self.adapter.request(self.request)["outcome"], "done")
        self.assertEqual(self.adapter.submit_pending(capture=self.capture), "submitted")
        return self.adapter.held(D)

    def test_pinned_launcher_refuses_source_changes_and_ignores_shadow_modules_and_bytecode(self):
        import importlib.util
        import py_compile
        import shutil
        installed = pathlib.Path(self.path("installed"))
        installed.mkdir()
        shutil.copy(ROOT / "scripts/performance/scenario.py", installed / "scenario.py")
        shutil.copytree(ROOT / "scripts/performance/perflab", installed / "perflab",
                        ignore=shutil.ignore_patterns("__pycache__"))
        # copytree retains sealed gate-source modes. Only this disposable installation
        # is writable during provisioning and cleanup; the launcher sees sealed sources.
        def writable_directories():
            for directory, _, _ in os.walk(installed / "perflab"):
                os.chmod(directory, 0o700)
        self.addCleanup(writable_directories)
        writable_directories()
        spec = importlib.util.spec_from_file_location("scenario_launcher_test", installed / "scenario.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        config = installed / "config.json"
        config.write_bytes(document_bytes(self.config))
        config.chmod(0o600)
        (installed / "scenario.py").chmod(0o444)
        output = installed / "launcher"
        module.launcher(str(config), str(output))
        (installed / "fractions.py").write_text("raise RuntimeError('SHADOW-MUST-NOT-RUN')")
        poison = installed / "poison.py"
        poison.write_text("raise RuntimeError('BYTECODE-MUST-NOT-RUN')")
        cached = importlib.util.cache_from_source(str(installed / "perflab/values.py"))
        pathlib.Path(cached).parent.mkdir(parents=True, exist_ok=True)
        py_compile.compile(str(poison), cfile=cached,
            invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
        for directory, _, files in os.walk(installed / "perflab"):
            for name in files:
                os.chmod(pathlib.Path(directory) / name, 0o444)
            os.chmod(directory, 0o555)
        self.assertFalse(os.access(installed / "perflab", os.W_OK))
        self.assertFalse(os.access(installed / "perflab/values.py", os.W_OK))
        result = subprocess.run([sys.executable, "-I", str(output)], input=b"{}",
                                capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(b"executor.toml", result.stderr)  # Reached native config, without either poison.
        self.assertNotIn(b"MUST-NOT-RUN", result.stderr)
        # Permission refusal stays independent of content pinning, including scenario.py.
        (installed / "scenario.py").chmod(0o664)
        unsafe = subprocess.run([sys.executable, "-I", str(output)], input=b"{}",
                                capture_output=True, timeout=20)
        self.assertNotEqual(unsafe.returncode, 0)
        self.assertIn(b"unsafe installed adapter closure", unsafe.stderr)
        (installed / "scenario.py").chmod(0o444)
        # Deliberately tamper with this copy after the sealed-launch assertions.
        os.chmod(installed / "perflab/values.py", 0o644)
        with (installed / "perflab/values.py").open("a") as changed:
            changed.write("\n# changed installed source\n")
        result = subprocess.run([sys.executable, "-I", str(output)], input=b"{}",
                                capture_output=True, timeout=20)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"installed adapter closure changed", result.stderr)

    def test_duplicate_and_lost_submission_responses_reconcile_exactly_one_original_after_restart(self):
        self.adapter.request(self.request)  # response lost after durable publication
        restarted = scenario_adapter.Adapter(self.config)
        reconciled = dict(self.request, method="reconcile")
        self.assertEqual(restarted.request(reconciled)["outcome"], "done")
        self.assertEqual(restarted.request(self.request)["outcome"], "done")
        # Lose the second response: native original exists, adapter receipt not yet saved.
        real_atomic = executor.atomic
        def lose(path, doc, name):
            if name == "scenario-receipt":
                raise OSError("simulated lost native submission response")
            return real_atomic(path, doc, name)
        with mock.patch.object(executor, "atomic", side_effect=lose):
            with self.assertRaisesRegex(OSError, "lost native"):
                restarted.submit_pending(capture=self.capture)
        self.assertEqual(restarted.submit_pending(capture=self.capture), "submitted")
        self.assertEqual(restarted.submit_pending(capture=self.capture), "idle")
        with executor.Journal(self.c) as journal:
            self.assertEqual(len(journal.names()), 1)
            calls = []
            def execute(config, request, path, lease, installed):
                lease.check()
                calls.append(request["request_id"])
                return receipt()
            self.assertEqual(journal.cycle(execute=execute, lock_directory=self.locks), "complete")
            self.assertEqual(journal.cycle(execute=execute, lock_directory=self.locks), "idle")
            self.assertEqual(len(calls), 1)

    def test_changed_source_input_digest_destination_and_old_protocol_refuse_without_second_original(self):
        self.submit_intent()
        for mutation in (dict(input_digest=HEAD), dict(destination="native:elsewhere"),
                         dict(schema="af/scenario-adapter-request@1")):
            with self.subTest(mutation=mutation), self.assertRaises(ContractError):
                self.adapter.request(dict(self.request, **mutation))
        changed = copy.deepcopy(self.request)
        changed["inputs"]["candidate"]["value"]["task_id"] = "different"
        with self.assertRaisesRegex(ContractError, "changed"):
            self.adapter.request(changed)
        with executor.Journal(self.c) as journal:
            self.assertEqual(len(journal.names()), 1)

    def test_common_machine_lock_contention_and_claim_crash_preserve_fence_and_never_relaunch(self):
        intent = self.submit_intent()
        with executor.Journal(self.c) as journal:
            ident = journal.names()[0]
            with host_lock.MachineLock(self.locks):
                self.assertEqual(journal.cycle(lock_directory=self.locks), "busy")
            self.assertEqual(journal.state(ident)["state"], "queued")
            with host_lock.MachineLock(self.locks) as held:
                held.guard()
            fence = os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME + ".fence")
            inode = os.stat(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME)).st_ino
            with self.assertRaises(host_lock.Refused):
                journal.cycle(lock_directory=self.locks)
            self.assertTrue(os.path.isdir(fence))
            journal.transition(journal.request(ident), "claimed")
            journal.recover()
            self.assertEqual(journal.state(ident)["state"], "uncertain")
            self.assertEqual(journal.cycle(lock_directory=self.locks), "idle")
            self.assertEqual(os.stat(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME)).st_ino, inode)
        self.assertEqual(self.adapter.observe(intent)[0], "pending")

    def test_definitive_producer_refusal_creates_no_native_original(self):
        self.experiment["producer_digest"] = HEAD
        self.request["inputs"]["body"] = datum(document_bytes(self.experiment).decode())
        self.adapter.request(self.request)
        self.assertEqual(self.adapter.submit_pending(capture=self.capture), "refused")
        self.assertEqual(self.adapter.observe(self.adapter.held(D))[0], "failed")
        with executor.Journal(self.c) as journal:
            self.assertEqual(journal.names(), [])

    def test_torn_publication_is_uncertain_and_never_absent(self):
        os.mkdir(self.adapter.path(D), 0o700)
        with self.assertRaises((ContractError, OSError)):
            self.adapter.request(dict(self.request, method="reconcile"))

    def test_unsupported_source_is_a_definitive_refusal_and_never_falls_back_to_main(self):
        request = copy.deepcopy(self.request)
        request["inputs"]["candidate"]["value"]["result"]["outputs"] = {}
        result = self.adapter.request(request)
        self.assertEqual(result["outcome"], "failed")
        self.assertIn("unsupported source", result["evidence"])
        self.assertFalse(os.path.exists(self.adapter.path(D)))

    def test_source_capture_uses_native_exclusion_and_keeps_its_fence_on_uncertainty(self):
        self.adapter.request(self.request)
        def uncertain(*args):
            with self.assertRaises(host_lock.Refused) as caught:
                host_lock.MachineLock(self.locks)
            self.assertEqual(caught.exception.reason, "concurrent_shard")
            raise ContractError("native capture uncertain")
        self.assertEqual(self.adapter.submit_pending(capture=uncertain), "uncertain")
        self.assertTrue(os.path.isdir(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME + ".fence")))
        with executor.Journal(self.c) as journal:
            self.assertEqual(journal.names(), [])

    def native_capture_program(self, body):
        binary = pathlib.Path(self.config["af"])
        binary.write_text("#!/usr/bin/python3\n" + body + "\n")
        binary.chmod(0o700)
        self.config["af_digest"] = sha256_bytes(binary.read_bytes())

    def test_clean_native_capture_refusal_settles_fence_and_later_work_submits(self):
        self.native_capture_program("import sys; sys.exit(1)")
        self.adapter.request(self.request)
        self.assertEqual(self.adapter.submit_pending(), "refused")
        self.assertFalse(os.path.lexists(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME + ".fence")))
        other = dict(self.request, intent_id=HEAD)
        self.assertEqual(self.adapter.request(other)["outcome"], "done")
        self.assertEqual(self.adapter.submit_pending(capture=self.capture), "submitted")
        self.assertEqual(self.adapter.submit_pending(capture=self.capture), "idle")

    def test_torn_native_capture_refusal_keeps_fence_and_uncertain_status(self):
        self.native_capture_program("import os, sys; os.mkdir(sys.argv[sys.argv.index('--into')+1]); sys.exit(1)")
        self.adapter.request(self.request)
        self.assertEqual(self.adapter.submit_pending(), "uncertain")
        self.assertTrue(os.path.isdir(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME + ".fence")))
        with self.assertRaises(host_lock.Refused):
            host_lock.MachineLock(self.locks)

    def test_native_capture_timeout_keeps_fence_even_after_leader_reaped(self):
        self.native_capture_program("import time; time.sleep(30)")
        self.adapter.request(self.request)
        with mock.patch.object(scenario_source, "CAPTURE_SECONDS", 0.05):
            self.assertEqual(self.adapter.submit_pending(), "uncertain")
        self.assertTrue(os.path.isdir(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME + ".fence")))

    def research_wait(self, records):
        index = research_evidence.rebuild(records)
        self.config["evidence_index"] = research_evidence.retain(self.evidence, index, "evidence-index")
        self.config["evidence_index_digest"] = canonical_digest(index)
        return self.adapter.request(dict(self.request, method="observe", operation="observe",
            inputs=dict(subject=datum(canonical_digest(index)), revision=datum(D),
                        condition=datum("bounded_evidence"))))

    def test_research_wait_refuses_fabricated_native_task_even_with_pinned_index(self):
        record = Evidence().artifact(1)
        response = self.research_wait([record])
        self.assertIsNone(response["observation"])
        self.assertNotEqual(response["outcome"], "done")

    def test_research_wait_accepts_offline_verified_original_and_refuses_raw_tampering(self):
        intent = self.submit_intent()
        with executor.Journal(self.c) as journal:
            native = journal.request(contracts.read_json(executor.read(os.path.join(
                self.adapter.path(intent["intent_id"]), "receipt.json")), "scenario-receipt")["request_id"])
            path, _ = self.complete_core(journal, native, [])
            records = research_evidence.native(journal, [native["request_id"]])
        self.assertEqual(records[0]["provenance"], "verified_native")
        self.assertEqual(self.research_wait(records)["outcome"], "done")
        with open(os.path.join(path, "run/shards/0/samples.jsonl"), "ab") as stream:
            stream.write(b"tampered")
        response = self.research_wait(records)
        self.assertNotEqual(response["outcome"], "done")
        self.assertIsNone(response["observation"])

    def test_research_wait_rederives_native_original_and_refuses_changed_facts(self):
        intent = self.submit_intent()
        with executor.Journal(self.c) as journal:
            records = research_evidence.native(journal, journal.names())
        response = self.research_wait(records)
        self.assertEqual(response["outcome"], "done")
        for field, value in (("summary", "invented"), ("outcome", "positive"),
                             ("source_digest", HEAD), ("source_id", D)):
            with self.subTest(field=field):
                response = self.research_wait([dict(records[0], **{field: value})])
                self.assertNotEqual(response["outcome"], "done")
                self.assertIsNone(response["observation"])
        # Completed claims must pass the offline original verifier, not merely journal shape.
        with executor.Journal(self.c) as journal:
            journal.cycle(execute=lambda *args: receipt(), lock_directory=self.locks)
        fabricated = dict(records[0], provenance="verified_native")
        self.assertNotEqual(self.research_wait([fabricated])["outcome"], "done")
        self.assertFalse(os.path.lexists(os.path.join(self.locks, host_lock.MACHINE_LOCK_NAME + ".fence")))
        self.assertEqual(self.adapter.held(intent["intent_id"]), intent)

    def test_fake_verified_matched_negative_report_completes_and_is_not_reverified_on_restart(self):
        intent = self.submit_intent()
        with executor.Journal(self.c) as journal:
            journal.cycle(execute=lambda *args: receipt(measurement="fail"), lock_directory=self.locks)
        def verified(journal, ident, evaluated):
            verdict = json.loads((ROOT / "fixtures/performance/compare/synthetic-regression/verdict.json").read_text())
            # A fake native verifier is the hermetic seam; this test measures nothing.
            verdict["provenance"] = "measured"
            verdict["run_id"] = "local_" + ident[4:]
            evaluated[ident] = SimpleNamespace(verdict=verdict)
            return journal.request(ident), journal.state(ident), dict(measurement_status="fail")
        self.adapter.verify_pending(verifier=verified)
        outcome, evidence = self.adapter.observe(intent)
        self.assertEqual(outcome, "done")
        completion = contracts.read_json(evidence.encode(), "scenario-completion")
        self.assertEqual(completion["verdict"]["aggregate"]["result"], "fail")
        self.assertEqual(completion["candidate_snapshot_id"], HEAD)
        restarted = scenario_adapter.Adapter(self.config)
        restarted.verify_pending(verifier=lambda *args, **kwargs: self.fail("old certificate reverified"))
        self.assertEqual(restarted.observe(intent), (outcome, evidence))

    def test_native_report_verification_refusal_cannot_complete_a_wait(self):
        intent = self.submit_intent()
        with executor.Journal(self.c) as journal:
            journal.cycle(execute=lambda *args: receipt(), lock_directory=self.locks)
        def refused(*args, **kwargs):
            raise ContractError("missing retained shard evidence")
        self.adapter.verify_pending(verifier=refused)
        outcome, reason = self.adapter.observe(intent)
        self.assertEqual(outcome, "failed")
        self.assertIn("missing retained shard", reason)

    def test_refused_first_verification_does_not_starve_later_completed_intent_after_restart(self):
        first = self.submit_intent()
        second_request = copy.deepcopy(self.request)
        second_request["intent_id"] = BASE
        self.adapter.request(second_request)
        self.assertEqual(self.adapter.submit_pending(capture=self.capture), "submitted")
        second = self.adapter.held(BASE)
        with executor.Journal(self.c) as journal:
            for _ in range(2):
                journal.cycle(execute=lambda *args: receipt(measurement="fail"), lock_directory=self.locks)
        calls = []
        def verified(journal, ident, evaluated):
            calls.append(ident)
            if len(calls) == 1:
                raise ContractError("first report refused")
            verdict = json.loads((ROOT / "fixtures/performance/compare/synthetic-regression/verdict.json").read_text())
            verdict["provenance"] = "measured"
            verdict["run_id"] = "local_" + ident[4:]
            evaluated[ident] = SimpleNamespace(verdict=verdict)
            return journal.request(ident), journal.state(ident), dict(measurement_status="fail")
        self.adapter.verify_pending(verifier=verified)
        self.assertEqual(self.adapter.observe(first)[0], "failed")
        restarted = scenario_adapter.Adapter(self.config)
        restarted.verify_pending(verifier=verified)
        self.assertEqual(restarted.observe(first)[0], "failed")
        self.assertEqual(restarted.observe(second)[0], "done")
        self.assertEqual(len(set(calls)), 2)
        restarted.verify_pending(verifier=lambda *a, **k: self.fail("terminal intent reverified"))
        status = pathlib.Path(self.adapter.path(D)) / "status.json"
        changed = json.loads(status.read_text())
        changed["intent_digest"] = HEAD
        status.write_bytes(document_bytes(changed))
        with self.assertRaisesRegex(ContractError, "stale verification status"):
            restarted.verify_pending(verifier=verified)

    def test_oversized_native_completion_is_durably_refused_and_does_not_starve_later_report(self):
        first = self.submit_intent()
        second_request = copy.deepcopy(self.request)
        second_request["intent_id"] = BASE
        self.adapter.request(second_request)
        self.adapter.submit_pending(capture=self.capture)
        with executor.Journal(self.c) as journal:
            for _ in range(2):
                journal.cycle(execute=lambda *args: receipt(measurement="fail"), lock_directory=self.locks)
        calls = []
        def verified(journal, ident, evaluated):
            calls.append(ident)
            verdict = json.loads((ROOT / "fixtures/performance/compare/synthetic-regression/verdict.json").read_text())
            verdict["provenance"] = "measured"
            verdict["run_id"] = "local_" + ident[4:]
            if len(calls) == 1:
                verdict["cases"] *= 100  # Fake verifier's result exceeds the unchanged evidence bound.
            evaluated[ident] = SimpleNamespace(verdict=verdict)
            return journal.request(ident), journal.state(ident), dict(measurement_status="fail")
        self.adapter.verify_pending(verifier=verified)
        outcome, detail = self.adapter.observe(first)
        self.assertEqual(outcome, "failed")
        self.assertIn("exceeds bounded Scenario evidence", detail)
        restarted = scenario_adapter.Adapter(self.config)
        restarted.verify_pending(verifier=verified)
        self.assertEqual(restarted.observe(restarted.held(BASE))[0], "done")
        self.assertEqual(len(set(calls)), 2)

    def test_completion_needs_verified_exact_report_not_exit_or_html_and_rejects_stale_evidence(self):
        intent = self.submit_intent()
        self.assertEqual(self.adapter.observe(intent)[0], "pending")
        with executor.Journal(self.c) as journal:
            ident = journal.names()[0]
            journal.cycle(execute=lambda *args: receipt(measurement="incomplete"), lock_directory=self.locks)
            with open(os.path.join(journal.path(ident), "report.html"), "w") as stream:
                stream.write("not evidence")
        self.assertEqual(self.adapter.observe(intent)[0], "pending")
        def verified(journal, ident, evaluated):
            # Fake native adapter result; does not make a measurement claim in this test.
            verdict = json.loads((ROOT / "fixtures/performance/compare/synthetic-inconclusive/verdict.json").read_text())
            verdict["run_id"] = "local_" + ident[4:]
            evaluated[ident] = SimpleNamespace(verdict=verdict)
            return journal.request(ident), journal.state(ident), dict(measurement_status="incomplete")
        self.adapter.verify_pending(verifier=verified)
        self.assertEqual(self.adapter.observe(intent)[0], "failed")
        path = os.path.join(self.adapter.path(D), "verification.json")
        certificate = contracts.read_json(executor.read(path), "scenario-verification")
        certificate["result_sha256"] = D
        # Simulate external corruption; the validated writer itself refuses this mismatch.
        with open(path, "wb") as corrupted:
            corrupted.write(document_bytes(certificate))
        with self.assertRaisesRegex(ContractError, "digest changed"):
            self.adapter.observe(intent)

    def test_bounded_research_wait_is_token_free_and_pins_packet_into_typed_observation(self):
        request = dict(self.request, method="observe", operation="observe",
            inputs=dict(subject=datum(self.config["evidence_index_digest"]), revision=datum(HEAD),
                        condition=datum("bounded_evidence")))
        with mock.patch.object(executor.Journal, "submit", side_effect=AssertionError("no native experiment")):
            response = self.adapter.request(request)
        packet = json.loads(response["observation"]["evidence"])
        self.assertTrue(response["observation"]["satisfied"])
        self.assertEqual(packet["items"], [])
        self.assertTrue(packet["telemetry_is_diagnostic"])
        self.assertLessEqual(len(document_bytes(packet)), self.config["evidence_max_bytes"])


class Evidence(unittest.TestCase):
    def test_native_reader_over_installed_byte_bound_is_refused_before_hash_or_execution(self):
        # Sparse, private fixture: exercise the real reader's size check without
        # allocating/reading 256 MiB or relying on this host's debug binary size.
        with tempfile.TemporaryDirectory() as root:
            binary = pathlib.Path(root).resolve() / "af"
            with binary.open("xb") as stream:
                stream.truncate(256 * 1024 * 1024 + 1)
            binary.chmod(0o500)
            selection = dict(task_id="prior", artifact_id=D, observed_unix_ms=30, compatibility=D)
            with mock.patch.object(research_evidence, "sha256_bytes") as digest, \
                    mock.patch.object(research_evidence.subprocess, "run") as run:
                with self.assertRaisesRegex(ContractError, "executor receipt exceeds byte bound"):
                    research_evidence.resolve(dict(af=str(binary), af_digest=D), selection)
                digest.assert_not_called()
                run.assert_not_called()

    def artifact(self, number, **changes):
        return dict(dict(kind="finding", source_id=sha256_bytes(str(number).encode()), source_digest=D,
            provenance="native_task", native_receipt=dict(task_id="prior", revision_id=BASE, artifact_id=D),
            freshness_basis="capture_observed",
            observed_unix_ms=number, compatibility=D, compatibility_basis="owner_selection", outcome="negative", summary="retained finding"), **changes)

    def test_rebuild_and_bounded_packets_retain_negative_adopted_stale_and_incompatible_evidence(self):
        records = [self.artifact(1), self.artifact(2, outcome="adopted", compatibility=BASE),
                   self.artifact(3, outcome="inconclusive"), self.artifact(100)]
        index = research_evidence.rebuild(records)
        self.assertEqual(index, research_evidence.rebuild(list(reversed(records))))
        packet = research_evidence.packet(index, 3, 10, D, 8, max_items=3, max_bytes=4096)
        self.assertEqual([r["artifact"]["outcome"] for r in packet["items"]], ["inconclusive", "adopted", "negative"])
        self.assertFalse(packet["items"][1]["compatible"])
        self.assertFalse(packet["items"][2]["fresh"])
        self.assertEqual(packet["omissions"], [dict(reason="future", count=1)])
        bounded = research_evidence.packet(index, 100, 100, D, 1, max_items=1, max_bytes=1024)
        self.assertLessEqual(len(document_bytes(bounded)), 1024)
        self.assertTrue(bounded["omissions"])

    def selected(self, doc):
        selection = dict(task_id="prior", artifact_id=D, observed_unix_ms=30, compatibility=D)
        kind = {"af.task-review-findings/1": "af/TaskReviewFindings@1",
                "af.optimization-history/1": "af/OptimizationHistory@1"}[doc["schema"]]
        source = dict(schema="af/scenario-evidence-source@1", task_id="prior", revision_id=BASE,
            artifact=dict(artifact_id=D, content_id=canonical_digest(doc), type=kind, payload=doc))
        return selection, source

    def test_native_resolved_findings_keep_distinct_substance_provenance_and_bounds(self):
        doc = dict(schema="af.task-review-findings/1", missing_reviewers=[], findings=[
            dict(id="bugs-1", by="bugs", title="Avoid repeated decoding", body="Cache the parsed manifest per exact digest.", fix="Reuse decoded manifest."),
            dict(id="correctness-1", by="correctness", title="Keep fence on interrupted capture", body="Interrupted capture must preserve the common lock fence.", fix="Retain uncertainty.")])
        selection, source = self.selected(doc)
        records = research_evidence.captured([selection], {}, resolver=lambda *args: source)
        index = research_evidence.rebuild(records)
        packet = research_evidence.packet(index, 30, 30, D, 20, max_items=2, max_bytes=4096)
        encoded = document_bytes(packet)
        self.assertIn(b"Cache the parsed manifest", encoded)
        self.assertIn(b"preserve the common lock fence", encoded)
        for item in packet["items"]:
            self.assertEqual(item["artifact"]["native_receipt"]["artifact_id"], D)
            self.assertEqual(item["artifact"]["source_digest"], source["artifact"]["content_id"])
        bounded = research_evidence.packet(index, 30, 30, D, 20, max_items=1, max_bytes=1024)
        self.assertLessEqual(len(document_bytes(bounded)), 1024)
        self.assertLessEqual(len(bounded["items"]), 1)
        self.assertEqual(len(bounded["items"]) + sum(v["count"] for v in bounded["omissions"]), 2)
        with self.assertRaisesRegex(ContractError, "conflicting"):
            research_evidence.rebuild(records + [dict(records[0], outcome="positive")])
        changed = copy.deepcopy(source)
        changed["artifact"]["artifact_id"] = HEAD
        with self.assertRaisesRegex(ContractError, "receipt changed"):
            research_evidence.captured([selection], {}, resolver=lambda *a: changed)

    def test_hashed_fabricated_findings_and_adoption_files_are_never_authoritative_sources(self):
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            for schema in ("af.task-review-findings/1", "af.optimization-adoption-observation/1"):
                path = pathlib.Path(root) / "fabricated.json"
                data = document_bytes(dict(schema=schema, findings=[], causal_claim=False))
                path.write_bytes(data)
                with self.assertRaisesRegex(ContractError, "Task and artifact IDs"):
                    research_evidence.captured([[str(path), sha256_bytes(data), 1, D]], {})

    def test_history_capture_keeps_telemetry_kind_and_span_maxima_distinct_from_total_time(self):
        doc = dict(schema="af.optimization-history/1", cutoff_unix_ms=20, receipts=[],
            observations=[dict(spans=[dict(kind="active", status="exact", start_unix_ms="1", end_unix_ms="6"),
                dict(kind="active", status="exact", start_unix_ms="2", end_unix_ms="9"),
                dict(kind="tool", status="unknown")])])
        selection, source = self.selected(doc)
        records = research_evidence.captured([selection], {}, resolver=lambda *a: source)
        packet = research_evidence.packet(research_evidence.rebuild(records), 30, 30, D, 20)
        artifact = packet["items"][0]["artifact"]
        self.assertEqual(artifact["kind"], "telemetry")
        self.assertEqual(artifact["observed_unix_ms"], 20)
        self.assertIn("active=7", artifact["summary"])
        self.assertIn("1 non-exact spans excluded", artifact["summary"])
        self.assertNotIn("active=12", artifact["summary"])

    def test_prior_evidence_shapes_fail_closed_without_compatibility_reader(self):
        index = research_evidence.rebuild([self.artifact(1)])
        packet = research_evidence.packet(index, 10, 10, D, 1, max_bytes=4096)
        for kind, doc in (("evidence-index", index), ("evidence-packet", packet)):
            previous = dict(doc, schema=doc["schema"].replace("/2", "/1"))
            with self.assertRaises(ContractError):
                contracts.read_json(document_bytes(previous), kind)

    def test_packet_reader_refuses_false_freshness_compatibility_duplicates_cutoff_and_missing_native_receipts(self):
        index = research_evidence.rebuild([self.artifact(1)])
        packet = research_evidence.packet(index, 10, 10, D, 1, max_bytes=4096)
        for field in ("fresh", "compatible"):
            changed = copy.deepcopy(packet)
            changed["items"][0][field] = not changed["items"][0][field]
            with self.assertRaisesRegex(ContractError, "stale"):
                contracts.write_bytes(changed, "evidence-packet")
        changed = copy.deepcopy(packet)
        changed["items"].append(changed["items"][0])
        with self.assertRaisesRegex(ContractError, "duplicate"):
            contracts.write_bytes(changed, "evidence-packet")
        changed = dict(packet, cutoff_unix_ms=11)
        with self.assertRaisesRegex(ContractError, "cutoff"):
            contracts.write_bytes(changed, "evidence-packet")
        changed = copy.deepcopy(packet)
        del changed["items"][0]["artifact"]["native_receipt"]
        with self.assertRaisesRegex(ContractError, "source receipt"):
            contracts.read_json(document_bytes(changed), "evidence-packet")

    def test_inert_authoring_tool_uses_native_receipts_and_never_overwrites_or_activates(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            bundle = json.loads((ROOT / "fixtures/scenarios/performance-candidate.json").read_text())
            stages = bundle["definitions"]["performance"]["steps"]
            pins = {s["id"]: dict(schema="af/scenario-template-pin@2", template=s["operation"]["template"],
                template_id=canonical_digest(s["operation"]["template"])) for s in stages if s["operation"]["kind"] == "fresh_task"}
            for name, doc in (("pins.json", pins), ("launcher.json", dict(schema="af.perf-scenario-launcher/1",
                    executable="/opt/af-perf/scenario-adapter", digest=D)),
                    ("admission.json", dict(roadmap="performance-native", identity=D, revision=HEAD))):
                (root / name).write_text(json.dumps(doc))
            argv = [sys.executable, str(ROOT / "fixtures/scenarios/configure-performance.py"),
                "--bundle", str(ROOT / "fixtures/scenarios/performance-candidate.json"),
                "--pins", str(root / "pins.json"), "--launcher", str(root / "launcher.json"),
                "--admission", str(root / "admission.json"), "--configuration", D,
                "--producer", D, "--evidence-index", D, "--compatibility", D, "--output", str(root / "authored")]
            result = subprocess.run(argv, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            authored = json.loads((root / "authored/bundle.json").read_text())
            self.assertEqual(authored["definitions"]["performance"]["admission"]["identity"], D)
            policy = json.loads((root / "authored/policy.json").read_text())
            self.assertEqual(policy["adapters"]["lab"]["operations"], ["experiment_submit_v1", "observe"])
            before = (root / "authored/ant.json").read_bytes()
            self.assertNotEqual(subprocess.run(argv, capture_output=True, timeout=10).returncode, 0)
            self.assertEqual((root / "authored/ant.json").read_bytes(), before)
            self.assertEqual(policy["performance_publications"], {})
            activation = json.loads((ROOT / "fixtures/scenarios/performance-publication.example.json").read_text())
            path = root / "publication.json"
            path.write_text(json.dumps(activation))
            opted = argv[:-1] + [str(root / "with-publication"), "--publication", str(path)]
            result = subprocess.run(opted, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            authored = json.loads((root / "with-publication/bundle.json").read_text())
            publisher = authored["definitions"]["performance"]["steps"][-1]
            self.assertEqual(publisher["operation"]["grant"]["operation"], "publish_performance_pr_v1")
            self.assertFalse(publisher["operation"]["requires_decision"])
            self.assertEqual(set(publisher["inputs"]), {"subject", "revision", "body", "candidate", "verification", "report", "measurement"})
            opted_policy = json.loads((root / "with-publication/policy.json").read_text())
            self.assertEqual(opted_policy["performance_publications"]["performance-publication"], activation["scope"])
            self.assertEqual(opted_policy["decision_keys"], {})
            ant = json.loads((root / "with-publication/ant.json").read_text())
            self.assertEqual(ant["scenario"], authored)
            refused = opted.copy()
            refused[refused.index("--bundle") + 1] = str(ROOT / "fixtures/scenarios/performance-no-worthwhile.json")
            refused[refused.index("--output") + 1] = str(root / "no-worthwhile")
            result = subprocess.run(refused, capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "no-worthwhile").exists())



if __name__ == "__main__":
    unittest.main()
