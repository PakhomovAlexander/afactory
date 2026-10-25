#!/usr/bin/env python3
"""One bounded operator dispatch cycle. Disabled until a merged cohort is configured.

This is local orchestration, not a readiness judge. The laboratory validates every original
and recomputes readiness from retained evidence. This process never retries a dispatch,
edits a cohort, seals parameters, activates enforcement or invokes AF.
"""
import argparse
import base64
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
LAB = ROOT.parent
REPOSITORY = "PakhomovAlexander/afactory"


def require(condition, detail):
    if not condition:
        raise ValueError(detail)


def read(path, cap=1024 * 1024):
    require(not path.is_symlink() and path.is_file(), "%s is not a regular file" % path.name)
    with path.open("rb") as stream:
        data = stream.read(cap + 1)
    require(len(data) <= cap, "%s exceeds its byte limit" % path.name)
    return data


def atomic(path, doc):
    data = (json.dumps(doc, sort_keys=True, indent=2) + "\n").encode()
    require(len(data) <= 8 * 1024 * 1024, "operator state exceeds 8 MiB")
    fd, temporary = tempfile.mkstemp(prefix=".dispatch-", dir=str(path.parent))
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(str(path.parent), os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def runtime():
    """Load only the collector's installed, hashed library; never a mutable worktree."""
    manifest = json.loads(read(LAB / "collector-manifest.json"))
    require(re.fullmatch(r"[0-9a-f]{64}", manifest["collector_sha256"]),
            "installed runtime identity is malformed")
    folder = LAB / "collector" / manifest["collector_sha256"]
    require(folder.is_dir() and not folder.is_symlink(), "installed runtime is unavailable")
    for name, digest in manifest["files"]:
        path = folder / name
        require(path.resolve().is_relative_to(folder.resolve()), "runtime path escaped")
        require(hashlib.sha256(read(path, 4 * 1024 * 1024)).hexdigest() == digest,
                "installed runtime changed: " + name)
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(folder))
    from perflab import activation, calibration, contracts, transport
    return activation, calibration, contracts, transport


def choose(cohort, originals, journal, rows, now, trials_enabled):
    """Choose one due member without replacing originals or ambiguous local requests."""
    for member in journal:
        if member not in originals:
            return None, "dispatch_uncertain:" + member
    branch = cohort["dispatch"]["branch"]
    busy = [r for r in rows if r.get("event") == "workflow_dispatch"
            and r.get("head_branch") == branch and r.get("status") != "completed"]
    if busy:
        return None, "waiting_for_existing_dispatch"
    start = datetime.date.fromisoformat(cohort["schedule"]["start_date"])
    day = (now.date() - start).days
    due = [m for m in cohort["members"] if m["day"] == day
           and m["member_id"] not in originals]
    if not due:
        return None, "no_member_due"
    # Leave two hours before the UTC boundary for queueing and the bounded run. A missed
    # day stays missing; tomorrow never backfills yesterday's member.
    if now.hour >= 22:
        return None, "utc_day_closing"
    member = due[0]
    if member["kind"] not in ("aa", "preparation") and not trials_enabled:
        return None, "sealed_parameters_required"
    return member, "due"


def history(api, cohort, activation, calibration, state_dir):
    anchor = cohort["dispatch"]["after_run_number"]
    rows, seen, total = [], set(), None
    for page in range(1, 17):
        doc = api.get_json("/actions/workflows/performance.yml/runs?per_page=100&page=%d"
                           % page, "workflow history")
        require(type(doc.get("total_count")) is int and isinstance(doc.get("workflow_runs"), list),
                "workflow history is malformed")
        require(total is None or total == doc["total_count"], "history changed while reading")
        total = doc["total_count"]
        batch = doc["workflow_runs"]
        for row in batch:
            require(row["id"] not in seen, "history repeats a run")
            seen.add(row["id"])
            rows.append(row)
        if any(r["run_number"] <= anchor for r in rows) or len(rows) == total:
            break
        require(batch, "history ended before its anchor")
    else:
        raise ValueError("history exceeds sixteen pages; no dispatch was attempted")
    atomic(state_dir / "runs.json", {"total_count": total,
                                   "workflow_runs": [activation._slim_run(r) for r in rows]})
    trusted, summary = calibration._run_history(str(state_dir), cohort)
    refusals = []
    originals, _ = calibration._dispatches(cohort, trusted, lambda *x: refusals.append(x))
    require(not refusals, "cohort history has duplicate or foreign dispatches")
    return rows, originals, summary


def content(api, path, ref):
    require(re.fullmatch(r"[A-Za-z0-9_./-]+", path) and ".." not in path, "unsafe source path")
    require(re.fullmatch(r"[A-Za-z0-9_./-]+", ref) and ".." not in ref, "unsafe source ref")
    doc = api.get_json("/contents/%s?ref=%s" % (path, ref), "declared source")
    require(doc.get("type") == "file" and doc.get("encoding") == "base64"
            and type(doc.get("size")) is int and doc["size"] <= 256 * 1024,
            "declared source is not a bounded regular file")
    data = base64.b64decode(doc["content"].replace("\n", ""), validate=True)
    require(len(data) == doc["size"], "source size differs from its content")
    return data


def merged_head(api, control):
    """Prove control is reachable from one immutable main head before dispatching it."""
    head = api.get_json("/git/ref/heads/main", "main revision")["object"]["sha"]
    require(re.fullmatch(r"[0-9a-f]{40}", head), "main has no full commit identity")
    for page in range(1, 17):
        commits = api.get_json("/commits?sha=%s&per_page=100&page=%d" % (head, page),
                               "main ancestors")
        require(isinstance(commits, list) and len(commits) <= 100, "malformed ancestor listing")
        if any(c.get("sha") == control for c in commits):
            return head
        if len(commits) < 100:
            break
    raise ValueError("configured control is not proven merged within the ancestor bound")


def performance_tree(api, revision):
    """Exact scripts/performance tree identity at an immutable commit, without recursion."""
    revision = api.get_json("/git/commits/" + revision, "producer commit")["tree"]["sha"]
    require(re.fullmatch(r"[0-9a-f]{40}", revision), "producer root tree identity is malformed")
    for name in ("scripts", "performance"):
        doc = api.get_json("/git/trees/" + revision, "producer tree")
        entries = doc.get("tree")
        require(not doc.get("truncated") and isinstance(entries, list) and len(entries) <= 8192,
                "producer tree is unavailable or unbounded")
        matches = [entry for entry in entries if entry.get("path") == name]
        require(len(matches) == 1 and matches[0].get("type") == "tree"
                and matches[0].get("mode") == "040000", "producer subtree is unavailable")
        revision = matches[0]["sha"]
        require(re.fullmatch(r"[0-9a-f]{40}", revision), "producer tree identity is malformed")
    return revision


def submit(journal_path, journal, member, payload, post, stamp):
    """Durable intent precedes the single POST; any ambiguous result remains latched."""
    ident = member["member_id"]
    require(ident not in journal, "member already has a dispatch intent")
    journal[ident] = {"created_at": stamp, "state": "intent", "payload": payload}
    atomic(journal_path, journal)
    post(payload)
    journal[ident]["state"] = "submitted"
    atomic(journal_path, journal)


def cycle(dry_run=False):
    config_path = ROOT / "config.json"
    if not config_path.exists():
        return {"status": "disabled", "detail": "No merged cohort is configured."}
    config = json.loads(read(config_path))
    if config.get("enabled") is not True and not dry_run:
        return {"status": "disabled", "detail": "Operator configuration is disabled."}
    require(config.get("schema") == "af.perf-local-cohort-dispatcher/1", "unknown config schema")
    require(re.fullmatch(r"[a-z][a-z0-9_]{0,63}", config["cohort_id"]), "invalid cohort directory")
    state_dir = ROOT / "cohorts" / config["cohort_id"]
    require(state_dir.is_dir() and not state_dir.is_symlink(), "cohort directory is unavailable")
    activation, calibration, contracts, transport = runtime()
    cohort_bytes = read(state_dir / "cohort.json")
    cohort = contracts.read_json(cohort_bytes, "cohort")
    require(cohort["cohort_id"] == config["cohort_id"], "configured cohort identity differs")
    digest = contracts.canonical_digest(cohort)
    require(digest == config["cohort_digest"], "local cohort differs from the configured digest")
    require(cohort["dispatch"]["repository"] == REPOSITORY
            and cohort["dispatch"]["branch"] == "main", "another dispatch producer was configured")
    require(re.fullmatch(r"[0-9a-f]{40}", config["control_sha"]), "control must be an exact SHA")
    require(config["cohort_path"].startswith("fixtures/performance/"), "cohort is outside fixtures")
    credential = subprocess.run(["gh", "auth", "token", "--hostname", "github.com"],
                                capture_output=True, timeout=15, check=False)
    require(credential.returncode == 0, "gh could not read the operator credential")
    api = transport.Collector(transport.HttpsTransport(), REPOSITORY,
                              credential.stdout.decode().strip(), seconds=120)
    rows, originals, summary = history(api, cohort, activation, calibration, state_dir)
    journal_path = state_dir / "dispatches.json"
    journal = json.loads(read(journal_path)) if journal_path.exists() else {}
    now = datetime.datetime.now(datetime.timezone.utc)
    member, reason = choose(cohort, originals, journal, rows, now,
                            bool(config.get("calibration_path")))
    status = {"status": reason, "cohort_digest": digest, "originals": len(originals),
              "declared_members": len(cohort["members"]), "history": summary,
              "readiness": "not_evaluated", "checked_at": now.isoformat()}
    if member is None and not dry_run:
        return status
    head = merged_head(api, config["control_sha"])
    require(content(api, config["cohort_path"], config["control_sha"]) == cohort_bytes,
            "the control does not hold the exact local cohort")
    workflow = content(api, cohort["dispatch"]["workflow_path"], head)
    authority_workflow = content(api, cohort["dispatch"]["workflow_path"], cohort["authority_sha"])
    require(workflow == authority_workflow
            and hashlib.sha256(workflow).hexdigest() == config["workflow_sha256"],
            "main's workflow differs from the cohort authority; no dispatch attempted")
    require(performance_tree(api, config["control_sha"])
            == performance_tree(api, cohort["authority_sha"]),
            "control's scripts/performance differs from the cohort authority; no dispatch attempted")
    if member is None:
        # A disabled dry-run still checks the source when no member is currently due.
        require(api.get_json("/git/ref/heads/main", "main before dispatch")["object"]["sha"] == head,
                "main changed during preflight; no dispatch attempted")
        return status
    inputs = {"base": cohort["authority_sha"], "head": cohort["authority_sha"],
              "authority": cohort["authority_sha"], "control": config["control_sha"],
              "scope": "calibration", "tier": cohort["tier"], "executor": "ephemeral_hosted",
              "cohort": config["cohort_path"], "cohort_digest": digest,
              "member": member["member_id"], "dispatch_head": head}
    if member["kind"] not in ("aa", "preparation"):
        require(config["calibration_path"].startswith("fixtures/performance/"),
                "sealed parameters are outside fixtures")
        parameters = read(state_dir / "parameters.json")
        doc = contracts.read_json(parameters, "calibration-parameters")
        contracts.validate_calibration(doc, cohort=cohort)
        require(contracts.canonical_digest(doc) == config["calibration_digest"],
                "local parameters differ from the configured digest")
        require(content(api, config["calibration_path"], config["control_sha"]) == parameters,
                "the control does not hold the sealed parameters")
        inputs["calibration"] = config["calibration_path"]
    payload = {"ref": "main", "inputs": inputs}
    status.update({"member": member["member_id"], "dispatch_head": head,
                   "status": "would_dispatch"})
    require(api.get_json("/git/ref/heads/main", "main before dispatch")["object"]["sha"] == head,
            "main changed during preflight; no dispatch attempted")
    if dry_run:
        return status

    def post(value):
        result = subprocess.run([
            "gh", "api", "--hostname", "github.com", "--method", "POST",
            "repos/%s/actions/workflows/performance.yml/dispatches" % REPOSITORY, "--input", "-"
        ], input=json.dumps(value).encode(), capture_output=True, timeout=30, check=False)
        # Do not log credentials or arbitrary API/CLI output. A failed/ambiguous request
        # retains its intent and is never automatically sent again.
        require(result.returncode == 0, "dispatch response failed; inspect the retained intent")

    submit(journal_path, journal, member, payload, post, now.isoformat())
    status["status"] = "dispatch_submitted"
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    descriptor = os.open(str(ROOT / "dispatch.lock"), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("Another calibration dispatch cycle is running.")
            return 0
        status = cycle(args.dry_run)
        atomic(ROOT / "status.json", status)
        print(json.dumps(status, sort_keys=True))
        return 4 if status["status"].startswith("dispatch_uncertain:") else 0
    except Exception as error:
        status = {"status": "refused", "error_type": type(error).__name__, "detail": str(error)[:1024]}
        atomic(ROOT / "status.json", status)
        print(json.dumps(status, sort_keys=True))
        return 4
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    sys.exit(main())
