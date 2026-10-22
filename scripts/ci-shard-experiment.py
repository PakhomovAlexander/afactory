#!/usr/bin/env python3
"""Record nextest ci selections and compile-cache evidence for the CI experiment."""

import argparse
import json
from pathlib import Path
import subprocess


def selected(document):
    tests = set()
    ignored = set()
    for suite in document['rust-suites'].values():
        binary = suite['binary-id']
        for name, case in suite['testcases'].items():
            key = (binary, name)
            if case['ignored']:
                ignored.add(key)
            elif case['filter-match']['status'] == 'matches':
                tests.add(key)
    return tests, ignored


def record(args):
    command = ['cargo', 'nextest', 'list', '--message-format', 'json',
               '--run-ignored', 'default', '--profile', 'ci']
    if args.archive:
        command.extend(['--archive-file', str(args.archive)])
        if args.extract_to:
            command.extend(['--extract-to', str(args.extract_to), '--extract-overwrite'])
    else:
        command.append('--locked')
    if args.partition:
        command.extend(['--partition', args.partition])
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode:
        raise SystemExit(f'nextest list failed ({result.returncode}): {result.stderr}')
    document = json.loads(result.stdout)
    tests, ignored = selected(document)
    if not tests:
        raise SystemExit('refusing an empty nextest selection')
    args.output.write_text(json.dumps(document, sort_keys=True) + '\n')
    print(f'{args.output}: {len(tests)} runnable tests, {len(ignored)} ignored entries')


def verify(args):
    full = json.loads(args.full.read_text())
    first = json.loads(args.first.read_text())
    second = json.loads(args.second.read_text())
    all_tests, all_ignored = selected(full)
    one, one_ignored = selected(first)
    two, two_ignored = selected(second)
    if not all_tests or not one or not two:
        raise SystemExit('refusing an empty full manifest or shard')
    if one & two:
        raise SystemExit(f'shards overlap on {len(one & two)} tests')
    if one | two != all_tests:
        raise SystemExit(f'shard union differs: missing={len(all_tests - (one | two))}, '
                         f'extra={len((one | two) - all_tests)}')
    if one_ignored != all_ignored or two_ignored != all_ignored:
        raise SystemExit('ignored test inventory differs between manifests')
    print(f'complete disjoint ci partition: {len(one)} + {len(two)} = '
          f'{len(all_tests)} runnable tests; {len(all_ignored)} ignored entries unchanged')


def cache_stats(args):
    """Capture the server's compile counters before a long test can idle it out."""
    args.output.unlink(missing_ok=True)
    result = subprocess.run(
        ['sccache', '--show-stats', '--stats-format', 'json'],
        capture_output=True, text=True,
    )
    if result.returncode:
        raise SystemExit(f'sccache stats failed ({result.returncode}): {result.stderr}')
    try:
        raw = json.loads(result.stdout)
        stats = raw['stats']
        requests = stats['compile_requests']
        if type(requests) is not int or requests <= 0:
            raise ValueError('compile_requests must be positive')
        totals = {}
        for key in ('cache_hits', 'cache_misses'):
            counts = stats[key]['counts']
            if not isinstance(counts, dict) or any(
                not isinstance(lang, str) or not lang or type(count) is not int
                or count < 0 for lang, count in counts.items()
            ):
                raise ValueError(f'{key}.counts must contain nonnegative integer counts')
            totals[key] = sum(counts.values())
        if not 0 < totals['cache_hits'] + totals['cache_misses'] <= requests:
            raise ValueError('cache hits and misses must be nonzero in aggregate and fit requests')
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        raise SystemExit(f'invalid compile cache statistics: {error}') from error
    document = {
        'schema': 'af.ci-compile-cache/1', 'arm': args.arm,
        'phase': 'after-compile-before-test', 'compile_requests': requests,
        'cache_hits': totals['cache_hits'], 'cache_misses': totals['cache_misses'],
        'sccache': raw,
    }
    args.output.write_text(json.dumps(document, sort_keys=True) + '\n')
    print(f'{args.arm}: compile requests={requests}, hits={totals["cache_hits"]}, '
          f'misses={totals["cache_misses"]}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    listing = commands.add_parser('record')
    listing.add_argument('--output', required=True, type=Path)
    listing.add_argument('--archive', type=Path)
    listing.add_argument('--extract-to', type=Path)
    listing.add_argument('--partition', choices=['count:1/2', 'count:2/2'])
    listing.set_defaults(run=record)
    check = commands.add_parser('verify')
    for name in ('full', 'first', 'second'):
        check.add_argument(f'--{name}', required=True, type=Path)
    check.set_defaults(run=verify)
    cache = commands.add_parser('cache-stats')
    cache.add_argument('--arm', required=True, choices=['single', 'archive-build',
                       'rebuild-shard-1', 'rebuild-shard-2'])
    cache.add_argument('--output', required=True, type=Path)
    cache.set_defaults(run=cache_stats)
    args = parser.parse_args()
    args.run(args)


if __name__ == '__main__':
    main()
