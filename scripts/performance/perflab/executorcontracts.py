"""Closed contracts of the independent operator executor (ADR-0240).

Measurement formats stay unchanged: these receipts bind their native evidence to an actual
backend. Local originals never inhabit an Actions ID field.
"""
from .spec import Arr, Bool, Const, Field, Int, Nullable, Obj, Str
from .values import canonical_digest, require


def register(c):
    ID, SHA, DIGEST = c.ID, c.GIT_SHA, c.DIGEST
    path = Str(pattern=r"^/[^\x00\n]{1,1023}$", max_len=1024)
    backend = Str(enum=("local", "github_actions"))
    # An explicitly configured host tool or tool directory: never searched for on PATH.
    tool = Str(pattern=r"^/[A-Za-z0-9_./+-]{1,1023}$", max_len=1024)
    configuration = Obj(
        Field("schema", Const("af.perf-executor/3")), Field("backend", backend),
        Field("root", path), Field("repository_path", path), Field("repository", c.REPOSITORY),
        Field("control_sha", SHA), Field("trusted_ref", Str(pattern=r"^refs/heads/[A-Za-z0-9._/-]+$")),
        Field("machine_id", ID), Field("journal_id", ID),
        Field("build_image", c.images.REFERENCE), Field("sample_image", c.images.REFERENCE),
        Field("zigbuild_bin", Nullable(Str(pattern=r"^/[A-Za-z0-9_./+-]{1,1023}$", max_len=1024)), required=False),
        Field("max_requests", Int(1, 4096)),
        Field("max_queue", Int(1, 256)), Field("max_bytes", Int(1, c.limits.ARCHIVE_BYTES_MAX)),
        Field("archive_bytes", Int(1, c.limits.ARCHIVE_BYTES_MAX)),
        Field("history_bytes", Int(1, c.limits.ARCHIVE_BYTES_MAX)),
        Field("run_seconds", Int(120, 28800)), Field("cycle_seconds", Int(1, 300)),
        Field("poll_main", Bool()),
        Field("core", Obj(Field("tier", Str(enum=c.TIERS)), Field("environment", ID),
                          Field("shards", Int(1, 16)), Field("cases", Arr(ID, 0, 64, unique=True)))),
        Field("telemetry", Obj(Field("profile", Str(enum=c.TELEMETRY_PROFILES)),
             Field("experiments", Arr(Str(enum=c.TELEMETRY_EXPERIMENTS), 1, 3, unique=True)),
             Field("rounds", Int(1, c.TELEMETRY_ROUNDS_MAX)), Field("pairs", Nullable(Int(1, 40)), required=False),
             Field("scale", Nullable(Str(enum=("smoke", "full"))), required=False),
             Field("corpus_runs", Nullable(Int(1, 10000)), required=False))),
        Field("diagnostic", Obj(Field("recipe", ID), Field("cases", Arr(ID, 0, 16, unique=True)),
             Field("max_cases", Nullable(Int(1, c.limits.DIAGNOSTIC_CASES_MAX)), required=False),
             Field("perf", Nullable(tool), required=False),
             Field("inferno_bin", Nullable(tool), required=False),
             # The CPU observer of a contained CPU profile: perf inside the container (null) or
             # the trusted host observer of the owned contained process (`host_perf`).
             Field("observer", Nullable(Str(enum=("host_perf",))), required=False),
             Field("total_minutes", Int(1, c.limits.DIAGNOSTIC_RUN_MINUTES_MAX)),
             Field("duration_seconds", Int(1, c.limits.DIAGNOSTIC_SECONDS_MAX)),
             Field("frequency_hz", Int(1, c.limits.DIAGNOSTIC_HZ_MAX)),
             Field("telemetry", Nullable(Str(enum=("spans", "content"))), required=False))))
    frozen = Obj(Field("baseline_sha", SHA), Field("candidate_sha", SHA),
                 Field("control_sha", SHA), Field("authority_sha", SHA))
    submission = Obj(Field("kind", Str(enum=("manual", "main", "pr", "release", "calibration"))),
                     Field("key", Str(min_len=1, max_len=256)))
    # A profile's verified parent comparison: the completed local core original whose retained
    # plan and verdict choose and bind the diagnosed cases (the exact parent identity).
    parent = Obj(Field("request_id", ID), Field("request_digest", DIGEST),
                 Field("result_sha256", DIGEST), Field("plan_sha256", DIGEST),
                 Field("verdict_sha256", DIGEST))
    # A core child of a durable comparison: the shards of the comparison's one frozen plan it
    # executes, and the earlier child whose verified immutable build it reuses (none: it builds).
    shard = Int(0, 15)
    partition = Obj(Field("comparison_id", ID), Field("comparison_digest", DIGEST),
                    Field("index", Int(0, 15)), Field("shards", Arr(shard, 1, 16, unique=True)),
                    Field("build_from", Nullable(ID)))
    request = Obj(Field("schema", Const("af.perf-executor-request/2")), Field("request_id", ID),
        Field("backend", backend), Field("journal_id", ID), Field("machine_id", ID),
        Field("configuration_digest", DIGEST), Field("sources", frozen),
        Field("submission", submission), Field("work", Str(enum=("core", "telemetry", "profile"))),
        Field("created_unix_ms", c.UNIX_MS), Field("environment_digest", DIGEST),
        Field("producer_digest", DIGEST), Field("plan_sha256", Nullable(DIGEST)),
        Field("cohort_digest", Nullable(DIGEST)), Field("member_id", Nullable(ID)),
        Field("parameters_sha256", Nullable(DIGEST)), Field("parent", Nullable(parent)),
        Field("partition", Nullable(partition)))
    seconds = Int(0, 10 ** 9)
    serial = Obj(Field("backend", Const("local_serial")), Field("shards", Int(1, 256)),
        Field("build_seconds", seconds), Field("shard_sampling_seconds", seconds),
        Field("finish_reserve_seconds", seconds), Field("needed_seconds", seconds),
        Field("run_total_seconds", seconds), Field("run_seconds", seconds),
        Field("available_seconds", seconds), Field("fitting_shards", Int(0, 10 ** 6)),
        Field("feasible", Bool()))
    preflight = Obj(Field("schema", Const("af.perf-executor-preflight/1")),
        Field("work", Str(enum=("core", "telemetry", "profile"))),
        Field("status", Str(enum=("ready", "refused"))), Field("detail", Str(max_len=4096)),
        Field("request", Nullable(request)), Field("serial", Nullable(serial)),
        Field("tools", Arr(Obj(Field("name", ID), Field("state", Str(enum=("available", "unavailable"))),
                               Field("detail", Str(max_len=4096))), 0, 16)))

    def validate_preflight(doc, where="executor preflight"):
        preflight.validate(doc, where)
        require((doc["status"] == "ready") == (doc["request"] is not None and not doc["detail"]),
                "a ready preflight carries its request and no refusal")
        if doc["request"] is not None:
            validate_request(doc["request"], where + ".request")
        require(doc["status"] != "ready" or doc["serial"] is None or doc["serial"]["feasible"],
                "an infeasible serial schedule is never ready")
        return doc

    count = Int(0, 2 ** 31)
    overview_member = Obj(Field("request_id", ID),
        Field("work", Str(enum=("core", "telemetry", "profile"))), Field("submission", submission),
        Field("plan_sha256", Nullable(DIGEST)), Field("parent", Nullable(ID)),
        Field("state", Str(enum=("queued", "claimed", "complete", "failed", "interrupted",
                                 "uncertain", "submitted", "expired"))),
        Field("state_detail", Str(max_len=4096)),
        Field("started_unix_ms", Nullable(c.UNIX_MS)), Field("ended_unix_ms", Nullable(c.UNIX_MS)),
        Field("verification", Obj(Field("status", Str(enum=("verified", "refused", "not_terminal"))),
                                  Field("detail", Str(max_len=4096)))),
        Field("measurement_status", Nullable(Str(max_len=128))),
        Field("diagnostic_kind", Nullable(Str(enum=("cpu_profile", "trace", "counters")))),
        Field("coverage", Nullable(Obj(Field("required", count), Field("observed", count),
                                       Field("failed", count), Field("missing", count)))),
        Field("performance", Obj(Field("verdict", Nullable(Str(max_len=32))),
                                 Field("regressions", Arr(ID, 0, 4096)))),
        Field("links", Arr(c.REL_PATH, 0, 16)), Field("reasons", Arr(Str(max_len=4096), 0, 64)))
    comparison = Obj(Field("schema", Const("af.perf-comparison-overview/1")),
        Field("journal_id", ID), Field("machine_id", ID), Field("sources", frozen),
        Field("scope", Str(enum=("full", "partial"))),
        Field("status", Str(enum=("complete", "partial", "incomplete"))),
        Field("reasons", Arr(Str(max_len=4096), 0, 1024)),
        Field("members", Arr(overview_member, 1, 16)))

    def validate_comparison(doc, where="comparison overview"):
        comparison.validate(doc, where)
        ids = [m["request_id"] for m in doc["members"]]
        require(len(set(ids)) == len(ids), "an overview repeats an original")
        for m in doc["members"]:
            require((m["verification"]["status"] == "not_terminal") == (m["state"] != "complete"),
                    "only a complete original is verified or refused")
            require(m["verification"]["status"] == "verified" or (m["coverage"] is None
                    and m["performance"]["verdict"] is None and m["measurement_status"] is None),
                    "unverified evidence carries no coverage, verdict or measurement")
            require(m["coverage"] is None or m["coverage"]["observed"] + m["coverage"]["missing"]
                    == m["coverage"]["required"], "coverage must account for every required item")
            require(not m["reasons"] or all(r in doc["reasons"] for r in m["reasons"]),
                    "every member reason is a comparison reason")
        for m in doc["members"]:
            require(m["diagnostic_kind"] is None or (m["work"] == "profile"
                    and m["verification"]["status"] == "verified"),
                    "only a verified profile names its diagnostic kind")
        kinds = set((m["work"], m["diagnostic_kind"]) for m in doc["members"])
        cores = set(m["request_id"] for m in doc["members"] if m["work"] == "core")
        covered = (not doc["reasons"]
                   and {("core", None), ("telemetry", None), ("profile", "trace"),
                        ("profile", "cpu_profile")} <= kinds
                   and all(m["state"] == "complete" and m["verification"]["status"] == "verified"
                           and not m["reasons"] and (m["work"] != "profile" or m["parent"] in cores)
                           and (m["work"] != "core" or m["performance"]["verdict"] != "incomplete")
                           for m in doc["members"]))
        require((doc["status"] == "complete") == (doc["scope"] == "full" and covered),
                "a comparison is complete only when its full scope's every required original is "
                "complete, verified, healthy and fully covered")
        require((doc["status"] == "partial") == (doc["scope"] == "partial" and not doc["reasons"]),
                "a partial scope is never a complete release comparison")
        return doc

    states = ("queued", "claimed", "complete", "failed", "interrupted", "uncertain", "submitted", "expired")
    states_all = states
    state = Obj(Field("schema", Const("af.perf-executor-state/2")), Field("request_id", ID),
        Field("request_digest", DIGEST), Field("state", Str(enum=states)),
        Field("started_unix_ms", Nullable(c.UNIX_MS)), Field("ended_unix_ms", Nullable(c.UNIX_MS)),
        Field("result_sha256", Nullable(DIGEST)), Field("detail", Str(max_len=4096)))
    outcome = Obj(Field("schema", Const("af.perf-executor-result/2")), Field("request_id", ID),
        Field("request_digest", DIGEST), Field("backend", backend),
        Field("machine_id", ID), Field("environment_digest", DIGEST),
        Field("producer_digest", DIGEST), Field("sources", frozen), Field("host", c.HOST),
        Field("status", Str(enum=("complete", "failed", "interrupted", "uncertain", "submitted", "expired"))),
        Field("measurement_status", Str(max_len=128)), Field("required_check", Const(False)),
        Field("started_unix_ms", c.UNIX_MS), Field("ended_unix_ms", c.UNIX_MS),
        Field("evidence", Arr(Obj(Field("path", c.REL_PATH), Field("sha256", DIGEST),
                                  Field("size", Int(0, c.limits.ARCHIVE_BYTES_MAX))), 0, 8192)),
        Field("actions", Nullable(Obj(Field("repository", c.REPOSITORY),
                         Field("workflow", Str(enum=("performance.yml", "performance-telemetry.yml"))),
                         Field("control_sha", SHA), Field("requested_ref", c.SHORT_TEXT),
                         Field("receipt", Str(max_len=2048))))),
        Field("detail", Str(max_len=4096)))
    # A separately named current contract, not a migration of the hosted v4 declaration.
    replacements = {
        "schema": Const("af.perf-local-cohort/1"),
        "schedule": Obj(Field("start_date", c.DATE), Field("minimum_days", Int(7, c.DAYS_MAX)),
                        Field("runners", Const("persistent_vm")), Field("order", Const(c.COHORT_ORDER))),
        "dispatch": Obj(Field("journal_id", ID), Field("machine_id", ID),
                        Field("environment_digest", DIGEST), Field("producer_digest", DIGEST)),
    }
    local_cohort = Obj(*(Field(f.name, replacements.get(f.name, f.node), f.required)
                         for f in c.COHORT.fields))
    provenance = Obj(Field("request_id", ID), Field("request_digest", DIGEST),
        Field("result_sha256", Nullable(DIGEST)), Field("plan_digest", Nullable(DIGEST)),
        Field("started_unix_ms", Nullable(c.UNIX_MS)), Field("ended_unix_ms", Nullable(c.UNIX_MS)))
    member = Obj(*(Field(f.name, Nullable(provenance) if f.name == "provenance" else f.node,
                         f.required) for f in c.READINESS_MEMBER.fields))
    readiness_replacements = {
        "schema": Const("af.perf-local-readiness/1"), "members": Arr(member, 1, 256),
        "history": Obj(Field("journal_id", ID), Field("journal_digest", DIGEST),
                       Field("requests", Int(0, 4096))),
        "refused_evidence": Arr(Str(max_len=4096), 0, 256),
    }
    readiness = Obj(*(Field(f.name, readiness_replacements.get(f.name, f.node), f.required)
                      for f in c.READINESS.fields))

    def validate_config(doc, where="executor"):
        configuration.validate(doc, where)
        doc.setdefault("zigbuild_bin", None)
        if doc["zigbuild_bin"] is not None:
            import posixpath
            require("\n" not in doc["zigbuild_bin"]
                    and posixpath.normpath(doc["zigbuild_bin"]) == doc["zigbuild_bin"],
                    "zigbuild_bin: use a canonical image-local absolute directory")
        require(doc["backend"] != "local" or doc["core"]["environment"] != "linux_x86_64_musl_claw"
                or doc["zigbuild_bin"] is not None, "local musl requires zigbuild_bin in the pinned image")
        for key in ("pairs", "scale", "corpus_runs"):
            doc["telemetry"].setdefault(key, None)
        for key in ("telemetry", "max_cases", "perf", "inferno_bin", "observer"):
            doc["diagnostic"].setdefault(key, None)
        for key in ("perf", "inferno_bin"):
            value = doc["diagnostic"][key]
            import posixpath
            require(value is None or posixpath.normpath(value) == value,
                    "diagnostic." + key + ": use a canonical absolute host path")
        require(not (doc["diagnostic"]["cases"] and doc["diagnostic"]["max_cases"] is not None),
                "diagnostic: max_cases bounds a parent-selected profile; named cases are explicit")
        require(doc["archive_bytes"] + doc["history_bytes"] < doc["max_bytes"],
                "archive/history must leave capacity for original receipts and work")
        require(doc["max_queue"] <= doc["max_requests"], "queue exceeds journal bound")
        for key in ("root", "repository_path"):
            import os
            require(os.path.normpath(doc[key]) == doc[key], key + ": use canonical absolute paths")
        return doc

    def validate_result(doc, where="executor result"):
        outcome.validate(doc, where)
        require(doc["ended_unix_ms"] >= doc["started_unix_ms"], "executor outcome ends before its claim")
        require(doc["backend"] != "local" or (doc["actions"] is None and doc["status"] != "submitted"),
                "local outcome cannot claim Actions provenance")
        require(doc["backend"] != "github_actions" or doc["actions"] is not None,
                "hosted submission must retain its transport receipt")
        if doc["status"] == "expired":
            require(doc["backend"] == "local" and doc["measurement_status"] == "not_executed_off_schedule",
                    "expired calibration outcome must state non-execution off schedule")
        paths = [item["path"] for item in doc["evidence"]]
        require(paths == sorted(set(paths)), "executor evidence paths must be sorted and unique")
        return doc

    def validate_local_readiness(doc, where="local readiness", cohort=None, calibration=None):
        readiness.validate(doc, where)
        ids = [m["member_id"] for m in doc["members"]]
        require(len(set(ids)) == len(ids), "local readiness repeats a member")
        require(doc["calibration_digest"] == doc["recomputed_calibration_digest"],
                "parameters differ from the original A/A and preparation evidence")
        for m in doc["members"]:
            require((m["status"] == "missing") == any(r in c.MISSING_REASONS for r in m["reasons"]),
                    "missing local original must state missing evidence")
            require((m["status"] == "complete") == (not m["reasons"]), "member reasons disagree")
            require((m["status"] == "complete") == (m["verdict_sha256"] is not None),
                    "only a complete original has a verdict")
            require((m["provenance"] is None) == ("no_dispatch" in m["reasons"]),
                    "every original binds a local request")
            require(m["status"] == "complete" or not m["cases"], "incomplete original has cases")
            if m["status"] == "complete":
                require(m["provenance"]["result_sha256"] is not None
                        and m["provenance"]["started_unix_ms"] is not None
                        and m["provenance"]["plan_digest"] is not None,
                        "complete original lacks its receipt")
                require(set(m["cpu_groups"]) == set(m["cases"])
                        and all(g is not None for g in m["cpu_groups"].values()),
                        "complete local original must bind every case to its observed CPU group")
            c.validate_readiness_injection(m, where + ".members." + m["member_id"])
        if cohort is not None:
            c.validate_cohort(cohort)
            require(doc["cohort_digest"] == canonical_digest(cohort), "local cohort digest mismatch")
            require(doc["history"]["journal_id"] == cohort["dispatch"]["journal_id"],
                    "local readiness journal mismatch")
            require([(m["member_id"], m["kind"]) for m in doc["members"]] ==
                    [(m["member_id"], m["kind"]) for m in cohort["members"]],
                    "all original members must remain in their denominators")
            cases = {case["instance_id"] for case in cohort["cases"]}
            allowed = {group["group_id"] for group in cohort["allowed_cpu_groups"]}
            for m in doc["members"]:
                require(set(m["cpu_groups"]) <= cases and all(g is None or g in allowed
                        for g in m["cpu_groups"].values()), "local original has foreign cases or CPU groups")
                require(m["status"] != "complete" or set(m["cases"]) == cases,
                        "complete local original must contain every declared case")
            if calibration is not None:
                c.validate_readiness_totals(doc, where, cohort, calibration)
        return doc

    def validate_request(doc, where="executor request"):
        request.validate(doc, where)
        require(doc["parent"] is None or doc["work"] == "profile",
                "only a profile names a parent comparison")
        require(doc["parent"] is None or doc["plan_sha256"] == doc["parent"]["plan_sha256"],
                "a parent-selected profile diagnoses its parent's exact plan")
        require(doc["partition"] is None or (doc["work"] == "core" and doc["plan_sha256"]
                is not None and doc["cohort_digest"] is None),
                "only a core original is a partition of a comparison's plan")
        require(doc["partition"] is None or (doc["partition"]["index"] == 0)
                == (doc["partition"]["build_from"] is None),
                "exactly a comparison's first partition builds; later ones reuse its build")
        return doc

    # The trusted host CPU observer's record of one recording (`hostobserver`).
    target = Obj(Field("pid", Int(1, 2 ** 31)), Field("start_ticks", Int(0, 2 ** 63)),
                 Field("uid", Int(0, 2 ** 32)), Field("cmdline_sha256", DIGEST),
                 Field("container_id", Nullable(Str(pattern=r"^[0-9a-f]{64}$", max_len=64))),
                 Field("init_pid", Nullable(Int(1, 2 ** 31))))
    observer = Obj(Field("schema", Const("af.perf-host-observer/1")),
        Field("observer", Const("host_perf")), Field("perf_sha256", Nullable(DIGEST)),
        Field("event", Str(min_len=1, max_len=256)),
        Field("region", Str(enum=("whole_command", "acknowledged_region"))),
        Field("target", Nullable(target)), Field("attached_before_release", Bool()),
        Field("attach_ms", Nullable(Int(0, 10 ** 9))), Field("exit_code", Nullable(Int(-255, 255))),
        Field("stopped_confirmed", Bool()), Field("detail", Str(max_len=4096)))

    def validate_observer(doc, where="host observer"):
        observer.validate(doc, where)
        require(not doc["attached_before_release"] or (doc["target"] is not None
                and doc["attach_ms"] is not None),
                "an attachment names its frozen target and when it was acknowledged")
        return doc

    lanes = ("core", "telemetry", "trace", "cpu_profile")
    lane = Str(enum=lanes)
    key = Str(min_len=1, max_len=256)
    schedule = Obj(Field("backend", Const("local_serial")), Field("shards", Int(1, 16)),
        Field("build_seconds", seconds), Field("reuse_seconds", seconds),
        Field("shard_sampling_seconds", seconds), Field("finish_reserve_seconds", seconds),
        Field("available_seconds", seconds), Field("first_child_shards", Int(1, 16)),
        Field("later_child_shards", Int(1, 16)), Field("children", Int(1, 16)))
    child = Obj(Field("index", Int(0, 15)), Field("shards", Arr(shard, 1, 16, unique=True)),
                Field("key", key), Field("request_id", ID), Field("build_from", Nullable(ID)),
                Field("needed_seconds", seconds))
    diagnostic_lane = Obj(Field("lane", Str(enum=("trace", "cpu_profile"))),
        Field("recipe", Nullable(ID)), Field("observer", Nullable(Str(enum=("host_perf",)))),
        Field("key", key), Field("request_id", Nullable(ID)),
        Field("unsupported", Nullable(Str(max_len=4096))))
    declaration = Obj(Field("schema", Const("af.perf-comparison/1")),
        Field("comparison_id", ID), Field("journal_id", ID), Field("machine_id", ID),
        Field("submission", submission), Field("sources", frozen),
        Field("configuration_digest", DIGEST), Field("environment_digest", DIGEST),
        Field("producer_digest", DIGEST), Field("created_unix_ms", c.UNIX_MS),
        Field("plan_sha256", DIGEST), Field("run_id", ID),
        Field("lanes", Arr(lane, 4, 4, unique=True)), Field("schedule", schedule),
        Field("partitions", Arr(child, 1, 16)),
        Field("telemetry", Obj(Field("key", key), Field("request_id", ID))),
        Field("diagnostics", Arr(diagnostic_lane, 2, 2)))

    def validate_declaration(doc, where="comparison"):
        declaration.validate(doc, where)
        require(doc["lanes"] == list(lanes), "a full comparison declares core, telemetry, trace "
                "and CPU profile lanes")
        children = doc["partitions"]
        covered = [index for item in children for index in item["shards"]]
        require(sorted(covered) == list(range(doc["schedule"]["shards"]))
                and len(children) == doc["schedule"]["children"]
                and [item["index"] for item in children] == list(range(len(children))),
                "a comparison's partitions cover every shard of its plan exactly once, in order")
        require(children[0]["build_from"] is None and all(
                item["build_from"] == children[0]["request_id"] for item in children[1:]),
                "the first partition builds; every later one reuses its verified build")
        ids = [item["request_id"] for item in children] + [doc["telemetry"]["request_id"]] + [
            item["request_id"] for item in doc["diagnostics"] if item["request_id"]]
        require(len(ids) == len(set(ids)), "a comparison names each original once")
        require([item["lane"] for item in doc["diagnostics"]] == ["trace", "cpu_profile"]
                and all((item["recipe"] is None) == (item["unsupported"] is not None)
                        == (item["request_id"] is None) for item in doc["diagnostics"]),
                "each diagnostic lane names its recipe and original, or why it is unsupported")
        return doc

    coverage = Obj(Field("required", count), Field("observed", count), Field("failed", count),
                   Field("missing", count))
    aggregate_child = Obj(Field("index", Int(0, 15)), Field("request_id", ID),
        Field("state", Str(enum=states_all)),
        Field("verification", Str(enum=("verified", "refused", "not_complete"))),
        Field("result_sha256", Nullable(DIGEST)), Field("shards", Arr(shard, 1, 16, unique=True)),
        Field("included", Bool()), Field("detail", Str(max_len=4096)))
    aggregate = Obj(Field("schema", Const("af.perf-comparison-aggregate/1")),
        Field("comparison_id", ID), Field("comparison_digest", DIGEST),
        Field("plan_sha256", DIGEST), Field("builds_sha256", Nullable(DIGEST)),
        Field("partitions", Arr(aggregate_child, 1, 16)),
        Field("verdict_sha256", Nullable(DIGEST)),
        Field("result", Nullable(Str(enum=("pass", "fail", "incomplete")))),
        Field("coverage", Nullable(coverage)), Field("regressions", Arr(ID, 0, 4096)),
        Field("created_unix_ms", c.UNIX_MS))

    def validate_aggregate(doc, where="comparison aggregate"):
        aggregate.validate(doc, where)
        for item in doc["partitions"]:
            require(item["included"] == (item["verification"] == "verified")
                    and (item["verification"] == "not_complete") == (item["state"] != "complete"),
                    "exactly a verified complete partition contributes its shards")
        require((doc["verdict_sha256"] is None) == (doc["result"] is None)
                == (doc["coverage"] is None) and (doc["builds_sha256"] is not None
                                                  or doc["verdict_sha256"] is None),
                "a verdict is computed exactly over a verified build")
        require(doc["coverage"] is None or doc["coverage"]["observed"] + doc["coverage"]["missing"]
                == doc["coverage"]["required"], "coverage accounts for every planned record")
        require(doc["result"] != "pass" or all(item["included"] for item in doc["partitions"]),
                "a missing partition never passes")
        return doc

    member_states = states_all
    verification = Str(enum=("verified", "refused", "not_checked", "not_terminal"))
    report_member = Obj(Field("request_id", ID), Field("lane", lane),
        Field("work", Str(enum=("core", "telemetry", "profile"))), Field("key", key),
        Field("state", Str(enum=member_states + ("not_submitted",))),
        Field("state_detail", Str(max_len=4096)), Field("verification", verification),
        Field("result_sha256", Nullable(DIGEST)),
        Field("measurement_status", Nullable(Str(max_len=128))))
    report_lane = Obj(Field("lane", lane),
        Field("state", Str(enum=("waiting", "queued", "running", "complete", "failed",
                                 "unsupported", "not_applicable"))),
        Field("members", Arr(ID, 0, 16)), Field("coverage", Nullable(coverage)),
        Field("detail", Str(max_len=4096)), Field("links", Arr(c.REL_PATH, 0, 64)))
    report_doc = Obj(Field("schema", Const("af.perf-comparison-report/1")),
        Field("comparison_id", ID), Field("comparison_digest", DIGEST),
        Field("journal_id", ID), Field("machine_id", ID), Field("sources", frozen),
        Field("submission", submission),
        Field("status", Str(enum=("complete", "incomplete", "running"))),
        Field("verification", verification),
        Field("calibration", Obj(Field("eligible", Const(False)), Field("detail", Str(max_len=1024)))),
        Field("performance", Obj(Field("core_verdict", Nullable(Str(enum=("pass", "fail", "incomplete")))),
                                 Field("regressions", Arr(ID, 0, 4096)),
                                 Field("telemetry_verdict", Nullable(Str(max_len=32))))),
        Field("lanes", Arr(report_lane, 4, 4)), Field("members", Arr(report_member, 1, 32)),
        Field("diagnostic_selection", Arr(Obj(Field("lane", Str(enum=("trace", "cpu_profile"))),
            Field("instance_id", ID), Field("selected", Bool()), Field("reason", Str(max_len=256))),
            0, 256)),
        Field("reasons", Arr(Str(max_len=4096), 0, 1024)), Field("links", Arr(c.REL_PATH, 0, 256)))

    def validate_report(doc, where="comparison report"):
        report_doc.validate(doc, where)
        require([item["lane"] for item in doc["lanes"]] == list(lanes),
                "a comparison report states its four lanes in order")
        ids = [item["request_id"] for item in doc["members"]]
        require(len(ids) == len(set(ids)), "a comparison report states each original once")
        settled = all(item["state"] in ("complete", "not_applicable") for item in doc["lanes"])
        require((doc["status"] == "complete") == (settled and not doc["reasons"]
                and doc["verification"] == "verified"),
                "a comparison is complete only when every lane is complete (or has nothing to "
                "diagnose), every original verified and no reason remains")
        require(doc["status"] != "running" or any(item["state"] in ("waiting", "queued", "running")
                for item in doc["lanes"]), "a running comparison has an unfinished lane")
        require(doc["status"] == "complete" or doc["reasons"],
                "an unfinished or incomplete comparison says why")
        return doc

    export = Obj(Field("schema", Const("af.perf-comparison-export/1")),
        Field("comparison_id", ID), Field("comparison_digest", DIGEST),
        Field("report_sha256", DIGEST), Field("status", Str(enum=("complete", "incomplete", "running"))),
        Field("files", Arr(Obj(Field("path", c.REL_PATH), Field("size", Int(0, c.limits.ARCHIVE_BYTES_MAX)),
                               Field("sha256", DIGEST)), 1, 16384)))
    comparison_preflight = Obj(Field("schema", Const("af.perf-comparison-preflight/1")),
        Field("operation", Str(enum=("preflight", "launch"))),
        Field("status", Str(enum=("ready", "refused", "launched"))), Field("detail", Str(max_len=4096)),
        Field("unix_ms", c.UNIX_MS), Field("submission", submission),
        Field("comparison", Nullable(declaration)),
        Field("checks", Arr(Obj(Field("name", ID),
                                Field("state", Str(enum=("passed", "refused", "not_checked"))),
                                Field("detail", Str(max_len=4096))), 0, 64)))

    def validate_comparison_preflight(doc, where="comparison preflight"):
        comparison_preflight.validate(doc, where)
        require((doc["status"] == "refused") == bool(doc["detail"])
                and (doc["status"] == "refused") == (doc["comparison"] is None)
                and (doc["status"] == "launched") == (doc["operation"] == "launch"
                                                      and doc["status"] != "refused"),
                "a refusal carries its reason and no declaration; a ready or launched one its "
                "declaration")
        if doc["comparison"] is not None:
            validate_declaration(doc["comparison"], where + ".comparison")
        return doc

    c.LOCAL_COHORT = local_cohort
    c.validate_executor = validate_config
    c.validate_local_readiness = validate_local_readiness
    entries = {
        "machine-lock-anchor": (Obj(Field("schema", Const("af.perf-machine-lock-anchor/1")),
            Field("device", Int(0, 2 ** 64 - 1)), Field("inode", Int(1, 2 ** 64 - 1))), None),
        "executor-journal": (Obj(Field("schema", Const("af.perf-executor-journal/1")),
            Field("journal_id", ID), Field("repository", c.REPOSITORY),
            Field("originals", Arr(Obj(Field("request_id", ID), Field("request_digest", DIGEST)), 0, 4096))), None),
        "executor": (configuration, validate_config),
        "executor-request": (request, validate_request),
        "executor-preflight": (preflight, validate_preflight),
        "comparison-overview": (comparison, validate_comparison), "executor-state": (state, None),
        "executor-result": (outcome, validate_result), "local-cohort-declaration": (local_cohort, c.validate_cohort),
        "local-readiness-assessment": (readiness, validate_local_readiness),
        "host-observer": (observer, validate_observer),
        "comparison": (declaration, validate_declaration),
        "comparison-aggregate": (aggregate, validate_aggregate),
        "comparison-report": (report_doc, validate_report),
        "comparison-export": (export, None),
        "comparison-preflight": (comparison_preflight, validate_comparison_preflight),
    }
    for name, (node, validator) in entries.items():
        schema = node.by_name["schema"].node.value
        c.CONTRACTS[name] = (schema, node, validator or (lambda doc, where="executor", n=node: n.validate(doc, where)),
                             "Independent performance executor: " + name + " (ADR-0240).")
