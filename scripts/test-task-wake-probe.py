#!/usr/bin/env python3
"""Deterministic probe behavior, not host compatibility or independent live evidence."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('wake', ROOT / 'scripts/task-wake-probe.py')
W = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(W)
FIXTURE = ROOT / 'fixtures/task-wake-probe'


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.p = W.packet(W.parse((FIXTURE / 'pi.json').read_bytes()))

    def line(self, value):
        return (W.canonical(value) + '\n').encode()

    def test_offline_fixture_and_independent_stages(self):
        rows = W.validate_events((FIXTURE / 'pi.jsonl').read_bytes(), self.p)
        self.assertEqual([r['stage'] for r in rows], ['inbox_accepted', 'admission_intent',
                         'native_returned', 'admission_unknown'])
        rows += [W.event(self.p, 'turn_started', 'observer_supplied', 'turn_1'),
                 W.event(self.p, 'report_observed', 'observer_supplied', 'message_1')]
        self.assertEqual(len(W.validate_events(b''.join(map(self.line, rows)), self.p)), 6)
        self.assertNotIn('native_admitted', [r['stage'] for r in rows])

    def test_closed_canonical_bounded_packets(self):
        for p in [dict(self.p, command='touch sentinel'), dict(self.p, marker='$(bad)'),
                  dict(self.p, session_id='https://example.org'), dict(self.p, schema='old')]:
            with self.assertRaises(ValueError):
                W.packet(p)
        for raw in [b'{', b'[]\n', b'{"x":1,"x":2}\n', b' ' * 2049,
                    self.line(self.p) + b' ', self.line(dict(self.p, marker=42))]:
            with self.assertRaises(ValueError):
                W.packet(W.parse(raw))

    def test_wrong_identity_altered_keys_and_stage_validation(self):
        base = W.event(self.p, 'dry_run', 'offline')
        for key in (*W.IDENTITIES, 'payload_sha256', 'schema', 'stage', 'reason'):
            altered = dict(base, **{key: 'other'})
            with self.assertRaises(ValueError):
                W.validate_events(self.line(altered), self.p)
        with self.assertRaises(ValueError):
            W.validate_events(self.line(dict(base, extra='x')), self.p)
        with self.assertRaises(ValueError):
            W.validate_events(self.line(dict(base, reference='unearned')), self.p)
        for stage in ('native_admitted', 'turn_started', 'report_observed', 'admission_intent'):
            with self.assertRaises(ValueError):
                W.validate_events(self.line(W.event(self.p, stage, 'observer_supplied', 'ref')), self.p)
        intent = [W.event(self.p, 'inbox_accepted', 'persisted'),
                  W.event(self.p, 'admission_intent', 'before_native_call')]
        first = W.event(self.p, 'turn_started', 'observer_supplied', 'one')
        second = dict(first, reference='two')
        with self.assertRaises(ValueError):
            W.validate_events(b''.join(map(self.line, [*intent, first, second])), self.p)
        changed = dict(self.p, marker='AF_WAKE_1111111111111111')
        with self.assertRaises(ValueError):
            W.validate_events(self.line(base), changed)
        with self.assertRaises(ValueError):
            W.validate_events(self.line(base) * 129, self.p)
        with self.assertRaises(ValueError):
            W.validate_events(b'x' * (W.LIMIT * W.ROWS + 1), self.p)

    def test_each_host_refuses_live_without_original_session_api(self):
        for name in W.HOSTS:
            for scenario in W.SCENARIOS:
                p = dict(self.p, host=name, scenario=scenario)
                self.assertEqual(W.probe(p, p['session_id'])['stage'], 'dry_run')
                self.assertEqual(W.probe(p, p['session_id'], live=True)['stage'], 'unsupported')
                self.assertEqual(W.probe(p, p['session_id'], live=True, available=False)['stage'],
                                 'unavailable')
                with self.assertRaises(ValueError):
                    W.probe(p, 'wrong', live=True)

    def test_original_frontend_refusals_never_launch_recording_host_processes(self):
        # Stand in for the native clients at their process boundary. Unsupported hosts have
        # no approved API connection; they must never start a substitute app/SDK conversation.
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            sentinel = directory / 'native-calls'
            for host in W.HOSTS:
                client = directory / host
                client.write_text('#!' + sys.executable + '\nfrom pathlib import Path\n' +
                                  f'Path({str(sentinel)!r}).write_text("native call")\n')
                client.chmod(0o700)
            env = {**os.environ, 'PATH': str(directory) + os.pathsep + os.environ.get('PATH', '')}
            for host in W.HOSTS:
                for scenario in W.SCENARIOS:
                    p = dict(self.p, host=host, scenario=scenario)
                    file = directory / 'packet.json'
                    file.write_bytes(self.line(p))
                    result = subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/task-wake-probe.py'),
                                             'probe', '--live', '--packet', str(file),
                                             '--session-id', p['session_id']], env=env,
                                            capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(json.loads(result.stdout)['stage'], 'unsupported')
                    self.assertFalse(sentinel.exists())

    def test_cli_round_trip_and_no_live_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / 'packet.json'
            e = Path(tmp) / 'evidence.jsonl'
            command = [sys.executable, '-I', str(ROOT / 'scripts/task-wake-probe.py')]
            made = subprocess.run([*command, 'packet', '--host', 'pi', '--run-id', 'run_1',
                                   '--session-id', 'session_1', '--probe-id', 'probe_1',
                                   '--delivery-id', 'delivery_1', '--scenario', 'idle',
                                   '--marker', 'AF_WAKE_0123456789abcdef'],
                                  capture_output=True, check=True, timeout=10)
            p.write_bytes(made.stdout)
            args = ['--packet', str(p), '--session-id', 'session_1']
            dry = subprocess.run([*command, 'probe', *args], capture_output=True, timeout=10)
            self.assertEqual(dry.returncode, 0)
            e.write_bytes(dry.stdout)
            validated = subprocess.run([*command, 'validate', *args, '--evidence', str(e)],
                                       capture_output=True, timeout=10)
            self.assertEqual(validated.returncode, 0)
            self.assertIn(b'not independent live proof', validated.stdout)
            live = subprocess.run([*command, 'probe', *args, '--live'], capture_output=True, timeout=10)
            self.assertEqual(live.returncode, 2)
            self.assertEqual(json.loads(live.stdout)['stage'], 'unsupported')
            observed = subprocess.run([*command, 'observe', *args, '--stage', 'turn_started',
                                       '--reference', 'native_turn_1'], capture_output=True, timeout=10)
            self.assertEqual(observed.returncode, 0)
            self.assertEqual(json.loads(observed.stdout)['reason'], 'observer_supplied')
            # A local observation without a preceding admission intent cannot be joined.
            with self.assertRaises(ValueError):
                W.validate_events(dry.stdout + observed.stdout, W.parse(made.stdout))
            self.assertEqual(sorted(x.name for x in Path(tmp).iterdir()), ['evidence.jsonl', 'packet.json'])
            with self.assertRaises(ValueError):
                W.read_packet(p, 'wrong')
            p.write_bytes(b'x' * 2049)
            with self.assertRaises(ValueError):
                W.read_packet(p, 'session_1')

    def test_schema_parity(self):
        packet = json.loads((ROOT / 'schemas/task-wake-probe-v1.json').read_text())
        evidence = json.loads((ROOT / 'schemas/task-wake-evidence-v1.json').read_text())
        for schema, fields in [(packet, self.p), (evidence, W.event(self.p, 'dry_run', 'offline'))]:
            self.assertEqual(set(schema['required']), set(fields))
            self.assertEqual(set(schema['properties']), set(fields))
            self.assertFalse(schema['additionalProperties'])
        self.assertEqual(packet['properties']['host']['enum'], list(W.HOSTS))
        self.assertEqual(packet['properties']['scenario']['enum'], list(W.SCENARIOS))
        self.assertEqual(evidence['properties']['stage']['enum'], list(W.STAGES))
        self.assertEqual(evidence['properties']['reason']['enum'], list(W.REASONS))
        for key in ('run_id', 'session_id', 'probe_id', 'delivery_id'):
            self.assertEqual(packet['properties'][key]['pattern'], '^' + W.TOKEN + r'$(?![\s\S])')
        self.assertEqual(packet['properties']['schema']['const'], W.SCHEMA)
        self.assertEqual(evidence['properties']['schema']['const'], W.EVIDENCE)

    def test_native_pi_entrypoint_with_recording_api(self):
        result = subprocess.run(['node', '--disable-warning=ExperimentalWarning', '--test-reporter=tap',
                                 str(FIXTURE / 'pi.test.mjs')], cwd=ROOT,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('# fail 0', result.stdout)
        self.assertIn('# tests 25', result.stdout)

    def test_production_pi_hook_with_recording_api(self):
        result = subprocess.run(['node', '--disable-warning=ExperimentalWarning', '--test-reporter=tap',
                                 str(FIXTURE / 'extension.test.mjs')], cwd=ROOT,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('# fail 0', result.stdout)
        self.assertIn('# tests 11', result.stdout)


if __name__ == '__main__':
    unittest.main()
