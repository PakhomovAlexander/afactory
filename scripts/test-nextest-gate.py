#!/usr/bin/env python3
"""Exercise the real gate entry on a tiny sealed-source Rust project, offline: its private
JUnit store, the isolation rerun of failed tests and the report a Task check reads."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

# The gate script is loaded as a module below; no bytecode is left beside it.
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
NL = chr(10)
MANY = 26  # one more failed test than the isolation rerun's bound


def run(command, repo, env):
    result = subprocess.run(command, cwd=repo, env=env, capture_output=True, text=True)
    print(result.stdout, end='')
    print(result.stderr, end='')
    return result


def manifest(repo):
    return {str(p.relative_to(repo)): (hashlib.sha256(p.read_bytes()).hexdigest(),
                                     p.stat().st_mode & 0o777)
            for p in repo.rglob('*') if p.is_file()}


# `fails_when_crowded` fails exactly when `crowd` runs beside it: `crowd` holds a marker until
# the other has seen it. Alone, as the isolation rerun runs it, it sees nothing and passes.
LIB = NL.join([
    'use std::path::PathBuf;',
    'use std::time::{Duration, Instant};',
    'fn crowd_dir() -> Option<PathBuf> { std::env::var_os("AF_GATE_CROWD_DIR").map(PathBuf::from) }',
    'fn wait_for(path: &std::path::Path, seconds: u64) -> bool {',
    '    let deadline = Instant::now() + Duration::from_secs(seconds);',
    '    while Instant::now() < deadline {',
    '        if path.exists() { return true; }',
    '        std::thread::sleep(Duration::from_millis(20));',
    '    }',
    '    false',
    '}',
    '#[test] fn always_passes() { assert_eq!(2+2,4); }',
    '#[test] fn controlled_failure() {',
    'assert!(std::env::var_os("AF_STORE_TEST_FAIL").is_none()); }',
    '#[test] fn crowd() {',
    '    let Some(dir) = crowd_dir() else { return };',
    '    std::fs::write(dir.join("marker"), b"").unwrap();',
    '    wait_for(&dir.join("seen"), 60);',
    '    std::fs::remove_file(dir.join("marker")).unwrap();',
    '}',
    '#[test] fn fails_when_crowded() {',
    '    let Some(dir) = crowd_dir() else { return };',
    '    if wait_for(&dir.join("marker"), 10) {',
    '        std::fs::write(dir.join("seen"), b"").unwrap();',
    '        panic!("another test was running beside this one");',
    '    }',
    '}',
] + [f'#[test] fn many_{n}() {{ assert!(std::env::var_os("AF_GATE_MANY_FAIL").is_none()); }}'
     for n in range(MANY)] + [''])

# (label, environment, gate passes, flaky, failed, rerun, failed count)
SCENARIOS = [
    ('passing', {}, True, [], [], 'not_needed', 0),
    ('failing twice', {'AF_STORE_TEST_FAIL': '1'}, False, [], ['controlled_failure'],
     'isolated', 1),
    ('flaky', {'AF_GATE_CROWD_DIR': None}, True, ['fails_when_crowded'], [], 'isolated', 0),
    ('flaky and failing twice', {'AF_GATE_CROWD_DIR': None, 'AF_STORE_TEST_FAIL': '1'}, False,
     ['fails_when_crowded'], ['controlled_failure'], 'isolated', 1),
    ('over the rerun bound', {'AF_GATE_MANY_FAIL': '1'}, False, [],
     sorted(f'many_{n}' for n in range(MANY)), 'over_bound', MANY),
]


def gate_module():
    spec = importlib.util.spec_from_file_location('nextest_gate', ROOT / 'scripts' /
                                                  'nextest-gate.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def check_rerun_bounds():
    """The rerun stops at its time bound and keeps every test it did not show flaky failed; it
    reruns nothing above its test bound; and `--repeat` is an argument, never the environment."""
    gate = gate_module()
    now = [0.0]

    def clock():
        return now[0]

    ran = []

    def run_one(test, left):
        ran.append((test, left))
        now[0] += 400
        return test[1] != 'b'

    failed = [('x', 'a'), ('x', 'b'), ('x', 'c'), ('x', 'd')]
    flaky, again, state = gate.rerun_alone(failed, run_one, clock)
    assert (flaky, again, state) == ([('x', 'a'), ('x', 'c')], [('x', 'b'), ('x', 'd')],
                                     'over_bound'), (flaky, again, state)
    assert [left for _, left in ran] == [900, 500, 100], ran
    ran.clear()
    now[0] = 0.0
    assert gate.rerun_alone(failed, run_one, clock, max_seconds=10_000) == (
        [('x', 'a'), ('x', 'c'), ('x', 'd')], [('x', 'b')], 'isolated')
    many = [('x', str(n)) for n in range(26)]
    assert gate.rerun_alone(many, lambda *_: True) == ([], many, 'over_bound')
    os.environ['NEXTEST_REPEAT'] = '3'
    try:
        assert gate.own_options(['--profile', 'ci']) == (None, ['--profile', 'ci'])
    finally:
        del os.environ['NEXTEST_REPEAT']
    assert gate.own_options(['--repeat', '20', '--profile', 'ci']) == (20, ['--profile', 'ci'])
    assert gate.without_selection(['--locked', '--test-threads', '4', '-E', 'all()',
                                   '--profile=ci', '-j=2']) == ['--locked', '--profile=ci']
    print('PASS: the isolation rerun stops at its time and test bounds and fails with every '
          'test it did not show flaky; --repeat is an argument only')


def report_lines(path):
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def main():
    check_rerun_bounds()
    with tempfile.TemporaryDirectory(prefix='af gate space " ') as tmp:
        base = Path(tmp)
        repo = base / 'source'
        (repo / 'src').mkdir(parents=True)
        (repo / 'scripts').mkdir()
        (repo / '.config').mkdir()
        for name in ['verify.sh', 'verify-body.sh', 'gate-owner.py', 'nextest-gate.py',
                     'ci-step.py', 'test-time-report.py', 'test-classes.py']:
            shutil.copyfile(ROOT / 'scripts' / name, repo / 'scripts' / name)
        # The test recipe starts nextest through the quarantine exclusion, which reads these.
        for name in ['nextest.toml', 'test-classes.json']:
            shutil.copyfile(ROOT / '.config' / name, repo / '.config' / name)
        # An empty quarantine, not this repository's: its identities name binaries this
        # miniature crate does not build, and nextest refuses a `binary_id` that matches none.
        (repo / '.config' / 'test-quarantine.json').write_text(
            '{"schema": "af.test-quarantine/1", "tests": {}}\n')
        shutil.copyfile(ROOT / 'rust-toolchain.toml', repo / 'rust-toolchain.toml')
        # Use the actual Makefile test recipe; other gate stages do not apply to
        # this miniature crate. The production check target is not modified.
        makefile = (ROOT / 'Makefile').read_text()
        # `test` depends on `test-build` (the build receipt the kernel's phase monitor waits for),
        # so the slice starts at that target.
        recipe = makefile[makefile.index('test-build:'):makefile.index('# Open the release PR')]
        (repo / 'Makefile').write_text(NL.join([
            'TEST_RUNNER = nextest', 'TEST_THREADS = 4',
            'CI_STEP = python3 scripts/ci-step.py', 'check: test', recipe]))
        (repo / 'Cargo.toml').write_text(NL.join([
            '[package]', 'name="gate-store-regression"', 'version="0.1.0"',
            'edition="2021"', '']))
        (repo / 'src/lib.rs').write_text(LIB)
        # The children's temporary directory is a private one under `base`: under a managed
        # sandbox the platform default (`/var/folders/...`) is not writable, and rustc and
        # cargo fail to create their temporary files without a writable TMPDIR.
        env = {'PATH': os.environ['PATH'], 'LC_ALL': 'C', 'TZ': 'UTC',
               'HOME': str(base / 'home'), 'XDG_CACHE_HOME': str(base / 'cache'),
               'TMPDIR': str(base / 'tmp')}
        # Keep HOME/cache isolated without hiding the installed pinned toolchain
        # from rustup proxies. No ambient compiler flags or wrappers are inherited.
        for name, default in [('RUSTUP_HOME', '.rustup'), ('CARGO_HOME', '.cargo')]:
            env[name] = os.environ.get(name, str(Path.home() / default))
        Path(env['HOME']).mkdir()
        Path(env['TMPDIR']).mkdir()
        assert run(['cargo', 'generate-lockfile', '--offline'], repo, env).returncode == 0
        paths = [repo, *repo.rglob('*')]
        try:
            for p in paths:
                p.chmod(0o555 if p.is_dir() else 0o444)
            before = manifest(repo)
            # Explicitly prove that filesystem permissions enforce the seal.
            try:
                (repo / 'forbidden-write').write_text('must fail')
            except PermissionError:
                pass
            else:
                raise AssertionError('read-only fixture not enforced (run unprivileged)')
            target = Path(env['XDG_CACHE_HOME']) / 'af/gate-target'
            for index, (label, extra, passes, flaky, failed, rerun, failed_count) in enumerate(
                    SCENARIOS):
                selected = dict(env)
                for name, value in extra.items():
                    if value is None:
                        crowd = base / f'crowd-{index}'
                        crowd.mkdir()
                        value = str(crowd)
                    selected[name] = value
                report = base / f'report-{index}.jsonl'
                selected['AF_CHECK_REPORT'] = str(report)
                result = run(['bash', 'scripts/verify.sh'], repo, selected)
                assert (result.returncode == 0) == passes, label
                # The gate ends with the slow-test summary of the JUnit nextest just wrote: the
                # parallel run's, whose failures the rerun may have forgiven.
                tail = result.stdout[result.stdout.rindex('gate step timings: '):]
                assert f'{NL}### Test time: {4 + MANY} tests in ' in tail, (label, tail)
                assert (f'{NL}Failed tests:{NL}' in tail) == (label != 'passing'), (label, tail)
                full = lambda names: [f'gate-store-regression {name}' for name in names]
                assert ((f'{NL}flaky: ' + ', '.join(full(flaky)) + NL) in result.stdout) == \
                    bool(flaky), (label, result.stdout)
                if rerun == 'isolated' and failed:
                    assert f'{NL}failed twice: {", ".join(full(failed))}{NL}' in result.stdout, (
                        label, result.stdout)
                lines = report_lines(report)
                assert [(line['record'], line.get('label'), line.get('stage'))
                        for line in lines] == [('step', 'test-build', 'build'),
                                               ('tests', None, None),
                                               ('step', 'test', 'tests')], (label, lines)
                assert all(line['schema'] == 'af.check-report/1' for line in lines), lines
                tests = lines[1]
                assert tests['test_threads'] == 4, tests
                assert tests['rerun'] == rerun, (label, tests)
                assert tests['failed_count'] == failed_count, (label, tests)
                assert sorted(tests['failed']) == sorted(full(failed)), (label, tests)
                assert tests['flaky'] == [{'name': name, 'parallel': 'failed',
                                           'isolated': 'passed'} for name in full(flaky)], (
                    label, tests)
                assert lines[2]['exit_code'] == (0 if passes else 100), (label, lines)
                assert manifest(repo) == before
                assert not (repo / 'target').exists()
                assert not list(target.rglob('store.toml'))
            runs = sorted(target.glob('nextest-reports/run-*'))
            assert len(runs) == len(SCENARIOS), runs
            reports = [run / 'store/ci/junit.xml' for run in runs]
            cases = [ET.parse(p).findall('.//testcase') for p in reports]
            assert all(len(c) == 4 + MANY for c in cases), cases
            failures = sorted(sum(c.find('failure') is not None for c in cs) for cs in cases)
            assert failures == [0, 1, 1, 2, MANY], failures
            # Every isolated rerun ran exactly the one test it names, alone.
            isolated = sorted(target.glob('nextest-reports/run-*/isolated-*/store/ci/junit.xml'))
            assert len(isolated) == 4, isolated
            assert all(len(ET.parse(p).findall('.//testcase')) == 1 for p in isolated)
            print('PASS: real verify entry, sealed source, external JUnit, failure propagation, '
                  'config cleanup, test-time summary, isolation rerun of failed tests alone '
                  '(flaky forgiven, failed twice fails, over 25 none rerun), check report lines')
        finally:
            for p in paths:
                p.chmod(0o755 if p.is_dir() else 0o644)


if __name__ == '__main__':
    main()
