"""SDLC failure handling with fixture events and synthetic reports; no speed claim."""
import copy
import json
import os
import unittest
from datetime import datetime, timezone
from unittest import mock

from perflab import ci, collection, contracts, status
from perflab.values import ContractError
from tests import test_planning
from tests.support import ROOT, read


class StableCheck(test_planning.History):
    def setUp(self):
        super().setUp()
        self.activate()
        self.head = self.repo.git("rev-parse", "HEAD")
        event = ci.Event("workflow_dispatch", {}, self.head, "refs/heads/main", "owner/af",
                         42, 1, {"base": self.root, "head": self.head, "control": self.head,
                                 "scope": "full", "tier": "quick"})
        self.doc, self.plans = ci.schedule(self.git, event, ci.Settings({
            "linux_gnu_native_release": "registry.invalid/tools@sha256:" + "4" * 64}))
        self.schedule = self.path("schedule")
        ci.write(self.schedule, self.doc, self.plans)
        self.reports = self.path("reports")
        self.item = self.doc["comparisons"][0]
        self.assertEqual(self.item["status"], "planned", self.item)
        self.verdict = json.loads(read(os.path.join(ROOT,
            "fixtures/performance/compare/synthetic-pass/verdict.json")))
        # A report-shaped fixture of a qualified result, not retained measurement evidence.
        self.verdict.update(run_id=self.item["run_id"], plan_digest=self.item["plan_digest"],
                            scope="full", provenance="measured")
        self.verdict["aggregate"]["satisfies_required_check"] = True
        self.path_report = os.path.join(self.reports, "perf-report-" + self.item["run_id"],
                                        "verdict.json")
        os.makedirs(os.path.dirname(self.path_report))
        self.write_verdict()
        self.jobs = {name: {"result": "success"} for name in
                     ("plan", "build", "shard", "aggregate")}

    def write_verdict(self):
        with open(self.path_report, "wb") as stream:
            stream.write(contracts.write_bytes(self.verdict, "verdict"))

    def evaluate(self, **changes):
        values = dict(schedule_dir=self.schedule, reports_dir=self.reports, jobs=self.jobs,
                      repository="owner/af", run_id=42, attempt=1, sha=self.head)
        values.update(changes)
        return status.evaluate(**values)

    def test_success_requires_every_job_and_the_qualified_primary(self):
        self.assertTrue(self.evaluate()[0])
        for job in self.jobs:
            for result in ("failure", "cancelled", "skipped"):
                jobs = copy.deepcopy(self.jobs)
                jobs[job]["result"] = result
                self.assertFalse(self.evaluate(jobs=jobs)[0], (job, result))

    def test_missing_report_and_wrong_attempt_are_not_success(self):
        with self.assertRaises(ContractError):
            self.evaluate(attempt=2)
        os.unlink(self.path_report)
        with self.assertRaises(OSError):
            self.evaluate()

    def test_report_of_another_plan_is_refused(self):
        self.verdict["plan_digest"] = "sha256:" + "9" * 64
        self.write_verdict()
        with self.assertRaisesRegex(ContractError, "another plan"):
            self.evaluate()

    def test_single_flat_report_keeps_identity_and_rejects_mixed_layouts(self):
        named_dir = os.path.dirname(self.path_report)
        flat = os.path.join(self.reports, "verdict.json")
        os.rename(self.path_report, flat)
        os.rmdir(named_dir)
        self.assertTrue(self.evaluate()[0])
        self.path_report = flat
        self.verdict["run_id"] = "foreign_run"
        self.write_verdict()
        with self.assertRaisesRegex(ContractError, "another plan"):
            self.evaluate()
        self.verdict["run_id"] = self.item["run_id"]
        self.write_verdict()
        os.mkdir(named_dir)
        with self.assertRaisesRegex(ContractError, "mixed flat and named"):
            self.evaluate()

    def test_partial_and_synthetic_passes_cannot_satisfy_enforcement(self):
        for field, value in (("scope", "partial_scope"), ("provenance", "synthetic")):
            with self.subTest(field=field):
                original = copy.deepcopy(self.verdict)
                self.verdict[field] = value
                self.verdict["aggregate"]["satisfies_required_check"] = False
                self.write_verdict()
                self.assertFalse(self.evaluate()[0])
                self.verdict = original

    def test_regression_inconclusive_and_missing_records_remain_unqualified(self):
        for fixture in ("synthetic-regression", "synthetic-inconclusive", "synthetic-failed-shard"):
            self.verdict = json.loads(read(os.path.join(ROOT,
                "fixtures/performance/compare", fixture, "verdict.json")))
            self.verdict.update(run_id=self.item["run_id"], plan_digest=self.item["plan_digest"])
            self.write_verdict()
            self.assertFalse(self.evaluate()[0], fixture)

    def test_missing_plan_is_refused_and_default_policy_is_advisory(self):
        os.unlink(os.path.join(self.schedule, "plans", self.item["run_id"] + ".json"))
        with self.assertRaises(OSError):
            self.evaluate()
        self.assertEqual(status.enforcement(ROOT), "advisory")


class Api:
    repository = "owner/af"

    def __init__(self, rows, total=None):
        self.rows, self.total = rows, len(rows) if total is None else total
        self.calls = []

    def get_json(self, path, what):
        self.calls.append(path)
        if path.endswith(".yml"):
            name = path.rsplit("/", 1)[1]
            return {"id": 1 if name == "performance.yml" else 2,
                    "path": ".github/workflows/" + name}
        if "/2/runs?" in path:
            return {"total_count": 0, "workflow_runs": []}
        return {"total_count": self.total, "workflow_runs": self.rows}


class Collection(unittest.TestCase):
    def test_required_pr_policy_keeps_calibration_experiments_and_release_handoffs_advisory(self):
        for event in ("pull_request", "push"):
            self.assertEqual(status.effective_enforcement("required", event), "required")
        for event in ("workflow_dispatch", "workflow_run", "schedule"):
            self.assertEqual(status.effective_enforcement("required", event), "advisory")
        with self.assertRaises(ContractError):
            status.effective_enforcement("required", "unknown")

    def run_record(self):
        return {"id": 42, "workflow_id": 1, "status": "completed", "conclusion": "success",
                "repository": {"full_name": "owner/af"}}

    def test_discovery_binds_repository_workflow_and_complete_pagination(self):
        api = Api([self.run_record()])
        self.assertEqual(collection.completed(api, now=datetime(2026, 10, 21,
                         tzinfo=timezone.utc)), [self.run_record()])
        self.assertTrue(any("created=%3E%3D2026-10-08" in call for call in api.calls))
        for key, value in (("workflow_id", 3), ("status", "in_progress"),
                           ("repository", {"full_name": "foreign/repo"})):
            row = self.run_record()
            row[key] = value
            with self.assertRaises(ContractError):
                collection.completed(Api([row]))
        with self.assertRaises(ContractError):
            collection.completed(Api([self.run_record()], total=1001))
        with self.assertRaises(ContractError):
            collection.completed(Api([self.run_record(), self.run_record()]))

    def test_each_lab_is_collected_and_calibration_is_pinned(self):
        api, store = mock.Mock(), mock.Mock()
        api.repository = "owner/af"
        api.artifacts.return_value = [{"name": "af-perf-gh42_a1_primary-run"},
                                      {"name": "af-perf-gh42_a1_primary-shard-0"}]
        store.state.return_value = {}
        row = dict(self.run_record(), display_title="af-perf calibration sha256:abc aa_01")
        with mock.patch.object(collection.transport, "fetch", return_value=("imported", "x")) as fetch:
            self.assertTrue(collection.collect(store, api, [row], emit=lambda _: None))
            fetch.assert_called_once_with(store, api, 42, "gh42_a1_primary")
        store.pin.assert_called_once()
        api.artifacts.return_value = [{"name": "af-perf-gh43_a1_primary-run"}]
        self.assertFalse(collection.collect(store, api, [row], emit=lambda _: None))

    def test_a_failed_collection_is_retried_and_already_archived_runs_are_not_downloaded(self):
        api, store = mock.Mock(), mock.Mock()
        api.repository = "owner/af"
        api.artifacts.return_value = [{"name": "af-perf-gh42_a1_primary-run"}]
        store.state.return_value = {}
        with mock.patch.object(collection.transport, "fetch", side_effect=ContractError("offline")) as fetch:
            for _ in range(2):
                self.assertFalse(collection.collect(store, api, [self.run_record()], emit=lambda _: None))
            self.assertEqual(fetch.call_count, 2)
        known = mock.Mock()
        known.event = {"event": {"source": {"kind": "actions", "repository": "owner/af",
                                           "actions_run_id": 42}}}
        store.state.return_value = {"gh42_a1_primary": known}
        with mock.patch.object(collection.transport, "fetch") as fetch:
            self.assertTrue(collection.collect(store, api, [self.run_record()], emit=lambda _: None))
            fetch.assert_not_called()
