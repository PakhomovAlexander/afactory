import json,sys
request=json.load(sys.stdin)
# The goal names the candidate's files after `files=`, as one JSON object of path to text.
text=request['inputs']['requirements'][0]['payload']['text']
files=json.loads(text.split('files=',1)[1])
for path,body in sorted(files.items()):
    open(path,'w').write(body)
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Wrote '+', '.join(sorted(files))}]}}))
