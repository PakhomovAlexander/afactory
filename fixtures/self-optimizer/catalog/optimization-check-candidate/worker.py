ARM = 'candidate'
import json, os, subprocess, sys
from pathlib import Path
request = json.load(sys.stdin)
case = request['inputs']['case'][0]['payload']
policy = json.loads(Path('.af/optimization-policy.json').read_text())
configuration = request['inputs']['configuration'][0]['payload']
if configuration.get('sandbox_cache_kind') == 'cargo':
    cargo_home = Path(os.environ['CARGO_HOME'])
    assert os.environ.get('CARGO_NET_OFFLINE') == 'true'
    assert cargo_home.name == 'cargo' and '.af-cache' in cargo_home.parts
    assert any(path.is_file() for path in cargo_home.rglob('*'))
for check in policy['checks'].values():
    result = subprocess.run([sys.executable, '-B', check, case['input_path']], capture_output=True, timeout=10)
    if result.returncode:
        sys.exit(1)
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'receipt':[{'schema':'af.optimization-arm-receipt/1','arm':ARM}]}}))
