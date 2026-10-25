"""Operator orchestration over synthetic API replies; no credentials, dispatch or hosted claim."""
import base64
import copy
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
from unittest import mock

from perflab import activation, calibration, ci, contracts, transport
from perflab.values import ContractError
from tests import calibration_fixtures as fixtures
from tests import test_performance_workflow as workflow_tests
from tests import test_workflow as workflow_text
from tests.support import CLI, ROOT, TempDir, read, synthetic_suite

SPEC = importlib.util.spec_from_file_location(
    "cohort_dispatch", os.path.join(ROOT, "scripts/performance/cohort-dispatch.py"))
operator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(operator)
# The retained shard's full registry identity, including its OCI separator.
IMAGE = ("ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:"
         "26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2")
NOW = datetime.datetime(2025, 10, 9, 10, tzinfo=datetime.timezone.utc)


class GroupArguments(TempDir):
    def test_declaration_matches_observed_cpu_and_exact_registry_image(self):
        for cpu in ("AMD EPYC 9V74 80-Core Processor",
                    "Intel(R) Xeon(R) Platinum 8370C CPU @ 2.80GHz"):
            group = calibration.parse_group(["hosted", cpu, IMAGE])
            cohort = calibration.declare(
                synthetic_suite(), fixtures.AUTHORITY, "groups", "quick",
                "linux_x86_64_gnu_hosted", "linux_gnu_native_release", [group],
                fixtures.START, fixtures.REPOSITORY, fixtures.ANCHOR)
            cohort = contracts.read_json(contracts.write_bytes(cohort, "cohort"), "cohort")
            observed = {"host": {"cpu_model": {"value": cpu}},
                        "execution": {"sample_image": IMAGE}}
            self.assertEqual(calibration.shard_group(cohort, observed), "hosted")
            injection = calibration.plan_injection(cohort, None,
                                                  {"kind": "preparation", "member_id": "prep_01"},
                                                  ["synthetic_fast"])
            self.assertEqual(calibration.host_group(injection, cpu, IMAGE)["image"], IMAGE)
            for image in ("sha256:" + IMAGE.split("sha256:")[1],
                          IMAGE.replace("/afactory-perf-gnu", "/different"), IMAGE[:-1] + "0"):
                observed["execution"]["sample_image"] = image
                self.assertIsNone(calibration.shard_group(cohort, observed))
                self.assertIsNone(calibration.host_group(injection, cpu, image))

    def test_malformed_groups_and_old_combined_spelling_are_refused(self):
        for parts in ("hosted=CPU@" + IMAGE, ["hosted", "CPU"], ["bad id", "CPU", IMAGE],
                      ["hosted", "", IMAGE], ["hosted", " CPU", IMAGE],
                      ["hosted", "CPU", "ghcr.io/owner/image:latest"],
                      ["hosted", "CPU", "sha256:" + "a" * 64],
                      ["hosted\n", "CPU", IMAGE], ["hosted", "CPU\nmodel", IMAGE],
                      ["hosted", "CPU", IMAGE + "\n"],
                      ["hosted", "CPU", IMAGE + "@sha256:" + "a" * 64]):
            with self.subTest(parts=parts), self.assertRaises(ContractError):
                calibration.parse_group(parts)
        # Exercise argparse too: the old spelling has no alias, and separate values retain @.
        import importlib.util
        spec = importlib.util.spec_from_file_location("perf_cli_group_test", CLI)
        cli = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cli)
        args = cli.parser().parse_args([
            "cohort-declare", "--authority", fixtures.AUTHORITY, "--cohort-id", "groups",
            "--tier", "quick", "--environment", "linux_x86_64_gnu_hosted",
            "--recipe", "linux_gnu_native_release", "--group", "hosted", fixtures.CPU, IMAGE,
            "--start-date", fixtures.START, "--repository", fixtures.REPOSITORY,
            "--after-run-number", "41", "--output", self.path("unused.json")])
        self.assertEqual(calibration.parse_group(args.group[0])["cpu_model"], fixtures.CPU)


class Operator(TempDir):
    def setUp(self):
        super().setUp()
        self.root = Path(self.path("calibration"))
        self.state = self.root / "cohorts" / "synthetic_cohort"
        self.state.mkdir(parents=True)
        lab = fixtures.Lab(self.path("lab"))
        self.cohort = lab.cohort
        self.cohort["dispatch"]["repository"] = operator.REPOSITORY
        self.rows = list(lab.workflow_runs.values())
        self.rows[0]["repository"]["full_name"] = operator.REPOSITORY
        self.control, self.head = "b" * 40, "c" * 40
        self.workflow = read(workflow_tests.WORKFLOW)
        self.config = {
            "schema": "af.perf-local-cohort-dispatcher/1", "enabled": False,
            "cohort_id": self.cohort["cohort_id"],
            "cohort_digest": contracts.canonical_digest(self.cohort),
            "control_sha": self.control,
            "cohort_path": "fixtures/performance/synthetic-operator.json",
            "workflow_sha256": hashlib.sha256(self.workflow).hexdigest()}
        self.cohort_bytes = contracts.write_bytes(self.cohort, "cohort")
        (self.state / "cohort.json").write_bytes(self.cohort_bytes)
        self.config_path = self.root / "config.json"
        self.save_config()
        self.gets, self.posts, self.syncs = [], [], []
        self.final_head = self.head
        self.workflow_at_authority = self.workflow
        self.control_tree = "e" * 40
        self.authority_tree = self.control_tree
        self.remote_cohort = self.cohort_bytes
        self.ancestors = [{"sha": self.control}]
        self.post_returncode = 0
        self.move_at_post = False
        self.api = SimpleNamespace(get_json=self.get)
        frozen_clock = mock.Mock(wraps=datetime.datetime)
        frozen_clock.now.return_value = NOW
        for patch in (mock.patch.object(operator, "ROOT", self.root),
                      mock.patch.object(operator, "LAB", Path(self.tmp)),
                      mock.patch.object(operator, "runtime", return_value=(
                          activation, calibration, contracts, transport)),
                      mock.patch.object(operator.datetime, "datetime", frozen_clock),
                      mock.patch.object(transport, "Collector", return_value=self.api),
                      mock.patch.object(operator.subprocess, "run", side_effect=self.command)):
            patch.start()
            self.addCleanup(patch.stop)
        # Observe real fsyncs and on-disk intent, not an in-memory pretend journal.
        fsync = os.fsync
        def sync(fd):
            self.syncs.append(fd)
            fsync(fd)
        patch = mock.patch.object(operator.os, "fsync", side_effect=sync)
        patch.start()
        self.addCleanup(patch.stop)

    def save_config(self):
        self.config_path.write_text(json.dumps(self.config))

    def get(self, path, purpose):
        self.gets.append((path, purpose))
        if path.startswith("/actions/workflows/performance.yml/runs?"):
            return {"total_count": len(self.rows), "workflow_runs": copy.deepcopy(self.rows)}
        if path == "/git/ref/heads/main":
            return {"object": {"sha": self.final_head if purpose == "main before dispatch"
                              else self.head}}
        if path.startswith("/commits?"):
            return self.ancestors
        if path.startswith("/git/commits/"):
            sha = path.rsplit("/", 1)[1]
            return {"tree": {"sha": ("3" if sha == self.control else "4") * 40}}
        if path.startswith("/contents/"):
            source, ref = path[len("/contents/"):].split("?ref=")
            if source == self.config["cohort_path"]:
                data = self.remote_cohort
            elif source == contracts.DISPATCH_WORKFLOW:
                data = self.workflow_at_authority if ref == self.cohort["authority_sha"] \
                    else self.workflow
            else:
                raise AssertionError(path)
            return {"type": "file", "encoding": "base64", "size": len(data),
                    "content": base64.b64encode(data).decode()}
        if path.startswith("/git/trees/"):
            sha = path.rsplit("/", 1)[1]
            if sha in ("3" * 40, "4" * 40):
                name, tree = "scripts", ("1" if sha == "3" * 40 else "2") * 40
            else:
                name, tree = "performance", self.control_tree if sha == "1" * 40 \
                    else self.authority_tree
            return {"truncated": False, "tree": [
                {"path": name, "sha": tree, "mode": "040000", "type": "tree"}]}
        raise AssertionError(path)

    def command(self, args, **kwargs):
        if args[:3] == ["gh", "auth", "token"]:
            return SimpleNamespace(returncode=0, stdout=b"synthetic-credential")
        self.assertIn("POST", args)
        self.assertTrue(self.syncs, "intent must be fsynced before POST")
        payload = json.loads(kwargs["input"])
        journal = json.loads((self.state / "dispatches.json").read_text())
        self.assertEqual(journal[payload["inputs"]["member"]]["state"], "intent")
        self.assertEqual(journal[payload["inputs"]["member"]]["payload"], payload)
        self.posts.append(payload)
        if self.move_at_post:
            self.final_head = "f" * 40
        return SimpleNamespace(returncode=self.post_returncode)

    def main(self, dry_run=False):
        with mock.patch.object(sys, "argv", ["wrapper", *(["--dry-run"] if dry_run else [])]), \
                mock.patch("builtins.print"):
            code = operator.main()
        return code, json.loads((self.root / "status.json").read_text())

    def assert_no_dispatch(self):
        self.assertEqual(self.posts, [])
        self.assertFalse((self.state / "dispatches.json").exists())

    def test_disabled_dry_run_executes_real_bounded_preflight_and_wrapper_interface(self):
        code, status = self.main(dry_run=True)
        self.assertEqual((code, status["status"]), (0, "would_dispatch"))
        self.assertEqual(status["readiness"], "not_evaluated")
        self.assertTrue((self.state / "runs.json").is_file())
        self.assertIn(("/git/ref/heads/main", "main before dispatch"), self.gets)
        self.assertTrue(any(purpose == "producer tree" for _, purpose in self.gets))
        self.assertFalse(json.loads(self.config_path.read_text())["enabled"])
        self.assert_no_dispatch()

    def test_disabled_non_dry_run_is_a_noop(self):
        code, status = self.main()
        self.assertEqual((code, status["status"], self.gets), (0, "disabled", []))
        self.assert_no_dispatch()

    def test_disabled_dry_run_checks_the_producer_even_when_no_member_is_due(self):
        with mock.patch.object(operator.datetime.datetime, "now",
                               return_value=NOW + datetime.timedelta(days=10)):
            code, status = self.main(dry_run=True)
            self.assertEqual((code, status["status"]), (0, "no_member_due"))
            self.assertTrue(any(purpose == "producer tree" for _, purpose in self.gets))
            self.workflow_at_authority = b"incompatible old workflow"
            code, status = self.main(dry_run=True)
            self.assertEqual((code, status["status"]), (4, "refused"))
            self.assertIn("differs from the cohort authority", status["detail"])
        self.assert_no_dispatch()

    def test_disabled_dry_run_refuses_guard_failures_without_intent(self):
        for guard in ("cohort", "control", "history", "workflow", "scheduler", "main"):
            with self.subTest(guard=guard):
                remote, ancestors, rows = self.remote_cohort, self.ancestors, copy.deepcopy(self.rows)
                if guard == "cohort":
                    self.remote_cohort = b"different"
                elif guard == "control":
                    self.ancestors = []
                elif guard == "history":
                    self.rows[0]["run_number"] += 2
                elif guard == "workflow":
                    self.workflow_at_authority = b"old authority workflow"
                    # Re-pinning the local hash cannot bypass authority equality.
                    self.config["workflow_sha256"] = hashlib.sha256(self.workflow).hexdigest()
                    self.save_config()
                elif guard == "scheduler":
                    self.control_tree = "f" * 40
                elif guard == "main":
                    self.final_head = "f" * 40
                code, status = self.main(dry_run=True)
                self.assertEqual((code, status["status"]), (4, "refused"))
                self.assert_no_dispatch()
                self.remote_cohort, self.ancestors, self.rows = remote, ancestors, rows
                self.workflow_at_authority, self.control_tree = self.workflow, self.authority_tree
                self.final_head = self.head

    def test_latched_disabled_dry_run_runs_all_guards_and_preserves_intent(self):
        member, reason = operator.choose(self.cohort, {}, {}, self.rows, NOW, False)
        self.assertEqual(reason, "due")
        path = self.state / "dispatches.json"
        original = json.dumps({member["member_id"]: {"state": "intent"}}, indent=3).encode()
        path.write_bytes(original)
        code, status = self.main(dry_run=True)
        self.assertEqual(code, 4)
        self.assertEqual(status["status"], "dispatch_uncertain:" + member["member_id"])
        self.assertIn(("/git/ref/heads/main", "main before dispatch"), self.gets)
        self.assertTrue(any(purpose == "producer tree" for _, purpose in self.gets))
        self.assertEqual(path.read_bytes(), original)
        for guard in ("cohort", "control", "history", "workflow", "scheduler", "main"):
            with self.subTest(guard=guard):
                remote, ancestors, rows = self.remote_cohort, self.ancestors, copy.deepcopy(self.rows)
                if guard == "cohort":
                    self.remote_cohort = b"different"
                elif guard == "control":
                    self.ancestors = []
                elif guard == "history":
                    self.rows[0]["run_number"] += 2
                elif guard == "workflow":
                    self.workflow_at_authority = b"different workflow"
                elif guard == "scheduler":
                    self.control_tree = "f" * 40
                else:
                    self.final_head = "f" * 40
                code, status = self.main(dry_run=True)
                self.assertEqual((code, status["status"]), (4, "refused"))
                self.assertEqual(path.read_bytes(), original)
                self.assertEqual(self.posts, [])
                self.remote_cohort, self.ancestors, self.rows = remote, ancestors, rows
                self.workflow_at_authority, self.control_tree = self.workflow, self.authority_tree
                self.final_head = self.head

    def test_enabled_one_shot_fsyncs_intent_then_posts_exact_checked_main_head(self):
        self.config["enabled"] = True
        self.save_config()
        code, status = self.main()
        self.assertEqual((code, status["status"]), (0, "dispatch_submitted"))
        self.assertEqual(len(self.posts), 1)
        self.assertEqual(self.posts[0]["ref"], "main")
        self.assertEqual(self.posts[0]["inputs"]["dispatch_head"], self.head)
        journal = json.loads((self.state / "dispatches.json").read_text())
        self.assertEqual(journal[status["member"]]["state"], "submitted")
        code, status = self.main()
        self.assertEqual(code, 4)
        self.assertTrue(status["status"].startswith("dispatch_uncertain:"))
        self.assertEqual(len(self.posts), 1)

    def test_ambiguous_post_keeps_durable_intent_and_never_retries(self):
        self.config["enabled"] = True
        self.save_config()
        self.post_returncode = 1
        code, status = self.main()
        self.assertEqual((code, status["status"]), (4, "refused"))
        journal_bytes = (self.state / "dispatches.json").read_bytes()
        self.assertEqual(next(iter(json.loads(journal_bytes).values()))["state"], "intent")
        code, status = self.main()
        self.assertEqual(code, 4)
        self.assertTrue(status["status"].startswith("dispatch_uncertain:"))
        self.assertEqual((self.state / "dispatches.json").read_bytes(), journal_bytes)
        self.assertEqual(len(self.posts), 1)

    def test_main_movement_after_final_get_before_post_is_a_latched_non_member(self):
        self.config["enabled"] = True
        self.save_config()
        self.move_at_post = True
        code, status = self.main()
        self.assertEqual((code, status["status"]), (0, "dispatch_submitted"))
        payload = self.posts[0]
        # Execute the actual workflow guard at the SHA that wins the mutable-ref race.
        jobs = workflow_text.jobs(self.workflow.decode())
        step = workflow_tests.step_named(jobs["plan"], "Refuse a calibration source mismatch")
        with mock.patch.object(operator.subprocess, "run", wraps=REAL_RUN):
            result = subprocess.run(["bash", "-e", "-c", workflow_text.block(step, "run")],
                                    env=dict(os.environ, EVENT_NAME="workflow_dispatch",
                                             INPUT_SCOPE="calibration", GITHUB_SHA=self.final_head,
                                             DISPATCH_HEAD=payload["inputs"]["dispatch_head"]),
                                    capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"non-member; no plan, build or measurement", result.stderr)
        expression = next(line for line in self.workflow.decode().splitlines()
                          if line.startswith("run-name: ")).split("${{", 1)[1].rsplit("}}", 1)[0]
        title = eval(expression.replace("&&", "and").replace("||", "or"),
                     {"__builtins__": {}}, {
                         "github": SimpleNamespace(event_name="workflow_dispatch", sha=self.final_head),
                         "inputs": SimpleNamespace(**payload["inputs"]),
                         "format": lambda pattern, *args: pattern.format(*args)})
        self.assertEqual(title, "af-perf source-mismatch")
        # The same mismatch also fails closed if ci-schedule is invoked directly.
        doc, plans = ci.schedule(None, ci.Event("workflow_dispatch", {"inputs": payload["inputs"]},
                                              self.final_head, "refs/heads/main",
                                              operator.REPOSITORY, 8000, 1), ci.Settings())
        self.assertEqual((plans, ci.outputs(doc)["any"]), ({}, "false"))
        self.assertEqual(doc["comparisons"][0]["reason"], "source_mismatch")
        raced = dict(self.rows[0], id=8000, run_number=fixtures.ANCHOR + 1,
                     display_title=title, head_sha=self.final_head, event="workflow_dispatch")
        self.rows.insert(0, raced)
        code, status = self.main()
        self.assertEqual(code, 4)
        self.assertTrue(status["status"].startswith("dispatch_uncertain:"))
        self.assertEqual(len(self.posts), 1)
        self.assertEqual(len(json.loads((self.state / "runs.json").read_text())["workflow_runs"]), 2)

    def test_choose_keeps_utc_cutoff_missing_days_busy_runs_and_sealed_parameter_gate(self):
        choose = lambda now=NOW, rows=(), trials=False: operator.choose(
            self.cohort, {}, {}, rows, now, trials)
        self.assertEqual(choose(NOW.replace(hour=22))[1], "utc_day_closing")
        self.assertEqual(choose(NOW + datetime.timedelta(days=10))[1], "no_member_due")
        self.assertEqual(choose(rows=[{"event": "workflow_dispatch", "head_branch": "main",
                                      "status": "queued"}])[1], "waiting_for_existing_dispatch")
        trials = NOW + datetime.timedelta(days=3)
        self.assertEqual(choose(trials)[1], "sealed_parameters_required")
        self.assertEqual(choose(trials, trials=True)[1], "due")
        # Supplying a path does not bypass the typed, sealed parameter reader.
        self.config["calibration_path"] = "fixtures/performance/parameters.json"
        self.save_config()
        (self.state / "parameters.json").write_text("{}")
        with mock.patch.object(operator.datetime.datetime, "now", return_value=trials):
            code, status = self.main(dry_run=True)
        self.assertEqual((code, status["status"]), (4, "refused"))
        self.assert_no_dispatch()


REAL_RUN = subprocess.run


class InstalledRuntime(TempDir):
    def test_only_the_verified_installed_library_loads_and_tampering_refuses(self):
        folder = Path(self.path("collector", "a" * 64))
        package = folder / "perflab"
        package.mkdir(parents=True)
        entries = []
        for name in ("__init__", "activation", "calibration", "contracts", "transport"):
            path = package / (name + ".py")
            data = b'MARKER = "synthetic-installed-runtime"\n'
            path.write_bytes(data)
            entries.append(["perflab/" + path.name, hashlib.sha256(data).hexdigest()])
        manifest = Path(self.path("collector-manifest.json"))
        manifest.write_text(json.dumps({"collector_sha256": "a" * 64, "files": entries}))
        script = '''import importlib.util, pathlib, sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("operator_under_test", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.LAB = pathlib.Path(sys.argv[2])
modules = module.runtime()
assert all(item.MARKER == "synthetic-installed-runtime" for item in modules)
assert all(pathlib.Path(item.__file__).is_relative_to(module.LAB) for item in modules)
'''
        def run():
            return subprocess.run([sys.executable, "-I", "-B", "-c", script,
                                   operator.__file__, self.tmp], capture_output=True)
        result = run()
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertFalse(list(folder.rglob("*.pyc")))
        (package / "calibration.py").write_bytes(b"raise AssertionError('must not import')\n")
        result = run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"installed runtime changed: perflab/calibration.py", result.stderr)
        entries[0][0] = "../escape.py"
        manifest.write_text(json.dumps({"collector_sha256": "a" * 64, "files": entries}))
        result = run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"runtime path escaped", result.stderr)

    def test_frozen_old_cohort_is_preserved_byte_for_byte(self):
        path = os.path.join(ROOT, "fixtures/performance/cohort-quick-20261023-hosted-v6.json")
        self.assertEqual(hashlib.sha256(read(path)).hexdigest(),
                         "62955096a58517dad3544425ead9b18ebe3b85de94b8518d5ce85deb761d5ab7")
