"""The stable SDLC check. Only an eligible measured primary can satisfy enforcement.

Reports come from this workflow attempt's trusted aggregator. They are checked against the
frozen schedule and plans, never selected by a candidate-provided artifact path. Branch
protection also needs owner review of workflow changes (docs/performance-ci.md).
"""
import os

from . import contracts, tomlfile
from .values import require, sha256_bytes


def enforcement(root):
    path = os.path.join(root, "fixtures/performance/enforcement.toml")
    with open(path, "rb") as stream:
        data = tomlfile.loads(stream.read(tomlfile.MAX_TOML_BYTES + 1), path)
    contracts.validate_enforcement(data)
    return data["mode"]


def read(path, kind):
    with open(path, "rb") as stream:
        raw = stream.read(contracts.limits.MAX_JSON_BYTES + 1)
    return contracts.read_json(raw, kind, path), raw


def effective_enforcement(mode, event):
    require(event in ("pull_request", "push", "schedule", "workflow_dispatch", "workflow_run"),
            "unknown performance workflow event")
    # Calibration originals must be able to succeed before activation. Explicit experiments
    # and the post-publication release handoff are advisory, even after PR gating is enabled.
    return mode if event in ("pull_request", "push") else "advisory"


def evaluate(schedule_dir, reports_dir, jobs, repository, run_id, attempt, sha):
    """(qualified, details). Missing jobs or evidence never produce a qualified result."""
    doc, _ = read(os.path.join(schedule_dir, "schedule.json"), "schedule")
    actions = doc["actions"]
    require((actions["repository"], actions["run_id"], actions["run_attempt"], actions["sha"])
            == (repository, run_id, attempt, sha), "schedule belongs to another workflow run")
    details = []
    for name in ("plan", "build", "shard", "aggregate"):
        result = jobs.get(name, {}).get("result", "missing")
        if result != "success":
            details.append("%s job: %s" % (name, result))
    primary = [item for item in doc["comparisons"] if item["role"] == "primary"]
    require(len(primary) == 1, "no unique primary comparison in the schedule")
    item = primary[0]
    if doc["status"] != "scheduled" or item["status"] != "planned":
        details.append("primary unavailable: %s" % (item["reason"] or doc["status"]))
        return False, details
    plan, raw = read(os.path.join(schedule_dir, "plans", item["run_id"] + ".json"), "run")
    require(sha256_bytes(raw) == item["plan_sha256"]
            and plan["plan_digest"] == item["plan_digest"]
            and plan["run_id"] == item["run_id"], "primary plan differs from the schedule")
    verdict, _ = read(os.path.join(reports_dir, "perf-report-" + item["run_id"],
                                   "verdict.json"), "verdict")
    require((verdict["run_id"], verdict["plan_digest"])
            == (plan["run_id"], plan["plan_digest"]), "verdict belongs to another plan")
    eligible = (item["required_check_eligible"] and plan["required_check_eligible"]
                and plan["scope"] == "full" and plan["authority"]["mode"] == "active")
    if not eligible:
        details.append("primary has no eligible full scope under an active authority")
    aggregate = verdict["aggregate"]
    if not aggregate["satisfies_required_check"]:
        details.append("primary verdict: %s; does not satisfy the required check"
                       % aggregate["result"])
    if any(s["status"] != "complete" or s["matched_records"] != s["expected_records"]
           for s in verdict["shards"]):
        details.append("primary has missing or incomplete shards")
    return not details, details
