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
COLUMNS = ['Round', 'Task', 'Outcome', 'Findings', 'Tokens', 'Active']
# Issue #193: the type 6 names of GFM 0.29 and CommonMark 0.31 together, each with whether
# GitHub rendered the pipeline line when the name's open tag sat on the line under an `Intro`
# paragraph line right above it, as GitHub's markdown API rendered the renderer fixture (the
# round table rendered in every case). Every name but one is a type 6 tag on GitHub: it
# interrupts the paragraph, and its block, up to the blank line, holds the pipeline line.
# `search`, which only CommonMark 0.31 lists, is a type 7 tag there, which cannot; `source`,
# which only GFM 0.29 lists, is type 6.
TYPE6_AUDIT = {
    **dict.fromkeys((
        'address', 'article', 'aside', 'base', 'basefont', 'blockquote', 'body', 'caption',
        'center', 'col', 'colgroup', 'dd', 'details', 'dialog', 'dir', 'div', 'dl', 'dt',
        'fieldset', 'figcaption', 'figure', 'footer', 'form', 'frame', 'frameset', 'h1', 'h2',
        'h3', 'h4', 'h5', 'h6', 'head', 'header', 'hr', 'html', 'iframe', 'legend', 'li', 'link',
        'main', 'menu', 'menuitem', 'nav', 'noframes', 'ol', 'optgroup', 'option', 'p', 'param',
        'section', 'source', 'summary', 'table', 'tbody', 'td', 'tfoot', 'th', 'thead', 'title',
        'tr', 'track', 'ul'), False),
    'search': True,
}
assert len(TYPE6_AUDIT) == 63, len(TYPE6_AUDIT)


LINES = '<!-- af-pr-lines:v1 -->\nLines added: none\n<!-- /af-pr-lines -->\n'
TAIL = '\n## Lines added\n\n' + LINES + '\n## Checklist\n\n- [x] `make check` passes locally.\n'


def description(block, lines=None):
    """A description carrying the report `block` and the lines block (`LINES`, or `lines`)."""
    tail = TAIL if lines is None else TAIL.replace(LINES, lines)
    return '## What\n\nPaginate the listing.\n\n## af task report\n\n' + block + tail


def right_after(lead, block):
    """A description whose `block` follows `lead` with no blank line between."""
    return '## What\n\nPaginate the listing.\n\n' + lead + block + TAIL


def event(body, author='contributor', branch='af/paginate', head_repo='owner/afactory'):
    """A pull request event into `owner/afactory`, from `head_repo` (None: a deleted fork)."""
    return {'action': 'edited', 'pull_request': {
        'body': body, 'user': {'login': author},
        'head': {'ref': branch, 'repo': head_repo and {'full_name': head_repo}},
        'base': {'ref': 'main', 'repo': {'full_name': 'owner/afactory'}}}}


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
    main_table = '\n'.join([header, separator] + table) + '\n'
    pipeline = line_starting(BLOCK, '**')
    prefix, steps = pipeline.split('**: ', 1)
    steps_in_code = prefix + '**: `' + steps + '`'
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

    def with_unknown_usage(block):
        """The block with `(+1 unknown)` after the first round row's and the totals row's
        Tokens, as the renderer prints a charged total beside unknown usage (ADR-0143)."""
        tokens = COLUMNS.index('Tokens')
        row = cells_with(tokens, round_cells[tokens] + ' (+1 unknown)')
        totals = [cell.strip() for cell in total.strip().strip('|').split('|')]
        totals[tokens] += ' (+1 unknown)'
        return block.replace(rows[0] + '\n', row + '\n').replace(
            total, '| ' + ' | '.join(totals) + ' |')

    unknown_pipeline = '**unknown pipeline**: not retained'
    # Four-space-indented lines right after a block that ends a paragraph are code.
    four = indented(BLOCK, '    ')

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
        # A Tokens cell with Attempts whose usage is unknown (ADR-0143), in a round row and in
        # the totals row.
        'Tokens cells with unknown usage': event(description(with_unknown_usage(BLOCK))),
        # The line the renderer prints for a Task whose plan is not retained.
        'the unknown-pipeline line': event(description(BLOCK.replace(
            pipeline, unknown_pipeline))),
        'the unknown-pipeline line beside a pipeline line': event(description(BLOCK.replace(
            pipeline + '\n', pipeline + '\n\n' + unknown_pipeline + '\n'))),
        # A line indented inside an HTML block is not code; the block ends at a blank line.
        'an indented line inside an HTML block': event(description(
            '<div>\n    an indented note\n</div>\n\n' + BLOCK)),
        # An ordered item that does not start at 1 cannot interrupt a paragraph, so the block
        # right after it is a paragraph line, not a list item's: it still counts.
        'the block after a paragraph line that reads like an item': event(right_after(
            'The Tasks ran in\n2024. These are they:\n\n', BLOCK)),
        # Issue #193: a raw HTML block closed before the block hides only its own lines, and
        # an element that does not stop Markdown rendering, such as `<details>`, hides nothing.
        'the block after a closed multi-line pre': event(description(
            '<pre>\naf task report T1\n</pre>\n\n' + BLOCK)),
        'the block after a pre closed on its line': event(right_after(
            '<pre>af task report T1</pre>\n', BLOCK)),
        'the block inside a details element': event(description(
            '<details>\n<summary>af task report</summary>\n\n' + BLOCK + '\n</details>\n')),
        # A raw block ends at the closing tag of any of the four names, not only its own.
        'the block after a script closed by </pre>': event(description(
            '<script>\n</pre>\n\n' + BLOCK)),
        'the block after a textarea closed by </pre>': event(description(
            '<textarea>\n</pre>\n\n' + BLOCK)),
        'the block after a pre closed by </textarea>': event(description(
            '<pre>\n</textarea>\n\n' + BLOCK)),
        # A comment left open above the block closes at the begin marker's `-->`: the block
        # renders after it.
        'a comment left open above the block': event(right_after(
            '<!-- paste the report below\n', BLOCK)),
        # A tag nested in an HTML block does not change where that block ends (CommonMark
        # 4.6): this `<pre>` opens no raw block, the `<div>` ends at the block's first blank
        # line, and GitHub renders the pipeline line and the table.
        'the block inside a pre nested in a div': event(description(
            '<div>\n<pre>\n' + BLOCK + '</pre>\n</div>\n')),
        # A raw block ends on the first line holding a closing tag, even inside a comment.
        'the block after a pre closed inside a comment': event(description(
            '<pre>\n<!-- </pre> -->\n' + BLOCK + '</pre>\n')),
        'the block after a div whose attribute names pre': event(description(
            '<div data-note="<pre>"></div>\n\n' + BLOCK)),
        'the block after a div holding a commented pre': event(description(
            '<div>\n<!--\n<pre>\n-->\n</div>\n\n' + BLOCK)),
        # A multi-line comment inside the block hides only itself.
        'a multi-line comment inside the block': event(description(BLOCK.replace(
            pipeline + '\n', pipeline + '\n\n<!-- a note\nthat spans lines -->\n'))),
        # A `<!--` that GitHub shows as text opens no comment: in a quoted attribute value, in
        # a code span, escaped, or inline and not closed within its paragraph.
        'the block after a div whose attribute holds <!--': event(description(
            '<div data-note="<!--"></div>\n\n' + BLOCK)),
        'a pipeline line between code spans of <!-- and -->': event(description(BLOCK.replace(
            pipeline + '\n', '`<!--`\n' + pipeline + '\n`-->`\n'))),
        'a pipeline line between an escaped <!-- and -->': event(description(BLOCK.replace(
            pipeline + '\n', '\\<!--\n' + pipeline + '\n-->\n'))),
        'a pipeline line after a paragraph that leaves <!-- open': event(description(
            BLOCK.replace(pipeline + '\n', 'Prefix <!--\n\n' + pipeline + '\n\n-->\n'))),
        # A comment hides only its own text: what comes before and after it shows.
        'a pipeline line after an inline comment closed above it': event(description(
            BLOCK.replace(pipeline + '\n', 'Prefix <!-- a\nnote -->\n' + pipeline + '\n'))),
        # A comment an HTML block leaves open closes at the first `-->` of raw HTML, here an
        # inline comment before the block's content.
        'a block comment closed by an inline comment': event(description(BLOCK.replace(
            begin + '\n', begin + '\n<div>\n<!--\n\nClosed <!-- here -->\n\n'))),
        # A comment opened in a table cell and closed in a later row is text: each row is
        # inline content of its own.
        'a table cell that opens <!-- above a row that has -->': event(description(
            after_row(cells_with(3, '<!-- x') + '\n' + cells_with(3, 'y -->')))),
        # A line indented four columns that continues a paragraph is not code.
        'an indented line continuing the pipeline line': event(description(BLOCK.replace(
            pipeline + '\n', pipeline + '\n    continued\n'))),
        # A fence inside an HTML block is the block's text, and closed inside it hides nothing
        # after the block's blank line.
        'the block after a div holding a closed fence': event(description(
            '<div>\n```\n```\n</div>\n\n' + BLOCK)),
        # A quoted attribute value closed within its HTML block hides nothing after it.
        'the block after a div whose quoted attribute spans lines': event(description(
            '<div data-note="a\nb">\n</div>\n\n' + BLOCK)),
        # A code span hides nothing GitHub renders around it: a bold pipeline prefix before
        # steps in a code span, a Task cell in a code span, a backtick line a blank line ends,
        # and a table under a backtick line, whose header the paragraph above it cannot hold.
        'pipeline steps in a code span': event(description(BLOCK.replace(
            pipeline, steps_in_code))),
        'a Task cell in a code span': event(description(with_row(
            cells_with(1, '`' + round_cells[1] + '`')))),
        'a pipeline line after a blank line ends a backtick line': event(description(
            BLOCK.replace(pipeline + '\n', '`\n\n' + pipeline + '\n`\n'))),
        'the round table between backtick lines closed after its blank line': event(
            description(BLOCK.replace(header + '\n', '`\n' + header + '\n').replace(
                total + '\n\n', total + '\n\n`\n'))),
        'the round table right after a paragraph line': event(description(BLOCK.replace(
            header + '\n', 'The rounds:\n' + header + '\n'))),
        'the round table right after a paragraph and a blank line': event(description(
            BLOCK.replace(header + '\n', 'The rounds:\n\n' + header + '\n'))),
        'a pipeline line after a code span closed above it': event(description(BLOCK.replace(
            pipeline + '\n', 'Ran `af\ntask report`\n' + pipeline + '\n'))),
    }
    for case, payload in passing.items():
        code, said = check(payload)
        assert code == 0, f'{case} failed:\n{said}'
    code, said = check(description(BLOCK), as_body=True)
    assert code == 0, said

    inside_code = 'the af task report block is inside a code block'
    inside_raw = 'the af task report block is inside a raw HTML block'
    hidden = 'part of the af task report block is inside an HTML block'
    # Issue #193: GitHub shows a raw HTML block's lines as literal text, or hides them, however
    # they are indented: its markers are text, and between visible markers its pipeline line
    # and table do not count.
    raw_cases = {}
    for tag in ('pre', 'script', 'style', 'textarea'):
        raw_cases[f'the block inside <{tag}>'] = (event(description(
            f'<{tag}>\n' + BLOCK + f'</{tag}>\n')), inside_raw)
        raw_cases[f'the block indented four spaces inside <{tag}>'] = (event(description(
            f'<{tag}>\n' + four + f'</{tag}>\n')), inside_raw)
        raw_cases[f'visible markers around a body inside <{tag}>'] = (event(description(
            BLOCK.replace(begin + '\n', begin + f'\n<{tag}>\n').replace(
                end + '\n', f'</{tag}>\n' + end + '\n'))), hidden)
    # GitHub escapes a `<script>`, `<style>` or `<textarea>` tag and keeps the `<!--` the
    # block holds: that comment stays open after the raw block's closing tag and hides the
    # pipeline line and the round table (its markdown API, audited for issue #193).
    heading = '### af task report\n\n'
    for tag in ('script', 'style', 'textarea'):
        raw_cases[f'a comment opened in <{tag}> left open past its close'] = (event(description(
            BLOCK.replace(heading, heading + f'<{tag}>\n<!--\n</{tag}>\n'))), hidden)
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
        # A heading, a thematic break, a fence, an HTML block and a list item each end a
        # paragraph: an indented block right after one, with no blank line, is code.
        'the block indented right after an ATX heading': (event(right_after(
            '## af task report\n', four)), inside_code),
        'the block indented right after a setext heading': (event(right_after(
            'af task report\n==============\n', four)), inside_code),
        'the block indented right after a setext heading under a dash line': (event(right_after(
            'af task report\n---\n', four)), inside_code),
        'the block indented right after a thematic break': (event(right_after(
            'Above.\n\n* * *\n', four)), inside_code),
        'the block indented right after a fenced code block': (event(right_after(
            '```sh\naf task report T1\n```\n', four)), inside_code),
        'the block indented right after a one-line HTML block': (event(right_after(
            '<pre>af task report T1</pre>\n', four)), inside_code),
        'the block indented right after a comment that interrupts a paragraph': (event(
            right_after('Paste below.\n<!-- the report -->\n', four)), inside_code),
        'the block indented right after a list item': (event(right_after(
            '- [x] Made through af Tasks.\n', four)), inside_code),
        'the block indented right after an item that interrupts a paragraph': (event(
            right_after('The Tasks:\n1. implement\n', four)), inside_code),
        'an unknown-pipeline line that says something else': (event(description(
            BLOCK.replace(pipeline, '**unknown pipeline**: collected'))), 'no pipeline line'),
        'a Dependabot-like name': (event('', 'dependabot'), 'no af task report block'),
        'a branch that only mentions release': (event('', branch='af/release/notes'),
                                                'no af task report block'),
        # A fork can name its branch anything: only this repository's release/ branch is exempt.
        'a release/ branch from a fork': (event(None, branch='release/0.12.0',
                                                head_repo='someone/afactory'),
                                          'no af task report block'),
        'a release/ branch from a deleted fork': (event(None, branch='release/0.12.0',
                                                        head_repo=None),
                                                  'no af task report block'),
        **raw_cases,
        'the block inside an uppercase PRE with an attribute': (event(description(
            '<PRE class="report">\n' + BLOCK + '</PRE>\n')), inside_raw),
        'the block inside a pre left open': (event(description('<pre>\n' + BLOCK)),
                                             inside_raw),
        'the block inside a pre that interrupts a paragraph': (event(right_after(
            'The report:\n<pre>\n', BLOCK + '</pre>\n')), inside_raw),
        # A fence inside a raw block is its text: it does not end the block.
        'the block after a closed fence inside a pre': (event(description(
            '<pre>\n```\n```\n' + BLOCK + '</pre>\n')), inside_raw),
        'the round table inside a pre': (event(description(BLOCK.replace(
            header + '\n', '<pre>\n' + header + '\n').replace(
                total + '\n\n', total + '\n\n</pre>\n'))), 'no round table'),
        # A comment opened after the begin marker hides everything up to the end marker's
        # `-->`, which closes it.
        'visible markers around a body inside a comment': (event(description(BLOCK.replace(
            begin + '\n', begin + '\n<!--\n'))), hidden),
        'the pipeline line inside a multi-line comment': (event(description(BLOCK.replace(
            pipeline + '\n', '<!--\n' + pipeline + '\n-->\n'))), 'no pipeline line'),
        # A comment opened inside an HTML block stays open after the block ends at its blank
        # line, and Markdown text never closes it, `-->` included: here it runs to the end
        # marker's `-->`.
        'a comment left open in a div after the begin marker': (event(description(
            BLOCK.replace(begin + '\n', begin + '\n<div>\n<!--\n\n'))), hidden),
        'a comment left open in a div past a text -->': (event(description(BLOCK.replace(
            begin + '\n', begin + '\n<div>\n<!--\n\n-->\n\n'))), hidden),
        # An inline comment spans the lines of its paragraph.
        'the pipeline line inside an inline multi-line comment': (event(description(
            BLOCK.replace(pipeline + '\n', 'Prefix <!--\n' + pipeline + '\n-->\n'))),
            'no pipeline line'),
        # Between visible markers, a pipeline line or a round table shown as code does not
        # count: GitHub renders neither.
        'the pipeline line in a fence': (event(description(BLOCK.replace(
            pipeline + '\n', '```\n' + pipeline + '\n```\n'))), 'no pipeline line'),
        'the round table in a fence closed after a blank line': (event(description(
            BLOCK.replace(header + '\n', '```\n' + header + '\n').replace(
                total + '\n\n', total + '\n\n```\n'))), 'no round table'),
        'the pipeline line indented four spaces': (event(description(BLOCK.replace(
            pipeline, '    ' + pipeline))), 'no pipeline line'),
        'the round table indented four spaces': (event(description(BLOCK.replace(
            main_table, indented(main_table, '    ')))), 'no round table'),
        # A fence does not end a `<div>` block, which runs to its blank line: GitHub shows the
        # lines after the fence as the block's literal text.
        'the pipeline line in a div after a closed fence': (event(description(BLOCK.replace(
            pipeline + '\n', '<div>\n```\n```\n' + pipeline + '\n'))), 'no pipeline line'),
        'the round table in a div after a closed fence': (event(description(BLOCK.replace(
            header + '\n', '<div>\n```\n```\n' + header + '\n'))), 'no round table'),
        # A tag an HTML block leaves in a quoted attribute value at its blank line swallows
        # what GitHub renders after it, up to that quote in raw HTML.
        'the block inside an unfinished quoted attribute': (event(description(
            '<div data-note="\n\n' + BLOCK + '\n">\n')), 'no pipeline line'),
        'the pipeline line inside an unfinished quoted attribute': (event(description(
            BLOCK.replace(pipeline + '\n', '<div data-note="\n\n' + pipeline
                          + '\n\n<p>"</p>\n'))), 'no pipeline line'),
        'the round table inside an unfinished quoted attribute': (event(description(
            BLOCK.replace(header + '\n', '<div data-note="\n\n' + header + '\n').replace(
                total + '\n\n', total + '\n\n<p>"</p>\n'))), 'no round table'),
        # A code span may span the lines of a paragraph: GitHub shows a pipeline line between
        # backtick lines as code, its `**` included, as it does a header row whose delimiter
        # row the closing backtick line keeps from it.
        'the pipeline line in a code span across lines': (event(description(BLOCK.replace(
            pipeline + '\n', '`\n' + pipeline + '\n`\n'))), 'no pipeline line'),
        'the pipeline line in a double-backtick code span across lines': (event(description(
            BLOCK.replace(pipeline + '\n', '``\n' + pipeline + '\n``\n'))),
            'no pipeline line'),
        'the pipeline line in a code span opened at the end of a paragraph line': (event(
            description(BLOCK.replace(pipeline + '\n', 'The pipeline: `\n' + pipeline
                                      + '`\n'))), 'no pipeline line'),
        'a pipeline prefix in a code span closed after it': (event(description(BLOCK.replace(
            pipeline + '\n', '`\n' + prefix + '**: ` ' + steps + '\n'))), 'no pipeline line'),
        'the round table header in a code span across lines': (event(description(BLOCK.replace(
            header + '\n', '`\n' + header + '\n`\n'))), 'no round table'),
        # A backtick line right below the totals row is a row of the table, not a code span's
        # end: the table's last row is then no totals row.
        'the round table between backtick lines closed without a blank line': (event(
            description(BLOCK.replace(header + '\n', '`\n' + header + '\n').replace(
                total + '\n', total + '\n`\n'))), 'has no `Total:` row'),
    }
    messages = {}
    for case, (payload, expected) in failing.items():
        code, said = check(payload)
        assert code == 1, f'{case} passed:\n{said}'
        assert expected in said, f'{case}: expected {expected!r} in:\n{said}'
        assert HOW in said, f'{case} does not say how to produce the block:\n{said}'
        messages[case] = said
    # The pipeline line and the round table are each required: hiding one fails on it alone,
    # and says why GitHub does not render it.
    in_code = 'part of the af task report block is inside a code block'
    in_span = 'part of the af task report block is inside a code span'
    alone = {
        'the pipeline line in a fence': ('no round table', in_code),
        'the round table in a fence closed after a blank line': ('no pipeline line', in_code),
        'the pipeline line indented four spaces': ('no round table', in_code),
        'the round table indented four spaces': ('no pipeline line', in_code),
        'the pipeline line in a div after a closed fence': ('no round table', hidden),
        'the round table in a div after a closed fence': ('no pipeline line', hidden),
        'the pipeline line inside an unfinished quoted attribute': ('no round table', hidden),
        'the round table inside an unfinished quoted attribute': ('no pipeline line', hidden),
        'the pipeline line in a code span across lines': ('no round table', in_span),
        'the pipeline line in a double-backtick code span across lines': ('no round table',
                                                                          in_span),
        'the pipeline line in a code span opened at the end of a paragraph line': (
            'no round table', in_span),
        'a pipeline prefix in a code span closed after it': ('no round table', in_span),
    }
    for case, (other, why) in alone.items():
        assert other not in messages[case], f'{case}: {other!r} in:\n{messages[case]}'
        assert why in messages[case], f'{case}: expected {why!r} in:\n{messages[case]}'
    assert hidden in messages['the block inside an unfinished quoted attribute']
    for tag in ('script', 'style', 'textarea'):
        said = messages[f'a comment opened in <{tag}> left open past its close']
        assert 'no pipeline line' in said and 'no round table' in said, said
    distinct = ['a missing block', 'a missing end marker', 'a missing pipeline line',
                'a missing column', 'an extra column', 'reordered columns', 'an empty table',
                'two blocks', 'a missing totals row', 'a one-cell placeholder row',
                'a short row', 'an empty Task cell', 'a separator of the wrong width',
                'the block in a backtick fence', 'the block inside <pre>',
                'visible markers around a body inside <pre>',
                'the pipeline line in a code span across lines']
    assert len({messages[case] for case in distinct}) == len(distinct), \
        'each required part has its own message'

    # Each type 6 name of GFM 0.29 or CommonMark 0.31, opened on the line under an `Intro`
    # paragraph line right above the pipeline line: the checker passes exactly where GitHub
    # renders that pipeline line (TYPE6_AUDIT).
    for tag, rendered in TYPE6_AUDIT.items():
        code, said = check(event(description(BLOCK.replace(
            pipeline + '\n', f'Intro\n<{tag}>\n' + pipeline + '\n'))))
        assert code == (0 if rendered else 1), f'<{tag}>: exit {code}:\n{said}'
        if not rendered:
            assert 'no pipeline line' in said and 'no round table' not in said, \
                f'<{tag}>:\n{said}'
    # The lines added block (ADR-0195): exactly one, rendered as text, holding one line of the
    # tool's form; the figures are compared with commits only when some are named (see
    # scripts/test-pr-lines.py), so these cases judge the shape.
    def lines_block(line):
        return '<!-- af-pr-lines:v1 -->\n' + line + '\n<!-- /af-pr-lines -->\n'

    lines_passing = {
        'a lines block with figures': event(description(BLOCK, lines_block(
            'Lines added: crates/12k docs/1.1k tests/5k Cargo.lock/17'))),
        'a lines block with a dot directory and a tenth': event(description(BLOCK, lines_block(
            'Lines added: .github/412 x/9.9k'))),
        'the lines block before the report block': event(LINES + '\n' + description(BLOCK, '')),
        'a comment after the lines line': event(description(BLOCK, lines_block(
            'Lines added: docs/3 <!-- counted 2026-10-22 -->'))),
        'the lines block with CRLF line endings': event(description(BLOCK).replace('\n', '\r\n')),
    }
    for case, payload in lines_passing.items():
        code, said = check(payload)
        assert code == 0, f'{case} failed:\n{said}'
        assert 'the figures were not compared' in said, f'{case}:\n{said}'
    lines_failing = {
        'no lines block': (event(description(BLOCK, '')), 'no lines added block'),
        'two lines blocks': (event(description(BLOCK, LINES + LINES)), '2 lines added blocks'),
        'an empty lines block': (event(description(BLOCK, lines_block(''))), 'holds 0 lines'),
        "the unfilled template's lines block": (event(TEMPLATE), 'holds 0 lines'),
        'a lines block of two lines': (event(description(BLOCK, lines_block(
            'Lines added: docs/3\nLines added: tests/4'))), 'holds 2 lines'),
        'a lines line of another shape': (event(description(BLOCK, lines_block(
            'Lines: docs/3'))), 'is not `Lines added: dir/figure'),
        'an exact figure past 1000': (event(description(BLOCK, lines_block(
            'Lines added: docs/5000'))), 'is not `Lines added: dir/figure'),
        'a figure without its directory': (event(description(BLOCK, lines_block(
            'Lines added: 5k'))), 'is not `Lines added: dir/figure'),
        'a lines block in a backtick fence': (event(description(BLOCK, '```\n' + LINES
                                                                + '```\n')), 'inside a code block'),
        'a lines block indented four spaces': (event(description(BLOCK, ''.join(
            '    ' + line for line in LINES.splitlines(keepends=True)))), 'inside a code block'),
        'a lines block inside <pre>': (event(description(BLOCK, '<pre>\n' + LINES + '</pre>\n')),
                                       'inside a raw HTML block'),
        'the lines line inside a comment': (event(description(BLOCK, lines_block(
            '<!-- Lines added: docs/3 -->'))), 'inside a code block, an HTML block or an HTML comment'),
        # A line that opens with a comment is an HTML block to GitHub, whatever follows the
        # comment on it (CommonMark 4.6, type 2).
        'the lines line after a comment that opens it': (event(description(BLOCK, lines_block(
            '<!-- counted --> Lines added: docs/3'))), 'inside a code block, an HTML block or an HTML comment'),
        'a lines block with its end marker first': (event(description(BLOCK,
            '<!-- /af-pr-lines -->\nLines added: none\n<!-- af-pr-lines:v1 -->\n')),
            'comes before the begin marker'),
        'a lines block without its end marker': (event(description(BLOCK,
            '<!-- af-pr-lines:v1 -->\nLines added: none\n')), 'has no end marker'),
    }
    for case, (payload, expected) in lines_failing.items():
        code, said = check(payload)
        assert code == 1, f'{case} passed:\n{said}'
        assert expected in said, f'{case}: expected {expected!r} in:\n{said}'
        assert 'scripts/pr-lines.py' in said, f'{case} does not say how to produce the block:\n{said}'
        if case != "the unfilled template's lines block":
            assert 'af task report: the description carries one well-formed block' in said, \
                f'{case}: the report block is judged on its own:\n{said}'
    # Both checks run: a description failing both says both.
    code, said = check(event(description(BLOCK.replace(pipeline + '\n', ''), '')))
    assert code == 1 and 'no pipeline line' in said and 'no lines added block' in said, said
    assert HOW in said and 'scripts/pr-lines.py' in said, said
    # --no-diff keeps the shape check and says the figures were not compared.
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / 'body.md'
        path.write_text(description(BLOCK, lines_block('Lines added: docs/3')), encoding='utf-8')
        result = subprocess.run([sys.executable, '-I', str(SCRIPT), '--body', str(path),
                                 '--no-diff'], capture_output=True, text=True, env={})
        assert result.returncode == 0, result.stdout + result.stderr
        assert 'the figures were not compared' in result.stdout, result.stdout
    print(f'check-pr-report: {len(passing) + 1} passing and {len(failing)} failing cases, '
          f'{len(TYPE6_AUDIT)} type 6 names; lines added: {len(lines_passing)} passing and '
          f'{len(lines_failing)} failing cases')


if __name__ == '__main__':
    main()
