#!/usr/bin/env python3
"""Meaningful injected discovery/closure failures for the fast contract gate."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('inventory', Path(__file__).with_name('contract-inventory.py'))
INVENTORY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INVENTORY)


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'scripts').mkdir()
        (self.root / 'schemas').mkdir()
        (self.root / 'owner.rs').write_text('reader, writer, semantic validation and parity fixture')
        self.document = {'$id': 'urn:test:1', 'type': 'object', 'properties': {'x': {'type': 'string'}}}
        self.write('schemas/example.json', self.document)
        self.inventory = {'schema': 'af.contract-inventory/1', 'families': [
            {'name': 'example', 'reader_writer': ['owner.rs'], 'semantic_owner': ['owner.rs'],
             'tests': ['owner.rs'], 'schemas': ['schemas/example.json']}], 'requests': []}
        self.save()

    def write(self, path, value):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(value))

    def save(self):
        self.write('scripts/contract-inventory.json', self.inventory)

    def check(self):
        with contextlib.redirect_stdout(io.StringIO()):
            INVENTORY.check(self.root)

    def test_current_repository(self):
        with contextlib.redirect_stdout(io.StringIO()):
            INVENTORY.check()

    def test_missing_or_orphan_schema_fails(self):
        self.check()
        self.write('schemas/unlisted.json', {'type': 'string'})
        with self.assertRaisesRegex(ValueError, 'missing=.*unlisted'):
            self.check()
        (self.root / 'schemas/unlisted.json').unlink()
        (self.root / 'schemas/example.json').unlink()
        with self.assertRaisesRegex(ValueError, 'orphan=.*example'):
            self.check()

    def test_version_replacement_updates_inventory_and_canonical_port_together(self):
        worker = 'fixtures/pkg/worker.toml'
        path = self.root / worker
        path.parent.mkdir(parents=True)
        path.write_text('[signature.contract.outputs.result]\nartifact_type="custom/Result@1"\n')
        output = 'fixtures/pkg/outputs/result.schema.json'
        self.write(output, self.document)
        family = self.inventory['families'][0]
        family['schemas'].append(output)
        request = {'worker': worker, 'outputs': {'result': output}, 'strict': False,
                   'reason': 'open object retains kernel validation',
                   'canonical': {'result': 'schemas/example.json'}, 'excluded_outputs': {}}
        self.inventory['requests'].append(request)
        self.save()
        self.check()

        # An upstream replacement retires the old schema; retaining either old
        # inventory membership or the old canonical mapping must still refuse.
        current = dict(self.document, **{'$id': 'urn:test:2', 'required': ['x']})
        (self.root / 'schemas/example.json').unlink()
        self.write('schemas/example-v2.json', current)
        self.write(output, current)
        with self.assertRaisesRegex(ValueError, 'contract inventory drift'):
            self.check()
        family['schemas'][0] = 'schemas/example-v2.json'
        self.save()
        with self.assertRaisesRegex(ValueError, 'canonical schema drift'):
            self.check()
        request['canonical']['result'] = 'schemas/example-v2.json'
        self.save()
        self.check()
        self.write(output, self.document)
        with self.assertRaisesRegex(ValueError, 'canonical schema drift'):
            self.check()

    def test_duplicate_id_and_broken_ref_fail_but_literals_are_not_schemas(self):
        self.document['properties']['x'] = {'const': {'$ref': 'literal-data'}}
        self.write('schemas/example.json', self.document)
        self.check()
        self.write('schemas/other.json', self.document)
        self.inventory['families'][0]['schemas'].append('schemas/other.json')
        self.save()
        with self.assertRaisesRegex(ValueError, 'duplicate canonical schema ID'):
            self.check()
        self.write('schemas/other.json', {'$id': 'urn:other', '$ref': 'urn:missing'})
        with self.assertRaisesRegex(ValueError, 'unresolved reference'):
            self.check()
        self.write('schemas/other.json', {'$ref': 'urn:test:1#/properties/missing'})
        with self.assertRaisesRegex(ValueError, 'missing'):
            self.check()

    def test_invalid_schema_node_and_missing_owner_fail(self):
        self.write('schemas/example.json', {'type': 'object', 'properties': {'x': 42}})
        with self.assertRaisesRegex(ValueError, 'schema node must'):
            self.check()
        self.write('schemas/example.json', self.document)
        (self.root / 'owner.rs').unlink()
        with self.assertRaisesRegex(ValueError, 'missing reader_writer'):
            self.check()

    def test_uri_fragments_decode_before_pointer_resolution(self):
        for fragment, name in [
                ('a%20b', 'a b'), ('%D1%81%D1%85%D0%B5%D0%BC%D0%B0', 'схема'),
                ('%7E0', '~'), ('%7e1', '/'), ('%7E01', '~1'), ('a%2520b', 'a%20b')]:
            with self.subTest(fragment=fragment):
                document = {'$ref': '#/' + fragment, name: {'type': 'string'}}
                self.write('schemas/example.json', document)
                self.check()
                # A raw encoded-name decoy does not resolve a missing decoded target.
                document[fragment] = document.pop(name)
                self.write('schemas/example.json', document)
                with self.assertRaises(ValueError):
                    self.check()
        for fragment in ['default%2F0', 'default/00', 'default/+0']:
            self.write('schemas/example.json', {'$ref': '#/' + fragment,
                                               'default': [{'type': 'string'}]})
            self.check()
        for fragment in ['%', '%2', '%GG', '%FF', '%C3%28', 'default/-1']:
            self.write('schemas/example.json', {'$ref': '#/' + fragment,
                                               fragment: {'type': 'string'},
                                               'default': [{'type': 'string'}]})
            with self.assertRaises(ValueError):
                self.check()

    def test_new_port_without_schema_is_not_silently_excluded(self):
        worker = self.root / 'fixtures/pkg/worker.toml'
        worker.parent.mkdir(parents=True)
        worker.write_text('[signature.contract.outputs.new]\nartifact_type="custom/New@1"\n')
        with self.assertRaisesRegex(ValueError, 'missing whole Worker request sets'):
            self.check()

    def test_orphan_output_schema_is_rejected_even_when_listed(self):
        worker = self.root / 'fixtures/pkg/worker.toml'
        worker.parent.mkdir(parents=True)
        worker.write_text('[signature.contract.outputs.result]\nartifact_type="custom/Result@1"\n')
        self.write('fixtures/pkg/outputs/orphan.schema.json', {'type': 'string'})
        self.inventory['families'][0]['schemas'].append('fixtures/pkg/outputs/orphan.schema.json')
        self.save()
        with self.assertRaisesRegex(ValueError, 'orphan output schema'):
            self.check()


if __name__ == '__main__':
    unittest.main()
