#!/usr/bin/env python3
"""Check the Pi validation record against its non-self-referential source projection.

--projection prints the binding to retain before measurement. --check never writes;
--measurement-log additionally checks a supplied real log (never an implicit local path).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat

ROOT = Path(__file__).resolve().parent.parent
RECORD = 'docs/design/pi-provider-validation.md'
EXCLUDED = frozenset({'.git', 'target', RECORD})
BEGIN = '<!-- af-pi-validation-current:v1 -->'
END = '<!-- /af-pi-validation-current -->'
FILTER = 'test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)'


def projection(root):
    """The documented af.pi.validation-source/1 algorithm; no data-selected exclusions."""
    entries = []

    def visit(relative):
        with os.scandir(root / relative) as children:
            for child in children:
                path = (relative + '/' if relative else '') + child.name
                if path in EXCLUDED:
                    continue
                mode = child.stat(follow_symlinks=False).st_mode
                if stat.S_ISDIR(mode):
                    entries.append([path, 'directory', False, None])
                    visit(path)
                elif stat.S_ISREG(mode):
                    entries.append([path, 'file', bool(mode & 0o111),
                                    hashlib.sha256((root / path).read_bytes()).hexdigest()])
                elif stat.S_ISLNK(mode):
                    content = os.fsencode(os.readlink(root / path))
                    entries.append([path, 'symlink', False, hashlib.sha256(content).hexdigest()])
                else:
                    raise ValueError('unsupported source entry: ' + path)

    visit('')
    entries.sort(key=lambda entry: entry[0])
    digest = hashlib.sha256(b'af.pi.validation-source/1\0')
    for entry in entries:
        digest.update((json.dumps(entry, ensure_ascii=True, separators=(',', ':')) + '\n')
                      .encode('ascii'))
    return {'source_sha256': 'sha256:' + digest.hexdigest(), 'entries': len(entries),
            'files': sum(entry[1] == 'file' for entry in entries)}


def source_line(source):
    return 'pi-validation source: ' + json.dumps(source, sort_keys=True, separators=(',', ':'))


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate claim field: ' + key)
        result[key] = value
    return result


def read_claim(root):
    text = (root / RECORD).read_text(encoding='utf-8')
    # Count the marker family, so unsupported/duplicate versions fail closed too.
    if text.count('<!-- af-pi-validation-current:') != 1 or text.count(END) != 1:
        raise ValueError('expected exactly one versioned current claim')
    pattern = re.escape(BEGIN) + r'\s*```json\n(.*?)\n```\s*' + re.escape(END)
    match = re.search(pattern, text, re.DOTALL)
    if match is None:
        raise ValueError('unsupported or malformed current claim')
    claim = json.loads(match[1], object_pairs_hook=unique_object)
    # This flat contract uses only these JSON Schema keywords. Reading the shipped schema
    # here keeps the stdlib reader and its versioned on-disk shape together.
    schema = json.loads((ROOT / 'schemas/pi-validation-claim-v1.json').read_text())
    if not isinstance(claim, dict) or set(claim) != set(schema['required']):
        raise ValueError('current claim fields differ from schema')
    for key, rule in schema['properties'].items():
        value = claim[key]
        expected = str if rule['type'] == 'string' else int
        if type(value) is not expected:
            raise ValueError('invalid claim field: ' + key)
        if ('const' in rule and value != rule['const'] or
                'minimum' in rule and value < rule['minimum'] or
                'pattern' in rule and re.fullmatch(rule['pattern'], value) is None):
            raise ValueError('invalid claim field: ' + key)
    return claim


def check(root, measurement_log=None):
    claim = read_claim(root)
    source = projection(root)
    if source != {key: claim[key] for key in source}:
        raise ValueError('current claim does not match candidate source: ' + source_line(source))
    if measurement_log is not None:
        content = measurement_log.read_bytes()
        if 'sha256:' + hashlib.sha256(content).hexdigest() != claim['log_sha256']:
            raise ValueError('measurement log SHA256 differs from current claim')
        lines = content.decode('utf-8').splitlines()
        for prefix, expected in [
                ('pi-validation source: ', source_line(source)),
                ('command: ', 'command: ' + claim['command']),
                ('filter: ', 'filter: ' + claim['filter']),
                ('make exit: ', 'make exit: ' + str(claim['exit_code']))]:
            if [line for line in lines if line.startswith(prefix)] != [expected]:
                raise ValueError('measurement log binding/outcome differs: ' + prefix)
        summaries = [line.strip() for line in lines if line.strip().startswith('Summary [')]
        if summaries != [claim['summary']]:
            raise ValueError('measurement log summary differs from current claim')
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--check', action='store_true')
    mode.add_argument('--projection', action='store_true')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--measurement-log', type=Path)
    args = parser.parse_args()
    if args.measurement_log and not args.check:
        parser.error('--measurement-log requires --check')
    try:
        source = check(args.root, args.measurement_log) if args.check else projection(args.root)
    except (OSError, ValueError) as error:
        parser.exit(1, 'pi-validation: ' + str(error) + '\n')
    print(source_line(source))
    if args.check:
        print('current claim matches candidate source; measurement log ' +
              ('verified' if args.measurement_log else 'not supplied (not read)'))


if __name__ == '__main__':
    main()
