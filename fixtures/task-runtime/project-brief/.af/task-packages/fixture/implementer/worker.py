import json,sys
request=json.load(sys.stdin)
# A Brief arrives as one value of its declared port, with no Snapshot of its own.
for value in request['inputs'].get('project',[]):
    assert value['artifact_type']=='af/ProjectBrief@1' and 'snapshot_id' not in value
    assert value['payload']['schema']=='af/ProjectBrief@1'
open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\n    return items[offset:offset+limit]\n')
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Implemented pagination'}]}}))
