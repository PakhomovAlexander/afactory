#!/usr/bin/env python3
"""Hermetic functional tests of Scenario submission/evidence, never benchmarks or models."""
import copy
import json
import os
import pathlib
import subprocess
import sys
import unittest
from types import SimpleNamespace
from unittest import mock

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts/performance"))

from perflab import contracts, executor, hostlock, researchevidence, scenarioadapter  # noqa: E402
from perflab.values import ContractError, canonical_digest, document_bytes, sha256_bytes  # noqa: E402
from tests.test_executor import ExecutorFixture, PRODUCER, receipt  # noqa: E402

D = "sha256:" + "1" * 64
BASE = "sha256:" + "2" * 64
HEAD = "sha256:" + "3" * 64


def datum(value):
    return dict(type="text", value=value)


class ScenarioLab(ExecutorFixture):
    def setUp(self):
        super().setUp()
        os.mkdir(self.c["root"], 0o700)
        self.spool = os.path.join(self.c["root"], "scenario-spool")
        self.evidence = os.path.join(self.c["root"], "scenario-evidence")
        os.mkdir(self.spool, 0o700)
        os.mkdir(self.evidence, 0o700)
        index = researchevidence.rebuild([])
        index_path = researchevidence.retain(self.evidence, index, "evidence-index")
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
        self.adapter = scenarioadapter.Adapter(self.config)
        self.experiment = dict(schema="af.perf-scenario-experiment/1", work="core",
            configuration_digest=canonical_digest(self.c), producer_digest=PRODUCER)
        self.request = dict(schema="af/scenario-adapter-request@2", method="execute", intent_id=D,
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
        pathlib.Path(cached).parent.mkdir(exist_ok=True)
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
        restarted = scenarioadapter.Adapter(self.config)
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
            with hostlock.MachineLock(self.locks):
                self.assertEqual(journal.cycle(lock_directory=self.locks), "busy")
            self.assertEqual(journal.state(ident)["state"], "queued")
            with hostlock.MachineLock(self.locks) as held:
                held.guard()
            fence = os.path.join(self.locks, hostlock.MACHINE_LOCK_NAME + ".fence")
            inode = os.stat(os.path.join(self.locks, hostlock.MACHINE_LOCK_NAME)).st_ino
            with self.assertRaises(hostlock.Refused):
                journal.cycle(lock_directory=self.locks)
            self.assertTrue(os.path.isdir(fence))
            journal.transition(journal.request(ident), "claimed")
            journal.recover()
            self.assertEqual(journal.state(ident)["state"], "uncertain")
            self.assertEqual(journal.cycle(lock_directory=self.locks), "idle")
            self.assertEqual(os.stat(os.path.join(self.locks, hostlock.MACHINE_LOCK_NAME)).st_ino, inode)
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
            with self.assertRaises(hostlock.Refused) as caught:
                hostlock.MachineLock(self.locks)
            self.assertEqual(caught.exception.reason, "concurrent_shard")
            raise ContractError("native capture uncertain")
        self.assertEqual(self.adapter.submit_pending(capture=uncertain), "refused")
        self.assertTrue(os.path.isdir(os.path.join(self.locks, hostlock.MACHINE_LOCK_NAME + ".fence")))
        with executor.Journal(self.c) as journal:
            self.assertEqual(journal.names(), [])

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
        restarted = scenarioadapter.Adapter(self.config)
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
        restarted = scenarioadapter.Adapter(self.config)
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
        restarted = scenarioadapter.Adapter(self.config)
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
    def artifact(self, number, **changes):
        return dict(dict(kind="finding", source_id=sha256_bytes(str(number).encode()), source_digest=D,
            provenance="native_task", native_receipt=dict(task_id="prior", revision_id=BASE, artifact_id=D),
            freshness_basis="capture_observed",
            observed_unix_ms=number, compatibility=D, compatibility_basis="owner_selection", outcome="negative", summary="retained finding"), **changes)

    def test_rebuild_and_bounded_packets_retain_negative_adopted_stale_and_incompatible_evidence(self):
        records = [self.artifact(1), self.artifact(2, outcome="adopted", compatibility=BASE),
                   self.artifact(3, outcome="inconclusive"), self.artifact(100)]
        index = researchevidence.rebuild(records)
        self.assertEqual(index, researchevidence.rebuild(list(reversed(records))))
        packet = researchevidence.packet(index, 3, 10, D, 8, max_items=3, max_bytes=4096)
        self.assertEqual([r["artifact"]["outcome"] for r in packet["items"]], ["inconclusive", "adopted", "negative"])
        self.assertFalse(packet["items"][1]["compatible"])
        self.assertFalse(packet["items"][2]["fresh"])
        self.assertEqual(packet["omissions"], [dict(reason="future", count=1)])
        bounded = researchevidence.packet(index, 100, 100, D, 1, max_items=1, max_bytes=1024)
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
        records = researchevidence.captured([selection], {}, resolver=lambda *args: source)
        index = researchevidence.rebuild(records)
        packet = researchevidence.packet(index, 30, 30, D, 20, max_items=2, max_bytes=4096)
        encoded = document_bytes(packet)
        self.assertIn(b"Cache the parsed manifest", encoded)
        self.assertIn(b"preserve the common lock fence", encoded)
        for item in packet["items"]:
            self.assertEqual(item["artifact"]["native_receipt"]["artifact_id"], D)
            self.assertEqual(item["artifact"]["source_digest"], source["artifact"]["content_id"])
        bounded = researchevidence.packet(index, 30, 30, D, 20, max_items=1, max_bytes=1024)
        self.assertLessEqual(len(document_bytes(bounded)), 1024)
        self.assertLessEqual(len(bounded["items"]), 1)
        self.assertEqual(len(bounded["items"]) + sum(v["count"] for v in bounded["omissions"]), 2)
        with self.assertRaisesRegex(ContractError, "conflicting"):
            researchevidence.rebuild(records + [dict(records[0], outcome="positive")])
        changed = copy.deepcopy(source)
        changed["artifact"]["artifact_id"] = HEAD
        with self.assertRaisesRegex(ContractError, "receipt changed"):
            researchevidence.captured([selection], {}, resolver=lambda *a: changed)

    def test_hashed_fabricated_findings_and_adoption_files_are_never_authoritative_sources(self):
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            for schema in ("af.task-review-findings/1", "af.optimization-adoption-observation/1"):
                path = pathlib.Path(root) / "fabricated.json"
                data = document_bytes(dict(schema=schema, findings=[], causal_claim=False))
                path.write_bytes(data)
                with self.assertRaisesRegex(ContractError, "Task and artifact IDs"):
                    researchevidence.captured([[str(path), sha256_bytes(data), 1, D]], {})

    def test_history_capture_keeps_telemetry_kind_and_span_maxima_distinct_from_total_time(self):
        doc = dict(schema="af.optimization-history/1", cutoff_unix_ms=20, receipts=[],
            observations=[dict(spans=[dict(kind="active", status="exact", start_unix_ms="1", end_unix_ms="6"),
                dict(kind="active", status="exact", start_unix_ms="2", end_unix_ms="9"),
                dict(kind="tool", status="unknown")])])
        selection, source = self.selected(doc)
        records = researchevidence.captured([selection], {}, resolver=lambda *a: source)
        packet = researchevidence.packet(researchevidence.rebuild(records), 30, 30, D, 20)
        artifact = packet["items"][0]["artifact"]
        self.assertEqual(artifact["kind"], "telemetry")
        self.assertEqual(artifact["observed_unix_ms"], 20)
        self.assertIn("active=7", artifact["summary"])
        self.assertIn("1 non-exact spans excluded", artifact["summary"])
        self.assertNotIn("active=12", artifact["summary"])

    def test_prior_evidence_shapes_fail_closed_without_compatibility_reader(self):
        index = researchevidence.rebuild([self.artifact(1)])
        packet = researchevidence.packet(index, 10, 10, D, 1, max_bytes=4096)
        for kind, doc in (("evidence-index", index), ("evidence-packet", packet)):
            previous = dict(doc, schema=doc["schema"].replace("/2", "/1"))
            with self.assertRaises(ContractError):
                contracts.read_json(document_bytes(previous), kind)

    def test_packet_reader_refuses_false_freshness_compatibility_duplicates_cutoff_and_missing_native_receipts(self):
        index = researchevidence.rebuild([self.artifact(1)])
        packet = researchevidence.packet(index, 10, 10, D, 1, max_bytes=4096)
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
            pins = {s["id"]: dict(schema="af/scenario-template-pin@1", template=s["operation"]["template"],
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


if __name__ == "__main__":
    unittest.main()
