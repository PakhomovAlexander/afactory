#!/usr/bin/env python3
"""Exercise the real release resolver with local repositories; no GitHub side effects."""
import os
import copy
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('resolve-release.sh').resolve()
GIT = shutil.which('git')


class ReleaseResolution(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.api = self.root / 'api.json'
        self.api_log = self.root / 'api.log'
        gh = self.bin / 'gh'
        gh.write_text('#!' + sys.executable + '\n' + '''import json, os, sys
from pathlib import Path
with Path(os.environ['FAKE_API_LOG']).open('a') as f:
    f.write(json.dumps(sys.argv[1:]) + '\\n')
pages = json.loads(Path(os.environ['FAKE_API']).read_text())
if pages == 'error':
    sys.exit('fixture API failure')
page = int(sys.argv[-1].split('page=')[-1])
print(json.dumps(pages[page - 1] if page <= len(pages) else []))
''')
        gh.chmod(0o755)
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

    def pr(self, labels=None, **overrides):
        return dict({'base': {'ref': 'main'}, 'head': {'ref': 'release/v1.0.1'},
                     'labels': [{'name': label} for label in (labels or [])],
                     'merge_commit_sha': self.git('rev-parse', 'HEAD'),
                     'merged_at': '2026-10-25T11:36:48Z', 'number': 335}, **overrides)

    def resolve(self, ref_type='branch', ref_name='main', shim=None, labels=None, pages=None):
        output = self.root / 'output'
        output.write_text('')
        self.api.write_text(json.dumps(pages if pages is not None else
                                       [[self.pr(labels)]] if labels is not None else [[]]))
        # Never inherit hosted runner identity, credentials or output paths. Always supply a
        # fake gh, even for paths expected to avoid the API: an unexpected call stays offline.
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('GITHUB_', 'GH_', 'AF_'))}
        env.update(GITHUB_SHA=self.git('rev-parse', 'HEAD'), GH_REPO='fixture/repo',
                   GITHUB_REF_TYPE=ref_type, GITHUB_REF_NAME=ref_name,
                   GITHUB_OUTPUT=str(output), FAKE_API=str(self.api),
                   FAKE_API_LOG=str(self.api_log), PATH=str(self.bin) + os.pathsep + env['PATH'])
        if shim:
            wrapper = self.bin / 'git'
            wrapper.write_text('#!/bin/bash\n' + shim + '\nexec "$REAL_GIT" "$@"\n')
            wrapper.chmod(0o755)
            env.update(REAL_GIT=GIT, RACE_COMMIT=self.base)
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

    def test_squash_release_freezes_fast_then_tag_and_main_reruns_ignore_labels(self):
        result, values = self.resolve(labels=['release:fast'])
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(values['test_plan'])
        self.assertEqual(plan['selected'], ['functional', 'e2e', 'uix'])
        self.assertEqual(plan['sha'], self.release)
        for ref_type in ('branch', 'tag'):
            result, values = self.resolve(ref_type, 'v1.0.1' if ref_type == 'tag' else 'main',
                                          labels=['release:full'])
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(values['test_plan']), plan)

    def test_unlabeled_merged_release_pr_keeps_fast_selection_after_tagging(self):
        result, values = self.resolve(labels=[])
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(values['test_plan'])
        self.assertEqual(plan['context'], 'release_pr')
        self.assertEqual(plan['profile'], 'fast')
        calls = self.api_log.read_text()
        for ref_type in ('branch', 'tag'):
            result, retry = self.resolve(ref_type, 'v1.0.1' if ref_type == 'tag' else 'main',
                                         labels=['release:full'])
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(retry['test_plan']), plan)
        self.assertEqual(self.api_log.read_text(), calls)

    def test_full_merged_release_pr_keeps_full_selection_after_tagging(self):
        result, values = self.resolve(labels=['release:full'])
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(values['test_plan'])
        self.assertEqual(plan['context'], 'release_pr')
        self.assertEqual(plan['selected'], ['functional', 'e2e', 'uix', 'perf'])
        calls = self.api_log.read_text()
        for ref_type in ('branch', 'tag'):
            result, retry = self.resolve(ref_type, 'v1.0.1' if ref_type == 'tag' else 'main',
                                         labels=[])
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(retry['test_plan']), plan)
        self.assertEqual(self.api_log.read_text(), calls)

    def test_prior_binding_version_on_matching_tag_fails_closed_without_api(self):
        result, _ = self.resolve(labels=[])
        self.assertEqual(result.returncode, 0, result.stderr)
        tag = 'refs/tags/v1.0.1'
        binding = json.loads(self.git('for-each-ref', '--format=%(contents)', tag))
        binding['schema'] = 'af.release-tests/1'
        binding['plan']['schema'] = 'af.test-plan/1'
        self.git('push', 'origin', ':' + tag)
        self.git('tag', '-d', 'v1.0.1')
        self.git('tag', '-a', 'v1.0.1', '-m', json.dumps(binding))
        self.git('push', 'origin', tag)
        calls = self.api_log.read_text()
        result, _ = self.resolve('tag', 'v1.0.1', pages='error')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('invalid release selection annotation', result.stderr)
        self.assertEqual(self.api_log.read_text(), calls)

    def test_merge_commit_binds_the_merged_release_pr(self):
        self.git('branch', 'release/v1.0.1')
        self.git('checkout', '--detach', self.base)
        self.git('merge', '--no-ff', '-qm', 'merge release', 'release/v1.0.1')
        result, values = self.resolve(labels=['test:functional', 'test:e2e'])
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(values['test_plan'])
        self.assertEqual(plan['selected'], ['functional', 'e2e'])
        self.assertEqual(plan['sha'], self.git('rev-parse', 'HEAD'))

    def test_fast_forward_release_uses_exact_commit_pr_labels(self):
        self.git('branch', 'release/v1.0.1')
        self.git('checkout', '--detach', self.base)
        self.git('merge', '--ff-only', 'release/v1.0.1')
        self.assertEqual(self.git('rev-parse', 'HEAD'), self.release)
        result, values = self.resolve(labels=['release:fast'])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(values['test_plan'])['profile'], 'fast')

    def test_manual_tag_is_full_despite_stale_metadata_and_associated_labels(self):
        policy = self.repo / '.config/release-tests.json'
        policy.parent.mkdir(exist_ok=True)
        policy.write_text('{"schema":"af.release-test-policy/1","version":"1.0.0","labels":["release:fast"]}')
        self.git('add', '.config')
        self.git('commit', '--amend', '--no-edit', '-q')
        self.git('tag', '-a', 'v1.0.1', '-m', 'manual release')
        self.git('push', 'origin', 'refs/tags/v1.0.1')
        result, values = self.resolve('tag', 'v1.0.1', labels=['release:fast'])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(values['test_plan'])['selected'],
                         ['functional', 'e2e', 'uix', 'perf'])
        self.assertFalse(self.api_log.exists())
        self.git('tag', '-d', 'v1.0.1')
        stale = {'schema': 'af.release-tests/2', 'sha': self.base, 'version': '1.0.0',
                 'source_pr': None, 'plan': {}}
        self.git('tag', '-a', 'v1.0.1', '-m', json.dumps(stale))
        self.git('remote', 'set-url', 'origin', str(self.root / 'empty.git'))
        self.git('init', '--bare', str(self.root / 'empty.git'))
        result, _ = self.resolve('tag', 'v1.0.1')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.api_log.exists())

    def test_stale_prior_release_metadata_is_ignored(self):
        policy = self.repo / '.config/release-tests.json'
        policy.parent.mkdir(exist_ok=True)
        policy.write_text('{"version":"1.0.0","labels":["release:fast"]}')
        self.git('add', '.config')
        self.git('commit', '--amend', '--no-edit', '-q')
        result, values = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(values['test_plan'])['profile'], 'full')

    def test_unrelated_prs_never_supply_fast_authority(self):
        changes = [{'head': {'ref': 'other'}}, {'head': {'ref': 'release/v1.0.2'}},
                   {'base': {'ref': 'other'}}, {'merged_at': None},
                   {'merge_commit_sha': self.base}]
        result, values = self.resolve(pages=[[self.pr(['release:fast'], **change) for change in changes]])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(values['test_plan'])['profile'], 'full')

    def test_ambiguous_exact_prs_fail_before_creating_tag(self):
        result, _ = self.resolve(pages=[[self.pr(['release:fast']), self.pr(['release:full'], number=336)]])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('multiple exact merged release PRs', result.stderr)
        self.assertEqual(self.git('tag', '--list'), '')
        self.assertEqual(self.git('ls-remote', '--tags', 'origin'), '')

    def test_pagination_reaches_exact_pr_and_freezes_provenance_without_source_edits(self):
        before = self.git('rev-parse', 'HEAD')
        unrelated = self.pr(['release:full'], number=1, head={'ref': 'other'})
        result, values = self.resolve(pages=[[unrelated] * 100, [self.pr(['release:fast'])]])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(values['test_plan'])['profile'], 'fast')
        calls = [json.loads(row) for row in self.api_log.read_text().splitlines()]
        self.assertEqual(len(calls), 2)
        self.assertTrue(calls[0][-1].endswith('page=1'))
        self.assertTrue(calls[1][-1].endswith('page=2'))
        binding = json.loads(self.git('for-each-ref', '--format=%(contents)', 'refs/tags/v1.0.1'))
        self.assertEqual(binding['source_pr'], {'repository': 'fixture/repo', 'number': 335,
                         'merge_commit_sha': before, 'base': 'main', 'head': 'release/v1.0.1',
                         'merged_at': '2026-10-25T11:36:48Z'})
        self.assertEqual(self.git('rev-parse', 'HEAD'), before)
        self.assertEqual(self.git('diff', '--exit-code'), '')
        self.assertFalse((self.repo / '.config/release-tests.json').exists())
        # API failure after the annotation was created cannot change or block a retry.
        result, retry = self.resolve(pages='error')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(retry['test_plan'], values['test_plan'])
        self.assertEqual(len(self.api_log.read_text().splitlines()), 2)

    def test_ambiguity_on_a_later_api_page_also_refuses(self):
        first = [self.pr(['release:fast'])] + [self.pr(head={'ref': 'other'})] * 99
        result, _ = self.resolve(pages=[first, [self.pr(['release:full'], number=336)]])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('multiple exact merged release PRs', result.stderr)
        self.assertEqual(self.git('tag', '--list'), '')

    def test_api_error_invalid_shape_or_conflicting_labels_fail_closed(self):
        for pages in ('error', [{'unexpected': 'shape'}], [[self.pr(['test:perf', 'no-perf'])]]):
            result, _ = self.resolve(pages=pages)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.git('tag', '--list'), '')
            self.assertEqual(self.git('ls-remote', '--tags', 'origin'), '')

    def test_frozen_binding_refuses_sha_version_catalog_and_tag_target_mismatches(self):
        result, _ = self.resolve(labels=['release:fast'])
        self.assertEqual(result.returncode, 0, result.stderr)
        original = json.loads(self.git('for-each-ref', '--format=%(contents)', 'refs/tags/v1.0.1'))
        # Only disposable local repositories: keep the resolver's remote lookup empty so the
        # production annotation reader, rather than tag-object race detection, judges each case.
        empty = self.root / 'empty.git'
        self.git('init', '--bare', str(empty))
        self.git('remote', 'set-url', 'origin', str(empty))
        for field, value, diagnostic in (
                ('sha', self.base, 'SHA/version mismatch'),
                ('version', '9.9.9', 'SHA/version mismatch'),
                ('catalog_sha256', '0' * 64, 'resolver/catalog'),
                ('tag_target', self.base, 'triggering commit')):
            with self.subTest(field=field):
                binding = copy.deepcopy(original)
                target = self.release
                if field == 'catalog_sha256':
                    binding['plan'][field] = value
                elif field == 'tag_target':
                    target = value
                else:
                    binding[field] = value
                self.git('tag', '-d', 'v1.0.1')
                self.git('tag', '-a', 'v1.0.1', '-m', json.dumps(binding), target)
                result, values = self.resolve('tag', 'v1.0.1', pages='error')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)
                self.assertNotIn('test_plan', values)
                self.assertEqual(self.git('--git-dir=' + str(empty), 'tag', '--list'), '')

    def test_lightweight_manual_tag_defaults_full(self):
        self.git('tag', 'v1.0.1')
        self.git('push', 'origin', 'refs/tags/v1.0.1')
        result, values = self.resolve('tag', 'v1.0.1', pages='error')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(values['test_plan'])['profile'], 'full')
        self.assertFalse(self.api_log.exists())


if __name__ == '__main__':
    unittest.main()
