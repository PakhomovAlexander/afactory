#!/usr/bin/env python3
"""Compose the owner's daily nightly report from the night's record and the executor journal
(ADR-0271).

    python3 -I -B scripts/nightly/report.py compose --config PATH
                                                [--date YYYY-MM-DD | --night-of UNIT]
                                                [--output DIR | --output-root ROOT]

Without --date the night is the UTC date systemd last started UNIT, or else today's UTC date;
without --output the directory is ROOT/DATE, or ROOT/DATE.K with the smallest K >= 2 free, ROOT
being --output-root or the configuration's reports_root (ADR-0273). Every default is printed on
stderr, the directory written on stdout.
`compose` reads builds_root/DATE/nightly.json (af.nightly-build/1), the nightly executor's
journal (the originals keyed nightly:TAG:core and nightly:TAG:telemetry and the earlier nights'
core originals, each verified before use and bound to the night's build), its local telemetry
history, GitHub and three host facts, and writes into the new directory DIR exactly daily.json
(af.nightly-daily/1), report.html (rendered by perflab.render alone) and digest.md (the
Telegram digest, at most 3500 bytes). Every number is copied or recomputed from validated
inputs; an input that is missing or refused leaves its section unavailable with the reason,
never a guess. It never pushes, publishes, posts, deletes, runs Docker or cargo.

Exit codes: 0 composed (whatever is unavailable), 1 refused with the reason on stderr: the
night has no nightly.json, the configuration or command line is malformed, DIR exists, or ROOT
is no directory or already holds REPORTS_PER_NIGHT_MAX reports of the night.
docs/nightly.md is the operator page.
"""
import argparse
import datetime
import hashlib
import os
import platform
import subprocess
import sys
import tarfile
import time

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(ROOT, "scripts", "performance"))
sys.path.insert(0, HERE)

import nightlychain  # noqa: E402
import nightlygithub  # noqa: E402
from perflab import contracts, executor, hostlock, render, tomlfile  # noqa: E402
from perflab import report as labreport  # noqa: E402
from perflab.nightlycontracts import (HISTORY_NIGHTS_DEFAULT, PULL_REQUESTS_MAX,  # noqa: E402
                                      SECTIONS, TELEMETRY_SHOWN, UNCHANGED, number)
from perflab.values import (ContractError, canonical_digest, document_bytes,  # noqa: E402
                            parse_rational, sha256_bytes)

EXIT_COMPOSED, EXIT_REFUSED = 0, 1
DAILY_JSON, REPORT_HTML, DIGEST_MD = "daily.json", "report.html", "digest.md"
DIGEST_BYTES_MAX = 3500
STALE_DAYS = 14
DAY_MS = 24 * 3600 * 1000
STALE_SHOWN_MAX = 20
# The most report directories one night may have below a reports root: DATE, DATE.2 and on.
REPORTS_PER_NIGHT_MAX = 99
AF_BYTES_MAX = 1 << 30
SYSTEMCTL_SECONDS = 30
PROC_LOCKS = "/proc/locks"
# The most of /proc/locks read; a longer file cannot show that a lock is free.
LOCKS_BYTES_MAX = 1 << 20
# The target whose af the nightly executor measures (environment linux_x86_64_musl_claw).
MEASURED_TARGET = "x86_64-unknown-linux-musl"
METRIC_UNITS = {"wall_ms": "ms", "drain_ms": "ms", "latency_ms": "ms",
                "peak_rss_bytes": "bytes"}
FAILED = ("failure", "timed_out", "cancelled", "action_required", "startup_failure", "stale")


class Refused(Exception):
    """The night cannot be composed; the message says why."""


class Unavailable(Exception):
    """One input is missing or refused: its section says why and the night still composes."""


# What a GitHub read may raise: its own failure, or a document of another shape than asked.
GITHUB_ERRORS = (Unavailable, KeyError, TypeError, AttributeError, IndexError)


def now_ms():
    return int(time.time() * 1000)


# Configuration ------------------------------------------------------------------------------

def load_config(path):
    """The af.nightly-config/1 TOML at `path` with its `[report]` table, defaults applied."""
    try:
        with open(path, "rb") as stream:
            data = stream.read(tomlfile.MAX_TOML_BYTES + 1)
    except OSError as error:
        raise Refused("--config %s: %s" % (path, error))
    doc = contracts.validate_nightly_config(tomlfile.loads(data, path), path)
    if "report" not in doc:
        raise Refused("%s has no [report] table: compose needs its executor_config and "
                      "headline_instances (docs/nightly.md)" % path)
    table = dict(doc["report"])
    table.setdefault("history_nights", HISTORY_NIGHTS_DEFAULT)
    return dict(doc, report=table)


def read_night(config, date):
    """The night's af.nightly-build/1 record; without it there is no night to report."""
    path = os.path.join(config["builds_root"], date, "nightly.json")
    if not os.path.isfile(path):
        raise Refused("the night %s has no record: %s does not exist" % (date, path))
    try:
        record = contracts.read_file(path, "nightly-build")
    except (OSError, ContractError) as error:
        raise Refused("%s: %s" % (path, error))
    if record["date"] != date:
        raise Refused("%s records the night %s" % (path, record["date"]))
    return record


def load_executor(path):
    """The nightly executor's configuration (af.perf-executor/2), or Unavailable."""
    try:
        return executor.load_config(path)
    except (OSError, ContractError) as error:
        raise Unavailable("executor_config %s: %s" % (path, error))


# The shipped binaries -----------------------------------------------------------------------

def shipped_af(config, record, entry):
    """The sha256 of the `af` inside one target's archive, read only after the archive's own
    size and digest match the night's record."""
    path = os.path.join(config["builds_root"], record["date"], *entry["asset"].split("/"))
    try:
        with open(path, "rb") as stream:
            digest, size = hashlib.sha256(), 0
            for chunk in iter(lambda: stream.read(1 << 20), b""):
                digest.update(chunk)
                size += len(chunk)
                if size > entry["bytes"]:
                    break
        if size != entry["bytes"] or "sha256:" + digest.hexdigest() != entry["sha256"]:
            raise Unavailable("%s is not the archive nightly.json records" % entry["asset"])
        with tarfile.open(path, "r:gz") as tar:
            member = tar.getmember("af")
            if not member.isreg() or member.size > AF_BYTES_MAX:
                raise Unavailable("%s holds no regular af" % entry["asset"])
            stream = tar.extractfile(member)
            binary = hashlib.sha256()
            for chunk in iter(lambda: stream.read(1 << 20), b""):
                binary.update(chunk)
        return "sha256:" + binary.hexdigest()
    except (OSError, KeyError, tarfile.TarError) as error:
        raise Unavailable("%s: %s" % (entry["asset"], error))


# The executor journal -----------------------------------------------------------------------

class Journal:
    """A read-only view of the nightly executor's journal (ADR-0240, ADR-0266). It takes no
    lock, creates nothing and reads only below the configured root, each file confined
    (no symlink followed); every original is bound to the journal index by its digest."""

    def __init__(self, root, journal_id, repository):
        self.root = root
        try:
            self.index = contracts.read_json(executor.read(os.path.join(root, "journal.json")),
                                             "executor-journal")
        except (OSError, ContractError) as error:
            raise Unavailable("the executor journal %s: %s" % (root, error))
        # The journal is the configured executor's own, not any journal at that path.
        found = (self.index.get("journal_id"), self.index.get("repository"))
        if found != (journal_id, repository):
            raise Unavailable("the executor journal %s is %r of %r, not the configured %r of %r"
                              % (root, found[0], found[1], journal_id, repository))
        self.nightly = {}
        self.refused = {}
        self.cores = {}
        for item in self.index["originals"]:
            ident = item["request_id"]
            try:
                request = self.read(ident, "request.json", "executor-request")
                if canonical_digest(request) != item["request_digest"]:
                    raise ContractError("its request changed after the journal indexed it")
                if request["request_id"] != ident:
                    raise ContractError("its request names another original, %s"
                                        % request["request_id"])
            except (OSError, ContractError) as error:
                self.refused[ident] = str(error)
                continue
            if request["submission"]["kind"] == "nightly":
                self.nightly[request["submission"]["key"]] = request

    def path(self, ident, *parts):
        contracts.ID.validate(ident, "request ID")
        return os.path.join(self.root, "requests", ident, *parts)

    def read(self, ident, name, contract):
        where = "%s/%s" % (ident, name)
        return contracts.read_json(executor.read(self.path(ident, *name.split("/"))), contract,
                                   where)

    def original(self, key):
        """(request, state, result, evidence) of the completed original `key`, every receipt
        bound to the request and every evidence file rehashed against the result's sealed
        list (as `executor verify` binds them); Unavailable with the reason otherwise."""
        request = self.nightly.get(key)
        if request is None:
            raise Unavailable("the executor journal holds no original %s" % key)
        ident = request["request_id"]
        try:
            state = self.read(ident, "state.json", "executor-state")
            if (state["request_id"], state["request_digest"]) != (ident,
                                                                  canonical_digest(request)):
                raise ContractError("its state belongs to another original")
            if state["state"] != "complete":
                raise Unavailable("the original %s is %s%s" % (
                    key, state["state"], ": " + state["detail"] if state["detail"] else ""))
            result = self.read(ident, "result.json", "executor-result")
            if sha256_bytes(document_bytes(result)) != state["result_sha256"]:
                raise ContractError("its result is not the one its state sealed")
            if (result["request_id"] != ident
                    or result["request_digest"] != canonical_digest(request)
                    or result["sources"] != request["sources"] or result["backend"] != "local"
                    or result["status"] != "complete" or result["actions"] is not None
                    or result["environment_digest"] != request["environment_digest"]
                    or result["producer_digest"] != request["producer_digest"]):
                raise ContractError("its result is not this local original's")
            path = self.path(ident)
            if executor.file_manifest(path, executor.EVIDENCE_PREFIXES) != result["evidence"]:
                raise ContractError("its evidence files or their bytes changed after the "
                                    "result sealed them")
        except (OSError, ContractError) as error:
            raise Unavailable("the original %s is refused: %s" % (key, error))
        return request, state, result, dict((entry["path"], entry)
                                            for entry in result["evidence"])

    def sealed(self, ident, evidence, name, contract):
        """The evidence file `name` (a fixed name, never one a document supplies), read once
        and accepted only with the hash the result sealed."""
        entry = evidence.get(name)
        if entry is None:
            raise ContractError("its evidence holds no %s" % name)
        data = executor.read(self.path(ident, *name.split("/")))
        if sha256_bytes(data) != entry["sha256"]:
            raise ContractError("%s is not the file its result sealed" % name)
        return contracts.read_json(data, contract, "%s/%s" % (ident, name)), entry

    def core(self, tag):
        """`verify_core` of the night `tag`, once per composition."""
        if tag not in self.cores:
            try:
                self.cores[tag] = self.verify_core(tag)
            except Unavailable as error:
                self.cores[tag] = error
        if isinstance(self.cores[tag], Unavailable):
            raise self.cores[tag]
        return self.cores[tag]

    def verify_core(self, tag):
        """The verified core original of the night `tag`: (request, result, plan, verdict,
        verdict digest, report). The plan is the request's frozen plan, the result's binaries
        are the ones the sealed run/builds.json of that plan names (as `executor verify` binds
        them), the verdict names that plan and the report carries that verdict and lists every
        report member with the hash the result sealed."""
        key = "nightly:%s:core" % tag
        request, _, result, evidence = self.original(key)
        ident = request["request_id"]
        try:
            if request["work"] != "core" or request["submission"]["tag"] != tag:
                raise ContractError("its request is no core nightly of %s" % tag)
            plan, _ = self.sealed(ident, evidence, "plan.json", "run")
            if sha256_bytes(contracts.write_bytes(plan, "run")) != request["plan_sha256"]:
                raise ContractError("plan.json is not the plan its request froze")
            if plan["resolved"] != request["sources"]:
                raise ContractError("its plan names other sources than its request")
            built, _ = self.sealed(ident, evidence, "run/builds.json", "builds")
            if (built["run_id"], built["plan_digest"]) != (plan["run_id"], plan["plan_digest"]):
                raise ContractError("its run/builds.json belongs to another plan")
            if result.get("binaries") != executor.built_binaries(built):
                raise ContractError("its result's binaries are not the ones its sealed "
                                    "run/builds.json names")
            verdict, entry = self.sealed(ident, evidence, "report/verdict.json", "verdict")
            if (verdict["run_id"], verdict["plan_digest"]) != (plan["run_id"],
                                                               plan["plan_digest"]):
                raise ContractError("its verdict names another plan (%s) than its request "
                                    "froze (%s)" % (verdict["plan_digest"], plan["plan_digest"]))
            if verdict["scope"] != plan["scope"]:
                raise ContractError("its verdict's scope is not its plan's")
            doc, _ = self.sealed(ident, evidence, "report/report.json", "report")
            if doc["verdict"] != verdict or doc["plan_digest"] != plan["plan_digest"]:
                raise ContractError("its report carries another verdict than verdict.json")
            if (doc["identity"]["baseline_sha"], doc["identity"]["candidate_sha"]) != (
                    plan["resolved"]["baseline_sha"], plan["resolved"]["candidate_sha"]):
                raise ContractError("its report names other sources than its plan")
            for download in doc["downloads"]:
                member = evidence.get("report/" + download["path"])
                if member is None or (member["sha256"], member["size"]) != (
                        download["sha256"], download["size"]):
                    raise ContractError("its report bundle lists %s with another hash than "
                                        "the sealed file" % download["path"])
        except (OSError, ContractError) as error:
            raise Unavailable("the original %s is refused: %s" % (key, error))
        return request, result, plan, verdict, entry["sha256"], doc

    def telemetry(self, record):
        """The verified telemetry original of the night `record` builds, bound to its head and
        baseline, and the figures the executor's local telemetry history holds for its result
        and sources."""
        tag = record["tag"]
        key = "nightly:%s:telemetry" % tag
        request, _, _, _ = self.original(key)
        if request["work"] != "telemetry" or request["submission"]["tag"] != tag:
            raise Unavailable("the original %s is refused: its request is no telemetry "
                              "nightly of %s" % (key, tag))
        bind(request, key, record["head_sha"], record)
        result_id = "local_" + request["request_id"][4:]
        path = os.path.join(self.root, "local-telemetry-history", "history.json")
        if not os.path.lexists(path):
            raise Unavailable("the executor root holds no local-telemetry-history/history.json")
        try:
            history = contracts.read_json(executor.read(path), "telemetry-history",
                                          "local-telemetry-history/history.json")
        except (OSError, ContractError) as error:
            raise Unavailable("local-telemetry-history/history.json: %s" % error)
        sources = request["sources"]
        figures = []
        for group in history["groups"]:
            for series in group["series"]:
                if series["experiment"] not in TELEMETRY_SHOWN:
                    continue
                for point in series["points"]:
                    if (point["result_id"], point["baseline_sha"], point["candidate_sha"]) != (
                            result_id, sources["baseline_sha"], sources["candidate_sha"]):
                        continue
                    change = point["median_change"]
                    figures.append({
                        "experiment": series["experiment"], "subject": series["subject"],
                        "level": series["level"], "metric": series["metric"],
                        "unit": METRIC_UNITS[series["metric"]], "state": point["state"],
                        "median_change_percent": None if change is None else number(
                            parse_rational(change, "median_change") * 100)})
        if not figures:
            raise Unavailable("the local telemetry history holds no overhead or read-latency "
                              "figure of %s (%s)" % (key, result_id))
        figures.sort(key=lambda item: (item["experiment"], item["subject"], item["level"],
                                       item["metric"]))
        return {"request_id": request["request_id"], "result_id": result_id, "figures": figures}


def bind(request, key, head, record=None):
    """Unavailable unless the original `key` measured the commit `head` and, given the night's
    `record`, against that night's baseline: an original under the night's tag that measured
    another commit is no evidence of this build."""
    sources, derived = request["sources"], request["submission"]["baseline"]
    if sources["candidate_sha"] != head:
        raise Unavailable("the original %s is refused: it measured the candidate %s, not the "
                          "night's head %s" % (key, sources["candidate_sha"], head))
    if record is not None and (sources["baseline_sha"] != record["baseline_sha"] or (
            derived is not None and (derived["tag"], derived["sha"])
            != (record["baseline_tag"], record["baseline_sha"]))):
        raise Unavailable("the original %s is refused: it measured against the baseline %s, "
                          "not the build's %s (%s)" % (
                              key, sources["baseline_sha"] if derived is None
                              else "%s (%s)" % (derived["tag"], derived["sha"]),
                              record["baseline_tag"], record["baseline_sha"]))


# GitHub -------------------------------------------------------------------------------------

class GitHub(nightlygithub.Client):
    """The reads the daily report makes; a failure leaves its section unavailable."""

    error = Unavailable
    agent = "af-nightly-report"

    def commit_subject(self, sha):
        doc = self.request("/repos/%s/commits/%s" % (self.repository, sha))
        message = doc.get("commit", {}).get("message") if isinstance(doc, dict) else None
        if not isinstance(message, str):
            raise Unavailable("GitHub commit %s: no message" % sha)
        return message.split("\n", 1)[0][:1024]

    def release_date(self, tag):
        doc = self.request("/repos/%s/releases/tags/%s" % (self.repository, tag))
        published = doc.get("published_at") if isinstance(doc, dict) else None
        return _date(published, "release %s published_at" % tag)

    def compare(self, base, head):
        """[(sha, committed unix ms)] of every commit after `base` up to `head`."""
        commits, total = [], None
        for page in range(1, nightlygithub.PAGES_MAX + 1):
            doc = self.request("/repos/%s/compare/%s...%s?per_page=%d&page=%d" % (
                self.repository, base, head, nightlygithub.PER_PAGE, page))
            batch = doc.get("commits") if isinstance(doc, dict) else None
            total = doc.get("total_commits") if isinstance(doc, dict) else None
            if not isinstance(batch, list) or type(total) is not int:
                raise Unavailable("GitHub compare %s...%s: not a comparison" % (base, head))
            for item in batch:
                try:
                    commits.append((item["sha"], _unix_ms(item["commit"]["committer"]["date"],
                                                          "commit date")))
                except (KeyError, TypeError):
                    raise Unavailable("GitHub compare %s...%s: a malformed commit" % (base,
                                                                                      head))
            if not batch or len(commits) >= total:
                break
        if len(commits) != total:
            raise Unavailable("GitHub compare %s...%s lists %d of its %s commits" % (
                base, head, len(commits), total))
        return commits

    def merged_into(self, commits):
        """[{number, title}] of the pull requests whose merge commit is one of `commits`,
        walking closed pull requests from the most recently updated back to the oldest of
        those commits (a day earlier, for clock skew)."""
        if not commits:
            return []
        shas = set(sha for sha, _ in commits)
        cutoff = min(when for _, when in commits) - DAY_MS
        closed = self.pages("/repos/%s/pulls?state=closed&sort=updated&direction=desc"
                            % self.repository,
                            until=lambda item: _unix_ms(item.get("updated_at"),
                                                        "pull updated_at") < cutoff)
        merged = [{"number": _number(item), "title": _text(item.get("title"))}
                  for item in closed if item.get("merge_commit_sha") in shas
                  and item.get("merged_at") is not None]
        return sorted(merged, key=lambda item: item["number"])

    def backlog(self, head, now):
        issues = [item for item in self.pages("/repos/%s/issues?state=open" % self.repository)
                  if "pull_request" not in item]
        labels = {}
        for item in issues:
            for label in item.get("labels") or []:
                name = label.get("name") if isinstance(label, dict) else None
                if isinstance(name, str) and name:
                    labels[name] = labels.get(name, 0) + 1
        day = now - DAY_MS
        since = datetime.datetime.fromtimestamp(day // 1000, datetime.timezone.utc)
        recent = [item for item in self.pages("/repos/%s/issues?state=all&since=%s" % (
            self.repository, since.strftime("%Y-%m-%dT%H:%M:%SZ"))) if "pull_request" not in item]
        stale_before = now - STALE_DAYS * DAY_MS
        stale = sorted((item for item in issues
                        if _unix_ms(item.get("updated_at"), "issue updated_at") < stale_before),
                       key=lambda item: (item["updated_at"], _number(item)))
        pulls = sorted(self.pages("/repos/%s/pulls?state=open" % self.repository),
                       key=_number)
        shown = []
        for item in pulls[:PULL_REQUESTS_MAX]:
            detail = self.request("/repos/%s/pulls/%d" % (self.repository, _number(item)))
            state = detail.get("mergeable_state") if isinstance(detail, dict) else None
            shown.append({"number": _number(item), "title": _text(item.get("title")),
                          "draft": item.get("draft") is True,
                          "mergeable_state": state if isinstance(state, str) and state
                          and len(state) <= 32 else "unknown"})
        partial = None if len(pulls) <= PULL_REQUESTS_MAX else (
            "%d open pull requests: the %d lowest-numbered carry their mergeable state, one "
            "request each" % (len(pulls), PULL_REQUESTS_MAX))
        checks = {"total": 0, "success": 0, "failure": 0, "skipped": 0, "pending": 0, "other": 0}
        for run in self.check_runs(head):
            checks["total"] += 1
            if run["status"] != "completed":
                checks["pending"] += 1
            elif run["conclusion"] == "success":
                checks["success"] += 1
            elif run["conclusion"] in ("skipped", "neutral"):
                checks["skipped"] += 1
            elif run["conclusion"] in FAILED:
                checks["failure"] += 1
            else:
                checks["other"] += 1
        return {"open_issues": len(issues),
                "labels": [{"name": name[:256], "count": labels[name]}
                           for name in sorted(labels)],
                "opened_24h": sum(1 for item in recent
                                  if _unix_ms(item.get("created_at"), "created_at") >= day),
                "closed_24h": sum(1 for item in recent if item.get("closed_at") is not None
                                  and _unix_ms(item["closed_at"], "closed_at") >= day),
                "stale_issues": len(stale),
                "stale": [{"number": _number(item), "title": _text(item.get("title")),
                           "updated": item["updated_at"][:10]}
                          for item in stale[:STALE_SHOWN_MAX]],
                "open_pull_requests": len(pulls),
                "draft_pull_requests": sum(1 for item in pulls if item.get("draft") is True),
                "pull_requests": shown, "pull_requests_partial": partial,
                "head_checks": checks}


def _unix_ms(text, where):
    try:
        moment = datetime.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ")
    except (TypeError, ValueError):
        raise Unavailable("GitHub %s %r is not a UTC time" % (where, text))
    return int(moment.replace(tzinfo=datetime.timezone.utc).timestamp()) * 1000


def _date(text, where):
    _unix_ms(text, where)
    return text[:10]


def _number(item):
    value = item.get("number")
    if type(value) is not int or not 1 <= value < 2 ** 31:
        raise Unavailable("GitHub answered an issue or pull request without its number")
    return value


def _text(value):
    return " ".join(value.split())[:1024] if isinstance(value, str) else ""


# Host facts ---------------------------------------------------------------------------------

class Host:
    """The three machine facts, each read without side effects; tests substitute a fake or
    another /proc/locks and system. A method raises Unavailable when the fact cannot be read
    here."""

    def __init__(self, locks=PROC_LOCKS, system=platform.system):
        self.locks = locks
        self.system = system

    def free_bytes(self, path):
        try:
            info = os.statvfs(path)
        except OSError as error:
            raise Unavailable("statvfs %s: %s" % (path, error))
        return info.f_bavail * info.f_frsize

    def lock_held(self, path):
        """Whether any process holds a lock on the machine lock's inode, read from
        /proc/locks: the report never takes the lock, so it never makes a cycle busy."""
        if self.system() != "Linux":
            raise Unavailable("the machine lock is read from /proc/locks, which only Linux has")
        try:
            info = os.stat(path)
            with open(self.locks, "rb") as stream:
                data = stream.read(LOCKS_BYTES_MAX + 1)
        except OSError as error:
            raise Unavailable("%s: %s" % (path, error))
        if len(data) > LOCKS_BYTES_MAX:
            raise Unavailable("%s holds more than %d bytes, so its read cannot show whether "
                              "%s is held" % (self.locks, LOCKS_BYTES_MAX, path))
        text = data.decode("ascii", "replace")
        wanted = "%02x:%02x:%d" % (os.major(info.st_dev), os.minor(info.st_dev), info.st_ino)
        return any(wanted in line.split() for line in text.splitlines())

    def failed_units(self):
        try:
            done = subprocess.run(["systemctl", "--failed", "--plain", "--no-legend"],
                                  stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=SYSTEMCTL_SECONDS,
                                  env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"})
        except (OSError, subprocess.TimeoutExpired) as error:
            raise Unavailable("systemctl --failed: %s" % (str(error) or type(error).__name__))
        if done.returncode != 0:
            raise Unavailable("systemctl --failed exited %d" % done.returncode)
        return sum(1 for line in done.stdout.decode("utf-8", "replace").splitlines()
                   if line.strip())


def _fact(read, *args):
    try:
        return {"value": read(*args), "reason": None}
    except Unavailable as error:
        return {"value": None, "reason": str(error)[:2048]}


# Composition --------------------------------------------------------------------------------

def _reason(error):
    text = str(error) or type(error).__name__
    return text if len(text) <= 2048 else text[:2045] + "..."


def performance_section(core, shipped):
    request, result, plan, verdict, verdict_sha256, doc = core
    aggregate = verdict["aggregate"]
    instances = []
    for entry in verdict["cases"]:
        change = entry["change"]
        moved = entry["status"] not in UNCHANGED or entry["outcome"] != "pass"
        instances.append({
            "instance_id": entry["instance_id"], "criticality": entry["criticality"],
            "unit": entry["metric"]["unit"],
            "baseline_median": None if entry["baseline"] is None
            else number(entry["baseline"]["median"]),
            "candidate_median": None if entry["candidate"] is None
            else number(entry["candidate"]["median"]),
            "relative_change": None if change is None or change["percent"] is None
            else number(change["percent"]),
            "interval": None if entry["interval"] is None else {
                "lower": number(entry["interval"]["lower"]),
                "upper": number(entry["interval"]["upper"])},
            "status": entry["status"], "classification": entry["statistical_classification"],
            "outcome": entry["outcome"], "moved": moved})
    flags = sorted(set(flag for entry in verdict["health"] for flag in entry["flags"]))
    binaries = result.get("binaries")
    measured = None if binaries is None or binaries["candidate"] is None \
        else binaries["candidate"]["sha256"]
    if measured is None:
        why = "the core original's result names no measured candidate af"
    elif shipped is None:
        why = "the shipped %s af is unavailable" % MEASURED_TARGET
    else:
        why = None
    return {
        "request_id": request["request_id"], "run_id": plan["run_id"],
        "plan_digest": plan["plan_digest"], "verdict_sha256": verdict_sha256,
        "result": aggregate["result"], "exit_code": aggregate["exit_code"],
        "scope": verdict["scope"], "provenance": verdict["provenance"],
        "headline": labreport.headline(doc)[:2048],
        "satisfies_required_check": aggregate["satisfies_required_check"],
        "reasons": [{"code": item["code"], "instance_id": item["instance_id"],
                     "detail": item["detail"]} for item in aggregate["reasons"][:64]],
        "instances": instances,
        "unchanged": sum(1 for item in instances if not item["moved"]),
        "coverage": dict(aggregate["coverage"]),
        "sentinel": {"shards": len(verdict["health"]),
                     "healthy": sum(1 for entry in verdict["health"]
                                    if entry["status"] == "healthy"),
                     "flags": flags[:64]},
        "measured_candidate_sha256": measured, "shipped_sha256": shipped,
        "measured_is_shipped": None if why else measured == shipped,
        "measured_reason": why}


def baseline_name(request):
    derived = request["submission"]["baseline"]
    return derived["tag"] if derived is not None else request["sources"]["baseline_sha"][:12]


def trend_section(journal, tag, nights, headline, current, current_reason):
    """Per headline instance, one point for each of the last `nights` earlier nights the
    journal holds a core original of (by tag date, however far apart) and one for this night,
    in night order, each that night's own candidate median; nothing is paired across nights
    (ADR-0172). This night's point is its bound core original `current` alone; without it the
    night is refused with `current_reason`."""
    known = set(entry["instance_id"] for entry in current[3]["cases"]) if current else None
    unknown = [name for name in headline if known is not None and name not in known]
    if unknown:
        raise Unavailable("headline instance %s is no instance of this night's verdict"
                          % ", ".join(unknown))
    earlier = sorted(key.split(":")[1] for key in journal.nightly
                     if key.endswith(":core") and key.split(":")[1] < tag)[-nights:]
    found, refused = [], []
    for night in earlier:
        try:
            found.append((night, journal.core(night)))
        except Unavailable as error:
            refused.append({"tag": night, "reason": _reason(error)})
    if current is not None:
        found.append((tag, current))
    elif "nightly:%s:core" % tag in journal.nightly:
        refused.append({"tag": tag, "reason": _reason(current_reason)})
    instances = []
    for name in headline:
        points, unit, previous = [], None, None
        for night, core in found:
            case = next((entry for entry in core[3]["cases"] if entry["instance_id"] == name),
                        None)
            base = baseline_name(core[0])
            if case is not None:
                unit = unit or case["metric"]["unit"]
            points.append({"tag": night, "baseline_tag": base,
                           "candidate_median": None if case is None or case["candidate"] is None
                           else number(case["candidate"]["median"]),
                           "release": base if previous is not None and base != previous
                           else None})
            previous = base
        instances.append({"instance_id": name, "unit": unit, "points": points})
    if not found:
        raise Unavailable("the executor journal holds no verified core original of this "
                          "night or the %d before it" % nights)
    return {"nights": nights, "instances": instances, "refused": refused}


def compose(config, date, github, host, clock):
    """The af.nightly-daily/1 document of the night `date`."""
    record = read_night(config, date)
    tag = record["tag"]
    table = config["report"]
    completeness = {}
    sections = dict((name, None) for name in SECTIONS)

    def mark(name, reason=None):
        completeness[name] = {"available": reason is None,
                              "reason": "composed from validated inputs" if reason is None
                              else _reason(reason)}

    composed = clock()
    skipped = record["status"] == "skipped"
    try:
        machine_config = load_executor(table["executor_config"])
        journal = Journal(machine_config["root"], machine_config["journal_id"],
                          machine_config["repository"])
        journal_error = None
    except Unavailable as error:
        machine_config, journal, journal_error = None, None, error

    # The build.
    targets, shipped = [], None
    for entry in record["targets"]:
        try:
            digest, why = shipped_af(config, record, entry), None
        except Unavailable as error:
            digest, why = None, _reason(error)
        if entry["target"] == MEASURED_TARGET:
            shipped = digest
        targets.append({"target": entry["target"], "asset": entry["asset"],
                        "sha256": entry["sha256"], "bytes": entry["bytes"],
                        "af_sha256": digest, "af_reason": why})
    try:
        subject, subject_error = github.commit_subject(record["head_sha"]), None
    except GITHUB_ERRORS as error:
        subject, subject_error = None, error
    build = {"status": record["status"], "head_sha": record["head_sha"],
             "head_subject": subject, "version": record["version"],
             "previous_tag": record["previous_tag"],
             "previous_head_sha": record["previous_head_sha"],
             "skip_reason": record["skip_reason"], "targets": targets}
    sections["build"] = build
    unshipped = [entry for entry in targets if entry["af_sha256"] is None]
    mark("build", None if subject_error is None and not unshipped else "; ".join(
        ([("the head's subject: %s" % subject_error)] if subject_error else [])
        + ["the shipped af of %s: %s" % (entry["target"], entry["af_reason"])
           for entry in unshipped]))

    # The measurements.
    core = None
    skip_words = ("no new commits reached main since %s: this night stands on %s and "
                  "measured nothing" % (record["previous_tag"], record["previous_tag"]))
    if skipped:
        for name in ("performance", "telemetry", "trend", "since_last_night"):
            mark(name, skip_words)
    elif journal is None:
        for name in ("performance", "telemetry", "trend"):
            mark(name, journal_error)
    else:
        core_error = None
        try:
            found = journal.core(tag)
            bind(found[0], "nightly:%s:core" % tag, record["head_sha"], record)
            core = found
            sections["performance"] = performance_section(core, shipped)
            mark("performance")
        except Unavailable as error:
            core_error = error
            mark("performance", error)
        try:
            sections["telemetry"] = journal.telemetry(record)
            mark("telemetry")
        except Unavailable as error:
            mark("telemetry", error)
        try:
            sections["trend"] = trend_section(journal, tag, table["history_nights"],
                                              table["headline_instances"], core, core_error)
            mark("trend")
        except Unavailable as error:
            mark("trend", error)

    # The baseline: the build's, which a core original is bound to before it is used.
    reasons = []
    base_tag, base_sha = record["baseline_tag"], record["baseline_sha"]
    baseline = {"tag": base_tag, "sha": base_sha, "published": None, "commits_since": None,
                "pull_requests_since": None, "reasons": []}
    try:
        baseline["published"] = github.release_date(base_tag)
    except GITHUB_ERRORS as error:
        reasons.append("its publication date: %s" % _reason(error))
    try:
        commits = github.compare(base_sha, record["head_sha"])
        baseline["commits_since"] = len(commits)
        baseline["pull_requests_since"] = len(github.merged_into(commits))
    except GITHUB_ERRORS as error:
        reasons.append("the commits and pull requests since it: %s" % _reason(error))
    baseline["reasons"] = [_reason(text) for text in reasons[:4]]
    sections["baseline"] = baseline
    mark("baseline", "; ".join(reasons) if reasons else None)

    # Since last night.
    if not skipped:
        since = {"previous_tag": record["previous_tag"],
                 "previous_head_sha": record["previous_head_sha"],
                 "merged_pull_requests": None, "verdict_changes": None, "reasons": []}
        missing = []
        if record["previous_head_sha"] is None:
            missing.append("this is the first nightly: there is no previous night")
        else:
            try:
                since["merged_pull_requests"] = github.merged_into(
                    github.compare(record["previous_head_sha"], record["head_sha"]))
            except GITHUB_ERRORS as error:
                missing.append("the pull requests merged since %s: %s" % (
                    record["previous_tag"], _reason(error)))
            if core is None:
                missing.append("verdict changes: this night has no verified core verdict")
            elif journal is not None:
                try:
                    found = journal.core(record["previous_tag"])
                    bind(found[0], "nightly:%s:core" % record["previous_tag"],
                         record["previous_head_sha"])
                    before = found[3]
                    previous = dict((entry["instance_id"], entry["status"])
                                    for entry in before["cases"])
                    since["verdict_changes"] = [
                        {"instance_id": entry["instance_id"],
                         "previous": previous[entry["instance_id"]], "current": entry["status"]}
                        for entry in core[3]["cases"] if entry["instance_id"] in previous
                        and previous[entry["instance_id"]] != entry["status"]]
                except Unavailable as error:
                    missing.append("verdict changes: %s" % _reason(error))
        since["reasons"] = [_reason(text) for text in missing[:4]]
        sections["since_last_night"] = since
        mark("since_last_night", "; ".join(missing) if missing else None)

    # The backlog.
    try:
        sections["backlog"] = github.backlog(record["head_sha"], composed)
        mark("backlog")
    except GITHUB_ERRORS as error:
        mark("backlog", "GitHub: %s" % _reason(error))

    # The machine.
    sections["machine"] = {
        "free_bytes": _fact(host.free_bytes, config["builds_root"]),
        "lock_held": _fact(host.lock_held, hostlock.default_path()),
        "failed_units": _fact(host.failed_units)}
    gaps = ["%s: %s" % (name, fact["reason"]) for name, fact in sections["machine"].items()
            if fact["reason"] is not None]
    mark("machine", "; ".join(gaps) if gaps else None)

    performance = sections["performance"]
    if performance is not None:
        label = performance["result"].upper()
        words = "%s: %s (%s scope, %s evidence)" % (tag, label, performance["scope"],
                                                    performance["provenance"])
    elif skipped:
        label, words = "UNAVAILABLE", "%s: UNAVAILABLE (%s)" % (tag, skip_words)
    else:
        label = "UNAVAILABLE"
        words = "%s: UNAVAILABLE (%s)" % (tag, completeness["performance"]["reason"])
    doc = dict(sections, schema=contracts.CONTRACTS["nightly-daily"][0], date=date, tag=tag,
               composed_unix_ms=composed, repository=config["repository"],
               host={"machine_id": machine_config["machine_id"] if machine_config else None},
               headline={"label": label, "text": words[:2048]}, completeness=completeness)
    return contracts.validate_nightly_daily(doc)


# The page -----------------------------------------------------------------------------------

def _shown(value):
    return "—" if value is None else value["shown"]


def interval_text(item):
    interval = item["interval"]
    if interval is None:
        return "not computed"
    return "[%s, %s]" % (interval["lower"]["shown"], interval["upper"]["shown"])


PERFORMANCE_HEADERS = ("Instance", "Criticality", "Baseline median", "Candidate median",
                       "Unit", "Relative change (%)", "Interval", "Classification", "Outcome")


def performance_row(item):
    return (item["instance_id"], item["criticality"], _shown(item["baseline_median"]),
            _shown(item["candidate_median"]), item["unit"], _shown(item["relative_change"]),
            interval_text(item), item["classification"] or item["status"],
            render.outcome(item["outcome"]))


def _unavailable(doc, name):
    return render.paragraph("Unavailable: " + doc["completeness"][name]["reason"],
                            "unavailable")


def _note(doc, name):
    entry = doc["completeness"][name]
    return "" if entry["available"] else render.paragraph(
        "Partly unavailable: " + entry["reason"], "unavailable")


def _known(value):
    return "unavailable" if value is None else value


def _bytes(value):
    return "%s (%.1f GiB)" % (value, value / float(1 << 30))


def headline_section(doc):
    build, baseline, headline = doc["build"], doc["baseline"], doc["headline"]
    css = headline["label"].lower() if headline["label"] != "UNAVAILABLE" else "unavailable"
    rows = [("Night", "%s (%s)" % (doc["tag"], build["status"])),
            ("Head", "%s %s" % (build["head_sha"], build["head_subject"] or
                                "(subject unavailable)")),
            ("Version", build["version"]),
            ("Previous night", build["previous_tag"] or "none"),
            ("Baseline", "%s %s" % (baseline["tag"], baseline["sha"])),
            ("Baseline published", baseline["published"] or "unavailable"),
            ("Commits since the baseline", _known(baseline["commits_since"])),
            ("Pull requests since the baseline", _known(baseline["pull_requests_since"])),
            ("Host", doc["host"]["machine_id"] or "unavailable")]
    for entry in build["targets"]:
        rows.append(("Archive %s" % entry["target"], "%s %s (%d bytes); af %s" % (
            entry["asset"], entry["sha256"], entry["bytes"],
            entry["af_sha256"] or "unavailable: " + entry["af_reason"])))
    if build["skip_reason"]:
        rows.append(("Skipped", build["skip_reason"]))
    return render.section(
        "Headline", render.verdict(css, headline["label"]), render.paragraph(headline["text"]),
        render.table(("Field", "Value"), rows), _note(doc, "build"), _note(doc, "baseline"))


def performance_part(doc):
    performance = doc["performance"]
    if performance is None:
        return render.section("Performance", _unavailable(doc, "performance"),
                              telemetry_part(doc))
    moved = [item for item in performance["instances"] if item["moved"]]
    sentinel = performance["sentinel"]
    coverage = performance["coverage"]
    if performance["measured_is_shipped"] is None:
        binary = "Measured candidate af: unknown whether it is the shipped one: %s." % (
            performance["measured_reason"])
    else:
        binary = "Measured candidate af %s %s the shipped %s af %s." % (
            performance["measured_candidate_sha256"],
            "is" if performance["measured_is_shipped"] else "is not", MEASURED_TARGET,
            performance["shipped_sha256"])
    parts = [render.paragraph(performance["headline"]),
             render.table(PERFORMANCE_HEADERS, [performance_row(item) for item in moved])
             if moved else render.paragraph("No instance moved."),
             render.table(("Count", "Value"), [
                 ("Unchanged instances (within tolerance or non-regression, passing)",
                  performance["unchanged"]),
                 ("Healthy sentinel shards", sentinel["healthy"]),
                 ("Sentinel shards", sentinel["shards"]),
                 ("Planned records matched", coverage["matched_records"]),
                 ("Planned records", coverage["expected_records"]),
                 ("Planned records missing", coverage["missing_records"])]),
             render.paragraph("Sentinel flags: %s." % (", ".join(sentinel["flags"]) or "none")),
             render.paragraph(binary)]
    if performance["reasons"]:
        parts.append(render.table(("Code", "Instance", "Detail"), [
            (item["code"], item["instance_id"] or "", item["detail"])
            for item in performance["reasons"]]))
    parts.append(telemetry_part(doc))
    return render.section("Performance", *parts)


def telemetry_part(doc):
    telemetry = doc["telemetry"]
    if telemetry is None:
        return render.heading("Telemetry") + _unavailable(doc, "telemetry")
    return render.heading("Telemetry") + render.table(
        ("Experiment", "Subject", "Level", "Metric", "Unit", "State", "Median change (%)"),
        [(item["experiment"], item["subject"], item["level"], item["metric"], item["unit"],
          item["state"], _shown(item["median_change_percent"]))
         for item in telemetry["figures"]])


def trend_part(doc):
    trend = doc["trend"]
    if trend is None:
        return render.section("Trend", _unavailable(doc, "trend"))
    parts = [render.paragraph(
        "Each point is one night's own paired comparison: its candidate median against that "
        "night's baseline; points of different nights are never paired with each other. Up "
        "to %d earlier nights plus this night." % trend["nights"], "note")]
    for item in trend["instances"]:
        releases = ["%s from %s" % (point["release"], point["tag"]) for point in item["points"]
                    if point["release"]]
        title = "candidate median by night: %s (%s)%s" % (
            item["instance_id"], item["unit"] or "no unit",
            "; release " + ", ".join(releases) if releases else "")
        points = [{"sequence": index, "phase": "measured", "pair": index + 1, "position": 1,
                   "side": "candidate",
                   "status": "ok" if point["candidate_median"] else "missing",
                   "value": point["candidate_median"]["exact"]
                   if point["candidate_median"] else None, "failure": None}
                  for index, point in enumerate(item["points"])]
        parts.append(render.heading(item["instance_id"]))
        parts.append(render.plot(points, title))
        parts.append(render.table(("Night", "Baseline", "Candidate median", "Release"), [
            (point["tag"], point["baseline_tag"], _shown(point["candidate_median"]),
             point["release"] or "") for point in item["points"]]))
    if trend["refused"]:
        parts.append(render.table(("Night", "Refused because"), [
            (item["tag"], item["reason"]) for item in trend["refused"]]))
    return render.section("Trend", *parts)


def since_part(doc):
    since = doc["since_last_night"]
    if since is None:
        return render.section("Since last night", _unavailable(doc, "since_last_night"))
    parts = [render.paragraph("Previous night: %s (%s)." % (
        since["previous_tag"] or "none", since["previous_head_sha"] or "none"))]
    if since["merged_pull_requests"] is not None:
        parts.append(render.table(("Pull request", "Title"), [
            ("#%d" % item["number"], item["title"]) for item in since["merged_pull_requests"]])
            if since["merged_pull_requests"] else render.paragraph("No pull request merged."))
    if since["verdict_changes"] is not None:
        parts.append(render.table(("Instance", "Previous", "Now"), [
            (item["instance_id"], item["previous"], item["current"])
            for item in since["verdict_changes"]])
            if since["verdict_changes"] else render.paragraph("No verdict changed."))
    parts.append(_note(doc, "since_last_night"))
    return render.section("Since last night", *parts)


def backlog_part(doc):
    backlog = doc["backlog"]
    if backlog is None:
        return render.section("Backlog", _unavailable(doc, "backlog"))
    checks = backlog["head_checks"]
    parts = [
        render.table(("Count", "Value"), [
            ("Open issues", backlog["open_issues"]),
            ("Issues opened in the last 24 h", backlog["opened_24h"]),
            ("Issues closed in the last 24 h", backlog["closed_24h"]),
            ("Stale issues (no update for %d days)" % STALE_DAYS, backlog["stale_issues"]),
            ("Open pull requests", backlog["open_pull_requests"]),
            ("Draft pull requests", backlog["draft_pull_requests"]),
            ("The head's check runs", checks["total"]),
            ("The head's check runs succeeded", checks["success"]),
            ("The head's check runs failed", checks["failure"]),
            ("The head's check runs skipped", checks["skipped"]),
            ("The head's check runs pending", checks["pending"]),
            ("The head's other check runs", checks["other"])]),
        render.table(("Label", "Open issues"), [(item["name"], item["count"])
                                                for item in backlog["labels"]]),
        render.table(("Stale issue", "Title", "Last update"), [
            ("#%d" % item["number"], item["title"], item["updated"])
            for item in backlog["stale"]]),
        render.table(("Pull request", "Title", "Draft", "Mergeable state"), [
            ("#%d" % item["number"], item["title"], "yes" if item["draft"] else "no",
             item["mergeable_state"]) for item in backlog["pull_requests"]])]
    if backlog["pull_requests_partial"] is not None:
        parts.append(render.paragraph("Partly read: " + backlog["pull_requests_partial"],
                                      "unavailable"))
    return render.section("Backlog", *parts)


def machine_part(doc):
    machine = doc["machine"]

    def shown(name, text):
        fact = machine[name]
        return "unavailable: " + fact["reason"] if fact["value"] is None else text(fact["value"])

    return render.section("Machine", render.table(("Fact", "Value"), [
        ("Free bytes of builds_root's filesystem", shown("free_bytes", _bytes)),
        ("Machine lock", shown("lock_held", lambda held: "held" if held else "free")),
        ("Failed systemd units", shown("failed_units", str))]))


def html_bytes(doc):
    """The self-contained page of `doc`: each section drawn from its own part of the record,
    so the page and daily.json cannot disagree."""
    sections = [headline_section(doc), performance_part(doc), trend_part(doc), since_part(doc),
                backlog_part(doc), machine_part(doc),
                render.section("Completeness", render.table(
                    ("Section", "Available", "Why"), [
                        (name, "yes" if doc["completeness"][name]["available"] else "no",
                         doc["completeness"][name]["reason"]) for name in SECTIONS]))]
    return render.page("AF nightly %s" % doc["tag"], sections, search=False)


# The digest ---------------------------------------------------------------------------------

def _plain(text, limit=240):
    text = " ".join(str(text).replace("`", "'").split())
    return text if len(text) <= limit else text[:limit - 3] + "..."


def _signed(shown):
    return shown if shown.startswith("-") or shown == "0" else "+" + shown


def _moved_line(item):
    change = item["relative_change"]
    return "- `%s` %s: %s, interval %s %s (%s)" % (
        item["instance_id"], item["classification"] or item["status"],
        "no relative change" if change is None else "%s%%" % _signed(change["shown"]),
        interval_text(item), item["unit"], item["outcome"])


def _count(value):
    return "?" if value is None else str(value)


def digest_text(doc):
    """The Telegram digest: the headline first, then what moved, the queue and the machine,
    links as bare URLs on their own lines, at most DIGEST_BYTES_MAX bytes."""
    build, baseline = doc["build"], doc["baseline"]
    head = "head %s; baseline %s; commits since it: %s; pull requests since it: %s" % (
        build["head_sha"][:12], baseline["tag"], _count(baseline["commits_since"]),
        _count(baseline["pull_requests_since"]))
    performance = doc["performance"]
    moved, tail = [], []
    if performance is not None:
        moved = [_moved_line(item) for item in performance["instances"] if item["moved"]]
        sentinel = performance["sentinel"]
        tail.append("Unchanged: %d; sentinels: %d of %d shards healthy" % (
            performance["unchanged"], sentinel["healthy"], sentinel["shards"]))
    elif build["skip_reason"] is None:
        tail.append("Performance unavailable: " + _plain(doc["completeness"]["performance"][
            "reason"]))
    since = doc["since_last_night"]
    if since is not None and since["merged_pull_requests"] is not None:
        tail.append("Merged since %s: %d pull requests" % (since["previous_tag"],
                                                          len(since["merged_pull_requests"])))
    backlog = doc["backlog"]
    if backlog is None:
        queue = "Queue unavailable: " + _plain(doc["completeness"]["backlog"]["reason"])
    else:
        queue = ("Queue: open issues %d (opened %d, closed %d in 24 h; stale %d); open pull "
                 "requests %d (drafts %d)" % (backlog["open_issues"], backlog["opened_24h"],
                                              backlog["closed_24h"], backlog["stale_issues"],
                                              backlog["open_pull_requests"],
                                              backlog["draft_pull_requests"]))
    machine = doc["machine"]
    free, held, failed = (machine[name]["value"] for name in ("free_bytes", "lock_held",
                                                              "failed_units"))
    machine_line = "Machine: %s free; machine lock %s; failed units: %s" % (
        "?" if free is None else "%.1f GiB" % (free / float(1 << 30)),
        "?" if held is None else ("held" if held else "free"), _count(failed))
    links = ["https://github.com/%s/commit/%s" % (doc["repository"], build["head_sha"])]
    if since is not None and since["previous_head_sha"]:
        links.append("https://github.com/%s/compare/%s...%s" % (
            doc["repository"], since["previous_head_sha"], build["head_sha"]))

    def text(shown):
        lines = [_plain(doc["headline"]["text"], 400), head, ""]
        if performance is not None:
            lines.append("Moved:" if moved else "Moved: none")
            lines.extend(moved[:shown])
            if shown < len(moved):
                lines.append("and %d more moved instances: see report.html" % (
                    len(moved) - shown))
        lines.extend(tail)
        if lines[-1]:
            lines.append("")
        lines.extend([queue, "", machine_line, ""] + links)
        return "\n".join(lines) + "\n"

    shown = len(moved)
    while len(text(shown).encode("utf-8")) > DIGEST_BYTES_MAX and shown:
        shown -= 1
    data = text(shown).encode("utf-8")
    if len(data) > DIGEST_BYTES_MAX:
        raise ContractError("the digest exceeds %d bytes" % DIGEST_BYTES_MAX)
    return data


# Command line -------------------------------------------------------------------------------

# A test seam: called with the output path right after its directory is created, before it is
# opened, so a test can prove that a directory swapped for a symlink in that window gets no file.
_after_mkdir = None


def report_names(date):
    """The names a night's report directory takes below a reports root, in the order they are
    tried: `DATE`, then `DATE.2`, `DATE.3` and on (ADR-0273)."""
    return [date] + ["%s.%d" % (date, k) for k in range(2, REPORTS_PER_NIGHT_MAX + 1)]


def write(doc, output=None, root=None):
    """Create a new directory and write exactly the three files into it, by descriptor; the
    path of the directory written.

    The directory is `output`, which must not exist, or else the first of `report_names` free
    below `root`, each tried by an exclusive mkdir. The parent is opened as a directory, the
    new directory is created and opened below it without following a link, and each file is
    created by name below that open directory, so a rename or a symlink put in the way between
    the steps changes nothing outside the directory this call made."""
    files = ((DAILY_JSON, contracts.write_bytes(doc, "nightly-daily")),
             (REPORT_HTML, html_bytes(doc)), (DIGEST_MD, digest_text(doc)))
    if root is None:
        where = "--output %s" % output
        parent, name = os.path.split(os.path.abspath(output).rstrip(os.sep))
        if not name:
            raise Refused("%s: not a directory name" % where)
        names = [name]
    else:
        where = "--output-root %s" % root
        parent, names = os.path.abspath(root), report_names(doc["date"])
    parent_fd = created_fd = dir_fd = None
    try:
        try:
            parent_fd = os.open(parent or os.sep, os.O_RDONLY | os.O_DIRECTORY)
            for name in names:
                try:
                    os.mkdir(name, 0o755, dir_fd=parent_fd)
                    break
                except FileExistsError:
                    if root is None:
                        raise
            else:
                raise Refused("%s already holds %d reports of the night %s"
                              % (where, len(names), doc["date"]))
            created_fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                 dir_fd=parent_fd)
            created = os.fstat(created_fd)
        except OSError as error:
            raise Refused("%s: %s (a report is written into a new directory)" % (where, error))
        written = os.path.join(parent, name)
        if _after_mkdir is not None:
            _after_mkdir(written if root is not None else output)
        try:
            dir_fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                             dir_fd=parent_fd)
            reopened = os.fstat(dir_fd)
            if (reopened.st_dev, reopened.st_ino) != (created.st_dev, created.st_ino):
                raise Refused("%s is not the directory this report created"
                              % (where if root is None else written))
            for filename, data in files:
                fd = os.open(filename, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             0o644, dir_fd=dir_fd)
                with os.fdopen(fd, "wb") as stream:
                    stream.write(data)
        except OSError as error:
            raise Refused("%s is not the directory this report created: %s"
                          % (where if root is None else written, error))
    finally:
        for fd in (dir_fd, created_fd, parent_fd):
            if fd is not None:
                os.close(fd)
    return written


class Parser(argparse.ArgumentParser):
    def error(self, message):
        self.print_usage(sys.stderr)
        sys.stderr.write("nightly: refused: %s\n" % message)
        sys.exit(EXIT_REFUSED)


def parse(argv):
    parser = Parser(prog="report.py", description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", parser_class=Parser)
    commands.required = True
    command = commands.add_parser("compose", help="compose daily.json, report.html and "
                                  "digest.md of one night into a new directory")
    command.add_argument("--config", required=True, help="the af.nightly-config/1 TOML with "
                         "its [report] table")
    nightlychain.add_night_options(command)
    directory = command.add_mutually_exclusive_group()
    directory.add_argument("--output", help="the new directory to write into")
    directory.add_argument("--output-root", help="write into ROOT/DATE, or ROOT/DATE.K "
                           "with the smallest K >= 2 free when DATE exists (default: the "
                           "configuration's reports_root)")
    return parser.parse_args(argv)


def main(argv=None, github=None, host=None, clock=now_ms, show=nightlychain.systemctl_show):
    args = parse(argv)
    try:
        date, chosen = nightlychain.night(args, clock, show, Refused)
        if chosen:
            sys.stderr.write(chosen)
        if args.output is not None and os.path.lexists(args.output):
            raise Refused("--output %s exists: a report is written into a new directory"
                          % args.output)
        config = load_config(args.config)
        root = args.output_root
        if args.output is None and root is None:
            if "reports_root" not in config:
                raise Refused("compose needs --output, --output-root or the configuration's "
                              "reports_root")
            root = config["reports_root"]
            sys.stderr.write("nightly: --output-root %s (the configuration's reports_root)\n"
                             % root)
        if root is not None and not os.path.isdir(root):
            raise Refused("--output-root %s is not a directory" % root)
        if github is None:
            try:
                token = nightlygithub.read_token(config.get("github_token_file"), Unavailable)
            except Unavailable as error:
                github = Offline(str(error))
            else:
                github = GitHub(config["repository"], token)
        doc = compose(config, date, github, host or Host(), clock)
        written = write(doc, output=args.output, root=root)
    except (Refused, ContractError) as error:
        sys.stderr.write("nightly: refused: %s\n" % error)
        return EXIT_REFUSED
    sys.stdout.write(written + "\n")
    sys.stderr.write("nightly: composed %s: %s\n" % (written, doc["headline"]["text"]))
    return EXIT_COMPOSED


class Offline(GitHub):
    """GitHub when the token file cannot be read: every read is unavailable with that reason,
    never asked anonymously instead."""

    def __init__(self, reason):
        GitHub.__init__(self, "unread/unread")
        self.reason = reason

    def request(self, path):
        raise Unavailable(self.reason)


if __name__ == "__main__":
    sys.exit(main())
