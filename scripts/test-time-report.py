#!/usr/bin/env python3
"""Summarize where test time goes in a nextest JUnit report, and compare runs.

    test-time-report.py summary JUNIT [--nextest-config .config/nextest.toml] [--top N]
                                      [--format markdown|json]
    test-time-report.py compare [--nextest-config PATH] BASE_JUNIT... -- HEAD_JUNIT...

Times are read as the exact decimals nextest wrote and summed as decimals, so the wall time
and test count are nextest's own and the totals carry no float drift. The exclusive block is
the summed time of the tests that the config's exclusive overrides (`threads-required =
"num-test-threads"`) match with `test(/regex/)` filters: nothing else runs while they do.
This is a report, never a gate: it does not judge a run, and a failed test is only counted.
"""
import argparse
from decimal import ROUND_HALF_UP, Decimal
import json
from pathlib import Path
import re
import statistics
import sys
import xml.etree.ElementTree as ET

DEFAULT_CONFIG = Path('.config/nextest.toml')
DEFAULT_TOP = 20
MIN_CHANGE = Decimal(1)
# Lower edge inclusive, upper edge exclusive: 0.1 s is in 0.1-1, 60 s in >=60.
BUCKETS = [('<0.1', None, Decimal('0.1')), ('0.1-1', Decimal('0.1'), Decimal(1)),
           ('1-3', Decimal(1), Decimal(3)), ('3-10', Decimal(3), Decimal(10)),
           ('10-30', Decimal(10), Decimal(30)), ('30-60', Decimal(30), Decimal(60)),
           ('>=60', Decimal(60), None)]
REGEX_FILTER = re.compile(r'test\(/((?:\\.|[^/\\])*)/\)')
# A test absent from one side of a comparison, unlike one present without a time (None).
MISSING = object()


class ReportError(Exception):
    """A report or config that cannot be read; the message names it."""


def seconds(text, where):
    try:
        value = Decimal(text)
    except ArithmeticError:
        raise ReportError(f'{where}: time {text!r} is not a number') from None
    if not value.is_finite() or value < 0:
        raise ReportError(f'{where}: time {text!r} is not a duration')
    return value


def read_run(path):
    """One nextest run: its wall time (None when absent) and every testcase it recorded."""
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError) as error:
        raise ReportError(f'cannot read JUnit {path}: {error}') from None
    if root.tag != 'testsuites':
        raise ReportError(f'{path}: root element is <{root.tag}>, not <testsuites>')
    wall = root.get('time')
    cases = []
    for suite in root.iter('testsuite'):
        for case in suite.iter('testcase'):
            binary = case.get('classname') or suite.get('name') or ''
            name = case.get('name') or ''
            time = case.get('time')
            failed = case.find('failure') is not None or case.find('error') is not None
            cases.append({'binary': binary, 'test': name, 'failed': failed,
                          'seconds': None if time is None else seconds(time, f'{path}: {name}')})
    return {'wall': None if wall is None else seconds(wall, f'{path}: <testsuites>'),
            'cases': cases}


def exclusive_patterns(path, explicit):
    """Regexes of the exclusive overrides' `test(/.../)` filters, or None without a config."""
    if not path.is_file():
        if explicit:
            raise ReportError(f'nextest config {path} does not exist')
        return None
    try:
        import tomllib
    except ImportError:
        raise ReportError('reading the nextest config needs Python 3.11 or later') from None
    try:
        config = tomllib.loads(path.read_text(encoding='utf-8'))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ReportError(f'cannot read nextest config {path}: {error}') from None
    patterns = []
    for name in sorted(config.get('profile', {})):
        for override in config['profile'][name].get('overrides', []):
            if override.get('threads-required') != 'num-test-threads':
                continue
            for regex in REGEX_FILTER.findall(override.get('filter', '')):
                try:
                    patterns.append(re.compile(regex.replace('\\/', '/')))
                except re.error as error:
                    raise ReportError(f'{path}: filter regex {regex!r}: {error}') from None
    return patterns


def summarize(run, patterns):
    """Totals of one run, all exact decimals or counts; None where the input lacks a value."""
    timed = [c for c in run['cases'] if c['seconds'] is not None]
    total = sum((c['seconds'] for c in timed), Decimal(0))
    wall = run['wall']
    exclusive = None
    if patterns is not None:
        matched = [c for c in timed if any(p.search(c['test']) for p in patterns)]
        exclusive = {'tests': len(matched), 'seconds': sum((c['seconds'] for c in matched),
                                                           Decimal(0))}
    histogram = []
    for label, low, high in BUCKETS:
        inside = [c['seconds'] for c in timed
                  if (low is None or c['seconds'] >= low) and (high is None or c['seconds'] < high)]
        spent = sum(inside, Decimal(0))
        histogram.append({'bucket': label, 'count': len(inside), 'seconds': spent,
                          'share': spent / total if total else None})
    slowest = sorted(timed, key=lambda c: (-c['seconds'], c['binary'], c['test']))
    return {
        'wall_seconds': wall, 'tests': len(run['cases']), 'test_seconds': total,
        'untimed_tests': len(run['cases']) - len(timed),
        'parallelism': total / wall if wall else None, 'exclusive': exclusive,
        'failures': sum(c['failed'] for c in run['cases']),
        'failed': sorted(({'binary': c['binary'], 'test': c['test']}
                          for c in run['cases'] if c['failed']),
                         key=lambda c: (c['binary'], c['test'])),
        'histogram': histogram, 'slowest': slowest,
    }


def fixed(value, places=3):
    return str(value.quantize(Decimal(1).scaleb(-places), rounding=ROUND_HALF_UP))


def secs(value):
    return 'unknown' if value is None else f'{fixed(value)} s'


def percent(fraction):
    return '—' if fraction is None else f'{fixed(fraction * 100, 1)}%'


def ratio(value):
    return 'unknown' if value is None else fixed(value, 2)


def counted(count, noun):
    return f'{count} {noun}' + ('' if count == 1 else 's')


def cell(text):
    return text.replace('\\', '\\\\').replace('|', '\\|')


def summary_markdown(totals, top):
    wall, exclusive = totals['wall_seconds'], totals['exclusive']
    lines = [f'### Test time: {counted(totals["tests"], "test")} in {secs(wall)}', '',
             '| Total | Value |', '| --- | ---: |',
             f'| Wall (nextest) | {secs(wall)} |',
             f'| Tests | {totals["tests"]} |',
             f'| Test-seconds (summed) | {secs(totals["test_seconds"])} |',
             f'| Achieved parallelism | {ratio(totals["parallelism"])} |']
    if exclusive is None:
        lines.append('| Exclusive block | unknown (no nextest config) |')
    else:
        share = exclusive['seconds'] / wall if wall else None
        lines.append(f'| Exclusive block | {secs(exclusive["seconds"])}, '
                     f'{counted(exclusive["tests"], "test")}, {percent(share)} of wall |')
    lines.append(f'| Failures | {totals["failures"]} |')
    if totals['untimed_tests']:
        lines.append(f'| Tests without a time | {totals["untimed_tests"]} |')
    lines += ['', '| Duration (s) | Tests | Seconds | Share of test-seconds |',
              '| --- | ---: | ---: | ---: |']
    lines += [f'| {b["bucket"]} | {b["count"]} | {fixed(b["seconds"])} | {percent(b["share"])} |'
              for b in totals['histogram']]
    slowest = totals['slowest'][:top]
    if slowest:
        lines += ['', f'Slowest {len(slowest)} tests:', '',
                  '| # | Seconds | Binary | Test |', '| ---: | ---: | --- | --- |']
        lines += [f'| {i} | {fixed(c["seconds"])} | {cell(c["binary"])} | {cell(c["test"])} |'
                  for i, c in enumerate(slowest, 1)]
    if totals['failed']:
        lines += ['', 'Failed tests:', '', '| Binary | Test |', '| --- | --- |']
        lines += [f'| {cell(c["binary"])} | {cell(c["test"])} |' for c in totals['failed']]
    return '\n'.join(lines) + '\n'


def number(value, places=3):
    return None if value is None else float(fixed(value, places))


def summary_json(totals, top):
    exclusive = totals['exclusive']
    document = {
        'schema': 'af.test-time-summary/1',
        'wall_seconds': number(totals['wall_seconds']), 'tests': totals['tests'],
        'test_seconds': number(totals['test_seconds']),
        'untimed_tests': totals['untimed_tests'],
        'parallelism': number(totals['parallelism']),
        'exclusive': None if exclusive is None else {
            'tests': exclusive['tests'], 'seconds': number(exclusive['seconds'])},
        'failures': totals['failures'], 'failed': totals['failed'],
        'histogram': [{'bucket': b['bucket'], 'count': b['count'],
                       'seconds': number(b['seconds']), 'share': number(b['share'], 4)}
                      for b in totals['histogram']],
        'slowest': [{'binary': c['binary'], 'test': c['test'], 'seconds': number(c['seconds'])}
                    for c in totals['slowest'][:top]],
    }
    return json.dumps(document, indent=2, sort_keys=True) + '\n'


def median(values):
    """The median of the known values, exact (a mean of two decimals at worst); else None."""
    known = [Decimal(v) for v in values if v is not None]
    return statistics.median(known) if known else None


def side(runs, patterns):
    """Medians of each total over one side's runs, and each test's median time."""
    totals = [summarize(run, patterns) for run in runs]
    medians = {key: median([t[key] for t in totals])
               for key in ['wall_seconds', 'tests', 'test_seconds', 'parallelism', 'failures']}
    medians['exclusive'] = (None if patterns is None
                            else median([t['exclusive']['seconds'] for t in totals]))
    times = {}
    for run in runs:
        for case in run['cases']:
            times.setdefault((case['binary'], case['test']), []).append(case['seconds'])
    return medians, {key: median(values) for key, values in times.items()}


def signed(value, render):
    return ('+' if value > 0 else '') + render(value)


def compare_markdown(base_runs, head_runs, patterns):
    base, base_tests = side(base_runs, patterns)
    head, head_tests = side(head_runs, patterns)
    count = str
    rows = [('Wall (nextest)', 'wall_seconds', secs), ('Tests', 'tests', count),
            ('Test-seconds (summed)', 'test_seconds', secs),
            ('Exclusive block', 'exclusive', secs),
            ('Achieved parallelism', 'parallelism', ratio), ('Failures', 'failures', count)]
    lines = [f'### Test time: base ({counted(len(base_runs), "run")}) '
             f'vs head ({counted(len(head_runs), "run")})', '',
             'Medians per side.', '',
             '| Total | Base | Head | Delta | Change |', '| --- | ---: | ---: | ---: | ---: |']
    for label, key, render in rows:
        old, new = base[key], head[key]
        if old is None or new is None:
            delta = change = '—'
        else:
            delta = signed(new - old, render)
            change = signed((new - old) / old, percent) if old else '—'
        shown = [('unknown' if v is None else render(v)) for v in (old, new)]
        lines.append(f'| {label} | {shown[0]} | {shown[1]} | {delta} | {change} |')
    changes = []
    for key in sorted(base_tests.keys() | head_tests.keys()):
        old, new = base_tests.get(key, MISSING), head_tests.get(key, MISSING)
        if old is MISSING:
            changes.append((1, Decimal(0), key, '—', secs(new), 'added'))
        elif new is MISSING:
            changes.append((2, Decimal(0), key, secs(old), '—', 'removed'))
        elif old is not None and new is not None and abs(new - old) >= MIN_CHANGE:
            changes.append((0, -abs(new - old), key, secs(old), secs(new),
                            signed(new - old, secs)))
    lines += ['', f'Tests that changed by at least {MIN_CHANGE} s, were added or were removed '
              '(median time per side):', '']
    if changes:
        lines += ['| Binary | Test | Base | Head | Delta |', '| --- | --- | ---: | ---: | ---: |']
        lines += [f'| {cell(b)} | {cell(t)} | {old} | {new} | {delta} |'
                  for _, _, (b, t), old, new, delta in sorted(changes)]
    else:
        lines.append('None.')
    return '\n'.join(lines) + '\n'



def parser():
    top = argparse.ArgumentParser(prog='test-time-report.py', description=__doc__.split('\n')[0])
    commands = top.add_subparsers(dest='command', required=True)
    summary = commands.add_parser('summary', help='summarize one nextest JUnit report')
    summary.add_argument('junit', type=Path)
    summary.add_argument('--nextest-config', type=Path)
    summary.add_argument('--top', type=int, default=DEFAULT_TOP)
    summary.add_argument('--format', choices=['markdown', 'json'], default='markdown')
    compare = commands.add_parser(
        'compare', usage='%(prog)s [--nextest-config PATH] BASE_JUNIT... -- HEAD_JUNIT...',
        help='compare base and head runs by their medians')
    compare.add_argument('--nextest-config', type=Path)
    compare.add_argument('base', type=Path, nargs='+')
    return top


def main(argv):
    head = []
    if argv[:1] == ['compare']:
        if '--' not in argv:
            parser().error('compare needs BASE_JUNIT... -- HEAD_JUNIT...')
        split = argv.index('--')
        argv, head = argv[:split], [Path(p) for p in argv[split + 1:]]
        if not head:
            parser().error('compare needs at least one HEAD_JUNIT after --')
    args = parser().parse_args(argv)
    if args.command == 'summary' and args.top < 0:
        parser().error('--top must not be negative')
    try:
        patterns = exclusive_patterns(args.nextest_config or DEFAULT_CONFIG,
                                      args.nextest_config is not None)
        if args.command == 'summary':
            totals = summarize(read_run(args.junit), patterns)
            render = summary_json if args.format == 'json' else summary_markdown
            sys.stdout.write(render(totals, args.top))
        else:
            sys.stdout.write(compare_markdown([read_run(p) for p in args.base],
                                              [read_run(p) for p in head], patterns))
    except ReportError as error:
        print(f'test-time-report: {error}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
