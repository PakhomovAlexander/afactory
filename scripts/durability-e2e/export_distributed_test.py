"""Exercise the distributed exporter with assembled local evidence."""
from contextlib import closing
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

from export_distributed import export

MANIFESTS = Path(__file__).with_name("manifests")


class Exporter(unittest.TestCase):
    def run_export(self, case="green", *, after_intents=("native-intent",),
                   failed_check=None, controller_failed=False, witnessed=True,
                   legacy_provenance=False):
        manifest = json.loads((MANIFESTS / f"broker-{case}.json").read_text())
        clean = case == "clean"
        outcome = "failed" if case == "red" else "succeeded"
        gate_status = "running" if case == "red" else "succeeded"
        result = manifest["fault"]["check_result"]
        request = {"id": "external-check", "revision": "candidate"}
        worker_root = "/worker/trial"
        initial = {"phase": "initialize", "host": "fixture", "state": "empty"}
        accepted = {**initial, **request, "phase": "accept", "state": "pending"}
        finished = {**accepted, "phase": "finish", "state": "done", "result": result}
        marked = {"phase": "mark", "host": "engine", "root": worker_root, "id": request["id"]}
        fixture_tree = manifest["workload"]["fixture_git_tree"]
        outbox = {"host": "engine", "root": worker_root, "request": request,
                  "pid": 10, "execution": "run"}
        if not legacy_provenance:
            outbox["fixture_git_tree"] = fixture_tree
        down = {"phase": "down", "host": "engine", "pid": 10, "killed": -9, "execution": "run"}
        terminal = {"phase": "terminal", "execution": "run", "scenario": outcome,
                    "host": "engine", "pid": 10 if clean else 20, "binary_sha256": "build",
                    "terminal_s": 1, "checks": dict.fromkeys((
                        "same_attempts", "same_charge", "same_candidate", "same_intent",
                        "terminal_steps_settled", "expected_outcome",
                    ), True)}
        if case == "red":
            terminal["checks"]["terminal_steps_settled"] = False
        if failed_check:
            terminal["checks"][failed_check] = False
        report = {
            "schema": "af-durability/broker-recovery@1",
            "engine": manifest["placement"]["engine"], "fixture": manifest["placement"]["fixture"],
            "external_ledger": finished, "down": None if clean else down, "terminal": terminal,
            "fault": None if clean else "committed_check_reply_withheld_and_scheduler_killed",
            "verdict": "fail" if controller_failed or not all(terminal["checks"].values()) else "pass",
        }
        child = {"status": "succeeded", "outputs": {
            "result": {"value": {"result": {"outputs": {"snapshot": {"snapshot_id": "candidate"}}}}},
        }}

        def run_view(status, step_status, intents):
            return {"runs": {"run": {
                "execution_id": "run", "status": status, "waiting": {"kind": "reconcile"},
                "frames": {"root": {"steps": [
                    child, {"status": step_status, "intents": [{"id": intent} for intent in intents]},
                ]}},
            }}}

        task = {"state": {"outcome": "verified"}, "result": {"acceptance": "satisfied"},
                "history": [], "chargeable_tokens": 0, "attempts": 1}
        limits = {key: manifest["workload"][key]
                  for key in ("max_tokens", "max_dispatches", "max_wall_ms")}
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)

            def write(path, value):
                file = root / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(json.dumps(value))

            for action, evidence in (("initialize", initial), ("accept", accepted),
                                     ("finish", finished), ("mark", marked),
                                     ("stage", terminal if clean else down if witnessed else None),
                                     ("restart", terminal)):
                output = "AF_DURABILITY " + json.dumps(evidence) if evidence else ""
                write(f"control/{action}/logs.json", {"output": output})
            write("control/distributed-report.json", report)
            write("control/mark/request.json", {"argv": [None] * 6 + [
                json.dumps(finished if clean else accepted),
            ]})
            write("control/stage/outbox.json", outbox)
            write("engine/before.json", run_view("waiting", "running", ("native-intent",)))
            write("engine/after.json", run_view(outcome, gate_status, after_intents))
            write("engine/task-before.json", task)
            write("engine/task-after.json", task)
            write("engine/ant.json", {"execution": {"per_run": limits}})
            if legacy_provenance:
                write("engine/provenance.json", {
                    "host": "engine", "repository": worker_root + "/repo",
                    "fixture_git_tree": fixture_tree,
                })
            (root / "fixture").mkdir()
            with closing(sqlite3.connect(root / "fixture/checks.sqlite")) as db, db:
                db.execute("CREATE TABLE checks (id TEXT PRIMARY KEY, revision TEXT, result TEXT)")
                db.execute("CREATE TABLE history (sequence INTEGER PRIMARY KEY, phase TEXT, id TEXT)")
                db.execute("INSERT INTO checks VALUES (?, ?, ?)",
                           (request["id"], request["revision"], result))
                db.executemany("INSERT INTO history(phase,id) VALUES (?,?)", [
                    ("initialize", None), ("accept", request["id"]), ("finish", request["id"]),
                ])
            return export(root, manifest)

    def test_clean_and_green_pass(self):
        for case in ("clean", "green"):
            with self.subTest(case=case):
                result = self.run_export(case, legacy_provenance=case == "green")
                self.assertEqual(result["verdict"], "pass")
                self.assertEqual(result["violations"], [])
                self.assertEqual(result["fault_witnessed"], case == "green")

    def test_changed_native_intent_fails_despite_passing_reports(self):
        for intents in (("replacement-intent",), (), ("retry-intent", "native-intent")):
            with self.subTest(intents=intents):
                result = self.run_export(after_intents=intents)
                self.assertEqual(result["verdict"], "fail")
                self.assertEqual(result["violations"], ["native check intent changed"])

    def test_failed_terminal_assertion_is_preserved(self):
        result = self.run_export(failed_check="same_intent")
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["violations"], ["native terminal assertion failed: same_intent"])

    def test_failed_controller_verdict_is_preserved(self):
        result = self.run_export(controller_failed=True)
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["violations"], ["controller reported a failed distributed trial"])

    def test_missing_fault_witness_is_invalid_even_with_reported_failure(self):
        result = self.run_export(witnessed=False, failed_check="same_intent")
        self.assertEqual(result["verdict"], "invalid")
        self.assertFalse(result["fault_witnessed"])
        self.assertEqual(result["violations"], ["native terminal assertion failed: same_intent"])

    def test_red_terminal_step_violation_is_reported_once(self):
        result = self.run_export("red")
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["violations"], ["terminal Scenario has an unsettled step"])

    def test_red_replay_with_appended_intents_fails_only_for_unsettled_step(self):
        result = self.run_export("red", after_intents=(
            "native-intent", "retry-1", "retry-2", "retry-3",
        ))
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["violations"], ["terminal Scenario has an unsettled step"])


if __name__ == "__main__":
    unittest.main()
