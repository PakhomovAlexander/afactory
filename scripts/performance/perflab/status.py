"""The stable SDLC check. Only an eligible measured primary can satisfy enforcement.

Reports come from this workflow attempt's trusted aggregator. They are checked against the
frozen schedule and plans, never selected by a candidate-provided artifact path. Branch
protection also needs owner review of workflow changes (docs/performance-ci.md).
"""
import os
from urllib.parse import quote

from . import contracts, render, report, tomlfile
from .values import ContractError, require, sha256_bytes


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


def scheduled(schedule_dir, repository, run_id, attempt, sha):
    doc, _ = read(os.path.join(schedule_dir, "schedule.json"), "schedule")
    actions = doc["actions"]
    require((actions["repository"], actions["run_id"], actions["run_attempt"], actions["sha"])
            == (repository, run_id, attempt, sha), "schedule belongs to another workflow run")
    return doc


def planned_verdict(schedule_dir, reports_dir, item):
    """Read only the exact scheduled plan and its aggregator's verdict, in either layout."""
    plan, raw = read(os.path.join(schedule_dir, "plans", item["run_id"] + ".json"), "run")
    require(sha256_bytes(raw) == item["plan_sha256"]
            and plan["plan_digest"] == item["plan_digest"]
            and plan["run_id"] == item["run_id"], "primary plan differs from the schedule")
    flat = os.path.join(reports_dir, "verdict.json")
    if os.path.lexists(flat):
        require(not any(name.startswith("perf-report-") for name in os.listdir(reports_dir)),
                "mixed flat and named performance reports")
        verdict_path = flat
    else:
        verdict_path = os.path.join(reports_dir, "perf-report-" + item["run_id"], "verdict.json")
    verdict, _ = read(verdict_path, "verdict")
    require((verdict["run_id"], verdict["plan_digest"])
            == (plan["run_id"], plan["plan_digest"]), "verdict belongs to another plan")
    return plan, verdict


def presentation(schedule_dir, reports_dir, repository, run_id, attempt, sha,
                 qualified, details, mode, server_url="https://github.com"):
    """Present every comparison without promoting a bootstrap to primary qualification.

    Never render downloaded Markdown/HTML. Only validated, plan-bound numeric verdicts reach
    the trusted renderer. A missing or invalid comparison stays unavailable independently.
    """
    url = "%s/%s/actions/runs/%d/attempts/%d" % (
        server_url.rstrip("/"), quote(repository, safe="/"), run_id, attempt)
    sections, available, annotations = [], [], []
    missing_report = False
    try:
        doc = scheduled(schedule_dir, repository, run_id, attempt, sha)
        items = doc["comparisons"]
        if not items:
            sections.append("No comparison was scheduled: " + render.markdown_cell(doc["reason"]))
    except (ContractError, OSError, ValueError, KeyError) as error:
        items = []
        sections.append("Evidence unavailable or invalid: " + render.markdown_cell(error))
    for item in items:
        role = item["role"].replace("_", " ")
        verdict = None
        reason = item["reason"] or item["status"]
        if item["status"] == "planned":
            try:
                plan, verdict = planned_verdict(schedule_dir, reports_dir, item)
                require(verdict["scope"] == plan["scope"], "verdict scope differs from the plan")
            except (ContractError, OSError, ValueError, KeyError) as error:
                verdict = None
                reason = "evidence unavailable or invalid: %s" % error
        if verdict is None:
            label = "UNAVAILABLE (%s)" % role
            body = render.markdown_cell(reason)
            level = "warning"
            missing_report = missing_report or item["status"] == "planned"
        else:
            label = "%s (%s; %s; %s)" % (verdict["aggregate"]["result"].upper(), role,
                                                  verdict["scope"], verdict["provenance"])
            coverage = verdict["aggregate"]["coverage"]
            body = "%d/%d planned records matched; %d cases.\n\n%s" % (
                coverage["matched_records"], coverage["expected_records"],
                len(verdict["cases"]), report.compact_verdict_markdown(verdict))
            body += ("\nReport artifact: `%s` (download and open `report.html`; exact values "
                     "are in `verdict.json`)." % ("perf-report-" + item["run_id"]))
            level = "notice" if verdict["aggregate"]["result"] == "pass" else "warning"
            available.append(verdict)
        sections.append("### %s\n\n%s\n" % (render.markdown_cell(label), body))
        annotations.append((level, "%s. Enforcement: %s; primary %s. Report: %s" % (
            label, mode, "qualified" if qualified else "not qualified", url)))
    label = "UNAVAILABLE"
    if available:
        results = [entry["aggregate"]["result"] for entry in available]
        result = "fail" if "fail" in results else (
            "incomplete" if "incomplete" in results or missing_report else "pass")
        qualifiers = sorted(set(entry["scope"] for entry in available))
        qualifiers += sorted(set(entry["provenance"] for entry in available
                                 if entry["provenance"] != "measured"))
        if any(item["role"] == "primary" and item["status"] != "planned" for item in items):
            qualifiers.append("primary unavailable")
        if missing_report:
            qualifiers.append("report unavailable")
        label = "%s (%s)" % (result.upper(), "; ".join(qualifiers))
    summary = "## Performance report: %s\n\n" % render.markdown_cell(label)
    summary += ("[Run and report artifacts](%s)\n\n" % url)
    summary += "Primary required-check qualification: **%s**. Enforcement: **%s**.\n\n" % (
        "qualified pass" if qualified else "not qualified", mode)
    if mode == "advisory":
        summary += "The green check records advisory completion; it does not mean performance passed.\n\n"
    summary += "\n".join("- " + render.markdown_cell(line) for line in details) + "\n\n"
    summary += "\n".join(sections)
    if not annotations:
        annotations.append(("warning", "Performance UNAVAILABLE. Report: " + url))
    return label, summary, annotations


def evaluate(schedule_dir, reports_dir, jobs, repository, run_id, attempt, sha):
    """(qualified, details). Missing jobs or evidence never produce a qualified result."""
    doc = scheduled(schedule_dir, repository, run_id, attempt, sha)
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
    plan, verdict = planned_verdict(schedule_dir, reports_dir, item)
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
