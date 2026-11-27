#!/usr/bin/env python3
"""Hermetic pin checks and preparation with local Git, fake GitHub and real signatures."""
from contextlib import contextmanager
from pathlib import Path
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
GIT = shutil.which('git')
TARGETS = ['aarch64-apple-darwin', 'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl']


@contextmanager
def fixture_umask(required=0o022):
    """Create owned fixtures without group/world write bits, restoring the caller's umask."""
    previous = os.umask(0o077)
    os.umask(previous | required)
    try:
        yield
    finally:
        os.umask(previous)


def lock(version, worker='1.0.0', targets=TARGETS):
    digests = ''.join(f'{target} = "sha256:{version_digest(version, target)}"\n' for target in sorted(targets))
    return (f'version = 1\n\n[af]\nversion = "{version}"\n\n[af.digests]\n{digests}\n'
            f'[workers.bugs]\nversion = "{worker}"\n')


def version_digest(version, target):
    return hashlib.sha256(f'{version}/{target}'.encode()).hexdigest()


FAKE_GH = r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
state = pathlib.Path(os.environ['FAKE_PR_STATE'])
args = sys.argv[1:]
if args[0] == 'api':
    import hashlib
    sys.path.insert(0, os.environ['FAKE_AF_TESTS'])
    import fixture_af_pin as fixture
    assets = pathlib.Path(os.environ['FIXTURE_ASSETS'])
    if '/releases/tags/' in args[1]:
        version = args[1].split('/tags/v')[1]
        if os.environ.get('FAKE_AF_MOVE_MAIN'):
            subprocess.run(['bash', os.environ['FAKE_AF_MOVE_MAIN']], check=True)
        provenance = json.dumps(dict(schema='af.release-provenance/1',
            version=os.environ.get('PROVENANCE_VERSION', version),
            repository=os.environ.get('PROVENANCE_REPO', 'fixture/repo'), source='a'*40, run_id='1')).encode()
        if os.environ.get('BAD_PROVENANCE'): provenance = b'{bad json'
        names = ['af-v'+version+'-'+t+'.tar.gz' for t in fixture.TARGETS]
        sums = ''.join(fixture.version_digest(version,t)+'  '+n+'\n' for t,n in zip(fixture.TARGETS,names))
        mode = os.environ.get('PROVENANCE', 'absent')
        if mode in ('present', 'missing-asset'):
            sums += hashlib.sha256(provenance).hexdigest()+'  release-provenance.json\n'
        if os.environ.get('BAD_PROVENANCE_DIGEST'): sums = sums.replace(hashlib.sha256(provenance).hexdigest(), '0'*64)
        bad = os.environ.get('BAD_MANIFEST')
        if bad == 'missing': sums = sums.split('\n',1)[1]
        if bad == 'duplicate': sums += sums.splitlines()[0]+'\n'
        if bad == 'foreign': sums += 'f'*64+'  foreign.tar.gz\n'
        if bad == 'malformed': sums += 'oops\n'
        if bad == 'digest': sums = 'z'+sums[1:]
        if bad == 'wrong-version': sums = sums.replace('af-v'+version, 'af-v9.9.9')
        (assets/'SHA256SUMS').write_text(sums)
        (assets/'release-provenance.json').write_bytes(provenance)
        subprocess.run([os.environ['REAL_MINISIGN'], '-S', '-q', '-m', str(assets/'SHA256SUMS'),
            '-s', os.environ['FIXTURE_SECRET'], '-t', 'af v'+os.environ.get('COMMENT_VERSION', version)], check=True)
        if os.environ.get('BAD_SIGNATURE'): (assets/'SHA256SUMS').write_text(sums+'tamper')
        if os.environ.get('MISSING_SIGNATURE'): (assets/'SHA256SUMS.minisig').unlink()
        all_names = names+['SHA256SUMS','SHA256SUMS.minisig','install.sh']
        if mode in ('present', 'missing-row'): all_names.append('release-provenance.json')
        if os.environ.get('BAD_ASSETS') == 'duplicate': all_names.append(names[0])
        if os.environ.get('BAD_ASSETS') == 'missing': all_names.remove(names[0])
        if os.environ.get('BAD_ASSETS') == 'foreign': all_names.append('foreign.tar.gz')
        (assets/'names.json').write_text(json.dumps(all_names))
        print(json.dumps(dict(tag_name='v'+version, draft=False,
            html_url='https://github.com/fixture/repo/releases/tag/v'+version,
            assets=[dict(id=i+1,name=n) for i,n in enumerate(all_names)])))
    else:
        name = json.loads((assets/'names.json').read_text())[int(args[1].rsplit('/',1)[1])-1]
        path=assets/name
        if not path.exists(): sys.exit(1)
        sys.stdout.buffer.write(path.read_bytes())
    sys.exit(0)
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
        with fixture_umask():
            self.root = Path(self.tmp.name)
            self.repo = self.root / 'repo'
            (self.repo / 'scripts').mkdir(parents=True)
            (self.repo / '.af').mkdir()
            (self.repo / '.github/workflows').mkdir(parents=True)
            shutil.copytree(SCRIPTS / 'performance/perflab/_vendor',
                            self.repo / 'scripts/performance/perflab/_vendor')
            for script in ('af-pin.py', 'pin-release.sh', 'prepare-release-pin.py'):
                shutil.copy(SCRIPTS / script, self.repo / 'scripts' / script)
            (self.repo / '.github/workflows/release.yml').write_text(
                ''.join(f'          - target: {target}\n' for target in TARGETS))
            self.changelog('0.2.0', '0.1.0')
            (self.repo / '.af/af.lock').write_text(lock('0.2.0'))
            # Fake transport imports deterministic target digests from this fixture.
            bin_dir = self.root / 'bin'
            bin_dir.mkdir()
            shutil.copy(Path(__file__), bin_dir / 'fixture_af_pin.py')
            self.env = {**os.environ, 'AF': str(bin_dir / 'af'), 'FAKE_AF_TESTS': str(bin_dir), 'PYTHONDONTWRITEBYTECODE': '1',
                        'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
                        'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                        'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}
            for key in ('FAKE_AF_MOVE_MAIN', 'FAKE_PR_REFUSE', 'FAKE_PR_RACE',
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
            self.git('remote', 'add', 'origin', 'https://github.com/fixture/repo.git')
            assets = self.root / 'assets'
            assets.mkdir()
            key = self.repo / 'crates/af/keys/release.pub'
            key.parent.mkdir(parents=True)
            secret = self.root / 'release.key'
            subprocess.run(['minisign', '-G', '-W', '-p', str(key), '-s', str(secret)],
                           check=True, capture_output=True)
            self.env.update(FIXTURE_ASSETS=str(assets), FIXTURE_SECRET=str(secret),
                            REAL_MINISIGN=shutil.which('minisign'))
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

    def helper(self, version='0.3.0', repo='fixture/repo', *extra):
        return self.run_script('python3', '-I', 'scripts/prepare-release-pin.py', version, '--repo', repo, *extra)

    def test_prospective_check_preserves_current_previous_yanked_and_digest_rules(self):
        self.changelog('0.3.0', '0.2.0', '0.1.0')
        self.assertEqual(self.check().returncode, 0)
        self.assertEqual(self.run_script('python3', 'scripts/af-pin.py', '--check-next', '0.4.0').returncode, 1)
        self.changelog('0.3.0 [YANKED]', '0.2.0', '0.1.0')
        self.assertEqual(self.run_script('python3', 'scripts/af-pin.py', '--check-next', '0.4.0').returncode, 0)
        (self.repo / '.af/af.lock').write_text(lock('0.2.0', targets=TARGETS[:1]))
        self.assertEqual(self.run_script('python3', 'scripts/af-pin.py', '--check-next', '0.4.0').returncode, 1)
        (self.repo / '.af/af.lock').write_text(lock('0.4.0'))
        self.assertEqual(self.run_script('python3', 'scripts/af-pin.py', '--check-next', '0.4.0').returncode, 0)

    def test_published_rc3_archive_only_shape_with_real_local_signature(self):
        # Equivalent of published RC3: exactly three archive rows, no provenance asset/row.
        version = '0.15.1-rc.3'
        result = self.helper(version)
        self.assertEqual(result.returncode, 0, result.stderr)
        assets = Path(self.env['FIXTURE_ASSETS'])
        expected = {'af-v'+version+'-'+t+'.tar.gz' for t in TARGETS}
        self.assertEqual(set(json.loads((assets / 'names.json').read_text())),
                         expected | {'install.sh', 'SHA256SUMS', 'SHA256SUMS.minisig'})
        self.assertEqual(len((assets / 'SHA256SUMS').read_text().splitlines()), 3)
        text = (self.repo / '.af/af.lock').read_text()
        self.assertIn('version = "'+version+'"', text)
        for target in TARGETS:
            self.assertIn(target+' = "sha256:'+version_digest(version, target)+'"', text)
        self.assertEqual(self.helper(version, 'fixture/repo', '--check').returncode, 0)
        self.assertEqual(self.git('status', '--porcelain'), 'M .af/af.lock')

    def test_future_provenance_is_verified_and_never_ignored(self):
        before = (self.repo / '.af/af.lock').read_bytes()
        for mode in ('missing-asset', 'missing-row'):
            self.env['PROVENANCE'] = mode
            result = self.helper()
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)
        self.env['PROVENANCE'] = 'present'
        for flag, value in (('PROVENANCE_REPO', 'foreign/repo'), ('PROVENANCE_VERSION', '9.9.9'),
                            ('BAD_PROVENANCE_DIGEST', '1'), ('BAD_PROVENANCE', '1')):
            with self.subTest(flag=flag):
                self.env[flag] = value
                self.assertEqual(self.helper().returncode, 1)
                self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)
                self.env.pop(flag)
        self.assertEqual(self.helper().returncode, 0)

    def test_signed_inventory_failures_and_scratch_cleanup_before_write(self):
        before = (self.repo / '.af/af.lock').read_bytes()
        scratch = self.root / 'scratch'
        scratch.mkdir()
        self.env['TMPDIR'] = str(scratch)
        for flag, values in (('BAD_MANIFEST', ('missing', 'duplicate', 'foreign', 'malformed', 'digest', 'wrong-version')),
                             ('BAD_ASSETS', ('missing', 'duplicate', 'foreign')),
                             ('MISSING_SIGNATURE', ('1',)), ('COMMENT_VERSION', ('9.9.9',))):
            for value in values:
                with self.subTest(flag=flag, value=value):
                    self.env[flag] = value
                    self.assertEqual(self.helper().returncode, 1)
                    self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)
                    self.assertEqual(list(scratch.iterdir()), [])
            self.env.pop(flag)

    def test_real_wrong_key_and_tamper_fail_before_write(self):
        before = (self.repo / '.af/af.lock').read_bytes()
        other_secret = self.root / 'wrong.key'
        subprocess.run(['minisign', '-G', '-W', '-p', str(self.root / 'wrong.pub'),
                        '-s', str(other_secret)], check=True, capture_output=True)
        self.env['FIXTURE_SECRET'] = str(other_secret)
        self.assertEqual(self.helper().returncode, 1)
        self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)
        self.env['FIXTURE_SECRET'] = str(self.root / 'release.key')
        self.env['BAD_SIGNATURE'] = '1'
        self.assertEqual(self.helper().returncode, 1)
        self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)

    def test_exact_bytes_outside_af_tables_and_check_only(self):
        path = self.repo / '.af/af.lock'
        prefix = b'# preamble\r\nversion = 1\r\n\r\n'
        suffix = b'[task.pipelines."fixture"]\r\n# preserve comments\r\ndigest = "unchanged"\r\n'
        body = lock('0.2.0').split('[af]', 1)[1].split('[workers.bugs]', 1)[0]
        before = prefix + b'[af]' + body.encode() + suffix
        path.write_bytes(before)
        self.assertEqual(self.helper('0.3.0', 'fixture/repo', '--check').returncode, 1)
        self.assertEqual(path.read_bytes(), before)
        self.assertEqual(self.helper().returncode, 0)
        self.assertTrue(path.read_bytes().startswith(prefix))
        self.assertTrue(path.read_bytes().endswith(suffix))
        self.assertFalse(list(path.parent.glob('.af-pin-*')))

    def test_signed_refresh_preserves_indented_non_pin_tables(self):
        path = self.repo / '.af/af.lock'
        for header in ('  [workers.bugs]', '\t[task.pipelines."fixture"] # comment',
                       ' \t["workers" . "bugs"] # quoted', '  [[pipelines]]'):
            with self.subTest(header=header):
                prefix = lock('0.2.0').split('[workers.bugs]')[0]
                if header.startswith(' \t["workers"'):
                    prefix = prefix.replace('[af]', ' \t[ "af" ] # pin').replace(
                        '[af.digests]', '\t[ "af" . "digests" ] # signed digests')
                prefix = prefix.encode()
                suffix = (header + '\r\n# retain bytes\r\nversion = "1.0.0"\r\n').encode()
                before = prefix + suffix
                path.write_bytes(before)
                result = self.helper()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(path.read_bytes().endswith(suffix), path.read_bytes())
                result = self.helper('0.3.0', 'fixture/repo', '--check')
                self.assertEqual(result.returncode, 0, result.stderr)

    def indented_lock(self):
        path = self.repo / '.af/af.lock'
        text = lock('0.2.0').replace('[workers.bugs]', ' \t["workers" . "bugs"] # retain')
        text += '\n  [[task.pipelines."fixture"]] # array table\nvalue = "unchanged"\n'
        path.write_text(text)
        self.git('add', '.af/af.lock')
        self.git('commit', '-qm', 'indented non-pin tables')
        return path, text[text.index(' \t["workers"'):]

    def test_wrapper_and_published_branch_preserve_indented_tables(self):
        path, suffix = self.indented_lock()
        self.remote()
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(path.read_text().endswith(suffix))
        head = self.remote_head(self.branch)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertTrue(path.read_text().endswith(suffix))

    def test_indented_non_pin_tamper_is_refused_by_both_guards(self):
        path, _ = self.indented_lock()
        self.remote()
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        path.write_text(path.read_text().replace('version = "1.0.0"', 'version = "9.0.0"'))
        before = path.read_bytes()
        result = self.run_script('python3', '-I', 'scripts/af-pin.py',
                                 '--only-pin-moved', self.base, '0.3.0')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(path.read_bytes(), before)
        self.git('add', '.af/af.lock')
        self.git('commit', '--amend', '--no-edit', '-q')
        self.git('push', '-q', '--force', 'origin', 'HEAD:' + self.branch)
        head = self.remote_head(self.branch)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('beyond its [af] tables', result.stderr)
        self.assertEqual(path.read_bytes(), before)
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')

    def test_pin_only_guard_rejects_non_pin_bytes_and_invalid_toml(self):
        path, _ = self.indented_lock()
        result = self.helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        signed = path.read_bytes()
        cases = [signed.replace(b'# retain', b'# changed comment'),
                 signed.replace(b'version = "1.0.0"', b'version  = "1.0.0"'),
                 signed.replace(b'# retain\n', b'# retain\r\n'),
                 signed + b'  [workers.bugs]\nversion = "duplicate"\n']
        for data in cases:
            with self.subTest(data=data):
                path.write_bytes(data)
                result = self.run_script('python3', '-I', 'scripts/af-pin.py',
                                         '--only-pin-moved', 'HEAD', '0.3.0')
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(path.read_bytes(), data)

    def test_pin_only_guard_rejects_added_changed_removed_unknown_af_fields(self):
        path = self.repo / '.af/af.lock'
        for old_extra, new_extra in (('', 'extra = "added"\n'),
                                     ('extra = "old"\n', 'extra = "changed"\n'),
                                     ('extra = "removed"\n', '')):
            with self.subTest(old_extra=old_extra, new_extra=new_extra):
                path.write_text(lock('0.2.0').replace('[af]\n', '[af]\n' + old_extra))
                self.git('add', '.af/af.lock')
                self.git('commit', '--allow-empty', '-qm', 'guard base')
                after = lock('0.3.0').replace('[af]\n', '[af]\n' + new_extra)
                path.write_text(after)
                result = self.run_script('python3', '-I', 'scripts/af-pin.py',
                                         '--only-pin-moved', 'HEAD', '0.3.0')
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(path.read_text(), after)

    def test_pin_only_guard_accepts_valid_pin_and_rejects_malformed_base(self):
        path, _ = self.indented_lock()
        self.assertEqual(self.helper().returncode, 0)
        after = path.read_bytes()
        result = self.run_script('python3', '-I', 'scripts/af-pin.py',
                                 '--only-pin-moved', 'HEAD', '0.3.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        path.write_bytes(after + b'broken = "unterminated\n')
        self.git('add', '.af/af.lock')
        self.git('commit', '-qm', 'malformed base')
        path.write_bytes(after)
        result = self.run_script('python3', '-I', 'scripts/af-pin.py',
                                 '--only-pin-moved', 'HEAD', '0.3.0')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(path.read_bytes(), after)

    def test_helper_and_standalone_guard_enforce_the_shared_invariant(self):
        result = self.run_script('python3', '-I', '-c', """
import importlib.util
spec = importlib.util.spec_from_file_location('prepare', 'scripts/prepare-release-pin.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
validate = module.pin.validate_pin_edit
calls = []
def record(*args):
    calls.append(args)
    validate(*args)
module.pin.validate_pin_edit = record
module.prepare('0.3.0', 'fixture/repo')
assert module.pin.only_pin_moved('HEAD', '0.3.0') == 0
assert len(calls) == 2, calls
assert calls[0] == calls[1]
before = module.pin.LOCK.read_bytes()
def refuse(*args):
    raise ValueError('shared predicate refused')
module.pin.validate_pin_edit = refuse
try:
    module.prepare('0.4.0', 'fixture/repo')
except ValueError as error:
    assert str(error) == 'shared predicate refused', error
else:
    raise AssertionError('helper bypassed shared predicate')
assert module.pin.only_pin_moved('HEAD', '0.3.0') == 1
assert module.pin.LOCK.read_bytes() == before
""")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_table_like_values_are_preserved_and_not_pin_boundaries(self):
        path = self.repo / '.af/af.lock'
        prefix = ('version = 1\r\nnote = """\r\n[af]\r\n'
                  '  [af.digests]\r\n"""\r\n')
        suffix = ("  ['workers'.'bugs'] # comment\r\nnote = " + "'''" +
                  '\r\n[af]\r\n[af.digests]\r\n' + "'''" + '\r\n' +
                  'items = ["[af]", "[workers.fake]", { key = "[[af.digests]]" }]\r\n' +
                  '  [[task.pipelines."quoted.name"]] # retain\r\nversion = "1"')
        pin_tables = '[af]' + lock('0.2.0').split('[af]', 1)[1].split('[workers.bugs]')[0]
        before = prefix + pin_tables + suffix
        path.write_bytes(before.encode())
        result = self.helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(path.read_bytes().startswith(prefix.encode()))
        self.assertTrue(path.read_bytes().endswith(suffix.encode()))

    def test_unsafe_lock_shapes_refuse_before_write(self):
        path = self.repo / '.af/af.lock'
        original = lock('0.2.0')
        cases = [original + '  [workers.bugs]\nversion = "duplicate"\n',
                 original + 'broken = "unterminated\n',
                 original.replace('[af]\n', '[af]\nextra = "must not disappear"\n'),
                 original.replace('[af.digests]', '[af.digests] trailing'),
                 original.replace('[af.digests]', '["af.digests"]'),
                 original.replace('[af.digests]', '[[af.digests]]')]
        for text in cases:
            with self.subTest(text=text):
                path.write_bytes(text.encode())
                result = self.helper()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(path.read_bytes(), text.encode())
                self.assertFalse(list(path.parent.glob('.af-pin-*')))

    def test_prewrite_invariant_rejects_a_corrupt_replacement(self):
        path = self.repo / '.af/af.lock'
        before = path.read_bytes()
        # Reproduce the old destructive span independently of the new scanner.
        # Even a bad edit plan must not reach the atomic replacement.
        result = self.run_script('python3', '-I', '-c', """
import importlib.util
spec = importlib.util.spec_from_file_location('prepare', 'scripts/prepare-release-pin.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
scan = module.pin.table_spans
calls = 0
def corrupt(text):
    global calls
    calls += 1
    spans = scan(text)
    if calls == 1:
        spans[-2] = (spans[-2][0], spans[-2][1], len(text))
        spans.pop()
    return spans
module.pin.table_spans = corrupt
try:
    module.prepare('0.3.0', 'fixture/repo')
except ValueError:
    raise SystemExit(1)
""")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(path.read_bytes(), before)
        self.assertFalse(list(path.parent.glob('.af-pin-*')))

    def test_invalid_identity_and_hostile_paths_never_write(self):
        path = self.repo / '.af/af.lock'
        before = path.read_bytes()
        for version, repo in (('../0.3.0', 'fixture/repo'), ('0.3.0', 'other/repo'),
                              ('0.3.0', 'user:secret@host/repo')):
            result = self.helper(version, repo)
            self.assertEqual(result.returncode, 1)
            self.assertNotIn('secret', result.stderr)
            self.assertEqual(path.read_bytes(), before)
        for target in (path, path.parent, self.repo / 'crates/af/keys/release.pub'):
            with self.subTest(group_writable=target.name):
                mode = stat.S_IMODE(target.stat().st_mode)
                target.chmod(mode | 0o020)
                try:
                    self.assertEqual(self.helper().returncode, 1)
                    self.assertEqual(path.read_bytes(), before)
                    if target == path:
                        self.assertEqual(self.pin_release('0.3.0').returncode, 1)
                        self.assertEqual(path.read_bytes(), before)
                finally:
                    target.chmod(mode)
        external = self.root / 'foreign.lock'
        path.rename(external)
        path.symlink_to(external)
        self.assertEqual(self.helper().returncode, 1)
        self.assertEqual(external.read_bytes(), before)
        path.unlink()
        os.link(external, path)
        self.assertEqual(self.helper().returncode, 1)
        self.assertEqual(external.read_bytes(), before)
        path.unlink()
        with fixture_umask():
            path.write_bytes(before)
        parent = path.parent
        moved = self.root / 'authority'
        parent.rename(moved)
        parent.symlink_to(moved, target_is_directory=True)
        self.assertEqual(self.helper().returncode, 1)
        self.assertEqual((moved / 'af.lock').read_bytes(), before)

    def test_pin_release_moves_only_the_pin(self):
        result = self.pin_release('0.3.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('version = "0.3.0"', (self.repo / '.af/af.lock').read_text())
        self.assertEqual(self.git('status', '--porcelain'), 'M .af/af.lock')

    def test_pin_release_signature_failure_preserves_lock(self):
        before = (self.repo / '.af/af.lock').read_bytes()
        self.env['BAD_SIGNATURE'] = '1'
        result = self.pin_release('0.3.0')
        self.assertEqual(result.returncode, 1)
        self.assertIn('signed release pin preparation refused', result.stderr)
        self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_pin_release_is_a_no_op_when_already_pinned(self):
        result = self.pin_release('0.2.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('already pins af 0.2.0', result.stdout)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def noncanonical_pin(self, version):
        path = self.repo / '.af/af.lock'
        text = lock(version).replace('version = "' + version + '"',
                                     '  version  = "' + version + '" # selected release')
        text = text.replace(' = "sha256:', '  =  "sha256:')
        pin_text, workers = text.split('[workers.bugs]', 1)
        text = pin_text.replace('"\n', '" # pin comment\n') + '[workers.bugs]' + workers
        path.write_text(text)
        self.git('add', '.af/af.lock')
        self.git('commit', '-qm', 'valid noncanonical pin')
        return path.read_bytes()

    def test_noncanonical_helper_and_manual_pin_are_idempotent(self):
        before = self.noncanonical_pin('0.2.0')
        result = self.helper('0.2.0', 'fixture/repo', '--check')
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.pin_release('0.2.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('already pins af 0.2.0', result.stdout)
        self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_semantic_check_refuses_invalid_or_foreign_pin_without_writing(self):
        path = self.repo / '.af/af.lock'
        original = lock('0.2.0')
        cases = [original.replace(version_digest('0.2.0', TARGETS[0]), 'f' * 64),
                 original.replace(TARGETS[0], 'foreign-target'),
                 original.replace('[af.digests]', '[af.digests]\nforeign = "sha256:' + 'f' * 64 + '"'),
                 original.replace('[af]\n', '[af]\nversion = "0.2.0"\n'),
                 original.replace('[af.digests]', '[af.digests]\n' + TARGETS[0] + ' = "bad"'),
                 original.replace('0.2.0', '0.1.0')]
        for text in cases:
            with self.subTest(text=text):
                path.write_text(text)
                before = path.read_bytes()
                result = self.helper('0.2.0', 'fixture/repo', '--check')
                self.assertEqual(result.returncode, 1)
                self.assertEqual(path.read_bytes(), before)

    def test_noncanonical_main_pin_pr_is_idempotent(self):
        before = self.noncanonical_pin('0.3.0')
        self.remote()
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('on main already pins af 0.3.0', result.stdout)
        self.assertEqual((self.repo / '.af/af.lock').read_bytes(), before)
        self.assertEqual(self.git('status', '--porcelain'), '')
        self.assertEqual(self.remote_head('main'), self.base)
        self.assertEqual(self.git('ls-remote', '--heads', 'origin', self.branch), '')
        self.assertFalse(Path(self.env['FAKE_PR_STATE']).exists())

    def test_noncanonical_published_branch_is_not_reused(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        path = self.repo / '.af/af.lock'
        path.write_text(path.read_text().replace('[af]\n', '[af]\n\n'))
        self.git('add', '.af/af.lock')
        self.git('commit', '--amend', '--no-edit', '-q')
        self.git('push', '-q', '--force', 'origin', 'HEAD:' + self.branch)
        head = self.remote_head(self.branch)
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('does not match the signed release refresh', result.stderr)
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')

    def test_catalog_policy_and_worker_pins_unchanged_without_af(self):
        catalog = self.repo / '.af/task-catalog.toml'
        policy = self.repo / '.af/af.toml'
        catalog.write_text('provider_admission = "required"\n')
        policy.write_text('# project policy\n')
        self.git('add', '.af')
        self.git('commit', '-qm', 'project authority')
        before = (self.repo / '.af/af.lock').read_text().split('[workers.bugs]')[1]
        self.env['AF'] = '/nonexistent/af'
        result = self.pin_release('0.3.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(catalog.read_text(), 'provider_admission = "required"\n')
        self.assertEqual(policy.read_text(), '# project policy\n')
        self.assertEqual((self.repo / '.af/af.lock').read_text().split('[workers.bugs]')[1], before)
        self.assertEqual(self.git('status', '--porcelain'), 'M .af/af.lock')

    def test_existing_branch_signature_failure_preserves_checkout_and_ref(self):
        self.remote()
        self.assertEqual(self.pin_release('0.3.0', '--pr').returncode, 0)
        head = self.remote_head(self.branch)
        self.env['BAD_SIGNATURE'] = '1'
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.remote_head(self.branch), head)
        self.assertEqual(self.git('status', '--porcelain'), '')
        self.assertEqual(Path(self.env['FAKE_PR_STATE'] + '.creates').read_text(), '1')

    def remote(self):
        remote = self.root / 'remote.git'
        self.git('init', '-q', '--bare', '-b', 'main', str(remote))
        self.git('remote', 'set-url', 'origin', str(remote))
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
                        pin.index('Install signature verifier'))
        self.assertNotIn('sh install.sh', pin)
        self.assertNotIn('AF_INSTALL_VERSION', pin)
        self.assertIn('--pr', pin)
        script = (SCRIPTS / 'pin-release.sh').read_text()
        self.assertNotIn('HEAD:refs/heads/main', script)
        self.assertNotIn('--force', script)

    def test_remote_pr_refuses_non_pin_refresh_and_refused_branch_push(self):
        remote = self.remote()
        self.env['BAD_SIGNATURE'] = '1'
        result = self.pin_release('0.3.0', '--pr')
        self.assertEqual(result.returncode, 1)
        self.assertIn('signed release pin preparation refused', result.stderr)
        self.assertEqual(self.remote_head('main'), self.base)
        self.git('reset', '--hard', 'origin/main')
        self.env.pop('BAD_SIGNATURE')
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


class InheritedUmask(unittest.TestCase):
    def test_fixture_umask_tightens_group_write_preserves_strict_and_restores_on_error(self):
        original = os.umask(0o002)
        try:
            with fixture_umask():
                observed = os.umask(0o077)
                os.umask(observed)
                self.assertEqual(observed, 0o022)
            restored = os.umask(0o077)
            self.assertEqual(restored, 0o002)
            with self.assertRaisesRegex(RuntimeError, 'boom'):
                with fixture_umask():
                    observed = os.umask(0o022)
                    os.umask(observed)
                    self.assertEqual(observed, 0o077)
                    raise RuntimeError('boom')
            restored = os.umask(original)
            self.assertEqual(restored, 0o077)
        finally:
            os.umask(original)

    def test_release_pin_fixtures_and_wrapper_pass_under_umask_002(self):
        """A host with umask 0002 must pass fixture setup and pin-release.sh checkouts
        without weakening prepare-release-pin.py's non-group-writable ownership checks."""
        cases = (
            'Repository.test_exact_bytes_outside_af_tables_and_check_only',
            'Repository.test_pr_is_pin_only_on_current_main_and_reuses_one_pr',
            'Repository.test_main_moved_during_refresh_rebuilds_on_new_tip',
            'Repository.test_invalid_identity_and_hostile_paths_never_write',
        )
        child = (
            'import os, runpy, sys\n'
            'os.umask(0o002)\n'
            'sys.argv = sys.argv[1:]\n'
            'runpy.run_path(sys.argv[0], run_name="__main__")\n'
        )
        result = subprocess.run(
            [sys.executable, '-IB', '-c', child, str(Path(__file__).resolve()), *cases],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f'Ran {len(cases)} tests', result.stderr)
        self.assertIn('OK', result.stderr)


if __name__ == '__main__':
    unittest.main()
