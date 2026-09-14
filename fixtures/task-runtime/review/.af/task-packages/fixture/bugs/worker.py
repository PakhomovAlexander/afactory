import json,sys
r=json.load(sys.stdin)
assert r['inputs']['checks'][0]['payload']['outcome']=='passed'
print("{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"result\":[{\"verdict\":\"request-changes\",\"summary\":\"Fixture review\",\"reports\":[],\"benchmark_demands\":[],\"disputes\":[]}]}}")
