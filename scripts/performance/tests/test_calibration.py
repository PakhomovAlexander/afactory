"""Calibration cohorts, preparation, injection and readiness (ADR-0181 to ADR-0183).

Every number here is synthetic and labelled so: these tests establish the orchestration and
the exact readiness arithmetic, never empirical readiness, which needs the real hosted cohort
over at least five days.
"""
import copy
import io
import json
import os
import shutil
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest import mock
from fractions import Fraction

from perflab import calibration, contracts, gitrefs, runner, schedule, stats, synthetic
from perflab import transport
from perflab.values import ContractError, canonical_digest, format_rational

from tests import calibration_fixtures as fixtures
from tests import support

REGRESSION, WITHIN = stats.REGRESSION, stats.WITHIN_TOLERANCE


class Ready(object):
    """One complete synthetic cohort, run end to end through the real planner, synthetic
    bundles staged as the workflow's artifacts, preparation and readiness; built once
    (`calibration_fixtures.shared`). `prepared` has run only the A/A and preparation members
    and sealed the parameters from them: every lab a test changes is a copy of one of them."""
    tmp = prepared = lab = declared_bytes = sealed = doc = None


def setUpModule():
    cohort = fixtures.shared()
    Ready.tmp = tempfile.mkdtemp(prefix="af-perf-calibration-")
    Ready.prepared, Ready.lab = cohort.prepared, cohort.lab
    Ready.declared_bytes, Ready.sealed, Ready.doc = (cohort.declared_bytes, cohort.sealed,
                                                     cohort.doc)
    Deleted.base = Ready.prepared.clone(os.path.join(Ready.tmp, "eight"))
    Deleted.originals = {}
    minute = 0
    for member in Deleted.base.cohort["members"]:
        if member["kind"] in ("injection", "power"):
            minute += 1
            ident = member["member_id"]
            if ident in ("power_01", "power_02"):
                Deleted.originals[ident] = Deleted.base.run(
                    ident, minute=minute, values=lambda slot, case: fixtures.A0_MS)
            else:
                Deleted.base.run(ident, minute=minute)


def tearDownModule():
    shutil.rmtree(Ready.tmp, True)


class Deleted(object):
    """A ready lab's copy whose power_01 and power_02 originals did not detect: 8 of 10."""
    base = originals = None


def prepared(path):
    """A copy of the prepared lab: A/A and preparation run, parameters sealed, no trial yet."""
    return Ready.prepared.clone(path)



class MixedHostedGroups(unittest.TestCase):
    """Each case uses its own paired shard's CPU, even in a mixed-host whole trial."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="af-perf-mixed-groups-")
        cls.other_cpu = "Synthetic second CPU"
        cls.groups = [
            {"group_id": "hosted_x86", "cpu_model": fixtures.CPU, "image": fixtures.IMAGE},
            {"group_id": "second", "cpu_model": cls.other_cpu, "image": fixtures.IMAGE}]
        cls.options = {"baseline_ms": {fixtures.CPU: Fraction(100), cls.other_cpu: Fraction(200)},
                       "loop_cost_ns": {fixtures.CPU: Fraction(50), cls.other_cpu: Fraction(25)}}
        cls.cpus = {0: fixtures.CPU, 1: cls.other_cpu}
        cls.lab = fixtures.Lab(os.path.join(cls.tmp, "source"), groups=cls.groups)
        for index, member in enumerate(cls.lab.cohort["members"]):
            if member["kind"] in ("aa", "preparation"):
                cpus = cls.cpus if index % 2 == 0 else {0: cls.other_cpu, 1: fixtures.CPU}
                cls.lab.run(member["member_id"], minute=index, cpu=cpus, **cls.options)
        cls.sealed = cls.lab.prepare()
        cls.prepared = cls.lab.clone(os.path.join(cls.tmp, "prepared"))
        cls.lab.run_phase(("injection", "power"), cpu=cls.cpus, **cls.options)
        cls.doc = cls.lab.readiness()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, True)

    def test_mixed_shards_preserve_per_case_preparation_and_whole_trial_counts(self):
        self.assertTrue(self.doc["ready"])
        self.assertEqual(self.doc["tallies"]["aa_within_tolerance"], 20)
        self.assertEqual(self.doc["tallies"]["power_qualifying"], 10)
        self.assertEqual(self.doc["tallies"]["power_by_group"], {
            "hosted_x86": {"members": 10, "qualifying": 10},
            "second": {"members": 10, "qualifying": 10}})
        for case in self.lab.cohort["cases"]:
            first = contracts.calibration_group(self.sealed, case["instance_id"], "hosted_x86")
            second = contracts.calibration_group(self.sealed, case["instance_id"], "second")
            self.assertEqual(first["a0"], format_rational(Fraction(100)))
            self.assertEqual(second["a0"], format_rational(Fraction(200)))
            self.assertEqual(first["iterations"]["power"], 200000)
            self.assertEqual(second["iterations"]["power"], 800000)
        member = next(item for item in self.doc["members"] if item["member_id"] == "power_01")
        self.assertEqual(set(member["cpu_groups"].values()), {"hosted_x86", "second"})
        for ident, case in member["cases"].items():
            target = Fraction(10 if member["cpu_groups"][ident] == "hosted_x86" else 20)
            self.assertEqual(case["target_ms"], format_rational(target))
            self.assertTrue(case["injection_valid"])

    def test_one_unknown_shard_invalidates_the_whole_original(self):
        lab = self.prepared.clone(os.path.join(self.tmp, "unknown"))
        lab.run("power_01", minute=1, cpu={0: fixtures.CPU, 1: "Unlisted CPU"}, **self.options)
        doc = lab.readiness()
        member = next(item for item in doc["members"] if item["member_id"] == "power_01")
        self.assertEqual(member["status"], "invalid")
        self.assertIn("cpu_group_mismatch", member["reasons"])
        self.assertFalse(member["qualifies"])
        self.assertEqual(doc["tallies"]["power_members"], 10)
        self.assertEqual(doc["tallies"]["power_qualifying"], 0)

    def test_wrong_case_set_is_retained_as_an_invalid_original(self):
        lab = fixtures.Lab(os.path.join(self.tmp, "wrong-cases"))
        lab.run_phase(("aa",), skip=("aa_20",))

        def rename(plan):
            before, after = "synthetic_advisory", "synthetic_advisory_changed"
            for case in plan["cases"]:
                if case["instance_id"] == before:
                    case["instance_id"] = after
                    case["warmup_orders"] = schedule.block_orders(
                        plan["seed"], after, "warmup", case["warmup_pairs"])
                    case["measured_orders"] = schedule.block_orders(
                        plan["seed"], after, "measured", case["pairs"])
            for shard in plan["shards"]:
                shard["instances"] = [after if ident == before else ident
                                      for ident in shard["instances"]]

        lab.run("aa_20", minute=20, plan_edit=rename)
        lab.run_phase(("preparation",))
        lab.prepare()
        doc = lab.readiness()
        member = next(item for item in doc["members"] if item["member_id"] == "aa_20")
        self.assertEqual(member["status"], "invalid")
        self.assertIn("wrong_plan", member["reasons"])
        self.assertEqual(set(member["cpu_groups"]),
                         {case["instance_id"] for case in lab.cohort["cases"]})
        self.assertIsNone(member["cpu_groups"]["synthetic_advisory"])
        self.assertEqual(doc["tallies"]["aa_members"], 20)
        self.assertEqual(doc["tallies"]["aa_within_tolerance"], 19)
        self.assertFalse(doc["ready"])

    def test_missing_or_relabelled_case_group_cannot_validate(self):
        for change in ("missing", "relabelled", "unknown"):
            with self.subTest(change=change):
                doc = copy.deepcopy(self.doc)
                member = next(item for item in doc["members"] if item["member_id"] == "power_01")
                ident = next(ident for ident, group in member["cpu_groups"].items()
                             if group == "second")
                if change == "missing":
                    del member["cpu_groups"][ident]
                else:
                    member["cpu_groups"][ident] = "hosted_x86" if change == "relabelled" else "unknown"
                with self.assertRaises(ContractError):
                    contracts.validate_readiness(doc, cohort=self.lab.cohort, calibration=self.sealed)

class ReadyCohort(unittest.TestCase):
    def setUp(self):
        self.lab, self.sealed, self.doc = Ready.lab, Ready.sealed, Ready.doc
        self.declared_bytes, self.tmp = Ready.declared_bytes, Ready.tmp

    def test_a_complete_cohort_is_ready_from_recomputed_evidence(self):
        doc = self.doc
        self.assertTrue(doc["ready"])
        self.assertEqual((doc["tallies"]["aa_within_tolerance"], doc["tallies"]["aa_members"]),
                         (20, 20))
        self.assertEqual((doc["tallies"]["power_qualifying"], doc["tallies"]["power_members"]),
                         (10, 10))
        self.assertEqual(doc["groups"], {"hosted_x86": {"ready": True, "unavailable": []}})
        # The parameters were recomputed from the originals and are the sealed ones, and every
        # run after the cohort's anchor was listed.
        self.assertEqual(doc["recomputed_calibration_digest"], canonical_digest(self.sealed))
        self.assertEqual(doc["history"], {"after_run_number": fixtures.ANCHOR,
                                          "latest_run_number": fixtures.ANCHOR + 62,
                                          "runs": 62})
        member = [item for item in doc["members"] if item["member_id"] == "power_01"][0]
        self.assertEqual(member["provenance"]["runner"],
                         {"label": "ubuntu-latest", "runner_group": "GitHub Actions",
                          "jobs": 3})
        self.assertEqual(member["provenance"]["attempt_conclusion"], "success")
        # Descriptive trials are reported, not gated: 2% and 5% fall inside the tolerance.
        self.assertEqual(doc["tallies"]["injection"]["injection_10"]["regressions"], 10)
        self.assertEqual(doc["tallies"]["injection"]["injection_2"]["regressions"], 0)
        self.assertEqual(doc["claim"], contracts.READINESS_CLAIM)
        # The document passes its reader with the cohort and parameters it names.
        data = contracts.write_bytes(doc, "readiness")
        contracts.validate_readiness(contracts.read_json(data, "readiness"), "readiness",
                                     cohort=self.lab.cohort, calibration=self.sealed)

    def test_the_parameters_are_frozen_from_aa_and_preparation_samples(self):
        group = contracts.calibration_group(self.sealed, "synthetic_fast", "hosted_x86")
        # A0 is the median A/A baseline, e = max(5%, 1 ms / 100 ms), p = max(10%, 2e).
        self.assertEqual((group["a0"], group["efficiency"], group["power_target"]),
                         ("100", "1/20", "1/10"))
        self.assertEqual(group["a0_samples"], 20 * 12)
        self.assertEqual((group["loop_ns_per_iteration"], group["loop_samples"]), ("50", 2 * 12))
        self.assertEqual(group["iterations"], {"injection_2": 40000, "injection_5": 100000,
                                               "injection_10": 200000, "power": 200000})
        self.assertEqual(self.sealed["a0_member_ids"],
                         ["aa_%02d" % index for index in range(1, 21)])

    def test_actions_ids_allocated_later_leave_the_cohort_bytes_and_digest_unchanged(self):
        self.assertEqual(contracts.write_bytes(self.lab.cohort, "cohort"), self.declared_bytes)
        self.assertEqual(self.doc["cohort_digest"], canonical_digest(self.lab.cohort))
        self.assertNotIn(b"actions_run_id", self.declared_bytes)
        member = [item for item in self.doc["members"] if item["member_id"] == "power_01"][0]
        provenance = member["provenance"]
        self.assertEqual(provenance["run_attempt"], 1)
        self.assertEqual(provenance["lab_run_id"], "gh%d_a1_primary"
                         % provenance["actions_run_id"])
        self.assertEqual(provenance["artifact_name"],
                         "af-perf-%s-run" % provenance["lab_run_id"])
        # A cohort that writes an Actions run ID into a member is not this version's shape.
        edited = copy.deepcopy(self.lab.cohort)
        edited["members"][0]["actions_run_id"] = 1
        with self.assertRaises(ContractError):
            contracts.validate_cohort(edited)

    def test_slowdown_plans_inject_only_the_candidate_and_name_the_sealed_parameters(self):
        plan = self.lab.plan("power_01", "gh1_a1_primary", self.sealed)
        injection = plan["cohort"]["injection"]
        self.assertEqual(injection["feature"], "perf-calibration")
        self.assertEqual(injection["calibration_digest"], canonical_digest(self.sealed))
        self.assertEqual(injection["groups"][0]["iterations"],
                         {"synthetic_advisory": 200000, "synthetic_fast": 200000,
                          "synthetic_process__size_1": 200000})
        self.assertTrue(all(not budget["applicable"] for case in plan["cases"]
                            for budget in case["budgets"]))
        shard = object.__new__(runner.Shard)
        shard.plan = plan
        shard.injection = injection["groups"][0]
        slot = {"side": schedule.CANDIDATE, "phase": "measured",
                "instance_id": "synthetic_fast"}
        self.assertEqual(shard.injection_environment(slot),
                         {"AF_PERF_CALIBRATION_ITERATIONS": "200000"})
        for side, phase in ((schedule.BASELINE, "measured"), (schedule.BASELINE, "warmup"),
                            (schedule.BASELINE, "sentinel_start")):
            self.assertEqual(shard.injection_environment(dict(slot, side=side, phase=phase)),
                             {"AF_PERF_CALIBRATION_ITERATIONS": "0"})
        # An A/A member's plan is uninstrumented: no injection control at all.
        aa = self.lab.plan("aa_01", "gh2_a1_primary")
        self.assertIsNone(aa["cohort"]["injection"])
        shard.plan, shard.injection = aa, None
        self.assertEqual(shard.injection_environment(slot), {})
        # Preparation needs no parameters; a slowdown member cannot run without them, and an
        # A/A member cannot carry them.
        self.assertIsNone(self.lab.plan("prep_01", "gh3_a1_primary")["cohort"]["injection"][
            "calibration_digest"])
        with self.assertRaisesRegex(ContractError, "runs the sealed calibration"):
            self.lab.plan("power_01", "gh4_a1_primary")
        with self.assertRaisesRegex(ContractError, "without calibration parameters"):
            self.lab.plan("aa_01", "gh5_a1_primary", self.sealed)

    def test_calibration_builds_both_products_with_the_feature_and_never_the_supervisor(self):
        from perflab import builds

        def argv(plan, package, kind, name, features=True):
            builder = object.__new__(builds.Builder)
            builder.plan, builder.recipe = plan, plan["profile"]["recipe"]
            builder.zigbuild = None
            builder.toolchain = type("Toolchain", (), {"cargo": "/toolchain/bin/cargo"})()
            return builder.cargo("/worktree", package, kind, name, features)

        power = self.lab.plan("power_01", "gh1_a1_primary", self.sealed)
        aa = self.lab.plan("aa_01", "gh2_a1_primary")
        for package, kind, name in (("af", "--bin", "af"),
                                    ("review-pipeline", "--example", "perf-probe")):
            self.assertIn("perf-calibration", argv(power, package, kind, name))
            self.assertNotIn("perf-calibration", " ".join(argv(aa, package, kind, name)))
        supervisor = argv(power, "review-process", "--example", "perf-supervisor", False)
        self.assertNotIn("perf-calibration", " ".join(supervisor))

    def test_the_cli_prepares_and_recomputes_readiness_from_the_evidence(self):
        import cli
        target = os.path.join(self.tmp, "cli")
        os.makedirs(target)
        cohort = os.path.join(target, "cohort.json")
        with open(cohort, "wb") as stream:
            stream.write(self.declared_bytes)
        sealed = os.path.join(target, "calibration-parameters.json")
        with open(sealed, "wb") as stream:
            stream.write(contracts.write_bytes(self.sealed, "calibration-parameters"))
        output = os.path.join(target, "readiness.json")
        summary = os.path.join(target, "summary.md")
        # The synthetic authority is no Git commit: the CLI's replanner is given the lab's,
        # which freezes through the same planner (test_planning covers a repository's).
        replanned = []

        def replanner(git, cohort_doc, calibration_doc):
            replanned.append((git.repo, canonical_digest(cohort_doc),
                              canonical_digest(calibration_doc)))
            return self.lab.replan
        with mock.patch.object(calibration, "replanner", replanner), \
                mock.patch.object(calibration, "producer_check",
                                  lambda *args: self.lab.producer), \
                redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
            code = cli.main(["readiness", "--repo", target, "--cohort", cohort,
                             "--calibration", sealed, "--evidence", self.lab.evidence,
                             "--output", output, "--job-summary", summary])
        self.assertEqual(replanned, [(os.path.abspath(target), canonical_digest(self.lab.cohort),
                                      canonical_digest(self.sealed))])
        self.assertEqual(code, 0)
        self.assertEqual(contracts.read_file(output, "readiness"), self.doc)
        with open(summary) as stream:
            self.assertIn("Power trials valid in every critical case: 10 of 10", stream.read())


class Evidence(support.TempDir):
    """Member evidence bound to its declared logical identity, from retained provenance."""

    def lab(self):
        return prepared(self.path("lab"))

    def member(self, doc, member_id):
        return [item for item in doc["members"] if item["member_id"] == member_id][0]

    def refusals(self, doc):
        return sorted(set((item["source"], item["reason"], item["actions_run_id"])
                          for item in doc["refused_evidence"]))

    def test_a_rerun_a_replacement_and_strangers_never_stand_in_for_the_original(self):
        lab = self.lab()
        original = lab.run("power_01", minute=10, values=lambda slot, case: fixtures.A0_MS)
        # The same Actions run rerun (attempt 2) and a second dispatch of the same member both
        # produce a perfect regression; neither replaces the original non-detection.
        lab.run("power_01", minute=20, actions_run=original, attempt=2)
        replacement = lab.run("power_01", minute=30)
        # An attempt-2 artifact whose attempt-1 original uploaded nothing keeps it missing.
        rerun = lab.run("power_02", minute=40, attempt=2)
        doc = lab.readiness()
        self.assertEqual(self.refusals(doc), [
            ("artifact", "later_attempt", original), ("artifact", "later_attempt", rerun),
            ("artifact", "replacement", replacement), ("dispatch", "replacement", replacement)])
        first = self.member(doc, "power_01")
        self.assertEqual(first["provenance"]["actions_run_id"], original)
        self.assertEqual(first["status"], "complete")
        self.assertFalse(first["qualifies"])
        self.assertEqual(first["cases"]["synthetic_fast"]["classification"], WITHIN)
        second = self.member(doc, "power_02")
        self.assertEqual((second["status"], second["reasons"]), ("missing", ["artifact_missing"]))
        self.assertEqual(second["provenance"]["actions_run_id"], rerun)
        self.assertEqual(doc["tallies"]["power_members"], 10, "the denominator is fixed")
        self.assertFalse(doc["ready"])

    def test_a_members_own_dispatch_of_another_plan_is_invalid_and_strangers_are_refused(self):
        lab = self.lab()

        def unknown(plan):
            plan["cohort"]["member_id"] = "power_99"

        def other(plan):
            plan["cohort"]["cohort_digest"] = "sha256:" + "0" * 64
        lab.run("power_01", minute=1, plan_edit=unknown)
        lab.run("power_02", minute=2, plan_edit=other)
        # Runs named for an undeclared member, for another cohort, and for this cohort's member
        # by another branch's workflow bind no member, whatever they upload.
        stranger = lab.run("power_03", minute=3, title=lab.title("power_99"))
        elsewhere = lab.run("power_04", minute=4,
                            title=lab.title("power_04", "sha256:" + "1" * 64))
        foreign = lab.run("power_05", minute=5, producer={"head_branch": "feature"})
        doc = lab.readiness()
        for ident in ("power_01", "power_02"):
            self.assertEqual((self.member(doc, ident)["status"],
                              self.member(doc, ident)["reasons"]), ("invalid", ["wrong_plan"]))
        for ident in ("power_03", "power_04", "power_05"):
            self.assertEqual((self.member(doc, ident)["status"],
                              self.member(doc, ident)["reasons"]), ("missing", ["no_dispatch"]))
            self.assertIsNone(self.member(doc, ident)["provenance"])
        self.assertEqual(self.refusals(doc), [
            ("artifact", "foreign_dispatch", foreign), ("artifact", "other_cohort", elsewhere),
            ("artifact", "unknown_member", stranger), ("dispatch", "foreign_dispatch", foreign),
            ("dispatch", "unknown_member", stranger)])

    def test_missing_unhealthy_interrupted_and_misplaced_members_stay_in_the_denominator(self):
        lab = self.lab()
        lab.run("power_01", minute=1, health_shift=True)
        lab.run("power_02", minute=2, omit_shard=1)
        lab.run("power_03", minute=3, cpu="Another CPU")
        lab.run("power_04", day=0, minute=4)
        lab.run("power_05", minute=5, oracle_failure="synthetic_fast")
        lab.run("power_06", minute=6)
        doc = lab.readiness()
        observed = dict((item["member_id"], (item["status"], item["reasons"]))
                        for item in doc["members"] if item["kind"] == "power")
        self.assertEqual(observed["power_01"], ("unhealthy", ["health_unstable"]))
        self.assertEqual(observed["power_02"][0], "interrupted")
        self.assertIn("shard_missing", observed["power_02"][1])
        # A host of no allowed group could not have injected the declared dose.
        self.assertEqual(observed["power_03"][0], "invalid")
        self.assertIn("cpu_group_mismatch", observed["power_03"][1])
        # A member run off its declared day, here before the parameters were sealed.
        self.assertEqual(observed["power_04"][0], "invalid")
        self.assertIn("off_schedule", observed["power_04"][1])
        # A candidate whose oracle fails is that side's failure, never a detected regression.
        self.assertEqual(observed["power_05"], ("complete", []))
        self.assertEqual(self.member(doc, "power_05")["cases"]["synthetic_fast"][
            "classification"], "candidate_failure")
        self.assertFalse(self.member(doc, "power_05")["qualifies"])
        self.assertEqual(observed["power_06"], ("complete", []))
        self.assertTrue(self.member(doc, "power_06")["qualifies"])
        for ident in ("power_07", "power_08", "power_09", "power_10"):
            self.assertEqual(observed[ident], ("missing", ["no_dispatch"]))
        self.assertEqual(doc["tallies"]["power_members"], 10)
        self.assertEqual(doc["tallies"]["power_qualifying"], 1)
        self.assertEqual(doc["tallies"]["aa_members"], 20)
        self.assertEqual(doc["tallies"]["aa_within_tolerance"], 20)

    def test_a_trial_before_the_last_preparation_run_is_invalid(self):
        lab = fixtures.Lab(self.path("late"), authority=Ready.prepared.authority)
        lab.run_phase(("aa",))
        lab.run("prep_02", minute=1)
        lab.run("prep_01", day=3, minute=50)
        # The parameters are what these originals give: prep_01 is off its day and prepares
        # nothing, so the loop's cost comes from prep_02 alone.
        sealed = lab.prepare()
        self.assertEqual(sealed["groups"][0]["loop_samples"], 12)
        lab.run("power_01", day=3, minute=10)
        doc = lab.readiness()
        self.assertEqual(self.member(doc, "prep_01")["status"], "invalid")
        self.assertIn("before_calibration", self.member(doc, "power_01")["reasons"])

    def test_measured_injection_outside_its_band_is_a_non_detection(self):
        lab = self.lab()
        target = Fraction(10)
        for index, dose in enumerate((target * 3 / 4, target * 5 / 4,
                                      target * 3 / 4 - Fraction(1, 1000),
                                      target * 5 / 4 + Fraction(1, 1000)), 1):
            lab.run("power_%02d" % index, minute=index,
                    dose=lambda slot, dose=dose: dose)
        doc = lab.readiness()
        valid = [self.member(doc, "power_%02d" % index)["cases"]["synthetic_fast"][
            "injection_valid"] for index in range(1, 5)]
        self.assertEqual(valid, [True, True, False, False])
        self.assertEqual(self.member(doc, "power_01")["cases"]["synthetic_fast"]["target_ms"],
                         "10")
        self.assertEqual(self.member(doc, "power_03")["cases"]["synthetic_fast"][
            "injected_median_ms"], format_rational(Fraction(7499, 1000)))
        # The observed slowdown is still a regression; only the measured dose is invalid.
        self.assertEqual(self.member(doc, "power_04")["cases"]["synthetic_fast"][
            "classification"], REGRESSION)
        self.assertEqual([self.member(doc, "power_%02d" % index)["qualifies"]
                          for index in range(1, 5)], [True, True, False, False])

    def test_the_artifact_listing_carries_no_attempt_and_one_is_never_inferred(self):
        lab = self.lab()
        lab.run("power_01", minute=1)
        lab.artifacts[0]["workflow_run"]["run_attempt"] = 3
        with self.assertRaisesRegex(ContractError, "the artifact API has no run_attempt"):
            lab.readiness()


class OriginalDispatch(support.TempDir):
    """A member's original is its first dispatch, whether or not its evidence survived."""

    def lab(self):
        return prepared(self.path("lab"))

    def member(self, doc, member_id):
        return [item for item in doc["members"] if item["member_id"] == member_id][0]

    def refusals(self, doc):
        return sorted(set((item["source"], item["reason"], item["actions_run_id"])
                          for item in doc["refused_evidence"]))

    def test_an_original_that_failed_before_uploading_anything_is_never_replaced(self):
        lab = self.lab()
        failed = lab.dispatch("power_01", minute=1, conclusion="failure")
        replacement = lab.run("power_01", minute=5)
        doc = lab.readiness()
        member = self.member(doc, "power_01")
        self.assertEqual((member["status"], member["reasons"]),
                         ("missing", ["artifact_missing", "attempt_unsuccessful"]))
        self.assertEqual((member["provenance"]["actions_run_id"],
                          member["provenance"]["attempt_conclusion"],
                          member["provenance"]["artifact_id"]), (failed, "failure", None))
        self.assertFalse(member["qualifies"])
        self.assertEqual(self.refusals(doc), [("artifact", "replacement", replacement),
                                              ("dispatch", "replacement", replacement)])

    def test_unreadable_expired_or_omitted_originals_keep_their_identity(self):
        lab = self.lab()
        originals = dict((ident, lab.run(ident, minute=index)) for index, ident in enumerate(
            ("power_01", "power_02", "power_03"), 1))
        later = dict((ident, lab.run(ident, minute=10 + index)) for index, ident in enumerate(
            ("power_01", "power_02", "power_03"), 1))
        manifest = os.path.join(lab.bundle(originals["power_01"]), transport.TRANSPORT_FILE)
        os.chmod(manifest, 0o600)
        with open(manifest, "wb") as stream:
            stream.write(b"{}")
        lab.artifact(originals["power_02"])["expired"] = True
        lab.artifacts.remove(lab.artifact(originals["power_03"]))
        doc = lab.readiness()
        for ident, reason in (("power_01", "artifact_unreadable"),
                              ("power_02", "artifact_expired"),
                              ("power_03", "artifact_missing")):
            member = self.member(doc, ident)
            self.assertEqual((member["status"], member["reasons"]), ("missing", [reason]))
            self.assertEqual(member["provenance"]["actions_run_id"], originals[ident])
            self.assertIn(("dispatch", "replacement", later[ident]), self.refusals(doc))
        self.assertEqual(doc["tallies"]["power_qualifying"], 0)

    def test_two_failed_power_originals_made_unreadable_never_let_replacements_reach_ten(self):
        # The sealed cohort's own evidence: two power originals that did not detect, then made
        # unreadable, with two later successful dispatches beside them. Selecting a member's
        # original among readable bundles turned 8 of 10 into 10 of 10 and ready; the dispatch
        # history keeps the originals, so it stays 8 of 10 and not ready.
        lab = Deleted.base.clone(self.path("eight"))
        originals = Deleted.originals
        before = lab.readiness()
        self.assertEqual((before["tallies"]["power_qualifying"], before["ready"]), (8, False))
        replacements = dict((ident, lab.run(ident, minute=210 + index))
                            for index, ident in enumerate(("power_01", "power_02"), 1))
        for ident, run in originals.items():
            manifest = os.path.join(lab.bundle(run), transport.TRANSPORT_FILE)
            os.chmod(manifest, 0o600)
            with open(manifest, "wb") as stream:
                stream.write(b"not json")
        doc = lab.readiness()
        self.assertEqual((doc["tallies"]["power_qualifying"], doc["tallies"]["power_members"],
                          doc["ready"]), (8, 10, False))
        for ident in ("power_01", "power_02"):
            member = self.member(doc, ident)
            self.assertEqual(member["provenance"]["actions_run_id"], originals[ident])
            self.assertEqual((member["status"], member["reasons"]),
                             ("missing", ["artifact_unreadable"]))
            self.assertIn(("artifact", "replacement", replacements[ident]),
                          self.refusals(doc))
        # Read as a whole, the replacements are perfect detections: only their identity, not
        # their outcome, keeps them out.
        self.assertTrue(all(self.member(doc, "power_%02d" % index)["qualifies"]
                            for index in range(3, 11)))

    def test_an_incomplete_capped_or_doubled_dispatch_history_is_never_judged(self):
        lab = self.lab()
        lab.run("power_01", minute=1)
        lab.run("power_02", minute=2)
        listed = sorted(lab.workflow_runs.values(), key=lambda run: -run["run_number"])
        total = len(listed)
        renumbered = copy.deepcopy(listed)
        renumbered[0]["run_number"] = renumbered[1]["run_number"]
        for runs, why in (({"total_count": total, "workflow_runs": listed[:2] + listed[3:]},
                           "not listed"),
                          ({"total_count": total, "workflow_runs": listed[:-1]},
                           "ends before the cohort's anchor"),
                          ({"total_count": total - 1, "workflow_runs": listed},
                           "of a workflow with"),
                          ({"total_count": total + 1, "workflow_runs": listed + listed[:1]},
                           "listed twice"),
                          ({"total_count": total, "workflow_runs": renumbered},
                           "run number %d is listed twice" % renumbered[0]["run_number"])):
            with self.subTest(why=why):
                lab.write_listing(runs)
                scratch = self.path("scratch-%d" % len(os.listdir(self.tmp)))
                os.makedirs(scratch)
                with self.assertRaisesRegex(ContractError, why):
                    calibration.collect(lab.cohort, lab.evidence, scratch, lab.replan,
                                        lab.producer)
        os.remove(os.path.join(lab.evidence, calibration.RUNS_FILE))
        with self.assertRaises(OSError):
            calibration.collect(lab.cohort, lab.evidence, self.path("scratch"), lab.replan,
                                lab.producer)

    def test_an_ambiguous_order_of_a_members_dispatches_is_never_judged(self):
        lab = self.lab()
        first = lab.run("power_01", minute=9)
        lab.run("power_01", minute=10)
        lab.workflow_runs[first]["created_at"] = fixtures.stamp(4, 30)
        with self.assertRaisesRegex(ContractError, "dispatches are ambiguous"):
            lab.readiness()
        lab.workflow_runs[first]["created_at"] = fixtures.stamp(3, 9)
        lab.workflow_runs[first]["id"], lab.workflow_runs[first + 1]["id"] = first + 1, first
        with self.assertRaisesRegex(ContractError, "dispatches are ambiguous"):
            lab.readiness()

    def test_the_original_attempts_own_record_decides_its_chronology(self):
        lab = self.lab()
        on_day = lab.run("power_01", minute=1)
        unrecorded = lab.run("power_02", minute=2, attempt_record=False)
        running = lab.run("power_03", minute=3)
        early = lab.run("power_04", minute=4)
        # Its artifacts were uploaded on the member's day, but the attempt itself started on
        # day 0, before any parameters were sealed; the upload time proves nothing.
        path = lab.attempt_record(early)
        with open(path) as stream:
            record = json.load(stream)
        record.update(run_started_at=fixtures.stamp(0, 1), created_at=fixtures.stamp(0, 1))
        with open(path, "w") as stream:
            json.dump(record, stream)
        lab.workflow_runs[early]["created_at"] = fixtures.stamp(0, 1)
        with open(lab.attempt_record(running)) as stream:
            record = json.load(stream)
        record.update(status="in_progress", conclusion=None)
        with open(lab.attempt_record(running), "w") as stream:
            json.dump(record, stream)
        doc = lab.readiness()
        self.assertEqual(self.member(doc, "power_01")["status"], "complete")
        self.assertTrue(self.member(doc, "power_01")["qualifies"])
        self.assertEqual(self.member(doc, "power_01")["provenance"]["actions_run_id"], on_day)
        self.assertEqual((self.member(doc, "power_02")["status"],
                          self.member(doc, "power_02")["reasons"]),
                         ("invalid", ["attempt_unavailable"]))
        self.assertEqual(self.member(doc, "power_03")["status"], "interrupted")
        self.assertIn("attempt_incomplete", self.member(doc, "power_03")["reasons"])
        self.assertEqual(self.member(doc, "power_04")["status"], "invalid")
        self.assertEqual(sorted(self.member(doc, "power_04")["reasons"]),
                         ["before_calibration", "off_schedule"])
        self.assertEqual(unrecorded, self.member(doc, "power_02")["provenance"]["actions_run_id"])


class FrozenPlan(support.TempDir):
    """Each member's plan must be the plan its cohort's authority freezes for it."""

    def test_a_self_consistent_plan_that_departs_from_the_cohort_never_qualifies(self):
        lab = prepared(self.path("lab"))

        def more_pairs(plan):
            case = [item for item in plan["cases"] if item["criticality"] == "critical"][0]
            case["pairs"] += 2
            case["rank"] = stats.critical_rank(case["pairs"], 1, Fraction(case["alpha"]))
            case["tail_probability"] = format_rational(
                stats.tail_probability(case["pairs"], case["rank"]))
            case["measured_orders"] = schedule.block_orders(
                plan["seed"], case["instance_id"], "measured", case["pairs"])

        def other_policy(plan):
            plan["authority"]["policy_digest"] = "sha256:" + "2" * 64

        def other_instruments(plan):
            # Beside everything the cohort declares: only planning the member again sees it.
            plan["authority"]["instrument_digest"] = "sha256:" + "3" * 64

        edits = (("power_01", more_pairs), ("power_02", other_policy),
                 ("power_03", other_instruments))
        for index, (ident, edit) in enumerate(edits, 1):
            plan = lab.plan(ident, "gh1_a1_primary", lab.calibration)
            edit(plan)
            plan["plan_digest"] = contracts.plan_digest(plan)
            contracts.validate_run(plan)
            lab.run(ident, minute=index, plan_edit=edit)
        lab.run("power_04", minute=4)
        doc = lab.readiness()
        members = dict((item["member_id"], item) for item in doc["members"])
        for ident, _ in edits:
            self.assertEqual((members[ident]["status"], members[ident]["reasons"]),
                             ("invalid", ["wrong_plan"]), ident)
            self.assertFalse(members[ident]["qualifies"])
        self.assertEqual(calibration._cohort_mismatch(lab.cohort, self.edited(
            lab, more_pairs)), "its cases, criticality or N")
        self.assertEqual(calibration._cohort_mismatch(lab.cohort, self.edited(
            lab, other_policy)), "its policy digest")
        self.assertIsNone(calibration._cohort_mismatch(lab.cohort, self.edited(
            lab, other_instruments)))
        self.assertEqual(members["power_04"]["status"], "complete")
        self.assertTrue(members["power_04"]["qualifies"])

    def edited(self, lab, edit):
        plan = lab.plan("power_09", "gh1_a1_primary", lab.calibration)
        edit(plan)
        return plan


class DeletedOriginal(support.TempDir):
    """A deleted original is detected by its missing run number, never replaced (ADR-0183)."""

    def member(self, doc, member_id):
        return [item for item in doc["members"] if item["member_id"] == member_id][0]

    def test_deleted_originals_beside_their_replacements_refuse_the_history(self):
        # The predecessor's counterexample: two failed power originals deleted with their
        # attempts and every artifact, two successful replacements left, and a listing whose
        # total_count matches what remains. The lowest surviving dispatch was taken as the
        # original, and 8 of 10 became 10 of 10 and ready.
        lab = Deleted.base.clone(self.path("deleted"))
        for index, ident in enumerate(("power_01", "power_02"), 1):
            lab.run(ident, minute=210 + index)
        for ident, run in Deleted.originals.items():
            lab.delete_run(run)
        lab.write_listing()
        with open(os.path.join(lab.evidence, calibration.RUNS_FILE)) as stream:
            listing = json.load(stream)
        self.assertEqual(listing["total_count"], len(listing["workflow_runs"]))
        numbers = sorted(lab.workflow_runs[run]["run_number"] for run in lab.workflow_runs)
        self.assertNotEqual(numbers, list(range(numbers[0], numbers[-1] + 1)))
        with self.assertRaisesRegex(ContractError, "after the cohort's anchor %d are not listed"
                                    % fixtures.ANCHOR):
            lab.readiness()
        # Nothing judged from it: preparation refuses the same history.
        with self.assertRaisesRegex(ContractError, "may be a member's original"):
            lab.prepare()

    def test_a_deleted_newest_original_leaves_its_member_missing_until_a_later_run(self):
        lab = prepared(self.path("newest"))
        lab.calibration = Ready.sealed
        kept = lab.run("power_02", minute=1)
        latest = lab.run("power_01", minute=2)
        lab.delete_run(latest)
        doc = lab.readiness()
        # No later run of the workflow exists, so none can stand in for the deleted one: the
        # member has no dispatch and stays in the denominator.
        self.assertEqual((self.member(doc, "power_01")["status"],
                          self.member(doc, "power_01")["reasons"]), ("missing", ["no_dispatch"]))
        self.assertEqual(self.member(doc, "power_02")["provenance"]["actions_run_id"], kept)
        self.assertEqual(doc["tallies"]["power_members"], 10)
        # Any later run, a replacement or another event's, exposes the gap.
        lab.other_run()
        with self.assertRaisesRegex(ContractError, "is not listed"):
            lab.readiness()

    def test_runs_of_other_events_interleave_and_bind_nothing(self):
        lab = prepared(self.path("interleaved"))
        lab.other_run()
        first = lab.run("power_01", minute=1)
        lab.other_run(title=lab.title("power_01"))
        lab.other_run(title="Nightly", event="schedule", branch="main")
        doc = lab.readiness()
        self.assertEqual(self.member(doc, "power_01")["provenance"]["actions_run_id"], first)
        self.assertTrue(self.member(doc, "power_01")["qualifies"])
        self.assertEqual([item["reason"] for item in doc["refused_evidence"]],
                         ["foreign_dispatch"])
        self.assertEqual(doc["history"]["runs"], 22 + 4)

    def test_a_run_for_the_cohort_at_or_before_its_anchor_refuses_the_history(self):
        lab = prepared(self.path("anchor"))
        lab.run("power_01", minute=1)
        lab.workflow_runs[fixtures.ANCHOR_RUN].update(
            display_title=lab.title("power_02"), event="workflow_dispatch")
        with self.assertRaisesRegex(ContractError, "numbered at or before its anchor"):
            lab.readiness()


class Repairs(support.TempDir):
    """The predecessor's published findings: each failed there and is refused here."""

    def member(self, doc, member_id):
        return [item for item in doc["members"] if item["member_id"] == member_id][0]

    def refusals(self, doc):
        return sorted(set((item["source"], item["reason"], item["actions_run_id"])
                          for item in doc["refused_evidence"]))

    def test_failed_original_attempts_with_complete_bundles_never_qualify(self):
        lab = self.lab = prepared(self.path("failed"))
        runs = {}
        for index, conclusion in enumerate(("failure", "cancelled", "timed_out", None,
                                            "skipped", "success"), 1):
            ident = "power_%02d" % index
            runs[ident] = lab.run(ident, minute=index, conclusion=conclusion)
        doc = lab.readiness()
        for index, conclusion in enumerate(("failure", "cancelled", "timed_out", None,
                                            "skipped"), 1):
            member = self.member(doc, "power_%02d" % index)
            # The bundle is complete and a perfect detection; the attempt did not succeed.
            self.assertEqual((member["status"], member["reasons"]),
                             ("failed", ["attempt_unsuccessful"]), conclusion)
            self.assertEqual(member["provenance"]["attempt_conclusion"], conclusion)
            self.assertEqual(member["provenance"]["actions_run_id"],
                             runs["power_%02d" % index])
            self.assertFalse(member["qualifies"])
        self.assertTrue(self.member(doc, "power_06")["qualifies"])
        self.assertEqual((doc["tallies"]["power_qualifying"], doc["tallies"]["power_members"]),
                         (1, 10))

    def test_two_failed_power_originals_end_a_ready_cohort(self):
        lab = Ready.lab.clone(self.path("ready"))
        for ident, conclusion in (("power_01", "failure"), ("power_02", None)):
            member = self.member(Ready.doc, ident)
            path = lab.attempt_record(member["provenance"]["actions_run_id"])
            with open(path) as stream:
                record = json.load(stream)
            record["conclusion"] = conclusion
            os.chmod(path, 0o600)
            with open(path, "w") as stream:
                json.dump(record, stream)
        doc = lab.readiness()
        self.assertEqual((doc["tallies"]["power_qualifying"], doc["ready"]), (8, False))

    def test_failed_aa_and_preparation_originals_supply_no_parameters(self):
        lab = fixtures.Lab(self.path("prep"), authority=Ready.prepared.authority)
        minute = 0
        for member in lab.cohort["members"]:
            if member["kind"] in ("aa", "preparation"):
                minute += 1
                failed = member["member_id"] in ("aa_20", "prep_01")
                lab.run(member["member_id"], minute=minute,
                        conclusion="failure" if failed else "success")
        sealed = lab.prepare()
        group = contracts.calibration_group(sealed, "synthetic_fast", "hosted_x86")
        # Nineteen A/A originals give A0 and one preparation original the loop's cost.
        self.assertEqual((group["a0_samples"], group["loop_samples"]), (19 * 12, 12))
        doc = lab.readiness()
        self.assertEqual((doc["tallies"]["aa_within_tolerance"], doc["tallies"]["aa_members"]),
                         (19, 20))
        self.assertEqual(self.member(doc, "aa_20")["reasons"], ["attempt_unsuccessful"])
        self.assertFalse(doc["ready"])

    def test_self_consistent_parameters_other_than_the_originals_give_are_refused(self):
        # A0 of 200 ms with every efficiency, target, loop count, plan and dose regenerated:
        # the trials are perfect against it, and only recomputation from the A/A and
        # preparation originals (A0 100 ms) refuses it.
        lab = prepared(self.path("forged"))
        forged = fixtures.forged_parameters(Ready.sealed, lab.cohort, Fraction(200))
        self.assertNotEqual(canonical_digest(forged), canonical_digest(Ready.sealed))
        lab.calibration = forged
        lab.run_phase(("power",))
        with self.assertRaisesRegex(ContractError, "not those the cohort's A/A and "
                                    "preparation originals give"):
            lab.readiness()
        runs, _, _ = lab.collect()
        recomputed = calibration.prepare(lab.cohort, runs,
                                         calibration.tolerances_of(runs, lab.cohort))
        self.assertEqual(recomputed, Ready.sealed)
        self.assertEqual(contracts.calibration_group(recomputed, "synthetic_fast",
                                                     "hosted_x86")["a0"], "100")

    def test_duplicate_original_artifacts_invalidate_the_member_in_either_order(self):
        for kind in ("run", "shard"):
            for reverse in (False, True):
                with self.subTest(kind=kind, reverse=reverse):
                    lab = prepared(self.path("duplicate-%s-%s" % (kind, reverse)))
                    original = lab.run("power_01", minute=1)
                    lab.run("power_02", minute=2)
                    name = ("af-perf-gh%d_a1_primary-run" % original if kind == "run"
                            else "af-perf-gh%d_a1_primary-shard-1" % original)
                    entry = [item for item in lab.artifacts if item["name"] == name][0]
                    twin = dict(entry, id=entry["id"] + 1000)
                    if reverse:
                        lab.artifacts.insert(0, twin)
                    else:
                        lab.artifacts.append(twin)
                    doc = lab.readiness()
                    member = self.member(doc, "power_01")
                    self.assertEqual((member["status"], member["reasons"]),
                                     ("invalid", ["artifact_ambiguous"]))
                    self.assertFalse(member["qualifies"])
                    duplicates = sorted(item["detail"].split()[1] for item in
                                        doc["refused_evidence"] if item["reason"] == "duplicate")
                    self.assertEqual(duplicates, sorted([str(entry["id"]), str(twin["id"])]))
                    self.assertTrue(self.member(doc, "power_02")["qualifies"])

    def test_only_fresh_hosted_runners_of_the_plan_measure_a_member(self):
        lab = prepared(self.path("runners"))

        def job_edit(change, names=("shard",)):
            def edit(jobs):
                for job in jobs:
                    if job["name"].split(" ")[0] in names:
                        change(job)
            return edit

        def drop_shard(jobs):
            jobs.remove([job for job in jobs if job["name"].endswith(" 1)")][0])

        edits = {
            # A renamed or dedicated label, a self-hosted runner group or name, the jobs of
            # another attempt, a missing shard job: none shows a fresh hosted runner.
            "power_01": job_edit(lambda job: job.update(labels=["trusted-perf"])),
            "power_02": job_edit(lambda job: job.update(runner_group_name="Default"),
                                 ("build",)),
            "power_03": job_edit(lambda job: job.update(runner_name="perf-box-1")),
            "power_04": job_edit(lambda job: job.update(run_attempt=2)),
            "power_05": drop_shard,
            "power_06": job_edit(lambda job: job.update(labels=["ubuntu-latest", "dedicated"])),
        }
        for index, ident in enumerate(sorted(edits), 1):
            lab.run(ident, minute=index, jobs_edit=edits[ident])
        lab.run("power_07", minute=7)
        absent = lab.run("power_08", minute=8)
        os.remove(lab.jobs_record(absent))
        doc = lab.readiness()
        for ident in sorted(edits) + ["power_08"]:
            member = self.member(doc, ident)
            self.assertEqual((member["status"], member["reasons"]),
                             ("invalid", ["runner_unverified"]), ident)
            self.assertIsNone(member["provenance"]["runner"])
            self.assertFalse(member["qualifies"])
        self.assertTrue(self.member(doc, "power_07")["qualifies"])

    def test_invalid_calendar_instants_are_invalid_evidence_without_a_traceback(self):
        lab = prepared(self.path("calendar"))
        original = lab.run("power_01", minute=1)
        path = lab.attempt_record(original)
        with open(path) as stream:
            record = json.load(stream)
        record["run_started_at"] = "2025-02-30T06:00:00Z"
        with open(path, "w") as stream:
            json.dump(record, stream)
        doc = lab.readiness()
        self.assertEqual((self.member(doc, "power_01")["status"],
                          self.member(doc, "power_01")["reasons"]),
                         ("invalid", ["attempt_unavailable"]))
        for where in ("run", "artifact"):
            with self.subTest(where=where):
                bad = prepared(self.path("calendar-" + where))
                run = bad.run("power_01", minute=1)
                if where == "run":
                    bad.workflow_runs[run]["created_at"] = "2025-02-30T06:00:00Z"
                else:
                    bad.artifact(run)["created_at"] = "2025-13-01T06:00:00Z"
                with self.assertRaisesRegex(ContractError, "is not a calendar instant"):
                    bad.readiness()
                bad.write_listing()
                import cli
                cohort = os.path.join(bad.root, "cohort.json")
                sealed = os.path.join(bad.root, "sealed.json")
                with open(cohort, "wb") as stream:
                    stream.write(contracts.write_bytes(bad.cohort, "cohort"))
                with open(sealed, "wb") as stream:
                    stream.write(contracts.write_bytes(Ready.sealed, "calibration-parameters"))
                errors = io.StringIO()
                with mock.patch.object(calibration, "replanner", lambda *args: bad.replan), \
                        mock.patch.object(calibration, "producer_check",
                                          lambda *args: bad.producer), \
                        redirect_stderr(errors), redirect_stdout(io.StringIO()):
                    code = cli.main(["readiness", "--repo", bad.root, "--cohort", cohort,
                                     "--calibration", sealed, "--evidence", bad.evidence,
                                     "--output", os.path.join(bad.root, "readiness.json")])
                self.assertEqual(code, 2)
                self.assertIn("is not a calendar instant", errors.getvalue())
                self.assertNotIn("Traceback", errors.getvalue())


class Closeout(support.TempDir):
    """The activation closeout's calibration findings (ADR-0184): each failed on the predecessor
    and is refused here, and the positive history and producer still qualify."""

    def member(self, doc, member_id):
        return [item for item in doc["members"] if item["member_id"] == member_id][0]

    def test_a_huge_admitted_run_number_is_a_bounded_gap_never_an_enumerated_span(self):
        # The predecessor expanded every number between the anchor and the newest run into a
        # set: a 570-byte listing with run number 2**31 - 1 exhausted a 64-MiB address space.
        lab = prepared(self.path("huge"))
        lab.run("power_01", minute=1)
        listed = sorted(lab.workflow_runs.values(), key=lambda run: -run["run_number"])
        huge = copy.deepcopy(listed[0])
        huge.update(id=huge["id"] + 1, run_number=2 ** 31 - 1, display_title="Nightly",
                    event="schedule")
        lab.write_listing({"total_count": len(listed) + 1, "workflow_runs": [huge] + listed})
        newest = listed[0]["run_number"]
        with self.assertRaisesRegex(ContractError, r"run numbers %d\.\.%d after the cohort's "
                                    r"anchor %d are not listed" % (newest + 1, 2 ** 31 - 2,
                                                                   fixtures.ANCHOR)):
            calibration.collect(lab.cohort, lab.evidence, self.path("scratch"), lab.replan,
                                lab.producer)
        # The diagnostics name at most GAPS_SHOWN ranges, from the listed numbers alone.
        numbers = list(range(fixtures.ANCHOR + 2, fixtures.ANCHOR + 200, 2))
        gaps = calibration.number_gaps(fixtures.ANCHOR, numbers)
        self.assertEqual(len(gaps), calibration.GAPS_SHOWN + 1)
        self.assertEqual(gaps[:2], [str(fixtures.ANCHOR + 1), str(fixtures.ANCHOR + 3)])
        self.assertEqual(calibration.number_gaps(5, [6, 7, 8]), [])
        self.assertEqual(calibration.number_gaps(5, [9, 2 ** 31 - 1]),
                         ["6..8", "10..%d" % (2 ** 31 - 2)])
        # A contiguous history is still judged, and a deleted original still refuses it.
        lab.write_listing()
        self.assertTrue(self.member(lab.readiness(), "power_01")["qualifies"])

    def producer_repo(self):
        """A repository holding the authority (registry, workflow, scheduler), the cohort's
        declaration descending from it, a later head that changes only product code, and heads
        and controls that change the workflow or the scheduler."""
        repo, authority = support.lab_repo(self.path("producer"))
        workflow = contracts.DISPATCH_WORKFLOW
        authority = repo.commit("producer", {workflow: b"name: Performance\n"})
        commits = {"authority": authority}
        commits["declared"] = repo.commit("declare", {
            "fixtures/performance/cohort.json": b"{}\n"})
        commits["head"] = repo.commit("product", {"README": b"product changed\n"})
        commits["changed_workflow"] = repo.commit("workflow", {
            workflow: b"name: Performance\non: push\n"})
        repo.git("checkout", "-q", "-b", "scheduler", commits["head"])
        commits["changed_scheduler"] = repo.commit("scheduler", {
            "scripts/performance/perflab/ci.py": b"# another scheduler\n"})
        return gitrefs.Git(repo.path), commits

    def test_the_reviewed_producer_qualifies_from_another_head_and_a_changed_one_never(self):
        git, commits = self.producer_repo()
        cohort = {"authority_sha": commits["authority"]}
        check = calibration.producer_check(git, cohort)

        def plan(control):
            return {"resolved": {"control_sha": control}}
        # Neither the head nor the control need equal the authority or each other.
        self.assertIsNone(check({"head_sha": commits["head"]}, plan(commits["declared"])))
        self.assertIsNone(check({"head_sha": commits["authority"]},
                                plan(commits["authority"])))
        for head, control, why in (
                (commits["changed_workflow"], commits["declared"], "workflow at head commit"),
                (commits["head"], commits["changed_scheduler"], "scheduling code"),
                ("0" * 40, commits["declared"], "cannot be shown")):
            with self.subTest(why=why):
                self.assertIn(why, check({"head_sha": head}, plan(control)))
        # An authority without the workflow shows no producer at all.
        bare = {"authority_sha": git.resolve(commits["authority"] + "^", "parent")}
        self.assertIn("has no", calibration.producer_check(git, bare)(
            {"head_sha": commits["head"]}, plan(commits["declared"])))

    def test_a_member_run_by_a_changed_producer_is_invalid_and_never_qualifies(self):
        lab = prepared(self.path("changed"))
        changed = synthetic.label_sha("changed workflow")
        lab.run("power_01", minute=1)
        lab.run("power_02", minute=2, producer={"head_sha": changed})
        with open(lab.attempt_record(max(lab.workflow_runs))) as stream:
            self.assertEqual(json.load(stream)["head_sha"], changed)
        lab.changed_producers.add(changed)
        doc = lab.readiness()
        self.assertTrue(self.member(doc, "power_01")["qualifies"])
        member = self.member(doc, "power_02")
        self.assertEqual((member["status"], member["qualifies"]), ("invalid", False))
        self.assertIn("producer_unverified", member["reasons"])
        self.assertEqual(member["provenance"]["head_sha"], changed)
        self.assertEqual(self.member(doc, "power_01")["provenance"]["head_sha"],
                         lab.authority)


class Arithmetic(unittest.TestCase):
    """The readiness decision over recorded member outcomes."""

    @classmethod
    def setUpClass(cls):
        cls.cohort = Ready.prepared.cohort
        cls.sealed = Ready.sealed

    def outcome(self, level, classification, valid=True):
        cases = {}
        for case in self.cohort["cases"]:
            ident = case["instance_id"]
            value = classification(ident) if callable(classification) else classification
            cases[ident] = {"classification": value, "injected_median_ms": None,
                            "target_ms": None, "injection_valid": None}
            if level not in ("aa", "preparation"):
                good = valid(ident) if callable(valid) else valid
                group = contracts.calibration_group(self.sealed, ident, "hosted_x86")
                target = contracts.target_ms(group, level)
                cases[ident].update(
                    injected_median_ms=format_rational(target if good else target * 13 / 10),
                    target_ms=format_rational(target), injection_valid=good)
        return cases

    def records(self, aa=WITHIN, power=None, statuses=None):
        statuses = statuses or {}
        out = []
        for member in self.cohort["members"]:
            ident = member["member_id"]
            status = statuses.get(ident, "complete")
            digest = canonical_digest(self.cohort)
            if status != "complete":
                out.append(fixtures.member_record(member, status, digest=digest))
                continue
            if member["kind"] == "aa":
                cases = self.outcome("aa", aa(ident) if callable(aa) else aa)
            elif member["kind"] == "power":
                cases = (power(ident) if power is not None
                         else self.outcome("power", REGRESSION))
            elif member["kind"] == "injection":
                cases = self.outcome(contracts.member_level(member), REGRESSION)
            else:
                cases = self.outcome("preparation", WITHIN)
            out.append(fixtures.member_record(member, cases=cases, digest=digest))
        return out

    def decide(self, members):
        return contracts.compute_readiness(self.cohort, self.sealed, members)

    def test_whole_trials_are_counted_never_per_case_tallies(self):
        # Case A fails only trial 1, case B only trial 2: each case detects 9 of 10, but only
        # 8 whole trials qualify, so the cohort is not ready.
        fast, process = "synthetic_fast", "synthetic_process__size_1"

        def power(ident):
            failing = {"power_01": fast, "power_02": process}.get(ident)
            return self.outcome("power", lambda case: (stats.INCONCLUSIVE if case == failing
                                                       else REGRESSION))
        tallies, _, qualifies, ready = self.decide(self.records(power=power))
        self.assertEqual(tallies["power_by_case"][fast]["detections"], 9)
        self.assertEqual(tallies["power_by_case"][process]["detections"], 9)
        self.assertEqual(tallies["power_qualifying"], 8)
        self.assertFalse(ready)
        # The same two failures in one trial leave 9 whole trials: ready.

        def same(ident):
            return self.outcome("power", stats.INCONCLUSIVE if ident == "power_01"
                                else REGRESSION)
        tallies, _, _, ready = self.decide(self.records(power=same))
        self.assertEqual(tallies["power_qualifying"], 9)
        self.assertTrue(ready)

    def test_an_invalid_dose_in_one_case_disqualifies_its_whole_trial(self):
        def power(ident):
            return self.outcome("power", REGRESSION,
                                valid=lambda case: not (ident in ("power_01", "power_02")
                                                        and case == "synthetic_fast"))
        tallies, _, _, ready = self.decide(self.records(power=power))
        self.assertEqual(tallies["power_qualifying"], 8)
        self.assertEqual(tallies["power_by_case"]["synthetic_fast"]["valid_injections"], 8)
        self.assertFalse(ready)

    def test_all_twenty_aa_members_must_be_within_tolerance(self):
        tallies, _, _, ready = self.decide(self.records())
        self.assertTrue(ready)
        # Eligible to pass is not within tolerance: one non-regression A/A member fails.
        for classification in (stats.NON_REGRESSION, stats.IMPROVEMENT, stats.INCONCLUSIVE):
            with self.subTest(classification=classification):
                tallies, _, _, ready = self.decide(self.records(
                    aa=lambda ident: classification if ident == "aa_20" else WITHIN))
                self.assertEqual((tallies["aa_within_tolerance"], tallies["aa_members"]),
                                 (19, 20))
                self.assertFalse(ready)
        # A missing twentieth member never becomes 19 of 19.
        for status in ("missing", "interrupted", "unhealthy", "failed", "invalid"):
            with self.subTest(status=status):
                tallies, _, _, ready = self.decide(self.records(statuses={"aa_20": status}))
                self.assertEqual((tallies["aa_within_tolerance"], tallies["aa_members"]),
                                 (19, 20))
                self.assertFalse(ready)

    def test_failed_power_members_stay_in_the_fixed_denominator(self):
        tallies, _, _, ready = self.decide(self.records(statuses={"power_10": "missing"}))
        self.assertEqual((tallies["power_qualifying"], tallies["power_members"]), (9, 10))
        self.assertTrue(ready)
        tallies, _, _, ready = self.decide(self.records(
            statuses={"power_09": "interrupted", "power_10": "unhealthy"}))
        self.assertEqual((tallies["power_qualifying"], tallies["power_members"]), (8, 10))
        self.assertFalse(ready)

    def test_an_unprepared_group_never_qualifies(self):
        members = self.records()
        for member in members:
            if member["kind"] == "power" and member["member_id"] <= "power_02":
                member["cpu_groups"] = {ident: "elsewhere" for ident in member["cases"]}
        tallies, _, _, ready = self.decide(members)
        self.assertEqual(tallies["power_by_group"], {
            "elsewhere": {"members": 2, "qualifying": 0},
            "hosted_x86": {"members": 8, "qualifying": 8}})
        self.assertFalse(ready)

    def test_the_dose_band_includes_exactly_its_limits(self):
        target = Fraction(10)
        valid = contracts.injection_valid
        self.assertTrue(valid(Fraction(15, 2), target))
        self.assertTrue(valid(Fraction(25, 2), target))
        self.assertFalse(valid(Fraction(15, 2) - Fraction(1, 10 ** 9), target))
        self.assertFalse(valid(Fraction(25, 2) + Fraction(1, 10 ** 9), target))
        self.assertFalse(valid(None, target))
        self.assertFalse(valid(Fraction(10), None))

    def test_a_written_readiness_is_recomputed_never_trusted(self):
        members = self.records(power=lambda ident: self.outcome(
            "power", stats.INCONCLUSIVE if ident <= "power_02" else REGRESSION))
        tallies, groups, qualifies, ready = self.decide(members)
        for member in members:
            member["qualifies"] = qualifies[member["member_id"]]
        numbers = [member["provenance"]["run_number"] for member in members
                   if member["provenance"] is not None]
        doc = {"schema": contracts.READINESS_SCHEMA, "cohort_id": self.cohort["cohort_id"],
               "cohort_digest": canonical_digest(self.cohort),
               "calibration_digest": canonical_digest(self.sealed),
               "recomputed_calibration_digest": canonical_digest(self.sealed),
               "history": {"after_run_number": fixtures.ANCHOR,
                           "latest_run_number": max(numbers),
                           "runs": max(numbers) - fixtures.ANCHOR}, "members": members,
               "refused_evidence": [], "tallies": tallies, "groups": groups, "ready": ready,
               "claim": contracts.READINESS_CLAIM}
        contracts.validate_readiness(doc, cohort=self.cohort, calibration=self.sealed)
        self.assertFalse(ready)
        forged = json.loads(json.dumps(doc))
        forged["ready"] = True
        with self.assertRaisesRegex(ContractError, "ready: does not match the recomputation"):
            contracts.validate_readiness(forged, cohort=self.cohort, calibration=self.sealed)
        forged = json.loads(json.dumps(doc))
        forged["tallies"]["power_qualifying"] = 9
        with self.assertRaisesRegex(ContractError, "tallies: do not match"):
            contracts.validate_readiness(forged, cohort=self.cohort, calibration=self.sealed)
        forged = json.loads(json.dumps(doc))
        forged["members"] = forged["members"][:-1]
        with self.assertRaisesRegex(ContractError, "every declared member"):
            contracts.validate_readiness(forged, cohort=self.cohort, calibration=self.sealed)
        forged = json.loads(json.dumps(doc))
        power = [item for item in forged["members"] if item["member_id"] == "power_05"][0]
        power["cases"]["synthetic_fast"]["injected_median_ms"] = "13"
        with self.assertRaisesRegex(ContractError, "injection_valid: does not follow"):
            contracts.validate_readiness(forged, cohort=self.cohort, calibration=self.sealed)
        # Parameters other than those the originals give are refused, even when the document
        # names them consistently.
        forged = json.loads(json.dumps(doc))
        forged["recomputed_calibration_digest"] = "sha256:" + "4" * 64
        with self.assertRaisesRegex(ContractError, "not those the A/A and preparation"):
            contracts.validate_readiness(forged, cohort=self.cohort, calibration=self.sealed)
        # A complete member's original attempt succeeded on a verified hosted runner.
        for edit, why in ((lambda member: member["provenance"].update(
                              attempt_conclusion="failure"), "only a successful original"),
                          (lambda member: member["provenance"].update(runner=None),
                           "ran on fresh hosted runners")):
            forged = json.loads(json.dumps(doc))
            edit([item for item in forged["members"] if item["member_id"] == "power_06"][0])
            with self.assertRaisesRegex(ContractError, why):
                contracts.validate_readiness(forged, cohort=self.cohort,
                                             calibration=self.sealed)


class Declarations(unittest.TestCase):
    """The cohort and parameters are fixed before execution and checked exactly."""

    def setUp(self):
        self.cohort = Ready.prepared.cohort

    def test_the_schedule_spans_five_days_in_phase_order(self):
        days = sorted(set(member["day"] for member in self.cohort["members"]))
        self.assertEqual(days, [0, 1, 2, 3, 4])
        short = copy.deepcopy(self.cohort)
        for member in short["members"]:
            member["day"] = min(member["day"], 3)
        with self.assertRaisesRegex(ContractError, "spans at least 5 days"):
            contracts.validate_cohort(short)
        early = copy.deepcopy(self.cohort)
        [member for member in early["members"] if member["member_id"] == "power_01"][0][
            "day"] = 1
        with self.assertRaisesRegex(ContractError, "precede every trial"):
            contracts.validate_cohort(early)
        with self.assertRaisesRegex(ContractError, "at least five days"):
            calibration.declare(support.synthetic_suite(), fixtures.AUTHORITY, "short", "quick",
                                "linux_x86_64_gnu_hosted", "linux_gnu_native_release",
                                [{"group_id": "g", "cpu_model": "c", "image": "i"}],
                                fixtures.START, fixtures.REPOSITORY, fixtures.ANCHOR, 4)
        # A cohort calibrates fresh hosted runners: a trusted local or dedicated environment
        # is refused at declaration.
        with self.assertRaisesRegex(ContractError, "not a GitHub-hosted environment"):
            calibration.declare(support.synthetic_suite(), fixtures.AUTHORITY, "local", "quick",
                                "macos_arm64_local", "macos_arm64_native_release",
                                [{"group_id": "g", "cpu_model": "c", "image": "i"}],
                                fixtures.START, fixtures.REPOSITORY, fixtures.ANCHOR)

    def test_a_declaration_is_deterministic(self):
        again = fixtures.Lab(os.path.join(Ready.tmp, "again"),
                             authority=Ready.prepared.authority).cohort
        self.assertEqual(canonical_digest(again), canonical_digest(self.cohort))
        self.assertEqual(len(set(member["seed"] for member in self.cohort["members"])),
                         len(self.cohort["members"]))

    def test_sealed_loop_counts_and_a0_are_recomputed(self):
        sealed = Ready.sealed
        forged = copy.deepcopy(sealed)
        forged["groups"][0]["iterations"]["power"] += 1
        with self.assertRaisesRegex(ContractError, "loop count nearest its target"):
            contracts.validate_calibration(forged)
        zero = copy.deepcopy(sealed)
        zero["groups"][0]["a0"] = "0"
        with self.assertRaisesRegex(ContractError, "zero A0 prevents readiness"):
            contracts.validate_calibration(zero)
        unavailable = copy.deepcopy(sealed)
        for group in unavailable["groups"]:
            if group["instance_id"] == "synthetic_fast":
                group.update(a0=None, a0_unavailable="no_aa_samples", efficiency=None,
                             power_target=None, iterations=None)
        contracts.validate_calibration(unavailable, cohort=self.cohort)
        self.assertEqual(contracts.group_readiness(self.cohort, unavailable),
                         {"hosted_x86": {"ready": False, "unavailable": ["synthetic_fast"]}})
        with self.assertRaisesRegex(ContractError, "no allowed CPU group is prepared"):
            calibration.plan_injection(self.cohort, unavailable, [
                member for member in self.cohort["members"]
                if member["member_id"] == "power_01"][0], ["synthetic_fast"])

    def test_the_injection_report_is_one_exact_line(self):
        line = b"af.perf-calibration-injection/1 iterations=200000 injected_ns=10000000"
        self.assertEqual(calibration.injected_report(b"noise\n" + line + b"\n", 200000),
                         ({"value": "10", "unit": "ms"}, None))
        for stderr, why in ((b"", "not exactly one"), (line + b"\n" + line, "not exactly one"),
                            (line.replace(b"200000", b"7"), "not the declared"),
                            (line + b" extra", "malformed")):
            metric, reason = calibration.injected_report(stderr, 200000)
            self.assertIsNone(metric)
            self.assertIn(why, reason)

    def test_only_calibration_plans_carry_injection_controls(self):
        from perflab import planning, synthetic
        suite = support.synthetic_suite()
        with self.assertRaisesRegex(ContractError, "no other plan carries an injection"):
            request = planning.Request(trigger="manual", tier="quick", scope="bootstrap",
                                       base="b", head="c", calibration=Ready.sealed,
                                       trusted_host_build=True)
            planning.freeze(suite, request, synthetic.identities(), "control",
                            synthetic.label_sha("baseline"), None,
                            {"suite_sha256": suite.raw_sha256,
                             "policy_digest": suite.policy_digest,
                             "instrument_digest": synthetic.label_digest("i"),
                             "fixture_digest": synthetic.label_digest("f")}, [], {
                                 role: {"cargo_lock_sha256": synthetic.label_digest(role)}
                                 for role in ("authority", "baseline", "candidate")})
        lab = fixtures.Lab(tempfile.mkdtemp(prefix="af-perf-calibration-"))
        plan = lab.plan("aa_01", "gh9_a1_primary")
        forged = copy.deepcopy(plan)
        forged["cohort"]["injection"] = lab.plan("prep_01", "gh9_a1_primary")["cohort"][
            "injection"]
        forged["plan_digest"] = contracts.plan_digest(forged)
        with self.assertRaisesRegex(ContractError, "an A/A member is uninstrumented"):
            contracts.validate_run(forged)
        prep = lab.plan("prep_01", "gh9_a1_primary")
        forged = copy.deepcopy(prep)
        forged["cases"][0]["budgets"] = [{"budget_id": "b", "metric": "wall_ms",
                                          "limit": "50", "unit": "ms", "minimum_samples": 12,
                                          "state": "enabled", "applicable": True}]
        forged["plan_digest"] = contracts.plan_digest(forged)
        with self.assertRaisesRegex(ContractError, "an injected run enforces no budget"):
            contracts.validate_run(forged)


if __name__ == "__main__":
    unittest.main()
