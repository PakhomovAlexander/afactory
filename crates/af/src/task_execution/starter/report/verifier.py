import json, string, sys
r = json.load(sys.stdin)
i = {k: v[0] for k, v in r['inputs'].items()}
assert {'requirements', 'document', 'checks', 'source', 'sources'} <= set(i), sorted(i)
snapshot = i['source']['snapshot_id']
checks = i['checks']['payload']
assert checks['outcome'] == 'passed' and checks['source_snapshot_id'] == snapshot
text = i['document']['payload']['text']
plain = text
for c in string.punctuation:
    plain = plain.replace('\\' + c, c)
# The verifier reads the same Snapshot, read-only, and checks the report's claims against it.
with open('README.md', encoding='utf-8') as readme:
    lines = readme.read().splitlines()
accepted = (i['requirements']['payload']['text'] == 'Report the starter layout from its committed files.'
    and '# Starter layout' in plain and '## Findings' in plain
    and 'README.md has %d lines' % len(lines) in plain
    and '`README.md:%d`' % len(lines) in text and '`report.json`' in text)
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'result': [{
    'document_id': i['document']['artifact_id'], 'sources_id': i['sources']['artifact_id'],
    'requirements_id': i['requirements']['artifact_id'],
    'check_receipt_id': i['checks']['artifact_id'], 'source_snapshot_id': snapshot,
    'outcome': 'passed' if accepted else 'failed',
    'summary': 'Cited README lines and the Task file checked against the same Snapshot.'}]}}))
