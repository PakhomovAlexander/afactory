#!/usr/bin/env python3
"""Cheap, offline release contract preflight, independent of selected test classes.

The workspace version must explicitly select the current telemetry fixture contract.
The laboratory checks exactly its schemas; Rust publication schemas retain Rust parity tests.
Run after a version bump, before committing or publishing the release PR.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/performance'))
from perflab import contracts, telemetry_lab, toml_file  # noqa: E402


def check(root=ROOT):
    overhead = telemetry_lab.script_module(str(root / 'scripts/telemetry-overhead.py'))
    manifest = toml_file.loads((root / 'Cargo.toml').read_bytes(), 'Cargo.toml')
    version = manifest['workspace']['package']['version']
    declaration = json.loads((root / 'scripts/release-contract.json').read_text())
    if set(declaration) != {'schema', 'version', 'source', 'capabilities', 'contract', 'receipt'}:
        raise ValueError('invalid release contract declaration fields')
    import re
    if not isinstance(declaration['source'], str) or not re.fullmatch(r'(?:[0-9a-f]{40}|sha256:[0-9a-f]{64})', declaration['source']):
        raise ValueError('invalid exact release source identity')
    if declaration['schema'] != 'af.dev-release-contract/1' or declaration['version'] != version:
        raise ValueError('%s is named by no fixture contract release declaration' % version)
    if (declaration['contract'] != overhead.select_contract()
            or declaration['receipt'] != overhead.CHECK_RECEIPT_CURRENT
            or overhead.CURRENT_CONTRACT != 'native-task-review@1'
            or overhead.CHECK_RECEIPT_CURRENT != 'af/TaskCheckReceipt@2'):
        raise ValueError('workspace release must explicitly select the current committed fixtures')
    directory = root / 'schemas'
    generated = contracts.schema_documents()
    names = contracts.lab_schema_names(directory)
    if names != sorted(generated):
        raise ValueError('lab schema ownership mismatch: missing %s; retired or unowned %s' % (
            sorted(set(generated) - set(names)), sorted(set(names) - set(generated))))
    for name, document in generated.items():
        rendered = (json.dumps(document, indent=2, ensure_ascii=False) + '\n').encode('utf-8')
        if (directory / name).read_bytes() != rendered:
            raise ValueError('%s is stale: run python3 scripts/performance/schemas.py --write' % name)
    if declaration['capabilities'] != capabilities(root):
        raise ValueError('release contract capabilities differ from exact committed fixtures')
    return version


def capabilities(root):
    """Exact schema and fixture bytes, not a release range or inferred compatibility."""
    paths = sorted([*root.glob('schemas/*.json'),
                    *root.glob('fixtures/telemetry-bench/**/*')])
    digest = hashlib.sha256()
    for path in paths:
        if path.is_file():
            digest.update(str(path.relative_to(root)).encode() + b'\0' + path.read_bytes() + b'\0')
    return 'sha256:' + digest.hexdigest()


def prepare(root, source):
    manifest = toml_file.loads((root / 'Cargo.toml').read_bytes(), 'Cargo.toml')
    path = root / 'scripts/release-contract.json'
    previous = path.read_bytes()
    declaration = json.loads(previous)
    # A new version may select only the already reviewed exact capabilities.
    check_version = declaration['version']
    declaration['version'] = manifest['workspace']['package']['version']
    declaration['source'] = source
    path.write_text(json.dumps(declaration, indent=2) + '\n')
    try:
        check(root)
    except Exception:
        path.write_bytes(previous)
        raise
    return check_version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepare', metavar='SOURCE_SHA')
    args = parser.parse_args()
    try:
        if args.prepare:
            import re
            if not re.fullmatch('[0-9a-f]{40}', args.prepare):
                raise ValueError('preparation requires exact source SHA')
            prepare(ROOT, args.prepare)
        version = check()
    except (ValueError, OSError, KeyError) as error:
        print('release-contract: %s' % error, file=sys.stderr)
        return 1
    print('release-contract: %s explicitly selects current telemetry fixtures; lab schemas match' % version)
    return 0


if __name__ == '__main__':
    sys.exit(main())
