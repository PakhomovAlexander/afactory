"""The release-performance handoff workflow's contract (ADR-0180), read as text.

`.github/workflows/performance-release.yml` is read line by line with the helpers of
tests.test_workflow, and its branch guard's own shell is executed. Passing these tests is not a
hosted run: no handoff has run on GitHub, and the staging demonstration of
docs/performance-ci.md stays pending until an authorized run.
"""
import os
import re
import shutil
import subprocess
import unittest

from perflab import release
from tests import test_workflow as text
from tests.support import ROOT, TempDir, read
from tests.test_performance_workflow import PIN, step_named, with_value

WORKFLOW = os.path.join(ROOT, ".github/workflows/performance-release.yml")
UPSTREAM = os.path.join(ROOT, release.RELEASE_WORKFLOW_PATH)


class ReleaseWorkflow(unittest.TestCase):
    def setUp(self):
        self.text = read(WORKFLOW).decode("utf-8")
        self.jobs = text.jobs(self.text)

    def test_it_runs_only_for_completed_release_runs_and_explicit_remeasurement(self):
        head = self.text[:self.text.index("\njobs:")]
        on = head[head.index("\non:\n"):head.index("\npermissions:")]
        self.assertIn("  workflow_run:\n    workflows: [Release]\n    types: [completed]\n", on)
        self.assertIn("  workflow_dispatch:\n", on)
        self.assertEqual(re.findall(r"^  ([a-z_]+):", on, re.M),
                         ["workflow_run", "workflow_dispatch"])
        self.assertEqual(re.findall(r"^      ([a-z_]+):", on, re.M),
                         ["release_run", "remeasure_of"])
        self.assertIn("group: performance-release-${{ github.event.workflow_run.id || "
                      "github.event.inputs.release_run }}\n  cancel-in-progress: false", head)

    def test_the_upstream_identity_it_binds_is_release_yml_and_its_job_names(self):
        upstream = read(UPSTREAM).decode("utf-8")
        self.assertIn("\nname: Release\n", upstream)
        jobs = text.jobs(upstream)
        self.assertEqual(text.key(jobs["publish"], "name"), release.PUBLISH_JOB)
        self.assertEqual(text.key(jobs["pin"], "name"), release.PIN_JOB)
        self.assertIn("\n    branches: [main]\n", upstream)
        # The tags release.yml publishes are exactly the ones the handoff accepts.
        for tag in ("v1.2.3", "v0.12.0-rc.3"):
            self.assertIsNotNone(release.RELEASE_TAG.match(tag))
        for tag in ("v1.2", "1.2.3", "v1.2.3-beta.1", "v01.2.3"):
            self.assertIsNone(release.RELEASE_TAG.match(tag))

    def test_read_only_token_in_the_handoff_alone_and_none_in_the_measurement(self):
        self.assertIn("\npermissions:\n  contents: read\n\n", self.text)
        handoff = "\n".join(self.jobs["handoff"])
        self.assertIn("    permissions:\n      contents: read\n      actions: read\n", handoff)
        measure = "\n".join(self.jobs["measure"])
        self.assertIn("    permissions:\n      contents: read\n", measure)
        self.assertNotIn("actions: read", measure)
        for forbidden in (": write", "secrets.", "id-token", "GITHUB_TOKEN", "github-script",
                          "pull_request"):
            self.assertNotIn(forbidden, self.text)
        with_token = [step[0] for step in text.steps(self.jobs["handoff"])
                      if any("github.token" in line for line in step)]
        self.assertEqual(len(with_token), 2)
        for name in ("Verify the upstream release", "Confirm the first valid claim"):
            step = step_named(self.jobs["handoff"], name)
            self.assertEqual(with_value(step, "AF_PERF_RELEASE_TOKEN"), "${{ github.token }}")
        self.assertEqual(self.text.count("${{ github.token }}"), 2)

    def test_nothing_upstream_is_downloaded_or_executed(self):
        self.assertNotIn("download-artifact", self.text)
        self.assertNotRegex(self.text, r"\brun-id:")
        self.assertNotIn("workflow_run.artifacts_url", self.text)
        uses = re.findall(r"^\s*(?:- )?uses: (\S+)", self.text, re.M)
        for value in uses:
            if value.startswith("./"):
                self.assertEqual(value, "./.github/workflows/performance.yml")
            else:
                self.assertRegex(value, PIN)
        for step in text.steps(self.jobs["handoff"]):
            if "actions/checkout@" in step[0]:
                self.assertIn("          persist-credentials: false", step)
                self.assertEqual(with_value(step, "ref"), "${{ github.sha }}")
        for line in self.text.splitlines():
            if "cli.py" in line and "python3" in line:
                self.assertIn("python3 scripts/performance/cli.py ci-release-", line)

    def test_the_claim_is_written_before_it_is_confirmed_and_only_confirmed_runs_measure(self):
        names = [step[0] for step in text.steps(self.jobs["handoff"])]
        order = [[index for index, line in enumerate(names) if prefix in line][0]
                 for prefix in ("Refuse anything but the default branch",
                                "actions/checkout@", "Verify the upstream release",
                                "Write the immutable claim", "Confirm the first valid claim",
                                "Retain the handoff decision")]
        self.assertEqual(order, sorted(order))
        upload = step_named(self.jobs["handoff"], "Write the immutable claim")
        self.assertEqual(with_value(upload, "if"), "steps.handoff.outputs.claim == 'true'")
        self.assertEqual(with_value(upload, "name"),
                         "${{ steps.handoff.outputs.claim_artifact }}")
        self.assertEqual(with_value(upload, "retention-days"),
                         str(release.CLAIM_RETENTION_DAYS))
        self.assertEqual(with_value(upload, "if-no-files-found"), "error")
        confirm = step_named(self.jobs["handoff"], "Confirm the first valid claim")
        self.assertEqual(with_value(confirm, "if"), "steps.handoff.outputs.claim == 'true'")
        retain = step_named(self.jobs["handoff"], "Retain the handoff decision")
        self.assertEqual(with_value(retain, "if"), "always()")
        self.assertIsNotNone(text.key(self.jobs["handoff"], "timeout-minutes"))
        self.assertIn("      measure: ${{ steps.confirm.outputs.measure }}",
                      self.jobs["handoff"])
        self.assertEqual(text.key(self.jobs["measure"], "needs"), "handoff")
        self.assertEqual(text.key(self.jobs["measure"], "if"),
                         "needs.handoff.outputs.measure == 'true'")
        self.assertEqual(text.key(self.jobs["measure"], "uses"),
                         "./.github/workflows/performance.yml")
        self.assertIn("      release_claim: ${{ needs.handoff.outputs.claim_artifact }}",
                      self.jobs["measure"])


@unittest.skipUnless(shutil.which("bash"), "bash needed")
class Steps(TempDir):
    def test_only_the_default_branch_runs_the_handoff(self):
        script = text.block(step_named(text.jobs(read(WORKFLOW).decode("utf-8"))["handoff"],
                                       "Refuse anything but the default branch"), "run")
        for ref, accepted in (("refs/heads/main", True), ("refs/heads/topic", False),
                              ("refs/tags/v1.0.0", False), ("", False)):
            with self.subTest(ref=ref):
                result = subprocess.run(["bash", "-e", "-c", script], cwd=self.tmp,
                                        env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                                             "REF": ref},
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode == 0, accepted, result.stderr.decode())
