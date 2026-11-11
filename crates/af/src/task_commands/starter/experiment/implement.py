import json
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) == {'source', 'requirements'}
specification = request['inputs']['requirements'][0]['payload'].get('specification')
assert specification['schema'] == 'tutorial.experiment/1'
# The candidate: the measured command writes this many bytes. The kernel, not this Worker,
# measures whether that is an improvement.
with open('size.txt', 'w') as target:
    target.write('%d\n' % specification['size'])
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {
    'report': [{'summary': 'Set size.txt to %d.' % specification['size']}]
}}))
