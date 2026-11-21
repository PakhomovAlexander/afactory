"""Run the file relay adapter against isolated outbox and receipt fixtures."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock

from control import Control, HTTPError


class BrokerAdapter(unittest.TestCase):
    def setUp(self):
        work = tempfile.TemporaryDirectory()
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        self.adapter = self.root / "adapter.py"
        shutil.copyfile(Path(__file__).with_name("broker_check_adapter.py"), self.adapter)
        self.call = {
            "destination": "fixture:checks", "intent_id": "original", "input_digest": "bound",
            "inputs": {"subject": {"value": "pagination"}, "revision": {"value": "candidate"}},
        }

    def invoke(self, method):
        return subprocess.run(
            [sys.executable, self.adapter], input=json.dumps({**self.call, "method": method}),
            capture_output=True, text=True, timeout=5,
        )

    def start_check(self):
        pending = self.invoke("execute")
        self.assertEqual(pending.returncode, 0, pending.stderr)
        self.assertEqual(json.loads(pending.stdout)["outcome"], "pending")
        return json.loads((self.root / "request.json").read_text())

    def test_receipt_requires_the_exact_durable_intent(self):
        request = self.start_check()
        receipt = self.root / "receipt.json"
        receipt.write_text(json.dumps({**request, "intent": "other", "result": "green"}))
        unrelated = self.invoke("reconcile")
        self.assertNotEqual(unrelated.returncode, 0)
        self.assertIn("unrelated check", unrelated.stderr)

        receipt.write_text(json.dumps({**request, "result": "green"}))
        exact = self.invoke("reconcile")
        self.assertEqual(exact.returncode, 0, exact.stderr)
        self.assertEqual(json.loads(exact.stdout)["outcome"], "done")
        self.call["intent_id"] = "other"
        unrelated = self.invoke("reconcile")
        self.assertNotEqual(unrelated.returncode, 0)
        self.assertIn("different intent", unrelated.stderr)

    def test_retry_replays_only_the_exact_original_red_receipt(self):
        request = self.start_check()
        outbox = self.root / "request.json"
        original_outbox = outbox.read_bytes()
        receipt = self.root / "receipt.json"
        receipt.write_text(json.dumps({**request, "result": "red"}))
        original_receipt = receipt.read_bytes()
        self.call.update(intent_id="retry", input_digest="retry-bound")

        for method in ("reconcile", "execute"):
            with self.subTest(method=method):
                replay = self.invoke(method)
                self.assertEqual(replay.returncode, 0, replay.stderr)
                response = json.loads(replay.stdout)
                self.assertEqual(response["outcome"], "failed")
                self.assertEqual(response["intent_id"], "retry")
                self.assertEqual(response["input_digest"], "retry-bound")
                self.assertEqual(outbox.read_bytes(), original_outbox)
                self.assertEqual(receipt.read_bytes(), original_receipt)
                self.assertFalse((self.root / "request.pending").exists())

        for field in ("subject", "revision"):
            with self.subTest(candidate_field=field):
                original = self.call["inputs"][field]["value"]
                self.call["inputs"][field]["value"] = original + "-other"
                rejected = self.invoke("reconcile")
                self.call["inputs"][field]["value"] = original
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("different candidate", rejected.stderr)

    def test_retry_rejects_pending_forged_and_orphaned_receipts(self):
        request = self.start_check()
        self.call["intent_id"] = "retry"
        receipt = self.root / "receipt.json"
        rejected = self.invoke("reconcile")
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("different intent", rejected.stderr)

        for field, value in (("id", "other-check"), ("revision", "other-revision"),
                             ("intent", "retry")):
            with self.subTest(receipt_field=field):
                receipt.write_text(json.dumps({**request, "result": "red", field: value}))
                rejected = self.invoke("reconcile")
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("unrelated check", rejected.stderr)

        receipt.write_text(json.dumps({**request, "result": "red"}))
        (self.root / "request.json").unlink()
        rejected = self.invoke("reconcile")
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("has no durable check request", rejected.stderr)

    def test_broker_conflict_does_not_use_an_existing_jobs_receipts(self):
        control = Control(None)
        path = "/v1/resources/r-101/jobs"
        control.request = Mock(side_effect=HTTPError(path, 409, "job already exists"))
        with self.assertRaises(HTTPError):
            control.submit("r-101", "trial-001", ["command"], self.root / "evidence")
        control.request.assert_called_once_with(path, {"name": "trial-001", "argv": ["command"]})


if __name__ == "__main__":
    unittest.main()
