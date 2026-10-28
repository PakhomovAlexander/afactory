"""Recompute a verdict from retained evidence; never add, drop or replace a sample.

A run directory holds the run bundle (`run.json`, `builds.json`, `manifest.json`) and one
bundle per shard under `shards/<index>/` (`shard.json`, `samples.jsonl`, optional `logs/`).
Every planned slot, sentinels included, must be matched by exactly one record, in planned
order, on a shard whose terminal record says it completed; identity, order, hash, recipe and
shape corruption is refused as integrity failure, and a missing record of any case or sentinel
or a failed or interrupted shard as coverage failure. Precedence of the aggregate result:
integrity or coverage failure (incomplete), then any critical failure (fail), then any other
critical incomplete case (incomplete), then pass.
"""
from fractions import Fraction
import os
import re

from . import bundle, contracts, images, limits, schedule, stats
from .values import ContractError, canonical_bytes, format_rational, sha256_bytes

RUN_FILES = ("builds.json", "run.json")
SHARD_DIR = "shards"
LEVEL_INTEGRITY = 0
LEVEL_FAIL = 1
LEVEL_INCOMPLETE = 2


class InvalidInput(ContractError):
    """The plan itself is unusable: exit 2 before any verdict."""


# Every code an integrity reason carries: evidence that is not a valid execution of the plan
# (ADR-0185). Cost and shipping accounting read these to keep an affected execution out of their
# counts; `Reasons.add` refuses an integrity reason outside the list, so it stays complete.
INTEGRITY_CODES = ("build_incompatible", "builds_invalid", "duplicate_record",
                   "identity_mismatch", "missing_records", "missing_shard", "record_shape",
                   "reordered_records", "run_bundle_invalid", "run_deadline_exceeded",
                   "shard_bundle_invalid", "shard_identity", "shard_not_complete",
                   "unexpected_record", "unexpected_shard")


class Reasons:
    def __init__(self):
        self.items = {}

    def add(self, level, code, detail, shard=None, instance_id=None):
        if level == LEVEL_INTEGRITY and code not in INTEGRITY_CODES:
            raise AssertionError("integrity reason %s is not in INTEGRITY_CODES" % code)
        key = (level, code, -1 if shard is None else shard, instance_id or "")
        if key not in self.items:
            self.items[key] = {"code": code, "shard": shard, "instance_id": instance_id,
                               "detail": detail[:4096]}

    def levels(self):
        return set(key[0] for key in self.items)

    def ordered(self):
        return [self.items[key] for key in sorted(self.items)]


def _read_plan(run_dir):
    path = os.path.join(run_dir, "run.json")
    if not os.path.isfile(path) or os.path.islink(path):
        raise InvalidInput("%s: no regular run.json" % run_dir)
    try:
        raw = _read(path)
        plan = contracts.read_json(raw, "run", path)
    except ContractError as error:
        raise InvalidInput(str(error)) from error
    if plan["scope"] == "diagnostic":
        raise InvalidInput("%s: a diagnostic run has no timing verdict" % path)
    return plan, raw


def _verify_run_bundle(run_dir, raw_plan, reasons):
    entries = sorted(os.listdir(run_dir))
    for name in entries:
        if name not in RUN_FILES + (bundle.MANIFEST, SHARD_DIR):
            reasons.add(LEVEL_INTEGRITY, "run_bundle_invalid", "unexpected entry " + name)
    shards_path = os.path.join(run_dir, SHARD_DIR)
    if os.path.islink(shards_path) or (os.path.exists(shards_path)
                                       and not os.path.isdir(shards_path)):
        reasons.add(LEVEL_INTEGRITY, "run_bundle_invalid", "shards/ is not a directory")
    try:
        identity, hashes = bundle.verify(run_dir, limits.MAX_JSON_BYTES * 2,
                                         allowed=lambda path: path in RUN_FILES,
                                         skip=(SHARD_DIR,), label="run")
    except ContractError as error:
        reasons.add(LEVEL_INTEGRITY, "run_bundle_invalid", str(error))
        return None, None
    missing = [name for name in RUN_FILES if name not in hashes]
    if missing:
        reasons.add(LEVEL_INTEGRITY, "run_bundle_invalid",
                    "the run bundle lacks " + ", ".join(missing))
        return None, None
    if sha256_bytes(raw_plan) != hashes["run.json"]:
        reasons.add(LEVEL_INTEGRITY, "run_bundle_invalid",
                    "the evaluated run.json is not the bundle's run.json")
        return None, None
    return identity, hashes


def _read(path, limit=limits.MAX_JSON_BYTES):
    with open(path, "rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ContractError("%s: exceeds %d bytes" % (path, limit))
    return data


def _builds(run_dir, plan, hashes, reasons, found=None):
    """Each side's expected binary per case kind, or None when no build is usable; `found`,
    when given, receives the validated build manifest."""
    if hashes is None or "builds.json" not in hashes:
        return None
    try:
        data = bundle.read_member(run_dir, "builds.json", hashes, limits.MAX_JSON_BYTES, "run")
        doc = contracts.read_json(data, "builds", "builds.json")
    except ContractError as error:
        reasons.add(LEVEL_INTEGRITY, "builds_invalid", str(error))
        return None
    if found is not None:
        found.append(doc)
    resolved = plan["resolved"]
    mismatches = []
    if doc["run_id"] != plan["run_id"] or doc["plan_digest"] != plan["plan_digest"]:
        mismatches.append("names another run or plan")
    if (doc["recipe_id"] != plan["profile"]["recipe_id"]
            or doc["recipe_digest"] != plan["profile"]["recipe_digest"]):
        mismatches.append("names another recipe")
    if doc["supervisor"]["source_sha"] != resolved["authority_sha"]:
        mismatches.append("the supervisor was not built from the authority")
    for side, key in ((schedule.BASELINE, "baseline_sha"), (schedule.CANDIDATE, "candidate_sha")):
        if doc["sides"][side]["source_sha"] != resolved[key]:
            mismatches.append("the %s build is not of the planned source" % side)
    # The recorded settings and Cargo.lock of every build must be the frozen recipe's and the
    # digest planning read from that source's Git objects; a copied recipe digest proves nothing.
    expected = contracts.recipe_settings(plan["profile"]["recipe"])
    if doc["linker"]["kind"] != expected["linker"]:
        mismatches.append("the recorded linker is %s, not the recipe's %s"
                          % (doc["linker"]["kind"], expected["linker"]))
    # The build ran exactly as the plan froze it (ADR-0170): in the frozen image, recorded by
    # the same canonical reference (and, for an image ID, that ID), or as the frozen host build.
    frozen = plan["profile"]["build"]
    isolation = doc["isolation"]
    if frozen["isolation"] == "container":
        wrong = (images.mismatch(frozen["image"], isolation["image"])
                 if isolation["kind"] == "container" else "the build ran on a host")
        if wrong:
            mismatches.append("the plan froze build image %s; %s" % (frozen["image"], wrong))
    elif isolation["kind"] != "host_process" or isolation["image"] is not None:
        mismatches.append("the plan froze a %s build; the build ran in %s"
                          % (frozen["isolation"].replace("_", " "),
                             isolation["image"]["reference"]))
    # The run's execution start and total deadline are the plan's frozen limit from the build's
    # start; a build that ended after that deadline is over-limit evidence.
    run = doc["execution"]["run"]
    if run["total_minutes"] != plan["limits"]["run_total_minutes"]:
        mismatches.append("the run deadline is %d minutes, not the plan's %d"
                          % (run["total_minutes"], plan["limits"]["run_total_minutes"]))
    inputs = plan["build_inputs"]
    for name, entry, role in (("supervisor", doc["supervisor"], "authority"),
                              (schedule.BASELINE, doc["sides"][schedule.BASELINE],
                               schedule.BASELINE),
                              (schedule.CANDIDATE, doc["sides"][schedule.CANDIDATE],
                               schedule.CANDIDATE)):
        for key in contracts.RECIPE_SETTINGS:
            if entry["settings"][key] != expected[key]:
                mismatches.append("the %s build used %s %s, not the recipe's %s"
                                  % (name, key, _shown(entry["settings"][key]),
                                     _shown(expected[key])))
        lock = entry["cargo_lock_sha256"]
        if lock is not None and lock != inputs[role]["cargo_lock_sha256"]:
            mismatches.append("the %s build's Cargo.lock %s is not the planned %s"
                              % (name, lock, inputs[role]["cargo_lock_sha256"]))
    if mismatches:
        reasons.add(LEVEL_INTEGRITY, "builds_invalid", "builds.json " + "; ".join(mismatches))
        return None
    failed = [side for side in contracts.SIDES if doc["sides"][side]["status"] != "succeeded"]
    if doc["supervisor"]["status"] != "succeeded":
        failed.append("supervisor")
    kinds = set(case["kind"] for case in plan["cases"])
    # Sentinels run the baseline probe on every shard; cases need their own kind's binary.
    needed = {schedule.BASELINE: kinds | {"library"}, schedule.CANDIDATE: kinds}
    for side in contracts.SIDES:
        binaries = doc["sides"][side]["binaries"]
        for kind, key in (("process", "af"), ("library", "probe")):
            if kind in needed[side] and binaries[key] is None and side not in failed:
                failed.append("%s %s binary" % (side, key))
    binding = doc["adapter_binding"]
    if "library" in kinds and not failed and not binding["equivalent"]:
        # Library cases compare two probe adapters only within one reviewed equivalence group.
        failed.append("library cases: the baseline adapter %s (%s) and candidate adapter %s "
                      "(%s) are not one equivalence group"
                      % (binding["baseline"], binding["baseline_group"], binding["candidate"],
                         binding["candidate_group"]))
    if failed:
        reasons.add(LEVEL_INTEGRITY, "build_incompatible",
                    "no usable build for " + ", ".join(failed))
        return None
    binaries = dict((side, {"process": doc["sides"][side]["binaries"]["af"],
                            "library": doc["sides"][side]["binaries"]["probe"]})
                    for side in contracts.SIDES)
    binaries["supervisor"] = doc["supervisor"]["binary_sha256"]
    binaries["run"] = run
    if doc["execution"]["ended_unix_ms"] > run["deadline_unix_ms"]:
        reasons.add(LEVEL_INTEGRITY, "run_deadline_exceeded", "the build ended %d ms after the "
                    "run's total deadline" % (doc["execution"]["ended_unix_ms"]
                                              - run["deadline_unix_ms"]))
    return binaries


def _shown(value):
    return "null" if value is None else canonical_bytes(value).decode("utf-8")


def _shard_allowed(path):
    return path in ("samples.jsonl", "shard.json") or path.startswith("logs/")


class ShardEvidence:
    def __init__(self, index, expected):
        self.index = index
        self.expected = expected
        self.bundle = None
        self.provenance = None
        self.status = None
        self.failure = None
        self.records = {}
        # The verified bundle's member hashes and its terminal record, for reports.
        self.hashes = None
        self.doc = None

    def entry(self):
        return {"shard": self.index, "status": self.status, "failure": self.failure,
                "expected_records": self.expected, "matched_records": len(self.records)}


# The execution mode a plan's environment requires of every measured shard (ADR-0168).
MODES = {"trusted_local_process_group": "trusted_local", "linux_container": "linux_container"}
# The scope a case's primary metric must have: what its timing boundary measures.
PRIMARY_SCOPES = {"child_process_lifetime": "child_process_lifetime",
                  "reported_interval": "probe_reported",
                  "library_region": "library_region"}


def _load_shard(run_dir, plan, index, binaries, reasons, builds_sha=None):
    slots = schedule.expected_slots(plan, index)
    evidence = ShardEvidence(index, len(slots))
    root = os.path.join(run_dir, SHARD_DIR, str(index))
    if not os.path.lexists(root):
        reasons.add(LEVEL_INTEGRITY, "missing_shard", "no evidence was uploaded for shard %d"
                    % index, shard=index)
        return evidence
    try:
        label = "shards/%d" % index
        identity, hashes = bundle.verify(root, plan["limits"]["shard_bundle_bytes"],
                                         allowed=_shard_allowed, label=label)
        shard_doc = contracts.read_json(
            bundle.read_member(root, "shard.json", hashes, limits.MAX_JSON_BYTES, label), "shard",
            "shards/%d/shard.json" % index)
        samples_data = bundle.read_member(root, "samples.jsonl", hashes,
                                          plan["limits"]["shard_bundle_bytes"], label)
        records = contracts.read_samples(samples_data, "shards/%d/samples.jsonl" % index)
    except ContractError as error:
        reasons.add(LEVEL_INTEGRITY, "shard_bundle_invalid", str(error), shard=index)
        return evidence
    evidence.bundle = identity
    evidence.hashes = hashes
    problems = []
    if (shard_doc["run_id"], shard_doc["plan_digest"], shard_doc["shard"]) != (
            plan["run_id"], plan["plan_digest"], index):
        problems.append("shard.json names another run, plan or shard")
    if shard_doc["expected_records"] != len(slots):
        problems.append("shard.json expects %d records; the plan expects %d"
                        % (shard_doc["expected_records"], len(slots)))
    if (shard_doc["record_count"] != len(records)
            or shard_doc["samples_bytes"] != len(samples_data)
            or shard_doc["samples_sha256"] != sha256_bytes(samples_data)):
        problems.append("shard.json does not describe samples.jsonl")
    execution = shard_doc["execution"]
    if shard_doc["provenance"] == "measured":
        wanted = MODES[plan["profile"]["environment"]["containment"]]
        if execution["mode"] != wanted:
            problems.append("shard.json ran in %s mode; the plan's environment requires %s"
                            % (execution["mode"], wanted))
        if binaries is not None and execution["supervisor_sha256"] != binaries["supervisor"]:
            problems.append("shard.json names a supervisor the build manifest does not")
        if builds_sha is not None and execution["builds_sha256"] != builds_sha:
            problems.append("shard.json ran another build than the run bundle's builds.json")
    if binaries is not None and execution["run"] != binaries["run"]:
        # One run, one execution start and total deadline (ADR-0170).
        problems.append("shard.json names another run start or total deadline than its "
                        "build recorded")
    if problems:
        reasons.add(LEVEL_INTEGRITY, "shard_identity", "; ".join(problems), shard=index)
        return evidence
    evidence.doc = shard_doc
    evidence.provenance = shard_doc["provenance"]
    evidence.status = shard_doc["status"]
    evidence.failure = shard_doc["failure"]
    run_deadline = execution["run"]["deadline_unix_ms"]
    late = [record["sequence"] for record in records if record["started_unix_ms"]
            > execution["deadline_unix_ms"]]
    if shard_doc["ended_unix_ms"] > run_deadline or late:
        # Evidence past the run's total deadline (ADR-0170) is never complete evidence.
        reasons.add(LEVEL_INTEGRITY, "run_deadline_exceeded",
                    "shard %d %s" % (index, "ended %d ms after the run's total deadline"
                                     % (shard_doc["ended_unix_ms"] - run_deadline)
                                     if shard_doc["ended_unix_ms"] > run_deadline else
                                     "started sequence %d after its deadline" % late[0]),
                    shard=index)
    if evidence.status != "complete":
        # The shard itself says it did not finish (an interrupted schedule, an unconfirmed
        # cleanup): its records stay visible, but they are not complete evidence of the run.
        failure = evidence.failure
        reasons.add(LEVEL_INTEGRITY, "shard_not_complete",
                    "shard %d ended %s (%s: %s) with %d of %d planned records"
                    % (index, evidence.status, failure["reason"], failure["detail"],
                       len(records), len(slots)), shard=index)
    by_key = dict((schedule.slot_key(slot), slot) for slot in slots)
    cases = dict((case["instance_id"], case) for case in plan["cases"])
    # A seeded sample copied a seed its own side wrote and the shard sealed (ADR-0169): a CLI
    # seed, or the native seed its library case declares (ADR-0170).
    seeded = dict(((seed["side"], seed["digest"]), seed) for seed in shard_doc["seeds"]
                  if seed["status"] == "ready")
    seed_digests = set(digest for _, digest in seeded)
    sources = {schedule.BASELINE: plan["resolved"]["baseline_sha"],
               schedule.CANDIDATE: plan["resolved"]["candidate_sha"]}
    workload = plan["health"]["workload"]
    highest = None
    for line, record in enumerate(records, 1):
        key = schedule.slot_key(record)
        where = "shard %d record %s/%s/%d/%d" % ((index,) + key)
        # Records appear in strictly increasing planned sequence. A partial shard may skip
        # slots, which stay missing; a record at or below an earlier sequence is reordered or
        # repeated evidence, refused without being counted as missing.
        if highest is not None and record["sequence"] <= highest:
            reasons.add(LEVEL_INTEGRITY, "reordered_records",
                        "shards/%d/samples.jsonl:%d holds sequence %d after sequence %d; "
                        "records follow the planned order" % (index, line, record["sequence"],
                                                              highest), shard=index)
        highest = record["sequence"] if highest is None else max(highest, record["sequence"])
        slot = by_key.get(key)
        if record["run_id"] != plan["run_id"] or record["plan_digest"] != plan["plan_digest"] \
                or record["shard"] != index:
            reasons.add(LEVEL_INTEGRITY, "identity_mismatch", where + ": names another run, "
                        "plan or shard", shard=index)
            continue
        if slot is None:
            reasons.add(LEVEL_INTEGRITY, "unexpected_record", where + ": not a planned slot",
                        shard=index)
            continue
        if key in evidence.records:
            reasons.add(LEVEL_INTEGRITY, "duplicate_record", where + ": recorded twice",
                        shard=index)
            evidence.records[key] = None
            continue
        identity_problems = []
        if record["sequence"] != slot["sequence"]:
            identity_problems.append("ran out of the planned order")
        if record["side"] != slot["side"]:
            identity_problems.append("ran the wrong side for its AB/BA position")
        if record["source_sha"] != sources[record["side"]]:
            identity_problems.append("measured another source")
        if record["host_id"] != shard_doc["host_id"]:
            identity_problems.append("ran on another host than its shard")
        kind = "library" if record["instance_id"] == workload else \
            cases[record["instance_id"]]["kind"]
        if record["execution"]["containment"] != shard_doc["execution"]["containment"]:
            identity_problems.append("ran in another containment than its shard")
        if binaries is not None and record["binary_sha256"] != binaries[record["side"]][kind]:
            identity_problems.append("executed a binary the build manifest does not name")
        fixture = record["identity"]["fixture_digest"]
        if kind == "process" and record["status"] == "ok" and (
                fixture in seed_digests or _uses_seed(cases[record["instance_id"]])) \
                and (record["side"], fixture) not in seeded:
            identity_problems.append("copied no seed its own side wrote in this shard")
        if kind == "library" and record["instance_id"] != workload \
                and cases[record["instance_id"]]["seed"] is not None \
                and record["status"] == "ok":
            native = seeded.get((record["side"], fixture))
            if native is None or (native["kind"], native["seed"], native["writer"]) != (
                    "native", cases[record["instance_id"]]["seed"]["seed_id"],
                    cases[record["instance_id"]]["seed"]["contract_id"]):
                identity_problems.append("read no copy of the %s seed its own side wrote in "
                                         "this shard" % cases[record["instance_id"]]["seed"]
                                         ["seed_id"])
        shape = _metric_shape(plan, cases, record)
        if shape:
            reasons.add(LEVEL_INTEGRITY, "record_shape", where + ": " + shape, shard=index)
            evidence.records[key] = None
            continue
        if identity_problems:
            reasons.add(LEVEL_INTEGRITY, "identity_mismatch",
                        where + ": " + "; ".join(identity_problems), shard=index)
            evidence.records[key] = None
            continue
        evidence.records[key] = record
    evidence.records = dict((key, value) for key, value in evidence.records.items()
                            if value is not None)
    return evidence


def _uses_seed(case):
    """Whether a process case reads side-written state: the CLI state family's seeds."""
    return case["family"] == "cli_state"


INJECTED_METRIC = "calibration_injected_ms"


def _declared_metrics(plan, cases, instance_id):
    if instance_id == plan["health"]["workload"]:
        return {plan["health"]["metric"]: "ms"}, plan["health"]["metric"]
    case = cases[instance_id]
    declared = dict((metric["key"], metric["unit"]) for metric in case["optional_metrics"])
    declared[case["primary_metric"]["key"]] = case["primary_metric"]["unit"]
    cohort = plan["cohort"]
    if cohort is not None and cohort["injection"] is not None:
        # Every sample of a preparation or slowdown member reports the injection it measured
        # inside its timed operation, or says why not (ADR-0181).
        declared[INJECTED_METRIC] = "ms"
    return declared, case["primary_metric"]["key"]


def _metric_shape(plan, cases, record):
    """A reason the record's identity or metrics break the plan's declarations, or None."""
    declared, primary = _declared_metrics(plan, cases, record["instance_id"])
    workload = plan["health"]["workload"]
    sentinel = record["instance_id"] == workload
    case = None if sentinel else cases[record["instance_id"]]
    identity = record["identity"]
    if identity["case_id"] != (workload if sentinel else case["case_id"]):
        return "names case %s, not its planned case" % identity["case_id"]
    if identity["parameters"] != ({} if sentinel else case["parameters"]):
        return "names another parameter assignment than its planned instance"
    boundary = "library_region" if sentinel else case["timing"]["boundary"]
    if primary in record["metric_scopes"] \
            and record["metric_scopes"][primary] != PRIMARY_SCOPES[boundary]:
        return "reports %s with scope %s, not its timing boundary %s" % (
            primary, record["metric_scopes"][primary], boundary)
    for key, metric in record["metrics"].items():
        if key not in declared:
            return "reports undeclared metric " + key
        if metric["unit"] != declared[key]:
            return "reports %s in %s, not %s" % (key, metric["unit"], declared[key])
    for key in record["unavailable_metrics"]:
        if key not in declared:
            return "declares undeclared metric %s unavailable" % key
        if key == primary:
            return "declares its required metric %s unavailable" % key
    for key in declared:
        if key != primary and key not in record["metrics"] \
                and key not in record["unavailable_metrics"]:
            return "omits optional metric %s without an unavailable reason" % key
    return None


def _failure_kind(record, primary):
    if record["status"] == "failed":
        return record["failure"]["kind"]
    if primary not in record["metrics"]:
        return "missing_required_metric"
    return None


def _value(record, key):
    return Fraction(record["metrics"][key]["value"])


def _health(plan, evidence):
    health = plan["health"]
    floor = Fraction(health["denominator_floor"])
    phases = {"sentinel_start": [], "sentinel_end": []}
    flags = []
    for slot in schedule.expected_slots(plan, evidence.index):
        if slot["phase"] not in phases:
            continue
        record = evidence.records.get(schedule.slot_key(slot))
        if record is None:
            if "missing_sentinel" not in flags:
                flags.append("missing_sentinel")
            continue
        if _failure_kind(record, health["metric"]) is not None:
            if "sentinel_failure" not in flags:
                flags.append("sentinel_failure")
            continue
        phases[slot["phase"]].append(_value(record, health["metric"]))
    entry = {"shard": evidence.index, "status": "unstable", "flags": flags,
             "start_median": None, "end_median": None, "shift": None,
             "start_mad_ratio": None, "end_mad_ratio": None}
    if flags:
        return entry
    result = stats.sentinel_health(phases["sentinel_start"], phases["sentinel_end"],
                                   Fraction(health["max_median_shift"]),
                                   Fraction(health["max_mad_ratio"]), floor)
    for key in ("start_median", "end_median", "shift", "start_mad_ratio", "end_mad_ratio"):
        entry[key] = format_rational(result[key])
    entry["flags"] = result["flags"]
    entry["status"] = "unstable" if result["flags"] else "healthy"
    return entry


def _summary(values):
    return {"median": format_rational(stats.median(values)),
            "p95": format_rational(stats.nearest_rank(values, 95)),
            "max": format_rational(max(values))}


def _case(plan, case, evidence, health_entry, reasons):
    """One case's entry. Its status takes, in order: missing records, an incomplete shard,
    functional failures, environment health, a regression, a missed enabled budget, an
    inconclusive interval; the interval, budgets and failures are always retained."""
    primary = case["primary_metric"]["key"]
    entry = {
        "instance_id": case["instance_id"], "criticality": case["criticality"],
        "exploratory": case["exploratory"], "shard": case["shard"], "pairs": case["pairs"],
        "rank": case["rank"], "alpha": case["alpha"], "metric": case["primary_metric"],
        "tolerance": {"relative": case["tolerance"]["relative"],
                      "absolute": case["tolerance"]["absolute"]},
        "status": None, "statistical_classification": None, "outcome": None,
        "interval": None, "baseline": None, "candidate": None, "change": None,
        "missing_records": 0, "failures": [], "budgets": [],
    }
    pairs = {}
    missing = 0
    for phase, orders in (("warmup", case["warmup_orders"]), ("measured", case["measured_orders"])):
        for pair in range(1, len(orders) + 1):
            for position in (1, 2):
                record = evidence.records.get((phase, case["instance_id"], pair, position))
                if record is None:
                    missing += 1
                    continue
                kind = _failure_kind(record, primary)
                if kind is not None:
                    entry["failures"].append({"side": record["side"], "phase": phase,
                                              "pair": pair, "position": position,
                                              "kind": kind})
                elif phase == "measured":
                    pairs.setdefault(pair, {})[record["side"]] = _value(record, primary)
    entry["missing_records"] = missing
    sides_failed = set(item["side"] for item in entry["failures"])
    complete = [pairs[pair] for pair in sorted(pairs) if len(pairs[pair]) == 2]
    critical = case["criticality"] == "critical"
    statistic = None
    if not missing and not sides_failed and len(complete) == case["pairs"]:
        baseline = [pair[schedule.BASELINE] for pair in complete]
        candidate = [pair[schedule.CANDIDATE] for pair in complete]
        losses = stats.paired_losses(baseline, candidate,
                                     Fraction(case["tolerance"]["relative"]),
                                     Fraction(case["tolerance"]["absolute"]),
                                     case["primary_metric"]["direction"])
        lower, upper = stats.order_interval(losses, case["rank"])
        statistic = stats.classify(lower, upper)
        entry["interval"] = {"lower": format_rational(lower), "upper": format_rational(upper)}
        entry["statistical_classification"] = statistic
        entry["baseline"] = _summary(baseline)
        entry["candidate"] = _summary(candidate)
        base_median = stats.median(baseline)
        change = stats.median(candidate) - base_median
        entry["change"] = {"absolute": format_rational(change),
                           "percent": (format_rational(change * 100 / base_median)
                                       if base_median else None)}
    budget_missed = False
    for budget in case["budgets"]:
        result = "not_applicable"
        observed = None
        if entry["candidate"] is not None:
            observed = entry["candidate"]["max"]
        if budget["applicable"]:
            if budget["state"] != "enabled":
                result = "pending_calibration"
            elif observed is None:
                result = "not_evaluated"
            elif Fraction(observed) < Fraction(budget["limit"]):
                result = "met"
            else:
                result = "missed"
                budget_missed = True
        entry["budgets"].append({"budget_id": budget["budget_id"], "state": budget["state"],
                                 "applicable": budget["applicable"], "limit": budget["limit"],
                                 "result": result, "observed_max": observed})

    ident = case["instance_id"]
    if missing:
        status, outcome = "missing_records", "incomplete"
        # A missing record is a coverage failure whatever the case's criticality; only an
        # advisory case's present negative evidence (a failure, a regression) never gates.
        reasons.add(LEVEL_INTEGRITY, "missing_records", "%d of %d planned records of %s case "
                    "%s are missing" % (missing, 2 * (len(case["warmup_orders"])
                                                      + len(case["measured_orders"])),
                                        case["criticality"], ident),
                    shard=case["shard"], instance_id=ident)
    elif evidence.status != "complete":
        status, outcome = "shard_not_complete", "incomplete"
    elif sides_failed == {schedule.CANDIDATE}:
        status, outcome = "candidate_failure", "fail"
    elif schedule.BASELINE in sides_failed:
        status, outcome = "baseline_failure", "incomplete"
    elif health_entry["status"] != "healthy":
        status, outcome = "environment_unstable", "incomplete"
    elif statistic == stats.REGRESSION:
        status, outcome = statistic, "fail"
    elif budget_missed:
        # Every candidate sample was observed, so a sample at or above an enabled limit is a
        # definitive miss even when the relative interval cannot rule a regression in or out.
        status, outcome = "budget_missed", "fail"
    elif statistic == stats.INCONCLUSIVE:
        status, outcome = statistic, "incomplete"
    else:
        status, outcome = statistic, "pass"
    entry["status"] = status
    entry["outcome"] = outcome
    if critical and outcome == "fail":
        reasons.add(LEVEL_FAIL, status, "%s: %s" % (ident, status), shard=case["shard"],
                    instance_id=ident)
    elif critical and outcome == "incomplete" and status not in ("missing_records",
                                                                 "shard_not_complete"):
        reasons.add(LEVEL_INCOMPLETE, status, "%s: %s" % (ident, status), shard=case["shard"],
                    instance_id=ident)
    return entry


def _calibrated_hosts(plan, evidence, reasons):
    """An eligible plan's activation calibrated exact CPU/image groups (ADR-0184): a shard whose
    observed CPU model and sample image are none of them, or unknown, measured outside the
    calibration, so the run cannot satisfy the required check. Its evidence still counts: a
    regression there stays a failure."""
    activation = plan["authority"]["activation"]
    if not plan["required_check_eligible"] or activation is None:
        return
    calibrated = set((group["cpu_model"], group["image"])
                     for group in activation["calibrated_groups"])
    for item in evidence:
        # A synthetic shard observed no host and satisfies nothing; a measured one is bound.
        if item.doc is None or item.provenance != "measured":
            continue
        observed = (item.doc["host"]["cpu_model"].get("value"),
                    item.doc["execution"]["sample_image"])
        if observed not in calibrated:
            reasons.add(LEVEL_INCOMPLETE, "uncalibrated_cpu_group",
                        "shard %d ran on CPU %s with sample image %s, outside the groups %s its "
                        "activation's cohort calibrated; full-check status is unavailable there"
                        % (item.index, observed[0] or "unknown", observed[1] or "unknown",
                           ", ".join(group["group_id"]
                                     for group in activation["calibrated_groups"])),
                        shard=item.index)


class Evaluation(object):
    """A run's recomputed verdict and the evidence it was computed from: the plan, the run
    bundle's verified member hashes, the validated build manifest (None when unusable) and
    each planned shard's verified evidence. Reports and the archive read only this."""

    def __init__(self, plan, verdict, hashes, builds_doc, evidence, recorded_builds=None):
        self.plan = plan
        self.verdict = verdict
        self.hashes = hashes
        self.builds = builds_doc
        self.evidence = evidence
        # The validated build manifest of this run and plan even when no build in it is usable
        # (a failed build): what it recorded, never a binary to verify against.
        self.recorded_builds = recorded_builds


def compare(run_dir):
    """Compute the verdict document of a run directory."""
    return evaluate(run_dir).verdict


def evaluate(run_dir):
    """The `Evaluation` of a run directory; its verdict is exactly `compare`'s."""
    plan, raw_plan = _read_plan(run_dir)
    reasons = Reasons()
    run_identity, hashes = _verify_run_bundle(run_dir, raw_plan, reasons)
    found = []
    binaries = _builds(run_dir, plan, hashes, reasons, found)
    shards_root = os.path.join(run_dir, SHARD_DIR)
    planned = [str(shard["index"]) for shard in plan["shards"]]
    if os.path.isdir(shards_root) and not os.path.islink(shards_root):
        for name in sorted(os.listdir(shards_root)):
            if name not in planned or not re.match(r"^(?:0|[1-9][0-9]?)$", name):
                reasons.add(LEVEL_INTEGRITY, "unexpected_shard", "shards/%s is not a planned "
                            "shard" % name)
    builds_sha = hashes.get("builds.json") if hashes else None
    evidence = [_load_shard(run_dir, plan, shard["index"], binaries, reasons, builds_sha)
                for shard in plan["shards"]]
    health = [_health(plan, item) for item in evidence]
    workload = plan["health"]["workload"]
    for item in evidence:
        sentinels = [slot for slot in schedule.expected_slots(plan, item.index)
                     if slot["instance_id"] == workload]
        absent = sum(1 for slot in sentinels if schedule.slot_key(slot) not in item.records)
        if absent:
            reasons.add(LEVEL_INTEGRITY, "missing_records", "%d of %d planned sentinel records "
                        "of shard %d are missing" % (absent, len(sentinels), item.index),
                        shard=item.index, instance_id=workload)
    _calibrated_hosts(plan, evidence, reasons)
    for entry in health:
        if entry["status"] != "healthy":
            reasons.add(LEVEL_INCOMPLETE, "environment_unstable",
                        "shard %d: %s" % (entry["shard"], ", ".join(entry["flags"])),
                        shard=entry["shard"])
    cases = [_case(plan, case, evidence[case["shard"]], health[case["shard"]], reasons)
             for case in plan["cases"]]
    levels = reasons.levels()
    if LEVEL_INTEGRITY in levels:
        result = "incomplete"
    elif LEVEL_FAIL in levels:
        result = "fail"
    elif LEVEL_INCOMPLETE in levels:
        result = "incomplete"
    else:
        result = "pass"
    provenances = sorted(set(item.provenance for item in evidence if item.provenance))
    provenance = ("none" if not provenances else provenances[0] if len(provenances) == 1
                  else "mixed")
    builds_sha = hashes.get("builds.json") if hashes else None
    shards = [item.entry() for item in evidence]
    expected = sum(entry["expected_records"] for entry in shards)
    matched = sum(entry["matched_records"] for entry in shards)
    verdict = {
        "schema": contracts.VERDICT_SCHEMA, "run_id": plan["run_id"],
        "plan_digest": plan["plan_digest"], "scope": plan["scope"], "provenance": provenance,
        "inputs": {"run_bundle": run_identity, "builds_sha256": builds_sha,
                   "shard_bundles": [{"shard": item.index, "bundle": item.bundle}
                                     for item in evidence]},
        "shards": shards,
        "algorithm": plan["verdict_policy"]["algorithm"],
        "family_alpha": plan["verdict_policy"]["family_alpha"],
        "family_size": plan["verdict_policy"]["family_size"],
        "case_alpha": plan["verdict_policy"]["case_alpha"],
        "health": health, "cases": cases,
        "aggregate": {"result": result, "exit_code": contracts.EXIT_CODES[result],
                      "satisfies_required_check": (result == "pass"
                                                   and plan["required_check_eligible"]
                                                   and provenance == "measured"),
                      "coverage": {"expected_records": expected, "matched_records": matched,
                                   "missing_records": expected - matched},
                      "reasons": reasons.ordered()},
    }
    recorded = (found[0] if found and (found[0]["run_id"], found[0]["plan_digest"])
                == (plan["run_id"], plan["plan_digest"]) else None)
    return Evaluation(plan, contracts.validate_verdict(verdict), hashes,
                      found[0] if found and binaries is not None else None, evidence,
                      recorded)
