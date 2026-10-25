import json,sys
request=json.load(sys.stdin)
findings=request['inputs'].get('findings')
if findings is None:
    open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\n    return items[offset:offset+limit]\n')
    summary='Implemented pagination'
else:
    # A continuation starts from the reviewed candidate, never the base, and reads the
    # findings as a typed input document.
    source=open('pagination.py').read()
    assert 'offset:offset+limit' in source, source
    document=findings[0]
    assert document['artifact_type']=='af/TaskReviewFindings@1', document
    open('seen-findings.json','w').write(json.dumps(document['payload'],sort_keys=True))
    open('NOTES.md','w').write('paginate(items, offset, limit) returns one page.\n')
    summary='; '.join(f"{f['id']}: fixed" for f in document['payload']['findings'])
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':summary}]}}))
