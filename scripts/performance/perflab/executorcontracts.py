"""Closed contracts of the independent operator executor (ADR-0235).

Measurement formats stay unchanged: these receipts bind their native evidence to an actual
backend. Local originals never inhabit an Actions ID field.
"""
from .spec import Arr, Bool, Const, Field, Int, Nullable, Obj, Str
from .values import canonical_digest, require


def register(c):
    ID, SHA, DIGEST = c.ID, c.GIT_SHA, c.DIGEST
    path = Str(pattern=r"^/[^\x00\n]{1,1023}$", max_len=1024)
    backend = Str(enum=("local", "github_actions"))
    configuration = Obj(
        Field("schema", Const("af.perf-executor/1")), Field("backend", backend),
        Field("root", path), Field("repository_path", path), Field("repository", c.REPOSITORY),
        Field("control_sha", SHA), Field("trusted_ref", Str(pattern=r"^refs/heads/[A-Za-z0-9._/-]+$")),
        Field("machine_id", ID), Field("journal_id", ID),
        Field("build_image", c.images.REFERENCE), Field("sample_image", c.images.REFERENCE),
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
        Field("diagnostic", Obj(Field("recipe", ID), Field("cases", Arr(ID, 1, 16, unique=True)),
             Field("total_minutes", Int(1, c.limits.DIAGNOSTIC_RUN_MINUTES_MAX)),
             Field("duration_seconds", Int(1, c.limits.DIAGNOSTIC_SECONDS_MAX)),
             Field("frequency_hz", Int(1, c.limits.DIAGNOSTIC_HZ_MAX)),
             Field("telemetry", Nullable(Str(enum=("spans", "content"))), required=False))))
    frozen = Obj(Field("baseline_sha", SHA), Field("candidate_sha", SHA),
                 Field("control_sha", SHA), Field("authority_sha", SHA))
    submission = Obj(Field("kind", Str(enum=("manual", "main", "pr", "release", "calibration"))),
                     Field("key", Str(min_len=1, max_len=256)))
    request = Obj(Field("schema", Const("af.perf-executor-request/1")), Field("request_id", ID),
        Field("backend", backend), Field("journal_id", ID), Field("machine_id", ID),
        Field("configuration_digest", DIGEST), Field("sources", frozen),
        Field("submission", submission), Field("work", Str(enum=("core", "telemetry", "profile"))),
        Field("created_unix_ms", c.UNIX_MS), Field("environment_digest", DIGEST),
        Field("producer_digest", DIGEST), Field("plan_sha256", Nullable(DIGEST)),
        Field("cohort_digest", Nullable(DIGEST)), Field("member_id", Nullable(ID)),
        Field("parameters_sha256", Nullable(DIGEST)))
    states = ("queued", "claimed", "complete", "failed", "interrupted", "uncertain", "submitted")
    state = Obj(Field("schema", Const("af.perf-executor-state/1")), Field("request_id", ID),
        Field("request_digest", DIGEST), Field("state", Str(enum=states)),
        Field("started_unix_ms", Nullable(c.UNIX_MS)), Field("ended_unix_ms", Nullable(c.UNIX_MS)),
        Field("result_sha256", Nullable(DIGEST)), Field("detail", Str(max_len=4096)))
    outcome = Obj(Field("schema", Const("af.perf-executor-result/1")), Field("request_id", ID),
        Field("request_digest", DIGEST), Field("backend", backend),
        Field("machine_id", ID), Field("environment_digest", DIGEST),
        Field("producer_digest", DIGEST), Field("sources", frozen), Field("host", c.HOST),
        Field("status", Str(enum=("complete", "failed", "interrupted", "uncertain", "submitted"))),
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
        for key in ("pairs", "scale", "corpus_runs"):
            doc["telemetry"].setdefault(key, None)
        doc["diagnostic"].setdefault("telemetry", None)
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
        "executor-request": (request, None), "executor-state": (state, None),
        "executor-result": (outcome, validate_result), "local-cohort-declaration": (local_cohort, c.validate_cohort),
        "local-readiness-assessment": (readiness, validate_local_readiness),
    }
    for name, (node, validator) in entries.items():
        schema = node.by_name["schema"].node.value
        c.CONTRACTS[name] = (schema, node, validator or (lambda doc, where="executor", n=node: n.validate(doc, where)),
                             "Independent performance executor: " + name + " (ADR-0235).")
