import json
import sys
from pathlib import Path

request = json.load(sys.stdin)
assert set(request['inputs']) == {'request'}
planning = request['inputs']['request'][0]['payload']
assert planning['schema'] == 'af.planning-request/1'
assert planning['task']['facts'].get('capability') == 'tutorial.pagination/1'
proposal = json.loads(Path(__file__).with_name('proposal.json').read_text())
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'proposal': [proposal]}}))
