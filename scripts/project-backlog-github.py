#!/usr/bin/env python3
"""GitHub Issues destination for an already committed Project backlog write intent."""
import importlib.util
from pathlib import Path
from urllib.parse import parse_qs, urlencode, urlsplit


def load_sibling(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


github = load_sibling("scenario-github-adapter")
listing = load_sibling("backlog-open-issues")
MARKER_PREFIX = "<!-- af-backlog-issue/v1 "
RECEIPT_SCHEMA = "af/backlog-github-issue-receipt@1"
PAGE_FILTER = {"state": "all", "sort": "created", "direction": "asc"}
PAGE_SCOPE = {**{key: [value] for key, value in PAGE_FILTER.items()},
              "per_page": [str(listing.PAGE_SIZE)]}


class Conflict(github.Refused):
    """Observed evidence contradicts the committed intent."""


class Incomplete(github.Refused):
    """A bounded observation cannot establish the outcome."""


class IssueTransport(github.GitHub):
    """Explicitly constructed by the authorized caller with its private token file."""

    def __init__(self, token_file):
        super().__init__({"token_file": token_file})
        self.reader = listing.HttpsTransport(self.context)

    def issue_page(self, path):
        try:
            rows, headers = listing.get(self.reader, path, self.token, self.deadline)
        except listing.Refused:
            raise Incomplete("GitHub issue page unavailable") from None
        return rows, headers.get("link", "")


def actor(value):
    github.need(isinstance(value, dict), "missing publisher identity")
    return {"id": github.integer(value.get("id")),
            "login": github.string(value.get("login"), github.LOGIN, limit=44)}


def validate(intent, authority):
    github.fields(intent, ("intent_id", "destination", "repository_id", "publisher", "item_id",
                           "revision_id", "title", "body"))
    github.fields(authority, ("destination", "repository_id", "publisher"))
    for key in ("intent_id", "item_id", "revision_id"):
        github.string(intent[key], github.DIGEST, limit=71)
    for key in ("title", "body"):
        value = github.string(intent[key], limit=256 if key == "title" else 8192)
        allowed = "\r\n\t" if key == "body" else ""
        github.need(value.strip() and MARKER_PREFIX not in value
                    and all(not (ord(char) < 32 or 127 <= ord(char) <= 159)
                            or char in allowed for char in value), "invalid approved issue text")
    for value in (authority, intent):
        github.string(value["destination"], "github:" + github.REPO, limit=147)
        github.integer(value["repository_id"])
        github.fields(value["publisher"], ("id", "login"))
        actor(value["publisher"])
    github.need(all(intent[key] == authority[key] for key in authority),
                "issue intent differs from local destination authority")


def authenticate(service, intent, repo):
    if actor(service.request("GET", "/user")) != intent["publisher"]:
        raise Conflict("credential publisher is unauthorized")
    row = service.request("GET", repo)
    if (not isinstance(row, dict) or row.get("id") != intent["repository_id"]
            or type(row.get("id")) is not int
            or row.get("full_name") != intent["destination"][len("github:"):]
            or row.get("url") != listing.API_URL + repo):
        raise Conflict("repository identity changed")


def issue_identity(row, path):
    github.need(isinstance(row, dict), "invalid issue row")
    ident, number = github.integer(row.get("id")), github.integer(row.get("number"))
    api = listing.API_URL + path
    github.need(row.get("url") == f"{api}/{number}"
                and row.get("repository_url") == api.removesuffix("/issues"),
                "issue row belongs to another destination")
    return ident, number


def pagination(link, path, repository_id, page):
    try:
        more, last = listing.pagination(link, path, repository_id, page)
        for entry in link.split(",") if link else ():
            target = entry.split("<", 1)[1].split(">", 1)[0]
            query = parse_qs(urlsplit(target).query, keep_blank_values=True, strict_parsing=True)
            del query["page"]  # The shared parser checked the exact page relation.
            github.need(query == PAGE_SCOPE, "GitHub pagination scope changed")
        return more, last
    except (listing.Refused, github.Refused, ValueError):
        raise Incomplete("GitHub issue pagination inconclusive") from None


def observe(service, intent, path, marker_scope, receipt):
    ids, numbers, matches = set(), set(), []
    more, last, verify_end = False, None, False
    for page in range(1, github.MAX_PAGES + 1):
        query = urlencode({**PAGE_FILTER, "per_page": listing.PAGE_SIZE, "page": page})
        rows, link = service.issue_page(path + "?" + query)
        if not isinstance(rows, list) or len(rows) > listing.PAGE_SIZE or (more and not rows):
            raise Incomplete("GitHub issue page incomplete")
        more, advertised = pagination(link, path, intent["repository_id"], page)
        if verify_end and (rows or more or advertised is not None):
            raise Incomplete("GitHub issue listing omitted a continuation")
        if last is None:
            last = advertised
        if ((advertised is not None and advertised != last)
                or (more and len(rows) != listing.PAGE_SIZE)
                or (last is not None and not (page <= last <= github.MAX_PAGES
                                              and more == (page < last)))):
            raise Incomplete("GitHub issue listing changed or exceeds its bound")
        for row in rows:
            ident, number = issue_identity(row, path)
            if ident in ids or number in numbers:
                raise Incomplete("GitHub issue listing repeated an identity")
            ids.add(ident)
            numbers.add(number)
            body = row.get("body")
            github.need("body" in row and (body is None or isinstance(body, str)), "invalid issue body")
            if body and marker_scope in body:
                receipt(row)
                matches.append((ident, number))
        if not more:
            verify_end = last is None and len(rows) == listing.PAGE_SIZE
            if not verify_end:
                break
    else:
        raise Incomplete("GitHub issue pagination bound exhausted")
    if len(matches) > 1:
        raise Conflict("multiple issues contain the publication marker")
    if not matches:
        return None
    ident, number = matches[0]
    current = service.request("GET", f"{path}/{number}")
    if issue_identity(current, path) != (ident, number):
        raise Conflict("issue identity changed between listing and detail")
    text = current.get("body")
    if not isinstance(text, str) or marker_scope not in text:
        raise Incomplete("publication marker no longer visible on the issue")
    return receipt(current)


def publish_issue(intent, authority, service, *, commit_send=None):
    """Reconcile by default. A fresh send requires an exclusive durable commit callback."""
    try:
        validate(intent, authority)
    except (github.Refused, UnicodeError):
        return {"status": "refused", "reason": "invalid or unauthorized issue write intent"}

    # Capture the exact authorized request before calling any external code.
    intent = {**intent, "publisher": dict(intent["publisher"])}
    repo = "/repos/" + intent["destination"][len("github:"):]
    path = repo + "/issues"
    public = {"title": intent["title"], "body": intent["body"]}
    scope = f"{MARKER_PREFIX}{intent['item_id']} {intent['revision_id']}"
    marker = scope + " " + github.digest(github.canonical(public)) + " -->"
    body = public["body"] + "\n\n" + marker

    def receipt(row):
        ident, number = issue_identity(row, path)
        text = row.get("body")
        try:
            same_publisher = actor(row.get("user")) == intent["publisher"]
        except github.Refused:
            same_publisher = False
        if ("pull_request" in row or not same_publisher
                or row.get("html_url") != f"https://github.com/{intent['destination'][7:]}/issues/{number}"
                or not isinstance(text, str) or text.count(MARKER_PREFIX) != 1
                or marker not in text.split("\n") or row.get("state") not in ("open", "closed")):
            raise Conflict("issue marker, publisher, destination or state conflicts")
        return {**{key: intent[key] for key in ("intent_id", "item_id", "revision_id", "destination",
                                                "repository_id", "publisher")},
                "schema": RECEIPT_SCHEMA, "issue_id": ident, "issue_number": number,
                "state": row["state"], "content_matches": row.get("title") == public["title"] and text == body}

    claimed = False

    def before_send():
        nonlocal claimed
        # An exception during the commit can leave a durable claim. Never assume it rolled back.
        if claimed:
            raise Incomplete("transport repeated the send guard")
        claimed = True
        try:
            committed = commit_send(intent["intent_id"])
        except Exception:
            raise Incomplete("durable send claim is inconclusive") from None
        if committed is not True:
            raise Incomplete("send claim was not acquired")

    def result(status, reason):
        return {"status": status, "reason": reason, "intent_id": intent["intent_id"]}

    try:
        authenticate(service, intent, repo)
        found = observe(service, intent, path, scope, receipt)
        if found is not None:
            return {"status": "done", "receipt": found}
        if commit_send is None:
            return result("pending", "issue marker is not yet visible")
        authenticate(service, intent, repo)
        row = service.request("POST", path, {"title": public["title"], "body": body},
                              before_send=before_send)
        if not claimed:
            raise Incomplete("transport omitted the durable send guard")
        return {"status": "done", "receipt": receipt(row)}
    except Conflict as error:
        if claimed:
            return result("pending", "create response could not establish an issue receipt")
        return result("conflict", str(error))
    except Incomplete as error:
        return result("pending", str(error))
    except (github.TransportUnavailable, OSError):
        if not claimed and commit_send is not None:
            return result("retry", "send guard was not reached")
        return result("pending", "issue outcome remains inconclusive")
    except (github.Refused, UnicodeError):
        if claimed or commit_send is None:
            return result("pending", "issue outcome remains inconclusive")
        return result("refused", "issue request was refused before dispatch")
