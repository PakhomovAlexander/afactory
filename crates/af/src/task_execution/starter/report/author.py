import json, sys
r = json.load(sys.stdin)
i = r['inputs']
assert {'requirements', 'source', 'sources'} <= set(i), sorted(i)
sources = i['sources'][0]['payload']['sources']
# The working directory is a clone of the exact source Snapshot; nothing written here is kept.
with open('README.md', encoding='utf-8') as readme:
    lines = readme.read().splitlines()
with open('report.json', encoding='utf-8') as task:
    task_id = json.load(task)['task_id']
body = 'README.md has %d lines and report.json names the Task %s.' % (len(lines), task_id)
notes = [sources[name]['text'] for name in sorted(sources)]
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'draft': [{
    'schema': 'af.document-draft/2', 'title': 'Starter layout',
    'sections': [{'heading': 'Findings', 'body': '\n\n'.join([body] + notes)}],
    'citations': sorted(sources),
    # A draft lists its citations sorted by path, then line.
    'repository_citations': sorted([
        {'path': 'README.md'},
        {'path': 'README.md', 'line': len(lines)},
        {'path': 'report.json'},
    ], key=lambda c: (c['path'], c.get('line', 0)))}]}}))
