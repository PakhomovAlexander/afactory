#!/usr/bin/env python3
"""Exercise the real gate entry on a tiny sealed-source Rust project, offline: its private
JUnit store, the isolation rerun of failed tests and the report a Task check reads."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
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
SPEC = importlib.util.spec_from_file_location(
    'make_test_fixture', ROOT / 'scripts/make-test-fixture.py')
MAKE_FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MAKE_FIXTURE)


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

# A tiny ignored test with the production selector's identity. It proves report placement and
# execution through the actual Makefile command, not containment (the real live probe does that).
PROBE_NAME = 'task_review_execute_checks::a_container_check_executes_on_the_native_task_host'
PROBE = NL.join([
    'mod task_review_execute_checks {',
    '    #[test] #[ignore]',
    '    fn a_container_check_executes_on_the_native_task_host() {',
    '        use std::io::Write;',
    '        let mut marker = std::fs::OpenOptions::new().create(true).append(true)',
    '            .open(std::env::var_os("AF_GATE_PROBE_MARKER").unwrap()).unwrap();',
    '        marker.write_all(b"executed").unwrap();',
    '        assert!(std::env::var_os("AF_GATE_PROBE_FAIL").is_none());',
    '    }',
    '}',
    '',
])


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
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / 'filter with spaces.txt'
        expression = ' | '.join(f'test(=module::case_{n})' for n in range(300))
        assert len(expression) > 1024
        path.write_text(expression)
        repeat, arguments = gate.own_options(['--repeat', '2', '--filterset-file', str(path),
                                               '--profile', 'ci'])
        assert repeat == 2 and arguments == ['-E', expression, '--profile', 'ci']
        assert gate.without_selection(arguments) == ['--profile', 'ci']
        try:
            gate.own_options(['--filterset-file'])
        except SystemExit:
            pass
        else:
            raise AssertionError('missing filter file argument was accepted')
        path.unlink()
        try:
            gate.own_options(['--filterset-file', str(path)])
        except FileNotFoundError:
            pass
        else:
            raise AssertionError('missing filter file fell back to an unfiltered run')
    assert gate.without_selection(['--locked', '--test-threads', '4', '-E', 'all()',
                                   '--profile=ci', '-j=2']) == ['--locked', '--profile=ci']
    print('PASS: the isolation rerun stops at its time and test bounds and fails with every '
          'test it did not show flaky; --repeat is an argument only')


def report_lines(path):
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def check_full_route(repo, base, env, before):
    """Execute the actual full wrapper/Make graph, with the shared tiny Rust fixture.

    Only unrelated CI payloads and sandbox probe bodies are controlled stand-ins. The
    ordinary Rust selection, exact ignored native selection, owner and report writers are real.
    The host's captured FULL gate must separately execute the actual live container bodies.
    """
    target = Path(env['XDG_CACHE_HOME']) / 'af/gate-target'
    for label, parallel, failure in [('full', False, ''), ('parallel-full', True, ''),
                                     ('parallel-build-fails', True, 'build'),
                                     ('missing-runtime', False, 'sandbox'),
                                     ('timeout-probe-fails', False, 'timeout'),
                                     ('native-probe-fails', False, 'native')]:
        marker, events = base / (label + '-native'), base / (label + '-events')
        report = base / (label + '-report.jsonl')
        selected = dict(env, FULL_ROUTE='1', AF_GATE_PROBE_MARKER=str(marker),
                        AF_GATE_ROUTE_EVENTS=str(events), AF_GATE_ROUTE_FAIL=failure,
                        AF_CHECK_REPORT=str(report))
        if parallel:
            selected['MAKEFLAGS'] = '-j4'
        if failure == 'native':
            selected['AF_GATE_PROBE_FAIL'] = '1'
        previous = set(target.glob('nextest-reports/run-*'))
        result = run(['bash', 'scripts/verify.sh', '--profile', 'full'], repo, selected)
        assert (result.returncode == 0) == (not failure), (label, result)
        runs = set(target.glob('nextest-reports/run-*')) - previous
        assert len(runs) == 1, (label, runs)
        run_dir = runs.pop()
        if failure == 'build':
            assert events.read_text().splitlines() == ['test-build']
            assert not marker.exists() and not list(run_dir.rglob('junit.xml'))
            records = report_lines(report)
            assert [(r['record'], r.get('label'), r.get('exit_code')) for r in records] == [
                ('step', 'test-build', 19)], records
            assert 'controlled compilation failure' in result.stderr
            assert manifest(repo) == before and not (repo / 'target').exists()
            continue
        # One build and one ordinary Rust run precede every live probe, even under -j.
        # Failures stop the remaining probe commands, without an alternate/local fallback.
        wanted = ['test-build', 'test', 'sandbox']
        if failure != 'sandbox':
            wanted.append('timeout')
        assert events.read_text().splitlines() == wanted, (label, events.read_text())
        assert marker.exists() == (failure not in ('sandbox', 'timeout')), label
        if marker.exists():
            assert marker.read_bytes() == b'executed', label  # No retry or duplicate proof.
        if failure == 'sandbox':
            assert 'controlled missing container runtime' in result.stderr
        if failure == 'timeout':
            assert 'controlled timeout cleanup failure' in result.stderr
        records = report_lines(report)
        assert [(r['record'], r.get('label')) for r in records] == [
            ('step', 'test-build'), ('step', 'test'), ('tests', None)], (label, records)
        assert all(r['exit_code'] == 0 for r in records if r['record'] == 'step')
        tests = records[-1]
        # Single-run mode reports its own JUnit/status, not an isolation-rerun tests record.
        # The folded ordinary selection remains clean even when the later proof fails.
        assert tests['failed_count'] == 0, (label, tests)
        if failure == 'native':
            assert 'isolation rerun:' not in result.stdout
        ordinary = ET.parse(run_dir / 'store/ci/junit.xml').findall('.//testcase')
        assert len(ordinary) == 4 + MANY, (label, ordinary)
        assert not any(c.find('failure') is not None for c in ordinary)
        native_junit = run_dir / 'repeat-1/store/ci/junit.xml'
        assert native_junit.exists() == marker.exists(), label
        if native_junit.exists():
            cases = ET.parse(native_junit).findall('.//testcase')
            assert [(c.get('classname'), c.get('name')) for c in cases] == [
                ('af-runtime::it', PROBE_NAME)], (label, cases)
            assert (cases[0].find('failure') is not None) == (failure == 'native')
            assert cases[0].find('skipped') is None
        assert manifest(repo) == before and not (repo / 'target').exists()
        assert not list(target.rglob('store.toml'))
    print('PASS: actual full wrapper/Make graph executes ordinary Rust once then all probes; '
          'parallel build ordering, missing-runtime/cleanup/native failure propagation, '
          'single execution, external reports and sealed source preserved')


def main():
    check_rerun_bounds()
    with tempfile.TemporaryDirectory(prefix='af gate space " ') as tmp:
        base = Path(tmp)
        repo = base / 'source'
        (repo / 'src').mkdir(parents=True)
        (repo / 'scripts').mkdir()
        (repo / '.config').mkdir()
        (repo / 'tests').mkdir()
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
        # Retain both targets and the runner conditionals, independent of release comments.
        recipe = MAKE_FIXTURE.test_recipe(makefile)
        probe_recipe = makefile.split('review-kernel-container-probes:\n', 1)[1].split(
            '\n\n', 1)[0].splitlines()[-1]
        (repo / 'Full.mk').write_text(makefile)
        (repo / 'Makefile').write_text(NL.join([
            'ifeq ($(FULL_ROUTE),1)', 'include Full.mk',
            'CI_STEP = python3 scripts/fixture-step.py',
            # These unrelated stand-in suites produce no base evidence. Keep their unused
            # shell arguments simple; the real nextest reports still exercise the quoted path.
            'BASE_EVIDENCE = unused-fixture-evidence', 'else',
            'TEST_RUNNER = nextest', 'TEST_THREADS = 4',
            'CI_STEP = python3 scripts/ci-step.py',
            'check: $(if $(PROBE_ONLY),native-probe-store,test)', recipe,
            # Only the native recipe applies to this miniature crate; keep its exact selector.
            'native-probe-store:', probe_recipe, 'endif', '']))
        # Keep the production full graph and its build/test receipt commands. Unrelated
        # preflight/lint/Python/feature suites are no-ops, avoiding a nested real gate.
        (repo / 'scripts/fixture-step.py').write_text(NL.join([
            'import os, pathlib, subprocess, sys',
            'if sys.argv[3] not in ("test-build", "test"): sys.exit(0)',
            'with pathlib.Path(os.environ["AF_GATE_ROUTE_EVENTS"]).open("a") as f:',
            '    f.write(sys.argv[3] + "\\n")',
            'sys.exit(subprocess.call([sys.executable, "scripts/ci-step.py", *sys.argv[1:]]))',
            '',
        ]))
        (repo / 'Cargo.toml').write_text(NL.join([
            '[package]', 'name="af-runtime"', 'version="0.1.0"',
            'edition="2021"', '']))
        (repo / 'src/lib.rs').write_text(LIB)
        (repo / 'tests/it.rs').write_text(PROBE)
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
        tools = base / 'tools'
        tools.mkdir()
        real_cargo = shutil.which('cargo')
        assert real_cargo
        # The sandbox bodies are stand-ins only in this routing fixture. Assert the exact
        # retained commands; all other Cargo calls execute the actual tiny Rust project.
        sandbox_args = ['test', '--locked', '-p', 'af-sandbox', '--test',
                        'container_probes', '--', '--ignored']
        timeout_args = ['test', '--locked', '-p', 'af-sandbox',
                        'container::tests::a_timed_out_container_is_removed_before_execution_returns',
                        '--', '--ignored', '--exact']
        cargo = tools / 'cargo'
        cargo.write_text(NL.join([
            f'#!{sys.executable}', 'import os, pathlib, sys',
            'args = sys.argv[1:]',
            'if args == ["test", "--locked", "--no-run"] and '
            'os.environ.get("AF_GATE_ROUTE_FAIL") == "build":',
            '    print("controlled compilation failure", file=sys.stderr)',
            '    sys.exit(19)',
            'if "af-sandbox" in args:',
            f'    assert args in ({sandbox_args!r}, {timeout_args!r}), args',
            f'    kind = "sandbox" if args == {sandbox_args!r} else "timeout"',
            '    with pathlib.Path(os.environ["AF_GATE_ROUTE_EVENTS"]).open("a") as f:',
            '        f.write(kind + "\\n")',
            '    if os.environ.get("AF_GATE_ROUTE_FAIL") == kind:',
            '        print("controlled missing container runtime" if kind == "sandbox" else',
            '              "controlled timeout cleanup failure", file=sys.stderr)',
            '        sys.exit(23)',
            '    sys.exit(0)',
            f'os.execv({real_cargo!r}, [{real_cargo!r}, *args])', '',
        ]))
        cargo.chmod(0o755)
        env['PATH'] = str(tools) + os.pathsep + env['PATH']
        env['TEST_THREADS'] = '4'
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
                full = lambda names: [f'af-runtime {name}' for name in names]
                assert ((f'{NL}flaky: ' + ', '.join(full(flaky)) + NL) in result.stdout) == \
                    bool(flaky), (label, result.stdout)
                if rerun == 'isolated' and failed:
                    assert f'{NL}failed twice: {", ".join(full(failed))}{NL}' in result.stdout, (
                        label, result.stdout)
                lines = report_lines(report)
                assert [(line['record'], line.get('label'), line.get('stage'))
                        for line in lines] == [('step', 'test-build', 'build'),
                                               ('step', 'test', 'tests'),
                                               ('tests', None, None)], (label, lines)
                assert all(line['schema'] == 'af.check-report/1' for line in lines), lines
                tests = lines[2]
                assert tests['test_threads'] == 4, tests
                assert tests['rerun'] == rerun, (label, tests)
                assert tests['failed_count'] == failed_count, (label, tests)
                assert sorted(tests['failed']) == sorted(full(failed)), (label, tests)
                assert tests['flaky'] == [{'name': name, 'parallel': 'failed',
                                           'isolated': 'passed'} for name in full(flaky)], (
                    label, tests)
                assert lines[1]['exit_code'] == (0 if passes else 100), (label, lines)
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
            # Reproduce the native gate's read-only source condition with the production
            # probe recipe. verify.sh/gate-owner.py supply the report store and supervision.
            marker = base / 'native-probe-executed'
            probe_report = base / 'native-probe-report.jsonl'
            selected = dict(env, PROBE_ONLY='1', AF_GATE_PROBE_MARKER=str(marker),
                            AF_CHECK_REPORT=str(probe_report))
            result = run(['bash', 'scripts/verify.sh'], repo, selected)
            assert result.returncode == 0, result
            assert marker.read_bytes() == b'executed'
            live_runs = set(target.glob('nextest-reports/run-*')) - set(runs)
            assert len(live_runs) == 1, live_runs
            junit = next(iter(live_runs)) / 'repeat-1/store/ci/junit.xml'
            cases = ET.parse(junit).findall('.//testcase')
            assert [(c.get('classname'), c.get('name')) for c in cases] == [
                ('af-runtime::it', PROBE_NAME)], cases
            assert not any(c.find('failure') is not None or c.find('skipped') is not None
                           for c in cases)
            marker.unlink()
            result = run(['bash', 'scripts/verify.sh'], repo,
                         dict(selected, AF_GATE_PROBE_FAIL='1'))
            assert result.returncode != 0, result
            assert marker.read_bytes() == b'executed'  # Exactly one failed execution, no retry.
            assert 'isolation rerun:' not in result.stdout
            failure_runs = set(target.glob('nextest-reports/run-*')) - set(runs) - live_runs
            assert len(failure_runs) == 1, failure_runs
            failed_junit = next(iter(failure_runs)) / 'repeat-1/store/ci/junit.xml'
            cases = ET.parse(failed_junit).findall('.//testcase')
            assert len(cases) == 1 and cases[0].find('failure') is not None, cases
            assert manifest(repo) == before and not (repo / 'target').exists()
            assert not list(target.rglob('store.toml'))
            check_full_route(repo, base, env, before)
            print('PASS: real verify entry, sealed source, external JUnit, failure propagation, '
                  'config cleanup, test-time summary, isolation rerun of failed tests alone '
                  '(flaky forgiven, failed twice fails, over 25 none rerun), check report lines')
        finally:
            for p in paths:
                p.chmod(0o755 if p.is_dir() else 0o644)
        # The same recipe also works in a writable checkout without gate report variables.
        marker.unlink()
        result = run(['make', 'native-probe-store'], repo,
                     dict(env, AF_GATE_PROBE_MARKER=str(marker)))
        assert result.returncode == 0 and marker.read_bytes() == b'executed', result
        junit = repo / 'target/nextest/ci/junit.xml'
        cases = ET.parse(junit).findall('.//testcase')
        assert [(c.get('classname'), c.get('name')) for c in cases] == [
            ('af-runtime::it', PROBE_NAME)], cases
        # A removed/renamed exact test must fail rather than claim empty coverage.
        marker.unlink()
        command = ['make', '-n', 'native-probe-store']
        recipe = subprocess.check_output(command, cwd=repo, env=env, text=True)
        command = shlex.split(recipe.replace(PROBE_NAME, 'retired_native_probe'))
        result = run(command, repo, dict(env, AF_GATE_PROBE_MARKER=str(marker)))
        assert result.returncode == 4 and not marker.exists(), result  # nextest NO_TESTS_RUN
        print('PASS: exact native probe recipe executes on sealed and writable sources; '
              'gate-owned JUnit records execution; failed proof runs once; obsolete selection fails')


if __name__ == '__main__':
    main()
