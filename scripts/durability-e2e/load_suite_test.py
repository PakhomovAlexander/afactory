"""Offline coverage for the manual load controller and worker evidence boundary."""
from argparse import Namespace
import errno
from collections import Counter
import json
from pathlib import Path
import signal
import sys
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

from control import Control, HTTPError, save
from load_cases import CASES, MARKER, load_profiles
from load_suite import (BINDINGS_SCHEMA, SUITE, compare_runs, max_overlap, normalized, run,
                        schedule)
from load_worker import execute, judge, run_case, witness

RUN = "af-load-" + "a" * 20
WORKERS = ["r-101", "r-102"]


class FakeControl(Control):
    def __init__(self, unrelated=False):
        super().__init__(None)
        self.unrelated = unrelated
        self.jobs = {}
        self.counts = Counter()
        self.barriers = {worker: threading.Barrier(2) for worker in WORKERS}
        self.lock = threading.Lock()

    def request(self, path, body=None):
        if path == "/v1/resources":
            return [dict(id=worker, profile="af", state="allocated", power="running")
                    for worker in WORKERS]
        worker = path.split("/")[3]
        if body is None:
            if self.unrelated and worker == WORKERS[1]:
                return [{"state": "active", "substate": "running", "name": "unrelated"}]
            return []
        address = path + "/" + body["name"]
        with self.lock:
            self.jobs[address] = body["argv"]
        return {"status": address, "logs": address + "/logs"}

    def wait_job(self, status_path, evidence, seconds):
        worker = status_path.split("/")[3]
        job = status_path.rsplit("/", 1)[-1]
        argv = self.jobs[status_path]
        if job.endswith("-probe"):
            binding = json.loads(argv[argv.index("preflight") + 1])
            event = {"event": "preflight", "boot_id": worker, "namespace": None,
                     "environment": {"cpus": 4}, "pins": {k: binding[k] for k in (
                         "binary_sha256", "source_commit", "source_workload", "suite_workload")}}
        else:
            with self.lock:
                self.counts[worker] += 1
                index = self.counts[worker]
            if index <= 2:
                # Both calls must be independently waiting on the same worker.
                self.barriers[worker].wait(timeout=4)
            case = argv[argv.index("--case") + 1]
            if case == "restart-green":
                event = {"event": "error", "phase": "trial", "reason": "original worker error"}
            else:
                start = index if index <= 2 else 6
                event = {"event": "trial", "verdict": "pass", "phase": "verified", "reason": "native pass",
                         "fault_witness": "receipt.json" if case != "clean" else None,
                         "interval_ns": [start, start + 4], "metrics": {"cpu_s": 0.2}}
        ok = event["event"] != "error"
        status = {"properties": {"ExecMainStatus": str(int(not ok))}}
        save(evidence / "status.json", status)
        save(evidence / "logs.json", {"output": MARKER + json.dumps({"job": job, **event})})
        return ok


class LoadSuite(unittest.TestCase):
    def setUp(self):
        work = tempfile.TemporaryDirectory()
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)

    def case_json(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        save(path, data)

    def run_args(self):
        path = self.root / "workers.json"
        entries = [{"worker": worker, "binary": "/opt/af", "source": "/srv/source",
                    "suite": "/srv/suite", "scratch": "/scratch/durability",
                    "binary_sha256": "a" * 64, "source_commit": "b" * 40,
                    "source_workload": "c" * 64} for worker in WORKERS]
        save(path, {"schema": BINDINGS_SCHEMA, "workers": entries})
        return Namespace(manifest=SUITE / "manifests/load-cases.json", profile="short", workers=path,
                         repeat=1, concurrency=2, seed=0, run_id=RUN, evidence=self.root / "cohort")

    def test_balanced_schedule_uses_every_worker_and_preserves_manifest_order(self):
        workers = list("abcde")
        ordered = ["clean", "gate-red"]
        rows = schedule(RUN, ordered, workers, 3, 0)
        self.assertEqual([row["case"] for row in rows], ordered * 3)
        self.assertEqual(sorted(Counter(row["worker"] for row in rows).values()), [1, 1, 1, 1, 2])
        self.assertEqual(rows, schedule(RUN, ordered, workers, 3, 0))
        self.assertEqual(len({row["job"] for row in rows}), 6)
        profiles = load_profiles(SUITE / "manifests/load-cases.json")
        self.assertEqual(set(profiles["full"] + profiles["optional-fixtures"]), CASES.keys())

    def test_overlap_counts_genuine_overlaps_and_not_adjacent_intervals(self):
        self.assertEqual(max_overlap([(10, 20), (11, 21), (14, 16), (21, 25), (22, 22)]), 3)
        self.assertEqual(max_overlap([(10, 20), (20, 30)]), 1)
        self.assertEqual(max_overlap([]), 0)

    def test_cohort_records_overlap_worker_error_and_genuine_missing_metrics(self):
        args = self.run_args()
        control = FakeControl()
        self.assertEqual(run(args, control), 1)
        report = json.loads((args.evidence / "report.json").read_text())
        self.assertEqual(len(list((args.evidence / "trials").glob("*.json"))), 5)
        self.assertEqual(report["workers"]["r-101"]["observed_max_overlap"], 2)
        self.assertEqual(report["aggregate"]["spent_case_cpu_s"]["measured"], 4)
        self.assertTrue(all(row["measurements"]["terminal_s"] is None for row in report["trials"]))
        self.assertEqual(len(control.jobs), 7)
        invalid, = (row for row in report["trials"] if row["status"] == "invalid/harness")
        self.assertEqual(invalid["reason"], "original worker error")
        self.assertIsNone(invalid["measurements"]["cpu_s"])
        self.assertIn("original worker error", (args.evidence / "report.md").read_text())

    def test_preflight_active_job_blocks_every_probe_and_writes_all_trials(self):
        args = self.run_args()
        control = FakeControl(unrelated=True)
        self.assertEqual(run(args, control), 1)
        report = json.loads((args.evidence / "report.json").read_text())
        self.assertEqual(report["aggregate"]["outcomes"], {"invalid/harness": 5})
        self.assertIsNone(report["aggregate"]["spent_case_cpu_s"]["sum"])
        self.assertIn("unrelated", report["preflight_error"])
        self.assertFalse(control.jobs)

    def test_503_reconciles_by_read_and_preserves_receipt_without_second_post(self):
        control = Control(None)
        prefix = "/v1/resources/r-101/jobs"
        control.request = Mock(side_effect=[HTTPError(prefix, 503, "ambiguous"), {"pending": True}])
        evidence = self.root / "broker"
        self.assertEqual(control.submit(WORKERS[0], "job", ["native-case"], evidence), prefix + "/job")
        self.assertEqual(len(control.request.call_args_list), 2)
        control.request.assert_called_with(prefix + "/job")
        self.assertEqual(json.loads((evidence / "reconciled-status.json").read_text()), {"pending": True})

    def test_expected_red_passes_with_receipt_but_running_step_is_engine_failure(self):
        self.case_json("summary.json", {"scenario_status": "failed", "attempts": 1,
                                        "task_state": {"outcome": "gate_failed"}})
        self.case_json("task-checks.json", {"receipt": {"checks": [{"exit_code": 1}]}})
        self.assertEqual(judge("gate-red", self.root, 0, False, "")["verdict"], "pass")
        self.case_json("observed.json", {"status": "failed", "frames": {"root": {"steps": [
            {"step": "implement", "status": "running"}]}}})
        row = judge("gate-red", self.root, 0, False, "")
        self.assertEqual(row["verdict"], "fail")
        self.assertIn("terminal Scenario has running step implement", row["violations"])

    def test_witnessed_replay_failure_and_premature_worker_exit_are_distinct(self):
        self.case_json("cas-fault.json", {"errno": errno.ENOSPC})
        self.assertIsNone(witness("artifact-enospc", self.root))
        diagnostic = "cas io: No space left on device (os error 28)"
        intent_id, execution_id = "sha256:" + "a" * 64, "sha256:" + "b" * 64
        self.case_json("at-full.json", {"status": "failed", "failure": diagnostic,
            "execution_id": execution_id, "frames": {"root": {"steps": [
                {"step": "second", "status": "failed", "intents": [{"id": intent_id}],
                 "deadline_unix_ms": 2000}]}}})
        self.case_json("cas-fault.json", {"errno": errno.ENOSPC, "pressure_errno": errno.ENOSPC,
            "native_diagnostic": diagnostic, "step": "root/second", "intent_id": intent_id,
            "execution_id": execution_id, "deadline_unix_ms": 2000,
            "task_id": "scenario-" + "a" * 64})
        self.case_json("artifact-summary.json", {"scenario": "failed", "evidence": {
            "same_scenario_recovers": False, "prior_task_unchanged": True}})
        failure = judge("artifact-enospc", self.root, 1, False, "native case failed")
        self.assertEqual(failure["verdict"], "fail")
        self.assertIn("same_scenario_recovers", failure["violations"])
        (self.root / "cas-fault.json").unlink()
        error = judge("artifact-enospc", self.root, 1, False, "existing diagnostic")
        self.assertEqual(error["verdict"], "invalid")
        self.assertEqual(error["phase"], "before-fault-witness")
        self.assertEqual(error["reason"], "existing diagnostic")

    def test_checks_recovery_requires_ordered_receipts_for_the_persisted_check(self):
        check_id, revision, intent = "a" * 64, "sha256:" + "b" * 64, "sha256:" + "c" * 64
        stale = "sha256:" + "0" * 64
        crash = {"scheduler_pid": 10, "exit": -signal.SIGKILL, "fixture_pid": 11,
                 "fixture_alive": True}

        def call(sequence, action, status, served_revision):
            return {"sequence": sequence, "action": action, "check_id": check_id, "intent": intent,
                    "status": status, "served_revision": served_revision}

        for case_id, fault, create_status, read_status, served in (
                ("restart-green", "lost-reply", 0, 503, None),
                ("restart-red", "lost-reply", 0, 503, None),
                ("expired-auth", "expired-auth", 201, 401, None),
                ("wrong-revision", "wrong-revision", 201, 200, stale)):
            self.case_json("recovery-summary.json", {
                "fault": fault, "scenario": "failed" if case_id == "restart-red" else "succeeded",
                "terminal_steps_settled": True,
                "requirements": {"same_execution": True, "same_check_intent": True,
                                 "exact_candidate_check": True, "correct_outcome": True},
            })
            persisted = [{"id": check_id, "revision": revision}]
            create = call(2, "create", create_status, revision if create_status == 201 else None)
            read_fault = call(3, "read", read_status, served)
            valid = {"checks": persisted, "calls": [call(1, "read", 404, None), create, read_fault]}
            self.case_json("crash.json", crash)
            self.case_json("ledger-before.json", valid)
            result = judge(case_id, self.root, 0, False, "")
            self.assertEqual(result["verdict"], "pass", case_id)
            self.assertEqual(result["fault_witness"], "ledger-before.json + crash.json")

            wrong_response = revision if case_id == "wrong-revision" else stale
            invalid = {
                "missing": None,
                "no saved check": {**valid, "checks": []},
                "unrelated check": {**valid, "calls": [create, {**read_fault, "check_id": "d" * 64}]},
                "unrelated intent": {**valid, "calls": [create, {**read_fault, "intent": "sha256:" + "d" * 64}]},
                "wrong response revision": {**valid, "calls": [create, {**read_fault, "served_revision": wrong_response}]},
                "wrong create status": {**valid, "calls": [call(2, "create", 200, revision), read_fault]},
                "malformed call after witness": {**valid, "calls": [create, read_fault, None]},
                "reversed export order": {**valid, "calls": [read_fault, create]},
                "read before create": {**valid, "calls": [{**read_fault, "sequence": 1}, create]},
            }
            if case_id == "wrong-revision":
                invalid["saved revision equals stale response"] = {
                    "checks": [{"id": check_id, "revision": stale}],
                    "calls": [{**create, "served_revision": stale}, read_fault],
                }
            for reason, ledger in invalid.items():
                with self.subTest(case=case_id, reason=reason):
                    if ledger is None:
                        (self.root / "ledger-before.json").unlink()
                    else:
                        self.case_json("ledger-before.json", ledger)
                    result = judge(case_id, self.root, 0, False, "")
                    self.assertEqual(result["verdict"], "invalid")
                    self.assertEqual(result["phase"], "before-fault-witness")
                    self.assertIsNone(result["fault_witness"])

            self.case_json("ledger-before.json", valid)
            for field, value in (("exit", -signal.SIGTERM), ("exit", float(-signal.SIGKILL)),
                                 ("fixture_alive", False)):
                self.case_json("crash.json", {**crash, field: value})
                self.assertIsNone(witness(case_id, self.root))

    def test_worker_interval_is_retained_without_measuring_terminal_time(self):
        args = Namespace(run=RUN, job="job", case="clean")
        root = self.root / RUN
        self.case_json(f"{RUN}/preflight.json", {"boot_id": "boot",
                                                  "scratch_device": self.root.stat().st_dev,
                                                  "scratch_inode": self.root.stat().st_ino})
        interval = [1_000_000_000, 9_000_000_000]

        def execute_case(argv, output, env, job_dir, seconds):
            save(job_dir / "summary.json", {"scenario_status": "succeeded", "task_state": {}, "attempts": 1})
            return 0, False, "", interval

        binding = {"binary": "/opt/af", "source": "/srv/source", "binary_sha256": "a" * 64,
                   "suite_workload": "b" * 64, "source_workload": "c" * 64}
        with (patch("load_worker.verify", return_value=self.root),
              patch("load_worker.canonical", return_value=root),
              patch.object(Path, "read_text", return_value="boot\n"),
              patch("load_worker.available"), patch("load_worker.check_capacity"),
              patch("load_worker.adopt_children"), patch("load_worker.emit"),
              patch("load_worker.execute", side_effect=execute_case)):
            result = run_case(binding, args)
        self.assertEqual(result["verdict"], "pass")
        self.assertEqual(result["interval_ns"], interval)
        self.assertIn("cpu_s", result["metrics"])
        row = {"case": args.case, "job": args.job, "worker": WORKERS[0], "seed": 0}
        record = normalized(root, row, result, "short", binding, {})
        self.assertNotIn("terminal_s", record["metrics"])

    def test_terminal_budget_refusal_is_a_policy_pass_with_separate_token_counts(self):
        state = {"attempts": 1, "chargeable_tokens": 4097, "budget": 20480, "unknown_usage": []}
        self.case_json("budget-summary.json", {"requirements": {"durable_budget_outcome": True},
                                              "exhausted": state, "funded": state,
                                              "terminal_steps_settled": True})
        self.case_json("provider-evidence.json", {"calls": [{"id": 1, "operation": "exhausted-admission"}],
                                                 "events": [{"call": 1, "kind": "charged", "detail": {
                                                     "input_tokens": 4097, "output_tokens": 0}}]})
        result = judge("budget", self.root, 0, False, "")
        self.assertEqual(result["verdict"], "pass")
        self.assertEqual(result["metrics"], {"simulated_tokens": 4097, "provider_tokens": 0,
                                            "unknown_settlements": 0})

    def test_optional_provider_fault_requires_the_authors_response_to_be_written(self):
        ledger = {"calls": [{"id": 8, "operation": "exhausted-author"}], "events": []}
        self.case_json("provider-evidence.json", ledger)
        self.assertIsNone(witness("malformed-usage", self.root))
        ledger["events"].append({"call": 8, "kind": "response_written", "detail": {}})
        self.case_json("provider-evidence.json", ledger)
        self.assertEqual(witness("malformed-usage", self.root), "provider-evidence.json")

    def test_child_cancel_replay_defect_has_an_engine_failure_verdict(self):
        self.case_json("outage.json", {"stopped_signal": 19})
        self.case_json("cancel.json", {"request": {"task_id": "child"}})
        self.case_json("cancel-summary.json", {"invariants": {"cancelled_child_not_redispatched": False}})
        result = judge("child-cancel", self.root, 1, False, "")
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["violations"], ["cancelled_child_not_redispatched"])

    def test_subprocess_error_keeps_original_diagnostic_and_real_interval(self):
        output = self.root / "worker.log"
        code, timeout, diagnostic, interval = execute(
            [sys.executable, "-B", "-c", "raise RuntimeError('fixture setup broke')"],
            output, {}, self.root, 2,
        )
        self.assertNotEqual(code, 0)
        self.assertFalse(timeout)
        self.assertLess(interval[0], interval[1])
        self.assertIn("RuntimeError: fixture setup broke", diagnostic)
        self.assertIn("RuntimeError: fixture setup broke", output.read_text())

    def test_comparison_pairs_across_builds_and_reports_mismatches_absence_and_regressions(self):
        before, after = (self.root / side for side in ("before", "after"))
        for directory in (before, after):
            (directory / "trials").mkdir(parents=True)
        binding = {"binary_sha256": "a" * 64, "suite_workload": "b" * 64, "source_workload": "c" * 64}
        placement = {"worker_environment": {}, "concurrency": 2}

        def add(directory, seed, verdict, *, config=placement):
            row = {"case": "clean", "seed": seed, "worker": WORKERS[0], "job": "compare"}
            result = {"verdict": verdict, "reason": verdict, "metrics": {}}
            engine = binding if directory == before else {**binding, "binary_sha256": "z" * 64}
            record = normalized(directory, row, result, "short", engine, config)
            save(directory / "trials" / f"{seed}.json", record)

        add(before, 0, "fail")
        add(after, 0, "pass")
        add(before, 1, "pass")
        add(after, 1, "fail")
        add(before, 2, "pass")
        add(after, 2, "pass", config={**placement, "concurrency": 1})
        add(before, 3, "pass")
        add(before, 4, "invalid", config={})
        add(after, 4, "pass")
        result = compare_runs(before, after)
        self.assertEqual(result["transitions"], {"fail-to-pass": 1, "regression": 1,
                                                "different_inputs": 1, "unpaired": 1,
                                                "unavailable_inputs": 1})
        self.assertEqual(result["pairs"][0]["missing_metrics"]["cpu_s"], ["before", "after"])


if __name__ == "__main__":
    unittest.main()
