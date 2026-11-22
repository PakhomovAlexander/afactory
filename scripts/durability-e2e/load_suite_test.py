"""Offline coverage for the manual load controller and worker evidence boundary."""
from argparse import Namespace
from copy import deepcopy
import errno
from collections import Counter
import json
import io
import runpy
import signal
from pathlib import Path
import sys
import shutil
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

from control import Control, HTTPError, save
from load_cases import CASES, MARKER, SUITE_FILES, files_identity, load_profiles
from recovery_evidence import SAFE, native_hash
from load_identity import ANCESTRY, CGROUP_FIELDS, CPU_FIELDS, complete_identity, compute_identity
from load_suite import (BINDINGS_SCHEMA, SUITE, compare_runs, max_overlap, normalized, run,
                        schedule)
from load_worker import before_check, execute, judge, main as worker_main, run_case, witness

# Reuse the repository's dependency-free JSON Schema evaluator for wire parity.
sys.path.insert(0, str(SUITE.parent / "performance"))
from perflab.json_subset import Schema  # noqa: E402

RUN = "af-load-" + "a" * 20
WORKERS = ["r-101", "r-102"]


def native_identity(root):
    proc, cgroup = root / "proc", root / "cgroup"
    (proc / "self/ns").mkdir(parents=True, exist_ok=True)
    namespace = proc / "self/ns/cgroup"
    namespace.unlink(missing_ok=True)
    namespace.symlink_to("cgroup:[4026531835]")
    (cgroup / "job").mkdir(parents=True, exist_ok=True)
    (proc / "self/cgroup").write_text("0::/job\n")
    (cgroup / "cgroup.controllers").write_text("cpu cpuset memory\n")
    (cgroup / "cgroup.subtree_control").write_text("cpu cpuset memory\n")
    (proc / "self/mountinfo").write_text(f"1 0 0:1 / {cgroup} rw - cgroup2 cgroup rw\n")
    (proc / "cpuinfo").write_text("processor: 0\nvendor_id: GenuineIntel\ncpu family: 6\n"
                                   "model: 85\nmodel name: Fixture CPU\nstepping: 1\n"
                                   "physical id: 0\ncore id: 0\nsiblings: 1\ncpu cores: 1\nflags: sse\n")
    (proc / "meminfo").write_text("MemTotal: 4194304 kB\nMemAvailable: 2000000 kB\n")
    for name, value in {"cpu.max": "max 100000", "cpu.weight": "100",
                        "cpuset.cpus.effective": "0", "cpuset.mems.effective": "0",
                        "memory.max": "max", "memory.high": "max", "memory.swap.max": "max"}.items():
        (cgroup / "job" / name).write_text(value)
        if name.startswith("cpuset"):
            (cgroup / name).write_text(value)
    return compute_identity(proc, cgroup, {0})


def rebind_capture(before, ledger):
    """Recompute every dependent identity, including the independent ledger link."""
    run = next(iter(before["runs"].values()))
    pin = run["pin"]
    definition = pin["bundle"]["definitions"]["clean"]
    pin["definitions"] = {"clean": native_hash(definition)}
    pin["revision"] = native_hash([pin["bundle"], pin["policy"]])
    implementation, checks = run["frames"]["root"]["steps"]
    candidate = implementation["outputs"]["result"]
    revision = candidate["value"]["result"]["outputs"]["snapshot"]["snapshot_id"]
    inputs = [{}, {"body": {"type": "text", "value": "Fixture CI"}, "candidate": candidate,
                   "revision": {"type": "text", "value": revision},
                   "subject": {"type": "text", "value": "pagination"}}]
    for index, progress in enumerate((implementation, checks)):
        intent = progress["intents"][0]
        intent["id"] = native_hash([run["execution_id"], pin["revision"],
                                   "root/" + progress["step"], 1])
        intent["input_digest"] = native_hash([definition["steps"][index], inputs[index]])
    run["waiting"]["intent_id"] = checks["intents"][0]["id"]
    for call in ledger["calls"]:
        call["intent"] = run["waiting"]["intent_id"]


class FakeControl(Control):
    def __init__(self, unrelated=False, identity=None):
        super().__init__(None)
        self.unrelated = unrelated
        self.identity = identity
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
            event = {"event": "preflight", "schema": "af-durability/load-preflight@2", "boot_id": worker, "namespace": None,
                     "environment": {"cpus": 4, "compute_identity": self.identity}, "pins": {k: binding[k] for k in (
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
                         "compute_identity": self.identity, "interval_ns": [start, start + 4], "metrics": {"cpu_s": 0.2}}
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
        self.identity = native_identity(self.root)

    def case_json(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        save(path, data)

    def run_fake(self, args, control):
        # Some sandbox hosts mark the entire temporary root with a synthetic .git.
        # Hide only that host marker for this offline cohort fixture, preserving
        # the production path guard and every evidence/fixture file operation.
        exists = Path.exists
        marker = Path(tempfile.gettempdir()) / ".git"
        with patch.object(Path, "exists", autospec=True,
                          side_effect=lambda path: False if path == marker else exists(path)):
            return run(args, control)

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
        control = FakeControl(identity=self.identity)
        self.assertEqual(self.run_fake(args, control), 1)
        report = json.loads((args.evidence / "report.json").read_text())
        self.assertEqual(len(list((args.evidence / "trials").glob("*.json"))), 5)
        self.assertEqual(report["workers"]["r-101"]["observed_max_overlap"], 2)
        self.assertEqual(report["aggregate"]["spent_case_cpu_s"]["measured"], 4)
        self.assertEqual(len(control.jobs), 7)
        self.assertEqual(report["schema"], "af-durability/load-report@2")
        for path in (args.evidence / "trials").glob("*.json"):
            record = json.loads(path.read_text())
            if record["verdict"] == "pass":
                self.assertIs(record["workload_evidence_complete"], CASES[record["case"]].script != "budget")
        invalid, = (row for row in report["trials"] if row["status"] == "invalid/harness")
        self.assertEqual(invalid["reason"], "original worker error")
        self.assertIsNone(invalid["measurements"]["cpu_s"])
        self.assertIn("original worker error", (args.evidence / "report.md").read_text())

    def test_preflight_active_job_blocks_every_probe_and_writes_all_trials(self):
        args = self.run_args()
        control = FakeControl(unrelated=True)
        self.assertEqual(self.run_fake(args, control), 1)
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
        before = json.loads((SUITE / "fixtures/recovery-before-native.json").read_text())["restart-green"]["before"]
        self.assertEqual(before_check(before)[1],
                         "sha256:3042cadee2f99825847aff18e8ff59c42b87ad8d3978f7b29c79c93644ee053e")
        self.assert_ordered_check_receipts(before)

    def test_v12_checks_recovery_requires_ordered_receipts_for_the_persisted_check(self):
        captures = json.loads((SUITE / "fixtures/recovery-before-native-v12.json").read_text())
        for case_id, capture in captures.items():
            with self.subTest(case=case_id):
                self.assert_ordered_check_receipts(capture["before"])

    def assert_ordered_check_receipts(self, before):
        run_id = next(iter(before["runs"]))
        check_id, revision, intent = before_check(before)
        adapter = self.root / "adapter.py"
        shutil.copyfile(SUITE / "check_adapter.py", adapter)
        self.case_json("config.json", {"endpoint": "http://fixture.invalid"})
        (self.root / "token").write_text("private")
        request = {"destination": "fixture:checks", "method": "reconcile", "intent_id": intent,
                   "input_digest": "sha256:" + "f" * 64,
                   "inputs": {"subject": {"type": "text", "value": "pagination"},
                              "revision": {"type": "text", "value": revision}}}
        response = io.StringIO(json.dumps({"id": check_id, "revision": revision, "result": "pending"}))
        transport = Mock()
        transport.open.return_value = response
        with patch("sys.stdin", io.StringIO(json.dumps(request))), patch("sys.stdout", io.StringIO()), \
                patch("urllib.request.build_opener", return_value=transport):
            runpy.run_path(str(adapter), run_name="__main__")
        sent = transport.open.call_args.args[0]
        self.assertEqual(sent.full_url, "http://fixture.invalid/checks/" + check_id)
        self.assertEqual(sent.get_header("X-fixture-intent"), intent)
        self.assertEqual(sent.get_header("X-fixture-revision"), revision)
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
            self.case_json("before.json", before)
            self.case_json("ledger-before.json", valid)
            result = judge(case_id, self.root, 0, False, "")
            self.assertEqual(result["verdict"], "pass", case_id)
            self.assertEqual(result["fault_witness"], "before.json + ledger-before.json + crash.json")

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
            # Entire internally consistent ledgers must not supply their own identity.
            for label, other_check, other_revision, other_intent in (
                    ("different candidate", "030d235dadc5c87c6cd64e93199c8ef3a8a21f33ead69bf20b4304c7f71748df",
                     "sha256:" + "d" * 64, intent),
                    ("different intent", check_id, revision, "sha256:" + "d" * 64)):
                invalid[label] = {
                    "checks": [{"id": other_check, "revision": other_revision}],
                    "calls": [{**row, "check_id": other_check, "intent": other_intent,
                               "served_revision": other_revision if row["served_revision"] == revision
                               else row["served_revision"]} for row in valid["calls"]],
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
            malformed = {"missing": None, "unwrapped run": before["runs"][run_id],
                         "empty": {}, "bad runs": {"schema": "af/scenario-list@11", "runs": []}}
            for label, path, value in (
                    ("second run", ("runs", "other"), before["runs"][run_id]),
                    ("bad snapshot", ("runs", run_id, "frames", "root", "steps", 0,
                                      "outputs", "result", "value", "result", "outputs",
                                      "snapshot", "snapshot_id"), "bad"),
                    ("wrong snapshot artifact", ("runs", run_id, "frames", "root", "steps", 0,
                                                 "outputs", "result", "value", "result", "outputs",
                                                 "snapshot", "artifact_type"), "af/Document@1"),
                    ("duplicate step", ("runs", run_id, "frames", "root", "steps"),
                     [before["runs"][run_id]["frames"]["root"]["steps"][1]] * 2),
                    ("ambiguous intents", ("runs", run_id, "frames", "root", "steps", 1,
                                           "intents"), [{"id": intent}] * 2),
                    ("bad intent digest", ("runs", run_id, "frames", "root", "steps", 1,
                                           "intents", 0, "input_digest"), None),
                    ("wrong waiting intent", ("runs", run_id, "waiting", "intent_id"),
                     "sha256:" + "d" * 64),
                    ("bad candidate type", ("runs", run_id, "frames", "root", "steps", 0,
                                           "outputs", "result", "type"), "text")):
                bad = deepcopy(before)
                target = bad
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = value
                malformed[label] = bad
            for reason, evidence in malformed.items():
                with self.subTest(case=case_id, before=reason):
                    if evidence is None:
                        (self.root / "before.json").unlink()
                    else:
                        self.case_json("before.json", evidence)
                    result = judge(case_id, self.root, 0, False, "")
                    self.assertEqual((result["verdict"], result["phase"]),
                                     ("invalid", "before-fault-witness"))
            (self.root / "before.json").write_text('{"runs":')
            self.assertIsNone(witness(case_id, self.root))
            schema_field = '"schema": ' + json.dumps(before["schema"])
            duplicate = json.dumps(before).replace(schema_field, '"schema": "bad", ' + schema_field)
            (self.root / "before.json").write_text(duplicate)
            self.assertIsNone(witness(case_id, self.root))
            nested_duplicate = json.dumps(before).replace(
                '"execution_id": ', '"execution_id": "discarded", "execution_id": ')
            (self.root / "before.json").write_text(nested_duplicate)
            self.assertIsNone(witness(case_id, self.root))
            (self.root / "before.json").write_text('{"runs":' + '[' * 2000 + '0' + ']' * 2000 + '}')
            self.assertIsNone(witness(case_id, self.root))
            self.case_json("before.json", before)
            # No after.json is required to establish a real fault before failed recovery.
            self.case_json("recovery-summary.json", {"requirements": {"correct_outcome": False}})
            failed = judge(case_id, self.root, 1, False, "recovery failed")
            self.assertEqual((failed["verdict"], failed["phase"]), ("fail", "verify-outcome"))
            (self.root / "recovery-summary.json").unlink()
            incomplete = judge(case_id, self.root, 1, False, "recovery stopped")
            self.assertEqual(incomplete["phase"], "after-fault-witness")
            for field, value in (("exit", -signal.SIGTERM), ("exit", float(-signal.SIGKILL)),
                                 ("fixture_alive", False)):
                self.case_json("crash.json", {**crash, field: value})
                self.assertIsNone(witness(case_id, self.root))

    def test_complete_native_before_contracts_and_required_field_near_misses(self):
        captured = json.loads((SUITE / "fixtures/recovery-before-native.json").read_text())
        scenario = json.loads((SUITE.parents[1] / "schemas/scenario-v11.json").read_text())["$defs"]
        task = json.loads((SUITE.parents[1] / "schemas/task-contracts-v4.json").read_text())["$defs"]
        listing = json.loads((SUITE.parents[1] / "schemas/scenario-list-v12.json").read_text())
        run_schema = json.loads((SUITE.parents[1] / "schemas/scenario-run-v12.json").read_text())
        self.assertEqual(scenario["Run"], {k: v for k, v in run_schema.items() if not k.startswith("$")
                                         and k not in ("title", "description")})

        def at(value, path):
            for key in path:
                value = value[key]
            return value

        current = json.loads((SUITE / "fixtures/recovery-before-native-v12.json").read_text())
        for case_id, capture in list(captured.items()) + list(current.items()):
            before, ledger = capture["before"], capture["ledger"]
            r = ("runs", next(iter(before["runs"])))
            f = r + ("frames", "root")
            pin = r + ("pin",)
            definition = pin + ("bundle", "definitions", "clean")
            progress = [f + ("steps", i) for i in range(2)]
            datum = progress[0] + ("outputs", "result")
            receipt = datum + ("value",)
            result = receipt + ("result",)
            records = [((), listing), (r, scenario["Run"]), (f, scenario["Frame"]),
                       (pin, scenario["Pin"]), (pin + ("bundle",), scenario["Bundle"]),
                       (pin + ("policy",), scenario["Policy"]), (definition, scenario["Scenario"]),
                       (r + ("limits",), scenario["Limits"]),
                       (definition + ("limits",), scenario["Limits"]),
                       (receipt, scenario["TaskReceipt"]), (result, task["taskResult"]),
                       (datum, {"required": ["type", "value"]}),
                       (definition + ("steps", 0, "operation", "target"), scenario["TaskTarget"]),
                       (r + ("waiting",), scenario["Waiting"]["oneOf"][2])]
            for path in progress:
                records.extend([(path, scenario["Progress"]),
                                (path + ("intents", 0), scenario["Intent"])])
            for port in at(before, result)["outputs"]:
                records.append((result + ("outputs", port), task["artifactInput"]))
            for index in range(2):
                records.append((definition + ("steps", index), scenario["Step"]))
            adapter_path = pin + ("policy", "adapters", "checks")
            records.append((adapter_path, scenario["Adapter"]))
            charge_key = next(iter(at(before, r)["task_charges"]))
            records.append((r + ("task_charges", charge_key), scenario["TaskCharge"]))
            self.case_json("crash.json", {"exit": -signal.SIGKILL, "fixture_alive": True})
            self.case_json("ledger-before.json", ledger)
            self.case_json("recovery-summary.json", {"scenario": "succeeded", "requirements": {
                "same_execution": True, "same_check_intent": True, "correct_outcome": True}})
            self.case_json("before.json", before)
            expected = before_check(before)
            self.assertIsNotNone(expected, case_id)
            self.assertEqual(ledger["checks"], [{"id": expected[0], "revision": expected[1]}])
            self.assertEqual(judge(case_id, self.root, 0, False, "")["verdict"], "pass")
            for path, schema in records:
                record = at(before, path)
                self.assertTrue(set(schema["required"]) <= record.keys(), path)
                mutations = [(key, "remove") for key in schema["required"]]
                mutations += [(key, "corrupt") for key in schema["required"]]
                mutations.append(("unexpected", "add"))
                for key, action in mutations:
                    with self.subTest(case=case_id, path=path, field=key, action=action):
                        broken = deepcopy(before)
                        target = at(broken, path)
                        if action == "remove":
                            del target[key]
                        elif action == "corrupt":
                            target[key] = [] if target[key] is None else None
                        else:
                            target[key] = True
                        self.assertIsNone(before_check(broken))
                        self.case_json("before.json", broken)
                        judged = judge(case_id, self.root, 0, False, "matching summary")
                        self.assertEqual((judged["verdict"], judged["phase"]),
                                         ("invalid", "before-fault-witness"))

    def test_native_before_identity_relationships_and_typed_near_misses(self):
        before = json.loads((SUITE / "fixtures/recovery-before-native.json").read_text())["restart-green"]["before"]
        self.assert_native_identity_near_misses(before)

    def test_v12_native_before_identity_relationships_and_typed_near_misses(self):
        captures = json.loads((SUITE / "fixtures/recovery-before-native-v12.json").read_text())
        for case_id, capture in captures.items():
            with self.subTest(case=case_id):
                self.assert_native_identity_near_misses(capture["before"])

    def assert_native_identity_near_misses(self, before):
        scenario = json.loads((SUITE.parents[1] / "schemas/scenario-v11.json").read_text())["$defs"]
        run_id = next(iter(before["runs"]))
        r = ("runs", run_id)
        f = r + ("frames", "root")
        progress = f + ("steps",)
        receipt = progress + (0, "outputs", "result", "value")
        result = receipt + ("result",)
        other = "sha256:" + "d" * 64
        run = before["runs"][run_id]
        changes = [
            (r + ("id",), "other"), (r + ("execution_id",), other),
            (r + ("created_unix_ms",), run["updated_unix_ms"] + 1),
            (r + ("dispatches",), True), (f + ("dispatches",), 1),
            (r + ("limits", "max_tokens"), 1001),
            (r + ("pin", "revision"), other),
            (r + ("pin", "definitions", "clean"), other),
            (f + ("scenario",), "other"), (f + ("inputs",), {"extra": {"type": "text", "value": "x"}}),
            (f + ("started_unix_ms",), run["created_unix_ms"] - 1),
            (progress + (0, "deadline_unix_ms"), run["created_unix_ms"]),
            (progress + (1, "started_unix_ms"), run["updated_unix_ms"] + 1),
            (progress + (0, "status"), "waiting"), (progress + (0, "skipped"), "skipped"),
            (progress + (1, "intents", 0, "id"), other),
            (progress + (0, "intents", 0, "input_digest"), other),
            (progress + (1, "intents", 0, "input_digest"), other),
            (progress + (0, "intents", 0, "phase"), "uncertain"),
            (progress + (1, "intents", 0, "evidence"), [False]),
            (r + ("waiting", "step"), "root/implement"),
            (r + ("waiting", "diagnostic"), ""),
            (receipt + ("task_id",), "other"), (receipt + ("plan_id",), other),
            (receipt + ("revision_id",), other), (receipt + ("charged_tokens",), True),
            (result + ("task_revision_id",), other),
            (result + ("execution",), "failed"), (result + ("acceptance",), "unsatisfied"),
            (result + ("domain_conclusion",), " "), (result + ("evidence",), []),
            (result + ("evidence",), [other, other]),
            (result + ("missing_obligations",), ["pending"]),
            (result + ("outputs", "snapshot", "artifact_ids"), [other, other]),
            (result + ("outputs", "snapshot", "snapshot_id"), other),
        ]
        for path, value in changes:
            with self.subTest(path=path, value=value):
                broken = deepcopy(before)
                target = broken
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = value
                self.assertIsNone(before_check(broken))
        # Recompute every pin/intent/input digest around malformed variable
        # policy and target fields; self-consistent hashes cannot replace native
        # bounds or required tags. These values are deliberately well typed.
        pin_path = r + ("pin",)
        definition = pin_path + ("bundle", "definitions", "clean")
        adapter = pin_path + ("policy", "adapters", "checks")
        target_path = definition + ("steps", 0, "operation", "target")
        bounded_changes = [
            (adapter + ("max_wall_ms",), scenario["Adapter"]["properties"]["max_wall_ms"]["maximum"] + 1),
            (adapter + ("max_output_bytes",), scenario["Adapter"]["properties"]["max_output_bytes"]["maximum"] + 1),
            (adapter + ("executable",), "/" + "x" * 4096),
            (target_path + ("repo",), "/" + "x" * 4096),
            (target_path + ("state",), "relative"),
            (definition + ("schema",), "af/scenario@9"),
            (definition + ("steps", 1, "inputs", "subject", "value", "value"), "other-subject"),
            (definition + ("steps", 1, "inputs", "revision", "projection", "port"), "verification"),
            (definition + ("completion", 0, "value", "value"), 1),
        ]
        for path, value in bounded_changes:
            broken = deepcopy(before)
            changed = broken
            for key in path[:-1]:
                changed = changed[key]
            changed[path[-1]] = value
            native_run = broken["runs"][run_id]
            pin = native_run["pin"]
            definition = pin["bundle"]["definitions"]["clean"]
            pin["definitions"] = {"clean": native_hash(definition)}
            pin["revision"] = native_hash([pin["bundle"], pin["policy"]])
            implementation, checks = native_run["frames"]["root"]["steps"]
            candidate = implementation["outputs"]["result"]
            revision = candidate["value"]["result"]["outputs"]["snapshot"]["snapshot_id"]
            inputs = [{}, {"body": {"type": "text", "value": "Fixture CI"}, "candidate": candidate,
                           "revision": {"type": "text", "value": revision},
                           "subject": {"type": "text", "value": "pagination"}}]
            for index, progress in enumerate((implementation, checks)):
                intent = progress["intents"][0]
                intent["id"] = native_hash([native_run["execution_id"], pin["revision"],
                                             "root/" + progress["step"], 1])
                intent["input_digest"] = native_hash([definition["steps"][index], inputs[index]])
            native_run["waiting"]["intent_id"] = checks["intents"][0]["id"]
            self.assertIsNone(before_check(broken), path)
        # A valid-looking candidate cannot swap the Task while keeping the same
        # check ledger. Rebind the input digest so the linked target check is
        # independently exercised, rather than merely failing the input hash.
        for key in ("task_id", "plan_id", "revision_id"):
            broken = deepcopy(before)
            native_run = broken["runs"][run_id]
            implementation, checks = native_run["frames"]["root"]["steps"]
            candidate = implementation["outputs"]["result"]
            candidate["value"][key] = "other" if key == "task_id" else other
            if key == "revision_id":
                candidate["value"]["result"]["task_revision_id"] = other
            step = native_run["pin"]["bundle"]["definitions"]["clean"]["steps"][1]
            snapshot = candidate["value"]["result"]["outputs"]["snapshot"]["snapshot_id"]
            inputs = {"body": {"type": "text", "value": "Fixture CI"}, "candidate": candidate,
                      "revision": {"type": "text", "value": snapshot},
                      "subject": {"type": "text", "value": "pagination"}}
            checks["intents"][0]["input_digest"] = native_hash([step, inputs])
            self.assertIsNone(before_check(broken), key)

    def test_matched_native_versions_and_rehashed_wall_and_deadline_bounds(self):
        for fixture in ("recovery-before-native.json", "recovery-before-native-v12.json"):
            captures = json.loads((SUITE / "fixtures" / fixture).read_text())
            for case_id, capture in captures.items():
                before, ledger = capture["before"], capture["ledger"]
                run_id = next(iter(before["runs"]))
                r = ("runs", run_id)
                pin = r + ("pin",)
                definition = pin + ("bundle", "definitions", "clean")
                version = 12 if before["schema"] == "af/scenario-list@12" else 11
                self.case_json("crash.json", {"exit": -signal.SIGKILL, "fixture_alive": True})

                def check(candidate, linked_ledger, valid):
                    self.case_json("before.json", candidate)
                    self.case_json("ledger-before.json", linked_ledger)
                    self.assertEqual(witness(case_id, self.root) is not None, valid)
                    self.assertEqual(before_check(candidate) is not None, valid)

                check(before, ledger, True)
                # Each level must match the complete version tuple, even when
                # all hashes and ledger references have been recomputed.
                tags = [((), "af/scenario-list", version),
                        (r, "af/scenario-run", version), (pin, "af/scenario-pin", version),
                        (pin + ("bundle",), "af/scenario-bundle", version - 1),
                        (definition, "af/scenario", version - 1)]
                for path, tag, expected in tags:
                    for replacement in (expected - 1, expected + 1, 99):
                        broken, linked = deepcopy(before), deepcopy(ledger)
                        target = broken
                        for key in path:
                            target = target[key]
                        target["schema"] = f"{tag}@{replacement}"
                        rebind_capture(broken, linked)
                        with self.subTest(fixture=fixture, case=case_id, tag=tag, version=replacement):
                            check(broken, linked, False)
                for key, value in (("id", "bad/id"), ("execution_id", "sha256:bad")):
                    broken, linked = deepcopy(before), deepcopy(ledger)
                    broken["runs"][run_id][key] = value
                    rebind_capture(broken, linked)
                    check(broken, linked, False)

                # Rebind valid-looking pin/intent hashes and the matching
                # ordered ledger around an invalid fixed definition wall.
                # 60s covers Checks but not implement; 59999 covers neither.
                for wall in (59999, 60000, 89999, 90000):
                    broken, linked = deepcopy(before), deepcopy(ledger)
                    run = broken["runs"][run_id]
                    run["limits"]["max_wall_ms"] = wall
                    run["pin"]["bundle"]["definitions"]["clean"]["limits"]["max_wall_ms"] = wall
                    rebind_capture(broken, linked)
                    with self.subTest(fixture=fixture, case=case_id, wall=wall):
                        check(broken, linked, wall == 90000)
                # Native Runs may choose a lower wall than their definition;
                # only the definition must cover every pinned step timeout.
                shorter = deepcopy(before)
                shorter["runs"][run_id]["limits"]["max_wall_ms"] = 1
                check(shorter, ledger, True)

                # Make deadline == start + timeout while overflowing SAFE.
                # Both Run and Frame wall sums remain within SAFE, so this
                # specifically exercises the Progress deadline bound.
                broken, linked = deepcopy(before), deepcopy(ledger)
                run = broken["runs"][run_id]
                run["limits"]["max_wall_ms"] = 90000
                run["pin"]["bundle"]["definitions"]["clean"]["limits"]["max_wall_ms"] = 90000
                run["created_unix_ms"] = SAFE - 100000
                run["updated_unix_ms"] = SAFE - 1
                frame = run["frames"]["root"]
                frame["started_unix_ms"] = run["created_unix_ms"]
                for progress, timeout in zip(frame["steps"], (90000, 60000)):
                    progress["started_unix_ms"] = SAFE - 1
                    progress["deadline_unix_ms"] = progress["started_unix_ms"] + timeout
                rebind_capture(broken, linked)
                check(broken, linked, False)

    def test_v12_restart_red_running_checks_remains_recovery_failure(self):
        capture = json.loads((SUITE / "fixtures/recovery-before-native-v12.json").read_text())["restart-red"]
        self.case_json("before.json", capture["before"])
        self.case_json("ledger-before.json", capture["ledger"])
        self.case_json("crash.json", {"exit": -signal.SIGKILL, "fixture_alive": True})
        self.case_json("recovery-summary.json", {"scenario": "failed", "requirements": {
            "same_execution": True, "same_check_intent": True, "correct_outcome": True}})
        self.case_json("after.json", {"status": "failed", "frames": {"root": {"steps": [
            {"step": "implement", "status": "succeeded"}, {"step": "checks", "status": "running"}]}}})
        judged = judge("restart-red", self.root, 1, False, "")
        self.assertEqual((judged["verdict"], judged["phase"]), ("fail", "verify-outcome"))
        self.assertIn("terminal Scenario has running step checks", judged["violations"])

    def test_worker_interval_and_terminal_duration_are_retained(self):
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
        self.assertEqual(result["metrics"]["terminal_s"], 8)
        self.assertEqual(record["metrics"]["terminal_s"], 8)

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

    def test_case_interval_excludes_delayed_orphan_cleanup_on_exit_and_timeout(self):
        for outcome in (0, 7, "timeout"):
            with self.subTest(outcome=outcome):
                clock = [10_000_000_000]
                events = []

                def run_case(*args, **kwargs):
                    events.append("subprocess")
                    kwargs["stdout"].write(b"case diagnostic\n")
                    clock[0] = 20_000_000_000
                    if outcome == "timeout":
                        raise subprocess.TimeoutExpired(args[0], kwargs["timeout"])
                    return Mock(returncode=outcome)

                def now():
                    events.append("clock")
                    return clock[0]

                def cleanup():
                    events.append("cleanup")
                    clock[0] += 100_000_000_000

                with patch("load_worker.sys.platform", "linux"), \
                        patch("load_worker.subprocess.run", side_effect=run_case), \
                        patch("load_worker.time.monotonic_ns", side_effect=now), \
                        patch("load_worker.stop_orphans", side_effect=cleanup) as stop:
                    code, timeout, diagnostic, interval = execute(
                        ["native-case"], self.root / "worker.log", {}, self.root, 10)
                stop.assert_called_once_with()
                self.assertEqual(events, ["clock", "subprocess", "clock", "cleanup"])
                self.assertEqual(clock[0], 120_000_000_000)
                self.assertEqual(interval, [10_000_000_000, 20_000_000_000])
                self.assertEqual((interval[1] - interval[0]) / 1e9, 10)
                # A later case runs during cleanup, but never overlaps this subprocess.
                self.assertEqual(max_overlap([interval, [30_000_000_000, 40_000_000_000]]), 1)
                self.assertEqual(code, None if outcome == "timeout" else outcome)
                self.assertEqual(timeout, outcome == "timeout")
                if outcome == 0:
                    self.assertEqual(diagnostic, "")
                else:
                    expected = ("native process exceeded 10s deadline" if timeout else
                                "native process exited 7")
                    self.assertEqual(diagnostic, expected + "\ncase diagnostic")

    def test_subprocess_launch_error_still_cleans_up_and_preserves_error(self):
        error = OSError("fixture launch failed")
        with patch("load_worker.sys.platform", "linux"), \
                patch("load_worker.subprocess.run", side_effect=error), \
                patch("load_worker.stop_orphans") as stop:
            with self.assertRaises(OSError) as raised:
                execute(["native-case"], self.root / "worker.log", {}, self.root, 10)
        self.assertIs(raised.exception, error)
        stop.assert_called_once_with()

    def test_trial_rechecks_identity_without_changing_outcome_or_measurements(self):
        changed = deepcopy(self.identity)
        changed["memory_bytes"] += 1024
        for final, expected in ((self.identity, self.identity), (changed, None), (None, None)):
            with self.subTest(final=final):
                result = {"verdict": "pass", "reason": "native pass", "metrics": {"cpu_s": 1}}
                argv = ["load_worker.py", "trial", "{}", RUN, RUN + "-0000", "--case", "clean"]
                with patch.object(sys, "argv", argv), patch("load_worker.os.umask"), \
                        patch("load_worker.run_case", return_value=result), \
                        patch("load_worker.compute_identity", side_effect=[self.identity, final]), \
                        patch("load_worker.emit") as emit:
                    self.assertEqual(worker_main(), 0)
                value = emit.call_args.args[2]
                self.assertEqual(value["compute_identity"], expected)
                self.assertEqual(value["verdict"], "pass")
                self.assertEqual(value["metrics"], {"cpu_s": 1})

    def test_evidence_checkout_guard_still_refuses_before_broker_calls(self):
        args = self.run_args()
        args.evidence = SUITE / "never-created-evidence"
        control = FakeControl()
        with self.assertRaisesRegex(ValueError, "retain evidence outside Git checkouts"):
            run(args, control)
        self.assertFalse(control.jobs)

    def test_controller_and_transitive_suite_implementation_bytes_are_bound(self):
        copy = self.root / "suite"
        for name in SUITE_FILES:
            target = copy / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SUITE / name, target)
        self.assertTrue({"load_suite.py", "control.py", "compare.py", "load_identity.py"} <= set(SUITE_FILES))
        original = files_identity(copy, SUITE_FILES)
        for name in SUITE_FILES:
            with self.subTest(name=name):
                path = copy / name
                data = path.read_bytes()
                path.write_bytes(data + b"\n")
                self.assertNotEqual(original, files_identity(copy, SUITE_FILES))
                path.write_bytes(data)

    def test_native_identity_is_stable_and_missing_hardware_or_limits_is_unavailable(self):
        self.assertIsNotNone(self.identity)
        proc, cgroup = self.root / "proc", self.root / "cgroup"
        (proc / "meminfo").write_text("MemTotal: 4194304 kB\nMemAvailable: 1 kB\n")
        self.assertEqual(self.identity, compute_identity(proc, cgroup, {0}))
        cpuinfo = proc / "cpuinfo"
        original = cpuinfo.read_text()
        cpuinfo.write_text(original.replace("Fixture CPU", "Other same-count CPU"))
        self.assertNotEqual(self.identity, compute_identity(proc, cgroup, {0}))
        cpuinfo.write_text(original)
        for name, value in (("memory.max", "1073741824"), ("memory.high", "536870912"),
                            ("memory.swap.max", "0"), ("cpuset.cpus.effective", "1")):
            path = cgroup / "job" / name
            previous = path.read_text()
            path.write_text(value)
            self.assertNotEqual(self.identity, compute_identity(proc, cgroup, {0}))
            path.write_text(previous)
        self.assertIsNone(compute_identity(proc, cgroup, {1}))
        (cgroup / "job/cpu.max").write_text("50000 100000")
        self.assertNotEqual(self.identity, compute_identity(proc, cgroup, {0}))
        (cgroup / "job/cpu.max").unlink()
        self.assertIsNone(compute_identity(proc, cgroup, {0}))
        (cgroup / "cgroup.subtree_control").write_text("cpuset memory")
        known_disabled = compute_identity(proc, cgroup, {0})
        self.assertIsNotNone(known_disabled)
        self.assertEqual(known_disabled["cgroups"][0]["cpu.max"], "controller-disabled")
        (cgroup / "cgroup.subtree_control").unlink()
        self.assertIsNone(compute_identity(proc, cgroup, {0}))
        native_identity(self.root)
        (proc / "cpuinfo").write_text("processor: 0\n")
        self.assertIsNone(compute_identity(proc, cgroup, {0}))

    def test_cgroup_names_are_volatile_but_ancestor_constraints_are_bound(self):
        proc, cgroup = self.root / "proc", self.root / "cgroup"
        (cgroup / "job").rename(cgroup / "another-job")
        (proc / "self/cgroup").write_text("0::/another-job")
        self.assertEqual(self.identity, compute_identity(proc, cgroup, {0}))
        (cgroup / "cpu.max").write_text("100000 100000")
        self.assertNotEqual(self.identity, compute_identity(proc, cgroup, {0}))
        (proc / "self/mountinfo").write_text(f"1 0 0:1 /hidden {cgroup} rw - cgroup2 cgroup rw")
        self.assertIsNone(compute_identity(proc, cgroup, {0}))
        native_identity(self.root)
        (proc / "cpuinfo").write_text("x" * (4 * 1024 * 1024 + 1))
        self.assertIsNone(compute_identity(proc, cgroup, {0}))

    def test_initial_namespace_and_full_mount_prove_leaf_and_root_ancestry(self):
        proc, cgroup = self.root / "proc", self.root / "cgroup"
        self.assertIsNotNone(self.identity)
        self.assertEqual(len(self.identity["cgroups"]), 2)
        self.assertEqual(self.identity["cgroups"][-1]["cpu.max"], "unbounded-root")
        (proc / "self/cgroup").write_text("0::/\n")
        identity = compute_identity(proc, cgroup, {0})
        self.assertIsNotNone(identity)
        self.assertEqual(identity["cgroups"], self.identity["cgroups"][-1:])
        (proc / "self/mountinfo").write_text(f"1 0 0:1 /hidden {cgroup} rw - cgroup2 cgroup rw")
        self.assertIsNone(compute_identity(proc, cgroup, {0}))

    def test_covered_full_mount_never_proves_visible_ancestry(self):
        proc, cgroup = self.root / "proc", self.root / "cgroup"
        full = f"1 0 0:1 / {cgroup} rw - cgroup2 cgroup rw\n"
        for cover in (full.replace("1 0 ", "2 0 "),
                      f"2 0 0:1 /hidden {cgroup} rw - cgroup2 cgroup rw\n",
                      f"2 0 0:2 / {cgroup} rw - tmpfs tmpfs rw\n",
                      f"2 1 0:1 /hidden {cgroup}/job rw - cgroup2 cgroup rw\n",
                      f"2 0 0:2 / {cgroup.parent} rw - tmpfs tmpfs rw\n"):
            for rows in (full + cover, cover + full):
                with self.subTest(rows=rows):
                    (proc / "self/mountinfo").write_text(rows)
                    self.assertIsNone(compute_identity(proc, cgroup, {0}))
        # A genuine enclosing mount in the parent chain is supported.
        (proc / "self/mountinfo").write_text(
            f"2 0 0:2 / {cgroup.parent} rw - tmpfs tmpfs rw\n" + full.replace("1 0 ", "1 2 "))
        self.assertEqual(compute_identity(proc, cgroup, {0}), self.identity)

    def test_private_namespace_root_hides_limits_even_with_matching_pid_one(self):
        proc, cgroup = self.root / "proc", self.root / "cgroup"
        (proc / "self/cgroup").write_text("0::/\n")
        # All visible controls stay identical while enclosing host limits change.
        for path in (cgroup / "job").iterdir():
            (cgroup / path.name).write_bytes(path.read_bytes())
        self.assertIsNotNone(compute_identity(proc, cgroup, {0}))
        namespace = proc / "self/ns/cgroup"
        namespace.unlink()
        namespace.symlink_to("cgroup:[4026533000]")
        (proc / "self/ns/pid").symlink_to("pid:[4026533001]")
        (proc / "1/ns").mkdir(parents=True)
        (proc / "1/ns/cgroup").symlink_to("cgroup:[4026533000]")
        (proc / "1/ns/pid").symlink_to("pid:[4026533001]")
        self.assertEqual((proc / "1/ns/cgroup").readlink(), namespace.readlink())
        hidden = self.root / "hidden-host-ancestor"
        hidden.mkdir()
        for quota, memory in (("100000 100000", "1073741824"), ("50000 100000", "536870912")):
            (hidden / "cpu.max").write_text(quota)
            (hidden / "memory.max").write_text(memory)
            self.assertIsNone(compute_identity(proc, cgroup, {0}))

    def test_missing_or_uncertain_namespace_proof_is_unavailable(self):
        proc, cgroup = self.root / "proc", self.root / "cgroup"
        namespace = proc / "self/ns/cgroup"
        namespace.unlink()
        self.assertIsNone(compute_identity(proc, cgroup, {0}))
        for target in ("cgroup:[0]", "pid:[4026531835]", "cgroup:[4026531835]extra"):
            with self.subTest(target=target):
                namespace.symlink_to(target)
                self.assertIsNone(compute_identity(proc, cgroup, {0}))
                namespace.unlink()
        namespace.write_text("cgroup:[4026531835]")
        self.assertIsNone(compute_identity(proc, cgroup, {0}))
        with patch("load_identity.os.readlink", side_effect=PermissionError("denied")):
            self.assertIsNone(compute_identity(proc, cgroup, {0}))

    def test_complete_pairs_and_changed_or_missing_identity_through_saved_reports(self):
        before, after = self.root / "before", self.root / "after"
        for directory in (before, after):
            (directory / "trials").mkdir(parents=True)
        binding = {"binary_sha256": "a" * 64, "suite_workload": "b" * 64, "source_workload": "c" * 64}
        placement = {"workers": WORKERS, "producer_sha256": "d" * 64,
                     "worker_environment": {worker: {"compute_identity": self.identity} for worker in WORKERS}}
        row = {"case": "clean", "seed": 0, "worker": WORKERS[0], "job": "pair"}
        result = {"verdict": "pass", "reason": "native pass", "metrics": {"terminal_s": 2},
                  "compute_identity": self.identity}

        def record(directory, config, pins, trial):
            save(directory / "trials/0.json", normalized(directory, row, trial, "short", pins, config))

        record(before, placement, binding, result)
        record(after, placement, binding, {**result, "metrics": {"terminal_s": 1}})
        pair, = compare_runs(before, after)["pairs"]
        self.assertEqual(pair["comparison"], "paired")
        self.assertEqual(pair["delta"], {"terminal_s": -1})
        variants = []
        proc, cgroup = self.root / "proc", self.root / "cgroup"
        namespace = proc / "self/ns/cgroup"
        namespace.unlink()
        for target in ("cgroup:[4026533000]", None):
            if target is not None:
                namespace.symlink_to(target)
            unavailable = compute_identity(proc, cgroup, {0})
            self.assertIsNone(unavailable)
            incomplete = deepcopy(placement)
            incomplete["worker_environment"][WORKERS[0]]["compute_identity"] = unavailable
            variants.append((incomplete, binding, {**result, "compute_identity": unavailable}, "unavailable_inputs"))
            namespace.unlink(missing_ok=True)
        changed = deepcopy(placement)
        changed["worker_environment"][WORKERS[0]]["compute_identity"]["cpus"][0][1]["model name"] = "Other CPU"
        changed_trial = {**result, "compute_identity": changed["worker_environment"][WORKERS[0]]["compute_identity"]}
        variants.append((changed, binding, changed_trial, "different_inputs"))
        variants.append((placement, {**binding, "suite_workload": "e" * 64}, result, "different_inputs"))
        variants.append(({**placement, "producer_sha256": "f" * 64}, binding, result, "different_inputs"))
        missing = deepcopy(placement)
        missing["worker_environment"][WORKERS[1]]["compute_identity"] = None
        variants.extend([(missing, binding, result, "unavailable_inputs"),
                         (placement, binding, {**result, "compute_identity": None}, "unavailable_inputs"),
                         ({**placement, "producer_sha256": None}, binding, result, "unavailable_inputs")])
        for config, pins, trial, expected in variants:
            with self.subTest(expected=expected, config=config):
                record(after, config, pins, trial)
                pair, = compare_runs(before, after)["pairs"]
                self.assertEqual(pair["comparison"], expected)
                self.assertEqual(pair["delta"], {})
                if expected == "unavailable_inputs":
                    # Equal incomplete evidence on both sides must not pair either.
                    record(before, config, pins, trial)
                    pair, = compare_runs(before, after)["pairs"]
                    self.assertEqual(pair["comparison"], "unavailable_inputs")
                    self.assertEqual(pair["delta"], {})
                    for directory in (before, after):
                        saved = json.loads((directory / "trials/0.json").read_text())
                        self.assertEqual(saved["verdict"], "pass")
                        self.assertEqual(saved["metrics"], result["metrics"])
                    record(before, placement, binding, result)
        record(after, placement, binding, result)
        path = before / "trials/0.json"
        entry = json.loads(path.read_text())
        del entry["workload_evidence_complete"]
        save(path, entry)
        self.assertEqual(compare_runs(before, after)["pairs"][0]["comparison"], "unavailable_inputs")
        save(after / "trials/0.json", entry)
        self.assertEqual(compare_runs(before, after)["pairs"][0]["comparison"], "unavailable_inputs")
        entry["schema"] = "af-durability/trial-report@1"
        save(path, entry)
        with self.assertRaisesRegex(ValueError, "expected .*af-durability/load-trial@3"):
            compare_runs(before, after)

    def test_identity_wire_schema_and_validator_parity(self):
        schema = json.loads((SUITE.parents[1] / "schemas/durability-compute-identity-v2.json").read_text())
        self.assertEqual(set(schema["required"]), set(self.identity))
        self.assertEqual(set(schema["properties"]), set(self.identity))
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(schema["properties"]["schema"]["const"], self.identity["schema"])
        self.assertEqual(schema["properties"]["ancestry"]["const"], ANCESTRY)
        self.assertEqual(set(schema["properties"]["cpus"]["items"]["prefixItems"][1]["required"]), CPU_FIELDS)
        self.assertEqual(set(schema["properties"]["cgroups"]["items"]["required"]), CGROUP_FIELDS)
        validator = Schema(schema, "compute-identity-v2")
        self.assertIsNone(validator.errors(self.identity))
        self.assertTrue(complete_identity(self.identity))
        for field in self.identity:
            broken = deepcopy(self.identity)
            del broken[field]
            self.assertFalse(complete_identity(broken))
            self.assertIsNotNone(validator.errors(broken))
        self.assertFalse(complete_identity({**self.identity, "extra": True}))
        for value in (None, [], True, 0, "proof"):
            self.assertFalse(complete_identity(value))
        trial_schema = json.loads((SUITE / "manifests/load-trial-v3.schema.json").read_text())
        row = {"case": "clean", "seed": 0, "worker": WORKERS[0], "job": "schema"}
        entry = normalized(self.root, row, {"verdict": "invalid", "reason": "unavailable", "metrics": {}},
                           "short", {"binary_sha256": "a" * 64, "source_workload": "b" * 64,
                                     "suite_workload": "c" * 64}, {})
        self.assertEqual(set(trial_schema["required"]), set(entry))
        self.assertEqual(set(trial_schema["properties"]), set(entry))
        self.assertFalse(trial_schema["additionalProperties"])
        self.assertEqual(trial_schema["properties"]["schema"]["const"], entry["schema"])
        proof_schema = trial_schema["properties"]["compute_evidence"]["anyOf"][1]
        self.assertIsNone(entry["compute_evidence"])
        self.assertEqual(set(proof_schema["required"]), {"worker", "cohort", "trial"})
        self.assertFalse(proof_schema["additionalProperties"])
        # Resolve the canonical schema reference for the subset evaluator, which only
        # handles internal definitions. The committed schema uses normal JSON Schema refs.
        trial_schema = json.loads(json.dumps(trial_schema).replace(
            '"urn:af:schema:durability-compute-identity:2"', '"#/$defs/compute"'))
        trial_schema["$defs"] = {"compute": schema}
        trial_validator = Schema(trial_schema, "load-trial-v3")
        self.assertIsNone(trial_validator.errors(entry))
        entry["workload_evidence_complete"] = True
        entry["compute_evidence"] = {"worker": WORKERS[0], "trial": self.identity,
                                     "cohort": {worker: self.identity for worker in WORKERS}}
        self.assertIsNone(trial_validator.errors(entry))
        entry["compute_evidence"]["trial"] = {**self.identity, "ancestry": None}
        self.assertIsNotNone(trial_validator.errors(entry))

    def test_saved_preproof_reports_remain_readable_but_never_pair(self):
        # Literal saved wire record: no call to today's normalizer or producer.
        legacy = json.loads((SUITE / "fixtures/load-trial-v2-preproof.json").read_text())
        before, after = self.root / "before", self.root / "after"
        for directory in (before, after):
            (directory / "trials").mkdir(parents=True)
            save(directory / "trials/0.json", legacy)
        pair, = compare_runs(before, after)["pairs"]
        self.assertEqual(pair["comparison"], "unavailable_inputs")
        self.assertEqual(pair["delta"], {})
        self.assertEqual(pair["before"], legacy)
        self.assertEqual(pair["after"], legacy)
        self.assertEqual(pair["before"]["metrics"], {"terminal_s": 2, "cpu_s": 1})
        self.assertTrue(pair["before"]["workload_evidence_complete"])

    def test_retained_proof_is_validated_even_with_matching_fingerprints(self):
        before, after = self.root / "before", self.root / "after"
        row = {"case": "clean", "seed": 0, "worker": WORKERS[0], "job": "proof"}
        placement = {"workers": WORKERS, "producer_sha256": "d" * 64,
                     "worker_environment": {w: {"compute_identity": self.identity} for w in WORKERS}}
        binding = {"binary_sha256": "a" * 64, "suite_workload": "b" * 64, "source_workload": "c" * 64}
        result = {"verdict": "pass", "reason": "native pass", "metrics": {"cpu_s": 1},
                  "compute_identity": self.identity}
        entry = normalized(before, row, result, "short", binding, placement)
        for directory in (before, after):
            (directory / "trials").mkdir(parents=True)
            save(directory / "trials/0.json", entry)
        self.assertEqual(compare_runs(before, after)["pairs"][0]["comparison"], "paired")
        missing_cohort = deepcopy(entry["compute_evidence"])
        missing_cohort["cohort"][WORKERS[1]] = None
        legacy_cohort = deepcopy(entry["compute_evidence"])
        legacy_cohort["cohort"][WORKERS[1]] = {
            **self.identity, "schema": "af-durability/compute-identity@1"}
        for proof in (None, [], {}, missing_cohort, legacy_cohort,
                      {**entry["compute_evidence"], "worker": "missing"}):
            broken = {**entry, "compute_evidence": proof}
            for directory in (before, after):
                save(directory / "trials/0.json", broken)
            self.assertEqual(compare_runs(before, after)["pairs"][0]["comparison"], "unavailable_inputs")
        for mutate in (lambda i: i.pop("ancestry"),
                       lambda i: i.update(ancestry=None),
                       lambda i: i.update(schema="af-durability/compute-identity@1"),
                       lambda i: i["ancestry"].update(namespace="cgroup:[4026533000]"),
                       lambda i: i["ancestry"].update(visibility="unknown"),
                       lambda i: i["ancestry"].update(extra=True)):
            broken = deepcopy(entry)
            mutate(broken["compute_evidence"]["trial"])
            for directory in (before, after):
                save(directory / "trials/0.json", broken)
            pair, = compare_runs(before, after)["pairs"]
            self.assertEqual(pair["comparison"], "unavailable_inputs")
            self.assertEqual(pair["delta"], {})
        changed = deepcopy(entry)
        for identity in [changed["compute_evidence"]["trial"], *changed["compute_evidence"]["cohort"].values()]:
            identity["memory_bytes"] += 1024
        save(before / "trials/0.json", entry)
        save(after / "trials/0.json", changed)
        self.assertEqual(compare_runs(before, after)["pairs"][0]["comparison"], "different_inputs")

    def test_comparison_pairs_across_builds_and_reports_mismatches_absence_and_regressions(self):
        before, after = (self.root / side for side in ("before", "after"))
        for directory in (before, after):
            (directory / "trials").mkdir(parents=True)
        binding = {"binary_sha256": "a" * 64, "suite_workload": "b" * 64, "source_workload": "c" * 64}
        placement = {"worker_environment": {worker: {"compute_identity": self.identity} for worker in WORKERS},
                     "workers": WORKERS, "producer_sha256": "d" * 64, "concurrency": 2}

        def add(directory, seed, verdict, *, config=placement):
            row = {"case": "clean", "seed": seed, "worker": WORKERS[0], "job": "compare"}
            result = {"verdict": verdict, "reason": verdict, "metrics": {}, "compute_identity": self.identity}
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
