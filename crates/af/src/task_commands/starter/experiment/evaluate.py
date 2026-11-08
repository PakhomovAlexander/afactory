import json
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) == {'source', 'requirements', 'checks', 'comparison'}
assert request['inputs']['checks'][0]['payload']['outcome'] == 'passed'
# The kernel dispatches this evaluator only after a passed comparison; it reads the
# comparison and cannot change it.
comparison = request['inputs']['comparison'][0]['payload']
accepted = comparison['outcome'] == 'passed' and comparison['objective'] == 'smaller'
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'result': [{
    'outcome': 'passed' if accepted else 'failed',
    'reason': 'The kernel comparison of the sealed source met the declared objective.'
}]}}))
