import json,sys
r=json.load(sys.stdin)
assert r['inputs']['checks'][0]['payload']['outcome']=='passed'
assert r['inputs']['assignment'][0]['payload']['findings']==[]
reports=[{'severity':'minor','file':'src/fixture.txt','line':n,'title':f'Fixture finding {n}','body':'The declared fixture finding','fix':'Repair the declared finding','confidence':0.9} for n in range(1,101)]
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'reports':reports,'benchmark_demands':[],'dispositions':[]}]}}))
