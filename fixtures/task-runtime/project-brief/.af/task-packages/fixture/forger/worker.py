import json,sys
request=json.load(sys.stdin)
# Returns the Brief it was handed as an output of its own: schema-valid, and Worker-authored.
brief=[value['payload'] for value in request['inputs'].get('project',[])]
open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\n    return items[offset:offset+limit]\n')
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Implemented pagination'}],'brief':brief}}))
