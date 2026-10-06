#!/usr/bin/env python3
"""Exercise `check-pr-report.py` through its real entry, offline (ADR-0142).

The valid block is `fixtures/task-report/report.md`: the `af task report` renderer's output for
a test Store, which `crates/af` checks it still prints. So the checker and the renderer cannot
drift apart: a renderer change fails there, a checker change that refuses the block fails here.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts' / 'check-pr-report.py'
BLOCK = (ROOT / 'fixtures' / 'task-report' / 'report.md').read_text(encoding='utf-8')
TEMPLATE = (ROOT / '.github' / 'PULL_REQUEST_TEMPLATE.md').read_text(encoding='utf-8')
HOW = 'af task report TASK_ID'


def description(block):
    return ('## What\n\nPaginate the listing.\n\n## af task report\n\n' + block
            + '\n## Checklist\n\n- [x] `make check` passes locally.\n')


def event(body, author='contributor', branch='af/paginate'):
    return {'action': 'edited', 'pull_request': {
        'body': body, 'user': {'login': author}, 'head': {'ref': branch}}}


def check(payload, *, as_body=False):
    """Run the script on an event payload (or a bare body) and return (exit code, output)."""
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / ('body.md' if as_body else 'event.json')
        path.write_text(payload if as_body else json.dumps(payload), encoding='utf-8')
        flag = '--body' if as_body else '--event'
        result = subprocess.run([sys.executable, '-I', str(SCRIPT), flag, str(path)],
                                capture_output=True, text=True, env={})
        return result.returncode, result.stdout + result.stderr


def line_starting(text, prefix):
    return next(line for line in text.splitlines() if line.startswith(prefix))


def main():
    header = line_starting(BLOCK, '| Task |')
    rows = [line for line in BLOCK.splitlines()
            if line.startswith('| ') and not line.startswith(('| Task |', '| --- |', '| Node |'))
            and BLOCK.index(line) < BLOCK.index('**Totals:**')]
    assert rows, BLOCK
    totals = line_starting(BLOCK, '**Totals:**')
    begin, end = '<!-- af-task-report:v1 -->', '<!-- /af-task-report -->'
    empty_table = BLOCK
    for row in rows:
        empty_table = empty_table.replace(row + '\n', '')
    separator = line_starting(BLOCK, '| --- |')
    task_cells = rows[0][2:-2].split(' | ')

    def with_row(row):
        """The block with its first Task row replaced by `row`."""
        return BLOCK.replace(rows[0] + '\n', row + '\n')

    passing = {
        'the renderer fixture': event(description(BLOCK)),
        'CRLF line endings': event(description(BLOCK).replace('\n', '\r\n')),
        'a Dependabot pull request without a block': event('Bumps serde.', 'dependabot[bot]'),
        'a release pull request without a block': event(None, branch='release/0.12.0'),
        # GitHub keeps an escaped or encoded pipe inside its cell.
        'an escaped pipe in a cell': event(description(with_row(
            '| ' + ' | '.join([task_cells[0], task_cells[1], 'a\\|b'] + task_cells[3:])
            + ' |'))),
        'an encoded pipe in a cell': event(description(with_row(
            '| ' + ' | '.join([task_cells[0], task_cells[1], 'a&#124;b'] + task_cells[3:])
            + ' |'))),
    }
    for case, payload in passing.items():
        code, said = check(payload)
        assert code == 0, f'{case} failed:\n{said}'
    code, said = check(description(BLOCK), as_body=True)
    assert code == 0, said

    failing = {
        'a missing block': (event(description('')), 'no af task report block'),
        'a null body': (event(None), 'no af task report block'),
        'a missing end marker': (event(description(BLOCK.replace(end + '\n', ''))),
                                 'no end marker'),
        'a missing begin marker': (event(description(BLOCK.replace(begin + '\n', ''))),
                                   'no begin marker'),
        'markers out of order': (event(description(end + '\n' + BLOCK.replace(
            begin + '\n', '').replace(end + '\n', '') + begin + '\n')), 'comes before'),
        'a missing column': (event(description(BLOCK.replace(
            header, header.replace(' Active time |', '')))), 'missing the column Active time'),
        'an extra column': (event(description(BLOCK.replace(header, header + ' Cost |'))),
                            'has the extra column Cost: the v1 header has exactly 9'),
        'reordered columns': (event(description(BLOCK.replace(header, header.replace(
            '| Active time | Wall time |', '| Wall time | Active time |')))),
            'the summary table columns are out of order'),
        'a repeated column': (event(description(BLOCK.replace(
            header, header.replace(' Rounds |', ' Kind |')))),
            'missing the column Rounds'),
        'a missing table': (event(description(BLOCK.replace(header + '\n', ''))),
                            'no summary table'),
        'an empty table': (event(description(empty_table)), 'has no Task row'),
        'two blocks': (event(description(BLOCK + '\n' + BLOCK)), '2 af task report blocks'),
        'a missing totals line': (event(description(BLOCK.replace(totals + '\n', ''))),
                                  'no totals line'),
        'the unfilled template': (event(TEMPLATE), 'no summary table'),
        'a one-cell placeholder row': (event(description(with_row('| x |'))),
                                       'Task row 1 of the summary table is a one-cell'),
        'a short row': (event(description(with_row('| ' + ' | '.join(task_cells[:-1]) + ' |'))),
                        'Task row 1 of the summary table has 8 cells, but the header has 9'),
        'a long row': (event(description(with_row('| ' + ' | '.join(task_cells + ['x']) + ' |'))),
                       'has 10 cells, but the header has 9'),
        'an empty Task cell': (event(description(with_row(
            '|  | ' + ' | '.join(task_cells[1:]) + ' |'))), 'has an empty Task cell'),
        'empty Kind and Outcome cells': (event(description(with_row('| ' + ' | '.join(
            [task_cells[0], '', task_cells[2], ''] + task_cells[4:]) + ' |'))),
            'has an empty Kind, Outcome cells'),
        'a separator of the wrong width': (event(description(BLOCK.replace(
            separator, separator.replace('| --- ', '', 1)))),
            'separator row has 8 cells, but the header has 9'),
        'an unescaped pipe in a cell': (event(description(with_row(
            '| ' + ' | '.join([task_cells[0], task_cells[1], 'a|b'] + task_cells[3:]) + ' |'))),
            'has 10 cells'),
        'a Dependabot-like name': (event('', 'dependabot'), 'no af task report block'),
        'a branch that only mentions release': (event('', branch='af/release/notes'),
                                                'no af task report block'),
    }
    messages = {}
    for case, (payload, expected) in failing.items():
        code, said = check(payload)
        assert code == 1, f'{case} passed:\n{said}'
        assert expected in said, f'{case}: expected {expected!r} in:\n{said}'
        assert HOW in said, f'{case} does not say how to produce the block:\n{said}'
        messages[case] = said
    distinct = ['a missing block', 'a missing end marker', 'a missing column', 'an extra column',
                'reordered columns', 'an empty table',
                'two blocks', 'a missing totals line', 'a one-cell placeholder row',
                'a short row', 'an empty Task cell', 'a separator of the wrong width']
    assert len({messages[case] for case in distinct}) == len(distinct), \
        'each required part has its own message'
    print(f'check-pr-report: {len(passing) + 1} passing and {len(failing)} failing cases')


if __name__ == '__main__':
    main()
