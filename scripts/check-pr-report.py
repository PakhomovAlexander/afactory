#!/usr/bin/env python3
"""Require the `af task report` block in a pull request description (ADR-0142).

Every change to this repository is made through af Tasks, and its pull request description
carries the `af task report` of those Tasks. This check reads only the pull request event
payload GitHub hands the workflow (`$GITHUB_EVENT_PATH`): no API call, no token. It cannot
compare the report with the Store it came from, which lives on the author's machine; it
checks that a well-formed block is there.

A pull request opened by Dependabot, or from a `release/` branch, is exempt. Anything else
passes only with exactly one block: the begin marker, then the end marker, and between them
the summary table header with every column, at least one Task row, and the totals line.

    check-pr-report.py [--event PATH]      the event payload (default: $GITHUB_EVENT_PATH)
    check-pr-report.py --body FILE         a description on its own, as a local check
"""
import argparse
import json
import os
from pathlib import Path
import sys

BEGIN = '<!-- af-task-report:v1 -->'
END = '<!-- /af-task-report -->'
COLUMNS = ['Task', 'Kind', 'Pipeline', 'Outcome', 'Rounds', 'Attempts', 'Tokens',
           'Active time', 'Wall time']
TOTALS = '**Totals:**'
EXEMPT_AUTHORS = {'dependabot[bot]'}
EXEMPT_BRANCH_PREFIX = 'release/'
HOW = ('Run `af task report TASK_ID...` for the af Tasks that made this change (from the '
       'repository, with the same --state if you used one) and paste its whole output, both '
       'markers included, into the "af task report" section of the description.')


def cells(line):
    """The cells of one Markdown table row, or None for a line that is not one."""
    line = line.strip()
    if not line.startswith('|') or not line.endswith('|') or len(line) < 2:
        return None
    return [cell.strip() for cell in line[1:-1].split('|')]


def is_separator(row):
    return bool(row) and all(cell and set(cell) <= set(':-') and '-' in cell for cell in row)


def problems(body):
    """What is missing from `body`, as messages; an empty list passes."""
    lines = (body or '').splitlines()
    begins = [n for n, line in enumerate(lines) if line.strip() == BEGIN]
    ends = [n for n, line in enumerate(lines) if line.strip() == END]
    if not begins and not ends:
        return [f'no af task report block: the description has neither `{BEGIN}` nor `{END}`']
    if len(begins) > 1 or len(ends) > 1:
        return [f'{max(len(begins), len(ends))} af task report blocks: keep exactly one, '
                'covering every Task behind this change']
    if not begins:
        return [f'the af task report block has no begin marker `{BEGIN}`']
    if not ends:
        return [f'the af task report block has no end marker `{END}`']
    if ends[0] < begins[0]:
        return [f'the end marker `{END}` comes before the begin marker `{BEGIN}`']
    block = lines[begins[0] + 1:ends[0]]
    header = None
    for n, line in enumerate(block):
        row = cells(line)
        if row and len(set(row) & set(COLUMNS)) >= 3:
            header = n
            break
    if header is None:
        return ['the af task report block has no summary table: expected a header row with '
                + ', '.join(COLUMNS)]
    found = []
    missing = [column for column in COLUMNS if column not in cells(block[header])]
    if missing:
        found.append('the summary table is missing the column'
                     + ('s ' if len(missing) > 1 else ' ') + ', '.join(missing))
    rows = []
    separator = header + 1 < len(block) and is_separator(cells(block[header + 1]))
    if not separator:
        found.append('the summary table header is not followed by its `| --- |` separator row')
    else:
        for line in block[header + 2:]:
            row = cells(line)
            if row is None:
                break
            rows.append(row)
    if separator and not any(any(cell for cell in row) for row in rows):
        found.append('the summary table has no Task row')
    if not any(line.strip().startswith(TOTALS) for line in block[header:]):
        found.append(f'the af task report block has no totals line starting with `{TOTALS}`')
    return found


def exemption(event):
    """Why `event`'s pull request needs no block, or None."""
    pull = (event or {}).get('pull_request') or {}
    author = ((pull.get('user') or {}).get('login') or '')
    branch = ((pull.get('head') or {}).get('ref') or '')
    if author in EXEMPT_AUTHORS:
        return f'opened by {author}'
    if branch.startswith(EXEMPT_BRANCH_PREFIX):
        return f'a release pull request from {branch}'
    return None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    source = parser.add_mutually_exclusive_group()
    source.add_argument('--event', type=Path, help='pull request event payload (JSON)')
    source.add_argument('--body', type=Path, help='a pull request description (Markdown)')
    args = parser.parse_args(argv)
    if args.body is not None:
        body = args.body.read_text(encoding='utf-8')
    else:
        path = args.event or os.environ.get('GITHUB_EVENT_PATH')
        if not path:
            parser.error('give --event PATH, --body FILE or set GITHUB_EVENT_PATH')
        event = json.loads(Path(path).read_text(encoding='utf-8'))
        if why := exemption(event):
            print(f'af task report: not required, {why}')
            return 0
        body = ((event.get('pull_request') or {}).get('body')) or ''
    found = problems(body)
    if not found:
        print('af task report: the description carries one well-formed block')
        return 0
    for problem in found:
        print(f'af task report: {problem}', file=sys.stderr)
    print(f'af task report: {HOW}', file=sys.stderr)
    return 1


if __name__ == '__main__':
    sys.exit(main())
