import json,sys
request=json.load(sys.stdin)
open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\n    return items[offset:offset+limit]\n')
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Implemented pagination'}]}}))
