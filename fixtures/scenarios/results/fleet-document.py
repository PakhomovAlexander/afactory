#!/usr/bin/env python3
"""Example author-side fleet document producer using only native read-only Task inspection.

Not a dashboard schema or a recovery adapter. Run only against explicitly selected Stores.
No remote hosts, notifications, retries or execution are performed by this example.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--af', required=True, type=Path)
    parser.add_argument('--environment', required=True)
    parser.add_argument('--repo', required=True, type=Path)
    parser.add_argument('--task-state', required=True, type=Path)
    parser.add_argument('--task', required=True)
    args = parser.parse_args()
    af = args.af.resolve(strict=True)
    repo = args.repo.resolve(strict=True)
    state = args.task_state.resolve(strict=True)
    receipts = []

    def read(command, expected):
        result = subprocess.run([str(af), 'task', *command, '--repo', str(repo),
                                 '--state', str(state), '--json'], capture_output=True,
                                text=True, check=True, timeout=30)
        value = json.loads(result.stdout)
        if value.get('schema') != expected or value.get('task_id') != args.task:
            raise ValueError('Unexpected native Task identity/version')
        receipts.append({'command': ['af', 'task', *command, '--repo', str(repo),
                                     '--state', str(state), '--json'],
                         'sha256_stdout': hashlib.sha256(result.stdout.encode()).hexdigest()})
        return value

    shown = read(['show', args.task], 'af/task-inspection@20')
    status = read(['status', args.task], 'af/task-status@2')
    observed = time.time_ns() // 1_000_000
    plan = read(['explain', args.task, '--plan', shown['plan_id']], 'af/task-plan-inspection@6')
    after = read(['show', args.task], 'af/task-inspection@20')
    if (not plan['current_plan'] or plan['task_revision_id'] != shown['revision_id']
            or any(after[k] != shown[k] for k in ['revision_id', 'plan_id', 'state', 'chargeable_tokens'])
            or status['state'] != shown['state']
            or status['charged_tokens'] != shown['chargeable_tokens']):
        raise ValueError('Task changed during observation; capture again, never mix revisions')
    provenance = {'repository': str(repo), 'store': str(state), 'task': args.task,
                  'revision': shown['revision_id'], 'plan': shown['plan_id'],
                  'recorded_event_ids': plan['recorded_event_ids'],
                  'requirements': plan['revision']['inputs'].get('requirements'),
                  'read_receipts': receipts}
    document = {'schema': 'af/result-document@1', 'observed_unix_ms': observed,
                'stale_after_ms': 60000, 'blocks': [
                    {'kind': 'markdown', 'content': '## Recorded fleet observation\n\n'
                     'Purpose is the exact native Task goal, including captured issue context when '
                     'present. Missing issue references stay missing. Elapsed ms uses the native '
                     'Task clock: to now while running, to its last event once stopped; it is not '
                     'summed Attempt time. Tokens are recorded charged usage, excluding reserves; '
                     'unknown-usage Attempts remain explicitly unknown. No recovery was executed.'},
                    {'kind': 'table', 'title': 'Selected local Task',
                     'columns': ['Environment', 'Store', 'Task', 'Purpose / captured issue context',
                                 'Native state / terminal cause', 'Elapsed ms (native clock)',
                                 'Recorded charged tokens', 'Unknown-usage Attempts', 'Provenance'],
                     'rows': [[args.environment, str(state), args.task, plan['revision']['goal'],
                               status['state'], status['elapsed_ms'], status['charged_tokens'],
                               status['unknown_usage_attempts'], provenance]]}]}
    print(json.dumps(document, ensure_ascii=False, separators=(',', ':')))


if __name__ == '__main__':
    main()
