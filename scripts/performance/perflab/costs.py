"""Cost and shipping-budget demonstrations from retained runs (p6b, ADR-0183, ADR-0184,
ADR-0185; design section 15).

Two acceptance obligations need real executions, which this module cannot supply; it computes
their evidence from retained run directories, whatever produced them, and says honestly what is
still missing:

- **Cost** (`cost_report`, `af.perf-cost/3`). Five real pull-request comparisons and five
  same-source comparisons, each with a cold and a warm compiled build cache. Every run given is
  retained, failed or not: its source, plan, run bundle, build manifest, binary and shard
  identities, its measured build time and, warm, the separate run-owned priming build's time,
  each shard's sampling time, every case's sample count and total sample wall time, its build and
  shard artifact bytes, and every failure. Its compiled-cache treatment is observed in its build
  manifest (`af.perf-builds/6`): cold builds started from observed empty worktree targets, warm
  ones from targets a bounded priming build of the same sources had just filled. Dependency
  preparation (`dependency_fetch`) is identified separately and is never a build cache. An
  operator's declaration must agree, and only names the cache of a run whose manifest was not
  retained. Every execution failure fails the attempt: a build or shard that failed, is missing or
  did not complete, a missing planned record, a sample that failed, timed out or lacks its
  required metric, a health sentinel that failed or an unstable shard, and any integrity refusal.
  A failed attempt stays in the document with its costs and failures and never counts. A healthy
  complete comparison that found a product regression has no failure and counts. Each group is
  demonstrated by five completed measured executions, each its own retained run and bundle
  (repeated runs of one source pair count separately); its median and maximum build, priming and
  sampling times are those of its completed runs.
- **Shipping budgets** (`shipping_report`, `af.perf-shipping/3`). Twenty shipping samples of every
  discovery command on each named environment (Linux and macOS) and of every state read on Linux,
  each strictly below its limit (50 ms and 200 ms). Every run must have frozen exactly the reviewed
  suite's policy, and its budgets, metrics, recipe, environment and instances with it; a run or a
  run bundle given twice is refused, so a sample counts once. Samples are never pooled across
  product identities: each budget instance reports one target per candidate source, shipping
  binary, recipe and observed hardware/image fingerprint, with its own denominator, maximum,
  median and nearest-rank p95, and every run and host that contributed. Every planned measured
  candidate slot of an applicable budget is accounted for in its target: a sample, a failed sample
  or a missing one; a shard that did not complete, an unusable build, an integrity refusal or an
  unhealthy shard makes its budget instances incomplete in that run while its samples stay shown.
  A target is met only by enough samples, all strictly below the limit, with no failed or missing
  planned sample and healthy evidence; a budget instance is met only when every target it holds
  is. A miss is optimization work, never a relaxed limit.

Either document is `demonstrated` only from measured evidence covering every requirement; synthetic
bundles, missing groups, too few samples and failures leave it `pending` (or `missed`) with the
reasons. Fixtures and implementation tests establish the arithmetic only.
"""
import os
from fractions import Fraction

from . import compare, contracts, schedule
from . import suite as suite_module
from .values import canonical_digest, format_rational, require

KINDS = ("pr", "same_source")
CACHES = ("cold", "warm")
RUNS_PER_GROUP = 5


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def _kind(plan):
    resolved = plan["resolved"]
    if resolved["baseline_sha"] == resolved["candidate_sha"]:
        return "same_source"
    require(plan["trigger"]["kind"] == "pr", "run %s compares two sources but is no pull-request "
            "comparison; only PR and same-source runs are cost evidence" % plan["run_id"])
    return "pr"


def _cache(builds, declared):
    """(cache, source, dependency fetch, cache identity, priming ms): the compiled-cache
    treatment observed in the build manifest when it was retained, which a declaration must
    match; otherwise the declaration, or unknown. The dependency fetch is reported beside it and
    never decides it."""
    require(declared in (None,) + CACHES, "a run's build cache is cold or warm")
    if builds is None:
        return (declared, ("declared" if declared is not None else "unknown"), None, None,
                None)
    record = builds["build_cache"]
    observed = record["treatment"]
    require(declared in (None, observed), "run %s built from %s worktree targets, a %s compiled "
            "cache; the declared %s cache is not what its build manifest shows"
            % (builds["run_id"], "primed" if observed == "warm" else "observed empty",
               observed, declared))
    priming = record["priming"]
    priming_ms = None if priming is None else priming["ended_unix_ms"] - priming[
        "started_unix_ms"]
    return (observed, "observed", builds["isolation"]["dependency_fetch"], record["identity"],
            priming_ms)


def execution_failures(evaluation):
    """Every way a run's retained execution evidence is not a valid, healthy execution of its
    plan, as (shard index or None for the whole run, failure). Derived from the build manifest,
    each shard's terminal record, the planned records, every sample failure (a timeout or a
    missing required metric included), shard health and the verdict's integrity reasons; a
    healthy, complete comparison that found a regression has none."""
    plan, verdict = evaluation.plan, evaluation.verdict
    builds = evaluation.builds or evaluation.recorded_builds
    out = []

    def fail(shard, where, detail):
        out.append((shard, {"where": where, "detail": detail[:1024]}))
    if builds is None:
        fail(None, "build", "no valid build manifest was retained")
    else:
        if evaluation.builds is None:
            fail(None, "build", "no build of the manifest is usable")
        for side in contracts.SIDES:
            if builds["sides"][side]["status"] != "succeeded":
                fail(None, "build", "the %s build failed" % side)
        if builds["supervisor"]["status"] != "succeeded":
            fail(None, "build", "the supervisor build failed")
    for evidence in evaluation.evidence:
        doc = evidence.doc
        if doc is None:
            fail(evidence.index, "shard %d" % evidence.index,
                 "no valid shard evidence was retained")
        elif doc["status"] != "complete":
            fail(evidence.index, "shard %d" % evidence.index, "%s: %s" % (
                doc["status"], (doc["failure"] or {}).get("detail", "")))
    missing = verdict["aggregate"]["coverage"]["missing_records"]
    if missing:
        fail(None, "records", "%d planned records are missing" % missing)
    for entry in verdict["health"]:
        if entry["status"] != "healthy":
            fail(entry["shard"], "health shard %d" % entry["shard"], "%s: %s" % (
                entry["status"], ", ".join(entry["flags"]) or "no flag"))
    covered = ("missing_records", "missing_shard", "shard_not_complete")
    for reason in verdict["aggregate"]["reasons"]:
        if reason["code"] in compare.INTEGRITY_CODES and reason["code"] not in covered:
            fail(reason["shard"], "integrity", "%s: %s" % (reason["code"], reason["detail"]))
    for entry in verdict["cases"]:
        for failure in entry["failures"]:
            fail(entry["shard"], "case %s" % entry["instance_id"],
                 "%s %s sample of pair %d failed: %s" % (failure["side"], failure["phase"],
                                                         failure["pair"], failure["kind"]))
    return out


def run_cost(run_dir, cache=None):
    """The cost record of one retained run directory; `cache` is the operator's declaration of
    its build cache, which must agree with what its build manifest shows."""
    evaluation = compare.evaluate(run_dir)
    plan, verdict = evaluation.plan, evaluation.verdict
    # A failed build's manifest still records its time, cache and failure.
    builds = evaluation.builds or evaluation.recorded_builds
    cache, source, fetch, identity, priming_ms = _cache(builds, cache)
    build_ms = build_bytes = None
    binaries = dict((side, {"af": None, "probe": None}) for side in contracts.SIDES)
    supervisor = None
    if builds is not None:
        execution = builds["execution"]
        # The measured build pass only: a warm run's priming is reported on its own.
        build_ms = execution["ended_unix_ms"] - builds["build_cache"]["measured_started_unix_ms"]
        build_bytes = builds["artifacts"]["bytes"]
        supervisor = builds["supervisor"]["binary_sha256"]
        for side in contracts.SIDES:
            binaries[side] = dict(builds["sides"][side]["binaries"])
    shards, shard_bytes = [], 0
    for evidence in evaluation.evidence:
        doc = evidence.doc
        if doc is None:
            shards.append(None)
            continue
        shards.append(doc["ended_unix_ms"] - doc["started_unix_ms"])
        shard_bytes += doc["samples_bytes"] + doc["logs_bytes"]
    failures = [failure for _, failure in execution_failures(evaluation)]
    cases = {}
    for case in plan["cases"]:
        records = [record for record in evaluation.evidence[case["shard"]].records.values()
                   if record["instance_id"] == case["instance_id"]]
        timed = [record["execution"]["host_wall_ns"] for record in records
                 if record["execution"]["host_wall_ns"] is not None]
        cases[case["instance_id"]] = {
            "samples": len(records), "unmeasured": len(records) - len(timed),
            "sample_ms": format_rational(Fraction(sum(timed), 1000000)) if timed else None}
    resolved = plan["resolved"]
    return {"run_id": plan["run_id"], "kind": _kind(plan), "cache": cache,
            "cache_source": source, "cache_identity": identity, "dependency_fetch": fetch,
            "execution": "failed" if failures else "complete",
            "provenance": verdict["provenance"], "environment_id": plan["profile"]["environment_id"],
            "recipe_id": plan["profile"]["recipe_id"], "result": verdict["aggregate"]["result"],
            "identity": {"baseline_sha": resolved["baseline_sha"],
                         "candidate_sha": resolved["candidate_sha"],
                         "plan_digest": plan["plan_digest"],
                         "run_bundle": verdict["inputs"]["run_bundle"],
                         "builds_sha256": verdict["inputs"]["builds_sha256"],
                         "binaries": dict(binaries, supervisor=supervisor),
                         "shard_bundles": [item["bundle"]
                                           for item in verdict["inputs"]["shard_bundles"]]},
            "build_ms": build_ms, "priming_ms": priming_ms, "shard_sampling_ms": shards,
            "artifact_bytes": {"build": build_bytes, "shards": shard_bytes},
            "cases": cases, "failures": failures[:256]}


def _counted(run):
    """Whether a run counts toward its group: measured and executed without any failure."""
    return run["execution"] == "complete" and run["provenance"] == "measured"


def _spread(values):
    return ((format_rational(median(values)) if values else None),
            (max(values) if values else None))


def cost_groups(runs):
    """Each kind and cache group's runs, completed measured executions (one per retained run
    and bundle, never collapsed by source pair), failed attempts and failures, and its completed
    runs' build, priming and sampling median/maximum."""
    groups = {}
    for kind in KINDS:
        for cache in CACHES:
            members = [run for run in runs if (run["kind"], run["cache"]) == (kind, cache)]
            done = [run for run in members if _counted(run)]
            builds = _spread([run["build_ms"] for run in done])
            priming = _spread([run["priming_ms"] for run in done
                               if run["priming_ms"] is not None])
            sampling = _spread([max(run["shard_sampling_ms"]) for run in done])
            groups["%s_%s" % (kind, cache)] = {
                "runs": len(members),
                "completed": len(set((run["run_id"], run["identity"]["run_bundle"])
                                     for run in done)),
                "failed": sum(1 for run in members if run["execution"] == "failed"),
                "failures": sum(len(run["failures"]) for run in members),
                "build_ms_median": builds[0], "build_ms_max": builds[1],
                "priming_ms_median": priming[0], "priming_ms_max": priming[1],
                "sampling_ms_median": sampling[0], "sampling_ms_max": sampling[1]}
    return groups


def failed_attempts(runs):
    """Every retained run that did not execute completely, by run ID: never counted, never
    dropped."""
    return sorted(run["run_id"] for run in runs if run["execution"] == "failed")


def cost_status(runs, groups):
    """(status, reasons): demonstrated only with five completed measured executions in every
    group."""
    reasons = []
    for name, group in sorted(groups.items()):
        if group["completed"] < RUNS_PER_GROUP:
            reasons.append("%s: %d of %d completed measured executions (%d failed attempts)"
                           % (name, group["completed"], RUNS_PER_GROUP, group["failed"]))
    synthetic = sorted(run["run_id"] for run in runs if run["provenance"] != "measured")
    if synthetic:
        reasons.append("not measured evidence: " + ", ".join(synthetic[:16]))
    unknown = sorted(run["run_id"] for run in runs if run["cache"] is None)
    if unknown:
        reasons.append("no build manifest or declaration shows the cache of: "
                       + ", ".join(unknown[:16]))
    return ("pending" if reasons else "demonstrated"), reasons


def cost_report(entries):
    """The af.perf-cost/3 document of `entries`, (run directory, declared cache or None)
    pairs. A run, or a run bundle, given twice is refused."""
    runs = [run_cost(path, cache) for path, cache in entries]
    ids = [run["run_id"] for run in runs]
    require(len(set(ids)) == len(ids), "a run is given twice")
    bundles = [run["identity"]["run_bundle"] for run in runs
               if run["identity"]["run_bundle"] is not None]
    require(len(set(bundles)) == len(bundles), "a run bundle is given twice")
    runs.sort(key=lambda run: run["run_id"])
    groups = cost_groups(runs)
    status, reasons = cost_status(runs, groups)
    doc = {"schema": contracts.COST_SCHEMA, "status": status, "reasons": reasons,
           "runs_per_group": RUNS_PER_GROUP, "runs": runs, "groups": groups,
           "failed_attempts": failed_attempts(runs)}
    return contracts.validate_cost(doc, "cost")


# Shipping budgets ------------------------------------------------------------------------------

def required_budgets(suite):
    """Every (budget, environment, instance) a shipping demonstration covers: each budget's named
    environments, and every instance of a case carrying it in one of the budget's tiers that the
    environment supports."""
    doc = suite.doc
    required = {}
    for budget_id, budget in sorted(doc["budgets"].items()):
        for environment in budget["environments"]:
            instances = set()
            for tier in budget["tiers"]:
                for item in suite.instances(tier):
                    case = doc["cases"][item["case_id"]]
                    if budget_id in case["budgets"] and suite_module.supports(
                            doc, environment, item):
                        instances.add(item["instance_id"])
            for ident in sorted(instances):
                required[(budget_id, environment, ident)] = budget
    return required


def _bound_budget(plan, case, budget, required):
    """Refuse a run whose frozen budget is not the reviewed suite's, before any sample counts."""
    expected = required.get((budget["budget_id"], plan["profile"]["environment_id"],
                             case["instance_id"]))
    if expected is None:
        return
    frozen = dict((key, budget[key]) for key in ("metric", "limit", "unit", "minimum_samples"))
    reviewed = dict((key, expected[key]) for key in ("metric", "limit", "unit",
                                                     "minimum_samples"))
    require(frozen == reviewed, "run %s froze budget %s of %s as %s, not the reviewed suite's %s"
            % (plan["run_id"], budget["budget_id"], case["instance_id"], frozen, reviewed))


# The observed host fields that identify one hardware/image environment group; two shards pool
# samples only when all of them, and the sample image, are equal.
FINGERPRINT_KEYS = ("cpu_model", "logical_cpus", "memory_bytes", "virtualization", "kernel",
                    "os_image", "libc")
UNKNOWN_HOST = "unknown"


def host_fingerprint(doc):
    """(digest, label) of a shard's observed hardware/image group; unknown without evidence."""
    if doc is None:
        return canonical_digest({"host": UNKNOWN_HOST}), UNKNOWN_HOST
    host = doc["host"]
    values = dict((key, host[key].get("value")) for key in FINGERPRINT_KEYS)
    values["sample_image"] = doc["execution"]["sample_image"]
    label = "%s; %s" % (host["os_image"].get("value") or "unknown",
                        host["cpu_model"].get("value") or "unknown")
    return canonical_digest(values), label[:256]


def nearest_rank_p95(values):
    """The nearest-rank 95th percentile: the ceil(0.95 n)-th smallest sample."""
    ordered = sorted(values)
    return ordered[(95 * len(ordered) + 99) // 100 - 1]


def distribution(values):
    """(maximum, median, nearest-rank p95) of every retained sample, exactly; None without any."""
    if not values:
        return None, None, None
    return (format_rational(max(values)), format_rational(median(values)),
            format_rational(nearest_rank_p95(values)))


def _relevant_failures(evaluation):
    """{shard or None: [detail]} of the build, integrity and health failures that leave a run's
    budget instances incomplete; per-slot sample failures are counted where they occur."""
    out = {}
    for shard, failure in execution_failures(evaluation):
        if failure["where"].startswith("case ") or failure["where"] == "records":
            continue
        out.setdefault(shard, []).append("%s: %s" % (failure["where"], failure["detail"]))
    return out


def shipping_report(suite, run_dirs):
    """The af.perf-shipping/3 document of the candidate's measured samples in `run_dirs`, under
    exactly the reviewed `suite`, one target per product identity and environment group."""
    required = required_budgets(suite)
    entries = dict((key, {"targets": {}, "runs": set()}) for key in required)
    reasons, runs, seen, bundles = [], [], set(), set()
    for run_dir in run_dirs:
        evaluation = compare.evaluate(run_dir)
        plan, verdict = evaluation.plan, evaluation.verdict
        require(plan["run_id"] not in seen, "run %s is given twice; a sample counts once"
                % plan["run_id"])
        seen.add(plan["run_id"])
        bundle = verdict["inputs"]["run_bundle"]
        require(bundle is None or bundle not in bundles, "run bundle %s is given twice under "
                "another run; a sample counts once" % bundle)
        bundles.add(bundle)
        require(plan["authority"]["policy_digest"] == suite.policy_digest,
                "run %s froze policy %s, not the reviewed suite's %s; samples planned under "
                "another policy never count" % (plan["run_id"], plan["authority"][
                    "policy_digest"], suite.policy_digest))
        for case in plan["cases"]:
            for budget in case["budgets"]:
                _bound_budget(plan, case, budget, required)
        environment = plan["profile"]["environment_id"]
        binary = (None if evaluation.builds is None else
                  evaluation.builds["sides"][schedule.CANDIDATE]["binaries"]["af"])
        runs.append({"run_id": plan["run_id"], "plan_digest": plan["plan_digest"],
                     "run_bundle": bundle, "builds_sha256": verdict["inputs"]["builds_sha256"],
                     "candidate_sha": plan["resolved"]["candidate_sha"],
                     "candidate_binary": binary, "environment_id": environment,
                     "recipe_id": plan["profile"]["recipe_id"],
                     "provenance": verdict["provenance"],
                     "hosts": sorted(set(host_fingerprint(item.doc)[0]
                                         for item in evaluation.evidence))})
        if verdict["provenance"] != "measured":
            reasons.append("run %s is not measured evidence and counts nothing" % plan["run_id"])
            continue
        failures = _relevant_failures(evaluation)
        for case in plan["cases"]:
            evidence = evaluation.evidence[case["shard"]]
            for budget in case["budgets"]:
                key = (budget["budget_id"], environment, case["instance_id"])
                if key not in required or not budget["applicable"] \
                        or plan["profile"]["recipe_id"] not in required[key]["recipes"]:
                    continue
                fingerprint, label = host_fingerprint(evidence.doc)
                identity = (plan["resolved"]["candidate_sha"], binary,
                            plan["profile"]["recipe_id"], fingerprint)
                target = entries[key]["targets"].setdefault(identity, {
                    "candidate_sha": identity[0], "candidate_binary": binary,
                    "recipe_id": identity[2], "host_fingerprint": fingerprint, "host": label,
                    "samples": [], "failed": 0, "missing": 0, "planned": 0, "runs": set(),
                    "incomplete_runs": set()})
                entries[key]["runs"].add(plan["run_id"])
                unhealthy = failures.get(None, []) + failures.get(case["shard"], [])
                _count_slots(plan, case, evidence, required[key]["metric"], target, reasons,
                             unhealthy)
    out = []
    for key in sorted(required):
        budget_id, environment, ident = key
        budget, entry = required[key], entries[key]
        targets = []
        for identity in sorted(entry["targets"], key=lambda item: (
                item[0], item[1] or "", item[2], item[3])):
            target = entry["targets"][identity]
            values = target["samples"]
            high, middle, p95 = distribution(values)
            record = {"candidate_sha": target["candidate_sha"],
                      "candidate_binary": target["candidate_binary"],
                      "recipe_id": target["recipe_id"],
                      "host_fingerprint": target["host_fingerprint"], "host": target["host"],
                      "planned": target["planned"], "failed": target["failed"],
                      "missing": target["missing"],
                      "samples": [format_rational(value) for value in values[:4096]],
                      "max": high, "median": middle, "p95": p95,
                      "runs": sorted(target["runs"]),
                      "incomplete_runs": sorted(target["incomplete_runs"])}
            record["result"] = shipping_result(
                values, dict(record, limit=budget["limit"],
                             minimum_samples=budget["minimum_samples"]))
            targets.append(record)
        doc_entry = {"budget_id": budget_id, "environment_id": environment,
                     "instance_id": ident, "limit": budget["limit"], "unit": budget["unit"],
                     "minimum_samples": budget["minimum_samples"],
                     "targets": targets[:64], "runs": sorted(entry["runs"])}
        doc_entry["result"] = budget_result(doc_entry)
        out.append(doc_entry)
    doc = {"schema": contracts.SHIPPING_SCHEMA, "suite_sha256": suite.raw_sha256,
           "policy_digest": suite.policy_digest,
           "status": shipping_status(out, reasons), "reasons": sorted(set(reasons))[:256],
           "runs": sorted(runs, key=lambda run: run["run_id"]), "budgets": out}
    return contracts.validate_shipping(doc, "shipping")


def _count_slots(plan, case, evidence, metric, target, reasons, unhealthy):
    """Account for every planned measured candidate slot of one budget instance in one run: a
    sample, a failed sample or a missing one; a shard that did not complete, or a build,
    integrity or health failure of the run or its shard, leaves the instance incomplete in that
    run whatever it recorded, its samples still shown."""
    run_id = plan["run_id"]
    slots = [slot for slot in schedule.expected_slots(plan, case["shard"])
             if slot["instance_id"] == case["instance_id"] and slot["phase"] == "measured"
             and slot["side"] == schedule.CANDIDATE]
    target["planned"] += len(slots)
    target["runs"].add(run_id)
    if evidence.doc is None or evidence.doc["status"] != "complete":
        target["incomplete_runs"].add(run_id)
        reasons.append("run %s: shard %d of %s did not complete"
                       % (run_id, case["shard"], case["instance_id"]))
    for detail in unhealthy:
        target["incomplete_runs"].add(run_id)
        reasons.append(("run %s: %s is not valid, healthy evidence: %s"
                        % (run_id, case["instance_id"], detail))[:4096])
    for slot in slots:
        record = evidence.records.get(schedule.slot_key(slot))
        if record is None:
            target["missing"] += 1
            target["incomplete_runs"].add(run_id)
            reasons.append("run %s: a planned %s sample is missing" % (run_id,
                                                                       case["instance_id"]))
            continue
        if record["status"] != "ok" or metric not in record["metrics"]:
            target["failed"] += 1
            target["incomplete_runs"].add(run_id)
            reasons.append("run %s: a failed or unmeasured %s sample" % (run_id,
                                                                         case["instance_id"]))
            continue
        target["samples"].append(Fraction(record["metrics"][metric]["value"]))


def shipping_result(values, entry):
    """One target's result: missed when any sample reaches the limit; incomplete when a planned
    sample failed, is missing or its run's evidence is incomplete or unhealthy; insufficient
    below the minimum; met otherwise."""
    limit = Fraction(entry["limit"])
    if any(value >= limit for value in values):
        return "missed"
    if entry["failed"] or entry["missing"] or entry["incomplete_runs"]:
        return "incomplete"
    if len(values) < entry["minimum_samples"]:
        return "insufficient"
    return "met"


def budget_result(entry):
    """A budget instance's result over its targets: missed when any target missed, incomplete
    when any is, insufficient without targets or when any lacks samples, met only when every
    target is met on its own samples."""
    results = [target["result"] for target in entry["targets"]]
    for result in ("missed", "incomplete", "insufficient"):
        if result in results:
            return result
    return "met" if results else "insufficient"


def shipping_status(entries, reasons):
    if any(entry["result"] == "missed" for entry in entries):
        return "missed"
    if reasons or any(entry["result"] != "met" for entry in entries):
        return "pending"
    return "demonstrated"


def summary_markdown(doc):
    """A compact honest summary of either demonstration."""
    lines = []
    if doc["schema"] == contracts.COST_SCHEMA:
        lines += ["### AF cost demonstration: %s" % doc["status"], "",
                  "| Group | Runs | Completed | Failed | Build median / max ms | "
                  "Priming median / max ms | Sampling median / max ms | Failures |",
                  "| --- | --- | --- | --- | --- | --- | --- | --- |"]
        for name, group in sorted(doc["groups"].items()):
            lines.append("| %s | %d | %d | %d | %s / %s | %s / %s | %s / %s | %d |" % (
                name, group["runs"], group["completed"], group["failed"],
                group["build_ms_median"] or "-",
                "-" if group["build_ms_max"] is None else group["build_ms_max"],
                group["priming_ms_median"] or "-",
                "-" if group["priming_ms_max"] is None else group["priming_ms_max"],
                group["sampling_ms_median"] or "-",
                "-" if group["sampling_ms_max"] is None else group["sampling_ms_max"],
                group["failures"]))
        lines += ["", "Failed attempts, retained and never counted: %s"
                  % (", ".join(doc["failed_attempts"][:32]) or "none")]
    else:
        lines += ["### AF shipping budgets: %s" % doc["status"], "",
                  "| Budget | Environment | Instance | Candidate | Binary | Host | Samples | "
                  "Failed | Missing | Max | Median | p95 | Result |",
                  "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
        for entry in doc["budgets"]:
            if not entry["targets"]:
                lines.append("| %s | %s | %s | - | - | - | 0 of 0 | 0 | 0 | - | - | - | %s |"
                             % (entry["budget_id"], entry["environment_id"],
                                entry["instance_id"], entry["result"]))
            for target in entry["targets"]:
                lines.append("| %s | %s | %s | %s | %s | %s | %d of %d | %d | %d | %s | %s | %s "
                             "| %s |" % (
                                 entry["budget_id"], entry["environment_id"],
                                 entry["instance_id"], target["candidate_sha"][:12],
                                 (target["candidate_binary"] or "-")[:19], target["host"],
                                 len(target["samples"]), target["planned"], target["failed"],
                                 target["missing"], target["max"] or "-",
                                 target["median"] or "-", target["p95"] or "-",
                                 target["result"]))
    lines += [""] + ["- " + reason for reason in doc["reasons"]] + [""]
    return "\n".join(lines)


def write_new(path, doc, contract):
    data = contracts.write_bytes(doc, contract)
    with open(path, "xb") as stream:
        stream.write(data)
    return os.path.abspath(path)
