"""Offline reports of one run (package p4, ADR-0172).

`report --run DIR --output OUT` recomputes the run's verdict from its retained evidence
(`compare.evaluate`: every bundle rehashed, every planned slot matched) and writes, below a new
OUT, `report.html` (self-contained, `perflab.render`), the matching `report.json`
(`af.perf-report/3`, ADR-0177, ADR-0199), the recomputed `verdict.json`, the compact GitHub job-summary table
`summary.md` and `evidence/`, verified copies of the run's bundle members that the page links
as downloads. Nothing is added, dropped, retried or reclassified: the report shows the
verdict's classifications, its exact rational bounds and every planned slot as a raw point,
warmups, failures, sentinels and slots without an admitted record included.

A report never turns incomplete, advisory, bootstrap, proposed-policy or synthetic evidence into
a passing required check: it states exactly why a run does not satisfy it. It keeps measured
results apart from calibrated sensitivity, never infers a per-run minimum detectable effect from
an interval, and says what is unavailable rather than inventing it.

Diagnostic child runs (`--diagnostic DIR`, ADR-0177) are linked, never merged: each child bundle
is verified (every file rehashed, no symlink, traversal or unlisted file), its profiles.json must
name this run and plan as its parent and these sources, and every artifact it names must be a
verified member with that hash. Hashes prove integrity, not safety or consistency (ADR-0178): the
frozen child plan must carry this run's resolved sources, selectors and authority and each
selected case's workload (ADR-0179), and the profiles' sources, recipe and its frozen kind,
capture bounds and selection must be the child plan's own. Every view
is then admitted against the trusted renderer: each sanitized flamegraph must be exactly what
`svgsafe` makes of its original, each Perfetto export the export of its validated trace, and the
child's page exactly the page `diagnostics.page` draws from the admitted profiles and traces, so a
page or SVG with a script, an event attribute or an external reference is refused, never linked;
inferno's original SVG is archived evidence that no page links. A verified copy of the whole
child goes below the report's `diagnostics/<child run>/`, its own
page kept as the detailed view; the report shows each case's flamegraphs, trace and timeline
links, coverage, overhead labels and omissions. An unrelated, inconsistent, tampered or unsafe
child is refused. The parent's verdict and archived bundle and the child's bundle never change; a
report is a separate derived directory.
"""
from fractions import Fraction
import os

from . import (bundle, compare, contracts, diagnostics, limits, phasetrace, render,
               reproduce, schedule, svgsafe, telemetrydiag)
from .values import ContractError, document_bytes, require, sha256_bytes

REPORT_HTML = "report.html"
REPORT_JSON = "report.json"
VERDICT_JSON = "verdict.json"
SUMMARY_MD = "summary.md"
EVIDENCE = "evidence"
DIAGNOSTICS = "diagnostics"
TELEMETRY_UNAVAILABLE = ("no linked diagnostic child run recorded this case's telemetry: a "
                         "child run selects it explicitly (cli.py profile ... --telemetry spans, "
                         "ADR-0199); only CLI cases that run a kernel run record telemetry")
PROFILE_UNAVAILABLE = ("no linked diagnostic child run covers this case: CPU profiles and "
                       "phase traces are separate diagnostic child runs (cli.py profile --plan "
                       "run.json --case ID, ADR-0174) that never change this verdict; link one "
                       "with report --diagnostic DIR")
# What the comparison is, worded by the kind of evidence the verdict names: a measured run's
# retained records say what ran where; a synthetic run's values were constructed, and nothing ran.
# The measured sentence holds only for a run whose every planned record was admitted and
# succeeded in complete shards; `comparison` words any other measured or mixed run from its
# admitted records and names what is missing or failed (ADR-0187).
COMPARISON = {
    "measured": ("Measured evidence: every planned record was admitted and succeeded; each "
                 "pair's baseline and candidate samples were recorded within one shard, on one "
                 "host, the one its terminal record names (see Shards), interleaved in the "
                 "plan's seeded AB/BA order; intervals are this run's own paired comparison."),
    "synthetic": ("Synthetic evidence: constructed fixture values, not measurements. No "
                  "baseline or candidate was built or executed and no host ran these pairs; "
                  "the verdict, intervals and points are the plan's paired comparison computed "
                  "over the constructed values."),
    "mixed": ("Mixed evidence: some shard bundles hold measured records and some constructed "
              "synthetic values. Only a measured shard's records say what ran, on the host its "
              "terminal record names (see Shards); a synthetic shard's values were never "
              "executed. Intervals are the plan's paired comparison over both."),
    "none": ("No admitted evidence: no shard bundle was admitted, so this report shows no "
             "execution and no paired comparison, only what is missing."),
}
SENSITIVITY = {
    "measured": ("These are measured results, not detection sensitivity: no minimum detectable "
                 "effect is inferred from this run's intervals."),
    "synthetic": ("These are constructed values, neither measured results nor detection "
                  "sensitivity: no minimum detectable effect is inferred from this run's "
                  "intervals."),
    "mixed": ("These mix measured results with constructed values and are not detection "
              "sensitivity: no minimum detectable effect is inferred from this run's "
              "intervals."),
    "none": ("No result was admitted, and nothing here is detection sensitivity."),
}
CALIBRATION = ("Sensitivity is established only by a calibration cohort's readiness evidence "
               "(design section 15).")


def _hosts(doc):
    hosts = sorted(set(shard["terminal"]["host_id"] for shard in doc["shards"]
                       if shard["terminal"] is not None))
    if not hosts:
        return "no host that a terminal record names"
    if len(hosts) == 1:
        return "the one host their terminal records name"
    return "the %d hosts their terminal records name" % len(hosts)


def comparison(doc):
    """What the comparison is, in words that claim no more than the admitted records: a
    measured run with a missing or failed slot, or an unfinished shard, states how many planned
    records it holds and how many are missing or failed, never that every pair ran."""
    verdict = doc["verdict"]
    provenance = verdict["provenance"]
    if provenance not in ("measured", "mixed"):
        return COMPARISON[provenance]
    coverage = verdict["aggregate"]["coverage"]
    failed = sum(1 for shard in doc["sequences"] for point in shard["points"]
                 if point["status"] == "failed")
    unfinished = [str(shard["shard"]) for shard in doc["shards"]
                  if shard["terminal"] is None or shard["terminal"]["status"] != "complete"]
    if not coverage["missing_records"] and not failed and not unfinished:
        return COMPARISON[provenance]
    gaps = ("Of %d planned records, %d were admitted; missing: %d; failed samples among the "
            "admitted: %d%s."
            % (coverage["expected_records"], coverage["matched_records"],
               coverage["missing_records"], failed,
               "; shards that did not complete: %s" % ", ".join(unfinished) if unfinished
               else ""))
    if coverage["missing_records"] or failed:
        gaps += (" Not every planned pair was recorded and succeeded: the raw sequences mark "
                 "each missing and failed slot, and only a case whose every planned pair was "
                 "admitted and succeeded has an interval.")
    else:
        gaps += (" Every planned record was admitted and succeeded, but a shard's terminal "
                 "record says it did not complete (see Shards).")
    if provenance == "mixed":
        return "%s %s" % (COMPARISON["mixed"], gaps)
    return ("Measured evidence, incomplete: the admitted records were recorded within their "
            "shards on %s (see Shards), in the plan's seeded AB/BA order. %s"
            % (_hosts(doc), gaps))


def _point(slot, record, primary):
    point = {"sequence": slot["sequence"], "phase": slot["phase"],
             "instance_id": slot["instance_id"], "pair": slot["pair"],
             "position": slot["position"], "side": slot["side"], "status": "missing",
             "value": None, "failure": None, "detail": None}
    if record is None:
        return point
    if record["status"] == "failed":
        point.update(status="failed", failure=record["failure"]["kind"],
                     detail=record["failure"]["detail"])
    elif primary not in record["metrics"]:
        point.update(status="failed", failure="missing_required_metric")
    else:
        point.update(status="ok", value=record["metrics"][primary]["value"])
    return point


def sequences(evaluation):
    """Every planned slot of every shard as a raw point, in planned order."""
    plan = evaluation.plan
    cases = dict((case["instance_id"], case) for case in plan["cases"])
    out = []
    for evidence in evaluation.evidence:
        points = []
        for slot in schedule.expected_slots(plan, evidence.index):
            primary = (plan["health"]["metric"] if slot["instance_id"] not in cases
                       else cases[slot["instance_id"]]["primary_metric"]["key"])
            points.append(_point(slot, evidence.records.get(schedule.slot_key(slot)),
                                 primary))
        out.append({"shard": evidence.index, "points": points})
    return out


def required_check(plan, verdict):
    """Whether the run satisfies the required check and, when not, every reason why."""
    reasons = []
    if verdict["aggregate"]["result"] != "pass":
        reasons.append("result_not_pass")
    if plan["scope"] != "full":
        reasons.append("scope_not_full")
    if plan["authority"]["mode"] != "active":
        reasons.append("policy_not_active")
    if verdict["provenance"] != "measured":
        reasons.append("evidence_not_measured")
    return {"satisfied": not reasons, "reasons": reasons}


REASON_TEXT = {
    "result_not_pass": "the aggregate result is not pass",
    "scope_not_full": "only full scope can satisfy it; this run's scope is %s",
    "policy_not_active": "only an activated policy can; this plan's authority mode is %s",
    "evidence_not_measured": "only measured evidence can; this run's evidence is %s",
}


def _identity(plan, builds_doc):
    authority, profile = plan["authority"], plan["profile"]
    build = None
    if builds_doc is not None:
        sides = dict((side, {"af": builds_doc["sides"][side]["binaries"]["af"],
                             "probe": builds_doc["sides"][side]["binaries"]["probe"],
                             "adapter": builds_doc["sides"][side]["chosen_adapter"]})
                     for side in contracts.SIDES)
        build = dict(sides, provenance=builds_doc["provenance"],
                     supervisor_sha256=builds_doc["supervisor"]["binary_sha256"],
                     toolchain=builds_doc["toolchain"]["rustc"][:256] or "unknown",
                     isolation=builds_doc["isolation"]["kind"])
    return {
        "trigger": plan["trigger"]["kind"], "tier": plan["tier"], "scope": plan["scope"],
        "seed": plan["seed"], "parent_run_id": plan["parent_run_id"],
        "baseline_sha": plan["resolved"]["baseline_sha"],
        "candidate_sha": plan["resolved"]["candidate_sha"],
        "control_sha": plan["resolved"]["control_sha"],
        "authority_sha": plan["resolved"]["authority_sha"],
        "authority_mode": authority["mode"],
        # The report identifies the activation; its calibrated groups bind the verdict, whose
        # reasons name a shard observed outside them (ADR-0184).
        "activation": None if authority["activation"] is None else dict(
            (key, authority["activation"][key])
            for key in ("sha256", "admission", "readiness_sha256", "cohort_id")),
        "suite_sha256": authority["suite_sha256"], "policy_digest": authority["policy_digest"],
        "instrument_digest": authority["instrument_digest"],
        "fixture_digest": authority["fixture_digest"],
        "harness_changed": plan["harness"]["changed"],
        "environment_id": profile["environment_id"],
        "environment_digest": profile["environment_digest"], "recipe_id": profile["recipe_id"],
        "recipe_digest": profile["recipe_digest"],
        "build_isolation": profile["build"]["isolation"], "build_image": profile["build"]["image"],
        "health_rule": plan["health"]["rule"], "health_state": plan["health"]["state"],
        "build": build,
    }


def _sensitivity(plan, provenance):
    activation = plan["authority"]["activation"]
    preface = "%s %s" % (SENSITIVITY[provenance], CALIBRATION)
    if activation is None:
        return {"state": "not_calibrated",
                "detail": "%s No activation binds this plan (authority mode %s), so no "
                          "calibration evidence applies to it." % (preface,
                                                                   plan["authority"]["mode"])}
    return {"state": "calibrated_admission",
            "detail": "%s The plan's activation manifest %s records the %s admission of cohort "
                      "%s with readiness %s, calibrated on CPU/image group(s) %s; this report "
                      "does not recompute that evidence."
                      % (preface, activation["sha256"], activation["admission"],
                         activation["cohort_id"], activation["readiness_sha256"],
                         ", ".join(group["group_id"] for group in
                                   activation["calibrated_groups"]))}


def _case_extras(plan, verdict, children=()):
    by_id = dict((case["instance_id"], case) for case in plan["cases"])
    out = []
    for entry in verdict["cases"]:
        case = by_id[entry["instance_id"]]
        failures = dict((side, 0) for side in contracts.SIDES)
        for failure in entry["failures"]:
            failures[failure["side"]] += 1
        planned = 2 * (len(case["warmup_orders"]) + len(case["measured_orders"]))
        out.append({"instance_id": entry["instance_id"], "case_id": case["case_id"],
                    "failures": failures,
                    "denominator": {"planned_records": planned,
                                    "missing_records": entry["missing_records"],
                                    "measured_pairs": case["pairs"],
                                    "warmup_pairs": case["warmup_pairs"]},
                    "profile": _case_diagnostics(entry["instance_id"], "cpu_profile",
                                                 children),
                    "trace": _case_diagnostics(entry["instance_id"], "trace", children),
                    "telemetry": _case_telemetry(entry["instance_id"], children)})
    return out


# Diagnostic child runs (ADR-0177) -----------------------------------------------------------

class Child(object):
    """One verified diagnostic child run: its profiles document, bundle identity and hashes,
    and where its verified copy lies below the report."""

    def __init__(self, source, doc, plan, identity, hashes, profiles_sha256):
        self.source = source
        self.doc = doc
        self.plan = plan
        self.identity = identity
        self.hashes = hashes
        self.profiles_sha256 = profiles_sha256
        self.directory = "%s/%s" % (DIAGNOSTICS, doc["run_id"])
        # The kind its frozen plan froze, which its profiles were admitted only to repeat.
        self.kind = plan["diagnostic"]["recipe"]["kind"]

    def path(self, relative):
        return "%s/%s" % (self.directory, relative)

    def record(self):
        doc = self.doc
        return {"run_id": doc["run_id"], "plan_digest": doc["plan_digest"],
                "recipe_id": doc["recipe"]["recipe_id"], "kind": self.kind,
                "mode": doc["execution"]["mode"], "bundle_sha256": self.identity,
                "profiles_sha256": self.profiles_sha256,
                "cases": [item["instance_id"] for item in doc["selection"]["cases"]],
                "retained_scratch": doc["execution"]["retained_scratch"],
                "telemetry_level": doc["telemetry"]["level"],
                "page": self.path(PAGE), "directory": self.directory}


PAGE = "diagnostic.html"
CHILD_FILES = ("profiles.json", PAGE, "run.json")


def admit_child(source, plan):
    """The diagnostic child run at `source` when it is a verified, unaltered, consistent child
    of `plan` whose views are the trusted renderer's; anything unrelated, inconsistent, tampered
    or unsafe is refused before any of it is copied or shown (ADR-0178)."""
    label = "diagnostic %s" % os.path.basename(os.path.normpath(source))
    identity, hashes = bundle.verify(source, limits.DIAGNOSTIC_BYTES_MAX, label=label)
    for name in CHILD_FILES:
        require(name in hashes, "%s: holds no %s, so it is not a diagnostic child run"
                % (label, name))
    data = bundle.read_member(source, "profiles.json", hashes, limits.MAX_JSON_BYTES, label)
    doc = contracts.read_json(data, "profiles", label + "/profiles.json")
    child_plan = contracts.read_json(bundle.read_member(source, "run.json", hashes,
                                                        limits.MAX_JSON_BYTES, label),
                                     "run", label + "/run.json")
    require((doc["parent_run_id"], doc["parent_plan_digest"])
            == (plan["run_id"], plan["plan_digest"]),
            "%s: is a child of run %s (%s), not of this run %s (%s)"
            % (label, doc["parent_run_id"], doc["parent_plan_digest"], plan["run_id"],
               plan["plan_digest"]))
    resolved = plan["resolved"]
    require(doc["sources"] == {"baseline_sha": resolved["baseline_sha"],
                               "candidate_sha": resolved["candidate_sha"],
                               "authority_sha": resolved["authority_sha"]},
            "%s: measures other sources than this run" % label)
    require(child_plan["scope"] == "diagnostic"
            and (child_plan["run_id"], child_plan["plan_digest"])
            == (doc["run_id"], doc["plan_digest"])
            and child_plan["diagnostic"]["parent_run_id"] == plan["run_id"]
            and child_plan["diagnostic"]["parent_plan_digest"] == plan["plan_digest"],
            "%s: its frozen child plan is not the one its profiles name" % label)
    _consistent(label, plan, child_plan, doc)
    artifacts = []
    for entry in doc["captures"] + doc["traces"] + doc["counters"] + doc["telemetry"]["captures"]:
        artifacts.extend(entry["artifacts"])
    for entry in doc["renders"]:
        artifacts.extend((entry["original"], entry["sanitized"]))
    for artifact in artifacts:
        require(artifact["path"] in hashes and hashes[artifact["path"]] == artifact["sha256"],
                "%s: %s is not the verified member its profiles name" % (label,
                                                                       artifact["path"]))
    _admit_views(source, label, doc, hashes)
    return Child(source, doc, child_plan, identity, hashes, sha256_bytes(data))


# What a child plan takes from its parent unchanged (`planning._diagnostic`, ADR-0178).
PARENT_FIELDS = ("resolved", "selectors", "trigger", "tier", "authority", "build_inputs")
# The evidence each frozen recipe kind writes (`diagnostics.Profile.run`, ADR-0179).
EVIDENCE_LISTS = ("captures", "renders", "traces", "counters")
KIND_EVIDENCE = {"cpu_profile": ("captures", "renders"), "trace": ("traces",),
                 "counters": ("counters",)}


def _consistent(label, plan, child_plan, doc):
    """Refuse a child whose frozen plan and profiles disagree with each other or with `plan`,
    however consistent their hashes: the child plan carries the parent's sources, selectors,
    authority, environment, build and the workload of each selected case, and the profiles name
    the child plan's own sources, recipe, frozen recipe kind, capture bounds and selected cases,
    with evidence only of that kind. Its diagnostic recipe is its own, separate from the
    parent's, and its omissions and partial evidence are kept as they are."""
    for name in PARENT_FIELDS:
        require(child_plan[name] == plan[name], "%s: its frozen child plan's %s is not this "
                "run's, so it diagnosed other sources than this run measured" % (label, name))
    for name in ("environment_id", "environment_digest", "build"):
        require(child_plan["profile"][name] == plan["profile"][name], "%s: its frozen child "
                "plan's profile %s is not this run's" % (label, name))
    child_resolved = child_plan["resolved"]
    require(doc["sources"] == {"baseline_sha": child_resolved["baseline_sha"],
                               "candidate_sha": child_resolved["candidate_sha"],
                               "authority_sha": child_resolved["authority_sha"]},
            "%s: its profiles name other sources than its frozen child plan" % label)
    profile, recipe = child_plan["profile"], doc["recipe"]
    require((recipe["recipe_id"], recipe["recipe_digest"])
            == (profile["recipe_id"], profile["recipe_digest"])
            and all(recipe[name] == profile["recipe"][name]
                    for name in ("features", "debug_symbols", "frame_pointers")),
            "%s: its profiles claim recipe %s (%s), not its frozen child plan's %s (%s)"
            % (label, recipe["recipe_id"], recipe["recipe_digest"], profile["recipe_id"],
               profile["recipe_digest"]))
    # The kind a child's evidence is classified by is the one its digest-checked plan froze
    # with its recipe, never a spelling of the recipe ID (ADR-0179).
    frozen = child_plan["diagnostic"]["recipe"]
    require(frozen is not None, "%s: its frozen child plan froze no diagnostic recipe, so "
            "nothing states what kind of evidence it holds" % label)
    require((recipe["kind"], recipe["base_recipe"]) == (frozen["kind"], frozen["base_recipe"]),
            "%s: its profiles claim a %s recipe built like %s, but its frozen child plan froze "
            "recipe %s as a %s recipe built like %s" % (
                label, recipe["kind"], recipe["base_recipe"], profile["recipe_id"],
                frozen["kind"], frozen["base_recipe"]))
    held = [name for name in EVIDENCE_LISTS
            if doc[name] and name not in KIND_EVIDENCE[frozen["kind"]]]
    require(not held, "%s: a %s child holds %s, which is not evidence of its kind"
            % (label, frozen["kind"], ", ".join(held)))
    require((doc["capture"]["frequency_hz"], doc["capture"]["duration_ms"])
            == (child_plan["diagnostic"]["frequency_hz"],
                child_plan["diagnostic"]["duration_ms"]),
            "%s: its profiles claim other capture bounds than its frozen child plan" % label)
    instances = dict((case["instance_id"], case) for case in plan["cases"])
    planned = dict((case["instance_id"], case) for case in child_plan["cases"])
    selected = [item["instance_id"] for item in doc["selection"]["cases"]]
    for item in doc["selection"]["cases"]:
        require(item["instance_id"] in instances, "%s: selected %s, which is not an instance "
                "of this run" % (label, item["instance_id"]))
        require(item["instance_id"] in planned
                and planned[item["instance_id"]]["case_id"] == item["case_id"]
                == instances[item["instance_id"]]["case_id"], "%s: selected %s, which its "
                "frozen child plan does not freeze as that case" % (label, item["instance_id"]))
        _same_workload(label, instances[item["instance_id"]], planned[item["instance_id"]])
    require(len(selected) == len(set(selected)) and set(selected) == set(planned),
            "%s: its profiles select %s, not the cases its frozen child plan froze (%s)"
            % (label, ", ".join(selected) or "none", ", ".join(sorted(planned)) or "none"))
    for entry in (doc["captures"] + doc["renders"] + doc["traces"] + doc["counters"]
                  + doc["telemetry"]["captures"]):
        require(entry["instance_id"] in planned, "%s: holds evidence of %s, which its frozen "
                "child plan did not select" % (label, entry["instance_id"]))
    for entry in doc["telemetry"]["captures"]:
        require(entry["source_sha"] == child_resolved[entry["side"] + "_sha"],
                "%s: its %s telemetry capture of %s names another source than its frozen "
                "child plan's" % (label, entry["side"], entry["instance_id"]))


# The fields of a frozen case that a diagnostic child plans for itself (ADR-0179): its own pair
# count and the rank, alpha and tail probability that follow from it and from how many critical
# cases it selected, its own shard and its own seeded AB/BA orders. Every other field, the
# workload's parameters, seed, kind, timing, cache, oracle, execution and AF environment
# among them, is the parent case's, so a field added to a case later must match until it is
# explicitly allowed here.
DIAGNOSTIC_CASE_FIELDS = ("pairs", "rank", "alpha", "tail_probability", "shard",
                          "warmup_orders", "measured_orders")


def _same_workload(label, parent_case, child_case):
    """Refuse a selected child case whose workload is not its parent case's, whatever IDs the
    two share: the run contract never derives an instance ID from its parameters."""
    differing = sorted(name for name in set(parent_case) | set(child_case)
                       if name not in DIAGNOSTIC_CASE_FIELDS
                       and parent_case.get(name) != child_case.get(name))
    require(not differing, "%s: its frozen child plan's %s is not this run's workload: %s "
            "differs" % (label, parent_case["instance_id"], ", ".join(differing)))


def _admit_views(source, label, doc, hashes):
    """Admit what the report presents as a safe view of the child against the trusted renderer,
    never on its hash alone (ADR-0178): each sanitized flamegraph is exactly what the
    allowlist sanitizer makes of its original, each Perfetto export is the export of its
    validated trace, and the page is exactly the one `diagnostics.page` draws from the admitted
    profiles and traces, with the renderer's own CSP, escaping and local links. Anything else,
    a script, an event attribute or an external reference among it, is refused."""
    def member(path):
        return bundle.read_member(source, path, hashes, limits.DIAGNOSTIC_BYTES_MAX, label)

    for entry in doc["renders"]:
        where = "%s/%s" % (label, entry["sanitized"]["path"])
        try:
            again = svgsafe.sanitize(member(entry["original"]["path"]))
        except ContractError as error:
            raise ContractError("%s: its original is no SVG the sanitizer admits: %s"
                                % (where, error))
        require(again.data == member(entry["sanitized"]["path"])
                and (again.removed_elements, again.removed_attributes)
                == (entry["removed"]["elements"], entry["removed"]["attributes"]),
                "%s: is not what the trusted sanitizer makes of its original, so it is not "
                "shown" % where)
    traces = []
    for entry in doc["traces"]:
        if entry["omission"] is not None:
            traces.append(None)
            continue
        named = dict((artifact["path"].rsplit("/", 1)[-1], artifact["path"])
                     for artifact in entry["artifacts"])
        where = "%s: the %s trace of %s" % (label, entry["side"], entry["instance_id"])
        require("trace.json" in named and "perfetto.json" in named,
                "%s holds no trace.json and perfetto.json" % where)
        trace = phasetrace.load(member(named["trace.json"]), "%s/%s" % (label,
                                                                       named["trace.json"]))
        require(member(named["perfetto.json"]) == phasetrace.perfetto(
            trace, "%s %s" % (entry["instance_id"], entry["side"])),
            "%s: its perfetto.json is not the export of its trace" % where)
        region_spans, coverage = diagnostics.trace_coverage(trace, entry["region_phase"])
        require((entry["spans"], entry["dropped"], entry["open"], entry["async_spans"],
                 entry["region_spans"], entry["coverage"])
                == (len(trace["spans"]), trace["dropped"], trace["open"],
                    phasetrace.async_count(trace), region_spans, coverage),
                "%s: its profiles claim other spans or coverage than its trace" % where)
        traces.append(trace)
    for entry in doc["telemetry"]["captures"]:
        if entry["omission"] is not None:
            continue
        where = "%s: the %s telemetry capture of %s" % (label, entry["side"],
                                                        entry["instance_id"])
        named = dict((artifact["path"].rsplit("/", 1)[-1], artifact["path"])
                     for artifact in entry["artifacts"])
        documents = dict((kind, member(named[kind + ".json"]))
                         for kind in telemetrydiag.DOCUMENT_KINDS)
        try:
            derived = telemetrydiag.capture(documents, entry["run"]["task_id"])
            telemetrydiag.check_capture(entry, derived)
        except ContractError as error:
            raise ContractError("%s: %s" % (where, error))
        require(member(named[telemetrydiag.PERFETTO]) == telemetrydiag.perfetto(
            documents["show"], "%s %s" % (entry["instance_id"], entry["side"])),
            "%s: its perfetto.json is not the export of its show.json" % where)
    require(member(PAGE) == diagnostics.page(doc, traces),
            "%s: its %s is not the page the trusted renderer draws from its admitted evidence, "
            "so it is not linked" % (label, PAGE))


def admit_children(sources, plan):
    children = [admit_child(source, plan) for source in sources]
    names = [child.doc["run_id"] for child in children]
    require(len(names) == len(set(names)), "--diagnostic: each child run once")
    require(len(children) <= 16, "--diagnostic: at most 16 child runs")
    return children


def copy_child(child, output):
    """Copy every verified member of `child`, its manifest included, below the report: the
    offline archive of its detailed view, each file reread and checked against its hash."""
    with open(os.path.join(child.source, bundle.MANIFEST), "rb") as stream:
        manifest = stream.read(limits.MAX_JSON_BYTES + 1)
    require(sha256_bytes(manifest) == child.identity, "%s changed after verification"
            % child.directory)
    files = [(bundle.MANIFEST, manifest)] + [
        (path, bundle.read_member(child.source, path, child.hashes, limits.DIAGNOSTIC_BYTES_MAX,
                                  child.directory)) for path in sorted(child.hashes)]
    for path, data in files:
        _write_new(os.path.join(output, *child.path(path).split("/")), data)


def _overhead(entry):
    overhead = entry["overhead"]
    if overhead is None:
        return "overhead not measured"
    return "overhead %s (%s, target %s)" % (overhead["median_overhead"], overhead["label"],
                                            overhead["target"])


def _case_diagnostics(instance_id, kind, children):
    """A case's profile (`cpu_profile`) or trace state from the linked children of that kind
    which selected it: their verified items and every omission they recorded."""
    covering = [child for child in children if child.kind == kind
                and instance_id in [item["instance_id"]
                                    for item in child.doc["selection"]["cases"]]]
    if not covering:
        return {"state": "unavailable", "reason": PROFILE_UNAVAILABLE, "items": [],
                "omissions": []}
    items, omissions, partial = [], [], False
    for child in covering:
        doc, run_id = child.doc, child.doc["run_id"]
        for item in doc["omissions"]:
            if item["instance_id"] in (None, instance_id):
                omissions.append({"child_run_id": run_id, "side": item["side"],
                                  "reason": item["reason"], "detail": item["detail"]})
        if kind == "cpu_profile":
            for entry in doc["captures"]:
                if entry["instance_id"] == instance_id and entry["omission"] is not None:
                    omissions.append({"child_run_id": run_id, "side": entry["side"],
                                      "reason": entry["omission"]["reason"],
                                      "detail": entry["omission"]["detail"]})
            for entry in doc["renders"]:
                if entry["instance_id"] == instance_id:
                    side = entry["kind"] if entry["kind"] in contracts.SIDES else None
                    items.append({"child_run_id": run_id, "side": side, "kind": entry["kind"],
                                  "path": child.path(entry["sanitized"]["path"]),
                                  "sha256": entry["sanitized"]["sha256"],
                                  "note": ("color is the change in each stack's share of CPU "
                                           "samples" if side is None else None)})
        else:
            for entry in doc["traces"]:
                if entry["instance_id"] != instance_id:
                    continue
                if entry["omission"] is not None:
                    omissions.append({"child_run_id": run_id, "side": entry["side"],
                                      "reason": entry["omission"]["reason"],
                                      "detail": entry["omission"]["detail"]})
                    continue
                partial = partial or entry["coverage"] != "complete"
                note = "%s, region %s (%d spans): %s; %d dropped, %d open; %s" % (
                    entry["subject"], entry["region_phase"], entry["region_spans"],
                    entry["coverage"], entry["dropped"], entry["open"], _overhead(entry))
                for artifact in entry["artifacts"]:
                    name = artifact["path"].rsplit("/", 1)[-1]
                    if name in ("trace.json", "perfetto.json"):
                        items.append({"child_run_id": run_id, "side": entry["side"],
                                      "kind": "trace" if name == "trace.json" else "perfetto",
                                      "path": child.path(artifact["path"]),
                                      "sha256": artifact["sha256"], "note": note})
                items.append({"child_run_id": run_id, "side": entry["side"], "kind": "timeline",
                              "path": child.path(PAGE),
                              "sha256": child.hashes[PAGE], "note": note})
    if not items:
        state = "omitted"
    elif omissions or partial:
        state = "partial"
    else:
        state = "available"
    reason = {"omitted": "the linked child runs selected this case and recorded why nothing "
                         "was captured",
              "partial": "the linked child runs captured part of this case; see the omissions "
                         "and coverage",
              "available": "verified evidence of the linked child runs"}[state]
    return {"state": state, "reason": reason, "items": items[:64], "omissions": omissions[:64]}


def _case_telemetry(instance_id, children):
    """A case's telemetry state from the linked children that selected a telemetry level and
    this case: each capture's verified documents, Perfetto export and timeline, its partial
    reasons, and every telemetry omission (ADR-0199). Never a verdict input."""
    covering = [child for child in children if child.doc["telemetry"]["level"] is not None
                and instance_id in [item["instance_id"]
                                    for item in child.doc["selection"]["cases"]]]
    if not covering:
        return {"state": "unavailable", "reason": TELEMETRY_UNAVAILABLE, "items": [],
                "omissions": []}
    items, omissions, partial = [], [], False
    for child in covering:
        doc, run_id = child.doc, child.doc["run_id"]
        for item in doc["omissions"]:
            if item["kind"] == "telemetry" and item["instance_id"] in (None, instance_id):
                omissions.append({"child_run_id": run_id, "side": item["side"],
                                  "reason": item["reason"], "detail": item["detail"]})
        for entry in doc["telemetry"]["captures"]:
            if entry["instance_id"] != instance_id:
                continue
            if entry["omission"] is not None:
                omissions.append({"child_run_id": run_id, "side": entry["side"],
                                  "reason": entry["omission"]["reason"],
                                  "detail": entry["omission"]["detail"]})
                continue
            evidence = entry["evidence"]
            partial = partial or evidence["state"] != "complete"
            note = "level %s%s, %s%s; %d spans, %d executions; never a verdict sample" % (
                entry["level"], " (perturbing)" if entry["level"] == "content" else "",
                evidence["state"], (" (%s)" % ", ".join(evidence["reasons"])
                                    if evidence["reasons"] else ""),
                entry["spans"], len(entry["executions"]))
            for artifact in entry["artifacts"]:
                name = artifact["path"].rsplit("/", 1)[-1]
                if name in ("show.json", telemetrydiag.PERFETTO):
                    items.append({"child_run_id": run_id, "side": entry["side"],
                                  "kind": "telemetry" if name == "show.json"
                                  else "telemetry_perfetto",
                                  "path": child.path(artifact["path"]),
                                  "sha256": artifact["sha256"], "note": note})
            items.append({"child_run_id": run_id, "side": entry["side"], "kind": "timeline",
                          "path": child.path(PAGE), "sha256": child.hashes[PAGE],
                          "note": note})
    if not items:
        state = "omitted"
    elif omissions or partial:
        state = "partial"
    else:
        state = "available"
    reason = {"omitted": "the linked child runs selected this case and recorded why it holds "
                         "no telemetry capture",
              "partial": "the linked child runs captured part of this case's telemetry; see "
                         "the omissions and evidence states",
              "available": "verified telemetry documents of the linked child runs"}[state]
    return {"state": state, "reason": reason, "items": items[:64], "omissions": omissions[:64]}


def _shards(evaluation):
    out = []
    for evidence in evaluation.evidence:
        doc = evidence.doc
        terminal = None
        if doc is not None:
            terminal = {"status": doc["status"], "failure": doc["failure"],
                        "host_id": doc["host_id"], "host": doc["host"],
                        "mode": doc["execution"]["mode"],
                        "containment": doc["execution"]["containment"],
                        "sample_image": doc["execution"]["sample_image"],
                        "seeds": doc["seeds"], "observations": doc["observations"],
                        "started_unix_ms": doc["started_unix_ms"],
                        "ended_unix_ms": doc["ended_unix_ms"]}
        out.append({"shard": evidence.index, "terminal": terminal})
    return out


def build(evaluation, downloads, children=()):
    """The `af.perf-report/3` document of an evaluated run and its linked child runs."""
    plan, verdict = evaluation.plan, evaluation.verdict
    doc = {"schema": contracts.REPORT_SCHEMA, "run_id": plan["run_id"],
           "plan_digest": plan["plan_digest"], "comparison": "same_host_paired",
           "required_check": required_check(plan, verdict),
           "identity": _identity(plan, evaluation.builds),
           "sensitivity": _sensitivity(plan, verdict["provenance"]), "cases": _case_extras(plan, verdict, children),
           "diagnostics": [child.record() for child in children],
           "sequences": sequences(evaluation), "shards": _shards(evaluation),
           "downloads": downloads, "reproduction": reproduce.commands(plan), "verdict": verdict}
    return contracts.validate_report(doc)


# Evidence copies --------------------------------------------------------------------------

def _kind(path):
    name = path.rsplit("/", 1)[-1]
    if "/logs/" in path:
        return "log"
    return {"run.json": "plan", "builds.json": "build_manifest", "manifest.json": "manifest",
            "shard.json": "shard", "samples.jsonl": "samples"}[name]


def copy_evidence(run_dir, evaluation, target):
    """Copy every verified member of the run's bundles below `target`, each reread within its
    bundle's bound and checked against its verified hash, and each bundle's manifest checked
    against the identity the verdict names; the downloads they provide."""
    plan = evaluation.plan
    bundles = []
    if evaluation.hashes is not None:
        bundles.append((run_dir, "", evaluation.hashes, limits.MAX_JSON_BYTES,
                        evaluation.verdict["inputs"]["run_bundle"]))
    for evidence in evaluation.evidence:
        if evidence.hashes is not None:
            bundles.append((os.path.join(run_dir, compare.SHARD_DIR, str(evidence.index)),
                            "%s/%d/" % (compare.SHARD_DIR, evidence.index), evidence.hashes,
                            plan["limits"]["shard_bundle_bytes"], evidence.bundle))
    downloads = []
    for root, prefix, hashes, limit, identity in bundles:
        with open(os.path.join(root, bundle.MANIFEST), "rb") as stream:
            manifest = stream.read(limits.MAX_JSON_BYTES + 1)
        require(sha256_bytes(manifest) == identity,
                "%s%s changed after verification" % (prefix, bundle.MANIFEST))
        files = [(bundle.MANIFEST, manifest)]
        files += [(path, bundle.read_member(root, path, hashes, limit, prefix or "run"))
                  for path in sorted(hashes)]
        for path, data in files:
            relative = "%s/%s%s" % (EVIDENCE, prefix, path)
            _write_new(os.path.join(target, *relative.split("/")), data)
            downloads.append({"path": relative, "sha256": sha256_bytes(data),
                              "size": len(data), "kind": _kind(prefix + path)})
    return sorted(downloads, key=lambda item: item["path"])


def _write_new(path, data):
    """Write a new file below a directory this report created; nothing is overwritten."""
    os.makedirs(os.path.dirname(path), 0o755, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)


def prepare_output(output, run_dir=None):
    """A new, empty output directory that is no part of the run directory."""
    output = os.path.abspath(output)
    if os.path.lexists(output):
        require(os.path.isdir(output) and not os.path.islink(output)
                and not os.listdir(output),
                "--output %s exists and is not an empty directory; a report never overwrites"
                % output)
    if run_dir is not None:
        run = os.path.realpath(run_dir)
        parent = os.path.realpath(os.path.dirname(output))
        require(not (parent + os.sep).startswith(run + os.sep) and parent != run,
                "--output may not lie inside the run directory, whose bundle it would change")
    os.makedirs(output, 0o755, exist_ok=True)
    return output


# Rendering -----------------------------------------------------------------------------------

def _approx(value):
    if value is None:
        return None
    return "%s (≈ %.6g)" % (value, float(Fraction(value))) if "/" in value else value


def _interval(entry):
    interval = entry["interval"]
    if interval is None:
        return "not computed"
    return "[%s, %s]" % (_approx(interval["lower"]), _approx(interval["upper"]))


def _summary(entry, side):
    summary = entry[side]
    if summary is None:
        return "—"
    return "median %s; p95 %s; max %s" % (_approx(summary["median"]), _approx(summary["p95"]),
                                          _approx(summary["max"]))


def _change(entry):
    change = entry["change"]
    if change is None:
        return "—"
    percent = change["percent"]
    return "%s ms (%s)" % (_approx(change["absolute"]),
                           "no percentage: zero baseline median" if percent is None
                           else "%s%%" % _approx(percent))


def _short(value):
    """A value for the compact tables: a decimal as it is, a rational as its approximation;
    the exact value stays in the statistics."""
    if value is None:
        return "—"
    return "≈ %.6g" % float(Fraction(value)) if "/" in value else value


def _median(entry, side):
    return "—" if entry[side] is None else _short(entry[side]["median"])


def _absolute(entry):
    change = entry["change"]
    return "—" if change is None else "%s %s" % (_short(change["absolute"]),
                                                 entry["metric"]["unit"])


def _relative(entry):
    change = entry["change"]
    if change is None:
        return "—"
    if change["percent"] is None:
        return "none: zero baseline median"
    return "%s%%" % _short(change["percent"])


def _budgets(entry):
    if not entry["budgets"]:
        return "none declared"
    return "; ".join("%s < %s ms: %s%s" % (
        budget["budget_id"], budget["limit"], budget["result"].replace("_", " "),
        "" if budget["observed_max"] is None else " (observed max %s)"
        % _approx(budget["observed_max"])) for budget in entry["budgets"])


def headline(doc):
    """The aggregate result and whether it satisfies the required check, in words."""
    verdict = doc["verdict"]
    aggregate = verdict["aggregate"]
    identity = doc["identity"]
    if doc["required_check"]["satisfied"]:
        check = "satisfies the required check"
    else:
        values = {"scope_not_full": identity["scope"],
                  "policy_not_active": identity["authority_mode"],
                  "evidence_not_measured": verdict["provenance"]}
        check = "does not satisfy the required check: " + "; ".join(
            REASON_TEXT[reason] % values[reason] if "%s" in REASON_TEXT[reason]
            else REASON_TEXT[reason] for reason in doc["required_check"]["reasons"])
    return "%s (exit %d; %s scope, %s evidence); %s" % (
        aggregate["result"], aggregate["exit_code"], identity["scope"], verdict["provenance"],
        check)


def _identity_rows(doc):
    identity = doc["identity"]
    verdict = doc["verdict"]
    rows = [("Run", doc["run_id"]), ("Plan digest", doc["plan_digest"]),
            ("Trigger / tier / scope", "%s / %s / %s" % (identity["trigger"], identity["tier"],
                                                         identity["scope"])),
            ("Parent run", identity["parent_run_id"] or "none"),
            ("Baseline source", identity["baseline_sha"]),
            ("Candidate source", identity["candidate_sha"]),
            ("Control / authority", "%s / %s" % (identity["control_sha"],
                                                 identity["authority_sha"])),
            ("Authority mode", identity["authority_mode"]),
            ("Harness changed by the candidate", "yes" if identity["harness_changed"] else "no"),
            ("Algorithm", verdict["algorithm"]),
            ("Family alpha / K / case alpha", "%s / %d / %s" % (
                verdict["family_alpha"], verdict["family_size"], verdict["case_alpha"])),
            ("Suite / policy", "%s / %s" % (identity["suite_sha256"],
                                            identity["policy_digest"])),
            ("Instrument / fixtures", "%s / %s" % (identity["instrument_digest"],
                                                   identity["fixture_digest"])),
            ("Environment", "%s %s" % (identity["environment_id"],
                                       identity["environment_digest"])),
            ("Recipe", "%s %s" % (identity["recipe_id"], identity["recipe_digest"])),
            ("Build", "%s%s" % (identity["build_isolation"],
                                 " in " + identity["build_image"] if identity["build_image"]
                                 else "")),
            ("Health rule", "%s (%s)" % (identity["health_rule"], identity["health_state"])),
            ("Run bundle / builds.json", "%s / %s" % (verdict["inputs"]["run_bundle"],
                                                      verdict["inputs"]["builds_sha256"]))]
    build = identity["build"]
    if build is None:
        rows.append(("Build manifest", "unusable; see the coverage reasons"))
    else:
        rows.append(("Build provenance", "%s, %s, %s; supervisor %s" % (
            build["provenance"], build["isolation"], build["toolchain"],
            build["supervisor_sha256"])))
        for side in contracts.SIDES:
            rows.append(("%s binaries" % side.capitalize(), "af %s; probe %s; adapter %s" % (
                build[side]["af"], build[side]["probe"], build[side]["adapter"])))
    return rows


def _point_rows(points):
    return [(point["sequence"], point["phase"], point["pair"], point["position"],
             point["side"], point["status"], point["value"] or "—",
             point["failure"] or "", point["detail"] or "") for point in points]


def _criticality(entry):
    return entry["criticality"] + (" (exploratory)" if entry["exploratory"] else "")


def _value_rows(cases, extras):
    """Each case's outcome and its baseline, candidate, absolute and relative change, legible
    at a glance."""
    return [(entry["instance_id"], _criticality(entry), render.outcome(entry["outcome"]),
             entry["status"], _median(entry, "baseline"), _median(entry, "candidate"),
             _absolute(entry), _relative(entry),
             "%d / %d" % (extras[entry["instance_id"]]["failures"]["baseline"],
                          extras[entry["instance_id"]]["failures"]["candidate"]),
             "%d / %d" % (entry["missing_records"],
                          extras[entry["instance_id"]]["denominator"]["planned_records"]))
            for entry in cases]


VALUE_HEADERS = ("Case", "Criticality", "Outcome", "Status", "Baseline median",
                 "Candidate median", "Change", "Relative change", "Failures b / c",
                 "Missing / planned")


def _eligibility(doc):
    """Every reason the run does not satisfy the required check, one item each."""
    identity, verdict = doc["identity"], doc["verdict"]
    values = {"scope_not_full": identity["scope"], "policy_not_active": identity["authority_mode"],
              "evidence_not_measured": verdict["provenance"]}
    return [REASON_TEXT[reason] % values[reason] if "%s" in REASON_TEXT[reason]
            else REASON_TEXT[reason] for reason in doc["required_check"]["reasons"]]


def _details(summary, *body):
    """An expandable part of a section: closed until the reader opens it."""
    return "<details><summary>%s</summary>%s</details>" % (render.escape(summary), "".join(body))


def affected_cases(verdict):
    """The cases a reader must look at: those whose outcome failed or is incomplete, in the
    verdict's order. A passing case, an improvement or non-regression included, is not one."""
    return [entry for entry in verdict["cases"] if entry["outcome"] in ("fail", "incomplete")]


def passed_text(verdict):
    """The verdict's words when no case failed or is incomplete: every case passed, counted by
    its status, so an improvement is never called within tolerance."""
    counts = {}
    for entry in verdict["cases"]:
        counts[entry["status"]] = counts.get(entry["status"], 0) + 1
    passed = ("every case passed (%s); see Cases for each case's values"
              % ", ".join("%d %s" % (counts[status], status.replace("_", " "))
                          for status in sorted(counts)) if counts else "the run has no case")
    if verdict["aggregate"]["result"] != "pass":
        return ("No case failed or is incomplete, and %s, yet the run's result is %s: see "
                "Coverage and reasons." % (passed, verdict["aggregate"]["result"]))
    return "No case failed or is incomplete: %s." % passed


def _verdict(doc, extras):
    """What a reader needs first: the result, the evidence kind, whether the run can satisfy
    the required check, and the cases that failed or are incomplete with their values. A
    passing improvement or non-regression is no problem: it stays in the Cases table only."""
    verdict = doc["verdict"]
    aggregate = verdict["aggregate"]
    identity = doc["identity"]
    affected = affected_cases(verdict)
    result = render.outcome(aggregate["result"], "%s (exit %d)" % (aggregate["result"],
                                                                   aggregate["exit_code"]))
    lines = ['<p class="verdict">%s: %s of %d cases failed or incomplete; %s scope, '
             '%s evidence, authority %s.</p>'
             % (result.text, len(affected), len(verdict["cases"]),
                render.escape(identity["scope"]), render.escape(verdict["provenance"]),
                render.escape(identity["authority_mode"]))]
    text = comparison(doc)
    lines.append(render.paragraph(text, None if text == COMPARISON["measured"] else "note"))
    if doc["required_check"]["satisfied"]:
        lines.append(render.paragraph("Required check: this run satisfies the required check."))
    else:
        lines.append("<p>Required check: this run does not satisfy the required check:</p><ul>"
                     + "".join("<li>%s</li>" % render.escape(text)
                               for text in _eligibility(doc)) + "</ul>")
    if affected:
        lines.append(render.table(VALUE_HEADERS, _value_rows(affected, extras)))
    else:
        lines.append(render.paragraph(passed_text(verdict)))
    return render.section("Verdict", *lines)


def html_bytes(doc, downloads_present=True):
    """The self-contained report page of `doc`: the verdict and the affected cases first, then
    every case's values, and the exact statistics, provenance and raw points in sections and
    expandable details below."""
    verdict = doc["verdict"]
    aggregate = verdict["aggregate"]
    extras = dict((item["instance_id"], item) for item in doc["cases"])
    sections = [
        _verdict(doc, extras),
        render.section("Cases", render.table(VALUE_HEADERS,
                                             _value_rows(verdict["cases"], extras),
                                             searchable=True)),
        render.section("Statistics", _details(
            "Exact intervals, tolerances, distributions and budgets of every case",
            render.paragraph(doc["sensitivity"]["detail"], "note"),
            render.paragraph("Headline: " + headline(doc)),
            render.table(
                ("Case", "Classification", "Interval [L, U] (exact)", "Tolerance r / a",
                 "N / rank / alpha", "Baseline", "Candidate", "Median change", "Budgets"),
                [(entry["instance_id"], entry["statistical_classification"] or "none",
                  _interval(entry),
                  "%s / %s %s" % (entry["tolerance"]["relative"],
                                  entry["tolerance"]["absolute"], entry["metric"]["unit"]),
                  "%d / %d / %s" % (entry["pairs"], entry["rank"], entry["alpha"]),
                  _summary(entry, "baseline"), _summary(entry, "candidate"), _change(entry),
                  _budgets(entry))
                 for entry in verdict["cases"]], searchable=True))),
        render.section("Coverage and reasons", render.paragraph(
            "%d of %d planned records matched; %d missing." % (
                aggregate["coverage"]["matched_records"],
                aggregate["coverage"]["expected_records"],
                aggregate["coverage"]["missing_records"])),
            render.table(("Code", "Shard", "Case", "Detail"),
                         [(reason["code"], "" if reason["shard"] is None else reason["shard"],
                           reason["instance_id"] or "", reason["detail"])
                          for reason in aggregate["reasons"]], searchable=True)
            if aggregate["reasons"] else render.paragraph("No reason: the evidence is "
                                                          "complete.")),
        render.section("Health", render.table(
            ("Shard", "Status", "Flags", "Start median", "End median", "Shift",
             "Start MAD ratio", "End MAD ratio"),
            [(entry["shard"], entry["status"], ", ".join(entry["flags"]) or "none",
              _approx(entry["start_median"]) or "—", _approx(entry["end_median"]) or "—",
              _approx(entry["shift"]) or "—", _approx(entry["start_mad_ratio"]) or "—",
              _approx(entry["end_mad_ratio"]) or "—") for entry in verdict["health"]])),
        render.section("Provenance", _details(
            "Run, plan, sources, authority, environment, recipe and build identities",
            render.table(("Field", "Value"), _identity_rows(doc), searchable=True))),
    ]
    shard_rows = []
    for shard, entry in zip(doc["shards"], verdict["shards"]):
        terminal = shard["terminal"]
        if terminal is None:
            shard_rows.append((shard["shard"], entry["status"] or "no evidence", "", "", "",
                               "%d / %d" % (entry["matched_records"], entry["expected_records"]),
                               "none"))
            continue
        failure = terminal["failure"]
        seeds = "; ".join("%s %s %s %s" % (seed["side"], seed["seed"], seed["status"],
                                           seed["digest"] or seed["failure"]["kind"])
                          for seed in terminal["seeds"]) or "none"
        shard_rows.append((shard["shard"], terminal["status"],
                           "%s: %s" % (failure["reason"], failure["detail"]) if failure else "",
                           terminal["host_id"], "%s / %s" % (terminal["mode"],
                                                             terminal["containment"]),
                           "%d / %d" % (entry["matched_records"], entry["expected_records"]),
                           seeds))
    sections.append(render.section("Shards", render.table(
        ("Shard", "Status", "Failure", "Host", "Mode / containment", "Matched / planned",
         "Seeds"), shard_rows, searchable=True)))
    sequence_parts = []
    for shard in doc["sequences"]:
        by_case = {}
        for point in shard["points"]:
            by_case.setdefault(point["instance_id"], []).append(point)
        for instance_id, points in by_case.items():
            label = "shard %d: %s (%d points)" % (shard["shard"], instance_id, len(points))
            sequence_parts.append("<h3>%s</h3>" % render.escape(label))
            sequence_parts.append(render.plot(points, label))
            sequence_parts.append(render.table(
                ("Sequence", "Phase", "Pair", "Position", "Side", "Status", "Value (ms)",
                 "Failure", "Detail"), _point_rows(points), searchable=True))
    sections.append(render.section(
        "Raw sequences",
        render.paragraph("Every planned slot in planned order, warmups, sentinels and "
                         "failures included; a slot without an admitted record is missing. "
                         "Baseline points are blue, candidate red, warmups faded."),
        _details("Every shard's points, plotted and listed", "".join(sequence_parts))))
    sections.append(render.section(
        "Profiles and traces",
        render.paragraph("Diagnostics are separate child runs linked to this one; they never "
                         "change its verdict. Color in a difference flamegraph is the change in "
                         "each stack's share of CPU samples, not an absolute time saved."),
        render.table(("Case", "Profile", "Trace", "Telemetry", "Omissions"),
                     [(item["instance_id"], _diagnostic_cell(item["profile"]),
                       _diagnostic_cell(item["trace"]), _diagnostic_cell(item["telemetry"]),
                       "; ".join("%s %s: %s: %s" % (entry["child_run_id"],
                                                    entry["side"] or "both sides",
                                                    entry["reason"], entry["detail"])
                                 for entry in item["profile"]["omissions"]
                                 + item["trace"]["omissions"]
                                 + item["telemetry"]["omissions"]) or "none")
                      for item in doc["cases"]], searchable=True),
        render.table(("Child run", "Recipe", "Mode", "Telemetry", "Cases", "Kept scratch",
                      "Detailed view", "Bundle"),
                     [(child["run_id"], "%s (%s)" % (child["recipe_id"], child["kind"]),
                       child["mode"], child["telemetry_level"] or "none",
                       ", ".join(child["cases"]) or "none",
                       child["retained_scratch"],
                       render.Markup(render.link(child["page"], "diagnostic page")),
                       render.code(child["bundle_sha256"]))
                      for child in doc["diagnostics"]])
        if doc["diagnostics"] else render.paragraph("No diagnostic child run is linked to "
                                                    "this report.", "unavailable")))
    if downloads_present and doc["downloads"]:
        download_rows = [(render.Markup(render.link(item["path"], item["path"])), item["kind"],
                          item["size"], render.code(item["sha256"]))
                         for item in doc["downloads"]]
        downloads = render.table(("File", "Kind", "Bytes", "SHA256"), download_rows,
                                 searchable=True)
    else:
        downloads = render.paragraph("No evidence files were copied beside this page.",
                                     "unavailable")
    sections.append(render.section(
        "Downloads and reproduction", downloads,
        render.paragraph("Reproduce on a compatible host (RUN_DIR, NEW_REPORT_DIR and "
                         "NEW_RUN_ID are yours to choose; a rerun is a new run):"),
        render.table(("Purpose", "Command", "Limitations"),
                     [(item["purpose"], render.code(" ".join(item["argv"])),
                       "; ".join(item["limitations"]) or "none")
                      for item in doc["reproduction"]])))
    return render.page("AF performance run %s" % doc["run_id"], sections)


def _diagnostic_cell(state):
    """A case's profile or trace cell: its state, then a local link to each verified item."""
    if state["state"] == "unavailable":
        return render.Markup('<span class="unavailable">%s</span>'
                             % render.escape(state["reason"]))
    links = ["%s %s" % (render.escape(item["side"] or ""), render.link(
        item["path"], item["kind"].replace("_", " "))) + (
        " (%s)" % render.escape(item["note"]) if item["note"] and item["kind"] == "timeline"
        else "") for item in state["items"]]
    return render.Markup('<span class="%s">%s</span>: %s' % (
        "pass" if state["state"] == "available" else "unavailable",
        render.escape(state["state"]), "; ".join(links) or render.escape(state["reason"])))


def summary_markdown(doc):
    """The compact GitHub job-summary table: one row per case, every cell escaped."""
    verdict = doc["verdict"]
    lines = ["### AF performance: %s" % render.markdown_cell(headline(doc)), "",
             "| Case | Status | Outcome | Interval [L, U] | Baseline median | "
             "Candidate median | Change | Failures b/c | Missing |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    extras = dict((item["instance_id"], item) for item in doc["cases"])
    for entry in verdict["cases"]:
        failures = extras[entry["instance_id"]]["failures"]
        cells = (entry["instance_id"], entry["status"], entry["outcome"], _interval(entry),
                 entry["baseline"]["median"] if entry["baseline"] else "—",
                 entry["candidate"]["median"] if entry["candidate"] else "—", _change(entry),
                 "%d/%d" % (failures["baseline"], failures["candidate"]),
                 entry["missing_records"])
        lines.append("| %s |" % " | ".join(render.markdown_cell(cell) for cell in cells))
    lines.append("")
    lines.append(render.markdown_cell("%d of %d planned records matched. %s" % (
        verdict["aggregate"]["coverage"]["matched_records"],
        verdict["aggregate"]["coverage"]["expected_records"], comparison(doc))))
    lines.append("")
    lines.append("#### Diagnostics")
    lines.append("")
    if not doc["diagnostics"]:
        lines.append(render.markdown_cell("No diagnostic child run is linked: every case's "
                                          "profile and trace is unavailable here."))
        lines.append("")
        return "\n".join(lines)
    lines.append("| Case | Profile | Trace | Omissions |")
    lines.append("| --- | --- | --- | --- |")
    for item in doc["cases"]:
        omissions = item["profile"]["omissions"] + item["trace"]["omissions"]
        cells = (item["instance_id"], _summary_state(item["profile"]),
                 _summary_state(item["trace"]),
                 "; ".join("%s: %s" % (entry["side"] or "both sides", entry["reason"])
                           for entry in omissions) or "none")
        lines.append("| %s |" % " | ".join(render.markdown_cell(cell) for cell in cells))
    lines.append("")
    lines.append(render.markdown_cell("Child runs: %s. Their pages are in the report "
                                      "artifact; they never change this verdict." % ", ".join(
                                          child["run_id"] for child in doc["diagnostics"])))
    lines.append("")
    return "\n".join(lines)


def _summary_state(state):
    """A case's profile or trace state, with each trace's coverage and overhead label."""
    notes = sorted(set(item["note"] for item in state["items"]
                       if item["kind"] == "timeline" and item["note"]))
    return state["state"] + (" (%s)" % "; ".join(notes) if notes else "")


def write(run_dir, output, job_summary=None, evidence=True, diagnostics=()):
    """Evaluate `run_dir` and write its report below the new directory `output`; the report
    document. `job_summary`, when given, is a file the summary table is appended to;
    `diagnostics` are child run directories to verify and link (ADR-0177)."""
    return write_evaluated(run_dir, output, job_summary, evidence, diagnostics)[0]


def write_evaluated(run_dir, output, job_summary=None, evidence=True, diagnostics=()):
    """`write`, returning (report document, the evaluation it was built from)."""
    output = prepare_output(output, run_dir)
    evaluation = compare.evaluate(run_dir)
    # Every child is verified before anything of the report is written.
    children = admit_children(diagnostics, evaluation.plan)
    downloads = copy_evidence(run_dir, evaluation, output) if evidence else []
    for child in children:
        copy_child(child, output)
    verdict_bytes = document_bytes(evaluation.verdict)
    _write_new(os.path.join(output, VERDICT_JSON), verdict_bytes)
    downloads.append({"path": VERDICT_JSON, "sha256": sha256_bytes(verdict_bytes),
                      "size": len(verdict_bytes), "kind": "verdict"})
    doc = build(evaluation, downloads, children)
    _write_new(os.path.join(output, REPORT_JSON), contracts.write_bytes(doc, "report"))
    _write_new(os.path.join(output, REPORT_HTML), html_bytes(doc))
    summary = summary_markdown(doc)
    _write_new(os.path.join(output, SUMMARY_MD), summary.encode("utf-8"))
    if job_summary is not None:
        with open(job_summary, "a", encoding="utf-8") as stream:
            stream.write(summary + "\n")
    return doc, evaluation


def read(path):
    """A report document, read within the JSON bound and validated."""
    try:
        return contracts.read_file(path, "report")
    except (OSError, ValueError) as error:
        raise ContractError("%s: %s" % (path, error))
