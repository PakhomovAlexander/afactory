import json,sys
r=json.load(sys.stdin)
assert r['inputs']['checks'][0]['payload']['outcome']=='passed'
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'verdict':'approve','summary':'Pagination verified by bugs','reports':[],'benchmark_demands':[],'disputes':[]}]}}))
