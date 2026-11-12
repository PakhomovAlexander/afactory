#!/usr/bin/env python3
"""Hermetic subprocess release lifecycle against local Git and a fake GitHub server."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
GIT = shutil.which('git')
FAKE = r'''#!/usr/bin/env python3
import hashlib,json,os,subprocess,sys,time
from pathlib import Path
name=Path(sys.argv[0]).name
a=sys.argv[1:]
f=Path(os.environ['FAKE_DB']); d=json.loads(f.read_text())
def save(): f.write_text(json.dumps(d))
def crash(effect,when):
 if d.get('crash')==effect+':'+when:
  d['crash']=''; save(); sys.exit(55)
def effect(key,action):
 crash(key,'before'); action(); d.setdefault('effects',[]).append(key); save(); crash(key,'after')
def out(value): print(json.dumps(value)); sys.exit(0)
if name=='git':
 key = 'push' if 'push' in a else ('prepare' if 'commit-tree' in a else '')
 if key: crash(key,'before')
 p=subprocess.run([os.environ['REAL_GIT'],*a],capture_output=True)
 sys.stdout.buffer.write(p.stdout); sys.stderr.buffer.write(p.stderr)
 if p.returncode: d['last_git_error']=p.stderr.decode();save()
 if key and p.returncode==0:
  d.setdefault('effects',[]).append(key);save();crash(key,'after')
 sys.exit(p.returncode)
if name=='cargo':
 if a==['--version']: print('cargo test');sys.exit(0)
 if a==['nextest','--version']: print('cargo-nextest test');sys.exit(0)
 effect('metadata',lambda:None);sys.exit(0)
if name=='rustc': print('rustc 1.88.0 test');sys.exit(0)
if name=='minisign': sys.exit(1 if d.get('bad_signature') else 0)
if name=='npx': sys.exit(0)
if name!='gh': sys.exit(99)
if os.environ.get('FAKE_DELAY'): time.sleep(float(os.environ['FAKE_DELAY']))
if a[0]=='api':
 endpoint=a[1].replace('{owner}/{repo}','o/r').split('repos/o/r/',1)[1]
 if endpoint=='': out({'full_name':'o/r','permissions':{'push':not d.get('deny')}})
 if endpoint=='actions/secrets': out({'secrets':[{'name':n} for n in (['MINISIGN_SECRET_KEY','RELEASE_PIN_TOKEN'] if not d.get('no_secrets') else [])]})
 if endpoint.startswith('labels'): out([] if d.get('no_label') else [{'name':'release:fast'},{'name':'release:full'}])
 if endpoint.startswith('releases/'):
  out(next(r for r in d['releases'] if str(r['id'])==endpoint.split('/')[1]))
 if endpoint.startswith('releases'): out(d.get('releases',[]))
 if endpoint.startswith('actions/workflows'): out({'workflow_runs':[{'id':1,'head_sha':d['merged'],'conclusion':'failure'}]})
 if endpoint.startswith('actions/runs'): out({'jobs':[{'name':'sign and publish','conclusion':'success'},{'name':'pin the release in .af/af.lock','conclusion':'failure'}]})
if a[:2]==['pr','list']:
 head=a[a.index('--head')+1]
 if head.startswith('release/pin-'): out(d.get('pins',[]))
 out([d['pr']] if d.get('pr') and d.get('pr_branch') == head else [])
if a[:2]==['pr','create']:
 def create():
  head=a[a.index('--head')+1]
  sha=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'rev-parse','refs/heads/'+head],text=True).strip()
  d['pr_branch']=head
  d['pr']={'number':1,'url':'https://example.test/pr/1','state':'OPEN','headRefOid':sha,'baseRefName':'main','mergeCommit':None,'isCrossRepository':False,'labels':[{'name':a[a.index('--label')+1]}]}
 effect('pr',create);print('https://example.test/pr/1');sys.exit(0)
if a[:2]==['pr','checks']:
 out([{'name':'gate','bucket':d.get('checks','pass'),'link':'https://example.test/check'}])
if a[:2]==['pr','merge']:
 def merge():
  sha=d['pr']['headRefOid'];d['pr']['state']='MERGED';d['pr']['mergeCommit']={'oid':sha};d['merged']=sha
  subprocess.check_call([os.environ['REAL_GIT'],'--git-dir',d['remote'],'update-ref','refs/heads/main',sha])
 effect('merge',merge);sys.exit(0)
if a[:2]==['release','create']:
 def create_release():
  d['releases']=[{'id':1,'tag_name':a[2],'target_commitish':a[a.index('--target')+1], 'draft':True,'assets':[]}]
 effect('create-release',create_release);sys.exit(0)
if a[:2]==['release','upload']:
 def upload():
  import shutil
  path=Path(a[3]);shutil.copyfile(path,Path(d['assets'])/path.name)
  d['releases'][0]['assets'].append({'name':path.name})
 effect('upload',upload);sys.exit(0)
if a[:2]==['release','edit']:
 def publish(): d['releases'][0]['draft']=False
 effect('publish',publish);sys.exit(0)
if a[:2]==['release','download']:
 def download():
  name=a[a.index('--pattern')+1];directory=Path(a[a.index('--dir')+1]);directory.mkdir(exist_ok=True)
  (directory/name).write_bytes((Path(d['assets'])/name).read_bytes())
 effect('download',download);sys.exit(0)
print('unsupported fake request '+str(a),file=sys.stderr);sys.exit(98)
'''


class ReleaseCLI(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.source = self.base / 'source'
        self.source.mkdir()
        self.remote = self.base / 'remote.git'
        self.state = self.base / 'state'
        self.db = self.base / 'github.json'
        self.db.write_text(json.dumps({'remote': str(self.remote)}))
        self.bin = self.base / 'bin'
        self.bin.mkdir()
        (self.base / 'tmp').mkdir()
        for name in ('git', 'gh', 'cargo', 'rustc', 'minisign', 'npx'):
            path = self.bin / name
            path.write_text(FAKE)
            path.chmod(0o755)
        self.env = {**os.environ, 'PATH': str(self.bin) + os.pathsep + os.environ['PATH'],
                    'FAKE_DB': str(self.db), 'REAL_GIT': GIT,
                    'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
                    'HOME': str(self.base / 'home'), 'TMPDIR': str(self.base / 'tmp')}
        for name in ('dev', 'scripts/dev-release.py', 'scripts/release-contract.py',
                     'scripts/release-contract.json', 'scripts/release-policy.json',
                     'scripts/changelog-notes.py', 'crates/af/keys/release.pub',
                     'CHANGELOG.md', 'rust-toolchain.toml'):
            target = self.source / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)
        for name in ('schemas', 'scripts/performance/perflab'):
            shutil.copytree(ROOT / name, self.source / name)
        shutil.copyfile(ROOT / 'scripts/telemetry-overhead.py', self.source / 'scripts/telemetry-overhead.py')
        for fixture in (ROOT / 'fixtures').glob('telemetry-bench'):
            shutil.copytree(fixture, self.source / 'fixtures' / fixture.name)
        for name in ('retention.rs', 'roadmap.rs', 'store.rs'):
            path = self.source / 'crates/af-scenario/src' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / 'crates/af-scenario/src' / name, path)
        (self.source / 'changelog.d').mkdir()
        (self.source / 'changelog.d/README.md').write_text('Fragment instructions.\n')
        (self.source / 'changelog.d/note.md').write_text('- A reviewed feature.\n')
        (self.source / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.15.1-rc.3"\n')
        (self.source / 'Cargo.lock').write_text('# fixture lock\n')
        harness = self.source / 'scripts/performance/cli.py'
        harness.write_text('import pathlib,sys\np=pathlib.Path("fixtures/performance/kernel-measure/pin")\n'
                           'if "--write" in sys.argv:\n p.parent.mkdir(parents=True,exist_ok=True);p.write_text("pin")\n')
        verify = self.source / 'scripts/verify.sh'
        verify.write_text('#!' + sys.executable + '\nimport json,os,sys\nsys.exit(9 if json.load(open(os.environ["FAKE_DB"])).get("gate_fail") else 0)\n')
        verify.chmod(0o755)
        self.git('init', '-q', '-b', 'main', self.source)
        self.commit('initial')
        self.git('clone', '--bare', '--no-local', self.source, self.remote)
        self.initial = self.git('-C', self.source, 'rev-parse', 'HEAD')

    def git(self, *args):
        return subprocess.check_output([GIT, *map(str, args)], text=True, stderr=subprocess.DEVNULL, env=self.env).strip()

    def commit(self, title):
        self.git('-C', self.source, 'add', '-A')
        self.git('-C', self.source, '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                 'commit', '-qm', title)

    def data(self, **changes):
        value = json.loads(self.db.read_text())
        value.update(changes)
        self.db.write_text(json.dumps(value))
        return value

    def cli(self, command, *args, ok=True, extra_env=None):
        result = subprocess.run([sys.executable, str(self.source / 'dev'), 'release', command,
                                 '--repo', 'o/r', '--remote', str(self.remote), '--state', str(self.state),
                                 '--format', 'json', *args], cwd=self.source,
                                env={**self.env, **(extra_env or {})}, text=True, capture_output=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr + self.data().get('last_git_error',''))
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def state_value(self):
        return json.loads((self.state / 'run.json').read_text())

    def test_state_schema_parity_and_unknown_fields_fail_closed(self):
        import runpy
        module = runpy.run_path(str(ROOT / 'scripts/dev-release.py'))
        self.assertEqual(module['state_schema'](),
                         json.loads((ROOT / 'schemas/dev-release-run-v1.json').read_text()))
        self.cli('plan')
        path = self.state / 'run.json'
        state = json.loads(path.read_text())
        state['unknown'] = True
        path.write_text(json.dumps(state))
        self.assertIn('foreign_state', self.cli('resume', ok=False).stderr)

    def test_plan_dirty_caller_and_read_only_status(self):
        (self.source / 'Cargo.toml').write_text('dirty caller cannot supply bytes')
        result = self.cli('plan')
        state = json.loads(result.stdout)
        self.assertEqual(state['plan']['source'], self.initial)
        self.assertEqual(state['plan']['version'], '0.15.1-rc.4')
        self.assertIsNone(state['authorization'])
        self.assertEqual(self.data().get('effects', []), [])
        before = (self.state / 'run.json').read_bytes()
        self.cli('status')
        self.assertEqual((self.state / 'run.json').read_bytes(), before)
        self.cli('resume', ok=False)
        self.assertEqual((self.source / 'Cargo.toml').read_text(), 'dirty caller cannot supply bytes')

    def test_real_cut_reuses_exact_identity_and_pin_is_separate(self):
        self.cli('rc')
        state = self.state_value()
        self.assertEqual(state['stage'], 'publication')
        self.assertEqual(state['status'], 'waiting')
        self.assertEqual(self.data()['effects'].count('pr'), 1)
        self.cli('resume')
        self.assertEqual(self.data()['effects'].count('pr'), 1)
        self.assertEqual(self.data()['effects'].count('merge'), 1)
        self.assertEqual(self.git('-C', self.source, 'rev-parse', 'HEAD'), self.initial)
        self.publish_assets(state)
        self.cli('resume')
        ready = self.state_value()
        self.assertTrue(ready['published'])
        self.assertEqual(ready['pin']['state'], 'PENDING')
        self.assertEqual(ready['status'], 'ready')
        self.data(pins=[{'state': 'MERGED', 'url': 'https://example.test/pr/2'}])
        self.cli('resume')
        self.assertTrue(self.state_value()['published'])
        self.assertEqual(self.state_value()['pin']['state'], 'MERGED')

    def publish_assets(self, state):
        self.git('--git-dir', self.remote, 'update-ref', 'refs/tags/v0.15.1-rc.4', state['candidate'])
        assets = self.base / 'assets'
        assets.mkdir()
        names = ['af-v0.15.1-rc.4-' + t + '.tar.gz' for t in state['plan']['policy']['targets']]
        import hashlib
        for name in names:
            (assets / name).write_bytes(name.encode())
        (assets / 'release-provenance.json').write_text(json.dumps({
            'schema': 'af.release-provenance/1', 'version': state['plan']['version'],
            'source': state['candidate'], 'repository': 'o/r', 'run_id': '1'}))
        names.append('release-provenance.json')
        (assets / 'SHA256SUMS').write_text(''.join(hashlib.sha256((assets / n).read_bytes()).hexdigest() + '  ' + n + '\n' for n in names))
        (assets / 'SHA256SUMS.minisig').write_text('fixture signature checked by fake minisign')
        self.data(assets=str(assets), releases=[{'tag_name': 'v0.15.1-rc.4', 'draft': False,
                  'html_url': 'https://example.test/release', 'assets': [{'name': n} for n in
                  [*names, 'SHA256SUMS', 'SHA256SUMS.minisig']]}])

    def test_next_cut_requires_explicit_new_and_keeps_terminal_receipt(self):
        self.cli('rc')
        self.assertIn('unfinished_run', self.cli('plan', '--new', ok=False).stderr)
        self.publish_assets(self.state_value())
        self.cli('resume')
        self.cli('plan', '--new')
        self.assertEqual(self.state_value()['plan']['version'], '0.15.1-rc.5')
        self.assertTrue((self.state / 'history/0.15.1-rc.4.json').is_file())
        self.assertFalse((self.state / 'candidate').exists())

    def test_ambiguous_effects_reconcile_without_duplicate_prs(self):
        for effect in ('metadata', 'prepare', 'push', 'pr', 'merge'):
            for when in ('before', 'after'):
                with self.subTest(effect=effect, when=when):
                    # Each identity uses an independent local remote and owned state.
                    self.state = self.base / ('state-' + effect + '-' + when)
                    self.remote = self.base / ('remote-' + effect + '-' + when + '.git')
                    self.git('clone', '--bare', '--no-local', self.source, self.remote)
                    self.db.write_text(json.dumps({'remote': str(self.remote), 'crash': effect + ':' + when}))
                    self.cli('rc', ok=False)
                    self.cli('resume')
                    self.assertEqual(self.data()['effects'].count('pr'), 1)
                    self.assertEqual(self.data()['effects'].count('merge'), 1)
                    self.assertEqual(self.state_value()['status'], 'waiting')

    def test_conflicting_source_version_and_foreign_branch(self):
        self.assertIn('authority_source_mismatch', self.cli('plan', '--source', '0' * 40, ok=False).stderr)
        self.cli('plan')
        self.assertIn('version_conflict', self.cli('rc', '--version', '0.15.1-rc.9', ok=False).stderr)
        self.git('--git-dir', self.remote, 'update-ref', 'refs/heads/release/v0.15.1-rc.4', self.initial)
        self.assertIn('branch_collision', self.cli('rc', ok=False).stderr)

    def test_old_features_and_executor_source_mismatch(self):
        p = self.source / 'crates/af-scenario/src/retention.rs'
        p.write_text('// old source')
        self.commit('old features')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.assertIn('source_inclusion', self.cli('plan', ok=False).stderr)
        (self.source / 'scripts/release-policy.json').write_text('{}')
        self.assertIn('authority_source_mismatch', self.cli('plan', ok=False).stderr)

    def test_release_pr_profile_drift_and_fork_are_refused(self):
        self.data(checks='pending')
        self.cli('rc')
        pr = self.data()['pr']
        pr['labels'] = [{'name': 'release:full'}]
        self.data(pr=pr)
        self.assertIn('profile_conflict', self.cli('resume', ok=False).stderr)
        pr['labels'] = [{'name': 'release:fast'}]
        pr['isCrossRepository'] = True
        self.data(pr=pr)
        self.assertIn('pr_collision', self.cli('resume', ok=False).stderr)
        self.assertNotIn('merge', self.data()['effects'])

    def test_planned_metadata_symlink_refused_before_materialization(self):
        target = self.base / 'untouched'
        target.write_text('outside tree')
        manifest = self.source / 'Cargo.toml'
        manifest.unlink()
        manifest.symlink_to(target)
        self.commit('foreign metadata symlink')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.assertIn('planned_path', self.cli('plan', ok=False).stderr)
        self.assertEqual(target.read_text(), 'outside tree')
        self.assertFalse((self.state / 'candidate').exists())

    def test_permission_and_configuration_fail_before_validation(self):
        for field, message in (('deny', 'repository_permission'), ('no_secrets', 'runner_configuration'), ('no_label', 'release_label')):
            self.data(**{field: True})
            self.assertIn(message, self.cli('rc', ok=False).stderr)
            self.assertNotIn('metadata', self.data().get('effects', []))
            self.data(**{field: False})

    def test_local_gate_failure_and_version_collision_prevent_publication(self):
        self.git('--git-dir', self.remote, 'update-ref', 'refs/tags/v0.15.1-rc.4', self.initial)
        self.assertIn('version_conflict', self.cli('rc', '--version', '0.15.1-rc.4', ok=False).stderr)
        self.data(gate_fail=True)
        self.cli('rc', ok=False)
        self.assertNotIn('push', self.data().get('effects', []))
        self.assertEqual(self.state_value()['stage'], 'validation')
        self.data(gate_fail=False)
        self.cli('resume')

    def test_main_movement_and_rejected_checks(self):
        self.cli('plan')
        (self.source / 'new.txt').write_text('new main')
        self.commit('main moved')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.assertIn('stale_main', self.cli('rc', ok=False).stderr)
        self.git('--git-dir', self.remote, 'update-ref', 'refs/heads/main', self.initial)
        self.data(checks='fail')
        self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
        self.assertNotIn('merge', self.data()['effects'])
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.assertIn('stale_main', self.cli('resume', ok=False).stderr)
        self.git('--git-dir', self.remote, 'update-ref', 'refs/heads/main', self.initial)
        self.data(checks='pass')
        self.cli('resume')

    def test_duplicate_concurrent_invocation(self):
        self.cli('plan')
        process = subprocess.Popen([sys.executable, str(self.source / 'dev'), 'release', 'rc',
                    '--repo', 'o/r', '--remote', str(self.remote), '--state', str(self.state)],
                    env={**self.env, 'FAKE_DELAY': '.15'}, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            import time
            time.sleep(.1)
            result = self.cli('rc', ok=False)
            self.assertIn('concurrent_release', result.stderr)
        finally:
            _, err = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, err)

    def test_publication_effects_reconcile_without_clobber(self):
        self.cli('rc')
        state = self.state_value()
        self.publish_assets(state)
        originals = self.base / 'assets'
        for effect in ('create-release', 'upload', 'publish'):
            for when in ('before', 'after'):
                with self.subTest(effect=effect, when=when):
                    runner = self.base / (effect + '-' + when)
                    runner.mkdir()
                    shutil.copytree(originals, runner / 'dist')
                    for name in ('SHA256SUMS', 'SHA256SUMS.minisig'):
                        shutil.copyfile(originals / name, runner / name)
                        (runner / 'dist' / name).unlink()
                    (runner / 'install.sh').write_text('fixture installer')
                    (runner / 'notes.md').write_text('reviewed notes')
                    (runner / 'scripts').mkdir()
                    shutil.copyfile(ROOT / 'scripts/release-policy.json', runner / 'scripts/release-policy.json')
                    (runner / 'crates/af/keys').mkdir(parents=True)
                    shutil.copyfile(ROOT / 'crates/af/keys/release.pub', runner / 'crates/af/keys/release.pub')
                    remote_assets = runner / 'remote-assets'
                    remote_assets.mkdir()
                    self.data(releases=[], assets=str(remote_assets), crash=effect + ':' + when, effects=[])
                    command = [sys.executable, '-I', str(ROOT / 'scripts/release-publish.py'),
                               'v0.15.1-rc.4', state['candidate']]
                    env = {**self.env, 'GH_REPO': 'o/r'}
                    first = subprocess.run(command, cwd=runner, env=env, capture_output=True, text=True)
                    self.assertNotEqual(first.returncode, 0, first.stdout)
                    second = subprocess.run(command, cwd=runner, env=env, capture_output=True, text=True)
                    self.assertEqual(second.returncode, 0, second.stderr)
                    data = self.data()
                    self.assertEqual(data['effects'].count('create-release'), 1)
                    self.assertEqual(data['effects'].count('publish'), 1)
                    self.assertFalse(data['releases'][0]['draft'])
                    self.assertEqual(len({a['name'] for a in data['releases'][0]['assets']}), 7)
                    third = subprocess.run(command, cwd=runner, env=env, capture_output=True, text=True)
                    self.assertEqual(third.returncode, 0, third.stderr)
                    self.assertEqual(self.data()['effects'].count('upload'), 7)

    def test_draft_incomplete_wrong_source_and_signature_refusals(self):
        self.cli('rc')
        state = self.state_value()
        self.publish_assets(state)
        data = self.data()
        data['releases'][0]['draft'] = True
        self.data(releases=data['releases'])
        self.cli('resume')
        self.assertFalse(self.state_value().get('published', False))
        data['releases'][0]['draft'] = False
        assets = data['releases'][0]['assets']
        data['releases'][0]['assets'] = []
        self.data(releases=data['releases'])
        self.assertIn('incomplete_artifacts', self.cli('resume', ok=False).stderr)
        data['releases'][0]['assets'] = assets
        self.data(releases=data['releases'], bad_signature=True)
        self.cli('resume', ok=False)
        self.data(bad_signature=False, crash='download:after')
        self.cli('resume', ok=False)
        self.cli('resume')
        provenance = self.base / 'assets/release-provenance.json'
        document = json.loads(provenance.read_text())
        document['source'] = self.initial
        provenance.write_text(json.dumps(document))
        import hashlib
        sums = self.base / 'assets/SHA256SUMS'
        lines = sums.read_text().splitlines()
        sums.write_text('\n'.join(
            hashlib.sha256(provenance.read_bytes()).hexdigest() + '  release-provenance.json'
            if line.endswith('release-provenance.json') else line for line in lines) + '\n')
        self.assertIn('artifact_provenance', self.cli('resume', ok=False).stderr)
        self.assertFalse(self.state_value()['published'])
        self.git('--git-dir', self.remote, 'update-ref', 'refs/tags/v0.15.1-rc.4', self.initial)
        self.assertIn('tag_collision', self.cli('resume', ok=False).stderr)


if __name__ == '__main__':
    unittest.main()
