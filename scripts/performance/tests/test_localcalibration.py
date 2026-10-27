"""Synthetic local originals through the real journal and unchanged whole-trial judge.

These are finite protocol tests, not an empirical cohort or calibration qualification.
"""
import copy
import json
import os
from fractions import Fraction
from unittest import mock

from perflab import (budget, calibration, contracts, executor, gitrefs, localcalibration,
                     planning, schedule, synthetic)
from perflab.values import ContractError, canonical_digest, format_decimal
from tests import calibration_fixtures as fx
from tests.support import TempDir, lab_repo, read, SYNTHETIC_SUITE
from tests.test_executor import config, IMAGE, PRODUCER


class LocalOriginals(TempDir):
    def setUp(self):
        super().setUp()
        # The synthetic suite has a small critical family. Only its runner identity changes;
        # every N/K, case, oracle, tolerance, statistical minimum and target formula stays.
        raw = read(SYNTHETIC_SUITE).replace(b'runner = "github_hosted"', b'runner = "dedicated"')
        self.repo, self.authority = lab_repo(self.path("repo"), raw)
        self.c = config(self.path("journal"), self.repo.path, self.authority)
        self.c["core"].update(cases=[], environment="linux_x86_64_gnu_hosted")
        self.suite = planning.read_suite(gitrefs.Git(self.repo.path), self.authority)
        self.cohort = calibration.declare(self.suite, self.authority, "synthetic_local_cohort",
            "quick", "linux_x86_64_gnu_hosted", "linux_gnu_native_release",
            [{"group_id": "local_cpu", "cpu_model": fx.CPU, "image": IMAGE}], "2026-10-27", "fixture/repo", 0,
            minimum_days=7, local={"journal_id": self.c["journal_id"], "machine_id": self.c["machine_id"],
                "environment_digest": executor.environment(self.c), "producer_digest": PRODUCER})
        self.repo.commit("reviewed declaration", {"fixtures/performance/local-cohort.json":
            contracts.write_bytes(self.cohort, "local-cohort-declaration")})
        self.locks = self.path("locks")
        os.mkdir(self.locks)
        self.producer = mock.patch.object(executor, "producer", return_value=PRODUCER)
        self.producer.start()
        self.addCleanup(self.producer.stop)
        self.now = calibration.start_unix_ms(self.cohort) / 1000 + 3600
        self.clock = mock.patch.object(executor.time, "time", side_effect=lambda: self.now)
        self.clock.start()
        self.addCleanup(self.clock.stop)

    def original(self, journal, member, parameters=None, detection=True):
        self.now = calibration.start_unix_ms(self.cohort) / 1000 + member["day"] * 86400 + 3600
        request = journal.submit("core", self.authority, self.authority,
            canonical_digest(self.cohort) + ":" + member["member_id"], "calibration",
            cohort=self.cohort, member=member["member_id"], parameters=parameters)
        def execute(c, request, path, lease, installed):
            executor.retain_sources(c, request, path, budget.Budget(executor.time.monotonic() + 30,
                                                                    executor.time.monotonic, 0))
            plan = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
            injection = plan["cohort"]["injection"]
            def dose(slot):
                if not injection or slot["side"] != "candidate":
                    return Fraction(0)
                return Fraction(injection["groups"][0]["iterations"][slot["instance_id"]], 20_000)
            def value(slot, case):
                return Fraction(100) + (dose(slot) if detection else 0)
            observed = dict(synthetic.SYNTHETIC_HOST, cpu_model={"value": fx.CPU})
            def edit(record):
                record["host_id"] = contracts.host_id(observed)
                if injection and record["instance_id"] != plan["health"]["workload"]:
                    record["metrics"][calibration.INJECTED_METRIC] = {
                        "value": format_decimal(dose(record)), "unit": "ms"}
                    record["metric_scopes"][calibration.INJECTED_METRIC] = "probe_reported"
                return [record]
            run = os.path.join(path, "run")
            synthetic.write_run(plan, run, value, edit=edit, provenance="measured")
            synthetic.observe(run, plan, fx.CPU, IMAGE)
            return {"status": "complete", "measurement_status": "synthetic", "actions": None,
                "evidence": executor.file_manifest(path, executor.EVIDENCE_PREFIXES),
                "detail": "synthetic whole-trial arithmetic; not empirical evidence"}
        self.assertEqual(journal.cycle(execute=execute, lock_directory=self.locks), "complete")
        return request

    def test_local_and_hosted_declarations_have_distinct_authority_and_unchanged_minima(self):
        self.assertEqual(self.cohort["schedule"]["runners"], "persistent_vm")
        self.assertEqual(self.cohort["schedule"]["minimum_days"], 7)
        self.assertNotIn("after_run_number", self.cohort["dispatch"])
        self.assertEqual(sum(m["kind"] == "aa" for m in self.cohort["members"]), 20)
        self.assertEqual(sum(m["kind"] == "injection" for m in self.cohort["members"]), 30)
        self.assertEqual(sum(m["kind"] == "power" for m in self.cohort["members"]), 10)
        shortened = copy.deepcopy(self.cohort)
        shortened["schedule"]["minimum_days"] = 6
        with self.assertRaises(ContractError):
            contracts.validate_cohort(shortened)
        with executor.Journal(self.c) as journal:
            with self.assertRaises(ContractError):
                localcalibration.collect(journal, dict(self.cohort, schema=contracts.COHORT_SCHEMA))
            # Hosted v7 remains a current, separately declared hosted cohort.
            hosted = contracts.read_file(os.path.join(executor.ROOT,
                "fixtures/performance/cohort-quick-20261028-hosted-v7.json"), "cohort")
            self.assertEqual(hosted["schedule"]["runners"], "fresh_hosted")
            self.assertEqual(hosted["dispatch"]["event"], "workflow_dispatch")

    def test_original_local_journal_and_seal_preserve_whole_trial_denominators(self):
        with executor.Journal(self.c) as journal:
            originals = []
            for member in self.cohort["members"]:
                if member["kind"] in ("aa", "preparation"):
                    originals.append(self.original(journal, member))
            runs, refused, hist = localcalibration.collect(journal, self.cohort)
            self.assertEqual(refused, [])
            parameters = calibration.prepare(self.cohort, runs, calibration.tolerances_of(runs, self.cohort))
            # Trials cannot start with an uncommitted parameter document.
            trial = next(m for m in self.cohort["members"] if m["kind"] == "power")
            self.now = calibration.start_unix_ms(self.cohort) / 1000 + trial["day"] * 86400 + 3600
            with self.assertRaisesRegex(ContractError, "committed"):
                journal.submit("core", self.authority, self.authority, canonical_digest(self.cohort) + ":" + trial["member_id"],
                    "calibration", cohort=self.cohort, member=trial["member_id"], parameters=parameters)
            self.repo.commit("reviewed parameter seal", {"fixtures/performance/local-calibration-parameters.json":
                contracts.write_bytes(parameters, "calibration-parameters")})
            for member in self.cohort["members"]:
                if member["kind"] in ("injection", "power"):
                    self.original(journal, member, parameters, detection=member["member_id"] != "power_01")
            runs, refused, hist = localcalibration.collect(journal, self.cohort, parameters)
            self.assertEqual(refused, [])
            result = calibration.readiness(self.cohort, parameters, runs, refused, hist)
            self.assertEqual(result["schema"], "af.perf-local-readiness/1")
            self.assertEqual(result["tallies"]["aa_members"], 20)
            self.assertEqual(result["tallies"]["power_members"], 10)
            self.assertEqual(result["tallies"]["power_qualifying"], 9)
            self.assertTrue(result["ready"])
            for entry in result["members"]:
                self.assertIn("request_id", entry["provenance"])
                self.assertNotIn("actions_run_id", entry["provenance"])
            first = originals[0]
            self.assertEqual(journal.submit("core", self.authority, self.authority,
                first["submission"]["key"], "calibration", cohort=self.cohort,
                member=first["member_id"]), first)
            self.assertEqual(journal.cycle(execute=lambda *a: self.fail("original rerun"),
                                          lock_directory=self.locks), "idle")
            # The original journal and parameter seal are mandatory, regardless of verdicts.
            forged = copy.deepcopy(result)
            forged["tallies"]["power_members"] = 9
            with self.assertRaises(ContractError):
                contracts.validate_local_readiness(forged, cohort=self.cohort, calibration=parameters)
            changed = copy.deepcopy(parameters)
            changed["groups"][0]["a0_samples"] += 1
            with self.assertRaises(ContractError):
                calibration.readiness(self.cohort, changed, runs, refused, hist)
            original_state = journal.state(first["request_id"])
            state_path = os.path.join(journal.path(first["request_id"]), "state.json")
            uncertain = dict(original_state, state="uncertain", result_sha256=None)
            executor.atomic(state_path, uncertain, "executor-state")
            runs, refused, hist = localcalibration.collect(journal, self.cohort, parameters)
            self.assertNotEqual(runs[first["member_id"]].status, "complete")
            self.assertEqual(len(runs), len(self.cohort["members"]))
