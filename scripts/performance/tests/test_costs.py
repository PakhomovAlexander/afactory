"""Cost and shipping-budget demonstrations from retained runs (ADR-0183, ADR-0185).

Every run here is synthetic, written at test time; bundles labelled measured exist only to
exercise the rule that only measured evidence demonstrates anything. Nothing here is a cost or
speed claim: those need real pull-request, same-source and shipping executions.
"""
import copy
import io
import json
import os
import unittest
from contextlib import redirect_stderr, redirect_stdout
from fractions import Fraction

from perflab import contracts, costs, planning, schedule, synthetic
from perflab.values import ContractError

from tests import support

SUITE = support.synthetic_suite()


def plan(run_id, pr=False, tier="quick", environment=None, index=0, suite_doc=None,
         pairs=None, candidate=None):
    """A frozen plan of a pull-request comparison (base against merge) or a same-source one;
    `index` gives each comparison its own sources."""
    registry = suite_doc or SUITE
    profile = [item for item in registry.doc["tiers"][tier]["profiles"]
               if environment in (None, item["environment"])][0]
    recipe = registry.doc["recipes"][profile["recipe"]]
    request = planning.Request(
        trigger="pr" if pr else "manual", tier=tier, scope="bootstrap", base="b", head="c",
        run_id=run_id, seed=7, environment=environment, pr_number=7 if pr else None,
        trusted_host_build=recipe["os"] == "linux" and "container_image" not in recipe,
        pairs=pairs)
    ids = planning.Identities()
    ids.baseline = synthetic.label_sha("base %d" % index)
    ids.candidate = synthetic.label_sha("merge %d" % index) if pr else ids.baseline
    if candidate is not None:
        ids.candidate = synthetic.label_sha(candidate)
    ids.control = ids.baseline
    ids.trigger = ({"kind": "pr", "number": 7, "head_sha": synthetic.label_sha("head"),
                    "base_sha": ids.baseline, "merge_sha": ids.candidate,
                    "merge_base_sha": ids.baseline} if pr
                   else {"kind": "manual", "remeasure_of": None})
    digests = {"suite_sha256": registry.raw_sha256, "policy_digest": registry.policy_digest,
               "instrument_digest": synthetic.label_digest("instrument"),
               "fixture_digest": synthetic.label_digest("fixtures")}
    lock = {"cargo_lock_sha256": synthetic.label_digest("lock")}
    return planning.freeze(registry, request, ids, "control", ids.control, None, digests, [],
                           {"authority": lock, "baseline": lock, "candidate": lock})


PRIMED_MS = 20000


def cache_edit(cache, fetch="host_network"):
    """A build manifest edit recording a `cache` compiled build cache (af.perf-builds/6): warm
    primed the run's own targets for PRIMED_MS before the measured pass, cold observed them
    empty. The dependency fetch is recorded apart and never decides the cache."""
    def edit(doc):
        doc["isolation"]["dependency_fetch"] = fetch
        started = doc["execution"]["run"]["started_unix_ms"]
        record = doc["build_cache"]
        if cache == "warm":
            step = dict(doc["supervisor"]["step"], argv=["synthetic-priming"])
            record.update(treatment="warm", priming={"started_unix_ms": started,
                                   "ended_unix_ms": started + PRIMED_MS,
                                   "steps": [{"name": "prime_authority", "step": step}]},
                          observed=[{"worktree": "authority", "state": "primed",
                                     "compiled_bytes": 4096}],
                          measured_started_unix_ms=started + PRIMED_MS)
        else:
            record.update(treatment="cold", priming=None, measured_started_unix_ms=started)
    return edit


def rehost(run_dir, run_plan, cpu):
    """Relabel every shard of a written run as observed on hosts of `cpu`."""
    observed = dict(synthetic.SYNTHETIC_HOST, cpu_model={"value": cpu})
    for shard in run_plan["shards"]:
        root = os.path.join(run_dir, "shards", str(shard["index"]))
        path = os.path.join(root, "samples.jsonl")
        with open(path, "rb") as stream:
            lines = [json.loads(line) for line in stream.read().splitlines()]
        with open(path, "wb") as stream:
            for line in lines:
                line["host_id"] = contracts.host_id(observed)
                stream.write(contracts.document_bytes(line))

        def host(doc):
            doc["host"] = observed
            doc["host_id"] = contracts.host_id(observed)
        synthetic.reseal_shard(root, host)
    return run_dir


def failed_supervisor(doc):
    doc["supervisor"].update(status="failed", binary_sha256=None, artifact=None)


class Cost(support.TempDir):
    def write(self, run_id, pr, cache="cold", provenance="measured", index=0, edit_builds=None,
              **options):
        def builds(doc):
            cache_edit(cache)(doc)
            if edit_builds is not None:
                edit_builds(doc)
        return synthetic.write_run(plan(run_id, pr, index=index), self.path(run_id),
                                   options.pop("value", synthetic.constant(10)),
                                   provenance=provenance, edit_builds=builds, **options)

    def entries(self, provenance="measured", skip=(), declare=True, **options):
        out = []
        for kind in costs.KINDS:
            for cache in costs.CACHES:
                for index in range(costs.RUNS_PER_GROUP):
                    run_id = "%s_%s_%d" % (kind, cache, index)
                    if run_id in skip:
                        continue
                    out.append((self.write(run_id, kind == "pr", cache, provenance, index,
                                           **options), cache if declare else None))
        return out

    def test_five_completed_measured_executions_per_group_demonstrate_cost(self):
        entries = self.entries()
        doc = costs.cost_report(entries)
        self.assertEqual((doc["status"], doc["reasons"], doc["failed_attempts"]),
                         ("demonstrated", [], []))
        self.assertEqual(sorted(doc["groups"]), sorted(contracts.COST_GROUPS))
        run = [item for item in doc["runs"] if item["run_id"] == "pr_cold_0"][0]
        self.assertEqual((run["kind"], run["cache"], run["cache_source"],
                          run["dependency_fetch"], run["execution"], run["provenance"],
                          run["priming_ms"]),
                         ("pr", "cold", "observed", "host_network", "complete", "measured",
                          None))
        warm = [item for item in doc["runs"] if item["run_id"] == "pr_warm_0"][0]
        # The warm run's compiled cache was primed by its own build, recorded apart from the
        # measured pass; its dependency fetch is reported separately.
        self.assertEqual((warm["cache"], warm["dependency_fetch"], warm["priming_ms"]),
                         ("warm", "host_network", PRIMED_MS))
        self.assertEqual(warm["build_ms"], run["build_ms"] - PRIMED_MS)
        self.assertEqual(doc["groups"]["pr_warm"]["priming_ms_max"], PRIMED_MS)
        self.assertIsNone(doc["groups"]["pr_cold"]["priming_ms_max"])
        # The cost is bound to its retained source, plan, bundle, build and binaries.
        loaded = contracts.read_file(os.path.join(entries[0][0], "builds.json"), "builds")
        identity = run["identity"]
        self.assertEqual(identity["binaries"]["candidate"],
                         loaded["sides"]["candidate"]["binaries"])
        self.assertEqual(identity["binaries"]["supervisor"],
                         loaded["supervisor"]["binary_sha256"])
        self.assertTrue(identity["run_bundle"] and identity["builds_sha256"])
        self.assertEqual(run["build_ms"], loaded["execution"]["ended_unix_ms"]
                         - loaded["build_cache"]["measured_started_unix_ms"])
        self.assertEqual(run["cache_identity"], loaded["build_cache"]["identity"])
        self.assertEqual(len(run["shard_sampling_ms"]), len(plan("x")["shards"]))
        case = run["cases"]["synthetic_fast"]
        planned = plan("x")["cases"][0]
        self.assertEqual(case["samples"], 2 * (len(planned["warmup_orders"])
                                               + len(planned["measured_orders"])))
        group = doc["groups"]["pr_cold"]
        self.assertEqual((group["runs"], group["completed"], group["failed"],
                          group["failures"]), (5, 5, 0, 0))
        self.assertEqual(group["build_ms_max"], run["build_ms"])
        # The document's aggregates and status are recomputed by its reader.
        forged = dict(doc, status="pending")
        with self.assertRaisesRegex(ContractError, "status: does not match"):
            contracts.validate_cost(forged)
        forged = dict(doc, groups=dict(doc["groups"], pr_cold=dict(group, completed=6)))
        with self.assertRaisesRegex(ContractError, "groups: do not match"):
            contracts.validate_cost(forged)
        forged = copy.deepcopy(doc)
        forged["runs"][0]["cache"] = "warm"
        with self.assertRaisesRegex(ContractError, "an observed warm build records its priming"):
            contracts.validate_cost(forged)
        forged = copy.deepcopy(doc)
        forged["runs"][0]["cache_identity"] = None
        with self.assertRaisesRegex(ContractError, "cache: observed exactly with"):
            contracts.validate_cost(forged)
        self.assertEqual(contracts.read_json(contracts.write_bytes(doc, "cost"), "cost"), doc)

    def test_the_cache_is_observed_and_a_contradicting_declaration_is_refused(self):
        # The predecessor recorded the operator's cold or warm argument as the cache, and its
        # repair read prefetched dependencies as a warm compiled cache although every build
        # target was fresh (ADR-0185): dependency preparation is not a build cache.
        prefetched = self.write("prefetched_cold", True, "cold",
                                edit_builds=cache_edit("cold", "prefetched"))
        run = costs.cost_report([(prefetched, None)])["runs"][0]
        self.assertEqual((run["cache"], run["dependency_fetch"], run["priming_ms"]),
                         ("cold", "prefetched", None))
        with self.assertRaisesRegex(ContractError, "the declared warm cache is not what its "
                                    "build manifest shows"):
            costs.cost_report([(prefetched, "warm")])
        cold = self.write("declared_warm", True, "cold")
        with self.assertRaisesRegex(ContractError, "the declared warm cache is not what its "
                                    "build manifest shows"):
            costs.cost_report([(cold, "warm")])
        self.assertEqual(costs.cost_report([(cold, None)])["runs"][0]["cache"], "cold")
        with self.assertRaisesRegex(ContractError, "cold or warm"):
            costs.cost_report([(cold, "lukewarm")])
        # Without a retained manifest only a declaration names the cache: a failed attempt.
        gone = self.write("no_manifest", True, "warm")
        os.remove(os.path.join(gone, "builds.json"))
        run = costs.cost_report([(gone, "warm")])["runs"][0]
        self.assertEqual((run["cache"], run["cache_source"], run["execution"]),
                         ("warm", "declared", "failed"))
        unknown = costs.cost_report([(gone, None)])
        self.assertEqual(unknown["runs"][0]["cache_source"], "unknown")
        self.assertTrue(any("no build manifest or declaration" in reason
                            for reason in unknown["reasons"]))

    def test_failed_attempts_are_retained_and_never_count(self):
        # The predecessor counted five measured runs with failed builds or shards per group as
        # demonstrated, with no reason given.
        # Its repair still counted a complete-shard run whose samples failed or timed out, or
        # whose health sentinel timed out (ADR-0185): every execution failure fails the attempt.
        first = plan("probe")
        sample = [slot for slot in schedule.expected_slots(first, 0)
                  if slot["phase"] == "measured"][0]
        sentinel = [slot for slot in schedule.expected_slots(first, 0)
                    if slot["phase"].startswith("sentinel")][0]
        for name, options in (("failed_build", {"edit_builds": failed_supervisor}),
                              ("failed_shard", {"shard_status": {1: "failed"}}),
                              ("interrupted", {"shard_status": {0: "interrupted"}}),
                              ("sample_timeout",
                               {"failures": {schedule.slot_key(sample): "timeout"}}),
                              ("sentinel_timeout",
                               {"failures": {schedule.slot_key(sentinel): "timeout"}}),
                              ("unstable", {"sentinel": lambda slot: Fraction(
                                  10 if slot["phase"] == "sentinel_start" else 30)})):
            with self.subTest(attempt=name):
                os.makedirs(self.path(name))
                self.tmp, saved = self.path(name), self.tmp
                try:
                    doc = costs.cost_report(self.entries(**options))
                finally:
                    self.tmp = saved
                self.assertEqual(doc["status"], "pending")
                self.assertEqual(len(doc["failed_attempts"]), 20)
                for group in doc["groups"].values():
                    self.assertEqual((group["runs"], group["completed"], group["failed"]),
                                     (5, 0, 5))
                    self.assertIsNone(group["build_ms_median"])
                self.assertIn("pr_cold: 0 of 5 completed measured executions (5 failed "
                              "attempts)", doc["reasons"])
                self.assertTrue(all(run["failures"] for run in doc["runs"]))
                if name in ("sentinel_timeout", "unstable"):
                    self.assertTrue(any(item["where"].startswith("health shard")
                                        for item in doc["runs"][0]["failures"]))
                forged = copy.deepcopy(doc)
                forged["runs"][0]["execution"] = "complete"
                with self.assertRaisesRegex(ContractError, "complete exactly without"):
                    contracts.validate_cost(forged)

    def test_missing_shards_regressions_and_repeated_comparisons(self):
        entries = self.entries()
        # A missing shard is a failed attempt, kept beside five completed comparisons.
        missing = self.write("pr_cold_missing", True, "cold", index=9)
        import shutil
        shutil.rmtree(os.path.join(missing, "shards", "1"))
        doc = costs.cost_report(entries + [(missing, None)])
        self.assertEqual(doc["status"], "demonstrated")
        self.assertEqual(doc["failed_attempts"], ["pr_cold_missing"])
        self.assertEqual((doc["groups"]["pr_cold"]["completed"],
                          doc["groups"]["pr_cold"]["failed"]), (5, 1))
        # A completed comparison that found a regression is complete and keeps its cost.
        slower = synthetic.constant(10)

        def regression(slot, case):
            return Fraction(20) if slot["side"] == "candidate" else slower(slot, case)
        regressed = self.write("pr_warm_regressed", True, "warm", index=4, value=regression)
        kept = [entry for entry in entries if not entry[0].endswith("pr_warm_4")]
        doc = costs.cost_report(kept + [(regressed, None)])
        run = [item for item in doc["runs"] if item["run_id"] == "pr_warm_regressed"][0]
        self.assertEqual((run["result"], run["execution"]), ("fail", "complete"))
        self.assertEqual(doc["groups"]["pr_warm"]["completed"], 5)
        self.assertEqual(doc["status"], "demonstrated")
        # The same comparison measured again is another execution: the predecessor collapsed
        # repeated runs of one source pair into one comparison (ADR-0185).
        again = self.write("pr_warm_again", True, "warm", index=3)
        doc = costs.cost_report([entry for entry in entries
                                 if not entry[0].endswith("pr_warm_4")] + [(again, None)])
        self.assertEqual((doc["groups"]["pr_warm"]["runs"],
                          doc["groups"]["pr_warm"]["completed"]), (5, 5))
        self.assertEqual(doc["status"], "demonstrated")
        with self.assertRaisesRegex(ContractError, "given twice"):
            costs.cost_report([entries[0], entries[0]])

    def test_five_independent_same_source_runs_at_one_sha_count_five_times(self):
        same = [(self.write("same_%d" % index, False, "cold", index=0), None)
                for index in range(costs.RUNS_PER_GROUP)]
        doc = costs.cost_report(same)
        shas = set((run["identity"]["baseline_sha"], run["identity"]["candidate_sha"])
                   for run in doc["runs"])
        self.assertEqual(len(shas), 1)
        self.assertEqual(len(set(run["identity"]["run_bundle"] for run in doc["runs"])), 5)
        self.assertEqual(doc["groups"]["same_source_cold"]["completed"], 5)
        self.assertNotIn("same_source_cold", " ".join(doc["reasons"]))
        # A copied run directory is still one execution, refused when given twice.
        import shutil
        shutil.copytree(same[0][0], self.path("copied"))
        with self.assertRaisesRegex(ContractError, "given twice"):
            costs.cost_report(same + [(self.path("copied"), None)])

    def test_synthetic_evidence_and_missing_runs_stay_pending(self):
        doc = costs.cost_report(self.entries("synthetic", skip=("same_source_warm_4",)))
        self.assertEqual(doc["status"], "pending")
        self.assertIn("same_source_warm: 0 of 5 completed measured executions (0 failed "
                      "attempts)", doc["reasons"])
        self.assertTrue(any(reason.startswith("not measured evidence") for reason in
                            doc["reasons"]))

    def test_the_cli_writes_the_report_and_exits_by_its_status(self):
        import cli
        entries = self.entries()
        argv = ["cost-report", "--output", self.path("cost.json"), "--job-summary",
                self.path("summary.md")]
        for path, cache in entries:
            argv += ["--run", "%s=%s" % (path, cache)]
        with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
            self.assertEqual(cli.main(argv), 0)
            self.assertEqual(cli.main(["cost-report", "--run", "%s=%s" % entries[0],
                                       "--output", self.path("one.json")]), 4)
            self.assertEqual(cli.main(["cost-report", "--run", entries[0][0],
                                       "--output", self.path("observed.json")]), 4)
            self.assertEqual(cli.main(["cost-report", "--run", "%s=warm" % entries[0][0]]), 2)
        with open(self.path("summary.md")) as stream:
            summary = stream.read()
        self.assertIn("| pr_cold | 5 | 5 | 0 |", summary)
        self.assertIn("Failed attempts, retained and never counted: none", summary)


def target_of(entry):
    """A budget instance's one target, which a single identity's runs give it."""
    assert len(entry["targets"]) == 1, entry["targets"]
    return entry["targets"][0]


class Shipping(support.TempDir):
    def write(self, run_id, environment, value, provenance="measured", suite_doc=None,
              pairs=None, candidate=None, cpu=None, **options):
        run_plan = plan(run_id, tier="nightly", environment=environment, suite_doc=suite_doc,
                        pairs=pairs, candidate=candidate)
        path = synthetic.write_run(run_plan, self.path(run_id), value, provenance=provenance,
                                   **options)
        return rehost(path, run_plan, cpu) if cpu is not None else path

    def linux_entry(self, doc, instance=None):
        entries = [item for item in doc["budgets"]
                   if item["environment_id"] == "linux_x86_64_musl_hosted"
                   and (instance is None or item["instance_id"] == instance)]
        return entries[0]

    def test_every_shipping_sample_is_checked_strictly_against_its_limit(self):
        required = costs.required_budgets(SUITE)
        self.assertTrue(required)
        self.assertEqual(sorted(set(key[1] for key in required)),
                         ["linux_x86_64_musl_hosted", "macos_arm64_local"])
        fast = synthetic.constant(10)
        runs = [self.write("linux", "linux_x86_64_musl_hosted", fast),
                self.write("macos", "macos_arm64_local", fast)]
        doc = costs.shipping_report(SUITE, runs)
        self.assertEqual(doc["status"], "demonstrated", doc["reasons"])
        self.assertEqual((doc["policy_digest"], doc["suite_sha256"]),
                         (SUITE.policy_digest, SUITE.raw_sha256))
        self.assertEqual([run["run_id"] for run in doc["runs"]], ["linux", "macos"])
        for entry in doc["budgets"]:
            self.assertEqual(entry["result"], "met")
            target = target_of(entry)
            self.assertGreaterEqual(len(target["samples"]), entry["minimum_samples"])
            self.assertEqual((target["planned"], target["failed"], target["missing"]),
                             (len(target["samples"]), 0, 0))
            self.assertEqual((target["max"], target["median"], target["p95"]), ("10", "10", "10"))
            self.assertTrue(target["host"] and target["candidate_binary"])
        self.assertEqual(contracts.read_json(contracts.write_bytes(doc, "shipping"),
                                             "shipping"), doc)

        # One candidate sample at exactly the limit misses it: strictly below is required.
        def edge(slot, case):
            if slot["side"] == schedule.CANDIDATE and slot["phase"] == "measured" \
                    and slot["pair"] == 3 and slot["instance_id"] != "synthetic_fast":
                return Fraction(50)
            return Fraction(10)
        missed = costs.shipping_report(SUITE, [self.write("edge", "linux_x86_64_musl_hosted",
                                                        edge), runs[1]])
        self.assertEqual(missed["status"], "missed")
        self.assertIn("missed", [entry["result"] for entry in missed["budgets"]])
        forged = dict(missed, status="demonstrated")
        with self.assertRaisesRegex(ContractError, "status: does not match"):
            contracts.validate_shipping(forged)

    def test_a_run_given_twice_or_copied_counts_once_and_is_refused(self):
        # The predecessor counted the same directories twice: 10 samples became 20.
        import shutil
        fast = synthetic.constant(10)
        linux = self.write("linux", "linux_x86_64_musl_hosted", fast)
        macos = self.write("macos", "macos_arm64_local", fast)
        copied = self.path("copied")
        shutil.copytree(linux, copied)
        for runs in ([linux, macos, linux], [linux, copied, macos]):
            with self.subTest(runs=[os.path.basename(path) for path in runs]):
                with self.assertRaisesRegex(ContractError, "given twice"):
                    costs.shipping_report(SUITE, runs)

    def test_failed_or_missing_planned_samples_are_never_pooled_away(self):
        # The predecessor combined a run with failed and missing candidate samples with a
        # complete one and reported the pooled samples demonstrated, dropping the failures.
        fast = synthetic.constant(10)
        complete = self.write("linux", "linux_x86_64_musl_hosted", fast)
        macos = self.write("macos", "macos_arm64_local", fast)
        first = plan("broken", tier="nightly", environment="linux_x86_64_musl_hosted")
        budgeted = [case for case in first["cases"] if any(
            budget["applicable"] for budget in case["budgets"])][0]
        slots = [slot for slot in schedule.expected_slots(first, budgeted["shard"])
                 if slot["instance_id"] == budgeted["instance_id"]
                 and slot["phase"] == "measured" and slot["side"] == schedule.CANDIDATE]
        broken = self.write("broken", "linux_x86_64_musl_hosted", fast,
                            omit=[schedule.slot_key(slot) for slot in slots[:2]],
                            failures={schedule.slot_key(slots[2]): "timeout"},
                            shard_status={budgeted["shard"]: "failed"})
        doc = costs.shipping_report(SUITE, [broken, complete, macos])
        self.assertEqual(doc["status"], "pending")
        entry = self.linux_entry(doc, budgeted["instance_id"])
        target = target_of(entry)
        self.assertEqual((entry["result"], target["failed"], target["missing"],
                          target["incomplete_runs"]), ("incomplete", 1, 2, ["broken"]))
        self.assertEqual(target["planned"], 2 * len(slots))
        self.assertGreaterEqual(len(target["samples"]), entry["minimum_samples"])
        self.assertTrue(any("is missing" in reason for reason in doc["reasons"]))
        # A shard that did not complete leaves its budget instances incomplete.
        interrupted = self.write("interrupted", "linux_x86_64_musl_hosted", fast,
                                 shard_status={budgeted["shard"]: "interrupted"})
        doc = costs.shipping_report(SUITE, [interrupted, complete, macos])
        entry = self.linux_entry(doc, budgeted["instance_id"])
        self.assertEqual((entry["result"], target_of(entry)["incomplete_runs"]),
                         ("incomplete", ["interrupted"]))
        forged = copy.deepcopy(doc)
        for item in forged["budgets"]:
            for target in item["targets"]:
                target.update(result="met")
        with self.assertRaisesRegex(ContractError, "result: every sample"):
            contracts.validate_shipping(forged)

    def test_unhealthy_shipping_evidence_keeps_its_samples_but_never_demonstrates(self):
        # The predecessor demonstrated budgets from runs whose every budget sample was below its
        # limit while a health sentinel failed or the shard was unstable, or whose build was
        # unusable (ADR-0185).
        fast = synthetic.constant(10)
        macos = self.write("macos", "macos_arm64_local", fast)
        healthy = costs.shipping_report(SUITE, [
            self.write("healthy", "linux_x86_64_musl_hosted", fast), macos])
        self.assertEqual(healthy["status"], "demonstrated", healthy["reasons"])
        first = plan("sentinel", tier="nightly", environment="linux_x86_64_musl_hosted")
        sentinel = [slot for slot in schedule.expected_slots(first, 0)
                    if slot["phase"].startswith("sentinel")][0]
        for name, options in (
                ("sentinel", {"failures": {schedule.slot_key(sentinel): "timeout"}}),
                ("unstable", {"sentinel": lambda slot: Fraction(
                    10 if slot["phase"] == "sentinel_start" else 30)}),
                ("unusable_build", {"edit_builds": failed_supervisor})):
            with self.subTest(evidence=name):
                doc = costs.shipping_report(SUITE, [
                    self.write(name, "linux_x86_64_musl_hosted", fast, **options), macos])
                self.assertEqual(doc["status"], "pending")
                entry = self.linux_entry(doc)
                target = target_of(entry)
                self.assertEqual(entry["result"], "incomplete")
                self.assertEqual(target["incomplete_runs"], [name])
                self.assertGreaterEqual(len(target["samples"]), entry["minimum_samples"])
                self.assertTrue(any("is not valid, healthy evidence" in reason
                                    for reason in doc["reasons"]))

    def test_samples_of_different_binaries_or_hosts_are_never_pooled(self):
        # The predecessor pooled two 12-sample runs of different candidate sources and binaries
        # under one environment label into one demonstrated budget (ADR-0185).
        fast = synthetic.constant(10)
        macos = self.write("macos", "macos_arm64_local", fast)

        def binary(label):
            """The candidate's shipping binary hashed from `label`, in its build manifest and in
            every candidate sample it executed."""
            digest = synthetic.label_digest(label)

            def builds(doc):
                doc["sides"]["candidate"]["binaries"]["af"] = digest
                doc["sides"]["candidate"]["artifacts"]["af"]["sha256"] = digest

            def records(record):
                if record["side"] == schedule.CANDIDATE and record["binary_sha256"] \
                        == synthetic.binaries(schedule.CANDIDATE)["af"]:
                    record["binary_sha256"] = digest
                return [record]
            return {"edit_builds": builds, "edit": records}
        first = self.write("first", "linux_x86_64_musl_hosted", fast, pairs=12,
                           candidate="first", **binary("first binary"))
        second = self.write("second", "linux_x86_64_musl_hosted", fast, pairs=12,
                            candidate="second", **binary("second binary"))
        doc = costs.shipping_report(SUITE, [first, second, macos])
        self.assertEqual(doc["status"], "pending")
        entry = self.linux_entry(doc)
        self.assertEqual([len(target["samples"]) for target in entry["targets"]], [12, 12])
        self.assertEqual(sorted(target["result"] for target in entry["targets"]),
                         ["insufficient", "insufficient"])
        self.assertEqual(entry["result"], "insufficient")
        # Two runs of one binary on one hardware/image group pool into one 24-sample target.
        again = self.write("again", "linux_x86_64_musl_hosted", fast, pairs=12,
                           candidate="first", **binary("first binary"))
        doc = costs.shipping_report(SUITE, [first, again, macos])
        self.assertEqual(doc["status"], "demonstrated", doc["reasons"])
        target = target_of(self.linux_entry(doc))
        self.assertEqual((len(target["samples"]), target["runs"]), (24, ["again", "first"]))
        # The same binary on another hardware fingerprint under the same label is another target.
        elsewhere = self.write("elsewhere", "linux_x86_64_musl_hosted", fast, pairs=12,
                               candidate="first", cpu="Intel Xeon", **binary("first binary"))
        doc = costs.shipping_report(SUITE, [first, elsewhere, macos])
        entry = self.linux_entry(doc)
        self.assertEqual(len(set(target["host_fingerprint"] for target in entry["targets"])), 2)
        self.assertEqual(entry["result"], "insufficient")
        # A forged target that pools another run's identity is refused by the reader.
        forged = copy.deepcopy(costs.shipping_report(SUITE, [first, second, macos]))
        entry = self.linux_entry(forged)
        entry["targets"][0]["runs"] = sorted(entry["targets"][0]["runs"]
                                             + entry["targets"][1]["runs"])
        with self.assertRaisesRegex(ContractError, "not of this target's product identity"):
            contracts.validate_shipping(forged)

    def test_the_distribution_is_the_exact_maximum_median_and_nearest_rank_p95(self):
        self.assertEqual(costs.nearest_rank_p95([Fraction(n) for n in range(1, 21)]), 19)
        self.assertEqual(costs.nearest_rank_p95([Fraction(n) for n in range(1, 25)]), 23)
        self.assertEqual(costs.distribution([Fraction(1), Fraction(4), Fraction(2),
                                             Fraction(3)]), ("4", "5/2", "4"))
        self.assertEqual(costs.distribution([]), (None, None, None))
        fast = synthetic.constant(10)

        def spread(slot, case):
            return Fraction(slot["pair"], 2) if slot["side"] == schedule.CANDIDATE \
                else Fraction(10)
        doc = costs.shipping_report(SUITE, [
            self.write("spread", "linux_x86_64_musl_hosted", spread),
            self.write("macos", "macos_arm64_local", fast)])
        target = target_of(self.linux_entry(doc))
        values = sorted(Fraction(value) for value in target["samples"])
        self.assertEqual(target["max"], str(values[-1]))
        self.assertEqual(Fraction(target["p95"]), costs.nearest_rank_p95(values))
        self.assertEqual(Fraction(target["median"]), costs.median(values))
        summary = costs.summary_markdown(doc)
        self.assertIn("| Max | Median | p95 |", summary)
        forged = copy.deepcopy(doc)
        self.linux_entry(forged)["targets"][0]["p95"] = target["max"]
        if target["p95"] != target["max"]:
            with self.assertRaisesRegex(ContractError, "nearest-rank p95"):
                contracts.validate_shipping(forged)

    def test_samples_of_another_policy_never_count(self):
        # The predecessor took the limits of whatever suite it was given: 100 ms samples planned
        # against a 50 ms budget were demonstrated under a new 200 ms suite.
        from perflab import suite as suite_module
        relaxed_bytes = support.read(support.SYNTHETIC_SUITE).replace(b'limit = "50"',
                                                                       b'limit = "200"')
        self.assertNotEqual(relaxed_bytes, support.read(support.SYNTHETIC_SUITE))
        relaxed = suite_module.load_bytes(relaxed_bytes)
        slow = synthetic.constant(100)
        runs = [self.write("linux", "linux_x86_64_musl_hosted", slow),
                self.write("macos", "macos_arm64_local", slow)]
        self.assertEqual(costs.shipping_report(SUITE, runs)["status"], "missed")
        with self.assertRaisesRegex(ContractError, "froze policy .*, not the reviewed suite's"):
            costs.shipping_report(relaxed, runs)
        # Runs planned under the relaxed suite are judged by it, strictly.
        planned = [self.write("linux_relaxed", "linux_x86_64_musl_hosted", slow,
                              suite_doc=relaxed),
                   self.write("macos_relaxed", "macos_arm64_local", slow, suite_doc=relaxed)]
        with self.assertRaisesRegex(ContractError, "not the reviewed suite's"):
            costs.shipping_report(SUITE, planned)

    def test_the_cli_exits_by_the_demonstration_status(self):
        import cli
        fast = synthetic.constant(10)
        linux = self.write("linux", "linux_x86_64_musl_hosted", fast)
        macos = self.write("macos", "macos_arm64_local", fast)
        suite_path = self.path("suite.toml")
        with open(suite_path, "wb") as stream:
            stream.write(support.read(support.SYNTHETIC_SUITE))
        codes = []
        with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
            for index, runs in enumerate(([linux, macos], [linux], [linux, linux])):
                argv = ["shipping-report", "--suite", suite_path,
                        "--output", self.path("shipping-%d.json" % index)]
                for run_dir in runs:
                    argv += ["--run", run_dir]
                codes.append(cli.main(argv))
        self.assertEqual(codes, [0, 4, 2])

    def test_synthetic_or_absent_environments_leave_the_demonstration_pending(self):
        fast = synthetic.constant(10)
        doc = costs.shipping_report(SUITE, [self.write("linux", "linux_x86_64_musl_hosted",
                                                     fast)])
        self.assertEqual(doc["status"], "pending")
        mac = [entry for entry in doc["budgets"] if entry["environment_id"]
               == "macos_arm64_local"]
        self.assertTrue(mac and all(entry["result"] == "insufficient" for entry in mac))
        doc = costs.shipping_report(SUITE, [self.write("synthetic", "linux_x86_64_musl_hosted",
                                                     fast, provenance="synthetic")])
        self.assertEqual(doc["status"], "pending")
        self.assertIn("is not measured evidence", doc["reasons"][0])
        self.assertTrue(all(not entry["targets"] for entry in doc["budgets"]))


if __name__ == "__main__":
    unittest.main()
