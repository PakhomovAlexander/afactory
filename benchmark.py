import json,os,pathlib,subprocess,time,platform,hashlib,signal,threading
root=pathlib.Path.cwd();out=root/'measurements';out.mkdir(exist_ok=True)
meta={'hostname':platform.node(),'platform':platform.platform(),'cpu_count':os.cpu_count(),'runner_name':os.getenv('RUNNER_NAME'),'run_id':os.getenv('GITHUB_RUN_ID'),'threads':4,'order':['base1','head1','base2','head2','base3','head3'],'warm':'both checkout-owned targets prebuilt before measured cohort','native_acceptance':'unsatisfied; measurement only'}
for name,sha in [('base','6ca82d7d5e3ab94b9f426b2c99b8f44b95f87d42'),('head','e3ecc53db3e14abe5a9bd3bdb39b7480dafcfa1a')]:
 actual=subprocess.check_output(['git','-C',name,'rev-parse','HEAD'],text=True).strip();assert actual==sha;meta[name+'_sha']=actual
for e in json.loads((root/'candidate-manifest.json').read_text()):
 p=root/'head'/e['path'];assert hashlib.sha256(p.read_bytes()).hexdigest()==e['sha256'],e['path'];assert bool(p.stat().st_mode&0o111)==(e['mode']=='100755')
assert len(subprocess.check_output(['git','-C','head','ls-files','-z']).split(bytes([0])))-1==1362
meta['candidate_entire_tree_proof']='1362 raw SHA256 and executable modes match native snapshot f6cf269837fb30c4014ee3fe95c8ad596ad2b15e547235f177b4948f00342935'
(out/'metadata.json').write_text(json.dumps(meta,indent=2))
with (out/'environment.txt').open('w') as f:
 for cmd in [['uname','-a'],['lscpu'],['free','-b'],['df','-B1','.'],['rustc','+1.88.0','-Vv'],['cargo','+1.88.0','-V'],['cargo','nextest','--version'],['ld.lld','--version'],['make','--version']]:subprocess.run(cmd,stdout=f,stderr=subprocess.STDOUT)
env=os.environ.copy();env['CARGO_INCREMENTAL']='0';env['CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS']='-C link-arg=-fuse-ld=lld';env['RUSTUP_TOOLCHAIN']='1.88.0';env.pop('RUSTC_WRAPPER',None)
def run(cmd,name,dest,limit):
 e=env.copy();e['CARGO_TARGET_DIR']=str(root/name/'target');e['AF_CI_METRICS']=str(dest/'steps.jsonl');e['AF_GATE_NEXTEST_TARGET']=e['CARGO_TARGET_DIR'];e['AF_GATE_NEXTEST_RUN']=str(dest/'nextest');e['GITHUB_SHA']=meta[name+'_sha'];start=time.monotonic()
 with (dest/'command.log').open('w') as log,(dest/'resources.jsonl').open('w') as res:
  p=subprocess.Popen(cmd,cwd=root/name,env=e,stdout=log,stderr=subprocess.STDOUT,start_new_session=True);timeout=False
  while p.poll() is None:
   res.write(json.dumps({'elapsed':time.monotonic()-start,'load':os.getloadavg(),'meminfo':pathlib.Path('/proc/meminfo').read_text(),'disk_free':os.statvfs(root).f_bavail*os.statvfs(root).f_frsize})+'\n');res.flush()
   if time.monotonic()-start>limit:
    timeout=True;os.killpg(p.pid,signal.SIGTERM);time.sleep(5)
    try:os.killpg(p.pid,signal.SIGKILL)
    except ProcessLookupError:pass
    break
   try:p.wait(timeout=5)
   except subprocess.TimeoutExpired:pass
  code=p.wait()
 result={'command':cmd,'sha':meta[name+'_sha'],'wall_s':time.monotonic()-start,'exit':code,'timeout':timeout};(dest/'result.json').write_text(json.dumps(result,indent=2));print(dest.name,json.dumps(result),flush=True);return code
failed=False
for name in ['base','head']:
 dest=out/(name+'-prebuild');dest.mkdir();failed|=run(['cargo','test','--locked','--no-run'],name,dest,900)!=0
for pair in range(1,4):
 for name in ['base','head']:
  dest=out/(name+str(pair));dest.mkdir();failed|=run(['make','test','TEST_THREADS=4'],name,dest,1100)!=0
  junit=dest/'nextest/store/ci/junit.xml'
  if junit.exists():
   with (dest/'summary.json').open('w') as f:r=subprocess.run(['python3','scripts/test-time-report.py','summary',str(junit),'--format','json'],cwd=root/name,stdout=f);failed|=r.returncode!=0
  else:failed=True
(out/'cohort-result.json').write_text(json.dumps({'failed':failed,'completed_runs':6}))
raise SystemExit(1 if failed else 0)
