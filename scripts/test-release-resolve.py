#!/usr/bin/env python3
"""Exercise the real release resolver with local repositories; no GitHub side effects."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('resolve-release.sh').resolve()
GIT = shutil.which('git')


class ReleaseResolution(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.remote = self.root / 'remote.git'
        self.git('init', '--bare', str(self.remote))
        self.git('init')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('remote', 'add', 'origin', str(self.remote))
        self.commit('1.0.0')
        self.base = self.git('rev-parse', 'HEAD')
        self.commit('1.0.1')
        self.release = self.git('rev-parse', 'HEAD')

    def git(self, *args):
        return subprocess.check_output([GIT, *args], cwd=self.repo, stderr=subprocess.DEVNULL,
                                       text=True).strip()

    def commit(self, version):
        (self.repo / 'Cargo.toml').write_text('[workspace.package]\nversion = "' + version + '"\n')
        (self.repo / 'CHANGELOG.md').write_text('## [' + version + '] - today\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')

    def resolve(self, ref_type='branch', ref_name='main', shim=None):
        output = self.root / 'output'
        output.write_text('')
        env = dict(os.environ, GITHUB_SHA=self.git('rev-parse', 'HEAD'),
                   GITHUB_REF_TYPE=ref_type, GITHUB_REF_NAME=ref_name,
                   GITHUB_OUTPUT=str(output))
        if shim:
            bin_dir = self.root / 'bin'
            bin_dir.mkdir()
            wrapper = bin_dir / 'git'
            wrapper.write_text('#!/bin/bash\n' + shim + '\nexec "$REAL_GIT" "$@"\n')
            wrapper.chmod(0o755)
            env.update(PATH=str(bin_dir) + os.pathsep + env['PATH'], REAL_GIT=GIT,
                       RACE_COMMIT=self.base)
        result = subprocess.run(['bash', str(SCRIPT)], cwd=self.repo, env=env,
                                text=True, capture_output=True)
        values = dict(line.split('=', 1) for line in output.read_text().splitlines())
        return result, values

    def test_release_and_retry_select_the_exact_commit(self):
        for _ in range(2):
            result, values = self.resolve()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(values['tag'], 'v1.0.1')
        self.assertEqual(self.git('--git-dir=' + str(self.remote), 'rev-parse', 'v1.0.1^{}'),
                         self.release)

    def test_later_main_commit_cannot_win_even_when_it_runs_first(self):
        self.git('commit', '--allow-empty', '-qm', 'later PR')
        result, values = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], '')
        self.assertEqual(self.git('ls-remote', '--tags', 'origin'), '')
        self.git('checkout', '--detach', self.release)
        result, values = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], 'v1.0.1')

    def test_existing_tag_on_another_commit_preserves_main_validation(self):
        self.git('tag', 'v1.0.1', self.base)
        self.git('push', 'origin', 'refs/tags/v1.0.1')
        result, values = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], '')

    def test_push_race_never_overwrites_a_different_commit(self):
        result, values = self.resolve(shim='''if [[ "$1" == push ]]; then
  "$REAL_GIT" push origin "$RACE_COMMIT:refs/tags/v1.0.1" >/dev/null 2>&1
  exit 1
fi''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], '')
        self.assertEqual(self.git('--git-dir=' + str(self.remote), 'rev-parse', 'v1.0.1'), self.base)

    def test_failed_push_without_remote_tag_is_an_error(self):
        result, values = self.resolve(shim='if [[ "$1" == push ]]; then exit 1; fi')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(values, {})

    def test_competing_push_of_same_commit_can_continue(self):
        result, values = self.resolve(shim='''if [[ "$1" == push ]]; then
  "$REAL_GIT" "$@" >/dev/null 2>&1
  exit 1
fi''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], 'v1.0.1')

    def test_depth_two_checkout_has_required_parent(self):
        self.git('push', 'origin', 'HEAD:refs/heads/main')
        shallow = self.root / 'shallow'
        self.git('clone', '--depth=2', '--branch=main', self.remote.as_uri(), str(shallow))
        self.repo = shallow
        self.assertEqual(self.git('rev-parse', '--is-shallow-repository'), 'true')
        result, values = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], 'v1.0.1')

    def test_remote_read_failure_is_not_tag_absence(self):
        result, values = self.resolve(shim='if [[ "$1" == ls-remote ]]; then exit 128; fi')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(values, {})

    def test_missing_changelog_section_does_not_release(self):
        (self.repo / 'CHANGELOG.md').write_text('## [1.0.0]\n')
        self.git('add', '.')
        self.git('commit', '--amend', '--no-edit', '-q')
        result, values = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], '')

    def test_explicit_tag_must_match_the_workspace_version(self):
        result, values = self.resolve('tag', 'v1.0.1')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values['tag'], 'v1.0.1')
        result, values = self.resolve('tag', 'v9.9.9')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(values, {})


if __name__ == '__main__':
    unittest.main()
