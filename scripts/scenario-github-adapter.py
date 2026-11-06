#!/usr/bin/python3 -IS
"""Installed, digest-pinned GitHub Scenario adapter. Stdlib only; no runtime plugins.

Install with install-scenario-github-adapter.py. The source checkout is inert. This is a
trusted machine extension: af verifies signed decisions and pins this entire executable.
"""
import contextlib
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import ssl
import stat
import sys
import time
from urllib.parse import quote, urlencode, urlsplit

# Replaced only by the owner-side installer, before the executable digest is pinned in af.
CONFIG_PATH = None
CONFIG_DIGEST = None
LIMIT = 262144
HTTP_LIMIT = 2097152
STATE_LIMIT = 16777216
MAX_PAGES = 10
MAX_RECORDS = 4096
WALL_SECONDS = 24
REVIEW_MUTATION = ("mutation AfRequestReview($pull:ID!,$users:[ID!]!){"
                   "requestReviews(input:{pullRequestId:$pull,userIds:$users,union:true}){pullRequest{id}}}")
DIGEST = r"sha256:[0-9a-f]{64}"
SHA = r"[0-9a-f]{40}"
LOGIN = r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})(?:\[bot\])?"
REPO = r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_][A-Za-z0-9_.-]{0,99}"
PUB = r"github-pr:sha256:[0-9a-f]{64}"
CONFIG_SCHEMA = "af/github-adapter-config@1"
STATE_SCHEMA = "af/github-adapter-state@3"
PAYLOAD_SCHEMAS = {
    "publish_pr": "af/github-publish-pr@1",
    "followup": "af/github-followup@1",
    "notify": "af/github-notify@1",
}


class Refused(RuntimeError):
    """Safe bounded diagnostic; never holds remote response bytes or credentials."""


def need(ok, message):
    if not ok:
        raise Refused(message)


def fields(value, required, optional=()):
    need(isinstance(value, dict) and set(required) <= value.keys()
         and value.keys() <= set(required) | set(optional), "unexpected document fields")


def string(value, pattern=None, limit=65536):
    need(isinstance(value, str) and 0 < len(value.encode("utf-8")) <= limit
         and "\0" not in value, "invalid text")
    if pattern:
        need(re.fullmatch(pattern, value) is not None, "invalid identity")
    return value


def integer(value):
    need(type(value) is int and 0 < value <= 9007199254740991, "invalid numeric identity")
    return value


def canonical(value):
    # This adapter's ASCII-keyed payload contract, independent of af's opaque input_digest.
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
                      allow_nan=False).encode("utf-8")


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def decode(raw, limit=LIMIT):
    need(len(raw) <= limit, "document bound exceeded")

    def unique(pairs):
        result = {}
        for key, value in pairs:
            need(key not in result, "duplicate JSON key")
            result[key] = value
        return result

    try:
        return json.loads(raw, object_pairs_hook=unique,
                          parse_constant=lambda _: need(False, "nonfinite JSON number"))
    except (ValueError, UnicodeError, RecursionError) as exc:
        raise Refused("invalid JSON document") from exc


def outside_checkout(path):
    need(path.is_absolute() and path.resolve() == path, "path must be canonical absolute")
    for parent in [path, *path.parents]:
        need(not (parent / ".git").exists()
             and not ((parent / "HEAD").exists() and (parent / "objects").is_dir()
                      and (parent / "refs").is_dir()), "machine path is inside a Git checkout")


def private_path(path, directory=False):
    outside_checkout(path)
    meta = path.lstat()
    need(meta.st_uid == os.getuid() and stat.S_IMODE(meta.st_mode) == (0o700 if directory else 0o600)
         and (stat.S_ISDIR(meta.st_mode) if directory else stat.S_ISREG(meta.st_mode)),
         "machine path must be owner-only and have the expected type")
    # Writable shared ancestors are accepted only when the sticky bit protects entries (/tmp).
    for parent in path.parents:
        meta = parent.lstat()
        need(stat.S_ISDIR(meta.st_mode) and meta.st_uid in (0, os.getuid())
             and (not meta.st_mode & 0o022 or bool(meta.st_mode & stat.S_ISVTX)),
             "unsafe machine path ancestor")


def read_private(path, limit):
    private_path(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        meta = os.fstat(stream.fileno())
        need(stat.S_ISREG(meta.st_mode) and meta.st_uid == os.getuid()
             and stat.S_IMODE(meta.st_mode) == 0o600 and meta.st_nlink == 1 and meta.st_size <= limit,
             "unsafe or oversized private file")
        raw = stream.read(limit + 1)
        need(len(raw) <= limit, "private file bound exceeded")
        return raw


def branch(value):
    string(value, r"[A-Za-z0-9][A-Za-z0-9_./-]{0,199}")
    need(not any(part in value for part in ("..", "//", "@{"))
         and not value.endswith(("/", ".", ".lock"))
         and all(not p.startswith(".") and not p.endswith(".lock") for p in value.split("/")),
         "unsafe branch name")
    return value


def person(value):
    fields(value, ("id", "login"))
    integer(value["id"])
    string(value["login"], LOGIN)


def validate_config(value):
    fields(value, ("schema", "repository", "repository_id", "actor", "token_file", "state_dir",
                   "heads", "required_checks", "reviewers", "operations"))
    need(value["schema"] == CONFIG_SCHEMA, "unsupported config version")
    string(value["repository"], REPO)
    need(not value["repository"].endswith((".git", ".")), "invalid repository")
    integer(value["repository_id"])
    person(value["actor"])
    for key in ("token_file", "state_dir"):
        outside_checkout(Path(string(value[key])))
    need(isinstance(value["heads"], dict) and 0 < len(value["heads"]) <= 32,
         "explicit head/base mapping required")
    for head, base in value["heads"].items():
        branch(head)
        branch(base)
        need(head != base, "head and base must differ")
    checks = value["required_checks"]
    need(isinstance(checks, list) and 0 < len(checks) <= 32, "required checks must be nonempty")
    for check in checks:
        need(isinstance(check, dict), "invalid check identity")
        if check.get("kind") == "check":
            fields(check, ("kind", "name", "app_id"))
            integer(check["app_id"])
            string(check["name"], limit=200)
        else:
            fields(check, ("kind", "context", "creator_id"))
            need(check["kind"] == "status", "unsupported check kind")
            integer(check["creator_id"])
            string(check["context"], limit=200)
    need(len({canonical(c) for c in checks}) == len(checks), "duplicate required check")
    reviewers = value["reviewers"]
    need(isinstance(reviewers, list) and len(reviewers) <= 15, "reviewer bound exceeded")
    for reviewer in reviewers:
        person(reviewer)
    need(len({r["id"] for r in reviewers}) == len(reviewers)
         and len({r["login"].lower() for r in reviewers}) == len(reviewers), "duplicate reviewer")
    operations = value["operations"]
    need(isinstance(operations, list) and operations and len(set(operations)) == len(operations)
         and set(operations) <= {"publish_pr", "followup", "notify", "observe"},
         "invalid authorized operations")
    return value


def load_config():
    need(CONFIG_PATH is not None and CONFIG_DIGEST is not None, "adapter is not installed")
    path = Path(CONFIG_PATH)
    raw = read_private(path, LIMIT)
    need(digest(raw) == CONFIG_DIGEST, "pinned adapter configuration changed")
    config = validate_config(decode(raw))
    private_path(path.parent, directory=True)
    private_path(Path(config["token_file"]).parent, directory=True)
    private_path(Path(config["state_dir"]), directory=True)
    need(path != Path(config["token_file"]), "config cannot be its credential file")
    return config


def subject(operation, payload):
    prefix = {"publish_pr": "github-pr:", "followup": "github-followup:", "notify": "github-notify:"}
    return prefix[operation] + digest(canonical(payload))


def validate_payload(operation, value, config):
    if operation == "publish_pr":
        fields(value, ("schema", "head", "base", "title", "body"))
        branch(value["head"])
        branch(value["base"])
        need(config["heads"].get(value["head"]) == value["base"], "unauthorized head/base pair")
    elif operation == "followup":
        fields(value, ("schema", "publication", "title", "body"))
        string(value["publication"], PUB)
    else:
        fields(value, ("schema", "publication", "reviewers"))
        string(value["publication"], PUB)
        reviewers = value["reviewers"]
        need(isinstance(reviewers, list) and 0 < len(reviewers) <= 15, "empty reviewer set")
        for reviewer in reviewers:
            person(reviewer)
            need(reviewer in config["reviewers"], "unauthorized review recipient")
        need(reviewers == sorted(reviewers, key=lambda r: r["id"])
             and len({r["id"] for r in reviewers}) == len(reviewers), "reviewers must be sorted and unique")
    need(value["schema"] == PAYLOAD_SCHEMAS[operation], "unsupported operation payload version")
    if operation != "notify":
        string(value["title"], limit=200)
        need("\n" not in value["title"] and "\r" not in value["title"], "multiline title")
        string(value["body"], limit=48000)
        need("<!-- af-github:" not in value["body"], "reserved service marker in content")
    return value


def validate_request(request, config):
    fields(request, ("schema", "method", "intent_id", "input_digest", "operation", "destination", "inputs"))
    need(request["schema"] == "af/scenario-adapter-request@3", "unsupported request version")
    for key in ("intent_id", "input_digest"):
        string(request[key], DIGEST)
    op = request["operation"]
    need(isinstance(op, str) and op in config["operations"], "unauthorized operation")
    need(request["destination"] == "github:" + config["repository"], "unauthorized destination")
    observe = request["method"] == "observe"
    need((observe and op == "observe") or (request["method"] in ("execute", "reconcile")
                                          and op in PAYLOAD_SCHEMAS), "invalid method/operation")
    inputs = request["inputs"]
    fields(inputs, ("subject", "revision", "condition") if observe else ("subject", "revision", "body"),
           () if observe else ("decision", "evidence"))
    texts = {}
    for key in (("subject", "revision", "condition") if observe else ("subject", "revision", "body")):
        fields(inputs[key], ("type", "value"))
        need(inputs[key]["type"] == "text", "expected a text datum")
        texts[key] = string(inputs[key]["value"])
    string(texts["revision"], SHA)
    if observe:
        string(texts["subject"], PUB)
        need(texts["condition"] in ("checks_passed", "merged"), "unsupported observation")
        return texts, None
    payload = validate_payload(op, decode(texts["body"].encode()), config)
    need(texts["subject"] == subject(op, payload), "subject does not bind exact operation content")
    # af is the authenticated caller: its kernel verifies minisign, Run, pin, step and deadline.
    # Recheck object binding here; never accept workflow-supplied arbitrary context or argv.
    if op in ("publish_pr", "followup"):
        need("decision" in inputs, "publication requires a signed kernel decision")
    if "decision" in inputs:
        fields(inputs["decision"], ("type", "value"))
        need(inputs["decision"]["type"] == "decision", "invalid decision datum")
        decision = inputs["decision"]["value"]
        fields(decision, ("schema", "request", "signature"))
        need(decision["schema"] == "af/scenario-signed-decision@1", "invalid decision receipt")
        string(decision["signature"], limit=4096)
        d = decision["request"]
        fields(d, ("schema", "run_id", "execution_id", "pin_id", "step", "subject", "revision",
                   "owner", "approve", "expires_unix_ms"))
        need(d["schema"] == "af/scenario-decision@1", "invalid decision request version")
        for key in ("execution_id", "pin_id"):
            string(d[key], DIGEST)
        for key in ("run_id", "step", "owner"):
            string(d[key])
        integer(d["expires_unix_ms"])
        need(isinstance(d, dict) and d.get("subject") == texts["subject"]
             and d.get("revision") == texts["revision"] and d.get("approve") is True,
             "decision object differs")
    if "evidence" in inputs:
        fields(inputs["evidence"], ("type", "value"))
        need(inputs["evidence"]["type"] == "observation", "invalid observation datum")
        evidence = inputs["evidence"]["value"]
        fields(evidence, ("subject", "revision", "condition", "satisfied", "evidence"))
        need(evidence["subject"] == texts["subject"] and evidence["revision"] == texts["revision"]
             and evidence["satisfied"] is True and evidence["condition"] == "checks_passed",
             "stale or unrelated check evidence")
        string(evidence["evidence"])
    return texts, payload


def identity(request):
    return {k: v for k, v in request.items() if k != "method"}


def empty_state(config_digest):
    return {"schema": STATE_SCHEMA, "config_digest": config_digest, "records": {}}


def state_bytes(state):
    return canonical({**state, "checksum": digest(canonical(state))}) + b"\n"


def atomic_write(path, data):
    need(len(data) <= STATE_LIMIT, "state byte budget exhausted; retain existing receipts")
    # One bounded scratch slot, serialized by the journal lock. A killed writer cannot leave
    # an unbounded sequence of abandoned temporary receipts. Never read it as authoritative.
    temporary = path.with_name(path.name + ".next")
    if temporary.exists() or temporary.is_symlink():
        private_path(temporary)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    with os.fdopen(fd, "wb") as stream:
        meta = os.fstat(stream.fileno())
        need(stat.S_ISREG(meta.st_mode) and meta.st_uid == os.getuid()
             and stat.S_IMODE(meta.st_mode) == 0o600 and meta.st_nlink == 1,
             "unsafe journal scratch file")
        stream.truncate(0)
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class Journal:
    def __init__(self, config, config_digest):
        self.config = config
        self.config_digest = config_digest
        self.root = Path(config["state_dir"])
        self.path = self.root / "state.json"
        self.state = None

    @contextlib.contextmanager
    def locked(self):
        private_path(self.root, directory=True)
        # Never unlink or replace the lock inode. The installer creates both lock and journal.
        fd = os.open(self.root / "lock", os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            meta = os.fstat(fd)
            need(stat.S_ISREG(meta.st_mode) and meta.st_uid == os.getuid()
                 and stat.S_IMODE(meta.st_mode) == 0o600 and meta.st_nlink == 1, "unsafe journal lock")
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise Refused("adapter busy; reconcile the same intent") from exc
            self.state = self.load()
            yield self
        finally:
            os.close(fd)

    def load(self):
        state = decode(read_private(self.path, STATE_LIMIT), STATE_LIMIT)
        fields(state, ("schema", "config_digest", "records", "checksum"))
        checksum = state.pop("checksum")
        need(checksum == digest(canonical(state)), "journal checksum mismatch")
        need(state["schema"] == STATE_SCHEMA and state["config_digest"] == self.config_digest,
             "foreign or unsupported journal")
        records = state["records"]
        need(isinstance(records, dict) and len(records) <= MAX_RECORDS, "journal record bound exceeded")
        subjects = set()
        for key, record in records.items():
            fields(record, ("request", "request_digest", "phase", "object", "evidence"))
            request = record["request"]
            fields(request, ("schema", "intent_id", "input_digest", "operation", "destination", "inputs"))
            texts, _ = validate_request({**request, "method": "reconcile"}, self.config)
            need(key == request["intent_id"] and record["request_digest"] == digest(canonical(request)),
                 "journal intent identity mismatch")
            need(record["phase"] in ("prepared", "dispatched", "done", "failed"), "invalid journal phase")
            obj = record["object"]
            if obj is not None:
                fields(obj, ("id", "number"))
                integer(obj["id"])
                integer(obj["number"])
            if record["phase"] == "prepared":
                need(obj is None and record["evidence"] is None, "prepared journal has effect evidence")
            if record["phase"] in ("done", "failed"):
                need((obj is not None) if record["phase"] == "done" else (obj is None),
                     "terminal journal receipt differs from outcome")
                string(record["evidence"], limit=1000)
            else:
                need(record["evidence"] is None, "unsettled journal has final evidence")
            key_subject = (texts["subject"], texts["revision"])
            if record["phase"] != "failed":
                need(key_subject not in subjects, "duplicate publication intent in journal")
                subjects.add(key_subject)
        return state

    def save(self):
        atomic_write(self.path, state_bytes(self.state))

    def record(self, request):
        records = self.state["records"]
        key = request["intent_id"]
        wanted = identity(request)
        if key in records:
            record = records[key]
            need(record["request"] == wanted, "intent reused with different exact input or digest")
            return record
        for record in records.values():
            if record["phase"] == "failed":
                continue
            a, b = record["request"]["inputs"], request["inputs"]
            need((a["subject"], a["revision"]) != (b["subject"], b["revision"]),
                 "object already reserved by another intent; inspect its original receipt")
        need(len(records) < MAX_RECORDS, "journal receipt budget exhausted; do not discard uncertain intents")
        record = {"request": wanted, "request_digest": digest(canonical(wanted)),
                  "phase": "prepared", "object": None, "evidence": None}
        records[key] = record
        unsettled = sum(r["phase"] in ("prepared", "dispatched") for r in records.values())
        need(len(state_bytes(self.state)) + unsettled * 1024 <= STATE_LIMIT,
             "journal completion reservation exhausted; retain existing receipts")
        self.save()
        return record

    def publication(self, subject_value, revision):
        candidates = [r for r in self.state["records"].values()
                      if r["request"]["operation"] == "publish_pr" and r["phase"] != "failed"
                      and r["request"]["inputs"]["subject"]["value"] == subject_value
                      and r["request"]["inputs"]["revision"]["value"] == revision]
        need(len(candidates) == 1 and candidates[0]["phase"] == "done",
             "no completed local publication receipt for exact subject/revision")
        return candidates[0]


class GitHub:
    def __init__(self, config):
        raw = read_private(Path(config["token_file"]), 4096)
        try:
            token_text = raw.decode("ascii")
        except UnicodeError as exc:
            raise Refused("invalid credential file") from exc
        match = re.fullmatch(r"([A-Za-z0-9_]{20,255})(?:\n)?", token_text)
        need(match is not None, "invalid credential file")
        self.token = match.group(1)
        self.context = ssl.create_default_context()
        self.deadline = time.monotonic() + WALL_SECONDS
        self.calls = 0
        self.more = False

    def request(self, method, path, body=None):
        need((method in ("GET", "POST") and path.startswith("/repos/")) or
             (method == "GET" and path.startswith("/users/")) or
             (method == "GET" and path == "/user") or
             (method == "POST" and path == "/graphql"), "invalid API route")
        need(path.isascii() and "\r" not in path and "\n" not in path and "#" not in path,
             "invalid API path")
        self.calls += 1
        remaining = self.deadline - time.monotonic()
        need(remaining > 0 and self.calls <= 100, "bounded API observation exhausted")
        conn = http.client.HTTPSConnection("api.github.com", timeout=min(5, remaining),
                                           context=self.context)
        try:
            conn.request(method, path, body=canonical(body) if body is not None else None, headers={
                "Authorization": "Bearer " + self.token,
                "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
                "Content-Type": "application/json", "User-Agent": "af-scenario-github-v1",
            })
            response = conn.getresponse()
            # Never follow redirects or log returned URLs, headers, bodies or tokens.
            need(response.status == (200 if method == "GET" or path == "/graphql" else 201),
                 "GitHub request not successful; outcome remains unknown")
            need(response.getheader("Content-Encoding") in (None, "identity"), "encoded API response refused")
            size = response.getheader("Content-Length")
            if size is not None:
                need(size.isdigit() and int(size) <= HTTP_LIMIT, "API response bound exceeded")
            link = response.getheader("Link") or ""
            need(len(link) <= 8192, "pagination header bound exceeded")
            self.more = False
            if link:
                for entry in link.split(","):
                    match = re.fullmatch(r'\s*<([^>]+)>;\s*rel="(next|prev|first|last)"\s*', entry)
                    need(match is not None, "malformed API pagination header")
                    target = urlsplit(match.group(1))
                    need(target.scheme == "https" and target.netloc == "api.github.com"
                         and target.path == urlsplit(path).path and not target.fragment,
                         "foreign pagination URL refused")
                    self.more = self.more or match.group(2) == "next"
            raw = response.read(HTTP_LIMIT + 1)
            need(size is None or len(raw) == int(size), "truncated API response")
            data = decode(raw, HTTP_LIMIT)
            return data
        except (OSError, http.client.HTTPException) as exc:
            raise Refused("GitHub transport uncertain; reconcile the same intent") from exc
        finally:
            conn.close()

    def pages(self, path, key=None, params=None):
        rows, seen = [], set()
        total = None
        for page in range(1, MAX_PAGES + 1):
            data = self.request("GET", path + "?" + urlencode({**(params or {}), "per_page": 100, "page": page}))
            if key:
                need(isinstance(data, dict) and type(data.get("total_count")) is int,
                     "partial check listing")
                if total is None:
                    total = data["total_count"]
                need(data["total_count"] == total and 0 <= total <= MAX_PAGES * 100,
                     "check listing changed or exceeds bound")
                items = data.get(key)
            else:
                items = data
            need(isinstance(items, list) and len(items) <= 100, "invalid API page")
            for item in items:
                need(isinstance(item, dict), "invalid API object")
                ident = integer(item.get("id"))
                need(ident not in seen, "listing shifted or repeated; observe again")
                seen.add(ident)
            rows.extend(items)
            if len(items) < 100:
                need(not self.more, "partial API listing has another page")
                need(total is None or len(rows) == total, "incomplete check listing")
                return rows
        raise Refused("API pagination bound reached; retain pending intent")


class Adapter:
    def __init__(self, config, config_digest, service):
        self.config = config
        self.journal = Journal(config, config_digest)
        self.service = service
        self.repo = "/repos/" + config["repository"]

    def actor(self):
        user = self.service.request("GET", "/user")
        need(isinstance(user, dict) and user.get("id") == self.config["actor"]["id"]
             and user.get("login") == self.config["actor"]["login"], "credential actor is unauthorized")
        repo = self.service.request("GET", self.repo)
        need(isinstance(repo, dict) and repo.get("id") == self.config["repository_id"]
             and repo.get("full_name") == self.config["repository"], "repository identity changed")

    def locked_head(self, payload, revision):
        path = self.repo + "/branches/" + quote(payload["head"], safe="")
        protection = self.service.request("GET", path + "/protection")
        need(isinstance(protection, dict), "missing branch protection")
        for field, enabled in (("lock_branch", True), ("enforce_admins", True),
                               ("allow_deletions", False), ("allow_force_pushes", False),
                               ("allow_fork_syncing", False)):
            need(isinstance(protection.get(field), dict)
                 and protection[field].get("enabled") is enabled, "unsupported or unsafe head branch protection")
        head = self.service.request("GET", path)
        need(isinstance(head, dict) and head.get("name") == payload["head"]
             and head.get("protected") is True and isinstance(head.get("commit"), dict)
             and head["commit"].get("sha") == revision, "approved locked head revision changed")

    def marker(self, record):
        return "\n\n<!-- af-github:" + record["request"]["intent_id"] + ":" + record["request_digest"] + " -->"

    def payload(self, record):
        req = record["request"]
        return decode(req["inputs"]["body"]["value"].encode())

    def object_id(self, remote):
        need(isinstance(remote, dict), "invalid remote receipt")
        return {"id": integer(remote.get("id")), "number": integer(remote.get("number"))}

    def validate_remote(self, record, remote):
        req, p = record["request"], self.payload(record)
        obj = self.object_id(remote)
        if record["object"] is not None:
            need(record["object"] == obj, "foreign remote object receipt")
        pr = req["operation"] in ("publish_pr", "notify")
        kind, html_kind = ("pulls", "pull") if pr else ("issues", "issues")
        number = str(obj["number"])
        need(remote.get("url") == "https://api.github.com" + self.repo + "/" + kind + "/" + number
             and remote.get("html_url") == "https://github.com/" + self.config["repository"] + "/" + html_kind + "/" + number,
             "remote object belongs to another destination")
        if req["operation"] == "notify":
            source = self.journal.publication(p["publication"], req["inputs"]["revision"]["value"])
            need(obj == source["object"], "review receipt identifies an unrelated PR")
            self.validate_remote(source, remote)
            return obj
        need(isinstance(remote.get("user"), dict)
             and remote["user"].get("id") == self.config["actor"]["id"]
             and remote["user"].get("login") == self.config["actor"]["login"]
             and remote.get("title") == p["title"]
             and remote.get("body") == p["body"] + self.marker(record),
             "foreign actor, marker or changed publication content")
        if pr:
            for side, ref in (("head", p["head"]), ("base", p["base"])):
                value = remote.get(side)
                need(isinstance(value, dict) and value.get("ref") == ref
                     and isinstance(value.get("repo"), dict)
                     and value["repo"].get("id") == self.config["repository_id"]
                     and value["repo"].get("full_name") == self.config["repository"],
                     "PR branch or repository identity changed")
            need(remote["head"].get("sha") == req["inputs"]["revision"]["value"], "PR head is stale")
        else:
            need("pull_request" not in remote, "a PR cannot satisfy an issue receipt")
        return obj

    def read_object(self, record):
        need(record["object"] is not None, "remote receipt absent")
        kind = "pulls" if record["request"]["operation"] in ("publish_pr", "notify") else "issues"
        remote = self.service.request("GET", self.repo + "/" + kind + "/" + str(record["object"]["number"]))
        self.validate_remote(record, remote)
        return remote

    def publication(self, subject_value, revision):
        record = self.journal.publication(subject_value, revision)
        return record, self.read_object(record)

    def green(self, revision):
        required = self.config["required_checks"]
        for kind in ("check", "status"):
            checks = [c for c in required if c["kind"] == kind]
            if not checks:
                continue
            path = self.repo + "/commits/" + revision
            if kind == "check":
                rows = self.service.pages(path + "/check-runs", key="check_runs", params={"filter": "all"})
                suites = self.service.pages(path + "/check-suites", key="check_suites")
            else:
                rows = self.service.pages(path + "/statuses")
            for wanted in checks:
                matches = []
                for row in rows:
                    if kind == "check" and row.get("name") == wanted["name"]:
                        need(isinstance(row.get("app"), dict)
                             and row["app"].get("id") == wanted["app_id"], "untrusted check app for required name")
                        need(row.get("head_sha") == revision, "check belongs to a stale SHA")
                        matches.append(row)
                    elif kind == "status" and row.get("context") == wanted["context"]:
                        need(isinstance(row.get("creator"), dict)
                             and row["creator"].get("id") == wanted["creator_id"], "untrusted status creator")
                        # REST lists statuses only for the exact SHA in this fixed URL.
                        if "sha" in row:
                            need(row["sha"] == revision, "status belongs to a stale SHA")
                        matches.append(row)
                if not matches:
                    return False
                latest = max(matches, key=lambda row: integer(row["id"]))
                if kind == "check":
                    app_suites = [s for s in suites if isinstance(s.get("app"), dict)
                                  and s["app"].get("id") == wanted["app_id"]]
                    if not app_suites:
                        return False
                    newest = max(app_suites, key=lambda s: integer(s["id"]))
                    need(newest.get("head_sha") == revision, "stale check suite")
                    if (newest.get("status") != "completed" or newest.get("conclusion") != "success"
                            or not isinstance(latest.get("check_suite"), dict)
                            or latest["check_suite"].get("id") != newest["id"]):
                        return False
                    if latest.get("status") != "completed" or latest.get("conclusion") != "success":
                        return False
                elif latest.get("state") != "success":
                    return False
        return True

    def finish(self, record, obj):
        record.update(phase="done", object=obj,
                      evidence="GitHub " + record["request"]["operation"] + " receipt " + str(obj["id"]) + "/" + str(obj["number"]))
        self.journal.save()
        return "done", record["evidence"]

    def reconcile(self, record):
        if record["phase"] == "failed":
            return "failed", record["evidence"]
        if record["phase"] == "prepared":
            return "absent", "Durable local intent has never dispatched a mutation"
        if record["phase"] == "done":
            self.read_object(record)
            if record["request"]["operation"] == "followup":
                self.publication(self.payload(record)["publication"],
                                 record["request"]["inputs"]["revision"]["value"])
            return "done", record["evidence"]
        req, p = record["request"], self.payload(record)
        if req["operation"] == "notify":
            # GitHub has no intent marker for review requests. A reviewer can be removed after
            # success, so neither current absence nor presence proves this invocation's effect.
            return "pending", "Review request may have occurred; inspect PR manually; never redispatch this intent"
        if record["object"] is not None:
            remote = self.read_object(record)
        else:
            pr = req["operation"] == "publish_pr"
            path = self.repo + ("/pulls" if pr else "/issues")
            params = {"state": "all"}
            if pr:
                params.update(head=self.config["repository"].split("/")[0] + ":" + p["head"], base=p["base"])
            else:
                params.update(creator=self.config["actor"]["login"])
            rows = self.service.pages(path, params=params)
            matches = []
            for row in rows:
                if self.marker(record) in (row.get("body") or ""):
                    self.validate_remote(record, row)
                    matches.append(row)
            need(len(matches) <= 1, "multiple remote markers; manual recovery required")
            if not matches:
                return "pending", "Dispatch outcome unknown; empty remote listing proves no absence; retain receipt and inspect GitHub"
            record["object"] = self.object_id(matches[0])
            self.journal.save()
            remote = self.read_object(record)
        if req["operation"] == "publish_pr":
            self.locked_head(p, req["inputs"]["revision"]["value"])
        else:
            self.publication(p["publication"], req["inputs"]["revision"]["value"])
        return self.finish(record, self.object_id(remote))

    def execute(self, record):
        if record["phase"] != "prepared":
            return self.reconcile(record)
        req, p = record["request"], self.payload(record)
        op, revision = req["operation"], req["inputs"]["revision"]["value"]
        source = None
        if op == "publish_pr":
            self.locked_head(p, revision)
            path = self.repo + "/pulls"
            body = {"head": p["head"], "base": p["base"], "title": p["title"],
                    "body": p["body"] + self.marker(record), "draft": False,
                    "maintainer_can_modify": False}
        else:
            source, remote = self.publication(p["publication"], revision)
            if op == "followup":
                path = self.repo + "/issues"
                body = {"title": p["title"], "body": p["body"] + self.marker(record)}
            else:
                need(remote.get("state") == "open" and remote.get("draft") is False,
                     "review target must be an open non-draft PR")
                self.locked_head(self.payload(source), revision)
                need(self.green(revision), "review requires current successful exact-head checks")
                users = []
                for reviewer in p["reviewers"]:
                    user = self.service.request("GET", "/users/" + reviewer["login"])
                    need(isinstance(user, dict) and user.get("id") == reviewer["id"]
                         and user.get("login") == reviewer["login"] and user.get("type") == "User",
                         "review recipient identity changed or is not a user")
                    users.append(string(user.get("node_id"), r"[A-Za-z0-9_+/=-]{1,200}"))
                # REST requests mutable logins. Resolve immutable node IDs from verified
                # numeric identities, then dispatch one fixed mutation; a rename/reuse race
                # cannot notify a different account. No workflow supplies GraphQL text.
                pull_node = string(remote.get("node_id"), r"[A-Za-z0-9_+/=-]{1,200}")
                path = "/graphql"
                body = {"query": REVIEW_MUTATION, "variables": {"pull": pull_node, "users": users}}
        # This write is the at-most-once boundary. No code after it can return failed/absent.
        # HTTP errors, including 401/403/422, remain uncertain once dispatch is possible.
        record["phase"] = "dispatched"
        self.journal.save()
        remote = self.service.request("POST", path, body)
        if op == "notify":
            need(isinstance(remote, dict) and not remote.get("errors"),
                 "review mutation returned errors; outcome uncertain")
            data = remote.get("data")
            need(isinstance(data, dict) and isinstance(data.get("requestReviews"), dict),
                 "review mutation omitted receipt")
            pull = data["requestReviews"].get("pullRequest")
            need(isinstance(pull, dict) and pull.get("id") == pull_node,
                 "review mutation identifies an unrelated PR")
            remote = self.read_object(source)
            obj = self.object_id(remote)
            record["object"] = obj
            self.journal.save()
            reviewers = remote.get("requested_reviewers")
            need(isinstance(reviewers, list) and all(isinstance(r, dict) for r in reviewers)
                 and all(any(r.get("id") == wanted["id"] for r in reviewers) for wanted in p["reviewers"]),
                 "review request readback is incomplete")
            self.locked_head(self.payload(source), revision)
        else:
            obj = self.object_id(remote)
            record["object"] = obj
            self.journal.save()
            self.validate_remote(record, remote)
            self.read_object(record)
            if op == "publish_pr":
                self.locked_head(p, revision)
            else:
                self.read_object(source)
        return self.finish(record, obj)

    def observe(self, request, texts):
        source, remote = self.publication(texts["subject"], texts["revision"])
        need(remote.get("state") in ("open", "closed") and type(remote.get("merged")) is bool,
             "incomplete PR lifecycle evidence")
        if remote["state"] == "closed" and not remote["merged"]:
            return "failed", "PR closed without merge", None
        if texts["condition"] == "merged":
            satisfied = remote["state"] == "closed" and remote["merged"] is True
            if satisfied:
                string(remote.get("merged_at"), limit=100)
                string(remote.get("merge_commit_sha"), SHA)
        else:
            self.locked_head(self.payload(source), texts["revision"])
            satisfied = self.green(texts["revision"])
        # Bracket CI/merge observation with independently fetched exact PR identity and head.
        after = self.read_object(source)
        need(after.get("state") == remote["state"] and after.get("merged") is remote["merged"],
             "PR lifecycle changed during observation")
        evidence = "Exact PR " + str(source["object"]["number"]) + " at " + texts["revision"] + ": " + texts["condition"]
        return "done", evidence, {"subject": texts["subject"], "revision": texts["revision"],
                                  "condition": texts["condition"], "satisfied": satisfied, "evidence": evidence}

    def handle(self, request):
        texts, _ = validate_request(request, self.config)
        response = {"schema": "af/scenario-adapter-response@1", "intent_id": request["intent_id"],
                    "input_digest": request["input_digest"], "outcome": "pending",
                    "evidence": "Reconcile the retained intent", "observation": None}
        try:
            with self.journal.locked():
                self.actor()
                if request["method"] == "observe":
                    outcome, evidence, observation = self.observe(request, texts)
                    response.update(outcome=outcome, evidence=evidence, observation=observation)
                else:
                    record = self.journal.record(request)
                    try:
                        outcome, evidence = (self.execute(record) if request["method"] == "execute"
                                             else self.reconcile(record))
                        response.update(outcome=outcome, evidence=evidence)
                    except Refused as exc:
                        # Only a locally never-dispatched request can definitively fail.
                        if record["phase"] == "prepared":
                            record.update(phase="failed", evidence=str(exc))
                            self.journal.save()
                            response.update(outcome="failed", evidence=str(exc))
                        else:
                            response.update(outcome="pending", evidence=str(exc))
        except (Refused, OSError) as exc:
            # Unknown, corrupt or missing local/remote state never proves absence or success.
            response["evidence"] = str(exc) if isinstance(exc, Refused) else "Private journal unavailable; inspect retained state"
        return response


def schema_documents():
    """JSON Schema structural contracts; admission additionally checks identities and authority."""
    def obj(properties):
        return {"type": "object", "additionalProperties": False,
                "required": list(properties), "properties": properties}

    def text(pattern=None, maximum=65536):
        value = {"type": "string", "minLength": 1, "maxLength": maximum}
        if pattern:
            value["pattern"] = "^" + pattern + "$"
        return value

    def array(item, minimum=0, maximum=32):
        return {"type": "array", "items": item, "minItems": minimum, "maxItems": maximum,
                "uniqueItems": True}

    positive = {"type": "integer", "minimum": 1, "maximum": 9007199254740991}
    who = obj({"id": positive, "login": text(LOGIN)})
    check = obj({"kind": {"const": "check"}, "name": text(maximum=200), "app_id": positive})
    status = obj({"kind": {"const": "status"}, "context": text(maximum=200), "creator_id": positive})
    documents = {}

    def document(name, value):
        version = 3 if name == "github-adapter-state" else 1
        documents[name + "-v%d.json" % version] = {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "urn:af:schema:" + name + ":%d" % version,
            "description": "GitHub Scenario adapter v1. Semantic validation also binds exact identities, scope and durable state.",
            **value}

    document("github-adapter-config", obj({
        "schema": {"const": CONFIG_SCHEMA}, "repository": text(REPO), "repository_id": positive,
        "actor": who, "token_file": text(r"/.*"), "state_dir": text(r"/.*"),
        "heads": {"type": "object", "minProperties": 1, "maxProperties": 32,
                  "additionalProperties": text(r"[A-Za-z0-9][A-Za-z0-9_./-]{0,199}"),
                  "propertyNames": text(r"[A-Za-z0-9][A-Za-z0-9_./-]{0,199}")},
        "required_checks": array({"oneOf": [check, status]}, minimum=1),
        "reviewers": array(who, maximum=15),
        "operations": array({"enum": ["publish_pr", "followup", "notify", "observe"]}, minimum=1, maximum=4)}))
    for op, schema in PAYLOAD_SCHEMAS.items():
        props = {"schema": {"const": schema}}
        if op == "publish_pr":
            props.update(head=text(r"[A-Za-z0-9][A-Za-z0-9_./-]{0,199}"),
                         base=text(r"[A-Za-z0-9][A-Za-z0-9_./-]{0,199}"))
        else:
            props["publication"] = text(PUB)
        if op == "notify":
            props["reviewers"] = array(who, minimum=1, maximum=15)
        else:
            props.update(title=text(maximum=200), body=text(maximum=48000))
        document(schema.split("/")[1].split("@")[0], obj(props))
    request = obj({"schema": {"const": "af/scenario-adapter-request@3"},
                   "intent_id": text(DIGEST), "input_digest": text(DIGEST),
                   "operation": {"enum": list(PAYLOAD_SCHEMAS)}, "destination": text(),
                   "inputs": {"type": "object", "additionalProperties": {
                       "$ref": "urn:af:schema:scenario:4#/$defs/Datum"}}})
    record = obj({"request": request, "request_digest": text(DIGEST),
                  "phase": {"enum": ["prepared", "dispatched", "done", "failed"]},
                  "object": {"anyOf": [{"type": "null"}, obj({"id": positive, "number": positive})]},
                  "evidence": {"anyOf": [{"type": "null"}, text(maximum=1000)]}})
    document("github-adapter-state", obj({"schema": {"const": STATE_SCHEMA},
             "config_digest": text(DIGEST), "checksum": text(DIGEST),
             "records": {"type": "object", "maxProperties": MAX_RECORDS,
                         "propertyNames": text(DIGEST), "additionalProperties": record}}))
    return documents


def deadline_expired(_signal, _frame):
    raise Refused("adapter wall bound reached")


def main():
    os.environ.clear()  # No proxy, TLS, token, endpoint or Python configuration from callers.
    os.umask(0o077)
    need(sys.flags.isolated and sys.flags.no_site,
         "adapter requires isolated system Python without site imports")
    need(len(sys.argv) == 1, "adapter accepts stdin only; no arguments")
    signal.signal(signal.SIGALRM, deadline_expired)
    signal.alarm(WALL_SECONDS)
    try:
        request = decode(sys.stdin.buffer.read(LIMIT + 1))
        config = load_config()
        response = Adapter(config, CONFIG_DIGEST, GitHub(config)).handle(request)
        sys.stdout.buffer.write(canonical(response) + b"\n")
        sys.stdout.buffer.flush()
    finally:
        signal.alarm(0)


if __name__ == "__main__":
    try:
        main()
    except (Refused, OSError, ValueError, TypeError, KeyError, RecursionError):
        # Malformed/unauthorized input and setup errors have no protocol success response.
        # No exception detail or remote/service bytes escape through stderr.
        sys.stderr.write("GitHub adapter refused input or private machine setup; inspect owner configuration\n")
        sys.exit(1)
