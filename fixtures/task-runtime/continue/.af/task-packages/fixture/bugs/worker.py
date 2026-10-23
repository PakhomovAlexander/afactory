import json,os,sys
r=json.load(sys.stdin)
# The tree before the change is beside this one, read-only, with the change's paths (ADR-0203).
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
assert r['inputs']['checks'][0]['payload']['outcome']=='passed'
reports=[] if os.path.exists('NOTES.md') else [{'severity':'major','file':'pagination.py','line':1,'title':'paginate is undocumented','body':'Nothing says what offset and limit mean.','fix':'Add NOTES.md describing paginate.','confidence':0.9}]
# A declared base before the pinned catalog shows in the change: said as a minor finding, so a
# test can read which base the reviewer was given.
if any(change['path']=='.af/task-catalog.toml' for change in subject['changes']):
    reports.append({'severity':'minor','file':'pagination.py','line':1,'title':'the base predates the pinned catalog','body':'../base is the declared commit.','fix':'None needed.','confidence':0.5})
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'reports':reports,'benchmark_demands':[],'dispositions':[{'finding_id':f['finding_id'],'position':'not_reproduced','reason':'Checked the current Snapshot'} for f in r['inputs']['assignment'][0]['payload']['findings']]}]}}))
