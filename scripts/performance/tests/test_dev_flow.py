"""Source-verification regressions; no performance measurement or host namespace needed."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from perflab import calibration, cli_cases, contracts, dev_flow, executor, git_refs, planning, runner, schedule, stats, supervise, suite
from perflab.values import ContractError
from tests import contained_flow_probe, dev_flow_fixture
from tests.support import ROOT, TempDir, lab_repo, observe_json
from tests.test_oracle import supervised_reply

FIXTURE = Path(ROOT) / "fixtures/performance/cli/default-light-smoke"
spec = importlib.util.spec_from_file_location("flow_driver", FIXTURE / "driver.py")
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class FlowEvidence(TempDir):
    def transfer(self, treatment="cold"):
        doc = dev_flow_fixture.document(self.path(treatment), treatment)
        root = Path(doc["evidence_root"])
        files = {"work/evidence/" + p.name: p.read_bytes() for p in root.iterdir()
                 if "work/evidence/" + p.name in dev_flow.transfer_paths()}
        doc["evidence_root"] = dev_flow.CONTAINED_EVIDENCE_ROOT
        files["flow.json"] = json.dumps(doc).encode()
        return doc, files

    def test_both_treatments_use_the_same_strict_oracle_after_transfer(self):
        for treatment in ("cold", "primed"):
            _, files = self.transfer(treatment)
            self.assertEqual(dev_flow.validate_transferred(files), treatment)
            self.assertEqual(dev_flow.validate_transferred(files, treatment=treatment), treatment)
            opposite = "primed" if treatment == "cold" else "cold"
            with self.assertRaisesRegex(ValueError, f"expected {opposite}, found {treatment}"):
                dev_flow.validate_transferred(files, treatment=opposite)
        self.assertLessEqual(len(files), 64)

    def test_missing_malformed_unexpected_oversized_and_semantically_false_evidence_fails(self):
        doc, files = self.transfer()
        changes = [lambda f: f.pop("work/evidence/round-0-checks.json"),
                   lambda f: f.update(unexpected=b""),
                   lambda f: f.update({"flow.json": b"{"}),
                   lambda f: f.update({"flow.json": b"[]"}),
                   lambda f: f.update({"flow.json": b"x" * (1048576 + 1)}),
                   lambda f: f.update({"flow.json": b'{"schema":1,"schema":2}'}),
                   lambda f: f.update({"work/evidence/round-0-checks.json": b"{}"})]
        for change in changes:
            with self.subTest(change=change):
                bad = dict(files)
                change(bad)
                with self.assertRaises(ValueError):
                    dev_flow.validate_transferred(bad)
        for key, value in [("final_source", {}), ("rounds", doc["rounds"][:2]),
                           ("commands", list(reversed(doc["commands"]))),
                           ("cache_treatment", "unknown")]:
            bad = dict(files)
            bad["flow.json"] = json.dumps(dict(doc, **{key: value})).encode()
            with self.assertRaises(ValueError):
                dev_flow.validate_transferred(bad)
        with self.assertRaises(ValueError):
            dev_flow.validate_transferred(None)

    def test_actual_host_frame_reads_only_declared_bytes_and_requires_confirmed_cleanup(self):
        _, files = self.transfer()
        order = supervise.request(["/bin/true"], {}, "/perf/scratch", "", 1000, 1024, 2048,
                                  containment="pid_namespace", scratch="/perf/scratch",
                                  evidence_paths=dev_flow.transfer_paths())
        result = json.loads(Path(ROOT, "fixtures/performance/schema/supervisor-result/valid-namespace.json").read_text())
        result["stdout"].update(retained=0, total=0, truncated=0)
        result["evidence"] = {"status": "complete", "detail": "captured before clearing",
                              "files": [{"path": p, "bytes": len(files[p])}
                                        for p in dev_flow.transfer_paths()]}
        payload = b"".join(files[p] for p in dev_flow.transfer_paths())
        def read(doc=result, body=payload):
            return supervise.read_reply(json.dumps(doc).encode() + b"\n" + body, order)
        self.assertEqual(dev_flow.validate_transferred(read()[3]), "cold")
        for suffix in (b"extra",):
            with self.assertRaises(ContractError):
                read(body=payload + suffix)
        for alter in (lambda d: d["lifecycle"].update(status="cleanup_uncertain", remaining_after_cleanup=1),
                      lambda d: d["evidence"]["files"][0].update(path="unexpected")):
            bad = copy.deepcopy(result)
            alter(bad)
            with self.assertRaises(ContractError):
                read(bad)


class RunnerTreatment(TempDir):
    def test_runner_binds_both_evidence_paths_to_the_planned_treatment(self):
        repo, sha = lab_repo(self.path("repo"))
        plan = planning.plan(git_refs.Git(repo.path), planning.Request(
            base=sha, head=sha, authority=sha, scope="bootstrap", tier="e2e_long_running",
            trusted_host_build=True, run_id="flow_binding", seed=12))
        registry = cli_cases.load(str(Path(ROOT) / "fixtures/performance/cli.toml"))
        adapter = registry["adapters"]["af_cli_v4"]
        for mode in (runner.LINUX_CONTAINER, runner.TRUSTED_LOCAL):
            for case in plan["cases"]:
                planned = case["parameters"]["initial_cache"]
                for actual in ("cold", "primed"):
                    with self.subTest(mode=mode, planned=planned, actual=actual):
                        root = Path(self.path(mode, planned, actual))
                        root.mkdir(parents=True)
                        shard = object.__new__(runner.Shard)
                        shard.plan, shard.mode, shard.index = plan, mode, 0
                        shard.host_id, shard.injection = "fixture-host", None
                        shard.receipts = {}
                        shard.options = runner.Options(scratch=str(root))
                        shard.adapters = {"baseline": ("af_cli_v4", adapter)}
                        shard.versions = {"baseline": "0.34.0"}
                        shard.authority = mock.Mock(fixtures=str(FIXTURE.parent))
                        shard.binaries = mock.Mock()
                        shard.binaries.get.return_value = None
                        shard.verified = mock.Mock(return_value="/fixture/af")
                        shard.logs_bytes, shard.log_budget = 0, 0
                        slot = {"side": "baseline", "sequence": 0, "phase": "measured",
                                "instance_id": case["instance_id"], "pair": 0, "position": 0}
                        entry = cli_cases.command(adapter, case["case_id"], case["parameters"])
                        # The stdout always describes the PLANNED workload. Only the complete
                        # side-written evidence changes, including its Cargo freshness bytes.
                        freshness = dev_flow.COLD if planned == "cold" else dev_flow.PRIMED
                        stdout = json.dumps({
                            "schema": "af.perf-dev-flow-summary/1", "accepted_wall_ms": "4000",
                            "mock": dev_flow.BUSINESS,
                            "cargo_fresh": [fresh for fresh, _ in freshness],
                            "cargo_dirty": [dirty for _, dirty in freshness],
                        }).encode()
                        observed = observe_json(cli_cases.selectors(entry), stdout)
                        self.assertTrue(cli_cases.judge(entry, {}, 0, stdout, 0, [], observed)[0])

                        def supervise_sample(order, side, binaries, readonly):
                            local = mode == runner.TRUSTED_LOCAL
                            scratch = Path(order["cwd"]) if local else root
                            document = dev_flow_fixture.document(scratch, actual)
                            # Establish that opposite-treatment evidence is complete and
                            # internally valid before asking the runner to bind it.
                            self.assertEqual(dev_flow.validate(document), actual)
                            files = None
                            if local:
                                (scratch / "flow.json").write_text(json.dumps(document))
                            else:
                                evidence = Path(document["evidence_root"])
                                files = {"work/evidence/" + p.name: p.read_bytes()
                                         for p in evidence.iterdir()
                                         if "work/evidence/" + p.name in dev_flow.transfer_paths()}
                                document["evidence_root"] = dev_flow.CONTAINED_EVIDENCE_ROOT
                                files["flow.json"] = json.dumps(document).encode()
                                self.assertEqual(dev_flow.validate_transferred(files), actual)
                            reply = supervised_reply(stdout, lifecycle=(
                                "no_observed_descendants" if local else "clean"))
                            reply.result["stdout_json"] = observed
                            if not local:
                                reply.result["evidence"] = {"status": "complete", "detail": "",
                                    "files": [{"path": p, "bytes": len(files[p])}
                                              for p in dev_flow.transfer_paths()]}
                            return reply._replace(evidence=files), None

                        shard.supervise = supervise_sample
                        record = shard.process_sample(slot, case, ("/fixture/af", "sha256:af"))
                        if planned == actual:
                            self.assertEqual(record["status"], "ok", record["failure"])
                            self.assertEqual(record["oracle"]["status"], "passed")
                        else:
                            self.assertEqual(record["status"], "failed")
                            self.assertEqual(record["oracle"]["status"], "failed")
                            self.assertEqual(record["failure"]["kind"], "oracle_mismatch")
                            self.assertEqual(record["oracle"]["detail"],
                                             f"expected {planned}, found {actual}")


class Dose(TempDir):
    def instance(self, dose):
        args = type("Args", (), dict(af=sys.executable, work=self.path("work"),
             output=self.path("flow.json"), cache_seed=None, prime_only=False,
             primed=False, telemetry="off", rustup_home="/rustup", cargo_bin="/cargo"))()
        with mock.patch.dict(os.environ, {driver.INJECTION_ENV: dose}):
            return driver.Driver(args)

    def test_canonical_bounded_count_distinguishes_absent_and_zero(self):
        self.assertIsNone(driver.calibration_iterations(None))
        for value in (0, 1, 2**40):
            self.assertEqual(driver.calibration_iterations(str(value)), value)
        for value in ("", "00", "+1", "-1", " 1", "1.0", "1e2", str(2**40+1)):
            with self.assertRaises(ValueError):
                driver.calibration_iterations(value)

    def test_dose_is_one_bounded_rust_compilation_and_never_nested_environment(self):
        selected = self.instance(str(2**40))
        self.assertNotIn(driver.INJECTION_ENV, selected.env)
        selected.work.mkdir()
        with mock.patch.object(selected, "invoke", return_value=(None, 123)) as invoke:
            selected.inject_calibration()
        self.assertEqual(invoke.call_count, 1)
        self.assertEqual(invoke.call_args.args[1][0], "rustc")
        self.assertNotIn(driver.INJECTION_ENV, invoke.call_args.kwargs["env"])
        source = (selected.work / "calibration/injection.rs").read_text()
        self.assertLess(len(source), 1024)
        self.assertIn("while step < 1099511627776", source)
        self.assertFalse((selected.repo / "calibration").exists())

    def test_invocation_strips_even_an_explicit_child_environment_and_bounds_output(self):
        selected = self.instance("0")
        selected.repo.mkdir(parents=True)
        selected.evidence.mkdir()
        env = dict(os.environ, **{driver.INJECTION_ENV: "2"})
        result, _ = selected.invoke("env", [sys.executable, "-I", "-c",
             "import os; assert 'AF_PERF_CALIBRATION_ITERATIONS' not in os.environ"], env=env)
        self.assertEqual(result.returncode, 0)
        with self.assertRaisesRegex(RuntimeError, "output exceeded"):
            selected.invoke("overflow", [sys.executable, "-I", "-c", "print('x'*300000)"])


class LongPlanning(TempDir):
    def test_eight_pairs_are_balanced_and_standard_minimums_stay_twelve(self):
        repo, sha = lab_repo(self.path("repo"))
        def plan(tier="e2e_long_running", pairs=None):
            return planning.plan(git_refs.Git(repo.path), planning.Request(
                base=sha, head=sha, authority=sha, scope="bootstrap", tier=tier,
                pairs=pairs, trusted_host_build=True, run_id="long_flow", seed=12))
        doc = plan()
        self.assertEqual(doc["verdict_policy"]["algorithm"], stats.LONG_ALGORITHM)
        self.assertEqual([c["pairs"] for c in doc["cases"]], [8, 8])
        self.assertEqual(doc["limits"]["build_minutes"], 45)
        # The serial backend consumes the same frozen allowance, without fitting by
        # shortening either 90-minute shard or extending the 180-minute deadline.
        self.assertEqual(executor.serial_bounds(doc, 10800)["build_seconds"], 2700)
        with self.assertRaisesRegex(ContractError, "infeasible on the serial local backend"):
            executor.require_serial(doc, 10800)
        self.assertEqual(doc["limits"]["shard_sampling_minutes"], 90)
        self.assertEqual(doc["limits"]["run_total_minutes"], 180)
        self.assertTrue(all(c["warmup_pairs"] == 2 and c["sample_timeout_ms"] == 120000
                            for c in doc["cases"]))
        for case in doc["cases"]:
            self.assertEqual(case["measured_orders"].count("ab"), 4)
            self.assertEqual(case["measured_orders"].count("ba"), 4)
        slots = [slot for i in range(len(doc["shards"])) for slot in schedule.expected_slots(doc, i)]
        self.assertEqual(sum(slot["phase"] == "measured" for slot in slots), 32)
        self.assertIsNotNone(stats.critical_rank(8, 1, stats.Fraction(1, 40)))
        for change in (lambda d: d["trigger"].update(kind="nightly"),
                       lambda d: d["verdict_policy"].update(algorithm=stats.ALGORITHM)):
            bad = copy.deepcopy(doc)
            change(bad)
            bad["plan_digest"] = contracts.plan_digest(bad)
            with self.assertRaises(ContractError):
                contracts.validate_run(bad)
        for tier in ("quick", "nightly", "release"):
            ordinary = plan(tier)
            self.assertEqual(ordinary["limits"]["build_minutes"], 20, tier)
            self.assertEqual(executor.serial_bounds(ordinary, 10800)["build_seconds"], 1200)
            contracts.validate_run(contracts.read_json(contracts.write_bytes(ordinary, "run"), "run"))
            with self.assertRaisesRegex(ContractError, "12..200"):
                plan(tier, 8)
        with self.assertRaises(ContractError):
            plan(pairs=6)
        self.assertEqual(cli_cases.catalog_problems(str(FIXTURE / "repo")), [])


class ProvisionedProbe(unittest.TestCase):
    def test_fixture_capabilities_and_readable_binds_are_scoped_and_removed(self):
        argv = contained_flow_probe.command("name", "/binary", lab="/lab")
        self.assertIn("SYS_ADMIN", argv)
        self.assertIn("--read-only", argv)
        self.assertNotIn("--privileged", argv)
        self.assertIn("/lab", " ".join(argv))
        def run(argv, **kwargs):
            mounts = [x for x in argv if x.startswith("type=bind,src=")]
            for mount in mounts:
                path = Path(mount.split("src=",1)[1].split(",",1)[0])
                self.assertEqual(path.stat().st_mode & 0o777, 0o755)
            return subprocess.CompletedProcess(argv, 0, json.dumps(contained_flow_probe.EXPECTED).encode(), b"")
        removal = type("Removal", (), {"confirmed": True, "detail": "gone"})()
        with tempfile.TemporaryDirectory() as temp:
            binary = Path(temp, "supervisor")
            binary.write_bytes(b"fixture")
            with mock.patch.object(contained_flow_probe.subprocess, "run", side_effect=run), \
                 mock.patch.object(contained_flow_probe.supervise, "remove_container", return_value=removal) as remove:
                self.assertEqual(contained_flow_probe.run(binary), contained_flow_probe.EXPECTED)
                remove.assert_called_once()
        self.assertIn("contained_flow_probe.run", Path(ROOT, "scripts/performance/tests/telemetry_measurement_smoke.py").read_text())
        production = supervise.container_argv
        self.assertNotIn("SYS_ADMIN", __import__("inspect").getsource(production))


class PrimeAndProbeFailures(TempDir):
    def test_prime_only_never_injects_calibration(self):
        selected = Dose.instance(self, "99")
        selected.prime_only = True
        with mock.patch.object(selected, "prepare", side_effect=lambda: selected.evidence.mkdir(parents=True)), \
             mock.patch.object(selected, "inject_calibration") as inject, \
             mock.patch.object(selected, "json_invoke", return_value=({"task_id": "smoke-0"}, 1)), \
             mock.patch.object(selected, "preserve_cargo", return_value={}):
            self.assertEqual(selected.run()["schema"], "af.perf-dev-flow-seed/1")
            inject.assert_not_called()

    def test_probe_launch_failure_timeout_and_unconfirmed_removal_propagate(self):
        binary = Path(self.path("binary"))
        binary.write_bytes(b"fixture")
        owner = mock.Mock()
        confirmed = type("Removal", (), {"confirmed": True, "detail": "gone"})()
        uncertain = type("Removal", (), {"confirmed": False, "detail": "unconfirmed"})()
        for action, removal in ((subprocess.CompletedProcess([], 1, b"", b"failed"), confirmed),
                                (subprocess.TimeoutExpired("docker", 1), confirmed),
                                (subprocess.CompletedProcess([], 0, json.dumps(contained_flow_probe.EXPECTED).encode(), b""), uncertain)):
            with self.subTest(action=action):
                options = {"side_effect": action} if isinstance(action, BaseException) else {"return_value": action}
                with mock.patch.object(contained_flow_probe.subprocess, "run", **options), \
                     mock.patch.object(contained_flow_probe.supervise, "remove_container", return_value=removal) as remove:
                    with self.assertRaises((AssertionError, subprocess.TimeoutExpired)):
                        contained_flow_probe.run(binary, owner)
                    remove.assert_called_once()
                    owner.finish.assert_called_with(owner.begin.call_args.args[0], removal.confirmed)


class CalibrationClass(TempDir):
    def test_long_and_standard_cohort_counts_and_claims_remain_distinct(self):
        loaded = suite.load(str(Path(ROOT) / "fixtures/performance/suite.toml"))
        cohort = calibration.declare(loaded, "a" * 40, "long_cohort", "e2e_long_running",
             "linux_x86_64_gnu_hosted", "linux_gnu_native_release",
             [{"group_id": "cpu", "cpu_model": "Fixture CPU", "image": "sha256:" + "1" * 64}],
             "2026-11-15", "owner/repo", 0, minimum_days=5, preparation_members=1)
        self.assertEqual(cohort["schema"], "af.perf-cohort/5")
        self.assertEqual(cohort["execution_class"], "long_running")
        self.assertEqual([sum(m["kind"] == kind for m in cohort["members"])
                          for kind in ("aa", "preparation", "injection", "power")], [4, 1, 4, 4])
        self.assertEqual(cohort["target_formulas"]["readiness"], contracts.LONG_READINESS_FORMULA)
        self.assertEqual(contracts.cohort_counts({"execution_class": "standard"}),
                         ({"aa": 20, "power": 10}, 10, 9))
        bad = copy.deepcopy(cohort)
        bad["tier"] = "quick"
        with self.assertRaises(ContractError):
            contracts.validate_cohort(bad)
        bad = copy.deepcopy(cohort)
        bad["target_formulas"] = contracts.TARGET_FORMULAS
        with self.assertRaises(ContractError):
            contracts.validate_cohort(bad)
