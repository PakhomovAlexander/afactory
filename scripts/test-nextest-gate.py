#!/usr/bin/env python3
"""Exercise the real gate entry on a tiny sealed-source Rust project, offline."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent.parent
NL = chr(10)


def run(command, repo, env):
    result = subprocess.run(command, cwd=repo, env=env, capture_output=True, text=True)
    print(result.stdout, end='')
    print(result.stderr, end='')
    return result


def manifest(repo):
    return {str(p.relative_to(repo)): (hashlib.sha256(p.read_bytes()).hexdigest(),
                                     p.stat().st_mode & 0o777)
            for p in repo.rglob('*') if p.is_file()}


def main():
    with tempfile.TemporaryDirectory(prefix='af gate space " ') as tmp:
        base = Path(tmp)
        repo = base / 'source'
        (repo / 'src').mkdir(parents=True)
        (repo / 'scripts').mkdir()
        (repo / '.config').mkdir()
        for name in ['verify.sh', 'nextest-gate.py', 'ci-step.py']:
            shutil.copyfile(ROOT / 'scripts' / name, repo / 'scripts' / name)
        shutil.copyfile(ROOT / '.config/nextest.toml', repo / '.config/nextest.toml')
        shutil.copyfile(ROOT / 'rust-toolchain.toml', repo / 'rust-toolchain.toml')
        # Use the actual Makefile test recipe; other gate stages do not apply to
        # this miniature crate. The production check target is not modified.
        makefile = (ROOT / 'Makefile').read_text()
        recipe = makefile[makefile.index('test:'):makefile.index('# Open the release PR')]
        (repo / 'Makefile').write_text(NL.join([
            'TEST_RUNNER = nextest', 'TEST_THREADS = 4',
            'CI_STEP = python3 scripts/ci-step.py', 'check: test', recipe]))
        (repo / 'Cargo.toml').write_text(NL.join([
            '[package]', 'name="gate-store-regression"', 'version="0.1.0"',
            'edition="2021"', '']))
        (repo / 'src/lib.rs').write_text(NL.join([
            '#[test] fn always_passes() { assert_eq!(2+2,4); }',
            '#[test] fn controlled_failure() {',
            'assert!(std::env::var_os("AF_STORE_TEST_FAIL").is_none()); }', '']))
        env = {'PATH': os.environ['PATH'], 'LC_ALL': 'C', 'TZ': 'UTC',
               'HOME': str(base / 'home'), 'XDG_CACHE_HOME': str(base / 'cache')}
        # Keep HOME/cache isolated without hiding the installed pinned toolchain
        # from rustup proxies. No ambient compiler flags or wrappers are inherited.
        for name, default in [('RUSTUP_HOME', '.rustup'), ('CARGO_HOME', '.cargo')]:
            env[name] = os.environ.get(name, str(Path.home() / default))
        Path(env['HOME']).mkdir()
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
            for failing in [False, True]:
                selected = dict(env)
                if failing:
                    selected['AF_STORE_TEST_FAIL'] = '1'
                result = run(['bash', 'scripts/verify.sh'], repo, selected)
                assert (result.returncode != 0) == failing
                assert manifest(repo) == before
                assert not (repo / 'target').exists()
                assert not list(target.rglob('store.toml'))
            reports = list(target.glob('nextest-reports/run-*/store/ci/junit.xml'))
            assert len(reports) == 2, reports
            cases = [ET.parse(p).findall('.//testcase') for p in reports]
            assert all(len(c) == 2 for c in cases), cases
            assert sorted(sum(c.find('failure') is not None for c in cs) for cs in cases) == [0, 1]
            print('PASS: real verify entry, sealed source, 2 tests per run, external JUnit, failure propagation, config cleanup')
        finally:
            for p in paths:
                p.chmod(0o755 if p.is_dir() else 0o644)


if __name__ == '__main__':
    main()
