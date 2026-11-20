"""Unit-test the portable driver protocol without crediting a native engine pass."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from worker_failover import run

ROOT = Path(__file__).resolve().parent
INPUTS = json.loads((ROOT / "manifests" / "worker-failover.json").read_bytes())
FIRST = {"allocation": "worker-a", "attempt": "original"}
NEXT = {"allocation": "worker-b", "attempt": "replacement"}


class Driver(unittest.TestCase):
    def exercise(self, replacement, missing=False, subjects=None, premature=False):
        calls = []
        before = ["scenario.submitted", "child.admitted", "first_attempt.started"]
        recovery = ["worker.lost", "first_attempt.fenced", "replacement.started", "replacement.verified"]
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
            return run(INPUTS, Path(temp) / "trial", adapter), calls

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
