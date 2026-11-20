"""Unit-test the portable driver protocol without crediting a native engine pass."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from worker_failover import main, run

ROOT = Path(__file__).resolve().parent
INPUTS = json.loads((ROOT / "manifests" / "worker-failover.json").read_bytes())
FIRST = {"allocation": "worker-a", "attempt": "original"}
NEXT = {"allocation": "worker-b", "attempt": "replacement"}


class Driver(unittest.TestCase):
    def exercise(self, replacement, missing=False, subjects=None, premature=False,
                 recovery=None, timeout=False, adapter_failure=None):
        calls = []
        before = ["scenario.submitted", "child.admitted", "first_attempt.started"]
        if recovery is None:
            recovery = ["worker.lost", "first_attempt.fenced",
                        "replacement.started", "replacement.verified"]
        late = ["stale_completion.delivered", "stale_completion.rejected",
                "parent.succeeded", "slot.reused", "late_usage.reconciled"]

        def adapter(action, payload):
            calls.append(action)
            if action == "capabilities":
                return {"engine": {"build": "driver-test"},
                        "capabilities": [] if missing else
                        ["remote_worker_replacement", "late_completion_delivery"]}
            if action == "start":
                return {"scenario": "case-id"}
            if action == "isolate_worker":
                self.assertEqual(payload["worker"], FIRST)
                return {}
            if action == "deliver_stale":
                self.assertEqual(payload["worker"], FIRST)
                self.assertEqual(payload["replacement"], replacement)
                return {}
            self.assertEqual(action, "observe")
            facts = before[:]
            if "isolate_worker" in calls:
                facts += recovery
            if "deliver_stale" in calls or (premature and "isolate_worker" in calls):
                facts += late
            trace = []
            for n, fact in enumerate(facts):
                filename = f"fact-{n}.txt"
                body = fact.encode()
                (Path(payload["evidence_dir"]) / filename).write_bytes(body)
                trace.append({
                    "id": str(n), "scenario": "case-id", "fact": fact,
                    "parents": [str(n - 1)] if n else [],
                    "subject": (subjects or {}).get(
                        fact, replacement if fact.startswith("replacement") else FIRST
                    ),
                    "evidence": {"path": filename, "sha256": hashlib.sha256(body).hexdigest()},
                })
            return {"trace": trace}

        with tempfile.TemporaryDirectory() as temp:
            trial = Path(temp) / "trial"
            if adapter_failure:
                command = ["worker_failover.py", str(Path(temp) / "adapter-secret"),
                           str(ROOT / "manifests" / "worker-failover.json"), str(trial)]

                def run_adapter(command, *, input, check, **_kwargs):
                    action = command[1]
                    if action == "observe" and calls.count("observe") == 2:
                        if adapter_failure is subprocess.TimeoutExpired:
                            raise subprocess.TimeoutExpired(command, 30, "stdout-secret",
                                                            "stderr-secret")
                        completed = subprocess.CompletedProcess(
                            command, 7, "stdout-secret", "stderr-secret"
                        )
                    else:
                        response = adapter(action, json.loads(input))
                        completed = subprocess.CompletedProcess(command, 0, json.dumps(response))
                    if check:
                        completed.check_returncode()
                    return completed

                with patch.object(sys, "argv", command), patch("builtins.print"), \
                     patch("worker_failover.subprocess.run", side_effect=run_adapter), \
                     patch("worker_failover.time.sleep"):
                    status = main()
                exported = (trial / "report.json").read_bytes()
                self.assertNotIn(b"-secret", exported)
                result = json.loads(exported)
                self.assertIn(str(trial.resolve() / "trace.json"), result["evidence"])
                trace = json.loads((trial / "trace.json").read_bytes())
                self.assertEqual([event["fact"] for event in trace], before + recovery)
                return result, status
            if timeout:
                inputs = {**INPUTS, "fault": {**INPUTS["fault"], "deadline_s": 5}}
                with patch("worker_failover.time.monotonic", side_effect=range(6)), \
                     patch("worker_failover.time.sleep"):
                    return run(inputs, trial, adapter), calls
            return run(INPUTS, trial, adapter), calls

    def test_replacement_and_late_completion(self):
        result, calls = self.exercise(NEXT)
        self.assertEqual(result["verdict"], "pass")
        self.assertTrue(result["fault_witnessed"])
        self.assertEqual(calls.count("deliver_stale"), 1)

    def test_same_allocation_fails_after_actual_fault(self):
        result, calls = self.exercise({**NEXT, "allocation": FIRST["allocation"]})
        self.assertEqual(result["verdict"], "fail")
        self.assertNotIn("deliver_stale", calls)

    def test_unsupported_allocates_nothing(self):
        result, calls = self.exercise(NEXT, missing=True)
        self.assertEqual(result["verdict"], "unsupported")
        self.assertFalse(result["fault_witnessed"])
        self.assertEqual(calls, ["capabilities"])

    def test_started_and_verified_replacement_must_be_the_same_attempt(self):
        result, calls = self.exercise(NEXT, subjects={"replacement.started": FIRST})
        self.assertEqual(result["verdict"], "fail")
        self.assertIn("replacement.started refers to a different Worker or Attempt",
                      result["violations"])
        self.assertNotIn("deliver_stale", calls)

    def test_stale_completion_must_follow_harness_release(self):
        result, calls = self.exercise(NEXT, premature=True)
        self.assertEqual(result["verdict"], "fail")
        self.assertNotIn("deliver_stale", calls)

    def test_duplicate_and_causal_violation_survive_early_failure(self):
        result, calls = self.exercise(NEXT, recovery=[
            "worker.lost", "first_attempt.fenced", "replacement.started",
            "replacement.verified", "first_attempt.fenced",
        ])
        self.assertEqual(result["verdict"], "fail")
        self.assertEqual(result["violations"], [
            "duplicate milestone: first_attempt.fenced",
            "missing causal edge: first_attempt.fenced -> replacement.started",
        ])
        self.assertNotIn("deliver_stale", calls)

    def test_forbidden_charge_survives_timeout_with_or_without_witnessed_loss(self):
        for recovery, verdict, witnessed in (
            (["worker.lost", "first_attempt.fenced", "unaccounted_charge"], "fail", True),
            (["unaccounted_charge"], "invalid", False),
        ):
            with self.subTest(verdict=verdict):
                result, calls = self.exercise(NEXT, recovery=recovery, timeout=True)
                self.assertEqual(result["verdict"], verdict)
                self.assertIs(result["fault_witnessed"], witnessed)
                missing_fact = "replacement.verified" if witnessed else "worker.lost"
                self.assertEqual(result["violations"], [
                    "forbidden observation: unaccounted_charge",
                    f"worker failover deadline for {missing_fact}",
                ])
                self.assertNotIn("deliver_stale", calls)

    def test_forbidden_charge_survives_adapter_failure(self):
        for recovery, verdict, status, witnessed in (
            (["worker.lost", "unaccounted_charge"], "fail", 1, True),
            (["unaccounted_charge"], "invalid", 2, False),
        ):
            with self.subTest(verdict=verdict):
                result, exit_code = self.exercise(
                    NEXT, recovery=recovery, adapter_failure=subprocess.CalledProcessError
                )
                self.assertEqual(exit_code, status)
                self.assertEqual(result["verdict"], verdict)
                self.assertIs(result["fault_witnessed"], witnessed)
                self.assertEqual(result["violations"], [
                    "forbidden observation: unaccounted_charge",
                    "adapter execution failed (exit status 7)",
                ])

    def test_adapter_timeout_after_witnessed_loss(self):
        result, exit_code = self.exercise(
            NEXT, recovery=["worker.lost"], adapter_failure=subprocess.TimeoutExpired
        )
        self.assertEqual(exit_code, 1)
        self.assertEqual(result["verdict"], "fail")
        self.assertTrue(result["fault_witnessed"])
        self.assertEqual(result["violations"], ["adapter execution timed out after 30 seconds"])

    def test_late_usage_must_reconcile_original_attempt(self):
        result, _ = self.exercise(NEXT, subjects={"late_usage.reconciled": NEXT})
        self.assertEqual(result["verdict"], "fail")
        self.assertIn("late_usage.reconciled refers to a different Worker or Attempt",
                      result["violations"])

    def test_native_probe_runs_without_historical_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            probe = root / "probe.py"
            probe.write_bytes((ROOT / "native_worker_capabilities.py").read_bytes())
            probe.chmod(0o700)
            completed = subprocess.run(
                [sys.executable, "-B", str(ROOT / "worker_failover.py"), str(probe),
                 str(ROOT / "manifests/worker-failover.json"), str(root / "trial")],
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(completed.returncode, 3, completed.stderr)
            result = json.loads(completed.stdout)
            self.assertEqual(result["verdict"], "unsupported")
            self.assertEqual(result["engine"]["build"], "unprobed")
            self.assertEqual(result["missing_capabilities"],
                             ["late_completion_delivery", "remote_worker_replacement"])


if __name__ == "__main__":
    unittest.main()
