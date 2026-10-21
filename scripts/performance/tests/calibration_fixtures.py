"""Synthetic calibration cohorts for the readiness tests; test code only (ADR-0181 to ADR-0183).

A `Lab` declares a cohort over the synthetic registry and dispatches each member as the
workflow does: a workflow run named for the cohort's digest and the member and numbered after
the cohort's anchor, listed as the list-workflow-runs API does (every event, newest first, down
to the anchor run), with its attempt-1 record as the get-a-workflow-run-attempt API returns it
and its measuring jobs as the list-jobs-for-a-workflow-run-attempt API returns them. It freezes each member's calibration plan through the real planner, writes its
evidence as measured-labelled synthetic bundles, stages them as the `af-perf-*` artifacts the
workflow uploads, and lists them as the Actions artifact API does, without any `run_attempt`.
Every number here is synthetic: these fixtures establish orchestration and arithmetic, never
empirical readiness or hosted provenance. Everything is generated at test time.
"""
import atexit
import copy
import hashlib
import json
import os
import shutil
import tempfile
import time
from fractions import Fraction
from unittest import mock

from perflab import calibration, compare, contracts, planning, schedule, synthetic, transport
from perflab.values import canonical_digest, format_decimal

from tests import support

AUTHORITY = synthetic.label_sha("authority")
CPU = "Synthetic CPU @ 2.0GHz"
IMAGE = synthetic.label_digest("image")
START = "2025-10-09"
REPOSITORY = "afactory/af"
WORKFLOW = ".github/workflows/performance.yml"
A0_MS = Fraction(100)
COST_NS = Fraction(50)
# The performance workflow's run number when the cohort is declared, and that run.
ANCHOR = 41
ANCHOR_RUN = 6990
HOSTED_LABEL = "ubuntu-latest"


# Recomputing a verdict is a pure function of a run directory's bytes, and copies of one lab
# hold the same bundles many times: each distinct directory is evaluated once per process. The
# directory is still assembled from the evidence by `calibration.collect` every time.
_EVALUATIONS = {}
_EVALUATE = compare.evaluate


def _tree_digest(root):
    digest = hashlib.sha256()
    for current, dirs, files in sorted(os.walk(root)):
        dirs.sort()
        for name in sorted(files):
            path = os.path.join(current, name)
            with open(path, "rb") as stream:
                data = stream.read()
            digest.update(("%s\0%d\0" % (os.path.relpath(path, root), len(data))).encode())
            digest.update(data)
    return digest.hexdigest()


def evaluate_once(run_dir):
    key = _tree_digest(run_dir)
    if key not in _EVALUATIONS:
        _EVALUATIONS[key] = _EVALUATE(run_dir)
    return _EVALUATIONS[key]


def stamp(day, minute, second=0):
    return "2025-10-%02dT%02d:%02d:%02dZ" % (9 + day, 6 + minute // 60, minute % 60, second)


def hosted_job(name, actions_run, index, attempt=1, label=HOSTED_LABEL, head_sha=AUTHORITY):
    """One job of a workflow-run attempt as the jobs API lists it, on a standard hosted
    runner; `index` makes its ID unique within the run."""
    return {"id": actions_run * 100 + index, "run_id": actions_run, "run_attempt": attempt,
            "name": name, "status": "completed", "conclusion": "success",
            "head_sha": head_sha, "labels": [label], "runner_id": 7,
            "runner_name": "GitHub Actions 7", "runner_group_id": 0,
            "runner_group_name": "GitHub Actions"}


class Lab(object):
    def __init__(self, root, groups=None, days=5, authority=AUTHORITY):
        self.root = root
        self.authority = authority
        self.suite = support.synthetic_suite()
        self.groups = groups or [{"group_id": "hosted_x86", "cpu_model": CPU, "image": IMAGE}]
        self.cohort = calibration.declare(
            self.suite, authority, "synthetic_cohort", "quick", "linux_x86_64_gnu_hosted",
            "linux_gnu_native_release", self.groups, START, REPOSITORY, ANCHOR, days,
            preparation_members=2, preparation_iterations=1000000)
        self.declared = dict((member["member_id"], member) for member in self.cohort["members"])
        self.evidence = os.path.join(root, "evidence")
        os.makedirs(self.evidence)
        self.artifacts = []
        # The anchor: the workflow's latest run when the cohort was declared, a push.
        self.workflow_runs = {ANCHOR_RUN: {
            "id": ANCHOR_RUN, "run_number": ANCHOR, "run_attempt": 1,
            "display_title": "Merge the cohort declaration", "event": "push", "path": WORKFLOW,
            "head_branch": "main", "head_sha": self.authority,
            "repository": {"full_name": REPOSITORY}, "created_at": stamp(-1, 0),
            "run_started_at": stamp(-1, 0), "updated_at": stamp(-1, 1), "status": "completed",
            "conclusion": "success"}}
        self.next_run = 7000
        self.next_number = ANCHOR
        self.next_artifact = 100
        self.calibration = None
        self.changed_producers = set()

    def plan(self, member_id, run_id, calibration_doc=None):
        request = planning.Request(
            trigger="manual", tier="quick", scope="calibration", base=self.authority,
            head=self.authority, authority=self.authority, control=self.authority,
            run_id=run_id,
            cohort=self.cohort, member=member_id, calibration=calibration_doc,
            build_image=IMAGE)
        ids = planning.Identities()
        ids.baseline = ids.candidate = ids.control = self.authority
        ids.trigger = {"kind": "manual", "remeasure_of": None}
        digests = {"suite_sha256": self.suite.raw_sha256,
                   "policy_digest": self.suite.policy_digest,
                   "instrument_digest": synthetic.label_digest("instrument"),
                   "fixture_digest": synthetic.label_digest("fixtures")}
        lock = {"cargo_lock_sha256": synthetic.label_digest("authority/lock")}
        inputs = {"authority": lock, "baseline": lock, "candidate": lock}
        return planning.freeze(self.suite, request, ids, "proposed", self.authority, None,
                               digests, [], inputs)

    def replan(self, member, plan):
        """The trusted authority's plan of `member` again, as `calibration.replanner` freezes it
        from a repository: this registry, with the retained plan's run ID."""
        level = contracts.member_level(member)
        return self.plan(member["member_id"], plan["run_id"], self.calibration
                         if level not in (None, "preparation") else None)

    def producer(self, run, plan):
        """The trusted producer check of `calibration.producer_check`, over this lab: the
        workflow at a run's head commit and the scheduling code at its plan's control are the
        authority's unless the head or control is one of `changed_producers`."""
        for sha in (run["head_sha"], plan["resolved"]["control_sha"]):
            if sha in self.changed_producers:
                return "%s changed the producer" % sha
        return None

    def title(self, member_id, digest=None):
        return contracts.DISPATCH_RUN_NAME.format(
            cohort_digest=digest or canonical_digest(self.cohort), member_id=member_id)

    def dispatch(self, member_id, day=None, minute=0, attempt=1, actions_run=None, title=None,
                 producer=None, status="completed", conclusion="success", attempt_record=True):
        """A workflow run of the performance workflow dispatched for `member_id`, listed and
        with its attempt-1 record, and nothing it uploaded; the Actions run ID. An existing
        `actions_run` is rerun as `attempt`."""
        member = self.declared.get(member_id)
        if day is None:
            day = member["day"] if member is not None else 0
        started, updated = stamp(day, minute), stamp(day, minute, 30)
        if actions_run in self.workflow_runs:
            run = self.workflow_runs[actions_run]
            run.update(run_attempt=max(run["run_attempt"], attempt), run_started_at=started,
                       updated_at=updated)
            return actions_run
        actions_run = actions_run or self._run_id()
        self.next_number += 1
        run = {"id": actions_run, "run_number": self.next_number, "run_attempt": attempt,
               "display_title": title or self.title(member_id), "event": "workflow_dispatch",
               "path": WORKFLOW, "head_branch": "main", "head_sha": self.authority,
               "repository": {"full_name": REPOSITORY}, "created_at": started,
               "run_started_at": started, "updated_at": updated, "status": status,
               "conclusion": conclusion}
        run.update(producer or {})
        self.workflow_runs[actions_run] = run
        if attempt_record:
            directory = os.path.join(self.evidence, calibration.ATTEMPTS_DIR)
            os.makedirs(directory, exist_ok=True)
            with open(os.path.join(directory, "%d.json" % actions_run), "w") as stream:
                json.dump(dict(run, run_attempt=1), stream)
        return actions_run

    def run(self, member_id, day=None, minute=0, attempt=1, actions_run=None, dose=None,
            values=None, cpu=CPU, health_shift=False, omit_shard=None, oracle_failure=None,
            calibration_doc="sealed", plan_edit=None, jobs_edit=None, **dispatch):
        """Dispatch and execute one member synthetically and leave its artifacts and attempt
        1's job records (`jobs_edit` may change them); the Actions run ID."""
        member = self.declared[member_id]
        rerun = actions_run in self.workflow_runs
        actions_run = self.dispatch(member_id, day, minute, attempt, actions_run, **dispatch)
        run_id = "gh%d_a%d_primary" % (actions_run, attempt)
        sealed = self.calibration if calibration_doc == "sealed" else calibration_doc
        level = contracts.member_level(member)
        plan = self.plan(member_id, run_id, sealed if level not in (None, "preparation")
                         else None)
        if plan_edit is not None:
            plan_edit(plan)
            plan["plan_digest"] = contracts.plan_digest(plan)
        if attempt == 1 and not rerun:
            names = (["plan", "build (%s)" % run_id]
                     + ["shard (%s %d)" % (run_id, shard["index"]) for shard in plan["shards"]]
                     + ["aggregate (%s)" % run_id])
            jobs = [hosted_job(name, actions_run, index, head_sha=self.authority)
                    for index, name in enumerate(names)]
            if jobs_edit is not None:
                jobs_edit(jobs)
            directory = os.path.join(self.evidence, calibration.JOBS_DIR)
            os.makedirs(directory, exist_ok=True)
            with open(os.path.join(directory, "%d.json" % actions_run), "w") as stream:
                json.dump({"total_count": len(jobs), "jobs": jobs}, stream)
        injection = plan["cohort"]["injection"]
        counts = None
        if injection is not None:
            counts = [group for group in injection["groups"] if group["cpu_model"] == cpu]
            counts = counts[0]["iterations"] if counts else None

        def injected_ms(slot):
            if slot["side"] != schedule.CANDIDATE or counts is None:
                return Fraction(0)
            if dose is not None:
                return dose(slot)
            return counts[slot["instance_id"]] * COST_NS / 1000000

        def value(slot, case):
            if values is not None:
                return values(slot, case)
            return A0_MS + injected_ms(slot)

        observed = dict(synthetic.SYNTHETIC_HOST, cpu_model={"value": cpu})

        def edit(record):
            record["host_id"] = contracts.host_id(observed)
            if injection is not None and record["instance_id"] != plan["health"]["workload"]:
                slot = {"side": record["side"], "instance_id": record["instance_id"]}
                record["metrics"][calibration.INJECTED_METRIC] = {
                    "value": format_decimal(injected_ms(slot)), "unit": "ms"}
                record["metric_scopes"][calibration.INJECTED_METRIC] = "probe_reported"
            if oracle_failure is not None and record["side"] == schedule.CANDIDATE \
                    and record["instance_id"] == oracle_failure and record["phase"] == "measured":
                record.update(status="failed",
                              failure={"kind": "oracle_mismatch", "detail": "synthetic"})
                record["oracle"]["status"] = "failed"
                record["metrics"] = {}
                record["metric_scopes"] = {}
                record["unavailable_metrics"][calibration.INJECTED_METRIC] = "the sample failed"
            return [record]

        def sentinel(slot):
            return Fraction(10) if not health_shift or slot["phase"] == "sentinel_start" \
                else Fraction(30)

        run_dir = os.path.join(self.root, "runs", run_id)
        synthetic.write_run(plan, run_dir, value, sentinel=sentinel, edit=edit,
                            provenance="measured")
        for shard in plan["shards"]:
            root = os.path.join(run_dir, "shards", str(shard["index"]))

            def host(doc):
                doc["host"] = observed
                doc["host_id"] = contracts.host_id(observed)
            synthetic.reseal_shard(root, host)
        created = stamp(member["day"] if day is None else day, minute, 20)
        self._stage(run_dir, run_id, actions_run, created, "run_bundle", None)
        for shard in plan["shards"]:
            if shard["index"] != omit_shard:
                self._stage(run_dir, run_id, actions_run, created, "shard_bundle",
                            shard["index"])
        return actions_run

    def clone(self, root):
        """An independent copy of this lab's cohort, dispatches and evidence under `root`."""
        lab = copy.copy(self)
        lab.root = root
        lab.evidence = os.path.join(root, "evidence")
        shutil.copytree(self.evidence, lab.evidence)
        lab.artifacts = copy.deepcopy(self.artifacts)
        lab.workflow_runs = copy.deepcopy(self.workflow_runs)
        lab.changed_producers = set(self.changed_producers)
        return lab

    def forget(self, member_id):
        """Remove every dispatch of `member_id` and all it uploaded, as if it never ran."""
        gone = [ident for ident, run in self.workflow_runs.items()
                if run["display_title"] == self.title(member_id)]
        for ident in gone:
            del self.workflow_runs[ident]
            for directory in (calibration.ATTEMPTS_DIR, calibration.JOBS_DIR):
                record = os.path.join(self.evidence, directory, "%d.json" % ident)
                if os.path.exists(record):
                    os.remove(record)
        for item in [item for item in self.artifacts if item["workflow_run"]["id"] in gone]:
            self.artifacts.remove(item)
            shutil.rmtree(os.path.join(self.evidence, item["name"]))

    def delete_run(self, actions_run):
        """Delete one workflow run as GitHub does: its record, its attempts and jobs, and every
        artifact it uploaded disappear; the remaining runs keep their numbers."""
        del self.workflow_runs[actions_run]
        for directory in (calibration.ATTEMPTS_DIR, calibration.JOBS_DIR):
            record = os.path.join(self.evidence, directory, "%d.json" % actions_run)
            if os.path.exists(record):
                os.remove(record)
        for item in [item for item in self.artifacts
                     if item["workflow_run"]["id"] == actions_run]:
            self.artifacts.remove(item)
            shutil.rmtree(os.path.join(self.evidence, item["name"]))

    def bundle(self, actions_run):
        """The directory of the run bundle attempt 1 of `actions_run` uploaded."""
        return os.path.join(self.evidence, "af-perf-gh%d_a1_primary-run" % actions_run)

    def artifact(self, actions_run):
        """The artifact listing's entry of that run bundle."""
        name = "af-perf-gh%d_a1_primary-run" % actions_run
        return [item for item in self.artifacts if item["name"] == name][0]

    def attempt_record(self, actions_run):
        return os.path.join(self.evidence, calibration.ATTEMPTS_DIR, "%d.json" % actions_run)

    def jobs_record(self, actions_run):
        return os.path.join(self.evidence, calibration.JOBS_DIR, "%d.json" % actions_run)

    def other_run(self, title="Pull request #9", event="pull_request", branch="feature"):
        """A run of the workflow for another event, numbered in order like every run."""
        actions_run = self._run_id()
        self.next_number += 1
        self.workflow_runs[actions_run] = {
            "id": actions_run, "run_number": self.next_number, "run_attempt": 1,
            "display_title": title, "event": event, "path": WORKFLOW, "head_branch": branch,
            "head_sha": self.authority, "repository": {"full_name": REPOSITORY},
            "created_at": stamp(3, 0), "run_started_at": stamp(3, 0),
            "updated_at": stamp(3, 1), "status": "completed", "conclusion": "success"}
        return actions_run

    def _run_id(self):
        self.next_run += 1
        return self.next_run

    def _stage(self, run_dir, run_id, actions_run, created, content, shard):
        name = ("af-perf-%s-run" % run_id if shard is None
                else "af-perf-%s-shard-%d" % (run_id, shard))
        transport.stage(os.path.join(self.evidence, name), run_dir, content, shard)
        self.next_artifact += 1
        self.artifacts.append({"id": self.next_artifact, "name": name, "size_in_bytes": 1,
                               "expired": False, "created_at": created,
                               "workflow_run": {"id": actions_run, "repository_id": 1,
                                                "head_repository_id": 1, "head_branch": "main",
                                                "head_sha": self.authority}})

    def write_listing(self, runs=None):
        with open(os.path.join(self.evidence, calibration.ACTIONS_FILE), "w") as stream:
            json.dump({"total_count": len(self.artifacts), "artifacts": self.artifacts}, stream)
        # Newest first, as the list-workflow-runs API pages them, down to the anchor run.
        listed = sorted(self.workflow_runs.values(), key=lambda run: -run["run_number"])
        with open(os.path.join(self.evidence, calibration.RUNS_FILE), "w") as stream:
            json.dump(runs or {"total_count": len(listed), "workflow_runs": listed}, stream)

    def run_phase(self, kinds, skip=(), **options):
        minute = 0
        for member in self.cohort["members"]:
            if member["kind"] in kinds and member["member_id"] not in skip:
                minute += 1
                self.run(member["member_id"], minute=minute, **options)

    def collect(self):
        self.write_listing()
        scratch = os.path.join(self.root, "scratch-%d" % time.monotonic_ns())
        os.makedirs(scratch)
        with mock.patch.object(compare, "evaluate", evaluate_once):
            return calibration.collect(self.cohort, self.evidence, scratch, self.replan,
                                       self.producer)

    def prepare(self):
        runs, refused, _ = self.collect()
        self.calibration = calibration.prepare(self.cohort, runs,
                                               calibration.tolerances_of(runs, self.cohort))
        return self.calibration

    def readiness(self):
        runs, refused, history = self.collect()
        return calibration.readiness(self.cohort, self.calibration, runs, refused, history)


class Shared(object):
    """One complete synthetic cohort of a real Git authority holding the synthetic registry:
    the prepared lab (A/A and preparation run, parameters sealed from them) and the ready lab
    (every trial run too), with the cohort's declared bytes and its readiness. Built once per
    process and shared by the calibration and activation tests; every number is synthetic."""
    root = repo = authority = prepared = declared_bytes = sealed = lab = doc = None


_SHARED = []


def authority_repo(path):
    """A repository whose first commit holds the synthetic registry and stand-in instruments:
    an authority whose registry planning and activation read from Git."""
    return support.lab_repo(path, support.read(support.SYNTHETIC_SUITE))


def shared():
    if not _SHARED:
        cohort = Shared()
        cohort.root = tempfile.mkdtemp(prefix="af-perf-cohort-")
        atexit.register(shutil.rmtree, cohort.root, True)
        cohort.repo, cohort.authority = authority_repo(os.path.join(cohort.root, "repo"))
        cohort.prepared = Lab(os.path.join(cohort.root, "prepared"), authority=cohort.authority)
        cohort.declared_bytes = contracts.write_bytes(cohort.prepared.cohort, "cohort")
        cohort.prepared.run_phase(("aa", "preparation"))
        cohort.sealed = cohort.prepared.prepare()
        cohort.lab = cohort.prepared.clone(os.path.join(cohort.root, "ready"))
        cohort.lab.run_phase(("injection", "power"))
        cohort.doc = cohort.lab.readiness()
        _SHARED.append(cohort)
    return _SHARED[0]


def forged_parameters(sealed, cohort, a0):
    """Self-consistent parameters with every A0 replaced by `a0` ms and every efficiency,
    target and loop count regenerated: they validate as a document, and only recomputing them
    from the originals shows they are not the cohort's."""
    from perflab.values import format_rational
    forged = copy.deepcopy(sealed)
    for group in forged["groups"]:
        if group["a0"] is None:
            continue
        group["a0"] = format_rational(a0)
        eff = contracts.efficiency(Fraction(group["relative"]), Fraction(group["absolute"]), a0)
        group.update(efficiency=format_rational(eff),
                     power_target=format_rational(contracts.power_target(eff)))
        if group["loop_ns_per_iteration"] is not None:
            cost = Fraction(group["loop_ns_per_iteration"])
            group["iterations"] = dict(
                (level, contracts.loop_iterations(contracts.target_ms(group, level), cost))
                for level in contracts.LOOP_LEVELS)
    return contracts.validate_calibration(forged, "forged", cohort=cohort)


def member_record(member, status="complete", cases=None, group="hosted_x86", reasons=None,
                  digest=None):
    """One readiness member record of a cohort of `digest` for the arithmetic tests."""
    if reasons is None:
        reasons = {"complete": [], "missing": ["no_dispatch"], "interrupted": ["shard_missing"],
                   "failed": ["shard_failed"], "unhealthy": ["health_unstable"],
                   "invalid": ["off_schedule"]}[status]
    run = 9000 + member["seed"] % 1000000
    provenance = None if "no_dispatch" in reasons else {
        "actions_run_id": run, "run_number": run, "run_attempt": 1,
        "display_title": contracts.DISPATCH_RUN_NAME.format(
            cohort_digest=digest or synthetic.label_digest("cohort"),
            member_id=member["member_id"]),
        "head_sha": AUTHORITY, "attempt_started_unix_ms": 0, "attempt_updated_unix_ms": 0,
        "attempt_conclusion": "success", "lab_run_id": "gh%d_a1_primary" % run,
        "artifact_name": "af-perf-gh%d_a1_primary-run" % run, "artifact_id": run,
        "artifact_created_unix_ms": 0,
        "plan_digest": synthetic.label_digest(member["member_id"]),
        "runner": {"label": HOSTED_LABEL, "runner_group": "GitHub Actions", "jobs": 3}}
    complete = status == "complete"
    return {"member_id": member["member_id"], "kind": member["kind"], "status": status,
            "reasons": reasons, "provenance": provenance,
            "cpu_group": group if complete else None,
            "verdict_sha256": synthetic.label_digest("verdict") if complete else None,
            "cases": (cases or {}) if complete else {}, "qualifies": False}
