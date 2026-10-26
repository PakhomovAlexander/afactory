"""The closed contracts of separate diagnostic child runs (package p5, ADR-0174).

- `af.perf-diagnostics/1`: the reviewed diagnostic registry `fixtures/performance/diagnostics.toml`
  (selection and capture defaults, diagnostic recipes derived from verdict recipes, the pinned
  tools and their provisioning instructions, the trace overhead target and resource caps).
- `af.perf-profiles/5`: one child run's evidence, `profiles.json`: its identity and link to the
  parent comparison, what it selected and why, its budgets and the actual usage of them (never
  clamped), its explicit execution mode, containment and supervisor (with the container, image,
  CPUs and verified scratch bound of a contained run), every tool identity or why it is
  unavailable, every capture of a probe region or a CLI process with its decoded loss records,
  rendered graph, trace with the coverage of its region, and counter with its artifacts, and
  every omission with its reason and reproduction. Version 5 adds the explicitly selected
  telemetry level and each telemetry capture of a CLI case, derived from the side's own
  `af telemetry` documents (ADR-0214). Versions 1 to 4 are refused.
- `af.perf-trace/1`: the metadata-only phase spans a `perf-trace` build exports
  (`review_core::perf_trace`).

Diagnostics never touch a verdict: `verdict_effect` is the constant `none`.
"""
from fractions import Fraction

from . import limits
from .spec import (Arr, Bool, COUNT, Const, DECIMAL, DIGEST, Field, GIT_SHA, ID, Int, Map,
                   Nullable, Obj, RATIONAL, REL_PATH, SHORT_TEXT, Str, TEXT)
from .values import parse_decimal, parse_rational, require

SIDES = ("baseline", "candidate")
ARGV = Arr(Str(min_len=1, max_len=512), 1, 64)
ARTIFACT = Obj(Field("path", REL_PATH), Field("bytes", COUNT), Field("sha256", DIGEST))
OMISSION = Obj(Field("reason", ID), Field("detail", TEXT))

# af.perf-diagnostics/1 ---------------------------------------------------------------------

DIAGNOSTICS_SCHEMA = "af.perf-diagnostics/1"
RECIPE_KINDS = ("cpu_profile", "trace", "counters")
TOOL_NAMES = ("perf", "inferno_collapse_perf", "inferno_diff_folded", "inferno_flamegraph",
              "samply", "valgrind", "callgrind_control")
FEATURE = Str(pattern=r"^[a-z0-9_-]{1,64}$")

DIAGNOSTIC_RECIPE = Obj(
    Field("description", TEXT),
    Field("kind", Str(enum=RECIPE_KINDS)),
    # The verdict recipe of the suite this one is built like: same target, toolchain, builder,
    # linker and image; only what diagnosis needs is added.
    Field("base_recipe", ID),
    Field("features", Arr(FEATURE, 0, 8, unique=True)),
    Field("debug_symbols", Bool()),
    Field("frame_pointers", Bool()),
    Field("tools", Arr(Str(enum=TOOL_NAMES), 0, 8, unique=True)),
)

TOOL = Obj(
    Field("description", TEXT),
    Field("executable", Str(pattern=r"^[a-z][a-z0-9_-]{0,63}$", max_len=64)),
    Field("os", Arr(Str(enum=("linux", "macos")), 1, 2, unique=True)),
    Field("package", SHORT_TEXT),
    # A declared pin with the checksum of its published source, or none of the three: the
    # probed executable's own SHA-256 is always recorded.
    Field("version", Str(pattern=r"^[0-9]+\.[0-9]+\.[0-9]+$", max_len=32), required=False),
    Field("source_sha256", Str(pattern=r"^[0-9a-f]{64}$", max_len=64), required=False),
    Field("source_url", Str(pattern=r"^https://[A-Za-z0-9./_-]+$", max_len=256), required=False),
    # How the tool reports its own version; absent when it cannot (inferno's binaries refuse
    # --version), and then only the declared pin and the executable's bytes identify it.
    Field("version_argv", Arr(Str(min_len=1, max_len=64), 1, 4), required=False),
    Field("provisioning", TEXT),
)

DIAGNOSTICS = Obj(
    Field("schema", Const(DIAGNOSTICS_SCHEMA)),
    Field("description", TEXT),
    Field("selection", Obj(
        Field("default_cases", Const(limits.DIAGNOSTIC_CASES_DEFAULT)),
        Field("max_cases", Const(limits.DIAGNOSTIC_CASES_MAX)),
        Field("order", Const("regressed_then_inconclusive_by_instance_id")),
        Field("description", TEXT))),
    Field("capture", Obj(
        Field("frequency_hz", Int(1, limits.DIAGNOSTIC_HZ_MAX)),
        Field("duration_seconds", Int(1, limits.DIAGNOSTIC_SECONDS_MAX)),
        # The one sampled event, declared: never silently substituted by another.
        Field("event", Str(pattern=r"^[a-z][a-z0-9_-]{0,63}(?::[ukh]{1,3})?$", max_len=72)),
        Field("dwarf_stack_bytes", Const(limits.DIAGNOSTIC_DWARF_STACK_BYTES)),
        Field("extra_dwarf_captures_per_side", Const(1)),
        Field("dwarf_when_unknown_ratio_above", DECIMAL),
        # Fewer samples than this never attribute a region: the capture is kept, not rendered.
        Field("minimum_samples", Int(1, 1000000)),
        Field("max_repetitions", Int(1, 10000)),
        Field("handshake_timeout_seconds", Int(1, 120)),
        Field("description", TEXT))),
    Field("trace", Obj(
        Field("overhead_target", DECIMAL),
        Field("overhead_pairs", Int(limits.PAIRS_MIN, limits.PAIRS_MAX)),
        Field("description", TEXT))),
    Field("counters", Obj(
        Field("seconds", Int(1, limits.COUNTER_SECONDS_MAX)),
        Field("cases", Arr(ID, 1, limits.DIAGNOSTIC_CASES_MAX, unique=True)),
        Field("description", TEXT))),
    Field("limits", Obj(
        Field("total_minutes", Int(1, limits.DIAGNOSTIC_RUN_MINUTES_MAX)),
        Field("profile_bytes", Int(1, limits.DIAGNOSTIC_BYTES_MAX)),
        Field("builds", Int(0, limits.DIAGNOSTIC_BUILDS_MAX)),
        Field("tool_output_bytes", Int(1, limits.DIAGNOSTIC_TOOL_OUTPUT_BYTES_MAX)),
        Field("tool_seconds", Int(1, limits.DIAGNOSTIC_TOOL_SECONDS_MAX)))),
    Field("recipes", Map(r"^[a-z][a-z0-9_]{0,63}$", DIAGNOSTIC_RECIPE, 1, 16)),
    Field("tools", Map(r"^[a-z][a-z0-9_]{0,63}$", TOOL, 1, 16)),
)


def validate_diagnostics(doc, where="diagnostics.toml", suite_doc=None):
    """The registry's shape and rules; with `suite_doc`, every recipe's base must be one of its
    recipes, on the same operating system as every tool the recipe names."""
    DIAGNOSTICS.validate(doc, where)
    require(set(doc["tools"]) == set(TOOL_NAMES),
            where + ".tools: declares exactly " + ", ".join(sorted(TOOL_NAMES)))
    capture = doc["capture"]
    require(parse_decimal(capture["dwarf_when_unknown_ratio_above"], where) <= 1,
            where + ".capture.dwarf_when_unknown_ratio_above: a ratio is at most 1")
    target = parse_decimal(doc["trace"]["overhead_target"], where)
    require(0 < target < 1, where + ".trace.overhead_target: a fraction above zero below one")
    for name, tool in doc["tools"].items():
        at = "%s.tools.%s" % (where, name)
        require(("version" in tool) == ("source_sha256" in tool) == ("source_url" in tool),
                at + ": a pinned version names its source and that source's checksum")
    for recipe_id, recipe in doc["recipes"].items():
        at = "%s.recipes.%s" % (where, recipe_id)
        check_kind(recipe["kind"], recipe, recipe["tools"], at)
        if suite_doc is not None:
            base = suite_doc["recipes"].get(recipe["base_recipe"])
            require(base is not None, at + ".base_recipe: not a recipe of the suite")
            require(not base["instrumented"], at + ".base_recipe: a verdict recipe")
            for tool_name in recipe["tools"]:
                require(base["os"] in doc["tools"][tool_name]["os"],
                        at + ".tools: %s does not run on %s" % (tool_name, base["os"]))
    return doc


def check_kind(kind, recipe, tools, at):
    """The rules a declared recipe kind holds the recipe's build to; the registry and every
    frozen diagnostic plan check them (ADR-0179). They never infer a kind: the kind is declared
    and frozen, and a recipe that breaks its kind's rules is refused."""
    if kind == "cpu_profile":
        require(recipe["frame_pointers"] and recipe["debug_symbols"],
                at + ": a CPU profile recipe keeps frame pointers and debug symbols")
        require("perf-control" in recipe["features"] or "samply" in tools,
                at + ": a CPU profile recipe brackets the region (perf-control) or is "
                "samply's whole-process capture")
    if kind == "trace":
        require("perf-trace" in recipe["features"], at + ": a trace recipe has perf-trace")
    if kind == "counters":
        require(recipe["debug_symbols"], at + ": a counter recipe keeps debug symbols")


def frozen_identity(diagnostics_doc, recipe_id):
    """What a diagnostic plan freezes of its registry recipe beside the suite-shaped recipe
    (`af.perf-run/4`, ADR-0179): the declared kind, the verdict recipe it is built like and the
    tools it runs. A report classifies a child's evidence by this kind, never by its ID."""
    entry = diagnostics_doc["recipes"].get(recipe_id)
    require(entry is not None, "diagnostic recipe %s is not declared" % recipe_id)
    return {"kind": entry["kind"], "base_recipe": entry["base_recipe"],
            "tools": list(entry["tools"])}


def diagnostic_recipe(suite_doc, diagnostics_doc, recipe_id):
    """The suite-shaped recipe a diagnostic plan freezes: its base verdict recipe with only the
    diagnostic recipe's features, symbols, frame pointers and the instrumented flag changed.
    GNU CPU profiles freeze line-tables-only debug information in the existing
    rustflags field: function/line attribution and unwind information without local-variable
    and type information. These are diagnostic build settings, never verdict settings."""
    entry = diagnostics_doc["recipes"].get(recipe_id)
    require(entry is not None, "diagnostic recipe %s is not declared" % recipe_id)
    base = dict(suite_doc["recipes"][entry["base_recipe"]])
    base.update({"description": entry["description"], "features": list(entry["features"]),
                 "debug_symbols": entry["debug_symbols"],
                 "frame_pointers": entry["frame_pointers"], "instrumented": True})
    if entry["kind"] == "cpu_profile" and base["os"] == "linux" \
            and base["linker"] == "system_cc":
        base["rustflags"] = list(base["rustflags"]) + [
            "-Cdebuginfo=line-tables-only"]
    return base


# af.perf-trace/1 ---------------------------------------------------------------------------

TRACE_SCHEMA = "af.perf-trace/1"
PHASES = ("cli_startup", "cli_command", "canonicalize", "envelope_validate", "cas_put", "cas_get",
          "cas_get_artifact", "task_projection", "authority_plan_check", "task_runtime_new",
          "probe_setup", "probe_region")
# review_core::perf_trace::CAPACITY.
TRACE_CAPACITY_MAX = 65536
NS = Int(0, 2 ** 64 - 1)

TRACE_SPAN = Obj(Field("id", Int(1, 2 ** 64 - 1)), Field("parent", Nullable(Int(1, 2 ** 64 - 1))),
                 Field("phase", Str(enum=PHASES)), Field("tid", Int(1, 2 ** 64 - 1)),
                 Field("start_ns", NS), Field("end_ns", NS))

TRACE = Obj(
    Field("schema", Const(TRACE_SCHEMA)), Field("pid", Int(1, 2 ** 32 - 1)),
    Field("clock", Const("monotonic_ns")),
    Field("epoch_unix_ns", Str(pattern=r"^(?:0|[1-9][0-9]{0,30})$", max_len=31)),
    Field("capacity", Int(1, TRACE_CAPACITY_MAX)), Field("dropped", COUNT), Field("open", COUNT),
    Field("spans", Arr(TRACE_SPAN, 0, TRACE_CAPACITY_MAX)),
)


def validate_trace(doc, where="trace.json"):
    TRACE.validate(doc, where)
    require(len(doc["spans"]) + doc["open"] <= doc["capacity"],
            where + ": more spans than the recorder's capacity")
    ids = {}
    for index, span in enumerate(doc["spans"]):
        at = "%s.spans[%d]" % (where, index)
        require(span["id"] not in ids, at + ".id: duplicate span")
        require(span["start_ns"] <= span["end_ns"], at + ": ends before it starts")
        ids[span["id"]] = span
    order = [(span["start_ns"], span["id"]) for span in doc["spans"]]
    require(order == sorted(order), where + ".spans: ordered by start, then identity")
    for index, span in enumerate(doc["spans"]):
        parent = span["parent"]
        if parent is None:
            continue
        at = "%s.spans[%d]" % (where, index)
        # A parent still open at export, or one never kept, is not in the export.
        if parent in ids:
            require(ids[parent]["tid"] == span["tid"], at + ".parent: a span on another thread")
            require(ids[parent]["start_ns"] <= span["start_ns"],
                    at + ".parent: starts after its child")
        else:
            require(doc["open"] > 0, at + ".parent: names no exported or open span")
    return doc


# af.perf-profiles/5 ------------------------------------------------------------------------

PROFILES_SCHEMA = "af.perf-profiles/5"
# What a capture or trace observes: the native probe's acknowledged region, or a whole AF CLI
# process judged by its command's oracle (ADR-0176).
SUBJECTS = ("probe_region", "cli_process")
# How much of its region a trace holds: every span of it with nothing dropped or left open, the
# region with spans dropped or open, or no span of the region at all (a recorder filled before
# it, a setup-only trace), never presented as complete.
COVERAGE = ("complete", "partial", "region_missing")
CLI_REGION_PHASES = ("cli_startup", "cli_command")
MODES = ("trusted_local", "linux_container")
CAPTURE_TOOLS = ("perf", "samply")
COUNTER_TOOLS = ("callgrind", "dhat")
SELECTION_REASONS = ("regressed", "inconclusive", "explicit")
OMISSION_KINDS = ("cpu_profile", "flamegraph", "trace", "trace_overhead", "counter", "samply",
                  "build", "symbols", "telemetry")
RENDER_KINDS = ("baseline", "candidate", "difference_forward", "difference_reverse")
TOOL_STATES = ("available", "unavailable")
VERSION_PROVENANCE = ("tool_reported", "declared_pin_only", "unknown")
USE = Obj(Field("limit", COUNT), Field("used", COUNT))

PROFILE_TOOL = Obj(
    Field("name", Str(enum=TOOL_NAMES)), Field("state", Str(enum=TOOL_STATES)),
    Field("executable_sha256", Nullable(DIGEST)),
    Field("package", SHORT_TEXT), Field("declared_version", Nullable(SHORT_TEXT)),
    Field("reported_version", Nullable(Str(min_len=1, max_len=256))),
    Field("version_provenance", Str(enum=VERSION_PROVENANCE)),
    Field("detail", TEXT), Field("provisioning", TEXT),
)

STOPS = ("duration_reached", "repetition_cap", "diagnostic_budget", "recording_cap",
         "supervised_window", "single_repetition", "not_started")
REGION = Obj(
    # How the capture is bounded to the named region: the probe's acknowledged handshake, or
    # samply's whole process (labelled as such, never presented as region attribution).
    Field("mechanism", Str(enum=("acknowledged_handshake", "whole_process"))),
    # How each repetition is a fresh process: a fresh execution of the probe's image in the
    # one recorded task (perf), or a fresh process of its own (samply), ADR-0175.
    Field("process", Str(enum=("fresh_exec", "fresh_process"))),
    Field("repetitions", COUNT),
    # The probe-measured time of the recorded regions, the wall time recording was enabled
    # (handshakes included), the untimed setup outside it and the whole process.
    Field("setup_ms", DECIMAL), Field("region_ms", DECIMAL), Field("enabled_ms", DECIMAL),
    Field("wall_ms", DECIMAL),
    Field("observed", Bool()), Field("truncated_at_cap", Bool()),
    Field("stopped", Str(enum=STOPS)),
)

# The loss records decoded from the recording (ADR-0175): PERF_RECORD_LOST records and the
# events they say were lost, PERF_RECORD_LOST_SAMPLES records and the samples they say were
# potentially lost; two payloads of different meaning, never summed, all unknown when the
# recording could not be decoded.
LOSSES = Obj(
    Field("state", Str(enum=("decoded", "unknown"))),
    Field("lost_event_records", Nullable(COUNT)), Field("lost_events", Nullable(COUNT)),
    Field("lost_sample_records", Nullable(COUNT)),
    Field("potentially_lost_samples", Nullable(COUNT)),
    Field("detail", Str(max_len=1024)),
)
# The evidence byte cap: what the evidence actually holds, never clamped, the administrative
# reserve kept inside the cap for profiles.json, the page and the manifest, and any overage of
# the evidence beyond its room, stated rather than hidden (ADR-0175).
BYTES = Obj(Field("limit", COUNT), Field("administrative_reserve", COUNT),
            Field("evidence_used", COUNT), Field("evidence_overage", COUNT))

CAPTURE = Obj(
    Field("instance_id", ID), Field("side", Str(enum=SIDES)), Field("attempt", Int(1, 2)),
    Field("tool", Str(enum=CAPTURE_TOOLS)), Field("subject", Str(enum=SUBJECTS)),
    Field("binary_sha256", Nullable(DIGEST)),
    Field("event", Nullable(SHORT_TEXT)),
    Field("frequency_hz", Int(1, limits.DIAGNOSTIC_HZ_MAX)),
    Field("duration_ms", Int(1, limits.DIAGNOSTIC_SECONDS_MAX * 1000)),
    Field("effective_frequency_hz", Nullable(RATIONAL)),
    Field("unwind", Str(enum=("frame_pointer", "dwarf"))),
    Field("dwarf_stack_bytes", Nullable(Const(limits.DIAGNOSTIC_DWARF_STACK_BYTES))),
    Field("region", REGION),
    Field("samples", Nullable(COUNT)), Field("losses", LOSSES),
    Field("frames", Nullable(COUNT)), Field("unknown_frames", Nullable(COUNT)),
    Field("unknown_frame_ratio", Nullable(RATIONAL)),
    Field("symbols", Obj(Field("state", Str(enum=("retained", "referenced", "missing"))),
                         Field("build_ids", Arr(Str(pattern=r"^[0-9a-f]{16,64}$", max_len=64),
                                                0, 256)),
                         Field("binary_build_id_present", Nullable(Bool())))),
    Field("artifacts", Arr(ARTIFACT, 0, 64)),
    Field("omission", Nullable(OMISSION)),
)

RENDER = Obj(
    Field("instance_id", ID), Field("kind", Str(enum=RENDER_KINDS)),
    # Absolute sample counts for one side; for a difference, the change in each stack's share
    # of CPU samples after normalization, never an absolute time saved.
    Field("color", Str(enum=("sample_count", "sample_share_change"))),
    Field("inputs", Arr(DIGEST, 1, 2)),
    Field("original", ARTIFACT), Field("sanitized", ARTIFACT),
    Field("removed", Obj(Field("elements", COUNT), Field("attributes", COUNT))),
)

OVERHEAD = Obj(
    Field("pairs", Int(1, limits.PAIRS_MAX)),
    Field("off_recipe_digest", DIGEST), Field("on_recipe_digest", DIGEST),
    Field("off_binary_sha256", DIGEST), Field("on_binary_sha256", DIGEST),
    Field("off_ms", Arr(DECIMAL, 1, limits.PAIRS_MAX)),
    Field("on_ms", Arr(DECIMAL, 1, limits.PAIRS_MAX)),
    Field("median_overhead", RATIONAL), Field("target", DECIMAL),
    Field("label", Str(enum=("within_target", "perturbing"))),
)

TRACE_ENTRY = Obj(
    Field("instance_id", ID), Field("side", Str(enum=SIDES)),
    Field("subject", Str(enum=SUBJECTS)),
    # The phase whose spans are the traced region, how many the trace holds and what that
    # coverage is; null coverage exactly for an omitted trace.
    Field("region_phase", Str(enum=PHASES)), Field("region_spans", COUNT),
    Field("coverage", Nullable(Str(enum=COVERAGE))),
    Field("spans", COUNT), Field("dropped", COUNT), Field("open", COUNT),
    Field("async_spans", COUNT),
    Field("overhead", Nullable(OVERHEAD)),
    Field("artifacts", Arr(ARTIFACT, 0, 16)),
    Field("omission", Nullable(OMISSION)),
)

COUNTER = Obj(
    Field("instance_id", ID), Field("side", Str(enum=SIDES)),
    Field("tool", Str(enum=COUNTER_TOOLS)),
    Field("metric", Str(enum=("instructions", "allocations", "allocated_bytes"))),
    Field("scope", Str(enum=("region", "whole_process"))),
    Field("value", Nullable(COUNT)), Field("duration_ms", Nullable(DECIMAL)),
    Field("interpretation", Const("explanatory_proxy_not_wall_time")),
    Field("artifacts", Arr(ARTIFACT, 0, 16)),
    Field("omission", Nullable(OMISSION)),
)

# A telemetry capture (ADR-0214): the run the side's telemetry-recorded execution of a CLI case
# recorded, read back through that binary's own `af telemetry` documents, bound to the case,
# side, source and binary; its phases, a bounded timeline and an evidence state that is
# complete only with nothing open, unsealed, dropped, recovered, unreadable or incomplete.
TELEMETRY_LEVELS = ("spans", "content")
TELEMETRY_PARTIAL_REASONS = ("config_incomplete", "corrupt_segment",
                             "dropped_or_recovered_capture", "history_partial", "index_absent",
                             "live_segment", "open_execution", "open_spans",
                             "problems_recorded", "unreadable_files", "unsealed_execution",
                             "unsealed_segment")
TELEMETRY_LABEL = Str(min_len=1, max_len=128)
SPAN_ID = Str(pattern=r"^[0-9a-f]{16}$", max_len=16)
TELEMETRY_COUNTS = Obj(Field("live", COUNT), Field("unsealed", COUNT), Field("sealed", COUNT),
                       Field("corrupt", COUNT))
TELEMETRY_CAPTURE = Obj(
    Field("instance_id", ID), Field("side", Str(enum=SIDES)),
    Field("subject", Const("cli_process")), Field("level", Str(enum=TELEMETRY_LEVELS)),
    Field("source_sha", GIT_SHA), Field("binary_sha256", DIGEST),
    # The supervisor's wall time of the recorded execution: a diagnostic execution at this
    # level, never a verdict sample; null when it never ran.
    Field("command_wall_ms", Nullable(DECIMAL)),
    Field("run", Nullable(Obj(Field("store_id", Str(pattern=r"^[0-9a-f]{64}$", max_len=64)),
                              Field("run_id", TELEMETRY_LABEL),
                              Field("trace_id", Str(pattern=r"^[0-9a-f]{32}$", max_len=32)),
                              Field("kind", Const("task")),
                              Field("task_id", Nullable(TELEMETRY_LABEL))))),
    Field("af", Nullable(Obj(Field("version", Nullable(TELEMETRY_LABEL)),
                             Field("commit", Nullable(TELEMETRY_LABEL)),
                             Field("target", Nullable(TELEMETRY_LABEL))))),
    Field("executions", Arr(Obj(Field("execution_id", TELEMETRY_LABEL),
                                Field("ordinal", Nullable(Int(1, 2 ** 53))),
                                Field("segment", Str(enum=("live", "unsealed", "sealed"))),
                                Field("open", Bool()), Field("wall_us", COUNT),
                                Field("outcome", Nullable(TELEMETRY_LABEL))), 0, 16)),
    Field("evidence", Nullable(Obj(
        Field("state", Str(enum=("complete", "partial"))),
        Field("reasons", Arr(Str(enum=TELEMETRY_PARTIAL_REASONS), 0, 16, unique=True)),
        Field("index", Str(enum=("fresh", "direct", "absent"))),
        Field("history", Str(pattern=r"^(?:prefix:(?:0|[1-9][0-9]{0,18})|partial)$",
                             max_len=64)),
        Field("segments", TELEMETRY_COUNTS),
        Field("segment_captures", Arr(Obj(
            Field("execution_id", TELEMETRY_LABEL),
            Field("state", Nullable(Str(enum=("live", "unsealed", "sealed", "corrupt")))),
            Field("exit", Nullable(Str(enum=("ok", "error", "signal", "recovered")))),
            Field("capture", Nullable(TELEMETRY_LABEL))), 0, 16)),
        Field("problems", COUNT), Field("unreadable", COUNT), Field("open_spans", COUNT),
        Field("config", Str(enum=("complete", "incomplete", "unknown")))))),
    Field("spans", COUNT),
    Field("phases", Arr(Obj(Field("name", TELEMETRY_LABEL), Field("spans", COUNT),
                            Field("open", COUNT), Field("covered_us", COUNT),
                            Field("self_us", COUNT), Field("longest_us", COUNT)), 0, 128)),
    Field("timeline", Obj(
        Field("spans", Arr(Obj(Field("span_id", SPAN_ID), Field("parent", Nullable(SPAN_ID)),
                               Field("name", TELEMETRY_LABEL), Field("depth", Int(0, 64)),
                               Field("start_us", COUNT), Field("total_us", Nullable(COUNT)),
                               Field("self_us", Nullable(COUNT)),
                               Field("status", Str(enum=("ok", "error", "unset", "open"))),
                               Field("lane", Str(enum=("nested", "async", "instant"))),
                               Field("level", Int(0, 64))), 0, 512)),
        Field("omitted", COUNT))),
    Field("artifacts", Arr(ARTIFACT, 0, 8)),
    Field("omission", Nullable(OMISSION)),
)
TELEMETRY = Obj(
    # Selected explicitly (`profile --telemetry`), never implied by a recipe; null when none.
    Field("level", Nullable(Str(enum=TELEMETRY_LEVELS))),
    Field("perturbing", Bool()),
    Field("verdict_effect", Const("none")),
    Field("captures", Arr(TELEMETRY_CAPTURE, 0, 16)),
)
TELEMETRY_DOCUMENTS = ("runs.json", "show.json", "status.json", "config.json", "perfetto.json")

PROFILES = Obj(
    Field("schema", Const(PROFILES_SCHEMA)), Field("run_id", ID),
    Field("parent_run_id", ID), Field("parent_plan_digest", DIGEST),
    Field("plan_digest", DIGEST), Field("registry_sha256", DIGEST),
    Field("verdict_effect", Const("none")),
    Field("selection", Obj(
        Field("mode", Str(enum=("automatic", "explicit"))),
        Field("limit", Int(1, limits.DIAGNOSTIC_CASES_MAX)),
        Field("parent_verdict_sha256", Nullable(DIGEST)),
        Field("cases", Arr(Obj(Field("instance_id", ID), Field("case_id", ID),
                               Field("reason", Str(enum=SELECTION_REASONS))),
                           0, limits.DIAGNOSTIC_CASES_MAX)),
        Field("not_selected", Arr(Obj(Field("instance_id", ID),
                                      Field("reason", Str(enum=("over_case_limit",)))),
                                  0, limits.INSTANCES_MAX)))),
    Field("sources", Obj(Field("baseline_sha", GIT_SHA), Field("candidate_sha", GIT_SHA),
                         Field("authority_sha", GIT_SHA))),
    Field("recipe", Obj(Field("recipe_id", ID), Field("kind", Str(enum=RECIPE_KINDS)),
                        Field("recipe_digest", DIGEST), Field("base_recipe", ID),
                        Field("features", Arr(FEATURE, 0, 8)),
                        Field("debug_symbols", Bool()), Field("frame_pointers", Bool()),
                        Field("builds_sha256", Nullable(DIGEST)))),
    Field("capture", Obj(Field("frequency_hz", Int(1, limits.DIAGNOSTIC_HZ_MAX)),
                         Field("duration_ms", Int(1, limits.DIAGNOSTIC_SECONDS_MAX * 1000)),
                         Field("event", SHORT_TEXT),
                         Field("dwarf_stack_bytes", Const(limits.DIAGNOSTIC_DWARF_STACK_BYTES)))),
    Field("budgets", Obj(Field("wall_ms", USE), Field("bytes", BYTES), Field("cases", USE),
                         Field("builds", USE))),
    Field("execution", Obj(
        # Selected explicitly, never implied by a recipe: trusted local mode on this host under
        # process-group containment, or the contained Linux profile, one container per request.
        Field("mode", Str(enum=MODES)),
        Field("containment", Str(enum=("process_group", "pid_namespace"))),
        # What the plan's environment declares, and whether this execution is that one.
        Field("declared_containment", Str(enum=("trusted_local_process_group",
                                                "linux_container"))),
        Field("matches_environment", Bool()),
        # The verified authority supervisor every tool and candidate ran under.
        Field("supervisor_sha256", Nullable(DIGEST)),
        # The machine's coordination lock, held for the whole child run.
        Field("machine_lock", Nullable(Str(pattern=r"^/.", max_len=4096))),
        Field("scratch_bound", Str(enum=("not_enforced_in_trusted_local_mode",
                                         "verified_disk_bounded"))),
        # A contained run's pinned image, container runtime, CPUs and the verified scratch
        # filesystem's bound every container wrote within.
        Field("container", Nullable(Obj(
            Field("image", Str(pattern=r"^(?:sha256:[0-9a-f]{64}|[^\s@]+@sha256:[0-9a-f]{64})$",
                               max_len=512)),
            Field("runtime", SHORT_TEXT),
            Field("cpus", Arr(Int(0, 4095), 1, 64, unique=True)),
            Field("scratch_limit_bytes", COUNT)))),
        # Scratch kept because its removal was not confirmed.
        Field("retained_scratch", COUNT),
        Field("started_unix_ms", Int(0, 2 ** 53)),
        Field("ended_unix_ms", Int(0, 2 ** 53)),
        Field("deadline_unix_ms", Int(0, 2 ** 53)),
        Field("host", SHORT_TEXT))),
    Field("tools", Arr(PROFILE_TOOL, 0, 16)),
    Field("captures", Arr(CAPTURE, 0, 64)),
    Field("renders", Arr(RENDER, 0, 64)),
    Field("traces", Arr(TRACE_ENTRY, 0, 32)),
    Field("counters", Arr(COUNTER, 0, 64)),
    Field("telemetry", TELEMETRY),
    Field("omissions", Arr(Obj(Field("kind", Str(enum=OMISSION_KINDS)),
                               Field("instance_id", Nullable(ID)),
                               Field("side", Nullable(Str(enum=SIDES))),
                               Field("reason", ID), Field("detail", TEXT),
                               Field("reproduction", Nullable(ARGV))), 0, 256)),
)


def _ratio_at_most_one(text, at):
    require(parse_rational(text, at) <= 1 and parse_rational(text, at) >= 0,
            at + ": a ratio within 0..1")


def validate_profiles(doc, where="profiles.json"):
    PROFILES.validate(doc, where)
    require(doc["run_id"] != doc["parent_run_id"], where + ": a diagnostic is a separate run")
    selection = doc["selection"]
    selected = [item["instance_id"] for item in selection["cases"]]
    require(len(selected) == len(set(selected)), where + ".selection.cases: each case once")
    require(len(selected) <= selection["limit"], where + ".selection: over its case limit")
    require((selection["mode"] == "automatic") == (selection["parent_verdict_sha256"] is not None),
            where + ".selection: exactly an automatic selection names the verdict it read")
    reasons = [item["reason"] for item in selection["cases"]]
    if selection["mode"] == "explicit":
        require(set(reasons) <= {"explicit"} and not selection["not_selected"],
                where + ".selection: an explicit selection is exactly what was asked for")
    else:
        require("explicit" not in reasons, where + ".selection: automatic reasons only")
        ranked = [(SELECTION_REASONS.index(reason), instance)
                  for reason, instance in zip(reasons, selected)]
        require(ranked == sorted(ranked), where + ".selection.cases: regressed, then "
                "inconclusive, each by stable instance ID")
        require(not selection["not_selected"] or len(selected) == selection["limit"],
                where + ".selection.not_selected: only cases beyond the limit")
    for name in ("cases", "builds"):
        use = doc["budgets"][name]
        require(use["used"] <= use["limit"], where + ".budgets.%s: used beyond its limit" % name)
    used = doc["budgets"]["bytes"]
    require(used["administrative_reserve"] < used["limit"]
            and used["evidence_overage"] == max(0, used["evidence_used"] - used["limit"]
                                                + used["administrative_reserve"]),
            where + ".budgets.bytes: the actual evidence bytes, and exactly their overage "
            "beyond the cap less its administrative reserve")
    require((doc["execution"]["supervisor_sha256"] is None) <= (not doc["captures"]
                                                                and not doc["traces"]
                                                                and not doc["counters"]),
            where + ".execution: anything captured ran under a verified supervisor")
    execution = doc["execution"]
    contained = execution["mode"] == "linux_container"
    require(contained == (execution["containment"] == "pid_namespace")
            == (execution["scratch_bound"] == "verified_disk_bounded")
            == (execution["container"] is not None),
            where + ".execution: exactly a contained run has PID-namespace containment, a "
            "verified disk-bounded scratch and its container")
    require(execution["matches_environment"]
            == (execution["declared_containment"]
                == ("linux_container" if contained else "trusted_local_process_group")),
            where + ".execution.matches_environment: a mode matches only the environment that "
            "declares it")
    require(doc["budgets"]["cases"]["used"] == len(selected),
            where + ".budgets.cases: one per selected case")
    require(execution["started_unix_ms"] <= execution["ended_unix_ms"]
            and execution["started_unix_ms"] <= execution["deadline_unix_ms"],
            where + ".execution: ordered instants")
    tools = [tool["name"] for tool in doc["tools"]]
    require(len(tools) == len(set(tools)), where + ".tools: each tool once")
    for index, tool in enumerate(doc["tools"]):
        at = "%s.tools[%d]" % (where, index)
        require((tool["state"] == "available") == (tool["executable_sha256"] is not None),
                at + ": exactly an available tool has an executable identity")
        require((tool["version_provenance"] == "tool_reported")
                == (tool["reported_version"] is not None),
                at + ": a tool-reported version is exactly one the tool printed")
        require(tool["version_provenance"] != "declared_pin_only"
                or tool["declared_version"] is not None,
                at + ": a declared-pin version names its pin")
    paths = []
    seen = {}
    for index, capture in enumerate(doc["captures"]):
        at = "%s.captures[%d]" % (where, index)
        require(capture["instance_id"] in selected, at + ": a capture of a selected case")
        key = (capture["instance_id"], capture["side"], capture["tool"])
        seen.setdefault(key, []).append(capture)
        require((capture["unwind"] == "dwarf") == (capture["dwarf_stack_bytes"] is not None),
                at + ".dwarf_stack_bytes: exactly a DWARF capture declares its 32-KiB stack")
        if capture["attempt"] == 2:
            require(capture["tool"] == "perf" and capture["unwind"] == "dwarf",
                    at + ": the one additional capture is perf's declared DWARF alternative")
        else:
            require(capture["tool"] == "samply" or capture["unwind"] == "frame_pointer",
                    at + ": a first perf capture unwinds with frame pointers")
        region = capture["region"]
        bracketed = capture["tool"] == "perf" and capture["subject"] == "probe_region"
        require((region["mechanism"] == "acknowledged_handshake") == bracketed,
                at + ".region: exactly perf's capture of a probe records its acknowledged "
                "region; samply and a CLI process are whole processes")
        if capture["omission"] is None:
            require(capture["artifacts"] and region["observed"]
                    and capture["samples"] is not None and capture["samples"] > 0,
                    at + ": a capture without omission observed its region and has samples "
                    "and artifacts")
        require(region["observed"] == (region["repetitions"] > 0)
                and (region["stopped"] == "not_started") == (region["repetitions"] == 0),
                at + ".region: observed exactly when a repetition ran, and only then stopped "
                "for a reason other than not_started")
        losses = capture["losses"]
        decoded = [losses[key] for key in ("lost_event_records", "lost_events",
                                           "lost_sample_records", "potentially_lost_samples")]
        require(all(value is not None for value in decoded) if losses["state"] == "decoded"
                else all(value is None for value in decoded) and losses["detail"],
                at + ".losses: every decoded count, or all unknown with the reason")
        require(losses["state"] != "decoded"
                or ((decoded[0] == 0) <= (decoded[1] == 0)
                    and (decoded[2] == 0) <= (decoded[3] == 0)),
                at + ".losses: no payload without its records")
        require((region["process"] == "fresh_exec") == bracketed,
                at + ".region.process: perf's repetitions of a probe are fresh executions in "
                "its one recording, every other repetition a fresh process")
        if capture["unknown_frame_ratio"] is not None:
            _ratio_at_most_one(capture["unknown_frame_ratio"], at + ".unknown_frame_ratio")
            require(capture["frames"] is not None and capture["unknown_frames"] is not None
                    and capture["frames"] > 0 and Fraction(capture["unknown_frames"],
                                                           capture["frames"])
                    == parse_rational(capture["unknown_frame_ratio"], at),
                    at + ".unknown_frame_ratio: unknown frames over all frames")
        if capture["effective_frequency_hz"] is not None:
            require(parse_rational(capture["effective_frequency_hz"], at) > 0,
                    at + ".effective_frequency_hz: positive")
        paths.extend(item["path"] for item in capture["artifacts"])
    for key, attempts in seen.items():
        numbers = sorted(capture["attempt"] for capture in attempts)
        require(numbers == list(range(1, len(numbers) + 1)),
                where + ": capture attempts of %s/%s/%s are 1 then 2" % key)
    kinds = {}
    for index, render in enumerate(doc["renders"]):
        at = "%s.renders[%d]" % (where, index)
        require(render["instance_id"] in selected, at + ": a render of a selected case")
        difference = render["kind"].startswith("difference")
        require((render["color"] == "sample_share_change") == difference
                and len(render["inputs"]) == (2 if difference else 1),
                at + ": a difference compares two inputs by sample share change")
        kinds.setdefault(render["instance_id"], []).append(render["kind"])
        paths.extend([render["original"]["path"], render["sanitized"]["path"]])
    for instance, rendered in kinds.items():
        require(len(rendered) == len(set(rendered)), where + ".renders: each kind once")
        require(("difference_forward" in rendered) == ("difference_reverse" in rendered),
                where + ".renders: a difference is rendered in both directions, so stacks that "
                "disappear stay visible (%s)" % instance)
    for index, trace in enumerate(doc["traces"]):
        at = "%s.traces[%d]" % (where, index)
        require(trace["instance_id"] in selected, at + ": a trace of a selected case")
        require((trace["omission"] is None) == bool(trace["artifacts"])
                == (trace["coverage"] is not None),
                at + ": a trace retains artifacts and states its coverage, or records an "
                "omission")
        require(trace["region_phase"] == "probe_region" if trace["subject"] == "probe_region"
                else trace["region_phase"] in CLI_REGION_PHASES,
                at + ".region_phase: a probe's region, or a CLI process's startup or command")
        coverage = trace["coverage"]
        if coverage is not None:
            require((coverage == "region_missing") == (trace["region_spans"] == 0),
                    at + ".coverage: the region is missing exactly when no span of it was kept")
            require(coverage != "complete" or (trace["dropped"] == 0 and trace["open"] == 0),
                    at + ".coverage: a complete trace dropped no span and left none open")
            require(coverage != "partial" or trace["dropped"] > 0 or trace["open"] > 0,
                    at + ".coverage: a partial trace dropped a span or left one open")
            require(trace["region_spans"] <= trace["spans"],
                    at + ".region_spans: the region's spans are among the trace's")
        overhead = trace["overhead"]
        if overhead is not None:
            require(len(overhead["off_ms"]) == len(overhead["on_ms"]) == overhead["pairs"],
                    at + ".overhead: one off and one on sample per pair")
            require(overhead["off_recipe_digest"] != overhead["on_recipe_digest"],
                    at + ".overhead: compares two recipes")
            expected = trace_overhead(overhead["off_ms"], overhead["on_ms"], at)
            require(parse_rational(overhead["median_overhead"], at) == expected,
                    at + ".overhead.median_overhead: the median of per-pair relative overheads")
            label = ("perturbing" if expected > parse_decimal(overhead["target"], at)
                     else "within_target")
            require(overhead["label"] == label, at + ".overhead.label: follows from the target")
        paths.extend(item["path"] for item in trace["artifacts"])
    for index, counter in enumerate(doc["counters"]):
        at = "%s.counters[%d]" % (where, index)
        require(counter["instance_id"] in selected, at + ": a counter of a selected case")
        require((counter["omission"] is None) == (counter["value"] is not None),
                at + ": a counter value or an omission")
        require((counter["tool"] == "callgrind") == (counter["metric"] == "instructions"),
                at + ": callgrind counts instructions, dhat allocations")
        paths.extend(item["path"] for item in counter["artifacts"])
    paths.extend(_validate_telemetry(doc, selected, where))
    require(len(paths) == len(set(paths)), where + ": every artifact path once")
    return doc


def _validate_telemetry(doc, selected, where):
    """The telemetry rules of a child run (ADR-0214); the artifact paths its captures name."""
    telemetry = doc["telemetry"]
    level = telemetry["level"]
    at = where + ".telemetry"
    require(telemetry["perturbing"] == (level == "content"),
            at + ".perturbing: exactly the content level is labelled perturbing")
    require(level is not None or (not telemetry["captures"] and all(
        item["kind"] != "telemetry" for item in doc["omissions"])),
            at + ": no telemetry capture or omission without an explicitly selected level")
    require(level is None or doc["recipe"]["kind"] in ("cpu_profile", "trace"),
            at + ": telemetry is captured beside CPU profiles or phase traces, never counters")
    paths = []
    keys = set()
    for index, item in enumerate(telemetry["captures"]):
        here = "%s.captures[%d]" % (at, index)
        require(item["instance_id"] in selected, here + ": a capture of a selected case")
        require(item["level"] == level, here + ".level: the child run's selected level")
        key = (item["instance_id"], item["side"])
        require(key not in keys, here + ": one capture per case and side")
        keys.add(key)
        names = sorted(artifact["path"].rsplit("/", 1)[-1] for artifact in item["artifacts"])
        if item["omission"] is None:
            require(item["run"] is not None and item["af"] is not None
                    and item["evidence"] is not None and item["executions"]
                    and item["command_wall_ms"] is not None,
                    here + ": a capture without omission names its run, executions, evidence "
                    "and its execution's wall time")
            require(names == sorted(TELEMETRY_DOCUMENTS),
                    here + ".artifacts: exactly the four documents it was read from and its "
                    "Perfetto export")
            evidence = item["evidence"]
            require(evidence["reasons"] == sorted(evidence["reasons"]),
                    here + ".evidence.reasons: sorted")
            require((evidence["state"] == "complete") == (not evidence["reasons"]),
                    here + ".evidence: complete exactly when no reason makes it partial")
            facts = (("open_spans", evidence["open_spans"] > 0),
                     ("unsealed_segment", evidence["segments"]["unsealed"] > 0),
                     ("live_segment", evidence["segments"]["live"] > 0),
                     ("corrupt_segment", evidence["segments"]["corrupt"] > 0),
                     ("problems_recorded", evidence["problems"] > 0),
                     ("unreadable_files", evidence["unreadable"] > 0),
                     ("config_incomplete", evidence["config"] != "complete"),
                     ("index_absent", evidence["index"] == "absent"),
                     ("history_partial", evidence["history"] == "partial"),
                     ("open_execution", any(execution["open"]
                                            for execution in item["executions"])),
                     ("unsealed_execution", any(execution["segment"] != "sealed"
                                                for execution in item["executions"])))
            for reason, holds in facts:
                require(not holds or reason in evidence["reasons"],
                        here + ".evidence: %s is a reason it is partial" % reason)
            timeline = item["timeline"]
            require(len(timeline["spans"]) + timeline["omitted"] == item["spans"],
                    here + ".timeline: draws its spans and counts every other one")
            open_spans = sum(1 for span in timeline["spans"] if span["status"] == "open")
            require(open_spans <= evidence["open_spans"],
                    here + ".timeline: no more open spans than the evidence counts")
            for span in timeline["spans"]:
                require((span["status"] == "open") == (span["total_us"] is None)
                        == (span["self_us"] is None) == (span["lane"] == "instant"),
                        here + ".timeline: an open span has no total and is an instant")
            phases = [row["name"] for row in item["phases"]]
            require(phases == sorted(set(phases)), here + ".phases: each name once, sorted")
            require(sum(row["spans"] for row in item["phases"]) == item["spans"],
                    here + ".phases: every span under its name")
        else:
            require(item["run"] is None and item["evidence"] is None and not item["executions"]
                    and not item["phases"] and item["spans"] == 0
                    and not item["timeline"]["spans"] and item["timeline"]["omitted"] == 0,
                    here + ": an omitted capture claims no run, phase or timeline; what it wrote "
                    "stays counted in its artifacts")
        paths.extend(artifact["path"] for artifact in item["artifacts"])
    return paths


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def trace_overhead(off_ms, on_ms, where="overhead"):
    """The median of the paired relative overheads (on - off) / off, exactly; a pair whose
    uninstrumented time is zero has no defined relative overhead and is refused."""
    require(len(off_ms) == len(on_ms) and off_ms, where + ": paired samples")
    ratios = []
    for off, on in zip(off_ms, on_ms):
        off_value = parse_decimal(off, where)
        require(off_value > 0, where + ": an uninstrumented sample of zero time")
        ratios.append((parse_decimal(on, where) - off_value) / off_value)
    return median(ratios)
