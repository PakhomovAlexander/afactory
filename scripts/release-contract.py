#!/usr/bin/env python3
"""Cheap, offline release contract preflight, independent of selected test classes.

The workspace version must explicitly select the current telemetry fixture contract.
The laboratory checks exactly its schemas; Rust publication schemas retain Rust parity tests.
Run after a version bump, before committing or publishing the release PR.
"""
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
    try:
        if version not in overhead.CURRENT_RELEASES:
            raise overhead.BenchError('%s is named by no fixture contract release declaration' % version)
        selected = overhead.select_contract()
    except overhead.BenchError as error:
        raise ValueError(str(error)) from error
    if (selected != 'native-task-review@1'
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
    return version


def main():
    try:
        version = check()
    except (ValueError, OSError, KeyError) as error:
        print('release-contract: %s' % error, file=sys.stderr)
        return 1
    print('release-contract: %s explicitly selects current telemetry fixtures; lab schemas match' % version)
    return 0


if __name__ == '__main__':
    sys.exit(main())
