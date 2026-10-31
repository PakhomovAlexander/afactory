"""One combined overview of a local release comparison from retained native state (ADR-0240).

`overview(journal, ids, verify, scope)` reads each named original's request, state, result and
native evidence as the journal retains them, never a process exit or a file's existence alone,
and writes `af.perf-comparison-overview/1` with its Markdown and HTML renderings. Four judgments
stay apart: the original's state and terminal reason; its evidence verification
(`verify_original`, recomputed now); its coverage (required, observed, failed and missing
records or runs) with the core aggregate's own health result; and its performance verdict.

The operator declares the scope. A `full` release comparison requires a core, a telemetry, a
trace and a CPU profile original of the same exact sources, each terminal `complete`, verified
and fully covered, the core aggregate not `incomplete`, and every profile diagnosing a core
original of this overview (its `--parent`); only then is it `complete`. Anything else is
`incomplete` with every reason. A `partial` scope (a smoke, one family) is at best `partial`,
never `complete`. A complete comparison may still hold a real regression.
"""
import html
import os

from . import contracts, stats, telemetrylab
from .values import ContractError, require

SCHEMA = "af.perf-comparison-overview/1"
WORKS = ("core", "telemetry", "profile")
SCOPES = ("full", "partial")
# What a full release comparison requires: (work, diagnostic kind) of each original.
FULL = (("core", None), ("telemetry", None), ("profile", "trace"), ("profile", "cpu_profile"))
FILES = ("overview.json", "overview.md", "overview.html")


def _links(root, path, names):
    return [os.path.relpath(os.path.join(path, name), root) for name in names
            if os.path.isfile(os.path.join(path, name))]


def _core(path):
    verdict = contracts.read_file(os.path.join(path, "report", "verdict.json"), "verdict")
    aggregate = verdict["aggregate"]
    coverage = {"required": aggregate["coverage"]["expected_records"],
                "observed": aggregate["coverage"]["matched_records"],
                "failed": sum(len(case["failures"]) for case in verdict["cases"]),
                "missing": aggregate["coverage"]["missing_records"]}
    regressions = sorted(case["instance_id"] for case in verdict["cases"]
                         if case["statistical_classification"] == stats.REGRESSION)
    return coverage, {"verdict": aggregate["result"], "regressions": regressions}


def _profile_kind(path):
    """The verified profile's recipe kind and its parent core run, from its `profiles.json`."""
    doc = contracts.read_file(os.path.join(path, "profile", "profiles.json"), "profiles")
    return doc["recipe"]["kind"], doc["parent_run_id"]


def _telemetry(path):
    result, _, _ = telemetrylab.read_bundle(os.path.join(path, "run"))
    runs = result["runs"]
    failed = sum(run["status"] in ("failed", "timeout", "refused_document") for run in runs)
    missing = sum(run["status"] == "not_run" for run in runs)
    coverage = {"required": len(runs), "observed": len(runs) - missing, "failed": failed,
                "missing": missing}
    verdicts = [run["judgment"]["verdict"] for run in runs if run["judgment"] is not None]
    verdict = ("fail" if "fail" in verdicts else "pass" if verdicts and len(verdicts) == len(runs)
               and set(verdicts) == {"pass"} else "not_judged")
    return coverage, {"verdict": verdict, "regressions": []}, result["status"]


def member(journal, ident, verify):
    """One original's record; `verify(journal, ident)` is the native offline admission."""
    request, state = journal.request(ident), journal.state(ident)
    path = journal.path(ident)
    # A release overview's members are manual, PR or release originals: a nightly original is
    # verified on its own (`executor verify`, ADR-0266), never combined into a release overview.
    require(request["submission"]["kind"] != "nightly",
            "%s is a nightly original; an overview combines release comparison originals" % ident)
    # An earlier request version (ADR-0266) names no parent.
    parent = request.get("parent")
    record = {"request_id": ident, "work": request["work"], "submission": request["submission"],
              "plan_sha256": request["plan_sha256"],
              "parent": None if parent is None else parent["request_id"],
              "state": state["state"], "state_detail": state["detail"],
              "started_unix_ms": state["started_unix_ms"], "ended_unix_ms": state["ended_unix_ms"],
              "verification": {"status": "not_terminal", "detail": ""},
              "measurement_status": None, "coverage": None, "diagnostic_kind": None,
              "performance": {"verdict": None, "regressions": []}, "links": [], "reasons": []}
    reasons = record["reasons"]
    if state["state"] != "complete":
        reasons.append("%s original is %s: %s" % (request["work"], state["state"],
                                                  state["detail"] or "no terminal result"))
        return request, record
    try:
        _, _, result = verify(journal, ident)
    except (ContractError, OSError) as error:
        record["verification"] = {"status": "refused", "detail": str(error)[:4096]}
        reasons.append("%s evidence refused: %s" % (request["work"], str(error)[:512]))
        return request, record
    record["verification"] = {"status": "verified", "detail": ""}
    record["measurement_status"] = result["measurement_status"]
    root = journal.config["root"]
    if request["work"] == "core":
        record["coverage"], record["performance"] = _core(path)
        record["links"] = _links(root, path, ("report/report.html", "report/verdict.json",
                                              "history/history.html"))
        if record["performance"]["verdict"] == "incomplete":
            # The aggregate's own health result: a missing closing sentinel, an unstable
            # environment or an integrity reason leaves a fully observed shard ineligible.
            reasons.append("core aggregate result is incomplete: its health or integrity "
                           "reasons are in its report")
    elif request["work"] == "telemetry":
        record["coverage"], record["performance"], status = _telemetry(path)
        record["links"] = _links(root, path, ("run/report.html", "run/result.json"))
        if status != "complete":
            reasons.append("telemetry result is %s" % status)
    else:
        record["links"] = _links(root, path, ("profile/diagnostic.html", "profile/profiles.json",
                                              "parent-verdict.json"))
        if result["measurement_status"] == "available":
            record["diagnostic_kind"], _ = _profile_kind(path)
        if result["measurement_status"] != "available":
            reasons.append("profile is %s: %s" % (result["measurement_status"], result["detail"]))
    coverage = record["coverage"]
    if coverage is not None and (coverage["failed"] or coverage["missing"]):
        reasons.append("%s coverage: %d of %d observed, %d failed, %d missing" % (
            request["work"], coverage["observed"], coverage["required"], coverage["failed"],
            coverage["missing"]))
    return request, record


def overview(journal, ids, verify, scope):
    require(scope in SCOPES, "an overview's scope is full or partial")
    require(ids and len(ids) == len(set(ids)) and len(ids) <= 16,
            "an overview names 1 to 16 distinct originals")
    members, sources, reasons = [], None, []
    for ident in ids:
        request, record = member(journal, ident, verify)
        require(sources is None or request["sources"] == sources,
                "an overview combines originals of one exact source pair")
        sources = request["sources"]
        members.append(record)
        reasons.extend(record["reasons"])
    cores = set(record["request_id"] for record in members if record["work"] == "core")
    for record in members:
        if record["work"] == "profile" and record["parent"] not in cores:
            # A diagnosis belongs to the comparison whose regressions it profiles.
            reason = ("profile %s diagnoses %s, not a core original of this overview"
                      % (record["request_id"], record["parent"] or "no parent"))
            record["reasons"].append(reason)
            reasons.append(reason)
    if scope == "full":
        for work, kind in FULL:
            if not any(record["work"] == work and record["diagnostic_kind"] == kind
                       for record in members):
                reasons.append("no %s original: its coverage is missing, not passed"
                               % (work if kind is None else "%s %s" % (kind, work)))
    status = "incomplete" if reasons else "complete" if scope == "full" else "partial"
    doc = {"schema": SCHEMA, "journal_id": journal.config["journal_id"],
           "machine_id": journal.config["machine_id"], "sources": sources, "scope": scope,
           "status": status, "reasons": reasons, "members": members}
    return contracts.CONTRACTS["comparison-overview"][2](doc)


def markdown(doc):
    lines = ["# Performance comparison overview", "",
             "Comparison: **%s** (scope `%s`). Baseline `%s`, candidate `%s`, control `%s`." % (
                 doc["status"], doc["scope"], doc["sources"]["baseline_sha"],
                 doc["sources"]["candidate_sha"], doc["sources"]["control_sha"]), "",
             "Framework completeness, evidence verification, coverage and the performance "
             "verdict are separate: a complete comparison may hold a real regression.", "",
             "| Original | Work | State | Verification | Coverage (observed/required, failed, "
             "missing) | Performance verdict | Regressions |", "| --- | --- | --- | --- | --- | --- | --- |"]
    for m in doc["members"]:
        c = m["coverage"]
        coverage = ("%d/%d, %d, %d" % (c["observed"], c["required"], c["failed"], c["missing"])
                    if c else "n/a")
        lines.append("| `%s` | %s | %s | %s | %s | %s | %s |" % (
            m["request_id"], m["work"] + (" (%s)" % m["diagnostic_kind"]
                                          if m["diagnostic_kind"] else ""), m["state"], m["verification"]["status"], coverage,
            m["performance"]["verdict"] or "n/a",
            ", ".join("`%s`" % r for r in m["performance"]["regressions"]) or "none"))
    if doc["reasons"]:
        lines += ["", "## Why incomplete", ""] + ["- " + r.replace("\n", " ") for r in doc["reasons"]]
    lines += ["", "## Native reports", ""]
    for m in doc["members"]:
        lines += ["- `%s`: %s" % (m["request_id"], ", ".join("`%s`" % link for link in m["links"])
                                  or "none retained")]
    return ("\n".join(lines) + "\n").encode("utf-8")


def page(doc, base="."):
    """The HTML page; `base` is the journal root relative to the page's directory."""
    e = html.escape
    rows = []
    for m in doc["members"]:
        c = m["coverage"]
        links = " ".join('<a href="%s">%s</a>' % (e(os.path.join(base, link)), e(link))
                         for link in m["links"])
        rows.append("<tr><td><code>%s</code></td><td>%s</td><td>%s</td><td>%s</td><td>%s</td>"
                    "<td>%s</td><td>%s</td></tr>" % (
                        e(m["request_id"]), e(m["work"] + (" (%s)" % m["diagnostic_kind"]
                                                          if m["diagnostic_kind"] else "")),
                        e(m["state"]),
                        e(m["verification"]["status"]),
                        e("%d/%d, %d failed, %d missing" % (c["observed"], c["required"],
                          c["failed"], c["missing"]) if c else "n/a"),
                        e(m["performance"]["verdict"] or "n/a"), links))
    reasons = "".join("<li>%s</li>" % e(r) for r in doc["reasons"])
    return ("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Performance comparison "
            "overview</title></head><body><h1>Performance comparison: %s (scope %s)</h1><p>Baseline "
            "<code>%s</code>, candidate <code>%s</code>. Completeness, verification, coverage "
            "and the performance verdict are separate.</p><table><tr><th>Original</th><th>Work"
            "</th><th>State</th><th>Verification</th><th>Coverage</th><th>Verdict</th><th>"
            "Reports</th></tr>%s</table>%s</body></html>\n" % (
                e(doc["status"]), e(doc["scope"]), e(doc["sources"]["baseline_sha"]),
                e(doc["sources"]["candidate_sha"]), "".join(rows),
                "<h2>Why incomplete</h2><ul>%s</ul>" % reasons if reasons else "")).encode("utf-8")


def write(doc, output, root):
    """Write the three renderings into a new directory; an existing one is never replaced."""
    os.mkdir(output, 0o755)
    base = os.path.relpath(root, os.path.abspath(output))
    for name, data in zip(FILES, (contracts.write_bytes(doc, "comparison-overview"),
                                  markdown(doc), page(doc, base))):
        with open(os.path.join(output, name), "xb") as stream:
            stream.write(data)
