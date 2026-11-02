#!/usr/bin/env python3
"""Verify historical Pi evidence against an explicitly selected source and claim.

--projection prints a source binding; --check never writes or measures performance.
--measurement-log additionally verifies supplied log bytes, never an implicit local path.
A successful check says nothing about a different or future checkout.
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
BEGIN = '<!-- af-pi-validation-claim:v1 -->'
END = '<!-- /af-pi-validation-claim -->'
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


def validate_claim(claim):
    # This flat contract uses only these JSON Schema keywords. Reading the shipped schema
    # here keeps the stdlib reader and its versioned on-disk shape together.
    schema = json.loads((ROOT / 'schemas/pi-validation-claim-v1.json').read_text())
    if not isinstance(claim, dict) or set(claim) != set(schema['required']):
        raise ValueError('historical claim fields differ from schema')
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


def read_claims(record):
    text = record.read_text(encoding='utf-8')
    pattern = re.compile(re.escape(BEGIN) + r'\s*```json\n(.*?)\n```\s*' +
                         re.escape(END), re.DOTALL)
    matches = pattern.findall(text)
    # Remove only consumed delimiters, retaining their JSON and unrelated historical prose.
    # Any remaining family marker (opening or closing, even malformed) fails closed.
    if not matches or re.search(r'<!--[\s/]*af-pi-validation-claim',
                                pattern.sub(r'\1', text)):
        raise ValueError('unsupported or malformed historical claim blocks')
    claims = {}
    for content in matches:
        claim = validate_claim(json.loads(content, object_pairs_hook=unique_object))
        key = claim['source_sha256']
        if key in claims:
            raise ValueError('duplicate historical source claim: ' + key)
        claims[key] = claim
    return claims


def check(root, record, claim_source, measurement_log=None):
    claims = read_claims(record)
    if claim_source not in claims:
        raise ValueError('selected historical claim not found: ' + claim_source)
    claim = claims[claim_source]
    source = projection(root)
    if source != {key: claim[key] for key in source}:
        raise ValueError('historical claim does not match selected source: ' + source_line(source))
    if measurement_log is not None:
        content = measurement_log.read_bytes()
        if 'sha256:' + hashlib.sha256(content).hexdigest() != claim['log_sha256']:
            raise ValueError('measurement log SHA256 differs from historical claim')
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
            raise ValueError('measurement log summary differs from historical claim')
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--check', action='store_true')
    mode.add_argument('--projection', action='store_true')
    parser.add_argument('--root', type=Path, help='source directory; required for --check')
    parser.add_argument('--record', type=Path, default=ROOT / RECORD)
    parser.add_argument('--claim-source', help='recorded source_sha256 selecting historical evidence')
    parser.add_argument('--measurement-log', type=Path)
    args = parser.parse_args()
    if (args.measurement_log or args.claim_source) and not args.check:
        parser.error('--measurement-log and --claim-source require --check')
    if args.check and (args.root is None or args.claim_source is None):
        parser.error('--check requires explicit --root and --claim-source')
    try:
        source = (check(args.root, args.record, args.claim_source, args.measurement_log)
                  if args.check else projection(args.root or ROOT))
    except (OSError, ValueError) as error:
        parser.exit(1, 'pi-validation: ' + str(error) + '\n')
    print(source_line(source))
    if args.check:
        print('historical claim matches selected source; measurement log ' +
              ('verified' if args.measurement_log else 'not supplied (not read)'))


if __name__ == '__main__':
    main()
