import json
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) == {'source', 'requirements', 'review'}
assert request['inputs']['requirements'][0]['payload'].get('specification') == {'schema': 'tutorial.pagination/1', 'module': 'pagination.py', 'function': 'paginate', 'offset_default': 0, 'limit_default': 2, 'bounds': 'nonnegative_integers', 'preserve_input': True}
claims = request['inputs']['review'][0]['payload']['claims']
assert claims and all(value['file'] == 'pagination.py' for value in claims.values())
with open('pagination.py', 'w') as target:
    target.write('''def paginate(items, offset=0, limit=2):
    if type(offset) is not int or type(limit) is not int or offset < 0 or limit < 0:
        raise ValueError("invalid pagination bounds")
    return items[offset:offset + limit]
''')
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {
    'report': [{'summary': 'Repaired the captured pagination findings.'}]
}}))
