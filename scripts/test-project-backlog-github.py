#!/usr/bin/env python3
"""Offline checks of the installed adapter's stdin and GitHub HTTP boundary."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
ADAPTER = ROOT / "scripts/project-backlog-github.py"
HTTP = ROOT / "fixtures/project-backlog/fake-github-http.py"


def digest(char):
    return "sha256:" + char * 64


class AdapterTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.state = Path(directory.name)
        code, entrypoint = ADAPTER.read_text().split("\nif __name__ == ", 1)
        fixture = HTTP.read_text().replace("__AF_BACKLOG_FIXTURE_STATE__", repr(directory.name))
        self.executable = self.state / "backlog-github-adapter"
        self.executable.write_text(f"{code}\n{fixture}\nif __name__ == {entrypoint}")
        self.executable.chmod(0o700)
        self.scope = {"project_id": digest("1"), "destination_id": digest("2"),
                      "repository": "fixture/backlog", "repository_id": 23, "publisher_id": 42}
        self.marker = f"<!-- af-backlog-item:{digest('1')}:{digest('3')} -->"
        self.request = {**self.scope, "schema": "af/backlog-github-request@1",
                        "request_id": digest("4"), "item_id": digest("3"), "revision_id": digest("5"),
                        "operation": "create", "issue": None, "marker": self.marker,
                        "fields": {"title": "Blue excludes its required boundary.",
                                   "body": "The configured boundary must be inclusive."}}

    def run_adapter(self, request=None, raw=None, token="fixture_token"):
        request = self.request if request is None else request
        return subprocess.run(
            [self.executable], input=json.dumps(request).encode() if raw is None else raw,
            capture_output=True, timeout=10,
            env={"AF_BACKLOG_GITHUB_TOKEN": token, "AF_BACKLOG_GITHUB_SCOPE": json.dumps(self.scope),
                 "PYTHONPATH": "/ignored", "AF_PROVIDERS_FILE": "/private"},
        )

    def outcome(self, request=None):
        request = self.request if request is None else request
        result = self.run_adapter(request)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, b"")
        response = json.loads(result.stdout)
        outcome = response.pop("outcome")
        self.assertEqual(response, {"schema": "af/backlog-github-response@1",
                                    **{key: request[key] for key in ("request_id", "repository",
                                                                      "repository_id", "publisher_id")}})
        return outcome

    def test_rejects_bad_or_unscoped_requests_without_http_calls(self):
        for change in (
            {"project_id": digest("9")}, {"destination_id": digest("9")},
            {"repository": "other/backlog"}, {"repository_id": 24}, {"publisher_id": 43},
            {"publisher_id": True}, {"marker": "unbound"}, {"request_id": "unbound"},
            {"revision_id": ""}, {"extra": "private"}, {"issue": {"id": 444, "number": 1}},
            {"operation": "reconcile"},
            {"fields": {"title": "A", "body": self.marker}},
            {"fields": {"title": "A", "body": "a" * 8193}},
        ):
            with self.subTest(change=change):
                result = self.run_adapter({**self.request, **change})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout + result.stderr, b"")
        for raw in (b'{"schema":0,"schema":1}', b" " * 65537, b"\xff"):
            self.assertNotEqual(self.run_adapter(raw=raw).returncode, 0)
        self.assertNotEqual(self.run_adapter(token="").returncode, 0)
        self.assertFalse((self.state / "requests").exists())

    def test_create_confirmed_fields_match_native_digest(self):
        (self.state / "respond").touch()
        outcome = self.outcome()
        public = self.request["fields"]
        encoded = json.dumps([public["title"], public["body"]], separators=(",", ":")).encode()
        self.assertEqual(outcome, {"kind": "observed", "issue": {"id": 444, "number": 1},
                                   "author_id": 42, "marker": self.marker, "state": "open",
                                   "fields_id": "sha256:" + hashlib.sha256(
                                       b"review.kernel/content-id/v1\0" + encoded).hexdigest()})

    def test_dropped_reply_and_mapped_revisions_use_reads_only(self):
        mode = self.state / "mode"
        mode.write_text("lost_reply")
        self.assertEqual(self.outcome(), {"kind": "unavailable"})
        issue_path = self.state / "issue.json"
        self.assertTrue(issue_path.exists())
        reconcile = {**self.request, "operation": "reconcile", "fields": None}
        mode.write_text("delayed")
        self.assertEqual(self.outcome(reconcile), {"kind": "absent"})
        mode.write_text("duplicate")
        self.assertEqual(self.outcome(reconcile), {"kind": "conflict"})
        mode.write_text("normal")
        self.assertEqual(self.outcome(reconcile)["issue"]["id"], 444)

        revision = {**reconcile, "revision_id": digest("6"), "request_id": digest("7"),
                    "issue": {"id": 444, "number": 1}}
        for fault, expected in (("wrong_marker", "conflict"), ("transferred", "conflict"),
                                ("conflict", "conflict"), ("removed_marker", "conflict"),
                                ("wrong_issue_id", "conflict"), ("missing", "unavailable")):
            mode.write_text(fault)
            self.assertEqual(self.outcome(revision), {"kind": expected})
        mode.write_text("normal")
        issue = json.loads(issue_path.read_text())
        issue.update(title="Human title", body="Human body\n\n" + self.marker, state="closed")
        issue_path.write_text(json.dumps(issue))
        self.assertEqual(self.outcome(revision)["state"], "closed")
        self.assertEqual(json.loads(issue_path.read_text()), issue)
        calls = [json.loads(line) for line in (self.state / "requests").read_text().splitlines()]
        self.assertEqual(sum(call["method"] == "POST" for call in calls), 1)


if __name__ == "__main__":
    unittest.main()
