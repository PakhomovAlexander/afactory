ARM = 'baseline'
import json, subprocess, sys
from pathlib import Path
request = json.load(sys.stdin)
case = request['inputs']['case'][0]['payload']
policy = json.loads(Path('.af/optimization-policy.json').read_text())
for check in policy['checks'].values():
    result = subprocess.run([sys.executable, '-B', check, case['input_path']], capture_output=True, timeout=10)
    if result.returncode:
        sys.exit(1)
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'receipt':[{'schema':'af.optimization-arm-receipt/1','arm':ARM}]}}))
