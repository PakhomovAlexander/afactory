"""Native test authoring helper: no model/service configuration is activated."""
import importlib.util
import json
from pathlib import Path
import sys

root, repo, af, namespace, mode = sys.argv[1:]
root, repo = Path(root), Path(repo)
spec = importlib.util.spec_from_file_location('recipe', root / 'scripts/scenario-impl.py')
recipe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recipe)
base = repo / '.af/task-packages/fixture'
workers = {}
for role in recipe.ROLES:
    program = (base / role / 'worker.py').read_text()
    if role == 'implementer':
        program = program.replace("print(json.dumps(", """evidence = request['inputs']['requirements'][0]['payload'].get('specification', {}).get('scenario_evidence')
if evidence is not None:
    assert evidence['condition'] == 'checks_completed'
    packet = json.loads(evidence['evidence'])
    assert packet['state'] == 'repairable'
    open('seen-ci.json','w').write(json.dumps(packet, sort_keys=True))
print(json.dumps(""")
    if role == 'correctness':
        # Keep the ordinary Reviewer Result and add executable evidence on the exact source.
        program = program.replace("print(json.dumps(", "reply = ").rstrip()
        # The original expression ends in two closing parentheses (dumps and print).
        program = program[:-2] + '\n'
        program += '''
import subprocess, tempfile, shutil
from pathlib import Path
with tempfile.TemporaryDirectory() as scratch:
    # This small fixture's build is Python's compiler, installation copies its bytecode.
    compiled = Path(scratch) / 'pagination.pyc'
    import py_compile
    py_compile.compile('pagination.py', cfile=str(compiled), doraise=True)
    installed = Path(scratch) / 'private' / 'pagination.pyc'
    installed.parent.mkdir()
    shutil.copyfile(compiled, installed)
    test = subprocess.run([sys.executable, '-B', '-c',
        "import runpy,sys; s=runpy.run_path(sys.argv[1]); assert s['paginate'](list(range(7)),2,3)==[2,3,4]", str(installed)])
    assert test.returncode == 0
reply['outputs']['testing'] = [{'schema':'af/implementation-test-evidence@1',
    'snapshot_id':r['inputs']['source'][0]['snapshot_id'], 'status':'passed',
    'reason':'Compiled, privately installed and executed fixture bytecode',
    'commands':[{'purpose':purpose, 'command':command, 'exit_code':0, 'evidence':evidence}
        for purpose,command,evidence in [
            ('build', 'py_compile.compile pagination.py', 'Compiled sealed source'),
            ('install', 'shutil.copyfile compiled private/pagination.pyc', 'Copied exact compiled bytes'),
            ('test', 'python -B -c runpy.run_path private/pagination.pyc', 'offset=2 limit=3 returned [2,3,4]')]]}]
print(json.dumps(reply))
'''
    if role == 'evaluator':
        program = program.replace("scope=runpy", "assert request['inputs']['testing'][0]['payload']['status']=='passed'\nscope=runpy")
    compile(program, role, 'exec')
    runner = dict(kind='command', command=dict(program='/usr/bin/python3', args=[
        dict(value='-B', provenance='literal'), dict(value='-c', provenance='literal'),
        dict(value=program, provenance='literal')]))
    workers[role] = dict(runner=runner, attempt=dict(tokens=0, wall_ms=10000))
if mode == 'alternate':
    for worker in workers.values():
        worker['runner']['command']['args'].insert(0, dict(value='-I', provenance='literal'))
        worker['attempt']['wall_ms'] = 8000
team = dict(workers=workers, independence=dict(command_workers_by_package=True,
    distinct_principals=True, distinct_providers=False, distinct_models=False))
if mode == 'models':
    team = json.loads((root / 'fixtures/scenarios/impl/team.example.json').read_text())
out = repo / '.af/task-packages' / namespace
recipe.packages(out, Path(af), ['pagination'], team, namespace)
(repo / '.af/task-catalog.toml').write_bytes((out / 'catalog.toml').read_bytes())
