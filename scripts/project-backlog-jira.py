#!/usr/bin/python3 -IS
"""Narrow, receiver-owned Jira Cloud transport for verified AF backlog items."""

import base64
import hashlib
import http.client
import json
import os
import re
import signal
import ssl
import sys
import time
from urllib.parse import urlencode

DIGEST = r"sha256:[0-9a-f]{64}"
NUMBER = r"[1-9][0-9]{0,19}"
SITE = r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.atlassian\.net"
ACCOUNT = r"[a-zA-Z0-9:_-]{1,128}"
ISSUE_KEY = r"[A-Z][A-Z0-9_]{0,31}-" + NUMBER
FIELDS = "project,issuetype,creator,status,summary,description,labels"
WALL_SECONDS = 24
HTTP_LIMIT = 2 * 1024 * 1024
WIRE_LIMIT = 65536


class Conflict(ValueError):
    """The observation contradicts the intended identity or scope."""


def require(condition):
    if not condition:
        raise ValueError("invalid Jira backlog exchange")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
                      allow_nan=False).encode("utf-8")


def strict_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result)
            result[key] = value
        return result

    return json.loads(raw, object_pairs_hook=unique, parse_constant=lambda _: require(False))


def matches(pattern, value):
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def exact(value, keys):
    return isinstance(value, dict) and value.keys() == set(keys.split())


def validate(request, scope):
    require(exact(scope, "schema project_id site jira_project_id issue_type_id "
                         "publisher_account_id executable_digest"))
    require(scope["schema"] == "af/backlog-jira-destination@1")
    for key, pattern in (("project_id", DIGEST), ("site", SITE),
                         ("jira_project_id", NUMBER), ("issue_type_id", NUMBER),
                         ("publisher_account_id", ACCOUNT), ("executable_digest", DIGEST)):
        require(matches(pattern, scope[key]))
    require(exact(request, "schema request_id scope operation item_id issue_id fields"))
    require(request["schema"] == "af/backlog-jira-request@1" and request["scope"] == scope)
    require(matches(DIGEST, request["request_id"]) and matches(DIGEST, request["item_id"]))
    issue, fields = request["issue_id"], request["fields"]
    if request["operation"] == "create":
        require(issue is None and exact(fields, "title body"))
        for key, limit, multiline in (("title", 256, ""), ("body", 8192, "\n\r\t")):
            value = fields[key]
            require(isinstance(value, str) and value.strip() and len(value.encode("utf-8")) <= limit
                    and all(not (ord(char) < 32 or 127 <= ord(char) <= 159)
                            or char in multiline for char in value))
    else:
        require(request["operation"] == "reconcile" and fields is None
                and (issue is None or matches(NUMBER, issue)))


class Jira:
    def __init__(self, site, credential):
        require(exact(credential, "email api_token"))
        email, token = credential["email"], credential["api_token"]
        require(matches(r"[!-9;-~]{1,320}", email) and "@" in email
                and matches(r"[!-~]{1,2048}", token))
        self.site = site
        self.authorization = "Basic " + base64.b64encode(f"{email}:{token}".encode()).decode()
        self.context = ssl.create_default_context()
        self.deadline = time.monotonic() + WALL_SECONDS

    def request(self, method, path, body=None):
        left = self.deadline - time.monotonic()
        require(left > 0)
        conn = http.client.HTTPSConnection(self.site, timeout=min(5, left), context=self.context)
        try:
            conn.request(method, "/rest/api/3" + path,
                         body=None if body is None else canonical(body), headers={
                             "Authorization": self.authorization,
                             "Accept": "application/json",
                             "Content-Type": "application/json",
                         })
            response = conn.getresponse()
            require(response.status == (201 if path == "/issue" else 200))
            require(response.getheader("Content-Encoding") in (None, "identity"))
            data = response.read(HTTP_LIMIT + 1)
            require(len(data) <= HTTP_LIMIT)
            value = strict_json(data.decode("utf-8"))
            require(isinstance(value, dict))
            return value
        finally:
            conn.close()


def paragraph(line):
    return {"type": "paragraph", "content": [{"type": "text", "text": line}] if line else []}


def description(body):
    return {"version": 1, "type": "doc", "content": [paragraph(line) for line in body.split("\n")]}


def observed_body(adf):
    """Reverse AF's lossless mapping, retaining other ADF for edit hashing."""
    if exact(adf, "version type content") and adf["version"] == 1 and adf["type"] == "doc":
        if isinstance(adf["content"], list):
            lines = []
            for node in adf["content"]:
                if not isinstance(node, dict) or node.get("type") != "paragraph":
                    break
                content = node.get("content", [])
                if not content:
                    lines.append("")
                elif (isinstance(content, list) and len(content) == 1
                      and exact(content[0], "type text") and content[0]["type"] == "text"
                      and isinstance(content[0]["text"], str)):
                    lines.append(content[0]["text"])
                else:
                    break
            else:
                return "\n".join(lines)
    return adf


def observed(row, scope, marker, issue):
    fields = row.get("fields")
    require(isinstance(fields, dict))
    if (row.get("id") != issue or not matches(ISSUE_KEY, row.get("key"))
            or fields.get("project", {}).get("id") != scope["jira_project_id"]
            or fields.get("issuetype", {}).get("id") != scope["issue_type_id"]
            or fields.get("creator", {}).get("accountId") != scope["publisher_account_id"]
            or not isinstance(fields.get("labels"), list) or fields["labels"].count(marker) != 1):
        raise Conflict()
    status = fields.get("status", {}).get("statusCategory", {}).get("key")
    require(status in ("new", "indeterminate", "done"))
    summary = fields["summary"]
    require(isinstance(summary, str))
    body = observed_body(fields["description"])
    fields_id = "sha256:" + hashlib.sha256(
        b"review.kernel/content-id/v1\0" + canonical([summary, body])
    ).hexdigest()
    return {"kind": "observed", "id": issue, "key": row["key"], "closed": status == "done",
            "fields_id": fields_id}


def authenticate(service, scope):
    if service.request("GET", "/myself").get("accountId") != scope["publisher_account_id"]:
        raise Conflict()
    if service.request("GET", "/project/" + scope["jira_project_id"]).get("id") != scope["jira_project_id"]:
        raise Conflict()


def find_issue(service, scope, marker):
    query = urlencode({"jql": f'project = {scope["jira_project_id"]} AND labels = "{marker}"',
                       "fields": "labels", "maxResults": 2})
    result = service.request("GET", "/search/jql?" + query)
    rows = result["issues"]
    require(isinstance(rows, list))
    if len(rows) > 1:
        raise Conflict()
    require(result.get("isLast") is True and not result.get("nextPageToken"))
    if not rows:
        return None
    issue = rows[0]["id"]
    require(matches(NUMBER, issue))
    return issue


def dispatch(service, request):
    scope = request["scope"]
    marker = f'af-backlog-{scope["project_id"][7:]}-{request["item_id"][7:]}'
    try:
        authenticate(service, scope)
        issue = request["issue_id"] or find_issue(service, scope, marker)
        if issue is None:
            if request["operation"] == "reconcile":
                return {"kind": "absent"}
            authenticate(service, scope)
            public = request["fields"]
            created = service.request("POST", "/issue", {"fields": {
                "project": {"id": scope["jira_project_id"]},
                "issuetype": {"id": scope["issue_type_id"]},
                "summary": public["title"], "description": description(public["body"]),
                "labels": [marker],
            }})
            issue = created.get("id")
            require(matches(NUMBER, issue))
        row = service.request("GET", f"/issue/{issue}?" + urlencode({"fields": FIELDS}))
        return observed(row, scope, marker, issue)
    except Conflict:
        return {"kind": "conflict"}
    except (ValueError, OSError, http.client.HTTPException, KeyError, TypeError, AttributeError,
            RecursionError):
        return {"kind": "unavailable"}


def deadline_expired(_signal, _frame):
    raise TimeoutError()


def main():
    scope_json = os.environ.get("AF_BACKLOG_JIRA_SCOPE", "")
    credential_json = os.environ.get("AF_BACKLOG_JIRA_CREDENTIAL", "")
    os.environ.clear()
    require(sys.flags.isolated and sys.flags.no_site and len(sys.argv) == 1)
    signal.signal(signal.SIGALRM, deadline_expired)
    signal.alarm(WALL_SECONDS)
    try:
        raw = sys.stdin.buffer.read(WIRE_LIMIT + 1)
        require(len(raw) <= WIRE_LIMIT)
        request = strict_json(raw.decode("utf-8"))
        scope = strict_json(scope_json)
        validate(request, scope)
        outcome = dispatch(Jira(scope["site"], strict_json(credential_json)), request)
        print(json.dumps({"schema": "af/backlog-jira-response@1",
                          "request_id": request["request_id"], "outcome": outcome}))
    except (ValueError, OSError, RecursionError):
        return 1
    finally:
        signal.alarm(0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
