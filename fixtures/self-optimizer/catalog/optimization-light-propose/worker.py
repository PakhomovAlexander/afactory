import json, sys
request = json.load(sys.stdin)
profile = request['inputs']['profile'][0]
recipes = request['inputs']['recipes'][0]
diagnostic = request['inputs']['diagnostic'][0]
selected = diagnostic['payload']['selected_recipe_id']
if selected == 'context_retrieval_dedup':
    writable = request['inputs']['configuration'][0]['payload']['files']
    path = next(path for path in sorted(writable) if path.endswith('/instructions.md'))
    before = writable[path]['text']
    lines = before.splitlines(keepends=True)
    after = ''.join(line for index, line in enumerate(lines) if index == 0 or line != lines[index - 1])
    if after == before:
        raise SystemExit('fixture expected adjacent duplicate instructions')
    edits = {path: {'text': after, 'executable': False}}
    hypothesis = 'Remove one harmless duplicated instruction while retaining every required instruction.'
elif selected == 'sandbox_dependency_cache':
    edits = {'.af/cache/cargo.json': {'text': '{"schema":"af.sandbox-cache-selection/1","kind":"cargo"}\n', 'executable': False}}
    hypothesis = 'Select the administrator-approved bounded Cargo snapshot for the candidate arm.'
else:
    edits = {'.af/artifact-reuse/receipt.json': {'text': '{"schema":"af.artifact-reuse-request/1","reuse":"source_snapshot"}\n', 'executable': False}}
    hypothesis = 'Request trusted preparation of the current CAS artifact identity while fresh verification still runs.'
proposal = {
    'schema': 'af.optimization-proposal/1',
    'profile_id': profile['artifact_id'],
    'diagnostic_id': diagnostic['artifact_id'],
    'recipe_catalog_id': recipes['artifact_id'],
    'recipe_id': selected,
    'hypothesis': hypothesis,
    'edits': edits,
    'expected': {
        'comparable_future_runs': diagnostic['payload']['expected_comparable_workload'],
        'gross_token_savings_per_run': '1',
        'gross_time_savings_ms_per_run': '1',
        'recurring_tokens_per_run': '0',
        'recurring_time_ms_per_run': '0',
        'maximum_validation_tokens': '1000',
        'maximum_validation_time_ms': '60000'
    }
}
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'proposal':[proposal]}}))
