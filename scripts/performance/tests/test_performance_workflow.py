"""The continuous performance workflow's contract (ADR-0177), read as text.

The standard library has no YAML reader, so these tests read `.github/workflows/performance.yml`
line by line with the helpers of tests.test_workflow, and execute its own shell steps where the
decision is theirs (the trusted control's resolution, the attempt guard) against a disposable
Git repository. Passing them is not a hosted run: no workflow here has executed on GitHub.
"""
import json
import os
import re
import shutil
import subprocess
import unittest

from tests import test_workflow as text
from tests.support import ROOT, Repo, TempDir, fetched_checkout, read

WORKFLOW = os.path.join(ROOT, ".github/workflows/performance.yml")
PIN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$")


def step_named(job, prefix):
    for step in text.steps(job):
        if re.search(r"name: " + re.escape(prefix), step[0]) or any(
                re.search(r"^        name: " + re.escape(prefix), line) for line in step[:2]):
            return step
    raise AssertionError("no step named %s" % prefix)


def with_value(step, key):
    for line in step:
        match = re.match(r"^\s+%s: (.*?)\s*$" % re.escape(key), line)
        if match:
            return match.group(1)
    return None


class Workflow(unittest.TestCase):
    def setUp(self):
        self.text = read(WORKFLOW).decode("utf-8")
        self.jobs = text.jobs(self.text)

    def test_events_permissions_and_no_secrets(self):
        head = self.text[:self.text.index("\njobs:")]
        self.assertIn("  pull_request:\n    branches: [main]\n", head)
        self.assertIn("  push:\n    branches: [main]\n", head)
        self.assertIn("  schedule:\n", head)
        self.assertIn("  workflow_dispatch:\n", head)
        self.assertNotIn("pull_request_target", self.text)
        self.assertIn("\npermissions:\n  contents: read\n\n", self.text)
        self.assertEqual(self.text.count("permissions:"), 1, "no job widens the permissions")
        for forbidden in ("secrets.", "id-token", "pull-requests: write", "issues: write",
                          "github-script", "GITHUB_TOKEN"):
            self.assertNotIn(forbidden, self.text)

    def test_a_label_change_replaces_the_running_pull_request_measurement(self):
        # Adding `no-perf` must cancel a running measurement and removing it must measure
        # again: both are label events of the one pull request concurrency group (ADR-0196).
        head = self.text[:self.text.index("\njobs:")]
        self.assertIn("  pull_request:\n    branches: [main]\n"
                      "    # A label change starts a new run", head)
        self.assertIn("    types: [opened, synchronize, reopened, labeled, unlabeled]\n", head)
        self.assertIn("  group: performance-${{ github.event_name }}-${{ "
                      "github.event.pull_request.number || inputs.release_claim || github.ref }}\n"
                      "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n", head)

    def test_the_no_perf_scope_precedes_every_plan_build_and_matrix(self):
        names = list(self.jobs)
        self.assertEqual(names[:2], ["scope", "plan"])
        scope = self.jobs["scope"]
        self.assertIsNone(text.key(scope, "needs"))
        self.assertEqual(text.key(scope, "outputs"), "")
        self.assertIn("      exempt: ${{ steps.label.outputs.exempt }}", scope)
        # Only the trusted event base runs selection; candidate code is never executed.
        joined = "\n".join(scope)
        self.assertIn("github.event.pull_request.base.sha || github.sha", joined)
        self.assertIn("measurement-scope", joined)
        self.assertIn("persist-credentials: false", joined)
        for forbidden in ("cli.py", "download-artifact", "upload-artifact"):
            self.assertNotIn(forbidden, joined)
        self.assertEqual(text.key(self.jobs["plan"], "needs"), "scope")
        self.assertEqual(text.key(self.jobs["plan"], "if"),
                         "needs.scope.outputs.exempt == 'false'")
        # Everything else is planned from the plan job, so an exemption skips it all.
        self.assertEqual(text.key(self.jobs["build"], "needs"), "plan")
        self.assertEqual(text.key(self.jobs["build"], "if"), "needs.plan.outputs.any == 'true'")
        self.assertIn("plan", text.key(self.jobs["shard"], "needs"))
        self.assertIn("plan", text.key(self.jobs["aggregate"], "needs"))
        # The label is read nowhere else.
        self.assertEqual(self.text.count('"no-perf"'), 1)

    def test_the_stable_check_reports_an_exemption_and_otherwise_requires_the_plan(self):
        status = self.jobs["status"]
        self.assertEqual(text.key(status, "name"), "Performance check")
        self.assertEqual(text.key(status, "needs"), "[scope, plan, build, shard, aggregate]")
        self.assertEqual(text.key(status, "if"), "${{ always() }}")
        exempt = step_named(status, "Report the no-perf exemption")
        self.assertEqual(with_value(exempt, "if"), "${{ needs.scope.result == 'success' && "
                                                   "needs.scope.outputs.exempt == 'true' }}")
        guard = step_named(status, "Require a completed scope decision")
        self.assertEqual(text.steps(status)[0], guard)
        self.assertEqual(with_value(guard, "if"), "${{ always() }}")
        self.assertEqual(text.steps(status)[1], exempt)
        # Every other step is the measured path, which never runs for an exemption.
        for step in text.steps(status)[2:]:
            self.assertIn("        if: ${{ needs.scope.outputs.exempt != 'true' }}", step,
                          step[0])
        finish = text.block(step_named(status, "Finish the stable performance check"), "run")
        self.assertIn('test -n "$CONTROL_SHA"', finish)

    def test_every_action_is_pinned_and_every_checkout_keeps_no_credential(self):
        uses = re.findall(r"^\s*(?:- )?uses: (\S+)", self.text, re.M)
        self.assertTrue(uses)
        for value in uses:
            self.assertRegex(value, PIN)
        validate = read(os.path.join(ROOT, ".github/workflows/validate.yml")).decode()
        reviewed = set(re.findall(r"uses: (actions/\S+)", validate))
        self.assertTrue(set(value for value in uses if value.startswith("actions/")) <= reviewed
                        | set(re.findall(r"uses: (actions/\S+)", read(os.path.join(
                            ROOT, ".github/workflows/release.yml")).decode())))
        for name, job in self.jobs.items():
            for step in text.steps(job):
                if "actions/checkout@" in step[0]:
                    self.assertIn("          persist-credentials: false", step, name)

    def test_every_job_is_bounded_and_the_matrix_follows_the_frozen_plans(self):
        for name, job in self.jobs.items():
            self.assertIsNotNone(text.key(job, "timeout-minutes"), name)
        shard = "\n".join(self.jobs["shard"])
        self.assertIn("max-parallel: ${{ fromJSON(needs.plan.outputs.concurrent_jobs) }}", shard)
        self.assertIn("include: ${{ fromJSON(needs.plan.outputs.matrix) }}", shard)
        self.assertIn("fail-fast: false", shard)
        self.assertEqual(text.key(self.jobs["shard"], "needs"), "[plan, build]")
        self.assertEqual(text.key(self.jobs["aggregate"], "if"),
                         "${{ always() && needs.plan.outputs.any == 'true' }}")
        self.assertEqual(text.key(self.jobs["shard"], "if"),
                         "${{ !cancelled() && needs.plan.outputs.any == 'true' }}")
        self.assertEqual(text.key(self.jobs["build"], "runs-on"),
                         "${{ fromJSON(needs.plan.outputs.runner) }}")

    def test_build_once_and_shards_consume_only_the_published_build(self):
        build = "\n".join(self.jobs["build"])
        self.assertEqual(build.count(" build --plan "), 1)
        for name in ("shard", "aggregate"):
            self.assertNotIn(" build --plan ", "\n".join(self.jobs[name]),
                             "%s never rebuilds" % name)
        upload = step_named(self.jobs["build"], "Publish the hashed build")
        self.assertEqual(with_value(upload, "name"), "perf-build-${{ matrix.run_id }}")
        fetch = step_named(self.jobs["shard"], "Fetch the hashed build")
        self.assertEqual(with_value(fetch, "name"), "perf-build-${{ matrix.run_id }}")
        verify = text.block(step_named(self.jobs["shard"], "Check out the trusted control"),
                            "run")
        self.assertIn("ci-unpack", verify)

    def test_artifacts_are_unique_retained_and_never_flattened(self):
        self.assertIn("retention-days: ${{ github.event_name == 'pull_request' && 14 || 90 }}",
                      "\n".join(self.jobs["plan"]))
        for name in ("build", "shard", "aggregate"):
            for line in self.jobs[name]:
                if "retention-days:" in line:
                    self.assertIn("needs.plan.outputs.retention_days", line)
        shard_upload = step_named(self.jobs["shard"], "Upload the shard bundle")
        self.assertEqual(with_value(shard_upload, "name"),
                         "af-perf-${{ matrix.run_id }}-shard-${{ matrix.shard }}")
        self.assertEqual(with_value(shard_upload, "if"), "always()")
        download = step_named(self.jobs["aggregate"], "Fetch every shard bundle")
        self.assertEqual(with_value(download, "pattern"),
                         "af-perf-${{ matrix.run_id }}-shard-*")
        self.assertEqual(with_value(download, "merge-multiple"), "false")
        # Only numeric evidence in the collector's transport format is named af-perf-*.
        for value in re.findall(r"^\s+name: (af-perf-.*?)\s*$", self.text, re.M):
            if value.startswith("af-perf-"):
                self.assertRegex(value, r"^af-perf-\$\{\{ matrix.run_id \}\}-(shard-\$\{\{ "
                                        r"matrix.shard \}\}|run)$")

    def test_trusted_control_runs_every_script_and_credentials_are_checked_first(self):
        for line in self.text.splitlines():
            if "cli.py" in line and "python3" in line:
                self.assertIn('"$RUNNER_TEMP/control/scripts/performance/cli.py"', line)
        for job, guard, guarded in (("build", "Refuse any credential", "Build once"),
                                    ("shard", "Refuse any credential", "Run exactly this shard")):
            names = [step[0] for step in text.steps(self.jobs[job])]
            first = [index for index, line in enumerate(names) if guard in line][0]
            then = [index for index, line in enumerate(names) if guarded in line][0]
            self.assertLess(first, then)
        aggregate = text.block(step_named(self.jobs["aggregate"], "Recompute the verdict"),
                               "run")
        self.assertIn("--enforcement advisory", aggregate)
        self.assertIn('--job-summary "$GITHUB_STEP_SUMMARY"', aggregate)

    def test_a_release_claim_arrives_only_through_the_call_and_names_what_is_measured(self):
        head = self.text[:self.text.index("\njobs:")]
        self.assertIn("  workflow_call:\n    inputs:\n      release_claim:\n", head)
        self.assertIn("inputs.release_claim || github.ref }}", head,
                      "each release measurement has its own concurrency group")
        fetch = step_named(self.jobs["plan"], "Fetch the confirmed release claim")
        self.assertEqual(with_value(fetch, "if"), "${{ inputs.release_claim }}")
        self.assertEqual(with_value(fetch, "name"), "${{ inputs.release_claim }}")
        self.assertNotIn("run-id", "\n".join(fetch), "only this run's own claim is fetched")
        schedule = text.block(step_named(self.jobs["plan"], "Schedule from the trusted control"),
                              "run")
        self.assertIn('if [ -n "$RELEASE_CLAIM" ]; then set -- "$@" --release-claim '
                      '"$RUNNER_TEMP/release-claim/claim.json"; fi', schedule)
        names = [step[0] for step in text.steps(self.jobs["plan"])]
        order = [index for index, line in enumerate(names) for prefix in (
            "Resolve the trusted control", "Fetch the confirmed release claim",
            "Schedule from the trusted control") if prefix in line]
        self.assertEqual(order, sorted(order))

    def test_report_check_exposes_verdict_using_only_the_trusted_status_outputs(self):
        job = self.jobs["report"]
        self.assertEqual(text.key(job, "needs"), "[scope, status]")
        self.assertEqual(text.key(job, "if"), "${{ always() }}")
        self.assertIn("needs.status.outputs.report_label", text.key(job, "name"))
        self.assertIn("needs.scope.result != 'success'", text.key(job, "name"))
        self.assertIn("scope decision failed", text.key(job, "name"))
        self.assertIn("UNAVAILABLE", text.key(job, "name"))
        self.assertNotIn("uses:", "\n".join(job))
        show = step_named(job, "Show the performance report")
        self.assertIn("PERF_REPORT_SUMMARY: ${{ needs.status.outputs.report_summary }}", "\n".join(show))
        script = text.block(show, "run")
        self.assertNotIn("${{", script, "outputs enter shell only as environment data")
        self.assertIn('printf \'%s\\n\' "$PERF_REPORT_SUMMARY"', script)
        self.assertIn("trusted control did not publish", script)
        self.assertIn("Run summary and report artifacts", script)
        status = "\n".join(self.jobs["status"])
        self.assertIn("report_label: ${{ steps.exemption.outputs.report_label || "
                      "steps.verdict.outputs.report_label }}", status)
        self.assertIn("report_summary: ${{ steps.exemption.outputs.report_summary || "
                      "steps.verdict.outputs.report_summary }}", status)


@unittest.skipUnless(shutil.which("bash") and shutil.which("git"), "bash and git needed")
class Steps(TempDir):
    """The workflow's own shell, run as GitHub runs `run:` steps (`bash -e`)."""

    def setUp(self):
        TempDir.setUp(self)
        self.jobs = text.jobs(read(WORKFLOW).decode("utf-8"))
        self.repo = Repo(self.path("repo"))
        self.root = self.repo.commit("root", {"README": "r\n"})
        self.child = self.repo.commit("child", {"README": "c\n"})
        self.repo.tag("v1.0.0", self.child)

    def run_step(self, job, name, env, cwd=None):
        script = text.block(step_named(self.jobs[job], name), "run")
        runner_temp = self.path("runner-temp-%d" % len(os.listdir(self.tmp)))
        os.mkdir(runner_temp)
        values = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": self.repo.path,
                  "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                  "RUNNER_TEMP": runner_temp, "GITHUB_STEP_SUMMARY": self.path("summary"),
                  "GITHUB_OUTPUT": self.path("github-output"),
                  "GITHUB_RUN_ID": "50", "GITHUB_RUN_ATTEMPT": "2"}
        values.update(env)
        result = subprocess.run(["bash", "-e", "-c", script], cwd=cwd or self.repo.path,
                                env=values, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        control = os.path.join(runner_temp, "control")
        head = None
        if os.path.isdir(control):
            head = subprocess.run(["git", "-C", control, "rev-parse", "HEAD"],
                                  stdout=subprocess.PIPE).stdout.decode().strip()
        return result, head

    def test_the_control_is_the_event_base_the_first_parent_or_an_explicit_full_ref(self):
        name = "Resolve the trusted control revision"
        cases = (({"EVENT_NAME": "pull_request", "PR_BASE_SHA": self.root}, self.root),
                 ({"EVENT_NAME": "push", "EVENT_SHA": self.child}, self.root),
                 ({"EVENT_NAME": "schedule", "EVENT_SHA": self.child}, self.child),
                 ({"EVENT_NAME": "workflow_dispatch", "INPUT_BASE": "refs/tags/v1.0.0"},
                  self.child),
                 ({"EVENT_NAME": "workflow_dispatch", "INPUT_BASE": self.child,
                   "INPUT_CONTROL": self.root}, self.root))
        for env, expected in cases:
            with self.subTest(env=env):
                result, head = self.run_step("plan", name, env)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual(head, expected)
        for env, needle in (({"EVENT_NAME": "push", "EVENT_SHA": self.root}, "no first parent"),
                            ({"EVENT_NAME": "workflow_dispatch", "INPUT_BASE": "main"},
                             "not a full SHA or fully qualified ref"),
                            ({"EVENT_NAME": "workflow_dispatch", "INPUT_BASE": "f" * 40},
                             "does not resolve"),
                            ({"EVENT_NAME": "pull_request_target", "PR_BASE_SHA": self.root},
                             "is not scheduled")):
            with self.subTest(env=env):
                result, head = self.run_step("plan", name, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(needle, result.stderr.decode())
                self.assertIsNone(head)

    def test_a_dispatched_branch_control_is_read_where_the_checkout_fetched_it(self):
        # The pinned checkout's own fetch into a new checkout of main: the origin's other
        # branches are only refs/remotes/origin/* there, never local (ADR-0178).
        self.repo.git("branch", "root_control", self.root)
        self.repo.git("branch", "root_candidate", self.child)
        checkout = fetched_checkout(self.repo.path, self.path("checkout"))
        self.assertEqual(checkout.git("for-each-ref", "refs/heads/root_control"), "")
        name = "Resolve the trusted control revision"
        for env, expected in (
                ({"INPUT_BASE": self.child, "INPUT_CONTROL": "refs/heads/root_control"},
                 self.root),
                ({"INPUT_BASE": "refs/heads/root_control"}, self.root),
                ({"INPUT_BASE": self.root, "INPUT_CONTROL": "refs/heads/main"}, self.child),
                ({"INPUT_BASE": self.root, "INPUT_CONTROL": "refs/tags/v1.0.0"}, self.child),
                ({"INPUT_BASE": self.root, "INPUT_CONTROL": self.root}, self.root)):
            with self.subTest(env=env):
                result, head = self.run_step("plan", name, dict(
                    env, EVENT_NAME="workflow_dispatch"), cwd=checkout.path)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual(head, expected)
        result, head = self.run_step("plan", name, {
            "EVENT_NAME": "workflow_dispatch", "INPUT_BASE": "refs/heads/absent"},
            cwd=checkout.path)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not resolve", result.stderr.decode())
        self.assertIsNone(head)
        # A local branch that disagrees with the fetched one of its name is never guessed.
        checkout.git("branch", "root_control", self.child)
        result, head = self.run_step("plan", name, {
            "EVENT_NAME": "workflow_dispatch", "INPUT_BASE": "refs/heads/root_control"},
            cwd=checkout.path)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("is ambiguous", result.stderr.decode())
        self.assertIsNone(head)

    def test_a_release_claim_is_measured_from_the_handoffs_default_branch_sha(self):
        name = "Resolve the trusted control revision"
        for event in ("workflow_run", "workflow_dispatch"):
            with self.subTest(event=event):
                result, head = self.run_step("plan", name, {
                    "EVENT_NAME": event, "EVENT_SHA": self.child, "INPUT_BASE": self.root,
                    "RELEASE_CLAIM": "af-release-claim-" + "a" * 64})
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual(head, self.child)
        # A claim never turns another event into a release measurement, and a workflow_run
        # without one is not scheduled at all.
        for env in ({"EVENT_NAME": "pull_request", "PR_BASE_SHA": self.root,
                     "RELEASE_CLAIM": "af-release-claim-" + "a" * 64},
                    {"EVENT_NAME": "workflow_run", "EVENT_SHA": self.child}):
            with self.subTest(env=env):
                result, head = self.run_step("plan", name, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIsNone(head)

    def test_initial_installation_schedules_nothing_without_running_candidate_code(self):
        result, _ = self.run_step("plan", "Schedule from the trusted control", {
            "GITHUB_OUTPUT": self.path("outputs")})
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(read(self.path("outputs")), b"any=false\n")
        self.assertIn(b"trusted base predates installation", read(self.path("summary")))

    def test_a_job_rerun_never_runs_a_shard_of_an_earlier_attempt_again(self):
        name = "Refuse a job rerun"
        result, _ = self.run_step("shard", name, {"RUN_ID": "gh50_a2_primary"})
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        result, _ = self.run_step("shard", name, {"RUN_ID": "gh50_a1_primary"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("a rerun of the whole workflow plans a new run", result.stderr.decode())

    def test_a_calibration_run_is_named_for_its_member_before_any_job_and_plans_attempt_one(self):
        # The run name is GitHub's, from the dispatch inputs, as the run is created (ADR-0182);
        # every other event keeps its default name.
        from perflab import contracts
        workflow = read(WORKFLOW).decode("utf-8")
        head = workflow[:workflow.index("\non:\n")]
        line = [item for item in head.split("\n") if item.startswith("run-name: ")]
        self.assertEqual(line, ["run-name: ${{ github.event_name == 'workflow_dispatch' && "
                                "inputs.scope == 'calibration' && format('af-perf calibration "
                                "{0} {1}', inputs.cohort_digest, inputs.member) || '' }}"])
        self.assertEqual(contracts.DISPATCH_RUN_NAME.format(cohort_digest="{0}",
                                                            member_id="{1}"),
                         "af-perf calibration {0} {1}")
        title = "af-perf calibration {0} {1}".format("sha256:" + "a" * 64, "power_03")
        self.assertEqual(contracts.DISPATCH_TITLE.match(title).groups(),
                         ("sha256:" + "a" * 64, "power_03"))
        inputs = workflow[workflow.index("  workflow_dispatch:\n"):
                          workflow.index("  workflow_call:\n")]
        self.assertIn("      cohort_digest:\n", inputs)
        # Attempt 1 plans gh<run>_a1_primary, whose shards the guard runs on attempt 1 and
        # refuses on any rerun; the readiness original is that attempt.
        name = "Refuse a job rerun"
        result, _ = self.run_step("shard", name, {"RUN_ID": "gh13_a1_primary",
                                                  "GITHUB_RUN_ID": "13",
                                                  "GITHUB_RUN_ATTEMPT": "1"})
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        result, _ = self.run_step("shard", name, {"RUN_ID": "gh13_a1_primary",
                                                  "GITHUB_RUN_ID": "13",
                                                  "GITHUB_RUN_ATTEMPT": "2"})
        self.assertNotEqual(result.returncode, 0)

    def scope(self, event_name, event, attempt="1", claim=""):
        path = self.path("event-%d.json" % len(os.listdir(self.tmp)))
        with open(path, "w", encoding="utf-8") as stream:
            json.dump(event, stream)
        outputs = self.path("outputs")
        if os.path.exists(outputs):
            os.remove(outputs)
        result, _ = self.run_step("scope", "Read the no-perf label", {
            "EVENT_NAME": event_name, "GITHUB_EVENT_PATH": path, "GITHUB_RUN_ATTEMPT": attempt,
            "RELEASE_CLAIM": claim, "GITHUB_OUTPUT": outputs, "CONTROL_ROOT": ROOT})
        return result, (read(outputs).decode() if os.path.exists(outputs) else None)

    def test_shared_selection_controls_measurement_and_reruns(self):
        def pull(*names):
            return {"pull_request": {"labels": [{"name": n} for n in names]}}
        for labels, exempt in [((), True), (("no-perf",), True), (("test:perf",), False),
                               (("test:functional", "test:e2e"), True), (("release:full",), False),
                               (("No-Perf",), True), (("harmless; $(echo injected)",), True)]:
            for attempt in ("1", "2"):
                result, outputs = self.scope("pull_request", pull(*labels), attempt=attempt)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual(outputs, "exempt=%s\n" % str(exempt).lower())
        for labels in [("test:perf", "no-perf"), ("test:unknown",), ("no-unknown",)]:
            result, outputs = self.scope("pull_request", pull(*labels))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(outputs, (None, ""))
        for event in ("push", "schedule", "workflow_dispatch", "workflow_run"):
            result, outputs = self.scope(event, pull("no-perf"))
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(outputs, "exempt=false\n")
        result, outputs = self.scope("pull_request", pull("no-perf"), claim="af-release-claim-" + "a" * 64)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(outputs, "exempt=false\n")
        for event in ({"pull_request": {}}, {"pull_request": {"labels": [{"id": 1}]}}):
            result, outputs = self.scope("pull_request", event)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(outputs, (None, ""))

    def test_scope_failure_or_cancellation_cannot_pass_even_with_an_exemption_output(self):
        for state in ("success", "failure", "cancelled", "skipped", ""):
            for output in ("true", "false", "", "unknown"):
                with self.subTest(state=state, output=output):
                    result, _ = self.run_step("status", "Require a completed scope decision", {
                        "SCOPE_RESULT": state, "EXEMPT": output})
                    self.assertEqual(result.returncode == 0,
                                     state == "success" and output in ("true", "false"))

    def test_the_exemption_is_reported_as_one_and_a_missing_plan_never_passes(self):
        name = "Report the no-perf exemption"
        result, _ = self.run_step("status", name, {"EVENT_NAME": "pull_request",
                                                   "GITHUB_RUN_ATTEMPT": "1"})
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        summary = read(self.path("summary")).decode()
        self.assertIn("Performance check: exempt (perf class NOT RUN), not measured", summary)
        self.assertIn("no performance claim is made", summary)
        self.assertNotIn("qualified", summary)
        outputs = read(self.path("github-output")).decode()
        self.assertIn("report_label=EXEMPT (perf class NOT RUN; not measured)\n", outputs)
        self.assertIn("report_summary<<AF_PERFORMANCE_EXEMPT\n", outputs)
        self.assertIn("## Performance report: EXEMPT (perf class NOT RUN; not measured)\n", outputs)
        self.assertNotIn("PASS", outputs)
        self.assertNotIn("UNAVAILABLE", outputs)
        for env in ({"EVENT_NAME": "push", "GITHUB_RUN_ATTEMPT": "1"},):
            with self.subTest(env=env):
                result, _ = self.run_step("status", name, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(read(self.path("github-output")).decode(), outputs,
                                 "an invalid exemption must not publish outputs")
        # Without the exemption, a run whose plan never ran has no control and fails.
        result, _ = self.run_step("status", "Finish the stable performance check", {
            "CONTROL_SHA": "", "PERF_JOB_RESULTS": "{}"})
        self.assertNotEqual(result.returncode, 0)

    def test_the_report_step_preserves_explicit_exemption_and_measured_outputs(self):
        url = "https://github.com/owner/af/actions/runs/50/attempts/1"
        for summary in ("## Performance report: EXEMPT (perf class NOT RUN; not measured)\n\nNo performance claim is made.\n",
                        "## Performance report: PASS (bootstrap; primary unavailable)\n"):
            with self.subTest(summary=summary):
                path = self.path("shown-report")
                if os.path.exists(path):
                    os.unlink(path)
                result, _ = self.run_step("report", "Show the performance report", {
                    "PERF_SCOPE_RESULT": "success", "PERF_REPORT_SUMMARY": summary,
                    "PERF_RUN_URL": url, "GITHUB_STEP_SUMMARY": path})
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual(read(path).decode(), summary + "\n")

    def test_nonexempt_missing_output_and_failed_scope_are_explicitly_unavailable(self):
        url = "https://github.com/owner/af/actions/runs/50/attempts/1"
        for state in ("success", "failure", "cancelled", "skipped", ""):
            with self.subTest(state=state):
                path = self.path("fallback-report")
                if os.path.exists(path):
                    os.unlink(path)
                result, _ = self.run_step("report", "Show the performance report", {
                    "PERF_SCOPE_RESULT": state,
                    "PERF_REPORT_SUMMARY": "" if state == "success" else "EXEMPT (perf class NOT RUN; not measured)",
                    "PERF_RUN_URL": url, "GITHUB_STEP_SUMMARY": path})
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                summary = read(path).decode()
                self.assertIn("Performance report: UNAVAILABLE", summary)
                self.assertIn(url, summary)
                self.assertNotIn("EXEMPT", summary)
                self.assertNotIn("PASS", summary)
                self.assertIn("trusted control did not publish" if state == "success"
                              else "scope decision failed", summary)


ACTIVATION = os.path.join(ROOT, ".github/workflows/performance-activation.yml")


class ActivationWorkflow(TempDir):
    """The trusted activation collector (ADR-0184), read as text and its shell executed: a
    separate read-only job verifies a proposed activation with the base revision's own code.
    Passing these is not a hosted run."""

    def setUp(self):
        TempDir.setUp(self)
        self.text = read(ACTIVATION).decode("utf-8")
        self.jobs = text.jobs(self.text)

    def test_only_pull_requests_touching_the_records_run_it_with_a_scoped_read_only_token(self):
        head = self.text[:self.text.index("\njobs:")]
        self.assertIn("  pull_request:\n    branches: [main]\n    paths:\n"
                      "      - \"fixtures/performance/**\"\n", head)
        for forbidden in ("pull_request_target", "workflow_run", "secrets.", "id-token",
                          ": write", "write-all", "read-all", "github-script", "GITHUB_TOKEN",
                          "cargo ", "make ", "docker", "upload-artifact"):
            self.assertNotIn(forbidden, self.text)
        self.assertIn("\npermissions:\n  contents: read\n\n", self.text)
        self.assertEqual(sorted(self.jobs), ["verify"])
        job = self.jobs["verify"]
        self.assertIn("    permissions:\n      contents: read\n      actions: read\n",
                      "\n".join(job) + "\n")
        self.assertIsNotNone(text.key(job, "timeout-minutes"))
        # The token reaches the verification step alone, under the name it is read by.
        self.assertEqual(self.text.count("${{ github.token }}"), 1)
        verify = step_named(job, "Verify the proposed activation")
        self.assertEqual(text.env(verify)["AF_PERF_ACTIVATION_TOKEN"], "${{ github.token }}")
        for step in text.steps(job):
            if step != verify:
                self.assertNotIn("token", "\n".join(step).lower())

    def test_the_base_revisions_code_verifies_and_no_candidate_code_runs(self):
        job = self.jobs["verify"]
        for value in re.findall(r"^\s*(?:- )?uses: (\S+)", self.text, re.M):
            self.assertRegex(value, PIN)
        checkout = [step for step in text.steps(job) if "actions/checkout@" in step[0]]
        self.assertEqual(len(checkout), 1)
        self.assertIn("          persist-credentials: false", checkout[0])
        script = text.block(step_named(job, "Verify the proposed activation"), "run")
        self.assertIn('python3 -I "$RUNNER_TEMP/control/scripts/performance/cli.py" activate '
                      '--verify-proposed', script)
        for needle in ('--base "$PR_BASE_SHA"', '--revision "$GITHUB_SHA"',
                       '--repository "$GITHUB_REPOSITORY"',
                       "--token-env AF_PERF_ACTIVATION_TOKEN"):
            self.assertIn(needle, script)
        self.assertEqual(set(re.findall(r"\S*scripts/performance/cli\.py", self.text)),
                         {'"$RUNNER_TEMP/control/scripts/performance/cli.py'})
        # The measuring workflow keeps its own permissions: no Actions read, no token.
        measuring = read(WORKFLOW).decode("utf-8")
        self.assertNotIn("actions: read", measuring)
        self.assertNotIn("github.token", measuring)

    def run_steps(self, repo, env):
        runner_temp = self.path("runner-temp-%d" % len(os.listdir(self.tmp)))
        os.mkdir(runner_temp)
        values = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": repo.path,
                  "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                  "RUNNER_TEMP": runner_temp, "GITHUB_STEP_SUMMARY": self.path("summary"),
                  "GITHUB_REPOSITORY": "afactory/af", "PYTHONDONTWRITEBYTECODE": "1"}
        values.update(env)
        results = []
        for name in ("Check out the trusted control", "Verify the proposed activation"):
            script = text.block(step_named(self.jobs["verify"], name), "run")
            results.append(subprocess.run(["bash", "-e", "-c", script], cwd=repo.path,
                                          env=values, stdout=subprocess.PIPE,
                                          stderr=subprocess.PIPE))
            if results[-1].returncode != 0:
                break
        return results

    def test_the_steps_select_trust_from_the_base_and_fetch_only_a_changed_activation(self):
        # A base holding the laboratory's own code, and proposals on top of it; the steps run
        # the base's copy, whatever the proposal's scripts say.
        repo = Repo(self.path("repo"))
        lab = os.path.join(ROOT, "scripts", "performance")
        files = {}
        for current, dirs, names in os.walk(lab):
            dirs[:] = [name for name in dirs if name != "__pycache__"]
            for name in names:
                path = os.path.join(current, name)
                files[os.path.relpath(path, ROOT)] = read(path)
        base = repo.commit("base", files)
        hostile = repo.commit("proposal", {
            "scripts/performance/cli.py": b"raise SystemExit('candidate code ran')\n",
            "README": b"an ordinary change\n"})
        results = self.run_steps(repo, {"PR_BASE_SHA": base, "GITHUB_SHA": hostile})
        self.assertEqual([result.returncode for result in results], [0, 0],
                         [result.stderr.decode() for result in results])
        self.assertIn("no activation", results[1].stderr.decode())
        # A proposal that adds an activation needs the retained evidence: without the token
        # nothing is fetched and the activation is refused.
        activating = repo.commit("activate", {
            "fixtures/performance/activation.toml": b'schema = "af.perf-activation/3"\n'})
        results = self.run_steps(repo, {"PR_BASE_SHA": base, "GITHUB_SHA": activating})
        self.assertEqual(results[0].returncode, 0)
        self.assertEqual(results[1].returncode, 2)
        self.assertIn("activation refused", results[1].stderr.decode())
        # The base must be a full SHA of the event.
        results = self.run_steps(repo, {"PR_BASE_SHA": "main", "GITHUB_SHA": activating})
        self.assertNotEqual(results[0].returncode, 0)
        self.assertIn("not a full commit SHA", results[0].stderr.decode())
