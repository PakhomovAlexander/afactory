import json,sys
r=json.load(sys.stdin)
claims=r['inputs']['review'][0]['payload']['claims']
assert len(claims)==1 and 'negative' in next(iter(claims.values()))['body']
open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\n    if offset < 0: raise ValueError("negative offset")\n    return items[offset:offset+limit]\n')
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Repaired negative-offset validation'}]}}))
