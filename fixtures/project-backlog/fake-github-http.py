"""Offline HTTPSConnection fixture inserted before the real adapter entrypoint."""

import signal
from pathlib import Path

fixture_state = Path(__AF_BACKLOG_FIXTURE_STATE__)
fixture_repo = "/repos/fixture/backlog"
fixture_issues = fixture_repo + "/issues"


class FixtureResponse:
    def __init__(self, status, payload):
        self.status = status
        self.payload = json.dumps(payload).encode()

    def getheader(self, name):
        return str(len(self.payload)) if name == "Content-Length" else None

    def read(self, size):
        return self.payload[:size]


class FixtureConnection:
    def __init__(self, host, *, timeout, context):
        assert host == "api.github.com" and 0 < timeout <= 5
        assert not os.environ

    def request(self, method, path, body, headers):
        assert headers["Authorization"] == "Bearer fixture_token"
        assert method in ("GET", "POST")
        data = json.loads(body) if body is not None else None
        if body is not None:
            for private_text in (b"private-attachment", b"Different behavior", b"private-insufficient-repro"):
                assert private_text not in body
        with (fixture_state / "requests").open("a") as trace:
            trace.write(json.dumps({"method": method, "path": path, "body": data}) + "\n")

        issue_path = fixture_state / "issue.json"
        mode_file = fixture_state / "mode"
        mode = mode_file.read_text() if mode_file.exists() else "normal"
        if method == "POST":
            assert path == fixture_issues
            if (fixture_state / "abort-before-write").exists():
                os._exit(18)
            assert set(data) == {"title", "body"}
            assert data["title"] == "Blue excludes its required boundary."
            approved, marker = data["body"].split("\n\n")
            assert approved == "The configured boundary must be inclusive."
            assert re.fullmatch(f"{MARKER_PREFIX}{DIGEST}:{DIGEST} -->", marker)
            issue = {**data, "id": 444, "number": 1, "state": "open", "user": {"id": 42},
                     "url": API + fixture_issues + "/1", "repository_url": API + fixture_repo}
            with issue_path.open("x") as stored:
                json.dump(issue, stored)
                stored.flush()
                os.fsync(stored.fileno())
            if mode == "lost_reply":
                raise http.client.RemoteDisconnected()
            if not (fixture_state / "respond").exists():
                os.kill(os.getpid(), signal.SIGKILL)
            self.response = FixtureResponse(201, issue)
            return

        assert body is None
        if path == "/user":
            result = {"id": 42}
        elif path == fixture_repo:
            result = {"id": 24 if mode == "wrong_repository" else 23,
                      "full_name": "fixture/backlog", "url": API + fixture_repo}
        elif path == fixture_issues + "/1" or path.startswith(fixture_issues + "?"):
            issue = json.loads(issue_path.read_text()) if issue_path.exists() else None
            if issue is not None and mode == "conflict":
                issue["user"]["id"] = 43
            if issue is not None and mode == "wrong_marker":
                issue["body"] += "\n<!-- af-backlog-item:other -->"
            if issue is not None and mode == "removed_marker":
                issue["body"] = "Operator replaced the issue body"
            if issue is not None and mode == "wrong_issue_id":
                issue["id"] = 445
            if path.endswith("/1"):
                if issue is None or mode == "missing":
                    self.response = FixtureResponse(404, {})
                    return
                if mode == "transferred":
                    issue["repository_url"] = API + "/repos/other/backlog"
                result = issue
            else:
                assert path == fixture_issues + "?" + urlencode({**PAGE_FILTER, "page": 1})
                result = [] if issue is None or mode == "delayed" else [issue]
                if mode == "duplicate":
                    result.append({**issue, "id": 445, "number": 2, "url": API + fixture_issues + "/2"})
        else:
            raise AssertionError("unexpected GitHub route")
        self.response = FixtureResponse(200, result)

    def getresponse(self):
        return self.response

    def close(self):
        pass


http.client.HTTPSConnection = FixtureConnection
