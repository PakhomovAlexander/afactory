import json,sys,pathlib,os,hashlib
r=json.load(sys.stdin)
a=r['inputs']['assignment'][0]['payload']
s=r['inputs']['subject'][0]['payload']
assert a['reviewer']=="bugs"
assert all(f['source']=="bugs" for f in a['findings'])
assert len(a['findings']) == (1 if s['round']==2 and "bugs"=='correctness' else 0)
if 'change_scope' in s:
    patch=s['change_scope']['patch']
    b=pathlib.Path(patch['path']).read_bytes()
    assert len(b)==patch['bytes'] and len(b)>780*1024 and b'// readable source change' in b
    assert len(json.dumps(r))<65536 and 'canonical_patch_base64' not in json.dumps(r)
    assert 'sha256:'+hashlib.sha256(b'review.kernel/content-id/v1\0'+b).hexdigest()==patch['content_id']
    print('read exact patch bytes: '+json.dumps(patch,sort_keys=True),file=sys.stderr)
    if "valid"=='mutated':
        os.chmod(patch['path'],0o644)
        pathlib.Path(patch['path']).write_bytes(b'changed')
stage={'reports':[],'benchmark_demands':[],'dispositions':[]}
if s['round']==1 and "bugs"=='correctness':
    stage['reports']=[{'severity':'major','file':'lib.rs','line':1,'title':'Missing behavior','body':'The implementation omits the required behavior','fix':'Implement it','confidence':0.9}]
    stage['benchmark_demands']=[{'claim':'Runtime is bounded','why':'Large inputs matter','suggested_method':'Measure scaling'}]
if s['round']==2:
    stage['dispositions']=[{'finding_id':f['finding_id'],'position':'not_reproduced','reason':'Checked the same declared scope'} for f in a['findings']]
    if "bugs"=='correctness':
        if "valid"=='missing': stage['dispositions']=[]
        if "valid"=='duplicate': stage['dispositions']*=2
        if "valid"=='unassigned': stage['dispositions'].append({'finding_id':'not-assigned','position':'not_reproduced','reason':'Unexpected claim'})
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[stage]}}))
