import json
import runpy
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) == {'source', 'subject', 'history', 'checks'}
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
    'reports': reports, 'benchmark_demands': [], 'disputes': []
}]}}))
