import json
import runpy
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) == {'source', 'requirements', 'checks'}
assert request['inputs']['checks'][0]['payload']['outcome'] == 'passed'
accepted = request['inputs']['requirements'][0]['payload'].get('specification') == {'schema': 'tutorial.pagination/1', 'module': 'pagination.py', 'function': 'paginate', 'offset_default': 0, 'limit_default': 2, 'bounds': 'nonnegative_integers', 'preserve_input': True}
try:
    paginate = runpy.run_path('pagination.py')['paginate']
    for size in range(9):
        values = list(range(size))
        for offset in range(11):
            for limit in range(7):
                expected = [value for index, value in enumerate(values) if offset <= index < offset + limit]
                accepted = accepted and paginate(values, offset, limit) == expected
    for offset, limit in [(-1, 2), (0, -1), (1.2, 2), (0, '2'), (True, 2)]:
        try:
            paginate([0, 1, 2], offset, limit)
            accepted = False
        except ValueError:
            pass
except Exception:
    accepted = False
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'result': [{
    'outcome': 'passed' if accepted else 'failed',
    'reason': 'Checked the exact tutorial requirements, finite pagination cases and invalid bounds on the sealed source.'
}]}}))
