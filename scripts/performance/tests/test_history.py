"""Compatible history across runs (package p4, ADR-0172).

`ActualRunnerHistory` groups real shard bundles of the actual runner, in trusted local mode
with the stand-in binaries and fake supervisor of tests.test_runner (never product code, so no
number describes AF): two runs of one declared workload with different run IDs, sources and
independently written seed artifacts, and a run whose declared fixtures changed. `Grouping`
uses current-format synthetic runs, labeled synthetic.
"""
import json
import os

from perflab import archive, contracts, history, runner, synthetic
from perflab.values import ContractError, sha256_bytes
from tests import test_runner
from tests.support import TempDir, read, run_dir, synthetic_suite
from tests.test_report import assert_safe_page


class ActualRunnerHistory(test_runner.SeedHarness):
    def shard(self, run_id, base, head, fixtures=b"fixtures"):
        """One complete shard of the native cold projection, its seed written by each side."""
        plan = test_runner.plan_for(self.repo, base, head, [test_runner.COLD], run_id=run_id,
                                    environment="linux_x86_64_musl_hosted")
        plan["profile"] = test_runner.plan_for(self.repo, base, head, [test_runner.DISCOVERY],
                                               run_id=run_id)["profile"]
        for case in plan["cases"]:
            case["requires_linux_containment"] = False
        # The authority's declared fixture definitions, which a fixture change replaces.
        plan["authority"]["fixture_digest"] = sha256_bytes(fixtures)
        plan["plan_digest"] = contracts.plan_digest(plan)
        plan_path = self.path(run_id + ".json")
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        build = test_runner.make_build(self.path(run_id + "-build"), plan,
                                       {"baseline": test_runner.VERSION,
                                        "candidate": test_runner.VERSION})
        run = self.path("run-" + run_id)
        doc = runner.run_shard(self.options(plan_path, build, run_dir=run))
        self.assertEqual(doc["status"], "complete", doc["failure"])
        return run

    def test_one_declared_workload_groups_across_runs_and_a_changed_fixture_splits(self):
        first = self.shard("history_first", self.base, self.head)
        base2 = self.repo.commit("base 2", {"README": "product v3\n"})
        head2 = self.repo.commit("head 2", {"README": "product v4\n"})
        second = self.shard("history_second", base2, head2)
        changed = self.shard("history_changed", self.base, self.head,
                             fixtures=b"fixtures with a changed declared definition")
        # The archive's rebuilt copy of the first run is the same run, shown once.
        root = self.path("archive")
        archive.Archive.create(root)
        with archive.Archive(root) as store:
            self.assertEqual(store.import_run(first), ("imported", "history_first"))
        output = self.path("history")
        doc = history.write([first, second, changed, root], output)
        self.assertEqual(sorted(run["run_id"] for run in doc["runs"]),
                         ["history_changed", "history_first", "history_second"])
        self.assertEqual(doc["unavailable"], [])
        groups = [group for group in doc["groups"]
                  if group["identity"]["instance_id"] == test_runner.COLD]
        self.assertEqual(len(groups), 2)
        together = [group for group in groups if len(group["points"]) == 2][0]
        apart = [group for group in groups if len(group["points"]) == 1][0]
        self.assertEqual(sorted(point["run_id"] for point in together["points"]),
                         ["history_first", "history_second"])
        self.assertEqual([point["run_id"] for point in apart["points"]], ["history_changed"])
        self.assertEqual(together["related"], [{"group_id": apart["group_id"],
                                                "differs": ["fixture"]}])
        self.assertEqual(together["identity"]["provenance"], "measured")
        # Each run's sources and seed artifacts stay visible as provenance.
        points = dict((point["run_id"], point) for point in together["points"])
        self.assertNotEqual(points["history_first"]["candidate_sha"],
                            points["history_second"]["candidate_sha"])
        seeds = dict((run_id, dict((seed["side"], seed["digest"]) for seed in point["seeds"]))
                     for run_id, point in points.items())
        self.assertEqual(sorted(seeds["history_first"]), ["baseline", "candidate"])
        self.assertNotEqual(seeds["history_first"]["baseline"],
                            seeds["history_second"]["baseline"])
        self.assertNotEqual(seeds["history_first"]["baseline"],
                            seeds["history_first"]["candidate"])
        for point in together["points"]:
            self.assertEqual(point["missing_records"], 0)
            self.assertIsNotNone(point["interval"])
        page = read(os.path.join(output, history.HISTORY_HTML)).decode()
        self.assertIn("points are not paired with each other", page)
        self.assertIn("differs in fixture", page)
        assert_safe_page(self, page, output)
        for run in doc["runs"]:
            report_page = read(os.path.join(output, run["report"])).decode()
            assert_safe_page(self, report_page, os.path.dirname(os.path.join(output,
                                                                             run["report"])))
            self.assertIn("on one host", report_page)
        self.assertEqual(json.loads(read(os.path.join(output, history.HISTORY_JSON))), doc)


class Grouping(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.suite = synthetic_suite()

    def make_run(self, run_id, **options):
        provenance = options.pop("provenance", "synthetic")
        plan = synthetic.plan(self.suite, run_id=run_id, **options)
        return run_dir(self, plan, synthetic.constant("10.5"), provenance=provenance)

    def groups(self, doc, instance_id):
        return [group for group in doc["groups"]
                if group["identity"]["instance_id"] == instance_id]

    def test_policy_and_provenance_changes_split_and_sources_do_not(self):
        inputs = [self.make_run("same_a"), self.make_run("same_b", candidate="elsewhere"),
                  self.make_run("policy_changed", pairs=14),
                  self.make_run("measured_evidence", provenance="measured"),
                  self.make_run("fixture_changed", fixtures="other fixtures")]
        doc = history.write(inputs, self.path("history"))
        groups = self.groups(doc, "synthetic_fast")
        self.assertEqual(len(groups), 4)
        by_runs = dict((tuple(sorted(point["run_id"] for point in group["points"])), group)
                       for group in groups)
        self.assertIn(("same_a", "same_b"), by_runs)
        base = by_runs[("same_a", "same_b")]
        differs = dict((item["group_id"], item["differs"]) for item in base["related"])
        self.assertEqual(differs[by_runs[("policy_changed",)]["group_id"]], ["policy"])
        self.assertEqual(differs[by_runs[("measured_evidence",)]["group_id"]], ["provenance"])
        self.assertEqual(differs[by_runs[("fixture_changed",)]["group_id"]], ["fixture"])
        # Synthetic evidence is labeled as such and never satisfies the required check.
        for run in doc["runs"]:
            self.assertFalse(run["satisfies_required_check"])
        self.assertEqual(doc["comparison"], "trend_across_runs")

    def test_conflicting_duplicates_refuse_and_identical_ones_are_shown_once(self):
        first = self.make_run("dup")
        again = self.make_run("dup")
        doc = history.write([first, again], self.path("history"))
        self.assertEqual([run["run_id"] for run in doc["runs"]], ["dup"])
        other = synthetic.plan(self.suite, run_id="dup", candidate="different")
        conflicting = run_dir(self, other, synthetic.constant("10.5"))
        with self.assertRaises(ContractError) as caught:
            history.write([first, conflicting], self.path("history-2"))
        self.assertIn("with different evidence", str(caught.exception))

    def test_unreadable_inputs_and_expired_archived_runs_are_shown_as_unavailable(self):
        root = self.path("archive")
        archive.Archive.create(root)
        source = self.make_run("expiring")
        logs = os.path.join(source, "shards", "0", "logs")
        os.mkdir(logs)
        with open(os.path.join(logs, "000001.log"), "wb") as stream:
            stream.write(b"log\n")
        synthetic.reseal_shard(os.path.join(source, "shards", "0"))
        with archive.Archive(root) as store:
            store.import_run(source)
            admitted = store.state()["expiring"].event["unix_ms"]
            store.retain(admitted + 90 * archive.DAY_MS)
        doc = history.write([root, self.path("missing-run")], self.path("history"))
        run = doc["runs"][0]
        self.assertEqual(run["source"], "archive_summary")
        self.assertEqual(run["retention"], {"numeric": "complete",
                                            "profile_reproduction": "not_applicable",
                                            "expired": ["log"]})
        self.assertEqual(doc["unavailable"][0]["input"], "input 2")
        page = read(os.path.join(self.path("history"), history.HISTORY_HTML)).decode()
        self.assertIn("expired: log", page)
        with archive.Archive(root) as store:
            store.retain(admitted + 365 * archive.DAY_MS)
        doc = history.write([root], self.path("history-3"))
        self.assertEqual(doc["runs"], [])
        self.assertIn("numeric evidence expired", doc["unavailable"][0]["reason"])
