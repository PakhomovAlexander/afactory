#!/usr/bin/python3 -I
"""Private idempotent notification fixture. No network, GitHub or credentials."""
import json
import os
from pathlib import Path
import sys
root = Path(__file__).resolve().parent
request = json.loads(sys.stdin.buffer.read(4097))
assert request['operation'] == 'notify' and request['destination'] == 'fixture:owner'
assert request['method'] in ('reconcile', 'execute')
key = request['intent_id']
assert key.startswith('sha256:') and len(key) == 71
receipt = root / (key[7:] + '.json')
response = {'schema': 'af/scenario-adapter-response@1', 'intent_id': key,
    'input_digest': request['input_digest'], 'outcome': 'absent',
    'evidence': 'durable fixture notification lookup', 'observation': None}
if receipt.exists():
    response = json.loads(receipt.read_text())
    assert response['input_digest'] == request['input_digest']
elif request['method'] == 'execute':
    response.update(outcome='done', evidence='one scoped fixture notification retained')
    with receipt.open('x') as stream:
        json.dump(response, stream)
        stream.flush()
        os.fsync(stream.fileno())
    with (root / 'effects').open('a') as stream:
        stream.write(key + '\n')
    if (root / 'lose-response').exists():
        (root / 'lose-response').unlink()
        raise SystemExit(1)
print(json.dumps(response))
