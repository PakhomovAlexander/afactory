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

    def test_receipt_requires_the_exact_durable_intent(self):
        pending = self.invoke("execute")
        self.assertEqual(pending.returncode, 0, pending.stderr)
        self.assertEqual(json.loads(pending.stdout)["outcome"], "pending")
        request = json.loads((self.root / "request.json").read_text())
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

    def test_broker_conflict_does_not_use_an_existing_jobs_receipts(self):
        control = Control(None)
        path = "/v1/resources/r-101/jobs"
        control.request = Mock(side_effect=HTTPError(path, 409, "job already exists"))
        with self.assertRaises(HTTPError):
            control.submit("r-101", "trial-001", ["command"], self.root / "evidence")
        control.request.assert_called_once_with(path, {"name": "trial-001", "argv": ["command"]})


if __name__ == "__main__":
    unittest.main()
