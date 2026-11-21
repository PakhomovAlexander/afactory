# Inserted before the standalone adapter's entrypoint in the hermetic CLI regression.
from pathlib import Path
from urllib.parse import parse_qs

STATE = Path(__AF_BACKLOG_FIXTURE_STATE__)


class Jira:
    def __init__(self, site, credential):
        assert site == "fixture.atlassian.net"
        assert credential == {"email": "fixture@example.invalid", "api_token": "fixture_token"}

    def request(self, method, path, body=None):
        assert method in ("GET", "POST")
        with (STATE / "requests").open("a", encoding="utf-8") as log:
            log.write(json.dumps({"method": method, "path": path, "body": body}) + "\n")
        issue = STATE / "issue.json"
        mode_file = STATE / "mode"
        mode = mode_file.read_text() if mode_file.exists() else "initial"
        if path == "/myself":
            return {"accountId": "wrong" if mode == "wrong-publisher" else "fixture:publisher"}
        if path == "/project/10001":
            return {"id": "10001"}
        if path.startswith("/search/jql?"):
            assert method == "GET" and body is None
            query = parse_qs(path.split("?", 1)[1])
            assert query["jql"][0].startswith('project = 10001 AND labels = "af-backlog-')
            hits = [{"id": "10042"}] if issue.exists() and mode != "delayed" else []
            return {"issues": hits, "isLast": True}
        if method == "POST" and path == "/issue":
            assert not issue.exists()
            fields = body["fields"]
            assert fields["project"] == {"id": "10001"}
            assert fields["issuetype"] == {"id": "10002"}
            assert len(fields["labels"]) == 1
            fields["creator"] = {"accountId": "fixture:publisher"}
            fields["status"] = {"statusCategory": {"key": "new"}}
            issue.write_text(json.dumps({"id": "10042", "key": "AF-1", "fields": fields}))
            os._exit(1)
        assert method == "GET" and path.startswith("/issue/10042?")
        row = json.loads(issue.read_text())
        if mode == "wrong-type":
            row["fields"]["issuetype"]["id"] = "10003"
        return row
