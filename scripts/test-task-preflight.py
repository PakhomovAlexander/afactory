#!/usr/bin/env python3
"""Exercise advisory reporting against the captured native inspection fixture."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/task-preflight.py"
SPEC = importlib.util.spec_from_file_location("preflight", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Preflight(unittest.TestCase):
    def setUp(self):
        self.inspection = json.loads((ROOT / "fixtures/task-runtime/bound-inputs/inspection-bound.json").read_text())
        self.now = self.inspection["plan"]["limits"]["deadline_unix_ms"] - 30000
        self.checklist = {
            "schema": "af-preflight-checklist/1", "plan_id": self.inspection["plan_id"],
            "original_source_snapshot_id": self.inspection["plan"]["inputs"]["source"]["snapshot_id"],
            "criteria": [{"id": "pagination", "requirement": "Preserve pagination bounds",
                          "obligation": "verified", "check_name": "pagination",
                          "producer": {"node": "root.nodes.check", "port": "result"},
                          "consumer": {"node": "root.nodes.evaluate", "port": "checks"}}],
        }

    def run_report(self):
        return MODULE.report(self.inspection, self.checklist, self.now)

    def codes(self):
        return {issue["code"] for issue in self.run_report()["issues"]}

    def test_native_export_connected_without_granting_authority(self):
        before = copy.deepcopy(self.inspection)
        value = self.run_report()
        self.assertEqual(self.codes(), {"conditional_coverage", "coverage_edge_optional"})
        self.assertTrue(value["advisory_only"])
        self.assertEqual(value["lineage"]["candidate_replay"], "not_verified")
        self.assertEqual(before, self.inspection)

    def test_candidate_rerooting_is_reported_and_replay_is_not_inferred(self):
        self.checklist["retained_candidate_snapshot_id"] = self.checklist["original_source_snapshot_id"]
        self.checklist["original_source_snapshot_id"] = "sha256:" + "a" * 64
        self.assertTrue({"source_changed", "candidate_as_source", "replay_unverified"} <= self.codes())

    def test_stale_checklist(self):
        self.checklist["plan_id"] = "sha256:" + "a" * 64
        self.assertIn("stale_checklist", self.codes())

    def test_retained_diff_must_match_exact_identities(self):
        plan = json.loads((ROOT / "fixtures/task-runtime/preflight/review-plan.json").read_text())
        candidate = plan["resolved"]["candidate_snapshot_id"]
        self.checklist["original_source_snapshot_id"] = plan["resolved"]["base_snapshot_id"]
        self.inspection["plan"]["inputs"]["source"]["snapshot_id"] = plan["resolved"]["base_snapshot_id"]
        self.checklist["retained_candidate_snapshot_id"] = candidate
        value = MODULE.report(self.inspection, self.checklist, self.now, plan)
        self.assertEqual(value["lineage"]["intended_diff"], "nonempty_export")
        self.assertEqual(value["lineage"]["candidate_replay"], "not_verified")
        plan["subject"].update(empty=True, changed_paths=[])
        value = MODULE.report(self.inspection, self.checklist, self.now, plan)
        self.assertIn("empty_intended_diff", {row["code"] for row in value["issues"]})
        plan["resolved"]["candidate_snapshot_id"] = "sha256:" + "c" * 64
        value = MODULE.report(self.inspection, self.checklist, self.now, plan)
        self.assertIn("diff_identity_mismatch", {row["code"] for row in value["issues"]})

    def test_missing_twenty_run_check_is_not_covered_by_generic_gate(self):
        self.checklist["criteria"][0]["check_name"] = "linux_lock_twenty_runs"
        self.assertIn("missing_named_check", self.codes())

    def test_evidence_scheduled_after_evaluator(self):
        self.checklist["criteria"][0]["producer"] = {"node": "root.nodes.accept", "port": "result"}
        self.assertTrue({"evidence_after_consumer", "evidence_not_consumed"} <= self.codes())

    def test_order_alone_is_not_a_data_dependency(self):
        self.checklist["criteria"][0]["consumer"]["port"] = "requirements"
        self.assertIn("evidence_not_consumed", self.codes())

    def test_different_source_is_not_current_evidence(self):
        self.inspection["graph"]["nodes"]["root.nodes.evaluate"]["inputs"]["source"] = {
            "node": "root.inputs", "port": "source"}
        self.assertIn("evidence_source_mismatch", self.codes())

    def test_verifier_must_feed_covered_obligation(self):
        self.inspection["graph"]["coverage"]["verified"] = {"node": "root.nodes.check", "port": "result"}
        self.assertIn("verifier_not_in_acceptance", self.codes())

    def test_missing_mapping_and_conditional_evidence_are_unknown(self):
        self.inspection["plan"]["acceptance"]["goal"] = ["root.nodes.accept.result"]
        self.inspection["graph"]["nodes"]["root.nodes.check"]["conditions"] = [
            {"source": {"node": "root.nodes.implement", "port": "report"}, "outcome": "passed"}]
        self.assertTrue({"unmapped_obligation", "conditional_evidence"} <= self.codes())

    def test_charged_tokens_are_exact_and_not_called_available(self):
        self.inspection["chargeable_tokens"] = "18446744073709551616"
        self.inspection["attempts"] = 1
        value = self.run_report()
        self.assertEqual(value["budget"]["headroom_before_live_reservations"], "-18446744073709550616")
        self.assertIn("remaining_admission_unknown", self.codes())
        self.assertIn("budget_exceeded", self.codes())

    def test_implementer_cannot_spend_protected_reserve(self):
        self.inspection["graph"]["allowances"]["root.nodes.implement"]["tokens_per_attempt"] = 900
        self.assertIn("reservation_plus_reserve", self.codes())
        self.inspection["graph"]["slots"]["root.slots.implementer"]["role"] = "coder"
        self.assertIn("reservation_plus_reserve", self.codes())

    def test_unconditional_required_coverage_has_no_path_unknowns(self):
        self.inspection["graph"]["nodes"]["root.nodes.evaluate"]["conditions"] = []
        self.inspection["graph"]["nodes"]["root.nodes.accept"]["contract"]["inputs"]["evaluation"]["optional"] = False
        self.assertEqual(self.codes(), set())

    def test_condition_only_dependency_must_precede_consumer(self):
        self.inspection["graph"]["nodes"]["root.nodes.evaluate"]["conditions"] = [
            {"source": {"node": "root.nodes.accept", "port": "result"}, "outcome": "passed"}]
        with self.assertRaisesRegex(ValueError, "dependency order"):
            self.run_report()

    def test_running_zero_charge_is_not_pristine(self):
        self.inspection["phase"] = {"kind": "running"}
        self.assertIn("remaining_admission_unknown", self.codes())

    def test_owned_children_and_scopes_are_visible_without_double_counting(self):
        child = {"allowance": {"tokens_per_attempt": 800, "max_attempts": 1,
                               "wall_ms_per_attempt": 5000, "verification_attempts": 0}, "max_children": 3}
        scopes = {"root": {"tokens": 900, "members": ["root.nodes.implement"]}}
        self.inspection["graph"]["owned_children"] = {"root.children": child}
        self.inspection["graph"]["token_scopes"] = scopes
        value = self.run_report()
        self.assertEqual(value["budget"]["owned_child_templates"]["root.children"], child)
        self.assertEqual(value["budget"]["token_scopes"], scopes)
        self.assertIn("dynamic_budget_unknown", self.codes())

    def test_provider_admission_cost_is_visible(self):
        node = copy.deepcopy(self.inspection["graph"]["nodes"]["root.nodes.check"])
        node["operator"] = {"kind": "provider_admission", "bindings": []}
        self.inspection["graph"]["nodes"]["root.nodes.check"] = node
        self.inspection["graph"]["allowances"]["root.nodes.check"]["tokens_per_attempt"] = 32768
        rows = self.run_report()["budget"]["declared_allowances"]
        self.assertTrue(any(r["category"] == "provider_admission" and r["tokens_per_attempt"] == 32768 for r in rows))

    def test_expired_deadline(self):
        self.now += 30000
        self.assertIn("deadline_expired", self.codes())

    def test_duplicate_criterion_and_boolean_budget_are_rejected(self):
        self.checklist["criteria"] *= 2
        with self.assertRaises(ValueError):
            self.run_report()
        self.checklist["criteria"] = self.checklist["criteria"][:1]
        self.inspection["plan"]["limits"]["tokens"] = True
        with self.assertRaises(ValueError):
            self.run_report()

    def test_cli_json_exit_codes_and_read_only_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inspection, checklist = root / "inspection.json", root / "checklist.json"
            inspection.write_text(json.dumps(self.inspection))
            checklist.write_text(json.dumps(self.checklist))
            original = inspection.read_bytes()
            command = [sys.executable, str(SCRIPT), "--inspection", str(inspection),
                       "--checklist", str(checklist), "--now-unix-ms", str(self.now), "--json"]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("input_file_hashes", json.loads(result.stdout))
            self.assertEqual(inspection.read_bytes(), original)
            self.checklist["criteria"] = []
            checklist.write_text(json.dumps(self.checklist))
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            checklist.write_text('{"schema": "x", "schema": "y"}')
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn("duplicate JSON key", json.loads(result.stdout)["error"])

    def test_bad_obligation_has_consistent_text_and_json_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inspection, checklist = root / "inspection.json", root / "checklist.json"
            inspection.write_text(json.dumps(self.inspection))
            self.checklist["criteria"][0]["obligation"] = 5
            checklist.write_text(json.dumps(self.checklist))
            command = [sys.executable, str(SCRIPT), "--inspection", str(inspection), "--checklist", str(checklist)]
            for flags in ([], ["--json"]):
                result = subprocess.run(command + flags, capture_output=True, text=True)
                self.assertEqual(result.returncode, 1)
                self.assertNotIn("Traceback", result.stderr)
                self.assertIn("obligation name", result.stdout)


if __name__ == "__main__":
    unittest.main()
