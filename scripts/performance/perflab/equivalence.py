"""Execution evidence of an equivalent-adapter bridge (p6b closeout, ADR-0184; design section 5).

A narrowly scoped API bridge keeps the calibrated cohort only with owner-reviewed equivalence:
unchanged workload, timing, oracle and policy (the protected diff `activation.protected_diff`
checks), actual golden checks on both product sources, then a post-merge A/A. The last two are
executions, and this module computes their evidence (`af.perf-equivalence/1`) from two retained
runs that the bridged authority measured, never from an authored flag or a matching path list:

- **Golden check** (`golden`). Baseline the calibrated product source (the cohort's authority),
  candidate the bridged one, measured by the bridged authority under the activation's policy,
  tier, environment and recipe. Both builds succeed with adapters of the named group, the
  bridged product through the bridge's own adapter; every cohort case is planned with its N,
  and every planned sample of both sides is present and accepted by its golden oracle.
- **Post-merge A/A** (`post_merge_aa`). One source descending from the bridge, measured against
  itself through the bridge's adapter on both sides, on CPU/image groups the cohort calibrated,
  healthy, complete, and every critical case within tolerance.

Each check records the facts its run showed: identities, digests, adapters, probe binaries,
observed groups, completeness, health and every case's classification, failures and missing
records. Its reasons are a pure function of those facts and the record's context
(`check_reasons`), so a reader re-derives them and a fetched recomputation must reproduce the
record exactly. Owner approval of semantic equivalence remains a separate administrative
prerequisite that no record discharges.
"""
import os

from . import compare, contracts, runner, stats, transport
from .values import ContractError, require, sha256_bytes

ROLES = ("golden", "post_merge_aa")
CONTEXT = ("equivalence_group", "adapter_files", "bridge_adapters", "calibrated_authority_sha",
           "bridged_authority_sha", "policy_digest", "tier", "environment_id", "recipe_id",
           "cohort_cases", "calibrated_groups")


def context(cohort, base, admission, bridged, adapters):
    """The record's context: the bridge's group, files and adapters (`adapters` is the bridged
    authority's adapter registry), the calibrated and bridged authorities, and the calibrated
    activation `base`'s policy, scope and cohort cases."""
    group, files = admission["equivalence_group"], sorted(admission["adapter_files"])
    bridge = sorted(ident for ident, entry in adapters.items()
                    if entry["equivalence_group"] == group and set(files) <= set(entry["closure"]))
    require(bridge, "no adapter of equivalence group %s holds every named adapter file in its "
            "closure" % group)
    groups = [dict(item) for item in cohort["allowed_cpu_groups"]
              if item["group_id"] in base["calibrated_groups"]]
    return {"equivalence_group": group, "adapter_files": files, "bridge_adapters": bridge,
            "calibrated_authority_sha": base["authority_sha"],
            "bridged_authority_sha": bridged, "policy_digest": base["policy_digest"],
            "tier": cohort["tier"], "environment_id": base["environment_id"],
            "recipe_id": base["recipe_id"],
            "cohort_cases": [dict(case) for case in cohort["cases"]],
            "calibrated_groups": sorted(groups, key=lambda item: item["group_id"])}


def check_of(evaluation, actions_run_id, ctx, role):
    """The check `role` of one retained run's recomputed `evaluation`, retained by Actions run
    `actions_run_id`: its facts, and the reasons they give under `ctx`."""
    plan, verdict, builds = evaluation.plan, evaluation.verdict, evaluation.builds
    sides = contracts.SIDES
    succeeded = builds is not None and builds["supervisor"]["status"] == "succeeded" and all(
        builds["sides"][side]["status"] == "succeeded" for side in sides)
    binding = builds["adapter_binding"] if builds is not None else {}
    calibrated = dict(((group["cpu_model"], group["image"]), group["group_id"])
                      for group in ctx["calibrated_groups"])
    observed = []
    for item in evaluation.evidence:
        if item.doc is None:
            observed.append(None)
            continue
        observed.append(calibrated.get((item.doc["host"]["cpu_model"].get("value"),
                                        item.doc["execution"]["sample_image"])))
    check = {
        "actions_run_id": actions_run_id, "lab_run_id": plan["run_id"],
        "plan_digest": plan["plan_digest"],
        "verdict_sha256": sha256_bytes(contracts.document_bytes(verdict)),
        "builds_sha256": verdict["inputs"]["builds_sha256"],
        "scope": plan["scope"], "authority_mode": plan["authority"]["mode"],
        "tier": plan["tier"],
        "baseline_sha": plan["resolved"]["baseline_sha"],
        "candidate_sha": plan["resolved"]["candidate_sha"],
        "authority_sha": plan["resolved"]["authority_sha"],
        "policy_digest": plan["authority"]["policy_digest"],
        "environment_id": plan["profile"]["environment_id"],
        "environment_digest": plan["profile"]["environment_digest"],
        "recipe_id": plan["profile"]["recipe_id"],
        "recipe_digest": plan["profile"]["recipe_digest"],
        "instrument_digest": plan["authority"]["instrument_digest"],
        "fixture_digest": plan["authority"]["fixture_digest"],
        "adapters": {"baseline": binding.get("baseline"), "candidate": binding.get("candidate"),
                     "baseline_group": binding.get("baseline_group"),
                     "candidate_group": binding.get("candidate_group")},
        "probe_binaries": dict((side, builds["sides"][side]["binaries"]["probe"]
                                if builds is not None else None) for side in sides),
        "builds_succeeded": succeeded, "cpu_groups": observed,
        "shards_complete": all(entry["status"] == "complete" for entry in verdict["shards"]),
        "healthy": all(entry["status"] == "healthy" for entry in verdict["health"]),
        "provenance": verdict["provenance"], "result": verdict["aggregate"]["result"],
        "cases": dict((case["instance_id"], {
            "criticality": case["criticality"], "pairs": case["pairs"],
            "classification": case["status"], "failures": len(case["failures"]),
            "missing_records": case["missing_records"]}) for case in verdict["cases"]),
        "reasons": [], "verified": False}
    check["reasons"] = check_reasons(ctx, role, check)
    check["verified"] = not check["reasons"]
    return check


def check_reasons(ctx, role, check):
    """Why the recorded check `role` does not establish its obligation under the record's
    context `ctx`; empty when it does. A pure function of the record."""
    reasons = []

    def need(condition, text):
        if not condition:
            reasons.append(text)
    need(check["provenance"] == "measured", "the run is %s evidence, not measured"
         % check["provenance"])
    need(check["authority_sha"] == ctx["bridged_authority_sha"]
         and check["authority_mode"] == "proposed",
         "the run measured with %s authority %s, not the proposed bridge %s"
         % (check["authority_mode"], check["authority_sha"], ctx["bridged_authority_sha"]))
    need(check["policy_digest"] == ctx["policy_digest"],
         "the run's policy %s is not the calibrated %s" % (check["policy_digest"],
                                                             ctx["policy_digest"]))
    need((check["tier"], check["environment_id"], check["recipe_id"])
         == (ctx["tier"], ctx["environment_id"], ctx["recipe_id"]),
         "the run measured tier %s on %s with recipe %s, not the calibrated scope"
         % (check["tier"], check["environment_id"], check["recipe_id"]))
    need(check["builds_succeeded"], "a build failed or its manifest was not retained")
    group = ctx["equivalence_group"]
    adapters = check["adapters"]
    need(adapters["baseline_group"] == group and adapters["candidate_group"] == group,
         "the sides compiled adapters of groups %s and %s, not %s"
         % (adapters["baseline_group"], adapters["candidate_group"], group))
    need(check["shards_complete"], "a shard did not complete")
    for case in ctx["cohort_cases"]:
        ident = case["instance_id"]
        found = check["cases"].get(ident)
        if found is None or (found["criticality"], found["pairs"]) != (case["criticality"],
                                                                       case["pairs"]):
            reasons.append("case %s is not planned with its cohort N %d" % (ident,
                                                                           case["pairs"]))
            continue
        need(not found["failures"] and not found["missing_records"],
             "case %s has %d failed samples and %d missing records"
             % (ident, found["failures"], found["missing_records"]))
    bridge = ctx["bridge_adapters"]
    if role == "golden":
        need(check["baseline_sha"] == ctx["calibrated_authority_sha"]
             and check["candidate_sha"] == ctx["bridged_authority_sha"],
             "the golden check compares %s with %s, not the calibrated product source with the "
             "bridged one" % (check["baseline_sha"], check["candidate_sha"]))
        need(adapters["candidate"] in bridge, "the bridged product compiled adapter %s, not the "
             "bridge's %s" % (adapters["candidate"], ", ".join(bridge)))
    else:
        need(check["baseline_sha"] == check["candidate_sha"],
             "the post-merge A/A compares two sources")
        need(adapters["baseline"] in bridge and adapters["candidate"] in bridge,
             "the post-merge A/A compiled adapters %s and %s, not the bridge's %s"
             % (adapters["baseline"], adapters["candidate"], ", ".join(bridge)))
        need(all(item is not None for item in check["cpu_groups"]),
             "a shard ran outside the calibrated CPU/image groups")
        need(check["healthy"], "the environment was unstable")
        for case in ctx["cohort_cases"]:
            found = check["cases"].get(case["instance_id"])
            if case["criticality"] == "critical" and found is not None:
                need(found["classification"] == stats.WITHIN_TOLERANCE,
                     "critical case %s is %s, not within tolerance"
                     % (case["instance_id"], found["classification"]))
        need(check["result"] == "pass", "the A/A result is %s" % check["result"])
    return [text[:4096] for text in reasons[:64]]


def record(ctx, golden, post_merge_aa):
    """The af.perf-equivalence/1 document of the two checks under `ctx`."""
    doc = dict(ctx, schema=contracts.EQUIVALENCE_SCHEMA, golden=golden,
               post_merge_aa=post_merge_aa,
               verified=golden["verified"] and post_merge_aa["verified"],
               claim=contracts.EQUIVALENCE_CLAIM)
    return contracts.validate_equivalence(doc, "equivalence")


def compute(ctx, runs):
    """The record of `runs`, {role: (run directory, Actions run ID)}, recomputed from their
    retained evidence."""
    checks = {}
    for role in ROLES:
        run_dir, actions_run_id = runs[role]
        evaluation = compare.evaluate(run_dir)
        require(evaluation.plan["run_id"].startswith("gh%d_a" % actions_run_id),
                "%s: run %s was not planned by Actions run %d"
                % (role, evaluation.plan["run_id"], actions_run_id))
        checks[role] = check_of(evaluation, actions_run_id, ctx, role)
    return record(ctx, checks["golden"], checks["post_merge_aa"])


def artifact_names(lab, shards):
    return ["af-perf-%s-run" % lab] + ["af-perf-%s-shard-%d" % (lab, index)
                                       for index in range(shards)]


def assemble(evidence_dir, lab, scratch):
    """The run directory of lab run `lab` from its downloaded artifacts below `evidence_dir`
    (one directory per artifact, named as the artifact), assembled below `scratch`."""
    name = "af-perf-%s-run" % lab
    try:
        doc, files, _ = transport.read_directory(os.path.join(evidence_dir, name))
    except OSError as error:
        raise ContractError("the run bundle %s was not retained: %s" % (name, error))
    require(doc["content"] == "run_bundle" and doc["run_id"] == lab,
            "%s holds %s of run %s" % (name, doc["content"], doc["run_id"]))
    plan_bytes, builds_bytes = files.get("run.json"), files.get("builds.json")
    require(plan_bytes is not None and builds_bytes is not None,
            "%s holds no run.json or builds.json" % name)
    plan = contracts.read_json(plan_bytes, "run", name + "/run.json")
    require(plan["run_id"] == lab, "%s plans run %s" % (name, plan["run_id"]))
    run_dir = os.path.join(scratch, lab)
    shards_root = runner.prepare_run_dir(run_dir, plan_bytes, builds_bytes)
    for shard in plan["shards"]:
        path = os.path.join(evidence_dir, "af-perf-%s-shard-%d" % (lab, shard["index"]))
        if not os.path.isdir(path):
            continue
        doc, files, _ = transport.read_directory(path)
        require(doc["run_id"] == lab and doc["content"] == "shard_bundle"
                and doc["shard"] == shard["index"], "%s holds another bundle" % path)
        target = os.path.join(shards_root, str(shard["index"]))
        for relative, data in sorted(files.items()):
            member = os.path.join(target, *relative.split("/"))
            os.makedirs(os.path.dirname(member), 0o700, exist_ok=True)
            with open(member, "xb") as stream:
                stream.write(data)
    return run_dir


def summary(doc):
    """One line per check: what ran, and whether it verified."""
    lines = []
    for role in ROLES:
        check = doc[role]
        lines.append("- %s: run %s (Actions %d), %s -> %s, adapters %s / %s: %s" % (
            role.replace("_", " "), check["lab_run_id"], check["actions_run_id"],
            check["baseline_sha"][:12], check["candidate_sha"][:12],
            check["adapters"]["baseline"] or "-", check["adapters"]["candidate"] or "-",
            "verified" if check["verified"] else "not verified (%s)"
            % "; ".join(check["reasons"][:4])))
    return lines
