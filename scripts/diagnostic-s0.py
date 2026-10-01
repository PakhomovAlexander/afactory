#!/usr/bin/env python3
"""One unchanged S0 diagnostic selection; no acceptance authority."""
import datetime, hashlib, json, os, pathlib, re, subprocess, time
out = pathlib.Path(os.environ['RUNNER_TEMP']) / 's0-evidence'
out.mkdir(exist_ok=True)
script = pathlib.Path(__file__).resolve()
filter_text = script.with_name('diagnostic-s0-filter.txt').read_text().strip()
(out / 'filter.txt').write_text(filter_text + '\n')
names = re.findall(r'test\(=([^)]*)\)', filter_text)
assert len(names) == len(set(names)) == 16
(out / 'test-names.json').write_text(json.dumps(names, indent=2))
def run(label, argv):
    start = time.monotonic()
    meta = dict(argv=argv, cwd=os.getcwd(), started_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
    with (out / (label + '.stdout')).open('wb') as stdout, (out / (label + '.stderr')).open('wb') as stderr:
        p = subprocess.run(argv, stdout=stdout, stderr=stderr)
    meta.update(exit_code=p.returncode, elapsed_seconds=time.monotonic()-start, finished_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
    (out / (label + '.command.json')).write_text(json.dumps(meta, indent=2))
    for stream in ['stdout','stderr']:
        print((out / (label + '.' + stream)).read_text(errors='replace'), flush=True)
    return p.returncode
for label, argv in [('head',['git','rev-parse','HEAD']), ('status-before',['git','status','--porcelain=v1']), ('rustc',['rustc','-Vv']), ('cargo',['cargo','-V']), ('nextest',['cargo','nextest','--version']), ('cargo-metadata',['cargo','metadata','--locked','--format-version','1','--no-deps'])]:
    if run(label, argv): raise SystemExit(1)
assert (out/'head.stdout').read_text().strip() == 'db11e6e885a1268bc92bb5236f7cc8354f31f491'
assert not (out/'status-before.stdout').read_text().strip()
assert '0.9.132' in (out/'nextest.stdout').read_text()
os.environ.pop('AF_VERSION',None)
os.environ.pop('AF_DISPATCHED_FROM',None)
os.environ['AF_WORKSPACE_ROOT'] = os.getcwd()
(out/'build-env.json').write_text(json.dumps({k:os.environ.get(k) for k in ['CARGO_INCREMENTAL','SCCACHE_GHA_ENABLED','RUSTC_WRAPPER','CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS','AF_WORKSPACE_ROOT']}, indent=2))
code = run('focused', ['cargo','nextest','run','--locked','--profile','ci','--test-threads','4','-p','af','--test','it','-E',filter_text])
(out/'test-exit-code.txt').write_text(str(code)+'\n')
run('binaries', ['cargo','nextest','list','--locked','-p','af','--test','it','--list-type','binaries-only','--message-format','json'])
run('binary-version', ['target/debug/af','--version'])
run('status-after', ['git','status','--porcelain=v1'])
hashes = {}
for p in [pathlib.Path('target/debug/af'), *pathlib.Path('target/debug/deps').glob('it-*')]:
    if p.is_file() and os.access(p,os.X_OK): hashes[str(p)] = hashlib.sha256(p.read_bytes()).hexdigest()
(out/'binary-sha256.json').write_text(json.dumps(hashes,indent=2))
raise SystemExit(code)
