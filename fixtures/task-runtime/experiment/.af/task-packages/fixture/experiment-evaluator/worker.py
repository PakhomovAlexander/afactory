import json,sys
request=json.load(sys.stdin)
assert set(request['inputs'])=={'checks','comparison','requirements','source'}
assert request['inputs']['checks'][0]['payload']['outcome']=='passed'
# The kernel dispatches this evaluator only after a passed comparison; it reads, never writes, it.
assert request['inputs']['comparison'][0]['payload']['outcome']=='passed'
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':'passed','reason':'The kernel comparison passed the declared objective on the sealed source'}]}}))
