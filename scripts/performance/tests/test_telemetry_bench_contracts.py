"""The telemetry bench scripts' drain boundary and current fixture contract.

The drain uses two `CLOCK_MONOTONIC` readings. Current workload admission checks package
identity, exact current wire contracts and the measured binary planner without timing anything.
"""
import copy
import json
import os
import pathlib
import time

from perflab import telemetry_lab, toml_file
from tests.support import TempDir

overhead = telemetry_lab.script_module("scripts/telemetry-overhead.py")
class Drain(TempDir):
    def store(self, lines, reply_mtime_ns=None):
        """A Store whose CAS holds the dense harness's answer and its stderr."""
        state = pathlib.Path(self.tmp) / "store"
        objects = state / "cas" / "objects" / "ab"
        objects.mkdir(parents=True)
        reply = objects / "reply"
        reply.write_bytes(overhead.REPLY)
        if reply_mtime_ns is not None:
            os.utime(reply, ns=(reply_mtime_ns, reply_mtime_ns))
        (objects / "stderr").write_bytes(b"".join(json.dumps(line, separators=(",", ":"))
                                                  .encode() + b"\n"
                                                  for line in lines))
        return state

    def test_the_drain_is_ordered_monotonic_readings_whatever_the_file_timestamps(self):
        # A de-duplicated answer object may keep an earlier run's modification time, and the
        # wall clock may step back: neither reaches the drain, which reads only the harness's
        # and the measurement's CLOCK_MONOTONIC.
        written = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        state = self.store([{"type": "assistant"},
                            {"type": "result", "subtype": "af_bench_last_write",
                             "last_write_unix_ns": str(time.time_ns() + 10 ** 12),
                             "last_write_monotonic_ns": str(written)}],
                           reply_mtime_ns=1)
        self.assertEqual(overhead.last_write(state), written)
        exited = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        self.assertGreaterEqual(exited, written)
        self.assertGreaterEqual(overhead.drain_latency(written, exited), 0)
        self.assertEqual(overhead.drain_latency(1_000_000, 3_500_000), 2.5)

    def test_a_negative_raw_drain_is_kept_as_measured(self):
        self.assertEqual(overhead.drain_latency(3_000_000, 1_000_000), -2.0)

    def test_a_harness_without_a_monotonic_last_write_is_refused(self):
        # The superseded file-clock spelling is not read.
        state = self.store([{"type": "result", "subtype": "af_bench_last_write",
                             "last_write_unix_ns": "5", "last_write_fs_ns": "5"}])
        with self.assertRaisesRegex(overhead.BenchError, "CLOCK_MONOTONIC"):
            overhead.last_write(state)

    def test_judge_refuses_another_drain_boundary_or_clock(self):
        from tests import telemetry_fixtures as fx
        doc = fx.overhead_doc(20261021, 3)
        for key, value in (("boundary", "cas_commit"), ("clock", "filesystem")):
            changed = copy.deepcopy(doc)
            changed["drain"][key] = value
            with self.assertRaisesRegex(overhead.BenchError, "other boundaries"):
                overhead.summarize(changed)


class FixtureContracts(TempDir):
    def test_only_the_current_contract_is_selected_without_a_release_table(self):
        self.assertEqual(overhead.select_contract(), "native-task-review@1")
        self.assertEqual(overhead.select_contract(overhead.CURRENT_CONTRACT), overhead.CURRENT_CONTRACT)
        for retired in ("check-receipt@1", "check-receipt@2", "0.15.1-rc.1", "unknown"):
            with self.subTest(contract=retired), self.assertRaisesRegex(
                    overhead.BenchError, "unsupported fixture contract"):
                overhead.select_contract(retired)
        with self.assertRaisesRegex(overhead.BenchError, "not `af VERSION`"):
            overhead.release_version("0.15.1-rc.1")

    def test_workspace_and_rc2_release_guard_keep_the_current_native_contract(self):
        from tests.support import ROOT
        preflight = telemetry_lab.script_module("scripts/release-contract.py")
        manifest = toml_file.loads((pathlib.Path(ROOT) / "Cargo.toml").read_bytes(), "Cargo.toml")
        release = manifest["workspace"]["package"]["version"]
        self.assertEqual(preflight.check(), release)
        self.assertIn(release, overhead.CURRENT_RELEASES)
        self.assertIn("0.15.1-rc.2", overhead.CURRENT_RELEASES)
        self.assertEqual(overhead.select_contract(), "native-task-review@1")
        self.assertEqual(overhead.CHECK_RECEIPT_CURRENT, "af/TaskCheckReceipt@2")

    def test_current_receipt_and_release_provenance_are_checked_and_old_documents_refused(self):
        from tests import telemetry_fixtures as fx
        for module, make in ((overhead, fx.overhead_doc),
                             (telemetry_lab.script_module("scripts/telemetry-invocation-cost.py"),
                              fx.invocation_doc)):
            doc = make(1, 1, version="af 0.15.1-rc.1")
            self.assertEqual(module.summarize(copy.deepcopy(doc)), doc)
            for key, value in (("name", "check-receipt@2"),
                               ("check_receipt", "af/TaskCheckReceipt@1"),
                               ("release", "0.0.0")):
                changed = copy.deepcopy(doc)
                changed['fixture_contract'][key] = value
                with self.subTest(script=module.SCHEMA, field=key), self.assertRaises(overhead.BenchError if module is overhead else module.BenchError):
                    module.summarize(changed)
            recorded = pathlib.Path(self.path('document.json'))
            doc['schema'] = module.SCHEMA[:-1] + '2'
            recorded.write_text(json.dumps(doc))
            with self.assertRaisesRegex(module.BenchError, "not an"):
                module.judge(recorded)

    def test_every_fixture_has_exact_current_package_pins_and_no_rewrite(self):
        from perflab import cli_cases
        from unittest import mock
        import re
        for source in sorted(overhead.FIXTURES_DIR.glob('*/repo')):
            with self.subTest(fixture=source.parent.name):
                self.assertEqual(cli_cases.catalog_problems(str(source)), [])
                catalog = (source / '.af/task-catalog.toml').read_text()
                pins = {path: digest for digest, path in re.findall(
                    r'digest = "([^"]+)"\npath = "([^"]+)"', catalog)}
                before = {str(p.relative_to(source)): p.read_bytes() for p in source.rglob('*')
                          if p.is_file()}
                with mock.patch.object(overhead, 'af_digest', side_effect=lambda af, repo, path: pins[path]):
                    produced = overhead.apply_contract('unused', source.parent.name, source,
                                                       overhead.CURRENT_CONTRACT)
                self.assertEqual(sorted(produced['packages'].values()), sorted(pins.values()))
                self.assertEqual(before, {str(p.relative_to(source)): p.read_bytes()
                                         for p in source.rglob('*') if p.is_file()})
                with mock.patch.object(overhead, 'af_digest', return_value='sha256:'+'0'*64):
                    with self.assertRaisesRegex(overhead.BenchError, 'not the declared'):
                        overhead.apply_contract('unused', source.parent.name, source,
                                                overhead.CURRENT_CONTRACT)

    def test_binary_planner_refusal_keeps_the_original_diagnostic_without_measured_output(self):
        from unittest import mock
        from types import SimpleNamespace
        repo = pathlib.Path(self.path('fixture/pristine'))
        repo.mkdir(parents=True)
        refused = SimpleNamespace(returncode=1, stdout=b'{"schema":"af/error@2"}',
                                  stderr=b'Pipeline af.pipeline/6 is unsupported')
        with mock.patch.object(overhead.subprocess, 'run', return_value=refused) as launch:
            with self.assertRaisesRegex(overhead.BenchError, 'Pipeline af.pipeline/6 is unsupported'):
                overhead.admit_fixture('unsupported-af', 'telemetry-bench-concurrent', repo)
        self.assertNotIn('--execute', launch.call_args.args[0])
        self.assertFalse((repo.parent / 'admission').exists())
        self.assertFalse((repo.parent / '00-off/stdout').exists())

    def test_runtime_smoke_keeps_early_exit_instead_of_reading_absent_task_output(self):
        from tests import telemetry_runtime_smoke as smoke
        from perflab.telemetry_exec import Outcome
        outcome = Outcome(1, b'', 0, b'unsupported current fixture contract: original reason', 52, 0)
        artifact = pathlib.Path(self.tmp) / 'bench/telemetry-bench-concurrent/00-off/stdout'
        for content in (None, b'not JSON'):
            with self.subTest(content=content):
                if content is not None:
                    artifact.parent.mkdir(parents=True)
                    artifact.write_bytes(content)
                with self.assertRaisesRegex(AssertionError, 'original reason') as caught:
                    smoke.failed_task_document(pathlib.Path(self.tmp), outcome)
                self.assertNotIsInstance(caught.exception, FileNotFoundError)

    def test_successful_binary_admission_requires_a_current_zero_attempt_plan(self):
        from unittest import mock
        from types import SimpleNamespace
        repo = pathlib.Path(self.path('fixture/pristine'))
        repo.mkdir(parents=True)
        ready = {'schema':'af/task-inspection@20', 'attempts':0, 'phase':{'kind':'ready'},
                 'task_id':'telemetry-bench-concurrent', 'plan_id':'sha256:'+'1'*64}
        documents = [ready, {}, dict(ready, schema='af/task-inspection@18'),
                     dict(ready, attempts=1), dict(ready, attempts=False),
                     dict(ready, phase={'kind':'finished'}), dict(ready, plan_id='not-a-plan'),
                     dict(ready, task_id='another-task')]
        for document in documents:
            done = SimpleNamespace(returncode=0, stdout=json.dumps(document).encode(), stderr=b'')
            with mock.patch.object(overhead.subprocess, 'run', return_value=done):
                if document is ready:
                    overhead.admit_fixture('current-af', 'telemetry-bench-concurrent', repo)
                else:
                    with self.assertRaisesRegex(overhead.BenchError, 'malformed current workload admission'):
                        overhead.admit_fixture('bad-af', 'telemetry-bench-concurrent', repo)

    def test_store_sequence_retains_the_first_failure_and_never_runs_the_second_task(self):
        from unittest import mock
        from types import SimpleNamespace
        pristine = pathlib.Path(self.path('pristine'))
        pristine.mkdir()
        calls = []
        def launch(argv, **kwargs):
            calls.append(argv)
            kwargs['stderr'].write(b'original native check refusal')
            return SimpleNamespace(pid=123, returncode=None)
        with mock.patch.object(overhead.subprocess, 'Popen', side_effect=launch), \
                mock.patch.object(overhead.os, 'wait4', return_value=(123, 5 << 8,
                                  SimpleNamespace(ru_maxrss=100))):
            with self.assertRaisesRegex(overhead.BenchError, 'exit 5.*expected 0\\noriginal native check refusal'):
                overhead.run_once('af', 'telemetry-bench-store', pristine,
                                  pathlib.Path(self.path('work')), '00-off', 'off')
        self.assertEqual(len(calls), 1, 'a failed first Task prevents the second Task')
        self.assertEqual(calls[0][calls[0].index('--file') + 1],
                         str(overhead.task_files('telemetry-bench-store')[0]))

    def test_native_store_preserves_1000_checks_with_bounded_graphs_and_exact_smoke_checks(self):
        from unittest import mock
        fixture = overhead.FIXTURES_DIR / 'telemetry-bench-store/repo'
        review = toml_file.loads((fixture / '.af/task-packages/bench/store/pipeline.toml').read_bytes(), 'review')
        pipeline = toml_file.loads((fixture / '.af/task-packages/bench/checks/pipeline.toml').read_bytes(), 'pipeline')
        nodes = [n for n in pipeline['nodes'] if n['operator']['op'] == 'check']
        self.assertEqual(2 * sum(len(n['operator']['checks']) for n in nodes), 1000)
        self.assertEqual(len(overhead.task_files('telemetry-bench-store')), 2)
        self.assertLessEqual(len(pipeline['nodes']) + len(review['nodes']) + 2, 64)
        self.assertEqual(len(review['slots']), 4)
        self.assertEqual(review['max_parallel'], 4)
        policy = toml_file.loads((fixture / '.af/code-policy.toml').read_bytes(), 'policy')
        self.assertEqual(set(nodes[-1]['operator']['checks']), set(policy['checks']))
        repo = pathlib.Path(self.path('smoke'))
        overhead.copy_writable(fixture, repo)
        with mock.patch.object(overhead, 'af_digest', return_value='sha256:'+'0'*64):
            overhead.keep_checks('unused', repo, 10)
        smoke = toml_file.loads((repo / '.af/task-packages/bench/checks/pipeline.toml').read_bytes(), 'smoke')
        checks = [n for n in smoke['nodes'] if n['operator']['op'] == 'check']
        self.assertEqual(len(checks), 1)
        self.assertEqual(2 * len(checks[0]['operator']['checks']), 10)
        policy = toml_file.loads((repo / '.af/code-policy.toml').read_bytes(), 'policy')
        self.assertEqual(set(checks[0]['operator']['checks']), set(policy['checks']))


if __name__ == "__main__":
    import unittest
    unittest.main()
