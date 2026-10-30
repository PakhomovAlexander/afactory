#!/usr/bin/env python3
"""Provision the issue-triage labels (docs/design/issue-triage.md §4) and migrate the two legacy
ones; dry-run unless --apply is explicit. Never deletes a label."""
import argparse
import json
import shlex
import subprocess
import sys

sys.dont_write_bytecode = True

# Legacy issue label -> its triage class; --migrate touches only these two names.
MIGRATIONS = {'Agent UX Improvement': 'class/aix', 'test-perf': 'class/sdlc'}


def definitions():
    return [
        ('class/bug', 'EE366A', 'af does something other than its documentation, contract or ADR says'),
        ('class/perf', 'D93F0B', 'A measured cost (time, tokens, disk, memory) higher than it should be'),
        ('class/feature', '5195F5', 'A capability or contract that does not exist'),
        ('class/aix', '7057FF', 'Agent experience: output shapes, next-action lines, JSON documents, prompts'),
        ('class/uix', 'B16CFF', 'Human experience: TUI, help text, first screens, onboarding'),
        ('class/sdlc', '0E8A16', 'Tests, CI, gates, release, repository process, test speed and flakes'),
        ('class/security', '000000', 'Trust-boundary defect; handled privately by the owner'),
        ('class/docs', '1D76DB', 'Documentation alone'),
        ('class/research', 'C5DEF5', 'A study whose output is a report, not code'),
        ('class/decision', 'FBCA04', 'Nothing moves until the owner chooses'),
        ('size/S', '36EEA8', 'One module, repro present, no schema/contract/budget change, no fixture, '
                             'no ADR; light pipeline'),
        ('size/M', '9BE07A', 'Understood change not L or S: 2-3 crates, new fixture, additive field, '
                             'bounded ADR; default pipeline'),
        ('size/L', 'F0B429', 'Contract, schema-version, budget or sandbox change, or several Tasks; '
                             'design review first'),
        ('size/XL', 'EE366A', 'Needs a design document and owner decisions before any code'),
        ('pipeline/light', '36EEA8', 'default-light/implementation: Opus 5.5 medium + correctness + evaluator'),
        ('pipeline/default', '9BE07A', 'default/implementation: Opus 5.5 high; Terra, Sol, Astra reviewers; '
                                       'Astra evaluator'),
        ('pipeline/design', 'F0B429', 'Astra xhigh design review before the first implementing Task'),
        ('pipeline/hand', '999999', 'A config edit, doc line or GitHub action; no model Task'),
        ('triage/pending', 'E4E669', 'Not yet triaged, or re-triage requested'),
        ('triage/needs-info', 'FBCA04', 'Triage could not reach a confident class or size; '
                                        'the comment asks one question'),
        ('triage/disputed', 'D93F0B', 'The two triagers disagree on class or size; owner resolves by label'),
        ('triage/maybe-fixed', '0E8A16', 'Sweep found an explicit completed-fix reference on main; owner closes'),
        ('triage/duplicate', '999999', 'Both triagers named the same open issue as the subject'),
        ('triage/held', '000000', 'Under private owner review; no runner touches it'),
    ]


def repo_args(repo):
    return ['--repo', repo] if repo else []


def commands(repo=None):
    return [['gh', 'label', 'create', name, '--force', '--color', color,
             '--description', description, *repo_args(repo)]
            for name, color, description in definitions()]


def migration_commands(repo=None):
    """Read the open issues carrying a legacy label (a read, run even in dry-run) and return one
    edit per issue and legacy label: add its class, remove that legacy label, nothing else."""
    edits = []
    for legacy, target in MIGRATIONS.items():
        for issue in open_issues_with_label(legacy, repo):
            if legacy in [label['name'] for label in issue['labels']]:
                edits.append(['gh', 'issue', 'edit', str(issue['number']), '--add-label', target,
                              '--remove-label', legacy, *repo_args(repo)])
    return edits


PAGE = 100


def open_issues_with_label(legacy, repo=None):
    """Every open issue carrying `legacy`, read through the REST endpoint page by page until a
    short page, so no issue beyond a listing limit is left behind."""
    if repo is None:
        result = subprocess.run(['gh', 'repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner'],
                                capture_output=True, text=True)
        if result.returncode:
            sys.exit(f'gh repo view failed:\n{result.stderr}')
        repo = result.stdout.strip()
    issues = []
    page = 1
    while True:
        result = subprocess.run(['gh', 'api', f'repos/{repo}/issues', '--method', 'GET',
                                 '-f', 'state=open', '-f', f'labels={legacy}', '-f', f'per_page={PAGE}',
                                 '-f', f'page={page}'], capture_output=True, text=True)
        if result.returncode:
            sys.exit(f'gh api repos/{repo}/issues (label {legacy!r}, page {page}) failed:\n{result.stderr}')
        batch = [row for row in json.loads(result.stdout) if 'pull_request' not in row]
        issues.extend(batch)
        if len(json.loads(result.stdout)) < PAGE:
            return issues
        page += 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--repo')
    parser.add_argument('--migrate', action='store_true',
                        help='move open issues off the legacy labels '
                             f'({", ".join(f"{a!r} -> {b}" for a, b in MIGRATIONS.items())})')
    args = parser.parse_args()
    for command in migration_commands(args.repo) if args.migrate else commands(args.repo):
        if args.apply:
            subprocess.run(command, check=True)
        else:
            print(shlex.join(command))


if __name__ == '__main__':
    main()
