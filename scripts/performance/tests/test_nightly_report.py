"""The owner's daily nightly report (scripts/nightly/report.py, ADR-0271) with injected fakes.

Each test builds a builds root and a nightly executor journal in its own temporary directory
from the laboratory's valid schema fixtures (the night's record, the executor request, result
and state, a frozen plan, a verdict and a report), answers GitHub from a fake transport and
the machine from a fake host. Nothing reaches the network, starts a container or builds Rust.
The three nights' daily.json and digest.md are compared with committed goldens; report.html is
checked by structure: its sections in order, only the renderer's elements, and every cell of its
tables equal to daily.json's value formatted here, independently of the composer.
"""
import contextlib
import copy
import html.parser
import importlib.util
import io
import json
import os
import re
import unittest

from perflab import contracts, executor, host_lock, render
from perflab.nightly_contracts import DAILY_SCHEMA, three_significant
from perflab.values import ContractError, canonical_digest, sha256_bytes
from tests.support import GitHubOpener, ROOT, TempDir, read

REPORT = os.path.join(ROOT, "scripts", "nightly", "report.py")
_spec = importlib.util.spec_from_file_location("nightly_report", REPORT)
nightly = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(nightly)

SCHEMA = os.path.join(ROOT, "fixtures", "performance", "schema")
GOLDEN = os.path.join(SCHEMA, "nightly-daily")
COMPOSED_MS = 1793286000000  # 2026-10-29T15:00:00Z
HEADLINE = ["synthetic_fast", "synthetic_process__size_1"]
BASE = "dddddddddddddddddddddddddddddddddddddddd"
HEAD = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
PREVIOUS = "cccccccccccccccccccccccccccccccccccccccc"
OLDER_BASE = "9999999999999999999999999999999999999999"
OLDER_HEAD = "8888888888888888888888888888888888888888"
CONTROL = "8d71b256cd32b268931317214a5a30dc8c9c276d"
AF = {"x86_64-unknown-linux-musl": b"\x7fELF nightly af x86_64\n",
      "aarch64-unknown-linux-musl": b"\x7fELF nightly af aarch64\n"}
STABLE_AF = ("sha256:" + "a" * 64, 11534336)
# Every element perflab.render writes; the page holds no other.
RENDERED = {"html", "head", "meta", "title", "style", "body", "h1", "h2", "h3", "section", "p",
            "span", "code", "table", "thead", "tbody", "tr", "th", "td", "svg", "line", "text",
            "circle", "script"}


def fixture(relative):
    return json.loads(read(os.path.join(SCHEMA, relative)).decode("utf-8"))


def toml(doc, prefix=""):
    """A TOML text of `doc` (strings, integers, booleans, string lists and tables)."""
    lines, tables = [], []
    for key, value in doc.items():
        if isinstance(value, dict):
            tables.append((key, value))
        elif value is None:
            continue
        else:
            lines.append("%s = %s" % (key, json.dumps(value)))
    for key, value in tables:
        name = prefix + key
        lines.append("\n[%s]" % name)
        lines.append(toml(value, name + "."))
    return "\n".join(lines)


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as stream:
        stream.write(data)


# The journal -------------------------------------------------------------------------------

def passing_verdict(verdict):
    """The regression fixture with its regressing case within tolerance: a passing night."""
    verdict = copy.deepcopy(verdict)
    calm = next(case for case in verdict["cases"]
                if case["instance_id"] == "synthetic_process__size_1")
    for case in verdict["cases"]:
        if case["instance_id"] == "synthetic_fast":
            for field in ("status", "statistical_classification", "outcome", "interval",
                          "baseline", "candidate", "change"):
                case[field] = copy.deepcopy(calm[field])
            case["baseline"] = {"median": "19", "p95": "19", "max": "19"}
            case["candidate"] = {"median": "19", "p95": "19", "max": "19"}
    verdict["aggregate"].update(result="pass", exit_code=0, reasons=[])
    return verdict


def report_of(verdict, plan):
    """A valid af.perf-report/3 of `verdict` on the partial-scope fixture's template."""
    doc = fixture("report/valid-partial-scope.json")
    template = doc["cases"][0]
    doc["run_id"], doc["plan_digest"], doc["verdict"] = (verdict["run_id"],
                                                          verdict["plan_digest"], verdict)
    doc["identity"].update(scope=verdict["scope"], tier=plan["tier"],
                           baseline_sha=plan["resolved"]["baseline_sha"],
                           candidate_sha=plan["resolved"]["candidate_sha"])
    reasons = [reason for reason, holds in (
        ("result_not_pass", verdict["aggregate"]["result"] != "pass"),
        ("scope_not_full", verdict["scope"] != "full"),
        ("policy_not_active", doc["identity"]["authority_mode"] != "active"),
        ("evidence_not_measured", verdict["provenance"] != "measured")) if holds]
    doc["required_check"] = {"satisfied": not reasons, "reasons": reasons}
    doc["cases"] = [dict(copy.deepcopy(template), instance_id=case["instance_id"],
                         case_id=case["instance_id"],
                         denominator=dict(template["denominator"], measured_pairs=case["pairs"],
                                          missing_records=case["missing_records"]))
                    for case in verdict["cases"]]
    doc["sequences"] = [{"shard": shard["shard"], "points": []} for shard in verdict["shards"]]
    doc["shards"] = [{"shard": shard["shard"], "terminal": None} for shard in verdict["shards"]]
    data = contracts.write_bytes(verdict, "verdict")
    doc["downloads"] = [{"path": "verdict.json", "sha256": sha256_bytes(data),
                         "size": len(data), "kind": "verdict"}]
    return contracts.validate_report(doc)


def builds_of(plan, candidate_af, baseline_af=STABLE_AF):
    """A valid af.perf-builds record of `plan` whose sides built the af (sha256, bytes)
    given."""
    doc = fixture("builds/valid-synthetic.json")
    doc.update(run_id=plan["run_id"], plan_digest=plan["plan_digest"])
    for side, (sha256, size) in (("baseline", baseline_af), ("candidate", candidate_af)):
        doc["sides"][side]["binaries"]["af"] = sha256
        doc["sides"][side]["artifacts"]["af"].update(sha256=sha256, size=size)
    return contracts.validate_builds(doc)


class Journal:
    """A nightly executor journal below `root`, written from the schema fixtures."""

    def __init__(self, root):
        self.root = root
        self.index = {"schema": "af.perf-executor-journal/1", "journal_id": "claw_nightly_1",
                      "repository": "owner/af", "originals": []}
        os.makedirs(os.path.join(root, "requests"))
        self.flush()

    def flush(self):
        write(os.path.join(self.root, "journal.json"),
              contracts.write_bytes(self.index, "executor-journal"))

    def path(self, ident, *parts):
        return os.path.join(self.root, "requests", ident, *parts)

    def original(self, tag, work, baseline, candidate, baseline_tag, files=(), state="complete",
                 plan=None, binaries=None):
        key = "nightly:%s:%s" % (tag, work)
        ident = "exp_" + canonical_digest({"submission": {"kind": "nightly", "key": key},
                                           "work": work})[7:55]
        sources = {"baseline_sha": baseline, "candidate_sha": candidate,
                   "control_sha": CONTROL, "authority_sha": CONTROL}
        request = fixture("executor-request/valid-nightly-v3.json")
        request.update(request_id=ident, sources=sources, work=work,
                       plan_sha256=None if plan is None
                       else sha256_bytes(contracts.write_bytes(plan, "run")),
                       submission={"kind": "nightly", "key": key, "tag": tag,
                                   "baseline": {"tag": baseline_tag, "sha": baseline}})
        write(self.path(ident, "request.json"), contracts.write_bytes(request, "executor-request"))
        for name, data in files:
            write(self.path(ident, *name.split("/")), data)
        self.index["originals"].append({"request_id": ident,
                                        "request_digest": canonical_digest(request)})
        self.flush()
        result_sha = None
        if state == "complete":
            result = fixture("executor-result/valid-core-binaries.json")
            result.update(request_id=ident, request_digest=canonical_digest(request),
                          sources=sources, binaries=binaries,
                          evidence=executor.file_manifest(self.path(ident),
                                                          executor.EVIDENCE_PREFIXES))
            data = contracts.write_bytes(result, "executor-result")
            write(self.path(ident, "result.json"), data)
            result_sha = sha256_bytes(data)
        record = fixture("executor-state/valid-synthetic.json")
        record.update(request_id=ident, request_digest=canonical_digest(request), state=state,
                      result_sha256=result_sha, detail="" if state == "complete"
                      else "the claw executor has not run it yet",
                      started_unix_ms=None if state == "queued" else 1761740000000,
                      ended_unix_ms=1761745000000 if state == "complete" else None)
        write(self.path(ident, "state.json"), contracts.write_bytes(record, "executor-state"))
        return ident

    def core(self, tag, baseline, candidate, baseline_tag, verdict,
             af=("sha256:" + "e" * 64, 24), binaries=None, report=None, state="complete"):
        """A core original whose sealed run/builds.json built the candidate `af`; its result
        names those binaries unless `binaries` says otherwise."""
        plan = fixture("run/valid-bootstrap.json")
        plan["resolved"] = dict(plan["resolved"], baseline_sha=baseline, candidate_sha=candidate,
                                control_sha=CONTROL, authority_sha=CONTROL)
        plan["plan_digest"] = contracts.plan_digest(plan)
        verdict = dict(copy.deepcopy(verdict), run_id=plan["run_id"],
                       plan_digest=plan["plan_digest"])
        doc = report_of(verdict, plan) if report is None else report(verdict, plan)
        built = builds_of(plan, af)
        files = [("plan.json", contracts.write_bytes(plan, "run")),
                 ("run/builds.json", contracts.write_bytes(built, "builds")),
                 ("report/verdict.json", contracts.write_bytes(verdict, "verdict")),
                 ("report/report.json", json.dumps(doc, sort_keys=True).encode() + b"\n")]
        return self.original(tag, "core", baseline, candidate, baseline_tag, files, state, plan,
                             executor.built_binaries(built) if binaries is None else binaries)

    def telemetry(self, tag, baseline, candidate, baseline_tag):
        ident = self.original(tag, "telemetry", baseline, candidate, baseline_tag)
        text = read(os.path.join(SCHEMA, "telemetry-history/valid-one-group.json")).decode()
        text = (text.replace("tl_fixture_one", "local_" + ident[4:])
                .replace("1" * 40, baseline).replace("2" * 40, candidate))
        doc = contracts.validate_telemetry_history(json.loads(text))
        write(os.path.join(self.root, "local-telemetry-history", "history.json"),
              contracts.write_bytes(doc, "telemetry-history"))
        return ident


# GitHub and the host -----------------------------------------------------------------------

def pull(number, title, merge, updated, draft=False):
    return {"number": number, "title": title, "merge_commit_sha": merge,
            "merged_at": updated if merge else None, "updated_at": updated, "draft": draft}


class FakeGitHub:
    """api.github.com as the report reads it, its lists in pages; `down` makes every request
    fail."""

    COMMITS = {(PREVIOUS, HEAD): [("e1" * 20, "2026-10-28T18:00:00Z"), (HEAD,
                                                                        "2026-10-29T01:00:00Z")],
               (BASE, HEAD): [("e3" * 20, "2026-10-20T09:00:00Z"), ("e2" * 20,
                                                                     "2026-10-25T09:00:00Z"),
                              ("e1" * 20, "2026-10-28T18:00:00Z"), (HEAD,
                                                                    "2026-10-29T01:00:00Z")]}
    ISSUES = [
        {"number": 7, "title": "A <script> in a title", "labels": [{"name": "bug"}],
         "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-02T00:00:00Z",
         "closed_at": None},
        {"number": 9, "title": "Report *the* queue", "labels": [{"name": "bug"},
                                                                  {"name": "nightly"}],
         "created_at": "2026-10-29T08:00:00Z", "updated_at": "2026-10-29T09:00:00Z",
         "closed_at": None},
        {"number": 12, "title": "A pull request is no issue", "pull_request": {},
         "created_at": "2026-10-29T08:00:00Z", "updated_at": "2026-10-29T09:00:00Z"}]
    RECENT = [ISSUES[1], {"number": 5, "title": "Closed today", "labels": [],
                          "created_at": "2026-10-01T00:00:00Z",
                          "updated_at": "2026-10-29T10:00:00Z",
                          "closed_at": "2026-10-29T10:00:00Z"}]
    CLOSED = [pull(41, "Measure the nightly", "e1" * 20, "2026-10-28T19:00:00Z"),
              pull(40, "Closed without merging", None, "2026-10-27T19:00:00Z"),
              pull(38, "Before the baseline's commits", "f0" * 20, "2026-10-21T09:00:00Z"),
              pull(36, "Build the nightly", "e3" * 20, "2026-10-20T10:00:00Z"),
              pull(30, "Long ago", "f1" * 20, "2026-10-01T09:00:00Z")]
    OPEN = [pull(44, "Compose the report", None, "2026-10-29T11:00:00Z"),
            pull(45, "Draft: publish it", None, "2026-10-29T12:00:00Z", draft=True)]

    SUBJECT = "Compose the daily report (#44)"

    def __init__(self, down=False, open_pulls=None):
        self.down = down
        self.open = self.OPEN if open_pulls is None else open_pulls
        self.paths = []

    def get(self, path, headers):
        self.paths.append(path)
        if self.down:
            raise OSError("network is unreachable")
        route = path.split("?", 1)[0]
        page = int((re.search(r"[?&]page=([0-9]+)", path) or re.match("()", "")).group(1) or 1)
        per_page = int((re.search(r"[?&]per_page=([0-9]+)", path) or re.match("()", "")).group(1)
                       or 30)
        if page > 1 and "/compare/" in route:
            return 200, json.dumps({"total_commits": 0, "commits": []}).encode()
        prefix = "/repos/owner/af"
        if route == prefix + "/commits/%s" % HEAD:
            body = {"commit": {"message": self.SUBJECT + "\n\nThe body."}}
        elif route == prefix + "/commits/%s/check-runs" % HEAD:
            body = {"total_count": 3, "check_runs": [
                {"name": "check / lint", "status": "completed", "conclusion": "success"},
                {"name": "check / class perf", "status": "completed", "conclusion": "skipped"},
                {"name": "Performance", "status": "in_progress", "conclusion": None}]}
        elif route == prefix + "/releases/tags/v0.13.0":
            body = {"published_at": "2026-10-20T08:00:00Z"}
        elif route.startswith(prefix + "/compare/"):
            base, head = route[len(prefix + "/compare/"):].split("...")
            commits = self.COMMITS[(base, head)]
            body = {"total_commits": len(commits),
                    "commits": [{"sha": sha, "commit": {"committer": {"date": when}}}
                                for sha, when in commits]}
        elif route == prefix + "/pulls" and "state=closed" in path:
            body = self.CLOSED
        elif route == prefix + "/pulls":
            body = self.open
        elif route.startswith(prefix + "/pulls/"):
            body = {"mergeable_state": "draft" if route.endswith("/45") else "clean"}
        elif route == prefix + "/issues" and "state=open" in path:
            body = self.ISSUES
        elif route == prefix + "/issues":
            body = self.RECENT
        else:
            return 404, b'{"message": "Not Found"}'
        if isinstance(body, list):
            body = body[(page - 1) * per_page:page * per_page]
        return 200, json.dumps(body).encode()

    def client(self):
        return nightly.GitHub("owner/af", opener=GitHubOpener(self.get))


class FakeHost:
    def __init__(self, refuse=False):
        self.refuse = refuse
        self.asked = []

    def _answer(self, what, value):
        self.asked.append(what)
        if self.refuse:
            raise nightly.Unavailable("%s: refused by the fake host" % what)
        return value

    def free_bytes(self, path):
        return self._answer("free_bytes " + path, 214748364800)

    def lock_held(self, path):
        return self._answer("lock_held " + path, False)

    def failed_units(self):
        return self._answer("failed_units", 0)


# The page ----------------------------------------------------------------------------------

class Page(html.parser.HTMLParser):
    """The page's h2 headings in order, each section's tables (rows of cell texts), every
    element and every attribute."""

    def __init__(self, data):
        html.parser.HTMLParser.__init__(self)
        self.headings, self.tables, self.attributes, self.elements = [], {}, [], set()
        self._text, self._cell = None, None
        self.feed(data.decode("utf-8"))

    def handle_starttag(self, tag, attrs):
        self.elements.add(tag)
        self.attributes.extend(attrs)
        if tag == "h2":
            self._text = ""
        elif tag == "table":
            self.tables[self.headings[-1]].append([])
        elif tag == "tr":
            self.tables[self.headings[-1]][-1].append([])
        elif tag in ("td", "th"):
            self._cell = ""

    def handle_startendtag(self, tag, attrs):
        self.elements.add(tag)
        self.attributes.extend(attrs)

    def handle_endtag(self, tag):
        if tag == "h2":
            self.headings.append(self._text)
            self.tables[self._text] = []
            self._text = None
        elif tag in ("td", "th"):
            self.tables[self.headings[-1]][-1][-1].append(self._cell)
            self._cell = None

    def handle_data(self, data):
        if self._text is not None:
            self._text += data
        if self._cell is not None:
            self._cell += data


# What each table of the page must show, formatted here from daily.json alone.

def shown(value):
    return "\u2014" if value is None else value["shown"]


def known(value):
    return "unavailable" if value is None else str(value)


def table(headers, rows):
    return [list(headers)] + [[str(cell) for cell in row] for row in rows]


def expected_tables(doc):
    build, baseline = doc["build"], doc["baseline"]
    rows = [("Night", "%s (%s)" % (doc["tag"], build["status"])),
            ("Head", "%s %s" % (build["head_sha"], build["head_subject"]
                                or "(subject unavailable)")),
            ("Version", build["version"]), ("Previous night", build["previous_tag"] or "none"),
            ("Baseline", "%s %s" % (baseline["tag"], baseline["sha"])),
            ("Baseline published", baseline["published"] or "unavailable"),
            ("Commits since the baseline", known(baseline["commits_since"])),
            ("Pull requests since the baseline", known(baseline["pull_requests_since"])),
            ("Host", doc["host"]["machine_id"] or "unavailable")]
    rows += [("Archive " + entry["target"], "%s %s (%d bytes); af %s" % (
        entry["asset"], entry["sha256"], entry["bytes"],
        entry["af_sha256"] or "unavailable: " + entry["af_reason"]))
        for entry in build["targets"]]
    if build["skip_reason"]:
        rows.append(("Skipped", build["skip_reason"]))
    tables = {"Headline": [table(("Field", "Value"), rows)]}

    performance, part = doc["performance"], []
    if performance is not None:
        moved = [item for item in performance["instances"] if item["moved"]]
        if moved:
            part.append(table(nightly.PERFORMANCE_HEADERS, [
                (item["instance_id"], item["criticality"], shown(item["baseline_median"]),
                 shown(item["candidate_median"]), item["unit"], shown(item["relative_change"]),
                 "not computed" if item["interval"] is None else "[%s, %s]" % (
                     item["interval"]["lower"]["shown"], item["interval"]["upper"]["shown"]),
                 item["classification"] or item["status"], item["outcome"])
                for item in moved]))
        sentinel, coverage = performance["sentinel"], performance["coverage"]
        part.append(table(("Count", "Value"), [
            ("Unchanged instances (within tolerance or non-regression, passing)",
             performance["unchanged"]),
            ("Healthy sentinel shards", sentinel["healthy"]),
            ("Sentinel shards", sentinel["shards"]),
            ("Planned records matched", coverage["matched_records"]),
            ("Planned records", coverage["expected_records"]),
            ("Planned records missing", coverage["missing_records"])]))
        if performance["reasons"]:
            part.append(table(("Code", "Instance", "Detail"), [
                (item["code"], item["instance_id"] or "", item["detail"])
                for item in performance["reasons"]]))
    if doc["telemetry"] is not None:
        part.append(table(("Experiment", "Subject", "Level", "Metric", "Unit", "State",
                           "Median change (%)"), [
            (item["experiment"], item["subject"], item["level"], item["metric"], item["unit"],
             item["state"], shown(item["median_change_percent"]))
            for item in doc["telemetry"]["figures"]]))
    tables["Performance"] = part

    trend, part = doc["trend"], []
    if trend is not None:
        for item in trend["instances"]:
            part.append(table(("Night", "Baseline", "Candidate median", "Release"), [
                (point["tag"], point["baseline_tag"], shown(point["candidate_median"]),
                 point["release"] or "") for point in item["points"]]))
        if trend["refused"]:
            part.append(table(("Night", "Refused because"), [
                (item["tag"], item["reason"]) for item in trend["refused"]]))
    tables["Trend"] = part

    since, part = doc["since_last_night"], []
    if since is not None:
        if since["merged_pull_requests"]:
            part.append(table(("Pull request", "Title"), [
                ("#%d" % item["number"], item["title"])
                for item in since["merged_pull_requests"]]))
        if since["verdict_changes"]:
            part.append(table(("Instance", "Previous", "Now"), [
                (item["instance_id"], item["previous"], item["current"])
                for item in since["verdict_changes"]]))
    tables["Since last night"] = part

    backlog, part = doc["backlog"], []
    if backlog is not None:
        checks = backlog["head_checks"]
        part = [
            table(("Count", "Value"), [
                ("Open issues", backlog["open_issues"]),
                ("Issues opened in the last 24 h", backlog["opened_24h"]),
                ("Issues closed in the last 24 h", backlog["closed_24h"]),
                ("Stale issues (no update for 14 days)", backlog["stale_issues"]),
                ("Open pull requests", backlog["open_pull_requests"]),
                ("Draft pull requests", backlog["draft_pull_requests"]),
                ("The head's check runs", checks["total"]),
                ("The head's check runs succeeded", checks["success"]),
                ("The head's check runs failed", checks["failure"]),
                ("The head's check runs skipped", checks["skipped"]),
                ("The head's check runs pending", checks["pending"]),
                ("The head's other check runs", checks["other"])]),
            table(("Label", "Open issues"), [(item["name"], item["count"])
                                             for item in backlog["labels"]]),
            table(("Stale issue", "Title", "Last update"), [
                ("#%d" % item["number"], item["title"], item["updated"])
                for item in backlog["stale"]]),
            table(("Pull request", "Title", "Draft", "Mergeable state"), [
                ("#%d" % item["number"], item["title"], "yes" if item["draft"] else "no",
                 item["mergeable_state"]) for item in backlog["pull_requests"]])]
    tables["Backlog"] = part

    def fact(name, text):
        value = doc["machine"][name]
        return ("unavailable: " + value["reason"] if value["value"] is None
                else text(value["value"]))

    tables["Machine"] = [table(("Fact", "Value"), [
        ("Free bytes of builds_root's filesystem",
         fact("free_bytes", lambda value: "%d (%.1f GiB)" % (value, value / 2.0 ** 30))),
        ("Machine lock", fact("lock_held", lambda held: "held" if held else "free")),
        ("Failed systemd units", fact("failed_units", str))])]
    tables["Completeness"] = [table(("Section", "Available", "Why"), [
        (name, "yes" if doc["completeness"][name]["available"] else "no",
         doc["completeness"][name]["reason"]) for name in nightly.SECTIONS])]
    return tables


# The tests ---------------------------------------------------------------------------------

class Night(TempDir):
    """One builds root, nightly configuration and executor journal per test."""

    def setUp(self):
        TempDir.setUp(self)
        self.builds = self.path("builds")
        self.journal = Journal(self.path("executor", "nightly-journal"))
        machine = fixture("executor/valid-synthetic.json")
        machine.update(root=self.journal.root, journal_id="claw_nightly_1", repository="owner/af",
                       core=dict(machine["core"], tier="nightly", shards=4))
        self.executor_config = self.path("executor.toml")
        write(self.executor_config, toml(machine).encode())
        self.config = self.path("nightly.toml")
        self.write_config()
        self.github = FakeGitHub()
        self.host = FakeHost()

    def write_config(self, **report):
        values = {"executor_config": self.executor_config, "history_nights": 14,
                  "headline_instances": HEADLINE}
        values.update(report)
        doc = {"schema": "af.nightly-config/1", "repository": "owner/af",
               "mirror": self.path("mirror.git"), "builds_root": self.builds,
               "build_image": "sha256:" + "1" * 64, "qemu_aarch64": "/usr/bin/qemu-aarch64-static",
               "sign_key": self.path("sign.key"), "public_key": self.path("sign.pub"),
               "required_checks": ["check / lint"], "report": values}
        write(self.config, toml(doc).encode())

    def night(self, name="nightly-build/valid-built.json"):
        """The night's record and, for a built night, its two archives."""
        record = fixture(name)
        root = os.path.join(self.builds, record["date"])
        for entry in record["targets"]:
            archive = os.path.join(root, *entry["asset"].split("/"))
            os.makedirs(os.path.dirname(archive), exist_ok=True)
            # Committed bytes, not a fresh gzip stream: compression output varies between
            # Python/zlib versions, while these fixtures and the goldens must not.
            archived = read(os.path.join(ROOT, "fixtures", "performance", "nightly",
                                         "af-%s.tar.gz" % entry["target"].split("-")[0]))
            with open(archive, "wb") as stream:
                stream.write(archived)
            data = read(archive)
            entry.update(sha256=sha256_bytes(data), bytes=len(data))
        write(os.path.join(root, "nightly.json"), contracts.write_bytes(record, "nightly-build"))
        return record

    def shipped(self):
        return sha256_bytes(AF["x86_64-unknown-linux-musl"])

    def earlier_night(self, tag="nightly-2026.10.28", candidate=PREVIOUS):
        """An earlier built night's core original (by default the previous night's, on its
        head): a passing verdict on an older release."""
        verdict = passing_verdict(fixture("verdict/valid-synthetic-regression.json"))
        return self.journal.core(tag, OLDER_BASE, candidate, "v0.12.0", verdict)

    def tonight(self, candidate=HEAD, baseline=BASE, baseline_tag="v0.13.0", **options):
        """Tonight's core original, by default on the night's head and baseline, its builds
        record naming the shipped x86_64 af as the measured candidate."""
        options.setdefault("af", (self.shipped(), len(AF["x86_64-unknown-linux-musl"])))
        return self.journal.core("nightly-2026.10.29", baseline, candidate, baseline_tag,
                                 fixture("verdict/valid-synthetic-regression.json"), **options)

    def compose(self, date="2026-10-29", output=None, github=None):
        output = output or self.path("out")
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = nightly.main(["compose", "--config", self.config, "--date", date,
                                 "--output", output],
                                github=github or self.github.client(), host=self.host,
                                clock=lambda: COMPOSED_MS)
        return code, output, stderr.getvalue()

    def tree(self):
        found = {}
        for directory, _, files in os.walk(self.tmp):
            for name in files:
                info = os.lstat(os.path.join(directory, name))
                found[os.path.join(directory, name)] = (info.st_size, info.st_mtime_ns)
        return found

    def composed(self, **options):
        before = self.tree()
        code, output, err = self.compose(**options)
        self.assertEqual(code, 0, err)
        self.assertEqual(sorted(os.listdir(output)), ["daily.json", "digest.md", "report.html"])
        # The three files are all it wrote, and it changed nothing it read.
        after = self.tree()
        self.assertEqual(sorted(set(after) - set(before)),
                         [os.path.join(output, name) for name in ("daily.json", "digest.md",
                                                                  "report.html")])
        self.assertEqual(dict((path, after[path]) for path in before), before)
        doc = contracts.read_json(read(os.path.join(output, "daily.json")), "nightly-daily")
        return doc, read(os.path.join(output, "report.html")), read(os.path.join(output,
                                                                                 "digest.md"))

    def golden(self, name, doc, digest):
        """daily.json and digest.md equal their goldens, side by side in the schema fixtures
        and indexed together, with this test's temporary paths written as `TMP`."""
        text = json.dumps(doc, indent=2, sort_keys=True).replace(self.tmp, "TMP") + "\n"
        digest_text = digest.decode("utf-8").replace(self.tmp, "TMP")
        index = json.loads(read(os.path.join(SCHEMA, "index.json")).decode())["fixtures"]
        entry = next(entry for entry in index
                     if entry["path"] == "nightly-daily/valid-%s.json" % name)
        self.assertEqual(entry["digest"], "nightly-daily/digest-%s.md" % name)
        self.assertEqual(json.loads(text), json.loads(read(os.path.join(
            SCHEMA, entry["path"])).decode()))
        self.assertEqual(digest_text, read(os.path.join(SCHEMA, entry["digest"])).decode())

    def assert_page(self, doc, page):
        """The page's sections in order, its lab CSP and bundled script and style only, the
        renderer's elements only, and every cell of every table equal to the record's value
        formatted by `expected_tables`."""
        parsed = Page(page)
        self.assertEqual(parsed.headings, ["Headline", "Performance", "Trend",
                                           "Since last night", "Backlog", "Machine",
                                           "Completeness"])
        self.assertIn(('content', render.CSP), parsed.attributes)
        self.assertEqual(page.count(b"<script>"), 1)
        self.assertEqual(page.count(b"<style>"), 1)
        self.assertIn(("<style>%s</style>" % render.STYLE).encode(), page)
        self.assertIn(("<script>%s</script>" % render.SCRIPT).encode(), page)
        self.assertLessEqual(parsed.elements, RENDERED)
        for name, value in parsed.attributes:
            self.assertFalse(name.startswith("on"), name)
            if name != "xmlns":
                self.assertNotIn("http", value or "", name)
        self.assertEqual(parsed.tables, expected_tables(doc))


class CompleteNight(Night):
    def test_a_complete_night_composes_its_record_page_and_digest(self):
        self.night()
        self.earlier_night()
        self.tonight()
        self.journal.telemetry("nightly-2026.10.29", BASE, HEAD, "v0.13.0")
        doc, page, digest = self.composed()
        self.assertEqual(doc["schema"], DAILY_SCHEMA)
        self.assertEqual(doc["headline"]["label"], "FAIL")
        self.assertTrue(all(entry["available"] for entry in doc["completeness"].values()),
                        doc["completeness"])
        performance = doc["performance"]
        self.assertEqual([item["instance_id"] for item in performance["instances"]
                          if item["moved"]], ["synthetic_fast"])
        moved = performance["instances"][1]
        self.assertEqual((moved["relative_change"], moved["interval"]["lower"]["shown"]),
                         ({"exact": "50", "shown": "50.0"}, "10.0"))
        self.assertEqual(performance["unchanged"], 2)
        self.assertIs(performance["measured_is_shipped"], True)
        self.assertEqual(doc["build"]["targets"][0]["af_sha256"], self.shipped())
        self.assertEqual(doc["baseline"], {"tag": "v0.13.0", "sha": BASE,
                                           "published": "2026-10-20", "commits_since": 4,
                                           "pull_requests_since": 2, "reasons": []})
        self.assertEqual(doc["since_last_night"]["merged_pull_requests"],
                         [{"number": 41, "title": "Measure the nightly"}])
        self.assertEqual(doc["since_last_night"]["verdict_changes"],
                         [{"instance_id": "synthetic_fast", "previous": "within_tolerance",
                           "current": "regression"}])
        fast = doc["trend"]["instances"][0]
        self.assertEqual([(point["tag"], point["candidate_median"]["shown"], point["release"])
                          for point in fast["points"]],
                         [("nightly-2026.10.28", "19.0", None),
                          ("nightly-2026.10.29", "30.0", "v0.13.0")])
        self.assertEqual(doc["backlog"]["open_issues"], 2)
        self.assertEqual((doc["backlog"]["opened_24h"], doc["backlog"]["closed_24h"],
                          doc["backlog"]["stale_issues"]), (1, 1, 1))
        self.assertEqual(doc["backlog"]["head_checks"]["pending"], 1)
        self.assertEqual(doc["telemetry"]["figures"][0]["median_change_percent"]["exact"], "0")
        self.assertTrue(len(digest) <= nightly.DIGEST_BYTES_MAX)
        self.assertEqual(digest.decode("utf-8").split("\n", 1)[0], doc["headline"]["text"])
        self.assertIn("\nhttps://github.com/owner/af/commit/%s\n" % HEAD, digest.decode())
        self.assert_page(doc, page)
        self.golden("complete", doc, digest)
        # The host's facts are asked about builds_root and the executor's machine lock.
        self.assertEqual(self.host.asked, ["free_bytes " + self.builds,
                                           "lock_held " + host_lock.default_path(),
                                           "failed_units"])

    def test_a_measured_candidate_other_than_the_shipped_af_says_so(self):
        self.night()
        self.tonight(af=("sha256:" + "c" * 64, 24))
        doc, page, _ = self.composed()
        self.assertIs(doc["performance"]["measured_is_shipped"], False)
        self.assertEqual(doc["performance"]["measured_candidate_sha256"], "sha256:" + "c" * 64)
        self.assertIn(b"is not the shipped x86_64-unknown-linux-musl af", page)
        self.assert_page(doc, page)

    def test_the_page_fails_its_structure_check_when_a_number_differs_from_the_record(self):
        self.night()
        self.earlier_night()
        self.tonight()
        self.journal.telemetry("nightly-2026.10.29", BASE, HEAD, "v0.13.0")
        doc, page, _ = self.composed()
        self.assert_page(doc, page)
        moved = next(index for index, item in enumerate(doc["performance"]["instances"])
                     if item["moved"])
        for path, value in ((("baseline", "commits_since"), 999),
                            (("baseline", "pull_requests_since"), 888),
                            (("backlog", "open_pull_requests"), 7),
                            (("backlog", "head_checks", "pending"), 5),
                            (("performance", "unchanged"), 9),
                            (("performance", "coverage", "missing_records"), 3),
                            (("performance", "instances", moved, "candidate_median", "shown"),
                             "31.0"),
                            (("performance", "instances", moved, "interval", "upper", "shown"),
                             "11.0"),
                            (("trend", "instances", 0, "points", 0, "candidate_median", "shown"),
                             "18.0"),
                            (("telemetry", "figures", 0, "median_change_percent"),
                             {"exact": "1", "shown": "1.00"}),
                            (("machine", "free_bytes", "value"), 1)):
            with self.subTest(path=path):
                other = copy.deepcopy(doc)
                where = other
                for key in path[:-1]:
                    where = where[key]
                where[path[-1]] = value
                with self.assertRaises(AssertionError):
                    self.assert_page(other, page)

    def test_evidence_with_html_metacharacters_renders_escaped(self):
        self.night()
        self.tonight()
        github = FakeGitHub()
        github.SUBJECT = '<script>alert("head")</script> & <b onclick="x()">bold</b>'
        doc, page, _ = self.composed(github=github.client())
        self.assertEqual(doc["build"]["head_subject"], github.SUBJECT)
        self.assertEqual(page.count(b"<script>"), 1)
        self.assertNotIn(b"<b ", page)
        self.assertIn(b"&lt;script&gt;alert(&quot;head&quot;)&lt;/script&gt; &amp; "
                      b"&lt;b onclick=&quot;x()&quot;&gt;bold&lt;/b&gt;", page)
        self.assertIn(b"A &lt;script&gt; in a title", page)
        self.assert_page(doc, page)

    def test_the_composer_writes_no_html_of_its_own(self):
        # Every element of report.html comes from perflab.render: report.py holds no tag.
        source = read(REPORT).decode("utf-8")
        self.assertEqual(re.findall(r"<[A-Za-z!/?]", source), [])


class SkippedNight(Night):
    def test_a_skipped_night_composes_the_short_report(self):
        self.night("nightly-build/valid-skipped.json")
        self.earlier_night()
        doc, page, digest = self.composed()
        for name in ("performance", "telemetry", "trend", "since_last_night"):
            self.assertIsNone(doc[name])
            self.assertFalse(doc["completeness"][name]["available"])
            self.assertIn("no new commits reached main since nightly-2026.10.28",
                          doc["completeness"][name]["reason"])
        self.assertEqual(doc["headline"]["label"], "UNAVAILABLE")
        self.assertIn("stands on nightly-2026.10.28", doc["headline"]["text"])
        self.assertIsNotNone(doc["backlog"])
        self.assertTrue(digest.decode().startswith(doc["headline"]["text"] + "\n"))
        self.assert_page(doc, page)
        self.golden("skipped", doc, digest)


class NightWithoutCoreResult(Night):
    def test_a_night_whose_core_original_has_no_result_composes_without_performance(self):
        self.night()
        self.earlier_night()
        self.journal.core("nightly-2026.10.29", BASE, HEAD, "v0.13.0",
                          fixture("verdict/valid-synthetic-regression.json"), state="queued")
        doc, page, digest = self.composed()
        self.assertIsNone(doc["performance"])
        self.assertEqual(doc["headline"]["label"], "UNAVAILABLE")
        self.assertIn("is queued: the claw executor has not run it yet",
                      doc["completeness"]["performance"]["reason"])
        self.assertIn("this night has no verified core verdict",
                      doc["completeness"]["since_last_night"]["reason"])
        self.assertEqual([len(item["points"]) for item in doc["trend"]["instances"]], [1, 1])
        self.assertTrue(digest.decode().startswith(doc["headline"]["text"] + "\n"))
        self.assert_page(doc, page)
        self.golden("no-core-result", doc, digest)

    def test_a_night_without_its_record_is_refused_and_writes_nothing(self):
        code, output, err = self.compose()
        self.assertEqual(code, 1)
        self.assertIn(os.path.join(self.builds, "2026-10-29", "nightly.json"), err)
        self.assertFalse(os.path.lexists(output))
        self.assertEqual(self.github.paths, [])

    def test_an_existing_output_is_refused_and_left_alone(self):
        self.night()
        os.makedirs(self.path("out"))
        code, output, err = self.compose()
        self.assertEqual(code, 1)
        self.assertIn("exists", err)
        self.assertEqual(os.listdir(output), [])

    def test_a_configuration_without_its_report_table_is_refused(self):
        self.night()
        text = read(self.config).decode()
        write(self.config, text[:text.index("\n[report]")].encode())
        code, output, err = self.compose()
        self.assertEqual(code, 1)
        self.assertIn("has no [report] table", err)
        self.assertFalse(os.path.lexists(output))


class ReportRoot(Night):
    """`--output-root` and the configuration's `reports_root`, and the night defaulted to
    today's UTC date (ADR-0273)."""

    def run_main(self, *argv, clock=COMPOSED_MS):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = nightly.main(["compose", "--config", self.config] + list(argv),
                                github=self.github.client(), host=self.host,
                                clock=lambda: clock)
        return code, out.getvalue(), err.getvalue()

    def test_each_compose_takes_the_date_then_the_smallest_free_suffix_and_prints_it(self):
        self.night()
        self.tonight()
        root = self.path("reports")
        os.mkdir(root)
        os.mkdir(os.path.join(root, "2026-10-29.3"))
        written = []
        for expected in ("2026-10-29", "2026-10-29.2", "2026-10-29.4"):
            code, out, err = self.run_main("--date", "2026-10-29", "--output-root", root)
            self.assertEqual(code, 0, err)
            self.assertEqual(out, os.path.join(root, expected) + "\n")
            self.assertIn("nightly: composed %s: " % os.path.join(root, expected), err)
            written.append(read(os.path.join(root, expected, "daily.json")))
            self.assertEqual(sorted(os.listdir(os.path.join(root, expected))),
                             ["daily.json", "digest.md", "report.html"])
        self.assertEqual(sorted(os.listdir(root)), ["2026-10-29", "2026-10-29.2",
                                                    "2026-10-29.3", "2026-10-29.4"])
        self.assertEqual(os.listdir(os.path.join(root, "2026-10-29.3")), [])
        self.assertEqual(len(set(written)), 1)

    def test_without_date_or_directory_the_night_and_root_are_the_defaults_and_said(self):
        self.night()
        self.tonight()
        root = self.path("reports")
        os.mkdir(root)
        write(self.config, ('reports_root = "%s"\n' % root).encode() + read(self.config))
        # 2026-10-29T15:00:00Z.
        code, out, err = self.run_main()
        self.assertEqual(code, 0, err)
        self.assertEqual(out, os.path.join(root, "2026-10-29") + "\n")
        self.assertIn("nightly: --date 2026-10-29 (today's UTC date)\n", err)
        self.assertIn("nightly: --output-root %s (the configuration's reports_root)\n" % root,
                      err)
        doc = contracts.read_json(read(os.path.join(root, "2026-10-29", "daily.json")),
                                  "nightly-daily")
        self.assertEqual(doc["tag"], "nightly-2026.10.29")
        # An explicit directory is used as given, and the root says nothing.
        code, out, err = self.run_main("--date", "2026-10-29", "--output", self.path("out"))
        self.assertEqual((code, out), (0, self.path("out") + "\n"), err)
        self.assertNotIn("reports_root", err)

    def test_no_directory_no_root_or_both_is_refused_and_writes_nothing(self):
        self.night()
        before = sorted(os.listdir(self.tmp))
        code, out, err = self.run_main("--date", "2026-10-29")
        self.assertEqual((code, out), (1, ""))
        self.assertIn("nightly: refused: compose needs --output, --output-root or the "
                      "configuration's reports_root", err)
        code, _, err = self.run_main("--date", "2026-10-29", "--output-root",
                                     self.path("absent"))
        self.assertEqual(code, 1)
        self.assertIn("--output-root %s is not a directory" % self.path("absent"), err)
        self.assertEqual(sorted(os.listdir(self.tmp)), before)
        self.assertEqual(self.github.paths, [])
        with contextlib.redirect_stderr(io.StringIO()) as err:
            with self.assertRaises(SystemExit) as caught:
                nightly.parse(["compose", "--config", self.config, "--output", self.path("a"),
                               "--output-root", self.tmp])
        self.assertEqual(caught.exception.code, 1)
        self.assertIn("not allowed with argument", err.getvalue())


class JournalRefusals(Night):
    def refused(self):
        doc, page, _ = self.composed()
        self.assertIsNone(doc["performance"])
        self.assertIn(b"Unavailable: the original nightly:nightly-2026.10.29:core is refused",
                      page)
        return doc["completeness"]["performance"]["reason"]

    def test_a_verdict_of_another_plan_is_refused_not_shown(self):
        self.night()

        def other_plan(verdict, plan):
            verdict["plan_digest"] = "sha256:" + "4" * 64
            return report_of(verdict, plan)

        self.tonight(report=other_plan)
        self.assertIn("its verdict names another plan", self.refused())

    def test_a_report_member_with_another_hash_is_refused(self):
        self.night()

        def tampered(verdict, plan):
            doc = report_of(verdict, plan)
            doc["downloads"][0]["sha256"] = "sha256:" + "5" * 64
            return doc

        self.tonight(report=tampered)
        self.assertIn("lists verdict.json with another hash than the sealed file",
                      self.refused())

    def test_evidence_changed_after_its_result_sealed_it_is_refused(self):
        self.night()
        ident = self.tonight()
        path = self.journal.path(ident, "report", "verdict.json")
        write(path, read(path).replace(b'"fail"', b'"pass"', 1))
        self.assertIn("changed after the result sealed them", self.refused())

    def test_a_request_the_journal_did_not_index_is_never_read(self):
        self.night()
        ident = self.tonight()
        path = self.journal.path(ident, "request.json")
        request = json.loads(read(path).decode())
        request["created_unix_ms"] += 1
        write(path, contracts.write_bytes(request, "executor-request"))
        doc, _, _ = self.composed()
        self.assertIn("holds no original nightly:nightly-2026.10.29:core",
                      doc["completeness"]["performance"]["reason"])

    def test_a_journal_of_another_executor_is_refused(self):
        self.night()
        self.tonight()
        self.journal.index["journal_id"] = "claw_originals_1"
        self.journal.flush()
        doc, page, _ = self.composed()
        self.assertIsNone(doc["performance"])
        self.assertIn("is 'claw_originals_1' of 'owner/af', not the configured 'claw_nightly_1' "
                      "of 'owner/af'", doc["completeness"]["performance"]["reason"])
        self.assertIsNone(doc["trend"])

    def test_an_indexed_request_naming_another_original_is_refused(self):
        from perflab.values import canonical_digest
        self.night()
        ident = self.tonight()
        path = self.journal.path(ident, "request.json")
        request = json.loads(read(path).decode())
        # Indexed as `ident`, the request names another directory; its digest is re-indexed so
        # the digest check alone would admit it.
        request["request_id"] = ident[:-1] + ("0" if ident[-1] != "0" else "1")
        write(path, contracts.write_bytes(request, "executor-request"))
        for item in self.journal.index["originals"]:
            if item["request_id"] == ident:
                item["request_digest"] = canonical_digest(request)
        self.journal.flush()
        doc, _, _ = self.composed()
        self.assertIsNone(doc["performance"])
        self.assertIn("holds no original nightly:nightly-2026.10.29:core",
                      doc["completeness"]["performance"]["reason"])

    def test_an_output_directory_swapped_for_a_link_after_its_creation_gets_no_file(self):
        self.night()
        self.tonight()
        target = self.path("elsewhere")
        os.mkdir(target)
        moved = self.path("moved")

        def swap(path):
            os.rename(path, moved)
            os.symlink(target, path)

        nightly._after_mkdir = swap
        try:
            code, output, err = self.compose(output=self.path("out"))
        finally:
            nightly._after_mkdir = None
        self.assertEqual(code, 1)
        self.assertIn("is not the directory this report created", err)
        self.assertEqual(os.listdir(target), [])
        self.assertEqual(os.listdir(moved), [])

    def test_an_output_directory_swapped_for_a_real_directory_gets_no_file(self):
        self.night()
        self.tonight()
        replacement = self.path("replacement")
        os.mkdir(replacement)
        moved = self.path("moved-real")

        def swap(path):
            os.rename(path, moved)
            os.rename(replacement, path)

        nightly._after_mkdir = swap
        try:
            code, output, err = self.compose(output=self.path("out-real"))
        finally:
            nightly._after_mkdir = None
        self.assertEqual(code, 1)
        self.assertIn("is not the directory this report created", err)
        self.assertEqual(os.listdir(moved), [])
        self.assertEqual(os.listdir(os.path.join(self.tmp, "out-real")), [])

    def test_an_unknown_headline_instance_leaves_the_trend_unavailable(self):
        self.night()
        self.tonight()
        self.write_config(headline_instances=["no_such_instance"])
        doc, _, _ = self.composed()
        self.assertIsNone(doc["trend"])
        self.assertIn("no_such_instance is no instance of this night's verdict",
                      doc["completeness"]["trend"]["reason"])
        self.assertIsNotNone(doc["performance"])


class Binding(Night):
    """An original under the night's tag is evidence of this build only when it measured the
    night's head against its baseline, and its measured binaries are its builds record's."""

    def unavailable(self, name):
        doc, page, _ = self.composed()
        self.assertIsNone(doc[name])
        self.assertFalse(doc["completeness"][name]["available"])
        self.assert_page(doc, page)
        return doc, doc["completeness"][name]["reason"]

    def test_a_core_original_of_another_candidate_is_not_this_nights_performance(self):
        self.night()
        self.earlier_night()
        self.tonight(candidate=OLDER_HEAD)
        doc, reason = self.unavailable("performance")
        self.assertIn("it measured the candidate %s, not the night's head %s"
                      % (OLDER_HEAD, HEAD), reason)
        self.assertEqual(doc["headline"]["label"], "UNAVAILABLE")
        # Neither the trend nor the verdict changes use it.
        self.assertEqual([[point["tag"] for point in item["points"]]
                          for item in doc["trend"]["instances"]],
                         [["nightly-2026.10.28"]] * 2)
        self.assertEqual(doc["trend"]["refused"], [{"tag": "nightly-2026.10.29",
                                                    "reason": reason}])
        self.assertIsNone(doc["since_last_night"]["verdict_changes"])

    def test_a_core_original_against_another_baseline_is_refused(self):
        self.night()
        self.tonight(baseline=OLDER_BASE, baseline_tag="v0.12.0")
        doc, reason = self.unavailable("performance")
        self.assertIn("it measured against the baseline v0.12.0 (%s), not the build's v0.13.0 "
                      "(%s)" % (OLDER_BASE, BASE), reason)
        self.assertEqual((doc["baseline"]["tag"], doc["baseline"]["sha"]), ("v0.13.0", BASE))

    def test_a_telemetry_original_of_another_candidate_is_refused(self):
        self.night()
        self.tonight()
        self.journal.telemetry("nightly-2026.10.29", BASE, OLDER_HEAD, "v0.13.0")
        _, reason = self.unavailable("telemetry")
        self.assertIn("nightly:nightly-2026.10.29:telemetry is refused: it measured the "
                      "candidate %s" % OLDER_HEAD, reason)

    def test_the_previous_nights_verdict_counts_only_on_the_previous_head(self):
        self.night()
        self.earlier_night(candidate=OLDER_HEAD)
        self.tonight()
        doc, _, _ = self.composed()
        self.assertIsNone(doc["since_last_night"]["verdict_changes"])
        self.assertIn("verdict changes: the original nightly:nightly-2026.10.28:core is "
                      "refused: it measured the candidate %s, not the night's head %s"
                      % (OLDER_HEAD, PREVIOUS), doc["completeness"]["since_last_night"]["reason"])

    def test_result_binaries_other_than_its_builds_record_are_refused(self):
        self.night()
        self.tonight(binaries={"baseline": {"sha256": STABLE_AF[0], "bytes": STABLE_AF[1]},
                               "candidate": {"sha256": "sha256:" + "f" * 64, "bytes": 24}})
        doc, reason = self.unavailable("performance")
        self.assertIn("its result's binaries are not the ones its sealed run/builds.json names",
                      reason)
        self.assertEqual(doc["headline"]["label"], "UNAVAILABLE")

    def test_a_core_original_without_its_builds_record_is_refused(self):
        self.night()
        ident = self.tonight()
        os.remove(self.journal.path(ident, "run", "builds.json"))
        _, reason = self.unavailable("performance")
        self.assertIn("changed after the result sealed them", reason)


class Trend(Night):
    def test_the_trend_holds_the_last_earlier_nights_by_tag_date_and_tonight(self):
        self.night()
        for day in ("21", "25", "27"):
            self.earlier_night("nightly-2026.10.%s" % day, OLDER_HEAD)
        self.tonight()
        self.write_config(history_nights=2)
        doc, page, _ = self.composed()
        self.assertEqual(doc["trend"]["nights"], 2)
        for item in doc["trend"]["instances"]:
            self.assertEqual([point["tag"] for point in item["points"]],
                             ["nightly-2026.10.25", "nightly-2026.10.27", "nightly-2026.10.29"])
        self.assertEqual(doc["trend"]["refused"], [])
        self.assertIn(b"Up to 2 earlier nights plus this night.", page)
        self.assert_page(doc, page)


class Backlog(Night):
    def pulls(self, count):
        return [pull(100 + index, "Open pull request %d" % index, None,
                     "2026-10-29T11:00:00Z", draft=index % 3 == 0) for index in range(count)]

    def test_every_open_pull_request_carries_its_mergeable_state(self):
        self.night()
        github = FakeGitHub(open_pulls=self.pulls(31))
        doc, page, _ = self.composed(github=github.client())
        backlog = doc["backlog"]
        self.assertEqual(backlog["open_pull_requests"], 31)
        self.assertEqual([item["number"] for item in backlog["pull_requests"]],
                         list(range(100, 131)))
        self.assertTrue(all(item["mergeable_state"] == "clean"
                            for item in backlog["pull_requests"]))
        self.assertIsNone(backlog["pull_requests_partial"])
        self.assertEqual(sum(1 for path in github.paths if re.match(
            r"^/repos/owner/af/pulls/[0-9]+$", path)), 31)
        self.assert_page(doc, page)

    def test_more_open_pull_requests_than_the_bound_are_a_recorded_partial_read(self):
        self.night()
        github = FakeGitHub(open_pulls=self.pulls(nightly.PULL_REQUESTS_MAX + 1))
        doc, page, _ = self.composed(github=github.client())
        backlog = doc["backlog"]
        self.assertEqual(backlog["open_pull_requests"], nightly.PULL_REQUESTS_MAX + 1)
        self.assertEqual(len(backlog["pull_requests"]), nightly.PULL_REQUESTS_MAX)
        self.assertEqual(backlog["pull_requests_partial"],
                         "101 open pull requests: the 100 lowest-numbered carry their mergeable "
                         "state, one request each")
        self.assertIn(b"Partly read: 101 open pull requests", page)
        self.assert_page(doc, page)


class Lock(TempDir):
    """The machine lock read from a /proc/locks the test writes."""

    def setUp(self):
        TempDir.setUp(self)
        self.lock = self.path("af-perf-machine.lock")
        write(self.lock, b"")
        info = os.stat(self.lock)
        self.record = ("1: FLOCK  ADVISORY  WRITE 4242 %02x:%02x:%d 0 EOF\n" % (
            os.major(info.st_dev), os.minor(info.st_dev), info.st_ino)).encode()
        self.other = b"2: POSIX  ADVISORY  WRITE 4343 00:1f:7 0 EOF\n"

    def held(self, data):
        write(self.path("locks"), data)
        host = nightly.Host(locks=self.path("locks"), system=lambda: "Linux")
        return nightly._fact(host.lock_held, self.lock)

    def test_a_lock_record_within_the_bound_is_read(self):
        self.assertEqual(self.held(self.other + self.record), {"value": True, "reason": None})
        self.assertEqual(self.held(self.other), {"value": False, "reason": None})

    def test_a_record_beyond_the_bound_is_unavailable_never_free(self):
        filler = self.other * (nightly.LOCKS_BYTES_MAX // len(self.other) + 1)
        fact = self.held(filler + self.record)
        self.assertIsNone(fact["value"])
        self.assertIn("holds more than %d bytes" % nightly.LOCKS_BYTES_MAX, fact["reason"])

    def test_another_system_than_linux_is_unavailable(self):
        host = nightly.Host(locks=self.path("locks"), system=lambda: "Darwin")
        fact = nightly._fact(host.lock_held, self.lock)
        self.assertIsNone(fact["value"])
        self.assertIn("only Linux has", fact["reason"])


class Unavailability(Night):
    def test_github_down_leaves_the_backlog_unavailable_and_still_composes(self):
        self.night()
        self.tonight()
        github = FakeGitHub(down=True)
        doc, page, digest = self.composed(github=github.client())
        self.assertIsNone(doc["backlog"])
        self.assertIn("network is unreachable", doc["completeness"]["backlog"]["reason"])
        self.assertFalse(doc["completeness"]["since_last_night"]["available"])
        self.assertIsNone(doc["since_last_night"]["merged_pull_requests"])
        self.assertIsNone(doc["build"]["head_subject"])
        self.assertEqual((doc["baseline"]["published"], doc["baseline"]["commits_since"]),
                         (None, None))
        self.assertIsNotNone(doc["performance"])
        self.assertIn(b"Unavailable: GitHub:", page)
        self.assertIn("\nQueue unavailable: GitHub: ", digest.decode())
        # Every request was tried twice, and none of them more.
        self.assertEqual(len(github.paths), 2 * len(set(github.paths)))

    def test_a_refusing_host_leaves_each_machine_fact_null_with_its_reason(self):
        self.night()
        self.host = FakeHost(refuse=True)
        doc, page, digest = self.composed()
        for name in ("free_bytes", "lock_held", "failed_units"):
            self.assertIsNone(doc["machine"][name]["value"])
            self.assertIn("refused by the fake host", doc["machine"][name]["reason"])
        self.assertFalse(doc["completeness"]["machine"]["available"])
        self.assertIn("Machine: ? free; machine lock ?; failed units: ?", digest.decode())

    def test_a_missing_executor_configuration_leaves_the_journal_sections_unavailable(self):
        self.night()
        os.remove(self.executor_config)
        doc, _, _ = self.composed()
        self.assertIsNone(doc["host"]["machine_id"])
        for name in ("performance", "telemetry", "trend"):
            self.assertIn("executor_config", doc["completeness"][name]["reason"])


class Digest(Night):
    def test_the_digest_stays_within_its_bound_however_much_moved(self):
        self.night()
        self.tonight()
        doc, _, _ = self.composed()
        instance = next(item for item in doc["performance"]["instances"] if item["moved"])
        doc["performance"]["instances"] = [
            dict(instance, instance_id="moved_instance_with_a_long_name_%03d" % index)
            for index in range(300)]
        data = nightly.digest_text(doc)
        self.assertTrue(len(data) <= nightly.DIGEST_BYTES_MAX, len(data))
        text = data.decode("utf-8")
        self.assertTrue(text.startswith(doc["headline"]["text"] + "\n"))
        self.assertRegex(text, r"\nand [0-9]+ more moved instances: see report.html\n")
        self.assertIn("\nQueue: ", text)
        self.assertIn("\nMachine: ", text)

    def test_numbers_are_shown_to_three_significant_digits(self):
        for exact, shown in (("21/2", "10.5"), ("50", "50.0"), ("1/3", "0.333"),
                             ("1234", "1230"), ("0", "0"), ("-5/2", "-2.50"),
                             ("9995/10", "1000"), ("1/1000", "0.00100")):
            self.assertEqual(three_significant(exact), shown)


class Contract(unittest.TestCase):
    def test_a_shown_number_that_is_not_its_exact_rational_is_refused(self):
        doc = json.loads(read(os.path.join(GOLDEN, "valid-complete.json")).decode())
        doc["performance"]["instances"][1]["relative_change"]["shown"] = "49.9"
        with self.assertRaises(ContractError) as caught:
            contracts.validate_nightly_daily(doc)
        self.assertIn("the exact rational to three significant digits", str(caught.exception))
