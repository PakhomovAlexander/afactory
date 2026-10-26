"""Faithful reproduction commands of frozen plans (ADR-0177).

A report's rerun and a diagnostic omission's command are instructions a person runs later on a
compatible host. They must select the same experiment the original froze: the same resolved
source, control and authority revisions, the same trigger rule, scope and case family, the same
fixed schedule inputs (shards, pairs, seed), the same environment and build mode, and the same
capture bounds and selection of a diagnostic child. Only a new run identity and its parent link
change; anything a default would choose differently is stated explicitly. A mode that cannot be
reproduced from the plan alone names its input as a documented placeholder (`PLACEHOLDERS`) and
says so in `limitations`, never by silently reconstructing it from a default.

Every revision is passed as the full SHA the plan resolved, and the planner checks each one
against its trigger's rule, so a moved branch or a new tag refuses the rerun instead of choosing
a more convenient baseline.
"""

CLI = ["python3", "scripts/performance/cli.py"]
NEW_RUN_ID = "NEW_RUN_ID"
# Inputs a frozen plan does not hold, each a placeholder the person reproducing supplies.
PLACEHOLDERS = {
    "NEW_RUN_ID": "a new run ID of your choosing; a rerun is a new run",
    "RUN_DIR": "the run directory holding this run's evidence",
    "NEW_REPORT_DIR": "a new directory for the regenerated report",
    "COHORT_JSON": "the calibration cohort file whose canonical digest the plan records",
    "CALIBRATION_JSON": "the sealed calibration parameters whose canonical digest the plan "
                        "records",
    "{output}": "a new directory for the diagnostic child run",
    "{verdict}": "the parent run's verdict.json, whose digest the selection records",
    "{build}": "a verified build of the child plan, when one was given",
    "{parent_build}": "the parent run's verified build, when one was given",
    "{image}": "the sample image pinned by digest that the contained run used",
    "{scratch_fs}": "the verified disk-bounded scratch filesystem's mount point",
    "{perf}": "the configured Linux perf executable",
    "{inferno_bin}": "the directory of the pinned inferno tools",
    "{samply}": "the configured samply executable",
    "{valgrind_bin}": "the directory of valgrind and callgrind_control",
    "{toolchain_bin}": "the recipe toolchain's bin directory",
    "{cargo_home}": "a pre-fetched Cargo home holding every locked dependency",
    "{zigbuild_bin}": "the shipping recipe's cargo-zigbuild and zig directory",
}


def _uniform_pairs(plan):
    pairs = set(case["pairs"] for case in plan["cases"])
    return pairs.pop() if len(pairs) == 1 else None


def rerun(plan):
    """(argv, limitations) of `cli.py plan` freezing the same experiment as `plan` under a new
    run ID whose parent is `plan`'s run."""
    trigger = plan["trigger"]
    resolved = plan["resolved"]
    kind = trigger["kind"]
    argv = CLI + ["plan", "--trigger", kind, "--tier", plan["tier"], "--scope", plan["scope"],
                  "--environment", plan["profile"]["environment_id"],
                  "--shards", str(len(plan["shards"])), "--seed", str(plan["seed"]),
                  "--parent-run-id", plan["run_id"], "--run-id", NEW_RUN_ID]
    limitations = []
    # The revisions, each the SHA the plan resolved, checked by the planner against the
    # trigger's own rule.
    if kind == "pr":
        argv += ["--head", trigger["merge_sha"], "--base", resolved["baseline_sha"],
                 "--pr-base-sha", trigger["base_sha"], "--pr-head-sha", trigger["head_sha"]]
        if trigger["number"] is not None:
            argv += ["--pr-number", str(trigger["number"])]
        limitations.append("the PR merge commit %s and both its parents must be fetched; a "
                           "merge GitHub recomputed later is a different candidate"
                           % trigger["merge_sha"])
    elif kind == "main_push":
        argv += ["--head", trigger["pushed_sha"], "--base", resolved["baseline_sha"]]
    elif kind == "nightly":
        argv += ["--head", trigger["main_sha"], "--base", resolved["baseline_sha"]]
        limitations.append("the nightly baseline is re-derived as the highest stable tag "
                           "reachable from %s and must still be %s (%s); a newer reachable "
                           "release refuses the rerun" % (trigger["main_sha"],
                                                          trigger["baseline_tag"],
                                                          resolved["baseline_sha"]))
    elif kind == "release":
        argv += ["--head", trigger["tag"], "--base", resolved["baseline_sha"]]
        limitations.append("the release tag %s must still name %s"
                           % (trigger["tag"], trigger["tag_sha"]))
    else:
        argv += ["--base", resolved["baseline_sha"], "--head", resolved["candidate_sha"]]
        if trigger["remeasure_of"] is not None:
            argv += ["--remeasure-of", trigger["remeasure_of"]]
    argv += ["--control", resolved["control_sha"]]
    # The authority exactly as the plan selected it: an explicit SHA is a proposed (or, for
    # full scope, a checked active) authority; the control's own registry and a partial run's
    # re-read activation take none, or the rerun would become a proposed-policy run.
    mode = plan["authority"]["mode"]
    if mode == "proposed" or (mode == "active" and plan["scope"] == "full"):
        argv += ["--authority", resolved["authority_sha"]]
    if mode == "active":
        limitations.append("the control revision's activation %s must still bind authority %s"
                           % (plan["authority"]["activation"]["sha256"],
                              resolved["authority_sha"]))
    if plan["scope"] == "partial_scope":
        # Every selected instance, never a truncated prefix.
        for case in plan["cases"]:
            argv += ["--case", case["instance_id"]]
    pairs = _uniform_pairs(plan)
    if plan["scope"] in ("partial_scope", "bootstrap") and pairs is not None:
        argv += ["--pairs", str(pairs)]
    build = plan["profile"]["build"]
    if build["image_source"] == "configured":
        argv += ["--build-image", build["image"]]
    elif build["isolation"] == "trusted_host":
        argv += ["--trusted-host-build"]
    if plan["scope"] == "calibration":
        cohort = plan["cohort"]
        argv += ["--cohort", "COHORT_JSON", "--member", cohort["member_id"]]
        limitations.append("COHORT_JSON must be the cohort %s whose canonical digest is %s"
                           % (cohort["cohort_id"], cohort["cohort_digest"]))
        sealed = (cohort["injection"] or {}).get("calibration_digest")
        if sealed is not None:
            argv += ["--calibration", "CALIBRATION_JSON"]
            limitations.append("CALIBRATION_JSON must be the sealed calibration parameters "
                               "whose canonical digest is %s" % sealed)
    return argv, limitations


def commands(plan):
    """The report's reproduction entries: recompute, regenerate the report, rerun."""
    argv, limitations = rerun(plan)
    return [{"purpose": "recompute", "argv": CLI + ["compare", "--run", "RUN_DIR"],
             "limitations": []},
            {"purpose": "report",
             "argv": CLI + ["report", "--run", "RUN_DIR", "--output", "NEW_REPORT_DIR"],
             "limitations": []},
            {"purpose": "rerun", "argv": argv, "limitations": limitations}]


def profile(options, plan, recipe_tools, mode, selection, instance_id=None):
    """The `cli.py profile` command reproducing a diagnostic child run, or one case of it: the
    parent plan, the selection exactly as it was made (the named cases, or the verdict with its
    case limit), the frozen capture bounds and wall bound, the recipe, mode and build inputs,
    and the configured tools, each host-specific path a documented placeholder. A command for
    the whole run keeps the whole selection, so it reaches its stated capability."""
    diagnostic = plan["diagnostic"]
    argv = CLI + ["profile", "--plan", options.plan, "--recipe", plan["profile"]["recipe_id"],
                  "--mode", mode, "--output", "{output}", "--run-id", NEW_RUN_ID,
                  "--duration-seconds", str(diagnostic["duration_ms"] // 1000),
                  "--frequency-hz", str(diagnostic["frequency_hz"])]
    if options.total_minutes is not None:
        argv += ["--total-minutes", str(options.total_minutes)]
    if instance_id is not None:
        argv += ["--case", instance_id]
    elif selection["mode"] == "explicit":
        for item in selection["cases"]:
            argv += ["--case", item["instance_id"]]
    else:
        argv += ["--verdict", "{verdict}", "--max-cases", str(selection["limit"])]
    if mode == "linux_container":
        argv += ["--image", "{image}", "--scratch-fs", "{scratch_fs}"]
    if options.build:
        argv += ["--build", "{build}"]
    elif options.builder is None:
        argv += ["--no-build"]
    else:
        for flag, placeholder, value in (
                ("--toolchain-bin", "{toolchain_bin}", options.toolchain_bin),
                ("--cargo-home", "{cargo_home}", options.cargo_home),
                ("--zigbuild-bin", "{zigbuild_bin}", options.zigbuild_bin)):
            if value:
                argv += [flag, placeholder]
        if options.build_image:
            argv += ["--build-image", options.build_image]
    if options.parent_build:
        argv += ["--parent-build", "{parent_build}"]
    if getattr(options, "telemetry", None):
        argv += ["--telemetry", options.telemetry]
    flags = {"perf": ["--perf", "{perf}"],
             "inferno_flamegraph": ["--inferno-bin", "{inferno_bin}"],
             "samply": ["--samply", "{samply}"], "valgrind": ["--valgrind-bin", "{valgrind_bin}"]}
    for tool in recipe_tools:
        argv += flags.get(tool, [])
    return argv
