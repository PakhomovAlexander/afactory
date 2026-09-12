import json
import runpy
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) in [{'source', 'subject', 'history', 'checks'}, {'source', 'subject', 'history', 'checks', 'requirements'}]
if 'requirements' in request['inputs']:
    assert request['inputs']['requirements'][0]['payload'].get('specification', {}).get('schema') == 'tutorial.pagination/1'
assert request['inputs']['checks'][0]['payload']['outcome'] == 'passed'
passed = True
try:
    paginate = runpy.run_path('pagination.py')['paginate']
    for items in [[], [7], ['a', 'b', 'c'], list(range(17))]:
        original = items.copy()
        for offset in [0, 1, 3, 20]:
            for limit in [0, 1, 4, 21]:
                result = paginate(items, offset, limit)
                expected = []
                for index, value in enumerate(items):
                    if offset <= index < offset + limit:
                        expected.append(value)
                passed = passed and result == expected and items == original
except Exception:
    passed = False
reports = [] if passed else [{
    'severity': 'major', 'file': 'pagination.py', 'line': 1,
    'title': 'Preserve offset and limit semantics',
    'body': 'Pagination returned an incorrect window or changed the input sequence.',
    'fix': 'Return the requested window without mutating the input.', 'confidence': 1.0
}]
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'result': [{
    'verdict': 'approve' if passed else 'request-changes',
    'summary': 'Independent pagination window and input-preservation checks.',
    'reports': reports, 'benchmark_demands': [], 'disputes': []
}]}}))
