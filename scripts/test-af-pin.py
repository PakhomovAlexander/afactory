#!/usr/bin/env python3
"""Exercise the af pin check and scripts/pin-release.sh with local repositories and a stand-in
`af`; no network, no GitHub side effects."""
from pathlib import Path
import hashlib
import json
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
if os.environ.get("FAKE_AF_MOVE_MAIN"):
    import subprocess
    subprocess.run(["bash", os.environ["FAKE_AF_MOVE_MAIN"]], check=True)
open(".af/af.lock", "w").write(fixture.lock(sys.argv[4], os.environ.get("FAKE_AF_WORKER", "1.0.0")))
if os.environ.get("FAKE_AF_CATALOG"):
    open(".af/task-catalog.toml", "w").write("# unexpected catalog refresh\n")
'''


FAKE_GH = r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
state = pathlib.Path(os.environ['FAKE_PR_STATE'])
args = sys.argv[1:]
assert os.environ['GH_HOST'] == 'github.com'
assert os.environ['GH_TOKEN'] == os.environ['RELEASE_PIN_TOKEN']
if args[:2] == ['pr', 'list']:
    print(state.read_text() if state.exists() else '[]')
elif args[:2] == ['pr', 'create']:
    if os.environ.get('FAKE_PR_REFUSE'):
        sys.exit('PR permission denied')
    count = pathlib.Path(str(state) + '.creates')
    count.write_text(str(int(count.read_text()) + 1) if count.exists() else '1')
    head = args[args.index('--head') + 1]
    oid = subprocess.check_output(['git', '--git-dir=' + os.environ['FAKE_GIT_REMOTE'],
                                   'rev-parse', 'refs/heads/' + head], text=True).strip()
    state.write_text(json.dumps([dict(url='https://example.invalid/pull/1', state='OPEN', headRefOid=oid, baseRefName='main')]))
    if os.environ.get('FAKE_PR_RACE'):
        sys.exit('another creator opened the PR')
    print('https://example.invalid/pull/1')
else:
    sys.exit('unexpected gh invocation: ' + repr(args))
'''


# Keep the production HTTPS admission and credential selection real; substitute only the
# transport with the disposable bare repository after checking its URL and PAT helper.
FAKE_GIT = r'''#!/usr/bin/env python3
import os, subprocess, sys
args = sys.argv[1:]
url = 'https://github.com/fixture/repo.git'
if url in args:
    assert os.environ['GIT_TERMINAL_PROMPT'] == '0'
    assert os.environ['GIT_ASKPASS'] == 'false'
    for setting in ('protocol.allow=never', 'protocol.https.allow=always',
                    'http.followRedirects=false', 'http.extraheader=', 'credential.helper='):
        assert setting in args, args
    command = next(i for i, arg in enumerate(args) if arg in ('fetch', 'push', 'ls-remote'))
    credentials = subprocess.run([os.environ['REAL_GIT'], *args[:command], 'credential', 'fill'],
        input='protocol=https\nhost=github.com\npath=fixture/repo.git\n\n',
        text=True, capture_output=True, check=True).stdout
    assert 'username=x-access-token\n' in credentials
    assert 'password=' + os.environ['RELEASE_PIN_TOKEN'] + '\n' in credentials
    with open(os.environ['FAKE_GIT_CALLS'], 'a') as log:
        log.write(args[command] + '\n')
    args[args.index(url)] = os.environ['FAKE_GIT_REMOTE']
    args = ['-c', 'protocol.file.allow=always', *args]
elif any(arg in ('fetch', 'push', 'ls-remote') for arg in args):
    # Concurrent-main fixture scripts may use their clone's local origin. A regressed
    # production script must fail this test without ever reaching SSH or live HTTPS.
    target = subprocess.check_output([os.environ['REAL_GIT'], 'remote', 'get-url', 'origin'],
                                     text=True).strip()
    if target != os.environ.get('FAKE_GIT_REMOTE'):
        sys.exit('unexpected Git network transport in pin fixture')
sys.exit(subprocess.run([os.environ['REAL_GIT'], *args]).returncode)
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
        for key in ('FAKE_AF_WORKER', 'FAKE_AF_CATALOG', 'FAKE_AF_MOVE_MAIN', 'FAKE_PR_REFUSE', 'FAKE_PR_RACE',
                    'RELEASE_PIN_TOKEN', 'GH_TOKEN'):
            self.env.pop(key, None)
        (bin_dir / 'git').write_text(FAKE_GIT)
        (bin_dir / 'git').chmod(0o755)
        self.env['REAL_GIT'] = GIT
        self.env['FAKE_GIT_CALLS'] = str(self.root / 'git-calls')
        (bin_dir / 'gh').write_text(FAKE_GH)
        (bin_dir / 'gh').chmod(0o755)
        self.env['PATH'] = str(bin_dir) + os.pathsep + os.environ['PATH']
        self.env['FAKE_PR_STATE'] = str(self.root / 'prs.json')
        self.env['GH_REPO'] = 'fixture/repo'

        self.git('init', '-q', '-b', 'main')
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')

    def changelog(self, *versions):
        # A version given as 'X [YANKED]' is a release that was tagged but never published.
        sections = ''.join(f'## [{version.split()[0]}] - today{version[len(version.split()[0]):]}\n\n- change\n\n'
                           for version in versions)
        (self.repo / 'CHANGELOG.md').write_text('# Changelog\n\n## [Unreleased]\n\n' + sections)

    def git(self, *args, cwd=None):
        if hasattr(self, 'remote_path') and args[0] in ('push', 'fetch', 'ls-remote'):
            args = tuple(str(self.remote_path) if arg == 'origin' else arg for arg in args)
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
        self.assertIn('0.3.0 awaits its pin PR merge after publication', result.stdout)

    def test_two_releases_behind_fails_and_names_the_fix(self):
        self.changelog('0.4.0', '0.3.0', '0.2.0')
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('it must pin the newest release, 0.4.0', result.stderr)
        self.assertIn('scripts/pin-release.sh', result.stderr)

    def test_a_yanked_release_is_skipped(self):
        self.changelog('0.4.0', '0.3.0 [YANKED]', '0.2.0', '0.1.0')
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('0.4.0 awaits its pin PR merge after publication', result.stdout)

    def test_a_yanked_release_does_not_widen_the_lag(self):
        self.changelog('0.5.0', '0.4.0 [YANKED]', '0.3.0', '0.2.0')
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('it must pin the newest release, 0.5.0 (or 0.3.0 while 0.5.0 awaits its pin PR merge)', result.stderr)

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

    def test_catalog_changes_are_retained_and_refused_before_publication(self):
        catalog = self.repo / '.af/task-catalog.toml'
        catalog.write_text('# project catalog\n')
        self.git('add', '.af/task-catalog.toml')
        self.git('commit', '-qm', 'project catalog')
        self.remote()
        self.env['FAKE_AF_CATALOG'] = '1'
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('refresh changed more than .af/af.lock', result.stderr)
        self.assertEqual(catalog.read_text(), '# unexpected catalog refresh\n')
        self.assertEqual(self.git('ls-remote', '--heads', 'origin', self.branch), '')
        self.assertFalse(Path(self.env['FAKE_PR_STATE']).exists())

    def test_existing_branch_catalog_change_is_retained_and_refused(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        head = self.remote_head(self.branch)
        self.env['FAKE_AF_CATALOG'] = '1'
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('does not match the signed release refresh', result.stderr)
        self.assertEqual((self.repo / '.af/task-catalog.toml').read_text(),
                         '# unexpected catalog refresh\n')
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')


    def remote(self):
        remote = self.root / 'remote.git'
        self.git('init', '-q', '--bare', '-b', 'main', str(remote))
        self.git('remote', 'add', 'origin', str(remote))
        self.git('push', '-q', 'origin', 'main')
        self.env['RELEASE_PIN_TOKEN'] = 'github_pat_fixture'
        self.base = self.git('rev-parse', 'HEAD')
        self.branch = 'refs/heads/release/pin-v0.3.0'
        self.remote_path = remote
        self.env['FAKE_GIT_REMOTE'] = str(remote)
        self.git('remote', 'set-url', 'origin', 'https://github.com/fixture/repo.git')
        # A real ruleset stand-in: a pin must never attempt to update main.
        hook = remote / 'hooks/pre-receive'
        hook.write_text('#!/bin/sh\nwhile read old new ref; do\n'
                        '  if [ "$ref" = refs/heads/main ]; then echo "main requires PR" >&2; exit 1; fi\ndone\n')
        hook.chmod(0o755)
        return remote

    def remote_head(self, ref):
        return self.git('--git-dir=' + str(self.remote_path), 'rev-parse', ref)

    def test_pr_is_pin_only_on_current_main_and_reuses_one_pr(self):
        remote = self.remote()
        # A later main tip, before the job begins.
        self.mover()
        moved = self.run_script('bash', self.env.pop('FAKE_AF_MOVE_MAIN'))
        self.assertEqual(moved.returncode, 0, moved.stderr)
        self.base = self.remote_head('main')
        self.git('--git-dir=' + str(remote), 'config', 'receive.denyNonFastForwards', 'true')
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        head = self.remote_head(self.branch)
        self.assertEqual(self.git('rev-parse', head + '^'), self.base)
        self.assertEqual(self.git('diff', '--name-only', self.base, head), '.af/af.lock')
        self.assertEqual(self.remote_head('main'), self.base)
        self.assertIn('awaiting CI and human merge', result.stdout)
        again = self.pin_release('0.3.0', '--pr')
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')

    def test_pr_pushes_only_pin_branch_when_follow_tags_is_enabled(self):
        self.remote()
        before = set(self.git('ls-remote', '--refs', 'origin').splitlines())
        self.git('config', 'push.followTags', 'true')
        tag = 'v0.3.0'
        self.git('tag', '-a', tag, '-m', 'Unpublished release tag', 'main')
        self.assertEqual(self.git('cat-file', '-t', 'refs/tags/' + tag), 'tag')
        self.assertEqual(self.git('ls-remote', '--tags', 'origin'), '')

        result = self.pin_release('0.3.0', '--pr')

        self.assertEqual(result.returncode, 0, result.stderr)
        head = self.remote_head(self.branch)
        after = set(self.git('ls-remote', '--refs', 'origin').splitlines())
        self.assertEqual(after, before | {head + '\t' + self.branch})
        self.assertEqual(self.git('ls-remote', '--tags', 'origin'), '')
        self.assertEqual(self.remote_head('main'), self.base)
        self.assertEqual(self.git('rev-parse', head + '^'), self.base)
        self.assertEqual(self.git('diff', '--name-only', self.base, head), '.af/af.lock')
        rows = json.loads(Path(self.env['FAKE_PR_STATE']).read_text())
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['headRefOid'], head)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')
        self.assertIn('awaiting CI and human merge', result.stdout)

    def test_credentials_are_required_without_fallback(self):
        self.remote()
        for value in ('', 'ghs_workflow_token'):
            self.env['RELEASE_PIN_TOKEN'] = value
            self.env['GH_TOKEN'] = 'ghs_ambient'
            result = self.pin_release('0.3.0', '--pr')
            self.assertEqual(result.returncode, 1)
            self.assertIn('RELEASE_PIN_TOKEN', result.stderr)
            self.assertEqual(self.remote_head('main'), self.base)
            self.assertEqual(self.git('ls-remote', '--heads', 'origin', self.branch), '')

    def test_pr_refuses_ssh_embedded_credentials_and_mismatched_origins_before_publication(self):
        self.remote()
        for origin in ('git@github.com:fixture/repo.git',
                       'ssh://git@github.com/fixture/repo.git',
                       'https://user:secret@github.com/fixture/repo.git',
                       'https://github.com/other/repo.git'):
            with self.subTest(origin=origin):
                self.git('remote', 'set-url', 'origin', origin)
                result = self.pin_release('0.3.0', '--pr')
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn('origin must be credential-free HTTPS', result.stderr)
                self.assertFalse(Path(self.env['FAKE_GIT_CALLS']).exists())
                self.assertEqual(self.git('ls-remote', '--heads', str(self.remote_path), self.branch), '')
                self.assertFalse(Path(self.env['FAKE_PR_STATE']).exists())

    def test_pr_refuses_url_rewrites_and_scoped_credentials_before_transport(self):
        self.remote()
        for key, value in (('url.git@github.com:.insteadOf', 'https://github.com/'),
                           ('url.git@github.com:.pushInsteadOf', 'https://github.com/'),
                           ('credential.https://github.com.helper', 'ambient'),
                           ('http.https://github.com/.extraheader', 'Authorization: ambient'),
                           ('http.https://github.com/.followRedirects', 'true')):
            with self.subTest(key=key):
                self.git('config', key, value)
                result = self.pin_release('0.3.0', '--pr')
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn('use a clean checkout for PAT-only publication', result.stderr)
                self.assertFalse(Path(self.env['FAKE_GIT_CALLS']).exists())
                self.assertFalse(Path(self.env['FAKE_PR_STATE']).exists())
                self.git('config', '--unset-all', key)
        self.assertEqual(self.git('ls-remote', '--heads', str(self.remote_path), self.branch), '')

    def test_pr_uses_only_pat_helper_for_explicit_https_transport(self):
        self.remote()
        marker = self.root / 'ambient-helper-called'
        self.git('config', 'credential.helper', '!touch ' + str(marker))
        self.git('config', 'http.extraheader', 'Authorization: ambient')
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(marker.exists())
        self.assertIn('push', Path(self.env['FAKE_GIT_CALLS']).read_text().splitlines())
        self.assertEqual(self.remote_head('main'), self.base)

    def test_pr_permission_refusal_retains_branch_for_retry_without_claiming_pinned(self):
        self.remote()
        self.env['FAKE_PR_REFUSE'] = '1'
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('cannot create CI-triggering PR', result.stderr)
        self.assertIn('repository is not pinned', result.stderr)
        head = self.remote_head(self.branch)
        self.env.pop('FAKE_PR_REFUSE')
        again = self.pin_release('0.3.0', '--pr')
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(self.remote_head(self.branch), head)

    def test_closed_pr_is_refused_and_not_recreated(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        path = Path(self.env['FAKE_PR_STATE'])
        rows = json.loads(path.read_text())
        rows[0]['state'] = 'CLOSED'
        path.write_text(json.dumps(rows))
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('closed, ambiguous or has another base/head', result.stderr)
        self.assertEqual(Path(str(path) + '.creates').read_text(), '1')

    def test_racing_pr_creation_reuses_the_single_pr(self):
        self.remote()
        self.env['FAKE_PR_RACE'] = '1'
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('awaiting CI and human merge', result.stdout)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')

    def mover(self, every=False):
        other = self.root / 'other'
        self.git('clone', '-q', str(self.remote_path), str(other), cwd=self.root)
        # Only this local fixture's concurrent operator can advance main.
        (self.remote_path / 'hooks/pre-receive').unlink()
        script = self.root / 'move-main.sh'
        script.write_text('#!/bin/bash\nset -eu\ncd "OTHER"\nif [ -f once ] && [ "EVERY" = no ]; then exit 0; fi\necho later >> later.txt\ntouch once\ngit add later.txt once\ngit commit -qm later\ngit push -q origin main\n'.replace('OTHER', str(other)).replace('EVERY', 'yes' if every else 'no'))
        self.env['FAKE_AF_MOVE_MAIN'] = str(script)

    def test_main_moved_during_refresh_rebuilds_on_new_tip(self):
        self.remote()
        self.mover()
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('main moved during attempt 1', result.stderr)
        self.assertEqual(self.git('rev-parse', self.remote_head(self.branch) + '^'),
                         self.remote_head('main'))
        self.assertEqual(self.git('diff', '--name-only', self.remote_head('main'),
                                  self.remote_head(self.branch)), '.af/af.lock')

    def test_main_keeps_moving_is_bounded_without_a_branch_or_pr(self):
        self.remote()
        self.mover(every=True)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr.count('starting over'), 3)
        self.assertIn('main kept moving after three attempts', result.stderr)
        self.assertEqual(self.git('ls-remote', '--heads', 'origin', self.branch), '')
        self.assertFalse(Path(self.env['FAKE_PR_STATE']).exists())

    def test_main_moved_during_branch_push_retains_branch_without_opening_pr(self):
        remote = self.remote()
        advanced = self.git('--git-dir=' + str(remote), 'commit-tree', self.base + '^{tree}',
                            '-p', self.base, '-m', 'concurrent main merge')
        hook = remote / 'hooks/post-receive'
        hook.write_text('#!/bin/sh\nwhile read old new ref; do\n'
                        '  if [ "$ref" = "' + self.branch + '" ]; then\n'
                        '    git update-ref refs/heads/main ' + advanced + '\n'
                        '  fi\ndone\n')
        hook.chmod(0o755)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('main moved after branch publication', result.stderr)
        self.assertEqual(self.remote_head('main'), advanced)
        self.assertEqual(self.git('rev-parse', self.remote_head(self.branch) + '^'), self.base)
        self.assertFalse(Path(self.env['FAKE_PR_STATE']).exists())

    def test_main_moved_after_existing_pr_refuses_without_force_push_or_second_pr(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        head = self.remote_head(self.branch)
        self.mover()
        self.run_script('bash', self.env.pop('FAKE_AF_MOVE_MAIN'))
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('not based on current main', result.stderr)
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')

    def test_existing_branch_with_non_pin_content_is_refused(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        (self.repo / 'extra').write_text('unrelated')
        self.git('add', 'extra')
        self.git('commit', '--amend', '--no-edit', '-q')
        self.git('push', '-q', '--force', 'origin', 'HEAD:' + self.branch)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('not pin-only', result.stderr)
        self.assertEqual(self.remote_head('main'), self.base)

    def test_existing_branch_digest_must_equal_signed_refresh(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        path = self.repo / '.af/af.lock'
        data = path.read_text().replace(version_digest('0.3.0', TARGETS[0]), 'f' * 64)
        path.write_text(data)
        self.git('add', '.af/af.lock')
        self.git('commit', '--amend', '--no-edit', '-q')
        self.git('push', '-q', '--force', 'origin', 'HEAD:' + self.branch)
        head = self.remote_head(self.branch)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('does not match the signed release refresh', result.stderr)
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertEqual(self.remote_head('main'), self.base)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')

    def test_pr_with_changed_base_is_not_duplicated(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        path = Path(self.env['FAKE_PR_STATE'])
        rows = json.loads(path.read_text())
        rows[0]['baseRefName'] = 'other'
        path.write_text(json.dumps(rows))
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('another base/head', result.stderr)
        self.assertEqual(Path(str(path) + '.creates').read_text(), '1')

    def test_workflow_uses_ci_credentials_and_no_main_push_or_bypass(self):
        workflow = (SCRIPTS.parent / '.github/workflows/release.yml').read_text()
        pin = workflow.split('  pin:\n', 1)[1]
        self.assertIn('secrets.RELEASE_PIN_TOKEN', pin)
        self.assertIn('persist-credentials: false', pin)
        self.assertNotIn('ssh-key:', pin)
        self.assertNotIn('PIN_DEPLOY_KEY', pin)
        self.assertNotIn('GH_TOKEN: ${{ github.token }}', pin)
        self.assertLess(pin.index('Require CI-triggering pin credentials'),
                        pin.index('Install the published release'))
        self.assertIn('--pr', pin)
        script = (SCRIPTS / 'pin-release.sh').read_text()
        self.assertNotIn('HEAD:refs/heads/main', script)
        self.assertNotIn('--force', script)

    def test_remote_pr_refuses_non_pin_refresh_and_refused_branch_push(self):
        remote = self.remote()
        self.env['FAKE_AF_WORKER'] = '2.0.0'
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('beyond its [af] tables', result.stderr)
        self.assertEqual(self.remote_head('main'), self.base)
        self.git('reset', '--hard', 'origin/main')
        self.env.pop('FAKE_AF_WORKER')
        (remote / 'hooks/pre-receive').write_text('#!/bin/sh\necho "branch denied" >&2\nexit 1\n')
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('pin branch push refused', result.stderr)
        self.assertNotIn('starting over', result.stderr)
        self.assertEqual(self.remote_head('main'), self.base)

    def test_newer_main_pin_is_never_downgraded(self):
        (self.repo / '.af/af.lock').write_text(lock('0.4.0'))
        self.git('add', '.af/af.lock')
        self.git('commit', '-qm', 'newer pin')
        self.remote()
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('refusing downgrade', result.stderr)
        self.assertEqual(self.remote_head('main'), self.base)
        self.assertEqual(self.git('ls-remote', '--heads', 'origin', self.branch), '')
        self.assertFalse(Path(self.env['FAKE_PR_STATE']).exists())

    def test_merged_pin_is_idempotent(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        head = self.remote_head(self.branch)
        self.git('--git-dir=' + str(self.remote_path), 'update-ref', 'refs/heads/main', head)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('on main already pins af 0.3.0', result.stdout)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')


if __name__ == '__main__':
    unittest.main()
