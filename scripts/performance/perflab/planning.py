"""Freeze a comparison plan: resolved identities, authority, cases, schedule and limits.

Planning reads Git objects and the authority's registry; it never builds, checks out or
executes a revision. `resolve` applies the trigger's baseline rule and the scope's authority
rule; `freeze` is a pure function of the validated suite and those identities.
"""
import hashlib
import os

from . import contracts, gitrefs, images, limits, schedule, stats
from . import suite as suite_module
from .gitrefs import Unavailable
from .values import (ContractError, canonical_digest, format_rational, parse_decimal,
                     require, sha256_bytes)

SUITE_PATH = "fixtures/performance/suite.toml"
CARGO_LOCK_PATH = "Cargo.lock"
ACTIVATION_PATH = "fixtures/performance/activation.toml"
INSTRUMENT_PATHS = ("scripts/performance",)
FIXTURE_PATHS = ("fixtures/performance",)
FIXTURE_EXCLUDE = (ACTIVATION_PATH, "fixtures/performance/kernel-measure")
# The default-off instrumentation the probe and AF compile in (ADR-0174, ADR-0181): the phase
# trace recorder and the calibration loop are instrument code, never an optimization surface
# (ADR-0185); the product code that calls them stays free.
INSTRUMENT_HELPER_PATHS = (
    "crates/review-core/src/perf_calibration.rs",
    "crates/review-core/src/perf_trace.rs",
)
# Candidate changes under these paths are labeled harness_changed; the authority measures.
HARNESS_PATHS = INSTRUMENT_PATHS + FIXTURE_PATHS + INSTRUMENT_HELPER_PATHS + (
    "crates/review-pipeline/examples/perf-probe",
    "crates/review-pipeline/examples/perf-probe.rs",
    "crates/review-process/examples/perf-supervisor",
    "crates/review-process/examples/perf-supervisor.rs",
    ".github/workflows/performance.yml",
    ".github/workflows/performance-release.yml",
    ".github/workflows/performance-activation.yml",
)
HARNESS_PATHS_MAX = 256
# Every input that decides how the instrument and the products compile, wherever it is added
# (ADR-0184): the lock files and Cargo manifests, build scripts, the toolchain pin and Cargo
# configuration. Product sources are not here: candidates optimize them.
BUILD_CLOSURE_PATHS = (
    ":(glob)**/Cargo.lock", ":(glob)**/Cargo.toml", ":(glob)**/build.rs",
    ":(glob)**/rust-toolchain", ":(glob)**/rust-toolchain.toml", ":(glob)**/.cargo/**",
)
TRIGGERS = ("pr", "main_push", "nightly", "release", "manual")


class Request:
    """Every planning input, as given on the command line."""

    def __init__(self, **values):
        self.trigger = "manual"
        self.tier = None
        self.scope = "full"
        self.base = None
        self.head = None
        self.control = None
        self.authority = None
        self.cases = []
        self.shards = None
        self.pairs = None
        self.seed = None
        self.run_id = None
        self.parent_run_id = None
        self.environment = None
        self.pr_number = None
        self.pr_base_sha = None
        self.pr_head_sha = None
        self.remeasure_of = None
        self.cohort = None
        self.member = None
        # A slowdown member's sealed calibration parameters (ADR-0181).
        self.calibration = None
        self.parent_plan = None
        # How a Linux recipe without its own image pin builds, frozen here (ADR-0170).
        self.build_image = None
        self.trusted_host_build = False
        # A diagnostic plan's capture bounds (ADR-0174); the hard defaults otherwise.
        self.diagnostic_duration_ms = None
        self.diagnostic_frequency_hz = None
        # An operator configuration's declared run windows (`core.windows`, ADR-0269): the
        # shard sampling and run total minutes frozen instead of the tier's, under the same
        # hard maxima; None keeps the tier's own.
        self.windows = None
        for key, value in values.items():
            require(hasattr(self, key), "unknown planning input " + key)
            setattr(self, key, value)


class Identities:
    def __init__(self):
        self.baseline = None
        self.candidate = None
        self.control = None
        self.trigger = None


def _given_matches(git, selector, expected, what):
    if selector is not None:
        actual = git.resolve(selector, what)
        if actual != expected:
            raise ContractError("%s: %s resolves to %s, but the trigger's rule selects %s"
                                % (what, selector, actual, expected))


def resolve_trigger(git, request):
    """Resolve selectors once to full SHAs under the trigger's baseline rule."""
    ids = Identities()
    trigger = request.trigger
    require(trigger in TRIGGERS, "--trigger: one of " + ", ".join(TRIGGERS))
    require(request.head, "--head: the candidate selector is required")
    if trigger == "pr":
        require(request.pr_base_sha and request.pr_head_sha,
                "--trigger pr needs --pr-base-sha and --pr-head-sha from the event")
        merge = git.resolve(request.head, "--head")
        parents = git.parents(merge)
        if len(parents) != 2:
            raise Unavailable("not_a_merge_commit", "--head %s is not a two-parent PR merge "
                              "commit" % request.head)
        if parents[0] != request.pr_base_sha:
            raise ContractError("the merge commit's first parent %s disagrees with the event's "
                                "base SHA %s; the plan is refused" % (parents[0],
                                                                     request.pr_base_sha))
        if parents[1] != request.pr_head_sha:
            raise ContractError("the merge commit's second parent %s is not the PR head %s"
                                % (parents[1], request.pr_head_sha))
        _given_matches(git, request.base, parents[0], "--base")
        ids.candidate = merge
        ids.baseline = parents[0]
        ids.control = parents[0]
        if request.control is not None:
            _given_matches(git, request.control, parents[0], "--control")
        ids.trigger = {"kind": "pr", "number": request.pr_number, "head_sha": parents[1],
                       "base_sha": parents[0], "merge_sha": merge,
                       "merge_base_sha": git.merge_base(parents[0], parents[1])}
    elif trigger == "main_push":
        pushed = git.resolve(request.head, "--head")
        ids.candidate = pushed
        ids.baseline = gitrefs.first_parent(git, pushed, "pushed commit")
        _given_matches(git, request.base, ids.baseline, "--base")
        ids.control = ids.baseline
        if request.control is not None:
            _given_matches(git, request.control, ids.baseline, "--control")
        ids.trigger = {"kind": "main_push", "pushed_sha": pushed}
    elif trigger == "nightly":
        main = git.resolve(request.head, "--head")
        tag, sha = gitrefs.nightly_baseline(git, main)
        _given_matches(git, request.base, sha, "--base")
        ids.candidate, ids.baseline, ids.control = main, sha, main
        if request.control is not None:
            _given_matches(git, request.control, main, "--control")
        ids.trigger = {"kind": "nightly", "main_sha": main, "baseline_tag": tag}
    elif trigger == "release":
        tag_sha, prerelease, base_tag, base_sha = gitrefs.release_baseline(git, request.head)
        require(git.resolve(request.head, "--head") == tag_sha,
                "--head: the release tag resolves inconsistently")
        _given_matches(git, request.base, base_sha, "--base")
        ids.candidate, ids.baseline, ids.control = tag_sha, base_sha, tag_sha
        if request.control is not None:
            _given_matches(git, request.control, tag_sha, "--control")
        ids.trigger = {"kind": "release", "tag": request.head, "tag_sha": tag_sha,
                       "baseline_tag": base_tag, "prerelease": prerelease}
    else:
        require(request.base, "--trigger manual needs an explicit --base")
        ids.baseline = git.resolve(request.base, "--base")
        ids.candidate = git.resolve(request.head, "--head")
        ids.control = (git.resolve(request.control, "--control") if request.control
                       else ids.baseline)
        ids.trigger = {"kind": "manual", "remeasure_of": request.remeasure_of}
    return ids


def read_activation(git, control):
    """The control revision's activation manifest and the SHA-256 of its bytes, shape-checked
    only; (None, None) without one. `verified_activation` checks what it binds."""
    from . import activation
    return activation.read_manifest(git, control, ACTIVATION_PATH)


def verified_activation(git, control):
    """The control revision's reviewed activation, verified locally as an ordinary plan does
    (ADR-0183): its cohort, parameters and readiness rehashed from the control revision, the
    readiness decision recomputed from its member outcomes, its original runs and calibrated
    scope checked, and a bridge's execution evidence checked (ADR-0184); the old cohort is not
    fetched again. Nothing measures as active otherwise. An `activation.Verified`."""
    from . import activation
    return activation.verify(git, control, ACTIVATION_PATH)


def read_suite(git, authority):
    raw = git.read_file(authority, SUITE_PATH)
    if raw is None:
        raise Unavailable("no_registry", "the authority %s has no %s" % (authority, SUITE_PATH))
    return suite_module.load_bytes(raw, "%s@%s" % (SUITE_PATH, authority[:12]))


def resolve_authority(git, request, ids):
    """Select the measuring authority for the requested scope.

    A full plan needs the control revision's activation; calibration names a proposed SHA
    reachable from the trusted control revision and needs no activation, which is what
    makes a first activation possible. Bootstrap, partial and diagnostic plans are never
    eligible for the required check.
    """
    scope = request.scope
    activation, _ = read_activation(git, ids.control)
    if scope == "full":
        if activation is None:
            raise Unavailable("no_active_authority", "the control revision %s has no %s; "
                              "full-check status is unavailable before activation"
                              % (ids.control, ACTIVATION_PATH))
        authority = activation["authority_sha"]
        if request.authority is not None:
            _given_matches(git, request.authority, authority, "--authority")
        if not git.is_ancestor(authority, ids.control):
            raise Unavailable("authority_unreachable", "the active authority %s is not "
                              "reachable from the control revision" % authority)
        suite = read_suite(git, authority)
        if suite.policy_digest != activation["policy_digest"]:
            raise ContractError("the authority's registry digest %s differs from the activation's "
                                "%s" % (suite.policy_digest, activation["policy_digest"]))
        verified = verified_activation(git, ids.control)
        _calibrated_scope(suite, request, activation)
        return "active", authority, suite, _activation_record(verified)
    if scope == "calibration":
        require(request.authority, "--scope calibration needs --authority SHA")
        require(request.control is not None or request.trigger != "manual",
                "--scope calibration needs the trusted --control revision the proposed "
                "authority must be reachable from")
        authority = git.resolve(request.authority, "--authority")
        if not git.is_ancestor(authority, ids.control):
            raise Unavailable("authority_unreachable", "proposed authority %s is not reachable "
                              "from the trusted control revision %s" % (authority, ids.control))
        return "proposed", authority, read_suite(git, authority), None
    if request.authority is not None:
        authority = git.resolve(request.authority, "--authority")
        return "proposed", authority, read_suite(git, authority), None
    if scope == "partial_scope" and activation is not None:
        authority = activation["authority_sha"]
        if git.is_ancestor(authority, ids.control):
            suite = read_suite(git, authority)
            verified = (_verifies(git, ids.control)
                        if suite.policy_digest == activation["policy_digest"] else None)
            if verified is not None and _in_scope(suite, request, activation):
                return "active", authority, suite, _activation_record(verified)
    return "control", ids.control, read_suite(git, ids.control), None


def _verifies(git, control):
    try:
        return verified_activation(git, control)
    except (ContractError, Unavailable):
        return None


def _profile_of(suite, request):
    """The (environment, recipe) the plan will freeze for this request."""
    profile, _ = _select_profile(suite, request, request.tier, False)
    return profile["environment"], profile["recipe"]


def _in_scope(suite, request, activation):
    try:
        return _profile_of(suite, request) == (activation["environment_id"],
                                                activation["recipe_id"])
    except ContractError:
        return False


def _calibrated_scope(suite, request, activation):
    """An activation measures as active only in the environment and recipe its cohort
    calibrated; another tier's profile is unavailable, never presented as calibrated."""
    environment, recipe = _profile_of(suite, request)
    if (environment, recipe) != (activation["environment_id"], activation["recipe_id"]):
        raise Unavailable("uncalibrated_scope", "the activation's cohort %s calibrated %s with "
                          "recipe %s; a full plan of %s with recipe %s is not calibrated by it"
                          % (activation["cohort_id"], activation["environment_id"],
                             activation["recipe_id"], environment, recipe))


def _activation_record(verified):
    """What a plan binds of its verified activation, the exact CPU/image groups its cohort
    calibrated included: the verdict binds every shard's observed host to them (ADR-0184)."""
    manifest = verified.manifest
    groups = [dict(group) for group in verified.cohort["allowed_cpu_groups"]
              if group["group_id"] in manifest["calibrated_groups"]]
    return {"sha256": verified.sha256, "admission": manifest["admission"]["kind"],
            "readiness_sha256": manifest["readiness_sha256"],
            "cohort_id": manifest["cohort_id"],
            "calibrated_groups": sorted(groups, key=lambda group: group["group_id"])}


def read_build_inputs(git, ids, authority):
    """Each build's locked dependency input, read from the resolved commit's Git objects.

    The supervisor builds from the authority, each side from its own source. A source without
    a regular Cargo.lock cannot be built with locked dependencies, so the plan is unavailable.
    """
    inputs = {}
    for role, sha in (("authority", authority), ("baseline", ids.baseline),
                      ("candidate", ids.candidate)):
        raw = git.read_file(sha, CARGO_LOCK_PATH)
        if raw is None:
            raise Unavailable("missing_build_input", "the %s source %s has no %s; its build "
                              "cannot be bound to locked dependencies"
                              % (role, sha, CARGO_LOCK_PATH))
        inputs[role] = {"cargo_lock_sha256": sha256_bytes(raw)}
    return inputs


def plan(git, request):
    """Resolve and freeze a complete plan from a repository."""
    if request.scope == "diagnostic":
        return _diagnostic(git, request)
    ids = resolve_trigger(git, request)
    mode, authority, suite, activation = resolve_authority(git, request, ids)
    changed = git.changed_paths(ids.baseline, ids.candidate, HARNESS_PATHS)
    digests = {
        "suite_sha256": suite.raw_sha256, "policy_digest": suite.policy_digest,
        "instrument_digest": git.manifest_digest(authority, INSTRUMENT_PATHS),
        "fixture_digest": git.manifest_digest(authority, FIXTURE_PATHS, FIXTURE_EXCLUDE),
    }
    return freeze(suite, request, ids, mode, authority, activation, digests, changed,
                  read_build_inputs(git, ids, authority))


def _diagnostic(git, request, recipe=None):
    """A diagnostic child plan of `request.parent_plan`; `recipe`, when given, is the diagnostic
    recipe `(recipe_id, suite-shaped recipe, frozen identity)` it freezes (ADR-0174,
    ADR-0179), else it freezes its parent's verdict recipe."""
    parent = request.parent_plan
    require(parent is not None, "--scope diagnostic needs --parent RUN_JSON")
    contracts.validate_run(parent, "--parent")
    require(parent["scope"] != "diagnostic", "--parent: a diagnostic run's parent is a "
            "comparison, not another diagnostic")
    require(request.cases, "--scope diagnostic needs explicit --case selections")
    for name in ("base", "head", "control", "authority", "cohort", "member", "calibration",
                 "pairs", "build_image"):
        require(getattr(request, name) is None,
                "--scope diagnostic takes its identities from --parent, not --"
                + name.replace("_", "-"))
    require(not request.trusted_host_build, "--scope diagnostic builds as its --parent froze; "
            "not --trusted-host-build")
    resolved = parent["resolved"]
    ids = Identities()
    ids.baseline = resolved["baseline_sha"]
    ids.candidate = resolved["candidate_sha"]
    ids.control = resolved["control_sha"]
    ids.trigger = parent["trigger"]
    for sha in (ids.baseline, ids.candidate, ids.control, resolved["authority_sha"]):
        require(git.resolve(sha, "--parent") == sha, "--parent: names an unknown commit")
    suite = read_suite(git, resolved["authority_sha"])
    require(suite.policy_digest == parent["authority"]["policy_digest"],
            "--parent: the authority registry no longer matches the parent plan")
    selections = list(request.cases)
    parent_ids = [case["instance_id"] for case in parent["cases"]]
    for item in selections:
        require(item in parent_ids, "--case %s: not an instance of the parent run" % item)
    require(len(set(selections)) <= limits.DIAGNOSTIC_CASES_MAX,
            "--case: at most %d diagnostic cases" % limits.DIAGNOSTIC_CASES_MAX)
    request.tier = parent["tier"]
    request.environment = parent["profile"]["environment_id"]
    authority = parent["authority"]
    digests = dict((key, authority[key]) for key in ("suite_sha256", "policy_digest",
                                                     "instrument_digest", "fixture_digest"))
    selectors = dict(parent["selectors"])
    return freeze(suite, request, ids, authority["mode"], resolved["authority_sha"],
                 authority["activation"], digests, parent["harness"]["paths"],
                 parent["build_inputs"], selectors=selectors, environment_override=True,
                 build=parent["profile"]["build"],
                 diagnostic={"parent_run_id": parent["run_id"],
                             "parent_plan_digest": parent["plan_digest"],
                             "duration_ms": request.diagnostic_duration_ms
                             or limits.DIAGNOSTIC_SECONDS_DEFAULT * 1000,
                             "frequency_hz": request.diagnostic_frequency_hz
                             or limits.DIAGNOSTIC_HZ_DEFAULT,
                             "recipe": None if recipe is None else recipe[2]},
                 recipe=None if recipe is None else recipe[:2])


def _select_profile(suite, request, tier, environment_override):
    profiles = suite.doc["tiers"][tier]["profiles"]
    if request.environment is None:
        return profiles[0], True
    for index, profile in enumerate(profiles):
        if profile["environment"] == request.environment:
            require(index == 0 or request.trigger == "manual" or environment_override
                    or request.scope == "calibration",
                    "--environment: only a manual run selects a non-default tier profile")
            return profile, index == 0
    raise ContractError("--environment %s: not a profile of tier %s" % (request.environment,
                                                                       tier))


def _select_instances(suite, request, tier):
    available = suite.instances(tier)
    if request.scope in ("full", "bootstrap", "calibration"):
        require(not request.cases, "--case: a %s plan measures every instance of its tier"
                % request.scope)
        return available
    require(request.cases, "--scope %s needs at least one --case" % request.scope)
    by_id = dict((item["instance_id"], item) for item in available)
    chosen = {}
    for selection in request.cases:
        matched = [item for item in available
                   if item["instance_id"] == selection or item["case_id"] == selection]
        require(matched, "--case %s: no such case or instance in tier %s" % (selection, tier))
        for item in matched:
            chosen[item["instance_id"]] = item
    return [by_id[ident] for ident in sorted(chosen)]


def frozen_build(recipe_id, recipe, configured=None, trusted_host=False):
    """How the recipe builds, fixed before any build (ADR-0170): its own image pin, an image
    configured for an unpinned Linux recipe, an explicitly trusted Linux host build, or a
    macOS recipe's native trusted local build. Anything ambiguous or mismatched is refused."""
    if recipe["os"] == "macos":
        require(configured is None and not trusted_host,
                "--build-image and --trusted-host-build apply to Linux recipes; %s builds "
                "natively on its explicitly trusted local host" % recipe_id)
        return {"isolation": "native_host", "image": None, "image_source": None}
    require(configured is None or not trusted_host,
            "--build-image and --trusted-host-build exclude each other")
    if configured is not None:
        images.canonical(configured, "--build-image")
    pinned = recipe.get("container_image")
    if pinned is not None:
        require(not trusted_host, "recipe %s pins build image %s; it never builds on a host"
                % (recipe_id, pinned))
        require(configured is None or configured == pinned,
                "build_image_mismatch: recipe %s pins build image %s; --build-image %s is not "
                "that reference" % (recipe_id, pinned, configured))
        return {"isolation": "container", "image": pinned, "image_source": "recipe"}
    if configured is not None:
        return {"isolation": "container", "image": configured, "image_source": "configured"}
    require(trusted_host, "Linux recipe %s pins no build image: freeze one with --build-image "
            "NAME@sha256:... (or its sha256 image ID), or freeze an explicitly trusted host "
            "build, which claims no filesystem isolation, with --trusted-host-build" % recipe_id)
    return {"isolation": "trusted_host", "image": None, "image_source": None}


def default_seed(run_id):
    digest = hashlib.sha256(("af.perf-seed/1\x00" + run_id).encode("utf-8")).digest()
    return int.from_bytes(digest[:8], "big") >> 1


def new_run_id():
    return "run_" + os.urandom(12).hex()


def freeze(suite, request, ids, mode, authority, activation, digests, changed, build_inputs,
           selectors=None, environment_override=False, diagnostic=None, build=None,
           recipe=None):
    """Freeze the plan as a pure function of the registry, resolved identities and the build
    configuration; a diagnostic plan takes its parent's frozen `build`, and may freeze a
    diagnostic recipe `(recipe_id, recipe)` built like its base (ADR-0174)."""
    doc = suite.doc
    tier = request.tier
    require(tier in doc["tiers"], "--tier: one of " + ", ".join(sorted(doc["tiers"])))
    scope = request.scope
    require(scope in contracts.SCOPES, "--scope: one of " + ", ".join(contracts.SCOPES))
    profile, is_default = _select_profile(suite, request, tier, environment_override)
    environment = doc["environments"][profile["environment"]]
    recipe_id = profile["recipe"]
    if diagnostic is not None:
        require((recipe is None) == (diagnostic["recipe"] is None), "a diagnostic plan freezes "
                "its recipe's kind exactly when it freezes a diagnostic recipe")
    if recipe is not None:
        require(diagnostic is not None, "only a diagnostic plan freezes a diagnostic recipe")
        recipe_id, recipe = recipe
    else:
        recipe = doc["recipes"][recipe_id]
    if build is None:
        build = frozen_build(profile["recipe"], recipe, request.build_image,
                             request.trusted_host_build)
    if diagnostic is None:
        instances = _select_instances(suite, request, tier)
    else:
        wanted = set(request.cases)
        instances = [item for item in suite.instances(tier) if item["instance_id"] in wanted]
    for item in instances:
        require(suite_module.supports(doc, profile["environment"], item),
                "environment %s cannot contain %s; spawning cases require Linux containment"
                % (profile["environment"], item["instance_id"]))
    require(len(instances) <= limits.INSTANCES_MAX, "a plan holds at most 64 instances")

    cohort_record = None
    seed = request.seed
    if scope == "calibration":
        cohort_record, seed = _calibration_member(suite, request, ids, authority, tier,
                                                  profile, instances, seed)
    elif request.cohort is not None or request.member is not None \
            or request.calibration is not None:
        raise ContractError("--cohort, --member and --calibration belong to --scope "
                            "calibration; no other plan carries an injection control")
    if request.pairs is not None:
        require(scope in ("partial_scope", "bootstrap", "diagnostic"),
                "--pairs: full and calibration plans use the registry's frozen pair counts")
    run_id = request.run_id or new_run_id()
    if seed is None:
        seed = default_seed(run_id)

    family_alpha = parse_decimal(doc["verdict"]["family_alpha"], "verdict.family_alpha")
    advisory_alpha = parse_decimal(doc["verdict"]["advisory_alpha"], "verdict.advisory_alpha")
    critical = [item for item in instances if item["criticality"] == "critical"]
    family_size = len(critical)
    case_alpha = family_alpha / max(1, family_size)

    tier_doc = doc["tiers"][tier]
    shards = request.shards
    if shards is None:
        shards = min(tier_doc["shards"], len(instances))
    require(1 <= shards <= limits.SHARDS_MAX, "--shards: between 1 and 16")
    require(shards <= len(instances), "--shards: never more shards than instances")
    windows = tier_doc if request.windows is None else request.windows
    require(set(windows) >= {"shard_sampling_minutes", "run_total_minutes"},
            "declared windows name shard_sampling_minutes and run_total_minutes")
    assignment = schedule.assign_shards([item["instance_id"] for item in instances], shards)
    owner = dict((ident, index) for index, members in enumerate(assignment)
                 for ident in members)

    cases = []
    for item in instances:
        case = doc["cases"][item["case_id"]]
        policy = doc["policies"][case["policy"]]
        pairs = request.pairs if request.pairs is not None else case["pairs"][tier]
        require(type(pairs) is int and limits.PAIRS_MIN <= pairs <= limits.PAIRS_MAX
                and pairs % 2 == 0, "pairs: an even count within 12..200")
        alpha = case_alpha if item["criticality"] == "critical" else advisory_alpha
        rank = stats.critical_rank(pairs, 1, alpha)
        if rank is None:
            raise ContractError("%s: N=%d admits no interval at alpha/K=%s; the plan is "
                                "impossible" % (item["instance_id"], pairs,
                                                format_rational(alpha)))
        metric = doc["metrics"][case["primary_metric"]]
        budgets = []
        for budget_id in case["budgets"]:
            budget = doc["budgets"][budget_id]
            # An injected calibration run never demonstrates a budget (ADR-0181).
            applicable = (tier in budget["tiers"]
                          and profile["environment"] in budget["environments"]
                          and profile["recipe"] in budget["recipes"]
                          and (cohort_record is None or cohort_record["injection"] is None))
            if applicable and budget["state"] == "enabled":
                require(pairs >= budget["minimum_samples"],
                        "%s: budget %s needs %d samples" % (item["instance_id"], budget_id,
                                                            budget["minimum_samples"]))
            budgets.append({"budget_id": budget_id, "metric": budget["metric"],
                            "limit": budget["limit"], "unit": budget["unit"],
                            "minimum_samples": budget["minimum_samples"],
                            "state": budget["state"], "applicable": applicable})
        cases.append({
            "instance_id": item["instance_id"], "case_id": item["case_id"],
            "family": case["family"], "kind": case["kind"],
            "requires_linux_containment": case["requires_linux_containment"],
            "parameters": item["parameters"], "criticality": item["criticality"],
            "timing": case["timing"], "cache": case["cache"], "oracle": case["oracle"],
            "execution": case["execution"], "af_environment": case.get("af_environment", {}),
            "primary_metric": {"key": case["primary_metric"], "unit": metric["unit"],
                               "direction": metric["direction"]},
            "optional_metrics": [{"key": key, "unit": doc["metrics"][key]["unit"],
                                  "direction": doc["metrics"][key]["direction"]}
                                 for key in case["optional_metrics"]],
            "tolerance": {"policy_id": case["policy"],
                          "relative": policy["relative_tolerance"],
                          "absolute": policy["absolute_tolerance"],
                          "calibration": policy["calibration"]},
            "pairs": pairs, "warmup_pairs": case["warmup_pairs"], "rank": rank,
            "alpha": format_rational(alpha),
            "tail_probability": format_rational(stats.tail_probability(pairs, rank)),
            "exploratory": item["criticality"] == "advisory",
            "shard": owner[item["instance_id"]],
            "warmup_orders": schedule.block_orders(seed, item["instance_id"], "warmup",
                                                   case["warmup_pairs"]),
            "measured_orders": schedule.block_orders(seed, item["instance_id"], "measured",
                                                     pairs),
            "sample_timeout_ms": case["sample_timeout_seconds"] * 1000,
            "budgets": budgets,
            "seed": suite_module.seed_of(doc, item) if case["kind"] == "library" else None,
        })

    health = doc["health"]
    paths = sorted(changed)
    plan_doc = {
        "schema": contracts.RUN_SCHEMA, "run_id": run_id,
        "parent_run_id": request.parent_run_id, "scope": scope, "tier": tier,
        "trigger": ids.trigger,
        "selectors": selectors or {"base": request.base, "head": request.head,
                                   "control": request.control,
                                   "authority": request.authority},
        "resolved": {"baseline_sha": ids.baseline, "candidate_sha": ids.candidate,
                     "control_sha": ids.control, "authority_sha": authority},
        "build_inputs": build_inputs,
        "authority": dict(digests, mode=mode, activation=activation),
        "harness": {"changed": bool(paths), "paths": paths[:HARNESS_PATHS_MAX],
                    "truncated": len(paths) > HARNESS_PATHS_MAX},
        "profile": {"environment_id": profile["environment"],
                    "environment_digest": canonical_digest({"id": profile["environment"],
                                                            "environment": environment}),
                    "environment": environment, "recipe_id": recipe_id,
                    "recipe_digest": contracts.recipe_digest(recipe_id, recipe, build),
                    "recipe": recipe, "build": build, "default": is_default},
        "verdict_policy": {"algorithm": doc["verdict"]["algorithm"],
                           "family_alpha": format_rational(family_alpha),
                           "advisory_alpha": format_rational(advisory_alpha),
                           "family_size": family_size,
                           "case_alpha": format_rational(case_alpha)},
        "health": {"rule": health["rule"], "workload": health["workload"],
                   "metric": health["metric"], "unit": "ms",
                   "batch_repetitions": health["batch_repetitions"],
                   "start_pairs": health["start_pairs"], "end_pairs": health["end_pairs"],
                   "max_median_shift": health["max_median_shift"],
                   "max_mad_ratio": health["max_mad_ratio"],
                   "denominator_floor": health["denominator_floor"],
                   "state": health["state"], "oracle": health["oracle"]},
        "limits": dict(doc["limits"], shard_sampling_minutes=windows["shard_sampling_minutes"],
                       run_total_minutes=windows["run_total_minutes"],
                       concurrent_jobs=min(tier_doc["concurrent_jobs"], shards),
                       record_bytes=limits.RECORD_BYTES_MAX),
        "seed": seed,
        "shards": [{"index": index, "instances": members}
                   for index, members in enumerate(assignment)],
        "cases": cases, "cohort": cohort_record, "diagnostic": diagnostic,
        "required_check_eligible": scope == "full" and mode == "active",
    }
    plan_doc["plan_digest"] = contracts.plan_digest(plan_doc)
    return contracts.validate_run(plan_doc, "plan")


def _calibration_member(suite, request, ids, authority, tier, profile, instances, seed):
    cohort = request.cohort
    require(cohort is not None and request.member, "--scope calibration needs --cohort PATH "
            "and --member ID")
    contracts.validate_cohort(cohort, "--cohort")
    require(cohort["authority_sha"] == authority,
            "--cohort: declares authority %s, not %s" % (cohort["authority_sha"], authority))
    require(cohort["policy_digest"] == suite.policy_digest,
            "--cohort: declares a different policy digest than the proposed registry")
    require(cohort["tier"] == tier and cohort["environment_id"] == profile["environment"]
            and cohort["recipe_id"] == profile["recipe"],
            "--cohort: declares a different tier, environment or recipe")
    members = [member for member in cohort["members"] if member["member_id"] == request.member]
    require(members, "--member %s: not a declared cohort member" % request.member)
    member = members[0]
    require(ids.baseline == ids.candidate == member["source_sha"],
            "--member %s: both sides measure the declared source %s"
            % (request.member, member["source_sha"]))
    if seed is not None:
        require(seed == member["seed"], "--seed: the cohort fixes this member's seed")
    declared = [(case["instance_id"], case["criticality"], case["pairs"])
                for case in cohort["cases"]]
    planned = [(item["instance_id"], item["criticality"],
                suite.doc["cases"][item["case_id"]]["pairs"][tier]) for item in instances]
    require(declared == planned, "--cohort: declared cases and N differ from the registry's "
            "full critical family for tier " + tier)
    health = dict((key, value) for key, value in suite.doc["health"].items()
                  if key != "description")
    health["unit"] = "ms"
    require(cohort["health"] == health, "--cohort: declares a different health rule")
    from . import calibration
    injection = calibration.plan_injection(
        cohort, request.calibration, member, [item["instance_id"] for item in instances])
    record = {"cohort_id": cohort["cohort_id"], "cohort_digest": canonical_digest(cohort),
              "member_id": member["member_id"], "member_kind": member["kind"],
              "injection": injection}
    return record, member["seed"]
