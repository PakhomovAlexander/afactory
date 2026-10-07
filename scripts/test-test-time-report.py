#!/usr/bin/env python3
"""Exercise `test-time-report.py` through its real entry, and its wiring into `make test` and
the Task gate with a stand-in `cargo`, offline. It runs under Python 3.9, the Task gate's.

The summary's totals are checked exactly against `fixtures/test-time-report/`, a small nextest
JUnit and nextest config; the config reader is checked on the real `.config/nextest.toml`. The
wiring scenarios prove that the report never changes the test step's exit status, whether the
JUnit is there, left by an earlier run, miscounted, unreadable or the script is gone.
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts' / 'test-time-report.py'
FIXTURE = ROOT / 'fixtures' / 'test-time-report'
CONFIG = ['--nextest-config', str(FIXTURE / 'nextest.toml')]
WARNING = 'warning: no test-time report'
SPEC = importlib.util.spec_from_file_location('test_time_report', SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
# The real exclusive block (ADR-0124), as `.config/nextest.toml` lists it.
EXCLUSIVE = ['::identity_rechecks_cancel_an_inflight_status_process$',
             '::codex_subscription_probe_reaps_descendants_after_an_early_exit$',
             'setup_repairs_auth_directory_and_lock_modes_under_a_restrictive_umask$',
             'typed_reply_never_falls_back_and_failed_outputs_keep_accounting$',
             'light_strategy_generates_one_candidate_without_exposing_source_to_author_workers$']


def report(*args, cwd=None):
    """Run the script and return (exit code, stdout, stderr)."""
    result = subprocess.run([sys.executable, '-I', str(SCRIPT), *map(str, args)],
                            capture_output=True, text=True, cwd=cwd or ROOT, env={})
    return result.returncode, result.stdout, result.stderr


def summary(*args, cwd=None):
    code, out, err = report('summary', *args, cwd=cwd)
    assert code == 0, (code, out, err)
    return out


def junit(path, cases, wall='10.000', **declared):
    """Write a nextest-shaped JUnit; each case is (binary, test, time or None[, failure tag]).
    The root declares nextest's counts of them unless `declared` overrides one (None omits)."""
    suites = {}
    for binary, test, seconds, *failure in cases:
        suites.setdefault(binary, []).append((test, seconds, failure))
    counts = {'tests': len(cases), 'failures': sum(c[3:] == ('failure',) for c in cases),
              'errors': sum(c[3:] == ('error',) for c in cases)}
    counts.update(declared)
    attributes = ''.join(f' {k}="{v}"' for k, v in counts.items() if v is not None)
    lines = ['<?xml version="1.0" encoding="UTF-8"?>',
             '<testsuites name="nextest-run"%s%s>' % (
                 attributes, '' if wall is None else f' time="{wall}"')]
    for binary, tests in suites.items():
        lines.append(f'<testsuite name="{binary}" tests="{len(tests)}">')
        for test, seconds, failure in tests:
            timed = '' if seconds is None else f' time="{seconds}"'
            lines.append(f'<testcase name="{test}" classname="{binary}"{timed}>')
            if failure:
                lines.append(f'<{failure[0]} type="test failure">panicked</{failure[0]}>')
            lines.append('</testcase>')
        lines.append('</testsuite>')
    lines.append('</testsuites>')
    path.write_text('\n'.join(lines) + '\n', encoding='utf-8')
    return path


def table_row(text, first):
    return next(line for line in text.splitlines() if line.startswith(f'| {first} |'))


def check_fixture_totals():
    document = json.loads(summary(FIXTURE / 'junit.xml', *CONFIG, '--format', 'json'))
    assert document == {
        'schema': 'af.test-time-summary/1', 'wall_seconds': 7.0, 'tests': 6,
        'test_seconds': 8.6, 'untimed_tests': 0, 'parallelism': 1.229,
        # Anchored filters: `unit::alone_with_a_budget_twice` is not exclusive, and the
        # override that needs only two threads is not the exclusive block.
        'exclusive': {'tests': 2, 'seconds': 6.25},
        'failures': 1, 'failed': [{'binary': 'demo::it', 'test': 'mid::two'}],
        'histogram': [
            {'bucket': '<0.1', 'count': 1, 'seconds': 0.05, 'share': 0.0058},
            {'bucket': '0.1-1', 'count': 2, 'seconds': 0.8, 'share': 0.093},
            {'bucket': '1-3', 'count': 2, 'seconds': 3.75, 'share': 0.436},
            {'bucket': '3-10', 'count': 1, 'seconds': 4.0, 'share': 0.4651},
            {'bucket': '10-30', 'count': 0, 'seconds': 0.0, 'share': 0.0},
            {'bucket': '30-60', 'count': 0, 'seconds': 0.0, 'share': 0.0},
            {'bucket': '>=60', 'count': 0, 'seconds': 0.0, 'share': 0.0}],
        'slowest': [
            {'binary': 'demo::it', 'test': 'slow::alone_with_a_budget', 'seconds': 4.0},
            {'binary': 'demo::it', 'test': 'other::measured_elapsed', 'seconds': 2.25},
            {'binary': 'demo::it', 'test': 'mid::two', 'seconds': 1.5},
            {'binary': 'demo', 'test': 'unit::alone_with_a_budget_twice', 'seconds': 0.7},
            {'binary': 'demo', 'test': 'unit::a', 'seconds': 0.1},
            {'binary': 'demo::it', 'test': 'fast::one', 'seconds': 0.05}],
    }, document
    text = summary(FIXTURE / 'junit.xml', *CONFIG)
    assert text.startswith('### Test time: 6 tests in 7.000 s\n'), text
    assert table_row(text, 'Wall (nextest)') == '| Wall (nextest) | 7.000 s |'
    assert table_row(text, 'Tests') == '| Tests | 6 |'
    assert table_row(text, 'Test-seconds (summed)') == '| Test-seconds (summed) | 8.600 s |'
    assert table_row(text, 'Achieved parallelism') == '| Achieved parallelism | 1.23 |'
    assert table_row(text, 'Exclusive block') == (
        '| Exclusive block | 6.250 s, 2 tests, 89.3% of wall |')
    assert table_row(text, 'Failures') == '| Failures | 1 |'
    assert table_row(text, '1-3') == '| 1-3 | 2 | 3.750 | 43.6% |'
    assert table_row(text, '1') == '| 1 | 4.000 | demo::it | slow::alone_with_a_budget |'
    assert 'Tests without a time' not in text
    assert text.endswith('Failed tests:\n\n| Binary | Test |\n| --- | --- |\n'
                         '| demo::it | mid::two |\n'), text
    # Stable output: the same input renders the same bytes.
    assert summary(FIXTURE / 'junit.xml', *CONFIG) == text
    top = summary(FIXTURE / 'junit.xml', *CONFIG, '--top', '2')
    assert 'Slowest 2 tests:' in top and '| 3 |' not in top, top
    print('PASS: fixture totals, exclusive block, histogram, slowest and failures')


def check_config_lookup(tmp):
    # The default config is the working directory's .config/nextest.toml.
    in_repo = json.loads(summary(FIXTURE / 'junit.xml', '--format', 'json', cwd=ROOT))
    assert in_repo['exclusive'] == {'tests': 0, 'seconds': 0.0}, in_repo
    elsewhere = summary(FIXTURE / 'junit.xml', cwd=tmp)
    assert table_row(elsewhere, 'Exclusive block') == (
        '| Exclusive block | unknown (no nextest config) |'), elsewhere
    code, _, err = report('summary', FIXTURE / 'junit.xml', '--nextest-config',
                          tmp / 'absent.toml', cwd=tmp)
    assert code == 2 and 'does not exist' in err, (code, err)
    print('PASS: default and explicit nextest config')


def check_toml_subset():
    real = MODULE.parse_toml((ROOT / '.config/nextest.toml').read_text(encoding='utf-8'), 'real')
    ci = real['profile']['ci']
    assert (ci['retries'], ci['fail-fast'], ci['junit']) == (0, False, {'path': 'junit.xml'}), ci
    assert ci['slow-timeout'] == {'period': '120s'}, ci
    [override] = ci['overrides']
    assert (override['threads-required'], override['priority']) == ('num-test-threads', 100)
    # The multi-line literal filter keeps its text byte for byte, without the leading newline.
    assert override['filter'] == ''.join(
        f'{"| " if i else ""}test(/{regex}/)\n' for i, regex in enumerate(EXCLUSIVE)), override
    patterns = MODULE.exclusive_patterns(ROOT / '.config/nextest.toml', True)
    assert [p.pattern for p in patterns] == EXCLUSIVE, patterns
    assert patterns[0].search('task::identity_rechecks_cancel_an_inflight_status_process')
    assert not patterns[0].search('task::identity_rechecks_cancel_an_inflight_status_process_x')
    fixture = MODULE.parse_toml((FIXTURE / 'nextest.toml').read_text(encoding='utf-8'), 'fixture')
    assert [o['threads-required'] for o in fixture['profile']['ci']['overrides']] == [
        'num-test-threads', 2], fixture
    samples = {
        'strings': (r'a = "x\t\"\u00e9" # c' '\n' r"b = 'C:\\p'" '\n'
                    'c = """\nl1\\\n   l2"""\n' "d = '''\nq''''' \n"),
        'arrays': ('a = [1, -2_0, +3]\nb = [\n  "x", # c\n  \'y\',\n]\nc = []\n'
                   'd = { x = true, "y z" = { } }\n'),
        'tables': ('[p.q]\nk = 1\n[p]\nj = 2\n[[p.r]]\na = 1\n[[p.r]]\na = 2\n'
                   '[p.r.s]\nb = 3\n[ "p" . t ]\n'),
    }
    expected = {
        'strings': {'a': 'x\t"\u00e9', 'b': 'C:\\\\p', 'c': 'l1l2', 'd': "q''"},
        'arrays': {'a': [1, -20, 3], 'b': ['x', 'y'], 'c': [], 'd': {'x': True, 'y z': {}}},
        'tables': {'p': {'q': {'k': 1}, 'j': 2, 'r': [{'a': 1}, {'a': 2, 's': {'b': 3}}],
                         't': {}}},
    }
    for name, text in samples.items():
        assert MODULE.parse_toml(text, name) == expected[name], (name, text)
    try:  # Where the standard library reads TOML (3.11+), the subset must read it the same.
        import tomllib
    except ImportError:
        tomllib = None
    if tomllib:
        for path in [ROOT / '.config/nextest.toml', FIXTURE / 'nextest.toml']:
            text = path.read_text(encoding='utf-8')
            assert MODULE.parse_toml(text, path) == tomllib.loads(text), path
        for name, text in samples.items():
            assert tomllib.loads(text) == expected[name], name
    # Outside the subset or not TOML at all: an error, never a guess.
    for text in ['a = 1.5\n', 'a.b = 1\n', 'a = 1\na = 2\n', 'a = "open\n', 'a = 1 b\n',
                 '[t]\n[t]\n', 'a = 1979-05-27\n', 'a = 01\n', 'a = "\\q"\n', '[[a]]\n[a]\n',
                 "a = 'x\ny'\n", 'a = inf\n', "a = '''x''''''\n"]:
        try:
            MODULE.parse_toml(text, 'bad')
        except MODULE.ReportError as error:
            assert str(error).startswith('cannot read nextest config bad: line '), error
        else:
            raise AssertionError(f'parsed outside the subset: {text!r}')
    print('PASS: the TOML subset reads the real .config/nextest.toml and the fixture'
          + (', as tomllib does' if tomllib else ' without tomllib'))


def check_declared_counts(tmp):
    cases = [('b', 'fails', '1.000', 'failure'), ('b', 'errors', '0.500', 'error'),
             ('b', 'passes', '0.250')]
    consistent = junit(tmp / 'counted.xml', cases, wall='2.000')
    assert json.loads(summary(consistent, *CONFIG, '--format', 'json'))['tests'] == 3
    # Undeclared counts are not checked: there is nothing to disagree with.
    undeclared = junit(tmp / 'undeclared.xml', cases, tests=None, failures=None, errors=None)
    assert json.loads(summary(undeclared, *CONFIG, '--format', 'json'))['failures'] == 2
    for name, declared, message in [
            ('tests', {'tests': 4}, 'declares tests="4", its testcases count 3'),
            ('failures', {'failures': 2}, 'declares failures="2", its testcases count 1'),
            ('errors', {'errors': 0}, 'declares errors="0", its testcases count 1'),
            ('garbled', {'tests': 'three'}, 'declares tests="three"')]:
        path = junit(tmp / f'miscounted-{name}.xml', cases, wall='2.000', **declared)
        for args in [[], ['--format', 'json']]:
            code, out, err = report('summary', path, *CONFIG, *args)
            assert code == 2 and not out and message in err, (name, code, out, err)
    print('PASS: a JUnit whose testcases do not add up to its declared counts has no summary')


def check_histogram_edges(tmp):
    edges = ['0', '0.099', '0.100', '0.999', '1.000', '2.999', '3.000', '9.999', '10.000',
             '29.999', '30.000', '59.999', '60.000', '125.500']
    path = junit(tmp / 'edges.xml', [('b', f't{i:02}', s) for i, s in enumerate(edges)],
                 wall='300.000')
    document = json.loads(summary(path, *CONFIG, '--format', 'json'))
    assert [(b['bucket'], b['count'], b['seconds']) for b in document['histogram']] == [
        ('<0.1', 2, 0.099), ('0.1-1', 2, 1.099), ('1-3', 2, 3.999), ('3-10', 2, 12.999),
        ('10-30', 2, 39.999), ('30-60', 2, 89.999), ('>=60', 2, 185.5)], document
    assert document['test_seconds'] == 333.694 and document['tests'] == 14, document
    print('PASS: histogram bucket edges (lower inclusive, upper exclusive)')


def check_failures_and_missing_times(tmp):
    path = junit(tmp / 'failed.xml', [
        ('b', 'fails', '1.250', 'failure'), ('b', 'errors', '0.250', 'error'),
        ('b', 'passes', '0.500'), ('b', 'untimed', None)], wall='1.000')
    document = json.loads(summary(path, *CONFIG, '--format', 'json'))
    assert document['failures'] == 2 and document['tests'] == 4, document
    assert document['failed'] == [{'binary': 'b', 'test': 'errors'},
                                  {'binary': 'b', 'test': 'fails'}], document
    # A testcase without a time counts as a test but adds no seconds and has no bucket.
    assert document['untimed_tests'] == 1 and document['test_seconds'] == 2.0, document
    assert sum(b['count'] for b in document['histogram']) == 3, document
    assert [c['test'] for c in document['slowest']] == ['fails', 'passes', 'errors'], document
    assert table_row(summary(path, *CONFIG), 'Tests without a time') == (
        '| Tests without a time | 1 |')
    # Without the run's time the wall and parallelism are unknown, never zero.
    no_wall = junit(tmp / 'no-wall.xml', [('b', 'passes', '0.500')], wall=None)
    document = json.loads(summary(no_wall, *CONFIG, '--format', 'json'))
    assert document['wall_seconds'] is None and document['parallelism'] is None, document
    text = summary(no_wall, *CONFIG)
    assert text.startswith('### Test time: 1 test in unknown\n'), text
    assert table_row(text, 'Achieved parallelism') == '| Achieved parallelism | unknown |'
    assert table_row(text, 'Exclusive block') == (
        '| Exclusive block | 0.000 s, 0 tests, — of wall |'), text
    for name, body in [('broken.xml', '<testsuites><testsuite>'),
                       ('wrong-root.xml', '<testsuite/>'),
                       ('bad-time.xml', '<testsuites time="soon"/>')]:
        (tmp / name).write_text(body, encoding='utf-8')
        code, out, err = report('summary', tmp / name, *CONFIG)
        assert code == 2 and not out and err.startswith('test-time-report: '), (name, err)
    code, _, err = report('summary', tmp / 'absent.xml', *CONFIG)
    assert code == 2 and 'cannot read JUnit' in err, err
    print('PASS: failed and errored testcases, missing time attributes, unreadable JUnit')


def check_compare(tmp):
    base = [junit(tmp / f'base-{i}.xml', [
        ('b', 'grows', grows), ('b', 'steady', '5.000'), ('b', 'removed', '0.300'),
        ('c', 'x::alone_with_a_budget', '3.000')], wall=wall)
        for i, (grows, wall) in enumerate([('1.000', '10.000'), ('2.000', '12.000'),
                                           ('9.000', '30.000')])]
    head = [junit(tmp / f'head-{i}.xml', [
        ('b', 'grows', grows), ('b', 'steady', steady), ('b', 'added', '0.001'),
        ('c', 'x::alone_with_a_budget', '2.000')], wall=wall)
        for i, (grows, steady, wall) in enumerate([('3.500', '5.400', '8.000'),
                                                   ('4.500', '5.600', '9.000')])]
    code, text, err = report('compare', *CONFIG, *base, '--', *head)
    assert code == 0, (code, err)
    assert text.startswith('### Test time: base (3 runs) vs head (2 runs)\n'), text
    # Medians per side: base walls 10, 12, 30 -> 12; head walls 8, 9 -> 8.5.
    assert table_row(text, 'Wall (nextest)') == (
        '| Wall (nextest) | 12.000 s | 8.500 s | -3.500 s | -29.2% |'), text
    assert table_row(text, 'Tests') == '| Tests | 4 | 4 | 0 | 0.0% |', text
    # Summed per run: base 9.3, 10.3, 17.3 -> 10.3; head 10.901, 12.101 -> 11.501.
    assert table_row(text, 'Test-seconds (summed)') == (
        '| Test-seconds (summed) | 10.300 s | 11.501 s | +1.201 s | +11.7% |'), text
    assert table_row(text, 'Exclusive block') == (
        '| Exclusive block | 3.000 s | 2.000 s | -1.000 s | -33.3% |'), text
    # Parallelism per run: base 0.93, 0.858, 0.577; head 1.363, 1.345. The delta is exact.
    assert table_row(text, 'Achieved parallelism') == (
        '| Achieved parallelism | 0.86 | 1.35 | +0.50 | +57.7% |'), text
    assert table_row(text, 'Failures') == '| Failures | 0 | 0 | 0 | — |', text
    changes = text.split('(median time per side):\n\n', 1)[1].splitlines()
    assert changes == [
        '| Binary | Test | Base | Head | Delta |', '| --- | --- | ---: | ---: | ---: |',
        # grows: base 1, 2, 9 -> 2; head 3.5, 4.5 -> 4. steady moves 0.5 s and is not listed.
        '| b | grows | 2.000 s | 4.000 s | +2.000 s |',
        '| c | x::alone_with_a_budget | 3.000 s | 2.000 s | -1.000 s |',
        '| b | added | — | 0.001 s | added |',
        '| b | removed | 0.300 s | — | removed |'], changes
    same = report('compare', *CONFIG, base[0], '--', base[0])[1]
    assert same.startswith('### Test time: base (1 run) vs head (1 run)\n'), same
    assert same.endswith('(median time per side):\n\nNone.\n'), same
    for argv in [['compare', base[0]], ['compare', base[0], '--'], ['compare', '--', base[0]]]:
        code, _, err = report(*argv)
        assert code == 2 and 'usage' in err, (argv, code, err)
    print('PASS: compare medians, deltas, per-test changes, added and removed tests')


def exclusive_config(path, regexes):
    """A nextest config whose exclusive override lists exactly these test regexes."""
    filters = '\n| '.join(f'test(/{regex}/)' for regex in regexes)
    path.write_text(f"[[profile.ci.overrides]]\nfilter = '''\n{filters}\n'''\n"
                    'threads-required = "num-test-threads"\npriority = 100\n', encoding='utf-8')
    return path


def check_compare_per_side_config(tmp):
    # A change that splits `x::split` out of the exclusive list and adds `x::joins` to it.
    base_config = exclusive_config(tmp / 'base-nextest.toml', ['::split$', '::stays$'])
    head_config = exclusive_config(tmp / 'head-nextest.toml', ['::stays$', '::joins$'])
    base = junit(tmp / 'per-side-base.xml', [
        ('x', 'x::split', '4.000'), ('x', 'x::stays', '2.000'), ('x', 'x::joins', '1.000'),
        ('x', 'x::never', '0.500')], wall='9.000')
    head = junit(tmp / 'per-side-head.xml', [
        ('x', 'x::split', '4.500'), ('x', 'x::stays', '2.250'), ('x', 'x::joins', '1.250'),
        ('x', 'x::never', '0.500')], wall='8.000')
    per_side = ['--base-nextest-config', base_config, '--head-nextest-config', head_config]
    code, text, err = report('compare', *per_side, base, '--', head)
    assert code == 0, (code, err)
    # Base: split + stays = 6; head: stays + joins = 3.5. One list for both sides gives
    # base 3 with the head's list, or head 6.75 with the base's.
    assert table_row(text, 'Exclusive block') == (
        '| Exclusive block | 6.000 s | 3.500 s | -2.500 s | -41.7% |'), text
    assert (f"Exclusive block from each side's nextest config: base `{base_config}`, "
            f'head `{head_config}`.\n') in text, text
    # Each per-side option falls back to --nextest-config, alone or beside the other.
    for args, row in [
            (['--nextest-config', base_config, '--head-nextest-config', head_config],
             '| Exclusive block | 6.000 s | 3.500 s | -2.500 s | -41.7% |'),
            (['--nextest-config', head_config, '--base-nextest-config', base_config],
             '| Exclusive block | 6.000 s | 3.500 s | -2.500 s | -41.7% |'),
            (['--nextest-config', base_config],
             '| Exclusive block | 6.000 s | 6.750 s | +0.750 s | +12.5% |'),
            (['--nextest-config', head_config],
             '| Exclusive block | 3.000 s | 3.500 s | +0.500 s | +16.7% |')]:
        code, text, err = report('compare', *args, base, '--', head)
        assert code == 0 and table_row(text, 'Exclusive block') == row, (args, code, err, text)
        # The configs are named only when the sides read different ones.
        named = "nextest config: base" in text
        assert named == (len(args) == 4), (args, text)
    # Without --nextest-config the other side reads the working directory's default config.
    code, text, err = report('compare', '--head-nextest-config', head_config, base, '--', head,
                             cwd=tmp)
    assert code == 0, (code, err)
    assert table_row(text, 'Exclusive block') == (
        '| Exclusive block | unknown | 3.500 s | — | — |'), text
    assert (f"nextest config: base `{Path('.config/nextest.toml')}`, head `{head_config}`."
            in text), text
    code, _, err = report('compare', '--base-nextest-config', tmp / 'absent.toml', base, '--',
                          head)
    assert code == 2 and 'does not exist' in err, (code, err)
    print('PASS: compare reads each side\'s exclusive block from its own nextest config')


FAKE_CARGO = '''
import json, os, shutil, sys
from pathlib import Path
args = sys.argv[1:]
if args[:1] == ['test']:
    sys.exit(0)
assert args[:2] == ['nextest', 'run'], args
store = Path('target/nextest')
if '--tool-config-file' in args:
    config = args[args.index('--tool-config-file') + 1].split(':', 1)[1]
    store = Path(json.loads(Path(config).read_text().split('dir = ', 1)[1]))
if os.environ.get('FAKE_JUNIT'):
    (store / 'ci').mkdir(parents=True, exist_ok=True)
    shutil.copyfile(os.environ['FAKE_JUNIT'], store / 'ci' / 'junit.xml')
sys.exit(int(os.environ['FAKE_EXIT']))
'''


def project(base, *, script=True):
    """A copy of the real test recipe, gate scripts and nextest config, with a stand-in cargo."""
    repo = base / 'repo'
    (repo / 'scripts').mkdir(parents=True)
    (repo / '.config').mkdir()
    names = ['verify.sh', 'nextest-gate.py', 'ci-step.py'] + (['test-time-report.py'] * script)
    for name in names:
        shutil.copyfile(ROOT / 'scripts' / name, repo / 'scripts' / name)
    shutil.copyfile(ROOT / '.config/nextest.toml', repo / '.config/nextest.toml')
    makefile = (ROOT / 'Makefile').read_text()
    start = makefile.index('\ntest:\n') + 1
    recipe = makefile[start:makefile.index('# Open the release PR')]
    (repo / 'Makefile').write_text('\n'.join([
        'TEST_RUNNER = nextest', 'TEST_THREADS = 4', 'CI_STEP = python3 scripts/ci-step.py',
        'check: test', recipe]))
    tools = base / 'bin'
    tools.mkdir()
    (tools / 'cargo').write_text(f'#!{sys.executable}\n' + FAKE_CARGO)
    (tools / 'cargo').chmod(0o755)
    # The wiring runs under the interpreter running this test, 3.9 in the Task gate.
    (tools / 'python3').symlink_to(sys.executable)
    return repo


def environment(base, exit_code, junit_path=None):
    """A clean environment whose `cargo` is the stand-in, exiting with `exit_code`."""
    env = {'PATH': f'{base / "bin"}{os.pathsep}{os.environ["PATH"]}', 'LC_ALL': 'C',
           'HOME': str(base), 'FAKE_EXIT': str(exit_code)}
    if junit_path:
        env['FAKE_JUNIT'] = str(junit_path)
    return env


def make_test(*, exit_code, junit_path, script, prior):
    """Run `make test` in a fresh project; return (result, test step record, step summary).
    `prior` is what an earlier run left where this run's JUnit goes: None, a JUnit as recent as
    this run, or a directory that cannot be removed."""
    with tempfile.TemporaryDirectory(prefix='af test time ') as tmp:
        base = Path(tmp)
        repo = project(base, script=script)
        metrics, step_summary = base / 'metrics.jsonl', base / 'step-summary.md'
        env = environment(base, exit_code, junit_path)
        env.update(AF_CI_METRICS=str(metrics), GITHUB_STEP_SUMMARY=str(step_summary))
        old = repo / 'target/nextest/ci/junit.xml'
        if prior == 'junit':
            old.parent.mkdir(parents=True)
            shutil.copyfile(FIXTURE / 'junit.xml', old)
        elif prior == 'directory':
            (old / 'kept').mkdir(parents=True)
        result = subprocess.run(['make', 'test'], cwd=repo, env=env, capture_output=True,
                                text=True)
        records = [json.loads(line) for line in metrics.read_text().splitlines()]
        step = next(r for r in records if r['label'] == 'test')
        written = step_summary.read_text() if step_summary.exists() else ''
        if prior == 'junit' and not junit_path:
            assert not old.exists(), 'the earlier JUnit outlived the run that replaced it'
        return result, step, written


def check_make_test_status(tmp):
    broken = tmp / 'broken.xml'
    broken.write_text('<testsuites>', encoding='utf-8')
    miscounted = tmp / 'miscounted.xml'
    miscounted.write_text((FIXTURE / 'junit.xml').read_text(encoding='utf-8').replace(
        'tests="6" failures="1"', 'tests="7" failures="1"', 1), encoding='utf-8')
    scenarios = [
        # (JUnit written, report script present, earlier run's leftover, summary expected,
        #  warning detail)
        (None, True, None, False, 'nextest wrote no JUnit'),
        (FIXTURE / 'junit.xml', True, None, True, None),
        (broken, True, None, False, 'cannot read JUnit'),
        (miscounted, True, None, False, 'declares tests="7", its testcases count 6'),
        (FIXTURE / 'junit.xml', False, None, False, None),
        # The earlier JUnit is removed before nextest starts, however recent: a run that
        # writes none has none.
        (None, True, 'junit', False, 'nextest wrote no JUnit'),
        (FIXTURE / 'junit.xml', True, 'junit', True, None),
        (None, True, 'directory', False, 'cannot remove the earlier JUnit')]
    for i, (junit_path, script, prior, expected, detail) in enumerate(scenarios):
        # Every outcome of the report, under a nextest that passes and one that fails.
        for exit_code in [0, [1, 3, 101][i % 3]]:
            result, step, written = make_test(exit_code=exit_code, junit_path=junit_path,
                                              script=script, prior=prior)
            label = (exit_code, junit_path and junit_path.name, script, prior)
            output = result.stdout + result.stderr
            assert step['exit_code'] == exit_code, (label, step, output)
            assert (result.returncode == 0) == (exit_code == 0), (label, result.returncode,
                                                                  output)
            assert ('### Test time:' in result.stdout) == expected, (label, output)
            assert ('### Test time: 6 tests in 7.000 s' in result.stdout) == expected, label
            assert (WARNING in result.stderr) != expected, (label, output)
            assert ('### Test time:' in written) == expected, (label, written)
            if detail:
                assert detail in result.stderr, (label, output)
    print('PASS: make test and its recorded test step keep nextest\'s exit status, passing or '
          'failing, with a missing, earlier, miscounted or unreadable JUnit and without the '
          'report script')


def check_gate_entry_status():
    for exit_code in [0, 1, 7]:
        with tempfile.TemporaryDirectory(prefix='af test time ') as tmp:
            base = Path(tmp)
            repo = project(base)
            result = subprocess.run(
                [sys.executable, 'scripts/nextest-gate.py', '--locked', '--profile', 'ci'],
                cwd=repo, env=environment(base, exit_code), capture_output=True, text=True)
            assert result.returncode == exit_code, (exit_code, result.stdout, result.stderr)
            assert WARNING in result.stderr, result.stderr
            # A reused run directory: its earlier JUnit is gone before nextest starts.
            run = base / 'target' / 'nextest-reports' / 'reused'
            old = run / 'store/ci/junit.xml'
            old.parent.mkdir(parents=True)
            shutil.copyfile(FIXTURE / 'junit.xml', old)
            env = environment(base, exit_code)
            env.update(AF_GATE_NEXTEST_TARGET=str(base / 'target'), AF_GATE_NEXTEST_RUN=str(run))
            result = subprocess.run(
                [sys.executable, 'scripts/nextest-gate.py', '--locked', '--profile', 'ci'],
                cwd=repo, env=env, capture_output=True, text=True)
            assert result.returncode == exit_code, (exit_code, result.stdout, result.stderr)
            assert f'{WARNING}: nextest wrote no JUnit at {old.resolve()}' in result.stderr, (
                result.stderr)
            assert '### Test time:' not in result.stdout and not old.exists(), result.stdout
    print('PASS: the gate entry returns nextest\'s exact exit status without a JUnit and never '
          'reports an earlier run\'s')


def check_task_gate():
    for exit_code, preset in [(0, False), (1, False), (0, True)]:
        with tempfile.TemporaryDirectory(prefix='af test time ') as tmp:
            base = Path(tmp)
            repo = project(base)
            target = base / 'gate-target'
            env = environment(base, exit_code, FIXTURE / 'junit.xml')
            env['AF_GATE_TARGET_DIR'] = str(target)
            if preset:
                env['AF_CI_METRICS'] = str(base / 'chosen.jsonl')
            result = subprocess.run(['bash', 'scripts/verify.sh'], cwd=repo, env=env,
                                    capture_output=True, text=True)
            output = result.stdout + result.stderr
            assert (result.returncode == 0) == (exit_code == 0), output
            runs = list(target.glob('nextest-reports/run-*'))
            assert len(runs) == 1, runs
            assert (runs[0] / 'store/ci/junit.xml').is_file(), output
            # verify.sh records every step's time beside the gate's nextest reports, unless the
            # caller already chose a file.
            metrics = base / 'chosen.jsonl' if preset else runs[0] / 'ci-metrics.jsonl'
            assert (runs[0] / 'ci-metrics.jsonl').exists() != preset, output
            records = [json.loads(line) for line in metrics.read_text().splitlines()]
            assert [r['label'] for r in records] == ['test-build', 'test'], records
            assert records[1]['exit_code'] == exit_code, records
            # The gate output ends with the slow-test summary of the JUnit in its run directory.
            tail = result.stdout[result.stdout.rindex('gate step timings: '):].splitlines()
            assert tail[0] == f'gate step timings: {metrics}', tail
            assert tail[1] == '### Test time: 6 tests in 7.000 s', tail
            assert tail[-1] == '| demo::it | mid::two |', tail
    print('PASS: the Task gate times every step beside its nextest reports and ends with the '
          'summary')


def main():
    with tempfile.TemporaryDirectory(prefix='af test time ') as tmp:
        tmp = Path(tmp)
        check_fixture_totals()
        check_config_lookup(tmp)
        check_toml_subset()
        check_declared_counts(tmp)
        check_histogram_edges(tmp)
        check_failures_and_missing_times(tmp)
        check_compare(tmp)
        check_compare_per_side_config(tmp)
        check_make_test_status(tmp)
    check_gate_entry_status()
    check_task_gate()


if __name__ == '__main__':
    main()
