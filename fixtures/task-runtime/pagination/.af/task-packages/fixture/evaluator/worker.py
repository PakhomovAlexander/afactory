import json,sys,runpy
request=json.load(sys.stdin)
assert request['inputs']['checks'][0]['payload']['outcome']=='passed'
scope=runpy.run_path('pagination.py')
assert scope['paginate'](list(range(7)),2,3)==[2,3,4]
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':'passed','reason':'Verified offset and limit on the sealed source'}]}}))
