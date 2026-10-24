"""The trusted telemetry collection lane (ADR-0200).

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
- **Accounting.** `collection.json` (`af.perf-telemetry-collection/1`) records every run the
  cycle saw with what it kept, why it kept nothing, what it refused and what it pruned; the
  kept results are the idempotency record, so a failed collection is tried again next cycle
  while its run is within the lookback, and nothing is collected twice.
"""
import os
import re
import shutil
from datetime import datetime, timedelta, timezone

from . import contracts, telemetrylab, tomlfile, transport
from .values import ContractError, require, sha256_bytes

ARTIFACT = re.compile(r"^perf-telemetry-([1-9][0-9]{0,15})-([1-9][0-9]{0,3})$")
COLLECTION_JSON = "collection.json"
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


def completed_runs(api, ident, lookback_days, runs_max, now):
    """The completed runs of workflow `ident` created within the lookback, oldest first, at most
    `runs_max`; (runs, whether more were listed than that)."""
    since = (now - timedelta(days=lookback_days)).strftime("%Y-%m-%dT%H:%M:%SZ")
    rows, total = [], None
    for page in range(1, PAGES_MAX + 1):
        doc = api.get_json(
            "/actions/workflows/%d/runs?status=completed&created=%%3E%%3D%s&per_page=%d&page=%d"
            % (ident, since, PAGE_SIZE, page), "the completed telemetry runs")
        require(isinstance(doc, dict) and isinstance(doc.get("workflow_runs"), list)
                and type(doc.get("total_count")) is int, "the run listing is not one")
        require(total is None or total == doc["total_count"],
                "the run listing changed during pagination; collect again")
        total = doc["total_count"]
        rows.extend(doc["workflow_runs"])
        if len(rows) >= total or len(rows) >= runs_max:
            break
        require(doc["workflow_runs"], "the run listing ended before its declared count")
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
    return rows[:runs_max], (total or 0) > runs_max


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

    def __init__(self, config, api, git, wall, now=None):
        self.config = config
        self.api = api
        self.git = git
        self.wall = wall
        self.now = now or datetime.now(timezone.utc)
        self.records = []

    def run(self):
        config = self.config
        started = int(self.wall() * 1000)
        detail = ""
        ident = None
        with telemetrylab.HistoryRoot(config["root"], config["bytes_max"],
                                      config["results_max"], config["retention_days"],
                                      self.wall) as kept:
            kept.prune()
            try:
                ident = workflow_id(self.api)
                runs, more = completed_runs(self.api, ident, config["lookback_days"],
                                            config["runs_max"], self.now)
                if more:
                    detail = ("more completed runs than the cycle's %d; the oldest were "
                              "collected and the rest wait for the next cycle"
                              % config["runs_max"])
                for run in runs:
                    self.api.remaining("collecting run %d" % run["id"])
                    self.collect(kept, run)
            except (ContractError, OSError) as error:
                detail = ("the cycle ended early: %s" % error)[:4096]
            refused = [{"input": "Actions run %d attempt %d" % (item["run_id"], item["attempt"]),
                        "reason": item["reason"]}
                       for item in self.records if item["outcome"] == "refused"]
            history = kept.write(refused)
            doc = {"schema": contracts.TELEMETRY_COLLECTION_SCHEMA,
                   "repository": config["repository"], "workflow_id": ident,
                   "started_unix_ms": started,
                   "ended_unix_ms": max(started, int(self.wall() * 1000)),
                   "status": "ok", "detail": detail, "runs": self.records[:4096],
                   "pruned": history["pruned"], "results": len(history["results"]),
                   "used_bytes": history["used_bytes"]}
            doc["status"] = "failed" if detail or any(
                item["outcome"] == "refused" for item in doc["runs"]) else "ok"
            data = contracts.write_bytes(doc, "telemetry-collection")
            path = os.path.join(kept.root, COLLECTION_JSON)
            temporary = path + ".tmp-" + os.urandom(4).hex()
            with open(temporary, "xb") as stream:
                stream.write(data)
            os.replace(temporary, path)
        return doc

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
            kept.import_result(staging, authority)
        except (ContractError, OSError) as error:
            self.record(run, attempt, "refused", artifact, reason=str(error))
            return
        finally:
            for path in (staging, authority):
                if os.path.lexists(path):
                    shutil.rmtree(path, ignore_errors=True)
        self.record(run, attempt, "collected", artifact, result_id)

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
