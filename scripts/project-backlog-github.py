#!/usr/bin/python3 -IS
"""Standalone adapter for the native Project backlog GitHub subprocess protocol."""

import hashlib
import http.client
import json
import os
import re
import signal
import ssl
import sys
import time
from urllib.parse import parse_qs, urlencode, urlsplit

API = "https://api.github.com"
MARKER_PREFIX = "<!-- af-backlog-item:"
DIGEST = r"sha256:[0-9a-f]{64}"
SCOPE = ("project_id", "destination_id", "repository", "repository_id", "publisher_id")
WIRE_LIMIT = 65536  # Includes JSON escaping of the bounded public fields.
HTTP_LIMIT = 2 * 1024 * 1024
WALL_SECONDS = 24
PAGE_SIZE = 100
MAX_PAGES = 10
PAGE_FILTER = {"state": "all", "sort": "created", "direction": "asc", "per_page": str(PAGE_SIZE)}


class Refused(ValueError):
    """The request or a bounded observation could not be validated."""


class Conflict(Refused):
    """Provider observations contradict the authorized scope or issue identity."""


def need(condition):
    if not condition:
        raise Refused()


def fields(value, keys):
    need(isinstance(value, dict) and value.keys() == set(keys))


def numeric(value):
    need(type(value) is int and 0 < value <= (1 << 53) - 1)
    return value


def digest(value):
    need(isinstance(value, str) and re.fullmatch(DIGEST, value))


def decode(raw, limit):
    need(len(raw) <= limit)

    def unique(pairs):
        result = {}
        for key, value in pairs:
            need(key not in result)
            result[key] = value
        return result

    return json.loads(raw.decode("utf-8"), object_pairs_hook=unique,
                      parse_constant=lambda _: need(False))


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False, allow_nan=False).encode("utf-8")


def validate(request, scope):
    fields(scope, SCOPE)
    fields(request, (*SCOPE, "schema", "request_id", "operation", "item_id", "revision_id",
                     "marker", "issue", "fields"))
    need(request["schema"] == "af/backlog-github-request@1")
    for key in ("project_id", "destination_id", "item_id", "revision_id", "request_id"):
        digest(request[key])
    for key in ("repository_id", "publisher_id"):
        numeric(request[key])
        numeric(scope[key])
    need(all(request[key] == scope[key] for key in SCOPE))
    repo = scope["repository"]
    need(isinstance(repo, str) and len(repo) <= 141 and re.fullmatch(r"[a-z0-9_.-]+/[a-z0-9_.-]+", repo)
         and all(part not in (".", "..") for part in repo.split("/")))
    need(request["marker"] == f"{MARKER_PREFIX}{scope['project_id']}:{request['item_id']} -->")
    issue, public = request["issue"], request["fields"]
    if request["operation"] == "create":
        need(issue is None)
        fields(public, ("title", "body"))
        for key, limit, allowed in (("title", 256, ""), ("body", 8192, "\r\n\t")):
            value = public[key]
            need(isinstance(value, str) and value.strip() and len(value.encode("utf-8")) <= limit
                 and MARKER_PREFIX not in value
                 and all(not (ord(char) < 32 or 127 <= ord(char) <= 159)
                         or char in allowed for char in value))
    else:
        need(request["operation"] == "reconcile" and public is None)
        if issue is not None:
            fields(issue, ("id", "number"))
            numeric(issue["id"])
            numeric(issue["number"])


class GitHub:
    def __init__(self, token):
        need(re.fullmatch(r"[A-Za-z0-9_-]{1,1024}", token))
        self.token = token
        self.context = ssl.create_default_context()
        self.deadline = time.monotonic() + WALL_SECONDS

    def request(self, path, body=None):
        remaining = self.deadline - time.monotonic()
        need(remaining > 0)
        conn = http.client.HTTPSConnection("api.github.com", timeout=min(5, remaining),
                                           context=self.context)
        try:
            conn.request("GET" if body is None else "POST", path,
                         body=None if body is None else canonical(body), headers={
                             "Authorization": "Bearer " + self.token,
                             "Accept": "application/vnd.github+json",
                             "X-GitHub-Api-Version": "2022-11-28",
                             "Content-Type": "application/json",
                             "User-Agent": "af-project-backlog-v1",
                         })
            response = conn.getresponse()
            need(response.status == (200 if body is None else 201))
            need(response.getheader("Content-Encoding") in (None, "identity"))
            size = response.getheader("Content-Length")
            need(size is None or (size.isascii() and size.isdecimal()
                                  and len(size) <= 8 and int(size) <= HTTP_LIMIT))
            link = response.getheader("Link") or ""
            need(len(link) <= 8192)
            raw = response.read(HTTP_LIMIT + 1)
            need(size is None or len(raw) == int(size))
            return decode(raw, HTTP_LIMIT), link
        finally:
            conn.close()


def pagination(link, path, repository_id, page):
    # Validate GitHub's named and numeric repository routes. Generate our own next URL.
    relations = {}
    for entry in link.split(",") if link else ():
        match = re.fullmatch(r'\s*<([^>]+)>;\s*rel="(next|prev|first|last)"\s*', entry)
        need(match is not None and match[2] not in relations)
        url = urlsplit(match[1])
        need(url.scheme == "https" and url.netloc == "api.github.com" and not url.fragment
             and url.path in (path, f"/repositories/{repository_id}/issues"))
        query = parse_qs(url.query, keep_blank_values=True, strict_parsing=True, max_num_fields=5)
        number = query.pop("page", [])
        need(query == {key: [value] for key, value in PAGE_FILTER.items()}
             and len(number) == 1 and re.fullmatch(r"[1-9][0-9]{0,8}", number[0]))
        relations[match[2]] = int(number[0])
    need(relations.get("next", page + 1) == page + 1
         and relations.get("prev", page - 1) == page - 1 and relations.get("first", 1) == 1)
    return "next" in relations, relations.get("last")


def identity(row, path):
    need(isinstance(row, dict))
    issue = {"id": numeric(row.get("id")), "number": numeric(row.get("number"))}
    if (row.get("url") != f"{API}{path}/{issue['number']}"
            or row.get("repository_url") != API + path.removesuffix("/issues")):
        raise Conflict()
    return issue


def observation(row, issue, request, path):
    if identity(row, path) != issue:
        raise Conflict()
    user, body, title = row.get("user"), row.get("body"), row.get("title")
    marker = request["marker"]
    if ("pull_request" in row or not isinstance(user, dict)
            or numeric(user.get("id")) != request["publisher_id"]
            or not isinstance(body, str) or body.count(MARKER_PREFIX) != 1
            or marker not in body.split("\n")):
        raise Conflict()
    state = row.get("state")
    need(state in ("open", "closed") and isinstance(title, str))
    suffix = "\n\n" + marker
    public_body = body[:-len(suffix)] if body.endswith(suffix) else body.replace(marker, "", 1)
    fields_id = "sha256:" + hashlib.sha256(
        b"review.kernel/content-id/v1\0" + canonical([title, public_body])
    ).hexdigest()
    return {"kind": "observed", "issue": issue, "marker": marker, "author_id": user["id"],
            "state": state, "fields_id": fields_id}


def find_issue(service, request, path):
    ids, numbers, match = set(), set(), None
    more, last, verify_end = False, None, False
    for page in range(1, MAX_PAGES + 1):
        query = urlencode({**PAGE_FILTER, "page": page})
        rows, link = service.request(path + "?" + query)
        need(isinstance(rows, list) and len(rows) <= PAGE_SIZE and (rows or not more))
        more, advertised = pagination(link, path, request["repository_id"], page)
        need(not verify_end or (not rows and not more and advertised is None))
        if last is None:
            last = advertised
        need(advertised is None or advertised == last)
        need(not more or len(rows) == PAGE_SIZE)
        need(last is None or (page <= last <= MAX_PAGES and more == (page < last)))
        for row in rows:
            issue = identity(row, path)
            ident, number = issue["id"], issue["number"]
            need(ident not in ids and number not in numbers)
            ids.add(ident)
            numbers.add(number)
            body = row.get("body")
            need("body" in row and (body is None or isinstance(body, str)))
            if body and request["marker"] in body:
                if match is not None:
                    raise Conflict()
                observation(row, issue, request, path)
                match = issue
        if not more:
            verify_end = last is None and len(rows) == PAGE_SIZE
            if not verify_end:
                return match
    raise Refused()


def authenticate(service, request):
    actor, _ = service.request("/user")
    need(isinstance(actor, dict))
    if numeric(actor.get("id")) != request["publisher_id"]:
        raise Conflict()
    repo, _ = service.request("/repos/" + request["repository"])
    need(isinstance(repo, dict))
    name = repo.get("full_name")
    if (numeric(repo.get("id")) != request["repository_id"] or not isinstance(name, str)
            or not name.isascii() or name.lower() != request["repository"]
            or repo.get("url") != f"{API}/repos/{name}"):
        raise Conflict()
    return f"/repos/{name}/issues"


def dispatch(service, request):
    try:
        path = authenticate(service, request)
        issue = request["issue"] or find_issue(service, request, path)
        if issue is not None:
            row, _ = service.request(f"{path}/{issue['number']}")
            return observation(row, issue, request, path)
        if request["operation"] == "reconcile":
            return {"kind": "absent"}
        if authenticate(service, request) != path:
            raise Conflict()
        public = request["fields"]
        row, _ = service.request(path, {"title": public["title"],
                                        "body": public["body"] + "\n\n" + request["marker"]})
        return observation(row, identity(row, path), request, path)
    except Conflict:
        return {"kind": "conflict"}
    except (ValueError, OSError, http.client.HTTPException, RecursionError):
        return {"kind": "unavailable"}


def deadline_expired(_signal, _frame):
    raise TimeoutError()


def main():
    token = os.environ.get("AF_BACKLOG_GITHUB_TOKEN", "")
    scope_json = os.environ.get("AF_BACKLOG_GITHUB_SCOPE", "")
    os.environ.clear()
    need(sys.flags.isolated and sys.flags.no_site and len(sys.argv) == 1)
    signal.signal(signal.SIGALRM, deadline_expired)
    signal.alarm(WALL_SECONDS)
    try:
        request = decode(sys.stdin.buffer.read(WIRE_LIMIT + 1), WIRE_LIMIT)
        scope = decode(scope_json.encode("utf-8"), 4096)
        validate(request, scope)
        outcome = dispatch(GitHub(token), request)
        response = {"schema": "af/backlog-github-response@1", "request_id": request["request_id"],
                    **{key: scope[key] for key in ("repository", "repository_id", "publisher_id")},
                    "outcome": outcome}
        sys.stdout.buffer.write(canonical(response) + b"\n")
        sys.stdout.buffer.flush()
    finally:
        signal.alarm(0)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, RecursionError):
        sys.exit(1)
