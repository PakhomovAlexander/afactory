"""Activation verification and equivalent-adapter admission (p6b, ADR-0183, ADR-0184).

`fixtures/performance/activation.toml` (`af.perf-activation/3`) binds the authority the laboratory
measures with to the cohort that calibrated it: the cohort, its sealed parameters and its readiness
as reviewed files of the control revision with their SHA-256, every member's original Actions
run, and the environment, recipe and CPU/image groups the cohort calibrated. Nothing an author
wrote is taken as success:

- **Reviewed verification** (`verify(..., evidence=None)`), what an ordinary plan does. The
  records are read from Git at the control revision and rehashed; the readiness document is
  validated against its cohort and parameters, which recomputes every tally, qualification and
  the decision from its member outcomes; it must be ready; its original runs must be exactly the
  manifest's; the manifest's groups must be exactly the groups the parameters prepared for every
  critical case; and the authority's registry must have the manifest's policy digest. The old
  cohort is not fetched again. An active plan carries the calibrated groups' exact CPU models
  and images, and its verdict binds every measured shard's observed host to them.
- **Proposed verification** (`proposed_changes`, `fetch_evidence`, then
  `verify(..., evidence=DIR)`), what an activation change needs, run by the trusted activation
  workflow from the pull request's base revision with a read-only Actions token. When the
  manifest or any record it names differs from the base, trusted control code fetches the
  cohort's retained Actions evidence: the workflow-run history down to the cohort's anchor, each
  original's attempt-1 record and jobs, the complete artifact listings of every run named for
  the cohort and the originals' bundles. Readiness is recomputed from it (`calibration.collect`,
  `calibration.readiness`), parameters and producers included, and its membership,
  classifications, doses, denominators and decision must equal the reviewed document's exactly.
  Missing, expired, capped or inconsistent evidence is refused. The archive is a retention
  backup, never a promotion source. An unchanged activation is only verified locally.
- **Equivalent adapters** (`admission.kind = "equivalent_adapter"`). The manifest names the
  reviewed calibrated activation it extends, retained in the control revision. Both bind the
  same cohort, records, runs and scope; only the authority differs. Between the two authorities
  the protected harness and the complete build closure (lock files, Cargo manifests, build
  scripts, toolchain and Cargo configuration, additions included) may differ only in the named
  adapter files, the adapter registry's entries of the named equivalence group, and the
  calibration records. The bridge's execution evidence (`af.perf-equivalence/1`, equivalence.py)
  must verify: a golden check of the calibrated and the bridged product sources and a post-merge
  A/A, each a retained run of the bridged authority, rechecked locally and recomputed from the
  fetched runs in proposed verification. A matching file list does not establish semantic
  equivalence: owner approval remains the pending obligation the verification names.

No verification activates anything, edits a ruleset or installs `.af/` authority: activation is a
reviewed change, and enforcement stays advisory until an operator makes a check required.
"""
import hashlib
import json
import os

from . import adapters as adapters_module
from . import calibration, contracts, equivalence as equivalence_module, planning, tomlfile
from . import gitrefs, transport
from .values import ContractError, require

HISTORY_PAGES_MAX = 100
JOBS_PAGES_MAX = 2
ARTIFACT_PAGES_MAX = 10
PAGE_SIZE = 100
# Where `fetch_evidence` writes a bridge's two retained runs' artifacts.
EQUIVALENCE_DIR = "equivalence"
# The run fields readiness reads; the fetcher keeps only these of each listed run.
RUN_FIELDS = ("id", "run_number", "run_attempt", "display_title", "event", "path",
              "head_branch", "head_sha", "created_at", "run_started_at", "updated_at",
              "status", "conclusion")


def _sha256(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def _record(git, revision, path, digest, contract, what):
    raw = git.read_file(revision, path)
    require(raw is not None, "%s: %s is not in revision %s" % (what, path, revision[:12]))
    require(_sha256(raw) == digest, "%s: %s at %s has SHA-256 %s, not the manifest's %s"
            % (what, path, revision[:12], _sha256(raw), digest))
    return contracts.read_json(raw, contract, path)


def read_manifest(git, revision, path=planning.ACTIVATION_PATH):
    """(manifest, SHA-256 of its bytes) at `revision`; None when there is none."""
    raw = git.read_file(revision, path)
    if raw is None:
        return None, None
    doc = tomlfile.loads(raw, path)
    require(doc.get("schema") == contracts.ACTIVATION_SCHEMA,
            "%s.schema: expected %s, found %r" % (path, contracts.ACTIVATION_SCHEMA,
                                                  doc.get("schema")))
    contracts.validate_activation(doc, path)
    return doc, _sha256(raw)


class Verified(object):
    """What `verify` established: the manifest, its records and the verification document."""

    def __init__(self, manifest, sha, cohort, sealed, readiness, doc):
        self.manifest = manifest
        self.sha256 = sha
        self.cohort = cohort
        self.calibration = sealed
        self.readiness = readiness
        self.doc = doc


def calibrated_scope(cohort, sealed):
    """(calibrated groups, {uncalibrated group: critical cases without A0 or a loop count}): a
    group is calibrated only when the sealed parameters prepared every critical case there."""
    groups = contracts.group_readiness(cohort, sealed)
    calibrated = sorted(gid for gid, entry in groups.items() if entry["ready"])
    uncalibrated = dict((gid, sorted(entry["unavailable"])) for gid, entry in groups.items()
                        if not entry["ready"])
    return calibrated, uncalibrated


def _bound_records(git, revision, manifest, where):
    """The manifest's cohort, parameters and readiness, rehashed and checked against it and
    against each other; the readiness decision recomputed from its member outcomes."""
    cohort = _record(git, revision, manifest["cohort_path"], manifest["cohort_sha256"],
                     "cohort", where + ".cohort_path")
    sealed = _record(git, revision, manifest["calibration_path"], manifest["calibration_sha256"],
                     "calibration-parameters", where + ".calibration_path")
    retained = _record(git, revision, manifest["readiness_path"], manifest["readiness_sha256"],
                       "readiness", where + ".readiness_path")
    contracts.validate_readiness(retained, manifest["readiness_path"], cohort=cohort,
                                 calibration=sealed)
    require(cohort["cohort_id"] == manifest["cohort_id"],
            where + ".cohort_id: the cohort record is %s" % cohort["cohort_id"])
    require(cohort["policy_digest"] == manifest["policy_digest"],
            where + ".policy_digest: the cohort calibrated policy %s" % cohort["policy_digest"])
    require((cohort["environment_id"], cohort["recipe_id"])
            == (manifest["environment_id"], manifest["recipe_id"]),
            where + ": the cohort calibrated environment %s and recipe %s; an activation never "
            "broadens them" % (cohort["environment_id"], cohort["recipe_id"]))
    require(retained["ready"], where + ".readiness_path: the recomputed readiness of cohort %s "
            "is not ready" % cohort["cohort_id"])
    runs = calibration.original_runs(retained)
    require(runs == manifest["cohort_actions_run_ids"],
            where + ".cohort_actions_run_ids: the readiness record's originals are %s"
            % ", ".join(str(run) for run in runs[:16]))
    calibrated, uncalibrated = calibrated_scope(cohort, sealed)
    require(manifest["calibrated_groups"] == calibrated,
            where + ".calibrated_groups: the parameters prepared every critical case only in "
            "%s; %s" % (", ".join(calibrated) or "no group", "; ".join(
                "%s lacks %s" % (gid, ", ".join(cases)) for gid, cases in
                sorted(uncalibrated.items())) or "no other group"))
    return cohort, sealed, retained, calibrated, uncalibrated


def verify(git, revision, manifest_path=planning.ACTIVATION_PATH, evidence=None, scratch=None,
           replan=None, producer=None):
    """Verify the activation manifest at `revision` of `git`; a `Verified`. With `evidence`, a
    directory `fetch_evidence` wrote (the retained Actions evidence), readiness is recomputed
    from it and must reproduce the reviewed record; `replan` is the plan check of
    `calibration.collect` (the authority's own replanner by default). Every refusal is a
    ContractError or `gitrefs.Unavailable`."""
    manifest, sha = read_manifest(git, revision, manifest_path)
    require(manifest is not None, "%s is not in revision %s" % (manifest_path, revision[:12]))
    where = manifest_path
    cohort, sealed, retained, calibrated, uncalibrated = _bound_records(git, revision, manifest,
                                                                        where)
    admission = manifest["admission"]
    equivalence = record = None
    if admission["kind"] == "calibrated":
        require(manifest["authority_sha"] == cohort["authority_sha"],
                where + ".authority_sha: the cohort calibrated authority %s; another authority "
                "needs its own cohort or an equivalent-adapter admission"
                % cohort["authority_sha"])
    else:
        equivalence, record = _equivalence(git, revision, manifest, cohort, where)
    authority = manifest["authority_sha"]
    require(git.is_ancestor(authority, revision),
            where + ".authority_sha: %s is not reachable from revision %s"
            % (authority, revision[:12]))
    suite = planning.read_suite(git, authority)
    require(suite.policy_digest == manifest["policy_digest"],
            where + ".policy_digest: the authority's registry digest is %s"
            % suite.policy_digest)
    recomputed = None
    if evidence is not None:
        recomputed = _recompute(git, cohort, sealed, retained, evidence, scratch, replan,
                                producer, where)
        if record is not None:
            bound = _recompute_equivalence(record, evidence, scratch, where, git,
                                           cohort["dispatch"]["repository"],
                                           manifest["authority_sha"])
            for role in equivalence_module.ROLES:
                equivalence[role]["provenance"] = bound[role]
    tallies = retained["tallies"]
    pending = ["owner_review_at_exact_head"]
    if equivalence is not None:
        pending += ["semantic_equivalence_owner_approval"]
    doc = {"schema": contracts.ACTIVATION_VERIFICATION_SCHEMA,
           "mode": "reviewed" if evidence is None else "fetched", "revision": revision,
           "activation_sha256": sha, "admission": admission["kind"],
           "authority_sha": authority, "policy_digest": manifest["policy_digest"],
           "cohort_id": manifest["cohort_id"], "cohort_sha256": manifest["cohort_sha256"],
           "calibration_sha256": manifest["calibration_sha256"],
           "readiness_sha256": manifest["readiness_sha256"],
           "members": len(retained["members"]),
           "original_runs": calibration.original_runs(retained),
           "tallies": dict((key, tallies[key]) for key in (
               "aa_within_tolerance", "aa_members", "power_qualifying", "power_members")),
           "environment_id": manifest["environment_id"], "recipe_id": manifest["recipe_id"],
           "calibrated_groups": calibrated, "uncalibrated_groups": uncalibrated,
           "recomputed": recomputed, "equivalence": equivalence, "pending": pending}
    contracts.validate_activation_verification(doc, "activation verification")
    return Verified(manifest, sha, cohort, sealed, retained, doc)


def _recompute(git, cohort, sealed, retained, evidence, scratch, replan, producer, where):
    """Readiness recomputed from the fetched evidence; its judgment must equal the record's."""
    if replan is None:
        replan = calibration.replanner(git, cohort, sealed)
    if producer is None:
        producer = calibration.producer_check(git, cohort)
    runs, refused, history = calibration.collect(cohort, evidence, scratch, replan, producer)
    fresh = calibration.readiness(cohort, sealed, runs, refused, history)
    expected, found = calibration.judgment(retained), calibration.judgment(fresh)
    if found != expected:
        detail = "its tallies or decision"
        for before, after in zip(expected["members"], found["members"]):
            if before != after:
                detail = "member %s: %s with %s, recorded %s with %s" % (
                    before["member_id"], after["status"], ", ".join(after["reasons"]) or
                    "no reason", before["status"], ", ".join(before["reasons"]) or "no reason")
                break
        raise ContractError(
            "%s.readiness_path: the readiness recomputed from the retained Actions evidence "
            "differs from the reviewed record (%s); missing, expired or changed evidence never "
            "activates" % (where, detail))
    require(fresh["ready"], where + ": the recomputed readiness is not ready")
    return {"history_latest_run_number": history["latest_run_number"],
            "refused_evidence": len(refused)}


# Equivalent-adapter admission ----------------------------------------------------------------

def _equivalence(git, revision, manifest, cohort, where):
    admission = manifest["admission"]
    path = admission["calibrated_activation_path"]
    raw = git.read_file(revision, path)
    require(raw is not None, where + ".admission.calibrated_activation_path: %s is not in "
            "revision %s" % (path, revision[:12]))
    require(_sha256(raw) == admission["calibrated_activation_sha256"],
            where + ".admission.calibrated_activation_sha256: %s has SHA-256 %s"
            % (path, _sha256(raw)))
    base = tomlfile.loads(raw, path)
    require(base.get("schema") == contracts.ACTIVATION_SCHEMA,
            path + ".schema: expected %s" % contracts.ACTIVATION_SCHEMA)
    contracts.validate_activation(base, path)
    require(base["admission"]["kind"] == "calibrated",
            path + ": an equivalent adapter extends a calibrated activation, never another "
            "bridge")
    bound = ("policy_digest", "cohort_id", "cohort_path", "cohort_sha256", "calibration_path",
             "calibration_sha256", "readiness_path", "readiness_sha256",
             "cohort_actions_run_ids", "environment_id", "recipe_id", "calibrated_groups")
    for key in bound:
        require(manifest[key] == base[key], where + ".%s: an equivalent adapter keeps the "
                "calibrated activation's %s" % (key, key))
    calibrated_authority = base["authority_sha"]
    require(calibrated_authority == cohort["authority_sha"],
            path + ".authority_sha: not the authority the cohort calibrated")
    bridged = manifest["authority_sha"]
    require(bridged != calibrated_authority, where + ".authority_sha: the same authority needs "
            "no equivalent-adapter admission")
    require(git.is_ancestor(calibrated_authority, bridged),
            where + ".authority_sha: the bridge does not descend from the calibrated authority")
    changed = protected_diff(git, calibrated_authority, bridged, admission, manifest, base)
    record = bridge_evidence(git, revision, manifest, base, cohort, where)
    runs = dict((role, {"actions_run_id": record[role]["actions_run_id"],
                        "lab_run_id": record[role]["lab_run_id"],
                        "plan_digest": record[role]["plan_digest"],
                        "baseline_sha": record[role]["baseline_sha"],
                        "candidate_sha": record[role]["candidate_sha"],
                        "adapters": {"baseline": record[role]["adapters"]["baseline"],
                                     "candidate": record[role]["adapters"]["candidate"]},
                        "provenance": None})
                for role in equivalence_module.ROLES)
    return dict(runs, calibrated_activation_sha256=admission["calibrated_activation_sha256"],
                calibrated_authority_sha=calibrated_authority, changed_paths=changed,
                evidence_sha256=admission["equivalence_sha256"]), record


def bridge_context(git, manifest, base, cohort):
    """The context a bridge's evidence record must carry, from the manifest, the calibrated
    activation it extends, the cohort and the bridged authority's adapter registry."""
    return equivalence_module.context(cohort, base, manifest["admission"],
                                      manifest["authority_sha"],
                                      _adapters(git, manifest["authority_sha"]))


def bridge_evidence(git, revision, manifest, base, cohort, where):
    """The bridge's reviewed execution evidence: its record rehashed from the control revision,
    its reasons re-derived, its context exactly the manifest's, both checks verified and the
    post-merge A/A's source descending from the bridge. Without it the bridge never measures as
    active: no authored flag or matching path list replaces the executions (ADR-0184)."""
    admission = manifest["admission"]
    record = _record(git, revision, admission["equivalence_path"],
                     admission["equivalence_sha256"], "equivalence",
                     where + ".admission.equivalence_path")
    expected = bridge_context(git, manifest, base, cohort)
    for key in equivalence_module.CONTEXT:
        require(record[key] == expected[key], "%s.admission.equivalence_path: the evidence "
                "record's %s is not the bridge's" % (where, key))
    if not record["verified"]:
        raise ContractError(
            "%s.admission.equivalence_path: the bridge's execution evidence does not verify "
            "(%s); a bridge without verified golden checks on both product sources and a "
            "post-merge A/A stays advisory" % (where, "; ".join(
                "%s: %s" % (role, reason) for role in equivalence_module.ROLES
                for reason in record[role]["reasons"][:2])))
    aa = record["post_merge_aa"]["candidate_sha"]
    try:
        merged = git.is_ancestor(manifest["authority_sha"], aa)
    except ContractError:
        merged = False
    require(merged, "%s.admission.equivalence_path: the post-merge A/A measured %s, which does "
            "not descend from the bridge %s" % (where, aa, manifest["authority_sha"]))
    return record


def bridge_replanner(git, ctx):
    """The plan the bridged authority freezes again for a bridge run (ADR-0186): the manual
    bootstrap dispatch of `ctx`'s calibrated tier that the reviewed workflow makes, with only
    what a dispatch may choose taken from the retained plan (its run ID, control revision, shard
    count and frozen build) and the sources the role names: the calibrated against the bridged
    product source for the golden check, one source against itself for the post-merge A/A.
    Registry, policy, workload, timing, environment, recipe, fixtures, instruments and the pair
    counts all come from the bridged authority itself."""
    from . import planning

    def replan(role, plan):
        resolved = plan["resolved"]
        if role == "golden":
            base, head = ctx["calibrated_authority_sha"], ctx["bridged_authority_sha"]
        else:
            require(resolved["baseline_sha"] == resolved["candidate_sha"],
                    "the post-merge A/A compares two sources")
            base = head = resolved["candidate_sha"]
        build = plan["profile"]["build"]
        request = planning.Request(
            trigger="manual", tier=ctx["tier"], scope="bootstrap", base=base, head=head,
            control=resolved["control_sha"], authority=ctx["bridged_authority_sha"],
            run_id=plan["run_id"], shards=len(plan["shards"]),
            environment=None if plan["profile"]["default"] else ctx["environment_id"],
            build_image=build["image"] if build["image_source"] == "configured" else None,
            trusted_host_build=build["isolation"] == "trusted_host")
        return planning.plan(git, request)
    return replan


def _replanned_bridge(replan, role, plan, at):
    """Refuse a retained bridge plan unless the bridged authority freezes exactly it again."""
    try:
        expected = replan(role, plan)
    except (ContractError, gitrefs.Unavailable) as error:
        raise ContractError("%s: the bridged authority cannot plan this run again: %s"
                            % (at, error))
    differing = sorted(key for key in set(expected) | set(plan)
                       if key not in ("selectors", "plan_digest")
                       and expected.get(key) != plan.get(key))
    require(calibration._frozen_identity(expected) == calibration._frozen_identity(plan),
            "%s: its frozen plan is not the plan the bridged authority freezes for this dispatch "
            "(%s differ); changed policy, workload, timing, environment, recipe, fixtures or "
            "instruments never bridge" % (at, ", ".join(differing[:8]) or "its identity"))


def _recompute_equivalence(record, evidence, scratch, where, git, repository, bridged,
                           replan=None):
    """The bridge's record recomputed from its two retained runs, fetched below `evidence`'s
    equivalence directory: each run must first be bound to its producer (`bridge_provenance`)
    and its frozen plan be exactly the one the bridged authority freezes again for its dispatch
    (`bridge_replanner`, ADR-0186), then the recomputation must be exactly the reviewed record.
    {role: provenance}."""
    root = os.path.join(evidence, EQUIVALENCE_DIR)
    runs, bound = {}, {}
    ctx = dict((key, record[key]) for key in equivalence_module.CONTEXT)
    if replan is None:
        replan = bridge_replanner(git, ctx)
    for role in equivalence_module.ROLES:
        lab = record[role]["lab_run_id"]
        target = os.path.join(scratch, "equivalence", role)
        os.makedirs(target)
        run_dir = equivalence_module.assemble(root, lab, target)
        plan = contracts.read_file(os.path.join(run_dir, "run.json"), "run")
        bound[role] = bridge_provenance(git, root, role, record[role]["actions_run_id"], plan,
                                        repository, bridged, where)
        _replanned_bridge(replan, role, plan, "%s.admission.equivalence_path: %s run %d"
                          % (where, role, record[role]["actions_run_id"]))
        runs[role] = (run_dir, record[role]["actions_run_id"])
    fresh = equivalence_module.compute(ctx, runs)
    if contracts.document_bytes(fresh) != contracts.document_bytes(record):
        differing = [role for role in equivalence_module.ROLES if fresh[role] != record[role]]
        raise ContractError(
            "%s.admission.equivalence_path: the evidence recomputed from the retained runs "
            "differs from the reviewed record (%s); missing, expired or changed evidence never "
            "activates" % (where, ", ".join(differing) or "its verification"))
    return bound


# The bridge runs' producer records `fetch_bridge_runs` writes beside their artifacts.
PROVENANCE_DIR = "provenance"
BRIDGE_RUN_FILE = "run.json"
BRIDGE_ATTEMPT_FILE = "attempt.json"
BRIDGE_JOBS_FILE = "jobs.json"


def lab_attempt(lab, actions_run_id, what):
    """The attempt a lab run ID names, gh<run>_a<attempt>_<name>: exactly the one selected."""
    match = calibration.LAB_RUN.match(lab)
    require(match is not None and int(match.group(1)) == actions_run_id,
            "%s: lab run %s was not planned by Actions run %d" % (what, lab, actions_run_id))
    return int(match.group(2))


def bridge_provenance(git, root, role, actions_run_id, plan, repository, bridged, where):
    """Bind one bridge run to the producer that ran it (ADR-0185), from the run, attempt and job
    records the trusted fetcher retained: the reviewed Performance workflow of `repository`, the
    exact attempt the lab run ID names, completed successfully; the workflow file at its head
    commit and the scheduling code at the plan's control revision byte-identical to the
    `bridged` authority's (neither commit need equal the authority or each other); and every
    build and shard job of that attempt on a fresh GitHub-hosted runner of the plan's label. A
    missing, changed or unrelated producer refuses the activation."""
    at = "%s.admission.equivalence_path: %s run %d" % (where, role, actions_run_id)
    directory = os.path.join(root, PROVENANCE_DIR, role)
    attempt = lab_attempt(plan["run_id"], actions_run_id, at)
    try:
        run = calibration._run_identity(calibration._bounded_json(
            os.path.join(directory, BRIDGE_RUN_FILE)), at + " run record")
        selected = calibration._bounded_json(os.path.join(directory, BRIDGE_ATTEMPT_FILE))
        found = calibration._run_identity(selected, at + " attempt record")
    except OSError as error:
        raise ContractError("%s: its producer records were not retained: %s" % (at, error))
    require(run["id"] == actions_run_id and found["id"] == actions_run_id,
            at + ": the fetched records are of another run")
    require(run["repository"] == repository and found["repository"] == repository,
            at + ": ran in %s, not %s" % (run["repository"], repository))
    require(run["path"] == contracts.DISPATCH_WORKFLOW and found["path"] == run["path"],
            at + ": was produced by %s, not the reviewed %s" % (run["path"],
                                                                contracts.DISPATCH_WORKFLOW))
    require(found["attempt"] == attempt, at + ": the attempt record is attempt %d, not the "
            "selected attempt %d" % (found["attempt"], attempt))
    require(found["head_sha"] == run["head_sha"] and found["event"] == run["event"],
            at + ": the attempt record names another head commit or event")
    require(selected.get("status") == "completed" and selected.get("conclusion") == "success",
            at + ": attempt %d is %s with conclusion %s; only a completed successful attempt "
            "is bridge evidence" % (attempt, selected.get("status"), selected.get("conclusion")))
    control = plan["resolved"]["control_sha"]
    try:
        require(git.read_file(bridged, contracts.DISPATCH_WORKFLOW) is not None,
                "the bridged authority %s has no %s" % (bridged, contracts.DISPATCH_WORKFLOW))
        for sha, paths, what in (
                (run["head_sha"], calibration.PRODUCER_WORKFLOW_PATHS,
                 "the workflow at head commit"),
                (control, calibration.PRODUCER_SCHEDULER_PATHS,
                 "the scheduling code at control revision")):
            require(git.resolve(sha, what) == sha, "%s %s is not a full commit" % (what, sha))
            changed = git.changed_paths(bridged, sha, paths)
            require(not changed, "%s %s differs from the bridged authority's: %s"
                    % (what, sha[:12], ", ".join(changed[:8])))
    except (ContractError, gitrefs.Unavailable) as error:
        raise ContractError("%s: its producer is not the bridged authority's: %s" % (at, error))
    runner = calibration._measuring_runner(None, run, plan, attempt,
                                           os.path.join(directory, BRIDGE_JOBS_FILE))
    require(runner is not None, at + ": the build and shard jobs of attempt %d are not all shown "
            "on fresh GitHub-hosted runners of the plan's label" % attempt)
    return {"repository": repository, "workflow_path": run["path"], "event": run["event"],
            "run_attempt": attempt, "conclusion": "success", "head_sha": run["head_sha"],
            "control_sha": control, "runner": runner}


def protected_diff(git, calibrated, bridged, admission, manifest, base):
    """The protected paths that differ between the calibrated and the bridged authority, after
    refusing any change but the named adapter files, the adapter registry's entries of the
    named equivalence group, and the calibration records."""
    for sha in (calibrated, bridged):
        require(git.read_file(sha, planning.CARGO_LOCK_PATH) is not None,
                "the authority %s has no %s; its build closure cannot be compared"
                % (sha, planning.CARGO_LOCK_PATH))
    # The harness and the complete build closure, additions and deletions included: a changed
    # lock, manifest, build script, toolchain or Cargo configuration changes the instrument.
    changed = git.changed_paths(calibrated, bridged,
                                planning.HARNESS_PATHS + planning.BUILD_CLOSURE_PATHS)
    records = set([planning.ACTIVATION_PATH, admission["calibrated_activation_path"]]
                  + [doc[key] for doc in (manifest, base)
                     for key in ("cohort_path", "calibration_path", "readiness_path")])
    files = set(admission["adapter_files"])
    outside = [path for path in changed
               if path not in files and path not in records
               and path != adapters_module.ADAPTERS_PATH]
    require(not outside, "the protected diff of an equivalent adapter changes %s beyond its "
            "named adapter files and registry support entries; a changed workload, timing "
            "boundary, oracle, policy, harness or build input needs a new contract and full "
            "calibration"
            % ", ".join(outside[:8]))
    group = admission["equivalence_group"]
    before = _adapters(git, calibrated)
    after = _adapters(git, bridged)
    for ident in sorted(set(before) | set(after)):
        old, new = before.get(ident), after.get(ident)
        if old == new:
            continue
        require(all(entry is None or entry["equivalence_group"] == group
                    for entry in (old, new)),
                "%s: adapter %s outside equivalence group %s changed"
                % (adapters_module.ADAPTERS_PATH, ident, group))
    closure = set(path for entry in after.values() if entry["equivalence_group"] == group
                  for path in entry["closure"])
    require(files <= closure, "the named adapter files %s are not in the closure of any "
            "adapter of equivalence group %s" % (", ".join(sorted(files - closure)[:8]), group))
    for path in (planning.SUITE_PATH,):
        require(git.read_file(calibrated, path) == git.read_file(bridged, path),
                "%s differs between the calibrated and the bridged authority" % path)
    return changed


def _adapters(git, sha):
    raw = git.read_file(sha, adapters_module.ADAPTERS_PATH)
    if raw is None:
        return {}
    return adapters_module.load_bytes(raw, "%s@%s" % (adapters_module.ADAPTERS_PATH,
                                                       sha[:12]))["adapters"]


# Proposed verification -----------------------------------------------------------------------

def proposed_changes(git, base, revision, manifest_path=planning.ACTIVATION_PATH):
    """The activation inputs that differ between the trusted `base` and the proposed `revision`:
    the manifest itself and every record it names at either revision (cohort, parameters,
    readiness, and a bridge's calibrated activation and evidence record). Empty when the
    activation is unchanged, which an ordinary pull request verifies locally."""
    paths = set([manifest_path])
    for sha in (base, revision):
        try:
            manifest, _ = read_manifest(git, sha, manifest_path)
        except ContractError:
            if sha == revision:
                raise
            # A base manifest this release does not read is replaced, never read.
            manifest = None
        if manifest is None:
            continue
        paths.update(manifest[key] for key in ("cohort_path", "calibration_path",
                                               "readiness_path"))
        if manifest["admission"]["kind"] == "equivalent_adapter":
            paths.update(manifest["admission"][key] for key in ("calibrated_activation_path",
                                                                "equivalence_path"))
    return [path for path in sorted(paths)
            if git.read_file(base, path) != git.read_file(revision, path)]


# Fetching the retained evidence ---------------------------------------------------------------

def _slim_run(item):
    require(isinstance(item, dict), "a workflow run is not an object")
    run = dict((key, item.get(key)) for key in RUN_FIELDS)
    repository = item.get("repository")
    run["repository"] = {"full_name": repository.get("full_name")
                         if isinstance(repository, dict) else None}
    return run


def fetch_evidence(collector, cohort, target):
    """Write the cohort's retained Actions evidence below the new directory `target`, in the
    layout `calibration.collect` reads, through the trusted `collector` (read-only token):
    `runs.json` (the workflow's runs of every event, newest first, down to the cohort's anchor),
    `attempts/<run>.json` and `jobs/<run>.json` of each original's attempt 1, `actions.json`
    (the artifact listings of every run named for the cohort) and each original's `af-perf-*`
    bundles. An expired artifact stays listed and is not downloaded."""
    contracts.validate_cohort(cohort)
    require(collector.repository == cohort["dispatch"]["repository"],
            "--repository: the cohort's members ran in %s" % cohort["dispatch"]["repository"])
    os.makedirs(target)
    anchor = cohort["dispatch"]["after_run_number"]
    workflow = os.path.basename(cohort["dispatch"]["workflow_path"])
    runs, seen, total = [], set(), 0
    reached = anchor == 0
    for page in range(1, HISTORY_PAGES_MAX + 1):
        doc = collector.get_json("/actions/workflows/%s/runs?per_page=%d&page=%d"
                                 % (workflow, PAGE_SIZE, page), "the workflow-run listing")
        require(isinstance(doc, dict) and isinstance(doc.get("workflow_runs"), list)
                and type(doc.get("total_count")) is int,
                "the workflow-run listing has no workflow_runs and total_count")
        total = max(total, doc["total_count"])
        for item in doc["workflow_runs"]:
            run = _slim_run(item)
            # Runs created while the pages are read shift older ones onto the next page; a
            # repeat is the same run, and a run deleted meanwhile leaves a gap readiness refuses.
            if run["id"] in seen:
                continue
            seen.add(run["id"])
            runs.append(run)
            if type(run["run_number"]) is int and run["run_number"] <= anchor:
                reached = True
        if reached or not doc["workflow_runs"]:
            break
    require(reached or len(runs) >= total, "the workflow's runs after anchor %d exceed %d "
            "pages; the history cannot be read whole" % (anchor, HISTORY_PAGES_MAX))
    _write_json(os.path.join(target, calibration.RUNS_FILE),
                {"total_count": max(total, len(runs)), "workflow_runs": runs})
    history, _ = calibration._run_history(target, cohort)
    originals, roles = calibration._dispatches(cohort, history, lambda *args: None)
    for name in (calibration.ATTEMPTS_DIR, calibration.JOBS_DIR):
        os.makedirs(os.path.join(target, name))
    for run in sorted(originals.values(), key=lambda item: item["id"]):
        attempt = collector.get_json("/actions/runs/%d/attempts/1" % run["id"],
                                     "attempt 1 of run %d" % run["id"])
        _write_json(os.path.join(target, calibration.ATTEMPTS_DIR, "%d.json" % run["id"]),
                    attempt)
        jobs = _complete_listing(
            collector, "/actions/runs/%d/attempts/1/jobs" % run["id"], "jobs",
            JOBS_PAGES_MAX, "the jobs of attempt 1 of run %d" % run["id"])
        _write_json(os.path.join(target, calibration.JOBS_DIR, "%d.json" % run["id"]),
                    {"total_count": len(jobs), "jobs": jobs})
    artifacts = []
    originals_by_run = dict((run["id"], member) for member, run in originals.items())
    for run_id in sorted(roles):
        listed = _complete_listing(collector, "/actions/runs/%d/artifacts" % run_id,
                                   "artifacts", ARTIFACT_PAGES_MAX,
                                   "the artifact listing of run %d" % run_id)
        for item in listed:
            require(isinstance(item, dict) and isinstance(item.get("workflow_run"), dict)
                    and item["workflow_run"].get("id") == run_id,
                    "an artifact of run %d names another run" % run_id)
        artifacts += listed
        if run_id not in originals_by_run:
            continue
        lab = calibration.original_lab_run(run_id)
        names = [item["name"] for item in listed]
        for item in listed:
            name = item.get("name")
            if not isinstance(name, str) or not name.startswith("af-perf-%s-" % lab) \
                    or names.count(name) != 1 or item.get("expired") is not False:
                continue
            data = collector.download(transport._artifact(item, run_id))
            if data is None:
                continue
            doc, files, views = transport.read_transport(data, collector.remaining)
            _write_artifact(os.path.join(target, name), doc, files, views)
    _write_json(os.path.join(target, calibration.ACTIONS_FILE),
                {"total_count": len(artifacts), "artifacts": artifacts})
    return target


def fetch_bridge_runs(collector, record, target):
    """Write a bridge's two retained runs, the golden check's and the post-merge A/A's, below
    `target`'s equivalence directory: each run's complete artifact listing must show its run
    bundle exactly once and unexpired, and every shard bundle at most once; a missing, expired
    or ambiguous artifact refuses the activation rather than leaving a gap."""
    root = os.path.join(target, EQUIVALENCE_DIR)
    os.makedirs(root)
    for role in equivalence_module.ROLES:
        run_id, lab = record[role]["actions_run_id"], record[role]["lab_run_id"]
        # The producer of the run (ADR-0185): the run, the exact attempt its lab run ID names
        # and that attempt's complete job listing, verified before its artifacts qualify.
        attempt = lab_attempt(lab, run_id, role)
        directory = os.path.join(root, PROVENANCE_DIR, role)
        os.makedirs(directory)
        _write_json(os.path.join(directory, BRIDGE_RUN_FILE), _slim_run(collector.get_json(
            "/actions/runs/%d" % run_id, "%s run %d" % (role, run_id))))
        _write_json(os.path.join(directory, BRIDGE_ATTEMPT_FILE), _slim_run(collector.get_json(
            "/actions/runs/%d/attempts/%d" % (run_id, attempt),
            "attempt %d of %s run %d" % (attempt, role, run_id))))
        jobs = _complete_listing(collector, "/actions/runs/%d/attempts/%d/jobs"
                                 % (run_id, attempt), "jobs", JOBS_PAGES_MAX,
                                 "the jobs of attempt %d of %s run %d" % (attempt, role, run_id))
        _write_json(os.path.join(directory, BRIDGE_JOBS_FILE),
                    {"total_count": len(jobs), "jobs": jobs})
        listed = _complete_listing(collector, "/actions/runs/%d/artifacts" % run_id,
                                   "artifacts", ARTIFACT_PAGES_MAX,
                                   "the artifact listing of run %d" % run_id)
        names = [item.get("name") for item in listed]
        for item in listed:
            require(isinstance(item.get("workflow_run"), dict)
                    and item["workflow_run"].get("id") == run_id,
                    "an artifact of run %d names another run" % run_id)
            name = item.get("name")
            if not isinstance(name, str) or not name.startswith("af-perf-%s-" % lab):
                continue
            require(names.count(name) == 1, "%s: artifact %s is listed %d times; neither is "
                    "chosen" % (role, name, names.count(name)))
            if name == "af-perf-%s-run" % lab:
                require(item.get("expired") is False, "%s: the run bundle %s has expired; "
                        "expired evidence never activates" % (role, name))
            if item.get("expired") is not False:
                continue
            data = collector.download(transport._artifact(item, run_id))
            require(data is not None, "%s: artifact %s could not be downloaded" % (role, name))
            doc, files, views = transport.read_transport(data, collector.remaining)
            _write_artifact(os.path.join(root, name), doc, files, views)
        require("af-perf-%s-run" % lab in names, "%s: run %d retains no run bundle of lab run "
                "%s" % (role, run_id, lab))
    return target


def _complete_listing(collector, path, key, pages_max, what):
    """Every item of a paginated listing whose pages each state the same `total_count`, read
    within `pages_max` pages: a listing longer than the bound, shorter or longer than its count,
    inconsistent between pages, or naming one item twice is refused, never shortened. The bound
    is a refusal, not a truncation."""
    items, ids, total = [], set(), None
    for page in range(1, pages_max + 1):
        doc = collector.get_json("%s?per_page=%d&page=%d" % (path, PAGE_SIZE, page), what)
        require(isinstance(doc, dict) and isinstance(doc.get(key), list)
                and type(doc.get("total_count")) is int and doc["total_count"] >= 0,
                "%s has no %s and total_count" % (what, key))
        if total is not None and doc["total_count"] != total:
            raise ContractError("%s changed its total_count from %d to %d between pages; it is "
                                "not one listing" % (what, total, doc["total_count"]))
        total = doc["total_count"]
        for item in doc[key]:
            require(isinstance(item, dict) and type(item.get("id")) is int,
                    "%s holds an item without an integer id" % what)
            require(item["id"] not in ids, "%s lists %s %d twice" % (what, key, item["id"]))
            ids.add(item["id"])
            items.append(item)
        if len(items) >= total or not doc[key]:
            break
    require(len(items) == total, "%s holds %d of its %d %s within %d pages of %d; an incomplete "
            "or inconsistent listing is never original evidence"
            % (what, len(items), total, key, pages_max, PAGE_SIZE))
    return items


def _write_json(path, doc):
    with open(path, "x", encoding="utf-8") as stream:
        json.dump(doc, stream, sort_keys=True, separators=(",", ":"))


def _write_artifact(target, doc, files, views):
    """An artifact's checked members as the directory `download-artifact` writes."""
    members = [(transport.TRANSPORT_FILE, contracts.write_bytes(doc, "transport"))]
    members += [(transport.BUNDLE_PREFIX + name, data) for name, data in sorted(files.items())]
    members += [(transport.VIEWS_PREFIX + name, data) for name, data in views]
    os.makedirs(target)
    for name, data in members:
        path = os.path.join(target, *name.split("/"))
        os.makedirs(os.path.dirname(path), 0o755, exist_ok=True)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)


def summary_markdown(doc):
    """The verification's honest summary: what was verified, the scope, and what is pending."""
    lines = ["### AF activation verification: %s admission, %s" % (
                 doc["admission"], "fetched and recomputed" if doc["mode"] == "fetched"
                 else "reviewed records checked locally"), "",
             "- Authority %s, policy %s, cohort %s" % (doc["authority_sha"], doc["policy_digest"],
                                                       doc["cohort_id"]),
             "- A/A within tolerance: %d of %d; power trials: %d of %d whole trials"
             % (doc["tallies"]["aa_within_tolerance"], doc["tallies"]["aa_members"],
                doc["tallies"]["power_qualifying"], doc["tallies"]["power_members"]),
             "- Calibrated scope: %s / %s on %s" % (doc["environment_id"], doc["recipe_id"],
                                                     ", ".join(doc["calibrated_groups"]))]
    for gid, cases in sorted(doc["uncalibrated_groups"].items()):
        lines.append("- Not calibrated: group %s (%s)" % (gid, ", ".join(cases)))
    if doc["equivalence"] is not None:
        lines.append("- Equivalent adapter over calibrated authority %s; protected changes: %s"
                     % (doc["equivalence"]["calibrated_authority_sha"],
                        ", ".join(doc["equivalence"]["changed_paths"]) or "none"))
        for role in equivalence_module.ROLES:
            run = doc["equivalence"][role]
            lines.append("- Verified %s: run %s (Actions %d), %s -> %s through adapters %s / %s"
                         % (role.replace("_", " "), run["lab_run_id"], run["actions_run_id"],
                            run["baseline_sha"][:12], run["candidate_sha"][:12],
                            run["adapters"]["baseline"], run["adapters"]["candidate"]))
    lines.append("- Pending: %s" % ", ".join(doc["pending"]))
    lines += ["", "Enforcement stays advisory until an operator makes the check required.", ""]
    return "\n".join(lines)

