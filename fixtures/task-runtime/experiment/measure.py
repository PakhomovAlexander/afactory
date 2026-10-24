# The measured command of the experiment fixture (ADR-0132): write size.txt's number of bytes
# into $TMPDIR, the repetition's private runtime directory, and report what was written on the
# last stdout line. mode.txt selects a misbehaviour the kernel must record.
import json
import os
import re
import stat
import subprocess
import signal
import sys
import time

mode = open('mode.txt').read().strip()
size = int(open('size.txt').read().strip())
if mode == 'counter':
    # One more byte on every run: counter.txt names a file outside the read-only source.
    path = open('counter.txt').read().strip()
    count = int(open(path).read()) if os.path.exists(path) else 0
    open(path, 'w').write(str(count + 1))
    size += count
if mode == 'warm':
    # A build product in whatever CARGO_TARGET_DIR the kernel bound, written only when absent:
    # a warm directory already holds it.
    built = os.path.join(os.environ['CARGO_TARGET_DIR'], 'build.bin')
    os.makedirs(os.path.dirname(built), exist_ok=True)
    if not os.path.exists(built):
        open(built, 'wb').write(b'x' * 4096)
if mode == 'cargo':
    # ADR-0201 through the real measure operator, every repetition and A -> B -> A. Sources
    # are stamped by content: each source group (a Cargo package directory, or the root) carries
    # one stamp, none in the future, and a group whose content the key did not build last is
    # stamped after every product. Only the binary below proves which Snapshot was built.
    target_dir = os.environ['CARGO_TARGET_DIR']
    groups = {}
    for parent, _, files in os.walk('.'):
        for name in files:
            path = os.path.join(parent, name)
            if not stat.S_ISREG(os.lstat(path).st_mode):
                continue
            group = next((g for g in ('app', 'word') if path.startswith('./' + g + '/')), '')
            groups.setdefault(group, set()).add(os.lstat(path).st_mtime_ns)
    for group, stamps in groups.items():
        assert len(stamps) == 1, (group, sorted(stamps))
        assert stamps.pop() <= time.time_ns(), 'source stamp of %r is in the future' % group
    expected = int(re.search(r'\{\s*(\d+)\s*\}', open('word/src/lib.rs').read()).group(1))
    subprocess.run(['cargo', 'build', '--offline', '--locked', '--quiet'], check=True)
    repeated = subprocess.run(['cargo', 'build', '--offline', '--locked', '--message-format=json'],
                              check=True, capture_output=True, text=True)
    artifacts = [json.loads(line) for line in repeated.stdout.splitlines()
                 if json.loads(line).get('reason') == 'compiler-artifact']
    assert artifacts and all(a['fresh'] for a in artifacts), 'second build recompiled workspace units'
    marker = os.path.join(os.environ['CARGO_HOME'], 'registry', 'freshness-sentinel')
    if os.path.exists(marker):
        assert open(marker).read() == 'registry preserved'
        print('registry cache reused')
    printed = subprocess.run([os.path.join(target_dir, 'debug', 'app')], check=True,
                             capture_output=True, text=True).stdout.strip()
    print('built', printed, 'from a source of', expected)
    if int(printed) != expected:
        raise SystemExit('the build linked another Snapshot: printed %s, the source says %d'
                         % (printed, expected))
    size = int(printed)
target = os.path.join(os.environ['TMPDIR'], 'out.bin')
with open(target, 'wb') as out:
    out.write(b'x' * size)
written = os.path.getsize(target)
print('wrote', written, 'bytes')
if mode == 'sleep':
    # Past any wall_ms a test declares, with output already written: the kernel must record a
    # timeout and keep what was printed.
    sys.stdout.flush()
    time.sleep(60)
if mode == 'kill':
    # Ended by a signal after printing: the kernel records no exit code, only what was printed.
    sys.stdout.flush()
    os.kill(os.getpid(), signal.SIGKILL)
if mode == 'exit':
    raise SystemExit(3)
if mode == 'mutate':
    os.chmod('.', 0o755)
    open('added.txt', 'w').write('x')
unit = 'count' if mode == 'count' else 'bytes'
print(json.dumps({'schema': 'af.measure-report/1',
                  'metrics': {'bytes_written': {'value': str(written), 'unit': unit}}}))
