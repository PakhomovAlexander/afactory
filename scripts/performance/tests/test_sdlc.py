"""SDLC failure handling with fixture events and synthetic reports; no speed claim."""
import copy
import json
import os
import importlib.util
import subprocess
import sys
import unittest
from datetime import datetime, timezone
from unittest import mock

from perflab import ci, collection, contracts, status
from perflab.values import ContractError
from tests import test_ci, test_planning
from tests.support import ROOT, SUITE, read


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

    def presentation(self, **changes):
        values = dict(schedule_dir=self.schedule, reports_dir=self.reports,
                      repository="owner/af", run_id=42, attempt=1, sha=self.head,
                      qualified=False, details=["primary not qualified"], mode="advisory")
        values.update(changes)
        return status.presentation(**values)

    def test_report_exposes_actual_verdict_scope_evidence_coverage_and_rounded_values(self):
        self.verdict["cases"][0]["baseline"]["median"] = "123456789/1000000"
        self.write_verdict()
        label, summary, annotations = self.presentation()
        self.assertEqual(label, "PASS (full)")
        self.assertIn("124/124 planned records matched", summary)
        self.assertIn("≈ 123.457", summary)
        self.assertNotIn("123456789/1000000", summary)
        self.assertIn("perf-report-" + self.item["run_id"], summary)
        self.assertIn("https://github.com/owner/af/actions/runs/42/attempts/1", summary)
        self.assertIn("not qualified", summary)
        self.assertIn("advisory completion", summary)
        self.assertEqual(annotations[0][0], "notice")

    def test_each_real_verdict_is_visible_without_changing_qualification(self):
        for fixture, label in (("synthetic-regression", "FAIL"),
                               ("synthetic-inconclusive", "INCOMPLETE")):
            self.verdict = json.loads(read(os.path.join(ROOT,
                "fixtures/performance/compare", fixture, "verdict.json")))
            self.verdict.update(run_id=self.item["run_id"], plan_digest=self.item["plan_digest"],
                                scope="full")
            self.write_verdict()
            title, summary, annotations = self.presentation()
            self.assertEqual(title, label + " (full; synthetic)")
            self.assertIn("not qualified", summary)
            self.assertEqual(annotations[0][0], "warning")

    def test_report_and_annotations_use_the_actions_server_on_github_enterprise(self):
        _, summary, annotations = self.presentation(server_url="https://github.example.test/")
        url = "https://github.example.test/owner/af/actions/runs/42/attempts/1"
        self.assertIn("[Run and report artifacts](" + url + ")", summary)
        self.assertIn(url, annotations[0][1])
        self.assertNotIn("https://github.com/", summary)

    def test_unavailable_primary_does_not_hide_bootstrap_report(self):
        # The actual pre-activation shape: the primary cannot be planned, but the control's
        # bootstrap comparison measured successfully. It never qualifies as the primary.
        self.repo.git("rm", "fixtures/performance/activation.toml")
        control = self.repo.commit("remove activation", {})
        candidate = self.repo.commit("candidate", {"README": "candidate\n"})
        event = ci.Event("push", {}, candidate, "refs/heads/main", "owner/af", 43, 1)
        doc, plans = ci.schedule(self.git, event, ci.Settings({
            "linux_gnu_native_release": "registry.invalid/tools@sha256:" + "4" * 64}))
        schedule_dir = self.path("bootstrap-schedule")
        ci.write(schedule_dir, doc, plans)
        bootstrap = next(item for item in doc["comparisons"] if item["role"] == "control_bootstrap")
        self.verdict.update(run_id=bootstrap["run_id"], plan_digest=bootstrap["plan_digest"],
                            scope="bootstrap")
        self.verdict["aggregate"]["satisfies_required_check"] = False
        self.path_report = os.path.join(self.reports, "verdict.json")
        self.write_verdict()
        os.unlink(os.path.join(self.reports, "perf-report-" + self.item["run_id"], "verdict.json"))
        os.rmdir(os.path.join(self.reports, "perf-report-" + self.item["run_id"]))
        label, summary, _ = self.presentation(schedule_dir=schedule_dir, run_id=43, sha=candidate)
        self.assertEqual(label, "PASS (bootstrap; primary unavailable)")
        self.assertIn("PASS (control bootstrap; bootstrap; measured)", summary)
        self.assertIn("124/124", summary)
        self.assertFalse(status.evaluate(schedule_dir, self.reports, self.jobs,
                                        "owner/af", 43, 1, candidate)[0])

    def test_missing_foreign_and_mixed_reports_are_presented_as_unavailable(self):
        for change in ("missing", "foreign", "mixed", "scope"):
            with self.subTest(change=change):
                self.write_verdict()
                if change == "missing":
                    os.unlink(self.path_report)
                elif change == "foreign":
                    self.verdict["plan_digest"] = "sha256:" + "9" * 64
                    self.write_verdict()
                elif change == "scope":
                    self.verdict["scope"] = "bootstrap"
                    self.verdict["aggregate"]["satisfies_required_check"] = False
                    self.write_verdict()
                else:
                    with open(os.path.join(self.reports, "verdict.json"), "wb") as stream:
                        stream.write(read(self.path_report))
                label, _, _ = self.presentation()
                self.assertEqual(label, "UNAVAILABLE")
                self.verdict["plan_digest"] = self.item["plan_digest"]
                self.verdict["scope"] = "full"
                flat = os.path.join(self.reports, "verdict.json")
                if os.path.exists(flat):
                    os.unlink(flat)

    def test_cli_exports_report_and_annotations_without_altering_advisory_exit(self):
        script = os.path.join(ROOT, "scripts/performance/ci-status.py")
        env = dict(os.environ, GITHUB_EVENT_NAME="pull_request", GITHUB_REPOSITORY="owner/af",
                   GITHUB_SERVER_URL="https://github.example.test",
                   GITHUB_RUN_ID="42", GITHUB_RUN_ATTEMPT="1", GITHUB_SHA=self.head,
                   PERF_JOB_RESULTS=json.dumps(self.jobs), GITHUB_OUTPUT=self.path("outputs"),
                   GITHUB_STEP_SUMMARY=self.path("summary"))
        result = subprocess.run([sys.executable, "-I", script, "--schedule", self.schedule,
                                 "--reports", self.reports], env=env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn(b"::notice title=Performance report::PASS", result.stdout)
        self.assertIn(b"report_label<<af_perf_", read(env["GITHUB_OUTPUT"]))
        self.assertIn(b"report_summary<<af_perf_", read(env["GITHUB_OUTPUT"]))
        self.assertIn(b"124/124", read(env["GITHUB_STEP_SUMMARY"]))
        self.assertIn(b"https://github.example.test/owner/af/actions/runs/42/attempts/1",
                      read(env["GITHUB_STEP_SUMMARY"]))


class AnnotationSafety(unittest.TestCase):
    def test_newlines_percent_and_property_separators_cannot_inject_commands(self):
        path = os.path.join(ROOT, "scripts/performance/ci-status.py")
        spec = importlib.util.spec_from_file_location("ci_status", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        line = module.command("warning", "title,:\n", "%\r\n::error::forged")
        self.assertEqual(line, "::warning title=title%2C%3A%0A::%25%0D%0A::error::forged")


class ComparisonPrecedence(test_planning.History):
    def test_pass_cannot_hide_another_comparisons_failure_or_missing_report(self):
        self.activate()
        policy = read(SUITE).decode().replace(
            "pairs = { quick = 40, nightly = 40, release = 40 }",
            "pairs = { quick = 12, nightly = 40, release = 40 }", 1)
        merge, base, head = test_ci.Events.merged(self, {test_planning.planning.SUITE_PATH: policy})
        doc, plans = ci.schedule(self.git, test_ci.Events.pr_event(self, merge, base, head),
                                 ci.Settings({"linux_gnu_native_release": test_ci.IMAGE}))
        schedule_dir, reports_dir = self.path("schedule"), self.path("reports")
        ci.write(schedule_dir, doc, plans)
        paths = []
        for item in doc["comparisons"]:
            plan = contracts.read_json(plans[item["run_id"]], "run")
            verdict = json.loads(read(os.path.join(ROOT,
                "fixtures/performance/compare/synthetic-pass/verdict.json")))
            verdict.update(run_id=item["run_id"], plan_digest=item["plan_digest"],
                           scope=plan["scope"])
            path = os.path.join(reports_dir, "perf-report-" + item["run_id"], "verdict.json")
            os.makedirs(os.path.dirname(path))
            with open(path, "wb") as stream:
                stream.write(contracts.write_bytes(verdict, "verdict"))
            paths.append((path, verdict))

        def label():
            return status.presentation(schedule_dir, reports_dir, "owner/af", 4242, 1,
                                       merge, False, [], "advisory")[0]

        self.assertTrue(label().startswith("PASS"))
        path, verdict = paths[-1]
        os.unlink(path)
        self.assertTrue(label().startswith("INCOMPLETE"), label())
        self.assertIn("report unavailable", label())
        failed = json.loads(read(os.path.join(ROOT,
            "fixtures/performance/compare/synthetic-regression/verdict.json")))
        failed.update(run_id=verdict["run_id"], plan_digest=verdict["plan_digest"],
                      scope=verdict["scope"])
        with open(path, "wb") as stream:
            stream.write(contracts.write_bytes(failed, "verdict"))
        self.assertTrue(label().startswith("FAIL"), label())


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
