import json,sys
r=json.load(sys.stdin)
assert r['inputs']['checks'][0]['payload']['outcome']=='passed'
print("{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"result\":[{\"verdict\":\"request-changes\",\"summary\":\"Fixture review\",\"reports\":[{\"severity\":\"major\",\"file\":\"lib.rs\",\"line\":1,\"title\":\"Missing behavior\",\"body\":\"The implementation omits the required behavior\",\"fix\":\"Implement the requested behavior\",\"confidence\":0.9}],\"benchmark_demands\":[],\"disputes\":[]}]}}")
