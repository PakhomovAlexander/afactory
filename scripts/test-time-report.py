#!/usr/bin/env python3
"""Summarize where test time goes in a nextest JUnit report, and compare runs.

    test-time-report.py summary JUNIT [--nextest-config .config/nextest.toml] [--top N]
                                      [--format markdown|json]
    test-time-report.py compare [--nextest-config PATH] BASE_JUNIT... -- HEAD_JUNIT...

Times are read as the exact decimals nextest wrote and summed as decimals, so the wall time
and test count are nextest's own and the totals carry no float drift; testcases that do not add
up to the root's declared `tests`, `failures` or `errors` are an error, not a summary. The
config is read with a small TOML subset reader (Python 3.9 has no tomllib). The exclusive block is
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
            kinds = {kind for kind in ('failure', 'error') if case.find(kind) is not None}
            cases.append({'binary': binary, 'test': name, 'failed': bool(kinds), 'kinds': kinds,
                          'seconds': None if time is None else seconds(time, f'{path}: {name}')})
    # The root's own counts are nextest's; testcases that do not add up to them are not this
    # run's whole report, so no total is printed from them.
    for attribute, count in [('tests', len(cases)),
                             ('failures', sum('failure' in c['kinds'] for c in cases)),
                             ('errors', sum('error' in c['kinds'] for c in cases))]:
        declared = root.get(attribute)
        if declared is not None and declared.strip() != str(count):
            raise ReportError(f'{path}: <testsuites> declares {attribute}="{declared}", '
                              f'its testcases count {count}')
    return {'wall': None if wall is None else seconds(wall, f'{path}: <testsuites>'),
            'cases': cases}


class Toml:
    """The TOML subset nextest configs here use, without tomllib (Python 3.9 has none): tables,
    arrays of tables, bare or quoted keys, all four string forms, integers, booleans, arrays and
    inline tables, with comments. Anything else (floats, dates, dotted keys) is a ReportError,
    never a guess."""

    BARE_KEY = re.compile(r'[A-Za-z0-9_-]+')
    INTEGER = re.compile(r'[+-]?(?:0|[1-9](?:_?[0-9])*)(?![0-9A-Za-z_.:+-])')
    ESCAPES = {'b': '\b', 't': '\t', 'n': '\n', 'f': '\f', 'r': '\r', '"': '"', '\\': '\\'}

    def __init__(self, text, where):
        self.text, self.where, self.pos = text, where, 0
        self.defined = set()

    def error(self, message):
        line = self.text.count('\n', 0, self.pos) + 1
        raise ReportError(f'cannot read nextest config {self.where}: line {line}: {message}')

    def peek(self, token):
        return self.text.startswith(token, self.pos)

    def expect(self, token):
        if not self.peek(token):
            self.error(f'expected {token!r}')
        self.pos += len(token)

    def spaces(self):
        while self.pos < len(self.text) and self.text[self.pos] in ' \t':
            self.pos += 1

    def comment(self):
        if self.peek('#'):
            end = self.text.find('\n', self.pos)
            self.pos = len(self.text) if end < 0 else end

    def blank(self):
        """Whitespace, newlines and comments."""
        while True:
            self.spaces()
            self.comment()
            if self.peek('\r\n'):
                self.pos += 2
            elif self.peek('\n'):
                self.pos += 1
            else:
                return

    def end_of_line(self):
        self.spaces()
        self.comment()
        if self.pos < len(self.text) and not (self.peek('\n') or self.peek('\r\n')):
            self.error('expected the end of the line')

    def document(self):
        root = table = {}
        while True:
            self.blank()
            if self.pos == len(self.text):
                return root
            if self.peek('[['):
                table = self.header(root, '[[', ']]')
            elif self.peek('['):
                table = self.header(root, '[', ']')
            else:
                self.pair(table)
            self.end_of_line()

    def keys(self):
        keys = [self.key()]
        self.spaces()
        while self.peek('.'):
            self.pos += 1
            self.spaces()
            keys.append(self.key())
            self.spaces()
        return keys

    def key(self):
        if self.peek('"') or self.peek("'"):
            if self.peek('"""') or self.peek("'''"):
                self.error('a multi-line string is not a key')
            return self.string()
        match = self.BARE_KEY.match(self.text, self.pos)
        if not match:
            self.error('expected a key')
        self.pos = match.end()
        return match.group()

    def header(self, root, opening, closing):
        self.pos += len(opening)
        self.spaces()
        keys = self.keys()
        self.expect(closing)
        node, path = root, ()
        for key in keys[:-1]:
            node, path = self.descend(node, key, path)
        last = keys[-1]
        if opening == '[[':
            tables = node.setdefault(last, [])
            if not isinstance(tables, list):
                self.error(f'{".".join(keys)} is not an array of tables')
            tables.append({})
            return tables[-1]
        path += (last,)
        if path in self.defined or not isinstance(node.setdefault(last, {}), dict):
            self.error(f'table {".".join(keys)} is defined twice')
        self.defined.add(path)
        return node[last]

    def descend(self, node, key, path):
        child = node.setdefault(key, {})
        if isinstance(child, list) and child and isinstance(child[-1], dict):
            return child[-1], path + (key, len(child) - 1)
        if not isinstance(child, dict):
            self.error(f'{key} is not a table')
        return child, path + (key,)

    def pair(self, table):
        keys = self.keys()
        if len(keys) != 1:
            self.error('dotted keys are outside the subset this report reads')
        self.expect('=')
        self.spaces()
        if keys[0] in table:
            self.error(f'key {keys[0]} is defined twice')
        table[keys[0]] = self.value()

    def value(self):
        if self.peek('"') or self.peek("'"):
            return self.string()
        if self.peek('{'):
            return self.inline_table()
        if self.peek('['):
            return self.array()
        for word, value in (('true', True), ('false', False)):
            if self.peek(word):
                self.pos += len(word)
                return value
        match = self.INTEGER.match(self.text, self.pos)
        if not match:
            self.error('expected a string, integer, boolean, array or inline table')
        self.pos = match.end()
        return int(match.group().replace('_', ''))

    def string(self):
        for quote, literal in (("'''", True), ('"""', False)):
            if self.peek(quote):
                self.pos += len(quote)
                # A newline right after the opening delimiter is trimmed.
                if self.peek('\r\n'):
                    self.pos += 2
                elif self.peek('\n'):
                    self.pos += 1
                return self.characters(quote, literal, multiline=True)
        quote = self.text[self.pos]
        self.pos += 1
        return self.characters(quote, quote == "'", multiline=False)

    def characters(self, quote, literal, multiline):
        out = []
        while True:
            if self.pos >= len(self.text):
                self.error('unterminated string')
            if self.peek(quote):
                run = len(quote)
                # Up to two more quotes before the closing delimiter are content: `'''a'''''`.
                while multiline and self.text.startswith(quote[0], self.pos + run):
                    run += 1
                if run > len(quote) + 2:
                    self.error('too many quotes closing a multi-line string')
                out.append(quote[0] * (run - len(quote)))
                self.pos += run
                return ''.join(out)
            char = self.text[self.pos]
            if char == '\n' and not multiline:
                self.error('newline in a single-line string')
            if char == '\\' and not literal:
                out.append(self.escape(multiline))
                continue
            out.append(char)
            self.pos += 1

    def escape(self, multiline):
        self.pos += 1
        char = self.text[self.pos:self.pos + 1]
        if char in self.ESCAPES:
            self.pos += 1
            return self.ESCAPES[char]
        if char in ('u', 'U'):
            width = 4 if char == 'u' else 8
            digits = self.text[self.pos + 1:self.pos + 1 + width]
            if len(digits) != width or not all(c in '0123456789abcdefABCDEF' for c in digits):
                self.error(f'bad \\{char} escape')
            self.pos += 1 + width
            return chr(int(digits, 16))
        if multiline:
            # A line-ending backslash trims the newline and the whitespace after it.
            start = self.pos
            self.spaces()
            if self.peek('\n') or self.peek('\r\n'):
                self.blank_lines()
                return ''
            self.pos = start
        self.error(f'bad escape \\{char}')

    def blank_lines(self):
        while self.pos < len(self.text) and self.text[self.pos] in ' \t\r\n':
            self.pos += 1

    def inline_table(self):
        self.pos += 1
        table = {}
        self.spaces()
        if self.peek('}'):
            self.pos += 1
            return table
        while True:
            self.spaces()
            self.pair(table)
            self.spaces()
            if self.peek('}'):
                self.pos += 1
                return table
            self.expect(',')

    def array(self):
        self.pos += 1
        values = []
        while True:
            self.blank()
            if self.peek(']'):
                self.pos += 1
                return values
            values.append(self.value())
            self.blank()
            if self.peek(']'):
                self.pos += 1
                return values
            self.expect(',')


def parse_toml(text, where):
    """The config as tomllib would read it, for the subset `Toml` accepts."""
    return Toml(text, where).document()


def exclusive_patterns(path, explicit):
    """Regexes of the exclusive overrides' `test(/.../)` filters, or None without a config."""
    if not path.is_file():
        if explicit:
            raise ReportError(f'nextest config {path} does not exist')
        return None
    try:
        text = path.read_text(encoding='utf-8')
    except (OSError, UnicodeDecodeError) as error:
        raise ReportError(f'cannot read nextest config {path}: {error}') from None
    config = parse_toml(text, path)
    profiles = config.get('profile', {})
    if not isinstance(profiles, dict):
        raise ReportError(f'{path}: profile is not a table')
    patterns = []
    for name, profile in sorted(profiles.items()):
        overrides = profile.get('overrides', []) if isinstance(profile, dict) else None
        if not isinstance(overrides, list) or not all(isinstance(o, dict) for o in overrides):
            raise ReportError(f'{path}: profile.{name}.overrides is not an array of tables')
        for override in overrides:
            if not isinstance(override.get('filter', ''), str):
                raise ReportError(f'{path}: profile.{name}.overrides filter is not a string')
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
