"""Telemetry experiments (ADR-0214, ADR-0215) over a stand-in executor.

Every document a stand-in run prints is built from synthetic pairs or samples and completed by
the real bench script's own rule (`tests/telemetry_fixtures.py`); nothing here runs AF or
measures anything, and no figure is a performance claim. What is exercised is the driver: the
registry, the alternating rounds and shared seeds, the private environment, every failure state,
the level as a separate dimension, comparisons only between compatible same-host documents,
admission of a document only as its planned run with every figure recomputed, offline
verification only under the exact measurement authority, and the durable history's bounds.
"""
import copy
import json
import os
import random
import statistics
import unittest
from unittest import mock
import shutil

from perflab import contracts, telemetryexec, telemetrylab, tomlfile
from perflab.values import ContractError
from tests import telemetry_fixtures as fx
from tests.support import ROOT, TempDir, read


class Runs(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.binaries = fx.binaries(self)

    def run_lab(self, executor=None, output="result", **values):
        return telemetrylab.run(fx.settings(self, executor or fx.StandIn(self.binaries),
                                            output, **values))

    def test_rounds_alternate_sides_share_seeds_and_compare_each_level_apart(self):
        executor = fx.StandIn(self.binaries)
        result = self.run_lab(executor)
        self.assertEqual(result["status"], "complete")
        self.assertEqual(len(result["runs"]), 12)
        for run in result["runs"]:
            self.assertEqual(run["status"], "ok", run["detail"])
            self.assertEqual(run["position"], (("baseline", "candidate").index(run["side"])
                                               + run["round"]) % 2)
            self.assertEqual(run["seed"], 20261021 + run["round"])
            self.assertIn("{af}", run["argv"])
            self.assertEqual(run["judgment"]["verdict"], "not_judged")
        self.assertEqual(result["corpus"]["facts"]["segments"], 20)
        self.assertEqual((result["execution"]["mode"], result["execution"]["cleanup"],
                          result["execution"]["problems"]), ("trusted_local", "confirmed", []))
        levels = set((item["experiment"], item["subject"], item["level"], item["metric"],
                      item["shape"]) for item in result["comparisons"])
        for name in fx.FIXTURES:
            self.assertIn(("overhead", name, "off", "wall_ms", None), levels)
            self.assertIn(("overhead", name, "spans", "wall_ms", None), levels)
        wall = [item for item in result["comparisons"]
                if (item["experiment"], item["metric"], item["level"])
                == ("overhead", "wall_ms", "off")]
        for item in wall:
            self.assertEqual(item["state"], "compared")
            self.assertEqual(len(item["rounds"]), 2)
        # Every execution ran in its own new directory with nothing of the caller's
        # environment, and a read only on its own copy of the corpus.
        for execution in executor.calls:
            self.assertEqual(sorted(execution.env), sorted(
                telemetrylab.private_environment("x")))
            self.assertTrue(execution.env["HOME"].startswith(self.path("result.work")))
            if "generate" in execution.argv:
                # The generator writes the master corpus, watched with its own directory.
                self.assertEqual(execution.writable, [execution.cwd,
                                                      self.path("result.work", "corpus")])
            else:
                self.assertEqual(execution.writable, [execution.cwd])
            self.assertFalse(any(name.startswith(("AF_", "XDG_", "OTEL_"))
                                 for name in execution.env))
            if "measure" in execution.argv:
                corpus = execution.argv[execution.argv.index("--dir") + 1]
                self.assertEqual(corpus, os.path.join(execution.cwd, "corpus"))
            others = [path for side, path in self.binaries.items()
                      if path not in execution.argv]
            self.assertFalse(set(others) & set(execution.readable), "another side's binary")
        self.assertFalse(os.path.exists(self.path("result.work")), "the work is removed")
        verified, _, _ = telemetrylab.verify(self.path("result"))
        self.assertEqual(verified, result)
        page = read(self.path("result", telemetrylab.REPORT_HTML)).decode()
        self.assertIn("never contribute a verdict sample", page)
        self.assertIn("trusted local mode", page)

    def test_failures_timeouts_and_refused_documents_are_recorded_never_dropped(self):
        def failed(execution):
            return telemetryexec.Outcome(1, b"", 0, b"telemetry-overhead: exit 4\n", 28, 5000)

        def timed_out(execution):
            return telemetryexec.Outcome(None, b"{", 1, b"", 0, 9000, timed_out=True)

        def unknown_field(execution):
            argv = execution.argv
            doc = fx.overhead_doc(int(argv[argv.index("--seed") + 1]), 2)
            doc["note"] = "<b>x</b>"
            data = json.dumps(doc).encode()
            return telemetryexec.Outcome(0, data, len(data), b"", 0, 10)
        executor = fx.StandIn(self.binaries, {
            ("overhead", "candidate", 20261021): failed,
            ("invocation_cost", "baseline", 20261022): timed_out,
            ("overhead", "baseline", 20261022): unknown_field})
        result = self.run_lab(executor)
        self.assertEqual(result["status"], "partial")
        states = dict(((run["experiment"], run["round"], run["side"]), run["status"])
                      for run in result["runs"])
        self.assertEqual(states[("overhead", 0, "candidate")], "failed")
        self.assertEqual(states[("invocation_cost", 1, "baseline")], "timeout")
        self.assertEqual(states[("overhead", 1, "baseline")], "refused_document")
        wall = [item for item in result["comparisons"]
                if (item["experiment"], item["metric"]) == ("overhead", "wall_ms")]
        for item in wall:
            self.assertEqual(item["state"], "missing")
            self.assertEqual(item["rounds"], [])
            self.assertIsNone(item["median_change"])
        telemetrylab.verify(self.path("result"))

    def test_documents_of_another_protocol_or_host_are_never_compared(self):
        def other(doc):
            doc["machine"]["cpus"] = 64
        executor = fx.StandIn(self.binaries, {("overhead", "candidate"): {"extra": other}})
        result = self.run_lab(executor, experiments=["overhead"])
        wall = [item for item in result["comparisons"] if item["metric"] == "wall_ms"]
        self.assertTrue(wall)
        for item in wall:
            self.assertEqual(item["state"], "incompatible")
            self.assertIn("protocol, corpus or machine", item["reason"])
        changed = dict(fx.HOST, cpu_model={"value": "another"})
        result = self.run_lab(output="moved", experiments=["overhead"],
                              hosts=[fx.HOST, changed])
        self.assertFalse(result["host"]["same_host"])
        self.assertEqual(result["status"], "partial")
        self.assertTrue(all(item["state"] == "incompatible" for item in result["comparisons"]))

    def test_documents_unlike_their_planned_run_are_refused_even_when_both_sides_agree(self):
        # Both sides print five pairs where two were planned: their documents agree with each
        # other, but neither is the run the configuration planned, so nothing is compared.
        executor = fx.StandIn(self.binaries, {("overhead", "baseline"): {"pairs": 5},
                                              ("overhead", "candidate"): {"pairs": 5}})
        result = self.run_lab(executor, experiments=["overhead"])
        self.assertEqual(result["status"], "failed")
        for run in result["runs"]:
            self.assertEqual(run["status"], "refused_document")
            self.assertIn("not the planned 2 pairs", run["detail"])
        self.assertTrue(all(item["state"] == "missing" for item in result["comparisons"]))
        # Another binary's document is never this side's.
        executor = fx.StandIn(self.binaries, {("overhead", "candidate"): {
            "version": "af 9.9.9"}})
        result = self.run_lab(executor, output="foreign", experiments=["overhead"])
        foreign = [run for run in result["runs"] if run["side"] == "candidate"]
        self.assertTrue(all(run["status"] == "refused_document" for run in foreign))
        self.assertIn("not this side's", foreign[0]["detail"])
        self.assertEqual(result["status"], "partial")

    def test_a_small_corpus_over_its_bounds_is_admitted_unjudged_with_its_exit_status(self):
        def slow(execution):
            argv = execution.argv
            corpus = argv[argv.index("--dir") + 1]
            doc = fx.latency_doc(10, 1000, int(argv[argv.index("--repeat") + 1]),
                                 telemetrylab.corpus_facts(corpus))
            data = json.dumps(doc).encode()
            # The script exits 3 whenever its reads are over their bounds, at any scale.
            return telemetryexec.Outcome(3, data, len(data), b"", 0, 10)
        executor = fx.StandIn(self.binaries, {("read_latency", "candidate"): slow})
        result = self.run_lab(executor, experiments=["read_latency"])
        for run in result["runs"]:
            self.assertEqual(run["status"], "ok", run["detail"])
            self.assertEqual(run["judgment"]["verdict"], "not_judged")
        latency = [item for item in result["comparisons"] if item["metric"] == "latency_ms"]
        self.assertTrue(all(item["state"] == "compared" for item in latency))

    def test_a_side_whose_version_query_failed_is_never_measured(self):
        def broken(execution):
            return telemetryexec.Outcome(1, b"", 0, b"no", 2, 10)
        executor = fx.StandIn(self.binaries, {("version", "candidate"): broken})
        result = self.run_lab(executor, experiments=["overhead"])
        self.assertIsNone(result["binaries"]["candidate"]["version"])
        candidate = [run for run in result["runs"] if run["side"] == "candidate"]
        self.assertTrue(all(run["status"] == "not_run" for run in candidate))
        self.assertIn("--version query", candidate[0]["detail"])

    def test_a_spent_budget_records_every_remaining_run_as_not_run(self):
        result = self.run_lab(free_bytes=lambda path: 1)
        self.assertEqual(result["status"], "failed")
        self.assertTrue(all(run["status"] == "not_run" for run in result["runs"]))
        self.assertEqual(result["corpus"]["status"], "not_run")
        self.assertIn("free space", result["runs"][0]["detail"])

    def test_a_pull_request_is_never_measured_in_trusted_local_mode(self):
        with self.assertRaises(ContractError) as caught:
            self.run_lab(trigger="pr")
        self.assertIn("only in linux_container mode", str(caught.exception))
        self.assertFalse(os.path.exists(self.path("result")))
        with self.assertRaises(ContractError):
            telemetrylab.run(fx.settings(self, None, mode=None))

    def test_an_uncertain_cleanup_stops_the_run_and_keeps_its_work(self):
        seen = []

        def uncertain(execution):
            seen.append(execution.cwd)
            return telemetryexec.Outcome(None, b"", 0, b"", 0, 10, cleanup="uncertain",
                                         cleanup_detail="container x removal unconfirmed")
        executor = fx.StandIn(self.binaries, {("overhead", "baseline", 20261021): uncertain})
        result = self.run_lab(executor, experiments=["overhead"])
        execution = result["execution"]
        self.assertEqual((execution["cleanup"], execution["retained"]),
                         ("uncertain", self.path("result.work")))
        self.assertTrue(os.path.isdir(seen[0]), "what it could reach is kept")
        states = [(run["round"], run["side"], run["status"]) for run in result["runs"]]
        self.assertEqual(states[0], (0, "baseline", "failed"))
        self.assertTrue(all(status == "not_run" for _, _, status in states[1:]))
        self.assertIn("cleanup was not confirmed", result["runs"][1]["detail"])
        self.assertEqual(result["status"], "failed")
        telemetrylab.verify(self.path("result"))
        forged = copy.deepcopy(result)
        forged["execution"].update(cleanup="confirmed", retained=None)
        with self.assertRaises(ContractError):
            contracts.validate_telemetry_result(dict(forged, status="complete"))

    def test_an_execution_that_writes_outside_its_own_state_stops_the_run(self):
        def tamper_other(execution):
            # A hostile candidate run writes to the other side's binary and the result.
            with open(self.binaries["baseline"], "a") as stream:
                stream.write("# rewritten\n")
            with open(self.path("result", "planted.json"), "w") as stream:
                stream.write("{}")
            return fx.StandIn(self.binaries).run(execution)
        executor = fx.StandIn(self.binaries, {("overhead", "candidate", 20261021):
                                              tamper_other})
        result = self.run_lab(executor, experiments=["overhead"])
        problems = result["execution"]["problems"]
        self.assertIn("the baseline binary changed", problems)
        self.assertIn("the result directory holds what the controller did not write", problems)
        tampered = [run for run in result["runs"] if (run["round"], run["side"])
                    == (0, "candidate")][0]
        self.assertEqual(tampered["status"], "failed")
        self.assertIn("protected state changed", tampered["detail"])
        self.assertTrue(all(run["status"] == "not_run" for run in result["runs"]
                            if run["round"] == 1))
        self.assertNotEqual(result["status"], "complete")

    def test_verification_refuses_a_changed_document_even_with_a_consistent_manifest(self):
        self.run_lab(experiments=["overhead"])
        output = self.path("result")
        path = os.path.join(output, "raw", "overhead", "round-0-candidate.json")
        doc = json.loads(read(path).decode())
        doc["fixtures"][0]["pairs"][0]["spans"]["wall_ms"] = 1.0
        with open(path, "w") as stream:
            stream.write(json.dumps(doc))
        result = json.loads(read(os.path.join(output, telemetrylab.RESULT_JSON)).decode())
        for run in result["runs"]:
            if run["document"] and run["document"]["path"].endswith("round-0-candidate.json"):
                fx.rehash(run, read(path))
        fx.rewrite(output, result)
        with self.assertRaises(ContractError) as caught:
            telemetrylab.verify(output)
        self.assertIn("is not what its script's rule gives its pairs", str(caught.exception))

    def test_a_result_that_claims_complete_with_a_failed_run_is_refused(self):
        executor = fx.StandIn(self.binaries, {("overhead", "candidate"): lambda execution:
                                              telemetryexec.Outcome(1, b"", 0, b"", 0, 10)})
        result = self.run_lab(executor, experiments=["overhead"])
        forged = copy.deepcopy(result)
        forged["status"] = "complete"
        with self.assertRaises(ContractError):
            contracts.validate_telemetry_result(forged)
        forged = copy.deepcopy(result)
        forged["runs"] = forged["runs"][1:]
        with self.assertRaises(ContractError):
            contracts.validate_telemetry_result(forged)
        forged = copy.deepcopy(result)
        forged["configuration"]["overhead"]["pairs"] = 20
        with self.assertRaises(ContractError):
            contracts.validate_telemetry_result(forged)


class Authority(TempDir):
    """A result is admitted only under exactly the measurement authority it names."""

    def setUp(self):
        TempDir.setUp(self)
        self.binaries = fx.binaries(self)
        telemetrylab.run(fx.settings(self, fx.StandIn(self.binaries), "result",
                                     experiments=["read_latency"]))
        self.output = self.path("result")
        self.result = json.loads(read(os.path.join(self.output,
                                                   telemetrylab.RESULT_JSON)).decode())

    def forge_passes(self, result):
        # The synthetic 10-run corpus is never judged; a forger changes both judgments to
        # `pass`, and every hash, the page and the manifest agree.
        for run in result["runs"]:
            self.assertEqual(run["judgment"]["verdict"], "not_judged")
            run["judgment"] = {"verdict": "pass", "reasons": []}
        fx.rewrite(self.output, result)

    def test_forged_passes_are_refused_under_the_right_authority(self):
        self.forge_passes(copy.deepcopy(self.result))
        with self.assertRaises(ContractError) as caught:
            telemetrylab.verify(self.output)
        self.assertIn("its judgment is not what its script's rule gives it",
                      str(caught.exception))

    def test_a_result_naming_another_authority_is_refused_never_kept_as_recorded(self):
        forged = copy.deepcopy(self.result)
        forged["authority"]["digest"] = "sha256:" + "f" * 64
        self.forge_passes(forged)
        with self.assertRaises(ContractError) as caught:
            telemetrylab.verify(self.output)
        self.assertIn("can only be verified with a checkout of exactly that authority",
                      str(caught.exception))
        doc = telemetrylab.history(self.path("history"), [self.output])
        self.assertEqual(doc["results"], [])
        self.assertIn("exactly that authority", doc["refused"][0]["reason"])
        self.assertEqual(telemetrylab.admit_for_upload(self.output)[0], "refused")

    def test_a_checkout_of_the_named_authority_verifies_it(self):
        registry, _ = telemetrylab.load_registry()
        root = self.path("authority")
        for relative in telemetrylab.authority_files(registry):
            target = os.path.join(root, relative)
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copyfile(os.path.join(ROOT, relative), target)
        result, _, _ = telemetrylab.verify(self.output, root)
        self.assertEqual(result, self.result)
        with open(os.path.join(root, "scripts", "telemetry-corpus.py"), "ab") as stream:
            stream.write(b"\n# another authority\n")
        with self.assertRaises(ContractError):
            telemetrylab.verify(self.output, root)


class Admission(TempDir):
    """Every figure a document records is recomputed from its own samples and the trusted
    protocol; a document is only ever the run it was planned as."""

    def latency(self, runs=10000, slow=1.0, **values):
        doc = fx.latency_doc(runs, slow, **values)
        expectation = fx.expect("read_latency", runs=runs)
        return doc, expectation

    def admit(self, experiment, doc, expectation):
        registry, _ = telemetrylab.load_registry()
        return telemetrylab.admit(experiment, doc, expectation, registry)

    def test_read_latency_booleans_are_recomputed_never_trusted(self):
        doc, expectation = self.latency(slow=1000)
        self.assertEqual(self.admit("read_latency", doc, expectation)[0], "fail")
        for item in doc["measurements"]:
            item["within_bound"] = True
        doc["passed"] = True
        with self.assertRaises(ContractError) as caught:
            self.admit("read_latency", doc, expectation)
        self.assertIn("within_bound is not its latency against its bound", str(caught.exception))
        doc, expectation = self.latency()
        self.assertEqual(self.admit("read_latency", doc, expectation), ("pass", []))
        doc["passed"] = False
        with self.assertRaises(ContractError):
            self.admit("read_latency", doc, expectation)

    def test_read_latency_aggregates_bounds_and_samples_follow_the_protocol(self):
        doc, expectation = self.latency(slow=1000)
        doc["measurements"][1]["warm"]["latency_ms"] = 10.0
        doc["measurements"][1]["within_bound"] = True
        with self.assertRaises(ContractError) as caught:
            self.admit("read_latency", doc, expectation)
        self.assertIn("not the median of its samples", str(caught.exception))
        doc, expectation = self.latency(slow=1000)
        for item in doc["measurements"]:
            item["bound_ms"] = 20000
            item["within_bound"] = True
        doc["passed"] = True
        with self.assertRaises(ContractError) as caught:
            self.admit("read_latency", doc, expectation)
        self.assertIn("not the script's", str(caught.exception))
        doc, expectation = self.latency()
        doc["measurements"][2]["warm"]["samples_ms"] = [10.0]
        doc["measurements"][2]["warm"]["latency_ms"] = 10.0
        with self.assertRaises(ContractError) as caught:
            self.admit("read_latency", doc, expectation)
        self.assertIn("1 samples, not the protocol's 3", str(caught.exception))
        doc, expectation = self.latency()
        doc["measurements"] = doc["measurements"][:2]
        doc["passed"] = True
        with self.assertRaises(ContractError):
            self.admit("read_latency", doc, expectation)

    def test_a_corpus_name_alone_is_never_the_full_scale_proof(self):
        # A 10-run corpus renamed as the full one, with all-pass figures.
        doc, _ = self.latency(runs=10, name="telemetry-corpus-10k")
        with self.assertRaises(ContractError) as caught:
            self.admit("read_latency", doc, fx.expect("read_latency", runs=10))
        self.assertIn("not what 10 runs are named", str(caught.exception))
        # A document that claims the full corpus where a 10-run one was generated.
        doc, _ = self.latency(runs=10000)
        with self.assertRaises(ContractError) as caught:
            self.admit("read_latency", doc, fx.expect("read_latency", runs=10))
        self.assertIn("not the generated 10 runs", str(caught.exception))
        # Another corpus of the right size: its files are not the generated ones.
        doc, expectation = self.latency()
        doc["corpus"]["segment_bytes"] = 999
        with self.assertRaises(ContractError) as caught:
            self.admit("read_latency", doc, expectation)
        self.assertIn("not the generated corpus's", str(caught.exception))
        doc, expectation = self.latency(runs=10)
        expectation = fx.expect("read_latency", runs=10)
        self.assertEqual(self.admit("read_latency", doc, expectation)[0], "not_judged")

    def test_paired_documents_are_recomputed_whole_and_bound_to_their_plan(self):
        doc = fx.overhead_doc(20261021, 2)
        expectation = fx.expect("overhead")
        self.assertEqual(self.admit("overhead", doc, expectation)[0], "not_judged")
        for mutate, message in (
                (lambda d: d["fixtures"][0].update(median_pct=-50.0), "rule gives its pairs"),
                (lambda d: d["fixtures"][0].update(passed=False), "rule gives its pairs"),
                (lambda d: d["protocol"].update(seed=7), "not the planned"),
                (lambda d: d["protocol"].update(scale="full"), "not the planned"),
                (lambda d: d["fixtures"].pop(), "fixtures are not exactly"),
                (lambda d: d["af"].update(version="af 1.0.0"), "not this side's")):
            changed = copy.deepcopy(doc)
            mutate(changed)
            with self.assertRaises(ContractError) as caught:
                self.admit("overhead", changed, expectation)
            self.assertIn(message, str(caught.exception))
        doc = fx.invocation_doc(20261021, 2)
        self.assertEqual(self.admit("invocation_cost", doc, fx.expect("invocation_cost"))[0],
                         "not_judged")
        changed = copy.deepcopy(doc)
        changed["cases"] = changed["cases"][:1]
        with self.assertRaises(ContractError):
            self.admit("invocation_cost", changed, fx.expect("invocation_cost"))


class History(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.binaries = fx.binaries(self)

    def run_lab(self, output, **values):
        return telemetrylab.run(fx.settings(self, fx.StandIn(self.binaries), output,
                                            experiments=["overhead"], **values))

    def test_history_groups_only_one_configuration_authority_mode_and_host(self):
        self.run_lab("one")
        self.run_lab("two")
        self.run_lab("three", pairs=3)
        root = self.path("history")
        doc = telemetrylab.history(root, [self.path("one"), self.path("two"),
                                          self.path("three")])
        self.assertEqual(len(doc["results"]), 3)
        self.assertEqual(sorted(len(group["results"]) for group in doc["groups"]), [1, 2])
        self.assertEqual(set(group["mode"] for group in doc["groups"]), {"trusted_local"})
        self.assertEqual(doc["refused"], [])
        again = telemetrylab.history(root, [self.path("one")])
        self.assertEqual(again, doc)
        # Another bundle under a kept result's ID is refused, never replaces it.
        self.run_lab("impostor", result_id="tl_test_one", pairs=4)
        refused = telemetrylab.history(root, [self.path("impostor")])
        self.assertEqual(len(refused["refused"]), 1)
        self.assertIn("another bundle", refused["refused"][0]["reason"])
        self.assertEqual(len(refused["results"]), 3)
        html = read(os.path.join(root, telemetrylab.HISTORY_HTML)).decode()
        self.assertIn("never a trend", html)

    def test_the_root_is_locked_bounded_and_pruned(self):
        for name in ("one", "two", "three"):
            self.run_lab(name)
        root = self.path("history")
        with telemetrylab.HistoryRoot(root):
            with self.assertRaises(ContractError) as caught:
                telemetrylab.history(root, [self.path("one")])
            self.assertIn("another telemetry history command holds", str(caught.exception))
        doc = telemetrylab.history(root, [self.path("one"), self.path("two")], results_max=1)
        self.assertEqual([item["result_id"] for item in doc["results"]], ["tl_test_one"])
        self.assertIn("already keeps its 1 results", doc["refused"][0]["reason"])
        doc = telemetrylab.history(root, [self.path("two")])
        used = doc["used_bytes"]
        # The byte bound is checked before anything of an import is written.
        with telemetrylab.HistoryRoot(root, bytes_max=used + 1) as kept:
            with self.assertRaises(ContractError) as caught:
                kept.import_result(self.path("three"))
            self.assertIn("past its %d-byte bound" % (used + 1), str(caught.exception))
            self.assertEqual(kept.names(), ["tl_test_one", "tl_test_two"])
            self.assertEqual(kept.used_bytes(), used)
        # Retention prunes whole results, recorded in the write that removed them.
        doc = telemetrylab.history(root, retention_days=1,
                                   wall=lambda: 4102444800.0)
        self.assertEqual(doc["results"], [])
        self.assertEqual(sorted(item["result_id"] for item in doc["pruned"]),
                         ["tl_test_one", "tl_test_two"])
        self.assertEqual(os.listdir(os.path.join(root, telemetrylab.RESULTS)), [])


class Upload(TempDir):
    """What the workflow uploads: only a verified, credential-free result bundle."""

    def setUp(self):
        TempDir.setUp(self)
        self.binaries = fx.binaries(self)

    def test_only_a_verified_credential_free_result_is_admitted(self):
        self.assertEqual(telemetrylab.admit_for_upload(self.path("absent"))[0],
                         "missing_result")
        # A partial result is a valid result: it is uploaded and says it is partial.
        executor = fx.StandIn(self.binaries, {("overhead", "candidate"): lambda execution:
                                              telemetryexec.Outcome(1, b"", 0, b"", 0, 10)})
        telemetrylab.run(fx.settings(self, executor, "partial", experiments=["overhead"]))
        outcome, detail, result = telemetrylab.admit_for_upload(self.path("partial"))
        self.assertEqual((outcome, result["status"]), ("admitted", "partial"))
        # A log that captured a token is never uploaded.
        token = b"ghp_" + b"A" * 36

        def leaky(execution):
            data = json.dumps(fx.overhead_doc(20261021, 2)).encode()
            return telemetryexec.Outcome(0, data, len(data), b"token " + token, 40, 10)
        executor = fx.StandIn(self.binaries, {("overhead", "baseline", 20261021): leaky})
        telemetrylab.run(fx.settings(self, executor, "leaky", experiments=["overhead"]))
        self.assertEqual(telemetrylab.admit_for_upload(self.path("leaky"))[0],
                         "credential_found")
        # A result that does not verify is never uploaded.
        telemetrylab.run(fx.settings(self, fx.StandIn(self.binaries), "broken",
                                     experiments=["overhead"]))
        with open(self.path("broken", telemetrylab.SUMMARY_MD), "a") as stream:
            stream.write("forged\n")
        self.assertEqual(telemetrylab.admit_for_upload(self.path("broken"))[0], "refused")


class Registry(TempDir):
    def test_the_registry_names_exactly_the_bench_scripts_and_their_levels(self):
        doc, _ = telemetrylab.load_registry()
        self.assertEqual(doc["experiments"]["overhead"]["levels"], ["off", "spans"])
        self.assertEqual(doc["experiments"]["read_latency"]["levels"], ["content"])
        for profile in contracts.TELEMETRY_PROFILES:
            self.assertLessEqual(doc["profiles"][profile]["timeout_minutes"], 355)
        raw = tomlfile.loads(read(os.path.join(ROOT, telemetrylab.REGISTRY_PATH)), "t")
        for mutate in (lambda d: d["experiments"]["overhead"].update(
                           script="scripts/other.py"),
                       lambda d: d["experiments"]["overhead"].update(levels=["spans"]),
                       lambda d: d["experiments"]["overhead"].update(levels=["off", "content"]),
                       lambda d: d["profiles"]["pr"].update(rounds=9),
                       lambda d: d["profiles"]["pr"]["overhead"].update(pairs=41),
                       lambda d: d["containment"].update(pid_limit=1)):
            broken = copy.deepcopy(raw)
            mutate(broken)
            with self.assertRaises(ContractError):
                contracts.validate_telemetry_lab(broken)

    def test_the_authority_digest_follows_every_file_it_reads_and_refuses_links(self):
        registry, _ = telemetrylab.load_registry()
        root = self.path("root")
        for relative in telemetrylab.authority_files(registry):
            target = os.path.join(root, relative)
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copyfile(os.path.join(ROOT, relative), target)
        digest, files = telemetrylab.authority_digest(registry, root)
        self.assertEqual((digest, files), telemetrylab.authority_digest(registry))
        self.assertIn("scripts/performance/perflab/telemetryexec.py",
                      telemetrylab.authority_files(registry))
        with open(os.path.join(root, "fixtures/telemetry-bench/README.md"), "ab") as stream:
            stream.write(b"\n")
        self.assertNotEqual(telemetrylab.authority_digest(registry, root)[0], digest)
        os.symlink("/etc/passwd", os.path.join(root, "fixtures/telemetry-bench/extra"))
        with self.assertRaises(ContractError):
            telemetrylab.authority_digest(registry, root)

    def test_overrides_stay_within_the_caps(self):
        registry, _ = telemetrylab.load_registry()
        for values in ({"rounds": 0}, {"rounds": 9}, {"pairs": 41}, {"pairs": 0},
                       {"experiments": ["made_up"]}, {"scale": "huge"}):
            with self.assertRaises(ContractError):
                telemetrylab.configuration(registry, "manual", **values)
        first = telemetrylab.configuration(registry, "pr")
        self.assertEqual(first, telemetrylab.configuration(registry, "pr"))
        self.assertNotEqual(first["digest"],
                            telemetrylab.configuration(registry, "pr", pairs=4)["digest"])


class BootstrapEquivalence(unittest.TestCase):
    @staticmethod
    def literal(values, seed, label):
        # Independent copy of the original protocol: never lower its resample count.
        rng = random.Random(f'{seed}:{label}')
        count = len(values)
        medians = sorted(statistics.median([values[rng.randrange(count)] for _ in range(count)])
                         for _ in range(10000))
        return [round(medians[250], 3), round(medians[9749], 3)]

    def test_uniform_and_general_inputs_equal_the_literal_10000_resamples(self):
        module = telemetrylab.script_module("scripts/telemetry-overhead.py")
        self.assertEqual(module.RESAMPLES, 10000)
        inputs = ([0.0], [-0.0], [0.0] * 20, [-0.0] * 20,
                  [-1.23456] * 3, [2.34567] * 20, [7] * 4,
                  [-4.5, 0.0, 1.25, 8.75], [0.0, -0.0, 0.0],
                  [1, 1.0], [1.0001, 1.0002, 1.0003],
                  [-9.0, -2.0, -2.0, 0.0, 3.0])
        for values in inputs:
            for seed, label in ((0, "zero"), (20261021, "drain"), (-17, "signed")):
                with self.subTest(values=values, seed=seed, label=label):
                    # JSON also observes the sign of zero and integer/float differences.
                    self.assertEqual(json.dumps(module.bootstrap_interval(values, seed, label)),
                                     json.dumps(self.literal(values, seed, label)))

    def test_uniform_shortcut_avoids_resampling_but_general_input_keeps_the_protocol(self):
        module = telemetrylab.script_module("scripts/telemetry-overhead.py")
        original = module.random.Random
        with mock.patch.object(module.random, "Random", wraps=original) as seeded:
            self.assertEqual(module.bootstrap_interval([1.25] * 20, 123, "same"), [1.25, 1.25])
            seeded.assert_not_called()
        for values in ([0.0, 1.0], [0.0, -0.0], [1, 1.0]):
            with self.subTest(values=values):
                rng = original("123:general")
                with mock.patch.object(rng, "randrange", wraps=rng.randrange) as draw, \
                        mock.patch.object(module.random, "Random", return_value=rng) as seeded:
                    result = module.bootstrap_interval(values, 123, "general")
                seeded.assert_called_once_with("123:general")
                self.assertEqual(draw.call_count, 10000 * len(values))
                self.assertEqual(json.dumps(result), json.dumps(self.literal(values, 123, "general")))
