import json, subprocess, sys
from pathlib import Path
request = json.load(sys.stdin)
inputs = request['inputs']
config = inputs['configuration'][0]['payload']
comparison = inputs['comparison'][0]['payload']
policy = json.loads(Path('.af/optimization-policy.json').read_text())
passed = all(subprocess.run([sys.executable, '-B', check, case['input_path']], capture_output=True, timeout=10).returncode == 0 for case in policy['experiment']['cases'] for check in policy['checks'].values())
evaluation = {'schema':'af.optimization-evaluation/1', 'task_revision_id':config['task_revision_id'], 'requirements_id':config['requirements_id'], 'candidate_snapshot_id':config['candidate_snapshot_id'], 'specification_id':comparison['specification_id'], 'prepared_id':comparison['prepared_id'], 'comparison_id':inputs['comparison'][0]['artifact_id'], 'conclusion':'accepted' if passed and comparison['conclusion']=='accepted' else 'rejected', 'reason':'independent_candidate_checks'}
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'evaluation':[evaluation]}}))
