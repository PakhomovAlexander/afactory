# The measured command of the experiment fixture (ADR-0124): write size.txt's number of bytes
# into $TMPDIR, the repetition's private runtime directory, and report what was written on the
# last stdout line. mode.txt selects a misbehaviour the kernel must record.
import json
import os
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
