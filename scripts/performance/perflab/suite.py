"""The reviewed registry `fixtures/performance/suite.toml` (`af.perf-suite/4`).

It separates workload (cases, parameters, oracles, timing and cache scope), policy
(criticality, tolerances, budgets, samples), recipe (build and instrument) and environment
(capabilities and resources). Loading validates every cross-reference and expands every
parameter domain before anything is planned. Version 2 added the native probe declaration and
each library case's probe contract (ADR-0163). Version 3 names the seed a library case's probe
reads, speaks `af.perf-probe-request/2`, and lets a recipe pin its build image by either
canonical digest form (ADR-0170); earlier versions are refused.
Version 4 admits a reviewed driver-reported process interval for the browser frame (ADR-0259).
"""
from fractions import Fraction
import itertools

from . import limits
from . import images
from .spec import (AnyOf, Arr, Bool, Const, DECIMAL, Field, ID, Int, Map, Obj, REL_PATH,
                   SHORT_TEXT, Str, TEXT, Tagged, UNIT, tagged_variant)
from .stats import ALGORITHM, HEALTH_RULE
from .values import (ContractError, ID_PATTERN, canonical_digest, is_id, parse_decimal,
                     require, sha256_bytes)
from . import tomlfile

SCHEMA = "af.perf-suite/4"
PROBE_REQUEST_SCHEMA = "af.perf-probe-request/2"
PROBE_MESSAGE_SCHEMA = "af.perf-probe/1"
TIERS = ("quick", "nightly", "release")
CAPABILITY_STATES = ("supported", "not_yet_executable")
DEFAULT_RELATIVE = Fraction(5, 100)
DEFAULT_ABSOLUTE_MS = Fraction(1)
TASK_LEASE_MS = 60000
ARTIFACT_ENVELOPE_CAP_BYTES = 8 * 1024 * 1024

_TIER_KEY = r"^(?:quick|nightly|release)$"
_AF_ENV_KEY = r"^AF_[A-Z0-9_]{1,62}$"

CAPABILITY = Str(enum=CAPABILITY_STATES)
PENDING_NOTE = Str(max_len=512)

METRIC_DEF = Obj(
    Field("unit", UNIT),
    Field("direction", Str(enum=("lower_is_better", "higher_is_better"))),
    Field("description", TEXT),
)

POLICY = Obj(
    Field("metric", ID),
    Field("relative_tolerance", DECIMAL),
    Field("absolute_tolerance", DECIMAL),
    Field("calibration", Str(enum=("initial", "calibrated"))),
    Field("calibration_cohort", ID, required=False),
    Field("description", TEXT),
)

BUDGET = Obj(
    Field("metric", ID),
    Field("limit", DECIMAL),
    Field("unit", UNIT),
    Field("comparison", Const("every_sample_strictly_below")),
    Field("minimum_samples", Int(1, limits.PAIRS_MAX)),
    Field("state", Str(enum=("pending_calibration", "enabled"))),
    Field("tiers", Arr(Str(enum=TIERS), 1, 3, unique=True)),
    Field("environments", Arr(ID, 1, 16, unique=True)),
    Field("recipes", Arr(ID, 1, 16, unique=True)),
    Field("description", TEXT),
)

FIXTURE = Obj(
    Field("kind", Str(enum=("typed_envelope", "canonical_small", "event_log", "file_tree",
                            "cli_state", "cli_task", "cli_registry"))),
    Field("description", TEXT),
    Field("serialized_envelope_bytes", Int(1, ARTIFACT_ENVELOPE_CAP_BYTES), required=False),
    Field("size_rule", Str(enum=("exact", "approximate")), required=False),
    Field("events", Int(1, 10 ** 7), required=False),
    Field("files", Int(1, 10 ** 7), required=False),
    Field("file_bytes", Int(1, 1 << 20), required=False),
    Field("golden", Str(enum=("pending_native_oracle", "available"))),
)

RECIPE = Obj(
    Field("description", TEXT),
    Field("os", Str(enum=("linux", "macos"))),
    Field("target", Str(pattern=r"^[a-z0-9_]+(?:-[a-z0-9_]+){2,3}$", max_len=64)),
    Field("toolchain", Str(pattern=r"^[0-9]+\.[0-9]+\.[0-9]+$", max_len=32)),
    Field("profile", Const("release")),
    Field("builder", Str(enum=("cargo", "cargo_zigbuild"))),
    Field("cargo_zigbuild", Str(pattern=r"^[0-9]+\.[0-9]+\.[0-9]+$", max_len=32), required=False),
    Field("zig", Str(pattern=r"^[0-9]+\.[0-9]+\.[0-9]+$", max_len=32), required=False),
    Field("linker", Str(enum=("system_cc", "zig_cc", "apple_ld"))),
    Field("features", Arr(Str(pattern=r"^[a-z0-9_-]{1,64}$"), 0, 32, unique=True)),
    Field("rustflags", Arr(Str(pattern=r"^[A-Za-z0-9=_,.:+-]{1,128}$"), 0, 32)),
    Field("instrumented", Bool()),
    Field("debug_symbols", Bool()),
    Field("frame_pointers", Bool()),
    Field("container_image", images.REFERENCE, required=False),
    Field("state", CAPABILITY),
    Field("pending", PENDING_NOTE),
)

ENVIRONMENT = Obj(
    Field("description", TEXT),
    Field("os", Str(enum=("linux", "macos"))),
    Field("arch", Str(enum=("x86_64", "aarch64"))),
    Field("libc", Str(enum=("gnu", "musl", "darwin"))),
    Field("runner", Str(enum=("github_hosted", "trusted_local", "dedicated"))),
    Field("runner_label", SHORT_TEXT),
    Field("containment", Str(enum=("linux_container", "trusted_local_process_group"))),
    Field("logical_cpus", Int(1, 256), required=False),
    Field("prefer_separate_physical_cores", Bool(), required=False),
    Field("memory_limit_bytes", Int(64 * limits.MIB, 1024 * limits.GIB), required=False),
    Field("pid_limit", Int(16, 65536), required=False),
    Field("swap", Str(enum=("disabled", "host_default"))),
    Field("network", Str(enum=("disabled", "host_default"))),
    Field("cpu_quota", Str(enum=("none", "declared"))),
    Field("scratch_limit_bytes", Int(64 * limits.MIB, 64 * limits.GIB)),
    Field("state", CAPABILITY),
    Field("pending", PENDING_NOTE),
)

PROFILE = Obj(Field("environment", ID), Field("recipe", ID))

TIER = Obj(
    Field("profiles", Arr(PROFILE, 1, 8)),
    Field("shards", Int(1, limits.SHARDS_MAX)),
    Field("concurrent_jobs", Int(1, limits.CONCURRENT_JOBS_MAX)),
    Field("shard_sampling_minutes", Int(1, limits.LONG_SHARD_SAMPLING_MINUTES_MAX)),
    Field("run_total_minutes", Int(1, limits.LONG_RUN_TOTAL_MINUTES_MAX)),
    Field("description", TEXT),
)

LIMITS = Obj(
    Field("sample_log_bytes", Int(1, limits.SAMPLE_LOG_BYTES_MAX)),
    Field("shard_bundle_bytes", Int(1, limits.SHARD_BUNDLE_BYTES_MAX)),
    Field("build_artifact_bytes", Int(1, limits.BUILD_ARTIFACT_BYTES_MAX)),
    Field("worktree_bytes", Int(1, limits.WORKTREE_BYTES_MAX)),
    Field("free_disk_floor_bytes", Int(limits.FREE_DISK_FLOOR_BYTES, 2 ** 62)),
    Field("build_minutes", Int(1, limits.BUILD_MINUTES_MAX)),
)

ORACLE = Obj(
    Field("kind", Str(enum=("golden_canonical_digest", "golden_cas_digest",
                            "golden_source_manifest", "golden_semantic_projection",
                            "golden_authority_outcome", "cli_version_and_usage_tokens",
                            "cli_task_outcome", "cli_state_fields",
                            "persistence_round_trip"))),
    Field("state", Str(enum=("pending_native_oracle", "pending_cli_fixture", "available"))),
    Field("description", TEXT),
)

TIMING = Tagged("boundary", {
    "child_process_lifetime": tagged_variant(
        "boundary", "child_process_lifetime", Field("description", TEXT)),
    "reported_interval": tagged_variant(
        "boundary", "reported_interval", Field("field", ID), Field("description", TEXT)),
    "library_region": tagged_variant(
        "boundary", "library_region", Field("region", ID),
        Field("setup_excluded", Arr(SHORT_TEXT, 0, 16)), Field("description", TEXT)),
})

CACHE = Obj(
    Field("process", Str(enum=("fresh",))),
    Field("store", Str(enum=("fresh", "not_applicable"))),
    Field("projection", Str(enum=("application_cold", "prewarmed", "by_parameter",
                                  "not_applicable"))),
    Field("os_page_cache", Str(enum=("warm",))),
)

PARAMETER = Tagged("kind", {
    "enum": tagged_variant("kind", "enum", Field("values", Arr(ID, 1, 32, unique=True))),
    "integer": tagged_variant("kind", "integer",
                              Field("values", Arr(Int(0, 10 ** 12), 1, 32, unique=True))),
    "fixture": tagged_variant("kind", "fixture", Field("values", Arr(ID, 1, 32, unique=True))),
})

EXECUTION = Obj(Field("state", CAPABILITY), Field("pending", PENDING_NOTE))

# The seed a library case's probe reads (ADR-0170): the writer contract each side's own probe
# runs once per shard over the named parameters of an instance's assignment, whose output the
# shard seals and every sample of those parameters copies privately.
CASE_SEED = Obj(Field("case_id", ID), Field("contract_id", ID),
                Field("parameters", Arr(ID, 1, 4, unique=True)), Field("description", TEXT))

# The workload contract a library case's probe implements, and the production operations it
# performs in one sample. Cases may share a contract only with the same region and cache.
CASE_PROBE = Obj(Field("contract_id", ID), Field("repetitions", Int(1, 1)),
                 Field("seed", CASE_SEED, required=False))

# The native probe: its source, protocol and the golden entries its oracle checks. Paths are
# relative to the repository root; the golden document to this registry's directory.
PROBE = Obj(
    Field("source", REL_PATH),
    Field("request", Const(PROBE_REQUEST_SCHEMA)),
    Field("message", Const(PROBE_MESSAGE_SCHEMA)),
    Field("golden", REL_PATH),
    Field("sentinel_contract", ID),
    Field("state", CAPABILITY),
    Field("pending", PENDING_NOTE),
    Field("description", TEXT),
)

CASE = Obj(
    Field("family", ID),
    Field("description", TEXT),
    Field("kind", Str(enum=("process", "library"))),
    Field("requires_linux_containment", Bool()),
    Field("timing", TIMING),
    Field("cache", CACHE),
    Field("oracle", ORACLE),
    Field("execution", EXECUTION),
    Field("parameters", Map(ID_PATTERN, PARAMETER, 0, 4), required=False),
    Field("tier_parameters", Map(_TIER_KEY, Map(ID_PATTERN, Arr(
        AnyOf(ID, Int(0, 10 ** 12)), 1, 32, unique=True), 1, 4), 1, 3), required=False),
    Field("tiers", Arr(Str(enum=TIERS), 1, 3, unique=True)),
    Field("criticality", Str(enum=("critical", "advisory"))),
    Field("primary_metric", ID),
    Field("optional_metrics", Arr(ID, 0, 16, unique=True)),
    Field("policy", ID),
    Field("pairs", Map(_TIER_KEY, Int(limits.PAIRS_MIN, limits.PAIRS_MAX), 1, 3)),
    Field("warmup_pairs", Int(0, limits.WARMUP_PAIRS_MAX)),
    Field("sample_timeout_seconds", Int(1, limits.SAMPLE_TIMEOUT_SECONDS_MAX)),
    Field("budgets", Arr(ID, 0, 8, unique=True)),
    Field("af_environment", Map(_AF_ENV_KEY, Str(max_len=256), 0, 16), required=False),
    Field("probe", CASE_PROBE, required=False),
)

HEALTH = Obj(
    Field("rule", Const(HEALTH_RULE)),
    Field("workload", ID),
    Field("metric", ID),
    Field("batch_repetitions", Int(1, 100000)),
    Field("start_pairs", Int(1, limits.SENTINEL_PAIRS_MAX)),
    Field("end_pairs", Int(1, limits.SENTINEL_PAIRS_MAX)),
    Field("max_median_shift", DECIMAL),
    Field("max_mad_ratio", DECIMAL),
    Field("denominator_floor", DECIMAL),
    Field("state", Str(enum=("provisional", "calibrated"))),
    Field("oracle", ORACLE),
    Field("description", TEXT),
)

VERDICT = Obj(
    Field("algorithm", Const(ALGORITHM)),
    Field("family_alpha", DECIMAL),
    Field("advisory_alpha", DECIMAL),
)

CONSTRAINTS = Obj(
    Field("task_lease_ms", Const(TASK_LEASE_MS)),
    Field("artifact_envelope_cap_bytes", Const(ARTIFACT_ENVELOPE_CAP_BYTES)),
    Field("description", TEXT),
)

SUITE = Obj(
    Field("schema", Const(SCHEMA)),
    Field("suite_id", ID),
    Field("description", TEXT),
    Field("product_constraints", CONSTRAINTS),
    Field("probe", PROBE),
    Field("verdict", VERDICT),
    Field("health", HEALTH),
    Field("limits", LIMITS),
    Field("metrics", Map(ID_PATTERN, METRIC_DEF, 1, 32)),
    Field("policies", Map(ID_PATTERN, POLICY, 1, 32)),
    Field("budgets", Map(ID_PATTERN, BUDGET, 0, 32)),
    Field("fixtures", Map(ID_PATTERN, FIXTURE, 1, 64)),
    Field("recipes", Map(ID_PATTERN, RECIPE, 1, 16)),
    Field("environments", Map(ID_PATTERN, ENVIRONMENT, 1, 16)),
    Field("tiers", Map(_TIER_KEY, TIER, 1, 3)),
    Field("cases", Map(ID_PATTERN, CASE, 1, 64)),
)



class Suite:
    """A validated registry: its parsed document, byte identity and expanded instances."""

    def __init__(self, document, raw):
        self.doc = document
        self.raw_sha256 = sha256_bytes(raw)
        self.policy_digest = canonical_digest(document)

    def instances(self, tier):
        return expand(self.doc, tier)


def load_bytes(raw, where="suite.toml", parser=None):
    document = tomlfile.loads(raw, where, parser)
    validate(document, where)
    return Suite(document, raw)


def load(path, parser=None):
    with open(path, "rb") as stream:
        raw = stream.read(tomlfile.MAX_TOML_BYTES + 1)
    return load_bytes(raw, str(path), parser)


def _fraction(value, where):
    return parse_decimal(value, where)


def validate(doc, where="suite"):
    SUITE.validate(doc, where)
    verdict = doc["verdict"]
    for key in ("family_alpha", "advisory_alpha"):
        alpha = _fraction(verdict[key], where + ".verdict." + key)
        require(0 < alpha < 1, where + ".verdict.%s: must lie strictly between 0 and 1" % key)

    metrics = doc["metrics"]
    for policy_id, policy in doc["policies"].items():
        at = where + ".policies." + policy_id
        require(policy["metric"] in metrics, at + ".metric: unknown metric " + policy["metric"])
        relative = _fraction(policy["relative_tolerance"], at + ".relative_tolerance")
        absolute = _fraction(policy["absolute_tolerance"], at + ".absolute_tolerance")
        require(relative <= 1, at + ".relative_tolerance: must not exceed 1")
        require(absolute > 0, at + ".absolute_tolerance: must be strictly positive")
        default = (metrics[policy["metric"]]["unit"] == "ms"
                   and (relative, absolute) == (DEFAULT_RELATIVE, DEFAULT_ABSOLUTE_MS))
        if policy["calibration"] == "calibrated":
            require("calibration_cohort" in policy,
                    at + ": a calibrated policy names its calibration_cohort")
        else:
            require("calibration_cohort" not in policy,
                    at + ": an initial policy names no calibration_cohort")
            require(default, at + ": a tolerance other than the initial wall-time policy "
                    "(r = 0.05, a = 1 ms) must be calibrated")

    fixtures = doc["fixtures"]
    for fixture_id, fixture in fixtures.items():
        at = where + ".fixtures." + fixture_id
        if fixture["kind"] == "typed_envelope":
            require("serialized_envelope_bytes" in fixture and "size_rule" in fixture,
                    at + ": a typed envelope declares serialized_envelope_bytes and size_rule")
        else:
            require("serialized_envelope_bytes" not in fixture and "size_rule" not in fixture,
                    at + ": only a typed envelope declares an envelope size")
        if fixture["kind"] in ("event_log", "cli_state"):
            require("events" in fixture, at + ": an event fixture declares its events")
        if fixture["kind"] == "file_tree":
            require("files" not in fixture and "file_bytes" in fixture,
                    at + ": a file tree declares file_bytes; its file count is a parameter")

    recipes = doc["recipes"]
    for recipe_id, recipe in recipes.items():
        at = where + ".recipes." + recipe_id
        zig = recipe["builder"] == "cargo_zigbuild"
        require(zig == ("cargo_zigbuild" in recipe) == ("zig" in recipe),
                at + ": cargo_zigbuild and zig versions are declared exactly for zigbuild")
        require(zig == (recipe["linker"] == "zig_cc"), at + ": zigbuild links through zig_cc")
        require(not recipe["instrumented"],
                at + ": verdict recipes are uninstrumented; diagnostics declare their own")
        # A Linux recipe builds in a pinned build image: its own `container_image`, or one the
        # build is configured with and records (ADR-0169). A macOS recipe builds on its
        # explicitly trusted local host.
        require(recipe["os"] == "linux" or "container_image" not in recipe,
                at + ": only a Linux recipe builds in a container image")
        if recipe["state"] == "supported":
            require(not recipe["pending"], at + ": a supported recipe has no pending note")
        else:
            require(recipe["pending"], at + ": a not-yet-executable recipe says what is pending")

    environments = doc["environments"]
    for env_id, env in environments.items():
        at = where + ".environments." + env_id
        container = env["containment"] == "linux_container"
        require(container == (env["os"] == "linux"),
                at + ": Linux environments use linux_container; macOS is trusted local")
        if container:
            require(all(key in env for key in ("memory_limit_bytes", "pid_limit", "logical_cpus",
                                                "prefer_separate_physical_cores")),
                    at + ": a container environment declares its CPU set, memory and PID limits")
            require(env["swap"] == "disabled" and env["network"] == "disabled",
                    at + ": a container environment disables swap and network")
        else:
            require(not any(key in env for key in ("memory_limit_bytes", "pid_limit",
                                                   "logical_cpus",
                                                   "prefer_separate_physical_cores")),
                    at + ": a trusted local process group claims no CPU set or container limits")
        require((env["libc"] == "darwin") == (env["os"] == "macos"),
                at + ": darwin libc exactly on macOS")
        if env["state"] == "supported":
            require(not env["pending"], at + ": a supported environment has no pending note")
        else:
            require(env["pending"], at + ": a not-yet-executable environment says what is pending")

    tiers = doc["tiers"]
    for tier_id, tier in tiers.items():
        at = where + ".tiers." + tier_id
        sampling_max = (limits.QUICK_SHARD_SAMPLING_MINUTES_MAX if tier_id == "quick"
                        else limits.LONG_SHARD_SAMPLING_MINUTES_MAX)
        total_max = (limits.QUICK_RUN_TOTAL_MINUTES_MAX if tier_id == "quick"
                     else limits.LONG_RUN_TOTAL_MINUTES_MAX)
        require(tier["shard_sampling_minutes"] <= sampling_max,
                at + ".shard_sampling_minutes: exceeds the hard maximum %d" % sampling_max)
        require(tier["run_total_minutes"] <= total_max,
                at + ".run_total_minutes: exceeds the hard maximum %d" % total_max)
        require(tier["shard_sampling_minutes"] <= tier["run_total_minutes"],
                at + ": the total deadline dominates the shard sampling deadline")
        require(tier["concurrent_jobs"] <= tier["shards"],
                at + ": concurrent jobs never exceed shards")
        seen = []
        for index, profile in enumerate(tier["profiles"]):
            p_at = "%s.profiles[%d]" % (at, index)
            require(profile["environment"] in environments,
                    p_at + ": unknown environment " + profile["environment"])
            require(profile["recipe"] in recipes, p_at + ": unknown recipe " + profile["recipe"])
            env = environments[profile["environment"]]
            recipe = recipes[profile["recipe"]]
            require(env["os"] == recipe["os"], p_at + ": recipe and environment OS differ")
            require(recipe["target"].startswith(env["arch"] + "-"),
                    p_at + ": recipe target does not match the environment architecture")
            require(env["libc"] == "darwin" or recipe["target"].endswith("-" + env["libc"]),
                    p_at + ": recipe target does not match the environment libc")
            require(profile["environment"] not in seen, p_at + ": duplicate environment")
            seen.append(profile["environment"])

    budgets = doc["budgets"]
    for budget_id, budget in budgets.items():
        at = where + ".budgets." + budget_id
        require(budget["metric"] in metrics, at + ".metric: unknown metric")
        require(metrics[budget["metric"]]["unit"] == budget["unit"],
                at + ".unit: differs from the metric's unit")
        require(_fraction(budget["limit"], at + ".limit") > 0, at + ".limit: must be positive")
        for env_id in budget["environments"]:
            require(env_id in environments, at + ".environments: unknown " + env_id)
        for recipe_id in budget["recipes"]:
            require(recipe_id in recipes, at + ".recipes: unknown " + recipe_id)

    health = doc["health"]
    at = where + ".health"
    require(health["workload"] in fixtures
            and fixtures[health["workload"]]["kind"] == "canonical_small",
            at + ".workload: names a canonical_small fixture")
    require(health["metric"] in metrics and metrics[health["metric"]]["unit"] == "ms",
            at + ".metric: a known millisecond metric")
    require(_fraction(health["denominator_floor"], at + ".denominator_floor") > 0,
            at + ".denominator_floor: must be positive")
    for key in ("max_median_shift", "max_mad_ratio"):
        require(_fraction(health[key], at + "." + key) > 0, at + "." + key + ": must be positive")

    probe = doc["probe"]
    if probe["state"] == "supported":
        require(not probe["pending"], where + ".probe: a supported probe has no pending note")
    else:
        require(probe["pending"], where + ".probe: say what the probe is pending")
    for case_id, case in doc["cases"].items():
        _validate_case(doc, case_id, case, where + ".cases." + case_id)
    contracts = {}
    for case_id, case in sorted(doc["cases"].items()):
        if "probe" in case:
            scope = (case["timing"], case["cache"])
            first = contracts.setdefault(case["probe"]["contract_id"], (case_id, scope))
            require(first[1] == scope, where + ".cases.%s.probe: contract %s is shared with %s, "
                    "whose timing region or cache treatment differs"
                    % (case_id, case["probe"]["contract_id"], first[0]))
    require(probe["sentinel_contract"] not in contracts,
            where + ".probe.sentinel_contract: the sentinel has its own contract, not a case's")
    if health["oracle"]["state"] == "available":
        require(probe["state"] == "supported" and fixtures[health["workload"]]["golden"]
                == "available", at + ".oracle: an available sentinel oracle needs a supported "
                "probe and the workload's golden entries")

    writers = {}
    for case_id, case in sorted(doc["cases"].items()):
        seed = case.get("probe", {}).get("seed")
        if seed is not None:
            first = writers.setdefault(seed["case_id"], (case_id, seed["contract_id"]))
            require(first[1] == seed["contract_id"],
                    where + ".cases.%s.probe.seed: writer %s has another contract in %s"
                    % (case_id, seed["case_id"], first[0]))
            require(seed["contract_id"] not in contracts,
                    where + ".cases.%s.probe.seed: a writer contract is not a case's" % case_id)

    for tier_id in tiers:
        instances = expand(doc, tier_id)
        require(len(instances) <= limits.INSTANCES_MAX,
                where + ".tiers.%s: expands to %d instances; the maximum is %d"
                % (tier_id, len(instances), limits.INSTANCES_MAX))
        require(any(item["criticality"] == "critical" for item in instances),
                where + ".tiers.%s: selects no critical instance" % tier_id)
        default = tiers[tier_id]["profiles"][0]
        unsupported = [item["instance_id"] for item in instances
                       if not supports(doc, default["environment"], item)]
        require(not unsupported, where + ".tiers.%s: its default environment cannot run %s"
                % (tier_id, ", ".join(unsupported)))
    for case_id, case in doc["cases"].items():
        for tier_id in case["tiers"]:
            require(tier_id in tiers, where + ".cases.%s.tiers: unknown tier %s"
                    % (case_id, tier_id))
    return doc


def _validate_case(doc, case_id, case, at):
    metrics = doc["metrics"]
    require(case["primary_metric"] in metrics, at + ".primary_metric: unknown metric")
    for metric in case["optional_metrics"]:
        require(metric in metrics, at + ".optional_metrics: unknown metric " + metric)
        require(metric != case["primary_metric"],
                at + ".optional_metrics: the primary metric is not optional")
    require(case["policy"] in doc["policies"], at + ".policy: unknown policy")
    require(doc["policies"][case["policy"]]["metric"] == case["primary_metric"],
            at + ".policy: governs a different metric than the primary metric")
    require(sorted(case["pairs"]) == sorted(case["tiers"]),
            at + ".pairs: declares pairs for exactly the case's tiers")
    for tier_id, pairs in case["pairs"].items():
        require(pairs % 2 == 0, at + ".pairs.%s: measured pairs come in AB/BA blocks; "
                "use an even count" % tier_id)
    require(case["warmup_pairs"] % 2 == 0,
            at + ".warmup_pairs: warmups come in AB/BA blocks; use an even count")
    timing = case["timing"]["boundary"]
    require((case["kind"] == "process") == (timing in ("child_process_lifetime",
                                                       "reported_interval")),
            at + ".timing: process cases time the child lifetime or a reported interval; "
            "library cases time a region")
    if timing == "reported_interval":
        require(case_id == "tui_open" and case["timing"]["field"] == "browser_ms",
                at + ".timing: only tui_open reports browser_ms")
    require(("splash_ms" in case["optional_metrics"]) == (timing == "reported_interval"),
            at + ".optional_metrics: splash_ms is the PTY driver's, beside its reported "
            "interval")
    if case["cache"]["projection"] == "by_parameter":
        domain = case.get("parameters", {}).get("projection")
        require(domain is not None and domain["kind"] == "enum"
                and sorted(domain["values"]) == ["application_cold", "prewarmed"],
                at + ".cache.projection: by_parameter needs a projection parameter of "
                "application_cold and prewarmed")
    parameters = case.get("parameters", {})
    for name, domain in parameters.items():
        if domain["kind"] == "fixture":
            for value in domain["values"]:
                require(value in doc["fixtures"],
                        at + ".parameters.%s: unknown fixture %s" % (name, value))
    for tier_id, restriction in case.get("tier_parameters", {}).items():
        require(tier_id in case["tiers"],
                at + ".tier_parameters.%s: the case does not run in that tier" % tier_id)
        for name, values in restriction.items():
            require(name in parameters, at + ".tier_parameters.%s: unknown parameter %s"
                    % (tier_id, name))
            domain = parameters[name]["values"]
            for value in values:
                require(any(type(value) is type(item) and value == item for item in domain),
                        at + ".tier_parameters.%s.%s: %s is outside the domain"
                        % (tier_id, name, value))
    for budget_id in case["budgets"]:
        require(budget_id in doc["budgets"], at + ".budgets: unknown budget " + budget_id)
        budget = doc["budgets"][budget_id]
        require(budget["metric"] == case["primary_metric"],
                at + ".budgets.%s: applies to a different metric" % budget_id)
        for tier_id in budget["tiers"]:
            if tier_id in case["pairs"]:
                require(case["pairs"][tier_id] >= budget["minimum_samples"],
                        at + ".budgets.%s: %s pairs are fewer than the budget's %d samples"
                        % (budget_id, tier_id, budget["minimum_samples"]))
    if case["kind"] == "library":
        require("probe" in case, at + ".probe: a library case names its probe contract")
        seed = case["probe"].get("seed")
        if seed is not None:
            require(seed["parameters"] == sorted(seed["parameters"])
                    and all(name in parameters for name in seed["parameters"]),
                    at + ".probe.seed.parameters: sorted parameters of the case")
            require(seed["case_id"] not in doc["cases"],
                    at + ".probe.seed: a seed writer is not a timed case")
    else:
        require("probe" not in case, at + ".probe: only library cases run in the native probe")
    if case["oracle"]["state"] == "available" and case["kind"] == "library":
        require(doc["probe"]["state"] == "supported",
                at + ".oracle: an available native oracle needs a supported probe")
        for name, domain in parameters.items():
            if domain["kind"] == "fixture":
                for value in domain["values"]:
                    require(doc["fixtures"][value]["golden"] == "available",
                            at + ".oracle: fixture %s has no golden entries" % value)
    if case["execution"]["state"] == "supported":
        require(case["oracle"]["state"] == "available",
                at + ".execution: a supported case has an available independent oracle")
        require(not case["execution"]["pending"], at + ".execution: supported has no pending")
    else:
        require(case["execution"]["pending"], at + ".execution: say what is pending")


def supports(doc, environment_id, instance):
    environment = doc["environments"][environment_id]
    if instance["requires_linux_containment"]:
        return environment["containment"] == "linux_container"
    return True


def seed_of(doc, instance):
    """The seed an expanded library instance reads, as `{seed_id, case_id, contract_id,
    parameters}`, or None. Instances whose seed parameters agree share one seed."""
    case = doc["cases"][instance["case_id"]]
    declared = case.get("probe", {}).get("seed")
    if declared is None:
        return None
    parameters = dict((name, instance["parameters"][name]) for name in declared["parameters"])
    return {"seed_id": instance_id(declared["case_id"], sorted(parameters.items())),
            "case_id": declared["case_id"], "contract_id": declared["contract_id"],
            "parameters": parameters}


def instance_id(case_id, assignment):
    if not assignment:
        return case_id
    parts = ["%s_%s" % (name, value) for name, value in assignment]
    return case_id + "__" + "_".join(parts)


def expand(doc, tier):
    """Every instance of every case in `tier`, sorted by instance ID.

    Parameters are taken in name order and values in declared order; a tier restriction
    selects a subset of a domain. Expansion happens before sharding and never at run time.
    """
    out = []
    for case_id in sorted(doc["cases"]):
        case = doc["cases"][case_id]
        if tier not in case["tiers"]:
            continue
        parameters = case.get("parameters", {})
        restriction = case.get("tier_parameters", {}).get(tier, {})
        names = sorted(parameters)
        domains = []
        for name in names:
            values = parameters[name]["values"]
            if name in restriction:
                values = [value for value in values if value in restriction[name]]
            domains.append([(name, value) for value in values])
        for assignment in itertools.product(*domains):
            ident = instance_id(case_id, assignment)
            require(is_id(ident), "cases.%s: instance ID %s exceeds the ID rule"
                    % (case_id, ident))
            out.append({"instance_id": ident, "case_id": case_id,
                        "parameters": dict(assignment), "criticality": case["criticality"],
                        "requires_linux_containment": case["requires_linux_containment"]})
    out.sort(key=lambda item: item["instance_id"])
    ids = [item["instance_id"] for item in out]
    if len(set(ids)) != len(ids):
        raise ContractError("tier %s: two cases expand to the same instance ID" % tier)
    return out
