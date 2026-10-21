"""Frozen planning: baseline rules, authority and scopes, expansion, schedules and limits."""
import copy
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

from perflab import contracts, gitrefs, planning, schedule, stats, suite
from perflab.gitrefs import Unavailable
from perflab.values import ContractError, canonical_digest, sha256_bytes
from tests.support import CLI, SUITE, Repo, TempDir, lab_repo, read, synthetic_suite

READ_ONLY_GIT = {"rev-parse", "rev-list", "merge-base", "for-each-ref", "cat-file", "ls-tree",
                 "diff"}


def build_history(path):
    """R(v0.1.0) - C1(v0.2.0-rc.1) - C2(v0.2.0) - C3 - M, where M merges F (from C3)."""
    repo, root = lab_repo(path)
    shas = {"root": root}
    repo.tag("v0.1.0", root, annotated=True)
    shas["c1"] = repo.commit("c1", {"README": "c1\n"})
    repo.tag("v0.2.0-rc.1", shas["c1"])
    shas["c2"] = repo.commit("c2", {"README": "c2\n"})
    repo.tag("v0.2.0", shas["c2"], annotated=True)
    shas["c3"] = repo.commit("c3", {"README": "c3\n"})
    repo.git("checkout", "-q", "-b", "feature")
    shas["feature"] = repo.commit("feature", {"scripts/performance/cli.py": "# changed\n",
                                              "src.txt": "feature\n"})
    repo.git("checkout", "-q", "main")
    repo.git("merge", "-q", "--no-ff", "-m", "merge", "feature")
    shas["merge"] = repo.git("rev-parse", "HEAD")
    return shas


class History(TempDir):
    @classmethod
    def setUpClass(cls):
        cls.template = tempfile.mkdtemp(prefix="af-perf-history-")
        cls.shas = build_history(os.path.join(cls.template, "repo"))

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.template, True)

    def setUp(self):
        TempDir.setUp(self)
        shutil.copytree(os.path.join(self.template, "repo"), self.path("repo"), symlinks=True)
        self.repo = Repo.existing(self.path("repo"))
        for name, sha in self.shas.items():
            setattr(self, name, sha)
        self.git = gitrefs.Git(self.repo.path)
        self.calls = []
        original = self.git._run

        def recording(args, check=True, stdin=None):
            self.calls.append(args[0])
            return original(args, check, stdin)

        self.git._run = recording

    def plan(self, **values):
        values.setdefault("tier", "quick")
        values.setdefault("run_id", "test_run")
        values.setdefault("seed", 11)
        if values.get("scope") != "diagnostic" and values.get("environment") \
                != "macos_arm64_local" and "build_image" not in values:
            # Nothing here is built: an unpinned Linux recipe is frozen as a trusted host build.
            values.setdefault("trusted_host_build", True)
        return planning.plan(self.git, planning.Request(**values))

    def pr(self, **values):
        values.setdefault("scope", "bootstrap")
        return self.plan(trigger="pr", head=self.merge, pr_base_sha=self.c3,
                         pr_head_sha=self.feature, pr_number=7, **values)

    def activate(self, authority=None, digest=None, **changes):
        """Commit a reviewed activation of `authority` with its cohort, parameters and ready
        readiness records (activation_fixtures); `digest` and `changes` alter the manifest."""
        from tests import activation_fixtures
        authority = authority or self.root
        files, documents = activation_fixtures.records(suite.load(SUITE), authority)
        if digest is not None:
            changes["policy_digest"] = digest
        files[planning.ACTIVATION_PATH] = activation_fixtures.manifest(files, documents,
                                                                       **changes)
        return self.repo.commit("activate", files)


class Baselines(History):
    def test_pr_compares_the_merge_with_its_first_parent_which_is_the_control(self):
        plan = self.pr()
        self.assertEqual(plan["resolved"], {"baseline_sha": self.c3, "candidate_sha": self.merge,
                                            "control_sha": self.c3, "authority_sha": self.c3})
        self.assertEqual(plan["trigger"], {"kind": "pr", "number": 7, "head_sha": self.feature,
                                           "base_sha": self.c3, "merge_sha": self.merge,
                                           "merge_base_sha": self.c3})
        self.assertEqual(plan["harness"], {"changed": True, "truncated": False,
                                           "paths": ["scripts/performance/cli.py"]})
        self.assertEqual(plan["authority"]["mode"], "control")
        self.assertFalse(plan["required_check_eligible"])
        self.assertTrue(set(self.calls) <= READ_ONLY_GIT, set(self.calls))

    def test_pr_event_disagreement_refuses_the_plan(self):
        with self.assertRaises(ContractError) as caught:
            self.plan(trigger="pr", scope="bootstrap", head=self.merge, pr_base_sha=self.c2,
                      pr_head_sha=self.feature)
        self.assertIn("disagrees with the event's base SHA", str(caught.exception))
        with self.assertRaises(ContractError):
            self.plan(trigger="pr", scope="bootstrap", head=self.merge, pr_base_sha=self.c3,
                      pr_head_sha=self.c2)
        with self.assertRaises(ContractError):
            self.pr(control=self.c2)
        with self.assertRaises(ContractError):
            self.pr(base=self.c2)
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="pr", scope="bootstrap", head=self.c3, pr_base_sha=self.c2,
                      pr_head_sha=self.c3)
        self.assertEqual(caught.exception.reason, "not_a_merge_commit")

    def test_main_push_uses_the_first_parent_and_a_root_has_none(self):
        plan = self.plan(trigger="main_push", scope="bootstrap", head=self.c3)
        self.assertEqual(plan["resolved"]["baseline_sha"], self.c2)
        self.assertEqual(plan["resolved"]["control_sha"], self.c2)
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="main_push", scope="bootstrap", head=self.root)
        self.assertEqual(caught.exception.reason, "missing_parent")

    def test_nightly_uses_the_highest_reachable_stable_tag(self):
        plan = self.plan(trigger="nightly", scope="bootstrap", tier="nightly", head="main")
        self.assertEqual(plan["resolved"]["baseline_sha"], self.c2)
        self.assertEqual(plan["trigger"]["baseline_tag"], "v0.2.0")
        self.repo.tag("0.2.0", self.c1)
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="nightly", scope="bootstrap", tier="nightly", head="main")
        self.assertEqual(caught.exception.reason, "ambiguous_release_tag")

    def test_nightly_without_a_stable_release_is_unavailable(self):
        plan = self.plan(trigger="nightly", scope="bootstrap", tier="nightly", head=self.c1)
        self.assertEqual(plan["trigger"]["baseline_tag"], "v0.1.0")
        # Removing v0.1.0 leaves only the prerelease reachable from c1.
        self.repo.git("tag", "-d", "v0.1.0")
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="nightly", scope="bootstrap", tier="nightly", head=self.c1)
        self.assertEqual(caught.exception.reason, "no_release_baseline")

    def test_release_baselines_follow_semver_and_prerelease_rules(self):
        plan = self.plan(trigger="release", scope="bootstrap", tier="release", head="v0.2.0")
        self.assertEqual((plan["resolved"]["baseline_sha"], plan["trigger"]["baseline_tag"]),
                         (self.root, "v0.1.0"))
        self.assertEqual(plan["resolved"]["candidate_sha"], self.c2)
        rc2 = self.repo.commit("rc2", {"README": "rc2\n"})
        self.repo.tag("v0.3.0-rc.2", rc2)
        self.repo.tag("v0.3.0-rc.10", self.repo.commit("rc10", {"README": "rc10\n"}))
        plan = self.plan(trigger="release", scope="bootstrap", tier="release", head="v0.3.0-rc.10")
        self.assertEqual(plan["trigger"]["baseline_tag"], "v0.3.0-rc.2")
        self.assertTrue(plan["trigger"]["prerelease"])
        self.repo.tag("v0.3.0", self.repo.commit("final", {"README": "final\n"}))
        plan = self.plan(trigger="release", scope="bootstrap", tier="release", head="v0.3.0")
        self.assertEqual(plan["trigger"]["baseline_tag"], "v0.2.0")
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="release", scope="bootstrap", tier="release", head="v0.1.0")
        self.assertEqual(caught.exception.reason, "no_release_baseline")
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="release", scope="bootstrap", tier="release", head="v9.9.9")
        self.assertEqual(caught.exception.reason, "unresolved_ref")

    def test_manual_selectors_resolve_once_and_refuse_unsafe_spellings(self):
        plan = self.plan(scope="bootstrap", base="v0.1.0", head="main~1")
        self.assertEqual(plan["resolved"]["baseline_sha"], self.root)
        self.assertEqual(plan["selectors"]["base"], "v0.1.0")
        self.assertEqual(plan["resolved"]["candidate_sha"], self.c3)
        with self.assertRaises(Unavailable) as caught:
            self.plan(scope="bootstrap", base="nope", head="main")
        self.assertEqual(caught.exception.reason, "unresolved_ref")
        for bad in ("--all", "main..feature", "a b", ""):
            with self.assertRaises(ContractError):
                self.plan(scope="bootstrap", base=bad, head="main")

    def test_semver_precedence(self):
        ordered = ["v1.0.0-alpha", "v1.0.0-alpha.1", "v1.0.0-alpha.beta", "v1.0.0-beta",
                   "v1.0.0-beta.2", "v1.0.0-beta.11", "v1.0.0-rc.1", "v1.0.0", "v1.0.1"]
        keys = [gitrefs.version_key(gitrefs.parse_version(tag)) for tag in ordered]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(len(set(keys)), len(keys))
        for bad in ("v1.0", "v01.0.0", "1.0.0+build", "release-1"):
            self.assertIsNone(gitrefs.parse_version(bad), bad)


class Authority(History):
    def test_full_scope_is_unavailable_before_activation(self):
        with self.assertRaises(Unavailable) as caught:
            self.plan(scope="full", base=self.c2, head=self.c3)
        self.assertEqual(caught.exception.reason, "no_active_authority")

    def test_full_scope_measures_with_the_activated_authority(self):
        control = self.activate()
        later = self.repo.commit("candidate", {"README": "later\n"})
        plan = self.plan(trigger="main_push", scope="full", head=later)
        self.assertEqual(plan["resolved"]["control_sha"], control)
        self.assertEqual(plan["resolved"]["authority_sha"], self.root)
        self.assertEqual(plan["authority"]["mode"], "active")
        self.assertEqual(plan["authority"]["activation"]["admission"], "calibrated")
        self.assertTrue(plan["required_check_eligible"])
        loaded = suite.load(SUITE)
        self.assertEqual(plan["authority"]["policy_digest"], loaded.policy_digest)
        self.assertEqual(plan["authority"]["suite_sha256"], loaded.raw_sha256)
        self.assertEqual([case["instance_id"] for case in plan["cases"]],
                         [item["instance_id"] for item in loaded.instances("quick")])
        self.assertEqual(plan["verdict_policy"]["family_size"], 28)
        self.assertEqual(plan["verdict_policy"]["case_alpha"], "1/560")
        self.assertEqual(len(plan["shards"]), 2)
        with self.assertRaises(ContractError):
            self.plan(trigger="main_push", scope="full", head=later, cases=["cli_discovery"])
        with self.assertRaises(ContractError):
            self.plan(trigger="main_push", scope="full", head=later, pairs=12)

    def test_activation_must_match_the_authority_registry_and_be_reachable(self):
        self.activate(digest="sha256:" + "0" * 64)
        later = self.repo.commit("candidate", {"README": "later\n"})
        with self.assertRaises(ContractError) as caught:
            self.plan(trigger="main_push", scope="full", head=later)
        self.assertIn("differs from the activation", str(caught.exception))
        self.repo.git("checkout", "-q", "-b", "side", self.root)
        side = self.repo.commit("side", {"README": "side\n"})
        self.repo.git("checkout", "-q", "main")
        self.activate(authority=side)
        later = self.repo.commit("candidate 2", {"README": "later 2\n"})
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="main_push", scope="full", head=later)
        self.assertEqual(caught.exception.reason, "authority_unreachable")

    def test_an_unverified_activation_never_measures_as_active(self):
        # The reviewed records are rehashed and readiness recomputed from them whenever an
        # active plan is frozen (ADR-0183): another readiness hash, or a manifest naming more
        # than its cohort calibrated, is refused, and a partial plan falls back to the control.
        for changes, why in (({"readiness_sha256": "sha256:" + "ab" * 32}, "has SHA-256"),
                             ({"calibrated_groups": ["epyc", "xeon"]}, "calibrated_groups"),
                             ({"cohort_actions_run_ids": [1, 2]}, "cohort_actions_run_ids")):
            with self.subTest(why=why):
                self.activate(**changes)
                later = self.repo.commit("candidate %s" % why, {"README": why + "\n"})
                with self.assertRaisesRegex(ContractError, why):
                    self.plan(trigger="main_push", scope="full", head=later)
                partial = self.plan(trigger="manual", scope="partial_scope", base=later,
                                    head=later, control=later, cases=["cli_discovery"])
                self.assertEqual(partial["authority"]["mode"], "control")

    def test_an_activation_measures_only_the_environment_and_recipe_it_calibrated(self):
        self.activate()
        later = self.repo.commit("candidate", {"README": "later\n"})
        self.assertEqual(self.plan(trigger="main_push", scope="full", head=later)[
            "authority"]["mode"], "active")
        # The nightly tier's shipping profile was not calibrated by the quick cohort.
        with self.assertRaises(Unavailable) as caught:
            self.plan(trigger="main_push", scope="full", head=later, tier="nightly",
                      build_image="registry.example/musl@sha256:" + "1" * 64)
        self.assertEqual(caught.exception.reason, "uncalibrated_scope")
        self.assertIn("linux_x86_64_musl_hosted", str(caught.exception))

    def test_only_a_calibrated_cpu_image_group_satisfies_the_required_check(self):
        # The predecessor carried only the environment and recipe into the plan: a full plan
        # sampled on the explicitly uncalibrated Intel Xeon group satisfied the check (ADR-0184).
        from perflab import compare, synthetic
        from tests import activation_fixtures
        files, documents = activation_fixtures.records(
            suite.load(SUITE), self.root, groups=(("epyc", "AMD EPYC", "ubuntu-24.04"),
                                                   ("xeon", "Intel Xeon", "ubuntu-24.04")),
            prepared=["epyc"])
        files[planning.ACTIVATION_PATH] = activation_fixtures.manifest(files, documents)
        self.repo.commit("activate", files)
        later = self.repo.commit("candidate", {"README": "later\n"})
        plan = self.plan(trigger="main_push", scope="full", head=later)
        self.assertEqual(plan["authority"]["activation"]["calibrated_groups"],
                         [{"group_id": "epyc", "cpu_model": "AMD EPYC",
                           "image": "ubuntu-24.04"}])
        self.assertTrue(plan["required_check_eligible"])

        # The verdict binds each measured shard's observed host to those groups: a measured
        # synthetic-registry run of a full plan bound to this activation's groups.
        bound = synthetic.plan(synthetic_suite(), scope="full", run_id="bound")
        bound["authority"]["activation"] = copy.deepcopy(plan["authority"]["activation"])
        bound["plan_digest"] = contracts.plan_digest(bound)
        contracts.validate_run(bound)

        def run(name, cpu, image="ubuntu-24.04", shard_cpu=None):
            run_dir = synthetic.write_run(bound, self.path(name), synthetic.constant(10),
                                          provenance="measured")
            for shard in bound["shards"]:
                model = (shard_cpu or {}).get(shard["index"], cpu)
                observed = dict(synthetic.SYNTHETIC_HOST, cpu_model={"value": model}
                                if model else {"unknown": "not observed"})
                root = os.path.join(run_dir, "shards", str(shard["index"]))
                with open(os.path.join(root, "samples.jsonl"), "rb") as stream:
                    lines = [json.loads(line) for line in stream.read().splitlines()]
                with open(os.path.join(root, "samples.jsonl"), "wb") as stream:
                    for record in lines:
                        record["host_id"] = contracts.host_id(observed)
                        stream.write(contracts.document_bytes(record))

                def edit(doc, observed=observed):
                    doc["host"] = observed
                    doc["host_id"] = contracts.host_id(observed)
                    doc["execution"]["sample_image"] = image
                synthetic.reseal_shard(root, edit)
            return compare.compare(run_dir)["aggregate"]
        calibrated = run("epyc", "AMD EPYC")
        self.assertEqual((calibrated["result"], calibrated["satisfies_required_check"]),
                         ("pass", True))
        for name, options in (("xeon", {"cpu": "Intel Xeon"}),
                              ("unknown", {"cpu": None}),
                              ("image", {"cpu": "AMD EPYC", "image": "ubuntu-22.04"}),
                              ("one_shard", {"cpu": "AMD EPYC",
                                             "shard_cpu": {0: "Intel Xeon"}})):
            with self.subTest(host=name):
                aggregate = run(name, **options)
                self.assertEqual((aggregate["result"], aggregate["satisfies_required_check"]),
                                 ("incomplete", False))
                self.assertIn("uncalibrated_cpu_group",
                              [reason["code"] for reason in aggregate["reasons"]])
        # The cohort's aggregate denominators are unchanged: the activation verifies as before.
        from perflab import activation
        doc = activation.verify(self.git, later).doc
        self.assertEqual(doc["uncalibrated_groups"], {"xeon": sorted(
            case["instance_id"] for case in documents[0]["cases"]
            if case["criticality"] == "critical")})
        self.assertEqual(doc["tallies"]["power_members"], 10)

    def test_a_candidate_cannot_activate_its_own_policy(self):
        # The candidate adds an activation; the control revision (its first parent) has none.
        candidate = self.activate()
        with self.assertRaises(Unavailable):
            self.plan(trigger="main_push", scope="full", head=candidate)

    def cohort(self, **changes):
        loaded = suite.load(SUITE)
        cases = [{"instance_id": item["instance_id"], "criticality": item["criticality"],
                  "pairs": loaded.doc["cases"][item["case_id"]]["pairs"]["quick"]}
                 for item in loaded.instances("quick")]
        members = []
        seed = 1000
        for kind, count, percent, days in (("aa", 20, None, (0, 1)),
                                           ("injection", 10, 2, (3, 4)),
                                           ("injection", 10, 5, (3, 4)),
                                           ("injection", 10, 10, (3, 4)),
                                           ("power", 10, None, (3, 4)),
                                           ("preparation", 2, None, (2,))):
            for index in range(count):
                seed += 1
                members.append({"member_id": "%s%s_%02d" % (kind, percent or "", index),
                                "kind": kind, "source_sha": self.root, "seed": seed,
                                "injection_percent": percent,
                                "day": days[index % len(days)]})
        health = dict((key, value) for key, value in loaded.doc["health"].items()
                      if key != "description")
        health["unit"] = "ms"
        doc = {"schema": "af.perf-cohort/4", "cohort_id": "first_cohort",
               "authority_sha": self.root, "policy_digest": loaded.policy_digest,
               "tier": "quick", "environment_id": "linux_x86_64_gnu_hosted",
               "recipe_id": "linux_gnu_native_release", "cases": cases,
               "family_size": sum(1 for case in cases if case["criticality"] == "critical"),
               "health": health,
               "allowed_cpu_groups": [{"group_id": "epyc", "cpu_model": "AMD EPYC",
                                       "image": "ubuntu-24.04"}],
               "schedule": {"start_date": "2026-11-02", "minimum_days": 5,
                            "runners": "fresh_hosted", "order": contracts.COHORT_ORDER},
               "preparation": {"iterations": 1000000},
               "dispatch": {"repository": "afactory/af",
                            "workflow_path": contracts.DISPATCH_WORKFLOW, "branch": "main",
                            "event": "workflow_dispatch",
                            "run_name": contracts.DISPATCH_RUN_NAME, "after_run_number": 41},
               "target_formulas": dict(contracts.TARGET_FORMULAS),
               "members": members}
        doc.update(changes)
        return contracts.validate_cohort(doc)

    def test_calibration_plans_a_declared_member_without_activation(self):
        cohort = self.cohort()
        plan = self.plan(scope="calibration", authority=self.root, control=self.c3,
                         base=self.root, head=self.root, cohort=cohort, member="aa_03",
                         seed=None)
        self.assertEqual(plan["authority"]["mode"], "proposed")
        self.assertIsNone(plan["authority"]["activation"])
        self.assertEqual(plan["cohort"], {"cohort_id": "first_cohort",
                                          "cohort_digest": canonical_digest(cohort),
                                          "member_id": "aa_03", "member_kind": "aa",
                                          "injection": None})
        self.assertEqual(plan["seed"], [m for m in cohort["members"]
                                        if m["member_id"] == "aa_03"][0]["seed"])
        # A preparation member injects the cohort's declared count on the candidate; a power
        # member needs the sealed calibration parameters first (ADR-0181).
        prep = self.plan(scope="calibration", authority=self.root, control=self.c3,
                         base=self.root, head=self.root, cohort=cohort, member="preparation_00",
                         seed=None)
        injection = prep["cohort"]["injection"]
        self.assertEqual((injection["level"], injection["calibration_digest"]),
                         ("preparation", None))
        self.assertEqual(set(injection["groups"][0]["iterations"].values()), {1000000})
        with self.assertRaisesRegex(ContractError, "runs the sealed calibration parameters"):
            self.plan(scope="calibration", authority=self.root, control=self.c3,
                      base=self.root, head=self.root, cohort=cohort, member="power_03",
                      seed=None)
        self.assertFalse(plan["required_check_eligible"])
        with self.assertRaises(ContractError):
            self.plan(scope="calibration", authority=self.root, control=self.c3, base=self.root,
                      head=self.c1, cohort=cohort, member="aa_03", seed=None)
        with self.assertRaises(ContractError):
            self.plan(scope="calibration", authority=self.root, control=self.c3, base=self.root,
                      head=self.root, cohort=cohort, member="missing", seed=None)
        with self.assertRaises(ContractError):
            self.plan(scope="calibration", authority=self.root, control=self.c3, base=self.root,
                      head=self.root, cohort=cohort, member="aa_03", seed=5)
        with self.assertRaises(ContractError):
            self.plan(scope="calibration", authority=self.root, control=self.c3, base=self.root,
                      head=self.root, cohort=cohort, member="aa_03", seed=None, pairs=12)
        with self.assertRaises(ContractError) as caught:
            self.plan(scope="calibration", authority=self.root, base=self.root, head=self.root,
                      cohort=cohort, member="aa_03", seed=None)
        self.assertIn("trusted --control", str(caught.exception))

    def test_readiness_freezes_each_members_plan_again_from_the_authority(self):
        # The replan of calibration.collect, over this repository: the dispatched plan is
        # frozen again exactly, and any self-consistent departure from what the authority
        # freezes for the member is seen (ADR-0182).
        from perflab import calibration
        cohort = self.cohort()
        member = [m for m in cohort["members"] if m["member_id"] == "aa_03"][0]
        plan = self.plan(scope="calibration", authority=self.root, control=self.c3,
                         base=self.root, head=self.root, cohort=cohort, member="aa_03",
                         seed=None, run_id="gh13_a1_primary")
        replan = calibration.replanner(self.git, cohort)
        self.assertTrue(calibration._replanned(replan, member, plan))
        for edit in (lambda doc: doc["authority"].update(fixture_digest="sha256:" + "4" * 64),
                     lambda doc: doc["build_inputs"]["candidate"].update(
                         cargo_lock_sha256="sha256:" + "5" * 64),
                     lambda doc: doc["limits"].update(concurrent_jobs=1),
                     lambda doc: doc.update(seed=doc["seed"] + 1)):
            changed = json.loads(json.dumps(plan))
            edit(changed)
            changed["plan_digest"] = contracts.plan_digest(changed)
            self.assertFalse(calibration._replanned(replan, member, changed))
        # Planning again refuses as the dispatch would have, and a refusal is no match.
        unknown = dict(member, member_id="aa_99")
        self.assertFalse(calibration._replanned(replan, unknown, plan))

    def test_calibration_authority_must_be_reachable_from_trusted_control(self):
        cohort = self.cohort(authority_sha=self.feature,
                             members=[dict(m, source_sha=self.feature)
                                      for m in self.cohort()["members"]])
        with self.assertRaises(Unavailable) as caught:
            self.plan(scope="calibration", authority=self.feature, control=self.c3,
                      base=self.feature, head=self.feature, cohort=cohort, member="aa_00",
                      seed=None)
        self.assertEqual(caught.exception.reason, "authority_unreachable")

    def test_cohort_must_declare_the_registry_family(self):
        cohort = self.cohort()
        cohort["cases"] = cohort["cases"][1:]
        cohort["family_size"] = sum(1 for c in cohort["cases"] if c["criticality"] == "critical")
        with self.assertRaises(ContractError) as caught:
            self.plan(scope="calibration", authority=self.root, control=self.c3,
                      base=self.root, head=self.root, cohort=cohort, member="aa_00", seed=None)
        self.assertIn("full critical family", str(caught.exception))

    def test_bootstrap_may_measure_with_a_proposed_policy(self):
        plan = self.pr(authority=self.merge)
        self.assertEqual(plan["authority"]["mode"], "proposed")
        self.assertEqual(plan["resolved"]["authority_sha"], self.merge)
        same = self.plan(scope="bootstrap", base=self.merge, head=self.merge,
                         authority=self.merge)
        self.assertEqual(same["resolved"]["baseline_sha"], same["resolved"]["candidate_sha"])


class Scopes(History):
    def test_partial_scope_selects_cases_or_instances(self):
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["cli_discovery", "state_projection__log_events_10000_projection_"
                                "prewarmed"])
        ids = [case["instance_id"] for case in plan["cases"]]
        self.assertEqual(len(ids), 5)
        self.assertEqual(ids, sorted(ids))
        self.assertEqual(plan["verdict_policy"]["family_size"], 5)
        self.assertEqual(plan["scope"], "partial_scope")
        self.assertFalse(plan["required_check_eligible"])
        with self.assertRaises(ContractError):
            self.plan(scope="partial_scope", base=self.c2, head=self.c3, cases=["nope"])
        with self.assertRaises(ContractError):
            self.plan(scope="partial_scope", base=self.c2, head=self.c3)
        with self.assertRaises(ContractError):
            self.plan(scope="bootstrap", base=self.c2, head=self.c3, cases=["cli_discovery"])

    def test_pair_overrides_are_bounded_even_and_explicit(self):
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["cli_discovery"], pairs=12)
        self.assertEqual(set(case["pairs"] for case in plan["cases"]), {12})
        for bad in (13, 10, 202):
            with self.assertRaises(ContractError):
                self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                          cases=["cli_discovery"], pairs=bad)

    def test_environment_profiles_and_containment(self):
        plan = self.plan(scope="partial_scope", tier="nightly", base=self.c2, head=self.c3,
                         environment="macos_arm64_local",
                         cases=["cli_discovery", "canonical_serialize"])
        self.assertEqual(plan["profile"]["recipe_id"], "macos_arm64_native_release")
        self.assertFalse(plan["profile"]["default"])
        applicable = [b for case in plan["cases"] for b in case["budgets"] if b["applicable"]]
        self.assertEqual(len(applicable), 4)
        self.assertEqual(set(b["state"] for b in applicable), {"pending_calibration"})
        with self.assertRaises(ContractError) as caught:
            self.plan(scope="partial_scope", tier="nightly", base=self.c2, head=self.c3,
                      environment="macos_arm64_local", cases=["cli_task_start"])
        self.assertIn("require Linux containment", str(caught.exception))
        with self.assertRaises(ContractError):
            self.plan(scope="bootstrap", tier="nightly", base=self.c2, head=self.c3,
                      environment="macos_arm64_local")
        with self.assertRaises(ContractError):
            self.plan(trigger="main_push", scope="partial_scope", tier="nightly", head=self.c3,
                      environment="macos_arm64_local", cases=["cli_discovery"])

    def test_shards_never_exceed_instances(self):
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["cli_discovery"], shards=4)
        self.assertEqual([len(shard["instances"]) for shard in plan["shards"]], [1, 1, 1, 1])
        with self.assertRaises(ContractError):
            self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                      cases=["cli_discovery"], shards=5)
        single = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                           cases=["cli_discovery__command_help"])
        self.assertEqual(len(single["shards"]), 1)
        self.assertEqual(single["limits"]["concurrent_jobs"], 1)

    def test_diagnostic_children_link_their_parent(self):
        parent = self.pr()
        child = self.plan(scope="diagnostic", parent_plan=parent, run_id="diag_run",
                          cases=["cli_discovery__command_help"], tier=None, seed=3)
        self.assertEqual(child["diagnostic"]["parent_run_id"], parent["run_id"])
        self.assertEqual(child["diagnostic"]["parent_plan_digest"], parent["plan_digest"])
        self.assertEqual(child["resolved"], parent["resolved"])
        self.assertEqual(child["diagnostic"]["frequency_hz"], 997)
        with self.assertRaises(ContractError):
            self.plan(scope="diagnostic", parent_plan=parent, cases=["nope"], tier=None)
        many = [case["instance_id"] for case in parent["cases"]][:9]
        with self.assertRaises(ContractError):
            self.plan(scope="diagnostic", parent_plan=parent, cases=many, tier=None)
        with self.assertRaises(ContractError):
            self.plan(scope="diagnostic", parent_plan=parent, cases=many[:1], tier=None,
                      base=self.c2)
        self.assertEqual(child["build_inputs"], parent["build_inputs"])


class BuildInputs(History):
    def lock(self, sha):
        return sha256_bytes(self.repo.git("show", "%s:Cargo.lock" % sha).encode() + b"\n")

    def test_each_source_binds_the_cargo_lock_of_its_own_git_objects(self):
        self.repo.git("checkout", "-q", "-b", "locked", self.c3)
        locked = self.repo.commit("lock", {"Cargo.lock": "# changed lock\n"})
        self.repo.git("checkout", "-q", "main")
        plan = self.plan(scope="bootstrap", base=self.c2, head=locked)
        self.assertEqual(plan["build_inputs"], {
            "authority": {"cargo_lock_sha256": self.lock(self.c2)},
            "baseline": {"cargo_lock_sha256": self.lock(self.c2)},
            "candidate": {"cargo_lock_sha256": sha256_bytes(b"# changed lock\n")}})
        self.assertNotEqual(plan["build_inputs"]["baseline"], plan["build_inputs"]["candidate"])
        self.assertTrue(set(self.calls) <= READ_ONLY_GIT, set(self.calls))

    def test_a_source_without_a_cargo_lock_is_unavailable(self):
        self.repo.git("rm", "-q", "Cargo.lock")
        unlocked = self.repo.commit("unlock")
        with self.assertRaises(Unavailable) as caught:
            self.plan(scope="bootstrap", base=self.c2, head=unlocked)
        self.assertEqual(caught.exception.reason, "missing_build_input")
        self.assertIn("candidate", str(caught.exception))


class Frozen(History):
    def test_plans_are_deterministic_and_self_describing(self):
        first = self.pr()
        second = self.pr()
        self.assertEqual(contracts.write_bytes(first, "run"), contracts.write_bytes(second, "run"))
        other = self.pr(seed=12)
        self.assertNotEqual(first["plan_digest"], other["plan_digest"])
        for case in first["cases"]:
            orders = case["warmup_orders"] + case["measured_orders"]
            self.assertEqual(len(case["measured_orders"]), case["pairs"])
            for block in range(0, len(orders), 2):
                self.assertIn(tuple(orders[block:block + 2]), (("ab", "ba"), ("ba", "ab")))
            self.assertEqual(case["rank"], stats.critical_rank(case["pairs"], 28,
                                                               stats.Fraction(1, 20)))
        ids = [case["instance_id"] for case in first["cases"]]
        self.assertEqual([s["instances"] for s in first["shards"]], [ids[0::2], ids[1::2]])

    def test_the_reader_recomputes_every_frozen_value(self):
        plan = self.pr()

        def refused(mutate, message):
            changed = copy.deepcopy(plan)
            mutate(changed)
            changed["plan_digest"] = contracts.plan_digest(changed)
            with self.assertRaises(ContractError) as caught:
                contracts.validate_run(changed)
            self.assertIn(message, str(caught.exception))

        tampered = copy.deepcopy(plan)
        tampered["seed"] += 1
        with self.assertRaises(ContractError):
            contracts.validate_run(tampered)
        refused(lambda p: p["cases"][0]["measured_orders"].reverse()
                if p["cases"][0]["measured_orders"] != p["cases"][0]["measured_orders"][::-1]
                else p["cases"][0].update(measured_orders=["ab"] * p["cases"][0]["pairs"]),
                "seeded AB/BA")
        refused(lambda p: p["cases"][0].update(rank=p["cases"][0]["rank"] - 1), "largest")
        refused(lambda p: p["verdict_policy"].update(family_size=27), "K counts")
        refused(lambda p: p["shards"].reverse(), "indexes run")
        refused(lambda p: p["cases"][0].update(alpha="1/20"), "alpha/K")
        refused(lambda p: p.update(required_check_eligible=True), "only an active full plan")
        refused(lambda p: p["resolved"].update(control_sha=self.c2), "first parent")
        refused(lambda p: p["limits"].update(free_disk_floor_bytes=1), "free_disk_floor_bytes")
        refused(lambda p: p["profile"]["recipe"].update(toolchain="1.89.0"), "recipe_digest")
        refused(lambda p: p.update(extra=1), "unknown field extra")
        refused(lambda p: p["build_inputs"]["baseline"].update(
            cargo_lock_sha256="sha256:" + "0" * 64), "name one commit, so one Cargo.lock")
        refused(lambda p: p["profile"]["build"].update(isolation="native_host"),
                "unpinned Linux recipe")
        refused(lambda p: p["profile"].update(build={
            "isolation": "container", "image": "sha256:" + "1" * 64, "image_source": "recipe"}),
            "unpinned Linux recipe")
        process = [index for index, case in enumerate(plan["cases"])
                   if case["kind"] == "process"][0]
        refused(lambda p: p["cases"][process].update(seed={
            "seed_id": "x", "case_id": "x", "contract_id": "x_v1",
            "parameters": {"command": "help"}}), "only a library probe reads a seed")
        refused(lambda p: p["cases"][0].update(seed={
            "seed_id": "x", "case_id": "x", "contract_id": "x_v1",
            "parameters": {"package": "issue_255_medium"}}), "a subset of the instance")
        refused(lambda p: p.update(schema="af.perf-run/5"), "expected 'af.perf-run/6'")

    def test_expected_slots_cover_sentinels_warmups_and_pairs(self):
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["cli_discovery__command_help"])
        slots = schedule.expected_slots(plan, 0)
        self.assertEqual(len(slots), 2 * (5 + 2 + 40 + 5))
        self.assertEqual([slot["sequence"] for slot in slots], list(range(len(slots))))
        sentinels = [slot for slot in slots if slot["phase"].startswith("sentinel")]
        self.assertEqual(set(slot["side"] for slot in sentinels), {"baseline"})
        measured = [slot for slot in slots if slot["phase"] == "measured"]
        for pair in range(1, 41):
            sides = [slot["side"] for slot in measured if slot["pair"] == pair]
            self.assertEqual(sorted(sides), ["baseline", "candidate"])


IMAGE_ID = "sha256:" + "a" * 64
NAMED = "ghcr.io/example/af-build@sha256:" + "b" * 64


class FrozenBuild(History):
    """How a recipe builds is frozen in the plan and its recipe identity (ADR-0170)."""

    def pinned(self, image):
        """A commit of this history whose registry pins the Linux GNU recipe to `image`."""
        text = read(SUITE).decode("utf-8").replace(
            'frame_pointers = false\nstate = "supported"\npending = ""\n\n'
            '[recipes.linux_musl_shipping]',
            'frame_pointers = false\ncontainer_image = "%s"\nstate = "supported"\n'
            'pending = ""\n\n[recipes.linux_musl_shipping]' % image, 1)
        self.assertIn(image, text)
        return self.repo.commit("pin", {"fixtures/performance/suite.toml": text})

    def build_plan(self, **values):
        values.setdefault("trusted_host_build", False)
        return self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["cli_discovery__command_help"], **values)

    def test_an_unpinned_linux_recipe_needs_an_explicit_frozen_build(self):
        with self.assertRaises(ContractError) as caught:
            self.build_plan()
        self.assertIn("pins no build image", str(caught.exception))
        for image, source in ((IMAGE_ID, "configured"), (NAMED, "configured")):
            plan = self.build_plan(build_image=image)
            self.assertEqual(plan["profile"]["build"], {"isolation": "container",
                                                        "image": image, "image_source": source})
        host = self.build_plan(trusted_host_build=True)
        self.assertEqual(host["profile"]["build"]["isolation"], "trusted_host")
        with self.assertRaises(ContractError) as caught:
            self.build_plan(build_image=IMAGE_ID, trusted_host_build=True)
        self.assertIn("exclude each other", str(caught.exception))
        for wrong in ("sha256:" + "A" * 64, "Example/af@sha256:" + "b" * 64,
                      "example/af:1.0@sha256:" + "b" * 64, "example/af:latest",
                      "sha256:" + "a" * 63):
            with self.assertRaises(ContractError, msg=wrong) as caught:
                self.build_plan(build_image=wrong)
            self.assertIn("canonical form", str(caught.exception))

    def test_different_effective_builds_have_different_recipe_identities(self):
        plans = [self.build_plan(build_image=IMAGE_ID), self.build_plan(build_image=NAMED),
                 self.build_plan(trusted_host_build=True)]
        self.assertEqual(len(set(plan["profile"]["recipe_id"] for plan in plans)), 1)
        self.assertEqual(len(set(plan["profile"]["recipe_digest"] for plan in plans)), 3)
        self.assertEqual(len(set(plan["plan_digest"] for plan in plans)), 3)
        for plan in plans:
            self.assertEqual(plan["profile"]["recipe_digest"], contracts.recipe_digest(
                plan["profile"]["recipe_id"], plan["profile"]["recipe"],
                plan["profile"]["build"]))

    def test_a_recipe_pin_is_its_build_and_may_only_be_restated(self):
        for image in (IMAGE_ID, NAMED):
            head = self.pinned(image)
            plan = self.plan(scope="partial_scope", base=head, head=head, authority=head,
                             cases=["cli_discovery__command_help"], trusted_host_build=False)
            self.assertEqual(plan["profile"]["build"], {"isolation": "container",
                                                        "image": image,
                                                        "image_source": "recipe"})
            restated = self.plan(scope="partial_scope", base=head, head=head, authority=head,
                                 cases=["cli_discovery__command_help"], build_image=image)
            self.assertEqual(restated, plan)
            other = NAMED if image == IMAGE_ID else IMAGE_ID
            with self.assertRaises(ContractError) as caught:
                self.plan(scope="partial_scope", base=head, head=head, authority=head,
                          cases=["cli_discovery__command_help"], build_image=other)
            self.assertIn("build_image_mismatch", str(caught.exception))
            with self.assertRaises(ContractError) as caught:
                self.plan(scope="partial_scope", base=head, head=head, authority=head,
                          cases=["cli_discovery__command_help"], trusted_host_build=True)
            self.assertIn("never builds on a host", str(caught.exception))

    def test_a_macos_recipe_builds_natively_and_takes_no_build_option(self):
        plan = self.plan(scope="partial_scope", tier="nightly", base=self.c2, head=self.c3,
                         environment="macos_arm64_local", cases=["cli_discovery"])
        self.assertEqual(plan["profile"]["build"], {"isolation": "native_host", "image": None,
                                                    "image_source": None})
        for option in ({"build_image": IMAGE_ID}, {"trusted_host_build": True}):
            with self.assertRaises(ContractError):
                self.plan(scope="partial_scope", tier="nightly", base=self.c2, head=self.c3,
                          environment="macos_arm64_local", cases=["cli_discovery"], **option)

    def test_a_diagnostic_child_builds_as_its_parent_froze(self):
        parent = self.build_plan(build_image=NAMED)
        child = self.plan(scope="diagnostic", parent_plan=parent, run_id="diag_run",
                          cases=["cli_discovery__command_help"], tier=None, seed=3)
        self.assertEqual(child["profile"]["build"], parent["profile"]["build"])
        for option in ({"build_image": IMAGE_ID}, {"trusted_host_build": True}):
            with self.assertRaises(ContractError):
                self.plan(scope="diagnostic", parent_plan=parent, run_id="diag_run",
                          cases=["cli_discovery__command_help"], tier=None, **option)

    def test_seeded_library_instances_name_the_registry_s_seed(self):
        plan = self.plan(scope="partial_scope", tier="nightly", base=self.c2, head=self.c3,
                         cases=["state_projection", "source_capture", "cli_discovery"])
        seeds = dict((case["instance_id"], case["seed"]) for case in plan["cases"])
        log = {"seed_id": "state_projection_seed__log_events_10000",
               "case_id": "state_projection_seed", "contract_id": "state_projection_seed_v1",
               "parameters": {"log": "events_10000"}}
        self.assertEqual(seeds["state_projection__log_events_10000_projection_prewarmed"], log)
        self.assertEqual(seeds["state_projection__log_events_10000_projection_application_cold"],
                         log)
        self.assertEqual(seeds["source_capture__files_10000_tree_source_files_128b"]["seed_id"],
                         "source_capture_seed__files_10000_tree_source_files_128b")
        self.assertEqual(seeds["cli_discovery__command_help"], None)


class Command(History):
    def run_cli(self, *args):
        result = subprocess.run([sys.executable, "-I", CLI] + list(args),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        return result.returncode, result.stdout, result.stderr.decode()

    def test_plan_list_and_refusal_exit_codes(self):
        code, out, _ = self.run_cli("plan", "--repo", self.repo.path, "--scope", "bootstrap",
                                    "--tier", "quick", "--base", self.c2, "--head", self.c3,
                                    "--run-id", "cli_run", "--seed", "4",
                                    "--build-image", NAMED)
        self.assertEqual(code, 0)
        plan = contracts.read_json(out, "run")
        self.assertEqual(plan["run_id"], "cli_run")
        self.assertEqual(plan["profile"]["build"]["image"], NAMED)
        code, out, _ = self.run_cli("plan", "--repo", self.repo.path, "--scope", "bootstrap",
                                    "--tier", "quick", "--base", self.c2, "--head", self.c3)
        self.assertEqual(code, 2)
        self.assertIn("pins no build image", contracts.read_json(out, "plan-refusal")["detail"])
        code, out, err = self.run_cli("plan", "--repo", self.repo.path, "--tier", "quick",
                                      "--base", self.c2, "--head", self.c3)
        self.assertEqual(code, 4, err)
        refusal = contracts.read_json(out, "plan-refusal")
        self.assertEqual((refusal["status"], refusal["reason"]),
                         ("unavailable", "no_active_authority"))
        code, out, _ = self.run_cli("plan", "--repo", self.repo.path, "--scope", "bootstrap",
                                    "--tier", "quick", "--base", self.c2, "--head", self.c3,
                                    "--pairs", "13", "--trusted-host-build")
        self.assertEqual(code, 2)
        self.assertEqual(contracts.read_json(out, "plan-refusal")["status"], "invalid")
        code, out, _ = self.run_cli("list", "--suite", SUITE, "--json")
        self.assertEqual(code, 0)
        listing = json.loads(out.decode())
        self.assertEqual(len(listing["tiers"]["quick"]), 28)
        self.assertEqual(listing["policy_digest"], suite.load(SUITE).policy_digest)
        broken = self.path("broken.toml")
        with open(broken, "wb") as stream:
            stream.write(read(SUITE).replace(b'relative_tolerance = "0.05"',
                                             b"relative_tolerance = 0.05"))
        self.assertEqual(self.run_cli("list", "--suite", broken)[0], 2)

    def test_synthetic_registry_plans_without_git(self):
        loaded = synthetic_suite()
        self.assertEqual(len(loaded.instances("quick")), 3)


if __name__ == "__main__":
    unittest.main()
