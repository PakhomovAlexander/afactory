#!/usr/bin/env python3
"""Hermetic subprocess release lifecycle against local Git and a fake GitHub server."""
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import signal
import unittest
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
GIT = shutil.which('git')
FAKE = r'''#!/usr/bin/env python3
import hashlib,json,os,subprocess,sys,time
from pathlib import Path
name=Path(sys.argv[0]).name
a=sys.argv[1:]
f=Path(os.environ['FAKE_DB']); d=json.loads(f.read_text()); ci_run_id=d.get('ci_run_id',10)
def save():
 temporary=Path(str(f)+'.'+str(os.getpid())+'.tmp');temporary.write_text(json.dumps(d));temporary.replace(f)
def crash(effect,when):
 if d.get('crash')==effect+':'+when:
  d['crash']=''; save(); sys.exit(55)
def effect(key,action):
 crash(key,'before'); action(); d.setdefault('effects',[]).append(key); save(); crash(key,'after')
def out(value): print(json.dumps(value)); sys.exit(0)
if name=='git':
 if 'fetch' in a and 'origin' in a: time.sleep(float(os.environ.get('FAKE_GIT_FETCH_DELAY','0')))
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
Path(str(f)+'.controller-'+str(os.getppid())).touch()
if os.environ.get('FAKE_DELAY'): time.sleep(float(os.environ['FAKE_DELAY']))
if a[0]=='api':
 request=a[1].replace('{owner}/{repo}','o/r')
 d.setdefault('api_requests',[]).append(request);save()
 if request=='repos/o/r/':
  print('HTTP 404: malformed repository endpoint',file=sys.stderr);sys.exit(1)
 if request=='repos/o/r': out({'full_name':'o/r','permissions':{'push':not d.get('deny')}})
 endpoint=request.split('repos/o/r/',1)[1]
 if endpoint.startswith('actions/secrets'):
  rows=d.get('secret_rows',[{'name':n} for n in ([] if d.get('no_secrets') else (['MINISIGN_SECRET_KEY'] if d.get('no_pin') else ['MINISIGN_SECRET_KEY','RELEASE_PIN_TOKEN']))])
  page=int(endpoint.split('page=')[-1]);batch=rows[(page-1)*100:page*100]
  if d.get('secret_duplicate') and page==2: batch=[rows[0]]
  if d.get('secret_missing_page') and page==2: sys.exit(1)
  if d.get('secret_partial') and page==1: batch=batch[:30]
  response={'total_count':len(rows),'secrets':batch}
  if d.get('secret_missing_total'): response.pop('total_count')
  if d.get('secret_changing_total') and page==2: response['total_count']+=1
  out(response)
 if endpoint.startswith('labels'): out([] if d.get('no_label') else [{'name':'release:fast'},{'name':'release:full'}])
 if endpoint.startswith('releases/'):
  out(next(r for r in d['releases'] if str(r['id'])==endpoint.split('/')[1]))
 if endpoint.startswith('releases'):
  rows=d.get('releases',[]);page=int(endpoint.split('page=')[-1]);batch=rows[(page-1)*100:page*100]
  if d.get('release_missing_page') and page==2: sys.exit(1)
  if d.get('release_duplicate') and page==2: batch=[rows[0]]
  if d.get('release_malformed') and page==2: batch=[{'tag_name':'v2.0.0'}]
  if d.get('release_partial') and page==1: batch=batch[:30]
  if '--include' in a:
   print('HTTP/2.0 200 OK')
   links=[]
   if not d.get('release_no_link'):
    if page*100<len(rows): links.append('<https://api.github.com/repos/o/r/releases?per_page=100&page='+str(page+1)+'>; rel="next"')
    if page>1: links.append('<https://api.github.com/repos/o/r/releases?per_page=100&page='+str(page-1)+'>; rel="prev"')
    if page*100<len(rows): links.append('<https://api.github.com/repos/o/r/releases?per_page=100&page='+str((len(rows)+99)//100)+'>; rel="last"')
    if page>1: links.append('<https://api.github.com/repos/o/r/releases?per_page=100&page=1>; rel="first"')
    if page==2 and d.get('release_omit_later_next'): links=[link for link in links if 'rel="next"' not in link and 'rel="last"' not in link]
   if links: print('Link: '+', '.join(links))
   print()
  if d.get('release_change_after_read'):
   batch=[dict(row) for row in batch]
   d['releases'][0]['draft']=not d['releases'][0].get('draft',False)
   d['release_change_after_read']=False;save()
  out(batch)
 if endpoint.startswith('rules/branches/main'): out([] if d.get('no_strict') else [{'type':'required_status_checks','ruleset_id':1,'ruleset_source_type':'Repository','parameters':{'strict_required_status_checks_policy':True,'required_status_checks':[{'context':'validation / lint'}]}}])
 if endpoint=='rulesets/1': out({'enforcement':'active','bypass_actors':[{}] if d.get('bypass') else []})
 if endpoint=='actions/workflows/release.yml': out({'id':339974639,'path':'.github/workflows/release.yml'})
 if endpoint=='actions/workflows/ci.yml': out({'id':339965080,'state':'disabled_manually' if d.get('ci_disabled') else 'active','path':'.github/workflows/ci.yml'})
 if endpoint.startswith('actions/artifacts?'):
  rows=d.get('bundles',[]);out({'total_count':len(rows),'artifacts':rows})
 if endpoint=='actions/runs/1': out({'workflow_id':d.get('bundle_workflow_id',339974639),'head_sha':d['merged'],'path':d.get('bundle_path','.github/workflows/release.yml'),'event':'push'})
 if (endpoint.startswith(('actions/workflows/ci.yml/runs','actions/runs/10','actions/runs/20','git/commits/')) or (endpoint.startswith('actions/artifacts/') and endpoint.endswith('/zip'))):
  import importlib.util,io,zipfile
  observed_run_id=ci_run_id
  archive_attempt=None
  if endpoint.startswith('actions/runs/'): observed_run_id=int(endpoint.split('/')[2])
  if endpoint.startswith('actions/artifacts/'):
   artifact_id=int(endpoint.split('/')[2]);observed_run_id=artifact_id//1000;archive_attempt=artifact_id%1000

  pr=d['pr'];head=pr['headRefOid'];base=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'rev-parse',head+'^'],text=True).strip()
  tree=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'rev-parse',head+'^{tree}'],text=True).strip()
  if not d.get('ci_merge'):
   env={**os.environ,'GIT_AUTHOR_NAME':'Test','GIT_AUTHOR_EMAIL':'test@example.invalid','GIT_COMMITTER_NAME':'Test','GIT_COMMITTER_EMAIL':'test@example.invalid'}
   d['ci_merge']=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'commit-tree',tree,'-p',base,'-p',head,'-m','test merge'],env=env,text=True).strip();save()
  spec=importlib.util.spec_from_file_location('classes',Path(d['source'])/'scripts/test-classes.py');resolver=importlib.util.module_from_spec(spec);spec.loader.exec_module(resolver)
  labels=[] if observed_run_id==10 and d.get('ci_prior_run') else d.get('ci_plan_labels',[x['name'] for x in pr['labels']])
  plan=resolver.resolve(labels,'release_pr',d['ci_merge'],d['pr_branch'][len('release/v'):]);plan.update(d.get('ci_plan_override',{}))
  if endpoint.startswith('git/commits/'):
   row={'sha':d['ci_merge'],'tree':{'sha':tree},'parents':[{'sha':base},{'sha':head}]};row.update(d.get('ci_commit_override',{}));out(row)
  attempt=archive_attempt or (1 if observed_run_id==10 and d.get('ci_prior_run') else d.get('ci_attempt',1))
  buf=io.BytesIO()
  with zipfile.ZipFile(buf,'w') as z:
   info=zipfile.ZipInfo('plan.json');z.writestr(info,json.dumps(plan))
  archive=buf.getvalue()
  if endpoint.startswith('actions/artifacts/'): sys.stdout.buffer.write(archive);sys.exit(0)
  if '/artifacts?' in endpoint:
   rows=[] if d.get('ci_plan_missing') else [{'id':observed_run_id*1000+d.get('artifact_attempt',attempt),'name':'validation-plan-'+str(d.get('artifact_attempt',attempt)),'expired':False,'digest':(d.get('plan_digest') or 'sha256:'+hashlib.sha256(archive).hexdigest()),'workflow_run':{'id':observed_run_id,'head_sha':head}}]
   out({'total_count':len(rows)+(1 if d.get('ci_artifacts_partial_page') else 0),'artifacts':rows})
  state='pass' if observed_run_id==10 and d.get('ci_superseded') else d.get('checks','pass')
  status='in_progress' if state=='pending' else 'completed'
  row={'id':observed_run_id,'workflow_id':339965080,'run_attempt':attempt,'head_sha':head,'head_branch':d['pr_branch'],'path':'.github/workflows/ci.yml','event':'pull_request','status':status,'conclusion':{'pass':'success','fail':'failure','cancel':'cancelled'}.get(state),'pull_requests':[{'number':pr['number'],'head':{'sha':head},'base':{'ref':'main','sha':base}}]}
  row.update(d.get('ci_override',{}))
  if endpoint=='actions/runs/'+str(observed_run_id):
   if d.get('rerun_during_read'): row['run_attempt']+=1
   out(row)
  if endpoint.startswith('actions/workflows/ci.yml/runs'):
   rows=[] if d.get('ci_missing') else ([{**row,'id':10,'run_attempt':1}] if d.get('ci_prior_run') else [])+[row];out({'total_count':len(rows)+(1 if d.get('ci_runs_partial_page') else 0),'workflow_runs':rows})
  names=['validation / resolve','validation / lint']
  for os_name in ('ubuntu-latest','macos-latest'):
   names += ['validation / '+n+' ('+os_name+')' for n in ['base','check']+['class '+c for c in plan['selected']]]
  hosted=resolver.catalog()['hosted_checks'];names += ['validation / '+n for n in hosted]
  jobs=[{'id':i+1,'name':n,'head_sha':head,'run_id':observed_run_id,'run_attempt':(d.get('job_attempt') if d.get('job_attempt') is not None else attempt),'status':'completed','conclusion':'skipped' if n.removeprefix('validation / ') in hosted and hosted[n.removeprefix('validation / ')] not in plan['selected'] else 'success'} for i,n in enumerate(names)]
  if d.get('job_attempt') is None:
   for j in jobs: j.pop('run_attempt') # Documented REST shape binds attempt via endpoint.
  if d.get('ci_duplicate'): jobs.append({**jobs[0],'id':999})
  if d.get('ci_partial'): jobs.pop(0)
  jobs=[j for j in jobs if j['name']!=d.get('missing_job')]
  if d.get('ci_skipped'): jobs[0]['conclusion']='skipped'
  if d.get('skip_job'):
   for j in jobs:
    if j['name']==d['skip_job']: j['conclusion']='skipped'
  if d.get('ci_paginated'):
   # More than one page, with the mandatory telemetry-runtime job on the last page.
   jobs=jobs[:-1]+[{**jobs[0],'id':100+i,'name':'foreign '+str(i)} for i in range(100-len(jobs)+1)]+jobs[-1:]
   page=int(endpoint.split('page=')[-1]);out({'total_count':len(jobs),'jobs':jobs[(page-1)*100:page*100]})
  if d.get('new_run_during_read'):
   d.update(ci_run_id=20,ci_prior_run=True,ci_superseded=True,checks=d.pop('new_run_during_read'));save()
  out({'total_count':len(jobs)+(1 if d.get('ci_partial_page') else 0),'jobs':jobs})
 if endpoint.startswith('actions/workflows'): out({'workflow_runs':[{'id':1,'head_sha':d['merged'],'status':d.get('workflow_status','in_progress'),'conclusion':'failure'}]})
 if endpoint.startswith('actions/runs'): out({'jobs':d.get('jobs',[{'name':'sign and publish','conclusion':'success'},{'name':'pin the release in .af/af.lock','conclusion':'failure'}])})
if a[:2]==['run','download']:
 import shutil
 shutil.copytree(d['bundle_dir'],a[a.index('--dir')+1],dirs_exist_ok=True);sys.exit(0)
if a[:2]==['pr','list']:
 head=a[a.index('--head')+1]
 if head.startswith('release/pin-'): out(d.get('pins',[]))
 if d.get('pause_after_validation') and d.get('gate_runs'):
  d['paused']=True;save();time.sleep(30)
 out([d['pr']] if d.get('pr') and d.get('pr_branch') == head else [])
if a[:2]==['pr','create']:
 def create():
  head=a[a.index('--head')+1]
  sha=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'rev-parse','refs/heads/'+head],text=True).strip()
  d['pr_branch']=head
  d['pr']={'number':1,'url':'https://example.test/pr/1','state':'OPEN','headRefOid':sha,'baseRefName':'main','mergeCommit':None,'isCrossRepository':False,'labels':[{'name':a[a.index('--label')+1]}]}
 effect('pr',create);print('https://example.test/pr/1');sys.exit(0)
if a[:2]==['pr','checks']:
 if d.get('formal_checks') is None:
  print('no required checks reported on the release branch',file=sys.stderr);sys.exit(1)
 out([{'name':'gate','bucket':d['formal_checks'],'link':'https://example.test/check'}])
if a[:2]==['pr','merge']:
 def merge():
  head=d['pr']['headRefOid']
  base=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'rev-parse',head+'^'],text=True).strip()
  if d.get('race_merge'):
   env={**os.environ,'GIT_AUTHOR_NAME':'Test','GIT_AUTHOR_EMAIL':'test@example.invalid','GIT_COMMITTER_NAME':'Test','GIT_COMMITTER_EMAIL':'test@example.invalid'}
   tree=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'rev-parse',base+'^{tree}'],text=True).strip()
   moved=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'commit-tree',tree,'-p',base,'-m','concurrent main'],env=env,text=True).strip()
   subprocess.check_call([os.environ['REAL_GIT'],'--git-dir',d['remote'],'update-ref','refs/heads/main',moved])
  main=subprocess.check_output([os.environ['REAL_GIT'],'--git-dir',d['remote'],'rev-parse','refs/heads/main'],text=True).strip()
  if main!=base: sys.exit(1) # server-enforced strict freshness
  if d.get('defer_merge'): return
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
  next(r for r in d['releases'] if r['tag_name']==a[2])['assets'].append({'name':path.name})
 effect('upload',upload);sys.exit(0)
if a[:2]==['release','edit']:
 def publish(): next(r for r in d['releases'] if r['tag_name']==a[2])['draft']=False
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
        self.db.write_text(json.dumps({'remote': str(self.remote), 'source': str(self.source)}))
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
                     'scripts/release-observation.py', 'scripts/af-pin.py', '.af/af.lock', '.github/workflows/release.yml',
                     'scripts/changelog-notes.py', 'crates/af/keys/release.pub',
                     'CHANGELOG.md', 'rust-toolchain.toml', '.github/workflows/ci.yml',
                     '.github/workflows/validate.yml', 'scripts/test-classes.py',
                     '.config/test-classes.json'):
            target = self.source / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)
        for name in ('schemas', 'scripts/performance/perflab'):
            # Copy bytes, not the sealed source's read-only file modes: these
            # disposable fixture schemas are intentionally mutated below.
            shutil.copytree(ROOT / name, self.source / name, copy_function=shutil.copyfile)
        shutil.copyfile(ROOT / 'scripts/telemetry-overhead.py', self.source / 'scripts/telemetry-overhead.py')
        for fixture in (ROOT / 'fixtures').glob('telemetry-bench'):
            shutil.copytree(fixture, self.source / 'fixtures' / fixture.name)
        # copytree preserves directory modes even with copyfile. Only this
        # disposable copy may be made writable/searchable for fixture mutations.
        for directory in (self.source, *self.source.rglob('*')):
            if directory.is_dir():
                directory.chmod(directory.stat().st_mode | stat.S_IRWXU)
        policy = json.loads((ROOT / 'scripts/release-policy.json').read_text())
        for name in policy['required_features']:
            path = self.source / name
            if not path.exists():
                path.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, path)
        (self.source / 'changelog.d').mkdir()
        (self.source / 'changelog.d/README.md').write_text('Fragment instructions.\n')
        (self.source / 'changelog.d/note.md').write_text('- A reviewed feature.\n')
        (self.source / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.15.1-rc.3"\n')
        declaration_path = self.source / 'scripts/release-contract.json'
        declaration = json.loads(declaration_path.read_text())
        declaration['version'] = '0.15.1-rc.3'
        declaration_path.write_text(json.dumps(declaration))
        (self.source / 'CHANGELOG.md').write_text('# Changelog\n\n## [Unreleased]\n\n## [0.15.1-rc.3] - today\n\n## [0.15.1-rc.2] - today\n')
        (self.source / '.af/af.lock').write_text('[af]\nversion = "0.15.1-rc.3"\n\n[af.digests]\n' + ''.join(
            target + ' = "sha256:' + 'a' * 64 + '"\n' for target in policy['targets']))
        (self.source / 'Cargo.lock').write_text('# fixture lock\n')
        harness = self.source / 'scripts/performance/cli.py'
        harness.write_text('import pathlib,sys\np=pathlib.Path("fixtures/performance/kernel-measure/pin")\n'
                           'if "--write" in sys.argv:\n p.parent.mkdir(parents=True,exist_ok=True);p.write_text("pin")\n')
        verify = self.source / 'scripts/verify.sh'
        verify.write_text('#!' + sys.executable + '\nimport json,os,sys\n'
                          'p=os.environ["FAKE_DB"];d=json.load(open(p));'
                          'd["gate_runs"]=d.get("gate_runs",0)+1;json.dump(d,open(p,"w"));'
                          'import subprocess\n'
                          'subprocess.run([sys.executable,"-I","scripts/release-contract.py"],check=True)\n'
                          'sys.exit(9 if d.get("gate_fail") else 0)\n')
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
        if changes:
            value.update(changes)
            temporary = self.db.with_suffix('.tmp')
            temporary.write_text(json.dumps(value))
            temporary.replace(self.db)
        return value

    def cli(self, command, *args, ok=True, extra_env=None, drive=False):
        result = subprocess.run([sys.executable, str(self.source / 'dev'), 'release', command,
                                 '--repo', 'o/r', '--remote', str(self.remote), '--state', str(self.state),
                                 '--format', 'json', *([] if drive or command not in ('rc', 'resume') else ['--step']), *args], cwd=self.source,
                                env={**self.env, **(extra_env or {})}, text=True, capture_output=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr + self.data().get('last_git_error',''))
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def state_value(self):
        return json.loads((self.state / 'run.json').read_text())

    def start_driver(self, command='rc', timeout=30, profile=None):
        process = subprocess.Popen([sys.executable, str(self.source / 'dev'), 'release', command,
                    '--repo', 'o/r', '--remote', str(self.remote), '--state', str(self.state),
                    '--format', 'json', '--poll-interval', '1', '--timeout', str(timeout),
                    *(['--profile', profile] if profile else [])],
                    cwd=self.source, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    text=True)
        def cleanup():
            if process.poll() is None:
                process.kill()
            process.communicate()
        self.addCleanup(cleanup)
        return process

    def await_stage(self, process, stage):
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            if process.poll() is not None:
                self.fail('driver exited early: ' + str(process.communicate()))
            if (self.state / 'run.json').exists():
                state = self.state_value()
                if (state['stage'] == stage and state['status'] == 'waiting'
                        and Path(str(self.db) + '.controller-' + str(process.pid)).exists()):
                    return state
            time.sleep(.05)
        self.fail('driver did not reach waiting ' + stage)

    def test_one_real_invocation_drives_checks_merge_and_signed_publication(self):
        self.data(checks='pending')
        process = self.start_driver()
        state = self.await_stage(process, 'checks')
        identity = state['identity']
        # Read-only status observes the running driver without acquiring its lease.
        before = (self.state / 'run.json').read_bytes()
        self.cli('status')
        self.assertEqual((self.state / 'run.json').read_bytes(), before)
        self.data(checks='pass', defer_merge=True)
        state = self.await_stage(process, 'merge')
        time.sleep(2)
        self.assertEqual(self.data()['effects'].count('merge'), 1)
        data = self.data()
        pr = data['pr']
        pr.update(state='MERGED', mergeCommit={'oid': state['candidate']})
        self.data(pr=pr, merged=state['candidate'], defer_merge=False)
        self.git('--git-dir', self.remote, 'update-ref', 'refs/heads/main', state['candidate'])
        state = self.await_stage(process, 'publication')
        self.data(jobs=[{'name': 'sign and publish', 'conclusion': None}])
        self.publish_assets(state)
        time.sleep(2)
        self.assertIsNone(process.poll())
        self.assertNotIn('download', self.data()['effects'])
        self.data(jobs=[{'name': 'sign and publish', 'conclusion': 'success'},
                        {'name': 'pin the release in .af/af.lock', 'conclusion': 'failure'}])
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 0, err)
        ready = json.loads(out)
        self.assertEqual(ready['identity'], identity)
        self.assertEqual(ready['status'], 'ready')
        self.assertTrue(ready['published'])
        self.assertEqual(ready['pin']['state'], 'PENDING')
        self.assertEqual(self.data()['effects'].count('pr'), 1)
        self.assertEqual(self.data()['effects'].count('merge'), 1)
        self.assertEqual(self.data()['gate_runs'], 1)

    def test_one_full_invocation_waits_for_labeled_plan_after_opened_fast(self):
        self.data(ci_plan_labels=[])
        process = self.start_driver(profile='full', timeout=60)
        self.await_stage(process, 'checks')
        time.sleep(2)
        self.assertNotIn('merge', self.data()['effects'])
        self.data(ci_plan_labels=['release:full'], ci_plan_missing=True, ci_run_id=20, ci_prior_run=True)
        time.sleep(2)
        self.assertNotIn('merge', self.data()['effects'])
        self.data(ci_plan_missing=False)
        self.publish_assets(self.await_stage(process, 'publication'))
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 0, err)
        ready = json.loads(out)
        self.assertTrue(ready['published'])
        self.assertEqual(ready['receipts']['checks']['checks'][0]['plan']['profile'], 'full')
        self.assertEqual(ready['receipts']['checks']['checks'][0]['run_id'], 20)
        self.assertEqual(self.data()['effects'].count('pr'), 1)
        self.assertEqual(self.data()['effects'].count('merge'), 1)
        self.cli('resume')
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_timeout_and_interrupt_resume_same_authorized_run(self):
        self.data(checks='pending')
        self.cli('rc')
        process = self.start_driver('resume', timeout=8)
        self.await_stage(process, 'checks')
        _, err = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 124, err)
        state = self.state_value()
        self.assertEqual(state['status'], 'waiting')
        self.assertNotIn('failure', state)
        self.assertIn('driver_timeout', state['receipts']['checks']['next'])
        identity = state['identity']
        process = self.start_driver('resume')
        self.await_stage(process, 'checks')
        process.send_signal(signal.SIGTERM)
        _, err = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 130, err)
        self.assertIn('operator_interrupted', self.state_value()['receipts']['checks']['next'])
        self.assertEqual(self.state_value()['status'], 'waiting')
        self.assertNotIn('failure', self.state_value())
        self.data(checks='pass')
        process = self.start_driver('resume')
        self.publish_assets(self.await_stage(process, 'publication'))
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 0, err)
        self.assertEqual(json.loads(out)['identity'], identity)
        self.assertEqual(self.data()['gate_runs'], 1)
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_interrupt_between_stages_keeps_successful_validation_receipt(self):
        self.data(pause_after_validation=True)
        process = self.start_driver()
        deadline = time.monotonic() + 25
        while not self.data().get('paused') and time.monotonic() < deadline:
            self.assertIsNone(process.poll())
            time.sleep(.05)
        self.assertTrue(self.data().get('paused'))
        self.assertEqual(self.state_value()['receipts']['validation']['status'], 'complete')
        process.send_signal(signal.SIGINT)
        _, err = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 130, err)
        self.assertEqual(self.state_value()['status'], 'waiting')
        self.assertEqual(self.state_value()['receipts']['validation']['status'], 'complete')
        self.data(pause_after_validation=False)
        process = self.start_driver('resume')
        self.publish_assets(self.await_stage(process, 'publication'))
        _, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 0, err)
        self.assertEqual(self.data()['gate_runs'], 1)

    def test_default_driver_stops_for_validation_and_release_workflow_failures(self):
        self.data(gate_fail=True)
        result = self.cli('rc', '--timeout', '30', drive=True, ok=False)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(self.state_value()['stage'], 'validation')
        self.assertNotIn('push', self.data().get('effects', []))
        self.data(gate_fail=False, checks='fail')
        result = self.cli('resume', '--timeout', '30', drive=True, ok=False)
        self.assertIn('checks_rejected', result.stderr)
        self.data(checks='pass', jobs=[{'name': 'build linux', 'conclusion': 'failure'},
                                     {'name': 'sign and publish', 'conclusion': 'skipped'}])
        result = self.cli('resume', '--timeout', '30', drive=True, ok=False)
        self.assertIn('release_workflow_failed: build linux', result.stderr)
        # Failure is detected even before the tag or release exists.
        self.assertEqual(self.state_value()['status'], 'failed')
        self.assertFalse(self.state_value()['published'])
        self.data(jobs=[{'name': 'sign and publish', 'conclusion': 'skipped'}],
                  workflow_status='completed')
        self.assertIn('no successful publication join',
                      self.cli('resume', drive=True, ok=False).stderr)
        self.publish_assets(self.state_value())
        self.data(jobs=[{'name': 'sign and publish', 'conclusion': 'success'},
                        {'name': 'pin the release in .af/af.lock', 'conclusion': 'failure'}])
        ready = json.loads(self.cli('resume', drive=True).stdout)
        self.assertEqual(ready['status'], 'ready')
        self.assertNotIn('failure', ready)
        self.assertEqual(ready['pin']['state'], 'PENDING')

    def test_state_schema_parity_and_unknown_fields_fail_closed(self):
        import runpy
        module = runpy.run_path(str(ROOT / 'scripts/dev-release.py'))
        self.assertEqual(module['state_schema'](),
                         json.loads((ROOT / 'schemas/dev-release-run-v2.json').read_text()))
        self.cli('plan')
        path = self.state / 'run.json'
        state = json.loads(path.read_text())
        original_policy = state['plan']['policy']['schema']
        state['plan']['policy']['schema'] = 'af.dev-release-policy/2'
        state['identity'] = module['digest'](state['plan'])
        path.write_text(json.dumps(state))
        self.assertIn('foreign_state', self.cli('status', ok=False).stderr)
        state['plan']['policy']['schema'] = original_policy
        state['identity'] = module['digest'](state['plan'])
        state['schema'] = 'af.dev-release-run/1'
        path.write_text(json.dumps(state))
        self.assertIn('foreign_state', self.cli('resume', ok=False).stderr)
        state['schema'] = module['SCHEMA']
        state['unknown'] = True
        path.write_text(json.dumps(state))
        self.assertIn('foreign_state', self.cli('resume', ok=False).stderr)

    def test_initialization_refuses_remote_failure_even_with_local_cache(self):
        self.git('-C', self.source, 'remote', 'add', 'origin', 'git@github.com:o/r.git')
        shutil.rmtree(self.remote)
        result = self.cli('plan', ok=False)
        self.assertIn('local object cache acquired', result.stderr)
        self.assertIn('source_acquisition_failed', result.stderr)
        self.assertIn('retry 2/2', result.stderr)
        self.assertFalse((self.state / 'run.json').exists())
        self.assertFalse((self.state / 'cache-acquisition').exists())
        self.assertEqual(self.data().get('effects', []), [])

    def test_initialization_timeout_status_and_progress_without_a_plan(self):
        self.env['FAKE_GIT_FETCH_DELAY'] = '10'
        process = self.start_driver(timeout=3)
        deadline = time.monotonic() + 10
        while not (self.state / 'lock').exists() and process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.02)
        status = self.cli('status')
        self.assertEqual(json.loads(status.stdout)['status'], 'initializing')
        self.assertFalse((self.state / 'run.json').exists())
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 124, stdout + stderr)
        self.assertIn('authoritative main/tag fetch started', stderr)
        self.assertNotIn('authoritative main/tag fetch complete', stderr)
        self.assertIn('no plan or authorization retained', stderr)
        self.assertFalse((self.state / 'cache-acquisition').exists())
        self.assertEqual(json.loads(self.cli('status').stdout)['status'], 'missing_run')

    def test_normal_initialization_automatically_seeds_and_planned_run_never_reseeds(self):
        self.git('-C', self.source, 'remote', 'add', 'origin', 'https://github.com/o/r.git')
        self.git('-C', self.source, 'tag', 'v999.0.0')
        (self.source / 'Cargo.toml').write_text('dirty caller')
        result = self.cli('plan')
        self.assertIn('local object cache acquired', result.stderr)
        self.assertIn('authoritative main/tag fetch started', result.stderr)
        self.assertIn('authoritative main/tag fetch complete', result.stderr)
        mirror = self.state / 'repository.git'
        self.assertEqual(self.git('--git-dir', mirror, 'rev-parse', 'refs/cache/caller-head'), self.initial)
        self.assertEqual(self.state_value()['plan']['version'], '0.15.1-rc.4')
        self.assertEqual(self.state_value()['plan']['source'], self.initial)
        self.assertEqual((self.source / 'Cargo.toml').read_text(), 'dirty caller')
        self.assertNotIn('cache acquisition', self.cli('plan').stderr)

    def test_plan_uses_canonical_repository_and_preserves_nested_queries(self):
        result = self.cli('plan')
        self.assertIn('authoritative main/tag fetch complete', result.stderr)
        state = json.loads(result.stdout)
        self.assertEqual(state['plan']['source'], self.initial)
        self.assertIsNone(state['authorization'])
        self.assertEqual(state['receipts']['preflight']['status'], 'planned')
        data = self.data()
        requests = data['api_requests']
        self.assertEqual(requests[0], 'repos/o/r')
        self.assertEqual(requests.count('repos/o/r'), 1)
        self.assertNotIn('repos/o/r/', requests)
        for endpoint in ('actions/secrets?per_page=100&page=1',
                         'rules/branches/main?per_page=100&page=1',
                         'rulesets/1', 'actions/workflows/ci.yml',
                         'labels?per_page=100', 'releases?per_page=100&page=1'):
            with self.subTest(endpoint=endpoint):
                self.assertIn('repos/o/r/' + endpoint, requests)
        self.assertEqual(data.get('effects', []), [])

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
        self.data(assets=str(assets), releases=[{'id':1, 'tag_name': 'v0.15.1-rc.4', 'draft': False,
                  'html_url': 'https://example.test/release', 'assets': [{'name': n} for n in
                  [*names, 'SHA256SUMS', 'SHA256SUMS.minisig']]}])

    def test_next_cut_requires_explicit_new_and_keeps_terminal_receipt(self):
        self.cli('rc')
        self.assertIn('unfinished_run', self.cli('plan', '--new', ok=False).stderr)
        self.publish_assets(self.state_value())
        self.cli('resume')
        # A separate reviewed pin PR, not publication, supplies the next prerequisite.
        self.git('-C', self.source, 'fetch', str(self.remote), 'main')
        self.git('-C', self.source, 'reset', '--hard', 'FETCH_HEAD')
        lock = self.source / '.af/af.lock'
        lock.write_text(lock.read_text().replace('0.15.1-rc.3', '0.15.1-rc.4'))
        self.commit('reviewed RC4 pin')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.cli('plan', '--new')
        self.assertEqual(self.state_value()['plan']['version'], '0.15.1-rc.5')
        self.assertEqual(len(list((self.state / 'history').glob('0.15.1-rc.4-*.json'))), 1)
        self.assertFalse((self.state / 'candidate').exists())

    def test_prospective_pin_fails_before_gate_or_authority_from_fetched_bytes(self):
        path = self.source / '.af/af.lock'
        correct = path.read_text()
        path.write_text(correct.replace('0.15.1-rc.3', '0.15.1-rc.2'))
        self.commit('old pin')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        path.write_text(correct)
        (self.source / 'scripts/af-pin.py').write_text('raise SystemExit(0)\n')
        result = self.cli('rc', ok=False)
        self.assertIn('pin_prerequisite', result.stderr)
        self.assertIn('separate reviewed published-release pin PR', result.stderr)
        self.assertFalse((self.state / 'run.json').exists())
        self.assertFalse((self.state / 'candidate').exists())
        self.assertEqual(self.data().get('gate_runs', 0), 0)
        self.assertFalse(self.data().get('effects'))

    def test_prerequisite_diagnostics_are_bounded_and_redacted(self):
        (self.source / 'scripts/af-pin.py').write_text(
            'import sys\nsys.stderr.write("credential=secret-fixture\\n" * 1000)\nraise SystemExit(1)\n')
        self.commit('failing source checker')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        result = self.cli('rc', ok=False)
        self.assertIn('pin_prerequisite', result.stderr)
        self.assertNotIn('secret-fixture', result.stderr)
        self.assertLess(len(result.stderr), 2048)
        self.assertFalse((self.state / 'run.json').exists())
        self.assertEqual(self.data().get('gate_runs', 0), 0)
        self.assertFalse(self.data().get('effects'))

    def test_source_pin_symlink_cannot_borrow_caller_evidence(self):
        path = self.source / '.af/af.lock'
        external = self.base / 'caller.lock'
        external.write_bytes(path.read_bytes())
        path.unlink()
        path.symlink_to(external)
        self.commit('redirected pin')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.assertIn('pin_prerequisite', self.cli('rc', ok=False).stderr)
        self.assertFalse((self.state / 'run.json').exists())
        self.assertFalse(self.data().get('effects'))

    def test_prospective_yanked_and_missing_digest_through_cli(self):
        path = self.source / '.af/af.lock'
        path.write_text(path.read_text().replace('0.15.1-rc.3', '0.15.1-rc.2'))
        changelog = self.source / 'CHANGELOG.md'
        changelog.write_text(changelog.read_text().replace('[0.15.1-rc.3] - today',
                                                        '[0.15.1-rc.3] - today [YANKED]'))
        self.commit('yanked newest release')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.cli('plan')
        self.assertIsNone(self.state_value()['authorization'])
        self.assertEqual(self.data().get('gate_runs', 0), 0)
        self.state = self.base / 'missing-digest-state'
        path.write_text(path.read_text().replace('aarch64-apple-darwin =', 'unknown ='))
        self.commit('missing required digest')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.assertIn('pin_prerequisite', self.cli('plan', ok=False).stderr)
        self.assertFalse((self.state / 'run.json').exists())
        self.assertFalse(self.data().get('effects'))

    def test_failed_local_new_retains_exact_evidence_and_separate_authority(self):
        self.data(gate_fail=True)
        self.cli('rc', ok=False)
        original = (self.state / 'run.json').read_bytes()
        source = self.state_value()['plan']['source']
        (self.source / 'changelog.d/fix.md').write_text('- Corrected source.\n')
        self.commit('correct source')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.cli('rc', '--new', ok=False)
        second = (self.state / 'run.json').read_bytes()
        self.assertNotEqual(self.state_value()['plan']['source'], source)
        self.assertEqual(self.state_value()['authorization'], 'normal-release-pr-check-merge-and-signed-publication-v1')
        self.cli('plan', '--new')
        self.assertIsNone(self.state_value()['authorization'])
        self.assertCountEqual([p.read_bytes() for p in (self.state / 'history').iterdir()], [original, second])
        self.assertEqual(self.state_value()['plan']['version'], '0.15.1-rc.4')
        self.assertFalse((self.state / 'candidate').exists())
        self.assertNotIn('push', self.data().get('effects', []))

    def completed_local_failure(self):
        # A real completed validation followed by refusal before branch intent.
        self.cli('plan')
        self.git('--git-dir', self.remote, 'update-ref',
                 'refs/heads/release/v0.15.1-rc.4', self.initial)
        result = self.cli('rc', ok=False)
        self.assertIn('branch_collision', result.stderr)
        state = self.state_value()
        self.assertEqual(state['stage'], 'validation')
        self.assertEqual(state['receipts']['validation']['status'], 'complete')
        self.assertTrue(state['receipts']['validation']['commands'])
        return state

    def test_complete_local_validation_failure_can_be_archived(self):
        self.completed_local_failure()
        original = (self.state / 'run.json').read_bytes()
        self.git('--git-dir', self.remote, 'update-ref', '-d', 'refs/heads/release/v0.15.1-rc.4')
        self.cli('plan', '--new')
        self.assertEqual([p.read_bytes() for p in (self.state / 'history').iterdir()], [original])
        self.assertFalse((self.state / 'candidate').exists())
        self.assertIsNone(self.state_value()['authorization'])
        self.assertNotIn('push', self.data().get('effects', []))

    def test_impossible_completed_validation_refuses_without_cleanup(self):
        baseline = self.completed_local_failure()
        commands = baseline['receipts']['validation']['commands']
        cases = [[], commands[:-1], commands + commands[:1], list(reversed(commands)),
                 None, {}, [None], [{'argv': []}]]
        for field, value in (('argv', ['foreign-command']), ('exit', 1), ('exit', True),
                             ('stdout_sha256', 'malformed')):
            changed = copy.deepcopy(commands)
            changed[0][field] = value
            cases.append(changed)
        changed = copy.deepcopy(commands)
        gate = next(c for c in changed if c['argv'][0] == 'env')
        gate['argv'][1] = 'AF_GATE_TARGET_DIR=/foreign/target'
        cases.append(changed)
        for invalid in cases:
            with self.subTest(commands=invalid):
                state = copy.deepcopy(baseline)
                state['receipts']['validation']['commands'] = invalid
                path = self.state / 'run.json'
                path.write_text(json.dumps(state))
                before = path.read_bytes()
                effects = self.data().get('effects', [])
                self.cli('plan', '--new', ok=False)
                self.assertEqual(path.read_bytes(), before)
                self.assertEqual(self.data().get('effects', []), effects)
                self.assertFalse((self.state / 'history').exists())
                self.assertTrue((self.state / 'candidate').exists())

    def test_new_refuses_active_ambiguous_or_later_state_unchanged(self):
        self.data(gate_fail=True)
        self.cli('rc', ok=False)
        path = self.state / 'run.json'
        baseline = self.state_value()
        cases = [dict(status=status) for status in ('waiting', 'planned', 'intent', 'complete')]
        cases += [dict(stage='branch'), dict(published=False), dict(merged='a'*40),
                  dict(future_effect={}), dict(pin={'state': 'PENDING', 'url': 'https://example.test/pr'})]
        for stage in ('branch', 'pr', 'checks', 'merge', 'publication'):
            cases.append(dict(receipts={**baseline['receipts'], stage: {'status': 'intent', 'at': 1}}))
        cases.append(dict(receipts={**baseline['receipts'], 'validation':
            {**baseline['receipts']['validation'], 'url': 'https://example.test/pr'}}))
        cases.append(dict(receipts={**baseline['receipts'], 'preparation': {'status': 'complete', 'at': 1}}))
        cases.append(dict(receipts={**baseline['receipts'], 'validation': {'status': 'waiting', 'at': 1}}))
        cases.append(dict(receipts={**baseline['receipts'], 'validation': {
            'status': 'complete', 'at': 1, 'key': 'a'*64, 'commands': [{'publication': 'intent'}]}}))
        for change in cases:
            with self.subTest(change=change):
                path.write_text(json.dumps({**baseline, **change}))
                before = path.read_bytes()
                effects = self.data().get('effects', [])
                self.cli('plan', '--new', ok=False)
                self.assertEqual(path.read_bytes(), before)
                self.assertEqual(self.data().get('effects', []), effects)
                self.assertFalse((self.state / 'history').exists())
                self.assertTrue((self.state / 'candidate').exists())

    def test_new_refuses_foreign_receipt_before_reading(self):
        self.data(gate_fail=True)
        self.cli('rc', ok=False)
        path = self.state / 'run.json'
        before = path.read_bytes()
        path.unlink()
        os.mkfifo(path, 0o600)
        result = subprocess.run([sys.executable, str(self.source / 'dev'), 'release', 'plan',
            '--new', '--repo', 'o/r', '--remote', str(self.remote), '--state', str(self.state)],
            cwd=self.source, env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertTrue(stat.S_ISFIFO(path.lstat().st_mode))
        self.assertFalse((self.state / 'history').exists())
        path.unlink()
        external = self.base / 'foreign-receipt.json'
        external.write_bytes(before)
        path.symlink_to(external)
        self.cli('plan', '--new', ok=False)
        self.assertEqual(external.read_bytes(), before)
        self.assertFalse((self.state / 'history').exists())
        self.assertNotIn('push', self.data().get('effects', []))

    def test_archive_collision_and_foreign_cleanup_fail_closed(self):
        self.data(gate_fail=True)
        self.cli('rc', ok=False)
        before = (self.state / 'run.json').read_bytes()
        history = self.state / 'history'
        history.mkdir(mode=0o700)
        archive = history / ('0.15.1-rc.4-' + hashlib.sha256(before).hexdigest() + '.json')
        archive.write_bytes(before + b' ')
        self.cli('plan', '--new', ok=False)
        self.assertEqual(archive.read_bytes(), before + b' ')
        self.assertEqual((self.state / 'run.json').read_bytes(), before)
        archive.unlink()
        history.rmdir()
        tree = self.state / 'candidate'
        moved = self.state / 'foreign'
        tree.rename(moved)
        tree.symlink_to(moved, target_is_directory=True)
        self.cli('plan', '--new', ok=False)
        self.assertEqual((self.state / 'run.json').read_bytes(), before)
        self.assertTrue(moved.exists())
        self.assertFalse(history.exists())

    def test_interrupted_archive_idempotent_and_history_bounded(self):
        self.data(gate_fail=True)
        self.cli('rc', ok=False)
        before = (self.state / 'run.json').read_bytes()
        history = self.state / 'history'
        history.mkdir(mode=0o700)
        archive = history / ('0.15.1-rc.4-' + hashlib.sha256(before).hexdigest() + '.json')
        os.link(self.state / 'run.json', archive)
        for index in range(15):
            (history / (str(index) + '.json')).write_bytes(before)
        self.cli('rc', '--new', ok=False)
        self.assertEqual(archive.read_bytes(), before)
        current = (self.state / 'run.json').read_bytes()
        self.cli('plan', '--new', ok=False)
        self.assertEqual((self.state / 'run.json').read_bytes(), current)
        self.assertEqual(len(list(history.iterdir())), 16)

    def test_ambiguous_effects_reconcile_without_duplicate_prs(self):
        for effect in ('metadata', 'prepare', 'push', 'pr', 'merge'):
            for when in ('before', 'after'):
                with self.subTest(effect=effect, when=when):
                    # Each identity uses an independent local remote and owned state.
                    self.state = self.base / ('state-' + effect + '-' + when)
                    self.remote = self.base / ('remote-' + effect + '-' + when + '.git')
                    self.git('clone', '--bare', '--no-local', self.source, self.remote)
                    self.db.write_text(json.dumps({'remote': str(self.remote), 'source':str(self.source), 'crash': effect + ':' + when}))
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

    def test_read_only_source_keeps_contract_mutations_in_owned_fixture(self):
        sealed = self.base / 'sealed'
        shutil.copytree(self.source, sealed, ignore=shutil.ignore_patterns('.git'))
        originals = {}
        for path in (sealed, *sealed.rglob('*')):
            mode = path.stat().st_mode
            if path.is_dir():
                self.addCleanup(path.chmod, mode)
            path.chmod(mode & ~0o222)
            originals[path] = (path.read_bytes() if path.is_file() else None,
                               path.stat().st_mode)
        # Run the real setup and admission/mutation paths with read-only inputs.
        # Only the nested test's fresh temporary fixture may receive writes.
        result = unittest.TestResult()
        with mock.patch.object(sys.modules[__name__], 'ROOT', sealed):
            unittest.TestSuite([
                ReleaseCLI('test_plan_dirty_caller_and_read_only_status'),
                ReleaseCLI('test_current_contracts_and_captured_authority_are_required'),
                ReleaseCLI('test_declared_schema_identity_is_required_with_rebound_capabilities'),
            ]).run(result)
        self.assertEqual(result.testsRun, 3)
        self.assertTrue(result.wasSuccessful(), result.errors + result.failures)
        self.assertEqual({sealed, *sealed.rglob('*')}, set(originals))
        for path, (content, mode) in originals.items():
            if content is not None:
                self.assertEqual(path.read_bytes(), content, str(path))
            self.assertEqual(path.stat().st_mode, mode, str(path))

    def test_current_contracts_and_captured_authority_are_required(self):
        policy = json.loads((ROOT / 'scripts/release-policy.json').read_text())
        # Exercise fetched-source admission, not just the policy's token list. Each
        # mutation is committed to the disposable remote and restored before the next.
        for name, tokens in policy['required_features'].items():
            path = self.source / name
            original = path.read_text()
            for token in tokens:
                with self.subTest(path=name, token=token):
                    self.assertIn(token, original)
                    path.write_text(original.replace(token, 'foreign-contract'))
                    self.commit('foreign feature')
                    self.git('-C', self.source, 'push', str(self.remote), 'main')
                    result = self.cli('plan', ok=False)
                    self.assertIn('source_inclusion', result.stderr)
                    self.assertIn(name, result.stderr)
                    self.assertFalse((self.state / 'candidate').exists())
                    self.assertFalse(self.data().get('effects'))
                    path.write_text(original)
            self.commit('restore feature')
        # Missing required schema paths must also fail before materialization.
        path = self.source / 'schemas/scenario-task-template-v2.json'
        original = path.read_bytes()
        path.unlink()
        self.commit('absent template contract')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.cli('plan', ok=False)
        self.assertFalse((self.state / 'candidate').exists())
        self.assertFalse(self.data().get('effects'))
        path.write_bytes(original)
        self.assertEqual(path.read_bytes(), original)

    def test_declared_schema_identity_is_required_with_rebound_capabilities(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('release_contract', ROOT / 'scripts/release-contract.py')
        contract = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(contract)
        policy = json.loads((ROOT / 'scripts/release-policy.json').read_text())
        declaration_path = self.source / 'scripts/release-contract.json'
        original_declaration = declaration_path.read_bytes()
        for name, tokens in policy['required_features'].items():
            if not name.startswith('schemas/'):
                continue
            with self.subTest(schema=name):
                path = self.source / name
                original = path.read_bytes()
                document = json.loads(original)
                expected = document['properties']['schema']['const']
                document['properties']['schema']['const'] = 'af/foreign@1'
                document['description'] = 'Retains expected identity: ' + expected
                path.write_text(json.dumps(document))
                self.assertTrue(all(token in path.read_text() for token in tokens))
                declaration = json.loads(original_declaration)
                declaration['capabilities'] = contract.capabilities(self.source)
                declaration_path.write_text(json.dumps(declaration))
                # Exact-byte preflight accepts these explicitly reviewed bytes;
                # fetched-source identity admission must independently refuse them.
                self.assertEqual(contract.check(self.source), '0.15.1-rc.3')
                self.commit('foreign declared identity with reviewed capabilities')
                self.git('-C', self.source, 'push', str(self.remote), 'main')
                result = self.cli('plan', ok=False)
                self.assertIn('source_inclusion: foreign required schema identity ' + name, result.stderr)
                self.assertFalse((self.state / 'candidate').exists())
                self.assertFalse(self.data().get('effects'))
                path.write_bytes(original)
                declaration_path.write_bytes(original_declaration)
        self.commit('restore reviewed schemas')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.cli('plan')

    def test_malformed_required_schema_identity_is_refused(self):
        name = 'schemas/scenario-v6.json'
        path = self.source / name
        for content in ('null', '{}', '{"properties": []}',
                        '{"properties": {"schema": {}}}', '{invalid json'):
            with self.subTest(content=content):
                path.write_text(content)
                self.commit('malformed required schema')
                self.git('-C', self.source, 'push', str(self.remote), 'main')
                result = self.cli('plan', ok=False)
                self.assertIn('source_inclusion: invalid required schema ' + name, result.stderr)
                self.assertFalse((self.state / 'candidate').exists())
                self.assertFalse(self.data().get('effects'))

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

    def test_missing_pin_is_independent_of_publication(self):
        self.data(no_pin=True)
        self.cli('rc')
        self.assertEqual(self.state_value()['pin']['state'], 'CREDENTIAL_MISSING')
        self.publish_assets(self.state_value())
        self.assertTrue(json.loads(self.cli('resume').stdout)['published'])
        self.assertEqual(self.state_value()['pin']['state'], 'CREDENTIAL_MISSING')

    def test_server_expected_base_prerequisite_before_effects(self):
        for flag in ('no_strict', 'bypass'):
            self.data(**{flag: True})
            self.assertIn('expected_base_prerequisite', self.cli('plan', ok=False).stderr)
            self.assertEqual(self.data().get('effects', []), [])
            self.data(**{flag: False})

    def test_full_profile_retains_full_workflow_and_policy(self):
        self.cli('rc', '--profile', 'full')
        state = self.state_value()
        self.assertEqual(state['plan']['profile'], 'full')
        self.assertEqual(state['plan']['policy']['validation'][-1][-1], 'full')
        self.assertEqual(state['receipts']['checks']['status'], 'complete')
        self.assertEqual(state['receipts']['validation']['status'], 'complete')

    def test_missing_workflow_policy_fails_preflight(self):
        self.data(ci_disabled=True)
        self.assertIn('validation_policy', self.cli('plan', ok=False).stderr)
        self.assertEqual(self.data().get('effects', []), [])

    def test_main_racing_merge_is_refused_by_server(self):
        self.data(race_merge=True)
        self.cli('rc', ok=False)
        self.assertEqual(self.data()['pr']['state'], 'OPEN')
        self.assertNotIn('merge', self.data()['effects'])
        self.assertIn('stale_main', self.cli('resume', ok=False).stderr)

    def test_exact_ci_evidence_and_additional_formal_checks(self):
        self.data(ci_missing=True)
        self.cli('rc')
        self.assertEqual(self.state_value()['stage'], 'checks')
        for override in ({'head_sha':'0'*40}, {'event':'push'}, {'path':'.github/workflows/other.yml'},
                         {'pull_requests':[]}, {'run_attempt':2, 'status':'in_progress'}):
            self.data(ci_missing=False, ci_override=override)
            self.cli('resume')
            self.assertNotIn('merge', self.data()['effects'])
        self.data(ci_override={})
        for flag in ('ci_partial', 'ci_skipped'):
            self.data(**{flag:True})
            self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
            self.data(**{flag:False})
        self.data(checks='cancel')
        self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
        self.data(checks='pass', formal_checks='fail')
        self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
        self.data(formal_checks='pending')
        self.cli('resume')
        self.assertNotIn('merge', self.data()['effects'])
        self.data(formal_checks='pass', ci_attempt=2, rerun_during_read=True)
        self.cli('resume')
        self.assertNotIn('merge', self.data()['effects'])
        self.data(rerun_during_read=False)
        self.cli('resume')
        self.assertEqual(self.state_value()['receipts']['checks']['checks'][0]['attempt'], 2)
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_retained_plan_profile_race_and_late_evidence(self):
        # Current labels are full, but the opened event actually resolved the fast default.
        self.data(ci_plan_labels=[])
        self.cli('rc', '--profile', 'full')
        self.assertEqual(self.data()['pr']['labels'], [{'name': 'release:full'}])
        self.assertNotIn('merge', self.data()['effects'])
        self.data(checks='cancel') # Label-triggered concurrency can cancel the older fast run.
        self.cli('resume')
        self.assertNotIn('merge', self.data()['effects'])
        self.data(checks='pass', ci_plan_labels=['release:full'], ci_plan_missing=True)
        self.cli('resume')
        self.assertNotIn('merge', self.data()['effects'])
        self.data(ci_plan_missing=False, artifact_attempt=1, ci_attempt=2)
        self.cli('resume')
        self.assertNotIn('merge', self.data()['effects'])
        self.data(artifact_attempt=2, ci_override={'path': '.github/workflows/ci.yml@refs/pull/1/merge'})
        self.cli('resume')
        evidence = self.state_value()['receipts']['checks']['checks'][0]
        self.assertEqual(evidence['plan']['profile'], 'full')
        self.assertIn('perf', evidence['plan']['selected'])
        self.assertNotEqual(evidence['plan']['sha'], self.state_value()['candidate'])
        self.assertEqual(evidence['attempt'], 2)
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_new_exact_run_during_evidence_read_waits_then_rejects_or_continues(self):
        self.data(new_run_during_read='pending')
        self.cli('rc')
        self.assertEqual(self.data()['ci_run_id'], 20)
        self.assertEqual(self.state_value()['stage'], 'checks')
        self.assertNotIn('merge', self.data()['effects'])
        self.cli('resume') # The newer run is still pending.
        self.assertNotIn('merge', self.data()['effects'])
        for conclusion in ('fail', 'cancel'):
            self.data(checks=conclusion)
            self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
            self.assertNotIn('merge', self.data()['effects'])
        self.data(checks='pass')
        self.cli('resume')
        evidence = self.state_value()['receipts']['checks']['checks'][0]
        self.assertEqual(evidence['run_id'], 20)
        self.assertEqual(evidence['attempt'], 1)
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_cancelled_opened_run_without_plan_waits_for_labeled_full(self):
        self.data(checks='cancel', ci_plan_missing=True, ci_plan_labels=[])
        self.cli('rc', '--profile', 'full')
        self.assertEqual(self.state_value()['stage'], 'checks')
        self.assertNotIn('merge', self.data()['effects'])
        self.cli('resume') # No newer run or retained plan yet.
        self.assertNotIn('merge', self.data()['effects'])
        self.data(checks='fail') # Missing plans do not hide other failures.
        self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
        self.assertNotIn('merge', self.data()['effects'])
        self.data(checks='cancel', ci_run_id=20, ci_prior_run=True, ci_plan_labels=['release:full'])
        self.cli('resume') # A cancelled run is still unclassified without its plan.
        self.assertNotIn('merge', self.data()['effects'])
        self.data(ci_plan_missing=False)
        self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
        self.assertNotIn('merge', self.data()['effects'])
        self.data(checks='pass')
        self.cli('resume')
        evidence = self.state_value()['receipts']['checks']['checks'][0]
        self.assertEqual(evidence['run_id'], 20)
        self.assertEqual(evidence['plan']['profile'], 'full')
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_complete_roster_and_exact_plan_fail_closed(self):
        self.data(ci_missing=True)
        self.cli('rc')
        self.data(ci_missing=False)
        for name in ('validation / class functional (macos-latest)',
                     'validation / class e2e (ubuntu-latest)',
                     'validation / class uix (macos-latest)',
                     'validation / container-probes',
                     'validation / telemetry-build-smoke',
                     'validation / telemetry-runtime-smoke'):
            self.data(missing_job=name)
            self.assertIn('incomplete or ambiguous', self.cli('resume', ok=False).stderr)
            self.assertNotIn('merge', self.data()['effects'])
        self.data(missing_job=None)
        for flag, value, error in (
                ('ci_partial_page', True, 'incomplete_observation'),
                ('ci_runs_partial_page', True, 'incomplete_observation'),
                ('ci_artifacts_partial_page', True, 'incomplete_observation'),
                ('ci_duplicate', True, 'incomplete or ambiguous'),
                ('ci_paginated', True, 'incomplete or ambiguous'),
                ('job_attempt', 0, 'identity/attempt'),
                ('plan_digest', 'sha256:'+'0'*64, 'download/digest'),
                ('ci_plan_override', {'catalog_sha256': '0'*64}, 'resolver/catalog'),
                ('ci_plan_override', {'sha': '0'*40}, 'source/base/tree'),
                ('ci_plan_override', {'version': '0.0.0'}, 'context/version'),
                ('ci_commit_override', {'parents': [{'sha':'0'*40}]}, 'source/base/tree'),
                ('ci_commit_override', {'tree': {'sha':'0'*40}}, 'source/base/tree')):
            self.data(**{flag:value})
            self.assertIn(error, self.cli('resume', ok=False).stderr)
            self.assertNotIn('merge', self.data()['effects'])
            self.data(**{flag:{} if isinstance(value, dict) else False if value is True else None})
        self.data(job_attempt=1)
        # Empty PR associations and wrong immutable workflow identity are never adopted.
        for override in ({'pull_requests': []}, {'workflow_id': 123}):
            self.data(ci_override=override)
            self.cli('resume')
            self.assertNotIn('merge', self.data()['effects'])
        self.data(ci_override={})
        self.cli('resume')
        jobs = self.state_value()['receipts']['checks']['checks'][0]['jobs']
        skipped = {j['name'] for j in jobs if j['conclusion'] == 'skipped'}
        self.assertEqual(skipped, {'validation / telemetry-build-smoke',
                                   'validation / telemetry-runtime-smoke'})

    def test_resume_refuses_modified_resolver_or_catalog(self):
        self.data(ci_missing=True)
        self.cli('rc')
        for name in ('scripts/test-classes.py', '.config/test-classes.json', 'scripts/release-observation.py'):
            path = self.source / name
            original = path.read_text()
            path.write_text(original + '# changed execution authority')
            self.assertIn('authority_source_mismatch', self.cli('resume', ok=False).stderr)
            self.assertNotIn('merge', self.data()['effects'])
            path.write_text(original)
        self.data(ci_missing=False)
        self.cli('resume')
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_full_profile_cannot_exempt_telemetry(self):
        self.data(skip_job='validation / telemetry-runtime-smoke')
        self.assertIn('checks_rejected', self.cli('rc', '--profile', 'full', ok=False).stderr)
        self.assertNotIn('merge', self.data()['effects'])
        self.data(skip_job='validation / telemetry-build-smoke')
        self.assertIn('checks_rejected', self.cli('resume', ok=False).stderr)
        self.data(skip_job=None, missing_job='validation / class perf (macos-latest)')
        self.assertIn('incomplete or ambiguous', self.cli('resume', ok=False).stderr)
        self.assertNotIn('merge', self.data()['effects'])

    def test_retired_release_policy_is_refused_before_effects(self):
        path = self.source / 'scripts/release-policy.json'
        policy = json.loads(path.read_text())
        policy['schema'] = 'af.dev-release-policy/2'
        path.write_text(json.dumps(policy))
        self.commit('retired policy')
        self.git('-C', self.source, 'push', str(self.remote), 'main')
        self.assertIn('policy_version', self.cli('plan', ok=False).stderr)
        self.assertEqual(self.data().get('effects', []), [])

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
                    '--repo', 'o/r', '--remote', str(self.remote), '--state', str(self.state), '--step'],
                    env={**self.env, 'FAKE_DELAY': '.15'}, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            import time
            time.sleep(.1)
            result = self.cli('rc', ok=False)
            self.assertIn('concurrent_release', result.stderr)
        finally:
            _, err = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, err)

    @staticmethod
    def old_releases():
        # These release records deliberately have no surviving tags or branches.
        return [{'id':100+i, 'tag_name':'v0.1.0-rc.'+str(i+1),
                 'draft':True, 'assets':[]} for i in range(100)]

    def test_complete_release_reservation_and_secret_names(self):
        releases = self.old_releases() + [{'id':2, 'tag_name':'v0.15.1-rc.8'}]
        self.data(releases=releases, release_no_link=False)
        self.assertIn('version_conflict', self.cli('plan', '--version', '0.15.1-rc.8', ok=False).stderr)
        self.assertEqual(json.loads(self.cli('plan').stdout)['plan']['version'], '0.15.1-rc.9')

    def test_missing_release_links_refuse_reserved_later_tag(self):
        self.data(releases=self.old_releases()+[{'id':2, 'tag_name':'v0.15.1-rc.8', 'draft':True}],
                  release_no_link=True)
        self.assertIn('full release page lacks next metadata',
                      self.cli('plan', '--version', '0.15.1-rc.8', ok=False).stderr)
        self.assertEqual(self.data().get('effects', []), [])

    def test_release_collection_change_refuses_before_any_effect(self):
        self.data(releases=[{'id':1, 'tag_name':'v0.1.0', 'draft':False}],
                  release_change_after_read=True)
        self.assertIn('release collection changed', self.cli('plan', ok=False).stderr)
        self.assertEqual(self.data().get('effects', []), [])

    def test_exact_full_terminal_release_collection(self):
        self.data(releases=self.old_releases(), release_no_link=True)
        self.assertIn('incomplete_observation', self.cli('plan', ok=False).stderr)
        self.assertEqual(self.data().get('effects', []), [])

    def test_controller_reconciles_publication_on_later_release_page(self):
        self.cli('rc')
        self.publish_assets(self.state_value())
        releases = self.old_releases() + self.data()['releases']
        self.data(releases=releases, release_no_link=False)
        self.assertTrue(json.loads(self.cli('resume').stdout)['published'])
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_stable_omitted_third_release_page_refuses_before_effects(self):
        rows = [{'id':i, 'tag_name':'v0.1.0-rc.'+str(i+1)} for i in range(300)]
        rows[-1].update(tag_name='v0.15.1-rc.8', draft=True)
        self.data(releases=rows, release_omit_later_next=True)
        # Identical observations on repeated attempts must never authorize planning.
        for _ in range(2):
            self.assertIn('contradictory release end',
                          self.cli('plan', '--version', '0.15.1-rc.8', ok=False).stderr)
            self.assertEqual(self.data().get('effects', []), [])

    def test_release_observation_refuses_partial_missing_duplicate_and_over_budget(self):
        for no_link, flag in ((False, 'release_partial'),
                              (False, 'release_missing_page'), (False, 'release_duplicate'),
                              (True, 'release_missing_page'), (True, 'release_duplicate'),
                              (True, 'release_malformed')):
            with self.subTest(no_link=no_link, flag=flag):
                self.data(releases=self.old_releases() + [{'id':2, 'tag_name':'v0.15.1-rc.8'}],
                          release_no_link=no_link, **{flag:True})
                self.cli('plan', ok=False)
                self.assertEqual(self.data().get('effects', []), [])
                self.data(**{flag:False})
        for rows in ([{'id':1, 'tag_name':'v0.15.1-rc.8'}, {'id':2, 'tag_name':'v0.15.1-rc.8'}],
                     [{'tag_name':'v0.15.1-rc.8'}],
                     [{'id':i, 'tag_name':'v0.1.0-rc.'+str(i+1)} for i in range(1001)]):
            self.data(releases=rows)
            self.cli('plan', ok=False)
            self.assertEqual(self.data().get('effects', []), [])

    def test_secret_names_cross_default_and_explicit_page_boundaries(self):
        for count in (30, 100):
            self.state = self.base / ('state-secrets-'+str(count))
            rows = [{'name':'UNRELATED_'+str(i)} for i in range(count)]
            rows += [{'name':'MINISIGN_SECRET_KEY'}, {'name':'RELEASE_PIN_TOKEN'}]
            self.data(secret_rows=rows)
            self.cli('plan')
            self.assertIn('protected publication runner', self.state_value()['plan']['signing_readiness'])
        self.assertEqual(self.state_value()['pin']['state'], 'PENDING')

    def test_missing_pin_after_secret_name_boundary_is_independent(self):
        self.data(secret_rows=[{'name':'UNRELATED_'+str(i)} for i in range(100)] +
                  [{'name':'MINISIGN_SECRET_KEY'}])
        self.cli('rc')
        self.assertEqual(self.state_value()['pin']['state'], 'CREDENTIAL_MISSING')
        self.assertEqual(self.data()['effects'].count('merge'), 1)

    def test_secret_names_refuse_incomplete_and_duplicate_metadata(self):
        rows = [{'name':'UNRELATED_'+str(i)} for i in range(100)] + [{'name':'MINISIGN_SECRET_KEY'}]
        for flag in ('secret_duplicate', 'secret_missing_page', 'secret_partial',
                     'secret_missing_total', 'secret_changing_total'):
            with self.subTest(flag=flag):
                self.data(secret_rows=rows, **{flag:True})
                self.cli('plan', ok=False)
                self.assertEqual(self.data().get('effects', []), [])
                self.data(**{flag:False})

    def test_late_release_recovery_and_different_valid_signature(self):
        self.cli('rc')
        state = self.state_value()
        self.publish_assets(state)
        self.git('--git-dir', self.remote, 'update-ref', '-d', 'refs/tags/v0.15.1-rc.4')
        self.git('--git-dir', self.remote, 'update-ref', '-d', 'refs/heads/'+state['plan']['branch'])
        runner = self.base / 'runner'
        runner.mkdir()
        shutil.copytree(self.base / 'assets', runner / 'dist')
        for name in ('SHA256SUMS', 'SHA256SUMS.minisig'):
            shutil.move(runner / 'dist' / name, runner / name)
        (runner / 'install.sh').write_text('fixture installer')
        (runner / 'notes.md').write_text('reviewed notes')
        (runner / 'scripts').mkdir()
        shutil.copyfile(ROOT / 'scripts/release-policy.json', runner / 'scripts/release-policy.json')
        (runner / 'crates/af/keys').mkdir(parents=True)
        # Real independent valid signatures for identical sums, using a disposable key.
        minisign = shutil.which('minisign')
        self.assertIsNotNone(minisign)
        (self.bin / 'minisign').unlink()
        (self.bin / 'minisign').symlink_to(minisign)
        secret = runner / 'key'
        public = runner / 'crates/af/keys/release.pub'
        def sign(*args):
            subprocess.run([minisign, *map(str, args)], check=True, capture_output=True)
        sign('-G', '-W', '-s', secret, '-p', public)
        sums = runner / 'SHA256SUMS'
        signature = runner / 'SHA256SUMS.minisig'
        sign('-S', '-s', secret, '-m', sums, '-t', 'original')
        retained = runner / 'retained'
        retained.mkdir()
        shutil.copytree(runner / 'dist', retained / 'dist')
        for name in ('SHA256SUMS', 'SHA256SUMS.minisig', 'install.sh'):
            shutil.copyfile(runner / name, retained / name)
        original = signature.read_bytes()
        sign('-S', '-s', secret, '-m', sums, '-t', 'different valid signature')
        self.assertNotEqual(signature.read_bytes(), original)
        sign('-V', '-m', sums, '-p', public)
        remote_assets = runner / 'remote'
        remote_assets.mkdir()
        shutil.copyfile(signature, remote_assets / signature.name)
        signature.write_bytes(original)
        target = {'id':1, 'tag_name':'v0.15.1-rc.4', 'target_commitish':state['candidate'],
                  'draft':True, 'assets':[{'name':signature.name}]}
        self.data(releases=self.old_releases()+[target], release_no_link=False, bundles=[], assets=str(remote_assets), effects=[])
        env = {**self.env, 'GH_REPO':'o/r', 'GITHUB_OUTPUT':str(runner / 'output')}
        def helper(name):
            return subprocess.run([sys.executable, '-I', str(ROOT / ('scripts/release-'+name+'.py')),
                                   target['tag_name'], state['candidate']], cwd=runner, env=env,
                                  capture_output=True, text=True)
        rejected = helper('publish')
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn('conflicting published asset SHA256SUMS.minisig', rejected.stderr)
        self.assertNotIn('publish', self.data()['effects'])
        self.assertNotIn('create-release', self.data()['effects'])
        self.assertNotEqual((remote_assets / signature.name).read_bytes(), original)
        # An empty draft on page two still requires original retained bytes.
        target['assets'] = []
        self.data(releases=self.old_releases()+[target], effects=[])
        self.assertIn('original signed bundle unavailable', helper('bundle').stderr)
        self.assertEqual(signature.read_bytes(), original)
        for flag in ('release_missing_page', 'release_partial', 'release_duplicate'):
            self.data(release_no_link=(flag != 'release_partial'), **{flag:True})
            self.assertNotEqual(helper('bundle').returncode, 0)
            self.assertNotEqual(helper('publish').returncode, 0)
            self.assertEqual(self.data()['effects'], [])
            self.data(**{flag:False})
        self.data(release_no_link=False)
        bundle = {'id':5, 'name':'release-signed-'+target['tag_name']+'-'+state['candidate'],
                  'expired':False, 'workflow_run':{'id':1, 'head_sha':state['candidate']}}
        self.data(bundles=[{**bundle, 'expired':True}])
        self.assertIn('expired', helper('bundle').stderr)
        self.data(bundles=[bundle], bundle_dir=str(retained))
        signature.write_bytes(b'rebuilt signature')
        self.assertEqual(helper('bundle').returncode, 0)
        self.assertEqual(signature.read_bytes(), original)
        for path in retained.rglob('*'):
            if path.is_file():
                self.assertEqual((runner / path.relative_to(retained)).read_bytes(), path.read_bytes())
        self.assertEqual(helper('publish').returncode, 0)
        self.assertFalse(self.data()['releases'][-1]['draft'])
        # No record and no retained artifact is the legitimate first-run path.
        self.data(releases=[], bundles=[])
        self.assertEqual(helper('bundle').returncode, 0)
        self.assertIn('reused=false', (runner / 'output').read_text())

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
                    env = {**self.env, 'GH_REPO': 'o/r', 'GITHUB_OUTPUT': str(runner / 'output')}
                    bundle = runner / 'retained'
                    bundle.mkdir()
                    shutil.copytree(runner / 'dist', bundle / 'dist')
                    for name in ('SHA256SUMS', 'SHA256SUMS.minisig', 'install.sh'):
                        shutil.copyfile(runner / name, bundle / name)
                    self.data(bundle_path='.github/workflows/release.yml@main' if when == 'after' else '.github/workflows/release.yml', bundle_dir=str(bundle), bundles=[{
                        'name':'release-signed-v0.15.1-rc.4-' + state['candidate'],
                        'id':5, 'expired':False, 'workflow_run':{'id':1, 'head_sha':state['candidate']}}])
                    first = subprocess.run(command, cwd=runner, env=env, capture_output=True, text=True)
                    self.assertNotEqual(first.returncode, 0, first.stdout)
                    # Model an entirely new workflow run: archive mtimes/build
                    # bytes and provenance run ID differ. Restore original signed
                    # bytes before asking the publisher to reconcile the draft.
                    archive = next((runner / 'dist').glob('*.tar.gz'))
                    archive.write_bytes(b'different rebuilt bytes')
                    provenance = runner / 'dist/release-provenance.json'
                    changed = json.loads(provenance.read_text()); changed['run_id'] = '2'
                    provenance.write_text(json.dumps(changed))
                    restored = subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/release-bundle.py'),
                                              'v0.15.1-rc.4', state['candidate']],
                                             cwd=runner, env=env, capture_output=True, text=True)
                    self.assertEqual(restored.returncode, 0, restored.stderr)
                    self.assertEqual(json.loads(provenance.read_text())['run_id'], '1')
                    self.data(bundle_workflow_id=1)
                    foreign = subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/release-bundle.py'),
                                              'v0.15.1-rc.4', state['candidate']],
                                             cwd=runner, env=env, capture_output=True, text=True)
                    self.assertIn('foreign workflow/source provenance', foreign.stderr)
                    self.assertNotEqual(foreign.returncode, 0)
                    self.data(bundle_workflow_id=339974639)
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
                    # Expired/missing or foreign bundles never authorize rebuilt
                    # bytes to replace the original release assets.
                    bundle_row = self.data()['bundles'][0]
                    for bad in ([], [{**bundle_row, 'expired':True}],
                                [{**bundle_row, 'workflow_run':{'id':1, 'head_sha':'0'*40}}]):
                        self.data(bundles=bad)
                        rejected = subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/release-bundle.py'),
                                                   'v0.15.1-rc.4', state['candidate']],
                                                  cwd=runner, env=env, capture_output=True, text=True)
                        self.assertNotEqual(rejected.returncode, 0)
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


class MetadataObservation(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import importlib.util
        spec = importlib.util.spec_from_file_location('observation', ROOT / 'scripts/release-observation.py')
        cls.observation = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.observation)

    def test_release_headers_and_missing_pages_fail_closed(self):
        batch = [{'id':i, 'tag_name':'v1.0.0-rc.'+str(i+1)} for i in range(100)]
        next_link = 'Link: <https://api.github.com/repos/o/r/releases?per_page=100&page=2>; rel="next"'
        for response in ('[]', 'HTTP/2.0 403 Forbidden\n\n[]',
                         'HTTP/2.0 200 OK\nLink: invalid\n\n[]',
                         'HTTP/2.0 200 OK\n'+next_link+'\n\n[]',
                         'HTTP/2.0 200 OK\n'+next_link.replace('page=2', 'page=3')+'\n\n'+json.dumps(batch),
                         'HTTP/2.0 200 OK\n'+next_link.replace('"next"', '"last"')+'\n\n'+json.dumps(batch)):
            with self.subTest(response=response[:80]):
                with self.assertRaises(ValueError):
                    self.observation.releases(lambda _: response)
        responses = iter(['HTTP/2.0 200 OK\n'+next_link+'\n\n'+json.dumps(batch),
                          'HTTP/2.0 200 OK\n\n[]'])
        with self.assertRaises(ValueError):
            self.observation.releases(lambda _: next(responses))
        self.assertEqual(self.observation.releases(lambda _: 'HTTP/2.0 200 OK\r\n\r\n[]'), [])

    @staticmethod
    def release_response(rows, links=''):
        return 'HTTP/2.0 200 OK\n' + ('Link: '+links+'\n' if links else '') + '\n' + json.dumps(rows)

    @staticmethod
    def link(relation, page):
        return '<https://api.github.com/repos/o/r/releases?per_page=100&page='+str(page)+'>; rel="'+relation+'"'

    def test_stable_300_releases_cannot_hide_reserved_third_page(self):
        rows = [{'id':i, 'tag_name':'v'+str(i)} for i in range(300)]
        rows[-1].update(tag_name='reserved', draft=True)
        calls = []
        def read(endpoint):
            page = int(endpoint.split('page=')[-1])
            calls.append(page)
            links = ([self.link('next', 2), self.link('last', 3)] if page == 1
                     else [self.link('prev', 1)] if page == 2
                     else [self.link('prev', 2), self.link('first', 1)])
            return self.release_response(rows[(page-1)*100:page*100], ', '.join(links))
        for _ in range(2):
            with self.assertRaisesRegex(ValueError, 'contradictory release end'):
                self.observation.releases(read)
        self.assertEqual(calls, [1, 2, 1, 2])

    def test_terminal_evidence_matrix(self):
        for count in (0, 1, 99, 100, 101, 199, 200, 201, 300, 1000):
            for last in (False, True):
                for prev in (False, True):
                    with self.subTest(count=count, last=last, prev=prev):
                        rows = [{'id':i, 'tag_name':'v'+str(i)} for i in range(count)]
                        calls = []
                        def read(endpoint):
                            page = int(endpoint.split('page=')[-1])
                            calls.append(page)
                            links = []
                            if page*100 < count:
                                links.append(self.link('next', page+1))
                            if last:
                                links.append(self.link('last', max(1, (count+99)//100)))
                            if page > 1:
                                links.append(self.link('first', 1))
                                if prev:
                                    links.append(self.link('prev', page-1))
                            return self.release_response(rows[(page-1)*100:page*100], ', '.join(links))
                        accepted = (count <= 100 or prev) and (count == 0 or count % 100 != 0 or last)
                        if accepted:
                            self.assertEqual(self.observation.releases(read), rows)
                            self.assertEqual(calls, list(range(1, max(1, (count+99)//100)+1))*2)
                        else:
                            with self.assertRaisesRegex(ValueError, 'incomplete_observation'):
                                self.observation.releases(read)

    def test_pagination_relations_refuse_malformed_duplicate_and_contradictory(self):
        rows = [{'id':i, 'tag_name':'v'+str(i)} for i in range(200)]
        next_link = self.link('next', 2)
        for links in ([next_link, 'garbage'], [next_link, next_link],
                      [next_link, self.link('last', 1)], [self.link('last', 2)],
                      [next_link, self.link('prev', 1)], [next_link, self.link('first', 2)],
                      [next_link, self.link('last', 0)],
                      [next_link, self.link('last', 2).replace('page=2', 'page=2&page=3')],
                      [next_link, self.link('last', 2).replace('per_page=100', 'per_page=50')]):
            with self.subTest(links=links):
                with self.assertRaisesRegex(ValueError, 'incomplete_observation'):
                    self.observation.releases(lambda _: self.release_response(rows[:100], ', '.join(links)))
        for first_last, terminal_links in (
                (3, [self.link('prev', 1)]),  # Premature partial terminal.
                (3, [self.link('prev', 1), self.link('last', 2)]),
                (2, [self.link('prev', 1), self.link('last', 1)]),
                (2, [self.link('prev', 1), self.link('next', 3)]),
                (2, [self.link('prev', 1), self.link('prev', 1)]),
                (2, [self.link('prev', 2)]),
                (2, [self.link('prev', 1), self.link('first', 2)])):
            with self.subTest(first_last=first_last, terminal_links=terminal_links):
                responses = iter([self.release_response(rows[:100], next_link+', '+self.link('last', first_last)),
                                  self.release_response(rows[100:101], ', '.join(terminal_links))])
                with self.assertRaisesRegex(ValueError, 'incomplete_observation'):
                    self.observation.releases(lambda _: next(responses))

    def test_missing_metadata_cannot_hide_shifted_reserved_release(self):
        rows = [{'id':i, 'tag_name':'v1.0.0-rc.'+str(i+1)} for i in range(100)]
        reserved = {'id':100, 'tag_name':'v2.0.0', 'draft':True}
        # Reviewer example: delete the first row after page 1, moving reserved
        # from offset 100 to 99. Page 2 would now be empty.
        collection = rows + [reserved]
        calls = []
        def read(endpoint):
            calls.append(endpoint)
            page = int(endpoint.split('page=')[-1])
            batch = collection[(page-1)*100:page*100]
            if len(calls) == 1:
                del collection[0]
            return self.release_response(batch)
        with self.assertRaisesRegex(ValueError, 'full release page lacks next metadata'):
            self.observation.releases(read)
        self.assertEqual(len(calls), 1)  # Refuse before trusting an empty probe.

    def test_truncated_short_probe_and_empty_page_do_not_prove_completeness(self):
        rows = [{'id':i, 'tag_name':'v'+str(i)} for i in range(200)]
        calls = []
        def read(endpoint):
            calls.append(endpoint)
            page = int(endpoint.split('page=')[-1])
            batch = rows[(page-1)*100:page*100]
            if page == 2:
                batch = batch[:1]
            return self.release_response(batch)
        with self.assertRaisesRegex(ValueError, 'full release page lacks next metadata'):
            self.observation.releases(read)
        self.assertEqual(len(calls), 1)
        # Even with valid next metadata on page 1, a short terminal response
        # without its normal prev metadata cannot establish completeness.
        link = '<https://api.github.com/releases?per_page=100&page=2>; rel="next"'
        responses = iter([self.release_response(rows[:100], link),
                          self.release_response(rows[100:101]), self.release_response([])])
        with self.assertRaisesRegex(ValueError, 'missing release pagination metadata'):
            self.observation.releases(lambda _: next(responses))

    def test_release_links_never_supply_request_destinations(self):
        for count in (101, 200, 1000):
            with self.subTest(count=count):
                rows = [{'id':i, 'tag_name':'v'+str(i)} for i in range(count)]
                calls = []
                def read(endpoint):
                    calls.append(endpoint)
                    page = int(endpoint.split('page=')[-1])
                    links = []
                    for relation, number in [('next', page+1), ('prev', page-1),
                                             ('last', (count+99)//100), ('first', 1)]:
                        if (relation in ('next', 'last') and page*100 < count) or (relation in ('prev', 'first') and page > 1):
                            links.append('<https://untrusted.invalid/foreign?per_page=100&page='+str(number)+'>; rel="'+relation+'"')
                    return self.release_response(rows[(page-1)*100:page*100], ', '.join(links))
                self.assertEqual(self.observation.releases(read), rows)
                traversal = ['releases?per_page=100&page='+str(i+1) for i in range((count+99)//100)]
                self.assertEqual(calls, traversal * 2)

    def test_release_reread_detects_changed_collection(self):
        rows = [{'id':i, 'tag_name':'v'+str(i)} for i in range(101)]
        for change in ('delete', 'draft', 'tag'):
            with self.subTest(change=change):
                collection = [dict(row) for row in rows]
                calls = []
                def read(endpoint):
                    calls.append(endpoint)
                    page = int(endpoint.split('page=')[-1])
                    batch = collection[(page-1)*100:page*100]
                    links = ('<https://api.github.com/releases?per_page=100&page=2>; rel="next"'
                             if page == 1 else '<https://api.github.com/releases?per_page=100&page=1>; rel="prev"')
                    response = self.release_response(batch, links)
                    if len(calls) == 1:
                        if change == 'delete':
                            del collection[0]
                        elif change == 'draft':
                            collection[0]['draft'] = True
                        else:
                            collection[0]['tag_name'] = 'reserved'
                    return response
                with self.assertRaisesRegex(ValueError, 'incomplete_observation'):
                    self.observation.releases(read)

    def test_reread_detects_boundary_shift_with_normal_pagination(self):
        rows = [{'id':i, 'tag_name':'v'+str(i), 'draft':True} for i in range(202)]
        calls = []
        def read(endpoint):
            calls.append(endpoint)
            page = int(endpoint.split('page=')[-1])
            batch = rows[(page-1)*100:page*100]
            links = []
            if page*100 < len(rows):
                links.append('<https://api.github.com/releases?per_page=100&page='+str(page+1)+'>; rel="next"')
            if page*100 < len(rows):
                links.append(self.link('last', (len(rows)+99)//100))
            if page > 1:
                links += [self.link('prev', page-1), self.link('first', 1)]
            response = self.release_response(batch, ', '.join(links))
            if len(calls) == 1:
                del rows[0]
            return response
        with self.assertRaisesRegex(ValueError, 'release collection changed'):
            self.observation.releases(read)
        self.assertEqual(len(calls), 6)

    def test_complete_traversal_reread_compares_all_release_fields(self):
        rows = [{'id':i, 'tag_name':'v'+str(i), 'draft':False} for i in range(101)]
        calls = []
        def read(endpoint):
            calls.append(endpoint)
            page = int(endpoint.split('page=')[-1])
            link = '<https://api.github.com/releases?per_page=100&page='+str(2 if page == 1 else 1)+'>; rel="'+('next' if page == 1 else 'prev')+'"'
            response = self.release_response(rows[(page-1)*100:page*100], link)
            if len(calls) == 2:
                rows[-1]['draft'] = True
            return response
        with self.assertRaisesRegex(ValueError, 'release collection changed'):
            self.observation.releases(read)
        self.assertEqual(len(calls), 4)

    def test_release_traversal_budget(self):
        calls = []
        def read(endpoint):
            calls.append(endpoint)
            page = int(endpoint.split('page=')[-1])
            rows = [{'id':page*100+i, 'tag_name':'v'+str(page*100+i)} for i in range(100)]
            link = '<https://api.github.com/releases?per_page=100&page='+str(page+1)+'>; rel="next"'
            return self.release_response(rows, link)
        with self.assertRaisesRegex(ValueError, 'observation_budget'):
            self.observation.releases(read)
        self.assertEqual(len(calls), self.observation.MAX_PAGES)

    def test_names_metadata_totals_and_budget(self):
        for data in ({'secrets':[]}, {'total_count':True, 'secrets':[]},
                     {'total_count':1, 'secrets':[]}, {'total_count':0, 'secrets':[{'name':'KEY'}]},
                     {'total_count':1, 'secrets':[{'id':1}]},
                     {'total_count':2, 'secrets':[{'name':'KEY'}, {'name':'KEY'}]}):
            with self.subTest(data=data):
                with self.assertRaises(ValueError):
                    self.observation.pages(lambda _: data, 'actions/secrets', 'secrets', 'name')
        def api(endpoint):
            page = int(endpoint.split('page=')[-1])
            return {'total_count':1001, 'secrets':[{'name':'KEY_'+str(i)}
                    for i in range((page-1)*100, page*100)]}
        with self.assertRaisesRegex(ValueError, 'observation_budget'):
            self.observation.pages(api, 'actions/secrets', 'secrets', 'name')


class CacheAcquisition(unittest.TestCase):
    def setUp(self):
        import importlib.util
        from types import SimpleNamespace
        spec = importlib.util.spec_from_file_location('dev_release_cache', ROOT / 'scripts/dev-release.py')
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name).resolve()
        self.caller = self.base / 'caller'
        self.env = {**os.environ, 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1'}
        self.git('init', '-q', '-b', 'main', self.caller)
        (self.caller / 'file').write_text('committed')
        self.git('-C', self.caller, 'add', '.')
        self.git('-C', self.caller, '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                 'commit', '-qm', 'initial')
        self.head = self.git('-C', self.caller, 'rev-parse', 'HEAD')
        self.git('-C', self.caller, 'remote', 'add', 'origin', 'git@github.com:o/r.git')
        self.remote = self.base / 'remote.git'
        self.git('clone', '-q', '--bare', '--no-local', self.caller, self.remote)
        self.state = self.base / 'state'
        self.state.mkdir(mode=0o700)
        self.controller = self.module.Controller(SimpleNamespace(repo='o/r'), self.state)
        self.git('init', '-q', '--bare', self.controller.mirror)
        self.git('--git-dir', self.controller.mirror, 'remote', 'add', 'origin', self.remote)
        patcher = mock.patch.object(self.module, 'ROOT', self.caller)
        patcher.start()
        self.addCleanup(patcher.stop)

    def git(self, *args):
        return subprocess.check_output([GIT, *map(str, args)], env=self.env,
                                       text=True, stderr=subprocess.DEVNULL).strip()

    def seed(self):
        import contextlib
        import io
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            self.controller.seed_cache()
        self.assertFalse((self.state / 'cache-acquisition').exists())
        self.assertFalse((self.controller.mirror / 'objects/info/alternates').exists())
        return output.getvalue()

    def test_cache_dirty_hooks_config_tags_and_remote_authority(self):
        self.git('-C', self.caller, 'tag', 'v999.0.0')
        (self.caller / 'file').write_text('dirty caller')
        hook = self.caller / '.git/hooks/pre-upload-pack'
        hook.write_text('#!/bin/sh\ntouch "' + str(self.base / 'executed') + '"\nexit 1\n')
        hook.chmod(0o755)
        for name in ('reference-transaction', 'post-checkout', 'post-index-change'):
            executable = hook.parent / name
            executable.write_text(hook.read_text())
            executable.chmod(0o755)
        self.git('-C', self.caller, 'config', 'core.hooksPath', str(hook.parent))
        for key in ('uploadpack.packObjectsHook', 'core.sshCommand'):
            self.git('-C', self.caller, 'config', key, str(hook))
        before = {str(p.relative_to(self.caller)): p.read_bytes()
                  for p in self.caller.rglob('*') if p.is_file()}
        self.assertIn('cache acquired', self.seed())
        self.assertEqual(self.controller.git('rev-parse', 'refs/cache/caller-head'), self.head)
        self.assertEqual(self.controller.git('tag', '--list'), '')
        # Advance remote main without the caller; the authoritative fetch selects it.
        tree = self.git('--git-dir', self.remote, 'rev-parse', self.head + '^{tree}')
        with mock.patch.dict(os.environ, {'GIT_AUTHOR_NAME': 'Test', 'GIT_AUTHOR_EMAIL': 't@example.invalid',
                                        'GIT_COMMITTER_NAME': 'Test', 'GIT_COMMITTER_EMAIL': 't@example.invalid'}):
            remote_head = self.module.run(['git', '--git-dir', self.remote, 'commit-tree', tree,
                                          '-p', self.head, '-m', 'remote advance'], git_env={k: os.environ[k] for k in
                                          ('GIT_AUTHOR_NAME', 'GIT_AUTHOR_EMAIL', 'GIT_COMMITTER_NAME', 'GIT_COMMITTER_EMAIL')})
        self.git('--git-dir', self.remote, 'update-ref', 'refs/heads/main', remote_head)
        self.assertEqual(self.controller.fetch(acquisition=True), remote_head)
        self.assertEqual(self.controller.git('tag', '--list'), '')
        self.assertFalse((self.base / 'executed').exists())
        self.assertEqual(before, {str(p.relative_to(self.caller)): p.read_bytes()
                                 for p in self.caller.rglob('*') if p.is_file()})
        shutil.rmtree(self.caller)
        self.assertEqual(self.controller.git('show', self.head + ':file'), 'committed')

    def test_cache_linked_worktree_and_https_identity(self):
        linked = self.base / 'linked'
        self.git('-C', self.caller, 'worktree', 'add', '--detach', linked, self.head)
        self.git('-C', self.caller, 'config', 'remote.origin.url', 'https://github.com/o/r.git')
        with mock.patch.object(self.module, 'ROOT', linked):
            self.assertIn('cache acquired', self.seed())

    def test_cache_packed_head_and_symlinked_objects(self):
        self.git('-C', self.caller, 'pack-refs', '--all')
        self.assertIn('cache acquired', self.seed())
        objects = self.caller / '.git/objects'
        saved = self.caller / '.git/saved-objects'
        objects.rename(saved)
        objects.symlink_to(saved, target_is_directory=True)
        self.assertIn('cache skipped', self.seed())

    def test_cache_unsupported_foreign_unavailable_symlink_and_alternates(self):
        config = self.caller / '.git/config'
        original = config.read_text()
        cases = ['[remote "origin"]\nurl = https://github.com/foreign/repo.git\n',
                 original + '\n[include]\npath = /nonexistent\n',
                 original + '\n[extensions]\nobjectformat = sha256\n']
        for content in cases:
            config.write_text(content)
            self.assertIn('cache skipped', self.seed())
        config.write_text(original)
        alternate = self.caller / '.git/objects/info/alternates'
        alternate.write_text('/nonexistent\n')
        self.assertIn('cache skipped', self.seed())
        alternate.unlink()
        config.rename(config.with_suffix('.saved'))
        config.symlink_to(config.with_suffix('.saved'))
        self.assertIn('cache skipped', self.seed())
        with mock.patch.object(self.module, 'ROOT', self.base / 'missing'):
            self.assertIn('cache skipped', self.seed())
        config.unlink()
        config.with_suffix('.saved').rename(config)
        config.chmod(0o666)
        self.assertIn('cache skipped', self.seed())
        config.chmod(0o644)
        with mock.patch.object(self.module.os, 'getuid', return_value=os.getuid() + 1):
            self.assertIn('cache skipped', self.seed())

    def test_cache_cannot_replace_failed_acquisition_and_retry_is_bounded(self):
        self.assertIn('cache acquired', self.seed())
        shutil.rmtree(self.remote)
        with mock.patch.object(self.controller, 'git', wraps=self.controller.git) as invoked:
            with self.assertRaisesRegex(self.module.Refusal, 'source_acquisition_failed'):
                self.controller.fetch(acquisition=True)
            self.assertEqual(invoked.call_count, 2)
        self.assertFalse(self.controller.path.exists())
        self.assertEqual(self.controller.git('tag', '--list'), '')

    def test_initial_status_is_read_only_nonblocking_and_lease_based(self):
        missing = self.base / 'missing'
        self.assertEqual(self.module.initial_status(missing)['status'], 'missing_run')
        self.assertFalse(missing.exists())
        with self.module.lease(self.state):
            self.assertEqual(self.module.initial_status(self.state)['status'], 'initializing')
            self.assertFalse(self.controller.path.exists())
        self.assertEqual(self.module.initial_status(self.state)['status'], 'missing_run')
        # No new state, identity or plan is invented by observation.
        self.assertEqual(sorted(p.name for p in self.state.iterdir()), ['lock', 'repository.git'])

    def test_initial_status_refuses_fifo_without_blocking(self):
        os.mkfifo(self.state / 'lock')
        with self.assertRaisesRegex(self.module.Refusal, 'owned regular lock'):
            self.module.initial_status(self.state)

    def test_cache_interruption_cleans_and_never_reaches_remote(self):
        with mock.patch.object(self.module, 'cache_bundle', side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.controller.seed_cache()
        self.assertFalse((self.state / 'cache-acquisition').exists())
        self.assertFalse(self.controller.path.exists())
        scratch = self.state / 'cache-acquisition'
        scratch.mkdir()
        (scratch / 'interrupted').write_text('scratch')
        self.assertIn('cache acquired', self.seed())


if __name__ == '__main__':
    unittest.main()
