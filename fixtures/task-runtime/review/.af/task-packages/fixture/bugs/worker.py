import json,sys
r=json.load(sys.stdin)
assert r['inputs']['checks'][0]['payload']['outcome']=='passed'
assert r['inputs']['assignment'][0]['payload']['findings']==[]
print("{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"result\":[{\"verdict\":\"request-changes\",\"summary\":\"Fixture review\",\"reports\":[],\"benchmark_demands\":[],\"dispositions\":[]}]}}")
