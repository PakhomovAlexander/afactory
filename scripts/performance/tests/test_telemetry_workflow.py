"""The Performance telemetry workflow's trust limits (ADR-0214), read as text, and the event
resolution it runs from its trusted control, against disposable Git repositories.

Passing these is not a hosted run: no workflow here has executed on GitHub.
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import unittest

from perflab import gitrefs, telemetryexec, telemetrylab
from perflab.values import ContractError
from tests import telemetry_fixtures as fx
from tests import test_workflow as text
from tests.support import ROOT, Repo, TempDir, read

CLI = os.path.join(ROOT, "scripts", "performance", "cli.py")

WORKFLOW = os.path.join(ROOT, ".github/workflows/performance-telemetry.yml")
PIN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$")
# The comparison workflow and the hosted cohort it calibrates, frozen for that cohort
# (docs/performance-hosted-cohort-20261023.md). The current producer changed for the
# dispatch-head repair; only the historical cohort bytes are frozen.
FROZEN = {"fixtures/performance/cohort-quick-20261023-hosted-v6.json":
          "62955096a58517dad3544425ead9b18ebe3b85de94b8518d5ce85deb761d5ab7"}


class Workflow(unittest.TestCase):
    def setUp(self):
        self.text = read(WORKFLOW).decode("utf-8")
        self.jobs = text.jobs(self.text)

    def test_validation_builds_real_release_af_and_runs_the_contained_setup_regression(self):
        jobs = text.jobs(read(os.path.join(ROOT, '.github/workflows/validate.yml')).decode())
        job = jobs['telemetry-runtime-smoke']
        self.assertIsNone(text.key(job, 'if'))
        self.assertEqual(text.key(job, 'runs-on'), 'ubuntu-latest')
        body = '\n'.join(job)
        self.assertIn('persist-credentials: false', body)
        self.assertIn('telemetry-contained-build', body)
        self.assertIn('--source-sha "$GITHUB_SHA"', body)
        self.assertIn('telemetry_runtime_smoke.py', body)
        self.assertIn('--af "$RUNNER_TEMP/telemetry-runtime-build/af"', body)
        self.assertNotIn('--native', body)
        self.assertNotIn('continue-on-error', body)
        self.assertNotIn('secrets.', body)
        # Preserve the existing live hostile-write/allocation/diagnostic coverage too.
        probes = '\n'.join(jobs['telemetry-build-smoke'])
        self.assertIn('telemetry_build_smoke.py', probes)
        self.assertIn('telemetry_measurement_smoke.py', probes)

    def test_the_frozen_cohort_is_byte_identical_and_telemetry_stays_separate(self):
        for relative, digest in FROZEN.items():
            self.assertEqual(hashlib.sha256(read(os.path.join(ROOT, relative))).hexdigest(),
                             digest, relative)
        self.assertNotIn("telemetry", read(os.path.join(
            ROOT, ".github/workflows/performance.yml")).decode())

    def test_events_permissions_and_no_secrets(self):
        head = self.text[:self.text.index("\njobs:")]
        self.assertIn("  pull_request:\n    branches: [main]\n", head)
        self.assertIn("  schedule:\n", head)
        self.assertIn("  release:\n    types: [published]\n", head)
        self.assertIn("  workflow_dispatch:\n", head)
        for forbidden in ("pull_request_target", "workflow_run", "secrets.", "id-token",
                          "write", "github-script", "GITHUB_TOKEN", "launchd", "crontab"):
            self.assertNotIn(forbidden, self.text)
        self.assertIn("\npermissions:\n  contents: read\n\n", self.text)
        self.assertEqual(self.text.count("permissions:"), 1)

    def test_every_action_is_pinned_reviewed_and_every_checkout_keeps_no_credential(self):
        uses = re.findall(r"^\s*(?:- )?uses: (\S+)", self.text, re.M)
        self.assertTrue(uses)
        reviewed = set(re.findall(r"uses: (actions/\S+)", read(os.path.join(
            ROOT, ".github/workflows/performance.yml")).decode()))
        for value in uses:
            self.assertRegex(value, PIN)
            self.assertIn(value, reviewed)
        for name, job in self.jobs.items():
            self.assertIsNotNone(text.key(job, "timeout-minutes"), name)
            self.assertEqual(text.key(job, "runs-on"), "ubuntu-latest", name)
            for step in text.steps(job):
                if "actions/checkout@" in step[0]:
                    self.assertIn("          persist-credentials: false", step, name)

    def test_candidate_code_is_only_built_apart_and_measured_contained(self):
        build, measure = "\n".join(self.jobs["build"]), "\n".join(self.jobs["measure"])
        self.assertIn("cargo build --release --locked --offline --bin af", build)
        self.assertLess(build.index("ci-credentials --git-dir .git --environment"),
                        build.index("cargo build"))
        self.assertIn('telemetry-contained-build', build, "use the shared verified build boundary")
        helper = read(os.path.join(ROOT, "scripts/performance/perflab/telemetrybuild.py")).decode()
        self.assertIn("builds.BuildImage", helper)
        self.assertIn("builds.ContainerToolchain", helper)
        self.assertIn('(source, "/src", False)', helper)
        self.assertIn('(state, "/state", True)', helper)
        self.assertIn("telemetry-build-record", build)
        self.assertNotIn("cargo", measure.replace("af.build.json", ""),
                         "no build script runs where the control measures")
        self.assertEqual(text.key(self.jobs["measure"], "needs"), "[plan, build]")
        self.assertLess(measure.index("sha256sum"), measure.index("telemetry-run"))
        self.assertLess(measure.index("ci-credentials --git-dir .git --environment"),
                        measure.index("telemetry-run"))
        self.assertLess(measure.index("ci-scratch"), measure.index("telemetry-run"))
        # Every execution of a pull request's candidate is contained; nothing measures it on
        # the runner itself.
        self.assertIn("--mode linux_container", measure)
        self.assertIn('--image "$IMAGE" --scratch-fs "$RUNNER_TEMP/scratch-fs"', measure)
        self.assertNotIn("trusted_local", self.text)
        self.assertNotIn("--work", measure)
        for job in ("plan", "build", "measure"):
            for line in self.jobs[job]:
                if "scripts/performance/cli.py" in line:
                    self.assertIn('"$RUNNER_TEMP/control/scripts/performance/cli.py"', line,
                                  "%s runs only the trusted control's laboratory" % job)
        self.assertIn("--authority-sha \"$CONTROL\"", measure)
        self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
                      self.text)

    def test_nothing_is_planned_without_a_sample_image_pinned_by_digest(self):
        plan = "\n".join(self.jobs["plan"])
        self.assertIn("IMAGE: ${{ vars.AF_PERF_GNU_BUILD_IMAGE }}", plan)
        self.assertIn("grep -Eq '^[^[:space:]@]+@sha256:[0-9a-f]{64}$'", plan)
        self.assertLess(plan.index("nothing is measured uncontained"),
                        plan.index("telemetry-schedule"))

    def test_only_an_admitted_result_is_uploaded(self):
        steps = text.steps(self.jobs["measure"])
        names = ["\n".join(step) for step in steps]
        admit = [index for index, step in enumerate(names) if "id: admit" in step]
        upload = [index for index, step in enumerate(names)
                  if "name: perf-telemetry-${{ github.run_id }}" in step]
        measure = [index for index, step in enumerate(names) if "telemetry-run" in step]
        self.assertEqual((len(admit), len(upload), len(measure)), (1, 1, 1))
        self.assertLess(measure[0], admit[0])
        self.assertLess(admit[0], upload[0])
        self.assertIn("telemetry-admit", names[admit[0]])
        self.assertIn("if: ${{ !cancelled() }}", names[admit[0]])
        self.assertIn('--github-output "$GITHUB_OUTPUT"', names[admit[0]])
        self.assertIn("if: ${{ !cancelled() && steps.admit.outputs.admitted == 'true' }}",
                      names[upload[0]])
        self.assertIn("if-no-files-found: error", names[upload[0]])
        self.assertIn("retention-days: ${{ fromJSON(needs.plan.outputs.retention_days) }}",
                      names[upload[0]])
        # Nothing but the scratch's removal runs unconditionally.
        unconditional = [step for step in names if "always()" in step]
        self.assertEqual(len(unconditional), 1)
        self.assertIn("ci-scratch --cleanup", unconditional[0])

    def test_original_plan_is_retained_and_each_measurement_attempt_has_its_own_binding(self):
        plan = "\n".join(self.jobs["plan"])
        measure = "\n".join(self.jobs["measure"])
        self.assertIn('--binding "$RUNNER_TEMP/binding.json"', plan)
        self.assertIn("binding: ${{ steps.schedule.outputs.binding }}", plan)
        self.assertIn("name: telemetry-schedule-${{ github.run_id }}-${{ github.run_attempt }}", plan)
        self.assertIn("PLAN_BINDING: ${{ needs.plan.outputs.binding }}", measure)
        self.assertIn("telemetry-bind-attempt", measure)
        self.assertIn("name: telemetry-plan-${{ github.run_id }}-${{ github.run_attempt }}", measure)
        build = "\n".join(self.jobs["build"])
        for side in ("baseline", "candidate"):
            self.assertIn(side + "_artifact: ${{ steps.record.outputs." + side + "_artifact }}", build)
            self.assertIn("name: ${{ needs.build.outputs." + side + "_artifact }}", measure)
        self.assertIn('echo "${SIDE}_artifact=perf-telemetry-af-${SIDE}-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"', build)
        self.assertLess(measure.index("name: telemetry-plan-"), measure.index("telemetry-run"))
        self.assertIn("ADR-0214, ADR-0215", self.text)
        self.assertNotIn("ADR-0199", self.text)
        self.assertIn("historical cohort bytes remain fixed", self.text)
        validation = read(os.path.join(ROOT, ".github/workflows/validate.yml")).decode()
        self.assertIn("telemetry_build_smoke.py", validation)
        self.assertIn("telemetry-build-smoke:", validation)

    def test_the_control_resolution_is_the_base_for_a_pull_request(self):
        step = [item for item in text.steps(self.jobs["plan"])
                if "Resolve the trusted control revision" in "\n".join(item)][0]
        script = text.block(step, "run")
        self.assertIn('pull_request) control="$PR_BASE_SHA" ;;', script)
        self.assertIn('workflow_dispatch) control="${INPUT_CONTROL:-$INPUT_BASE}" ;;', script)


def commit_lab(repo, message, extra=None):
    files = {telemetrylab.REGISTRY_PATH: read(os.path.join(ROOT, telemetrylab.REGISTRY_PATH))}
    files.update(extra or {})
    return repo.commit(message, files)


class Events(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.repo = Repo(self.path("repo"))
        self.first = self.repo.commit("before the laboratory", {"README": b"x\n"})
        self.old = commit_lab(self.repo, "v1.0.0")
        self.repo.tag("v1.0.0", self.old)
        self.tip = self.repo.commit("work", {"src": b"y\n"})
        self.git = gitrefs.Git(self.repo.path)

    def test_a_pull_request_is_measured_against_its_base_with_its_base_as_control(self):
        event = {"pull_request": {"head": {"ref": "feature"}, "base": {"sha": self.old}}}
        doc = telemetrylab.schedule(self.git, "pull_request", event, {}, self.tip)
        self.assertEqual((doc["state"], doc["profile"], doc["control_sha"], doc["baseline_sha"],
                          doc["candidate_sha"]), ("planned", "pr", self.old, self.old, self.tip))
        outputs = dict(line.split("=", 1) for line in
                       telemetrylab.schedule_outputs(doc).splitlines())
        self.assertEqual(outputs["retention_days"], "14")
        self.assertEqual(json.loads(outputs["args"]), [])

    def test_a_gate_branch_or_a_control_before_the_laboratory_measures_nothing(self):
        gate = {"pull_request": {"head": {"ref": "af-gate/task/head"}, "base": {"sha": self.old}}}
        self.assertEqual(telemetrylab.schedule(self.git, "pull_request", gate, {},
                                               self.tip)["state"], "unavailable")
        early = {"pull_request": {"head": {"ref": "x"}, "base": {"sha": self.first}}}
        doc = telemetrylab.schedule(self.git, "pull_request", early, {}, self.tip)
        self.assertEqual(doc["state"], "unavailable")
        self.assertIn("predates", doc["reason"])
        self.assertEqual(telemetrylab.schedule_outputs(doc), "planned=false\n")

    def test_the_nightly_and_a_release_measure_against_the_previous_stable_release(self):
        doc = telemetrylab.schedule(self.git, "schedule", {}, {}, self.tip)
        self.assertEqual((doc["profile"], doc["baseline_sha"], doc["candidate_sha"]),
                         ("nightly", self.old, self.tip))
        new = commit_lab(self.repo, "v1.1.0", {"src": b"z\n"})
        self.repo.tag("v1.1.0", new)
        doc = telemetrylab.schedule(self.git, "release", {"release": {"tag_name": "v1.1.0"}}, {},
                                    new)
        self.assertEqual((doc["profile"], doc["control_sha"], doc["baseline_sha"],
                          doc["candidate_sha"]), ("release", new, self.old, new))
        with self.assertRaises(ContractError):
            telemetrylab.schedule(self.git, "release", {"release": {"tag_name": "v1.1.0"}}, {},
                                  self.tip)

    def test_a_dispatch_names_full_revisions_and_bounded_overrides_only(self):
        inputs = {"base": self.old, "head": self.tip, "profile": "manual",
                  "experiments": "overhead read_latency", "rounds": "3", "pairs": "5"}
        doc = telemetrylab.schedule(self.git, "workflow_dispatch", {}, inputs, self.tip)
        self.assertEqual((doc["control_sha"], doc["experiments"], doc["rounds"], doc["pairs"]),
                         (self.old, ["overhead", "read_latency"], 3, 5))
        outputs = dict(line.split("=", 1) for line in
                       telemetrylab.schedule_outputs(doc).splitlines())
        self.assertEqual(json.loads(outputs["args"]), ["--experiment", "overhead",
                                                       "--experiment", "read_latency",
                                                       "--rounds", "3", "--pairs", "5"])
        for bad in ({"base": "HEAD~1"}, {"head": "main"}, {"experiments": "rm -rf"},
                    {"rounds": "9"}, {"pairs": "41"}, {"profile": "turbo"},
                    {"rounds": "$(id)"}):
            with self.subTest(bad=bad):
                with self.assertRaises(ContractError):
                    telemetrylab.schedule(self.git, "workflow_dispatch", {},
                                          dict(inputs, **bad), self.tip)
        with self.assertRaises(ContractError):
            telemetrylab.schedule(self.git, "push", {}, {}, self.tip)


class AttemptBinding(TempDir):
    def test_rerun_preserves_frozen_sources_and_original_event_without_resolving_refs(self):
        event_path = self.path("event.json")
        head, merge, base = "a" * 40, "b" * 40, "c" * 40
        with open(event_path, "w") as stream:
            json.dump({"pull_request": {"head": {"sha": head}, "base": {"sha": base}}}, stream)
        binding = {"schema": "af.perf-telemetry-binding/1", "repository": "example/repo",
                   "run_id": 50, "attempt": 1, "event": "pull_request", "event_sha": merge,
                   "run_head_sha": head, "candidate_sha": merge, "baseline_sha": base,
                   "control_sha": base, "profile": "pr"}
        env = dict(os.environ, PLAN_BINDING=json.dumps(binding), GITHUB_EVENT_PATH=event_path,
                   EVENT_NAME="pull_request", GITHUB_REPOSITORY="example/repo",
                   GITHUB_RUN_ID="50", GITHUB_RUN_ATTEMPT="2", GITHUB_SHA=merge)
        output = self.path("binding.json")
        command = [sys.executable, "-I", "-B", CLI, "telemetry-bind-attempt", "--output", output]
        process = subprocess.run(command, env=env, capture_output=True, timeout=20)
        self.assertEqual(process.returncode, 0, process.stderr)
        from perflab import contracts
        self.assertEqual(contracts.read_file(output, "telemetry-binding"), dict(binding, attempt=2))
        for mutation in ({"GITHUB_RUN_ID": "51"}, {"GITHUB_SHA": head},
                         {"GITHUB_RUN_ATTEMPT": "0"}, {"GITHUB_REPOSITORY": "foreign/repo"}):
            with self.subTest(mutation=mutation):
                process = subprocess.run(command, env=dict(env, **mutation), capture_output=True,
                                         timeout=20)
                self.assertNotEqual(process.returncode, 0)
        for schema in ("af.perf-telemetry-binding/0", "af.perf-telemetry-binding/2"):
            with self.assertRaises(ContractError):
                contracts.validate_telemetry_binding(dict(binding, schema=schema))


class Admission(TempDir):
    """The admission step's four paths, through the command the workflow runs: only a verified,
    credential-free result is admitted, and the step fails visibly otherwise."""

    def setUp(self):
        TempDir.setUp(self)
        self.binaries = fx.binaries(self)

    def admit(self, result):
        output = self.path("github-output-%d" % len(os.listdir(self.tmp)))
        summary = output + ".md"
        process = subprocess.run([sys.executable, "-I", "-B", CLI, "telemetry-admit",
                                  "--result", result, "--github-output", output,
                                  "--job-summary", summary],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300)
        outputs = dict(line.split("=", 1) for line in read(output).decode().splitlines())
        return process.returncode, outputs, read(summary).decode()

    def test_missing_refused_credential_and_partial_results(self):
        code, outputs, summary = self.admit(self.path("absent"))
        self.assertEqual((code, outputs), (4, {"admitted": "false",
                                               "outcome": "missing_result"}))
        self.assertIn("not uploaded", summary)
        failing = fx.StandIn(self.binaries, {("overhead", "candidate"): lambda execution:
                                             telemetryexec.Outcome(1, b"", 0, b"", 0, 10)})
        telemetrylab.run(fx.settings(self, failing, "partial", experiments=["overhead"]))
        code, outputs, summary = self.admit(self.path("partial"))
        self.assertEqual((code, outputs), (0, {"admitted": "true", "outcome": "admitted"}))
        self.assertIn("partial", summary)
        with open(self.path("partial", "raw", "overhead", "round-0-baseline.json"), "ab") as s:
            s.write(b" ")
        code, outputs, summary = self.admit(self.path("partial"))
        self.assertEqual((code, outputs), (4, {"admitted": "false", "outcome": "refused"}))
        token = b"ghp_" + b"B" * 36

        def leaky(execution):
            data = json.dumps(fx.overhead_doc(20261021, 2)).encode()
            return telemetryexec.Outcome(0, data, len(data), b"token " + token, 40, 10)
        telemetrylab.run(fx.settings(self, fx.StandIn(self.binaries, {
            ("overhead", "baseline", 20261021): leaky}), "leaky", experiments=["overhead"]))
        code, outputs, summary = self.admit(self.path("leaky"))
        self.assertEqual((code, outputs), (4, {"admitted": "false",
                                               "outcome": "credential_found"}))
        self.assertNotIn(token.decode(), summary)
