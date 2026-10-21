"""Continuous scheduling of the laboratory in GitHub Actions (package p6a, ADR-0177).

`schedule` turns one Actions event into an `af.perf-schedule/1` document and the frozen plans it
names. It runs from the trusted control revision's own checkout, never from candidate code, and
resolves every selector exactly once, to full SHAs:

- **pull_request** to `main`: the exact PR merge commit against its first parent, which is the
  control and must equal the event's base SHA; the PR head, merge base and number are frozen in
  the plan. A base branch below `af-gate/` is a Remote Check gate pull request and is skipped,
  and so is any base but `main`. A PR is candidate-controlled: it is never trusted and runs only
  on ephemeral hosted runners, whether or not it comes from a fork.
- **push** to `main`: the pushed SHA against its first parent.
- **schedule**: the default branch's tip against the highest stable release tag reachable from it.
- **a confirmed release claim** (`release`, ADR-0180): the handoff of a completed upstream
  Release run, or an explicit remeasurement, that `perflab.release` verified and claimed first.
  Its published tag is compared with the highest smaller stable release (a prerelease's
  predecessor may be a prerelease), the tag that names the claimed SHA exactly; a
  remeasurement is a manual comparison of the same two commits naming the parent run it
  repeats. Nothing else of the event chooses what is measured.
- **workflow_dispatch**: explicit full SHAs or fully qualified refs only; a short or bare name
  that could be ambiguous is refused, never guessed. A tag resolves as given. A branch
  `refs/heads/NAME` is read where the pinned `actions/checkout` with full history put it: its
  fetch stores every branch as `refs/remotes/origin/NAME` and creates only the event's own branch
  locally, so the branch is the fetched remote-tracking ref, and a local branch of that name
  must name the same commit or the selector is refused as ambiguous (ADR-0178). The workflow's
  control step applies the same rule before any harness code is imported.

The primary comparison is a full-scope plan of the activated authority the control revision's
`fixtures/performance/activation.toml` binds. Before any activation exists it is unavailable,
never fabricated, and an advisory bootstrap under the control's own registry runs instead. A pull
request that changes the registry's policy also gets the two non-required proposed-policy
bootstrap comparisons, (base, merge) and (merge, merge), under the same untrusted containment.
Missing parents, unresolved refs, an event base that disagrees with the merge, an unsupported
parent shape or a missing build image are explicit unavailable or refused results; nothing
selects a more convenient baseline. Enforcement is always advisory here.
"""
import json
import os
import re

from . import contracts, gitrefs, images, limits, planning
from .values import ContractError, canonical_digest, require, sha256_bytes

MAIN = "main"
GATE_PREFIX = "af-gate/"
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
QUALIFIED_REF = re.compile(r"^refs/(heads|tags)/[A-Za-z0-9][A-Za-z0-9._/-]{0,200}$")
# Where the checkout's fetch (`+refs/heads/*:refs/remotes/origin/*`) keeps every branch.
FETCHED_BRANCHES = "refs/remotes/origin/"
TRUSTED_EVENTS = contracts.TRUSTED_EVENTS
DISPATCH_SCOPES = ("full", "partial_scope", "bootstrap", "calibration")
# Why a primary comparison may be unavailable although its event resolved as its rule requires.
RESOLVED_REASONS = ("no_active_authority", "authority_unreachable", "no_build_image")


class Settings(object):
    """What the workflow's trusted configuration supplies: each Linux recipe's build image
    pinned by a registry digest (never a machine-local image ID a hosted runner cannot pull),
    and the dedicated runner label a trusted event may request."""

    def __init__(self, build_images=None, dedicated_label=None, executor="ephemeral_hosted"):
        self.build_images = dict(build_images or {})
        for recipe, reference in self.build_images.items():
            images.canonical(reference, "build image of " + recipe)
            require(not images.is_image_id(reference),
                    "build image of %s: %s is a machine-local image ID; a hosted runner pulls "
                    "a registry reference NAME@sha256:..." % (recipe, reference))
        self.dedicated_label = dedicated_label or None
        require(executor in contracts.EXECUTORS, "--executor: one of "
                + ", ".join(contracts.EXECUTORS))
        self.executor = executor


class Event(object):
    """One Actions event as the workflow passes it: its name, the payload GitHub wrote to
    GITHUB_EVENT_PATH, GITHUB_SHA, GITHUB_REF, the repository, run ID and attempt, and a
    dispatch's inputs."""

    def __init__(self, name, payload, sha, ref, repository, run_id, run_attempt, inputs=None):
        self.name = name
        self.payload = payload if isinstance(payload, dict) else {}
        self.sha = sha
        self.ref = ref or ""
        self.repository = repository
        self.run_id = run_id
        self.run_attempt = run_attempt
        self.inputs = inputs if inputs is not None else (self.payload.get("inputs") or {})


def _text(value, what):
    require(isinstance(value, str) and value, "the event has no %s" % what)
    return value


def _sha(value, what):
    require(isinstance(value, str) and FULL_SHA.match(value) is not None,
            "the event's %s is not a full commit SHA" % what)
    return value


def _dispatch_ref(git, value, what):
    """A dispatch input: a full SHA or a fully qualified ref, resolved once; anything a short or
    bare name could make ambiguous is refused."""
    if value is None or value == "":
        return None
    require(isinstance(value, str) and (FULL_SHA.match(value) or QUALIFIED_REF.match(value)),
            "%s: %r is not a full commit SHA or a fully qualified refs/heads/ or refs/tags/ "
            "ref; an abbreviated or bare name may be ambiguous and is never guessed"
            % (what, value))
    if value.startswith("refs/heads/"):
        return _branch(git, value, what)
    return git.resolve(value, what)


def _branch(git, value, what):
    """The commit of the branch `refs/heads/NAME` in the checkout's layout: the fetched
    `refs/remotes/origin/NAME`, or the local branch of that name (only the event's own exists
    there), each read exactly; both, when both exist, must name one commit."""
    name = value[len("refs/heads/"):]
    found = {}
    for ref in (value, FETCHED_BRANCHES + name):
        sha = git.exact_ref(ref, what)
        if sha is not None:
            found[ref] = sha
    if not found:
        raise gitrefs.Unavailable("unresolved_ref", "%s: neither %s nor the fetched %s%s "
                                  "names a commit" % (what, value, FETCHED_BRANCHES, name))
    if len(set(found.values())) > 1:
        raise gitrefs.Unavailable("ambiguous_ref", "%s: %s is ambiguous here: %s; it is never "
                                  "guessed" % (what, value, ", ".join(
                                      "%s names %s" % item for item in sorted(found.items()))))
    return next(iter(found.values()))


class _Comparisons(object):
    def __init__(self, git, event, settings):
        self.git = git
        self.event = event
        self.settings = settings
        self.items = []
        self.plans = {}

    def run_id(self, role):
        return "gh%d_a%d_%s" % (self.event.run_id, self.event.run_attempt, role)

    def add(self, role, request_values, runner_label):
        """Freeze one comparison from the trusted control's planner, or record exactly why it
        is unavailable or refused."""
        values = dict(request_values)
        values["run_id"] = self.run_id(role)
        record = {"role": role, "run_id": values["run_id"], "status": "planned",
                  "reason": None, "detail": "", "plan_digest": None, "plan_sha256": None,
                  "required_check_eligible": False, "harness_changed": None, "shards": 0,
                  "concurrent_jobs": 0, "runner_label": None,
                  "request": {"trigger": values["trigger"], "tier": values["tier"],
                              "scope": values["scope"], "base": values.get("base"),
                              "head": values["head"], "control": values.get("control"),
                              "authority": values.get("authority"),
                              "cases": list(values.get("cases") or []),
                              "shards": values.get("shards"), "pairs": values.get("pairs"),
                              "pr_number": values.get("pr_number"),
                              "pr_base_sha": values.get("pr_base_sha"),
                              "pr_head_sha": values.get("pr_head_sha"),
                              "build_image": None}}
        try:
            try:
                image = self.build_image(values)
            except gitrefs.Unavailable as missing:
                # The event and authority decide first: a missing image is the reason only
                # when nothing else refuses the plan. This dry plan is never written or used.
                planning.plan(self.git, planning.Request(trusted_host_build=True, **values))
                raise missing
            record["request"]["build_image"] = image
            values["build_image"] = image
            plan = planning.plan(self.git, planning.Request(**values))
        except gitrefs.Unavailable as error:
            record.update(status="unavailable", reason=error.reason, detail=str(error)[:4096])
        except ContractError as error:
            record.update(status="refused", reason="invalid_plan", detail=str(error)[:4096])
        else:
            data = contracts.write_bytes(plan, "run")
            self.plans[plan["run_id"]] = data
            record.update(plan_digest=plan["plan_digest"], plan_sha256=sha256_bytes(data),
                          required_check_eligible=plan["required_check_eligible"],
                          harness_changed=plan["harness"]["changed"],
                          shards=len(plan["shards"]),
                          concurrent_jobs=plan["limits"]["concurrent_jobs"],
                          runner_label=runner_label(plan))
            record["detail"] = ("%s plan of %d instances, %d shards; authority %s (%s)%s" % (
                plan["scope"], len(plan["cases"]), len(plan["shards"]),
                plan["resolved"]["authority_sha"], plan["authority"]["mode"],
                "; the candidate changes the harness, which measures nothing of its own"
                if plan["harness"]["changed"] else ""))[:4096]
        self.items.append(record)
        return record

    def build_image(self, values):
        """The configured registry image of the recipe the plan will freeze, read from the
        registry that will measure it; None for a recipe that pins its own or builds natively.
        A Linux recipe without either is unavailable: CI never freezes a trusted host build."""
        trigger = values["trigger"]
        if values.get("authority"):
            source = values["authority"]
        elif trigger == "pr":
            source = values["pr_base_sha"]
        elif trigger == "main_push":
            source = gitrefs.first_parent(self.git, self.git.resolve(values["head"], "--head"),
                                          "pushed commit")
        elif trigger in ("nightly", "release"):
            source = self.git.resolve(values["head"], "--head")
        else:
            source = values.get("control") or values["base"]
        suite = planning.read_suite(self.git, source)
        tier = suite.doc["tiers"][values["tier"]]
        recipe_id = tier["profiles"][0]["recipe"]
        recipe = suite.doc["recipes"][recipe_id]
        if recipe["os"] != "linux" or recipe.get("container_image") is not None:
            return None
        image = self.settings.build_images.get(recipe_id)
        if image is None:
            raise gitrefs.Unavailable("no_build_image", "recipe %s pins no build image and none "
                                      "is configured for it; continuous runs build only in a "
                                      "registry image pinned by digest" % recipe_id)
        return image


def _trust(event, settings):
    trusted = event.name in TRUSTED_EVENTS
    detail = ("a %s event is trusted: it runs reviewed code from %s" % (event.name, MAIN)
              if trusted else "a pull request is candidate-controlled: it runs only on "
              "ephemeral hosted runners, with no secret and read-only permissions")
    executor = "ephemeral_hosted"
    if settings.executor == "dedicated":
        if not trusted:
            detail += "; dedicated capacity was requested and refused for this untrusted event"
        elif settings.dedicated_label is None:
            detail += "; dedicated capacity was requested but no dedicated runner label is " \
                      "configured, so it runs on ephemeral hosted runners"
        else:
            executor = "dedicated"
            detail += "; it runs on the configured dedicated runner"
    return {"trusted": trusted, "executor": executor, "detail": detail[:4096]}


def _skip(doc, reason, detail):
    doc.update(status="skipped", reason=reason, detail=detail[:4096])
    return doc


def schedule(git, event, settings, release=None):
    """(af.perf-schedule/2 document, {run ID: plan bytes}) of one event; `release` is the
    confirmed `af.perf-release-claim/1` this run wrote, when it measures a release."""
    require(isinstance(event.run_id, int) and event.run_id > 0
            and isinstance(event.run_attempt, int) and event.run_attempt > 0,
            "the Actions run ID and attempt are positive integers")
    doc = {"schema": contracts.SCHEDULE_SCHEMA,
           "actions": {"repository": event.repository, "run_id": event.run_id,
                       "run_attempt": event.run_attempt, "event": event.name[:64],
                       "ref": event.ref[:512], "sha": _sha(event.sha, "GITHUB_SHA")},
           "status": "scheduled", "reason": None, "detail": "",
           "pull_request": None, "trust": _trust(event, settings), "enforcement": "advisory",
           "policy_changed": None, "comparisons": [], "matrix": [], "concurrent_jobs": 0,
           "retention_days": 14 if event.name == "pull_request" else 90, "release": None}
    comparisons = _Comparisons(git, event, settings)
    trust = doc["trust"]

    def runner_label(plan):
        if trust["executor"] == "dedicated":
            return settings.dedicated_label
        return plan["profile"]["environment"]["runner_label"]

    if release is not None:
        _release(git, event, release, comparisons, runner_label, doc)
    elif event.name == "pull_request":
        pr = event.payload.get("pull_request")
        require(isinstance(pr, dict), "a pull_request event without its pull request")
        base, head = pr.get("base") or {}, pr.get("head") or {}
        head_repo = (head.get("repo") or {}).get("full_name") or ""
        record = {"number": pr.get("number"), "base_ref": _text(base.get("ref"), "base ref"),
                  "base_sha": _sha(base.get("sha"), "base SHA"),
                  "head_sha": _sha(head.get("sha"), "head SHA"),
                  "head_repository": head_repo[:201], "fork": head_repo != event.repository,
                  "merge_sha": event.sha}
        require(isinstance(record["number"], int) and record["number"] > 0,
                "the pull request has no number")
        doc["pull_request"] = record
        if record["base_ref"].startswith(GATE_PREFIX):
            return _skip(doc, "gate_branch", "base %s is a Remote Check gate branch; gate pull "
                         "requests are never measured" % record["base_ref"]), {}
        if record["base_ref"] != MAIN:
            return _skip(doc, "not_main", "only pull requests to %s are measured; this one "
                         "targets %s" % (MAIN, record["base_ref"])), {}
        pr_values = {"trigger": "pr", "tier": "quick", "head": record["merge_sha"],
                     "pr_number": record["number"], "pr_base_sha": record["base_sha"],
                     "pr_head_sha": record["head_sha"]}
        primary = comparisons.add("primary", dict(pr_values, scope="full"), runner_label)
        if primary["status"] == "unavailable" and primary["reason"] == "no_active_authority":
            comparisons.add("control_bootstrap", dict(pr_values, scope="bootstrap"),
                            runner_label)
        # Only an event whose revisions resolved as its rule requires measures a proposed
        # policy; a refused or unsupported event shape measures nothing else instead.
        if primary["status"] == "planned" or primary["reason"] in RESOLVED_REASONS:
            changed = _policy_changed(git, record["base_sha"], record["merge_sha"])
            doc["policy_changed"] = changed
            if changed:
                comparisons.add("proposed_bootstrap_base",
                                dict(pr_values, scope="bootstrap",
                                     authority=record["merge_sha"]), runner_label)
                comparisons.add("proposed_bootstrap_merge", {
                    "trigger": "manual", "tier": "quick", "scope": "bootstrap",
                    "base": record["merge_sha"], "head": record["merge_sha"],
                    "control": record["base_sha"], "authority": record["merge_sha"]},
                    runner_label)
    elif event.name == "push":
        if event.ref != "refs/heads/" + MAIN:
            return _skip(doc, "not_main", "only pushes to %s are measured; this one is %s"
                         % (MAIN, event.ref)), {}
        values = {"trigger": "main_push", "tier": "quick", "head": event.sha}
        primary = comparisons.add("primary", dict(values, scope="full"), runner_label)
        if primary["status"] == "unavailable" and primary["reason"] == "no_active_authority":
            comparisons.add("control_bootstrap", dict(values, scope="bootstrap"), runner_label)
    elif event.name == "schedule":
        values = {"trigger": "nightly", "tier": "nightly", "head": event.sha}
        primary = comparisons.add("primary", dict(values, scope="full"), runner_label)
        if primary["status"] == "unavailable" and primary["reason"] == "no_active_authority":
            comparisons.add("control_bootstrap", dict(values, scope="bootstrap"), runner_label)
    elif event.name == "workflow_dispatch":
        _dispatch(git, event, comparisons, runner_label, settings)
    else:
        # pull_request_target, a workflow_run without its confirmed release claim and anything
        # else never execute a candidate here.
        return _skip(doc, "unsupported_event", "event %s is not one the laboratory schedules; "
                     "a pull_request_target never executes candidate code"
                     % event.name), {}
    doc["comparisons"] = comparisons.items
    planned = [item for item in comparisons.items if item["status"] == "planned"]
    doc["matrix"] = [{"run_id": item["run_id"], "shard": shard}
                     for item in sorted(planned, key=lambda entry: entry["run_id"])
                     for shard in range(item["shards"])]
    doc["concurrent_jobs"] = min(limits.CONCURRENT_JOBS_MAX,
                                 max([item["concurrent_jobs"] for item in planned] or [0]))
    doc["detail"] = ("%d of %d comparisons planned; advisory" % (len(planned),
                                                               len(comparisons.items)))
    return contracts.validate_schedule(doc), comparisons.plans


def _policy_changed(git, control, merge):
    """Whether the merge's registry policy differs from the control's: a missing registry on
    either side is a change, never an error that hides the proposed policy."""
    digests = []
    for sha in (control, merge):
        try:
            digests.append(planning.read_suite(git, sha).policy_digest)
        except (gitrefs.Unavailable, ContractError):
            digests.append(None)
    return digests[0] != digests[1]


def _release(git, event, claim, comparisons, runner_label, doc):
    """The comparison of one confirmed release claim (ADR-0180): the claim must be this run's
    own, of this repository, and its tag must still name the claimed SHA here."""
    from . import release as handoff
    require(event.name in ("workflow_run", "workflow_dispatch"),
            "a release claim is measured from its handoff's own workflow_run or dispatch, not "
            "%s" % event.name)
    contracts.validate_release_claim(claim)
    claimant, identity = claim["claimant"], claim["identity"]
    require((claimant["actions_run_id"], claimant["actions_run_attempt"])
            == (event.run_id, event.run_attempt) and identity["repository"] == event.repository,
            "the release claim was written by another run or repository")
    require(event.name == ("workflow_run" if claim["kind"] == "release"
                           else "workflow_dispatch"),
            "a %s claim is not measured from a %s" % (claim["kind"], event.name))
    tag, sha = identity["tag"], identity["sha"]
    parent = claim["remeasure_of"]
    doc["release"] = {"kind": claim["kind"], "key": claim["key"],
                      "claim_artifact": handoff.claim_artifact(claim["key"], claim["kind"],
                                                               event.run_id),
                      "upstream_run_id": identity["upstream_run_id"], "tag": tag, "sha": sha,
                      "remeasure_of": None if parent is None else parent["lab_run_id"]}
    values = {"trigger": "release", "tier": "release", "head": tag}
    try:
        found = git.exact_ref("refs/tags/" + tag, "the release tag")
        if found != sha:
            raise gitrefs.Unavailable("tag_moved", "tag %s names %s here, not the claimed %s; "
                                      "nothing else is measured" % (tag, found, sha))
        if parent is not None:
            # The same two commits a release comparison resolves, as an explicit manual
            # comparison that names the parent run it repeats.
            _, _, _, baseline = gitrefs.release_baseline(git, tag)
            values = {"trigger": "manual", "tier": "release", "base": baseline, "head": sha,
                      "control": sha, "remeasure_of": parent["lab_run_id"]}
    except gitrefs.Unavailable as error:
        comparisons.items.append(_unplanned(comparisons, values, "unavailable", error.reason,
                                            str(error)))
        return
    primary = comparisons.add("primary", dict(values, scope="full"), runner_label)
    if primary["status"] == "unavailable" and primary["reason"] == "no_active_authority":
        comparisons.add("control_bootstrap", dict(values, scope="bootstrap"), runner_label)


def _unplanned(comparisons, values, status, reason, detail):
    """The record of a primary comparison refused or unavailable before planning."""
    return {"role": "primary", "run_id": comparisons.run_id("primary"), "status": status,
            "reason": reason, "detail": detail[:4096], "plan_digest": None, "plan_sha256": None,
            "required_check_eligible": False, "harness_changed": None, "shards": 0,
            "concurrent_jobs": 0, "runner_label": None,
            "request": {"trigger": values["trigger"], "tier": values["tier"], "scope": "full",
                        "base": values.get("base"), "head": values["head"],
                        "control": values.get("control"), "authority": None, "cases": [],
                        "shards": None, "pairs": None, "pr_number": None, "pr_base_sha": None,
                        "pr_head_sha": None, "build_image": None}}


def _dispatch(git, event, comparisons, runner_label, settings):
    inputs = event.inputs if isinstance(event.inputs, dict) else {}
    scope = inputs.get("scope") or "partial_scope"
    tier = inputs.get("tier") or "quick"

    def refused(reason, detail):
        comparisons.items.append({
            "role": "primary", "run_id": comparisons.run_id("primary"), "status": "refused",
            "reason": reason, "detail": detail[:4096], "plan_digest": None, "plan_sha256": None,
            "required_check_eligible": False, "harness_changed": None, "shards": 0,
            "concurrent_jobs": 0, "runner_label": None,
            "request": {"trigger": "manual", "tier": tier if tier in ("quick", "nightly",
                                                                        "release") else "quick",
                        "scope": scope if scope in DISPATCH_SCOPES else "partial_scope",
                        "base": None, "head": str(inputs.get("head") or "-")[:256],
                        "control": None, "authority": None, "cases": [], "shards": None,
                        "pairs": None, "pr_number": None, "pr_base_sha": None,
                        "pr_head_sha": None, "build_image": None}})

    try:
        require(scope in DISPATCH_SCOPES, "scope: one of " + ", ".join(DISPATCH_SCOPES))
        require(tier in ("quick", "nightly", "release"), "tier: quick, nightly or release")
        base = _dispatch_ref(git, inputs.get("base"), "base")
        head = _dispatch_ref(git, inputs.get("head"), "head")
        control = _dispatch_ref(git, inputs.get("control"), "control")
        authority = _dispatch_ref(git, inputs.get("authority"), "authority")
        require(base is not None and head is not None, "a dispatch names its base and head")
        cases = [item for item in str(inputs.get("cases") or "").split() if item]
        values = {"trigger": "manual", "tier": tier, "scope": scope, "base": base, "head": head,
                  "control": control, "authority": authority, "cases": cases}
        for key in ("shards", "pairs"):
            if inputs.get(key) not in (None, ""):
                require(re.match(r"^[0-9]{1,3}$", str(inputs[key])) is not None,
                        "%s: a whole number" % key)
                values[key] = int(inputs[key])
        if scope == "calibration" and settings.executor == "dedicated":
            # A cohort calibrates fresh hosted runners (design section 15): a dedicated or
            # persistent runner, configured or not, never runs one of its members (ADR-0183).
            refused("dedicated_calibration", "a calibration member runs only on fresh hosted "
                    "runners; the requested dedicated executor is refused, and this run stays "
                    "the member's failed original")
            return
        if scope == "calibration":
            # The cohort and its member come from the trusted control revision's own files,
            # so a calibration needs no activation and no candidate input chooses its plan.
            require(control is not None, "a calibration names its trusted control revision")
            path = inputs.get("cohort") or ""
            require(re.match(r"^fixtures/performance/[A-Za-z0-9_./-]{1,200}\.json$", path)
                    is not None and ".." not in path,
                    "cohort: a fixtures/performance/*.json path of the control revision")
            raw = git.read_file(control, path)
            require(raw is not None, "cohort: %s is not in the control revision" % path)
            values["cohort"] = contracts.read_json(raw, "cohort", path)
            values["member"] = inputs.get("member") or None
            # The workflow named this run for the digest and member it was dispatched with,
            # before any job ran, and readiness binds a member's original by that name
            # (ADR-0182). A cohort of other bytes is refused here, and the run stays that
            # member's dispatch, failed.
            digest = canonical_digest(values["cohort"])
            require(inputs.get("cohort_digest") == digest,
                    "cohort_digest: the run is named for %s, but %s at the control revision is "
                    "%s" % (inputs.get("cohort_digest"), path, digest))
            # A slowdown member's sealed calibration parameters come from the control too, so
            # its loop counts are reviewed bytes committed before it runs (ADR-0181).
            sealed = inputs.get("calibration") or ""
            if sealed:
                require(re.match(r"^fixtures/performance/[A-Za-z0-9_./-]{1,200}\.json$",
                                 sealed) is not None and ".." not in sealed,
                        "calibration: a fixtures/performance/*.json path of the control "
                        "revision")
                raw = git.read_file(control, sealed)
                require(raw is not None, "calibration: %s is not in the control revision"
                        % sealed)
                values["calibration"] = contracts.read_json(raw, "calibration-parameters",
                                                            sealed)
    except gitrefs.Unavailable as error:
        refused(error.reason, str(error))
        return
    except ContractError as error:
        refused("invalid_dispatch", str(error))
        return
    comparisons.add("primary", values, runner_label)


def write(output, doc, plans):
    """Write schedule.json and each frozen plan below the new directory `output`."""
    os.makedirs(os.path.join(output, "plans"))
    with open(os.path.join(output, "schedule.json"), "xb") as stream:
        stream.write(contracts.write_bytes(doc, "schedule"))
    for run_id, data in sorted(plans.items()):
        with open(os.path.join(output, "plans", run_id + ".json"), "xb") as stream:
            stream.write(data)


def outputs(doc):
    """The job outputs later jobs read: the shard matrix, the planned runs to build, the
    concurrency bound, each run's runner label and the artifact retention."""
    planned = [item for item in doc["comparisons"] if item["status"] == "planned"]
    labels = sorted(set(item["runner_label"] for item in planned))
    return {
        "any": "true" if planned else "false",
        "builds": json.dumps([item["run_id"] for item in planned], separators=(",", ":")),
        "matrix": json.dumps(doc["matrix"], separators=(",", ":")),
        "concurrent_jobs": str(max(1, doc["concurrent_jobs"])),
        "runner": json.dumps(labels[0] if len(labels) == 1 else "ubuntu-latest"),
        "retention_days": str(doc["retention_days"]),
    }


def summary_markdown(doc):
    """The plan job's summary of the validated schedule: every comparison, why anything is
    unavailable, the trust decision, and that enforcement is advisory."""
    from . import render
    lines = ["### AF performance schedule: %s" % render.markdown_cell(
        doc["status"] if doc["reason"] is None else "%s (%s)" % (doc["status"], doc["reason"])),
        "", render.markdown_cell("%s. Enforcement is advisory; no required check is set "
                                 "here. %s" % (doc["detail"], doc["trust"]["detail"])), ""]
    if doc["comparisons"]:
        lines += ["| Comparison | Run | Status | Required-check eligible | Detail |",
                  "| --- | --- | --- | --- | --- |"]
        for item in doc["comparisons"]:
            cells = (item["role"], item["run_id"], item["status"] if item["reason"] is None
                     else "%s: %s" % (item["status"], item["reason"]),
                     "yes" if item["required_check_eligible"] else "no", item["detail"])
            lines.append("| %s |" % " | ".join(render.markdown_cell(cell) for cell in cells))
        lines.append("")
    return "\n".join(lines)


def github_output_lines(values):
    return "".join("%s=%s\n" % (key, value) for key, value in sorted(values.items()))


# Aggregation ---------------------------------------------------------------------------------

SHARD_ARTIFACT = re.compile(r"^af-perf-([a-z][a-z0-9_]{0,63})-shard-([0-9]{1,2})$")


def shard_artifact(run_id, shard):
    """The one artifact name of a run's shard bundle: unique per run and shard, so no upload
    can overwrite another and no download flattens two into one directory."""
    return "af-perf-%s-shard-%d" % (run_id, shard)


def run_artifact(run_id):
    return "af-perf-%s-run" % run_id


def aggregate(plan_bytes, builds_bytes, shards_dir, run_dir, report_dir, job_summary=None,
              transport_dir=None):
    """Import every downloaded shard artifact of the plan into a new run directory, recompute
    the verdict and write the report from the validated evidence; (report document, the
    completeness record). A shard without its artifact stays missing, so the verdict is
    incomplete; an artifact of another run, an unplanned shard or one shard twice refuses."""
    from . import report, runner, transport
    plan = contracts.read_json(plan_bytes, "run", "run.json")
    builds = contracts.read_json(builds_bytes, "builds", "builds.json")
    require((builds["run_id"], builds["plan_digest"]) == (plan["run_id"], plan["plan_digest"]),
            "builds.json is of another run or plan")
    runner.prepare_run_dir(run_dir, plan_bytes, builds_bytes)
    planned = len(plan["shards"])
    received = {}
    names = sorted(os.listdir(shards_dir)) if os.path.isdir(shards_dir) else []
    for name in names:
        match = SHARD_ARTIFACT.match(name)
        require(match is not None and match.group(1) == plan["run_id"],
                "%s is not a shard artifact of run %s" % (name, plan["run_id"]))
        index = int(match.group(2))
        require(index < planned, "%s names shard %d; the plan has %d" % (name, index, planned))
        doc, files, _ = transport.read_directory(os.path.join(shards_dir, name))
        require(doc["run_id"] == plan["run_id"] and doc["content"] == "shard_bundle"
                and doc["shard"] == index,
                "%s holds %s %s of run %s, not shard %d of this run"
                % (name, doc["content"], doc["shard"], doc["run_id"], index))
        require(index not in received, "shard %d arrived twice" % index)
        target = os.path.join(run_dir, runner.SHARD_DIR, str(index))
        for relative, data in sorted(files.items()):
            path = os.path.join(target, *relative.split("/"))
            os.makedirs(os.path.dirname(path), 0o700, exist_ok=True)
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "wb") as stream:
                stream.write(data)
        received[index] = name
    completeness = {"planned": planned, "received": sorted(received),
                    "missing": [index for index in range(planned) if index not in received]}
    doc = report.write(run_dir, report_dir, job_summary=job_summary)
    if job_summary is not None and completeness["missing"]:
        with open(job_summary, "a", encoding="utf-8") as stream:
            stream.write("Shards without an artifact, kept as incomplete evidence: %s\n\n"
                         % ", ".join(str(index) for index in completeness["missing"]))
    if transport_dir is not None:
        views = []
        for name in (report.REPORT_HTML, report.SUMMARY_MD):
            with open(os.path.join(report_dir, name), "rb") as stream:
                views.append((name, stream.read()))
        transport.stage(transport_dir, run_dir, "run_bundle", views=views)
    return doc, completeness
