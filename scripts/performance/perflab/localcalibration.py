"""Local calibration originals, using the shared statistical judge unchanged (ADR-0235)."""
import os
import time

from . import calibration, compare, contracts, executor, gitrefs
from .values import ContractError, canonical_digest, require, sha256_bytes


def declare(config, cohort_id, groups, start_date, minimum_days=7, preparation_members=2):
    require(config["backend"] == "local", "declare a local cohort only for the local backend")
    git = executor.source_repository(config)
    control = git.resolve(config["control_sha"], "accepted committed producer")
    require(git.is_ancestor(control, git.exact_ref(config["trusted_ref"], "reviewed main")),
            "local producer must be committed on reviewed main")
    from . import planning
    suite = planning.read_suite(git, control)
    c = config["core"]
    require(not c["cases"], "calibration needs the complete critical family")
    recipe = next((p["recipe"] for p in suite.doc["tiers"][c["tier"]]["profiles"]
                   if p["environment"] == c["environment"]), None)
    require(recipe is not None, "local environment is not a tier profile")
    return calibration.declare(suite, control, cohort_id, c["tier"], c["environment"], recipe,
        groups, start_date, config["repository"], 0, minimum_days, preparation_members,
        local={"journal_id": config["journal_id"], "machine_id": config["machine_id"],
               "environment_digest": executor.environment(config),
               "producer_digest": executor.producer(git, control)})


def collect(journal, cohort, sealed=None):
    contracts.validate_cohort(cohort)
    require(cohort["schema"] == "af.perf-local-cohort/1", "local admission refuses hosted cohorts")
    digest = canonical_digest(cohort)
    dispatch = cohort["dispatch"]
    require(dispatch["journal_id"] == journal.config["journal_id"], "wrong original local journal")
    originals, summary = {}, []
    # Inspect the complete bounded journal, including failed/lost claims. A duplicate is a
    # refusal, never a choice of whichever replacement produced the nicer result.
    for ident in journal.names():
        request = journal.request(ident)
        summary.append([ident, canonical_digest(request), canonical_digest(journal.state(ident))])
        if request["cohort_digest"] != digest:
            continue
        member = request["member_id"]
        require(member not in originals, "duplicate local calibration original")
        require(request["submission"] == {"kind": "calibration", "key": digest + ":" + member},
                "unbound local member original")
        originals[member] = request
    runs, refused = {}, []
    for member in cohort["members"]:
        ident = member["member_id"]
        request = originals.get(ident)
        if request is None:
            runs[ident] = calibration.MemberRun(member)
            continue
        state = journal.state(request["request_id"])
        provenance = {"request_id": request["request_id"], "request_digest": canonical_digest(request),
            "result_sha256": state["result_sha256"], "plan_digest": None,
            "started_unix_ms": state["started_unix_ms"], "ended_unix_ms": state["ended_unix_ms"]}
        run = runs[ident] = calibration.MemberRun(member, provenance)
        run.attempt = None if state["started_unix_ms"] is None else {
            "started_unix_ms": state["started_unix_ms"],
            "updated_unix_ms": state["ended_unix_ms"] or state["started_unix_ms"]}
        if state["state"] != "complete":
            run.fail("missing" if state["state"] == "queued" else "interrupted",
                     "artifact_missing" if state["state"] == "queued" else "attempt_incomplete")
            continue
        try:
            executor.verify_original(journal, request["request_id"])
            require(request["backend"] == "local" and request["machine_id"] == dispatch["machine_id"]
                    and request["environment_digest"] == dispatch["environment_digest"]
                    and request["producer_digest"] == dispatch["producer_digest"], "local cohort execution identity changed")
            require(request["sources"]["control_sha"] == cohort["authority_sha"], "local producer changed")
            path = journal.path(request["request_id"])
            plan = contracts.read_json(executor.read(os.path.join(path, "plan.json")), "run")
            record = plan["cohort"]
            require(record is not None and record["cohort_digest"] == digest
                    and record["member_id"] == ident, "original measures a different local member")
            if member["kind"] in ("injection", "power"):
                require(sealed is not None and request["parameters_sha256"] ==
                        sha256_bytes(contracts.write_bytes(sealed, "calibration-parameters")), "original did not freeze the reviewed seal")
            git = gitrefs.Git(os.path.join(path, "sources.git"))
            run.plan = plan
            provenance["plan_digest"] = plan["plan_digest"]
            run.evaluation = compare.evaluate(os.path.join(path, "run"))
            run.replanned = calibration._replanned(calibration.replanner(git, cohort, sealed), member, plan)
            require(run.replanned, "original local plan cannot be reproduced")
        except (OSError, ContractError) as error:
            run.fail("invalid", "producer_unverified")
            refused.append("%s: %s" % (ident, error))
    require(set(originals) <= set(runs), "journal contains undeclared calibration members")
    return runs, refused, {"journal_id": dispatch["journal_id"], "journal_digest": canonical_digest(summary),
                            "requests": len(summary)}


def dispatch_due(journal, cohort, sealed=None):
    """Submit due originals once. Busy before claim may wait; a claim is never retried."""
    contracts.validate_cohort(cohort)
    require(cohort["schema"] == "af.perf-local-cohort/1", "local dispatch refuses hosted cohorts")
    digest = canonical_digest(cohort)
    now = int(time.time() * 1000)
    originals = {journal.request(i)["member_id"] for i in journal.names()
                 if journal.request(i)["cohort_digest"] == digest}
    submitted = []
    for member in cohort["members"]:
        start = calibration.start_unix_ms(cohort) + member["day"] * calibration.DAY_MS
        if member["member_id"] in originals or not start <= now < start + calibration.DAY_MS:
            continue
        submitted.append(journal.submit("core", member["source_sha"], member["source_sha"],
            digest + ":" + member["member_id"], "calibration", cohort=cohort,
            member=member["member_id"], parameters=sealed)["request_id"])
    return submitted
