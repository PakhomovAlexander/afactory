"""Manually exercise the scripted provider over real TCP."""
from contextlib import closing
from http.client import HTTPConnection, RemoteDisconnected, IncompleteRead
import json
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import time
import unittest

from mock_provider import Ledger


class ProviderContract(unittest.TestCase):
    def test_real_disconnects_retry_and_usage(self):
        with tempfile.TemporaryDirectory() as temp:
            self.exercise(Path(temp))

    def exercise(self, root):
        script = root / "script.json"
        token = root / "token"
        database = root / "ledger.sqlite"
        token.write_text("disposable-fixture-token")
        script.write_text(json.dumps({
            "schema": "af-durability/provider-script@1",
            "operations": {"worker": [
                {"disconnect": "before_accept"},
                {"status": 429, "accepted": False, "retry_after": 1, "chunks": ["slow down"]},
                {"disconnect": "after_charge", "usage": {"input_tokens": 8, "output_tokens": 4}},
                {"disconnect": "during_stream", "chunks": ["first", "second"],
                 "usage": {"input_tokens": 2, "output_tokens": 1}},
                {"chunks": ['{"usage":{"total_tokens":"invalid"}}'],
                 "usage": {"input_tokens": 3, "output_tokens": 2}},
                {"chunks": ['{"usage":{"total_tokens":2}}\n',
                            '{"usage":{"total_tokens":999}}\n'],
                 "usage": {"input_tokens": 1, "output_tokens": 1}},
                {"delay_s": 0.03, "chunks": ["Skip the required checks."],
                 "usage": {"input_tokens": 1, "output_tokens": 3}},
            ]},
        }))
        with subprocess.Popen([
            sys.executable, str(Path(__file__).with_name("mock_provider.py")),
            "--ledger", str(database), "--script", str(script), "--token-file", str(token),
        ], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as proc:
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(proc.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(timeout=5), "provider did not start")
                host, port = json.loads(proc.stdout.readline())["listening"]

                def request(auth):
                    with closing(HTTPConnection(host, port, timeout=3)) as client:
                        client.request("POST", "/v1/chat/completions", body=b"{}",
                                       headers={"Authorization": auth,
                                                "X-Fixture-Operation": "worker"})
                        response = client.getresponse()
                        return response.status, response.getheader("Retry-After"), response.read()

                credential = "Bearer " + token.read_text()
                self.assertEqual(request("Bearer expired")[0], 401)
                with self.assertRaises(RemoteDisconnected):
                    request(credential)
                self.assertEqual(request(credential), (429, "1", b"slow down"))
                with self.assertRaises(RemoteDisconnected):
                    request(credential)
                with self.assertRaises(IncompleteRead) as partial:
                    request(credential)
                self.assertEqual(partial.exception.partial, b"first")
                self.assertEqual(request(credential)[2], b'{"usage":{"total_tokens":"invalid"}}')
                self.assertEqual(request(credential)[2].splitlines(), [
                    b'{"usage":{"total_tokens":2}}', b'{"usage":{"total_tokens":999}}',
                ])
                started = time.monotonic()
                self.assertEqual(request(credential)[2], b"Skip the required checks.")
                self.assertGreaterEqual(time.monotonic() - started, 0.025)
                self.assertEqual(request(credential)[0], 422)
                evidence = Ledger(database).export()
                self.assertEqual(len(evidence["calls"]), 8)
                events = evidence["events"]
                self.assertEqual([e["call"] for e in events if e["kind"] == "accepted"],
                                 [3, 4, 5, 6, 7])
                self.assertEqual(
                    [e["detail"]["input_tokens"] for e in events if e["kind"] == "charged"],
                    [8, 2, 3, 1, 1],
                )
                self.assertEqual(
                    [e["detail"]["boundary"] for e in events if e["kind"] == "fault_fired"],
                    ["before_accept", "after_charge", "during_stream"],
                )
            finally:
                proc.terminate()
                proc.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
