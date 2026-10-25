"""The trusted telemetry collection lane (ADR-0215).

`cli.py telemetry-collect --config FILE` runs one bounded, idempotent collection cycle of the
Performance telemetry workflow's result artifacts into a durable telemetry history root. It is a
lane of its own, beside the archive collector (`collect.py`, `perflab.collection`), which is left
exactly as it is: that collector stays pinned to the control and report generations of the
frozen hosted calibration cohort, never reads these artifacts, and this lane never reads the
archive or its artifacts. The configuration (`af.perf-telemetry-collector/1`) is the operator's,
outside the repository; `scripts/performance/service/` holds an example and a systemd unit and
timer a future always-on Linux host can install. Nothing here installs or schedules anything.

It reuses the archive collector's conventions:

- **Credential.** The only credential is a read token named by `token_env`, read by the entry
  point and held only by `transport.Collector`, which sends it only to the configured API
  origin; signed storage receives no credential. No measurement, script or binary ever runs
  here, so nothing else can receive it.
- **Identity.** The workflow is identified by its file path; every run must be a completed run
  of exactly that workflow and repository; an artifact must be named
  `perf-telemetry-<run>-<attempt>` of that run, carry no other run's ID, and its downloaded
  bytes must have the SHA-256 the API declares for it when it declares one. The result inside
  must be `tl_<run>_<attempt>`, its trigger the run's event's, and for a scheduled or released
  run its candidate the run's own commit.
- **Authority.** A result is kept only verified under exactly the measurement authority it
  names: that commit must be reachable from one of the configured trusted refs of the trusted
  repository, and its authority files are read from Git objects there
  (`telemetrylab.materialize_authority`); the result's digest must be theirs. Nothing of an
  artifact is ever executed.
- **Bounds.** One deadline for the whole cycle, pages and runs per cycle, each artifact's bytes
  and its ZIP's members, ratio and decompressed bytes (`transport.read_zip`), the root's bytes,
  results and retention (`telemetrylab.HistoryRoot`), all under the root's one lock, every
  import staged and renamed whole.
- **Accounting.** `collection.json` (`af.perf-telemetry-collection/2`) records every attempted
  run with what it kept, why it kept nothing, what it refused and what it pruned. The kept
  results are the idempotency record. `collection-position.json` rotates attempted runs across
  cycles, including failures, while they remain within lookback; it never records success.
"""
import os
import re
import shutil
import stat
from datetime import datetime, timedelta, timezone

from . import budget as deadlines, contracts, telemetrylab, tomlfile, transport
from .values import ContractError, require, sha256_bytes

ARTIFACT = re.compile(r"^perf-telemetry-([1-9][0-9]{0,15})-([1-9][0-9]{0,3})$")
COLLECTION_JSON = "collection.json"
POSITION_JSON = "collection-position.json"
POSITION_BYTES_MAX = 1024
TERMINAL_SECONDS = 5
PAGE_SIZE = 100
PAGES_MAX = 10
TRIGGERS = {"pull_request": "pr", "schedule": "nightly", "release": "release",
            "workflow_dispatch": "manual"}


def load_config(path):
    with open(path, "rb") as stream:
        data = stream.read(tomlfile.MAX_TOML_BYTES + 1)
    doc = contracts.validate_telemetry_collector(tomlfile.loads(data, path), path)
    require(doc["artifact_bytes_max"] <= transport.ARTIFACT_BYTES_MAX,
            "%s.artifact_bytes_max: at most the collector's %d-byte artifact bound" % (
                path, transport.ARTIFACT_BYTES_MAX))
    return doc


def workflow_id(api):
    info = api.get_json("/actions/workflows/" + contracts.TELEMETRY_WORKFLOW,
                        "the Performance telemetry workflow's identity")
    ident = info.get("id")
    contracts.ACTIONS_ID.validate(ident, "workflow ID")
    require(info.get("path") == ".github/workflows/" + contracts.TELEMETRY_WORKFLOW,
            "the workflow's path is not .github/workflows/" + contracts.TELEMETRY_WORKFLOW)
    return ident


def completed_runs(api, ident, lookback_days, now):
    """A complete bounded listing, oldest first. Never select from a truncated newest prefix.

    Actions pagination is not a snapshot; changed counts, duplicate IDs and short pages are
    refused. Even a consistent listing describes only what this cycle observed.
    """
    since = (now - timedelta(days=lookback_days)).strftime("%Y-%m-%dT%H:%M:%SZ")
    rows, total = [], None
    for page in range(1, PAGES_MAX + 1):
        doc = api.get_json(
            "/actions/workflows/%d/runs?status=completed&created=%%3E%%3D%s&per_page=%d&page=%d"
            % (ident, since, PAGE_SIZE, page), "the completed telemetry runs")
        require(isinstance(doc, dict) and isinstance(doc.get("workflow_runs"), list)
                and type(doc.get("total_count")) is int and doc["total_count"] >= 0,
                "the run listing is not one")
        require(total is None or total == doc["total_count"],
                "the run listing changed during pagination; collect again")
        total = doc["total_count"]
        require(total <= PAGES_MAX * PAGE_SIZE,
                "the run listing exceeds the %d-run page bound; narrow the lookback "
                "before collecting again" % (PAGES_MAX * PAGE_SIZE))
        require(len(doc["workflow_runs"]) == min(PAGE_SIZE, total - len(rows)),
                "the run listing has a short or excess page; collect again")
        rows.extend(doc["workflow_runs"])
        if len(rows) == total:
            break
    seen = set()
    for run in rows:
        run_id = run.get("id") if isinstance(run, dict) else None
        contracts.ACTIONS_ID.validate(run_id, "run ID")
        require(run_id not in seen, "the run listing names run %d twice" % run_id)
        seen.add(run_id)
        attempt = run.get("run_attempt")
        require(type(attempt) is int and 1 <= attempt <= 1000, "run %d has no attempt" % run_id)
        require(run.get("workflow_id") == ident and run.get("status") == "completed"
                and (run.get("repository") or {}).get("full_name") == api.repository
                and run.get("path", ".github/workflows/" + contracts.TELEMETRY_WORKFLOW)
                == ".github/workflows/" + contracts.TELEMETRY_WORKFLOW,
                "run %d is not a completed run of this repository's telemetry workflow"
                % run_id)
    rows.sort(key=lambda run: run["id"])
    return rows


def read_position(kept, repository, ident):
    """A scheduling hint only, confined to this locked root; never proof of collection."""
    path = os.path.join(kept.root, POSITION_JSON)
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return 0, 0
    with os.fdopen(fd, "rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode),
                "the collection position is not a regular file")
        data = stream.read(POSITION_BYTES_MAX + 1)
    require(len(data) <= POSITION_BYTES_MAX, "the collection position exceeds its byte bound")
    doc = contracts.read_json(data, "telemetry-collection-position", path)
    require(doc["repository"] == repository and doc["workflow_id"] == ident,
            "the collection position belongs to another repository or workflow; "
            "stop collection and remove collection-position.json to reset")
    return doc["after_run_id"], doc["through_run_id"]


def write_position(kept, repository, ident, after_run_id, through_run_id):
    doc = {"schema": contracts.TELEMETRY_COLLECTION_POSITION_SCHEMA,
           "repository": repository, "workflow_id": ident, "after_run_id": after_run_id,
           "through_run_id": through_run_id}
    data = contracts.write_bytes(doc, "telemetry-collection-position")
    require(len(data) <= POSITION_BYTES_MAX, "the collection position exceeds its byte bound")
    path = os.path.join(kept.root, POSITION_JSON)
    temporary = path + ".tmp"
    # One fixed temporary under the history lock bounds leftovers from a killed writer.
    if os.path.lexists(temporary):
        os.unlink(temporary)
    try:
        with open(temporary, "xb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        fd = os.open(kept.root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        if os.path.lexists(temporary):
            os.unlink(temporary)


def scheduled_runs(runs, after, through, runs_max):
    """Finish a rotation's fixed upper ID before wrapping. New arrivals cannot extend it.

    Return (run, rotation upper ID) pairs, attempting each observed run at most once here.
    If the old upper ID disappeared, the remaining IDs still finish that rotation.
    """
    pending = [run for run in runs if after < run["id"] <= through]
    selected = [(run, through) for run in pending[:runs_max]]
    if len(selected) < runs_max and runs:
        seen = {run["id"] for run, _ in selected}
        selected.extend((run, runs[-1]["id"]) for run in runs if run["id"] not in seen)
    return selected[:runs_max]


def artifacts(api, run_id):
    """Every artifact of run `run_id` with its declared SHA-256 when the API declares one."""
    found, total = [], None
    for page in range(1, PAGES_MAX + 1):
        doc = api.get_json("/actions/runs/%d/artifacts?per_page=%d&page=%d" % (
            run_id, PAGE_SIZE, page), "the artifact listing")
        require(isinstance(doc, dict) and isinstance(doc.get("artifacts"), list)
                and type(doc.get("total_count")) is int,
                "the artifact listing has no artifacts and total_count")
        total = doc["total_count"]
        for item in doc["artifacts"]:
            entry = transport._artifact(item, run_id)
            digest = item.get("digest")
            entry["sha256"] = (digest if isinstance(digest, str) and re.match(
                r"^sha256:[0-9a-f]{64}$", digest) else None)
            found.append(entry)
        if len(found) >= total or not doc["artifacts"]:
            break
    require(total is not None and len(found) >= total,
            "the listing named %s artifacts, more than the %d a collection reads"
            % (total, len(found)))
    ids = [item["id"] for item in found]
    require(len(ids) == len(set(ids)), "the listing names one artifact twice")
    return found


class Cycle(object):
    """One collection cycle of `config` through the `transport.Collector` `api`, reading
    measurement authorities from the trusted repository `git` (`gitrefs.Git`)."""

    def __init__(self, config, api, git, wall, now=None, budget=None):
        self.config = config
        self.api = api
        self.git = git
        self.wall = wall
        self.now = now or datetime.now(timezone.utc)
        self.records = []
        self.interrupted_import = None
        self.budget = budget or deadlines.Budget(api.deadline, api.clock, TERMINAL_SECONDS)
        # Reachability, tree enumeration and cat-file all share the HTTP cycle's instant.
        self.git.budget = self.budget

    def run(self):
        config = self.config
        started = int(self.wall() * 1000)
        detail, ident, history = "", None, None
        kept = telemetrylab.HistoryRoot(config["root"], config["bytes_max"],
                                       config["results_max"], config["retention_days"],
                                       self.wall, budget=self.budget, sweep=False)
        self.pruned = kept.pruned
        fallback = {"schema": contracts.TELEMETRY_COLLECTION_SCHEMA,
                    "repository": config["repository"], "workflow_id": None,
                    "started_unix_ms": started, "ended_unix_ms": None,
                    "status": "failed", "detail": "cycle incomplete; final accounting unavailable",
                    "runs": [], "pruned": [], "results": None, "used_bytes": None}
        try:
            try:
                with self.budget.enforce("telemetry collection"):
                    kept.__enter__()
                    # If even terminal I/O is interrupted, this atomic record remains truthful:
                    # this cycle has not completed accounting or published a final success.
                    self.write_status(kept, fallback)
                    kept.cleanup_staging()
                    kept.prune()
                    ident = workflow_id(self.api)
                    after, through = read_position(kept, config["repository"], ident)
                    runs = completed_runs(self.api, ident, config["lookback_days"], self.now)
                    if len(runs) > config["runs_max"]:
                        detail = ("the listing held %d completed runs; this cycle attempts at "
                                  "most %d from its rotating position; unattempted runs remain "
                                  "pending within lookback" % (len(runs), config["runs_max"]))
                    for run, through in scheduled_runs(runs, after, through, config["runs_max"]):
                        self.budget.seconds("collecting run %d" % run["id"])
                        self.api.remaining("collecting run %d" % run["id"])
                        self.collect(kept, run)
                        write_position(kept, config["repository"], ident, run["id"], through)
                    refused = [{"input": "Actions run %d attempt %d" % (item["run_id"], item["attempt"]),
                                "reason": item["reason"]}
                               for item in self.records if item["outcome"] == "refused"]
                    history = kept.write(refused)
                    if history["refused"]:
                        detail = (detail + "; " if detail else "") + "history refused retained or imported evidence"
            except (deadlines.Interrupted, ContractError, OSError) as error:
                if kept.fd is None and not isinstance(error, (deadlines.Interrupted, deadlines.Expired)):
                    raise
                detail = ("the cycle ended early: %s" % error)[:4096]
            terminal = deadlines.Budget(self.budget.deadline, self.budget.clock, 0)
            try:
                with terminal.enforce("collection cleanup and terminal record"):
                    doc = dict(fallback, detail=detail or fallback["detail"])
                    if kept.fd is not None:
                        if self.interrupted_import is not None:
                            run, attempt, artifact, result_id = self.interrupted_import
                            # A deadline may land immediately after the verified atomic rename.
                            # Reconcile under the same lock and terminal deadline, never claim
                            # refusal for a result this cycle actually accepted.
                            if os.path.isdir(os.path.join(kept.results, result_id)):
                                if self.records and self.records[-1]["run_id"] == run["id"] \
                                        and self.records[-1]["attempt"] == attempt:
                                    self.records.pop()
                                self.record(run, attempt, "collected", artifact, result_id)
                        doc = self.document(started, ident, history, detail)
                        self.write_status(kept, self.document(started, ident, None,
                                          ("cycle incomplete; cleanup and final recording pending"
                                           + ("; " + detail if detail else ""))[:4096]))
                        # A deadline may have interrupted an artifact's finally block. Such
                        # state is unaccepted and may be left for the next locked sweep.
                        kept.cleanup_staging()
                        self.write_status(kept, doc)
            except (deadlines.Interrupted, ContractError, OSError) as error:
                # No validator, clock callback or I/O after the terminal timer has ended.
                # This schema-valid record explicitly has no terminal time or root accounting.
                doc = dict(fallback, workflow_id=ident, runs=self.records[:4096],
                           pruned=kept.pruned[:4096],
                           detail=((detail + "; " if detail else "")
                                   + "cleanup or terminal recording incomplete: %s" % error)[:4096])
            return doc
        finally:
            # This covers arbitrary exceptions as well as deadline and contract failures.
            kept.__exit__()

    def document(self, started, ident, history, detail):
        doc = {"schema": contracts.TELEMETRY_COLLECTION_SCHEMA,
               "repository": self.config["repository"], "workflow_id": ident,
               "started_unix_ms": started,
               "ended_unix_ms": max(started, int(self.wall() * 1000)),
               "status": "ok", "detail": detail, "runs": self.records[:4096],
               "pruned": self.pruned[:4096] if history is None else history["pruned"],
               "results": None if history is None else len(history["results"]),
               "used_bytes": None if history is None else history["used_bytes"]}
        doc["status"] = "failed" if detail or any(
            item["outcome"] == "refused" for item in doc["runs"]) else "ok"
        contracts.validate_telemetry_collection(doc)
        return doc

    def write_status(self, kept, doc):
        data = contracts.write_bytes(doc, "telemetry-collection")
        path = os.path.join(kept.root, COLLECTION_JSON)
        temporary = path + ".tmp"
        if os.path.lexists(temporary):
            os.unlink(temporary)
        try:
            with open(temporary, "xb") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.lexists(temporary):
                os.unlink(temporary)

    def record(self, run, attempt, outcome, artifact=None, result_id=None, reason=""):
        self.records.append({
            "run_id": run["id"], "attempt": attempt, "event": str(run.get("event"))[:64],
            "conclusion": (None if run.get("conclusion") is None
                           else str(run["conclusion"])[:64]),
            "outcome": outcome,
            "artifact": None if artifact is None else {
                "id": artifact["id"], "name": artifact["name"][:256], "size": artifact["size"],
                "sha256": artifact.get("downloaded") or artifact.get("sha256")},
            "result_id": result_id, "reason": reason[:4096]})

    def collect(self, kept, run):
        run_id = run["id"]
        found = {}
        try:
            for artifact in artifacts(self.api, run_id):
                if not artifact["name"].startswith("perf-telemetry-") \
                        or artifact["name"].startswith("perf-telemetry-af-"):
                    continue
                match = ARTIFACT.match(artifact["name"])
                require(match is not None and int(match.group(1)) == run_id,
                        "artifact %s is not this run's result" % artifact["name"][:256])
                attempt = int(match.group(2))
                require(attempt <= run["run_attempt"] and attempt not in found,
                        "artifact %s names an attempt this run does not have"
                        % artifact["name"])
                found[attempt] = artifact
        except (ContractError, OSError) as error:
            self.record(run, run["run_attempt"], "refused", reason=str(error))
            return
        if not found:
            self.record(run, run["run_attempt"], "no_result",
                        reason="the run (%s) retained no result artifact" % run.get(
                            "conclusion"))
            return
        for attempt in sorted(found):
            self.collect_artifact(kept, run, attempt, found[attempt])

    def collect_artifact(self, kept, run, attempt, artifact):
        result_id = "tl_%d_%d" % (run["id"], attempt)
        if os.path.isdir(os.path.join(kept.results, result_id)):
            self.record(run, attempt, "already_collected", artifact, result_id)
            return
        if artifact["expired"]:
            self.record(run, attempt, "expired", artifact, reason="the artifact expired")
            return
        staging = os.path.join(kept.results, telemetrylab.STAGING_PREFIX + "collect-"
                               + os.urandom(6).hex())
        authority = os.path.join(kept.results, telemetrylab.STAGING_PREFIX + "authority-"
                                 + os.urandom(6).hex())
        try:
            require(artifact["size"] <= self.config["artifact_bytes_max"],
                    "artifact %d declares %d bytes, more than the configured %d" % (
                        artifact["id"], artifact["size"], self.config["artifact_bytes_max"]))
            data = self.api.download(artifact)
            if data is None:
                self.record(run, attempt, "expired", artifact, reason="the artifact is gone")
                return
            require(len(data) <= self.config["artifact_bytes_max"],
                    "the download held more than the configured artifact bytes")
            artifact["downloaded"] = sha256_bytes(data)
            require(artifact["sha256"] is None or artifact["sha256"] == artifact["downloaded"],
                    "the downloaded bytes are not the artifact the API declares (%s, not %s)"
                    % (artifact["downloaded"], artifact["sha256"]))
            members = transport.read_zip(data, self.api.remaining, None,
                                         self.config["artifact_bytes_max"])
            transport._materialize(staging, members)
            result, _, _ = telemetrylab.read_bundle(staging)
            self.bind(run, attempt, result)
            self.api.remaining("reading the measurement authority")
            sha = result["sources"]["authority_sha"]
            require(any(self.git.is_ancestor(sha, ref) for ref in self.config["trusted_refs"]),
                    "its measurement authority %s is reachable from none of the trusted refs "
                    "%s" % (sha[:12], ", ".join(self.config["trusted_refs"])))
            telemetrylab.materialize_authority(self.git, sha, authority)
            self.api.remaining("verifying the result")
            self.interrupted_import = (run, attempt, artifact, result_id)
            kept.import_result(staging, authority)
        except deadlines.Interrupted as error:
            self.interrupted_import = (run, attempt, artifact, result_id)
            self.record(run, attempt, "refused", artifact, reason=str(error))
            raise
        except (ContractError, OSError) as error:
            self.interrupted_import = None
            self.record(run, attempt, "refused", artifact, reason=str(error))
            return
        finally:
            for path in (staging, authority):
                if self.budget.left() > 0 and os.path.lexists(path):
                    shutil.rmtree(path)
        self.record(run, attempt, "collected", artifact, result_id)
        self.interrupted_import = None

    def bind(self, run, attempt, result):
        """Refuse a result that is not the one this run and attempt measured."""
        expected = "tl_%d_%d" % (run["id"], attempt)
        require(result["result_id"] == expected, "it holds result %s, not %s" % (
            result["result_id"], expected))
        event = run.get("event")
        require(event in TRIGGERS and result["trigger"] == TRIGGERS[event],
                "its trigger %s is not the run's event %s" % (result["trigger"], event))
        if event in ("schedule", "release"):
            require(result["sources"]["candidate_sha"] == run.get("head_sha"),
                    "its candidate %s is not the run's commit %s" % (
                        result["sources"]["candidate_sha"][:12], str(run.get("head_sha"))[:12]))
