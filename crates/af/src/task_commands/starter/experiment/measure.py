# The measured command (ADR-0132): write size.txt's number of bytes into $TMPDIR, the
# repetition's private runtime directory, and report what was written on the last stdout line.
import json
import os

size = int(open('size.txt').read().strip())
target = os.path.join(os.environ['TMPDIR'], 'out.bin')
with open(target, 'wb') as out:
    out.write(b'x' * size)
written = os.path.getsize(target)
print('wrote', written, 'bytes')
print(json.dumps({'schema': 'af.measure-report/1',
                  'metrics': {'bytes_written': {'value': str(written), 'unit': 'bytes'}}}))
