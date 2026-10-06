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
    header = line_starting(BLOCK, '| Round |')
    separator = line_starting(BLOCK, '| ---: |')
    table = BLOCK.split(separator + '\n', 1)[1].split('\n\n', 1)[0].splitlines()
    rows, total = table[:-1], table[-1]
    assert rows and total.startswith('|  | Total: '), BLOCK
    pipeline = line_starting(BLOCK, '**')
    begin, end = '<!-- af-task-report:v1 -->', '<!-- /af-task-report -->'
    empty_table = BLOCK
    for row in rows:
        empty_table = empty_table.replace(row + '\n', '')
    round_cells = rows[0][2:-2].split(' | ')

    def with_row(row):
        """The block with its first round row replaced by `row`."""
        return BLOCK.replace(rows[0] + '\n', row + '\n')

    def after_row(row):
        """The block with `row` added after its first round row."""
        return BLOCK.replace(rows[0] + '\n', rows[0] + '\n' + row + '\n')

    def cells_with(index, value):
        """The first round row with its cell `index` replaced by `value`."""
        changed = list(round_cells)
        changed[index] = value
        return '| ' + ' | '.join(changed) + ' |'

    def indented(text, prefix):
        return ''.join(prefix + line if line.strip() else line
                       for line in text.splitlines(keepends=True))

    # GitHub renders a line without its leading or trailing `|` as a row of the table too.
    unopened_short = ' | '.join(round_cells[:-1]) + ' |'
    unclosed_short = '| ' + ' | '.join(round_cells[:-1])

    passing = {
        'the renderer fixture': event(description(BLOCK)),
        'CRLF line endings': event(description(BLOCK).replace('\n', '\r\n')),
        'a Dependabot pull request without a block': event('Bumps serde.', 'dependabot[bot]'),
        'a release pull request without a block': event(None, branch='release/0.12.0'),
        # GitHub keeps an escaped or encoded pipe inside its cell.
        'an escaped pipe in a cell': event(description(with_row(cells_with(2, 'a\\|b')))),
        'an encoded pipe in a cell': event(description(with_row(cells_with(2, 'a&#124;b')))),
        'a six-cell row without outer pipes': event(description(with_row(
            ' | '.join(round_cells)))),
        'a six-cell row without outer pipes after a row': event(description(after_row(
            ' | '.join(round_cells)))),
        'a comment beside visible text in a cell': event(description(with_row(
            cells_with(1, '<!--x--> ' + round_cells[1])))),
        # A fenced example elsewhere in the description is not the block, and does not count.
        'a fenced example beside the block': event(description(
            '```markdown\n' + BLOCK + '```\n\n' + BLOCK)),
        'a fence that only mentions backticks in its text': event(description(
            'Run `af task report` and paste ``` its output:\n\n' + BLOCK)),
    }
    for case, payload in passing.items():
        code, said = check(payload)
        assert code == 0, f'{case} failed:\n{said}'
    code, said = check(description(BLOCK), as_body=True)
    assert code == 0, said

    inside_code = 'the af task report block is inside a code block'
    failing = {
        'a missing block': (event(description('')), 'no af task report block'),
        'a null body': (event(None), 'no af task report block'),
        'a missing end marker': (event(description(BLOCK.replace(end + '\n', ''))),
                                 'no end marker'),
        'a missing begin marker': (event(description(BLOCK.replace(begin + '\n', ''))),
                                   'no begin marker'),
        'markers out of order': (event(description(end + '\n' + BLOCK.replace(
            begin + '\n', '').replace(end + '\n', '') + begin + '\n')), 'comes before'),
        'a missing pipeline line': (event(description(BLOCK.replace(pipeline + '\n', ''))),
                                    'no pipeline line'),
        'a pipeline line without steps': (event(description(BLOCK.replace(
            pipeline, pipeline.split('**: ')[0] + '**: '))), 'no pipeline line'),
        'a missing column': (event(description(BLOCK.replace(
            header, header.replace(' Findings |', '')))), 'missing the column Findings'),
        'an extra column': (event(description(BLOCK.replace(header, header + ' Cost |'))),
                            'has the extra column Cost: the v1 header has exactly 6'),
        'reordered columns': (event(description(BLOCK.replace(header, header.replace(
            '| Tokens | Active |', '| Active | Tokens |')))),
            'the round table columns are out of order'),
        'a repeated column': (event(description(BLOCK.replace(
            header, header.replace(' Findings |', ' Task |')))),
            'missing the column Findings'),
        'a missing table': (event(description(BLOCK.replace(header + '\n', ''))),
                            'no round table'),
        'an empty table': (event(description(empty_table)), 'has no round row'),
        'two blocks': (event(description(BLOCK + '\n' + BLOCK)), '2 af task report blocks'),
        'a missing totals row': (event(description(BLOCK.replace(total + '\n', ''))),
                                 'has no `Total:` row'),
        'a totals row that is not last': (event(description(BLOCK.replace(
            total + '\n', total + '\n' + rows[0] + '\n'))), 'has no `Total:` row'),
        'the unfilled template': (event(TEMPLATE), 'no round table'),
        'a one-cell placeholder row': (event(description(with_row('| x |'))),
                                       'round row 1 of the round table is a one-cell'),
        'a short row': (event(description(with_row('| ' + ' | '.join(round_cells[:-1]) + ' |'))),
                        'round row 1 of the round table has 5 cells, but the header has 6'),
        'a long row': (event(description(with_row(
            '| ' + ' | '.join(round_cells + ['x']) + ' |'))),
            'has 7 cells, but the header has 6'),
        'an empty Task cell': (event(description(with_row(cells_with(1, '')))),
                               'has an empty Task cell'),
        'empty Round and Outcome cells': (event(description(with_row('| ' + ' | '.join(
            ['', round_cells[1], ''] + round_cells[3:]) + ' |'))),
            'has an empty Round, Outcome cells'),
        # Issue #191: an HTML comment renders as nothing, so it does not fill a cell.
        'a Task cell holding only a comment': (event(description(with_row(
            cells_with(1, '<!--x-->')))), 'round row 1 of the round table has an empty Task'),
        'a Round cell holding only a comment': (event(description(with_row(
            cells_with(0, '<!--x-->')))), 'has an empty Round cell'),
        'an Outcome cell holding only an open comment': (event(description(with_row(
            cells_with(2, '<!-- verified')))), 'has an empty Outcome cell'),
        'a separator of the wrong width': (event(description(BLOCK.replace(
            separator, separator.replace('| ---: ', '', 1)))),
            'separator row has 5 cells, but the header has 6'),
        'an unescaped pipe in a cell': (event(description(with_row(cells_with(2, 'a|b')))),
                                        'has 7 cells'),
        'a short row without a leading pipe after a valid row': (
            event(description(after_row(unopened_short))),
            'round row 2 of the round table has 5 cells, but the header has 6: `'
            + unopened_short.strip() + '`'),
        'a short row without a trailing pipe after a valid row': (
            event(description(after_row(unclosed_short))),
            'round row 2 of the round table has 5 cells, but the header has 6: `'
            + unclosed_short.strip() + '`'),
        'a one-cell line after a valid row': (event(description(after_row('stray text'))),
                                              'round row 2 of the round table is a one-cell '
                                              'placeholder (`stray text`)'),
        # Issue #191: a block shown as code is not the report.
        'the block in a backtick fence': (event(description('```\n' + BLOCK + '```\n')),
                                          inside_code),
        'the block in a tilde fence with an info string': (event(description(
            '~~~markdown\n' + BLOCK + '~~~\n')), inside_code),
        'the block in a longer fence left open': (event(description('````md\n' + BLOCK)),
                                                  inside_code),
        'the block indented four spaces': (event(description(indented(BLOCK, '    '))),
                                           inside_code),
        'the block indented by a tab': (event(description(indented(BLOCK, '\t'))),
                                        inside_code),
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
    distinct = ['a missing block', 'a missing end marker', 'a missing pipeline line',
                'a missing column', 'an extra column', 'reordered columns', 'an empty table',
                'two blocks', 'a missing totals row', 'a one-cell placeholder row',
                'a short row', 'an empty Task cell', 'a separator of the wrong width',
                'the block in a backtick fence']
    assert len({messages[case] for case in distinct}) == len(distinct), \
        'each required part has its own message'
    print(f'check-pr-report: {len(passing) + 1} passing and {len(failing)} failing cases')


if __name__ == '__main__':
    main()
