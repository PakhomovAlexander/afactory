"""Reviewed activation records for planning and activation tests; test code only (ADR-0183).

`records` declares a cohort for any registry and authority and writes the parameters and a
ready readiness document a reviewer would commit beside `fixtures/performance/activation.toml`:
every member's outcome recorded, every tally and decision recomputed by the readers. Nothing
here ran: the outcomes are synthetic, so these records establish what a reviewed manifest must
bind, never empirical readiness. `manifest` writes the TOML manifest binding them by path and
SHA-256.
"""
import hashlib
import json
from fractions import Fraction

import os

from perflab import calibration, contracts, planning, stats, synthetic, transport
from perflab.values import canonical_digest, format_rational

from tests import calibration_fixtures

COHORT_PATH = "fixtures/performance/cohort-first.json"
CALIBRATION_PATH = "fixtures/performance/calibration-first.json"
READINESS_PATH = "fixtures/performance/readiness-first.json"
ANCHOR = calibration_fixtures.ANCHOR


def sha256(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def parameters(cohort, prepared_groups):
    """Sealed parameters of A0 100 ms and a 50 ns loop in `prepared_groups`; every other allowed
    group has no A/A sample, so no A0 and no loop count."""
    groups = []
    for case in cohort["cases"]:
        for allowed in sorted(cohort["allowed_cpu_groups"], key=lambda item: item["group_id"]):
            entry = {"instance_id": case["instance_id"], "cpu_group": allowed["group_id"],
                     "relative": "0.05", "absolute": "1", "a0": None, "a0_samples": 0,
                     "a0_unavailable": "no_aa_samples", "efficiency": None,
                     "power_target": None, "loop_ns_per_iteration": None, "loop_samples": 0,
                     "iterations": None}
            if allowed["group_id"] in prepared_groups:
                a0, cost = Fraction(100), Fraction(50)
                eff = contracts.efficiency(Fraction(1, 20), Fraction(1), a0)
                entry.update(a0=format_rational(a0), a0_samples=240, a0_unavailable=None,
                             efficiency=format_rational(eff),
                             power_target=format_rational(contracts.power_target(eff)),
                             loop_ns_per_iteration=format_rational(cost), loop_samples=24)
                entry["iterations"] = dict(
                    (level, contracts.loop_iterations(contracts.target_ms(entry, level), cost))
                    for level in contracts.LOOP_LEVELS)
            groups.append(entry)
    doc = {"schema": contracts.CALIBRATION_SCHEMA, "cohort_id": cohort["cohort_id"],
           "cohort_digest": canonical_digest(cohort),
           "a0_member_ids": sorted(m["member_id"] for m in cohort["members"]
                                   if m["kind"] == "aa"),
           "preparation_member_ids": sorted(m["member_id"] for m in cohort["members"]
                                            if m["kind"] == "preparation"),
           "preparation_iterations": cohort["preparation"]["iterations"],
           "groups": sorted(groups, key=lambda item: (item["instance_id"], item["cpu_group"]))}
    return contracts.validate_calibration(doc, "parameters", cohort=cohort)


def readiness(cohort, sealed, group, failing=()):
    """A readiness document in which every member ran in `group` and the power members in
    `failing` did not detect."""
    digest = canonical_digest(cohort)
    members = []
    for number, member in enumerate(cohort["members"], ANCHOR + 1):
        level = contracts.member_level(member)
        cases = {}
        for case in cohort["cases"]:
            ident = case["instance_id"]
            record = {"classification": stats.WITHIN_TOLERANCE, "injected_median_ms": None,
                      "target_ms": None, "injection_valid": None}
            if level not in (None, "preparation"):
                target = contracts.target_ms(
                    contracts.calibration_group(sealed, ident, group), level)
                record.update(classification=(stats.INCONCLUSIVE
                                              if member["member_id"] in failing
                                              else stats.REGRESSION),
                              injected_median_ms=format_rational(target),
                              target_ms=format_rational(target), injection_valid=True)
            cases[ident] = record
        entry = calibration_fixtures.member_record(member, cases=cases, group=group,
                                                   digest=digest)
        run = 9000 + number
        entry["provenance"].update(
            actions_run_id=run, run_number=number, lab_run_id="gh%d_a1_primary" % run,
            artifact_name="af-perf-gh%d_a1_primary-run" % run, artifact_id=run)
        members.append(entry)
    tallies, groups, qualifies, ready = contracts.compute_readiness(cohort, sealed, members)
    for entry in members:
        entry["qualifies"] = qualifies[entry["member_id"]]
    latest = ANCHOR + len(members)
    doc = {"schema": contracts.READINESS_SCHEMA, "cohort_id": cohort["cohort_id"],
           "cohort_digest": digest, "calibration_digest": canonical_digest(sealed),
           "recomputed_calibration_digest": canonical_digest(sealed),
           "history": {"after_run_number": ANCHOR, "latest_run_number": latest,
                       "runs": latest - ANCHOR},
           "members": members, "refused_evidence": [], "tallies": tallies, "groups": groups,
           "ready": ready, "claim": contracts.READINESS_CLAIM}
    return contracts.validate_readiness(doc, "readiness", cohort=cohort, calibration=sealed)


def records(suite, authority, groups=(("epyc", "AMD EPYC", "ubuntu-24.04"),), prepared=None,
            tier="quick", environment="linux_x86_64_gnu_hosted",
            recipe="linux_gnu_native_release"):
    """{path: bytes} of a ready cohort's reviewed records for `suite` at `authority`, and the
    documents."""
    allowed = [{"group_id": gid, "cpu_model": model, "image": image}
               for gid, model, image in groups]
    cohort = calibration.declare(suite, authority, "first_cohort", tier, environment, recipe,
                                 allowed, "2026-11-02", calibration_fixtures.REPOSITORY, ANCHOR)
    prepared = list(prepared or [groups[0][0]])
    sealed = parameters(cohort, prepared)
    ready = readiness(cohort, sealed, prepared[0])
    files = {COHORT_PATH: contracts.write_bytes(cohort, "cohort"),
             CALIBRATION_PATH: contracts.write_bytes(sealed, "calibration-parameters"),
             READINESS_PATH: contracts.write_bytes(ready, "readiness")}
    return files, (cohort, sealed, ready)


def _toml(value):
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, list):
        return "[" + ", ".join(_toml(item) for item in value) + "]"
    return str(value)


def manifest(files, documents, admission=None, **changes):
    """The TOML bytes of an activation manifest binding `files` (records), plus `changes`."""
    cohort, sealed, ready = documents
    calibrated, _ = calibrated_groups(cohort, sealed)
    fields = {"schema": contracts.ACTIVATION_SCHEMA, "authority_sha": cohort["authority_sha"],
              "policy_digest": cohort["policy_digest"], "cohort_id": cohort["cohort_id"],
              "cohort_path": COHORT_PATH, "cohort_sha256": sha256(files[COHORT_PATH]),
              "calibration_path": CALIBRATION_PATH,
              "calibration_sha256": sha256(files[CALIBRATION_PATH]),
              "readiness_path": READINESS_PATH, "readiness_sha256": sha256(files[READINESS_PATH]),
              "cohort_actions_run_ids": calibration.original_runs(ready),
              "environment_id": cohort["environment_id"], "recipe_id": cohort["recipe_id"],
              "calibrated_groups": calibrated}
    fields.update(changes)
    lines = ["%s = %s" % (key, _toml(value)) for key, value in fields.items()]
    lines += ["", "[admission]"]
    for key, value in (admission or {"kind": "calibrated"}).items():
        lines.append("%s = %s" % (key, _toml(value)))
    return ("\n".join(lines) + "\n").encode("utf-8")


def calibrated_groups(cohort, sealed):
    from perflab import activation
    return activation.calibrated_scope(cohort, sealed)


BRIDGE_ADAPTER = "native_probe_public_api_v1"
EQUIVALENCE_PATH = "fixtures/performance/equivalence-bridge.json"


def bridge_plan(git, baseline, candidate, authority, actions_run, mode="proposed",
                tier="quick", control=None, edit=None):
    """The plan of `baseline` against `candidate` measured by `authority`, frozen from Git
    exactly as the reviewed workflow's manual bootstrap dispatch with `--authority` plans it in
    attempt 1 of Actions run `actions_run` (`ci._dispatch`): no environment, seed or pair
    override. `mode="control"` dispatches without an authority. `edit`, when given, changes the
    frozen plan and its digest afterwards, as a forger with the plan's bytes could."""
    request = planning.Request(
        trigger="manual", tier=tier, scope="bootstrap", base=baseline, head=candidate,
        control=control or candidate, authority=authority if mode == "proposed" else None,
        run_id="gh%d_a1_primary" % actions_run, trusted_host_build=True)
    plan = planning.plan(git, request)
    if edit is not None:
        edit(plan)
        plan["plan_digest"] = contracts.plan_digest(plan)
        contracts.validate_run(plan)
    return plan


def measured_run(plan, run_dir, cpu="AMD EPYC", image="ubuntu-24.04", value=None,
                 adapters=(BRIDGE_ADAPTER, BRIDGE_ADAPTER), group=BRIDGE_ADAPTER,
                 oracle_failure=None, **options):
    """A measured-labelled synthetic run of `plan` on hosts of `cpu` and sample `image`, whose
    sides compiled `adapters` of `group`; `oracle_failure` names an instance whose candidate
    samples fail their oracle. Synthetic numbers: arithmetic and orchestration only."""
    observed = dict(synthetic.SYNTHETIC_HOST, cpu_model={"value": cpu})

    def edit_builds(doc):
        doc["adapter_binding"].update(baseline=adapters[0], candidate=adapters[1],
                                      baseline_group=group, candidate_group=group)
        for side, adapter in zip(contracts.SIDES, adapters):
            doc["sides"][side].update(chosen_adapter=adapter, equivalence_group=group)
            doc["sides"][side]["adapter_outcomes"][0].update(adapter_id=adapter,
                                                             equivalence_group=group)
            doc["sides"][side]["overlay"]["adapter_id"] = adapter

    def edit(record):
        record["host_id"] = contracts.host_id(observed)
        if oracle_failure is not None and record["instance_id"] == oracle_failure \
                and record["side"] == "candidate" and record["phase"] == "measured":
            record.update(status="failed",
                          failure={"kind": "oracle_mismatch", "detail": "synthetic"})
            record["oracle"]["status"] = "failed"
            record["metrics"] = {}
            record["metric_scopes"] = {}
        return [record]
    synthetic.write_run(plan, run_dir, value or synthetic.constant(10), edit=edit,
                        edit_builds=edit_builds, provenance="measured", **options)
    for shard in plan["shards"]:
        def host(doc):
            doc["host"] = observed
            doc["host_id"] = contracts.host_id(observed)
            doc["execution"]["sample_image"] = image
        synthetic.reseal_shard(os.path.join(run_dir, "shards", str(shard["index"])), host)
    return run_dir


def stage(run_dir, evidence, actions_run, artifacts, next_id):
    """Stage `run_dir`'s run and shard bundles as the workflow uploads them below `evidence`
    and list them in `artifacts` as the artifact API does; the next free artifact ID."""
    plan = contracts.read_file(os.path.join(run_dir, "run.json"), "run")
    lab = plan["run_id"]
    names = [("af-perf-%s-run" % lab, "run_bundle", None)] + [
        ("af-perf-%s-shard-%d" % (lab, shard["index"]), "shard_bundle", shard["index"])
        for shard in plan["shards"]]
    for name, content, shard in names:
        transport.stage(os.path.join(evidence, name), run_dir, content, shard)
        artifacts.append({"id": next_id, "name": name, "size_in_bytes": 1, "expired": False,
                          "created_at": "2026-11-09T06:00:00Z",
                          "workflow_run": {"id": actions_run}})
        next_id += 1
    return next_id
