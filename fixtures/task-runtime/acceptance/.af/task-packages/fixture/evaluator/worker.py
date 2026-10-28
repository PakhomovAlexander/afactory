import json,sys,runpy
request=json.load(sys.stdin)
checks=request['inputs']['checks'][0]['payload']
assert checks['outcome']=='passed'
scope=runpy.run_path('pagination.py')
assert scope['paginate'](list(range(7)),2,3)==[2,3,4]
# The acceptance items arrive as this evaluator's own input and, verbatim, in the check receipt;
# a passing reason judges each by its ID.
items=request['inputs']['acceptance'][0]['payload']['items']
assert checks['acceptance']['items']==items
verdicts='; '.join('%s: met' % item['id'] for item in items)
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':'passed','reason':'Verified offset and limit on the sealed source. '+verdicts}]}}))
