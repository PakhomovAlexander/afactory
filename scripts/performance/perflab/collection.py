"""Discover completed performance workflows and collect each original laboratory run.

The archive is the idempotency record. No cursor can skip a failed collection, and every scan
has a bounded lookback, pagination and deadline. Calibration evidence is pinned on import.
"""
import re
from datetime import datetime, timedelta, timezone

from . import archive, contracts, transport
from .values import ContractError, require

WORKFLOWS = ("performance.yml", "performance-release.yml")
LAB_ARTIFACT = re.compile(r"^af-perf-(gh([1-9][0-9]*)_a[1-9][0-9]*_[a-z][a-z0-9_]*)-(run|shard-[0-9]+)$")
MAX_PAGES = 10


def completed(api, lookback_days=13, now=None):
    require(type(lookback_days) is int and 1 <= lookback_days <= 89,
            "lookback-days must be between 1 and 89")
    since = ((now or datetime.now(timezone.utc)) - timedelta(days=lookback_days)).strftime(
        "%Y-%m-%dT%H:%M:%SZ")
    found = {}
    for workflow in WORKFLOWS:
        info = api.get_json("/actions/workflows/" + workflow, "performance workflow identity")
        ident = info.get("id")
        contracts.ACTIONS_ID.validate(ident, "workflow ID")
        require(info.get("path") == ".github/workflows/" + workflow,
                "unexpected performance workflow path")
        rows, total = [], None
        for page in range(1, MAX_PAGES + 1):
            doc = api.get_json(
                "/actions/workflows/%d/runs?status=completed&created=%%3E%%3D%s&per_page=100&page=%d"
                % (ident, since, page), "completed performance runs")
            require(isinstance(doc.get("workflow_runs"), list)
                    and type(doc.get("total_count")) is int, "invalid run listing")
            require(total is None or total == doc["total_count"],
                    "run listing changed during pagination; collect again")
            total = doc["total_count"]
            rows.extend(doc["workflow_runs"])
            if len(rows) >= total:
                break
            require(doc["workflow_runs"], "run listing ended before its declared count")
        require(len(rows) == total, "run listing exceeds the collection bound")
        ids = set()
        for run in rows:
            run_id = run.get("id")
            contracts.ACTIONS_ID.validate(run_id, "run ID")
            require(run_id not in ids and run_id not in found, "duplicate run in listing")
            ids.add(run_id)
            require(run.get("workflow_id") == ident and run.get("status") == "completed"
                    and (run.get("repository") or {}).get("full_name") == api.repository,
                    "run does not belong to this completed repository workflow")
            found[run_id] = run
    return [found[key] for key in sorted(found)]


def collect(store, api, runs, emit=print):
    failed = False
    for run in runs:
        run_id = run["id"]
        try:
            artifacts = api.artifacts(run_id)
            labs = set()
            for artifact in artifacts:
                match = LAB_ARTIFACT.fullmatch(artifact["name"])
                if match:
                    require(int(match[2]) == run_id, "artifact names another Actions run")
                    labs.add(match[1])
            if not labs:
                # Installation, an unavailable schedule, and an ineligible release have no
                # measurement bundle. Keep the workflow conclusion visible in the scan log.
                emit("Actions %d: no measurement artifacts (%s)" % (run_id, run.get("conclusion")))
                failed |= run.get("conclusion") not in ("success", "skipped")
            for lab in sorted(labs):
                known = store.state().get(lab)
                if known is not None:
                    source = known.event["event"]["source"]
                    require(source["kind"] == "actions" and source["repository"] == api.repository
                            and source["actions_run_id"] == run_id,
                            "archived run has another collection source")
                else:
                    outcome, _ = transport.fetch(store, api, run_id, lab)
                    emit("Actions %d / %s: %s" % (run_id, lab, outcome))
                if (run.get("display_title") or "").startswith("af-perf calibration "):
                    store.pin(lab, "active_cohort", "Automatically retained calibration original")
        except (ContractError, OSError) as error:
            failed = True
            emit("Actions %d: collection failed: %s" % (run_id, error))
    return not failed
