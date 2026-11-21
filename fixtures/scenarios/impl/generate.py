"""Native test authoring helper: no model/service configuration is activated."""
import importlib.util
import json
from pathlib import Path
import sys
import subprocess

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
    if role == 'implementer' and mode.startswith('native-'):
        program = r"""
import json, sys
from pathlib import Path
request = json.load(sys.stdin)
marker = Path('repair-stage')
stage = int(marker.read_text()) + 1 if marker.exists() else 0
if stage:
    evidence = request['inputs']['requirements'][0]['payload']['specification']['scenario_native_outcome']
    findings = request['inputs']['findings'][0]['payload']
    assert findings['continues']['result_id'] == evidence['receipt']['result_id']
    assert findings['continues']['snapshot_id'] == evidence['repair']['snapshot_id']
    Path('seen-findings.json').write_text(json.dumps(findings, sort_keys=True))
marker.write_text(str(stage))
failed_gate = MODE == 'native-gates' and stage < 2 or MODE in ('native-mixed', 'native-verifier') and stage == 1 or MODE == 'native-terminal'
Path('pagination.py').write_text('def paginate(items, offset=0, limit=2):\n    return ' + ('[]' if failed_gate else 'items[offset:offset+limit]') + '\n')
if stage >= 2 or MODE == 'native-verifier':
    Path('NOTES.md').write_text('paginate(items, offset, limit) returns one page.\n')
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Native stage ' + str(stage)}]}}))
""".replace('MODE', repr(mode))
    if role == 'implementer' and mode == 'native-kernel':
        # A real unchanged candidate makes native diff review refuse an empty Subject.
        program = "import json; print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Unchanged candidate'}]}}))"
    if role == 'implementer' and mode == 'native-exhausted':
        # The native Attempt wall ends actual execution and seals its available partial tree.
        program = program.replace("print(json.dumps(", "import time; time.sleep(60)\nprint(json.dumps(")
    if role == 'implementer' and mode == 'native-output-refused':
        # This is an actual command Worker output-admission refusal, not a model refusal.
        program = "import json; print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[]}}))"
    if role == 'evaluator' and mode in ('native-verifier', 'native-review', 'native-mixed'):
        condition = ("request['inputs']['requirements'][0]['payload'].get('specification', {}).get('independent_verifier') and int(open('repair-stage').read()) == 0"
                     if mode == 'native-verifier' else "int(open('repair-stage').read()) < 2")
        program = program.replace("'outcome':'passed'", "'outcome':('failed' if " + condition + " else 'passed')")
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

if mode != 'models':
    # Read-only reviewed verification; Task base is pinned separately to the original commit.
    text = (out / 'implementation/pipeline.toml').read_text()
    text = text.replace(namespace + '/implementation', namespace + '/verification')
    text = text.replace('kinds = ["implement"]', 'kinds = ["verification"]')
    text = text.replace('kind = "derived_from"', 'kind = "same_as"')
    # Review the complete source; the independently declared Task base still supplies
    # ../base and the exact diff_subject to every judging Worker.
    text = text.replace('[nodes.inputs.base]\nkind = "input"\nport = "source"\n', '')
    text = text.replace('artifact_type = "af/ReviewedImplementation@1"',
                        'artifact_type = "af/VerificationResult@1"')
    text = text.replace('[outputs.verification]\nkind = "node"\nnode = "accept"',
                        '[outputs.verification]\nkind = "node"\nnode = "accept_goal"')
    text = text.replace('[coverage.verified]\nkind = "node"\nnode = "accept"',
                        '[coverage.verified]\nkind = "node"\nnode = "accept_goal"')
    text = text.replace('independent_from = ["implementer"]', 'independent_from = []')
    text = text.replace('covers = ["goal"]', 'covers = []')
    text = text[:text.index('[coverage.goal]')] + text[text.index('[coverage.verified]'):]
    begin = text.index('[slots.implementer]')
    end = text.index('[[nodes]]\nid = "review"', begin)
    text = text[:begin] + text[end:]
    text = text.replace('kind = "node"\nnode = "seal"\nport = "snapshot"',
                        'kind = "input"\nport = "source"')
    text = text.replace('[outputs.snapshot]\nkind = "node"\nnode = "accept"\nport = "snapshot"',
                        '[outputs.snapshot]\nkind = "input"\nport = "source"')
    verification = out / 'verification'
    verification.mkdir()
    (verification / 'pipeline.toml').write_text(text)
    kind = out / 'verification-kind'
    kind.mkdir()
    (kind / 'kind.toml').write_text('schema = "af.task-kind/3"\nname = "' + namespace +
        '/verification-kind"\nversion = "1.0.0"\nkind = "verification"\nprofile = "implementation"\n')
    catalog = (repo / '.af/task-catalog.toml').read_text()
    for name, path in [('verification', verification), ('verification-kind', kind)]:
        digest = subprocess.check_output([af, 'catalog', 'digest', str(path)],
            text=True).strip()
        catalog += '\n[packages."' + namespace + '/' + name + '"]\nversion = "1.0.0"\ndigest = "' + digest + \
            '"\npath = "' + str(path.relative_to(repo)) + '"\n'
    catalog += '\n[kinds]\nverification = "' + namespace + '/verification-kind"\n'
    (repo / '.af/task-catalog.toml').write_text(catalog)

if mode == 'native-blocked':
    policy = repo / '.af/code-policy.toml'
    text = policy.read_text()
    assert 'program = "/usr/bin/python3"' in text
    policy.write_text(text.replace('program = "/usr/bin/python3"',
                                   'program = "/definitely/missing/af834-local-gate"'))
