import json,os,sys,runpy
request=json.load(sys.stdin)
# The tree before the change is beside this one, read-only (ADR-0212), and the change's paths
# are a typed input the kernel built (ADR-0254); nothing else is written beside the tree.
assert not os.path.exists('../diff-subject.json')
typed=request['inputs']['diff_subject'][0]
assert typed['artifact_type']=='af/DiffSubject@1' and typed['snapshot_id']==request['inputs']['source'][0]['snapshot_id'], typed
subject=typed['payload']
assert subject['schema']=='af/diff-subject@1' and subject['base']=='../base', subject
assert subject['snapshot_id']==typed['snapshot_id'], subject
assert subject['changes'], subject
for change in subject['changes']:
    assert ('before' in change)==os.path.exists('../base/'+change['path']), change
    assert ('after' in change)==os.path.exists(change['path']), change
try:
    open('../base/pagination.py','a').close()
    raise SystemExit('the base tree is writable')
except PermissionError:
    pass
assert request['inputs']['checks'][0]['payload']['outcome']=='passed'
scope=runpy.run_path('pagination.py')
assert scope['paginate'](list(range(7)),2,3)==[2,3,4]
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':'passed','reason':'Verified offset and limit on the sealed source'}]}}))
