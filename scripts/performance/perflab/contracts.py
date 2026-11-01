"""Closed, versioned evidence documents of the laboratory, with their readers and writers.

Each contract is a shape (validated and emitted as `schemas/performance-*-v<N>.json` from one
definition) plus semantic rules a JSON Schema cannot state: recomputed digests, ranks and
schedules, cross-field identities and resource bounds. Unknown versions and fields fail closed;
nothing migrates an earlier shape.
"""
from fractions import Fraction
import re

from . import diagcontracts, images, limits, reference, schedule, stats, suite
from .spec import (AnyOf, Arr, Bool, COUNT, Const, DECIMAL, DIGEST, Field, GIT_SHA, ID, Int,
                   JsonObject, Map, METRIC, Nullable, Obj, RATIONAL, REL_PATH, SHORT_TEXT, Str,
                   TEXT, Tagged, UNIT, json_schema, tagged_variant)
from .suite import (CACHE, ENVIRONMENT, EXECUTION, ORACLE, RECIPE, TIMING, TIERS)
from .values import (ContractError, ID_PATTERN, canonical_bytes, canonical_digest,
                     document_bytes, format_rational, loads_strict, parse_decimal,
                     parse_rational, require)

SCOPES = ("full", "partial_scope", "bootstrap", "calibration", "diagnostic")
SIDES = (schedule.BASELINE, schedule.CANDIDATE)
FAILURE_KINDS = ("oracle_mismatch", "timeout", "lease_expired", "nonzero_exit",
                 "missing_required_metric", "lifecycle_violation", "launch_failure")
DIRECTION = Str(enum=("lower_is_better", "higher_is_better"))
ORDER = Str(enum=schedule.ORDERS)
PARAM_VALUE = AnyOf(ID, Int(0, 10 ** 12))
ENV_KEY = r"^[A-Z_][A-Z0-9_]{0,63}$"
# Shared by the supervisor's request and result and the samples that record them.
CONTAINMENTS = ("process_group", "pid_namespace")
NANOS = Str(pattern=r"^(?:0|[1-9][0-9]{0,30})$", max_len=31)
TERMINATIONS = ("exited", "signalled", "timeout", "cancelled", "launch_failed", "status_unknown")
EXIT_OBSERVATIONS = ("unreaped", "reaped_by_polling", "unobserved", "none")
LIFECYCLE_STATUSES = ("clean", "no_observed_descendants", "unexpected_descendants",
                      "cleanup_uncertain")

# af.perf-run/4 -----------------------------------------------------------------------------

RUN_SCHEMA = "af.perf-run/7"

TRIGGER = Tagged("kind", {
    "pr": tagged_variant("kind", "pr", Field("number", Nullable(Int(1, 2 ** 31))),
                         Field("head_sha", GIT_SHA), Field("base_sha", GIT_SHA),
                         Field("merge_sha", GIT_SHA), Field("merge_base_sha", GIT_SHA)),
    "main_push": tagged_variant("kind", "main_push", Field("pushed_sha", GIT_SHA)),
    "nightly": tagged_variant("kind", "nightly", Field("main_sha", GIT_SHA),
                              Field("baseline_tag", SHORT_TEXT)),
    "release": tagged_variant("kind", "release", Field("tag", SHORT_TEXT),
                              Field("tag_sha", GIT_SHA), Field("baseline_tag", SHORT_TEXT),
                              Field("prerelease", Bool())),
    "manual": tagged_variant("kind", "manual", Field("remeasure_of", Nullable(ID))),
})

METRIC_REF = Obj(Field("key", ID), Field("unit", UNIT), Field("direction", DIRECTION))

CASE_BUDGET = Obj(
    Field("budget_id", ID), Field("metric", ID), Field("limit", DECIMAL), Field("unit", UNIT),
    Field("minimum_samples", Int(1, limits.PAIRS_MAX)),
    Field("state", Str(enum=("pending_calibration", "enabled"))),
    Field("applicable", Bool()),
)

# The seed a library instance reads (ADR-0170): its ID, the writer request's case and contract,
# and the instance parameters it is written over.
PLAN_SEED = Obj(Field("seed_id", ID), Field("case_id", ID), Field("contract_id", ID),
                Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 1, 4)))

PLAN_CASE = Obj(
    Field("instance_id", ID), Field("case_id", ID), Field("family", ID),
    Field("kind", Str(enum=("process", "library"))),
    Field("requires_linux_containment", Bool()),
    Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 0, 4)),
    Field("criticality", Str(enum=("critical", "advisory"))),
    Field("timing", TIMING), Field("cache", CACHE), Field("oracle", ORACLE),
    Field("execution", EXECUTION),
    Field("af_environment", Map(r"^AF_[A-Z0-9_]{1,62}$", Str(max_len=256), 0, 16)),
    Field("primary_metric", METRIC_REF),
    Field("optional_metrics", Arr(METRIC_REF, 0, 16)),
    Field("tolerance", Obj(Field("policy_id", ID), Field("relative", DECIMAL),
                           Field("absolute", DECIMAL),
                           Field("calibration", Str(enum=("initial", "calibrated"))))),
    Field("pairs", Int(limits.PAIRS_MIN, limits.PAIRS_MAX)),
    Field("warmup_pairs", Int(0, limits.WARMUP_PAIRS_MAX)),
    Field("rank", Int(1, limits.PAIRS_MAX)),
    Field("alpha", RATIONAL),
    Field("tail_probability", RATIONAL),
    Field("exploratory", Bool()),
    Field("shard", Int(0, limits.SHARDS_MAX - 1)),
    Field("warmup_orders", Arr(ORDER, 0, limits.WARMUP_PAIRS_MAX)),
    Field("measured_orders", Arr(ORDER, limits.PAIRS_MIN, limits.PAIRS_MAX)),
    Field("sample_timeout_ms", Int(1, limits.SAMPLE_TIMEOUT_SECONDS_MAX * 1000)),
    Field("budgets", Arr(CASE_BUDGET, 0, 8)),
    Field("seed", Nullable(PLAN_SEED)),
)

RUN_LIMITS = Obj(
    Field("sample_log_bytes", Int(1, limits.SAMPLE_LOG_BYTES_MAX)),
    Field("shard_bundle_bytes", Int(1, limits.SHARD_BUNDLE_BYTES_MAX)),
    Field("build_artifact_bytes", Int(1, limits.BUILD_ARTIFACT_BYTES_MAX)),
    Field("worktree_bytes", Int(1, limits.WORKTREE_BYTES_MAX)),
    Field("free_disk_floor_bytes", Int(limits.FREE_DISK_FLOOR_BYTES, 2 ** 62)),
    Field("build_minutes", Int(1, limits.BUILD_MINUTES_MAX)),
    Field("shard_sampling_minutes", Int(1, limits.LONG_SHARD_SAMPLING_MINUTES_MAX)),
    Field("run_total_minutes", Int(1, limits.LONG_RUN_TOTAL_MINUTES_MAX)),
    Field("concurrent_jobs", Int(1, limits.CONCURRENT_JOBS_MAX)),
    Field("record_bytes", Const(limits.RECORD_BYTES_MAX)),
)

# Build inputs bound at planning from each resolved source's Git objects, so an offline
# comparator checks the recorded builds against them instead of trusting copied strings.
BUILD_INPUT = Obj(Field("cargo_lock_sha256", DIGEST))
BUILD_ROLES = (("authority", "authority_sha"), ("baseline", "baseline_sha"),
               ("candidate", "candidate_sha"))

# How the frozen recipe builds (ADR-0170), fixed before any build: in a container of a pinned
# image (the recipe's own pin, or one configured at planning for a Linux recipe that has none),
# as an explicitly trusted Linux host process, or natively on a trusted local macOS host.
BUILD_ISOLATIONS = ("container", "trusted_host", "native_host")
PLAN_BUILD = Obj(Field("isolation", Str(enum=BUILD_ISOLATIONS)),
                 Field("image", Nullable(images.REFERENCE)),
                 Field("image_source", Nullable(Str(enum=("recipe", "configured")))))

RUN_HEALTH = Obj(
    Field("rule", Const(stats.HEALTH_RULE)), Field("workload", ID), Field("metric", ID),
    Field("unit", Const("ms")),
    Field("batch_repetitions", Int(1, 100000)),
    Field("start_pairs", Int(1, limits.SENTINEL_PAIRS_MAX)),
    Field("end_pairs", Int(1, limits.SENTINEL_PAIRS_MAX)),
    Field("max_median_shift", DECIMAL), Field("max_mad_ratio", DECIMAL),
    Field("denominator_floor", DECIMAL),
    Field("state", Str(enum=("provisional", "calibrated"))), Field("oracle", ORACLE),
)

# What a diagnostic plan freezes of its registry recipe beside the suite-shaped recipe
# (ADR-0179): the declared kind its evidence is classified by, the verdict recipe it is built
# like and its tools; null for a diagnostic plan that froze its parent's verdict recipe.
PLAN_DIAGNOSTIC_RECIPE = Obj(Field("kind", Str(enum=diagcontracts.RECIPE_KINDS)),
                             Field("base_recipe", ID),
                             Field("tools", Arr(Str(enum=diagcontracts.TOOL_NAMES), 0, 8,
                                                unique=True)))

# Calibration members (ADR-0181). Every slowdown member's candidate runs the declared loop count of
# its case and observed CPU group; the baseline, the sentinels and every A/A member run none.
MEMBER_KINDS = ("aa", "preparation", "injection", "power")
INJECTION_LEVELS = ("preparation", "injection_2", "injection_5", "injection_10", "power")
CALIBRATION_FEATURE = "perf-calibration"
CALIBRATION_ITERATIONS_MAX = 2 ** 40
PLAN_INJECTION = Obj(
    Field("feature", Const(CALIBRATION_FEATURE)),
    Field("level", Str(enum=INJECTION_LEVELS)),
    # The sealed calibration parameters a slowdown member's loop counts come from; a
    # preparation member measures the loop before they exist.
    Field("calibration_digest", Nullable(DIGEST)),
    Field("groups", Arr(Obj(Field("cpu_group", ID), Field("cpu_model", SHORT_TEXT),
                            Field("image", SHORT_TEXT),
                            Field("iterations", Map(ID_PATTERN,
                                                    Int(1, CALIBRATION_ITERATIONS_MAX), 1,
                                                    limits.INSTANCES_MAX))),
                        1, 16)))

AUTHORITY_MODES = ("active", "proposed", "control")
CPU_GROUP = Obj(Field("group_id", ID), Field("cpu_model", SHORT_TEXT), Field("image", SHORT_TEXT))

RUN = Obj(
    Field("schema", Const(RUN_SCHEMA)),
    Field("run_id", ID),
    Field("parent_run_id", Nullable(ID)),
    Field("scope", Str(enum=SCOPES)),
    Field("tier", Str(enum=TIERS)),
    Field("trigger", TRIGGER),
    Field("selectors", Obj(Field("base", Nullable(SHORT_TEXT)), Field("head", SHORT_TEXT),
                           Field("control", Nullable(SHORT_TEXT)),
                           Field("authority", Nullable(SHORT_TEXT)))),
    Field("resolved", Obj(Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
                          Field("control_sha", GIT_SHA), Field("authority_sha", GIT_SHA))),
    Field("build_inputs", Obj(*[Field(role, BUILD_INPUT) for role, _ in BUILD_ROLES])),
    Field("authority", Obj(
        Field("mode", Str(enum=AUTHORITY_MODES)),
        Field("activation", Nullable(Obj(
            Field("sha256", DIGEST), Field("admission", Str(enum=("calibrated",
                                                                    "equivalent_adapter"))),
            Field("readiness_sha256", DIGEST), Field("cohort_id", ID),
            # The exact CPU/image groups the activation's cohort calibrated (af.perf-run/7,
            # ADR-0184): a shard observed on any other host cannot satisfy the required check.
            Field("calibrated_groups", Arr(CPU_GROUP, 1, 16))))),
        Field("suite_sha256", DIGEST), Field("policy_digest", DIGEST),
        Field("instrument_digest", DIGEST), Field("fixture_digest", DIGEST))),
    Field("harness", Obj(Field("changed", Bool()),
                         Field("paths", Arr(REL_PATH, 0, 256)), Field("truncated", Bool()))),
    Field("profile", Obj(Field("environment_id", ID), Field("environment_digest", DIGEST),
                         Field("environment", ENVIRONMENT), Field("recipe_id", ID),
                         Field("recipe_digest", DIGEST), Field("recipe", RECIPE),
                         Field("build", PLAN_BUILD), Field("default", Bool()))),
    Field("verdict_policy", Obj(Field("algorithm", Const(stats.ALGORITHM)),
                                Field("family_alpha", RATIONAL),
                                Field("advisory_alpha", RATIONAL),
                                Field("family_size", Int(0, limits.INSTANCES_MAX)),
                                Field("case_alpha", RATIONAL))),
    Field("health", RUN_HEALTH),
    Field("limits", RUN_LIMITS),
    Field("seed", Int(0, 2 ** 63 - 1)),
    Field("shards", Arr(Obj(Field("index", Int(0, limits.SHARDS_MAX - 1)),
                            Field("instances", Arr(ID, 1, limits.INSTANCES_MAX))),
                        1, limits.SHARDS_MAX)),
    Field("cases", Arr(PLAN_CASE, 1, limits.INSTANCES_MAX)),
    # A calibration member's frozen identity, and for a preparation or slowdown member its
    # candidate-only injection (af.perf-run/5, ADR-0181).
    Field("cohort", Nullable(Obj(Field("cohort_id", ID), Field("cohort_digest", DIGEST),
                                 Field("member_id", ID),
                                 Field("member_kind", Str(enum=MEMBER_KINDS)),
                                 Field("injection", Nullable(PLAN_INJECTION))))),
    Field("diagnostic", Nullable(Obj(Field("parent_run_id", ID),
                                     Field("parent_plan_digest", DIGEST),
                                     Field("duration_ms", Int(1, limits.DIAGNOSTIC_SECONDS_MAX
                                                              * 1000)),
                                     Field("frequency_hz", Int(1, limits.DIAGNOSTIC_HZ_MAX)),
                                     Field("recipe", Nullable(PLAN_DIAGNOSTIC_RECIPE))))),
    Field("required_check_eligible", Bool()),
    Field("plan_digest", DIGEST),
)


def recipe_digest(recipe_id, recipe, build):
    """The resolved recipe identity (ADR-0170): the recipe and how it builds, its image
    included, so two plans whose effective build differs never share a recipe identity."""
    return canonical_digest({"id": recipe_id, "recipe": recipe, "build": build})


def check_build(recipe, build, where):
    """The frozen build is one the recipe admits: a macOS recipe builds natively; a Linux
    recipe's own pin is its image; an unpinned Linux recipe builds in an image configured at
    planning or, explicitly, as a trusted host process."""
    isolation, image, source = build["isolation"], build["image"], build["image_source"]
    require((isolation == "container") == (image is not None) == (source is not None),
            where + ": exactly a container build names its image and where the image came from")
    pinned = recipe.get("container_image")
    if recipe["os"] == "macos":
        require(isolation == "native_host", where + ": a macOS recipe builds natively on its "
                "explicitly trusted local host")
    elif pinned is not None:
        require(isolation == "container" and image == pinned and source == "recipe",
                where + ": the recipe pins build image %s, so it builds in exactly that" % pinned)
    else:
        require(isolation != "native_host" and source != "recipe",
                where + ": an unpinned Linux recipe builds in a configured image or as an "
                "explicitly trusted host process")
    if image is not None:
        images.canonical(image, where + ".image")
    return build


def plan_digest(plan):
    """Canonical digest of the plan excluding only its own digest field."""
    body = dict((key, value) for key, value in plan.items() if key != "plan_digest")
    return canonical_digest(body)


def _validate_plan_injection(plan, ids, where):
    """A calibration plan's injection: none for an A/A member, the preparation probe count for a
    preparation member, and sealed loop counts for a slowdown member, for every planned instance
    in every group it names; such a plan enforces no budget (ADR-0181)."""
    cohort = plan["cohort"]
    injection = cohort["injection"]
    at = where + ".cohort.injection"
    kind = cohort["member_kind"]
    if kind == "aa":
        require(injection is None, at + ": an A/A member is uninstrumented and injects nothing")
        return
    require(injection is not None, at + ": a %s member declares its injection" % kind)
    level = injection["level"]
    require(level == kind or (kind == "injection" and level.startswith("injection_")),
            at + ".level: %s does not belong to a %s member" % (level, kind))
    require((injection["calibration_digest"] is None) == (kind == "preparation"),
            at + ".calibration_digest: exactly a slowdown member names its sealed parameters")
    groups = [group["cpu_group"] for group in injection["groups"]]
    require(groups == sorted(set(groups)), at + ".groups: sorted unique CPU groups")
    for group in injection["groups"]:
        require(sorted(group["iterations"]) == sorted(ids),
                at + ".groups.%s.iterations: one loop count for every planned instance"
                % group["cpu_group"])
    for case in plan["cases"]:
        require(not any(budget["applicable"] for budget in case["budgets"]),
                where + ".cases.%s.budgets: an injected run enforces no budget"
                % case["instance_id"])


def validate_run(plan, where="run.json"):
    RUN.validate(plan, where)
    require(plan["plan_digest"] == plan_digest(plan), where + ".plan_digest: does not match "
            "the canonical plan bytes")
    cases = plan["cases"]
    ids = [case["instance_id"] for case in cases]
    require(ids == sorted(set(ids)), where + ".cases: instance IDs are unique and sorted")
    critical = [case for case in cases if case["criticality"] == "critical"]
    policy = plan["verdict_policy"]
    family_alpha = parse_rational(policy["family_alpha"], where + ".verdict_policy")
    advisory_alpha = parse_rational(policy["advisory_alpha"], where + ".verdict_policy")
    require(0 < family_alpha < 1 and 0 < advisory_alpha < 1,
            where + ".verdict_policy: alpha lies strictly between 0 and 1")
    require(policy["family_size"] == len(critical),
            where + ".verdict_policy.family_size: K counts exactly the critical instances")
    case_alpha = family_alpha / max(1, len(critical))
    require(parse_rational(policy["case_alpha"], where) == case_alpha,
            where + ".verdict_policy.case_alpha: must equal family_alpha / K")

    shard_count = len(plan["shards"])
    require(shard_count <= len(cases), where + ".shards: never more shards than instances")
    require([shard["index"] for shard in plan["shards"]] == list(range(shard_count)),
            where + ".shards: indexes run 0..S-1 in order")
    expected = schedule.assign_shards(ids, shard_count)
    for shard, members in zip(plan["shards"], expected):
        require(shard["instances"] == members,
                where + ".shards[%d]: not the sorted round-robin assignment" % shard["index"])
    owner = dict((ident, index) for index, members in enumerate(expected) for ident in members)

    for index, case in enumerate(cases):
        at = "%s.cases[%d]" % (where, index)
        alpha = case_alpha if case["criticality"] == "critical" else advisory_alpha
        require(case["exploratory"] == (case["criticality"] == "advisory"),
                at + ".exploratory: exactly the advisory instances are exploratory")
        require(parse_rational(case["alpha"], at) == alpha,
                at + ".alpha: critical instances use alpha/K, advisory instances alpha")
        require(case["pairs"] % 2 == 0 and case["warmup_pairs"] % 2 == 0,
                at + ": pairs and warmups come in two-pair blocks")
        rank = stats.critical_rank(case["pairs"], 1, alpha)
        require(rank is not None, at + ": no order-statistic interval exists for N=%d"
                % case["pairs"])
        require(case["rank"] == rank, at + ".rank: is not the largest admissible rank")
        require(parse_rational(case["tail_probability"], at)
                == stats.tail_probability(case["pairs"], rank),
                at + ".tail_probability: does not match the rank")
        require(case["shard"] == owner[case["instance_id"]], at + ".shard: wrong shard")
        require(case["warmup_orders"] == schedule.block_orders(
            plan["seed"], case["instance_id"], "warmup", case["warmup_pairs"]),
            at + ".warmup_orders: not the seeded AB/BA schedule")
        require(case["measured_orders"] == schedule.block_orders(
            plan["seed"], case["instance_id"], "measured", case["pairs"]),
            at + ".measured_orders: not the seeded AB/BA schedule")
        parse_decimal(case["tolerance"]["relative"], at + ".tolerance.relative")
        require(parse_decimal(case["tolerance"]["absolute"], at + ".tolerance.absolute") > 0,
                at + ".tolerance.absolute: must be strictly positive")
        keys = [metric["key"] for metric in case["optional_metrics"]]
        require(len(set(keys)) == len(keys) and case["primary_metric"]["key"] not in keys,
                at + ".optional_metrics: unique and distinct from the primary metric")
        for budget in case["budgets"]:
            require(budget["metric"] == case["primary_metric"]["key"]
                    and budget["unit"] == case["primary_metric"]["unit"],
                    at + ".budgets: a budget applies to the primary metric")
            if budget["applicable"] and budget["state"] == "enabled":
                require(case["pairs"] >= budget["minimum_samples"],
                        at + ".budgets: fewer pairs than the budget's samples")
        if case["requires_linux_containment"]:
            require(plan["profile"]["environment"]["containment"] == "linux_container",
                    at + ": the environment cannot contain this case")
        seed = case["seed"]
        if seed is not None:
            require(case["kind"] == "library", at + ".seed: only a library probe reads a seed")
            require(all(case["parameters"].get(name) == value
                        and type(case["parameters"].get(name)) is type(value)
                        for name, value in seed["parameters"].items()),
                    at + ".seed.parameters: a subset of the instance's assignment")
            require(seed["seed_id"] == suite.instance_id(seed["case_id"],
                                                         sorted(seed["parameters"].items())),
                    at + ".seed.seed_id: does not name the writer's assignment")

    run_limits = plan["limits"]
    tier_quick = plan["tier"] == "quick"
    sampling_max = (limits.QUICK_SHARD_SAMPLING_MINUTES_MAX if tier_quick
                    else limits.LONG_SHARD_SAMPLING_MINUTES_MAX)
    total_max = (limits.QUICK_RUN_TOTAL_MINUTES_MAX if tier_quick
                 else limits.LONG_RUN_TOTAL_MINUTES_MAX)
    require(run_limits["shard_sampling_minutes"] <= sampling_max
            and run_limits["run_total_minutes"] <= total_max
            and run_limits["shard_sampling_minutes"] <= run_limits["run_total_minutes"],
            where + ".limits: sampling and total deadlines exceed their hard maxima")
    require(run_limits["concurrent_jobs"] <= shard_count,
            where + ".limits.concurrent_jobs: never more concurrent jobs than shards")

    health = plan["health"]
    require(health["workload"] not in ids, where + ".health.workload: sentinels are not cases")
    for key in ("max_median_shift", "max_mad_ratio", "denominator_floor"):
        require(parse_decimal(health[key], where + ".health." + key) > 0,
                where + ".health.%s: must be positive" % key)

    scope = plan["scope"]
    mode = plan["authority"]["mode"]
    require((mode == "active") == (plan["authority"]["activation"] is not None),
            where + ".authority: exactly an active authority names its activation")
    if plan["authority"]["activation"] is not None:
        groups = plan["authority"]["activation"]["calibrated_groups"]
        ids_ = [group["group_id"] for group in groups]
        require(ids_ == sorted(set(ids_))
                and len(set((group["cpu_model"], group["image"]) for group in groups))
                == len(groups),
                where + ".authority.activation.calibrated_groups: sorted unique groups, one per "
                "CPU model and image")
    if scope == "full":
        require(mode == "active", where + ": a full plan needs an active authority")
    if scope == "calibration":
        require(mode == "proposed" and plan["cohort"] is not None,
                where + ": a calibration plan names its cohort and proposed authority")
        _validate_plan_injection(plan, ids, where)
    else:
        require(plan["cohort"] is None, where + ".cohort: only calibration plans name a cohort")
    require((scope == "diagnostic") == (plan["diagnostic"] is not None),
            where + ".diagnostic: exactly a diagnostic plan names its parent capture")
    require(plan["required_check_eligible"] == (scope == "full" and mode == "active"),
            where + ".required_check_eligible: only an active full plan is eligible")
    resolved = plan["resolved"]
    trigger = plan["trigger"]
    if trigger["kind"] == "pr":
        require(resolved["candidate_sha"] == trigger["merge_sha"]
                and resolved["baseline_sha"] == trigger["base_sha"]
                and resolved["control_sha"] == resolved["baseline_sha"],
                where + ".trigger: a PR compares its merge commit with the first parent, "
                "which is also its control revision")
    elif trigger["kind"] == "main_push":
        require(resolved["candidate_sha"] == trigger["pushed_sha"],
                where + ".trigger: a main push measures the pushed commit")
    elif trigger["kind"] == "nightly":
        require(resolved["candidate_sha"] == trigger["main_sha"],
                where + ".trigger: a nightly run measures frozen main")
    elif trigger["kind"] == "release":
        require(resolved["candidate_sha"] == trigger["tag_sha"],
                where + ".trigger: a release run measures the exact tag commit")
    inputs = plan["build_inputs"]
    for role, key in BUILD_ROLES:
        for other, other_key in BUILD_ROLES:
            if resolved[key] == resolved[other_key]:
                require(inputs[role] == inputs[other],
                        where + ".build_inputs: %s and %s name one commit, so one Cargo.lock"
                        % (role, other))
    harness = plan["harness"]
    require(harness["changed"] == bool(harness["paths"])
            and (harness["changed"] or not harness["truncated"]),
            where + ".harness: changed exactly when protected paths changed")
    require(harness["paths"] == sorted(set(harness["paths"])),
            where + ".harness.paths: sorted and unique")
    require(plan["profile"]["environment_digest"] == canonical_digest(
        {"id": plan["profile"]["environment_id"], "environment": plan["profile"]["environment"]}),
        where + ".profile.environment_digest: does not match the environment")
    profile = plan["profile"]
    check_build(profile["recipe"], profile["build"], where + ".profile.build")
    require(profile["recipe_digest"] == recipe_digest(profile["recipe_id"], profile["recipe"],
                                                      profile["build"]),
            where + ".profile.recipe_digest: does not match the recipe and its frozen build")
    frozen = plan["diagnostic"] and plan["diagnostic"]["recipe"]
    if frozen:
        at = where + ".diagnostic.recipe"
        require(profile["recipe"]["instrumented"], at + ": a frozen diagnostic recipe is an "
                "instrumented build, never the verdict recipe")
        diagcontracts.check_kind(frozen["kind"], profile["recipe"], frozen["tools"], at)
    return plan


# af.perf-builds/5 --------------------------------------------------------------------------

BUILDS_SCHEMA = "af.perf-builds/6"

LOG = Obj(Field("sha256", DIGEST), Field("bytes", COUNT), Field("truncated_bytes", COUNT))

# Every build-relevant field a recipe declares; its description and capability state are not
# build settings. A build records the settings it actually used, an optional recipe field that
# is absent as null, and the comparator requires them to equal the frozen recipe exactly.
RECIPE_SETTINGS = tuple(field.name for field in suite.RECIPE.fields
                        if field.name not in ("description", "state", "pending"))
BUILD_SETTINGS = Obj(*[Field(field.name, field.node if field.required else Nullable(field.node))
                       for field in suite.RECIPE.fields if field.name in RECIPE_SETTINGS])


def recipe_settings(recipe):
    """The build settings a frozen recipe requires, absent optional fields as null."""
    return dict((name, recipe.get(name)) for name in RECIPE_SETTINGS)


ARGV = Arr(Str(max_len=1024), 1, 64)
BUILD_ENVIRONMENT = Map(ENV_KEY, Str(max_len=1024), 0, 64)
ARTIFACT = Obj(Field("path", REL_PATH), Field("sha256", DIGEST), Field("size", COUNT))
# One compiler invocation: what ran, under which allowlisted environment, for how long and how
# it ended. A step that never ran (an adapter after the chosen one) has no execution.
STEP = Obj(
    Field("status", Str(enum=("succeeded", "failed", "timeout", "budget_exceeded",
                              "not_attempted"))),
    Field("argv", ARGV), Field("environment", BUILD_ENVIRONMENT),
    Field("deadline_seconds", Int(1, limits.BUILD_MINUTES_MAX * 60)),
    Field("elapsed_ms", COUNT), Field("exit_code", Nullable(Int(-1, 255))),
    Field("log", LOG),
)
ADAPTER_OUTCOME = Obj(
    Field("adapter_id", ID), Field("priority", Int(1, 4)), Field("equivalence_group", ID),
    Field("outcome", Str(enum=("compiled", "failed", "not_attempted"))),
    Field("step", Nullable(STEP)),
)
EMBEDDED_COMMIT = Obj(Field("expected", Str(pattern=r"^[0-9a-f]{12}$", max_len=12)),
                      Field("verified", Bool()), Field("method", Const("binary_bytes")))
OVERLAY = Obj(Field("adapter_id", ID), Field("digest", DIGEST),
              Field("files", Arr(Obj(Field("path", REL_PATH), Field("sha256", DIGEST)), 1, 32)))

# The pinned image a container build ran: the reference it was given, the local image ID that
# reference resolved to, and the platform the image declares.
BUILD_IMAGE = Obj(Field("reference", images.REFERENCE),
                  Field("id", DIGEST),
                  Field("platform", Str(pattern=r"^linux/[a-z0-9_]+$", max_len=64)))

BUILD_SIDE = Obj(
    Field("source_sha", GIT_SHA),
    Field("status", Str(enum=("succeeded", "failed"))),
    Field("worktree", Str(enum=("base", "head"))),
    Field("shared_with", Nullable(Const("baseline"))),
    Field("adapter_outcomes", Arr(ADAPTER_OUTCOME, 0, 4)),
    Field("chosen_adapter", Nullable(ID)),
    Field("equivalence_group", Nullable(ID)),
    Field("probe_coverage", Str(enum=("available", "unavailable"))),
    Field("coverage_reason", Nullable(TEXT)),
    Field("overlay", Nullable(OVERLAY)),
    Field("binaries", Obj(Field("af", Nullable(DIGEST)), Field("probe", Nullable(DIGEST)))),
    Field("artifacts", Obj(Field("af", Nullable(ARTIFACT)), Field("probe", Nullable(ARTIFACT)))),
    Field("embedded_commit", Nullable(EMBEDDED_COMMIT)),
    Field("source_unchanged", Bool()),
    Field("cargo_lock_sha256", Nullable(DIGEST)),
    Field("settings", BUILD_SETTINGS),
    Field("argv", ARGV),
    Field("environment", BUILD_ENVIRONMENT),
    Field("product", Nullable(STEP)),
    Field("log", LOG),
)

UNIX_MS = Int(0, 2 ** 53)
REPOSITORY = Str(pattern=r"^[A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100}$", max_len=201)
# The run's one execution start and absolute total deadline (ADR-0170): established when its
# build begins, as the start plus the plan's frozen run_total_minutes, and never reset by a later
# command; a caller's own deadline only tightens what a command itself may use.
RUN_EXECUTION = Obj(Field("started_unix_ms", UNIX_MS), Field("deadline_unix_ms", UNIX_MS),
                    Field("total_minutes", Int(1, limits.LONG_RUN_TOTAL_MINUTES_MAX)))
BUILD_EXECUTION = Obj(Field("run", RUN_EXECUTION),
                      Field("caller_deadline_unix_ms", Nullable(UNIX_MS)),
                      Field("deadline_unix_ms", UNIX_MS),
                      Field("ended_unix_ms", UNIX_MS))

BUILD_CACHE_ROLES = ("authority", "base", "head")
# The compiled build cache a run built against (af.perf-builds/6, ADR-0185), observed, never
# requested: cold starts from worktree targets observed empty; warm first runs a bounded priming
# build of the same sources, recipe, toolchain and protected inputs into the run's own worktree
# targets, then measures the build pass against them. Nothing outside the run's own directory is
# ever a build cache, and dependency preparation (`isolation.dependency_fetch`) is not one.
BUILD_CACHE = Obj(
    Field("treatment", Str(enum=("cold", "warm"))),
    Field("scope", Const("run_worktree_targets")),
    # The cache's identity: the plan, recipe, toolchain and each worktree's Cargo.lock.
    Field("identity", DIGEST),
    Field("priming", Nullable(Obj(Field("started_unix_ms", UNIX_MS),
                                  Field("ended_unix_ms", UNIX_MS),
                                  Field("steps", Arr(Obj(Field("name", ID), Field("step", STEP)),
                                                     1, 8))))),
    # Each worktree target as the measured pass found it: empty, or primed with its bytes.
    Field("observed", Arr(Obj(Field("worktree", Str(enum=BUILD_CACHE_ROLES)),
                              Field("state", Str(enum=("empty", "primed"))),
                              Field("compiled_bytes", COUNT)), 1, 3)),
    Field("measured_started_unix_ms", UNIX_MS),
)

BUILDS = Obj(
    Field("schema", Const(BUILDS_SCHEMA)), Field("run_id", ID), Field("plan_digest", DIGEST),
    Field("recipe_id", ID), Field("recipe_digest", DIGEST),
    Field("provenance", Str(enum=("measured", "synthetic"))),
    Field("execution", BUILD_EXECUTION),
    Field("build_cache", BUILD_CACHE),
    # How the build was isolated (ADR-0169): an explicitly trusted host process, or a build
    # container of a pinned image, without network for every compile and with only the run's
    # own directories mounted. Trusted host mode never claims filesystem isolation.
    Field("isolation", Obj(Field("kind", Str(enum=("host_process", "container"))),
                           Field("environment", Const("allowlist")),
                           Field("credential_free_git", Bool()),
                           Field("cargo_offline", Bool()),
                           Field("image", Nullable(BUILD_IMAGE)),
                           Field("compile_network", Str(enum=("host", "none"))),
                           Field("dependency_fetch",
                                 Str(enum=("prefetched", "host_network", "container_network"))),
                           Field("filesystem", Str(enum=("trusted_host", "run_mounts_only"))),
                           Field("detail", TEXT))),
    Field("toolchain", Obj(Field("rustc", SHORT_TEXT), Field("cargo", SHORT_TEXT),
                           Field("host", SHORT_TEXT))),
    Field("linker", Obj(Field("kind", Str(enum=("system_cc", "zig_cc", "apple_ld"))),
                        Field("identity", Nullable(SHORT_TEXT)),
                        Field("unavailable", Nullable(SHORT_TEXT)))),
    Field("supervisor", Obj(Field("source_sha", GIT_SHA),
                            Field("status", Str(enum=("succeeded", "failed"))),
                            Field("binary_sha256", Nullable(DIGEST)),
                            Field("artifact", Nullable(ARTIFACT)),
                            Field("source_unchanged", Bool()),
                            Field("cargo_lock_sha256", Nullable(DIGEST)),
                            Field("settings", BUILD_SETTINGS), Field("step", Nullable(STEP)),
                            Field("log", LOG))),
    Field("sides", Obj(Field("baseline", BUILD_SIDE), Field("candidate", BUILD_SIDE))),
    Field("adapter_binding", Obj(Field("baseline", Nullable(ID)), Field("candidate", Nullable(ID)),
                                 Field("baseline_group", Nullable(ID)),
                                 Field("candidate_group", Nullable(ID)),
                                 Field("equivalent", Bool()))),
    Field("artifacts", Obj(Field("manifest_sha256", Nullable(DIGEST)), Field("bytes", COUNT),
                           Field("limit_bytes", Int(1, limits.BUILD_ARTIFACT_BYTES_MAX)))),
    Field("budgets", Obj(Field("worktree_limit_bytes", Int(1, limits.WORKTREE_BYTES_MAX)),
                         Field("worktree_peak_bytes", COUNT),
                         Field("build_log_limit_bytes", Int(1, limits.BUILD_LOG_BYTES_MAX)))),
    Field("cleanup", Obj(Field("status", Str(enum=("removed", "retained", "failed"))),
                         Field("detail", TEXT))),
)

SECRET_MARKERS = ("TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "PRIVATE_KEY")


def _check_build_environment(environment, at):
    for key in environment:
        require(not any(marker in key for marker in SECRET_MARKERS),
                at + ": %s is not a sanitized build variable" % key)


def _check_artifact(artifact, digest, at):
    require((artifact is None) == (digest is None),
            at + ": an artifact exactly for a hashed binary")
    if artifact is not None:
        require(artifact["sha256"] == digest, at + ": the artifact is not the hashed binary")
        require(artifact["path"].startswith("artifacts/"),
                at + ": artifacts live below the run's artifacts/ directory")


def check_run_execution(run, where):
    require(run["deadline_unix_ms"] == run["started_unix_ms"] + run["total_minutes"] * 60000,
            where + ": the run's deadline is its start plus its frozen total minutes")
    return run


def effective_deadline(run_deadline, caller):
    return run_deadline if caller is None else min(run_deadline, caller)


def validate_builds(doc, where="builds.json"):
    BUILDS.validate(doc, where)
    execution = doc["execution"]
    check_run_execution(execution["run"], where + ".execution.run")
    require(execution["deadline_unix_ms"] == effective_deadline(
        execution["run"]["deadline_unix_ms"], execution["caller_deadline_unix_ms"]),
        where + ".execution.deadline_unix_ms: the earlier of the run's and the caller's")
    require(execution["ended_unix_ms"] >= execution["run"]["started_unix_ms"],
            where + ".execution: the build ends after the run starts")
    cache = doc["build_cache"]
    at = where + ".build_cache"
    roles = [item["worktree"] for item in cache["observed"]]
    require(roles == sorted(set(roles), key=BUILD_CACHE_ROLES.index),
            at + ".observed: each worktree once, in order")
    require(cache["measured_started_unix_ms"] <= execution["ended_unix_ms"],
            at + ".measured_started_unix_ms: the measured pass starts before the build ends")
    if cache["treatment"] == "cold":
        require(cache["priming"] is None and all(
            item["state"] == "empty" and item["compiled_bytes"] == 0
            for item in cache["observed"]),
            at + ": a cold build primes nothing and starts from observed empty targets")
    else:
        priming = cache["priming"]
        require(priming is not None and all(item["step"]["status"] == "succeeded"
                                            for item in priming["steps"])
                and execution["run"]["started_unix_ms"] <= priming["started_unix_ms"]
                <= priming["ended_unix_ms"] <= cache["measured_started_unix_ms"],
                at + ".priming: a warm build's priming succeeded before its measured pass")
        names = [item["name"] for item in priming["steps"]]
        require(len(set(names)) == len(names), at + ".priming.steps: each step once")
        for item in priming["steps"]:
            _check_build_environment(item["step"]["environment"], at + ".priming.steps")
        require(all(item["state"] == "primed" and item["compiled_bytes"] > 0
                    for item in cache["observed"]),
                at + ": a warm build's measured pass found every target primed")
    supervisor = doc["supervisor"]
    require((supervisor["status"] == "succeeded") == (supervisor["binary_sha256"] is not None),
            where + ".supervisor: a hash exactly for a successful supervisor build")
    require(supervisor["status"] != "succeeded" or (supervisor["cargo_lock_sha256"] is not None
                                                   and supervisor["source_unchanged"]),
            where + ".supervisor.cargo_lock_sha256: a successful build binds its Cargo.lock "
            "and left its source unchanged")
    _check_artifact(supervisor["artifact"], supervisor["binary_sha256"], where + ".supervisor")
    if supervisor["step"] is not None:
        _check_build_environment(supervisor["step"]["environment"],
                                 where + ".supervisor.step.environment")
    groups = {}
    for side in SIDES:
        entry = doc["sides"][side]
        at = where + ".sides." + side
        outcomes = entry["adapter_outcomes"]
        priorities = [item["priority"] for item in outcomes]
        require(priorities == sorted(set(priorities)),
                at + ".adapter_outcomes: unique fixed priorities in order")
        require(len(set(item["adapter_id"] for item in outcomes)) == len(outcomes),
                at + ".adapter_outcomes: unique adapters")
        compiled = [item["adapter_id"] for item in outcomes if item["outcome"] == "compiled"]
        require(entry["chosen_adapter"] == (compiled[0] if compiled else None),
                at + ".chosen_adapter: the first compiling adapter by priority, or none")
        require(len(compiled) <= 1, at + ".adapter_outcomes: compilation stops at the first "
                "compiling adapter")
        seen_compiled = False
        for item in outcomes:
            if seen_compiled:
                require(item["outcome"] == "not_attempted",
                        at + ".adapter_outcomes: no adapter after the chosen one is attempted")
            require((item["outcome"] == "not_attempted") == (item["step"] is None),
                    at + ".adapter_outcomes: exactly an attempted adapter records its step")
            if item["step"] is not None:
                _check_build_environment(item["step"]["environment"], at + ".adapter_outcomes")
            seen_compiled = seen_compiled or item["outcome"] == "compiled"
        chosen = [item for item in outcomes if item["outcome"] == "compiled"]
        require(entry["equivalence_group"] == (chosen[0]["equivalence_group"] if chosen
                                               else None),
                at + ".equivalence_group: the chosen adapter's group, named exactly with it")
        require((entry["overlay"] is not None) == (entry["chosen_adapter"] is not None)
                and (entry["overlay"] is None
                     or entry["overlay"]["adapter_id"] == entry["chosen_adapter"]),
                at + ".overlay: exactly the chosen adapter's overlay")
        require((entry["probe_coverage"] == "available") == (entry["binaries"]["probe"]
                                                            is not None)
                and (entry["coverage_reason"] is None) == (entry["probe_coverage"]
                                                          == "available"),
                at + ".probe_coverage: available exactly with a probe binary, otherwise with "
                "its reason")
        if entry["status"] == "succeeded":
            require(entry["binaries"]["af"] is not None,
                    at + ".binaries.af: a successful build hashes its AF binary")
            require(entry["cargo_lock_sha256"] is not None,
                    at + ".cargo_lock_sha256: a successful build binds its Cargo.lock")
            require(entry["embedded_commit"] is not None
                    and entry["embedded_commit"]["verified"]
                    and entry["source_sha"].startswith(entry["embedded_commit"]["expected"]),
                    at + ".embedded_commit: a successful build embeds its own source commit")
            require(entry["source_unchanged"],
                    at + ".source_unchanged: a successful build left its source and lock as "
                    "checked out, apart from the authority overlay")
        require(entry["binaries"]["probe"] is None or entry["chosen_adapter"] is not None,
                at + ".binaries.probe: a probe binary comes from a chosen adapter")
        for key in ("af", "probe"):
            _check_artifact(entry["artifacts"][key], entry["binaries"][key], at + "." + key)
        _check_build_environment(entry["environment"], at + ".environment")
        if entry["product"] is not None:
            _check_build_environment(entry["product"]["environment"], at + ".product")
        groups[side] = (entry["chosen_adapter"], entry["equivalence_group"])
    base, cand = doc["sides"]["baseline"], doc["sides"]["candidate"]
    require(base["shared_with"] is None and base["worktree"] == "base"
            and cand["worktree"] == "head",
            where + ".sides: the baseline is built in base and the candidate in head")
    if cand["shared_with"] is not None:
        same = ("source_sha", "status", "chosen_adapter", "equivalence_group", "overlay",
                "binaries", "artifacts", "cargo_lock_sha256", "settings")
        require(all(base[key] == cand[key] for key in same),
                where + ".sides.candidate.shared_with: one artifact serves both roles only for "
                "identical frozen build inputs")
    binding = doc["adapter_binding"]
    require((binding["baseline"], binding["baseline_group"]) == groups["baseline"]
            and (binding["candidate"], binding["candidate_group"]) == groups["candidate"],
            where + ".adapter_binding: records both sides' selections")
    require(binding["equivalent"] == (binding["baseline_group"] is not None
                                      and binding["baseline_group"]
                                      == binding["candidate_group"]),
            where + ".adapter_binding.equivalent: exactly when both sides chose one group")
    require((doc["linker"]["identity"] is None) != (doc["linker"]["unavailable"] is None),
            where + ".linker: identified, or with the reason it is not")
    isolation = doc["isolation"]
    contained = isolation["kind"] == "container"
    require(contained == (isolation["image"] is not None)
            == (isolation["compile_network"] == "none")
            == (isolation["filesystem"] == "run_mounts_only")
            and isolation["dependency_fetch"] != ("host_network" if contained
                                                  else "container_network"),
            where + ".isolation: a container build names its pinned image, compiles without "
            "network and mounts only the run's directories; a trusted host build claims none "
            "of these")
    return doc


# af.perf-sample/2 --------------------------------------------------------------------------

SAMPLE_SCHEMA = "af.perf-sample/2"
# What each reported metric measured (ADR-0168). The primary metric's scope is the case's timing
# boundary; fixture setup, OS accounting and cgroup figures are never mistaken for it.
METRIC_SCOPES = ("child_process_lifetime", "library_region", "probe_fixture_setup",
                 "reaped_child_rusage", "sample_cgroup", "probe_reported")
SAMPLE_TERMINATIONS = TERMINATIONS + ("setup_failed",)
ORACLE_KINDS = suite.ORACLE.fields[0].node.enum

SAMPLE = Obj(
    Field("schema", Const(SAMPLE_SCHEMA)), Field("run_id", ID), Field("plan_digest", DIGEST),
    Field("shard", Int(0, limits.SHARDS_MAX - 1)),
    Field("sequence", Int(0, 10 ** 6)),
    Field("phase", Str(enum=schedule.PHASES)),
    Field("instance_id", ID), Field("pair", Int(1, limits.PAIRS_MAX)),
    Field("position", Int(1, 2)), Field("side", Str(enum=SIDES)),
    Field("source_sha", GIT_SHA), Field("binary_sha256", DIGEST), Field("host_id", DIGEST),
    Field("started_unix_ms", Int(0, 2 ** 53)),
    Field("status", Str(enum=("ok", "failed"))),
    # Which workload contract ran, through which adapter, with which parameters and fixture.
    Field("identity", Obj(Field("case_id", ID), Field("contract_id", Nullable(ID)),
                          Field("adapter", Nullable(ID)),
                          Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 0, 4)),
                          Field("fixture_digest", Nullable(DIGEST)))),
    Field("metrics", Map(ID_PATTERN, METRIC, 0, 32)),
    Field("metric_scopes", Map(ID_PATTERN, Str(enum=METRIC_SCOPES), 0, 32)),
    Field("unavailable_metrics", Map(ID_PATTERN, Str(min_len=1, max_len=256), 0, 32)),
    Field("oracle", Obj(Field("status", Str(enum=("passed", "failed", "not_run"))),
                        Field("kind", Str(enum=ORACLE_KINDS)),
                        Field("observed_digest", Nullable(DIGEST)),
                        Field("detail", Str(max_len=1024)))),
    Field("execution", Obj(Field("containment", Str(enum=CONTAINMENTS)),
                           Field("termination", Nullable(Str(enum=SAMPLE_TERMINATIONS))),
                           Field("exit_code", Nullable(Int(-1, 255))),
                           Field("signal", Nullable(Int(1, 128))),
                           Field("exit_observation", Nullable(Str(enum=EXIT_OBSERVATIONS))),
                           Field("lifecycle", Nullable(Str(enum=LIFECYCLE_STATUSES))),
                           Field("host_wall_ns", Nullable(NANOS)),
                           Field("fixture_setup_ns", Nullable(NANOS)))),
    Field("failure", Nullable(Obj(Field("kind", Str(enum=FAILURE_KINDS)),
                                  Field("detail", TEXT)))),
    Field("logs", Obj(Field("stdout_bytes", COUNT), Field("stderr_bytes", COUNT),
                      Field("stdout_sha256", DIGEST), Field("stderr_sha256", DIGEST),
                      Field("truncated_bytes", COUNT),
                      Field("retained", Nullable(REL_PATH)))),
)


def validate_sample(record, where="sample"):
    SAMPLE.validate(record, where)
    failure = record["failure"]
    if record["status"] == "ok":
        require(failure is None and record["oracle"]["status"] == "passed",
                where + ": a successful sample passed its oracle and records no failure")
        require(record["execution"]["termination"] == "exited",
                where + ".execution: a successful sample's child exited by itself")
    else:
        require(failure is not None, where + ": a failed sample names its failure")
        if failure["kind"] == "oracle_mismatch":
            require(record["oracle"]["status"] == "failed",
                    where + ": an oracle mismatch records a failed oracle")
    overlap = set(record["metrics"]) & set(record["unavailable_metrics"])
    require(not overlap, where + ": metric %s is both present and unavailable"
            % ", ".join(sorted(overlap)))
    require(sorted(record["metric_scopes"]) == sorted(record["metrics"]),
            where + ".metric_scopes: names the scope of exactly the reported metrics")
    execution = record["execution"]
    require((execution["exit_code"] is not None) == (execution["termination"] == "exited"),
            where + ".execution.exit_code: exactly for an exit")
    require((execution["signal"] is not None)
            == (execution["termination"] in ("signalled", "cancelled")),
            where + ".execution.signal: exactly for a signal or a cancellation")
    require(execution["termination"] is not None or execution["exit_observation"] is None,
            where + ".execution: an exit observation belongs to a reported termination")
    logs = record["logs"]
    require(logs["retained"] is None or logs["retained"].startswith("logs/"),
            where + ".logs.retained: retained logs live below the shard's logs/")
    if record["phase"] in ("sentinel_start", "sentinel_end"):
        require(record["side"] == schedule.BASELINE,
                where + ": sentinels run the baseline binary on both positions")
    return record


def read_samples(data, where="samples.jsonl"):
    """Parse a JSONL sample stream: every line one record of at most 64 KiB."""
    require(len(data) <= limits.MAX_SAMPLES_BYTES, where + ": exceeds the shard bundle bound")
    if not data:
        return []
    require(data.endswith(b"\n"), where + ": the last record ends with a newline")
    records = []
    for number, line in enumerate(data[:-1].split(b"\n"), 1):
        at = "%s:%d" % (where, number)
        require(len(line) + 1 <= limits.RECORD_BYTES_MAX, at + ": a record exceeds 64 KiB")
        record = loads_strict(line, at, limits.RECORD_BYTES_MAX)
        require(isinstance(record, dict) and record.get("schema") == SAMPLE_SCHEMA,
                at + ".schema: expected %s" % SAMPLE_SCHEMA)
        validate_sample(record, at)
        require(canonical_bytes(record) == line, at + ": a record is canonical JSON")
        records.append(record)
    return records


def samples_bytes(records):
    return b"".join(document_bytes(validate_sample(record)) for record in records)


# af.perf-shard/4 ---------------------------------------------------------------------------

SHARD_SCHEMA = "af.perf-shard/4"

# A host observation is its value, or explicitly unknown with the reason; never an invented
# zero (ADR-0168).
OBSERVATION = AnyOf(Obj(Field("value", Str(min_len=1, max_len=1024))),
                    Obj(Field("unknown", Str(min_len=1, max_len=1024))))
HOST_KEYS = ("cpu_model", "cpu_topology", "logical_cpus", "kernel", "os_image", "libc",
             "memory_bytes", "virtualization", "microcode", "container_runtime", "affinity",
             "cpu_quota", "cache", "aslr", "python")
HOST = Obj(*[Field(key, OBSERVATION) for key in HOST_KEYS])
# Interference observed at the start and the end of a shard's schedule.
LOAD_KEYS = ("load_average", "cpu_steal", "cpu_pressure", "memory_pressure", "io_pressure",
             "cpu_throttling", "memory_events")
LOAD = Obj(*[Field(key, OBSERVATION) for key in LOAD_KEYS])

# The cli/setup.sh steps that write a side's seed, once per shard (ADR-0169).
CLI_SEED_WRITERS = ("seed_task_state", "seed_campaign_state")
# One side's seed in a shard: written once by that side's own writers under the supervisor,
# published, hashed and sealed; each sample that needs it copies it privately. A `cli` seed's
# writer is a cli/setup.sh step (ADR-0169); a `native` seed's is the probe's writer contract,
# whose own check the driver judged against the golden seed entry (ADR-0170).
SEED_FAILURE_KINDS = ("launch_failure", "timeout", "lifecycle_violation", "fixture_failed",
                      "oracle_mismatch", "driver")
SEED = Obj(
    Field("side", Str(enum=SIDES)), Field("seed", ID),
    Field("kind", Str(enum=("cli", "native"))),
    Field("writer", ID),
    Field("status", Str(enum=("ready", "failed"))),
    Field("af_sha256", Nullable(DIGEST)), Field("probe_sha256", DIGEST),
    Field("digest", Nullable(DIGEST)), Field("files", Nullable(COUNT)),
    Field("bytes", Nullable(COUNT)),
    Field("writer_wall_ns", Nullable(NANOS)), Field("host_wall_ns", Nullable(NANOS)),
    Field("termination", Nullable(Str(enum=SAMPLE_TERMINATIONS))),
    Field("lifecycle", Nullable(Str(enum=LIFECYCLE_STATUSES))),
    Field("samples", COUNT), Field("unchanged", Bool()),
    Field("failure", Nullable(Obj(Field("kind", Str(enum=SEED_FAILURE_KINDS)),
                                  Field("detail", TEXT)))),
)

SHARD = Obj(
    Field("schema", Const(SHARD_SCHEMA)), Field("run_id", ID), Field("plan_digest", DIGEST),
    Field("shard", Int(0, limits.SHARDS_MAX - 1)),
    Field("status", Str(enum=("complete", "failed", "interrupted"))),
    Field("provenance", Str(enum=("measured", "synthetic"))),
    Field("host_id", DIGEST), Field("host", HOST),
    Field("execution", Obj(
        Field("mode", Str(enum=("trusted_local", "linux_container", "synthetic"))),
        Field("containment", Str(enum=CONTAINMENTS)),
        Field("sample_image", Nullable(SHORT_TEXT)),
        Field("supervisor_sha256", DIGEST), Field("builds_sha256", Nullable(DIGEST)),
        Field("sampling_deadline_ms", Int(1, limits.LONG_SHARD_SAMPLING_MINUTES_MAX * 60000)),
        # The run's execution start and total deadline the shard read from its verified build,
        # a caller's tighter deadline, and the earliest of them and the sampling deadline, which
        # every slot's worst case was checked against before it started (ADR-0170).
        Field("run", RUN_EXECUTION),
        Field("caller_deadline_unix_ms", Nullable(UNIX_MS)),
        Field("deadline_unix_ms", UNIX_MS),
        Field("sample_cpus", Nullable(Arr(Int(0, 65535), 1, 256, unique=True))),
        # The machine's coordination lock the shard held from before its first shared write
        # until this record (ADR-0169); a synthetic shard held none.
        Field("coordination_lock", Nullable(Str(pattern=r"^/", min_len=1, max_len=4096))))),
    Field("seeds", Arr(SEED, 0, 16)),
    Field("observations", Obj(Field("start", LOAD), Field("end", LOAD))),
    Field("expected_records", COUNT), Field("record_count", COUNT),
    Field("slots", Obj(Field("failed", COUNT), Field("unstarted", COUNT))),
    Field("samples_sha256", DIGEST), Field("samples_bytes", COUNT),
    Field("logs_bytes", COUNT),
    Field("started_unix_ms", Int(0, 2 ** 53)), Field("ended_unix_ms", Int(0, 2 ** 53)),
    Field("failure", Nullable(Obj(Field("reason", ID), Field("detail", TEXT)))),
)


def host_id(host):
    return canonical_digest(host)


def validate_shard(doc, where="shard.json"):
    SHARD.validate(doc, where)
    require(doc["host_id"] == host_id(doc["host"]), where + ".host_id: does not match the host")
    require(doc["ended_unix_ms"] >= doc["started_unix_ms"], where + ": ends before it starts")
    execution = doc["execution"]
    check_run_execution(execution["run"], where + ".execution.run")
    require(execution["deadline_unix_ms"] == min(
        effective_deadline(execution["run"]["deadline_unix_ms"],
                           execution["caller_deadline_unix_ms"]),
        doc["started_unix_ms"] + execution["sampling_deadline_ms"]),
        where + ".execution.deadline_unix_ms: the earliest of the run's total deadline, the "
        "caller's and the shard's sampling deadline")
    require(doc["started_unix_ms"] >= execution["run"]["started_unix_ms"],
            where + ".started_unix_ms: a shard starts within its run")
    require((doc["status"] == "complete") == (doc["failure"] is None),
            where + ".failure: named exactly for a failed or interrupted shard")
    require(doc["record_count"] <= doc["expected_records"],
            where + ": more records than the plan expects")
    slots = doc["slots"]
    require(doc["record_count"] + slots["unstarted"] == doc["expected_records"],
            where + ".slots.unstarted: every planned slot without a record is unstarted")
    require(slots["failed"] <= doc["record_count"],
            where + ".slots.failed: failed samples are recorded ones")
    if doc["status"] == "complete":
        require(doc["record_count"] == doc["expected_records"],
                where + ": a complete shard holds every expected record")
    require((doc["provenance"] == "synthetic") == (doc["execution"]["mode"] == "synthetic"),
            where + ".execution.mode: exactly synthetic evidence was not executed")
    require(doc["provenance"] == "synthetic" or doc["execution"]["builds_sha256"] is not None,
            where + ".execution.builds_sha256: measured evidence names the build it ran")
    if doc["execution"]["mode"] == "linux_container":
        require(doc["execution"]["containment"] == "pid_namespace"
                and doc["execution"]["sample_image"] is not None,
                where + ".execution: a Linux container shard names its pinned image and runs "
                "each sample as a PID namespace")
    if doc["execution"]["mode"] == "trusted_local":
        require(doc["execution"]["containment"] == "process_group",
                where + ".execution: trusted local samples run in a process group")
    require((doc["provenance"] == "measured")
            == (doc["execution"]["coordination_lock"] is not None),
            where + ".execution.coordination_lock: named exactly by measured evidence")
    seen = set()
    for index, seed in enumerate(doc["seeds"]):
        at = "%s.seeds[%d]" % (where, index)
        key = (seed["side"], seed["seed"])
        require(key not in seen, at + ": one record per side and seed")
        seen.add(key)
        if seed["kind"] == "cli":
            require(seed["writer"] in CLI_SEED_WRITERS and seed["af_sha256"] is not None,
                    at + ": a CLI seed is written by a cli/setup.sh writer with the side's AF")
        else:
            require(seed["af_sha256"] is None and seed["writer"] not in CLI_SEED_WRITERS,
                    at + ": a native seed is written by the side's probe alone")
        ready = seed["status"] == "ready"
        require(ready == (seed["digest"] is not None) == (seed["files"] is not None)
                == (seed["bytes"] is not None) == (seed["failure"] is None),
                at + ": a ready seed has its digest, size and no failure; a failed one only "
                "its failure")
        require(ready or seed["samples"] == 0, at + ".samples: only a ready seed is copied")
        require(seed["unchanged"] or doc["status"] != "complete",
                at + ".unchanged: a seed that changed stops its shard")
    require(doc["provenance"] == "measured" or not doc["seeds"],
            where + ".seeds: only measured evidence writes seeds")
    return doc


# af.perf-manifest/1 ------------------------------------------------------------------------

MANIFEST_SCHEMA = "af.perf-manifest/1"

MANIFEST = Obj(
    Field("schema", Const(MANIFEST_SCHEMA)),
    Field("entries", Arr(Obj(Field("path", REL_PATH), Field("size", COUNT),
                             Field("sha256", DIGEST)), 1, 4096)),
)


def validate_manifest(doc, where="manifest.json"):
    MANIFEST.validate(doc, where)
    paths = [entry["path"] for entry in doc["entries"]]
    require(paths == sorted(set(paths)), where + ".entries: paths are unique and sorted")
    require("manifest.json" not in paths, where + ": a manifest does not list itself")
    for path in paths:
        require(not any(other.startswith(path + "/") for other in paths),
                where + ": %s is both a file and a directory" % path)
    return doc


# af.perf-verdict/2 -------------------------------------------------------------------------

VERDICT_SCHEMA = "af.perf-verdict/2"
# What a complete readiness member records per case. A case on a shard that did not complete
# belongs to a member that is not complete, so readiness never records `shard_not_complete`.
READINESS_CLASSIFICATIONS = (stats.REGRESSION, stats.IMPROVEMENT, stats.WITHIN_TOLERANCE,
                             stats.NON_REGRESSION, stats.INCONCLUSIVE, "candidate_failure",
                             "baseline_failure", "environment_unstable", "missing_records",
                             "budget_missed")
CASE_STATUSES = READINESS_CLASSIFICATIONS + ("shard_not_complete",)
SHARD_STATUSES = ("complete", "failed", "interrupted")
TERMINAL_FAILURE = Obj(Field("reason", ID), Field("detail", TEXT))
SUMMARY = Nullable(Obj(Field("median", RATIONAL), Field("p95", RATIONAL),
                       Field("max", RATIONAL)))
REASON = Obj(Field("code", ID), Field("shard", Nullable(Int(0, limits.SHARDS_MAX - 1))),
             Field("instance_id", Nullable(ID)), Field("detail", TEXT))

VERDICT = Obj(
    Field("schema", Const(VERDICT_SCHEMA)), Field("run_id", ID), Field("plan_digest", DIGEST),
    Field("scope", Str(enum=SCOPES)),
    Field("provenance", Str(enum=("measured", "synthetic", "mixed", "none"))),
    Field("inputs", Obj(Field("run_bundle", Nullable(DIGEST)),
                        Field("builds_sha256", Nullable(DIGEST)),
                        Field("shard_bundles", Arr(Obj(
                            Field("shard", Int(0, limits.SHARDS_MAX - 1)),
                            Field("bundle", Nullable(DIGEST))), 1, limits.SHARDS_MAX)))),
    # Each planned shard's terminal record as its shard.json states it (null when no shard.json
    # of this run and shard was readable) and how many planned slots one accepted record matched.
    Field("shards", Arr(Obj(
        Field("shard", Int(0, limits.SHARDS_MAX - 1)),
        Field("status", Nullable(Str(enum=SHARD_STATUSES))),
        Field("failure", Nullable(TERMINAL_FAILURE)),
        Field("expected_records", COUNT), Field("matched_records", COUNT)),
        1, limits.SHARDS_MAX)),
    Field("algorithm", Const(stats.ALGORITHM)),
    Field("family_alpha", RATIONAL), Field("family_size", Int(0, limits.INSTANCES_MAX)),
    Field("case_alpha", RATIONAL),
    Field("health", Arr(Obj(
        Field("shard", Int(0, limits.SHARDS_MAX - 1)),
        Field("status", Str(enum=("healthy", "unstable"))),
        Field("flags", Arr(ID, 0, 8)),
        Field("start_median", Nullable(RATIONAL)), Field("end_median", Nullable(RATIONAL)),
        Field("shift", Nullable(RATIONAL)), Field("start_mad_ratio", Nullable(RATIONAL)),
        Field("end_mad_ratio", Nullable(RATIONAL))), 1, limits.SHARDS_MAX)),
    Field("cases", Arr(Obj(
        Field("instance_id", ID), Field("criticality", Str(enum=("critical", "advisory"))),
        Field("exploratory", Bool()), Field("shard", Int(0, limits.SHARDS_MAX - 1)),
        Field("pairs", Int(limits.PAIRS_MIN, limits.PAIRS_MAX)),
        Field("rank", Int(1, limits.PAIRS_MAX)), Field("alpha", RATIONAL),
        Field("metric", METRIC_REF),
        Field("tolerance", Obj(Field("relative", DECIMAL), Field("absolute", DECIMAL))),
        Field("status", Str(enum=CASE_STATUSES)),
        Field("statistical_classification", Nullable(Str(enum=(
            stats.REGRESSION, stats.IMPROVEMENT, stats.WITHIN_TOLERANCE, stats.NON_REGRESSION,
            stats.INCONCLUSIVE)))),
        Field("outcome", Str(enum=("pass", "fail", "incomplete"))),
        Field("interval", Nullable(Obj(Field("lower", RATIONAL), Field("upper", RATIONAL)))),
        Field("baseline", SUMMARY), Field("candidate", SUMMARY),
        Field("change", Nullable(Obj(Field("absolute", RATIONAL),
                                     Field("percent", Nullable(RATIONAL))))),
        Field("missing_records", COUNT),
        Field("failures", Arr(Obj(Field("side", Str(enum=SIDES)),
                                  Field("phase", Str(enum=("warmup", "measured"))),
                                  Field("pair", Int(1, limits.PAIRS_MAX)),
                                  Field("position", Int(1, 2)),
                                  Field("kind", Str(enum=FAILURE_KINDS))), 0, 1024)),
        Field("budgets", Arr(Obj(
            Field("budget_id", ID), Field("state", Str(enum=("pending_calibration",
                                                             "enabled"))),
            Field("applicable", Bool()), Field("limit", DECIMAL),
            Field("result", Str(enum=("met", "missed", "not_evaluated", "pending_calibration",
                                      "not_applicable"))),
            Field("observed_max", Nullable(RATIONAL))), 0, 8)),
    ), 1, limits.INSTANCES_MAX)),
    Field("aggregate", Obj(
        Field("result", Str(enum=("pass", "fail", "incomplete"))),
        Field("exit_code", Int(0, 4)),
        Field("satisfies_required_check", Bool()),
        Field("coverage", Obj(Field("expected_records", COUNT), Field("matched_records", COUNT),
                              Field("missing_records", COUNT))),
        Field("reasons", Arr(REASON, 0, 4096)))),
)

EXIT_CODES = {"pass": 0, "fail": 3, "incomplete": 4}


def validate_verdict(doc, where="verdict.json"):
    VERDICT.validate(doc, where)
    aggregate = doc["aggregate"]
    require(aggregate["exit_code"] == EXIT_CODES[aggregate["result"]],
            where + ".aggregate.exit_code: pass is 0, fail 3, incomplete 4")
    require(not aggregate["satisfies_required_check"]
            or (doc["scope"] == "full" and aggregate["result"] == "pass"
                and doc["provenance"] == "measured"),
            where + ": only a measured passing full verdict satisfies the required check")
    require(aggregate["result"] == "pass" or aggregate["reasons"],
            where + ".aggregate.reasons: a non-passing verdict names its reasons")
    shards = doc["shards"]
    require([entry["shard"] for entry in shards] == list(range(len(shards))),
            where + ".shards: one entry per planned shard, in order")
    for entry in shards:
        at = "%s.shards[%d]" % (where, entry["shard"])
        require(entry["matched_records"] <= entry["expected_records"],
                at + ": more matched records than planned slots")
        require((entry["status"] in (None, "complete")) == (entry["failure"] is None),
                at + ".failure: retained exactly for a failed or interrupted shard")
    coverage = aggregate["coverage"]
    require(coverage["expected_records"] == sum(e["expected_records"] for e in shards)
            and coverage["matched_records"] == sum(e["matched_records"] for e in shards)
            and coverage["missing_records"]
            == coverage["expected_records"] - coverage["matched_records"],
            where + ".aggregate.coverage: does not add up the shards' records")
    require(aggregate["result"] != "pass"
            or (coverage["missing_records"] == 0
                and all(entry["status"] == "complete" for entry in shards)),
            where + ".aggregate: a passing verdict matches every planned record on complete "
            "shards")
    for index, case in enumerate(doc["cases"]):
        at = "%s.cases[%d]" % (where, index)
        require(case["exploratory"] == (case["criticality"] == "advisory"),
                at + ".exploratory: exactly advisory instances are exploratory")
        interval = case["interval"]
        if interval is not None:
            require(parse_rational(interval["lower"], at) <= parse_rational(interval["upper"], at),
                    at + ".interval: lower bound above upper bound")
            require(case["statistical_classification"] == stats.classify(
                Fraction(interval["lower"]), Fraction(interval["upper"])),
                at + ".statistical_classification: does not follow from the interval")
        else:
            require(case["statistical_classification"] is None,
                    at + ": no classification without an interval")
    return doc


# af.perf-profiles/7 (ADR-0224), af.perf-repetitions/1, af.perf-trace/1 and
# af.perf-diagnostics/1 (ADR-0174) -----------------------------------------------------------

PROFILES_SCHEMA = diagcontracts.PROFILES_SCHEMA
PROFILES = diagcontracts.PROFILES
validate_profiles = diagcontracts.validate_profiles
REPETITIONS_SCHEMA = diagcontracts.REPETITIONS_SCHEMA
validate_repetitions = diagcontracts.validate_repetitions
TRACE_SCHEMA = diagcontracts.TRACE_SCHEMA
validate_trace = diagcontracts.validate_trace
DIAGNOSTICS_SCHEMA = diagcontracts.DIAGNOSTICS_SCHEMA
validate_diagnostics = diagcontracts.validate_diagnostics


# Calibration cohort, parameters, readiness and activation (ADR-0181 to ADR-0183) -----------
#
# A cohort freezes its logical members before any of them runs: each member's ID, kind, source,
# seed, injected percent and scheduled day, and how a member is dispatched: by which workflow
# of which repository and branch, under which run name, after which of that workflow's run
# numbers. The Actions run that executes a member is execution provenance, recorded beside the
# cohort in readiness and never written back into it, so the cohort's bytes and digest stay what
# was declared (af.perf-cohort/4).

COHORT_SCHEMA = "af.perf-cohort/4"
# The run name the dispatching workflow gives a member's run before any job starts; a member's
# original is the first such run of the declared workflow, whatever its artifacts (ADR-0182).
DISPATCH_RUN_NAME = "af-perf calibration {cohort_digest} {member_id}"
DISPATCH_TITLE = re.compile(r"^af-perf calibration (sha256:[0-9a-f]{64}) ([a-z][a-z0-9_]{0,63})$")
DISPATCH_WORKFLOW = ".github/workflows/performance.yml"
# GitHub numbers every run of one workflow in order, from 1, and never renumbers or reuses a
# number; a rerun keeps its run's number. A gap in the numbers after a cohort's declared anchor is
# a deleted run, and nothing is judged from such a history (ADR-0183).
RUN_NUMBER = Int(0, 2 ** 31 - 1)
INJECTION_PERCENTS = (2, 5, 10)
COHORT_COUNTS = {"aa": 20, "power": 10}
INJECTION_MEMBERS = 10
POWER_DETECTIONS_REQUIRED = 9
PREPARATION_MEMBERS_MAX = 16
# A measured injection is valid when its median lies within 75% and 125% of its target, both
# limits included.
DOSE_LOWER = Fraction(3, 4)
DOSE_UPPER = Fraction(5, 4)
DATE = Str(pattern=r"^[0-9]{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12][0-9]|3[01])$", max_len=10)
DAYS_MAX = 365

COHORT_ORDER = ("aa, then preparation, then slowdown trials; the calibration parameters are "
                "sealed before the first trial")
TARGET_FORMULAS = {
    "efficiency": "e = max(r, a/A0)",
    "power": "p = max(1/10, 2*e)",
    "valid_injection": "3/4 * p*A0 <= median measured injected time <= 5/4 * p*A0",
    "readiness": ("all 20 A/A within tolerance in every critical case, and at least 9 of the "
                  "10 power trials each a valid regression in every critical case"),
}

COHORT_MEMBER = Obj(
    Field("member_id", ID),
    Field("kind", Str(enum=MEMBER_KINDS)),
    Field("source_sha", GIT_SHA), Field("seed", Int(0, 2 ** 63 - 1)),
    Field("injection_percent", Nullable(Int(1, 100))),
    # Days after the schedule's start date (UTC) on which this member's run must start.
    Field("day", Int(0, DAYS_MAX - 1)),
)

COHORT = Obj(
    Field("schema", Const(COHORT_SCHEMA)), Field("cohort_id", ID),
    Field("authority_sha", GIT_SHA), Field("policy_digest", DIGEST),
    Field("tier", Str(enum=TIERS)), Field("environment_id", ID), Field("recipe_id", ID),
    Field("cases", Arr(Obj(Field("instance_id", ID),
                           Field("criticality", Str(enum=("critical", "advisory"))),
                           Field("pairs", Int(limits.PAIRS_MIN, limits.PAIRS_MAX))),
                       1, limits.INSTANCES_MAX)),
    Field("family_size", Int(1, limits.INSTANCES_MAX)),
    Field("health", RUN_HEALTH),
    Field("allowed_cpu_groups", Arr(Obj(Field("group_id", ID), Field("cpu_model", SHORT_TEXT),
                                        Field("image", SHORT_TEXT)), 1, 16)),
    Field("schedule", Obj(Field("start_date", DATE),
                          Field("minimum_days", Int(5, DAYS_MAX)),
                          Field("runners", Const("fresh_hosted")),
                          Field("order", Const(COHORT_ORDER)))),
    # The loop count each preparation member's candidate runs to measure the loop's cost.
    Field("preparation", Obj(Field("iterations", Int(1, CALIBRATION_ITERATIONS_MAX)))),
    # The one trusted producer of member runs: a workflow_dispatch of this workflow file on this
    # repository's branch, whose run name binds the run to one member before it starts.
    Field("dispatch", Obj(Field("repository", REPOSITORY),
                          Field("workflow_path", Const(DISPATCH_WORKFLOW)),
                          Field("branch", Str(pattern=r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,199}$",
                                              max_len=200)),
                          Field("event", Const("workflow_dispatch")),
                          Field("run_name", Const(DISPATCH_RUN_NAME)),
                          # The workflow's latest run number the declarer observed before
                          # declaring, 0 before its first run: every dispatch of this cohort is
                          # numbered after it, and every run numbered after it must be listed.
                          Field("after_run_number", RUN_NUMBER))),
    Field("target_formulas", Obj(*[Field(name, Const(value))
                                   for name, value in sorted(TARGET_FORMULAS.items())])),
    Field("members", Arr(COHORT_MEMBER, 1, 256)),
)


def validate_cohort(doc, where="cohort.json"):
    if isinstance(doc, dict) and doc.get("schema") == "af.perf-local-cohort/1":
        LOCAL_COHORT.validate(doc, where)
    else:
        COHORT.validate(doc, where)
    import datetime
    try:
        datetime.date(*(int(part) for part in doc["schedule"]["start_date"].split("-")))
    except ValueError:
        raise ContractError(where + ".schedule.start_date: not a calendar date")
    ids = [case["instance_id"] for case in doc["cases"]]
    require(ids == sorted(set(ids)), where + ".cases: sorted unique instances")
    critical = [case for case in doc["cases"] if case["criticality"] == "critical"]
    require(doc["family_size"] == len(critical),
            where + ".family_size: K counts the critical instances")
    groups = [group["group_id"] for group in doc["allowed_cpu_groups"]]
    require(len(set(groups)) == len(groups), where + ".allowed_cpu_groups: unique groups")
    pairs = [(group["cpu_model"], group["image"]) for group in doc["allowed_cpu_groups"]]
    require(len(set(pairs)) == len(pairs),
            where + ".allowed_cpu_groups: one group per CPU model and image")
    members = doc["members"]
    member_ids = [member["member_id"] for member in members]
    require(len(set(member_ids)) == len(member_ids), where + ".members: unique member IDs")
    seeds = [member["seed"] for member in members]
    require(len(set(seeds)) == len(seeds), where + ".members: every member has its own seed")
    counts = {}
    for index, member in enumerate(members):
        at = "%s.members[%d]" % (where, index)
        injected = member["kind"] == "injection"
        require(injected == (member["injection_percent"] is not None),
                at + ".injection_percent: exactly injection members declare a percent")
        if injected:
            require(member["injection_percent"] in INJECTION_PERCENTS,
                    at + ".injection_percent: one of 2, 5 or 10")
        require(member["source_sha"] == doc["authority_sha"],
                at + ".source_sha: both sides of every member run the proposed authority")
        key = member_level(member) if member["kind"] != "aa" else "aa"
        counts[key] = counts.get(key, 0) + 1
    for kind, count in COHORT_COUNTS.items():
        require(counts.get(kind, 0) == count,
                where + ".members: exactly %d %s members are declared" % (count, kind))
    for percent in INJECTION_PERCENTS:
        require(counts.get("injection_%d" % percent, 0) == INJECTION_MEMBERS,
                where + ".members: exactly %d injection members at %d%%"
                % (INJECTION_MEMBERS, percent))
    require(1 <= counts.get("preparation", 0) <= PREPARATION_MEMBERS_MAX,
            where + ".members: declare 1..%d preparation runs" % PREPARATION_MEMBERS_MAX)
    days = dict((kind, [member["day"] for member in members if member["kind"] == kind])
                for kind in MEMBER_KINDS)
    every = [member["day"] for member in members]
    require(min(every) == 0 and max(every) + 1 >= doc["schedule"]["minimum_days"],
            where + ".members: the schedule starts on day 0 and spans at least %d days"
            % doc["schedule"]["minimum_days"])
    trials = days["injection"] + days["power"]
    require(max(days["aa"]) <= min(days["preparation"])
            and max(days["preparation"]) <= min(trials),
            where + ".members: A/A days precede preparation days, which precede every trial")
    return doc


def member_level(member):
    """The injection level of a cohort member, or None for an A/A member."""
    if member["kind"] == "injection":
        return "injection_%d" % member["injection_percent"]
    return None if member["kind"] == "aa" else member["kind"]


CALIBRATION_SCHEMA = "af.perf-calibration-parameters/2"
LOOP_LEVELS = ("injection_2", "injection_5", "injection_10", "power")
LEVEL_FRACTIONS = {"injection_2": Fraction(2, 100), "injection_5": Fraction(5, 100),
                   "injection_10": Fraction(10, 100)}

CALIBRATION = Obj(
    Field("schema", Const(CALIBRATION_SCHEMA)), Field("cohort_id", ID),
    Field("cohort_digest", DIGEST),
    # A0 is frozen from the 20 A/A members; the loop's cost from the preparation members.
    Field("a0_member_ids", Arr(ID, COHORT_COUNTS["aa"], COHORT_COUNTS["aa"], unique=True)),
    Field("preparation_member_ids", Arr(ID, 1, PREPARATION_MEMBERS_MAX, unique=True)),
    Field("preparation_iterations", Int(1, CALIBRATION_ITERATIONS_MAX)),
    Field("groups", Arr(Obj(
        Field("instance_id", ID), Field("cpu_group", ID),
        Field("relative", DECIMAL), Field("absolute", DECIMAL),
        # The median baseline time in ms of every measured A/A sample of the group, or why
        # there is none; zero or unavailable A0 prevents readiness for that group.
        Field("a0", Nullable(RATIONAL)), Field("a0_samples", COUNT),
        Field("a0_unavailable", Nullable(ID)),
        Field("efficiency", Nullable(RATIONAL)), Field("power_target", Nullable(RATIONAL)),
        # The loop's median cost per iteration in ns, from the preparation candidates.
        Field("loop_ns_per_iteration", Nullable(RATIONAL)), Field("loop_samples", COUNT),
        Field("iterations", Nullable(Obj(*[Field(level, Int(1, CALIBRATION_ITERATIONS_MAX))
                                           for level in LOOP_LEVELS])))),
        1, 1024)),
)


def efficiency(relative, absolute, a0):
    require(a0 > 0, "A0 must be positive: zero or unavailable A0 prevents readiness")
    return max(relative, absolute / a0)


def power_target(eff):
    return max(Fraction(1, 10), 2 * eff)


def level_fraction(group, level):
    """The injected fraction of A0 at `level` for one calibration group."""
    if level == "power":
        return parse_rational(group["power_target"], "power_target")
    return LEVEL_FRACTIONS[level]


def target_ms(group, level):
    """The injected time a level targets for one group: its fraction of A0, in ms."""
    return level_fraction(group, level) * parse_rational(group["a0"], "a0")


def loop_iterations(target, cost_ns):
    """The loop count whose cost at `cost_ns` per iteration is closest to `target` ms (ties
    upward), at least one; None when it exceeds the bound."""
    exact = target * 1000000 / cost_ns
    count = max(1, int((2 * exact + 1) // 2))
    return count if count <= CALIBRATION_ITERATIONS_MAX else None


def validate_calibration(doc, where="calibration-parameters.json", cohort=None):
    CALIBRATION.validate(doc, where)
    keys = [(group["instance_id"], group["cpu_group"]) for group in doc["groups"]]
    require(keys == sorted(set(keys)), where + ".groups: sorted unique case/group pairs")
    for index, group in enumerate(doc["groups"]):
        at = "%s.groups[%d]" % (where, index)
        relative = parse_decimal(group["relative"], at + ".relative")
        absolute = parse_decimal(group["absolute"], at + ".absolute")
        require(absolute > 0, at + ".absolute: the absolute tolerance is positive")
        require((group["a0"] is None) == (group["a0_unavailable"] is not None),
                at + ".a0_unavailable: exactly an unavailable A0 says why")
        if group["a0"] is None:
            require(group["efficiency"] is None and group["power_target"] is None
                    and group["iterations"] is None,
                    at + ": no target or loop count without A0")
            continue
        a0 = parse_rational(group["a0"], at + ".a0")
        require(a0 > 0, at + ".a0: zero A0 prevents readiness for that group")
        require(group["a0_samples"] > 0, at + ".a0_samples: A0 has samples")
        eff = efficiency(relative, absolute, a0)
        require(group["efficiency"] is not None
                and parse_rational(group["efficiency"], at) == eff,
                at + ".efficiency: must equal max(r, a/A0)")
        require(group["power_target"] is not None
                and parse_rational(group["power_target"], at) == power_target(eff),
                at + ".power_target: must equal max(1/10, 2e)")
        cost = group["loop_ns_per_iteration"]
        require((cost is None) == (group["loop_samples"] == 0),
                at + ".loop_ns_per_iteration: exactly measured from preparation samples")
        if cost is None:
            require(group["iterations"] is None, at + ".iterations: no loop count unmeasured")
            continue
        cost = parse_rational(cost, at + ".loop_ns_per_iteration")
        require(cost > 0, at + ".loop_ns_per_iteration: the loop has a cost")
        expected = dict((level, loop_iterations(target_ms(group, level), cost))
                        for level in LOOP_LEVELS)
        if any(count is None for count in expected.values()):
            expected = None
        require(group["iterations"] == expected,
                at + ".iterations: each is the loop count nearest its target at the prepared "
                "cost")
    if cohort is not None:
        validate_cohort(cohort)
        require(doc["cohort_id"] == cohort["cohort_id"]
                and doc["cohort_digest"] == canonical_digest(cohort),
                where + ": names a different cohort")
        declared = dict((member["member_id"], member["kind"]) for member in cohort["members"])
        require(sorted(doc["a0_member_ids"]) == sorted(
            ident for ident, kind in declared.items() if kind == "aa"),
            where + ".a0_member_ids: A0 comes from exactly the cohort's 20 A/A members")
        require(sorted(doc["preparation_member_ids"]) == sorted(
            ident for ident, kind in declared.items() if kind == "preparation"),
            where + ".preparation_member_ids: exactly the cohort's preparation members")
        require(doc["preparation_iterations"] == cohort["preparation"]["iterations"],
                where + ".preparation_iterations: the cohort's declared preparation count")
        cases = dict((case["instance_id"], case) for case in cohort["cases"])
        allowed = set(group["group_id"] for group in cohort["allowed_cpu_groups"])
        require(set(keys) == set((ident, group) for ident in cases for group in allowed),
                where + ".groups: one entry per declared case and allowed CPU group")
    return doc


def calibration_group(calibration, instance_id, cpu_group):
    for group in calibration["groups"]:
        if (group["instance_id"], group["cpu_group"]) == (instance_id, cpu_group):
            return group
    return None


READINESS_SCHEMA = "af.perf-readiness/6"
MEMBER_STATUSES = ("complete", "missing", "interrupted", "failed", "unhealthy", "invalid")
# Why a member has no evidence: never dispatched, or dispatched and its original attempt's run
# bundle absent, expired or unreadable. Each is still the member's original, never replaced.
MISSING_REASONS = ("no_dispatch", "artifact_missing", "artifact_expired", "artifact_unreadable")
READINESS_REASONS = MISSING_REASONS + (
    # Attempt 1 completed without success (failure, cancellation, timeout or any other
    # conclusion, or none): a failed original whatever its bundles hold (ADR-0183).
    "attempt_unsuccessful",
    # Two listed artifacts of an original's expected run or shard bundle name: neither is
    # chosen (ADR-0183).
    "artifact_ambiguous",
    # The original attempt's measuring jobs are not shown to have run on fresh hosted runners of
    # the plan's label (ADR-0183).
    "runner_unverified",
    # The original's workflow at its head commit, or the scheduling code at its plan's control
    # revision, is not byte-identical to the cohort authority's, or cannot be shown (ADR-0184).
    "producer_unverified",
    "attempt_unavailable", "attempt_incomplete", "off_schedule", "before_calibration",
    "cpu_group_mismatch", "unprepared_cpu_group", "wrong_plan", "shard_missing",
    "shard_interrupted", "shard_failed", "health_unstable", "records_incomplete",
    "not_measured")
ATTEMPT_CONCLUSIONS = ("success", "failure", "cancelled", "skipped", "timed_out",
                       "action_required", "neutral", "stale", "startup_failure")
# A member's original: the first run of the declared workflow dispatched under its run name,
# at attempt 1, as the workflow-run history and that attempt's own record show; then the run
# bundle that attempt uploaded, when it is listed and readable, and the hosted runners its
# measuring jobs ran on, as attempt 1's own job records show.
RUNNER_GROUP_HOSTED = "GitHub Actions"
MEASURING_RUNNER = Obj(Field("label", SHORT_TEXT), Field("runner_group", Const(RUNNER_GROUP_HOSTED)),
                       Field("jobs", Int(2, 64)))
PROVENANCE = Obj(
    Field("actions_run_id", Int(1, 2 ** 62)), Field("run_number", Int(1, 2 ** 31 - 1)),
    Field("run_attempt", Int(1, 1)),
    Field("display_title", SHORT_TEXT),
    # The commit whose workflow file GitHub ran for the dispatch (ADR-0184).
    Field("head_sha", GIT_SHA),
    Field("attempt_started_unix_ms", Nullable(UNIX_MS)),
    Field("attempt_updated_unix_ms", Nullable(UNIX_MS)),
    Field("attempt_conclusion", Nullable(Str(enum=ATTEMPT_CONCLUSIONS))),
    # The lab run ID the attempt plans, gh<run>_a1_primary, and its run bundle's name.
    Field("lab_run_id", ID), Field("artifact_name", SHORT_TEXT),
    Field("artifact_id", Nullable(Int(1, 2 ** 62))),
    Field("artifact_created_unix_ms", Nullable(UNIX_MS)), Field("plan_digest", Nullable(DIGEST)),
    Field("runner", Nullable(MEASURING_RUNNER)))
READINESS_CASE = Obj(
    Field("classification", Nullable(Str(enum=READINESS_CLASSIFICATIONS))),
    # The median of the candidate's measured injected times and its frozen target, in ms.
    Field("injected_median_ms", Nullable(RATIONAL)), Field("target_ms", Nullable(RATIONAL)),
    Field("injection_valid", Nullable(Bool())))
READINESS_MEMBER = Obj(
    Field("member_id", ID), Field("kind", Str(enum=MEMBER_KINDS)),
    Field("status", Str(enum=MEMBER_STATUSES)),
    Field("reasons", Arr(Str(enum=READINESS_REASONS), 0, 16, unique=True)),
    Field("provenance", Nullable(PROVENANCE)),
    Field("cpu_groups", Map(ID_PATTERN, Nullable(ID), 0, limits.INSTANCES_MAX)),
    Field("verdict_sha256", Nullable(DIGEST)),
    Field("cases", Map(ID_PATTERN, READINESS_CASE, 0, limits.INSTANCES_MAX)),
    Field("qualifies", Bool()))
REFUSED_EVIDENCE = Obj(
    # A dispatched run that is no member's original, or an artifact that is no original's.
    Field("source", Str(enum=("dispatch", "artifact"))),
    Field("artifact_name", Nullable(SHORT_TEXT)), Field("actions_run_id", Int(1, 2 ** 62)),
    Field("reason", Str(enum=("unknown_member", "other_cohort", "foreign_dispatch",
                              "replacement", "later_attempt", "duplicate", "unbound_run"))),
    Field("detail", TEXT))
READINESS_CLAIM = ("9 of 10 power trials is a readiness decision for the demonstrated targets, "
                   "not proof of 90% population detection power; group tallies are descriptive "
                   "and a mixed-host trial appears in each observed critical-case group")
READINESS = Obj(
    Field("schema", Const(READINESS_SCHEMA)), Field("cohort_id", ID),
    Field("cohort_digest", DIGEST), Field("calibration_digest", DIGEST),
    # The parameters recomputed from the A/A and preparation originals, which must be exactly
    # the sealed ones every trial names before any trial is judged (ADR-0183).
    Field("recomputed_calibration_digest", DIGEST),
    # The workflow's runs after the cohort's anchor, every number present: the history every
    # original was bound from, and in which no run was deleted (ADR-0183).
    Field("history", Obj(Field("after_run_number", RUN_NUMBER),
                         Field("latest_run_number", RUN_NUMBER), Field("runs", COUNT))),
    Field("members", Arr(READINESS_MEMBER, 1, 256)),
    Field("refused_evidence", Arr(REFUSED_EVIDENCE, 0, 256)),
    Field("tallies", Obj(
        Field("aa_members", COUNT), Field("aa_within_tolerance", COUNT),
        Field("power_members", COUNT),
        # Power trials in which every critical case is a valid regression: whole trials.
        Field("power_qualifying", COUNT),
        Field("power_by_case", Map(ID_PATTERN, Obj(Field("detections", COUNT),
                                                   Field("valid_injections", COUNT)),
                                   0, limits.INSTANCES_MAX)),
        Field("power_by_group", Map(ID_PATTERN, Obj(Field("members", COUNT),
                                                    Field("qualifying", COUNT)), 0, 17)),
        Field("injection", Map(r"^injection_(?:2|5|10)$", Obj(
            Field("members", COUNT), Field("regressions", COUNT),
            Field("valid_injections", COUNT)), 3, 3)))),
    Field("groups", Map(ID_PATTERN, Obj(Field("ready", Bool()),
                                        Field("unavailable", Arr(ID, 0, limits.INSTANCES_MAX))),
                        1, 16)),
    Field("ready", Bool()),
    Field("claim", Const(READINESS_CLAIM)),
)


def injection_valid(injected_median_ms, target):
    """Whether a measured median injection lies within its inclusive 75%..125% band."""
    if injected_median_ms is None or target is None or target <= 0:
        return False
    return DOSE_LOWER * target <= injected_median_ms <= DOSE_UPPER * target


def member_qualifies(cohort, calibration, member):
    """Whether one recorded member counts toward readiness: an A/A member within tolerance in
    every critical case; a slowdown member a regression with valid injection in every critical
    case of the same trial, in a group prepared for each; a preparation member complete."""
    critical = [case["instance_id"] for case in cohort["cases"]
                if case["criticality"] == "critical"]
    if member["status"] != "complete":
        return False
    cases = member["cases"]
    if member["kind"] == "aa":
        return all(cases.get(ident, {}).get("classification") == stats.WITHIN_TOLERANCE
                   for ident in critical)
    if member["kind"] == "preparation":
        return True
    for ident in critical:
        entry = cases.get(ident)
        group = calibration_group(calibration, ident, member["cpu_groups"].get(ident) or "")
        if entry is None or group is None or group["iterations"] is None:
            return False
        if entry["classification"] != stats.REGRESSION or entry["injection_valid"] is not True:
            return False
    return True


def group_readiness(cohort, calibration):
    """Each allowed CPU group's readiness inputs: ready when every critical case has a positive
    A0 and a prepared loop count there; the unavailable cases otherwise."""
    critical = [case["instance_id"] for case in cohort["cases"]
                if case["criticality"] == "critical"]
    groups = {}
    for allowed in cohort["allowed_cpu_groups"]:
        missing = [ident for ident in critical
                   if (calibration_group(calibration, ident, allowed["group_id"]) or {}).get(
                       "iterations") is None]
        groups[allowed["group_id"]] = {"ready": not missing, "unavailable": missing}
    return groups


def compute_readiness(cohort, calibration, members):
    """Recompute tallies and the readiness decision from cohort membership and outcomes.

    Every declared member stays in every denominator, whatever its status. A power trial counts
    only when the whole trial qualifies: every critical case a regression with valid injection
    in that same trial. Different trials failing in different cases are never combined.
    """
    critical = [case["instance_id"] for case in cohort["cases"]
                if case["criticality"] == "critical"]
    by_id = dict((member["member_id"], member) for member in members)
    tallies = {"aa_members": 0, "aa_within_tolerance": 0, "power_members": 0,
               "power_qualifying": 0,
               "power_by_case": dict((ident, {"detections": 0, "valid_injections": 0})
                                     for ident in critical),
               "power_by_group": {},
               "injection": dict(("injection_%d" % percent,
                                  {"members": 0, "regressions": 0, "valid_injections": 0})
                                 for percent in INJECTION_PERCENTS)}
    qualifies = {}
    for declared in cohort["members"]:
        member = by_id[declared["member_id"]]
        kind = declared["kind"]
        ok = member_qualifies(cohort, calibration, member)
        qualifies[declared["member_id"]] = ok
        complete = member["status"] == "complete"
        if kind == "aa":
            tallies["aa_members"] += 1
            tallies["aa_within_tolerance"] += int(ok)
        elif kind == "power":
            tallies["power_members"] += 1
            tallies["power_qualifying"] += int(ok)
            # A mixed-host trial participates in each observed critical-case group once.
            # Group tallies overlap; only power_qualifying counts whole trials once.
            observed = {member["cpu_groups"].get(ident) or "unobserved" for ident in critical}
            for group in sorted(observed):
                entry = tallies["power_by_group"].setdefault(
                    group, {"members": 0, "qualifying": 0})
                entry["members"] += 1
                entry["qualifying"] += int(ok)
            for ident in critical:
                case = member["cases"].get(ident) or {}
                if complete and case.get("injection_valid") is True:
                    tallies["power_by_case"][ident]["valid_injections"] += 1
                    if case.get("classification") == stats.REGRESSION:
                        tallies["power_by_case"][ident]["detections"] += 1
        elif kind == "injection":
            entry = tallies["injection"][member_level(declared)]
            entry["members"] += 1
            cases = [member["cases"].get(ident) or {} for ident in critical]
            if complete and all(case.get("classification") == stats.REGRESSION
                                for case in cases):
                entry["regressions"] += 1
            if complete and all(case.get("injection_valid") is True for case in cases):
                entry["valid_injections"] += 1
    groups = group_readiness(cohort, calibration)
    ready = (tallies["aa_within_tolerance"] == tallies["aa_members"] == COHORT_COUNTS["aa"]
             and tallies["power_members"] == COHORT_COUNTS["power"]
             and tallies["power_qualifying"] >= POWER_DETECTIONS_REQUIRED)
    return tallies, groups, qualifies, ready


def validate_readiness_injection(member, at):
    """Shared dose admission for hosted and local member judgments."""
    for ident, case in sorted(member["cases"].items()):
        injected = case["injected_median_ms"]
        target = case["target_ms"]
        expected = (None if member["kind"] in ("aa", "preparation") else injection_valid(
            None if injected is None else parse_rational(injected, at),
            None if target is None else parse_rational(target, at)))
        require(case["injection_valid"] == expected,
                at + ".cases.%s.injection_valid: does not follow from the measured median "
                "and its target" % ident)


def validate_readiness_totals(doc, where, cohort, calibration):
    """Shared sealed-target, denominator and whole-trial admission."""
    declared = {member["member_id"]: member for member in cohort["members"]}
    validate_calibration(calibration, cohort=cohort)
    require(doc["calibration_digest"] == canonical_digest(calibration),
            where + ".calibration_digest: names other calibration parameters")
    require(doc["recomputed_calibration_digest"] == doc["calibration_digest"],
            where + ".recomputed_calibration_digest: the sealed parameters are not those "
            "the A/A and preparation originals give")
    for member in doc["members"]:
        level = member_level(declared[member["member_id"]])
        for ident, case in member["cases"].items():
            group = calibration_group(calibration, ident, member["cpu_groups"].get(ident) or "")
            expected = None
            if level not in (None, "preparation") and group is not None \
                    and group["a0"] is not None:
                expected = format_rational(target_ms(group, level))
            require(case["target_ms"] == expected,
                    where + ".members.%s.cases.%s.target_ms: not the frozen target"
                    % (member["member_id"], ident))
    tallies, groups, qualifies, ready = compute_readiness(cohort, calibration,
                                                          doc["members"])
    for member in doc["members"]:
        require(member["qualifies"] == qualifies[member["member_id"]],
                where + ".members.%s.qualifies: does not match the recomputation"
                % member["member_id"])
    require(doc["tallies"] == tallies, where + ".tallies: do not match the recomputation")
    require(doc["groups"] == groups, where + ".groups: do not match the recomputation")
    require(doc["ready"] == ready, where + ".ready: does not match the recomputation")


def validate_readiness(doc, where="readiness.json", cohort=None, calibration=None):
    READINESS.validate(doc, where)
    ids = [member["member_id"] for member in doc["members"]]
    require(len(set(ids)) == len(ids), where + ".members: unique members")
    for index, member in enumerate(doc["members"]):
        at = "%s.members[%d]" % (where, index)
        dispatched = "no_dispatch" not in member["reasons"]
        require(dispatched == (member["provenance"] is not None),
                at + ".provenance: every dispatched member names its original Actions run")
        require((member["status"] == "missing")
                == any(reason in MISSING_REASONS for reason in member["reasons"]),
                at + ".status: exactly a member without evidence is missing")
        require(member["status"] == "complete" or not member["cases"],
                at + ".cases: only a complete member has case outcomes")
        require((member["status"] == "complete") == (member["verdict_sha256"] is not None),
                at + ".verdict_sha256: exactly a complete member names its verdict")
        require((member["status"] == "complete") == (not member["reasons"]),
                at + ".reasons: exactly an incomplete member says why")
        require(member["status"] != "complete"
                or (set(member["cpu_groups"]) == set(member["cases"])
                    and all(group is not None for group in member["cpu_groups"].values())),
                at + ".cpu_groups: a complete member binds every case to its observed group")
        provenance = member["provenance"]
        if provenance is not None:
            lab = "gh%d_a1_primary" % provenance["actions_run_id"]
            require(provenance["lab_run_id"] == lab
                    and provenance["artifact_name"] == "af-perf-%s-run" % lab,
                    at + ".provenance: the original attempt, its lab run and its artifact name "
                    "agree")
            match = DISPATCH_TITLE.match(provenance["display_title"])
            require(match is not None and match.group(2) == member["member_id"],
                    at + ".provenance.display_title: the run name of this member's dispatch")
            evidenced = member["status"] != "missing" \
                and "artifact_ambiguous" not in member["reasons"]
            require(not evidenced or (provenance["artifact_id"] is not None
                                      and provenance["plan_digest"] is not None),
                    at + ".provenance: a member with evidence names its run bundle and plan")
            require(member["status"] != "complete"
                    or provenance["attempt_started_unix_ms"] is not None,
                    at + ".provenance: a complete member's original attempt has its start")
            require(member["status"] != "complete"
                    or provenance["attempt_conclusion"] == "success",
                    at + ".provenance.attempt_conclusion: only a successful original attempt is "
                    "complete")
            require("attempt_unsuccessful" not in member["reasons"]
                    or member["status"] not in ("complete", "unhealthy"),
                    at + ".status: an unsuccessful original attempt is at least a failed member")
            require(member["status"] != "complete" or provenance["runner"] is not None,
                    at + ".provenance.runner: a complete member's measuring jobs ran on fresh "
                    "hosted runners")
        else:
            require(member["status"] == "missing", at + ".status: an undispatched member is "
                    "missing")
        validate_readiness_injection(member, at)
    history = doc["history"]
    require(history["latest_run_number"] - history["after_run_number"] == history["runs"],
            where + ".history: every run after the anchor, each number once")
    numbers = [member["provenance"]["run_number"] for member in doc["members"]
               if member["provenance"] is not None]
    require(len(set(numbers)) == len(numbers)
            and all(history["after_run_number"] < number <= history["latest_run_number"]
                    for number in numbers),
            where + ".members: every original is a distinct run of the listed history")
    if cohort is not None:
        validate_cohort(cohort)
        require(doc["cohort_id"] == cohort["cohort_id"]
                and doc["cohort_digest"] == canonical_digest(cohort),
                where + ": names a different cohort")
        require(history["after_run_number"] == cohort["dispatch"]["after_run_number"],
                where + ".history.after_run_number: not the cohort's anchor")
        declared = dict((member["member_id"], member) for member in cohort["members"])
        require(sorted(ids) == sorted(declared),
                where + ".members: every declared member, and only those, is recorded")
        for member in doc["members"]:
            provenance = member["provenance"]
            require(provenance is None or DISPATCH_TITLE.match(
                provenance["display_title"]).group(1) == doc["cohort_digest"],
                where + ".members.%s.provenance.display_title: a dispatch of another cohort"
                % member["member_id"])
        case_ids = {case["instance_id"] for case in cohort["cases"]}
        allowed = {group["group_id"] for group in cohort["allowed_cpu_groups"]}
        for member in doc["members"]:
            require(member["kind"] == declared[member["member_id"]]["kind"],
                    where + ".members.%s.kind: not the declared kind" % member["member_id"])
            require(set(member["cpu_groups"]) <= case_ids
                    and all(group is None or group in allowed
                            for group in member["cpu_groups"].values()),
                    where + ".members.%s.cpu_groups: only declared cases and allowed groups"
                    % member["member_id"])
            require(member["status"] != "complete" or set(member["cases"]) == case_ids,
                    where + ".members.%s.cases: a complete member contains every declared case"
                    % member["member_id"])
    if cohort is not None and calibration is not None:
        validate_readiness_totals(doc, where, cohort, calibration)
    return doc


ACTIVATION_SCHEMA = "af.perf-activation/3"
# Where a cohort's reviewed records live: committed files of the control revision, read from Git.
RECORD_PATH = Str(pattern=r"^fixtures/performance/[A-Za-z0-9_.-]{1,120}(?:/[A-Za-z0-9_.-]{1,120})?"
                          r"\.(?:json|toml)$", max_len=300)
ACTIVATION = Obj(
    Field("schema", Const(ACTIVATION_SCHEMA)),
    Field("authority_sha", GIT_SHA), Field("policy_digest", DIGEST), Field("cohort_id", ID),
    # The exact reviewed bytes of the cohort, its sealed parameters and its readiness, by path
    # in the control revision and SHA-256; activation verification rehashes each (ADR-0183).
    Field("cohort_path", RECORD_PATH), Field("cohort_sha256", DIGEST),
    Field("calibration_path", RECORD_PATH), Field("calibration_sha256", DIGEST),
    Field("readiness_path", RECORD_PATH), Field("readiness_sha256", DIGEST),
    # Every member's original Actions run, ascending: the identities verification recomputes.
    Field("cohort_actions_run_ids", Arr(Int(1, 2 ** 62), 1, 256, unique=True)),
    # The scope the cohort calibrated, and no more: its environment and recipe and the allowed
    # CPU/image groups prepared for every critical case. Another group stays uncalibrated.
    Field("environment_id", ID), Field("recipe_id", ID),
    Field("calibrated_groups", Arr(ID, 1, 16, unique=True)),
    Field("admission", Tagged("kind", {
        "calibrated": tagged_variant("kind", "calibrated"),
        "equivalent_adapter": tagged_variant(
            "kind", "equivalent_adapter", Field("equivalence_group", ID),
            Field("adapter_files", Arr(REL_PATH, 1, 32, unique=True)),
            # The reviewed calibrated activation this bridge extends, retained in the control
            # revision, and the SHA-256 of its bytes.
            Field("calibrated_activation_path", RECORD_PATH),
            Field("calibrated_activation_sha256", DIGEST),
            # The bridge's execution evidence (af.perf-equivalence/1, ADR-0184): the golden
            # check on both product sources and the post-merge A/A, recomputed from their
            # retained runs. Without verified evidence a bridge never measures as active.
            Field("equivalence_path", RECORD_PATH), Field("equivalence_sha256", DIGEST),
            Field("rationale", Str(min_len=1, max_len=4096))),
    })),
)


def validate_activation(doc, where="activation.toml"):
    ACTIVATION.validate(doc, where)
    runs = doc["cohort_actions_run_ids"]
    require(runs == sorted(runs), where + ".cohort_actions_run_ids: ascending")
    groups = doc["calibrated_groups"]
    require(groups == sorted(groups), where + ".calibrated_groups: sorted")
    paths = [doc["cohort_path"], doc["calibration_path"], doc["readiness_path"]]
    require(all(path.endswith(".json") for path in paths) and len(set(paths)) == 3,
            where + ": three distinct JSON records")
    if doc["admission"]["kind"] == "equivalent_adapter":
        admission = doc["admission"]
        files = admission["adapter_files"]
        require(files == sorted(files), where + ".admission.adapter_files: sorted")
        require(admission["calibrated_activation_path"].endswith(".toml"),
                where + ".admission.calibrated_activation_path: a retained activation manifest")
        require(admission["equivalence_path"].endswith(".json")
                and admission["equivalence_path"] not in paths,
                where + ".admission.equivalence_path: its own JSON record")
    return doc


# af.perf-equivalence/1 (ADR-0184) ---------------------------------------------------------
#
# The execution evidence of an equivalent-adapter bridge, recomputed from two retained runs that
# the bridged authority measured: the golden check, whose baseline is the calibrated product
# source and whose candidate is the bridged one, every planned sample of every cohort case
# present and accepted by its oracle on both sides with adapters of the named group; and the
# post-merge A/A, one source descending from the bridge measured against itself through the
# bridge's adapter on a calibrated CPU/image group, every critical case within tolerance. Each
# check keeps what its run showed, and its reasons follow from those facts alone; owner approval
# of semantic equivalence stays a separate administrative prerequisite.

EQUIVALENCE_SCHEMA = "af.perf-equivalence/1"
EQUIVALENCE_CLAIM = ("execution evidence of the bridge's golden checks and post-merge A/A; owner "
                     "approval of semantic equivalence is a separate prerequisite")
EQUIVALENCE_CASE = Obj(
    Field("criticality", Str(enum=("critical", "advisory"))),
    Field("pairs", Int(limits.PAIRS_MIN, limits.PAIRS_MAX)),
    Field("classification", Str(enum=CASE_STATUSES)),
    Field("failures", COUNT), Field("missing_records", COUNT))
EQUIVALENCE_CHECK = Obj(
    # Where the run was retained: its Actions run and the lab run ID that attempt planned,
    # gh<run>_a<attempt>_primary, named explicitly and never inferred from the latest attempt.
    Field("actions_run_id", Int(1, 2 ** 62)), Field("lab_run_id", ID),
    Field("plan_digest", DIGEST), Field("verdict_sha256", DIGEST),
    Field("builds_sha256", Nullable(DIGEST)),
    Field("scope", Str(enum=SCOPES)), Field("authority_mode", Str(enum=AUTHORITY_MODES)),
    Field("tier", Str(enum=TIERS)),
    Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
    Field("authority_sha", GIT_SHA), Field("policy_digest", DIGEST),
    Field("environment_id", ID), Field("environment_digest", DIGEST),
    Field("recipe_id", ID), Field("recipe_digest", DIGEST),
    Field("instrument_digest", DIGEST), Field("fixture_digest", DIGEST),
    # The adapter each side compiled and its group, and the probe binary it produced: null when
    # the build manifest was not retained or the side failed.
    Field("adapters", Obj(Field("baseline", Nullable(ID)), Field("candidate", Nullable(ID)),
                          Field("baseline_group", Nullable(ID)),
                          Field("candidate_group", Nullable(ID)))),
    Field("probe_binaries", Obj(Field("baseline", Nullable(DIGEST)),
                                Field("candidate", Nullable(DIGEST)))),
    Field("builds_succeeded", Bool()),
    # Each shard's observed CPU/image group among the calibrated activation's, or null.
    Field("cpu_groups", Arr(Nullable(ID), 1, limits.SHARDS_MAX)),
    Field("shards_complete", Bool()), Field("healthy", Bool()),
    Field("provenance", Str(enum=("measured", "synthetic", "mixed", "none"))),
    Field("result", Str(enum=("pass", "fail", "incomplete"))),
    Field("cases", Map(ID_PATTERN, EQUIVALENCE_CASE, 1, limits.INSTANCES_MAX)),
    Field("reasons", Arr(TEXT, 0, 64)), Field("verified", Bool()))
EQUIVALENCE = Obj(
    Field("schema", Const(EQUIVALENCE_SCHEMA)),
    Field("equivalence_group", ID), Field("adapter_files", Arr(REL_PATH, 1, 32, unique=True)),
    # The adapters of the group whose closure holds every named file: the bridge itself.
    Field("bridge_adapters", Arr(ID, 1, 4, unique=True)),
    Field("calibrated_authority_sha", GIT_SHA), Field("bridged_authority_sha", GIT_SHA),
    Field("policy_digest", DIGEST), Field("tier", Str(enum=TIERS)),
    Field("environment_id", ID), Field("recipe_id", ID),
    Field("cohort_cases", Arr(Obj(Field("instance_id", ID),
                                  Field("criticality", Str(enum=("critical", "advisory"))),
                                  Field("pairs", Int(limits.PAIRS_MIN, limits.PAIRS_MAX))),
                              1, limits.INSTANCES_MAX)),
    Field("calibrated_groups", Arr(Obj(Field("group_id", ID), Field("cpu_model", SHORT_TEXT),
                                       Field("image", SHORT_TEXT)), 1, 16)),
    Field("golden", EQUIVALENCE_CHECK), Field("post_merge_aa", EQUIVALENCE_CHECK),
    Field("verified", Bool()), Field("claim", Const(EQUIVALENCE_CLAIM)),
)


def validate_equivalence(doc, where="equivalence.json"):
    """Each check's reasons are recomputed from its recorded facts and the record's context, and
    the record is verified exactly when both checks are."""
    EQUIVALENCE.validate(doc, where)
    from . import equivalence
    require(doc["adapter_files"] == sorted(doc["adapter_files"]),
            where + ".adapter_files: sorted")
    require(doc["bridge_adapters"] == sorted(doc["bridge_adapters"]),
            where + ".bridge_adapters: sorted")
    for role in equivalence.ROLES:
        check = doc[role]
        require(check["lab_run_id"].startswith("gh%d_a" % check["actions_run_id"]),
                where + ".%s.lab_run_id: planned by its Actions run" % role)
        require(check["reasons"] == equivalence.check_reasons(doc, role, check),
                where + ".%s.reasons: do not follow from the recorded run" % role)
        require(check["verified"] == (not check["reasons"]),
                where + ".%s.verified: exactly a check without reasons" % role)
    require(doc["verified"] == all(doc[role]["verified"] for role in equivalence.ROLES),
            where + ".verified: exactly when both checks are")
    return doc


ACTIVATION_VERIFICATION_SCHEMA = "af.perf-activation-verification/3"
# What a human still owes an activation however its evidence verified (design sections 5, 12):
# owner review at the exact head, and for a bridge the owner's semantic approval. The golden
# checks and the post-merge A/A are executable evidence verification itself discharges.
ACTIVATION_PENDING = ("owner_review_at_exact_head", "semantic_equivalence_owner_approval")
# Where a bridge run came from, established only by a fetched verification (ADR-0185): the
# reviewed Performance workflow of this repository ran it, the exact attempt its lab run ID names
# completed successfully, the workflow file at its head commit and the scheduling code at its
# plan's control revision are byte-identical to the bridged authority's, and every build and
# shard job of that attempt ran on a fresh GitHub-hosted runner of the plan's label.
BRIDGE_PROVENANCE = Obj(
    Field("repository", REPOSITORY), Field("workflow_path", Const(DISPATCH_WORKFLOW)),
    Field("event", SHORT_TEXT), Field("run_attempt", Int(1, 9999)),
    Field("conclusion", Const("success")), Field("head_sha", GIT_SHA),
    Field("control_sha", GIT_SHA),
    Field("runner", Obj(Field("label", SHORT_TEXT), Field("runner_group", SHORT_TEXT),
                        Field("jobs", Int(1, 64)))))
EQUIVALENCE_RUN = Obj(Field("actions_run_id", Int(1, 2 ** 62)), Field("lab_run_id", ID),
                      Field("plan_digest", DIGEST), Field("baseline_sha", GIT_SHA),
                      Field("candidate_sha", GIT_SHA),
                      Field("adapters", Obj(Field("baseline", ID), Field("candidate", ID))),
                      # null in a reviewed (local) verification, which fetches nothing.
                      Field("provenance", Nullable(BRIDGE_PROVENANCE)))
ACTIVATION_VERIFICATION = Obj(
    Field("schema", Const(ACTIVATION_VERIFICATION_SCHEMA)),
    # fetched: the retained Actions evidence was fetched and readiness (and a bridge's
    # equivalence evidence) recomputed from it by trusted control code; reviewed: the committed
    # records were checked locally, as an ordinary pull request does, without fetching again.
    Field("mode", Str(enum=("fetched", "reviewed"))),
    Field("revision", GIT_SHA), Field("activation_sha256", DIGEST),
    Field("admission", Str(enum=("calibrated", "equivalent_adapter"))),
    Field("authority_sha", GIT_SHA), Field("policy_digest", DIGEST), Field("cohort_id", ID),
    Field("cohort_sha256", DIGEST), Field("calibration_sha256", DIGEST),
    Field("readiness_sha256", DIGEST),
    Field("members", Int(1, 256)), Field("original_runs", Arr(Int(1, 2 ** 62), 1, 256)),
    Field("tallies", Obj(Field("aa_within_tolerance", COUNT), Field("aa_members", COUNT),
                         Field("power_qualifying", COUNT), Field("power_members", COUNT))),
    Field("environment_id", ID), Field("recipe_id", ID),
    Field("calibrated_groups", Arr(ID, 1, 16, unique=True)),
    # Allowed groups the cohort did not prepare, with the critical cases lacking A0 or a loop
    # count there: never presented as calibrated.
    Field("uncalibrated_groups", Map(ID_PATTERN, Arr(ID, 1, limits.INSTANCES_MAX), 0, 16)),
    # A recomputation from fetched evidence: the history it read and how many refusals it made.
    Field("recomputed", Nullable(Obj(Field("history_latest_run_number", RUN_NUMBER),
                                     Field("refused_evidence", COUNT)))),
    Field("equivalence", Nullable(Obj(
        Field("calibrated_activation_sha256", DIGEST),
        Field("calibrated_authority_sha", GIT_SHA),
        # The protected paths that differ between the calibrated and the bridged authority:
        # only the named adapter files, the adapter registry's support entries and records.
        Field("changed_paths", Arr(REL_PATH, 0, 256, unique=True)),
        # The verified execution evidence: its record and the two runs it was computed from.
        Field("evidence_sha256", DIGEST),
        Field("golden", EQUIVALENCE_RUN), Field("post_merge_aa", EQUIVALENCE_RUN)))),
    Field("pending", Arr(Str(enum=ACTIVATION_PENDING), 1, len(ACTIVATION_PENDING), unique=True)),
)


def validate_activation_verification(doc, where="activation-verification.json"):
    ACTIVATION_VERIFICATION.validate(doc, where)
    runs = doc["original_runs"]
    require(runs == sorted(set(runs)), where + ".original_runs: ascending unique")
    require((doc["mode"] == "fetched") == (doc["recomputed"] is not None),
            where + ".recomputed: exactly a fetched verification recomputes")
    require((doc["admission"] == "equivalent_adapter") == (doc["equivalence"] is not None),
            where + ".equivalence: exactly an equivalent-adapter admission")
    require(not set(doc["calibrated_groups"]) & set(doc["uncalibrated_groups"]),
            where + ".uncalibrated_groups: a group is calibrated or not")
    tallies = doc["tallies"]
    require(tallies["aa_within_tolerance"] == tallies["aa_members"] == COHORT_COUNTS["aa"]
            and tallies["power_members"] == COHORT_COUNTS["power"]
            and tallies["power_qualifying"] >= POWER_DETECTIONS_REQUIRED,
            where + ".tallies: a verified activation is ready")
    required = ["owner_review_at_exact_head"]
    if doc["admission"] == "equivalent_adapter":
        required += ["semantic_equivalence_owner_approval"]
        golden = doc["equivalence"]["golden"]
        require(golden["baseline_sha"] == doc["equivalence"]["calibrated_authority_sha"]
                and golden["candidate_sha"] == doc["authority_sha"],
                where + ".equivalence.golden: the golden check measures the calibrated and the "
                "bridged product sources")
        aa = doc["equivalence"]["post_merge_aa"]
        require(aa["baseline_sha"] == aa["candidate_sha"],
                where + ".equivalence.post_merge_aa: one source against itself")
        for role in ("golden", "post_merge_aa"):
            run = doc["equivalence"][role]
            require((run["provenance"] is not None) == (doc["mode"] == "fetched"),
                    where + ".equivalence.%s.provenance: exactly a fetched verification binds "
                    "the run to its producer, attempt and hosted jobs" % role)
    require(doc["pending"] == required, where + ".pending: the obligations verification "
            "cannot discharge, exactly")
    return doc


# Cost and shipping-budget demonstrations (ADR-0183) -------------------------------------------

COST_SCHEMA = "af.perf-cost/3"
COST_GROUPS = ("pr_cold", "pr_warm", "same_source_cold", "same_source_warm")
COST_FAILURE = Obj(Field("where", SHORT_TEXT), Field("detail", TEXT))
COST_BINARIES = Obj(Field("af", Nullable(DIGEST)), Field("probe", Nullable(DIGEST)))
COST_RUN = Obj(
    Field("run_id", ID), Field("kind", Str(enum=("pr", "same_source"))),
    # The run's compiled-cache treatment as its build manifest observed it (af.perf-builds/6,
    # ADR-0185): cold from observed empty worktree targets, warm from targets a run-owned priming
    # build of the same sources filled; an operator's declaration only for a run whose manifest
    # was not retained; null unknown. The manifest's cache identity binds it.
    Field("cache", Nullable(Str(enum=("cold", "warm")))),
    Field("cache_source", Str(enum=("observed", "declared", "unknown"))),
    Field("cache_identity", Nullable(DIGEST)),
    # Dependency preparation, identified separately: never a compiled build cache.
    Field("dependency_fetch", Nullable(Str(enum=("prefetched", "host_network",
                                                 "container_network")))),
    # complete: no execution failure at all, whatever the verdict (a healthy regression counts);
    # failed: a retained attempt that never counts.
    Field("execution", Str(enum=("complete", "failed"))),
    Field("provenance", Str(enum=("measured", "synthetic", "mixed", "none"))),
    Field("environment_id", ID), Field("recipe_id", ID),
    Field("result", Str(enum=("pass", "fail", "incomplete"))),
    # The retained identities the cost is bound to: sources, plan, run bundle, build manifest,
    # every built binary and each shard bundle (null where not retained).
    Field("identity", Obj(Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
                          Field("plan_digest", DIGEST), Field("run_bundle", Nullable(DIGEST)),
                          Field("builds_sha256", Nullable(DIGEST)),
                          Field("binaries", Obj(Field("baseline", COST_BINARIES),
                                                Field("candidate", COST_BINARIES),
                                                Field("supervisor", Nullable(DIGEST)))),
                          Field("shard_bundles", Arr(Nullable(DIGEST), 1, limits.SHARDS_MAX)))),
    # The measured build pass and a warm run's priming build, each from the build manifest, and
    # each shard's terminal record; null when that evidence was not retained.
    Field("build_ms", Nullable(COUNT)), Field("priming_ms", Nullable(COUNT)),
    Field("shard_sampling_ms", Arr(Nullable(COUNT), 1, limits.SHARDS_MAX)),
    Field("artifact_bytes", Obj(Field("build", Nullable(COUNT)), Field("shards", COUNT))),
    Field("cases", Map(ID_PATTERN, Obj(Field("samples", COUNT), Field("unmeasured", COUNT),
                                       Field("sample_ms", Nullable(RATIONAL))),
                       1, limits.INSTANCES_MAX)),
    Field("failures", Arr(COST_FAILURE, 0, 256)))
COST_GROUP = Obj(Field("runs", COUNT),
                 # Completed measured executions: one per retained run and bundle.
                 Field("completed", COUNT), Field("failed", COUNT), Field("failures", COUNT),
                 Field("build_ms_median", Nullable(RATIONAL)),
                 Field("build_ms_max", Nullable(COUNT)),
                 Field("priming_ms_median", Nullable(RATIONAL)),
                 Field("priming_ms_max", Nullable(COUNT)),
                 Field("sampling_ms_median", Nullable(RATIONAL)),
                 Field("sampling_ms_max", Nullable(COUNT)))
COST = Obj(
    Field("schema", Const(COST_SCHEMA)),
    Field("status", Str(enum=("demonstrated", "pending"))),
    Field("reasons", Arr(TEXT, 0, 64)),
    Field("runs_per_group", Const(5)),
    Field("runs", Arr(COST_RUN, 0, 256)),
    Field("groups", Obj(*[Field(name, COST_GROUP) for name in COST_GROUPS])),
    # Every retained run that did not execute completely: shown, never counted.
    Field("failed_attempts", Arr(ID, 0, 256, unique=True)),
)


def validate_cost(doc, where="cost.json"):
    """Group counts, medians, maxima, failed attempts and the status are recomputed from the
    runs; each run's cache follows from its evidence and its execution from its failures."""
    COST.validate(doc, where)
    from . import costs
    ids = [run["run_id"] for run in doc["runs"]]
    require(ids == sorted(set(ids)), where + ".runs: sorted unique runs")
    bundles = [run["identity"]["run_bundle"] for run in doc["runs"]
               if run["identity"]["run_bundle"] is not None]
    require(len(set(bundles)) == len(bundles), where + ".runs: a run bundle counts once")
    for index, run in enumerate(doc["runs"]):
        at = "%s.runs[%d]" % (where, index)
        source = run["cache_source"]
        require((source == "observed") == (run["cache_identity"] is not None)
                and (source == "observed") == (run["dependency_fetch"] is not None)
                and (source == "unknown") == (run["cache"] is None),
                at + ".cache: observed exactly with the build manifest's cache identity and "
                "dependency fetch, declared only without one")
        require(run["priming_ms"] is None or (source == "observed" and run["cache"] == "warm"),
                at + ".priming_ms: only an observed warm build primed its targets")
        require(source != "observed" or (run["cache"] == "warm") == (run["priming_ms"]
                                                                     is not None),
                at + ".priming_ms: an observed warm build records its priming")
        require((run["execution"] == "complete") == (not run["failures"]),
                at + ".execution: complete exactly without any execution failure")
        require(run["execution"] == "failed" or (
            run["build_ms"] is not None
            and all(value is not None for value in run["shard_sampling_ms"])),
            at + ".execution: a complete run has its build and every shard's time")
    require(doc["groups"] == costs.cost_groups(doc["runs"]),
            where + ".groups: do not match the recomputation from the runs")
    require(doc["failed_attempts"] == costs.failed_attempts(doc["runs"]),
            where + ".failed_attempts: every failed attempt, exactly")
    require((doc["status"], doc["reasons"]) == costs.cost_status(doc["runs"], doc["groups"]),
            where + ".status: does not match the recomputation from the runs")
    return doc


SHIPPING_SCHEMA = "af.perf-shipping/3"
SHIPPING_RESULTS = ("met", "missed", "insufficient", "incomplete")
# One product identity and environment group of a budget instance (ADR-0185): one candidate
# source, shipping binary and recipe on one observed hardware/image fingerprint, with its own
# denominator. Samples are never pooled across targets.
SHIPPING_TARGET = Obj(
    Field("candidate_sha", GIT_SHA), Field("candidate_binary", Nullable(DIGEST)),
    Field("recipe_id", ID), Field("host_fingerprint", DIGEST), Field("host", SHORT_TEXT),
    # Every planned measured candidate slot of the target's runs: a sample, a failed sample or
    # a missing one.
    Field("planned", COUNT), Field("failed", COUNT), Field("missing", COUNT),
    Field("samples", Arr(RATIONAL, 0, 4096)),
    # Exactly the maximum, median and nearest-rank p95 of every retained sample.
    Field("max", Nullable(RATIONAL)), Field("median", Nullable(RATIONAL)),
    Field("p95", Nullable(RATIONAL)),
    Field("result", Str(enum=SHIPPING_RESULTS)),
    Field("runs", Arr(ID, 0, 256, unique=True)),
    # Runs in which a planned sample failed or is missing, whose shard did not complete, or
    # whose build, integrity or shard health evidence is not valid and healthy.
    Field("incomplete_runs", Arr(ID, 0, 256, unique=True)))
SHIPPING_BUDGET = Obj(
    Field("budget_id", ID), Field("environment_id", ID), Field("instance_id", ID),
    Field("limit", DECIMAL), Field("unit", Const("ms")), Field("minimum_samples", Int(1, 4096)),
    Field("targets", Arr(SHIPPING_TARGET, 0, 64)),
    Field("runs", Arr(ID, 0, 256, unique=True)),
    Field("result", Str(enum=SHIPPING_RESULTS)))
SHIPPING_RUN = Obj(
    Field("run_id", ID), Field("plan_digest", DIGEST), Field("run_bundle", Nullable(DIGEST)),
    Field("builds_sha256", Nullable(DIGEST)), Field("candidate_sha", GIT_SHA),
    Field("candidate_binary", Nullable(DIGEST)), Field("environment_id", ID),
    Field("recipe_id", ID),
    Field("provenance", Str(enum=("measured", "synthetic", "mixed", "none"))),
    # The hardware/image fingerprint of every shard the run retained.
    Field("hosts", Arr(DIGEST, 1, limits.SHARDS_MAX, unique=True)))
SHIPPING = Obj(
    Field("schema", Const(SHIPPING_SCHEMA)),
    # The reviewed suite every included run froze: its bytes and policy digest.
    Field("suite_sha256", DIGEST), Field("policy_digest", DIGEST),
    Field("status", Str(enum=("demonstrated", "missed", "pending"))),
    Field("reasons", Arr(TEXT, 0, 256)),
    # Every run given, once: the provenance its samples are audited against.
    Field("runs", Arr(SHIPPING_RUN, 0, 256)),
    Field("budgets", Arr(SHIPPING_BUDGET, 1, 1024)),
)


def validate_shipping(doc, where="shipping.json"):
    """Every sample is checked against its limit, strictly, and every planned slot is accounted
    for in its own target: the distributions, results and status are recomputed from the samples
    and counts, and no target pools another identity's samples."""
    SHIPPING.validate(doc, where)
    from . import costs
    keys = [(entry["budget_id"], entry["environment_id"], entry["instance_id"])
            for entry in doc["budgets"]]
    require(keys == sorted(set(keys)), where + ".budgets: sorted unique budget instances")
    ids = [run["run_id"] for run in doc["runs"]]
    require(ids == sorted(set(ids)), where + ".runs: sorted unique runs")
    bundles = [run["run_bundle"] for run in doc["runs"] if run["run_bundle"] is not None]
    require(len(set(bundles)) == len(bundles), where + ".runs: a run bundle counts once")
    given = dict((run["run_id"], run) for run in doc["runs"])
    for index, entry in enumerate(doc["budgets"]):
        at = "%s.budgets[%d]" % (where, index)
        identities = [(target["candidate_sha"], target["candidate_binary"] or "",
                       target["recipe_id"], target["host_fingerprint"])
                      for target in entry["targets"]]
        require(identities == sorted(set(identities)),
                at + ".targets: one sorted target per product identity and environment group")
        contributed = set()
        for number, target in enumerate(entry["targets"]):
            spot = "%s.targets[%d]" % (at, number)
            values = [parse_rational(value, spot + ".samples") for value in target["samples"]]
            require((target["max"], target["median"], target["p95"]) == costs.distribution(
                values), spot + ": not the maximum, median and nearest-rank p95 of the samples")
            require(target["planned"] == len(values) + target["failed"] + target["missing"],
                    spot + ".planned: every planned slot is a sample, a failure or missing")
            require(set(target["incomplete_runs"]) <= set(target["runs"]),
                    spot + ".incomplete_runs: runs of this target")
            for run_id in target["runs"]:
                run = given.get(run_id)
                require(run is not None and (run["candidate_sha"], run["candidate_binary"],
                                             run["recipe_id"]) == (
                    target["candidate_sha"], target["candidate_binary"], target["recipe_id"])
                    and run["environment_id"] == entry["environment_id"]
                    and target["host_fingerprint"] in run["hosts"],
                    spot + ".runs: run %s is not of this target's product identity and "
                    "environment group" % run_id)
            contributed.update(target["runs"])
            require(target["result"] == costs.shipping_result(values, dict(
                target, limit=entry["limit"], minimum_samples=entry["minimum_samples"])),
                spot + ".result: every sample strictly below the limit, at least the minimum, "
                "none failed or missing, healthy evidence")
        require(entry["runs"] == sorted(contributed), at + ".runs: exactly its targets' runs")
        require(entry["result"] == costs.budget_result(entry),
                at + ".result: met only when every target is met")
    require(doc["status"] == costs.shipping_status(doc["budgets"], doc["reasons"]),
            where + ".status: does not match the recomputation from the budgets")
    return doc

PROBE_SCHEMA = "af.perf-probe/1"
PROBE = Obj(
    Field("schema", Const(PROBE_SCHEMA)), Field("case_id", ID), Field("instance_id", ID),
    Field("contract_id", ID), Field("fixture_digest", Nullable(DIGEST)),
    Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 0, 4)),
    Field("metrics", Map(ID_PATTERN, METRIC, 1, 32)),
    Field("correctness", Obj(Field("status", Str(enum=("passed", "failed"))),
                             Field("digest", Nullable(DIGEST)), Field("count", Nullable(COUNT)),
                             Field("detail", TEXT))),
)


def validate_probe(doc, where="perf-probe"):
    PROBE.validate(doc, where)
    correctness = doc["correctness"]
    require(correctness["digest"] is not None or correctness["count"] is not None,
            where + ".correctness: names a digest or a count to check against golden entries")
    return doc


def read_probe_line(data, where="perf-probe"):
    require(len(data) <= limits.RECORD_BYTES_MAX, where + ": a control message exceeds 64 KiB")
    return validate_probe(loads_strict(data.rstrip(b"\n"), where, limits.RECORD_BYTES_MAX), where)


# af.perf-probe-request/2 -------------------------------------------------------------------

PROBE_REQUEST_SCHEMA = suite.PROBE_REQUEST_SCHEMA
PROBE_PATH = Str(pattern=r"^/", min_len=2, max_len=1024)
PROBE_REQUEST = Obj(
    Field("schema", Const(PROBE_REQUEST_SCHEMA)), Field("case_id", ID), Field("instance_id", ID),
    Field("contract_id", ID), Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 0, 4)),
    Field("repetitions", Int(1, 100000)),
    Field("scratch", PROBE_PATH),
    # The sample's private copy of the seed its contract reads, or null (ADR-0170).
    Field("seed", Nullable(PROBE_PATH)),
)


def validate_probe_request(doc, where="perf-probe-request"):
    PROBE_REQUEST.validate(doc, where)
    expected = suite.instance_id(doc["case_id"], sorted(doc["parameters"].items()))
    require(doc["instance_id"] == expected,
            where + ".instance_id: does not name the assignment; expected " + expected)
    seed = doc["seed"]
    require(seed is None or not (seed + "/").startswith(doc["scratch"].rstrip("/") + "/"),
            where + ".seed: the seed copy lies outside the probe's scratch")
    return doc


# af.perf-golden/2 --------------------------------------------------------------------------

GOLDEN_SCHEMA = reference.GOLDEN_SCHEMA
GOLDEN_FIXTURE = Tagged("kind", {
    "typed_envelope": tagged_variant(
        "kind", "typed_envelope",
        Field("artifact_type", Str(enum=(reference.REQUIREMENTS, reference.TASK_PACKAGE))),
        Field("serialized_envelope_bytes", Int(1, suite.ARTIFACT_ENVELOPE_CAP_BYTES)),
        Field("generation", Tagged("kind", {
            "requirements_text": tagged_variant("kind", "requirements_text",
                                                Field("text_bytes", COUNT)),
            "author_package_padding": tagged_variant(
                "kind", "author_package_padding", Field("padding_digits", Int(1, 2 ** 31)),
                Field("padding_tens", Int(0, 1))),
        })),
        Field("artifact_id", DIGEST), Field("content_id", DIGEST),
        Field("envelope_blob_id", DIGEST)),
    "canonical_small": tagged_variant(
        "kind", "canonical_small", Field("canonical", Str(min_len=2, max_len=4096)),
        Field("content_id", DIGEST), Field("repetitions", Int(1, 100000))),
    "file_tree": tagged_variant(
        "kind", "file_tree", Field("file_bytes", Int(1, 1 << 20)),
        Field("trees", Map(r"^[1-9][0-9]{0,6}$", Obj(Field("files", Int(1, 10 ** 7)),
                                                    Field("manifest_digest", DIGEST)), 1, 8))),
    "event_log": tagged_variant(
        "kind", "event_log", Field("events", Int(1, 10 ** 7)),
        Field("prefix_events", Int(0, 16)), Field("relevant_events", TEXT)),
})
GOLDEN_ENTRY = Obj(
    Field("instance_id", ID), Field("case_id", ID), Field("contract_id", ID),
    Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 0, 4)),
    Field("fixture_digest", Nullable(DIGEST)), Field("digest", DIGEST), Field("count", COUNT),
    Field("digest_of", Str(enum=("reference_bytes", "semantic"))),
    Field("semantic", Nullable(JsonObject())), Field("basis", SHORT_TEXT),
)
GOLDEN = Obj(
    Field("schema", Const(GOLDEN_SCHEMA)), Field("generator", Const(reference.GENERATOR)),
    Field("note", TEXT),
    Field("constants", Obj(Field("task_lease_ms", Const(suite.TASK_LEASE_MS)),
                           Field("artifact_envelope_cap_bytes",
                                 Const(suite.ARTIFACT_ENVELOPE_CAP_BYTES)),
                           Field("policy_id", DIGEST))),
    Field("fixtures", Map(ID_PATTERN, GOLDEN_FIXTURE, 1, 64)),
    Field("cap_boundary", Obj(
        Field("admitted", Obj(Field("serialized_envelope_bytes",
                                    Const(suite.ARTIFACT_ENVELOPE_CAP_BYTES)),
                              Field("text_bytes", COUNT), Field("artifact_id", DIGEST))),
        Field("refused", Obj(Field("serialized_envelope_bytes",
                                   Const(suite.ARTIFACT_ENVELOPE_CAP_BYTES + 1)),
                             Field("text_bytes", COUNT))))),
    Field("entries", Arr(GOLDEN_ENTRY, 1, 128)),
    # What each seed writer's own check must report, by seed ID (ADR-0170).
    Field("seeds", Arr(GOLDEN_ENTRY, 0, 32)),
)


def validate_golden(doc, where="golden"):
    GOLDEN.validate(doc, where)
    for key in ("entries", "seeds"):
        ids = [entry["instance_id"] for entry in doc[key]]
        require(ids == sorted(set(ids)), where + ".%s: IDs are unique and sorted" % key)
    for index, (key, entry) in enumerate([("entries", entry) for entry in doc["entries"]]
                                         + [("seeds", entry) for entry in doc["seeds"]]):
        at = "%s.%s[%d]" % (where, key, index)
        require(entry["instance_id"] == suite.instance_id(
            entry["case_id"], sorted(entry["parameters"].items())),
            at + ".instance_id: does not name the assignment")
        if entry["digest_of"] == "semantic":
            require(entry["semantic"] is not None
                    and entry["digest"] == reference.content_id(
                        reference.canonical(entry["semantic"], at + ".semantic")),
                    at + ".digest: is not the content ID of its semantic projection")
    cap = doc["cap_boundary"]
    require(cap["refused"]["text_bytes"] == cap["admitted"]["text_bytes"] + 1,
            where + ".cap_boundary: the refused envelope is one text byte past the cap")
    return doc


# af.perf-adapters/2 ------------------------------------------------------------------------

ADAPTERS_SCHEMA = "af.perf-adapters/2"
ADAPTERS_PER_CONTRACT_MAX = 4
# An adapter overlays only example sources of an existing package: it never replaces a product
# manifest, a crate's own sources or its dependency lock.
ADAPTER_PATH = r"^crates/[a-z0-9-]+/examples/[A-Za-z0-9._/-]+\.rs$"
SEMVER_TEXT = Str(pattern=r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$", max_len=64)
ADAPTER = Obj(
    Field("kind", Const("native_probe")),
    Field("description", TEXT),
    Field("priority", Int(1, ADAPTERS_PER_CONTRACT_MAX)),
    Field("equivalence_group", ID),
    # The first AF release whose public API the adapter is declared to cover; a release
    # before it has no declared native coverage and is refused before any build.
    Field("min_version", SEMVER_TEXT),
    Field("package", Str(pattern=r"^[a-z0-9-]{1,64}$", max_len=64)),
    Field("example", Str(pattern=r"^[a-z0-9-]{1,64}$", max_len=64)),
    Field("contracts", Arr(ID, 1, 64, unique=True)),
    Field("closure", Arr(Str(pattern=ADAPTER_PATH, max_len=256), 1, 32, unique=True)),
)
ADAPTERS = Obj(
    Field("schema", Const(ADAPTERS_SCHEMA)),
    Field("description", TEXT),
    Field("adapters", Map(ID_PATTERN, ADAPTER, 1, 16)),
)


def validate_adapters(doc, where="adapters.toml"):
    """Fixed unique priorities, at most four adapters per contract, sorted closures."""
    ADAPTERS.validate(doc, where)
    adapters = doc["adapters"]
    priorities = [entry["priority"] for entry in adapters.values()]
    require(len(set(priorities)) == len(priorities),
            where + ".adapters: every adapter has its own fixed priority")
    serving = {}
    for adapter_id, entry in sorted(adapters.items()):
        at = where + ".adapters." + adapter_id
        require(entry["contracts"] == sorted(entry["contracts"]), at + ".contracts: sorted")
        require(entry["closure"] == sorted(entry["closure"]), at + ".closure: sorted")
        entry_point = "crates/%s/examples/%s.rs" % (entry["package"], entry["example"])
        require(entry_point in entry["closure"],
                at + ".closure: holds its example's entry point " + entry_point)
        prefix = "crates/%s/examples/" % entry["package"]
        require(all(path.startswith(prefix) for path in entry["closure"]),
                at + ".closure: lies below its own package's examples/")
        for contract in entry["contracts"]:
            serving.setdefault(contract, []).append(adapter_id)
    for contract, ids in sorted(serving.items()):
        require(len(ids) <= ADAPTERS_PER_CONTRACT_MAX,
                where + ": contract %s has more than %d adapters"
                % (contract, ADAPTERS_PER_CONTRACT_MAX))
    return doc


# af.perf-cli/6 -----------------------------------------------------------------------------

CLI_SCHEMA = "af.perf-cli/6"
TUI_OPEN_SCHEMA = "af.perf-tui-open/1"
TUI_OPEN = Obj(Field("schema", Const(TUI_OPEN_SCHEMA)),
               Field("browser_ms", DECIMAL),
               Field("splash_ms", DECIMAL),
               Field("exit_code", Const(0)))


def validate_tui_open(doc, where="tui-open"):
    TUI_OPEN.validate(doc, where)
    require(parse_decimal(doc["splash_ms"], where + ".splash_ms") <=
            parse_decimal(doc["browser_ms"], where + ".browser_ms"),
            where + ": splash arrived after the browser frame")
    return doc
CLI_SETUPS = ("none", "task_repository", "task_started", "seed_copy", "registry_empty",
              "registry_stores_20x10000")
CLI_PLACEHOLDERS = ("af", "probe", "scratch", "package_version", "fixtures", "python")
# The reviewed PTY driver's command (ADR-0263): the absolute interpreter `{python}` names, never
# one the supervisor would look up on PATH, runs the driver isolated around the side's AF.
TUI_DRIVER_ARGV = ["{python}", "-I", "{fixtures}/tui-open.py", "{af}"]
AF_TASK_ID = Str(pattern=r"^[a-z][a-z0-9-]{0,63}$", max_len=64)
JSON_PATH = Str(pattern=r"^(?:[a-z0-9_]+|\*)(?:\.(?:[a-z0-9_]+|\*))*$", max_len=256)
# A path the supervisor's streaming JSON observation selects (ADR-0172): one to eight segments,
# each a member name of at most 64 characters or `*`.
JSON_STREAM_PATH = Str(pattern=r"^(?:[a-z0-9_]{1,64}|\*)(?:\.(?:[a-z0-9_]{1,64}|\*)){0,7}$",
                       max_len=520)
FIELD_PATH = r"^[a-z_][a-z0-9_]*(?:\.[a-z_][a-z0-9_]*)*$"
CLI_RULE = Tagged("kind", {
    "exit_code": tagged_variant("kind", "exit_code", Field("value", Int(-1, 255))),
    "stdout_lines": tagged_variant("kind", "stdout_lines", Field("value", Int(1, 10 ** 6))),
    "stdout_line": tagged_variant("kind", "stdout_line", Field("index", Int(0, 1024)),
                                  Field("equals", Str(min_len=1, max_len=512))),
    "stdout_contains": tagged_variant("kind", "stdout_contains",
                                      Field("value", Str(min_len=1, max_len=512))),
    # Counted by the supervisor over the entire stdout, so it holds for output of any size.
    "stdout_count": tagged_variant("kind", "stdout_count",
                                   Field("pattern", Str(min_len=1, max_len=128)),
                                   Field("value", Int(0, 10 ** 9))),
    # Declared fields of the entire stdout as one complete JSON document, which the supervisor
    # observes past any retained log (ADR-0172); member order, whitespace and undeclared fields
    # are free. `json_stream_equals`: the path reaches exactly one value, this string.
    # `json_stream_sequence`: every value the parent path reaches holds the path's member
    # exactly once, a plain nonnegative integer, numbered exactly first..last in order.
    # `json_stream_count`: exactly `count` of the path's values are this string.
    "json_stream_equals": tagged_variant("kind", "json_stream_equals",
                                         Field("path", JSON_STREAM_PATH),
                                         Field("value", Str(min_len=1, max_len=256))),
    "json_stream_sequence": tagged_variant("kind", "json_stream_sequence",
                                           Field("path", JSON_STREAM_PATH),
                                           Field("first", Int(0, 10 ** 7)),
                                           Field("last", Int(0, 10 ** 7))),
    "json_stream_count": tagged_variant("kind", "json_stream_count",
                                        Field("path", JSON_STREAM_PATH),
                                        Field("value", Str(min_len=1, max_len=256)),
                                        Field("count", Int(0, 10 ** 9))),
    "json_equals": tagged_variant("kind", "json_equals", Field("path", JSON_PATH),
                                  Field("value", AnyOf(Str(max_len=512), Int(-2 ** 53, 2 ** 53),
                                                       Bool()))),
    "json_matches": tagged_variant("kind", "json_matches", Field("path", JSON_PATH),
                                   Field("pattern", Str(min_len=1, max_len=256))),
    "json_count": tagged_variant("kind", "json_count", Field("path", JSON_PATH),
                                 Field("value", Int(0, 10 ** 7))),
    "json_contiguous": tagged_variant("kind", "json_contiguous", Field("path", JSON_PATH),
                                      Field("start", Int(0, 10 ** 7)),
                                      Field("count", Int(1, 10 ** 7))),
    "json_any": tagged_variant("kind", "json_any", Field("path", JSON_PATH),
                               Field("where", Map(FIELD_PATH, Str(min_len=1, max_len=256),
                                                  1, 8))),
    # Every declared item, exactly once (ADR-0170): every complete stdout line matches
    # `pattern`, whose group `n` is a decimal item number, the numbers are exactly
    # `first..last`, and the values of each `distinct` group differ between lines.
    "stdout_enumerates": tagged_variant(
        "kind", "stdout_enumerates", Field("pattern", Str(min_len=1, max_len=512)),
        Field("first", Int(0, 10 ** 6)), Field("last", Int(0, 10 ** 6)),
        Field("distinct", Arr(ID, 0, 4, unique=True))),
    # Every declared item, exactly once, in a JSON list: each element matches the `where`
    # values (`{n}` in one stands for its item number) and has the integer item number at
    # each `numbered` field, for exactly one n of `first..last`; every n is matched by exactly
    # one element, and each `distinct` field is a string no two elements share.
    "json_enumerates": tagged_variant(
        "kind", "json_enumerates", Field("path", JSON_PATH),
        Field("first", Int(0, 10 ** 6)), Field("last", Int(0, 10 ** 6)),
        Field("where", Map(FIELD_PATH, Str(min_len=1, max_len=256), 1, 8)),
        Field("numbered", Arr(Str(pattern=FIELD_PATH, max_len=128), 0, 4, unique=True)),
        Field("distinct", Arr(Str(pattern=FIELD_PATH, max_len=128), 0, 4, unique=True))),
    # A summary consistent with its list: for every key of the object at `counts`, exactly that
    # many elements at `path` have that value at `field`, and every element's value is a key.
    "json_tally": tagged_variant(
        "kind", "json_tally", Field("path", JSON_PATH),
        Field("field", Str(pattern=FIELD_PATH, max_len=128)), Field("counts", JSON_PATH)),
})
CLI_COMMAND = Obj(
    Field("case_id", ID),
    Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 0, 4)),
    Field("setup", Str(enum=CLI_SETUPS)),
    Field("seed", ID, required=False),
    Field("argv", Arr(Str(min_len=1, max_len=512), 1, 32), required=False),
    Field("cwd", Str(min_len=1, max_len=256), required=False),
    Field("rules", Arr(CLI_RULE, 1, 32), required=False),
    Field("pending", Str(min_len=1, max_len=512), required=False),
    Field("description", TEXT),
)
CLI_ADAPTER = Obj(
    Field("description", TEXT), Field("priority", Int(1, 4)),
    Field("min_version", SEMVER_TEXT),
    Field("commands", Arr(CLI_COMMAND, 1, 64)),
)
CLI = Obj(
    Field("schema", Const(CLI_SCHEMA)), Field("description", TEXT),
    Field("setup_script", REL_PATH),
    Field("task_fixture", Obj(Field("directory", REL_PATH), Field("task_id", AF_TASK_ID),
                              Field("description", TEXT))),
    # Large state each side writes once per shard through its own probe contract (and, for a
    # Campaign, its own `af review run`), below `probe_scratch` of the writer's scratch.
    Field("seeds", Map(ID_PATTERN, Obj(Field("writer", Str(enum=CLI_SEED_WRITERS)),
                                       Field("case_id", ID), Field("contract_id", ID),
                                       Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 1, 4)),
                                       Field("probe_scratch", REL_PATH),
                                       Field("events", Int(1, 10 ** 7)),
                                       Field("description", TEXT)), 1, 8)),
    Field("adapters", Map(ID_PATTERN, CLI_ADAPTER, 1, 4)),
)


def validate_cli(doc, where="cli.toml"):
    CLI.validate(doc, where)
    priorities = sorted(adapter["priority"] for adapter in doc["adapters"].values())
    require(priorities == sorted(set(priorities)), where + ".adapters: unique priorities")
    for adapter_id, adapter in sorted(doc["adapters"].items()):
        seen = set()
        for index, command in enumerate(adapter["commands"]):
            at = "%s.adapters.%s.commands[%d]" % (where, adapter_id, index)
            key = (command["case_id"], canonical_bytes(command["parameters"]))
            require(key not in seen, at + ": one command per instance")
            seen.add(key)
            runnable = [name for name in ("argv", "cwd", "rules") if name in command]
            if "pending" in command:
                require(not runnable and command["setup"] == "none" and "seed" not in command,
                        at + ": a pending command declares nothing to run")
                continue
            require(("seed" in command) == (command["setup"] in
                    ("seed_copy", "registry_stores_20x10000")),
                    at + ".seed: named exactly by a seed-copying setup")
            require("seed" not in command or command["seed"] in doc["seeds"],
                    at + ".seed: %s is not a declared seed" % command.get("seed"))
            require(len(runnable) == 3, at + ": a command declares argv, cwd and rules, or "
                    "is pending with its reason")
            driver = command["case_id"] == "tui_open"
            require(command["argv"] == TUI_DRIVER_ARGV if driver else
                    command["argv"][0] == "{af}",
                    at + ".argv: runs the side's AF binary or the declared PTY driver")
            require(driver or not any("{python}" in text
                                      for text in command["argv"] + [command["cwd"]]),
                    at + ".argv: only the PTY driver's command names {python}")
            for text in command["argv"] + [command["cwd"]]:
                for name in re.findall(r"\{([^}]*)\}", text):
                    require(name in CLI_PLACEHOLDERS,
                            at + ": undeclared placeholder {%s}" % name)
            require(command["cwd"].startswith("{scratch}"),
                    at + ".cwd: commands run in the sample's private scratch")
            kinds = [rule["kind"] for rule in command["rules"]]
            require(kinds.count("exit_code") == 1, at + ".rules: exactly one exit_code rule")
            require(kinds.count("stdout_count") <= 8,
                    at + ".rules: the supervisor counts at most eight patterns")
            require(sum(1 for kind in kinds if kind.startswith("json_stream_")) <= 8,
                    at + ".rules: the supervisor observes at most eight JSON selectors")
            require(len(kinds) >= 2, at + ".rules: an exit status alone never passes")
            for rule in command["rules"]:
                if rule["kind"] in ("json_matches", "stdout_enumerates"):
                    try:
                        compiled = re.compile(rule["pattern"])
                    except re.error as error:
                        raise ContractError(at + ".rules: invalid pattern: %s" % error)
                if rule["kind"] in ("stdout_enumerates", "json_enumerates"):
                    require(rule["first"] <= rule["last"]
                            and rule["last"] - rule["first"] < 100000,
                            at + ".rules: an enumeration runs first..last, at most 100000 items")
                if rule["kind"] == "json_stream_sequence":
                    require(rule["first"] <= rule["last"]
                            and rule["path"].rsplit(".", 1)[-1] != "*",
                            at + ".rules: a sequence runs first..last at an object member")
                if rule["kind"] in ("json_stream_equals", "json_stream_count"):
                    require(len(rule["value"].encode("utf-8")) <= 256,
                            at + ".rules: a declared value is at most 256 bytes")
                if rule["kind"] == "stdout_enumerates":
                    groups = compiled.groupindex
                    require("n" in groups and all(name in groups for name in rule["distinct"]),
                            at + ".rules: the pattern names group n and every distinct group")
                if rule["kind"] == "json_enumerates":
                    require(any("{n}" in value for value in rule["where"].values())
                            or rule["numbered"],
                            at + ".rules: an enumeration identifies each item by its number")
    return doc


# af.perf-supervisor-request/4 --------------------------------------------------------------

SUPERVISOR_REQUEST_SCHEMA = "af.perf-supervisor-request/4"
ABSOLUTE = Str(pattern=r"^/", min_len=1, max_len=4096)
COMMAND_ARGV = Arr(Str(max_len=4096), 1, 64)
COMMAND_ENVIRONMENT = Map(r"^[A-Z_][A-Z0-9_]{0,63}$", Str(max_len=4096), 0, 64)
# The untimed fixture step a sample may run first, under the same containment and in the same
# scratch (ADR-0168). Its output is drained and counted, never judged or returned.
SETUP = Obj(
    Field("argv", COMMAND_ARGV), Field("environment", COMMAND_ENVIRONMENT),
    Field("cwd", ABSOLUTE),
    Field("timeout_ms", Int(1, limits.SAMPLE_TIMEOUT_SECONDS_MAX * 1000)),
)
# One streamed JSON selector: a path, the kind of its values and, for a string selector, the
# declared values whose occurrences are counted.
JSON_SELECTOR = Obj(Field("path", JSON_STREAM_PATH),
                    Field("kind", Str(enum=("integer", "string"))),
                    Field("equals", Arr(Str(min_len=1, max_len=256), 0, 4)))
SUPERVISOR_REQUEST = Obj(
    Field("schema", Const(SUPERVISOR_REQUEST_SCHEMA)),
    Field("argv", COMMAND_ARGV),
    Field("environment", COMMAND_ENVIRONMENT),
    Field("cwd", ABSOLUTE),
    Field("stdin", Str(max_len=limits.RECORD_BYTES_MAX)),
    Field("timeout_ms", Int(1, limits.SAMPLE_TIMEOUT_SECONDS_MAX * 1000)),
    Field("stdout_cap_bytes", Int(1, limits.SAMPLE_LOG_BYTES_MAX)),
    Field("log_cap_bytes", Int(1, limits.SAMPLE_LOG_BYTES_MAX)),
    Field("cleanup_grace_ms", Int(1, 30000)),
    Field("containment", Str(enum=CONTAINMENTS)),
    Field("scratch", Nullable(Str(pattern=r"^/.", max_len=4096))),
    Field("setup", Nullable(SETUP)),
    # Byte patterns the supervisor counts over the child's whole stdout, past its cap too.
    Field("stdout_patterns", Arr(Str(min_len=1, max_len=128), 0, 8)),
    # Declared fields the supervisor observes over the child's whole stdout as one JSON
    # document, past its cap too (ADR-0172).
    Field("stdout_json", Arr(JSON_SELECTOR, 0, 8)),
)


def validate_supervisor_request(doc, where="perf-supervisor-request"):
    SUPERVISOR_REQUEST.validate(doc, where)
    commands = [(doc["argv"], doc["environment"], doc["cwd"], where)]
    if doc["setup"] is not None:
        setup = doc["setup"]
        commands.append((setup["argv"], setup["environment"], setup["cwd"], where + ".setup"))
    for argv, environment, cwd, at in commands:
        require(argv[0].startswith("/"), at + ".argv: the program is an absolute path")
        require(all("\x00" not in value for value in argv + list(environment.values()) + [cwd]),
                at + ": no argument, value or path holds NUL")
    require(doc["stdout_cap_bytes"] <= doc["log_cap_bytes"],
            where + ".log_cap_bytes: covers stdout and stderr together")
    require(doc["scratch"] is None or doc["containment"] == "pid_namespace",
            where + ".scratch: only pid_namespace containment owns a scratch")
    require(all(len(pattern.encode("utf-8")) <= 128 for pattern in doc["stdout_patterns"]),
            where + ".stdout_patterns: each pattern is at most 128 bytes")
    for index, selector in enumerate(doc["stdout_json"]):
        require((selector["kind"] == "integer") == (not selector["equals"])
                and all(len(value.encode("utf-8")) <= 256 for value in selector["equals"]),
                where + ".stdout_json[%d]: an integer selector declares no values and a string "
                "selector 1 to 4 values of at most 256 bytes" % index)
    require(len(canonical_bytes(doc)) <= limits.RECORD_BYTES_MAX,
            where + ": a request is at most 64 KiB")
    return doc


# af.perf-supervisor-result/5 ---------------------------------------------------------------

SUPERVISOR_RESULT_SCHEMA = "af.perf-supervisor-result/5"
# The result's own termination adds `setup_failed`: a declared setup did not end as required, so
# no measured child was launched; the lifecycle is then the setup's.
RESULT_TERMINATIONS = TERMINATIONS + ("setup_failed",)
STREAM = Obj(Field("retained", COUNT), Field("total", COUNT), Field("truncated", COUNT))
LIFECYCLE = Obj(Field("observation", Str(enum=("pid_namespace_scan", "process_group_scan",
                                               "pipes_only"))),
                Field("descendants_at_exit", Nullable(COUNT)),
                Field("remaining_after_cleanup", Nullable(COUNT)),
                Field("pipes_held", Bool()),
                Field("status", Str(enum=LIFECYCLE_STATUSES)),
                Field("detail", Str(max_len=4096)))
TERMINATION = Obj(Field("kind", Str(enum=TERMINATIONS)), Field("code", Nullable(Int(-1, 255))),
                  Field("signal", Nullable(Int(1, 128))), Field("detail", Str(max_len=4096)))
# A setup's record: how it ended, its untimed duration, output counts, a bounded stderr tail.
SETUP_RESULT = Obj(
    Field("termination", TERMINATION), Field("wall_ns", Nullable(NANOS)),
    Field("exit_observation", Str(enum=EXIT_OBSERVATIONS)),
    Field("stdout", STREAM), Field("stderr", STREAM),
    Field("stderr_tail", Str(max_len=4096)), Field("lifecycle", LIFECYCLE),
)
# The declared selectors' observation of the child's whole stdout as one JSON document
# (ADR-0172): whether it was exactly one complete document, and per selector the values its
# parent path reached (`holders`), its own values, repeated selected member names and values of
# another kind (the first by index); an integer selector's first and last valid value and the
# values that were not the previous plus one (the first of them); a string selector's count of
# each declared value.
SEQUENCE_NUMBER = Nullable(Int(0, 2 ** 64 - 1))
JSON_DOCUMENT = Obj(Field("status", Str(enum=("complete", "malformed", "incomplete",
                                              "too_deep"))),
                    Field("offset", Nullable(COUNT)), Field("detail", Str(max_len=256)))
JSON_SELECTED = Obj(Field("holders", COUNT), Field("count", COUNT), Field("duplicates", COUNT),
                    Field("invalid", COUNT), Field("first_invalid", Nullable(COUNT)),
                    Field("first", SEQUENCE_NUMBER), Field("last", SEQUENCE_NUMBER),
                    Field("breaks", COUNT),
                    Field("first_break", Nullable(Obj(Field("index", COUNT),
                                                      Field("after", SEQUENCE_NUMBER),
                                                      Field("value", SEQUENCE_NUMBER)))),
                    Field("matches", Arr(COUNT, 0, 4)))
STDOUT_JSON = Nullable(Obj(Field("document", JSON_DOCUMENT),
                           Field("selectors", Arr(JSON_SELECTED, 0, 8))))
OBSERVED = Map(r"^[a-z0-9_./-]{1,64}$",
               AnyOf(Obj(Field("value", Str(max_len=1024))),
                     Obj(Field("unavailable", Str(max_len=4096)))), 0, 32)
SUPERVISOR_RESULT = Obj(
    Field("schema", Const(SUPERVISOR_RESULT_SCHEMA)),
    Field("supervisor", Obj(Field("platform", Str(min_len=1, max_len=32)),
                            Field("arch", Str(min_len=1, max_len=32)),
                            Field("namespace_init", Bool()),
                            Field("containment", Str(enum=CONTAINMENTS)))),
    Field("termination", Obj(Field("kind", Str(enum=RESULT_TERMINATIONS)),
                             Field("code", Nullable(Int(-1, 255))),
                             Field("signal", Nullable(Int(1, 128))),
                             Field("detail", Str(max_len=4096)))),
    Field("timing", Obj(Field("child_wall_ns", Nullable(NANOS)),
                        Field("exit_observation", Str(enum=EXIT_OBSERVATIONS)),
                        Field("before_launch_ns", NANOS), Field("cleanup_ns", NANOS),
                        Field("after_exit_ns", NANOS))),
    Field("stdout", STREAM), Field("stderr", STREAM),
    Field("stdout_pattern_counts", Arr(COUNT, 0, 8)),
    Field("stdout_json", STDOUT_JSON),
    Field("lifecycle", LIFECYCLE),
    Field("setup", Nullable(SETUP_RESULT)),
    Field("scratch", Nullable(Obj(Field("status", Str(enum=("cleared", "retained", "refused"))),
                                  Field("detail", Str(max_len=4096))))),
    Field("rusage", Nullable(Obj(Field("scope", Const("reaped_child_and_its_waited_descendants")),
                                 Field("user_cpu_ns", NANOS), Field("system_cpu_ns", NANOS),
                                 Field("maxrss_bytes", COUNT), Field("minor_faults", COUNT),
                                 Field("major_faults", COUNT),
                                 Field("voluntary_switches", COUNT),
                                 Field("involuntary_switches", COUNT)))),
    Field("rusage_unavailable", Nullable(Str(max_len=4096))),
    Field("observations", Obj(Field("before", OBSERVED), Field("after", OBSERVED),
                              Field("unavailable", Nullable(Str(max_len=4096))))),
)


def _check_execution(doc, where):
    """The rules one launched child's record obeys, the measured child's or a setup's: its
    termination, how its exit was observed, its interval and its lifecycle (ADR-0167)."""
    termination = doc["termination"]
    kind = termination["kind"]
    observed = doc["timing"]["exit_observation"]
    status = doc["lifecycle"]["status"]
    require((termination["code"] is not None) == (kind == "exited"),
            where + ".termination.code: exactly for an exit")
    require((termination["signal"] is not None) == (kind in ("signalled", "cancelled")),
            where + ".termination.signal: exactly for a signal or a cancellation")
    require((doc["timing"]["child_wall_ns"] is not None)
            == (kind in ("exited", "signalled") and observed == "unreaped"),
            where + ".timing.child_wall_ns: exactly for a child that ended by itself and whose "
            "exit was observed unreaped")
    require((observed == "none") == (kind == "launch_failed"),
            where + ".timing.exit_observation: none exactly when nothing was launched")
    require(observed != "unobserved" or kind in ("timeout", "cancelled"),
            where + ".timing.exit_observation: an unobserved exit ends only at the deadline or "
            "a cancellation")
    require(kind != "status_unknown" or (observed != "none" and termination["detail"]),
            where + ".termination: an unknown status of a launched child, with its reason")
    require(observed not in ("reaped_by_polling", "unobserved") and kind != "status_unknown"
            or status == "cleanup_uncertain",
            where + ".lifecycle.status: an exit not observed unreaped, or an unknown status, "
            "leaves the cleanup uncertain")
    require(observed != "unobserved" or doc["rusage"] is None,
            where + ".rusage: none for a child that was never reaped")
    for stream in ("stdout", "stderr"):
        item = doc[stream]
        require(item["truncated"] == item["total"] - item["retained"] and item["truncated"] >= 0,
                where + "." + stream + ": truncated counts exactly what was not retained")
    _check_lifecycle(doc, where + ".lifecycle")


def setup_accepted(setup):
    """Whether a setup record lets the measured child launch: an exit 0 observed unreaped,
    with nothing left behind that its containment could see."""
    lifecycle = setup["lifecycle"]
    return (setup["termination"]["kind"] == "exited" and setup["termination"]["code"] == 0
            and setup["exit_observation"] == "unreaped"
            and (lifecycle["status"] == "clean"
                 or (lifecycle["status"] == "no_observed_descendants"
                     and lifecycle["observation"] == "pipes_only")))


def validate_supervisor_result(doc, where="perf-supervisor-result"):
    SUPERVISOR_RESULT.validate(doc, where)
    kind = doc["termination"]["kind"]
    setup = doc["setup"]
    if setup is not None:
        # The setup is one more launched child, checked by the same rules in its own record.
        _check_execution({"termination": setup["termination"],
                          "timing": {"child_wall_ns": setup["wall_ns"],
                                     "exit_observation": setup["exit_observation"]},
                          "stdout": setup["stdout"], "stderr": setup["stderr"],
                          "lifecycle": setup["lifecycle"], "rusage": None, "scratch": None,
                          "supervisor": doc["supervisor"]}, where + ".setup")
    if kind == "setup_failed":
        require(setup is not None and not setup_accepted(setup),
                where + ".termination: setup_failed exactly after a setup that did not exit 0 "
                "with a verified lifecycle")
        require(doc["termination"]["code"] is None and doc["termination"]["signal"] is None
                and doc["termination"]["detail"],
                where + ".termination: a failed setup states why")
        require(doc["timing"]["child_wall_ns"] is None
                and doc["timing"]["exit_observation"] == "none"
                and doc["stdout"]["total"] == 0 and doc["stderr"]["total"] == 0
                and doc["rusage"] is None,
                where + ": no measured child ran after a failed setup")
        require(doc["lifecycle"] == setup["lifecycle"],
                where + ".lifecycle: after a failed setup, the setup's lifecycle")
        scratch = doc["scratch"]
        require(scratch is None or scratch["status"] != "cleared"
                or (setup["lifecycle"]["status"] in ("clean", "unexpected_descendants")
                    and setup["lifecycle"]["observation"] == "pid_namespace_scan"),
                where + ".scratch: cleared only after a verified-empty namespace")
    else:
        require(setup is None or setup_accepted(setup),
                where + ".setup: the measured child launched only after an accepted setup")
        _check_execution(doc, where)
    require((doc["rusage"] is None) != (doc["rusage_unavailable"] is None),
            where + ".rusage: present, or unavailable with its reason")
    observed = doc["stdout_json"]
    if observed is not None:
        document = observed["document"]
        require((document["status"] == "complete") == (document["offset"] is None)
                and (document["offset"] is None
                     or document["offset"] <= doc["stdout"]["total"]),
                where + ".stdout_json.document: an offset within stdout exactly when it is not "
                "complete")
        for index, selected in enumerate(observed["selectors"]):
            at = "%s.stdout_json.selectors[%d]" % (where, index)
            broke = selected["first_break"]
            require(selected["breaks"] < max(selected["count"], 1)
                    and (broke is None) == (selected["breaks"] == 0)
                    and (broke is None or 0 < broke["index"] < selected["count"]),
                    at + ": breaks are later values, the first of them named")
            require(selected["count"] > 0 or (selected["first"] is None
                                              and selected["last"] is None),
                    at + ": no value without an occurrence")
            require(selected["invalid"] <= selected["count"]
                    and (selected["first_invalid"] is None) == (selected["invalid"] == 0)
                    and (selected["first_invalid"] is None
                         or selected["first_invalid"] < selected["count"]),
                    at + ": invalid values are values, the first of them named")
            require(all(found <= selected["count"] for found in selected["matches"]),
                    at + ".matches: no more than its values")
            require(max(selected["count"], selected["holders"]) <= doc["stdout"]["total"],
                    at + ": no more values than stdout bytes")
    observations = doc["observations"]
    require(observations["unavailable"] is None
            or (not observations["before"] and not observations["after"]),
            where + ".observations: none when unavailable")
    return doc


def _check_lifecycle(doc, at):
    """What each lifecycle status claims must be what its counts show (ADR-0167): a scan that
    did not count is a failed enumeration, never a clean or verified one."""
    lifecycle, kind = doc["lifecycle"], doc["termination"]["kind"]
    observation, status = lifecycle["observation"], lifecycle["status"]
    at_exit, remaining = lifecycle["descendants_at_exit"], lifecycle["remaining_after_cleanup"]
    require(observation == "pid_namespace_scan"
            or doc["supervisor"]["containment"] == "process_group",
            at + ".observation: a namespace is scanned as one")
    if kind == "launch_failed":
        require(status == "clean" and at_exit == 0 and remaining == 0
                and not lifecycle["pipes_held"],
                at + ": nothing was launched, so nothing remains")
        return
    require(status != "clean" or observation != "pipes_only",
            at + ".status: pipes alone never establish a clean lifecycle")
    require(at_exit is None or (kind in ("exited", "signalled")
                                and doc["timing"]["exit_observation"] == "unreaped"),
            at + ".descendants_at_exit: counted only at an exit observed unreaped")
    if observation == "pipes_only":
        require(at_exit is None and remaining is None,
                at + ": pipes alone count no process")
    if status in ("clean", "unexpected_descendants"):
        require(remaining == 0 and not lifecycle["pipes_held"],
                at + ".status: verified only when nothing remained and no pipe was held")
        require(kind not in ("exited", "signalled") or at_exit is not None,
                at + ".status: verified only when the descendants at the exit were counted")
    if status == "clean":
        require(not at_exit, at + ".status: clean only without live descendants at the exit")
    if status == "unexpected_descendants":
        require(bool(at_exit), at + ".status: unexpected descendants are counted ones")
    if status == "no_observed_descendants":
        require(observation == "pipes_only" and not lifecycle["pipes_held"],
                at + ".status: no observed descendants only from pipes that closed")
    if remaining or lifecycle["pipes_held"] or (observation != "pipes_only"
                                                 and remaining is None):
        require(status == "cleanup_uncertain",
                at + ".status: a remainder, a held pipe or a failed enumeration leaves the "
                "cleanup uncertain")
    require(status != "cleanup_uncertain" or lifecycle["detail"],
            at + ".detail: an uncertain cleanup says why")
    scratch = doc["scratch"]
    require(scratch is None or scratch["status"] != "cleared"
            or (status in ("clean", "unexpected_descendants")
                and observation == "pid_namespace_scan"),
            at + ": a scratch is cleared only after a verified-empty namespace")


# af.perf-capabilities/3 --------------------------------------------------------------------

CAPABILITIES_SCHEMA = "af.perf-capabilities/3"
TOOL = Obj(Field("identity", Nullable(Str(min_len=1, max_len=512))),
           Field("unavailable", Nullable(Str(min_len=1, max_len=1024))))
CAPABILITY = Obj(Field("status", Str(enum=("available", "unavailable"))),
                 Field("reasons", Arr(Str(min_len=1, max_len=1024), 0, 32)))
FILL_STOPS = ("refused", "error", "no_progress", "deadline", "limit_exceeded")
CAPACITY_ERRNOS = ("ENOSPC", "EDQUOT")
# The fill's evidence (ADR-0167): only the filesystem's own capacity refusal after progress,
# with the filesystem then reporting itself full and the fill file removed, verifies the bound.
FILL = Obj(Field("written_bytes", COUNT), Field("stopped", Str(enum=FILL_STOPS)),
           Field("errno", Nullable(Str(pattern=r"^(?:E[A-Z0-9]{1,15}|errno -?[0-9]{1,6})$",
                                       max_len=32))),
           Field("stage", Nullable(Str(enum=("open", "write", "fsync")))),
           Field("detail", Nullable(Str(min_len=1, max_len=512))),
           Field("available_bytes_at_stop", Nullable(COUNT)),
           Field("available_unknown", Nullable(Str(min_len=1, max_len=512))),
           Field("elapsed_ms", COUNT),
           Field("cleanup", Nullable(Str(min_len=1, max_len=1024))))
CAPABILITIES = Obj(
    Field("schema", Const(CAPABILITIES_SCHEMA)),
    Field("host", Obj(Field("platform", Str(min_len=1, max_len=32)),
                      Field("arch", Str(min_len=1, max_len=32)),
                      Field("kernel", Str(min_len=1, max_len=256)),
                      Field("python", Str(min_len=1, max_len=64)))),
    Field("tools", Map(ID_PATTERN, TOOL, 1, 16)),
    Field("trusted_local", Obj(Field("capability", CAPABILITY),
                               Field("scope", Const("declared_cases_without_linux_containment")),
                               Field("same_user_isolation", Const("not_provided")))),
    Field("linux_container", Obj(
        Field("capability", CAPABILITY),
        Field("scratch", Obj(Field("backend", Str(enum=("none", "dedicated_filesystem"))),
                             Field("path", Nullable(ABSOLUTE)),
                             Field("filesystem", Nullable(Str(min_len=1, max_len=64))),
                             Field("capacity_bytes", Nullable(COUNT)),
                             Field("enforcement", Str(enum=("verified", "not_verified",
                                                            "refused"))),
                             Field("fill", Nullable(FILL)),
                             Field("capability", CAPABILITY))))),
    Field("environments", Map(ID_PATTERN, CAPABILITY, 1, 16)),
)


def _check_fill(scratch, at):
    fill = scratch["fill"]
    require(scratch["enforcement"] != "verified" or fill is not None,
            at + ".fill: enforcement is verified only by a fill")
    if fill is None:
        return
    stopped = fill["stopped"]
    require((fill["errno"] is None) == (fill["detail"] is None)
            and (fill["errno"] is not None) == (stopped in ("refused", "error")),
            at + ".fill: an error and its errno exactly when the filesystem refused or failed")
    require((fill["stage"] is not None) == (stopped in ("refused", "error", "no_progress")),
            at + ".fill.stage: exactly for a stop at a call")
    require(stopped != "refused" or fill["errno"] in CAPACITY_ERRNOS,
            at + ".fill: a refusal is a capacity or quota error")
    require(fill["available_unknown"] is None or fill["available_bytes_at_stop"] is None,
            at + ".fill: free space observed, or why it was not")
    if scratch["enforcement"] == "verified":
        free = fill["available_bytes_at_stop"]
        require(stopped == "refused" and fill["written_bytes"] > 0 and free is not None
                and free < 2 * (1 << 20) and fill["cleanup"] is None
                and scratch["capacity_bytes"] is not None
                and fill["written_bytes"] <= scratch["capacity_bytes"],
                at + ".fill: verified only by a capacity refusal after progress, a full "
                "filesystem and a removed fill file")


def _check_capability(capability, at):
    require((capability["status"] == "available") == (not capability["reasons"]),
            at + ": unavailable exactly with its reasons")


def validate_capabilities(doc, where="capabilities"):
    CAPABILITIES.validate(doc, where)
    for name, tool in doc["tools"].items():
        require((tool["identity"] is None) != (tool["unavailable"] is None),
                where + ".tools.%s: an identity or the reason there is none" % name)
    _check_capability(doc["trusted_local"]["capability"], where + ".trusted_local")
    container = doc["linux_container"]
    _check_capability(container["capability"], where + ".linux_container")
    _check_capability(container["scratch"]["capability"], where + ".linux_container.scratch")
    require(container["scratch"]["capability"]["status"] == "unavailable"
            or container["scratch"]["enforcement"] == "verified",
            where + ".linux_container.scratch: available only with verified enforcement")
    _check_fill(container["scratch"], where + ".linux_container.scratch")
    require(container["capability"]["status"] == "unavailable"
            or container["scratch"]["capability"]["status"] == "available",
            where + ".linux_container: no container sample without a bounded scratch")
    for name, capability in doc["environments"].items():
        _check_capability(capability, where + ".environments." + name)
    return doc


# af.perf-report/3 ---------------------------------------------------------------------------
# Version 2 (ADR-0177) links validated diagnostic child runs and states each reproduction's
# limitations; version 3 (ADR-0214) adds each case's telemetry captures from the linked children
# that selected a telemetry level. Earlier versions are refused.

REPORT_SCHEMA = "af.perf-report/3"
TRIGGER_KINDS = ("pr", "main_push", "nightly", "release", "manual")
# Why a run does not satisfy the required check: only a passing, measured, full-scope run of an
# activated policy can.
REQUIRED_CHECK_REASONS = ("result_not_pass", "scope_not_full", "policy_not_active",
                          "evidence_not_measured")
DOWNLOAD_KINDS = ("plan", "build_manifest", "manifest", "shard", "samples", "log", "verdict")
# The most raw points one shard plans: every instance's measured and warmup pairs and both
# sentinel phases, two records a pair.
REPORT_POINTS_MAX = 2 * (limits.INSTANCES_MAX * (limits.PAIRS_MAX + limits.WARMUP_PAIRS_MAX)
                         + 2 * limits.SENTINEL_PAIRS_MAX)
REPORT_POINT = Obj(
    Field("sequence", COUNT), Field("phase", Str(enum=schedule.PHASES)), Field("instance_id", ID),
    Field("pair", Int(1, limits.PAIRS_MAX)), Field("position", Int(1, 2)),
    Field("side", Str(enum=SIDES)), Field("status", Str(enum=("ok", "failed", "missing"))),
    Field("value", Nullable(DECIMAL)), Field("failure", Nullable(Str(enum=FAILURE_KINDS))),
    Field("detail", Nullable(TEXT)))
# What a case's profile or trace is in the report: no linked child run covers it
# (`unavailable`), or the linked children's validated evidence, every item a verified copy below
# the report and every omission as the child recorded it.
DIAGNOSTIC_STATES = ("unavailable", "available", "partial", "omitted")
DIAGNOSTIC_ITEM_KINDS = ("baseline", "candidate", "difference_forward", "difference_reverse",
                         "trace", "perfetto", "timeline", "telemetry", "telemetry_perfetto")
DIAGNOSTIC_STATE = Obj(
    Field("state", Str(enum=DIAGNOSTIC_STATES)), Field("reason", TEXT),
    Field("items", Arr(Obj(Field("child_run_id", ID), Field("side", Nullable(Str(enum=SIDES))),
                           Field("kind", Str(enum=DIAGNOSTIC_ITEM_KINDS)),
                           Field("path", REL_PATH), Field("sha256", DIGEST),
                           Field("note", Nullable(TEXT))), 0, 64)),
    Field("omissions", Arr(Obj(Field("child_run_id", ID),
                               Field("side", Nullable(Str(enum=SIDES))), Field("reason", ID),
                               Field("detail", TEXT)), 0, 64)))
REPORT_CHILD = Obj(
    Field("run_id", ID), Field("plan_digest", DIGEST),
    Field("recipe_id", ID), Field("kind", Str(enum=diagcontracts.RECIPE_KINDS)),
    Field("mode", Str(enum=diagcontracts.MODES)),
    # The child bundle's manifest identity and its profiles.json, as verified.
    Field("bundle_sha256", DIGEST), Field("profiles_sha256", DIGEST),
    Field("cases", Arr(ID, 0, limits.DIAGNOSTIC_CASES_MAX, unique=True)),
    Field("retained_scratch", COUNT),
    # The telemetry level the child selected explicitly, or null (ADR-0214).
    Field("telemetry_level", Nullable(Str(enum=diagcontracts.TELEMETRY_LEVELS))),
    # The verified copy of the child's own page and bundle below the report.
    Field("page", REL_PATH), Field("directory", REL_PATH))
BINARY_IDENTITY = Obj(Field("af", Nullable(DIGEST)), Field("probe", Nullable(DIGEST)),
                      Field("adapter", Nullable(ID)))
REPORT_IDENTITY = Obj(
    Field("trigger", Str(enum=TRIGGER_KINDS)), Field("tier", Str(enum=TIERS)),
    Field("scope", Str(enum=SCOPES)), Field("seed", Int(0, 2 ** 63 - 1)),
    Field("parent_run_id", Nullable(ID)),
    Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
    Field("control_sha", GIT_SHA), Field("authority_sha", GIT_SHA),
    Field("authority_mode", Str(enum=AUTHORITY_MODES)),
    Field("activation", Nullable(Obj(
        Field("sha256", DIGEST), Field("admission", Str(enum=("calibrated",
                                                                "equivalent_adapter"))),
        Field("readiness_sha256", DIGEST), Field("cohort_id", ID)))),
    Field("suite_sha256", DIGEST), Field("policy_digest", DIGEST),
    Field("instrument_digest", DIGEST), Field("fixture_digest", DIGEST),
    Field("harness_changed", Bool()),
    Field("environment_id", ID), Field("environment_digest", DIGEST),
    Field("recipe_id", ID), Field("recipe_digest", DIGEST),
    Field("build_isolation", Str(enum=BUILD_ISOLATIONS)),
    Field("build_image", Nullable(images.REFERENCE)),
    Field("health_rule", Const(stats.HEALTH_RULE)),
    Field("health_state", Str(enum=("provisional", "calibrated"))),
    # The validated build manifest's binary identities, or null when it was unusable.
    Field("build", Nullable(Obj(
        Field("provenance", Str(enum=("measured", "synthetic"))),
        Field("supervisor_sha256", Nullable(DIGEST)), Field("toolchain", Str(max_len=256)),
        Field("isolation", Str(enum=("host_process", "container"))),
        Field("baseline", BINARY_IDENTITY), Field("candidate", BINARY_IDENTITY)))))
REPORT_SHARD = Obj(
    Field("shard", Int(0, limits.SHARDS_MAX - 1)),
    # The shard's terminal record when its bundle verified and named this run, else null.
    Field("terminal", Nullable(Obj(
        Field("status", Str(enum=SHARD_STATUSES)), Field("failure", Nullable(TERMINAL_FAILURE)),
        Field("host_id", DIGEST), Field("host", HOST),
        Field("mode", Str(enum=("trusted_local", "linux_container", "synthetic"))),
        Field("containment", Str(enum=CONTAINMENTS)),
        Field("sample_image", Nullable(SHORT_TEXT)), Field("seeds", Arr(SEED, 0, 16)),
        Field("observations", Obj(Field("start", LOAD), Field("end", LOAD))),
        Field("started_unix_ms", Int(0, 2 ** 53)), Field("ended_unix_ms", Int(0, 2 ** 53))))))
REPORT = Obj(
    Field("schema", Const(REPORT_SCHEMA)), Field("run_id", ID), Field("plan_digest", DIGEST),
    # What kind of comparison the intervals are: each pair on one host (never a history trend).
    Field("comparison", Const("same_host_paired")),
    Field("required_check", Obj(Field("satisfied", Bool()),
                                Field("reasons", Arr(Str(enum=REQUIRED_CHECK_REASONS), 0, 4,
                                                     unique=True)))),
    Field("identity", REPORT_IDENTITY),
    Field("sensitivity", Obj(Field("state", Str(enum=("not_calibrated",
                                                      "calibrated_admission"))),
                             Field("detail", TEXT))),
    Field("cases", Arr(Obj(
        Field("instance_id", ID), Field("case_id", ID),
        Field("failures", Obj(Field("baseline", COUNT), Field("candidate", COUNT))),
        Field("denominator", Obj(Field("planned_records", COUNT),
                                 Field("missing_records", COUNT),
                                 Field("measured_pairs", Int(limits.PAIRS_MIN, limits.PAIRS_MAX)),
                                 Field("warmup_pairs", Int(0, limits.WARMUP_PAIRS_MAX)))),
        Field("profile", DIAGNOSTIC_STATE), Field("trace", DIAGNOSTIC_STATE),
        Field("telemetry", DIAGNOSTIC_STATE)),
                       1, limits.INSTANCES_MAX)),
    Field("diagnostics", Arr(REPORT_CHILD, 0, 16)),
    Field("sequences", Arr(Obj(Field("shard", Int(0, limits.SHARDS_MAX - 1)),
                               Field("points", Arr(REPORT_POINT, 0, REPORT_POINTS_MAX))),
                           1, limits.SHARDS_MAX)),
    Field("shards", Arr(REPORT_SHARD, 1, limits.SHARDS_MAX)),
    Field("downloads", Arr(Obj(Field("path", REL_PATH), Field("sha256", DIGEST),
                               Field("size", COUNT), Field("kind", Str(enum=DOWNLOAD_KINDS))),
                           0, 4096)),
    # Each command reproduces the run's frozen configuration; what it cannot carry is a
    # documented placeholder named in its limitations (ADR-0177). A partial run's rerun names
    # every one of its instances.
    Field("reproduction", Arr(Obj(Field("purpose", Str(enum=("recompute", "report", "rerun"))),
                                  Field("argv", Arr(Str(min_len=1, max_len=512), 1, 256)),
                                  Field("limitations", Arr(TEXT, 0, 8))),
                              0, 8)),
    Field("verdict", VERDICT),
)


def validate_report(doc, where="report.json"):
    REPORT.validate(doc, where)
    verdict = validate_verdict(doc["verdict"], where + ".verdict")
    require((doc["run_id"], doc["plan_digest"]) == (verdict["run_id"], verdict["plan_digest"]),
            where + ": reports the run and plan its verdict names")
    check = doc["required_check"]
    require(check["satisfied"] == (not check["reasons"])
            == verdict["aggregate"]["satisfies_required_check"],
            where + ".required_check: satisfied exactly when the verdict satisfies it, and "
            "otherwise with its reasons")
    require(("result_not_pass" in check["reasons"]) == (verdict["aggregate"]["result"]
                                                        != "pass")
            and ("evidence_not_measured" in check["reasons"]) == (verdict["provenance"]
                                                                  != "measured")
            and ("scope_not_full" in check["reasons"]) == (doc["identity"]["scope"] != "full"),
            where + ".required_check.reasons: the verdict's own result, evidence and scope")
    require(doc["identity"]["scope"] == verdict["scope"],
            where + ".identity.scope: the verdict's scope")
    require([item["instance_id"] for item in doc["cases"]]
            == [entry["instance_id"] for entry in verdict["cases"]],
            where + ".cases: one entry per verdict case, in its order")
    for item, entry in zip(doc["cases"], verdict["cases"]):
        require(item["denominator"]["missing_records"] == entry["missing_records"]
                and item["denominator"]["measured_pairs"] == entry["pairs"]
                and sum(item["failures"].values()) == len(entry["failures"]),
                where + ".cases: the verdict's denominator and failures")
    shards = [entry["shard"] for entry in verdict["shards"]]
    require([item["shard"] for item in doc["sequences"]] == shards
            and [item["shard"] for item in doc["shards"]] == shards,
            where + ": one sequence and one shard entry per planned shard, in order")
    for item in doc["sequences"]:
        for index, point in enumerate(item["points"]):
            at = "%s.sequences[%d].points[%d]" % (where, item["shard"], index)
            require(point["sequence"] == index, at + ": points follow the planned order")
            status = point["status"]
            require((point["value"] is not None) == (status == "ok")
                    and (point["failure"] is not None) == (status == "failed")
                    and (point["detail"] is None or status == "failed"),
                    at + ": a value exactly when ok, a failure exactly when failed")
    paths = [item["path"] for item in doc["downloads"]]
    require(paths == sorted(set(paths)), where + ".downloads: sorted, each path once")
    children = [child["run_id"] for child in doc["diagnostics"]]
    require(len(children) == len(set(children)), where + ".diagnostics: each child run once")
    covering = {}
    for child in doc["diagnostics"]:
        require(child["run_id"] != doc["run_id"], where + ".diagnostics: a child is a separate "
                "run")
        require(child["telemetry_level"] is None or child["kind"] in ("cpu_profile", "trace"),
                where + ".diagnostics: telemetry is captured beside CPU profiles or traces")
        for case in child["cases"]:
            covering.setdefault((case, child["kind"]), set()).add(child["run_id"])
            if child["telemetry_level"] is not None:
                covering.setdefault((case, "telemetry"), set()).add(child["run_id"])
    for item in doc["cases"]:
        for field, kinds in (("profile", ("cpu_profile",)), ("trace", ("trace",)),
                             ("telemetry", ("telemetry",))):
            state = item[field]
            at = "%s.cases[%s].%s" % (where, item["instance_id"], field)
            linked = set()
            for kind in kinds:
                linked |= covering.get((item["instance_id"], kind), set())
            named = set(entry["child_run_id"] for entry in state["items"] + state["omissions"])
            require(named <= linked, at + ": names only linked child runs that selected it")
            require((state["state"] == "unavailable") == (not linked),
                    at + ": unavailable exactly when no linked child run covers the case")
            require(state["state"] != "available" or (state["items"]
                                                      and not state["omissions"]),
                    at + ": available with its items and no omission")
            require(state["state"] != "omitted" or not state["items"],
                    at + ": omitted with no item")
    for entry in doc["reproduction"]:
        require(entry["purpose"] == "rerun" or not entry["limitations"],
                where + ".reproduction: only a rerun has limitations")
    return doc


# The archive (ADR-0172) --------------------------------------------------------------------

ARCHIVE_SCHEMA = "af.perf-archive/1"
ARCHIVE_EVENT_SCHEMA = "af.perf-archive-event/1"
ARCHIVE_COLLECTION_SCHEMA = "af.perf-archive-collection/1"
ARCHIVE_STATUS_SCHEMA = "af.perf-archive-status/1"
TRANSPORT_SCHEMA = "af.perf-transport/1"
# What each archived member is, which decides how long it is kept: numeric evidence, plans,
# build manifests, verdicts and summaries at least 365 days; logs, transported views, profiles
# and their symbols 90 days by default.
ARCHIVE_CLASSES = ("plan", "build_manifest", "manifest", "shard", "samples", "verdict",
                   "summary", "log", "transported_view", "profile", "symbol")
ACTIONS_ID = Int(1, 2 ** 53)
ARCHIVE_MEMBER = Obj(Field("path", REL_PATH), Field("sha256", DIGEST), Field("size", COUNT),
                     Field("class", Str(enum=ARCHIVE_CLASSES)))
# What became of each transport artifact a collection listed: downloaded, expired (as listed
# or as its download answered), refused by the transport format, or never collected because the
# collection ended first.
ARTIFACT_STATES = ("downloaded", "expired", "refused", "not_collected")
TRANSPORT_ARTIFACT = Obj(Field("id", ACTIONS_ID), Field("name", Str(min_len=1, max_len=256)),
                         Field("size", COUNT), Field("created_unix_ms", Nullable(UNIX_MS)),
                         Field("expires_unix_ms", Nullable(UNIX_MS)),
                         Field("state", Str(enum=ARTIFACT_STATES)),
                         Field("sha256", Nullable(DIGEST)))
ARCHIVE_SOURCE = Tagged("kind", {
    "local": tagged_variant("kind", "local"),
    "actions": tagged_variant("kind", "actions", Field("repository", REPOSITORY),
                              Field("actions_run_id", ACTIONS_ID),
                              Field("artifacts", Arr(TRANSPORT_ARTIFACT, 0, 1000))),
})
ARCHIVE = Obj(Field("schema", Const(ARCHIVE_SCHEMA)),
              Field("capacity_bytes", Int(1, limits.ARCHIVE_BYTES_MAX)),
              Field("created_unix_ms", UNIX_MS))
ARCHIVE_EVENT = Obj(
    Field("schema", Const(ARCHIVE_EVENT_SCHEMA)), Field("sequence", Int(1, 10 ** 12 - 1)),
    Field("unix_ms", UNIX_MS),
    Field("event", Tagged("kind", {
        # A run admitted whole: its plan and verdict identity, the bundles it was verified as,
        # when its evidence ended, where it came from and every member's object.
        "run": tagged_variant(
            "kind", "run", Field("run_id", ID), Field("plan_digest", DIGEST),
            Field("source", ARCHIVE_SOURCE),
            Field("verdict", Obj(Field("result", Str(enum=("pass", "fail", "incomplete"))),
                                 Field("exit_code", Int(0, 4)),
                                 Field("scope", Str(enum=SCOPES)),
                                 Field("provenance", Str(enum=("measured", "synthetic", "mixed",
                                                               "none"))),
                                 Field("satisfies_required_check", Bool()))),
            Field("bundles", Obj(Field("run", DIGEST),
                                 Field("shards", Arr(Obj(
                                     Field("shard", Int(0, limits.SHARDS_MAX - 1)),
                                     Field("bundle", Nullable(DIGEST))), 1, limits.SHARDS_MAX)))),
            Field("run_ended_unix_ms", Nullable(UNIX_MS)),
            Field("members", Arr(ARCHIVE_MEMBER, 1, 65536))),
        # Members whose retention ended: the run's original manifest, what expired and when it
        # was due, and what remains available, so no compact view claims more than it holds.
        "retention": tagged_variant(
            "kind", "retention", Field("run_id", ID), Field("run_sequence", Int(1, 10 ** 12 - 1)),
            Field("manifest", Arr(ARCHIVE_MEMBER, 1, 65536)),
            Field("expired", Arr(Obj(Field("path", REL_PATH), Field("sha256", DIGEST),
                                     Field("class", Str(enum=ARCHIVE_CLASSES)),
                                     Field("due_unix_ms", UNIX_MS)), 1, 65536)),
            Field("available", Arr(REL_PATH, 0, 65536)),
            Field("numeric", Str(enum=("complete", "expired"))),
            Field("profile_reproduction", Str(enum=("not_applicable", "available",
                                                    "expired")))),
        # A run pinned as an active calibration cohort member or a release-budget
        # demonstration: kept whole until superseded, and then 365 days more.
        "pin": tagged_variant("kind", "pin", Field("run_id", ID),
                              Field("reason", Str(enum=("active_cohort", "release_budget"))),
                              Field("note", TEXT)),
        "supersede": tagged_variant("kind", "supersede", Field("run_id", ID),
                                    Field("superseded_by", Nullable(ID))),
        # Objects no available member references any more, removed by reference tracing.
        "gc": tagged_variant("kind", "gc",
                             Field("removed", Arr(Obj(Field("sha256", DIGEST),
                                                      Field("size", COUNT)), 1, 1 << 20)),
                             Field("freed_bytes", COUNT)),
    })),
)


def validate_archive(doc, where="archive.json"):
    ARCHIVE.validate(doc, where)
    return doc


def validate_archive_event(doc, where="archive event"):
    ARCHIVE_EVENT.validate(doc, where)
    event = doc["event"]
    kind = event["kind"]
    if kind == "run":
        paths = [member["path"] for member in event["members"]]
        require(paths == sorted(set(paths)), where + ".members: sorted, each path once")
        shards = [item["shard"] for item in event["bundles"]["shards"]]
        require(shards == list(range(len(shards))), where + ".bundles.shards: every planned "
                "shard in order")
    elif kind == "retention":
        paths = [member["path"] for member in event["manifest"]]
        require(paths == sorted(set(paths)), where + ".manifest: sorted, each path once")
        expired = [item["path"] for item in event["expired"]]
        require(expired == sorted(set(expired)) and set(expired) <= set(paths)
                and not set(expired) & set(event["available"])
                and set(event["available"]) <= set(paths)
                and event["available"] == sorted(set(event["available"])),
                where + ": expired and available members are disjoint parts of its manifest")
    elif kind == "gc":
        require(event["freed_bytes"] == sum(item["size"] for item in event["removed"]),
                where + ".freed_bytes: the removed objects' sizes")
    return doc


ARCHIVE_COLLECTION = Obj(
    Field("schema", Const(ARCHIVE_COLLECTION_SCHEMA)), Field("sequence", Int(1, 10 ** 12 - 1)),
    Field("started_unix_ms", UNIX_MS), Field("ended_unix_ms", UNIX_MS),
    Field("source", ARCHIVE_SOURCE),
    Field("status", Str(enum=("imported", "already_archived", "refused", "capacity_exhausted",
                              "transport_failed"))),
    Field("run_id", Nullable(ID)), Field("run_ended_unix_ms", Nullable(UNIX_MS)),
    Field("needed_bytes", Nullable(COUNT)), Field("available_bytes", Nullable(COUNT)),
    Field("detail", TEXT),
)


def validate_archive_collection(doc, where="archive collection"):
    ARCHIVE_COLLECTION.validate(doc, where)
    require(doc["ended_unix_ms"] >= doc["started_unix_ms"], where + ": ends before it starts")
    require((doc["status"] in ("imported", "already_archived")) <= (doc["run_id"] is not None),
            where + ".run_id: a collected run names its run")
    require((doc["status"] == "capacity_exhausted") <= (doc["needed_bytes"] is not None
                                                        and doc["available_bytes"] is not None),
            where + ": capacity exhaustion states what was needed and what was available")
    return doc


HEALTH_PROBLEMS = ("capacity_exhausted", "capacity_low", "collection_failed", "no_collection",
                   "transport_expired", "transport_expiring")
ARCHIVE_STATUS = Obj(
    Field("schema", Const(ARCHIVE_STATUS_SCHEMA)), Field("now_unix_ms", UNIX_MS),
    Field("capacity_bytes", Int(1, limits.ARCHIVE_BYTES_MAX)), Field("used_bytes", COUNT),
    Field("available_bytes", COUNT), Field("filesystem_free_bytes", COUNT),
    Field("runs", COUNT), Field("pinned", COUNT), Field("objects", COUNT),
    Field("last_collection", Nullable(Obj(Field("sequence", Int(1, 10 ** 12 - 1)),
                                          Field("status", Str(enum=(
                                              "imported", "already_archived", "refused",
                                              "capacity_exhausted", "transport_failed"))),
                                          Field("ended_unix_ms", UNIX_MS),
                                          Field("detail", TEXT)))),
    Field("last_successful_collection_unix_ms", Nullable(UNIX_MS)),
    # How long after a run's evidence ended it was collected: the last and the largest lag.
    Field("collection_lag_ms", Obj(Field("last", Nullable(COUNT)), Field("max", Nullable(COUNT)))),
    # Every transport artifact a collection saw that it did not download, with its expiry.
    Field("transport", Arr(Obj(Field("actions_run_id", ACTIONS_ID),
                               Field("artifact_id", ACTIONS_ID),
                               Field("name", Str(min_len=1, max_len=256)),
                               Field("expires_unix_ms", Nullable(UNIX_MS)),
                               Field("state", Str(enum=ARTIFACT_STATES)),
                               Field("risk", Str(enum=("expiring", "expired", "unknown")))),
                           0, 4096)),
    Field("health", Obj(Field("status", Str(enum=("ok", "failed"))),
                        Field("problems", Arr(Obj(Field("code", Str(enum=HEALTH_PROBLEMS)),
                                                  Field("detail", TEXT)), 0, 64)))),
)


def validate_archive_status(doc, where="archive status"):
    ARCHIVE_STATUS.validate(doc, where)
    require(doc["available_bytes"] == max(0, doc["capacity_bytes"] - doc["used_bytes"]),
            where + ".available_bytes: the capacity less what is used, never below zero")
    require((doc["health"]["status"] == "ok") == (not doc["health"]["problems"]),
            where + ".health: failed exactly with its problems")
    return doc


TRANSPORT = Obj(Field("schema", Const(TRANSPORT_SCHEMA)), Field("run_id", ID),
                Field("content", Str(enum=("run_bundle", "shard_bundle"))),
                Field("shard", Nullable(Int(0, limits.SHARDS_MAX - 1))),
                Field("views", Arr(REL_PATH, 0, 64, unique=True)))


def validate_transport(doc, where="af-perf-transport.json"):
    TRANSPORT.validate(doc, where)
    require((doc["content"] == "shard_bundle") == (doc["shard"] is not None),
            where + ".shard: named exactly by a shard bundle")
    return doc


# af.perf-history/1 --------------------------------------------------------------------------

HISTORY_SCHEMA = "af.perf-history/1"
# What a compatible group's runs share; a run's ID, sources and seed artifacts never split one.
HISTORY_COMPONENTS = ("workload", "fixture", "policy", "recipe", "environment", "provenance")
HISTORY_IDENTITY = Obj(
    Field("case_id", ID), Field("instance_id", ID),
    Field("parameters", Map(ID_PATTERN, PARAM_VALUE, 0, 4)),
    Field("workload", DIGEST), Field("fixture", DIGEST), Field("policy", DIGEST),
    Field("recipe", SHORT_TEXT), Field("environment", SHORT_TEXT),
    Field("provenance", Str(enum=("measured", "synthetic", "mixed", "none"))))
HISTORY_POINT = Obj(
    Field("run_id", ID), Field("started_unix_ms", Nullable(UNIX_MS)),
    Field("status", Str(enum=CASE_STATUSES)),
    Field("outcome", Str(enum=("pass", "fail", "incomplete"))),
    Field("classification", Nullable(Str(enum=(
        stats.REGRESSION, stats.IMPROVEMENT, stats.WITHIN_TOLERANCE, stats.NON_REGRESSION,
        stats.INCONCLUSIVE)))),
    Field("interval", Nullable(Obj(Field("lower", RATIONAL), Field("upper", RATIONAL)))),
    Field("baseline_median", Nullable(RATIONAL)), Field("candidate_median", Nullable(RATIONAL)),
    Field("change", Nullable(RATIONAL)), Field("missing_records", COUNT),
    Field("failures", Obj(Field("baseline", COUNT), Field("candidate", COUNT))),
    Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
    # The seed artifacts the case's samples copied, by side: provenance only.
    Field("seeds", Arr(Obj(Field("side", Str(enum=SIDES)), Field("seed", ID),
                           Field("digest", DIGEST)), 0, 16)))
HISTORY_RUN = Obj(
    Field("run_id", ID), Field("plan_digest", DIGEST), Field("input", SHORT_TEXT),
    Field("source", Str(enum=("run_dir", "archive", "archive_summary"))),
    Field("scope", Str(enum=SCOPES)), Field("tier", Str(enum=TIERS)),
    Field("trigger", Str(enum=TRIGGER_KINDS)),
    Field("provenance", Str(enum=("measured", "synthetic", "mixed", "none"))),
    Field("result", Str(enum=("pass", "fail", "incomplete"))),
    Field("satisfies_required_check", Bool()),
    Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
    Field("started_unix_ms", Nullable(UNIX_MS)), Field("report", REL_PATH),
    # An archived run's retention: what expired, and whether its numeric evidence and its
    # profiles can still be reproduced.
    Field("retention", Nullable(Obj(
        Field("numeric", Str(enum=("complete", "expired"))),
        Field("profile_reproduction", Str(enum=("not_applicable", "available", "expired"))),
        Field("expired", Arr(Str(enum=ARCHIVE_CLASSES), 0, len(ARCHIVE_CLASSES),
                             unique=True))))),
    Field("views", Arr(Obj(Field("name", REL_PATH), Field("path", REL_PATH),
                           Field("sha256", DIGEST), Field("size", COUNT)), 0, 64)))
HISTORY = Obj(
    Field("schema", Const(HISTORY_SCHEMA)),
    # What kind of comparison a trend is: separate runs, never a same-host paired result.
    Field("comparison", Const("trend_across_runs")),
    Field("runs", Arr(HISTORY_RUN, 0, 100000)),
    Field("groups", Arr(Obj(Field("group_id", DIGEST), Field("identity", HISTORY_IDENTITY),
                            Field("points", Arr(HISTORY_POINT, 1, 100000)),
                            Field("related", Arr(Obj(
                                Field("group_id", DIGEST),
                                Field("differs", Arr(Str(enum=HISTORY_COMPONENTS), 1,
                                                     len(HISTORY_COMPONENTS), unique=True))),
                                0, 1000))), 0, 100000)),
    Field("unavailable", Arr(Obj(Field("input", SHORT_TEXT), Field("run_id", Nullable(ID)),
                                 Field("reason", TEXT)), 0, 100000)),
)


def history_group_id(identity):
    """A compatible group's ID: the digest of its case and every identity component."""
    return canonical_digest(dict((key, identity[key]) for key in
                                 ("case_id", "instance_id", "parameters")
                                 + HISTORY_COMPONENTS))


def validate_history(doc, where="history.json"):
    HISTORY.validate(doc, where)
    runs = dict((run["run_id"], run) for run in doc["runs"])
    require(len(runs) == len(doc["runs"]), where + ".runs: each run once")
    for run in doc["runs"]:
        require(run["report"] == "runs/%s/report.html" % run["run_id"],
                where + ".runs: each run's report is its own page below runs/")
        require(run["satisfies_required_check"] <= (run["result"] == "pass"
                                                    and run["provenance"] == "measured"
                                                    and run["scope"] == "full"),
                where + ".runs: only a passing measured full-scope run satisfies the check")
    groups = dict((group["group_id"], group) for group in doc["groups"])
    require(len(groups) == len(doc["groups"]), where + ".groups: each group once")
    for group in doc["groups"]:
        at = "%s.groups[%s]" % (where, group["group_id"])
        require(group["group_id"] == history_group_id(group["identity"]),
                at + ".group_id: the digest of its identity")
        seen = [point["run_id"] for point in group["points"]]
        require(len(seen) == len(set(seen)) and set(seen) <= set(runs),
                at + ".points: each listed run at most once")
        for related in group["related"]:
            other = groups.get(related["group_id"])
            require(other is not None and other is not group
                    and other["identity"]["instance_id"] == group["identity"]["instance_id"]
                    and related["differs"] == [
                        component for component in HISTORY_COMPONENTS
                        if other["identity"][component] != group["identity"][component]],
                    at + ".related: another group of its case, by what differs")
    return doc


# af.perf-plan-refusal/1 --------------------------------------------------------------------

REFUSAL_SCHEMA = "af.perf-plan-refusal/1"
REFUSAL = Obj(
    Field("schema", Const(REFUSAL_SCHEMA)),
    Field("status", Str(enum=("invalid", "unavailable"))),
    Field("reason", ID), Field("detail", TEXT),
)


def validate_refusal(doc, where="refusal"):
    return REFUSAL.validate(doc, where)


# Registry of documents ---------------------------------------------------------------------

# Continuous scheduling (ADR-0177) ------------------------------------------------------------

SCHEDULE_SCHEMA = "af.perf-schedule/2"
BUILD_PACK_SCHEMA = "af.perf-build-pack/1"
ACTIONS_EVENTS = ("pull_request", "push", "schedule", "workflow_dispatch", "workflow_run")
# The events whose workflow runs reviewed code of main: a push, the schedule, a dispatch and a
# completed upstream Release run's handoff (ADR-0180).
TRUSTED_EVENTS = ("push", "schedule", "workflow_dispatch", "workflow_run")
# What each scheduled comparison is for: the event's own comparison; the advisory comparison
# under the control revision's registry before any activation exists; and a policy change's two
# proposed-policy bootstrap comparisons, (base, merge) and (merge, merge).
COMPARISON_ROLES = ("primary", "control_bootstrap", "proposed_bootstrap_base",
                    "proposed_bootstrap_merge")
COMPARISON_STATUSES = ("planned", "unavailable", "refused")
EXECUTORS = ("ephemeral_hosted", "dedicated")

# Release handoff (ADR-0180) ----------------------------------------------------------------

RELEASE_CLAIM_SCHEMA = "af.perf-release-claim/1"
RELEASE_HANDOFF_SCHEMA = "af.perf-release-handoff/1"
RELEASE_WORKFLOW_PATH = ".github/workflows/release.yml"
RELEASE_HANDOFF_WORKFLOW_PATH = ".github/workflows/performance-release.yml"
RELEASE_IDENTITY_DOMAIN = "af.perf-release-identity/1"
RELEASE_MODES = ("release", "remeasure")
RELEASE_STATUSES = ("claimed", "measure", "linked", "incomplete", "not_eligible", "refused")
RELEASE_HISTORY_STATES = ("not_read", "complete", "unavailable", "expired")
RELEASE_ATTEMPTS_MAX = 20
RELEASE_CLAIMS_MAX = 20
RELEASE_RESULTS_MAX = 16
RELEASE_TAG = Str(pattern=r"^v(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})"
                  r"(-rc\.(0|[1-9][0-9]{0,8}))?$", max_len=64)
RELEASE_CLAIM_ARTIFACT = Str(pattern=r"^af-release-(claim-[0-9a-f]{64}|remeasure-[0-9a-f]{64}"
                                     r"-[1-9][0-9]{0,15})$", max_len=128)
LAB_RUN_PREFIX = Str(pattern=r"^gh[1-9][0-9]{0,15}_a[1-9][0-9]{0,3}_$", max_len=32)
ACTIONS_ATTEMPT = Int(1, 1000)
# The immutable deduplication identity: the upstream run, never its attempt, its tag and SHA.
RELEASE_IDENTITY = Obj(Field("repository", REPOSITORY), Field("upstream_run_id", ACTIONS_ID),
                       Field("tag", RELEASE_TAG), Field("sha", GIT_SHA))


def release_identity_key(identity):
    """The claim key of a release identity: the SHA-256 of its domain-separated canonical
    serialization."""
    import hashlib
    data = canonical_bytes({"domain": RELEASE_IDENTITY_DOMAIN, "identity": identity})
    return "sha256:" + hashlib.sha256(data).hexdigest()


def release_claim_artifact(key, kind="release", actions_run_id=None):
    """The artifact name of a release claim, or of one explicit remeasurement's own claim; never
    an `af-perf-*` transport name the archive's collector reads."""
    require(isinstance(key, str) and re.match(r"^sha256:[0-9a-f]{64}$", key) is not None,
            "a claim key is sha256:<64 hex>")
    if kind == "release":
        return "af-release-claim-" + key[7:]
    require(type(actions_run_id) is int and actions_run_id > 0,
            "a remeasurement's claim names its Actions run")
    return "af-release-remeasure-%s-%d" % (key[7:], actions_run_id)


RELEASE_CLAIM = Obj(
    Field("schema", Const(RELEASE_CLAIM_SCHEMA)), Field("kind", Str(enum=RELEASE_MODES)),
    Field("key", DIGEST), Field("identity", RELEASE_IDENTITY),
    Field("upstream", Obj(Field("workflow_id", ACTIONS_ID),
                          Field("workflow_path", Const(RELEASE_WORKFLOW_PATH)),
                          Field("run_attempt", ACTIONS_ATTEMPT),
                          Field("published_in_attempt", ACTIONS_ATTEMPT))),
    Field("release", Obj(Field("id", ACTIONS_ID), Field("prerelease", Bool()),
                         Field("published_at", Str(min_len=1, max_len=64)))),
    Field("claimant", Obj(Field("repository", REPOSITORY),
                          Field("workflow_path", Const(RELEASE_HANDOFF_WORKFLOW_PATH)),
                          Field("actions_run_id", ACTIONS_ID),
                          Field("actions_run_attempt", ACTIONS_ATTEMPT),
                          Field("lab_run_prefix", LAB_RUN_PREFIX))),
    # An explicit remeasurement names the parent claim it repeats, which it never replaces.
    Field("remeasure_of", Nullable(Obj(Field("key", DIGEST),
                                       Field("claim_artifact_id", ACTIONS_ID),
                                       Field("actions_run_id", ACTIONS_ID),
                                       Field("lab_run_id", ID)))),
    Field("claimed_unix_ms", UNIX_MS))


def validate_release_claim(doc, where="claim.json"):
    RELEASE_CLAIM.validate(doc, where)
    require(doc["key"] == release_identity_key(doc["identity"]),
            where + ".key: the SHA-256 of the identity's canonical serialization")
    claimant = doc["claimant"]
    require(claimant["lab_run_prefix"] == "gh%d_a%d_" % (claimant["actions_run_id"],
                                                       claimant["actions_run_attempt"]),
            where + ".claimant.lab_run_prefix: the claiming run's own laboratory runs")
    require(doc["identity"]["repository"] == claimant["repository"],
            where + ".claimant: a claim of this repository's release")
    require(doc["upstream"]["published_in_attempt"] <= max(doc["upstream"]["run_attempt"],
                                                           RELEASE_ATTEMPTS_MAX),
            where + ".upstream.published_in_attempt: an attempt that was read")
    parent = doc["remeasure_of"]
    require((doc["kind"] == "remeasure") == (parent is not None),
            where + ".remeasure_of: exactly a remeasurement names its parent claim")
    require(parent is None or (parent["key"] == doc["key"]
                               and parent["actions_run_id"] != claimant["actions_run_id"]),
            where + ".remeasure_of: a parent claim of the same identity, by another run")
    return doc


RELEASE_HISTORY_CLAIM = Obj(
    Field("artifact_id", ACTIONS_ID), Field("actions_run_id", ACTIONS_ID),
    Field("created_unix_ms", UNIX_MS), Field("expired", Bool()), Field("valid", Bool()),
    Field("reason", Nullable(ID)))
RELEASE_HANDOFF = Obj(
    Field("schema", Const(RELEASE_HANDOFF_SCHEMA)),
    Field("stage", Str(enum=("handoff", "confirm"))), Field("mode", Str(enum=RELEASE_MODES)),
    Field("actions", Obj(Field("repository", REPOSITORY), Field("run_id", ACTIONS_ID),
                         Field("run_attempt", ACTIONS_ATTEMPT),
                         Field("event", Str(enum=("workflow_run", "workflow_dispatch"))),
                         Field("sha", GIT_SHA))),
    Field("upstream", Nullable(Obj(
        Field("run_id", ACTIONS_ID), Field("run_attempt", ACTIONS_ATTEMPT),
        Field("latest_attempt", ACTIONS_ATTEMPT), Field("workflow_id", ACTIONS_ID),
        Field("workflow_path", Const(RELEASE_WORKFLOW_PATH)), Field("event", Const("push")),
        Field("head_branch", SHORT_TEXT), Field("head_sha", GIT_SHA),
        Field("conclusion", Nullable(Str(min_len=1, max_len=64))),
        Field("created_unix_ms", UNIX_MS)))),
    # Every attempt read, with its publish and pin jobs' outcomes; a later failed pin never
    # hides an earlier successful publication.
    Field("publication", Nullable(Obj(
        Field("attempts", Arr(Obj(Field("attempt", ACTIONS_ATTEMPT), Field("jobs", COUNT),
                                  Field("publish", Nullable(Str(min_len=1, max_len=64))),
                                  Field("pin", Nullable(Str(min_len=1, max_len=64)))),
                              1, RELEASE_ATTEMPTS_MAX)),
        Field("published_in_attempt", ACTIONS_ATTEMPT)))),
    Field("release", Nullable(Obj(Field("tag", RELEASE_TAG), Field("sha", GIT_SHA),
                                  Field("id", ACTIONS_ID), Field("prerelease", Bool()),
                                  Field("published_at", Str(min_len=1, max_len=64))))),
    Field("identity", Nullable(RELEASE_IDENTITY)), Field("key", Nullable(DIGEST)),
    Field("history", Obj(Field("state", Str(enum=RELEASE_HISTORY_STATES)),
                         Field("detail", TEXT),
                         Field("claims", Arr(RELEASE_HISTORY_CLAIM, 0, RELEASE_CLAIMS_MAX)))),
    Field("status", Str(enum=RELEASE_STATUSES)), Field("reason", Nullable(ID)),
    Field("detail", TEXT), Field("claim_artifact", Nullable(RELEASE_CLAIM_ARTIFACT)),
    Field("first_claim", Nullable(Obj(Field("artifact_id", ACTIONS_ID),
                                      Field("actions_run_id", ACTIONS_ID),
                                      Field("actions_run_attempt", ACTIONS_ATTEMPT),
                                      Field("lab_run_prefix", LAB_RUN_PREFIX)))),
    Field("results", Arr(Obj(Field("artifact_id", ACTIONS_ID),
                             Field("name", Str(min_len=1, max_len=256)),
                             Field("expired", Bool())), 0, RELEASE_RESULTS_MAX)),
    # Release performance never gates publication (design section 12).
    Field("enforcement", Const("advisory")))


def validate_release_handoff(doc, where="handoff.json"):
    RELEASE_HANDOFF.validate(doc, where)
    status, stage, mode = doc["status"], doc["stage"], doc["mode"]
    require((doc["reason"] is None) == (status in ("claimed", "measure", "linked")),
            where + ".reason: exactly a run that does not claim, measure or link says why")
    require(doc["actions"]["event"] == ("workflow_run" if mode == "release"
                                        else "workflow_dispatch"),
            where + ".mode: a release handoff is a workflow_run, a remeasurement a dispatch")
    require(status != "claimed" or stage == "handoff",
            where + ".status: a claim is written by the handoff stage")
    require(status != "measure" or stage == "confirm",
            where + ".status: only the confirmation measures")
    require(stage == "handoff" or doc["claim_artifact"] is not None,
            where + ".claim_artifact: the confirmation is of a written claim")
    require((doc["key"] is None) == (doc["identity"] is None),
            where + ".key: exactly an identity has its key")
    identity = doc["identity"]
    if identity is not None:
        require(doc["key"] == release_identity_key(identity),
                where + ".key: the SHA-256 of the identity's canonical serialization")
        require(identity["repository"] == doc["actions"]["repository"]
                and doc["release"] is not None and doc["upstream"] is not None
                and doc["publication"] is not None
                and (identity["tag"], identity["sha"], identity["upstream_run_id"])
                == (doc["release"]["tag"], doc["release"]["sha"], doc["upstream"]["run_id"])
                and doc["release"]["sha"] == doc["upstream"]["head_sha"],
                where + ".identity: this repository's verified upstream run, tag and SHA")
    if doc["claim_artifact"] is not None:
        require(identity is not None and doc["claim_artifact"] == release_claim_artifact(
            doc["key"], mode, doc["actions"]["run_id"]),
            where + ".claim_artifact: the claim name of its key, mode and run")
    if status in ("claimed", "measure", "linked"):
        require(identity is not None and doc["history"]["state"] == "complete",
                where + ".status: only a verified identity with complete history goes on")
    if status in ("claimed", "measure"):
        require(doc["claim_artifact"] is not None, where + ".claim_artifact: what is written")
    if status == "linked":
        require(doc["first_claim"] is not None and any(not item["expired"]
                                                       for item in doc["results"]),
                where + ".results: a duplicate links the first claim's retained result")
    if mode == "remeasure" and status in ("claimed", "measure"):
        require(doc["first_claim"] is not None,
                where + ".first_claim: a remeasurement names the parent claim it repeats")
    for item in doc["history"]["claims"]:
        require(item["valid"] == (item["reason"] is None),
                where + ".history.claims: an invalid claim says why")
    return doc


SCHEDULE_REQUEST = Obj(
    Field("trigger", Str(enum=TRIGGER_KINDS)), Field("tier", Str(enum=TIERS)),
    Field("scope", Str(enum=SCOPES)),
    Field("base", Nullable(GIT_SHA)), Field("head", Str(min_len=1, max_len=256)),
    Field("control", Nullable(GIT_SHA)), Field("authority", Nullable(GIT_SHA)),
    Field("cases", Arr(ID, 0, limits.INSTANCES_MAX, unique=True)),
    Field("shards", Nullable(Int(1, limits.SHARDS_MAX))),
    Field("pairs", Nullable(Int(limits.PAIRS_MIN, limits.PAIRS_MAX))),
    Field("pr_number", Nullable(Int(1, 2 ** 31))),
    Field("pr_base_sha", Nullable(GIT_SHA)), Field("pr_head_sha", Nullable(GIT_SHA)),
    Field("build_image", Nullable(images.REFERENCE)))
SCHEDULE_COMPARISON = Obj(
    Field("role", Str(enum=COMPARISON_ROLES)), Field("run_id", ID),
    Field("status", Str(enum=COMPARISON_STATUSES)), Field("reason", Nullable(ID)),
    Field("detail", TEXT), Field("request", SCHEDULE_REQUEST),
    # The frozen plan's identity and what it is: never required unless an activated full plan.
    Field("plan_digest", Nullable(DIGEST)), Field("plan_sha256", Nullable(DIGEST)),
    Field("required_check_eligible", Bool()), Field("harness_changed", Nullable(Bool())),
    Field("shards", Int(0, limits.SHARDS_MAX)), Field("concurrent_jobs", Int(0, 8)),
    Field("runner_label", Nullable(SHORT_TEXT)))
SCHEDULE = Obj(
    Field("schema", Const(SCHEDULE_SCHEMA)),
    Field("actions", Obj(Field("repository", Str(pattern=r"^[A-Za-z0-9_.-]{1,100}/"
                                                 r"[A-Za-z0-9_.-]{1,100}$", max_len=201)),
                         Field("run_id", Int(1, 2 ** 53)), Field("run_attempt", Int(1, 1000)),
                         Field("event", Str(min_len=1, max_len=64)),
                         Field("ref", Str(max_len=512)), Field("sha", GIT_SHA))),
    Field("status", Str(enum=("scheduled", "skipped"))), Field("reason", Nullable(ID)),
    Field("detail", TEXT),
    Field("pull_request", Nullable(Obj(
        Field("number", Int(1, 2 ** 31)), Field("base_ref", Str(min_len=1, max_len=256)),
        Field("base_sha", GIT_SHA), Field("head_sha", GIT_SHA),
        Field("head_repository", Str(max_len=201)), Field("fork", Bool()),
        Field("merge_sha", GIT_SHA)))),
    Field("trust", Obj(Field("trusted", Bool()),
                       Field("executor", Str(enum=EXECUTORS)),
                       Field("detail", TEXT))),
    # The laboratory never enforces from a workflow until a reviewed activation and a
    # separately authorized ruleset change make it required (design section 12).
    Field("enforcement", Const("advisory")),
    Field("policy_changed", Nullable(Bool())),
    Field("comparisons", Arr(SCHEDULE_COMPARISON, 0, 4)),
    Field("matrix", Arr(Obj(Field("run_id", ID), Field("shard", Int(0, limits.SHARDS_MAX - 1))),
                        0, 4 * limits.SHARDS_MAX)),
    Field("concurrent_jobs", Int(0, limits.CONCURRENT_JOBS_MAX)),
    Field("retention_days", Int(1, 90)),
    # The confirmed release claim this schedule measures, or None (ADR-0180).
    Field("release", Nullable(Obj(
        Field("kind", Str(enum=RELEASE_MODES)), Field("key", DIGEST),
        Field("claim_artifact", RELEASE_CLAIM_ARTIFACT),
        Field("upstream_run_id", ACTIONS_ID), Field("tag", RELEASE_TAG),
        Field("sha", GIT_SHA), Field("remeasure_of", Nullable(ID))))))


def validate_schedule(doc, where="schedule.json"):
    SCHEDULE.validate(doc, where)
    event, release = doc["actions"]["event"], doc["release"]
    require(release is None or (doc["status"] == "scheduled" and event == (
        "workflow_run" if release["kind"] == "release" else "workflow_dispatch")),
            where + ".release: a release claim is scheduled from its own handoff's event")
    require(event != "workflow_run" or release is not None or doc["status"] == "skipped",
            where + ": a workflow_run schedules only a confirmed release claim")
    require(release is None or (release["claim_artifact"] == release_claim_artifact(
        release["key"], release["kind"], doc["actions"]["run_id"])
        and (release["kind"] == "remeasure") == (release["remeasure_of"] is not None)),
            where + ".release: the claim artifact and parent of its key and kind")
    require(release is None or all(
        item["request"]["trigger"] == ("release" if release["kind"] == "release" else "manual")
        and item["role"] in ("primary", "control_bootstrap") for item in doc["comparisons"]),
            where + ".release: a release claim plans only its release comparison")
    comparisons = doc["comparisons"]
    ids = [item["run_id"] for item in comparisons]
    require(len(ids) == len(set(ids)), where + ".comparisons: each run once")
    roles = [item["role"] for item in comparisons]
    require(len(roles) == len(set(roles)), where + ".comparisons: each role once")
    require((doc["status"] == "skipped") == (doc["reason"] is not None and not comparisons),
            where + ": a skipped event has its reason and no comparison")
    require((doc["actions"]["event"] == "pull_request") == (doc["pull_request"] is not None),
            where + ".pull_request: exactly a pull_request event records its pull request")
    trust = doc["trust"]
    require(trust["trusted"] or trust["executor"] == "ephemeral_hosted",
            where + ".trust: an untrusted event runs only on ephemeral hosted runners")
    require(doc["actions"]["event"] in TRUSTED_EVENTS or not trust["trusted"],
            where + ".trust: only a push, schedule, dispatch or release handoff is trusted")
    require(doc["retention_days"] == (14 if doc["actions"]["event"] == "pull_request" else 90),
            where + ".retention_days: 14 for a pull request, 90 otherwise")
    planned = dict((item["run_id"], item) for item in comparisons if item["status"] == "planned")
    for item in comparisons:
        at = "%s.comparisons[%s]" % (where, item["role"])
        require((item["status"] == "planned") == (item["plan_digest"] is not None)
                == (item["plan_sha256"] is not None) == (item["reason"] is None),
                at + ": a planned comparison has its plan, any other its reason")
        require(item["status"] == "planned" or (item["shards"] == 0
                                                 and item["concurrent_jobs"] == 0),
                at + ": nothing runs of a comparison that was not planned")
        require(not item["required_check_eligible"]
                or (item["role"] == "primary" and item["request"]["scope"] == "full"),
                at + ": only the primary full comparison can be eligible for the required check")
        require(item["role"] in ("primary", "control_bootstrap")
                or (item["request"]["scope"] == "bootstrap"
                    and item["request"]["authority"] is not None),
                at + ": a proposed-policy comparison is a bootstrap of the proposed authority")
    expected = [(run_id, shard) for run_id, item in sorted(planned.items())
                for shard in range(item["shards"])]
    require(sorted((entry["run_id"], entry["shard"]) for entry in doc["matrix"]) == expected,
            where + ".matrix: every shard of every planned comparison, once")
    require(doc["concurrent_jobs"] == max([item["concurrent_jobs"]
                                           for item in planned.values()] or [0]),
            where + ".concurrent_jobs: the largest planned comparison's frozen bound")
    return doc


BUILD_PACK = Obj(
    Field("schema", Const(BUILD_PACK_SCHEMA)), Field("run_id", ID),
    Field("plan_digest", DIGEST), Field("builds_sha256", DIGEST),
    Field("archive", Obj(Field("name", Const("build.tar")), Field("size", COUNT),
                         Field("sha256", DIGEST))),
    # Every file of the build directory, in path order, with the mode restored after its bytes
    # are verified: executables 0755, everything else 0644.
    Field("entries", Arr(Obj(Field("path", REL_PATH), Field("size", COUNT),
                             Field("sha256", DIGEST), Field("executable", Bool())),
                         1, limits.BUILD_PACK_ENTRIES_MAX)),
    Field("bytes", COUNT))


def validate_build_pack(doc, where="build-pack.json"):
    BUILD_PACK.validate(doc, where)
    paths = [entry["path"] for entry in doc["entries"]]
    require(paths == sorted(set(paths)), where + ".entries: sorted, each path once")
    require("builds.json" in paths, where + ".entries: the build's builds.json")
    require(sum(entry["size"] for entry in doc["entries"]) == doc["bytes"]
            and doc["bytes"] <= limits.BUILD_ARTIFACT_BYTES_MAX + limits.BUILD_PACK_EXTRA_BYTES,
            where + ".bytes: the entries' total, within the shared build artifact bound")
    builds = [entry for entry in doc["entries"] if entry["path"] == "builds.json"][0]
    require(builds["sha256"] == doc["builds_sha256"],
            where + ".builds_sha256: the packed builds.json")
    return doc



# Guarded kernel experiments (ADR-0185) ---------------------------------------------------------

KERNEL_INVENTORY_SCHEMA = "af.perf-kernel-inventory/1"
KERNEL_INVENTORY = Obj(
    Field("schema", Const(KERNEL_INVENTORY_SCHEMA)),
    # The digest the literal bootstrap of perflab.kernelguard computes over exactly these files.
    Field("harness_digest", DIGEST),
    Field("files", Arr(Obj(Field("path", REL_PATH), Field("size", COUNT),
                           Field("sha256", DIGEST)), 1, 8192)),
    # What the staged objectives compare, and whether the guarded inner metric may be selected.
    Field("objective_metric", Const("elapsed_ms")),
    Field("inner_metric", Str(enum=("pending",))),
    Field("inner_metric_reason", TEXT),
)


def validate_kernel_inventory(doc, where="inventory.json"):
    """The files are sorted, unique and exclude the staged subtree, and the digest is the
    bootstrap's own digest of them."""
    KERNEL_INVENTORY.validate(doc, where)
    from . import kernelguard
    paths = [item["path"] for item in doc["files"]]
    require(paths == sorted(set(paths), key=lambda path: path.encode("utf-8")),
            where + ".files: sorted unique paths")
    require(not any(kernelguard.RULES["staged"](path) for path in paths)
            and all(kernelguard.RULES["protected"](path) for path in paths),
            where + ".files: only protected paths, never the staged policy subtree")
    require(set(kernelguard.RULES["REQUIRED"]) <= set(paths),
            where + ".files: every required protected file")
    import hashlib
    digest = hashlib.sha256((kernelguard.RULES["SCHEMA"] + "\n").encode("utf-8"))
    for item in doc["files"]:
        digest.update(("%s\0%d\0%s\n" % (item["path"], item["size"], item["sha256"]))
                      .encode("utf-8"))
    require(doc["harness_digest"] == "sha256:" + digest.hexdigest(),
            where + ".harness_digest: not the bootstrap's digest of the listed files")
    return doc


# What the native kernel integration exports (ADR-0186, ADR-0187): every Task's exit code and
# kernel result, each output's complete typed envelope, and the public stdout and stderr of
# every measured repetition, bounded. Evidence only; no code decides anything from it. The
# reader recomputes what it can: each envelope's content and artifact IDs under the kernel's
# canonical JSON and domains, each whole stream's content ID, and the Task result's reference
# to every retained output. A stream cut to its head and tail keeps the exporter's identity of
# the bytes it read; nothing here rehashes the omitted middle.
KERNEL_RECEIPTS_SCHEMA = "af.perf-kernel-receipts/3"
# A retained stream keeps at most this many bytes of its head and of its tail: a stream of at
# most twice that is kept whole, split there; a longer one loses only its middle.
KERNEL_STREAM_PART_BYTES = 32 * 1024
KERNEL_STREAM_PART = Str(max_len=4 * ((KERNEL_STREAM_PART_BYTES + 2) // 3))
KERNEL_STREAM = Obj(
    # The kernel's content ID of the stream and the SHA-256 of its exact bytes. `retained`
    # says what the document holds: the `whole` stream, which a reader rehashes, or only its
    # `head_and_tail`, whose ID and hash are the exporter's and cannot be recomputed here.
    Field("id", DIGEST), Field("sha256", DIGEST), Field("bytes", COUNT),
    Field("retained", Str(enum=("whole", "head_and_tail"))),
    Field("encoding", Str(enum=("utf-8", "base64"))),
    Field("head", KERNEL_STREAM_PART), Field("tail", KERNEL_STREAM_PART),
    Field("omitted_bytes", COUNT))
KERNEL_OUTPUT_TYPES = {"baseline": "af/Measurement@1", "candidate": "af/Measurement@1",
                       "comparison": "af/MeasurementComparison@1"}
KERNEL_TEXT = Str(min_len=1, max_len=1024)
KERNEL_NAME = r"^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$"
KERNEL_ARTIFACT_TYPE = Str(pattern=r"^[a-z][a-z0-9.]*/[A-Za-z][A-Za-z0-9]*@[1-9][0-9]*$",
                           max_len=256)
# `review_core::ArtifactEnvelope`, closed as the kernel's serde form is: a producer is an
# Attempt or a kernel operation, and an absent subject Snapshot is omitted, never null.
KERNEL_ENVELOPE = Obj(
    Field("type", Str(enum=tuple(sorted(set(KERNEL_OUTPUT_TYPES.values()))))),
    Field("artifact_id", DIGEST), Field("content_id", DIGEST),
    Field("producer", Tagged("kind", {
        "attempt": Obj(Field("kind", Const("attempt")), Field("run_id", KERNEL_TEXT),
                       Field("node_id", KERNEL_TEXT),
                       Field("attempt_id", Str(pattern=r"^[0-9a-z]{26}$", max_len=26))),
        "kernel_operation": Obj(Field("kind", Const("kernel_operation")),
                                Field("run_id", KERNEL_TEXT),
                                Field("node_id", KERNEL_TEXT, required=False),
                                Field("operation_id", KERNEL_TEXT))})),
    Field("input_artifacts", Arr(DIGEST, 0, 4096)),
    Field("subject_snapshot_id", DIGEST, required=False),
    Field("payload", JsonObject()))
KERNEL_RECEIPT_OUTPUT = Obj(
    Field("artifact_id", DIGEST),
    Field("type", Str(enum=tuple(sorted(set(KERNEL_OUTPUT_TYPES.values()))))),
    Field("snapshot_id", Nullable(DIGEST)),
    # The complete artifact envelope: producer, inputs, subject Snapshot and payload.
    Field("envelope", KERNEL_ENVELOPE),
    # One entry per measured repetition, in order, with its retained streams.
    Field("streams", Arr(Obj(Field("repetition", Int(1, 1000)),
                             Field("stdout", Nullable(KERNEL_STREAM)),
                             Field("stderr", Nullable(KERNEL_STREAM))), 0, 1000)))
# `review_core::task::TaskResultV1` as `af task start --json` prints it: each output port's
# exact artifact IDs, type, cardinality and Snapshot, an absent Snapshot omitted.
KERNEL_TASK_RESULT = Obj(
    Field("task_revision_id", DIGEST),
    Field("execution", Str(enum=("completed", "incomplete", "exhausted"))),
    Field("acceptance", Str(enum=("satisfied", "unsatisfied", "inconclusive"))),
    Field("domain_conclusion", KERNEL_TEXT),
    Field("outputs", Map(KERNEL_NAME, Obj(
        Field("artifact_ids", Arr(DIGEST, 0, 4096, unique=True)),
        Field("artifact_type", KERNEL_ARTIFACT_TYPE),
        Field("cardinality", Str(enum=("one", "many"))),
        Field("snapshot_id", DIGEST, required=False)), 0, 256)),
    Field("evidence", Arr(DIGEST, 0, 4096, unique=True)),
    Field("missing_obligations", Arr(Str(pattern=KERNEL_NAME, max_len=128), 0, 256,
                                     unique=True)))
KERNEL_RECEIPTS = Obj(
    Field("schema", Const(KERNEL_RECEIPTS_SCHEMA)), Field("harness_digest", DIGEST),
    Field("instance", ID), Field("measure", ID), Field("objective", ID),
    Field("slowdown_ms", COUNT),
    Field("tasks", Arr(Obj(
        Field("task_id", Str(pattern=r"^[a-z][a-z0-9_-]{0,63}$", max_len=64)),
        Field("exit_code", Int(0, 255)),
        # The Task inspection's `result`, exactly as `af task start --json` printed it.
        Field("result", KERNEL_TASK_RESULT),
        Field("outputs", Obj(Field("baseline", KERNEL_RECEIPT_OUTPUT, required=False),
                             Field("candidate", KERNEL_RECEIPT_OUTPUT, required=False),
                             Field("comparison", KERNEL_RECEIPT_OUTPUT, required=False)))),
        1, 64)))

# The kernel's domain separators (`review_store::canonical`): a payload's or a blob's content
# ID, and an envelope's artifact ID over everything but that ID.
KERNEL_CONTENT_DOMAIN = b"review.kernel/content-id/v1\0"
KERNEL_ARTIFACT_DOMAIN = b"review.kernel/artifact-id/v1\0"
_KERNEL_SAFE_INTEGER = 2 ** 53 - 1


def _kernel_canonical(value, out, where):
    """RFC 8785 as `review_store::canonical` writes it: members ordered by UTF-16 code units,
    strings escaped only where JSON requires, integers in the interoperable range. A float is
    refused, as the laboratory's JSON reader already refuses it."""
    import json
    if value is None or value is True or value is False:
        out.append(json.dumps(value).encode("ascii"))
    elif type(value) is int:
        require(-_KERNEL_SAFE_INTEGER <= value <= _KERNEL_SAFE_INTEGER,
                where + ": an integer outside the kernel's safe range")
        out.append(b"%d" % value)
    elif isinstance(value, str):
        try:
            out.append(json.dumps(value, ensure_ascii=False).encode("utf-8"))
        except UnicodeEncodeError:
            raise ContractError(where + ": a string that is not Unicode scalar values")
    elif isinstance(value, list):
        out.append(b"[")
        for index, item in enumerate(value):
            if index:
                out.append(b",")
            _kernel_canonical(item, out, "%s[%d]" % (where, index))
        out.append(b"]")
    elif isinstance(value, dict):
        out.append(b"{")
        keys = sorted(value, key=lambda key: key.encode("utf-16-be", "surrogatepass"))
        for index, key in enumerate(keys):
            if index:
                out.append(b",")
            _kernel_canonical(key, out, where + ": key")
            out.append(b":")
            _kernel_canonical(value[key], out, where + "." + key)
        out.append(b"}")
    else:
        raise ContractError(where + ": not a canonical kernel JSON value")


def kernel_digest(domain, value, where):
    """The kernel's domain-separated SHA-256 of `value`'s canonical bytes."""
    import hashlib
    out = []
    _kernel_canonical(value, out, where)
    return "sha256:" + hashlib.sha256(domain + b"".join(out)).hexdigest()


def kernel_blob_id(data):
    """The kernel's content ID of an opaque blob, such as a stream."""
    import hashlib
    return "sha256:" + hashlib.sha256(KERNEL_CONTENT_DOMAIN + data).hexdigest()


# What Rust's `str::trim` removes: every character with the Unicode White_Space property. Python's
# `str.isspace` differs (it also holds U+001C to U+001F), so the set is spelled out.
_KERNEL_WHITESPACE = ("\t\n\x0b\x0c\r \x85\xa0\u1680\u2000\u2001\u2002\u2003\u2004\u2005"
                      "\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000")
_KERNEL_NAME_FULL = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}")
_KERNEL_ARTIFACT_TYPE_FULL = re.compile(r"[a-z][a-z0-9.]*/[A-Za-z][A-Za-z0-9]*@[1-9][0-9]*")


def _kernel_blank(text):
    """Whether `text.trim().is_empty()` holds in Rust."""
    return not text.strip(_KERNEL_WHITESPACE)


def _kernel_producer(producer, at):
    """`ArtifactEnvelope::validate`'s producer rule: every named run, node and operation is
    nonblank after Unicode trimming; a kernel operation's node, when present, too."""
    names = (("run_id", "node_id") if producer["kind"] == "attempt"
             else ("run_id", "operation_id") + (("node_id",) if "node_id" in producer else ()))
    for name in names:
        require(not _kernel_blank(producer[name]),
                "%s.%s: blank, as the kernel refuses a %s producer's" % (at, name,
                                                                         producer["kind"]))


def _kernel_task_result(result, at):
    """`TaskResultV1`'s deserialization and `validate` beyond the closed shape: both sets
    strictly ascending, a nonblank domain conclusion, satisfied acceptance only with completed
    execution, evidence, outputs and no missing obligation, and every present output an exact
    binding (`ArtifactInputV1::validate`) naming at least one artifact, exactly one for
    cardinality `one`. Names and types are matched whole, as the kernel's byte checks do, so a
    trailing newline is refused."""
    # `unique_set` reads `evidence` and `missing_obligations` into `BTreeSet<String>` and
    # refuses an entry not greater than the one before it. Rust orders a `String` by its UTF-8
    # bytes, which is Python's code point order. An output's `artifact_ids` is a `Vec`, in any
    # order.
    for name in ("evidence", "missing_obligations"):
        items = result[name]
        require(all(before < after for before, after in zip(items, items[1:])),
                "%s.%s: set entries must be sorted and unique, as the kernel reads them"
                % (at, name))
    require(not _kernel_blank(result["domain_conclusion"]),
            at + ".domain_conclusion: blank, as the kernel refuses a Task result's")
    if result["acceptance"] == "satisfied":
        require(result["execution"] == "completed" and not result["missing_obligations"]
                and result["evidence"] and result["outputs"],
                at + ": satisfied acceptance requires completed execution and evidence and "
                "outputs with no missing obligations")
    for name in result["missing_obligations"]:
        require(_KERNEL_NAME_FULL.fullmatch(name) is not None,
                at + ".missing_obligations: not a kernel name: %r" % name)
    for port in sorted(result["outputs"]):
        binding = result["outputs"][port]
        where = "%s.outputs.%s" % (at, port)
        require(_KERNEL_NAME_FULL.fullmatch(port) is not None,
                at + ".outputs: not a kernel output name: %r" % port)
        require(_KERNEL_ARTIFACT_TYPE_FULL.fullmatch(binding["artifact_type"]) is not None,
                where + ".artifact_type: not a versioned kernel artifact type")
        # The kernel's order: the binding's own cardinality first, then emptiness.
        require(binding["cardinality"] == "many" or len(binding["artifact_ids"]) == 1,
                where + ": cardinality one names exactly one artifact")
        require(binding["artifact_ids"],
                where + ": a present Task result output must contain an artifact")


def _kernel_envelope(envelope, at):
    """Both identities the envelope carries are the kernel's own of its content, and its
    producer is one the kernel admits."""
    _kernel_producer(envelope["producer"], at + ".producer")
    require(envelope["content_id"] == kernel_digest(KERNEL_CONTENT_DOMAIN, envelope["payload"],
                                                    at + ".payload"),
            at + ".content_id: not the kernel's content ID of the payload")
    unsigned = dict((key, item) for key, item in envelope.items() if key != "artifact_id")
    require(envelope["artifact_id"] == kernel_digest(KERNEL_ARTIFACT_DOMAIN, unsigned, at),
            at + ".artifact_id: not the kernel's artifact ID of the envelope")


def _kernel_stream(stream, where):
    import base64
    import hashlib
    decode = ((lambda text: text.encode("utf-8")) if stream["encoding"] == "utf-8"
              else (lambda text: base64.b64decode(text.encode("ascii"), validate=True)))
    try:
        head, tail = decode(stream["head"]), decode(stream["tail"])
    except ValueError:
        raise ContractError(where + ": not %s text" % stream["encoding"])
    # The schema bounds each part's characters; the format bounds its decoded bytes, which a
    # base64 or multibyte UTF-8 part of allowed length can exceed.
    require(len(head) <= KERNEL_STREAM_PART_BYTES and len(tail) <= KERNEL_STREAM_PART_BYTES,
            where + ": a retained part holds at most %d bytes" % KERNEL_STREAM_PART_BYTES)
    require(len(head) + len(tail) + stream["omitted_bytes"] == stream["bytes"],
            where + ": head, tail and omitted bytes are the stream's bytes")
    require((stream["retained"] == "whole") == (stream["omitted_bytes"] == 0),
            where + ".retained: whole exactly when no byte is omitted")
    if stream["omitted_bytes"]:
        # Only the exporter read the middle: its ID and hash are carried, not recomputed.
        require(len(head) == len(tail) == KERNEL_STREAM_PART_BYTES,
                where + ": a cut stream keeps exactly its first and last %d bytes"
                % KERNEL_STREAM_PART_BYTES)
    else:
        require(len(head) == min(stream["bytes"], KERNEL_STREAM_PART_BYTES)
                and stream["sha256"] == "sha256:" + hashlib.sha256(head + tail).hexdigest(),
                where + ": a whole stream is its head and tail, with that SHA-256")
        require(stream["id"] == kernel_blob_id(head + tail),
                where + ".id: not the kernel's content ID of the whole stream")


def _kernel_reference(result, port, output, at):
    """The Task result names exactly the retained output on `port`, or leaves the port absent
    when none is retained, as a failed Task does: the kernel never emits an empty binding."""
    binding = result["outputs"].get(port)
    if output is None:
        require(binding is None,
                at + ": the Task result names an output on this port that is not retained")
        return
    require(binding is not None, at + ": the Task result names no output on this port")
    require(binding["artifact_ids"] == [output["artifact_id"]]
            and binding["artifact_type"] == output["type"] and binding["cardinality"] == "one"
            and binding.get("snapshot_id") == output["snapshot_id"],
            at + ": the Task result's artifact, type and Snapshot are this output's")


def validate_kernel_receipts(doc, where="receipts.json"):
    """Each output is the artifact its Task's result names, its envelope's identities are the
    kernel's own of its content, a Measurement's streams are exactly its runs' streams in order,
    and every retained stream is consistent with its size and hash, and with its ID when it is
    kept whole."""
    KERNEL_RECEIPTS.validate(doc, where)
    for index, task in enumerate(doc["tasks"]):
        _kernel_task_result(task["result"], "%s.tasks[%d].result" % (where, index))
        for port in sorted(KERNEL_OUTPUT_TYPES):
            output = task["outputs"].get(port)
            at = "%s.tasks[%d].outputs.%s" % (where, index, port)
            _kernel_reference(task["result"], port, output, at)
            if output is None:
                continue
            require(output["type"] == KERNEL_OUTPUT_TYPES[port],
                    at + ".type: the port's artifact type")
            envelope = output["envelope"]
            require((envelope["artifact_id"], envelope["type"],
                     envelope.get("subject_snapshot_id"))
                    == (output["artifact_id"], output["type"], output["snapshot_id"]),
                    at + ".envelope: the artifact, type and Snapshot it names")
            _kernel_envelope(envelope, at + ".envelope")
            runs = envelope["payload"].get("runs") if port != "comparison" else []
            require(isinstance(runs, list) and all(isinstance(run, dict) for run in runs),
                    at + ".envelope.payload.runs: a list of runs")
            require([entry["repetition"] for entry in output["streams"]]
                    == list(range(1, len(runs) + 1)), at + ".streams: one per run, in order")
            for entry, run in zip(output["streams"], runs):
                for name in ("stdout", "stderr"):
                    stream = entry[name]
                    require((stream["id"] if stream else None) == run.get(name + "_id"),
                            "%s.streams[%d].%s: the run's own stream"
                            % (at, entry["repetition"] - 1, name))
                    if stream is not None:
                        _kernel_stream(stream, "%s.streams[%d].%s"
                                       % (at, entry["repetition"] - 1, name))
    return doc


ENFORCEMENT_SCHEMA = "af.perf-enforcement/1"
ENFORCEMENT = Obj(Field("schema", Const(ENFORCEMENT_SCHEMA)),
                  Field("mode", Str(enum=("advisory", "required"))))


def validate_enforcement(doc, where="enforcement.toml"):
    ENFORCEMENT.validate(doc, where)
    return doc


# Telemetry experiments (ADR-0214) ----------------------------------------------------------
# af.perf-telemetry-lab/3 is fixtures/performance/telemetry.toml; af.perf-telemetry-result/3 one
# experiment run of a baseline and a candidate binary on one host with this checkout's scripts
# and fixtures; af.perf-telemetry-history/2 the compatible series across results. Advisory: no
# verdict, sample or required check reads any of them.

TELEMETRY_LAB_SCHEMA = "af.perf-telemetry-lab/3"
TELEMETRY_RESULT_SCHEMA = "af.perf-telemetry-result/3"
TELEMETRY_HISTORY_SCHEMA = "af.perf-telemetry-history/2"
# A separate immutable Actions planning artifact, emitted before any candidate execution.
TELEMETRY_BINDING_SCHEMA = "af.perf-telemetry-binding/1"
TELEMETRY_BINDING = Obj(
    Field("schema", Const(TELEMETRY_BINDING_SCHEMA)), Field("repository", SHORT_TEXT),
    Field("run_id", ACTIONS_ID), Field("attempt", Int(1, 1000)),
    Field("event", Str(enum=("pull_request", "schedule", "release", "workflow_dispatch"))),
    Field("event_sha", GIT_SHA), Field("run_head_sha", GIT_SHA),
    Field("candidate_sha", GIT_SHA), Field("baseline_sha", GIT_SHA),
    Field("control_sha", GIT_SHA), Field("profile", Str(enum=("pr", "nightly", "release", "manual"))),
)


def validate_telemetry_binding(doc, where="binding.json"):
    TELEMETRY_BINDING.validate(doc, where)
    return doc


TELEMETRY_EXPERIMENTS = ("overhead", "invocation_cost", "read_latency")
TELEMETRY_PROFILES = ("pr", "nightly", "release", "manual")
TELEMETRY_DOCUMENTS = {"overhead": ("af/telemetry-overhead@2", "telemetry-overhead-v2.json",
                                    "scripts/telemetry-overhead.py", ("off", "spans")),
                       "invocation_cost": ("af/telemetry-invocation-cost@2",
                                           "telemetry-invocation-cost-v2.json",
                                           "scripts/telemetry-invocation-cost.py",
                                           ("off", "spans")),
                       "read_latency": ("af/telemetry-read-latency@2",
                                        "telemetry-read-latency-v2.json",
                                        "scripts/telemetry-corpus.py", ("content",))}
TELEMETRY_LEVEL_NAMES = ("off", "spans", "content")
TELEMETRY_ROUNDS_MAX = 8
TELEMETRY_PAIRS_MAX = 40
TELEMETRY_RUNS_MAX = len(TELEMETRY_EXPERIMENTS) * TELEMETRY_ROUNDS_MAX * 2
TELEMETRY_INPUT = Str(pattern=r"^(?:scripts|fixtures)/[A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)*$",
                      max_len=256)
TELEMETRY_PAIRED = Obj(Field("pairs", Int(1, TELEMETRY_PAIRS_MAX)),
                       Field("scale", Str(enum=("full", "smoke"))))
TELEMETRY_CORPUS = Obj(Field("runs", Int(10, 10000)), Field("repeat", Int(1, 9)))
TELEMETRY_EXPERIMENT = Obj(
    Field("description", TEXT), Field("script", TELEMETRY_INPUT),
    Field("document", Str(enum=tuple(sorted(item[0] for item in TELEMETRY_DOCUMENTS.values())))),
    Field("schema_file", Str(pattern=r"^telemetry-[a-z-]{1,48}-v[1-9][0-9]{0,2}\.json$",
                             max_len=64)),
    Field("inputs", Arr(TELEMETRY_INPUT, 1, 8, unique=True)),
    Field("levels", Arr(Str(enum=TELEMETRY_LEVEL_NAMES), 1, 3, unique=True)))
TELEMETRY_PROFILE = Obj(
    Field("description", TEXT), Field("rounds", Int(1, TELEMETRY_ROUNDS_MAX)),
    Field("timeout_minutes", Int(1, 355)),
    Field("work_bytes", Int(1 << 20, 64 << 30)),
    Field("retention_days", Int(1, 90)),
    Field("experiments", Arr(Str(enum=TELEMETRY_EXPERIMENTS), 1, 3, unique=True)),
    Field("overhead", TELEMETRY_PAIRED), Field("invocation_cost", TELEMETRY_PAIRED),
    Field("read_latency", TELEMETRY_CORPUS))
# The container every contained execution runs in (ADR-0215); its scratch filesystem's bound is
# the profile's work bytes.
TELEMETRY_CONTAINMENT = Obj(Field("logical_cpus", Int(1, 64)),
                            Field("memory_limit_bytes", Int(256 << 20, 64 << 30)),
                            Field("pid_limit", Int(64, 4096)))
TELEMETRY_LAB = Obj(
    Field("schema", Const(TELEMETRY_LAB_SCHEMA)), Field("description", TEXT),
    Field("seed", Int(0, 2 ** 31 - 1)), Field("containment", TELEMETRY_CONTAINMENT),
    Field("experiments", Obj(*[Field(name, TELEMETRY_EXPERIMENT)
                               for name in TELEMETRY_EXPERIMENTS])),
    Field("profiles", Obj(*[Field(name, TELEMETRY_PROFILE) for name in TELEMETRY_PROFILES])))


def validate_telemetry_lab(doc, where="telemetry.toml"):
    """The registry's shape, and every experiment exactly the bench script, document, schema
    and levels design section 12 names: a registry cannot point a measurement elsewhere."""
    TELEMETRY_LAB.validate(doc, where)
    for name, (document, schema_file, script, levels) in TELEMETRY_DOCUMENTS.items():
        entry = doc["experiments"][name]
        at = "%s.experiments.%s" % (where, name)
        require((entry["document"], entry["schema_file"], entry["script"])
                == (document, schema_file, script),
                at + ": names %s, %s and %s" % (script, document, schema_file))
        require(tuple(entry["levels"]) == levels, at + ".levels: exactly " + ", ".join(levels))
        require(script in entry["inputs"], at + ".inputs: hold its own script")
    return doc


TELEMETRY_RUN_STATUSES = ("ok", "failed", "timeout", "refused_document", "not_run")
TELEMETRY_VERDICTS = ("pass", "fail", "not_judged")
TELEMETRY_METRICS = ("wall_ms", "peak_rss_bytes", "drain_ms", "latency_ms")
TELEMETRY_FIGURES = ("overhead_median_pct", "overhead_upper_pct", "drain_median_ms",
                     "wall_median_pct", "wall_upper_pct", "rss_median_bytes", "latency_ms",
                     "index_bytes")
TELEMETRY_LOG = Obj(Field("path", REL_PATH), Field("sha256", DIGEST), Field("size", COUNT),
                    Field("truncated", Bool()))
# A sanitized projection, never an arbitrary refused document. The numeric token is kept
# verbatim up to 512 bytes, with the complete token and document identities even at the cap.
TELEMETRY_MEASUREMENT_PATH = (r"^[a-z_][a-z0-9_-]*(?:\[(?:0|[1-9][0-9]{0,7})\]|"
                              r"\.[a-z_][a-z0-9_-]*)*$")
TELEMETRY_MEASUREMENT_REFUSAL = Obj(
    Field("document_sha256", DIGEST), Field("document_bytes", Int(1, 16 * 1024 * 1024)),
    Field("field", Str(min_len=1, max_len=256,
                       pattern=TELEMETRY_MEASUREMENT_PATH)),
    Field("token", Str(min_len=1, max_len=512,
                       pattern=r"^(?:[-+0-9.eE]+|NaN|Infinity|-Infinity)$")),
    Field("token_bytes", Int(1, 16 * 1024 * 1024)), Field("token_sha256", DIGEST),
    Field("reason", Str(enum=("nonfinite", "negative", "not_positive", "not_integer",
                              "unrepresentable", "token_too_long"))))
TELEMETRY_RUN = Obj(
    Field("experiment", Str(enum=TELEMETRY_EXPERIMENTS)),
    Field("round", Int(0, TELEMETRY_ROUNDS_MAX - 1)), Field("side", Str(enum=SIDES)),
    # The side's position in its round: the sides alternate by round, baseline first in even
    # rounds, so neither side always runs first.
    Field("position", Int(0, 1)), Field("seed", Int(0, 2 ** 31 - 1 + TELEMETRY_ROUNDS_MAX)),
    Field("argv", Arr(Str(min_len=1, max_len=512), 1, 64)),
    Field("status", Str(enum=TELEMETRY_RUN_STATUSES)),
    Field("exit_code", Nullable(Int(-255, 255))), Field("wall_ms", Nullable(DECIMAL)),
    Field("document", Nullable(ARTIFACT)), Field("log", Nullable(TELEMETRY_LOG)),
    Field("measurement_refusal", Nullable(TELEMETRY_MEASUREMENT_REFUSAL)),
    Field("judgment", Nullable(Obj(Field("verdict", Str(enum=TELEMETRY_VERDICTS)),
                                   Field("reasons", Arr(Str(max_len=512), 0, 64))))),
    Field("detail", TEXT))
TELEMETRY_WITHIN = Obj(
    Field("experiment", Str(enum=TELEMETRY_EXPERIMENTS)),
    Field("round", Int(0, TELEMETRY_ROUNDS_MAX - 1)), Field("side", Str(enum=SIDES)),
    Field("subject", SHORT_TEXT), Field("level", Nullable(Str(enum=TELEMETRY_LEVEL_NAMES))),
    Field("figure", Str(enum=TELEMETRY_FIGURES)), Field("value", RATIONAL))
TELEMETRY_COMPARISON = Obj(
    Field("experiment", Str(enum=TELEMETRY_EXPERIMENTS)), Field("subject", SHORT_TEXT),
    Field("level", Str(enum=TELEMETRY_LEVEL_NAMES)), Field("metric", Str(enum=TELEMETRY_METRICS)),
    Field("shape", Nullable(Str(enum=("json", "stream-json")))),
    Field("state", Str(enum=("compared", "incompatible", "missing"))), Field("reason", TEXT),
    Field("rounds", Arr(Obj(Field("round", Int(0, TELEMETRY_ROUNDS_MAX - 1)),
                            Field("baseline", DECIMAL), Field("candidate", DECIMAL),
                            Field("change", RATIONAL)), 0, TELEMETRY_ROUNDS_MAX)),
    Field("median_change", Nullable(RATIONAL)))
# How one side's binary was built (ADR-0215): written by the build job beside the binary, bound
# to the side's source and the binary's hash, and kept in the result.
TELEMETRY_OWNER_SCHEMA = "af.perf-telemetry-owner/2"
FILE_IDENTITY = Arr(Int(0, 2 ** 64 - 1), 2, 2)
TELEMETRY_OWNER = Obj(
    Field("schema", Const(TELEMETRY_OWNER_SCHEMA)), Field("owner_uid", Int(0, 2 ** 32 - 1)),
    Field("control", FILE_IDENTITY), Field("lock", FILE_IDENTITY),
    Field("image", Str(min_len=1, max_len=4096)),
    Field("mount", Str(min_len=1, max_len=4096)),
    Field("image_identity", FILE_IDENTITY), Field("image_bytes", Int(1, 64 * limits.GIB)),
    Field("mount_identity", FILE_IDENTITY),
    Field("phase", Str(enum=("ready", "active", "stopped", "uncertain", "removed"))),
    Field("container", Nullable(Str(pattern=r"^af-perf-telemetry-[a-z0-9_-]{1,80}$",
                                    max_len=98))),
    Field("launches", Int(0, 1000000)),
    Field("filesystem_pending", Bool()),
)


def validate_telemetry_owner(doc, where="telemetry ownership"):
    TELEMETRY_OWNER.validate(doc, where)
    require(doc["phase"] != "removed" or not doc["filesystem_pending"],
            where + ": retired ownership still has pending filesystem work")
    require((doc["phase"] in ("ready", "removed")) == (doc["container"] is None),
            where + ": only pre-launch or retired ownership has no execution")
    require((doc["phase"] == "ready") == (doc["launches"] == 0)
            or doc["phase"] == "removed", where + ": launch accounting disagrees")
    return doc


TELEMETRY_BUILD_SCHEMA = "af.perf-telemetry-build/2"
TELEMETRY_BUILD = Obj(
    Field("schema", Const(TELEMETRY_BUILD_SCHEMA)), Field("side", Str(enum=SIDES)),
    Field("source_sha", GIT_SHA), Field("binary_sha256", DIGEST),
    # The build image exactly as its build admission took it (ADR-0170): a local image ID
    # `sha256:<64 hex>` or a manifest digest `name@sha256:<64 hex>`, never a tag.
    Field("image", images.REFERENCE),
    Field("rustc", Str(min_len=1, max_len=512)), Field("cargo", Str(min_len=1, max_len=512)),
    Field("command", Str(min_len=1, max_len=512)))


def validate_telemetry_build(doc, where="af.build.json"):
    TELEMETRY_BUILD.validate(doc, where)
    return doc


TELEMETRY_SIDE_BINARY = Obj(Field("source_sha", GIT_SHA), Field("sha256", DIGEST),
                            Field("version", Nullable(Str(min_len=1, max_len=256))),
                            Field("build", Nullable(TELEMETRY_BUILD)))
TELEMETRY_MODES = ("trusted_local", "linux_container")
TELEMETRY_EXECUTION = Obj(
    Field("mode", Str(enum=TELEMETRY_MODES)),
    Field("containment", Str(enum=("process_group", "pid_namespace"))),
    Field("container", Nullable(Obj(
        Field("image", Obj(Field("reference", Str(min_len=1, max_len=512)),
                           Field("id", Str(min_len=1, max_len=512)),
                           Field("platform", Str(min_len=1, max_len=64)))),
        Field("runtime", Str(max_len=256)), Field("cpus", Arr(Int(0, 4095), 1, 64)),
        Field("memory_limit_bytes", COUNT), Field("pid_limit", COUNT),
        Field("scratch_limit_bytes", COUNT)))),
    Field("work_bytes", COUNT), Field("file_bytes", COUNT),
    # `uncertain` when an execution's end or its writable state was not confirmed; its work is
    # then kept (`retained`) and nothing more ran.
    Field("cleanup", Str(enum=("confirmed", "uncertain"))),
    Field("retained", Nullable(Str(min_len=1, max_len=4096))),
    # What an execution changed that none may: a binary, the authority, the result directory
    # or the master corpus. Nothing more ran after the first.
    Field("problems", Arr(Str(max_len=512), 0, 64)),
    Field("limitations", TEXT))
TELEMETRY_CORPUS_FACTS = Obj(Field("segments", COUNT), Field("segment_bytes", COUNT),
                             Field("segment_stored_bytes", COUNT), Field("blob_files", COUNT),
                             Field("blob_stored_bytes", COUNT))
TELEMETRY_CONFIGURATION = Obj(
    Field("digest", DIGEST), Field("seed", Int(0, 2 ** 31 - 1)),
    Field("rounds", Int(1, TELEMETRY_ROUNDS_MAX)),
    Field("experiments", Arr(Str(enum=TELEMETRY_EXPERIMENTS), 1, 3, unique=True)),
    Field("overhead", TELEMETRY_PAIRED), Field("invocation_cost", TELEMETRY_PAIRED),
    Field("read_latency", TELEMETRY_CORPUS), Field("timeout_minutes", Int(1, 355)),
    Field("work_bytes", Int(1 << 20, 64 << 30)))
TELEMETRY_RESULT = Obj(
    Field("schema", Const(TELEMETRY_RESULT_SCHEMA)), Field("result_id", ID),
    Field("trigger", Str(enum=("pr", "main_push", "nightly", "release", "manual"))),
    Field("profile", Str(enum=TELEMETRY_PROFILES)),
    Field("started_unix_ms", Int(0, 2 ** 53)), Field("ended_unix_ms", Int(0, 2 ** 53)),
    Field("sources", Obj(Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
                         Field("authority_sha", GIT_SHA))),
    # The measurement authority both sides ran under: this checkout's registry, scripts,
    # fixtures and the product schemas the documents were read through, by content.
    Field("authority", Obj(Field("digest", DIGEST), Field("registry_sha256", DIGEST),
                           Field("files", COUNT))),
    Field("binaries", Obj(Field("baseline", TELEMETRY_SIDE_BINARY),
                          Field("candidate", TELEMETRY_SIDE_BINARY))),
    Field("configuration", TELEMETRY_CONFIGURATION),
    Field("execution", TELEMETRY_EXECUTION),
    Field("host", Obj(Field("host_id", DIGEST), Field("host", HOST),
                      Field("end_host_id", DIGEST), Field("same_host", Bool()))),
    Field("corpus", Nullable(Obj(Field("status", Str(enum=("ok", "failed", "timeout",
                                                           "not_run"))),
                                 Field("runs", Int(10, 10000)), Field("seed", Int(0, 2 ** 31 - 1)),
                                 Field("wall_ms", Nullable(DECIMAL)),
                                 Field("log", Nullable(TELEMETRY_LOG)),
                                 # The generated corpus's files, read by the controller.
                                 Field("facts", Nullable(TELEMETRY_CORPUS_FACTS)),
                                 Field("detail", TEXT)))),
    Field("runs", Arr(TELEMETRY_RUN, 0, TELEMETRY_RUNS_MAX)),
    Field("within", Arr(TELEMETRY_WITHIN, 0, 4096)),
    Field("comparisons", Arr(TELEMETRY_COMPARISON, 0, 1024)),
    Field("status", Str(enum=("complete", "partial", "failed"))),
    Field("verdict_effect", Const("none")), Field("enforcement", Const("advisory")))


def telemetry_configuration_digest(configuration):
    return canonical_digest(dict((key, value) for key, value in configuration.items()
                                 if key != "digest"))


def validate_telemetry_result(doc, where="result.json"):
    """A telemetry result's identities and accounting: its configuration digest, its host
    observed the same at both ends to call itself one host, every planned run present once in
    its alternating position, its status following from them, and every comparison's figures
    following from its rounds. The raw documents are checked again by `telemetrylab.verify`."""
    TELEMETRY_RESULT.validate(doc, where)
    configuration = doc["configuration"]
    require(configuration["digest"] == telemetry_configuration_digest(configuration),
            where + ".configuration.digest: the digest of the configuration")
    host = doc["host"]
    require(host["host_id"] == host_id(host["host"]), where + ".host.host_id: the host's digest")
    require(host["same_host"] == (host["host_id"] == host["end_host_id"]),
            where + ".host.same_host: exactly when the host was observed the same at both ends")
    require(doc["started_unix_ms"] <= doc["ended_unix_ms"], where + ": ordered instants")
    planned = set((experiment, index, side) for experiment in configuration["experiments"]
                  for index in range(configuration["rounds"]) for side in SIDES)
    seen = set()
    for index, run in enumerate(doc["runs"]):
        at = "%s.runs[%d]" % (where, index)
        key = (run["experiment"], run["round"], run["side"])
        require(key in planned and key not in seen, at + ": a planned run, once")
        seen.add(key)
        require(run["position"] == (SIDES.index(run["side"]) + run["round"]) % 2,
                at + ".position: baseline first in even rounds, candidate first in odd ones")
        require(run["seed"] == configuration["seed"] + run["round"],
                at + ".seed: the configuration's seed plus the round, the same for both sides")
        ok = run["status"] == "ok"
        require(ok == (run["document"] is not None) and ok == (run["judgment"] is not None),
                at + ": exactly a run that succeeded holds its document and its judgment")
        require(run["measurement_refusal"] is None or run["status"] == "refused_document",
                at + ": measurement refusal evidence belongs only to a refused document")
        require(run["status"] != "not_run" or (run["exit_code"] is None
                                               and run["wall_ms"] is None),
                at + ": a run that did not run has no exit or wall time")
    require(seen == planned, where + ".runs: every planned run, a run that never ran included")
    require(("read_latency" in configuration["experiments"]) == (doc["corpus"] is not None),
            where + ".corpus: exactly a result that measures read latency names its corpus")
    if doc["corpus"] is not None:
        require((doc["corpus"]["runs"], doc["corpus"]["seed"])
                == (configuration["read_latency"]["runs"], configuration["seed"]),
                where + ".corpus: generated at the configuration's scale and seed")
        require((doc["corpus"]["status"] == "ok") == (doc["corpus"]["facts"] is not None),
                where + ".corpus.facts: exactly a generated corpus has its facts")
    execution = doc["execution"]
    require(execution["containment"] == ("pid_namespace" if execution["mode"]
                                         == "linux_container" else "process_group")
            and (execution["container"] is None) == (execution["mode"] == "trusted_local"),
            where + ".execution: a contained result names its container, a trusted local one "
            "none")
    require(doc["trigger"] != "pr" or execution["mode"] == "linux_container",
            where + ".execution: a pull request's candidate is measured only contained")
    require((execution["cleanup"] == "uncertain") == (execution["retained"] is not None),
            where + ".execution: exactly an uncertain cleanup keeps its work")
    for side in SIDES:
        build = doc["binaries"][side]["build"]
        require(build is None or (build["side"], build["source_sha"], build["binary_sha256"])
                == (side, doc["sources"][side + "_sha"], doc["binaries"][side]["sha256"]),
                where + ".binaries.%s.build: the build of this side's source and binary" % side)
    complete = (host["same_host"] and all(run["status"] == "ok" for run in doc["runs"])
                and (doc["corpus"] is None or doc["corpus"]["status"] == "ok")
                and execution["cleanup"] == "confirmed" and not execution["problems"])
    failed = not any(run["status"] == "ok" for run in doc["runs"])
    require(doc["status"] == ("complete" if complete else "failed" if failed else "partial"),
            where + ".status: complete only when every planned run succeeded on one host, "
            "every cleanup was confirmed and nothing protected changed")
    keys = set()
    for index, item in enumerate(doc["comparisons"]):
        at = "%s.comparisons[%d]" % (where, index)
        key = (item["experiment"], item["subject"], item["level"], item["metric"], item["shape"])
        require(key not in keys, at + ": each comparison once")
        keys.add(key)
        if item["state"] == "compared":
            require(item["rounds"] and host["same_host"],
                    at + ": compared only from rounds on one host")
            changes = []
            for entry in item["rounds"]:
                base = parse_decimal(entry["baseline"], at)
                require(base > 0, at + ": a baseline of zero has no relative change")
                change = (parse_decimal(entry["candidate"], at) - base) / base
                require(parse_rational(entry["change"], at) == change,
                        at + ": each change is (candidate - baseline) / baseline")
                changes.append(change)
            changes.sort()
            middle = len(changes) // 2
            median = (changes[middle] if len(changes) % 2
                      else (changes[middle - 1] + changes[middle]) / 2)
            require(item["median_change"] is not None
                    and parse_rational(item["median_change"], at) == median,
                    at + ".median_change: the median of the rounds' changes")
        else:
            require(not item["rounds"] and item["median_change"] is None and item["reason"],
                    at + ": what is not compared says why and claims no figure")
    return doc


TELEMETRY_HISTORY_POINT = Obj(
    Field("result_id", ID), Field("candidate_sha", GIT_SHA), Field("baseline_sha", GIT_SHA),
    Field("started_unix_ms", Int(0, 2 ** 53)),
    Field("state", Str(enum=("compared", "incompatible", "missing"))),
    Field("median_change", Nullable(RATIONAL)))
TELEMETRY_HISTORY = Obj(
    Field("schema", Const(TELEMETRY_HISTORY_SCHEMA)),
    # The durable root's bounds (ADR-0215): bytes and results it may hold, and how long a
    # result is kept, when bounded.
    Field("limits", Obj(Field("bytes_max", Int(1 << 20, 1 << 40)),
                        Field("results_max", Int(1, 4096)),
                        Field("retention_days", Nullable(Int(1, 3650))))),
    Field("used_bytes", COUNT),
    Field("results", Arr(Obj(Field("result_id", ID), Field("bundle_sha256", DIGEST),
                             Field("page", REL_PATH), Field("authority_path", REL_PATH),
                             Field("trigger", Str(enum=("pr", "main_push", "nightly", "release",
                                                        "manual"))),
                             Field("profile", Str(enum=TELEMETRY_PROFILES)),
                             Field("mode", Str(enum=TELEMETRY_MODES)),
                             Field("started_unix_ms", Int(0, 2 ** 53)),
                             Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
                             Field("authority_sha", GIT_SHA),
                             Field("status", Str(enum=("complete", "partial", "failed"))),
                             Field("group_id", DIGEST)), 0, 4096)),
    # A group is one configuration under one measurement authority, in one execution mode and
    # image, on one host: only its results are ever placed in one series.
    Field("groups", Arr(Obj(
        Field("group_id", DIGEST), Field("configuration_digest", DIGEST),
        Field("authority_digest", DIGEST), Field("host_id", DIGEST),
        Field("mode", Str(enum=TELEMETRY_MODES)), Field("image_id", Nullable(SHORT_TEXT)),
        Field("results", Arr(ID, 1, 4096, unique=True)),
        Field("series", Arr(Obj(Field("experiment", Str(enum=TELEMETRY_EXPERIMENTS)),
                                Field("subject", SHORT_TEXT),
                                Field("level", Str(enum=TELEMETRY_LEVEL_NAMES)),
                                Field("metric", Str(enum=TELEMETRY_METRICS)),
                                Field("shape", Nullable(Str(enum=("json", "stream-json")))),
                                Field("points", Arr(TELEMETRY_HISTORY_POINT, 1, 4096))),
                            0, 1024))), 0, 4096)),
    # What this write removed for the root's bounds, and every input it refused.
    Field("pruned", Arr(Obj(Field("result_id", ID), Field("reason", TEXT)), 0, 4096)),
    Field("refused", Arr(Obj(Field("input", Str(max_len=1024)), Field("reason", TEXT)),
                         0, 4096)))


def telemetry_group_id(configuration_digest, authority_digest, host_id, mode, image_id):
    return canonical_digest({"configuration_digest": configuration_digest,
                             "authority_digest": authority_digest, "host_id": host_id,
                             "mode": mode, "image_id": image_id})


def validate_telemetry_history(doc, where="history.json"):
    TELEMETRY_HISTORY.validate(doc, where)
    groups = dict((group["group_id"], group) for group in doc["groups"])
    require(len(groups) == len(doc["groups"]), where + ".groups: each group once")
    ids = [item["result_id"] for item in doc["results"]]
    require(len(ids) == len(set(ids)), where + ".results: each result once")
    require(len(ids) <= doc["limits"]["results_max"]
            and doc["used_bytes"] <= doc["limits"]["bytes_max"],
            where + ": within the root's bounds")
    for item in doc["results"]:
        require(item["group_id"] in groups and item["result_id"]
                in groups[item["group_id"]]["results"],
                where + ".results: each result in its group")
        require(item["mode"] == groups[item["group_id"]]["mode"],
                where + ".results: each result in a group of its execution mode")
        require(item["authority_path"] == "authorities/" + groups[
            item["group_id"]]["authority_digest"].split(":")[1],
            where + ".results: exact retained authority path")
    for group in doc["groups"]:
        require(group["group_id"] == telemetry_group_id(
            group["configuration_digest"], group["authority_digest"], group["host_id"],
            group["mode"], group["image_id"]),
            where + ".groups: a group is identified by its configuration, authority, execution "
            "mode, image and host")
        for series in group["series"]:
            require(all(point["result_id"] in group["results"] for point in series["points"]),
                    where + ".groups: a series holds only its group's results")
    return doc


# The telemetry collector (ADR-0215): an operator's configuration of the trusted collection
# lane, separate from the archive collector, and what one cycle did.
TELEMETRY_COLLECTOR_SCHEMA = "af.perf-telemetry-collector/1"
TELEMETRY_COLLECTION_SCHEMA = "af.perf-telemetry-collection/2"
TELEMETRY_COLLECTION_POSITION_SCHEMA = "af.perf-telemetry-collection-position/1"
TELEMETRY_WORKFLOW = "performance-telemetry.yml"
ABSOLUTE_PATH = Str(pattern=r"^/[^\x00]*$", min_len=2, max_len=4096)
TELEMETRY_COLLECTOR = Obj(
    Field("schema", Const(TELEMETRY_COLLECTOR_SCHEMA)),
    Field("repository", REPOSITORY), Field("workflow", Const(TELEMETRY_WORKFLOW)),
    Field("api_url", Str(pattern=r"^https://[A-Za-z0-9.-]+(?::[0-9]{1,5})?(?:/api/v3)?$",
                         max_len=256)),
    Field("token_env", Str(pattern=r"^[A-Z_][A-Z0-9_]{0,63}$")),
    Field("root", ABSOLUTE_PATH), Field("authority_repository", ABSOLUTE_PATH),
    Field("trusted_refs", Arr(Str(pattern=r"^refs/(?:remotes/[A-Za-z0-9._-]+|heads|tags)/"
                                  r"[A-Za-z0-9._/-]{1,200}$", max_len=256), 1, 16,
                              unique=True)),
    Field("lookback_days", Int(1, 89)), Field("collection_seconds", Int(60, 3600)),
    Field("runs_max", Int(1, 1000)), Field("artifact_bytes_max", Int(1 << 20, 2 << 30)),
    Field("bytes_max", Int(1 << 20, 1 << 40)), Field("results_max", Int(1, 4096)),
    Field("retention_days", Int(1, 3650)))


def validate_telemetry_collector(doc, where="telemetry-collector.toml"):
    TELEMETRY_COLLECTOR.validate(doc, where)
    require(doc["artifact_bytes_max"] <= doc["bytes_max"],
            where + ".artifact_bytes_max: within the root's bytes")
    require(not (doc["authority_repository"] + "/").startswith(doc["root"] + "/")
            and not (doc["root"] + "/").startswith(doc["authority_repository"] + "/"),
            where + ": the trusted repository lies apart from the durable root")
    return doc


TELEMETRY_COLLECTION_POSITION = Obj(
    Field("schema", Const(TELEMETRY_COLLECTION_POSITION_SCHEMA)),
    Field("repository", REPOSITORY), Field("workflow_id", ACTIONS_ID),
    Field("after_run_id", ACTIONS_ID), Field("through_run_id", ACTIONS_ID))


def validate_telemetry_collection_position(doc, where="collection-position.json"):
    TELEMETRY_COLLECTION_POSITION.validate(doc, where)
    require(doc["after_run_id"] <= doc["through_run_id"],
            where + ": the attempted ID is within the rotation's upper ID")
    return doc


TELEMETRY_COLLECTION_OUTCOMES = ("collected", "already_collected", "no_result", "expired",
                                 "refused")
TELEMETRY_COLLECTION = Obj(
    Field("schema", Const(TELEMETRY_COLLECTION_SCHEMA)),
    Field("repository", REPOSITORY), Field("workflow_id", Nullable(ACTIONS_ID)),
    Field("started_unix_ms", Int(0, 2 ** 53)), Field("ended_unix_ms", Nullable(Int(0, 2 ** 53))),
    Field("status", Str(enum=("ok", "failed"))), Field("detail", TEXT),
    Field("runs", Arr(Obj(
        Field("run_id", ACTIONS_ID), Field("attempt", Int(1, 1000)),
        Field("event", Str(max_len=64)), Field("conclusion", Nullable(Str(max_len=64))),
        Field("outcome", Str(enum=TELEMETRY_COLLECTION_OUTCOMES)),
        Field("artifact", Nullable(Obj(Field("id", ACTIONS_ID), Field("name", SHORT_TEXT),
                                       Field("size", COUNT),
                                       Field("sha256", Nullable(DIGEST))))),
        Field("result_id", Nullable(ID)), Field("reason", TEXT)), 0, 4096)),
    Field("pruned", Arr(Obj(Field("result_id", ID), Field("reason", TEXT)), 0, 4096)),
    Field("results", Nullable(COUNT)), Field("used_bytes", Nullable(COUNT)))


def validate_telemetry_collection(doc, where="collection.json"):
    TELEMETRY_COLLECTION.validate(doc, where)
    require(doc["ended_unix_ms"] is None or doc["started_unix_ms"] <= doc["ended_unix_ms"],
            where + ": ordered observed instants")
    require(doc["ended_unix_ms"] is not None or doc["status"] == "failed",
            where + ": no terminal time means incomplete")
    require((doc["results"] is None) == (doc["used_bytes"] is None),
            where + ": result and byte accounting are known together")
    require(doc["results"] is not None or doc["status"] == "failed",
            where + ": unknown accounting is incomplete")
    for index, item in enumerate(doc["runs"]):
        at = "%s.runs[%d]" % (where, index)
        require((item["outcome"] in ("collected", "already_collected"))
                == (item["result_id"] is not None),
                at + ": exactly a collected result names its result")
        require(item["outcome"] != "refused" or item["reason"], at + ": a refusal says why")
    require(doc["status"] == ("failed" if doc["detail"] or any(
        item["outcome"] == "refused" for item in doc["runs"]) else "ok"),
        where + ".status: failed exactly when the cycle ended early or refused an artifact")
    return doc


CONTRACTS = {
    "enforcement": (ENFORCEMENT_SCHEMA, ENFORCEMENT, validate_enforcement,
                    "Reviewed SDLC enforcement policy. Required mode accepts only an eligible "
                    "full measured primary under an active, calibrated authority."),
    "suite": (suite.SCHEMA, suite.SUITE, suite.validate,
              "The reviewed performance registry fixtures/performance/suite.toml, as parsed TOML "
              "(ADR-0162). Workload, policy, recipe and environment stay separate; every "
              "reference, parameter domain and resource bound is also checked by the reader."),
    "run": (RUN_SCHEMA, RUN, validate_run,
            "A frozen comparison plan, run.json: resolved identities, each source's Cargo.lock "
            "digest read from its Git objects, authority digests, cases, seeded AB/BA schedule, "
            "sentinels and limits. The reader recomputes the plan digest, K, every rank, order "
            "and shard assignment; nothing rewrites a plan."),
    "builds": (BUILDS_SCHEMA, BUILDS, validate_builds,
               "Per-side build provenance of one run, builds.json (ADR-0166, ADR-0169): the "
               "isolation (a trusted host process, or a pinned build image without network), "
               "source and its "
               "verified embedded commit, the authority overlay, every adapter outcome in "
               "priority order, both sides' adapter bindings, binary, artifact and Cargo.lock "
               "hashes, the verified toolchain and linker, sanitized argv and environment, "
               "bounded logs, budgets, cleanup, and every recipe setting each side and the "
               "shared supervisor were built with."),
    "sample": (SAMPLE_SCHEMA, SAMPLE, validate_sample,
               "One record of samples.jsonl: one execution, failed or not, in its planned slot, "
               "with exact decimal metrics, oracle outcome and log hashes; at most 64 KiB."),
    "shard": (SHARD_SCHEMA, SHARD, validate_shard,
              "One shard's terminal record, shard.json: status, provenance, host identity, "
              "execution with the machine coordination lock it held, every side-written seed "
              "with its digest and outcome (ADR-0169), and the count, size and hash of its "
              "samples."),
    "manifest": (MANIFEST_SCHEMA, MANIFEST, validate_manifest,
                 "A bundle manifest: every file's sorted relative path, size and SHA256. Its own "
                 "SHA256 identifies the bundle; readers refuse missing, extra and symlinked "
                 "files."),
    "verdict": (VERDICT_SCHEMA, VERDICT, validate_verdict,
                "The laboratory's verdict of one run under paired_median_tolerance_v2: exact "
                "rational intervals, per-case classification, health, budgets, each shard's "
                "terminal status and record coverage, and the aggregate result (pass 0, fail 3, "
                "incomplete 4)."),
    "profiles": (PROFILES_SCHEMA, PROFILES, validate_profiles,
                 "One diagnostic child run, profiles.json (ADR-0174): its link to the parent "
                 "comparison, which never changes that verdict; the automatic or explicit "
                 "selection; sources, diagnostic recipe and builds; budgets and their use; the "
                 "explicit execution mode, its containment and, contained, its container and "
                 "verified scratch bound (ADR-0176); every tool's identity or why it is "
                 "unavailable; each CPU capture of a probe region or a CLI process with its "
                 "region, event, unwinder, effective frequency, samples, losses, unknown frames "
                 "and symbols; baseline, candidate and both-direction difference flamegraphs, "
                 "original and sanitized; phase traces with their region's coverage and "
                 "measured overhead; counters as explanatory proxies; and every omission with "
                 "its reason and reproduction. A capture of fresh processes lists its own "
                 "files and names its repetition index for the per-repetition ones."),
    "repetitions": (REPETITIONS_SCHEMA, diagcontracts.REPETITIONS, validate_repetitions,
                    "The repetition index of one capture of fresh processes, repetitions.json: "
                    "every repetition once and in order with its raw recording, its host "
                    "observer record and the region time it contributed, and the boundary that "
                    "time was measured at (the gated command's exec to its exit decoded from "
                    "the trusted host recording, the supervised child's wall, or samply's "
                    "driver wall). Every listed file is bound by its size and SHA256."),
    "trace": (TRACE_SCHEMA, diagcontracts.TRACE, validate_trace,
              "The metadata-only phase spans a perf-trace diagnostic build exports (ADR-0174): "
              "the process, the recorder's monotonic clock and wall epoch, its capacity, the "
              "spans dropped and still open, and each kept span's identity, parent, closed "
              "phase name, thread and interval. No payload, path, prompt or credential."),
    "diagnostics": (DIAGNOSTICS_SCHEMA, diagcontracts.DIAGNOSTICS, validate_diagnostics,
                    "fixtures/performance/diagnostics.toml as parsed TOML (ADR-0174): diagnostic "
                    "case selection, capture defaults within the hard caps, the declared event "
                    "and DWARF alternative, trace overhead target, counter cases, resource "
                    "limits, diagnostic recipes derived from verdict recipes, and every external "
                    "tool's pin, version provenance and provisioning instructions."),
    "cohort": (COHORT_SCHEMA, COHORT, validate_cohort,
               "A predeclared calibration cohort: authority, policy digest, cases with N and K, "
               "health rule, CPU groups, schedule, target formulas and every member."),
    "calibration-parameters": (CALIBRATION_SCHEMA, CALIBRATION, validate_calibration,
                               "Sealed calibration inputs per case and CPU group: A0, "
                               "e = max(r, a/A0), the power target and loop iterations, fixed "
                               "before slowdown trials."),
    "readiness": (READINESS_SCHEMA, READINESS, validate_readiness,
                  "A readiness calculation over every declared cohort member; tallies and the "
                  "decision are recomputed from member outcomes, never trusted as written."),
    "activation": (ACTIVATION_SCHEMA, ACTIVATION, validate_activation,
                   "fixtures/performance/activation.toml as parsed TOML (ADR-0183, ADR-0184): the "
                   "reviewed binding of an immutable authority SHA, policy digest, the cohort, "
                   "parameter and readiness records by path and bytes, every original run, the "
                   "calibrated environment, recipe and CPU groups, and the admission, a bridge "
                   "with its execution evidence record."),
    "activation-verification": (ACTIVATION_VERIFICATION_SCHEMA, ACTIVATION_VERIFICATION,
                                validate_activation_verification,
                                "One verified activation (ADR-0183, ADR-0184): whether trusted "
                                "control code fetched and recomputed the cohort's retained "
                                "evidence or checked the reviewed records locally, the rehashed "
                                "records, original runs and tallies, the calibrated and "
                                "uncalibrated scope, an equivalent adapter's protected diff and "
                                "verified execution evidence, and what only an owner can "
                                "establish."),
    "equivalence": (EQUIVALENCE_SCHEMA, EQUIVALENCE, validate_equivalence,
                    "An equivalent-adapter bridge's execution evidence (ADR-0184): the golden "
                    "check of the calibrated and the bridged product sources and the post-merge "
                    "A/A, each recomputed from a retained run of the bridged authority with its "
                    "identities, adapters, probe binaries, observed groups, completeness and "
                    "case outcomes, its reasons re-derived by the reader; owner approval of "
                    "semantic equivalence stays separate."),
    "cost": (COST_SCHEMA, COST, validate_cost,
             "A cost demonstration (ADR-0183, ADR-0184): every retained pull-request and "
             "same-source run, failed attempts included, with its observed cold or warm build "
             "cache, source, plan, bundle, build and binary identities, build and per-shard "
             "sampling time, per-case sample counts and wall time, artifact bytes and failures; "
             "each group's completed and failed runs, the median and maximum of its completed "
             "runs, and whether five distinct completed measured comparisons demonstrate it."),
    "shipping": (SHIPPING_SCHEMA, SHIPPING, validate_shipping,
                 "A shipping-budget demonstration (ADR-0183, ADR-0184) under one reviewed suite: "
                 "every run once, with its bundle and binary identity; every planned measured "
                 "candidate slot of each budget's instances on each named environment as a "
                 "sample, a failure or missing, samples checked strictly against the limit, the "
                 "hosts and incomplete runs, and whether at least the minimum complete samples "
                 "met every budget."),
    "kernel-inventory": (KERNEL_INVENTORY_SCHEMA, KERNEL_INVENTORY, validate_kernel_inventory,
                         "The staged inventory of a guarded kernel experiment's protected "
                         "harness (ADR-0185): every protected file's path, size and SHA-256 and "
                         "the digest the literal bootstrap recomputes from them before any "
                         "candidate import or build, with the objective metric and the pending "
                         "status of the guarded inner metric."),
    "kernel-receipts": (KERNEL_RECEIPTS_SCHEMA, KERNEL_RECEIPTS, validate_kernel_receipts,
                        "What the guarded kernel experiments' native integration exports "
                        "(ADR-0186, ADR-0187): each Task's exit code and typed kernel result, "
                        "which must name exactly the retained outputs, every output's complete "
                        "typed envelope whose content and artifact IDs the reader recomputes "
                        "with the kernel's canonical JSON and domains, and the public stdout "
                        "and stderr of every measured repetition with its content ID, SHA-256 "
                        "and size, whole (its ID recomputed) or as its first and last 32 KiB "
                        "(its ID and hash the exporter's, the omitted middle never rehashed). "
                        "Evidence for inspection, never a decision input."),
    "probe": (PROBE_SCHEMA, PROBE, validate_probe,
              "The native perf-probe's control message: case, contract, fixture and parameter "
              "identity, metrics and the correctness facts checked against golden entries."),
    "probe-request": (PROBE_REQUEST_SCHEMA, PROBE_REQUEST, validate_probe_request,
                      "The native perf-probe's request: one declared case contract, its exact "
                      "parameter assignment and repetitions, and an empty absolute scratch "
                      "directory; at most 64 KiB."),
    "golden": (GOLDEN_SCHEMA, GOLDEN, validate_golden,
               "fixtures/performance/golden/native.json: the native oracle's golden entries, "
               "derived by perflab.reference from reference bytes and explicit semantic facts, "
               "never from a judged execution (ADR-0163)."),
    "adapters": (ADAPTERS_SCHEMA, ADAPTERS, validate_adapters,
                 "fixtures/performance/adapters.toml as parsed TOML (ADR-0166): the reviewed "
                 "native probe adapters, each a fixed-priority overlay of authority example "
                 "files into a measured source, at most four per case contract."),
    "supervisor-request": (SUPERVISOR_REQUEST_SCHEMA, SUPERVISOR_REQUEST,
                           validate_supervisor_request,
                           "The perf-supervisor's closed request: exact argv, environment, "
                           "cwd, stdin, deadline, output caps and containment of one sample; "
                           "canonical JSON of at most 64 KiB (ADR-0166)."),
    "supervisor-result": (SUPERVISOR_RESULT_SCHEMA, SUPERVISOR_RESULT,
                          validate_supervisor_result,
                          "The perf-supervisor's result line: termination, how the exit was "
                          "observed, the monotonic child interval of an exit observed unreaped "
                          "and the overheads kept apart from it, capped output counts, pattern "
                          "counts and in-order number sequences over the whole stdout, the "
                          "lifecycle it could establish with counts that support its status, "
                          "scratch, rusage and cgroup observations, each present or unavailable "
                          "with a reason (ADR-0167)."),
    "capabilities": (CAPABILITIES_SCHEMA, CAPABILITIES, validate_capabilities,
                     "What this host can execute: actual tool identities, trusted local and "
                     "Linux container capability, the bounded-scratch backend with the "
                     "evidence of its enforcement fill, and each registry environment, with "
                     "the reason for anything unavailable (ADR-0167)."),
    "cli": (CLI_SCHEMA, CLI, validate_cli,
            "fixtures/performance/cli.toml as parsed TOML (ADR-0168, ADR-0169): per CLI adapter "
            "and process case instance, the actual AF binary's argv, its untimed fixture setup "
            "(a private copy of a side-written seed included) and the independent oracle rules "
            "its output must satisfy, or why it is pending; and each seed's writer."),
    "tui-open": (TUI_OPEN_SCHEMA, TUI_OPEN, validate_tui_open,
                 "The PTY driver's one-line report: launch-to-browser and launch-to-first-"
                 "output intervals in milliseconds and the child exit code."),
    "schedule": (SCHEDULE_SCHEMA, SCHEDULE, validate_schedule,
                 "One Actions event's continuous schedule (ADR-0177, ADR-0180): the event "
                 "resolved once to full SHAs with its pull request metadata, whether it is "
                 "trusted and which executor it may use, advisory enforcement, each comparison "
                 "planned from the trusted control (or why it is unavailable or refused), the "
                 "shard matrix and concurrency every later job reads, and the confirmed release "
                 "claim a release handoff measures."),
    "release-claim": (RELEASE_CLAIM_SCHEMA, RELEASE_CLAIM, validate_release_claim,
                      "The immutable claim a release handoff writes before any sample "
                      "(ADR-0180): the deduplication identity (upstream run, tag and SHA, never "
                      "the attempt) and its canonical key, the verified upstream workflow and "
                      "publication attempt, the published release, the claiming run and its "
                      "laboratory runs, and an explicit remeasurement's parent claim."),
    "release-handoff": (RELEASE_HANDOFF_SCHEMA, RELEASE_HANDOFF, validate_release_handoff,
                        "One release handoff decision (ADR-0180): the verified upstream run "
                        "and every attempt's publish and pin outcomes, the published release, "
                        "the identity and claim key, each claim of its history and why it is "
                        "valid or not, and whether this run claims, measures, links the first "
                        "claim's retained result, or why it runs nothing; always advisory."),
    "build-pack": (BUILD_PACK_SCHEMA, BUILD_PACK, validate_build_pack,
                   "One run's build, packed once by its credential-free build job for its shards "
                   "(ADR-0177): the archive's identity and every member's path, size, hash and "
                   "whether it is executable, restored only after its bytes are verified."),
    "report": (REPORT_SCHEMA, REPORT, validate_report,
               "One run's offline report (ADR-0172): its recomputed verdict, whether and why "
               "not it satisfies the required check, exact identity and provenance, every "
               "planned slot as a raw point, shard health and seeds, verified evidence "
               "downloads, the validated diagnostic child runs linked to it with each case's "
               "profile, trace and telemetry items or omissions (ADR-0177, ADR-0214), and "
               "reproduction commands "
               "that keep the frozen configuration and state their limitations."),
    "archive": (ARCHIVE_SCHEMA, ARCHIVE, validate_archive,
                "An archive root's archive.json (ADR-0172): its capacity, at most 1 TiB, and "
                "when it was created."),
    "archive-event": (ARCHIVE_EVENT_SCHEMA, ARCHIVE_EVENT, validate_archive_event,
                      "One immutable entry of an archive's append-only index (ADR-0172): an "
                      "admitted run and its content-addressed members, an explicit retention "
                      "record, a pin, a supersession or a reference-traced garbage "
                      "collection."),
    "archive-collection": (ARCHIVE_COLLECTION_SCHEMA, ARCHIVE_COLLECTION,
                           validate_archive_collection,
                           "One collection attempt of an archive (ADR-0172): its source, "
                           "outcome, the run it collected, capacity when it was exhausted and "
                           "every transport artifact it saw with its expiry."),
    "archive-status": (ARCHIVE_STATUS_SCHEMA, ARCHIVE_STATUS, validate_archive_status,
                       "An archive's operational status (ADR-0172): capacity, usage, runs, "
                       "collection lag and the last successful collection, transport expiry "
                       "risk and health problems."),
    "transport": (TRANSPORT_SCHEMA, TRANSPORT, validate_transport,
                  "The declared format of one Actions transport artifact (ADR-0172): which "
                  "run, a run or shard bundle, and the transported views it carries as data."),
    "history": (HISTORY_SCHEMA, HISTORY, validate_history,
                "Compatible history across runs (ADR-0172): every run with its source, result "
                "and retention, groups of one case under one workload, fixture, policy, "
                "recipe, environment and provenance identity with each run's point and seed "
                "provenance, how incompatible groups of a case differ, and what could not be "
                "shown."),
    "telemetry-lab": (TELEMETRY_LAB_SCHEMA, TELEMETRY_LAB, validate_telemetry_lab,
                      "fixtures/performance/telemetry.toml as parsed TOML (ADR-0214): the "
                      "telemetry experiments, each exactly a bench script of design section 12 "
                      "with its document, the product schema it is read through and its levels, "
                      "the inputs its measurement authority is identified by, and each "
                      "trigger's bounded profile of rounds, pairs, scale, corpus size, timeout, "
                      "work bytes and retention."),
    "telemetry-binding": (TELEMETRY_BINDING_SCHEMA, TELEMETRY_BINDING,
                          validate_telemetry_binding,
                          "Immutable Actions run/attempt planning receipt: original event "
                          "commit and branch head, planned baseline/candidate/control. "
                          "Retained separately from untrusted measurement results."),
    "telemetry-result": (TELEMETRY_RESULT_SCHEMA, TELEMETRY_RESULT, validate_telemetry_result,
                         "One telemetry experiment run (ADR-0214, ADR-0215): the baseline and "
                         "candidate sources, binaries and builds, the measurement authority "
                         "both ran under, the effective configuration and its digest, the "
                         "execution mode, container, bounds, cleanup and any protected state "
                         "that changed, the generated corpus's facts, the host observed at both "
                         "ends, every planned run with its alternating position, seed, status, "
                         "raw document, capped log and the script's own judgment, each side's "
                         "within-binary figures by level, and each same-host cross-revision "
                         "comparison by level, metric and shape, or why it is incompatible or "
                         "missing. Advisory; never a verdict."),
    "telemetry-owner": (TELEMETRY_OWNER_SCHEMA, TELEMETRY_OWNER, validate_telemetry_owner,
                        "Trusted bounded cleanup authority, outside candidate mounts; pending "
                        "launches and filesystem work must be settled before scratch removal."),
    "telemetry-build": (TELEMETRY_BUILD_SCHEMA, TELEMETRY_BUILD, validate_telemetry_build,
                        "How one side's AF binary of a telemetry experiment was built "
                        "(ADR-0215): its side, source commit, the binary's SHA-256, the "
                        "build image pinned by digest in its canonical form (a local image "
                        "ID or a manifest digest, ADR-0170), the toolchain identities and "
                        "the command."),
    "telemetry-collector": (TELEMETRY_COLLECTOR_SCHEMA, TELEMETRY_COLLECTOR,
                            validate_telemetry_collector,
                            "The trusted telemetry collector's configuration as parsed TOML "
                            "(ADR-0215): the repository and its Performance telemetry "
                            "workflow, the credential's environment name, the durable root, "
                            "the trusted repository and refs its measurement authorities are "
                            "read from, and the cycle's time, run, artifact, byte, result and "
                            "retention bounds."),
    "telemetry-collection": (TELEMETRY_COLLECTION_SCHEMA, TELEMETRY_COLLECTION,
                             validate_telemetry_collection,
                             "One telemetry collection cycle (ADR-0215): every completed run "
                             "it attempted, by Actions run and attempt, with its artifact, its "
                             "SHA-256, the result it kept or why it kept none, what it pruned "
                             "for the root's bounds, and the root's results and bytes."),
    "telemetry-collection-position": (
        TELEMETRY_COLLECTION_POSITION_SCHEMA, TELEMETRY_COLLECTION_POSITION,
        validate_telemetry_collection_position,
        "A bounded telemetry scheduling position in a locked history root: rotate after "
        "this attempted run ID up to a fixed upper run ID, wrap at the end, and retry all "
        "outcomes within lookback without new arrivals extending a rotation. "
        "Bound to one repository and workflow; never evidence of collection success."),
    "telemetry-history": (TELEMETRY_HISTORY_SCHEMA, TELEMETRY_HISTORY,
                          validate_telemetry_history,
                          "Compatible history of telemetry results (ADR-0214, ADR-0215): the "
                          "durable root's bounds and use, every result verified under its exact "
                          "measurement authority once, groups of one configuration under one "
                          "authority, execution mode and image on one host, each group's "
                          "series by experiment, subject, level, metric and shape, what was "
                          "pruned and every refused input with its reason."),
    "plan-refusal": (REFUSAL_SCHEMA, REFUSAL, validate_refusal,
                     "Why `plan` froze nothing: invalid input (exit 2) or an unavailable "
                     "baseline, authority or reference (exit 4)."),
}


# Earlier versions a reader still admits beside the current one, by contract and schema ID:
# (node, validator, description). Only an append-only original journal has them (ADR-0266);
# writers write only the current version and every other earlier version fails closed.
READABLE = {}


def schema_version(name, schema_id=None):
    """The version number of contract `name`, the integer after the slash of its schema ID
    (the current one, or the readable earlier `schema_id`)."""
    return int((schema_id or CONTRACTS[name][0]).rsplit("/", 1)[1])


def schema_file(name, schema_id=None):
    return "performance-%s-v%d.json" % (name, schema_version(name, schema_id))


def schema_documents():
    out = {}
    for name, (schema_id, node, _, description) in sorted(CONTRACTS.items()):
        versions = [(schema_id, node, description)] + [
            (earlier, entry[0], entry[2]) for earlier, entry in sorted(READABLE.get(name, {}).items())]
        for version_id, version_node, text in versions:
            version = schema_version(name, version_id)
            out[schema_file(name, version_id)] = json_schema(
                "urn:af:schema:performance-%s:%d" % (name, version), version_id, text, version_node)
    return out


def read_json(data, name, where=None):
    """Parse and validate one JSON document of contract `name` from bytes."""
    where = where or name
    schema_id, _, validate, _ = CONTRACTS[name]
    doc = loads_strict(data, where, limits.MAX_JSON_BYTES)
    require(isinstance(doc, dict), where + ": expected a JSON object")
    earlier = READABLE.get(name, {}).get(doc.get("schema"))
    if earlier is not None:
        return earlier[1](doc, where)
    require(doc.get("schema") == schema_id, where + ".schema: expected %s, found %r"
            % (schema_id, doc.get("schema")))
    return validate(doc, where)


def read_file(path, name):
    with open(path, "rb") as stream:
        data = stream.read(limits.MAX_JSON_BYTES + 1)
    return read_json(data, name, str(path))


def write_bytes(doc, name):
    """Validate `doc` and return the canonical bytes a writer stores."""
    validate = CONTRACTS[name][2]
    validate(doc)
    return document_bytes(doc)


# Register separately named local contracts after the shared measurement contracts.
from . import executorcontracts as _executorcontracts
import sys as _sys
_executorcontracts.register(_sys.modules[__name__])
from . import nightlycontracts as _nightlycontracts
_nightlycontracts.register(_sys.modules[__name__])
