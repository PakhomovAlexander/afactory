import json
import runpy
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) in [{'source', 'subject', 'history', 'checks', 'assignment'}, {'source', 'subject', 'history', 'checks', 'assignment', 'requirements'}]
assignment = request['inputs']['assignment'][0]['payload']
assert assignment['reviewer'] == 'correctness'
assert all(f['source'] == 'correctness' for f in assignment['findings'])
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
    'reports': reports, 'benchmark_demands': [],
    'dispositions': [{'finding_id': f['finding_id'],
                      'position': 'not_reproduced' if passed else 'corroborate',
                      'reason': 'Repeated the same declared source checks on the current Snapshot.'}
                     for f in assignment['findings']]
}]}}))
