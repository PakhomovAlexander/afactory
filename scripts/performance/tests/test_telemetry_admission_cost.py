"""Operation-local admission reuse with real copies, refusal, and locked restart."""
import os
from pathlib import Path
from types import MappingProxyType
from unittest import mock

from perflab import gitrefs, telemetrylab
from perflab.values import ContractError
from tests import test_telemetry_collect as tc
from tests.allocation_observer import observe
from tests.support import TempDir


class PreparedAdmission(TempDir):
    setUp = tc.Collection.setUp
    result = tc.Collection.result
    cycle = tc.Collection.cycle

    def attempt(self, data):
        return self.cycle([tc.run_entry(1)], {
            1: [tc.artifact_entry(1, 1, 'perf-telemetry-1-1', data)]})[0]

    def test_git_reuse_is_bound_and_immutable_and_materializes_every_admitted_byte(self):
        git = gitrefs.Git(self.repo.path)
        prepared = telemetrylab._GitAuthority(git, self.authority)
        target = self.path('expanded')
        with self.assertRaises(TypeError):
            prepared.members['Cargo.lock'] = b'changed'
        for reader, sha, evidence in ((git, self.untrusted, prepared),
                                      (gitrefs.Git(self.repo.path), self.authority, prepared),
                                      (git, self.authority, dict(prepared.members))):
            with self.subTest(sha=sha, reader=reader, evidence=type(evidence)):
                with self.assertRaises(ContractError):
                    telemetrylab.materialize_authority(reader, sha, target, _prepared=evidence)
                self.assertFalse(os.path.lexists(target))
        telemetrylab.materialize_authority(git, self.authority, target, _prepared=prepared)
        registry, _ = telemetrylab.load_registry(target)
        self.assertEqual(set(telemetrylab.authority_files(registry, target)), set(prepared.members))
        for name, data in prepared.members.items():
            self.assertEqual(Path(target, name).read_bytes(), data)
        self.assertEqual(telemetrylab.authority_digest(registry, target),
                         (prepared.digest, len(prepared.members)))
        # Even corrupted internal evidence must fail before mkdir, rather than trust a map.
        altered = dict(prepared.members, **{'Cargo.lock': b'corrupted reusable bytes'})
        object.__setattr__(prepared, 'members', MappingProxyType(altered))
        with self.assertRaisesRegex(ContractError, 'bytes changed'):
            telemetrylab.materialize_authority(git, self.authority, self.path('corrupt'),
                                               _prepared=prepared)
        self.assertFalse(os.path.lexists(self.path('corrupt')))

    def test_corrupted_preparation_is_refused_then_clean_cycle_can_be_verified_offline(self):
        data = tc.zipped(self.result(1))
        original = telemetrylab.read_files
        changed = []

        def corrupted(root, *args, **kwargs):
            members = original(root, *args, **kwargs)
            if os.path.basename(root).startswith('.staging-authority-') and not changed:
                # Same-sized corruption in admitted buffers, while disk and result remain
                # valid. Corrupt only the first preparation: a later disk read would hide
                # the invalid evidence supplied to the reservation and proposed for reuse.
                members['Cargo.lock'] = b'X' + members['Cargo.lock'][1:]
                changed.append(True)
            return members

        with observe(self, self.config['root'], lambda: self.config['bytes_max']), \
                mock.patch.object(telemetrylab, 'read_files', corrupted):
            doc = self.attempt(data)
        self.assertTrue(changed)
        self.assertEqual(doc['runs'][0]['outcome'], 'refused', doc)
        self.assertIn('authority changed before retention', doc['runs'][0]['reason'])
        self.assertFalse(os.path.exists(os.path.join(self.config['root'], 'results', 'tl_1_1')))
        with observe(self, self.config['root'], lambda: self.config['bytes_max']):
            good = self.attempt(data)
        self.assertEqual(good['status'], 'ok', good)
        history = telemetrylab.history(self.config['root'])
        self.assertEqual([r['result_id'] for r in history['results']], ['tl_1_1'])
        retained = os.path.join(self.config['root'], history['results'][0]['authority_path'])
        result, _, _ = telemetrylab.verify(os.path.join(self.config['root'], 'results', 'tl_1_1'),
                                          retained)
        self.assertEqual(result['result_id'], 'tl_1_1')
        # Persisted trees are mutable inputs on the next operation, never a global cache.
        with open(os.path.join(retained, 'Cargo.lock'), 'ab') as stream:
            stream.write(b'\nchanged after the first operation\n')
        history = telemetrylab.history(self.config['root'])
        self.assertEqual(history['results'], [])
        self.assertTrue(history['refused'])

    def test_mutation_between_verification_and_capture_is_refused(self):
        data = tc.zipped(self.result(1))
        original = telemetrylab.verify
        mutated = []

        def changed(source, root=None):
            result = original(source, root)
            if root and os.path.basename(root).startswith('.staging-authority-'):
                with open(os.path.join(root, 'Cargo.lock'), 'ab') as stream:
                    stream.write(b'\nmutated source\n')
                mutated.append(True)
            return result

        with mock.patch.object(telemetrylab, 'verify', changed):
            doc = self.attempt(data)
        self.assertTrue(mutated)
        self.assertEqual(doc['runs'][0]['outcome'], 'refused', doc)
        self.assertIn('authority changed before retention', doc['runs'][0]['reason'])
        self.assertFalse(os.path.exists(os.path.join(self.config['root'], 'results', 'tl_1_1')))

    def test_retention_reuse_refuses_wrong_root_and_corruption_before_any_copy(self):
        source = self.result(1)
        result, _, _ = telemetrylab.verify(source)
        prepared = telemetrylab._AuthorityCopy(telemetrylab.ROOT, None)
        with telemetrylab.HistoryRoot(self.config['root']) as kept:
            for root, evidence in ((self.repo.path, prepared),
                                   (telemetrylab.ROOT, dict(prepared.members))):
                with self.assertRaises(ContractError):
                    kept.retain_authority(result, root, _prepared=evidence)
                self.assertFalse(os.path.lexists(kept.authority_path(result)))
            altered = dict(prepared.members)
            del altered['Cargo.lock']
            object.__setattr__(prepared, 'members', MappingProxyType(altered))
            with self.assertRaisesRegex(ContractError, 'authority changed'):
                kept.retain_authority(result, telemetrylab.ROOT, _prepared=prepared)
            self.assertFalse(os.path.lexists(kept.authority_path(result)))
        with mock.patch.object(telemetrylab, 'AUTHORITY_BYTES_MAX', 10):
            with self.assertRaises(ContractError):
                telemetrylab._AuthorityCopy(telemetrylab.ROOT, None)
