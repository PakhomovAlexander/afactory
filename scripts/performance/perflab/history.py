"""Compatible history across runs (package p4, ADR-0172).

`history --inputs DIR... --output OUT` reads run directories and archive roots, recomputes every
run's verdict from its evidence (an archived run is rebuilt from its rehashed objects; one whose
bundles lost members to retention is shown from its archived summary, with what expired), and
writes `history.html`, the matching `history.json` (`af.perf-history/1`) and, below `runs/`, each
run's own report by the same trusted renderer, so history links to complete runs.

A group holds one case instance under one identity: its declared workload (case, parameters,
timing, cache, oracle, seed declaration and approved AF settings), the authority's declared
fixture definitions, its policy (metric, tolerance, pairs, rank, alpha, criticality, budgets),
recipe, environment and evidence provenance. A run's ID, sources, seed artifact digests and
randomized Store bytes are provenance shown on each point, never a reason to split a group; a
change to any declared identity is. A trend compares separate runs: each point is that run's
own same-host paired comparison, and nothing pairs points with each other.

Transported HTML or SVG views an archive kept are data: they are copied only as
`downloads/<sha256>.bin` and never embedded, opened or trusted for a CSP hash.
"""
import os
import shutil
import tempfile

from . import archive, compare, contracts, limits, render, report
from .values import ContractError, canonical_digest, require, sha256_bytes

HISTORY_HTML = "history.html"
HISTORY_JSON = "history.json"
RUNS = "runs"
DOWNLOADS = "downloads"
TREND = ("A trend compares separate runs. Each point is that run's own same-host paired "
         "comparison; points are not paired with each other, may come from different hosts "
         "and sources, and never form a paired verdict.")
COMPONENTS = contracts.HISTORY_COMPONENTS


def identity(plan, case, provenance):
    """The group identity of one planned case instance: what must match for two runs'
    results to belong to one compatible series."""
    workload = {"case_id": case["case_id"], "parameters": case["parameters"],
                "family": case["family"], "kind": case["kind"], "timing": case["timing"],
                "cache": case["cache"], "oracle": case["oracle"], "seed": case["seed"],
                "af_environment": case["af_environment"],
                "requires_linux_containment": case["requires_linux_containment"]}
    policy = {"algorithm": plan["verdict_policy"]["algorithm"],
              "metric": case["primary_metric"], "tolerance": case["tolerance"],
              "pairs": case["pairs"], "warmup_pairs": case["warmup_pairs"],
              "rank": case["rank"], "alpha": case["alpha"],
              "criticality": case["criticality"], "exploratory": case["exploratory"],
              "budgets": case["budgets"]}
    return {"case_id": case["case_id"], "instance_id": case["instance_id"],
            "parameters": case["parameters"], "workload": canonical_digest(workload),
            "fixture": plan["authority"]["fixture_digest"], "policy": canonical_digest(policy),
            "recipe": "%s %s" % (plan["profile"]["recipe_id"], plan["profile"]["recipe_digest"]),
            "environment": "%s %s" % (plan["profile"]["environment_id"],
                                      plan["profile"]["environment_digest"]),
            "provenance": provenance}


def _seeds(plan, case, shard_doc):
    """The seed artifacts a case's samples copied: provenance, never identity."""
    if shard_doc is None:
        return []
    found = []
    for seed in shard_doc["seeds"]:
        if seed["status"] != "ready":
            continue
        if case["seed"] is not None and seed["kind"] == "native" \
                and seed["seed"] == case["seed"]["seed_id"]:
            found.append(seed)
        elif case["kind"] == "process" and case["family"] == "cli_state" \
                and seed["kind"] == "cli":
            found.append(seed)
    return [{"side": seed["side"], "seed": seed["seed"], "digest": seed["digest"]}
            for seed in sorted(found, key=lambda item: (item["side"], item["seed"]))]


class Entry(object):
    """One run in history: its plan, report document and where they came from."""

    def __init__(self, label, source, doc, plan, shard_docs, started, retention=None,
                 views=()):
        self.label = label
        self.source = source
        self.doc = doc
        self.plan = plan
        self.shard_docs = shard_docs
        self.started = started
        self.retention = retention
        self.views = list(views)


def _load(inputs, output):
    """Every run of every input, recomputed, as Entries, and what could not be shown."""
    entries, unavailable = [], []
    for index, path in enumerate(inputs):
        label = "input %d" % (index + 1)
        if archive.is_archive(path):
            _load_archive(path, label, output, entries, unavailable)
            continue
        try:
            entries.append(_run_entry(path, label, "run_dir", output))
        except (ContractError, OSError) as error:
            unavailable.append({"input": label, "run_id": None,
                                "reason": ("%s: %s" % (type(error).__name__, error))[:4096]})
    return entries, unavailable


def _run_entry(path, label, source, output, retention=None, views=()):
    staging = os.path.join(output, RUNS, ".staging-" + os.urandom(6).hex())
    doc, evaluation = report.write_evaluated(path, staging)
    started = (evaluation.builds["execution"]["run"]["started_unix_ms"]
               if evaluation.builds is not None else None)
    entry = Entry(label, source, doc, evaluation.plan, [item.doc for item in evaluation.evidence],
                  started, retention, views)
    entry.staging = staging
    return entry


def _load_archive(root, label, output, entries, unavailable):
    with archive.Archive(root) as store:
        runs = store.state()
        for run_id, run in runs.items():
            retention = {"numeric": run.numeric(),
                         "profile_reproduction": run.profile_reproduction(),
                         "expired": sorted(set(item["class"] for item in run.expired.values()))}
            views = []
            for member in run.available():
                if member["class"] == "transported_view":
                    views.append((member["path"][len(archive.VIEWS):],
                                  store.read_object(member, limits.MAX_JSON_BYTES)))
            scratch = tempfile.mkdtemp(prefix="af-perf-history-")
            try:
                try:
                    run_dir = store.materialize(run_id, os.path.join(scratch, "run"))
                    entries.append(_run_entry(run_dir, label, "archive", output, retention,
                                              views))
                    continue
                except ContractError:
                    pass
                summary = [member for member in run.available()
                           if member["path"] == archive.SUMMARY_PATH]
                plan_member = [member for member in run.available()
                               if member["path"] == "run.json"]
                if not summary or not plan_member:
                    unavailable.append({"input": label, "run_id": run_id, "reason": (
                        "the run's numeric evidence expired; its retention record keeps the "
                        "original manifest, and nothing remains to show")})
                    continue
                doc = contracts.read_json(store.read_object(summary[0], limits.MAX_JSON_BYTES),
                                          "report", "archived report.json")
                plan = contracts.read_json(store.read_object(plan_member[0],
                                                             limits.MAX_JSON_BYTES),
                                           "run", "archived run.json")
                require(doc["run_id"] == run_id == plan["run_id"]
                        and doc["plan_digest"] == plan["plan_digest"],
                        "the archived summary of %s names another run" % run_id)
                staging = os.path.join(output, RUNS, ".staging-" + os.urandom(6).hex())
                os.makedirs(staging)
                with open(os.path.join(staging, report.REPORT_HTML), "wb") as stream:
                    stream.write(report.html_bytes(doc, downloads_present=False))
                with open(os.path.join(staging, report.REPORT_JSON), "wb") as stream:
                    stream.write(contracts.write_bytes(doc, "report"))
                entry = Entry(label, "archive_summary", doc, plan,
                              [item["terminal"] and _terminal_as_shard(item["terminal"])
                               for item in doc["shards"]], None, retention, views)
                entry.staging = staging
                entries.append(entry)
            except (ContractError, OSError) as error:
                unavailable.append({"input": label, "run_id": run_id,
                                    "reason": ("%s: %s" % (type(error).__name__,
                                                           error))[:4096]})
            finally:
                shutil.rmtree(scratch, ignore_errors=True)


def _terminal_as_shard(terminal):
    return {"seeds": terminal["seeds"]}


def build(entries, unavailable):
    """The `af.perf-history/1` document of loaded entries."""
    runs, groups = [], {}
    for entry in sorted(entries, key=lambda item: (item.started is None, item.started or 0,
                                                   item.doc["run_id"])):
        doc, plan = entry.doc, entry.plan
        verdict = doc["verdict"]
        views = []
        for name, data in entry.views:
            digest = sha256_bytes(data)
            views.append({"name": name, "path": "%s/%s.bin" % (DOWNLOADS, digest[7:]),
                          "sha256": digest, "size": len(data)})
        runs.append({"run_id": doc["run_id"], "plan_digest": doc["plan_digest"],
                     "input": entry.label, "source": entry.source,
                     "scope": plan["scope"], "tier": plan["tier"],
                     "trigger": plan["trigger"]["kind"], "provenance": verdict["provenance"],
                     "result": verdict["aggregate"]["result"],
                     "satisfies_required_check": verdict["aggregate"]["satisfies_required_check"],
                     "baseline_sha": plan["resolved"]["baseline_sha"],
                     "candidate_sha": plan["resolved"]["candidate_sha"],
                     "started_unix_ms": entry.started,
                     "report": "%s/%s/%s" % (RUNS, doc["run_id"], report.REPORT_HTML),
                     "retention": entry.retention, "views": views})
        cases = dict((case["instance_id"], case) for case in plan["cases"])
        extras = dict((item["instance_id"], item) for item in doc["cases"])
        for verdict_case in verdict["cases"]:
            case = cases[verdict_case["instance_id"]]
            ident = identity(plan, case, verdict["provenance"])
            key = contracts.history_group_id(ident)
            shard_doc = entry.shard_docs[case["shard"]] if case["shard"] < len(
                entry.shard_docs) else None
            group = groups.setdefault(key, {"group_id": key, "identity": ident, "points": []})
            group["points"].append({
                "run_id": doc["run_id"], "started_unix_ms": entry.started,
                "status": verdict_case["status"], "outcome": verdict_case["outcome"],
                "classification": verdict_case["statistical_classification"],
                "interval": verdict_case["interval"],
                "baseline_median": (verdict_case["baseline"]["median"]
                                    if verdict_case["baseline"] else None),
                "candidate_median": (verdict_case["candidate"]["median"]
                                     if verdict_case["candidate"] else None),
                "change": verdict_case["change"]["absolute"] if verdict_case["change"] else None,
                "missing_records": verdict_case["missing_records"],
                "failures": extras[verdict_case["instance_id"]]["failures"],
                "baseline_sha": plan["resolved"]["baseline_sha"],
                "candidate_sha": plan["resolved"]["candidate_sha"],
                "seeds": _seeds(plan, case, shard_doc)})
    ordered = sorted(groups.values(), key=lambda group: (group["identity"]["instance_id"],
                                                         group["group_id"]))
    for group in ordered:
        related = []
        for other in ordered:
            if other is group or other["identity"]["instance_id"] != \
                    group["identity"]["instance_id"]:
                continue
            related.append({"group_id": other["group_id"], "differs": [
                component for component in COMPONENTS
                if other["identity"][component] != group["identity"][component]]})
        group["related"] = related
    doc = {"schema": contracts.HISTORY_SCHEMA, "comparison": "trend_across_runs",
           "runs": runs, "groups": ordered, "unavailable": unavailable}
    return contracts.validate_history(doc)


def html_bytes(doc):
    sections = [render.section("What this is", render.paragraph(TREND, "note"))]
    sections.append(render.section("Runs", render.table(
        ("Run", "Input", "Source", "Scope", "Trigger", "Evidence", "Result",
         "Required check", "Baseline", "Candidate", "Retention", "Report",
         "Transported views (data only)"),
        [(run["run_id"], run["input"], run["source"], run["scope"], run["trigger"],
          run["provenance"], render.outcome(run["result"]),
          "satisfied" if run["satisfies_required_check"] else "not satisfied",
          render.code(run["baseline_sha"]), render.code(run["candidate_sha"]),
          "all evidence retained" if run["retention"] is None or not run["retention"]["expired"]
          else "expired: %s; numeric evidence %s; profile reproduction %s" % (
              ", ".join(run["retention"]["expired"]), run["retention"]["numeric"],
              run["retention"]["profile_reproduction"].replace("_", " ")),
          render.Markup(render.link(run["report"], "report")),
          render.Markup(" ".join(render.link(view["path"], "%s (%d bytes)" % (
              view["name"], view["size"])) for view in run["views"]) or "none"))
         for run in doc["runs"]], searchable=True)))
    for group in doc["groups"]:
        ident = group["identity"]
        related = "; ".join("%s differs in %s" % (item["group_id"][:19],
                                                   ", ".join(item["differs"]))
                            for item in group["related"]) or "none"
        points = []
        for index, point in enumerate(group["points"]):
            for side, value in (("baseline", point["baseline_median"]),
                                ("candidate", point["candidate_median"])):
                points.append({"sequence": index, "phase": "measured", "pair": index + 1,
                               "position": 1 if side == "baseline" else 2, "side": side,
                               "status": "ok" if value is not None else "missing",
                               "value": value, "failure": None})
        body = [render.table(("Field", "Value"), [
            ("Case", ident["instance_id"]), ("Workload", ident["workload"]),
            ("Fixture definitions", ident["fixture"]), ("Policy", ident["policy"]),
            ("Recipe", ident["recipe"]), ("Environment", ident["environment"]),
            ("Evidence", ident["provenance"]), ("Incompatible groups of this case", related)]),
            render.plot(points, "medians by run: %s" % ident["instance_id"]),
            render.table(("Run", "Status", "Outcome", "Classification", "Interval [L, U]",
                          "Baseline median", "Candidate median", "Change", "Missing",
                          "Failures b / c", "Seed artifacts (provenance)"),
                         [(point["run_id"], point["status"], render.outcome(point["outcome"]),
                           point["classification"] or "none",
                           "not computed" if point["interval"] is None else "[%s, %s]" % (
                               point["interval"]["lower"], point["interval"]["upper"]),
                           point["baseline_median"] or "—", point["candidate_median"] or "—",
                           point["change"] or "—", point["missing_records"],
                           "%d / %d" % (point["failures"]["baseline"],
                                        point["failures"]["candidate"]),
                           "; ".join("%s %s %s" % (seed["side"], seed["seed"], seed["digest"])
                                     for seed in point["seeds"]) or "none")
                          for point in group["points"]], searchable=True)]
        sections.append(render.section("Group %s: %s" % (group["group_id"][:19],
                                                          ident["instance_id"]), *body))
    if doc["unavailable"]:
        sections.append(render.section("Not shown", render.table(
            ("Input", "Run", "Why"), [(item["input"], item["run_id"] or "", item["reason"])
                                      for item in doc["unavailable"]])))
    return render.page("AF performance history", sections)


def write(inputs, output):
    """Load every input and write the history below the new directory `output`."""
    require(inputs, "--inputs: at least one run directory or archive root")
    output = report.prepare_output(output)
    for path in inputs:
        real = os.path.realpath(path)
        require(not (os.path.realpath(output) + os.sep).startswith(real + os.sep),
                "--output may not lie inside an input")
    os.makedirs(os.path.join(output, RUNS))
    entries, unavailable = _load(inputs, output)
    seen = {}
    unique = []
    for entry in entries:
        run_id = entry.doc["run_id"]
        if run_id in seen:
            require(seen[run_id].doc["plan_digest"] == entry.doc["plan_digest"]
                    and seen[run_id].doc["verdict"] == entry.doc["verdict"],
                    "run %s appears in %s and %s with different evidence" % (
                        run_id, seen[run_id].label, entry.label))
            shutil.rmtree(entry.staging)
            continue
        seen[run_id] = entry
        unique.append(entry)
    for entry in unique:
        os.rename(entry.staging, os.path.join(output, RUNS, entry.doc["run_id"]))
    doc = build(unique, unavailable)
    for entry in unique:
        for _, data in entry.views:
            # A transported view is data: an opaque download, never a page of this history.
            path = os.path.join(output, DOWNLOADS, sha256_bytes(data)[7:] + ".bin")
            if not os.path.exists(path):
                report._write_new(path, data)
    report._write_new(os.path.join(output, HISTORY_JSON), contracts.write_bytes(doc, "history"))
    report._write_new(os.path.join(output, HISTORY_HTML), html_bytes(doc))
    return doc
