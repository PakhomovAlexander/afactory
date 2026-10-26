"""Calibration cohorts, preparation, slowdown trials and readiness (p6b, ADR-0181 to ADR-0183).

A proposed policy becomes eligible for activation only through a fixed cohort (design section
15). This module declares one, plans its members, seals the calibration parameters and computes
readiness, always from retained evidence and never from an author's tallies:

- **Declaration** (`declare`). The cohort fixes, before anything runs, its authority, policy
  digest, critical family with its N and K, health rule, allowed CPU/image groups, schedule start
  and length (at least five days, fresh hosted runners) and every logical member: its ID, kind,
  seed, injected percent and day. Actions run IDs are not part of it: GitHub assigns them when a
  member is dispatched, and they are recorded beside the cohort as provenance, so the cohort's
  bytes and digest never change after the fact.
- **Members** run as `plan --scope calibration` comparisons of the authority against itself.
  The 20 A/A members are uninstrumented. Preparation and slowdown members build both sides with
  the default-off `perf-calibration` feature from the same source; only the candidate's
  environment names a nonzero loop count, for its own case and the CPU group its shard observes
  (`plan_injection`), and a host of an unprepared group refuses the shard before any sample.
- **Preparation** (`prepare`). A0 per case and group is the median baseline time of every
  measured A/A sample of the group; the loop's cost per iteration is the median of the
  preparation candidates' measured injections over their declared count. From those, each level's
  loop count is the one nearest its target, `level * A0`, with the power level
  `p = max(10%, 2 * max(r, a/A0))`. The parameters are sealed bytes whose digest every slowdown
  plan names, so no trial can precede them and nothing can be retuned after outcomes appear.
- **Readiness** (`readiness`). Every declared member stays in every denominator: missing,
  interrupted, failed, unhealthy and invalid members are recorded as such. A member's original
  is bound by its dispatch before any evidence exists (ADR-0182): the first run of the declared
  workflow, repository and branch whose run name names the cohort's digest and the member, at
  attempt 1, from the complete workflow-run history. Its retained run bundle is looked up only
  then, under the lab run ID that attempt plans, `gh<run>_a1_primary`; absent, expired or
  unreadable evidence leaves the original missing, and a later attempt, another dispatch of
  the same member, an unknown member or another producer is refused and recorded, never
  counted. Each plan must equal the one the trusted authority freezes for that member again,
  and verdicts are recomputed from the samples. Readiness requires all 20 A/A members within
  tolerance in every critical case, and at least 9 of the 10 power trials in which every
  critical case is a regression with valid injection in that same trial (median measured
  injection within 75%..125% of its target, inclusive). Whole trials are counted: per-case
  tallies are reported but never combined across trials.

Deleted runs, unsuccessful original attempts, ambiguous artifacts, unverified runners and
parameters other than those the originals give are refused or recorded, never resolved in a
member's favour (ADR-0183). So is an original whose head commit's workflow or whose control
revision's scheduling code is not the authority's own, and a gap in the run numbers is named
from the listed numbers alone, whatever their size (ADR-0184).

These are fixture-tested orchestration rules. They do not by themselves establish empirical
readiness; that needs the real hosted cohort over at least five days.
"""
import datetime
import hashlib
import os
import re
from fractions import Fraction

from . import compare, contracts, gitrefs, images, limits, runner, schedule, transport
from .values import (ContractError, canonical_digest, format_decimal, format_rational,
                     loads_strict, require, sha256_bytes)

DAY_MS = 24 * 60 * 60 * 1000
INJECTED_METRIC = compare.INJECTED_METRIC
ACTIONS_FILE = "actions.json"
RUNS_FILE = "runs.json"
ATTEMPTS_DIR = "attempts"
JOBS_DIR = "jobs"
# How GitHub names the runner of a job on a standard hosted runner, in the runner group of every
# hosted runner; with the plan's hosted label and the trusted producer, the evidence that a
# measuring job ran on a fresh hosted runner (ADR-0183). Labels alone are not.
HOSTED_RUNNER_NAME = re.compile(r"^GitHub Actions [1-9][0-9]{0,9}$")
LAB_RUN = re.compile(r"^gh([1-9][0-9]{0,18})_a([1-9][0-9]{0,3})_([a-z][a-z0-9_]{0,40})$")
RUN_ARTIFACT = re.compile(r"^af-perf-([a-z][a-z0-9_]{0,63})-run$")
SHARD_ARTIFACT = re.compile(r"^af-perf-([a-z][a-z0-9_]{0,63})-shard-([0-9]{1,2})$")
PREFIXES = {"aa": "aa", "preparation": "prep", "injection_2": "inj2", "injection_5": "inj5",
            "injection_10": "inj10", "power": "power"}


# Declaration -----------------------------------------------------------------------------

def member_seed(cohort_id, member_id):
    digest = hashlib.sha256(("af.perf-cohort-seed/1\0%s\0%s" % (cohort_id, member_id))
                            .encode("utf-8")).hexdigest()
    return int(digest[:15], 16)


def declare(suite, authority, cohort_id, tier, environment_id, recipe_id, groups, start_date,
            repository, after_run_number, minimum_days=5, preparation_members=2,
            preparation_iterations=1000000, branch="main"):
    """A complete cohort for the critical family of `tier` under `suite`: 20 A/A members on the
    first two days, the preparation members on the next, then the 30 descriptive and 10 power
    trials round-robin over the remaining days, so the schedule spans `minimum_days`. Every
    member is dispatched from `repository`'s `branch` by the performance workflow, under the
    member's run name, after the workflow's run number `after_run_number`, the latest the
    declarer observed (0 before its first run)."""
    require(minimum_days >= 5, "--days: a cohort spans at least five days")
    environment = suite.doc["environments"].get(environment_id) or {}
    require(environment.get("runner") == "github_hosted",
            "--environment: a cohort runs on fresh hosted runners; %s is not a GitHub-hosted "
            "environment" % environment_id)
    doc = suite.doc
    cases = [{"instance_id": item["instance_id"], "criticality": item["criticality"],
              "pairs": doc["cases"][item["case_id"]]["pairs"][tier]}
             for item in suite.instances(tier)]
    cases.sort(key=lambda case: case["instance_id"])
    health = dict((key, value) for key, value in doc["health"].items() if key != "description")
    health["unit"] = "ms"
    members = []

    def add(kind, level, count, days):
        for index in range(count):
            member_id = "%s_%02d" % (PREFIXES[level or kind], index + 1)
            members.append({"member_id": member_id, "kind": kind,
                            "source_sha": authority, "seed": member_seed(cohort_id, member_id),
                            "injection_percent": (int(level.split("_")[1])
                                                  if kind == "injection" else None),
                            "day": days[index % len(days)]})

    add("aa", None, contracts.COHORT_COUNTS["aa"], [0, 1])
    add("preparation", "preparation", preparation_members, [2])
    trial_days = list(range(3, minimum_days))
    for percent in contracts.INJECTION_PERCENTS:
        add("injection", "injection_%d" % percent, contracts.INJECTION_MEMBERS, trial_days)
    add("power", "power", contracts.COHORT_COUNTS["power"], trial_days)
    cohort = {
        "schema": contracts.COHORT_SCHEMA, "cohort_id": cohort_id, "authority_sha": authority,
        "policy_digest": suite.policy_digest, "tier": tier, "environment_id": environment_id,
        "recipe_id": recipe_id, "cases": cases,
        "family_size": sum(1 for case in cases if case["criticality"] == "critical"),
        "health": health,
        "allowed_cpu_groups": sorted(groups, key=lambda group: group["group_id"]),
        "schedule": {"start_date": start_date, "minimum_days": minimum_days,
                     "runners": "fresh_hosted", "order": contracts.COHORT_ORDER},
        "preparation": {"iterations": preparation_iterations},
        "dispatch": {"repository": repository, "workflow_path": contracts.DISPATCH_WORKFLOW,
                     "branch": branch, "event": "workflow_dispatch",
                     "run_name": contracts.DISPATCH_RUN_NAME,
                     "after_run_number": after_run_number},
        "target_formulas": dict(contracts.TARGET_FORMULAS),
        "members": members,
    }
    return contracts.validate_cohort(cohort, "cohort")


def parse_group(parts):
    """Three separate CLI arguments: ID, CPU MODEL, pinned registry IMAGE."""
    require(isinstance(parts, (list, tuple)) and len(parts) == 3,
            "--group: ID 'CPU MODEL' 'NAME@sha256:DIGEST'")
    ident, model, image = parts
    contracts.ID.validate(ident, "--group ID")
    require(re.fullmatch(contracts.ID_PATTERN, ident), "--group: an exact group ID")
    contracts.SHORT_TEXT.validate(model, "--group CPU MODEL")
    require(model == model.strip() and bool(model) and model.isprintable(),
            "--group: an exact nonempty printable CPU model")
    images.canonical(image, "--group IMAGE")
    require(re.fullmatch(images.REFERENCE_PATTERN, image) and not images.is_image_id(image),
            "--group: an exact registry image pinned by digest")
    return {"group_id": ident, "cpu_model": model, "image": image}


def start_unix_ms(cohort):
    date = datetime.date(*(int(part) for part in cohort["schedule"]["start_date"].split("-")))
    epoch = datetime.date(1970, 1, 1)
    return (date - epoch).days * DAY_MS


# The plan of one member ------------------------------------------------------------------

def plan_injection(cohort, calibration, member, instance_ids):
    """The frozen injection of one member's plan: None for an A/A member; the cohort's
    preparation count for every case and allowed group of a preparation member; for a slowdown
    member each case's sealed loop count at its level, in every group prepared for all its
    cases. A host of any other group refuses the shard."""
    level = contracts.member_level(member)
    if level is None:
        require(calibration is None, "an A/A member is planned without calibration parameters")
        return None
    if level == "preparation":
        require(calibration is None, "a preparation member precedes the calibration parameters")
    else:
        require(calibration is not None, "--calibration: a %s member runs the sealed "
                "calibration parameters' loop counts" % member["kind"])
        contracts.validate_calibration(calibration, "--calibration", cohort=cohort)
    groups = []
    for allowed in sorted(cohort["allowed_cpu_groups"], key=lambda group: group["group_id"]):
        if level == "preparation":
            counts = dict((ident, cohort["preparation"]["iterations"]) for ident in instance_ids)
        else:
            counts = {}
            for ident in instance_ids:
                group = contracts.calibration_group(calibration, ident, allowed["group_id"])
                if group is None or group["iterations"] is None:
                    counts = None
                    break
                counts[ident] = group["iterations"][level]
            if counts is None:
                continue
        groups.append({"cpu_group": allowed["group_id"], "cpu_model": allowed["cpu_model"],
                       "image": allowed["image"], "iterations": counts})
    require(groups, "no allowed CPU group is prepared for every case of member %s; it cannot "
            "inject a valid dose anywhere" % member["member_id"])
    return {"feature": contracts.CALIBRATION_FEATURE, "level": level,
            "calibration_digest": (None if calibration is None
                                   else canonical_digest(calibration)),
            "groups": groups}


def host_group(injection, cpu_model, image):
    """The plan's prepared group of a host with this CPU model and sample image, or None."""
    for group in injection["groups"]:
        if (group["cpu_model"], group["image"]) == (cpu_model, image):
            return group
    return None


def shard_group(cohort, shard_doc):
    """The allowed group a shard's observed CPU model and sample image belong to, or None."""
    model = shard_doc["host"]["cpu_model"].get("value")
    image = shard_doc["execution"]["sample_image"]
    for group in cohort["allowed_cpu_groups"]:
        if (group["cpu_model"], group["image"]) == (model, image):
            return group["group_id"]
    return None


def injected_report(stderr, iterations):
    """The measured injection, in ms, of the one `af.perf-calibration-injection/1` line a
    calibration build wrote to stderr for exactly `iterations`; (value, None) or (None, why)."""
    prefix = b"af.perf-calibration-injection/1 "
    lines = [line for line in stderr.split(b"\n") if line.startswith(prefix)]
    if len(lines) != 1:
        return None, "the process reported %d calibration injections, not exactly one" % len(
            lines)
    match = re.match(rb"^af\.perf-calibration-injection/1 iterations=(0|[1-9][0-9]{0,13}) "
                     rb"injected_ns=(0|[1-9][0-9]{0,30})$", lines[0])
    if match is None:
        return None, "the calibration injection report is malformed"
    if int(match.group(1)) != iterations:
        return None, "the process injected %s iterations, not the declared %d" % (
            match.group(1).decode(), iterations)
    return {"value": format_decimal(Fraction(int(match.group(2)), 1000000)), "unit": "ms"}, None


# Evidence --------------------------------------------------------------------------------
#
# A member's original is bound by its dispatch, before any of its evidence exists: the
# dispatching workflow names each member's run, `af-perf calibration <cohort digest> <member>`,
# from its inputs as the run is created. The workflow-run history of the declared workflow
# (`runs.json`, the list-workflow-runs pages of every event, newest first, down to the cohort's
# anchor) then shows every dispatch of every member, and the first is the member's original
# whatever became of it. GitHub numbers a workflow's runs in order and never reuses a number, so
# every number after the anchor must be listed: a deleted run leaves a gap, and nothing is
# judged from a history with one (ADR-0183). A deleted original can therefore never let a later
# dispatch stand in for it. A run deleted after the newest listed one had no later dispatch to
# stand in for it, so its member has none and stays missing. Attempt 1 of the original
# (`attempts/<run>.json`, the get-a-workflow-run-attempt response) gives its own start, end and
# conclusion, and its jobs (`jobs/<run>.json`, the list-jobs-for-a-workflow-run-attempt
# response) the runners its measuring jobs ran on. Only then is the run bundle attempt 1
# uploaded looked up in the artifact listing (`actions.json`, the listings of every run named
# for the cohort, which have no attempt): absent, expired or unreadable evidence leaves the
# original missing, two artifacts of one expected name leave it invalid, and nothing a later run
# uploads replaces it.

class MemberRun(object):
    """One declared member's original run, recomputed from its retained evidence."""

    def __init__(self, member, provenance=None):
        self.member = member
        self.provenance = provenance
        self.attempt = None
        self.plan = None
        self.evaluation = None
        self.status = "complete"
        self.reasons = []
        self.cpu_groups = {}
        # Whether the trusted authority freezes exactly this run's plan again (_replanned).
        self.replanned = False
        if provenance is None:
            self.fail("missing", "no_dispatch")

    # The status named when several apply, weakest first; every reason is kept. A member
    # without evidence is missing whatever else is known about its dispatch.
    PRECEDENCE = ("complete", "unhealthy", "failed", "interrupted", "invalid", "missing")

    def fail(self, status, reason):
        order = self.PRECEDENCE
        if order.index(status) > order.index(self.status):
            self.status = status
        if reason not in self.reasons:
            self.reasons.append(reason)


def _bounded_json(path):
    with open(path, "rb") as stream:
        data = stream.read(limits.MAX_JSON_BYTES + 1)
    require(len(data) <= limits.MAX_JSON_BYTES, "%s exceeds its bound" % path)
    return loads_strict(data, path, limits.MAX_JSON_BYTES)


def _artifact_listing(evidence_dir):
    path = os.path.join(evidence_dir, ACTIONS_FILE)
    doc = _bounded_json(path)
    require(isinstance(doc, dict) and isinstance(doc.get("artifacts"), list)
            and type(doc.get("total_count")) is int,
            "%s: an Actions artifact listing {\"total_count\": N, \"artifacts\": [...]}" % path)
    # The fetcher writes only complete listings (ADR-0184): one shorter than its count is a
    # truncated listing whose omitted artifacts nobody could see.
    require(doc["total_count"] == len(doc["artifacts"]),
            "%s: lists %d of %d artifacts; an incomplete listing is never original evidence"
            % (path, len(doc["artifacts"]), doc["total_count"]))
    found, ids = [], set()
    for index, item in enumerate(doc["artifacts"]):
        at = "%s.artifacts[%d]" % (path, index)
        require(isinstance(item, dict) and isinstance(item.get("workflow_run"), dict),
                at + ": an artifact with its workflow_run")
        run = item["workflow_run"]
        for key, value in (("id", item.get("id")), ("workflow_run.id", run.get("id"))):
            require(type(value) is int and value > 0, at + ".%s: a positive integer" % key)
        require(isinstance(item.get("name"), str), at + ".name: a string")
        require(item["id"] not in ids, at + ".id: artifact %d is listed twice" % item["id"])
        ids.add(item["id"])
        # The artifact API names the run, never its attempt; the attempt comes from the lab
        # run ID the original attempt planned.
        require("run_attempt" not in run, at + ".workflow_run: the artifact API has no "
                "run_attempt; an attempt is read from the member's frozen plan")
        found.append({"id": item["id"], "name": item["name"], "run_id": run["id"],
                      "created_unix_ms": _unix_ms(item.get("created_at"), at + ".created_at"),
                      "expired": item.get("expired") is True})
    return found


def _unix_ms(value, where):
    require(isinstance(value, str) and re.match(
        r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$", value) is not None,
        where + ": an RFC 3339 UTC second")
    try:
        moment = datetime.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        # 2025-02-30T06:00:00Z has the shape but names no instant: invalid evidence.
        raise ContractError(where + ": %r is not a calendar instant" % value)
    return int((moment - datetime.datetime(1970, 1, 1)).total_seconds()) * 1000


def _run_identity(item, at):
    """The fields of a workflow-run object that bind it to its producer and member."""
    require(isinstance(item, dict), at + ": a workflow run")
    require(type(item.get("id")) is int and item["id"] > 0, at + ".id: a positive integer")
    require(type(item.get("run_attempt")) is int and item["run_attempt"] >= 1,
            at + ".run_attempt: a positive integer")
    require(type(item.get("run_number")) is int and 1 <= item["run_number"] < 2 ** 31,
            at + ".run_number: a positive integer")
    for key in ("display_title", "event", "path"):
        require(isinstance(item.get(key), str), at + ".%s: a string" % key)
    require(item.get("head_branch") is None or isinstance(item["head_branch"], str),
            at + ".head_branch: a string or null")
    # The commit whose workflow file GitHub ran: the producer a member's run is bound to.
    require(isinstance(item.get("head_sha"), str)
            and re.match(r"^[0-9a-f]{40}$", item["head_sha"]) is not None,
            at + ".head_sha: a full commit SHA")
    repository = item.get("repository")
    require(isinstance(repository, dict) and isinstance(repository.get("full_name"), str),
            at + ".repository.full_name: a string")
    return {"id": item["id"], "number": item["run_number"], "attempt": item["run_attempt"],
            "title": item["display_title"],
            "event": item["event"], "path": item["path"], "branch": item["head_branch"],
            "head_sha": item["head_sha"], "repository": repository["full_name"],
            "created_unix_ms": _unix_ms(item.get("created_at"), at + ".created_at")}


def _run_history(evidence_dir, cohort):
    """Every run of the dispatching workflow numbered after the cohort's anchor, from
    `runs.json`: the list-workflow-runs pages of every event, newest first, concatenated down to
    a run at or before the anchor (all of them when the anchor is 0). GitHub numbers a
    workflow's runs in order from 1 and never reuses a number, so the numbers after the anchor
    must run without a gap to the newest listed run: a gap is a deleted run, possibly a member's
    original, and nothing is judged from such a history. A run named for this cohort at or
    before the anchor means the anchor was not the workflow's number when the cohort was
    declared, and refuses too. (runs after the anchor by ID, history summary)."""
    path = os.path.join(evidence_dir, RUNS_FILE)
    doc = _bounded_json(path)
    require(isinstance(doc, dict) and type(doc.get("total_count")) is int
            and isinstance(doc.get("workflow_runs"), list),
            "%s: a workflow-run listing {\"total_count\": N, \"workflow_runs\": [...]}" % path)
    runs = doc["workflow_runs"]
    require(len(runs) <= doc["total_count"],
            "%s: lists %d runs of a workflow with %d" % (path, len(runs), doc["total_count"]))
    anchor = cohort["dispatch"]["after_run_number"]
    digest = canonical_digest(cohort)
    history, ids, numbers = {}, set(), set()
    for index, item in enumerate(runs):
        run = _run_identity(item, "%s.workflow_runs[%d]" % (path, index))
        require(run["id"] not in ids, "%s: run %d is listed twice" % (path, run["id"]))
        require(run["number"] not in numbers, "%s: run number %d is listed twice"
                % (path, run["number"]))
        ids.add(run["id"])
        numbers.add(run["number"])
        if run["number"] <= anchor:
            match = contracts.DISPATCH_TITLE.match(run["title"])
            require(match is None or match.group(1) != digest,
                    "%s: run %d (number %d) is named for this cohort but numbered at or before "
                    "its anchor %d; the anchor is not the workflow's run number when the cohort "
                    "was declared, so its history cannot show every dispatch"
                    % (path, run["id"], run["number"], anchor))
            continue
        history[run["id"]] = run
    after = sorted(run["number"] for run in history.values())
    latest = after[-1] if after else anchor
    # The numbers are distinct, so they run without a gap exactly when there are as many as the
    # span; the gaps are named from the listed numbers, never by enumerating the span.
    if latest - anchor != len(after):
        gaps = number_gaps(anchor, after)
        one = len(gaps) == 1 and ".." not in gaps[0]
        raise ContractError(
            "%s: run number%s %s after the cohort's anchor %d %s not listed; a deleted or "
            "unlisted run may be a member's original, so no dispatch history is judged from it"
            % (path, "" if one else "s", ", ".join(gaps[:GAPS_SHOWN])
               + (", ..." if len(gaps) > GAPS_SHOWN else ""), anchor, "is" if one else "are"))
    require(anchor == 0 or (numbers and min(numbers) <= anchor),
            "%s: the listing ends before the cohort's anchor run %d, so it cannot show that "
            "no run after it is missing" % (path, anchor))
    return history, {"after_run_number": anchor, "latest_run_number": latest,
                     "runs": len(after)}


GAPS_SHOWN = 8


def number_gaps(anchor, numbers):
    """The unlisted runs between `anchor` and the largest of the sorted distinct `numbers`, as at
    most GAPS_SHOWN + 1 ranges ("5" or "5..9"): work and storage follow the listed numbers, never
    the span between them, however large a listed number is."""
    gaps, previous = [], anchor
    for number in numbers:
        if number > previous + 1:
            first, last = previous + 1, number - 1
            gaps.append(str(first) if first == last else "%d..%d" % (first, last))
            if len(gaps) > GAPS_SHOWN:
                break
        previous = number
    return gaps


def _original_attempt(evidence_dir, run):
    """Attempt 1 of the original `run`, from `attempts/<run>.json`, and why it cannot serve:
    absent, unreadable or of another run or identity (`attempt_unavailable`), or not finished
    (`attempt_incomplete`)."""
    path = os.path.join(evidence_dir, ATTEMPTS_DIR, "%d.json" % run["id"])
    try:
        doc = _bounded_json(path)
        found = _run_identity(doc, path)
        require(found["attempt"] == 1, path + ": not attempt 1")
        require(dict(found, attempt=None, created_unix_ms=None)
                == dict(run, attempt=None, created_unix_ms=None),
                path + ": another run's attempt, or one of another number, title or producer")
        conclusion = doc.get("conclusion")
        require(conclusion is None or conclusion in contracts.ATTEMPT_CONCLUSIONS,
                path + ".conclusion: a workflow-run conclusion or null")
        attempt = {"started_unix_ms": _unix_ms(doc.get("run_started_at"),
                                               path + ".run_started_at"),
                   "updated_unix_ms": _unix_ms(doc.get("updated_at"), path + ".updated_at"),
                   "status": doc.get("status"), "conclusion": conclusion}
        require(attempt["started_unix_ms"] <= attempt["updated_unix_ms"],
                path + ": the attempt ends before it starts")
    except (ContractError, OSError):
        return None, "attempt_unavailable"
    if attempt["status"] != "completed":
        return attempt, "attempt_incomplete"
    if attempt["conclusion"] != "success":
        # Failed, cancelled, timed out, skipped, stale or no conclusion at all: whatever its
        # bundles hold, the original did not succeed and never qualifies or prepares anything.
        return attempt, "attempt_unsuccessful"
    return attempt, None


def _measuring_runner(evidence_dir, run, plan, attempt=1, path=None):
    """The hosted runner provenance of the original attempt's measuring jobs, from
    `jobs/<run>.json` (attempt 1's job listing; `path` and `attempt` name another attempt's, for
    a bridge run, ADR-0185), or None when it is absent, unreadable, incomplete, or any build or
    shard job of the plan ran elsewhere: another label, runner group or runner name, another run
    or attempt, or twice. The plan's environment must be a GitHub-hosted one; a dedicated or
    self-hosted runner never calibrates a hosted cohort."""
    environment = plan["profile"]["environment"]
    if environment.get("runner") != "github_hosted":
        return None
    label = environment["runner_label"]
    lab = plan["run_id"]
    expected = set(["build (%s)" % lab] + ["shard (%s %d)" % (lab, shard["index"])
                                          for shard in plan["shards"]])
    if path is None:
        path = os.path.join(evidence_dir, JOBS_DIR, "%d.json" % run["id"])
    try:
        doc = _bounded_json(path)
        require(isinstance(doc, dict) and type(doc.get("total_count")) is int
                and isinstance(doc.get("jobs"), list)
                and len(doc["jobs"]) == doc["total_count"],
                path + ": a complete job listing of the attempt")
        found = {}
        for item in doc["jobs"]:
            require(isinstance(item, dict) and isinstance(item.get("name"), str),
                    path + ": a job with its name")
            if item["name"] not in expected:
                continue
            require(item["name"] not in found, path + ": job %s is listed twice" % item["name"])
            found[item["name"]] = item
        require(set(found) == expected, path + ": the attempt's measuring jobs are not all "
                "listed")
        for item in found.values():
            require(item.get("run_id") == run["id"] and item.get("run_attempt") == attempt
                    and item.get("head_sha") == run["head_sha"]
                    and item.get("status") == "completed"
                    and item.get("labels") == [label]
                    and item.get("runner_group_name") == contracts.RUNNER_GROUP_HOSTED
                    and isinstance(item.get("runner_name"), str)
                    and HOSTED_RUNNER_NAME.match(item["runner_name"]) is not None,
                    path + ": job %s did not run on a hosted %s runner of attempt %d"
                    % (item["name"], label, attempt))
    except (ContractError, OSError):
        return None
    return {"label": label, "runner_group": contracts.RUNNER_GROUP_HOSTED,
            "jobs": len(expected)}


def _dispatches(cohort, history, refuse):
    """Each declared member's original run: the first run of the declared workflow dispatched
    under the member's run name, from the declared repository and branch. Later dispatches of
    a member are replacements; runs of another producer never bind a member."""
    digest = canonical_digest(cohort)
    trusted = cohort["dispatch"]
    producer = (trusted["event"], trusted["workflow_path"], trusted["branch"],
                trusted["repository"])
    declared = set(member["member_id"] for member in cohort["members"])
    by_member, roles = {}, {}
    for run in sorted(history.values(), key=lambda item: item["number"]):
        match = contracts.DISPATCH_TITLE.match(run["title"])
        if match is None:
            continue
        if match.group(1) != digest:
            # Another cohort's member: evidence of that cohort, never of this one.
            roles[run["id"]] = ("other_cohort", None)
            continue
        if (run["event"], run["path"], run["branch"], run["repository"]) != producer:
            roles[run["id"]] = ("foreign_dispatch", None)
            refuse("dispatch", None, run["id"], "foreign_dispatch",
                   "run %d is named for this cohort but is a %s of %s on %s of %s, not a %s of "
                   "%s on %s of %s" % ((run["id"], run["event"], run["path"], run["branch"],
                                         run["repository"]) + producer))
            continue
        if match.group(2) not in declared:
            roles[run["id"]] = ("unknown_member", None)
            refuse("dispatch", None, run["id"], "unknown_member",
                   "member %s is not declared" % match.group(2))
            continue
        by_member.setdefault(match.group(2), []).append(run)
    originals = {}
    for member_id, runs in sorted(by_member.items()):
        first = runs[0]
        require(all(first["created_unix_ms"] <= run["created_unix_ms"]
                    and first["id"] < run["id"] for run in runs[1:]),
                "member %s's dispatches are ambiguous: run %d is the lowest number but not the "
                "lowest ID or the earliest" % (member_id, first["id"]))
        originals[member_id] = first
        roles[first["id"]] = ("original", member_id)
        for later in runs[1:]:
            roles[later["id"]] = ("replacement", member_id)
            refuse("dispatch", None, later["id"], "replacement",
                   "member %s was first dispatched as Actions run %d; a later dispatch never "
                   "replaces it" % (member_id, first["id"]))
    return originals, roles


def original_lab_run(actions_run_id):
    """The lab run ID attempt 1 of a member's dispatch plans (ci.py: gh<run>_a<attempt>_primary)."""
    return "gh%d_a1_primary" % actions_run_id


def collect(cohort, evidence_dir, scratch, replan, producer):
    """Every declared member's original run, bound through the dispatch history, every refused
    dispatch and artifact, and the history's summary. `evidence_dir` holds `runs.json`,
    `attempts/<run>.json` and `jobs/<run>.json` for the originals, `actions.json` and one
    directory per downloaded artifact, named as the artifact. `replan(member, plan)` freezes the
    plan the trusted authority gives that member, which each retained plan must equal
    (`_judge_run`); `producer(run, plan)` is None when the original's workflow and scheduling
    code are the authority's own, else why not (`producer_check`)."""
    contracts.validate_cohort(cohort)
    declared = dict((member["member_id"], member) for member in cohort["members"])
    history, summary = _run_history(evidence_dir, cohort)
    listing = _artifact_listing(evidence_dir)
    refused = []

    def refuse(source, name, run_id, reason, detail):
        refused.append({"source": source, "artifact_name": None if name is None else name[:256],
                        "actions_run_id": run_id, "reason": reason, "detail": detail[:4096]})

    originals, roles = _dispatches(cohort, history, refuse)
    candidates = {}
    for item in sorted(listing, key=lambda entry: (entry["created_unix_ms"], entry["id"])):
        match = RUN_ARTIFACT.match(item["name"]) or SHARD_ARTIFACT.match(item["name"])
        if match is None:
            continue
        role = roles.get(item["run_id"])
        parts = LAB_RUN.match(match.group(1))
        if role is None:
            refuse("artifact", item["name"], item["run_id"], "unbound_run",
                   "Actions run %d is no dispatch of a member of this cohort" % item["run_id"])
        elif role[0] in ("other_cohort", "foreign_dispatch", "unknown_member"):
            refuse("artifact", item["name"], item["run_id"], role[0],
                   "Actions run %d was dispatched for no member of this cohort"
                   % item["run_id"])
        elif role[0] == "replacement":
            refuse("artifact", item["name"], item["run_id"], "replacement",
                   "Actions run %d is a later dispatch of member %s, never its original"
                   % (item["run_id"], role[1]))
        elif parts is None or int(parts.group(1)) != item["run_id"] \
                or parts.group(3) != "primary":
            refuse("artifact", item["name"], item["run_id"], "unbound_run",
                   "lab run %s was not planned by Actions run %d" % (match.group(1),
                                                                    item["run_id"]))
        elif int(parts.group(2)) != 1:
            refuse("artifact", item["name"], item["run_id"], "later_attempt",
                   "attempt %s of Actions run %d is a rerun; member %s's original is attempt 1"
                   % (parts.group(2), item["run_id"], role[1]))
        else:
            candidates.setdefault(item["name"], []).append(item)
    # Two listed artifacts of one expected name are ambiguous evidence: neither is chosen,
    # whichever the listing shows first, and the member they belong to is invalid.
    bundles, ambiguous = {}, set()
    for name, items in sorted(candidates.items()):
        if len(items) == 1:
            bundles[name] = items[0]
            continue
        ambiguous.add(name)
        for item in items:
            refuse("artifact", name, item["run_id"], "duplicate",
                   "artifact %d is one of %d listed under this name; neither is chosen"
                   % (item["id"], len(items)))
    runs = {}
    for member_id, member in sorted(declared.items()):
        original = originals.get(member_id)
        if original is None:
            runs[member_id] = MemberRun(member)
            continue
        lab = original_lab_run(original["id"])
        name = "af-perf-%s-run" % lab
        item = bundles.get(name)
        provenance = {"actions_run_id": original["id"], "run_number": original["number"],
                      "run_attempt": 1, "display_title": original["title"],
                      "head_sha": original["head_sha"],
                      "attempt_started_unix_ms": None, "attempt_updated_unix_ms": None,
                      "attempt_conclusion": None, "lab_run_id": lab, "artifact_name": name,
                      "artifact_id": None if item is None else item["id"],
                      "artifact_created_unix_ms": None if item is None
                      else item["created_unix_ms"], "plan_digest": None, "runner": None}
        run = runs[member_id] = MemberRun(member, provenance)
        attempt, why = _original_attempt(evidence_dir, original)
        if attempt is not None:
            run.attempt = attempt
            provenance.update(attempt_started_unix_ms=attempt["started_unix_ms"],
                              attempt_updated_unix_ms=attempt["updated_unix_ms"],
                              attempt_conclusion=attempt["conclusion"])
        if why == "attempt_unavailable":
            run.fail("invalid", why)
        elif why == "attempt_incomplete":
            run.fail("interrupted", why)
        elif why == "attempt_unsuccessful":
            run.fail("failed", why)
        if name in ambiguous:
            run.fail("invalid", "artifact_ambiguous")
            continue
        if item is None:
            run.fail("missing", "artifact_missing")
            continue
        if item["expired"]:
            run.fail("missing", "artifact_expired")
            continue
        try:
            doc, files, _ = transport.read_directory(os.path.join(evidence_dir, name))
            require(doc["content"] == "run_bundle" and doc["run_id"] == lab,
                    "the artifact holds %s of run %s" % (doc["content"], doc["run_id"]))
            plan_bytes = files.get("run.json")
            builds_bytes = files.get("builds.json")
            require(plan_bytes is not None and builds_bytes is not None,
                    "the run bundle has no run.json or builds.json")
            plan = contracts.read_json(plan_bytes, "run", "run.json")
            require(plan["run_id"] == lab, "the plan is of run %s" % plan["run_id"])
        except (ContractError, OSError):
            run.fail("missing", "artifact_unreadable")
            continue
        provenance["plan_digest"] = plan["plan_digest"]
        run.plan = plan
        record = plan["cohort"]
        if record is None or record["cohort_digest"] != canonical_digest(cohort) \
                or record["cohort_id"] != cohort["cohort_id"] \
                or record["member_id"] != member_id:
            # The member's own dispatch planned another member or cohort.
            run.fail("invalid", "wrong_plan")
            run.plan = None
            continue
        shard_names = ["af-perf-%s-shard-%d" % (lab, shard["index"]) for shard in plan["shards"]]
        if any(shard_name in ambiguous for shard_name in shard_names):
            run.fail("invalid", "artifact_ambiguous")
            run.plan = None
            continue
        if producer(original, plan) is not None:
            # Another workflow or scheduler produced this run: whatever it measured, it is no
            # evidence of the reviewed producer's cohort (ADR-0184).
            run.fail("invalid", "producer_unverified")
        provenance["runner"] = _measuring_runner(evidence_dir, original, plan)
        if provenance["runner"] is None:
            run.fail("invalid", "runner_unverified")
        run_dir = os.path.join(scratch, lab)
        shards_root = runner.prepare_run_dir(run_dir, plan_bytes, builds_bytes)
        for shard, shard_name in zip(plan["shards"], shard_names):
            path = os.path.join(evidence_dir, shard_name)
            entry = bundles.get(shard_name)
            if entry is None or entry["expired"] or not os.path.isdir(path):
                continue
            try:
                doc, files, _ = transport.read_directory(path)
                require(doc["run_id"] == lab and doc["content"] == "shard_bundle"
                        and doc["shard"] == shard["index"], "%s holds another bundle"
                        % shard_name)
            except (ContractError, OSError):
                continue
            target = os.path.join(shards_root, str(shard["index"]))
            for relative, data in sorted(files.items()):
                path = os.path.join(target, *relative.split("/"))
                os.makedirs(os.path.dirname(path), 0o700, exist_ok=True)
                with open(path, "xb") as stream:
                    stream.write(data)
        run.evaluation = compare.evaluate(run_dir)
        run.replanned = _replanned(replan, member, plan)
    return runs, refused, summary


def replanner(git, cohort, calibration=None):
    """The `replan` of `collect` over a repository: the member planned again from the trusted
    authority's own registry, instruments, fixtures and locked inputs, as its dispatch planned
    it (ci.py), with the retained plan's run ID, control revision, shard count and frozen
    build. A slowdown member is planned with the sealed `calibration` readiness judges by."""
    from . import planning

    def replan(member, plan):
        build = plan["profile"]["build"]
        level = contracts.member_level(member)
        request = planning.Request(
            trigger="manual", tier=cohort["tier"], scope="calibration",
            base=member["source_sha"], head=member["source_sha"],
            control=plan["resolved"]["control_sha"], authority=cohort["authority_sha"],
            run_id=plan["run_id"], shards=len(plan["shards"]),
            environment=cohort["environment_id"], cohort=cohort, member=member["member_id"],
            calibration=calibration if level not in (None, "preparation") else None,
            build_image=build["image"] if build["image_source"] == "configured" else None,
            trusted_host_build=build["isolation"] == "trusted_host")
        return planning.plan(git, request)
    return replan


# The producer of every member run (ADR-0184): the dispatching workflow GitHub ran, read at the
# run's own head commit, and the scheduling code its jobs ran from the plan's trusted control
# revision. Both must be byte-identical to the cohort authority's, whatever else those commits
# hold: a cohort declaration commit descends from the authority, and neither commit need equal
# the authority or each other.
PRODUCER_WORKFLOW_PATHS = (contracts.DISPATCH_WORKFLOW,)
PRODUCER_SCHEDULER_PATHS = ("scripts/performance",)


def producer_check(git, cohort):
    """The `producer` of `collect` over a repository: None when the original run's workflow file
    at its head commit and the scheduling code at its plan's control revision are exactly the
    cohort authority's; otherwise why not, including a commit the repository cannot show."""
    authority = cohort["authority_sha"]

    def check(run, plan):
        sides = ((run["head_sha"], PRODUCER_WORKFLOW_PATHS, "the workflow at head commit"),
                 (plan["resolved"]["control_sha"], PRODUCER_SCHEDULER_PATHS,
                  "the scheduling code at control revision"))
        try:
            require(git.read_file(authority, contracts.DISPATCH_WORKFLOW) is not None,
                    "the authority %s has no %s" % (authority, contracts.DISPATCH_WORKFLOW))
            for sha, paths, what in sides:
                require(git.resolve(sha, what) == sha, "%s %s is not a full commit"
                        % (what, sha))
                changed = git.changed_paths(authority, sha, paths)
                if changed:
                    return "%s %s differs from the authority's: %s" % (
                        what, sha[:12], ", ".join(changed[:8]))
        except (ContractError, gitrefs.Unavailable) as error:
            return "the producer cannot be shown: %s" % error
        return None
    return check


def _frozen_identity(plan):
    """What a member's plan freezes, beside the selector text it was given and its digest."""
    return canonical_digest(dict((key, value) for key, value in plan.items()
                                 if key not in ("selectors", "plan_digest")))


def _replanned(replan, member, plan):
    """Whether the trusted authority, planning this member again with the retained plan's
    run ID, control and shard count, freezes exactly the retained plan."""
    try:
        expected = replan(member, plan)
    except (ContractError, gitrefs.Unavailable):
        return False
    return _frozen_identity(expected) == _frozen_identity(plan)


def _cohort_mismatch(cohort, plan):
    """The first way `plan` departs from what the cohort froze, or None."""
    declared = [(case["instance_id"], case["criticality"], case["pairs"])
                for case in cohort["cases"]]
    planned = [(case["instance_id"], case["criticality"], case["pairs"])
               for case in plan["cases"]]
    health = plan["health"]
    checks = (
        (planned == declared, "its cases, criticality or N"),
        (plan["verdict_policy"]["family_size"] == cohort["family_size"], "its family size"),
        (plan["authority"]["policy_digest"] == cohort["policy_digest"], "its policy digest"),
        (plan["authority"]["mode"] == "proposed", "its authority mode"),
        (plan["resolved"]["authority_sha"] == cohort["authority_sha"], "its authority"),
        (health == cohort["health"], "its health rule"),
        ((plan["tier"], plan["profile"]["environment_id"], plan["profile"]["recipe_id"])
         == (cohort["tier"], cohort["environment_id"], cohort["recipe_id"]),
         "its tier, environment or recipe"),
    )
    for agrees, what in checks:
        if not agrees:
            return what
    return None


def _judge_run(cohort, calibration, run, trials_after):
    """Set one original run's status, reasons and CPU group from its recomputed evidence."""
    member, plan, verdict = run.member, run.plan, run.evaluation.verdict
    if run.attempt is not None:
        start = start_unix_ms(cohort) + member["day"] * DAY_MS
        if not start <= run.attempt["started_unix_ms"] < start + DAY_MS:
            run.fail("invalid", "off_schedule")
        if member["kind"] in ("injection", "power") \
                and run.attempt["started_unix_ms"] <= trials_after:
            run.fail("invalid", "before_calibration")
    expected = None
    try:
        expected = plan_injection(cohort, calibration if contracts.member_level(member)
                                  not in (None, "preparation") else None, member,
                                  [case["instance_id"] for case in plan["cases"]])
    except ContractError:
        expected = False
    record = plan["cohort"]
    if (record["member_kind"] != member["kind"] or plan["seed"] != member["seed"]
            or record["injection"] != expected or plan["scope"] != "calibration"
            or plan["resolved"]["baseline_sha"] != member["source_sha"]
            or plan["resolved"]["candidate_sha"] != member["source_sha"]
            or _cohort_mismatch(cohort, plan) is not None or not run.replanned):
        run.fail("invalid", "wrong_plan")
    if verdict["provenance"] != "measured":
        # Readiness is empirical: synthetic or absent measurements never count.
        run.fail("invalid", "not_measured")
    run.cpu_groups = {}
    observed = {}
    for entry, evidence in zip(verdict["shards"], run.evaluation.evidence):
        if entry["status"] is None:
            run.fail("interrupted", "shard_missing")
        elif entry["status"] == "interrupted":
            run.fail("interrupted", "shard_interrupted")
        elif entry["status"] == "failed":
            run.fail("failed", "shard_failed")
        if evidence.doc is not None:
            group = shard_group(cohort, evidence.doc)
            observed[evidence.doc["shard"]] = group
            if group is None:
                run.fail("invalid", "cpu_group_mismatch")
    # A wrong plan remains an invalid original, including when it substitutes a case.
    # Report observations for the declared cases only; an absent case stays unobserved.
    owners = {case["instance_id"]: case["shard"] for case in plan["cases"]}
    for case in cohort["cases"]:
        ident = case["instance_id"]
        run.cpu_groups[ident] = observed.get(owners.get(ident))
    if any(entry["status"] != "healthy" for entry in verdict["health"]):
        run.fail("unhealthy", "health_unstable")
    if verdict["aggregate"]["coverage"]["missing_records"]:
        run.fail("interrupted", "records_incomplete")
    if record["injection"]:
        prepared = {group["cpu_group"] for group in record["injection"]["groups"]}
        if any(group is not None and group not in prepared
               for group in observed.values()):
            run.fail("invalid", "unprepared_cpu_group")


def _measured(run, instance_id, side, key):
    """Every measured value of `key` on `side` of one case, None when a planned record lacks
    it: a numerical summary of incomplete evidence is not computed."""
    case = [item for item in run.plan["cases"] if item["instance_id"] == instance_id][0]
    evidence = run.evaluation.evidence[case["shard"]]
    values = []
    for pair in range(1, case["pairs"] + 1):
        found = [record for record in evidence.records.values()
                 if record["instance_id"] == instance_id and record["phase"] == "measured"
                 and record["pair"] == pair and record["side"] == side]
        if len(found) != 1 or found[0]["status"] != "ok" or key not in found[0]["metrics"]:
            return None
        values.append(Fraction(found[0]["metrics"][key]["value"]))
    return values


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


# Preparation -----------------------------------------------------------------------------

def prepare(cohort, runs, tolerances):
    """Seal the calibration parameters from the A/A and preparation originals in `runs`.
    `tolerances` maps each instance to its plan's (relative, absolute) tolerance decimals."""
    contracts.validate_cohort(cohort)
    _judge_all(cohort, None, runs, kinds=("aa", "preparation"))
    groups = []
    iterations = cohort["preparation"]["iterations"]
    for case in cohort["cases"]:
        ident = case["instance_id"]
        relative, absolute = tolerances[ident]
        for allowed in sorted(cohort["allowed_cpu_groups"], key=lambda item: item["group_id"]):
            gid = allowed["group_id"]
            baseline, costs = [], []
            for run in runs.values():
                if run.status != "complete" or run.cpu_groups.get(ident) != gid:
                    continue
                if run.member["kind"] == "aa":
                    primary = _primary(run, ident)
                    baseline += _measured(run, ident, schedule.BASELINE, primary) or []
                elif run.member["kind"] == "preparation":
                    injected = _measured(run, ident, schedule.CANDIDATE, INJECTED_METRIC)
                    costs += [value * 1000000 / iterations for value in injected or []]
            entry = {"instance_id": ident, "cpu_group": gid, "relative": relative,
                     "absolute": absolute, "a0": None, "a0_samples": len(baseline),
                     "a0_unavailable": None, "efficiency": None, "power_target": None,
                     "loop_ns_per_iteration": None, "loop_samples": 0, "iterations": None}
            a0 = median(baseline) if baseline else None
            if a0 is None or a0 <= 0:
                entry["a0_unavailable"] = "no_aa_samples" if a0 is None else "zero_a0"
                groups.append(entry)
                continue
            eff = contracts.efficiency(Fraction(relative), Fraction(absolute), a0)
            entry.update(a0=format_rational(a0), efficiency=format_rational(eff),
                         power_target=format_rational(contracts.power_target(eff)))
            if costs and median(costs) > 0:
                cost = median(costs)
                entry.update(loop_ns_per_iteration=format_rational(cost),
                             loop_samples=len(costs))
                counts = dict((level, contracts.loop_iterations(
                    contracts.target_ms(entry, level), cost)) for level in contracts.LOOP_LEVELS)
                if all(count is not None for count in counts.values()):
                    entry["iterations"] = counts
            groups.append(entry)
    doc = {"schema": contracts.CALIBRATION_SCHEMA, "cohort_id": cohort["cohort_id"],
           "cohort_digest": canonical_digest(cohort),
           "a0_member_ids": sorted(member["member_id"] for member in cohort["members"]
                                   if member["kind"] == "aa"),
           "preparation_member_ids": sorted(member["member_id"] for member in cohort["members"]
                                            if member["kind"] == "preparation"),
           "preparation_iterations": iterations,
           "groups": sorted(groups, key=lambda item: (item["instance_id"], item["cpu_group"]))}
    return contracts.validate_calibration(doc, "calibration-parameters", cohort=cohort)


def _primary(run, instance_id):
    case = [item for item in run.plan["cases"] if item["instance_id"] == instance_id][0]
    return case["primary_metric"]["key"]


def tolerances_of(runs, cohort):
    """Each cohort case's (relative, absolute) tolerance, from the members' frozen plans, which
    the planner took from the cohort's registry; every plan must agree."""
    found = {}
    for run in runs.values():
        if run.plan is None or run.evaluation is None:
            continue
        for case in run.plan["cases"]:
            value = (case["tolerance"]["relative"], case["tolerance"]["absolute"])
            require(found.setdefault(case["instance_id"], value) == value,
                    "members' plans disagree on %s's tolerance" % case["instance_id"])
    missing = [case["instance_id"] for case in cohort["cases"] if case["instance_id"] not in found]
    require(not missing, "no member's plan names the tolerance of " + ", ".join(missing))
    return found


def _judge_all(cohort, calibration, runs, kinds):
    """Judge every evaluated original of `kinds`. Slowdown trials must start after every A/A
    and preparation original's attempt ended, so the parameters they name were sealed from
    completed evidence; the attempts' own records give both instants."""
    latest = max([run.attempt["updated_unix_ms"] for run in runs.values()
                  if run.attempt is not None and run.member["kind"] in ("aa", "preparation")]
                 or [0])
    for run in runs.values():
        if run.evaluation is not None and run.member["kind"] in kinds:
            _judge_run(cohort, calibration, run, latest)


# Readiness -------------------------------------------------------------------------------

def recompute_parameters(cohort, calibration, runs):
    """The parameters the A/A and preparation originals in `runs` give, which must be exactly
    the sealed `calibration` every trial names: A0, sample counts, loop cost, tolerances and
    every loop count. A document that differs, however self-consistent, is refused before any
    trial is judged; the recomputed values are never substituted into trials already run."""
    contracts.validate_calibration(calibration, "calibration-parameters", cohort=cohort)
    recomputed = prepare(cohort, runs, tolerances_of(runs, cohort))
    if canonical_digest(recomputed) != canonical_digest(calibration):
        differing = sorted(
            "%s/%s" % (group["instance_id"], group["cpu_group"])
            for group in calibration["groups"]
            if group != contracts.calibration_group(recomputed, group["instance_id"],
                                                    group["cpu_group"]))
        raise ContractError(
            "the sealed calibration parameters %s are not those the cohort's A/A and "
            "preparation originals give (%s)%s; trials are judged only by the parameters their "
            "originals freeze, and are never retuned" % (
                canonical_digest(calibration), canonical_digest(recomputed),
                ": " + ", ".join(differing[:8]) if differing else ""))
    return recomputed


def readiness(cohort, calibration, runs, refused, history):
    """The af.perf-readiness/6 document of a cohort's recomputed evidence."""
    recomputed = recompute_parameters(cohort, calibration, runs)
    _judge_all(cohort, calibration, runs, kinds=contracts.MEMBER_KINDS)
    members = []
    for declared in cohort["members"]:
        run = runs[declared["member_id"]]
        entry = {"member_id": declared["member_id"], "kind": declared["kind"],
                 "status": run.status, "reasons": sorted(run.reasons),
                 "provenance": run.provenance, "cpu_groups": dict(run.cpu_groups),
                 "verdict_sha256": None, "cases": {}, "qualifies": False}
        if run.status == "complete":
            verdict = run.evaluation.verdict
            entry["verdict_sha256"] = sha256_bytes(contracts.document_bytes(verdict))
            level = contracts.member_level(declared)
            for case in verdict["cases"]:
                ident = case["instance_id"]
                record = {"classification": case["status"], "injected_median_ms": None,
                          "target_ms": None, "injection_valid": None}
                if level not in (None, "preparation"):
                    group = contracts.calibration_group(
                        calibration, ident, run.cpu_groups.get(ident))
                    target = (contracts.target_ms(group, level)
                              if group is not None and group["a0"] is not None else None)
                    injected = _measured(run, ident, schedule.CANDIDATE, INJECTED_METRIC)
                    value = median(injected) if injected else None
                    record.update(
                        injected_median_ms=None if value is None else format_rational(value),
                        target_ms=None if target is None else format_rational(target),
                        injection_valid=contracts.injection_valid(value, target))
                entry["cases"][ident] = record
        members.append(entry)
    tallies, groups, qualifies, ready = contracts.compute_readiness(cohort, calibration, members)
    for entry in members:
        entry["qualifies"] = qualifies[entry["member_id"]]
    doc = {"schema": contracts.READINESS_SCHEMA, "cohort_id": cohort["cohort_id"],
           "cohort_digest": canonical_digest(cohort),
           "calibration_digest": canonical_digest(calibration),
           "recomputed_calibration_digest": canonical_digest(recomputed),
           "history": dict(history), "members": members,
           "refused_evidence": refused[:256], "tallies": tallies, "groups": groups,
           "ready": ready, "claim": contracts.READINESS_CLAIM}
    return contracts.validate_readiness(doc, "readiness", cohort=cohort, calibration=calibration)


# What an activation compares: the membership, classifications, doses, denominators and decision.
# The history grows and later refusals accrue after readiness is written, without changing them.
JUDGMENT_EXCLUDED = ("history", "refused_evidence")


def judgment(doc):
    """The part of a readiness document a later recomputation must reproduce exactly."""
    return dict((key, value) for key, value in doc.items() if key not in JUDGMENT_EXCLUDED)


def original_runs(doc):
    """The Actions run IDs of every dispatched member's original, ascending."""
    return sorted(member["provenance"]["actions_run_id"] for member in doc["members"]
                  if member["provenance"] is not None)


def summary_markdown(doc):
    """A compact, honest summary: the decision, whole-trial power tally, A/A tally and that the
    finite cohort is no population power claim."""
    tallies = doc["tallies"]
    lines = ["### AF calibration readiness: %s" % ("ready" if doc["ready"] else "not ready"), "",
             "- A/A within tolerance in every critical case: %d of %d"
             % (tallies["aa_within_tolerance"], tallies["aa_members"]),
             "- Power trials valid in every critical case: %d of %d (at least %d required)"
             % (tallies["power_qualifying"], tallies["power_members"],
                contracts.POWER_DETECTIONS_REQUIRED),
             "- Refused evidence: %d dispatch(es) and artifact(s)" % len(
                 doc["refused_evidence"]), "",
             "| Member | Kind | Status | Observed groups | Counts |",
             "| --- | --- | --- | --- | --- |"]
    for member in doc["members"]:
        observed = sorted({group or "unobserved" for group in member["cpu_groups"].values()})
        lines.append("| %s | %s | %s | %s | %s |" % (
            member["member_id"], member["kind"],
            member["status"] if not member["reasons"] else "%s (%s)" % (
                member["status"], ", ".join(member["reasons"])),
            ", ".join(observed) or "-", "yes" if member["qualifies"] else "no"))
    lines += ["", doc["claim"] + ".", ""]
    return "\n".join(lines)


def write_new(path, doc, contract):
    data = contracts.write_bytes(doc, contract)
    with open(path, "xb") as stream:
        stream.write(data)
    return data

