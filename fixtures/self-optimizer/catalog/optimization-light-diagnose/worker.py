import json, sys
request = json.load(sys.stdin)
profile = request['inputs']['profile'][0]
payload = profile['payload']
eligible = payload['eligible_recipe_ids']
if not eligible:
    raise SystemExit('no installed recipe is applicable to captured development evidence')
selected = 'sandbox_dependency_cache' if 'sandbox_dependency_cache' in eligible else eligible[0]
diagnostic = {
    'schema': 'af.optimization-diagnostic/1',
    'profile_id': profile['artifact_id'],
    'selected_recipe_id': selected,
    'avoidable_cost': 'The fixture development evidence identifies one repeated routine cost.',
    'evidence': ['captured aggregate development profile'],
    'expected_comparable_workload': payload['comparable_future_runs']
}
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'diagnostic':[diagnostic]}}))
