#!/usr/bin/python3 -B
"""Hermetic wire fixture. The integration test supplies a private state directory."""

import hashlib
import json
import os
from pathlib import Path
import sys

state = Path(__AF_BACKLOG_FIXTURE_STATE__)
raw = sys.stdin.read()
request = json.loads(raw)
assert request["schema"] == "af/backlog-github-request@1"
assert request["repository"] == "fixture/backlog"
assert request["repository_id"] == 23
assert request["publisher_id"] == 42
assert os.environ["AF_BACKLOG_GITHUB_TOKEN"] == "fixture_token"
assert "AF_PROVIDERS_FILE" not in os.environ
assert "private-attachment" not in raw
assert "Different behavior" not in raw
assert request["marker"] == (
    f"<!-- af-backlog-item:{request['project_id']}:{request['item_id']} -->"
)

with (state / "requests").open("a") as trace:
    trace.write(json.dumps(request) + "\n")
issue_path = state / "issue.json"
if request["operation"] == "create":
    assert request["issue"] is None
    if (state / "abort-before-write").exists():
        os._exit(18)
    fields = request["fields"]
    assert fields["title"] == "Blue excludes its required boundary."
    assert fields["body"] == "The configured boundary must be inclusive."
    encoded = json.dumps([fields["title"], fields["body"]], separators=(",", ":")).encode()
    observed = {
        "kind": "observed",
        "issue": {"id": 444, "number": 1},
        "marker": request["marker"],
        "author_id": 42,
        "state": "open",
        "fields_id": "sha256:" + hashlib.sha256(
            b"review.kernel/content-id/v1\0" + encoded
        ).hexdigest(),
    }
    with issue_path.open("x") as issue:
        json.dump(observed, issue)
        issue.flush()
        os.fsync(issue.fileno())
    if not (state / "respond").exists():
        os._exit(17)

assert request["operation"] in ("create", "reconcile")
mode = (state / "mode").read_text()
if request["operation"] == "reconcile":
    assert request["fields"] is None
    assert request["issue"] in (None, {"id": 444, "number": 1})
observed = {"kind": "absent"} if mode == "delayed" else json.loads(issue_path.read_text())
if mode == "conflict":
    observed["author_id"] = 43
elif mode == "wrong_marker":
    observed["marker"] = "<!-- af-backlog-item:other -->"
elif mode == "wrong_repository":
    request["repository_id"] = 24
print(json.dumps({
    "schema": "af/backlog-github-response@1",
    **{key: request[key] for key in ("request_id", "repository", "repository_id", "publisher_id")},
    "outcome": observed,
}))
