"""Reproduction commands keep the frozen configuration (ADR-0177; root finding
root-report-reproduction-1).

Each test freezes a plan with the actual planner in a disposable Git fixture, takes the rerun
command the report would print, runs that command itself through `cli.py plan` in the same
fixture, and compares the new plan with the original. Only the run's own identity, its parent
link and the selector spellings (now the resolved SHAs) may differ. Nothing is built or sampled.
The retained failing observation: a 24-instance partial selection with 12 pairs replanned to 20
instances with 40 pairs, a proposed authority became the control's, 1 shard became 3, and a
configured Linux image or a trusted host build was refused as invalid_plan.
"""
import json
import os
import subprocess
import sys

from perflab import contracts, diagnostics, planning, report, reproduce, suite
from tests.support import CLI, SUITE
from tests import test_planning

IMAGE = "registry.example.invalid/lab/rust@sha256:" + "4" * 64
# What a rerun may change: its own identity, its parent link and how its selectors are spelled.
ALLOWED = ("run_id", "parent_run_id", "plan_digest", "selectors")


class Reruns(test_planning.History):
    cohort = test_planning.Authority.cohort

    def rerun(self, plan, cohort=None):
        argv, limitations = reproduce.rerun(plan)
        self.assertEqual(argv[:3], ["python3", "scripts/performance/cli.py", "plan"])
        values = {"NEW_RUN_ID": "rerun_of_" + plan["run_id"]}
        if cohort is not None:
            path = self.path("cohort.json")
            with open(path, "w") as stream:
                json.dump(cohort, stream)
            values["COHORT_JSON"] = path
        command = [sys.executable, CLI] + [values.get(item, item) for item in argv[2:]] + [
            "--repo", self.repo.path, "--output", self.path("rerun.json")]
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stdout.decode() + result.stderr.decode())
        with open(self.path("rerun.json"), "rb") as stream:
            again = contracts.read_json(stream.read(), "run", "rerun.json")
        self.assertEqual(again["parent_run_id"], plan["run_id"])
        self.assertEqual(again["run_id"], "rerun_of_" + plan["run_id"])
        for key in ALLOWED:
            again.pop(key)
        original = dict((key, value) for key, value in plan.items() if key not in ALLOWED)
        self.assertEqual(again, original)
        return argv, limitations

    def test_a_large_partial_selection_with_custom_schedule_and_proposed_authority(self):
        instances = [item["instance_id"] for item in suite.load(SUITE).instances("quick")]
        self.assertGreater(len(instances), 20)
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3, control=self.c3,
                         authority=self.c1, cases=instances, shards=1, pairs=12, seed=99,
                         run_id="partial_run", build_image=IMAGE)
        self.assertEqual((plan["authority"]["mode"], len(plan["shards"])), ("proposed", 1))
        argv, _ = self.rerun(plan)
        self.assertEqual(argv.count("--case"), len(instances), "no case is truncated")
        self.assertIn(IMAGE, argv)

    def test_a_trusted_host_build_and_the_control_authority_stay_as_frozen(self):
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["cli_discovery", "source_capture"], shards=2, run_id="host_run")
        self.assertEqual(plan["authority"]["mode"], "control")
        self.assertEqual(plan["profile"]["build"]["isolation"], "trusted_host")
        argv, _ = self.rerun(plan)
        self.assertIn("--trusted-host-build", argv)
        self.assertNotIn("--authority", argv, "the control's registry is not a proposed policy")

    def test_a_pr_bootstrap_with_a_proposed_policy_keeps_its_event(self):
        plan = self.pr(authority=self.merge, run_id="pr_run", shards=3)
        argv, limitations = self.rerun(plan)
        self.assertIn(self.feature, argv)
        self.assertTrue(any(self.merge in text for text in limitations))

    def test_main_push_and_nightly_reruns_check_their_baselines(self):
        self.rerun(self.plan(trigger="main_push", scope="bootstrap", head=self.c3,
                             run_id="push_run"))
        plan = self.plan(trigger="nightly", tier="nightly", scope="bootstrap", head=self.c3,
                         run_id="nightly_run", build_image=IMAGE)
        argv, limitations = self.rerun(plan)
        self.assertIn(self.c2, argv)
        # A newer reachable release refuses the rerun rather than choosing it.
        self.repo.tag("v0.3.0", self.c3)
        values = {"NEW_RUN_ID": "late_rerun"}
        result = subprocess.run([sys.executable, CLI] + [values.get(item, item)
                                                          for item in argv[2:]]
                                + ["--repo", self.repo.path], stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 2, result.stderr.decode())
        self.assertIn("trigger's rule selects", result.stderr.decode())

    def test_a_full_active_plan_and_a_calibration_member_reproduce_exactly(self):
        # The pushed commit's first parent, its control, holds the activation.
        self.activate()
        pushed = self.repo.commit("after activation", {"README": "pushed\n"})
        full = self.plan(trigger="main_push", head=pushed, run_id="full_run")
        self.assertEqual(full["authority"]["mode"], "active")
        self.rerun(full)
        cohort = self.cohort()
        member = self.plan(scope="calibration", authority=self.root, control=self.c3,
                           base=self.root, head=self.root, cohort=cohort, member="aa_03",
                           seed=None, run_id="calibration_run")
        argv, limitations = self.rerun(member, cohort=cohort)
        self.assertIn("COHORT_JSON", argv)
        self.assertNotIn("CALIBRATION_JSON", argv)
        self.assertTrue(any(canonical in text for text in limitations
                            for canonical in (member["cohort"]["cohort_digest"],)))

    def test_the_report_prints_exactly_these_commands(self):
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["cli_discovery"], run_id="report_run")
        commands = reproduce.commands(plan)
        self.assertEqual([item["purpose"] for item in commands],
                         ["recompute", "report", "rerun"])
        self.assertEqual(commands[2]["argv"], reproduce.rerun(plan)[0])
        self.assertIs(report.reproduce, reproduce)


class DiagnosticCommands(test_planning.History):
    """A diagnostic omission's command keeps the child run's selection, capture bounds, recipe,
    mode and build inputs; each is exercised through argument parsing, selection and the
    child's frozen plan only, never a build or capture."""

    def parent(self):
        plan = self.plan(scope="partial_scope", base=self.c2, head=self.c3,
                         cases=["canonical_serialize", "cli_discovery"], run_id="parent_run")
        path = self.path("parent.json")
        with open(path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        return plan, path

    def replan(self, argv, parent, path):
        sys.path.insert(0, os.path.dirname(CLI))
        try:
            import cli as command_line
        finally:
            sys.path.pop(0)
        # The same child run ID, so the identity-derived seed is comparable too.
        values = {"{output}": self.path("child"), "{verdict}": self.path("verdict.json"),
                  "NEW_RUN_ID": "child_run"}
        args = command_line.parser().parse_args(
            [values.get(item, item) for item in argv[2:]])
        self.assertEqual(args.plan, path)
        selection, cases = diagnostics.select(parent, explicit=args.case or ())
        request = planning.Request(scope="diagnostic", parent_plan=parent,
                                   cases=[case["instance_id"] for case in cases],
                                   run_id=args.run_id,
                                   diagnostic_duration_ms=args.duration_seconds * 1000,
                                   diagnostic_frequency_hz=args.frequency_hz)
        return args, selection, planning._diagnostic(self.git, request)

    def test_a_global_omission_keeps_every_selected_case_and_custom_bounds(self):
        parent, path = self.parent()
        instances = [case["instance_id"] for case in parent["cases"]]
        request = planning.Request(scope="diagnostic", parent_plan=parent, cases=instances,
                                   run_id="child_run", diagnostic_duration_ms=7000,
                                   diagnostic_frequency_hz=499)
        child = planning._diagnostic(self.git, request)
        options = diagnostics.Options(plan=path, cases=instances, total_minutes=9,
                                      build="/somewhere/build", parent_build="/the/parent")
        selection, _ = diagnostics.select(parent, explicit=instances)
        argv = reproduce.profile(options, child, ["perf", "inferno_flamegraph"],
                                 "linux_container", selection)
        for flag in ("--case", "--duration-seconds", "--frequency-hz", "--total-minutes",
                     "--build", "--parent-build", "--perf", "--inferno-bin", "--image",
                     "--scratch-fs"):
            self.assertIn(flag, argv)
        self.assertEqual(argv.count("--case"), len(instances))
        args, again, replanned = self.replan(argv, parent, path)
        self.assertEqual(again, selection)
        self.assertEqual((args.total_minutes, args.mode), (9, "linux_container"))
        for key in ("run_id", "plan_digest"):
            replanned.pop(key)
            child.pop(key)
        self.assertEqual(replanned, child)

    def test_an_automatic_selection_names_its_verdict_and_limit(self):
        parent, path = self.parent()
        selection = {"mode": "automatic", "limit": 2, "parent_verdict_sha256": "sha256:" + "1" * 64,
                     "cases": [], "not_selected": []}
        request = planning.Request(scope="diagnostic", parent_plan=parent,
                                   cases=[parent["cases"][0]["instance_id"]], run_id="child_run")
        child = planning._diagnostic(self.git, request)
        argv = reproduce.profile(diagnostics.Options(plan=path), child, [], "trusted_local",
                                 selection)
        self.assertEqual(argv[argv.index("--verdict") + 1], "{verdict}")
        self.assertEqual(argv[argv.index("--max-cases") + 1], "2")
        self.assertIn("--no-build", argv)
        self.assertNotIn("--case", argv)
        one = reproduce.profile(diagnostics.Options(plan=path), child, [], "trusted_local",
                                selection, instance_id=parent["cases"][0]["instance_id"])
        self.assertNotIn("--verdict", one)
        self.assertEqual(one.count("--case"), 1)
