import json,sys
r=json.load(sys.stdin)
assert r['inputs']['checks'][0]['payload']['outcome']=='passed'
assert r['inputs']['assignment'][0]['payload']['findings']==[]
print("{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"result\":[{\"reports\":[{\"severity\":\"major\",\"file\":\"lib.rs\",\"line\":1,\"title\":\"Missing behavior\",\"body\":\"The implementation omits the required behavior\",\"fix\":\"Implement the requested behavior\",\"confidence\":0.9}],\"benchmark_demands\":[],\"dispositions\":[]}]}}")
