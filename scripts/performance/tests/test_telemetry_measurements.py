"""Physical measurement refusal at the real controller and offline verifier boundaries.

Synthetic documents and executors only: no workload, statistics or retry policy changes.
"""
import copy
import html
import json
import os

from perflab import contracts, telemetryexec, telemetrylab
from perflab.values import ContractError, sha256_bytes
from tests import telemetry_fixtures as fx
from tests.support import TempDir, read


class NumericAdmission(TempDir):
    @classmethod
    def setUpClass(cls):
        cls.registry, _ = telemetrylab.load_registry()
        cls.documents = {
            "overhead": fx.overhead_doc(20261021, 3),
            "invocation_cost": fx.invocation_doc(20261021, 3),
            "read_latency": fx.latency_doc(10),
        }

    def refused(self, experiment, change, field, reason="negative"):
        doc = copy.deepcopy(self.documents[experiment])
        change(doc)
        data = json.dumps(doc).encode()
        with self.assertRaises(telemetrylab.MeasurementRefusal) as caught:
            telemetrylab.read_document(experiment, data, "test", self.registry)
        error = caught.exception
        self.assertEqual((error.field, error.reason), (field, reason))
        evidence = telemetrylab.measurement_evidence(error, data)
        self.assertEqual(evidence["document_sha256"], sha256_bytes(data))
        run = {"experiment": experiment, "round": 0, "side": "candidate",
               "measurement_refusal": evidence}
        run["detail"] = telemetrylab.measurement_detail(run, evidence)
        telemetrylab.verify_measurement_refusal(run, self.registry)
        return evidence

    def test_each_physical_raw_measurement_is_checked_before_its_median(self):
        # Changing just the first of three samples leaves positive medians. None may hide
        # the invalid original. Both off/spans and both invocation shapes are checked.
        for level in ("off", "spans"):
            for metric in ("wall_ms", "peak_rss_bytes", "segments"):
                with self.subTest(experiment="overhead", level=level, metric=metric):
                    self.refused("overhead", lambda d: d["fixtures"][0]["pairs"][0][
                        level].update({metric: -1}),
                        "fixtures[0].pairs[0].%s.%s" % (level, metric))
            self.refused("overhead", lambda d: d["drain"]["pairs"][0].update(
                {level + "_ms": -0.25}), "drain.pairs[0].%s_ms" % level)
            for shape in ("json", "stream-json"):
                for metric in ("wall_ms", "peak_rss_bytes", "segments", "stdout_bytes",
                               "drain_ms"):
                    with self.subTest(level=level, shape=shape, metric=metric):
                        self.refused("invocation_cost", lambda d: d["cases"][0]["levels"][
                            level]["pairs"][0][shape].update({metric: -1}),
                            "cases[0].levels.%s.pairs[0].%s.%s" % (level, shape, metric))
        for temperature in ("warm", "cold"):
            for metric in ("latency_ms", "peak_rss_bytes", "index_bytes", "samples_ms"):
                def change(doc):
                    item = doc["measurements"][1]
                    if temperature == "cold":
                        item["cold"] = copy.deepcopy(item["warm"])
                    item[temperature][metric] = [-1, 11, 12] if metric == "samples_ms" else -1
                self.refused("read_latency", change, "measurements[1].%s.%s%s" % (
                    temperature, metric, "[0]" if metric == "samples_ms" else ""))

    def test_nonfinite_zero_fractional_and_unrepresentable_measurements(self):
        for value in (float("inf"), float("-inf"), float("nan")):
            evidence = self.refused("overhead", lambda d: d["drain"]["pairs"][0].update(
                off_ms=value), "drain.pairs[0].off_ms", "nonfinite")
            self.assertIn(evidence["token"], ("Infinity", "-Infinity", "NaN"))
        self.refused("invocation_cost", lambda d: d["cases"][0]["levels"]["off"][
            "pairs"][0]["json"].update(wall_ms=0),
            "cases[0].levels.off.pairs[0].json.wall_ms", "not_positive")
        self.refused("overhead", lambda d: d["fixtures"][0]["pairs"][0]["off"].update(
            peak_rss_bytes=1.5), "fixtures[0].pairs[0].off.peak_rss_bytes", "not_integer")
        for value in (1e100, 1e-100):
            self.refused("overhead", lambda d: d["drain"]["pairs"][0].update(off_ms=value),
                         "drain.pairs[0].off_ms", "unrepresentable")
        data = json.dumps(self.documents["overhead"]).encode().replace(b'"off_ms": 0.5',
                                                                         b'"off_ms": 1e999')
        with self.assertRaises(telemetrylab.MeasurementRefusal) as caught:
            telemetrylab.read_document("overhead", data, "overflow", self.registry)
        self.assertEqual(caught.exception.token, "1e999")
        self.assertEqual(caught.exception.reason, "nonfinite")
        # A non-finite derived figure also refuses before summarize can overwrite it.
        self.refused("overhead", lambda d: d["fixtures"][0].update(median_pct=float("inf")),
                     "fixtures[0].median_pct", "nonfinite")

    def test_numeric_token_evidence_is_bounded_and_original_spelling_is_retained(self):
        original = "-" + "1" * 600
        data = json.dumps(self.documents["overhead"]).encode().replace(
            b'"off_ms": 0.5', ('"off_ms": ' + original).encode())
        with self.assertRaises(telemetrylab.MeasurementRefusal) as caught:
            telemetrylab.read_document("overhead", data, "oversized", self.registry)
        evidence = telemetrylab.measurement_evidence(caught.exception, data)
        self.assertEqual(evidence["token"], original[:512])
        self.assertEqual(evidence["token_bytes"], len(original))
        self.assertEqual(evidence["token_sha256"], sha256_bytes(original.encode()))
        self.assertEqual(evidence["reason"], "token_too_long")
        run = {"experiment": "overhead", "round": 0, "side": "baseline",
               "measurement_refusal": evidence}
        run["detail"] = telemetrylab.measurement_detail(run, evidence)
        telemetrylab.verify_measurement_refusal(run, self.registry)
        evidence["token"] = "." * 512
        run["detail"] = telemetrylab.measurement_detail(run, evidence)
        with self.assertRaises(ContractError):
            telemetrylab.verify_measurement_refusal(run, self.registry)

    def test_unrepresentable_exact_median_is_refused_during_run_admission(self):
        doc = fx.overhead_doc(20261021, 2)
        for pair, value in zip(doc["drain"]["pairs"], (1e-78, 2e-78)):
            pair["off_ms"] = value
        doc = telemetrylab.script_module("scripts/telemetry-overhead.py").summarize(doc)
        # Both physical samples individually fit. Their exact median needs 81 characters,
        # beyond the existing result decimal contract, so admission owns this failure.
        telemetrylab.validate_measurements("overhead", doc, self.registry)
        with self.assertRaisesRegex(ContractError, "derived physical median"):
            telemetrylab.admit("overhead", doc, fx.expect("overhead"), self.registry)

    def test_signed_differences_percentages_and_intervals_are_valid(self):
        doc = copy.deepcopy(self.documents["overhead"])
        for fixture in doc["fixtures"]:
            for pair in fixture["pairs"]:
                pair["spans"]["wall_ms"] = pair["off"]["wall_ms"] / 2
        for pair in doc["drain"]["pairs"]:
            pair["spans_ms"] = 0.25
        doc = telemetrylab.script_module("scripts/telemetry-overhead.py").summarize(doc)
        self.assertLess(doc["drain"]["median_ms"], 0)
        self.assertLess(doc["fixtures"][0]["median_pct"], 0)
        telemetrylab.read_document("overhead", json.dumps(doc).encode(), "signed", self.registry)
        telemetrylab.admit("overhead", doc, fx.expect("overhead", pairs=3), self.registry)
        doc = copy.deepcopy(self.documents["invocation_cost"])
        for case in doc["cases"]:
            for entry in case["levels"].values():
                for pair in entry["pairs"]:
                    pair["stream-json"].update(wall_ms=450.0, peak_rss_bytes=500, drain_ms=0.1)
                    pair["json"]["drain_ms"] = 0.3
        doc = telemetrylab.script_module("scripts/telemetry-invocation-cost.py").summarize(doc)
        telemetrylab.read_document("invocation_cost", json.dumps(doc).encode(), "signed",
                                   self.registry)
        telemetrylab.admit("invocation_cost", doc, fx.expect("invocation_cost", pairs=3),
                          self.registry)


class ControllerRefusals(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.binaries = fx.binaries(self)

    def test_mixed_runs_retain_originals_and_replay_through_upload_and_history(self):
        def negative(doc):
            doc["drain"]["pairs"][0]["spans_ms"] = -0.125
            # Free-form candidate strings are deliberately absent from the projection.
            doc["af"]["path"] = "private candidate text must not be published"
        def faster(execution):
            doc = fx.overhead_doc(20261022, 3, slow=0.5)
            data = json.dumps(doc).encode()
            return telemetryexec.Outcome(0, data, len(data), b"", 0, 2000000)
        executor = fx.StandIn(self.binaries, {
            ("overhead", "baseline", 20261021): {"extra": negative},
            ("overhead", "candidate", 20261021): {"extra": negative},
            ("overhead", "candidate", 20261022): faster,
        })
        result = telemetrylab.run(fx.settings(self, executor, experiments=["overhead"], pairs=3))
        self.assertEqual(result["status"], "partial")
        self.assertEqual(result["execution"]["cleanup"], "confirmed")
        self.assertFalse(os.path.exists(self.path("result.work")))
        refused = [run for run in result["runs"] if run["status"] == "refused_document"]
        self.assertEqual({run["side"] for run in refused}, {"baseline", "candidate"})
        for run in refused:
            self.assertIsNone(run["document"])
            self.assertIsNone(run["judgment"])
            evidence = run["measurement_refusal"]
            self.assertEqual(evidence["field"], "drain.pairs[0].spans_ms")
            self.assertEqual(evidence["token"], "-0.125")
            self.assertIn("round-0/" + run["side"], run["detail"])
        self.assertTrue(all(entry["round"] == 1 for comp in result["comparisons"]
                            for entry in comp["rounds"]))
        self.assertTrue(any(comp["median_change"].startswith("-")
                            for comp in result["comparisons"] if comp["median_change"]))
        self.assertEqual(telemetrylab.verify(self.path("result"))[0], result)
        self.assertEqual(telemetrylab.admit_for_upload(self.path("result"))[0], "admitted")
        history = telemetrylab.history(self.path("history"), [self.path("result")])
        self.assertEqual(history["results"][0]["status"], "partial")
        self.assertEqual(history["refused"], [])
        for name in (telemetrylab.RESULT_JSON, telemetrylab.REPORT_HTML, telemetrylab.SUMMARY_MD):
            data = read(self.path("result", name))
            self.assertIn("drain.pairs[0].spans_ms", html.unescape(data.decode()))
            self.assertNotIn(b"private candidate text", data)

        # All hashes/rendered outputs agree after each forgery. The numeric refusal must
        # still replay and cannot be reassigned to a legitimately signed derived field.
        for change in (
            lambda e: e.update(token="0.125", token_bytes=5,
                               token_sha256=sha256_bytes(b"0.125")),
            lambda e: e.update(field="drain.median_ms"),
            lambda e: e.update(token_sha256="sha256:" + "0" * 64),
            lambda e: e.update(field="af.path"),
        ):
            forged = copy.deepcopy(result)
            run = next(run for run in forged["runs"] if run["measurement_refusal"])
            change(run["measurement_refusal"])
            run["detail"] = telemetrylab.measurement_detail(run, run["measurement_refusal"])
            fx.rewrite(self.path("result"), forged)
            with self.assertRaises(ContractError):
                telemetrylab.verify(self.path("result"))
            self.assertEqual(telemetrylab.admit_for_upload(self.path("result"))[0], "refused")

    def test_all_refused_runs_finalize_as_failed_and_verify_offline(self):
        def nonfinite(execution):
            seed = int(execution.argv[execution.argv.index("--seed") + 1])
            doc = fx.overhead_doc(seed, 3)
            data = json.dumps(doc).encode().replace(b'"spans_ms": 0.75',
                                                   b'"spans_ms": -1e999')
            return telemetryexec.Outcome(0, data, len(data), b"original stderr", 15, 2000000)
        executor = fx.StandIn(self.binaries, {
            ("overhead", side): nonfinite for side in ("baseline", "candidate")})
        result = telemetrylab.run(fx.settings(self, executor, experiments=["overhead"],
                                             pairs=3, rounds=1))
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["within"], [])
        self.assertEqual(result["comparisons"], [])
        self.assertTrue(all(run["status"] == "refused_document" for run in result["runs"]))
        self.assertTrue(all(run["measurement_refusal"]["token"] == "-1e999"
                            for run in result["runs"]))
        self.assertEqual(telemetrylab.verify(self.path("result"))[0], result)
        self.assertEqual(telemetrylab.admit_for_upload(self.path("result"))[0], "admitted")
        history = telemetrylab.history(self.path("history"), [self.path("result")])
        self.assertEqual(history["results"][0]["status"], "failed")
        self.assertEqual(result["execution"]["cleanup"], "confirmed")
        contracts.validate_telemetry_result(result)
        forged = copy.deepcopy(result)
        forged["runs"][0]["judgment"] = {"verdict": "pass", "reasons": []}
        with self.assertRaises(ContractError):
            contracts.validate_telemetry_result(forged)
