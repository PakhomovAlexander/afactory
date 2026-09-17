#!/usr/bin/env python3
"""Summarize `gh run view ID --json databaseId,headSha,attempt,createdAt,startedAt,jobs` without billing guesses."""
import datetime
import json
import sys


def stamp(value):
    if not value or value.startswith('0001-'):
        return None
    return datetime.datetime.fromisoformat(value.replace('Z', '+00:00'))


def duration(start, end):
    return (end - start).total_seconds() if start and end else None


def report(run):
    jobs = []
    for job in run['jobs']:
        steps = [{
            'name': step['name'], 'conclusion': step['conclusion'],
            'elapsed_seconds': duration(stamp(step['startedAt']), stamp(step['completedAt'])),
        } for step in job['steps']]
        jobs.append({
            'name': job['name'], 'conclusion': job['conclusion'],
            'started_at': job['startedAt'], 'completed_at': job['completedAt'],
            'elapsed_seconds': duration(stamp(job['startedAt']), stamp(job['completedAt'])),
            'steps': steps,
        })
    complete = bool(jobs) and all(job['status'] == 'completed' for job in run['jobs'])
    ends = [stamp(job['completedAt']) for job in run['jobs']]
    ends = [end for end in ends if end]
    return {
        'schema': 'af.ci-run-economics/1', 'run_id': run.get('databaseId'),
        'commit': run.get('headSha'), 'attempt': run.get('attempt'),
        'complete': complete,
        'wall_seconds': duration(stamp(run.get('startedAt')), max(ends)) if complete and ends else None,
        'since_created_seconds': duration(stamp(run['createdAt']), max(ends)) if complete and ends else None,
        'runner_seconds': sum(job['elapsed_seconds'] or 0 for job in jobs)
        if complete and all(job['elapsed_seconds'] is not None or job['conclusion'] == 'skipped' for job in jobs)
        else None,
        'billed_runner_minutes': None, 'tokens': None,
        'note': 'Wall time starts at this attempt start, including its job queues/coordination. Since-created time may include prior attempts and waiting between reruns. Runner seconds sum parallel job intervals; neither is billed cost. Retain failed and superseded attempts separately.',
        'jobs': jobs,
    }


if __name__ == '__main__':
    with open(sys.argv[1]) as source:
        print(json.dumps(report(json.load(source)), indent=2))
