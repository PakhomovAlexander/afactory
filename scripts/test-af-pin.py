#!/usr/bin/env python3
"""Exercise the af pin check and scripts/pin-release.sh with local repositories and a stand-in
`af`; no network, no GitHub side effects."""
from pathlib import Path
import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
GIT = shutil.which('git')
TARGETS = ['aarch64-apple-darwin', 'x86_64-unknown-linux-musl']


def lock(version, worker='1.0.0', targets=TARGETS):
    digests = ''.join(f'{target} = "sha256:{version_digest(version, target)}"\n' for target in targets)
    return (f'version = 1\n\n[af]\nversion = "{version}"\n\n[af.digests]\n{digests}\n'
            f'[workers.bugs]\nversion = "{worker}"\n')


def version_digest(version, target):
    return hashlib.sha256(f'{version}/{target}'.encode()).hexdigest()


# The stand-in `af`: `onboard --refresh-lock --af VERSION` rewrites the pin the way the real
# command does; FAKE_AF_WORKER also moves a Worker pin, which the script must refuse.
FAKE_AF = r'''#!/usr/bin/env python3
import os, sys
sys.path.insert(0, os.environ["FAKE_AF_TESTS"])
import fixture_af_pin as fixture
assert sys.argv[1:4] == ["onboard", "--refresh-lock", "--af"], sys.argv
open(".af/af.lock", "w").write(fixture.lock(sys.argv[4], os.environ.get("FAKE_AF_WORKER", "1.0.0")))
'''


class Repository(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / 'repo'
        (self.repo / 'scripts').mkdir(parents=True)
        (self.repo / '.af').mkdir()
        (self.repo / '.github/workflows').mkdir(parents=True)
        for script in ('af-pin.py', 'pin-release.sh'):
            shutil.copy(SCRIPTS / script, self.repo / 'scripts' / script)
        (self.repo / '.github/workflows/release.yml').write_text(
            ''.join(f'          - target: {target}\n' for target in TARGETS))
        self.changelog('0.2.0', '0.1.0')
        (self.repo / '.af/af.lock').write_text(lock('0.2.0'))
        # The stand-in imports lock() from here, so the fixture and the check share one shape.
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        shutil.copy(Path(__file__), bin_dir / 'fixture_af_pin.py')
        (bin_dir / 'af').write_text(FAKE_AF)
        (bin_dir / 'af').chmod(0o755)
        self.env = {**os.environ, 'AF': str(bin_dir / 'af'), 'FAKE_AF_TESTS': str(bin_dir), 'PYTHONDONTWRITEBYTECODE': '1',
                    'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
                    'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                    'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}
        self.env.pop('FAKE_AF_WORKER', None)
        self.git('init', '-q', '-b', 'main')
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')

    def changelog(self, *versions):
        sections = ''.join(f'## [{version}] - today\n\n- change\n\n' for version in versions)
        (self.repo / 'CHANGELOG.md').write_text('# Changelog\n\n## [Unreleased]\n\n' + sections)

    def git(self, *args, cwd=None):
        return subprocess.run([GIT, *args], cwd=cwd or self.repo, env=getattr(self, 'env', None),
                              check=True, capture_output=True, text=True).stdout.strip()

    def run_script(self, *args):
        return subprocess.run(list(args), cwd=self.repo, env=self.env, capture_output=True, text=True)

    def check(self):
        return self.run_script('python3', 'scripts/af-pin.py', '--check')

    def pin_release(self, *args):
        return self.run_script('bash', 'scripts/pin-release.sh', *args)

    def test_the_newest_release_passes(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('pins af 0.2.0', result.stdout)

    def test_the_previous_release_passes_while_the_newest_is_released(self):
        self.changelog('0.3.0', '0.2.0', '0.1.0')
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('pins 0.3.0 once it is published', result.stdout)

    def test_two_releases_behind_fails_and_names_the_fix(self):
        self.changelog('0.4.0', '0.3.0', '0.2.0')
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('it must pin the newest release, 0.4.0', result.stderr)
        self.assertIn('scripts/pin-release.sh', result.stderr)

    def test_a_target_without_a_digest_fails(self):
        (self.repo / '.af/af.lock').write_text(lock('0.2.0', targets=TARGETS[:1]))
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('no digest for x86_64-unknown-linux-musl', result.stderr)

    def test_pin_release_moves_only_the_pin(self):
        result = self.pin_release('0.3.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('version = "0.3.0"', (self.repo / '.af/af.lock').read_text())
        self.assertEqual(self.git('status', '--porcelain'), 'M .af/af.lock')

    def test_pin_release_refuses_a_refresh_that_moves_a_worker(self):
        self.env['FAKE_AF_WORKER'] = '2.0.0'
        result = self.pin_release('0.3.0')
        self.assertEqual(result.returncode, 1)
        self.assertIn('beyond its [af] tables', result.stderr)

    def test_pin_release_is_a_no_op_when_already_pinned(self):
        result = self.pin_release('0.2.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('already pins af 0.2.0', result.stdout)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_push_commits_the_pin_on_the_tip_of_main(self):
        remote = self.root / 'remote.git'
        self.git('init', '-q', '--bare', '-b', 'main', str(remote))
        self.git('remote', 'add', 'origin', str(remote))
        self.git('push', '-q', 'origin', 'main')
        # main moves after the release commit: the pin lands on its tip, not on the release.
        other = self.root / 'other'
        self.git('clone', '-q', str(remote), str(other), cwd=self.root)
        (other / 'later.txt').write_text('later\n')
        self.git('add', 'later.txt', cwd=other)
        self.git('commit', '-qm', 'later', cwd=other)
        self.git('push', '-q', 'origin', 'main', cwd=other)
        result = self.pin_release('0.3.0', '--push')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git('log', '-1', '--format=%s', 'origin/main'), 'Pin af 0.3.0 in .af/af.lock')
        self.assertEqual(self.git('show', 'origin/main^', '--format=%s', '-s'), 'later')
        self.assertEqual(self.git('diff', '--name-only', 'origin/main^', 'origin/main'), '.af/af.lock')
        again = self.pin_release('0.3.0', '--push')
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertIn('on main already pins af 0.3.0', again.stdout)

    def test_a_refused_push_is_reported_not_retried(self):
        remote = self.root / 'remote.git'
        self.git('init', '-q', '--bare', '-b', 'main', str(remote))
        self.git('remote', 'add', 'origin', str(remote))
        self.git('push', '-q', 'origin', 'main')
        hook = remote / 'hooks/pre-receive'
        hook.write_text('#!/bin/sh\necho "main requires a pull request" >&2\nexit 1\n')
        hook.chmod(0o755)
        result = self.pin_release('0.3.0', '--push')
        self.assertEqual(result.returncode, 1)
        self.assertIn('main refused the push', result.stderr)
        self.assertIn('main requires a pull request', result.stderr)
        self.assertNotIn('starting over', result.stderr)


if __name__ == '__main__':
    unittest.main()
