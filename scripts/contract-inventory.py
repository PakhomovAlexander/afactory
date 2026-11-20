#!/usr/bin/env python3
"""Offline discovery and ownership gate; no provider, Store, network or schema generation."""
import argparse
import json
from pathlib import Path
import re
import sys
import time
from urllib.parse import unquote_to_bytes

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / "performance"))
from perflab import toml_file  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
MAP_KEYS = ('properties', '$defs', 'definitions', 'patternProperties', 'dependentSchemas')
ONE_KEYS = ('items', 'additionalProperties', 'unevaluatedProperties', 'unevaluatedItems',
            'not', 'if', 'then', 'else', 'contains', 'propertyNames', 'contentSchema')
MANY_KEYS = ('allOf', 'anyOf', 'oneOf', 'prefixItems')


def nodes(value, depth=0):
    if depth > 64:
        raise ValueError('schema depth exceeds 64')
    if not isinstance(value, (dict, bool)):
        raise ValueError('schema node must be an object or boolean')
    yield value
    if isinstance(value, bool):
        return
    for key in MAP_KEYS:
        for child in value.get(key, {}).values():
            yield from nodes(child, depth + 1)
    for key in ONE_KEYS:
        if key in value:
            yield from nodes(value[key], depth + 1)
    for key in MANY_KEYS:
        for child in value.get(key, []):
            yield from nodes(child, depth + 1)


def discover(root):
    result = {}
    for directory in ('schemas', 'crates', 'fixtures', '.af', 'scripts'):
        for path in sorted((root / directory).rglob('*.json')):
            if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
                raise ValueError(f'symlink/escaping JSON document: {path}')
            try:
                value = json.loads(path.read_text())
            except json.JSONDecodeError:
                if directory == 'schemas' or path.name.endswith('.schema.json'):
                    raise
                # Non-schema negative fixtures may intentionally contain truncated documents.
                # An inventoried embedded schema disappearing still fails the set equality.
                continue
            if (directory == 'schemas' or path.name.endswith('.schema.json')
                    or isinstance(value, dict) and '$schema' in value):
                result[path.relative_to(root).as_posix()] = value
            # Structured package schemas are embedded in the shipped kind, independently of
            # Worker copies. Keep their exact JSON Pointer in the ownership inventory.
            if isinstance(value, dict) and value.get('schema') == 'af.structured-contract/2':
                for direction in ('inputs', 'outputs'):
                    for port, contract in value.get(direction, {}).items():
                        if 'schema' in contract:
                            escaped = port.replace('~', '~0').replace('/', '~1')
                            key = f'{path.relative_to(root).as_posix()}#/{direction}/{escaped}/schema'
                            result[key] = contract['schema']
    return result


def pointer(value, fragment):
    if not fragment:
        return value
    if not fragment.startswith('/'):
        raise ValueError(f'unsupported schema fragment: {fragment}')
    if re.search(r'%(?![0-9a-fA-F]{2})', fragment):
        raise ValueError(f'invalid schema fragment encoding: {fragment}')
    # URI decoding precedes pointer splitting and tilde unescaping, as in the
    # pinned kernel resolver. Strict UTF-8 refuses encoded-name decoys.
    decoded = unquote_to_bytes(fragment[1:]).decode('utf-8')
    for part in decoded.split('/'):
        part = part.replace('~1', '/').replace('~0', '~')
        if isinstance(value, list):
            if not re.fullmatch(r'\+?[0-9]+', part):
                raise ValueError(f'invalid schema array index: {part}')
            value = value[int(part)]
        else:
            value = value[part]
    return value


def check(root=ROOT):
    inventory = json.loads((root / 'scripts/contract-inventory.json').read_text())
    if set(inventory) != {'schema', 'families', 'requests'} or inventory['schema'] != 'af.contract-inventory/1':
        raise ValueError('invalid contract inventory')
    documents = discover(root)
    declared = {}
    families = set()
    for family in inventory['families']:
        if set(family) != {'name', 'reader_writer', 'semantic_owner', 'tests', 'schemas'}:
            raise ValueError('invalid family fields')
        if family['name'] in families:
            raise ValueError(f'duplicate family: {family["name"]}')
        families.add(family['name'])
        for role in ('reader_writer', 'semantic_owner', 'tests'):
            if not family[role] or any(not (root / p).is_file() for p in family[role]):
                raise ValueError(f'{family["name"]}: missing {role} owner/evidence')
        for path in family['schemas']:
            if path in declared:
                raise ValueError(f'duplicate inventory schema: {path}')
            declared[path] = family['name']
    if set(declared) != set(documents):
        raise ValueError(f'contract inventory drift: missing={sorted(set(documents)-set(declared))}; '
                         f'orphan={sorted(set(declared)-set(documents))}')
    resources = {}
    for path, document in documents.items():
        if path.startswith('schemas/') and isinstance(document, dict) and '$id' in document:
            identity = document['$id']
            if identity in resources:
                raise ValueError(f'duplicate canonical schema ID: {identity}')
            resources[identity] = document
    for path, document in documents.items():
        try:
            for index, node in enumerate(nodes(document)):
                if index > 16384:
                    raise ValueError('schema node count exceeds 16384')
                if not isinstance(node, dict) or '$ref' not in node:
                    continue
                base, _, fragment = node['$ref'].partition('#')
                target = document if not base else resources.get(base)
                if target is None:
                    relative = (Path(path).parent / base).as_posix()
                    target = documents.get(relative)
                if target is None:
                    raise ValueError(f'unresolved reference: {node["$ref"]}')
                pointer(target, fragment)
        except (ValueError, KeyError, TypeError, AttributeError, IndexError) as error:
            raise ValueError(f'{path}: {error}') from error
    expected_requests = {}
    for directory in ('crates', 'fixtures', '.af'):
        for worker in (root / directory).rglob('worker.toml'):
            manifest = toml_file.loads(worker.read_bytes(), str(worker))
            outputs = manifest.get('signature', {}).get('contract', {}).get('outputs', {})
            for schema_file in (worker.parent / 'outputs').glob('*.schema.json'):
                if schema_file.name[:-len('.schema.json')] not in outputs:
                    raise ValueError(f'{worker}: orphan output schema: {schema_file.name}')
            present = {port: (worker.parent / 'outputs' / (port + '.schema.json')).relative_to(root).as_posix()
                       for port in outputs if (worker.parent / 'outputs' / (port + '.schema.json')).is_file()}
            if outputs:
                excluded = {port: contract['artifact_type'] for port, contract in outputs.items() if port not in present}
                expected_requests[worker.relative_to(root).as_posix()] = (present, excluded)
    seen = set()
    for request in inventory['requests']:
        if set(request) != {'worker', 'outputs', 'strict', 'reason', 'canonical', 'excluded_outputs'}:
            raise ValueError('invalid request-set fields')
        worker = request['worker']
        if worker in seen or expected_requests.get(worker) != (request['outputs'], {p: e['artifact_type'] for p, e in request['excluded_outputs'].items()}):
            raise ValueError(f'{worker}: whole output-port request set drift')
        seen.add(worker)
        for port, exclusion in request['excluded_outputs'].items():
            if set(exclusion) != {'artifact_type', 'reason'} or not exclusion['reason']:
                raise ValueError(f'{worker}/{port}: unexplained kernel/fixture output exclusion')
        if not isinstance(request['strict'], bool) or not request['reason']:
            raise ValueError(f'{worker}: unexplained projection exclusion')
        if set(request['canonical']) != set(request['outputs']):
            raise ValueError(f'{worker}: missing canonical output mapping')
        for port, canonical in request['canonical'].items():
            original = documents[request['outputs'][port]]
            target = documents.get(canonical)
            def payload(doc):
                return {k: v for k, v in doc.items() if k not in ('$id', '$schema')} if isinstance(doc, dict) else doc
            if target is None or payload(original) != payload(target):
                raise ValueError(f'{worker}/{port}: canonical schema drift: {canonical}')
    if seen != set(expected_requests):
        raise ValueError(f'missing whole Worker request sets: {sorted(set(expected_requests)-seen)}')
    print(f'contract inventory: {len(documents)} schemas, {len(families)} families; ownership and references valid')
    return documents


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    started = time.monotonic()
    try:
        check(args.root)
    except (ValueError, OSError, json.JSONDecodeError) as error:
        print(f'contract inventory refused: {error}', file=sys.stderr)
        return 1
    print(f'inventory warm/build-independent wall: {time.monotonic()-started:.3f}s')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
