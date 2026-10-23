import json,os,sys,runpy
request=json.load(sys.stdin)
# The tree before the change is beside this one, read-only, with the change's paths (ADR-0198).
subject=json.load(open('../diff-subject.json'))
assert subject['schema']=='af/diff-subject@1' and subject['base']=='../base', subject
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
