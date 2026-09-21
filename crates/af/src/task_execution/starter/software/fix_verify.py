import json
import runpy
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) == {'source', 'repair', 'checks'}
assert request['inputs']['checks'][0]['payload']['outcome'] == 'passed'
context = request['inputs']['repair'][0]['payload']
checks = {'Preserve offset and limit semantics': True, 'Reject invalid pagination bounds': True}
try:
    paginate = runpy.run_path('pagination.py')['paginate']
    for offset in range(10):
        for limit in range(10):
            values = list(range(8))
            if paginate(values, offset, limit) != values[offset:offset + limit]:
                checks['Preserve offset and limit semantics'] = False
    for offset, limit in [(-1, 2), (0, -1), (None, 2), (0, 0.5), (True, 2)]:
        try:
            paginate([0, 1, 2], offset, limit)
            checks['Reject invalid pagination bounds'] = False
        except ValueError:
            pass
except Exception:
    checks = {name: False for name in checks}
claims = {}
for finding, claim in context['claims'].items():
    known = claim['file'] == 'pagination.py' and claim['title'] in checks
    outcome = ('positive' if checks[claim['title']] else 'negative') if known else 'inconclusive'
    claims[finding] = {
        'expected_view_id': claim['current_view_id'], 'attestation_id': claim['attestation_id'],
        'outcome': outcome,
        'reason': 'Checked this preserved pagination claim on current S2.' if known else 'This tutorial verifier cannot assess the supplied claim.'
    }
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'result': [{
    'continuation_id': context['continuation_id'],
    'subject_id': context['continuation']['current_subject_id'], 'claims': claims
}]}}))
