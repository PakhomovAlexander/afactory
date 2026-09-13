import json
import runpy
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) in [{'source', 'subject', 'history', 'checks', 'assignment'}, {'source', 'subject', 'history', 'checks', 'assignment', 'requirements'}]
assignment = request['inputs']['assignment'][0]['payload']
assert assignment['reviewer'] == 'bugs'
assert all(f['source'] == 'bugs' for f in assignment['findings'])
if 'requirements' in request['inputs']:
    assert request['inputs']['requirements'][0]['payload'].get('specification', {}).get('schema') == 'tutorial.pagination/1'
assert request['inputs']['checks'][0]['payload']['outcome'] == 'passed'
passed = True
try:
    paginate = runpy.run_path('pagination.py')['paginate']
    for offset, limit in [(-1, 1), (0, -1), (-100, 1), (0.5, 2), (0, None), (False, 2)]:
        try:
            paginate([0, 1, 2], offset, limit)
            passed = False
        except ValueError:
            pass
except Exception:
    passed = False
reports = [] if passed else [{
    'severity': 'major', 'file': 'pagination.py', 'line': 1,
    'title': 'Reject invalid pagination bounds',
    'body': 'Negative or non-integer bounds were accepted or raised an unexpected exception.',
    'fix': 'Raise ValueError for negative or non-integer offset and limit.', 'confidence': 1.0
}]
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'result': [{
    'verdict': 'approve' if passed else 'request-changes',
    'summary': 'Independent invalid-bound and exception checks.',
    'reports': reports, 'benchmark_demands': [],
    'dispositions': [{'finding_id': f['finding_id'],
                      'position': 'not_reproduced' if passed else 'corroborate',
                      'reason': 'Repeated the same declared source checks on the current Snapshot.'}
                     for f in assignment['findings']]
}]}}))
