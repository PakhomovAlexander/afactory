"""Continuous scheduling of Actions events (ADR-0177).

Fixture events over a disposable Git history drive the actual scheduler and planner: nothing is
built, run or uploaded, and no event here was delivered by GitHub. History: R(v0.1.0) - C1 -
C2(v0.2.0) - C3 - M, M merging a feature that changes the instrument (a harness change).
"""
import json
import os
import subprocess
import sys

from perflab import ci, contracts, gitrefs, planning, suite
from perflab.values import ContractError, canonical_digest
from tests import test_planning
from tests.support import CLI, SUITE, fetched_checkout, read

IMAGE = "registry.example.invalid/lab/rust@sha256:" + "4" * 64
MUSL = "registry.example.invalid/lab/zigbuild@sha256:" + "5" * 64
REPOSITORY = "owner/af"


class Events(test_planning.History):

    def settings(self, **changes):
        values = {"build_images": {"linux_gnu_native_release": IMAGE}}
        values.update(changes)
        return ci.Settings(**values)

    def pr_event(self, merge, base_sha, head_sha, base_ref="main", head_repo=REPOSITORY,
                 name="pull_request", number=7):
        payload = {"pull_request": {"number": number,
                                    "base": {"ref": base_ref, "sha": base_sha,
                                             "repo": {"full_name": REPOSITORY}},
                                    "head": {"ref": "topic", "sha": head_sha,
                                             "repo": {"full_name": head_repo}}}}
        return ci.Event(name, payload, merge, "refs/pull/%d/merge" % number, REPOSITORY,
                        4242, 1)

    def schedule(self, event, settings=None):
        doc, plans = ci.schedule(self.git, event, settings or self.settings())
        # Every document the scheduler writes passes its own reader.
        self.assertEqual(contracts.read_json(contracts.write_bytes(doc, "schedule"),
                                             "schedule"), doc)
        for item in doc["comparisons"]:
            if item["status"] == "planned":
                plan = contracts.read_json(plans[item["run_id"]], "run")
                self.assertEqual(plan["plan_digest"], item["plan_digest"])
                self.assertEqual(plan["run_id"], item["run_id"])
        return doc, plans

    def roles(self, doc):
        return dict((item["role"], item) for item in doc["comparisons"])

    def merged(self, files, base=None):
        """A PR merge commit over the main tip (or `base`), main itself unchanged: (merge,
        base, head)."""
        base = base or self.repo.git("rev-parse", "main")
        self.repo.git("checkout", "-q", "-b", "topic-%d" % len(self.repo.git("branch").split()),
                      base)
        head = self.repo.commit("topic", files)
        self.repo.git("checkout", "-q", "--detach", base)
        self.repo.git("merge", "-q", "--no-ff", "-m", "merge topic", head)
        merge = self.repo.git("rev-parse", "HEAD")
        self.repo.git("checkout", "-q", "main")
        return merge, base, head

    # Pull requests ------------------------------------------------------------------------

    def test_a_pr_before_activation_is_unavailable_full_and_measured_advisory(self):
        doc, plans = self.schedule(self.pr_event(self.merge, self.c3, self.feature))
        roles = self.roles(doc)
        self.assertEqual((roles["primary"]["status"], roles["primary"]["reason"]),
                         ("unavailable", "no_active_authority"))
        bootstrap = roles["control_bootstrap"]
        self.assertEqual(bootstrap["status"], "planned")
        self.assertFalse(bootstrap["required_check_eligible"])
        self.assertTrue(bootstrap["harness_changed"], "the feature changes the instrument")
        plan = contracts.read_json(plans[bootstrap["run_id"]], "run")
        self.assertEqual(plan["trigger"], {"kind": "pr", "number": 7, "head_sha": self.feature,
                                           "base_sha": self.c3, "merge_sha": self.merge,
                                           "merge_base_sha": self.c3})
        # The control's own registry measures both sides; the candidate's instrument does not.
        self.assertEqual((plan["authority"]["mode"], plan["resolved"]["authority_sha"]),
                         ("control", self.c3))
        self.assertEqual(plan["profile"]["build"], {"isolation": "container", "image": IMAGE,
                                                    "image_source": "configured"})
        self.assertFalse(doc["policy_changed"])
        self.assertEqual(doc["trust"], {"trusted": False, "executor": "ephemeral_hosted",
                                        "detail": doc["trust"]["detail"]})
        self.assertEqual((doc["enforcement"], doc["retention_days"]), ("advisory", 14))
        self.assertEqual(doc["matrix"], [{"run_id": bootstrap["run_id"], "shard": index}
                                         for index in range(bootstrap["shards"])])
        self.assertEqual(bootstrap["run_id"], "gh4242_a1_control_bootstrap")
        outputs = ci.outputs(doc)
        self.assertEqual(json.loads(outputs["builds"]), [bootstrap["run_id"]])
        self.assertEqual(json.loads(outputs["runner"]), "ubuntu-latest")
        self.assertIn("Enforcement is advisory", ci.summary_markdown(doc))

    def test_a_fork_never_gets_dedicated_capacity_or_anything_trusted(self):
        settings = self.settings(dedicated_label="perf-dedicated", executor="dedicated")
        doc, _ = self.schedule(self.pr_event(self.merge, self.c3, self.feature,
                                             head_repo="someone/af"), settings)
        self.assertTrue(doc["pull_request"]["fork"])
        self.assertEqual(doc["trust"]["executor"], "ephemeral_hosted")
        self.assertIn("refused for this untrusted event", doc["trust"]["detail"])
        for item in doc["comparisons"]:
            self.assertIn(item["runner_label"], (None, "ubuntu-latest"))
        # The reader refuses dedicated capacity for an untrusted event however it was written.
        forged = json.loads(json.dumps(doc))
        forged["trust"]["executor"] = "dedicated"
        with self.assertRaises(ContractError):
            contracts.validate_schedule(forged)

    def test_a_wrong_event_base_refuses_the_plan_and_chooses_nothing_else(self):
        doc, plans = self.schedule(self.pr_event(self.merge, self.c2, self.feature))
        self.assertEqual([(item["role"], item["status"]) for item in doc["comparisons"]],
                         [("primary", "refused")])
        self.assertIn("disagrees with the event's base SHA", doc["comparisons"][0]["detail"])
        self.assertEqual((doc["matrix"], plans), ([], {}))

    def test_an_unsupported_parent_shape_is_unavailable(self):
        doc, plans = self.schedule(self.pr_event(self.c3, self.c2, self.c3))
        self.assertEqual([(item["role"], item["status"], item["reason"])
                          for item in doc["comparisons"]],
                         [("primary", "unavailable", "not_a_merge_commit")])
        self.assertEqual(plans, {})

    def test_gate_branches_other_bases_and_pull_request_target_are_never_measured(self):
        for base_ref, reason in (("af-gate/task_1/base", "gate_branch"),
                                 ("release/0.13", "not_main")):
            doc, plans = self.schedule(self.pr_event(self.merge, self.c3, self.feature,
                                                     base_ref=base_ref))
            self.assertEqual((doc["status"], doc["reason"], doc["comparisons"], plans),
                             ("skipped", reason, [], {}))
            self.assertEqual(ci.outputs(doc)["any"], "false")
        event = ci.Event("pull_request_target", {}, self.merge, "refs/heads/main", REPOSITORY,
                         4242, 1)
        doc, _ = self.schedule(event)
        self.assertEqual((doc["status"], doc["reason"]), ("skipped", "unsupported_event"))

    def test_a_policy_change_adds_both_proposed_bootstraps_and_cannot_weaken_the_gate(self):
        # main activates its own registry; the PR lowers a case's pairs in the candidate's.
        self.activate()
        policy = read(SUITE).decode("utf-8").replace(
            "pairs = { quick = 40, nightly = 40, release = 40 }",
            "pairs = { quick = 12, nightly = 40, release = 40 }", 1)
        self.assertIn("quick = 12", policy)
        merge, base, head = self.merged({planning.SUITE_PATH: policy})
        doc, plans = self.schedule(self.pr_event(merge, base, head))
        roles = self.roles(doc)
        self.assertTrue(doc["policy_changed"])
        primary = contracts.read_json(plans[roles["primary"]["run_id"]], "run")
        self.assertEqual(primary["authority"]["mode"], "active")
        self.assertEqual(primary["resolved"]["authority_sha"], self.root)
        self.assertEqual(primary["authority"]["policy_digest"],
                         suite.load(SUITE).policy_digest, "the candidate's policy measures nothing")
        self.assertTrue(primary["harness"]["changed"])
        self.assertTrue(roles["primary"]["required_check_eligible"])
        for role, (baseline, candidate) in (("proposed_bootstrap_base", (base, merge)),
                                            ("proposed_bootstrap_merge", (merge, merge))):
            item = roles[role]
            self.assertEqual(item["status"], "planned", item["detail"])
            self.assertFalse(item["required_check_eligible"])
            plan = contracts.read_json(plans[item["run_id"]], "run")
            self.assertEqual((plan["scope"], plan["authority"]["mode"],
                              plan["resolved"]["authority_sha"]),
                             ("bootstrap", "proposed", merge))
            self.assertEqual((plan["resolved"]["baseline_sha"],
                              plan["resolved"]["candidate_sha"],
                              plan["resolved"]["control_sha"]), (baseline, candidate, base))
        self.assertEqual(doc["enforcement"], "advisory")

    def test_without_a_build_image_a_linux_recipe_is_unavailable_never_a_host_build(self):
        doc, plans = self.schedule(self.pr_event(self.merge, self.c3, self.feature),
                                   ci.Settings())
        roles = self.roles(doc)
        self.assertEqual(roles["control_bootstrap"]["reason"], "no_build_image")
        self.assertEqual(plans, {})
        with self.assertRaises(ContractError):
            ci.Settings({"linux_gnu_native_release": "sha256:" + "6" * 64})

    # Trusted events -----------------------------------------------------------------------

    def test_a_main_push_measures_its_first_parent_and_a_root_has_none(self):
        settings = self.settings(dedicated_label="perf-dedicated", executor="dedicated")
        event = ci.Event("push", {}, self.c3, "refs/heads/main", REPOSITORY, 9, 2)
        doc, plans = self.schedule(event, settings)
        roles = self.roles(doc)
        self.assertEqual(doc["trust"]["executor"], "dedicated")
        self.assertEqual(doc["retention_days"], 90)
        plan = contracts.read_json(plans[roles["control_bootstrap"]["run_id"]], "run")
        self.assertEqual((plan["resolved"]["baseline_sha"], plan["resolved"]["candidate_sha"]),
                         (self.c2, self.c3))
        self.assertEqual(roles["control_bootstrap"]["runner_label"], "perf-dedicated")
        self.assertEqual(roles["control_bootstrap"]["run_id"], "gh9_a2_control_bootstrap")
        doc, plans = self.schedule(ci.Event("push", {}, self.root, "refs/heads/main",
                                            REPOSITORY, 9, 1))
        self.assertEqual([(item["status"], item["reason"]) for item in doc["comparisons"]],
                         [("unavailable", "missing_parent")])
        doc, _ = self.schedule(ci.Event("push", {}, self.c3, "refs/heads/topic", REPOSITORY,
                                        9, 1))
        self.assertEqual(doc["reason"], "not_main")

    def test_the_nightly_needs_its_shipping_image_and_its_highest_stable_release(self):
        event = ci.Event("schedule", {}, self.c3, "refs/heads/main", REPOSITORY, 11, 1)
        doc, _ = self.schedule(event)
        self.assertEqual(self.roles(doc)["control_bootstrap"]["reason"], "no_build_image")
        settings = self.settings(build_images={"linux_musl_shipping": MUSL})
        doc, plans = self.schedule(event, settings)
        item = self.roles(doc)["control_bootstrap"]
        plan = contracts.read_json(plans[item["run_id"]], "run")
        self.assertEqual((plan["tier"], plan["trigger"]["baseline_tag"],
                          plan["resolved"]["baseline_sha"]), ("nightly", "v0.2.0", self.c2))

    def test_a_dispatch_takes_only_full_shas_or_qualified_refs(self):
        def dispatch(**inputs):
            return self.schedule(ci.Event("workflow_dispatch", {"inputs": inputs}, self.c3,
                                          "refs/heads/main", REPOSITORY, 12, 1))

        doc, _ = dispatch(base="main", head=self.c3, cases="cli_discovery")
        self.assertEqual((doc["comparisons"][0]["status"], doc["comparisons"][0]["reason"]),
                         ("refused", "invalid_dispatch"))
        self.assertIn("ambiguous", doc["comparisons"][0]["detail"])
        doc, _ = dispatch(base="f" * 40, head=self.c3, cases="cli_discovery")
        self.assertEqual(doc["comparisons"][0]["reason"], "unresolved_ref")
        doc, plans = dispatch(base="refs/tags/v0.2.0", head=self.c3, cases="cli_discovery",
                              shards="1", pairs="12")
        item = doc["comparisons"][0]
        self.assertEqual(item["status"], "planned", item["detail"])
        plan = contracts.read_json(plans[item["run_id"]], "run")
        self.assertEqual((plan["resolved"]["baseline_sha"], plan["scope"], plan["seed"] > 0),
                         (self.c2, "partial_scope", True))
        self.assertEqual(set(case["pairs"] for case in plan["cases"]), {12})
        self.assertTrue(doc["trust"]["trusted"])

    def test_a_dispatch_reads_branches_where_the_checkout_fetched_them(self):
        # The origin's branches, fetched with the pinned checkout's own refspecs into a new
        # checkout of main: they are only remote-tracking refs there (ADR-0178).
        self.repo.git("branch", "dispatch_baseline", self.c2)
        self.repo.git("branch", "dispatch_candidate", self.c3)
        checkout = fetched_checkout(self.repo.path, self.path("checkout"))
        for name in ("dispatch_baseline", "dispatch_candidate"):
            self.assertEqual(checkout.git("for-each-ref", "refs/heads/" + name), "")
            self.assertNotEqual(checkout.git("for-each-ref", "refs/remotes/origin/" + name), "")
        self.git = gitrefs.Git(checkout.path)

        def dispatch(**inputs):
            doc, plans = self.schedule(ci.Event("workflow_dispatch", {"inputs": dict(
                inputs, cases="cli_discovery")}, self.c3, "refs/heads/main", REPOSITORY, 12, 1))
            return doc["comparisons"][0], plans

        item, plans = dispatch(base="refs/heads/dispatch_baseline",
                               head="refs/heads/dispatch_candidate",
                               control="refs/heads/dispatch_baseline")
        self.assertEqual(item["status"], "planned", item["detail"])
        plan = contracts.read_json(plans[item["run_id"]], "run")
        self.assertEqual((plan["resolved"]["baseline_sha"], plan["resolved"]["candidate_sha"],
                          plan["resolved"]["control_sha"]), (self.c2, self.c3, self.c2))
        self.assertEqual((item["request"]["base"], item["request"]["head"]), (self.c2, self.c3),
                         "the exact resolved identities are frozen, not the branch names")
        # The event's own branch exists locally and on origin, naming one commit.
        item, plans = dispatch(base="refs/tags/v0.2.0", head="refs/heads/main")
        self.assertEqual(item["status"], "planned", item["detail"])
        self.assertEqual(contracts.read_json(plans[item["run_id"]], "run")["resolved"][
            "candidate_sha"], self.merge)
        # Full SHAs and tags resolve as before.
        item, _ = dispatch(base=self.c2, head="refs/heads/dispatch_candidate")
        self.assertEqual(item["status"], "planned", item["detail"])
        # A branch nobody fetched is unresolved; a short name is still never guessed.
        item, _ = dispatch(base="refs/heads/absent", head=self.c3)
        self.assertEqual((item["status"], item["reason"]), ("refused", "unresolved_ref"))
        item, _ = dispatch(base="dispatch_baseline", head=self.c3)
        self.assertEqual((item["status"], item["reason"]), ("refused", "invalid_dispatch"))
        # A local branch that disagrees with the fetched one of its name is ambiguous.
        checkout.git("branch", "dispatch_candidate", self.c2)
        item, _ = dispatch(base=self.c2, head="refs/heads/dispatch_candidate")
        self.assertEqual((item["status"], item["reason"]), ("refused", "ambiguous_ref"))
        self.assertIn("never guessed", item["detail"])
        # One that agrees is the same branch.
        checkout.git("branch", "-f", "dispatch_candidate", self.c3)
        item, _ = dispatch(base=self.c2, head="refs/heads/dispatch_candidate")
        self.assertEqual(item["status"], "planned", item["detail"])

    def test_a_dispatched_calibration_plans_a_proposed_authority_without_activation(self):
        cohort = test_planning.Authority.cohort(self)
        digest = canonical_digest(cohort)
        control = self.repo.commit("cohort", {
            "fixtures/performance/cohorts/first.json": json.dumps(cohort)})
        # The run was named for the cohort digest it was dispatched with (ADR-0182); a digest
        # other than the control's cohort is refused, and that run stays the member's failed
        # original.
        doc, _ = self.schedule(ci.Event("workflow_dispatch", {"inputs": {
            "base": self.root, "head": self.root, "control": control, "authority": self.root,
            "scope": "calibration", "dispatch_head": control, "cohort": "fixtures/performance/cohorts/first.json",
            "member": "aa_03", "cohort_digest": "sha256:" + "0" * 64}}, control,
            "refs/heads/main", REPOSITORY, 12, 1))
        self.assertEqual((doc["comparisons"][0]["status"], doc["comparisons"][0]["reason"]),
                         ("refused", "invalid_dispatch"))
        self.assertIn("the run is named for sha256:" + "0" * 64,
                      doc["comparisons"][0]["detail"])
        doc, plans = self.schedule(ci.Event("workflow_dispatch", {"inputs": {
            "base": self.root, "head": self.root, "control": control, "authority": self.root,
            "scope": "calibration", "dispatch_head": control, "cohort": "fixtures/performance/cohorts/first.json",
            "member": "aa_03", "cohort_digest": digest}}, control, "refs/heads/main",
            REPOSITORY, 13, 1))
        item = doc["comparisons"][0]
        self.assertEqual(item["status"], "planned", item["detail"])
        plan = contracts.read_json(plans[item["run_id"]], "run")
        self.assertEqual((plan["scope"], plan["authority"]["mode"], plan["authority"]["activation"],
                          plan["cohort"]["member_id"], plan["cohort"]["injection"]),
                         ("calibration", "proposed", None, "aa_03", None))
        # The member's frozen logical identity is planned by Actions run 13 attempt 1; that ID
        # is provenance in the lab run ID, never written into the cohort.
        self.assertEqual(plan["run_id"], "gh13_a1_primary")
        # A power member needs sealed parameters from the control revision; without them the
        # dispatch is refused, and with a path outside it too.
        doc, _ = self.schedule(ci.Event("workflow_dispatch", {"inputs": {
            "base": self.root, "head": self.root, "control": control, "authority": self.root,
            "scope": "calibration", "dispatch_head": control, "cohort": "fixtures/performance/cohorts/first.json",
            "member": "power_03", "cohort_digest": digest}}, control, "refs/heads/main",
            REPOSITORY, 14, 1))
        self.assertEqual(doc["comparisons"][0]["status"], "refused")
        self.assertIn("sealed calibration parameters", doc["comparisons"][0]["detail"])
        doc, _ = self.schedule(ci.Event("workflow_dispatch", {"inputs": {
            "base": self.root, "head": self.root, "control": control, "authority": self.root,
            "scope": "calibration", "dispatch_head": control, "cohort": "fixtures/performance/cohorts/first.json",
            "member": "power_03", "calibration": "fixtures/performance/absent.json",
            "cohort_digest": digest}},
            control, "refs/heads/main", REPOSITORY, 15, 1))
        self.assertEqual(doc["comparisons"][0]["status"], "refused")
        self.assertIn("is not in the control revision", doc["comparisons"][0]["detail"])

    def test_calibration_source_mismatch_never_plans_or_resolves_sources(self):
        from unittest import mock
        for dispatch_head in (None, "", self.c3[:12], "g" * 40, self.c2, self.c3 + "\n"):
            with self.subTest(dispatch_head=dispatch_head), \
                    mock.patch.object(self.git, "resolve", side_effect=AssertionError("source read")):
                doc, plans = self.schedule(ci.Event("workflow_dispatch", {"inputs": {
                    "scope": "calibration", "dispatch_head": dispatch_head}}, self.c3,
                    "refs/heads/main", REPOSITORY, 17, 1))
                self.assertEqual(doc["comparisons"][0]["reason"], "source_mismatch")
                self.assertIn("non-member", doc["comparisons"][0]["detail"])
                self.assertEqual((plans, doc["matrix"], ci.outputs(doc)["any"]), ({}, [], "false"))

    def test_a_calibration_member_never_runs_on_a_dedicated_executor(self):
        # A cohort calibrates fresh hosted runners (ADR-0183): dedicated capacity is refused
        # for a member whether or not a dedicated label is configured, while the same dispatch
        # on hosted runners plans its member for the environment's hosted label.
        cohort = test_planning.Authority.cohort(self)
        digest = canonical_digest(cohort)
        control = self.repo.commit("cohort", {
            "fixtures/performance/cohorts/first.json": json.dumps(cohort)})
        inputs = {"base": self.root, "head": self.root, "control": control,
                  "authority": self.root, "scope": "calibration", "dispatch_head": control,
                  "cohort": "fixtures/performance/cohorts/first.json", "member": "aa_03",
                  "cohort_digest": digest}
        for number, settings in enumerate((
                self.settings(executor="dedicated", dedicated_label="trusted-perf"),
                self.settings(executor="dedicated")), 20):
            doc, plans = self.schedule(ci.Event("workflow_dispatch", {"inputs": inputs},
                                                control, "refs/heads/main", REPOSITORY,
                                                number, 1), settings)
            item = doc["comparisons"][0]
            self.assertEqual((item["status"], item["reason"], plans),
                             ("refused", "dedicated_calibration", {}))
            self.assertIn("fresh hosted runners", item["detail"])
            self.assertEqual(ci.outputs(doc)["any"], "false")
        doc, plans = self.schedule(ci.Event("workflow_dispatch", {"inputs": inputs}, control,
                                            "refs/heads/main", REPOSITORY, 30, 1))
        item = doc["comparisons"][0]
        self.assertEqual((item["status"], item["runner_label"]), ("planned", "ubuntu-latest"))
        self.assertEqual(doc["trust"]["executor"], "ephemeral_hosted")
        # A dedicated push still uses its dedicated runner: only calibration is refused.
        doc, _ = self.schedule(ci.Event("push", {"before": self.c2}, self.c3,
                                        "refs/heads/main", REPOSITORY, 31, 1),
                               self.settings(executor="dedicated",
                                             dedicated_label="trusted-perf"))
        self.assertEqual(doc["trust"]["executor"], "dedicated")

    # The command line ---------------------------------------------------------------------

    def test_the_command_writes_the_schedule_plans_and_job_outputs(self):
        event = self.path("event.json")
        with open(event, "w") as stream:
            json.dump(self.pr_event(self.merge, self.c3, self.feature).payload, stream)
        outputs, summary = self.path("outputs"), self.path("summary.md")
        result = subprocess.run(
            [sys.executable, CLI, "ci-schedule", "--repo", self.repo.path,
             "--event-name", "pull_request", "--event", event, "--inputs", "null",
             "--sha", self.merge, "--ref", "refs/pull/7/merge", "--repository", REPOSITORY,
             "--run-id", "77", "--run-attempt", "1",
             "--build-image", "linux_gnu_native_release=" + IMAGE,
             "--output", self.path("schedule"), "--github-output", outputs,
             "--job-summary", summary], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        doc = contracts.read_file(self.path("schedule", "schedule.json"), "schedule")
        self.assertEqual(os.listdir(self.path("schedule", "plans")),
                         ["gh77_a1_control_bootstrap.json"])
        lines = dict(line.split("=", 1) for line in read(outputs).decode().splitlines())
        self.assertEqual(json.loads(lines["matrix"]), doc["matrix"])
        self.assertEqual(lines["retention_days"], "14")
        self.assertIn("unavailable: no&#95;active&#95;authority", read(summary).decode())
