import json
import sys

request = json.load(sys.stdin)
assert set(request['inputs']) == {'source', 'requirements'}
assert request['inputs']['requirements'][0]['payload'].get('specification') == {'schema': 'tutorial.pagination/1', 'module': 'pagination.py', 'function': 'paginate', 'offset_default': 0, 'limit_default': 2, 'bounds': 'nonnegative_integers', 'preserve_input': True}
with open('pagination.py', 'w') as target:
    target.write('''def paginate(items, offset=0, limit=2):
    if type(offset) is not int or type(limit) is not int:
        raise ValueError("offset and limit must be integers")
    if offset < 0 or limit < 0:
        raise ValueError("offset and limit must be nonnegative")
    return items[offset:offset + limit]
''')
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {
    'report': [{'summary': 'Implemented bounded pagination for the supplied tutorial goal.'}]
}}))
