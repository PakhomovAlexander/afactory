#!/usr/bin/env python3
"""Require the `af task report` block in a pull request description (ADR-0142).

Every change to this repository is made through af Tasks, and its pull request description
carries the `af task report` of those Tasks. This check reads only the pull request event
payload GitHub hands the workflow (`$GITHUB_EVENT_PATH`): no API call, no token. It cannot
compare the report with the Store it came from, which lives on the author's machine; it
checks that a well-formed block is there.

A pull request opened by Dependabot, or from a `release/` branch, is exempt. Anything else
passes only with exactly one block: the begin marker, then the end marker, and between them at
least one pipeline line (`**name@version**: ` and its steps, or `**unknown pipeline**: not
retained` for a Task whose plan is not retained), then the round table: a header with exactly
the six v1 columns in order, a separator row of that width, at least one round row, and a last
row whose Task cell starts with `Total:`. Every non-blank line after the separator, up to the
first blank line, is a row, with or without its outer `|` (GitHub renders both as rows), and
has exactly six cells; a round row's Round, Task and Outcome cells have visible content once
HTML comments are removed. A marker inside a fenced (``` or ~~~) or an indented code block is
text, not a marker: a block shown as code does not count, including one indented right after a
heading, a thematic break, a fence, an HTML block or a list item, which end a paragraph.

The workflow runs this script as the base branch holds it, on `pull_request_target`, so a pull
request can change neither the check nor the workflow that runs it (ADR-0142).

    check-pr-report.py [--event PATH]      the event payload (default: $GITHUB_EVENT_PATH)
    check-pr-report.py --body FILE         a description on its own, as a local check
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import re
import sys

BEGIN = '<!-- af-task-report:v1 -->'
END = '<!-- /af-task-report -->'
# The v1 round table header, exactly and in this order (ADR-0142).
COLUMNS = ['Round', 'Task', 'Outcome', 'Findings', 'Tokens', 'Active']
# The cells every round row fills: the row numbers its round, names its Task and says how it
# ended.
REQUIRED = ['Round', 'Task', 'Outcome']
# The Task cell of the table's last row, the totals row, starts with this text.
TOTAL = 'Total:'
# A pipeline line: `**name@version**: ` and at least one visible character of its steps.
PIPELINE = re.compile(r'^\*\*[^*\s]+@[^*\s]+\*\*: +\S')
# The pipeline line `af task report` prints for a Task whose plan is no longer retained (it was
# collected) or that never reached planning.
UNKNOWN_PIPELINE = '**unknown pipeline**: not retained'
# An HTML comment, or one left open to the end of the cell: neither is visible.
COMMENT = re.compile(r'<!--.*?(?:-->|$)', re.DOTALL)
FENCE = re.compile(r'^ {0,3}(`{3,}|~{3,})(.*)$')
# The blocks that end a paragraph (CommonMark 0.31): an ATX heading, a setext heading's
# underline, a thematic break and a list item; fences and HTML blocks are matched apart.
ATX = re.compile(r'^ {0,3}#{1,6}(?:[ \t]|$)')
SETEXT = re.compile(r'^ {0,3}(?:=+|-+)[ \t]*$')
THEMATIC = re.compile(r'^ {0,3}(?:(?:\*[ \t]*){3,}|(?:-[ \t]*){3,}|(?:_[ \t]*){3,})$')
LIST_ITEM = re.compile(r'^ {0,3}([-+*]|(\d{1,9})[.)])(?:[ \t]+(\S?)|$)')
# HTML block starts (CommonMark 4.6), matched on a line indented at most three columns: raw
# text (type 1), a comment (2), a processing instruction (3), a declaration (4), CDATA (5), a
# block-level tag (6) and any other lone tag (7). Types 6 and 7 end at a blank line.
HTML_RAW = re.compile(r'<(script|pre|style|textarea)(?:[ \t>]|$)', re.IGNORECASE)
HTML_BLOCK = re.compile(
    r'</?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd'
    r'|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset'
    r'|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|nav|noframes|ol'
    r'|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr'
    r'|track|ul)(?:[ \t]|/?>|$)', re.IGNORECASE)
HTML_TAG = re.compile(
    r'(?:<[A-Za-z][A-Za-z0-9-]*(?:[ \t]+[A-Za-z_:][\w.:-]*(?:[ \t]*=[ \t]*(?:[^ \t"\'=<>`]+'
    r'|\'[^\']*\'|"[^"]*"))?)*[ \t]*/?>|</[A-Za-z][A-Za-z0-9-]*[ \t]*>)[ \t]*$')
# A closing tag of a type 1 name, which is not a type 7 block either.
RAW_CLOSE = re.compile(r'</(?:script|pre|style|textarea)(?![A-Za-z0-9-])', re.IGNORECASE)
# The end condition of an HTML block that ends at a blank line.
BLANK = ''
EXEMPT_AUTHORS = {'dependabot[bot]'}
EXEMPT_BRANCH_PREFIX = 'release/'
HOW = ('Run `af task report TASK_ID...` for the af Tasks that made this change (from the '
       'repository, with the same --state if you used one) and paste its whole output, both '
       'markers included, into the "af task report" section of the description.')


def split(line):
    """The cells of `line` as GitHub splits a table row, whether or not it has its outer `|`,
    and whether a trailing `|` closed the last one.

    A backslash escapes the character after it, so `\\|` stays inside its cell, and every
    other `|` ends one; a leading `|` opens the first cell and a trailing `|` closes the last.
    """
    line = line.strip()
    found, cell, n, closed = [], '', 0, False
    while n < len(line):
        char = line[n]
        closed = char == '|'
        if char == '\\' and n + 1 < len(line):
            cell += line[n:n + 2]
            n += 2
            continue
        if char == '|':
            found.append(cell.strip())
            cell = ''
        else:
            cell += char
        n += 1
    if not closed:
        found.append(cell.strip())
    if line.startswith('|') and found:
        found.pop(0)
    return found, closed


def cells(line):
    """The cells of one Markdown table row written with both outer `|`, or None for any other
    line."""
    line = line.strip()
    if not line.startswith('|') or len(line) < 2:
        return None
    row, closed = split(line)
    return row if closed else None


def visible(cell):
    """What a cell shows once its HTML comments are removed."""
    return COMMENT.sub('', cell).strip()


def plural(n, one, many):
    return f'{n} {one if n == 1 else many}'


def is_separator(row):
    return bool(row) and all(cell and set(cell) <= set(':-') and '-' in cell for cell in row)


def html_block(text, paragraph):
    """The HTML block the stripped line `text` opens, as (opener length, end condition), or
    None. The end condition is the text whose line ends the block, searched after the opener,
    or BLANK for a block that ends at a blank line. A type 7 block, any other lone tag, cannot
    interrupt a paragraph (CommonMark 4.6)."""
    for opener, end in (('<!--', '-->'), ('<?', '?>'), ('<![CDATA[', ']]>')):
        if text.startswith(opener):
            return len(opener), end
    if match := HTML_RAW.match(text):
        return 1, f'</{match.group(1).lower()}>'
    if re.match(r'<![A-Za-z]', text):
        return 2, '>'
    if HTML_BLOCK.match(text) or (not paragraph and HTML_TAG.match(text)
                                  and not RAW_CLOSE.match(text)):
        return 1, BLANK
    return None


def ends_paragraph(line, paragraph):
    """Whether `line`, indented at most three columns, is a block after which no paragraph is
    open: an ATX heading, a setext heading's underline (only under a paragraph), a thematic
    break, or a list item. A list item interrupts a paragraph only when it has text and, if
    ordered, starts at 1; otherwise its line continues the paragraph."""
    if ATX.match(line) or THEMATIC.match(line) or (paragraph and SETEXT.match(line)):
        return True
    item = LIST_ITEM.match(line)
    if not item:
        return False
    return not paragraph or bool(item.group(3)) and (item.group(2) is None
                                                     or int(item.group(2)) == 1)


def code_lines(lines):
    """The indexes of the lines GitHub renders as code: inside a fenced code block, its fences
    included, or in an indented code block (four columns of indent, not continuing a paragraph).

    An ATX or setext heading, a thematic break, a fenced code block, an HTML block and a list
    item each end a paragraph, so a line indented four columns right after one, with no blank
    line between, is code. A line inside an HTML block, such as the multi-line comment of the
    pull request template's guidance, is not code however far it is indented; a fence opened
    inside an HTML block other than a comment still counts as code, the stricter reading. List
    items are not modelled as containers: a line indented under one counts as code, again the
    stricter reading.
    """
    code, fence, html, paragraph = set(), None, None, False
    for n, line in enumerate(lines):
        if fence:
            code.add(n)
            closing = FENCE.match(line)
            if (closing and closing.group(1)[0] == fence[0]
                    and len(closing.group(1)) >= len(fence) and not closing.group(2).strip()):
                fence = None
            continue
        opening = FENCE.match(line)
        opening = opening if opening and not (
            opening.group(1)[0] == '`' and '`' in opening.group(2)) else None
        if html is not None and not (opening and html != '-->'):
            if html == BLANK and not line.strip():
                html = None
            elif html != BLANK and html in line.lower():
                html = None
            paragraph = False
            continue
        html = None
        if opening:
            fence = opening.group(1)
            code.add(n)
            paragraph = False
            continue
        if not line.strip():
            paragraph = False
            continue
        indent = len(line) - len(line.lstrip(' \t'))
        if len(line[:indent].expandtabs(4)) >= 4:
            if not paragraph:
                code.add(n)
            continue
        text = line.strip()
        if block := html_block(text, paragraph):
            opener, end = block
            if end == BLANK or end not in text[opener:].lower():
                html = end
            paragraph = False
            continue
        paragraph = not ends_paragraph(line, paragraph)
    return code


def is_pipeline_line(line):
    """Whether `line` is a pipeline line: `**name@version**: ` and its steps, or the line the
    renderer prints for a Task whose plan is not retained."""
    return bool(PIPELINE.match(line.strip())) or line.strip() == UNKNOWN_PIPELINE


def problems(body):
    """What is missing from `body`, as messages; an empty list passes."""
    lines = (body or '').splitlines()
    code = code_lines(lines)
    marker = {n: line.strip() for n, line in enumerate(lines) if line.strip() in (BEGIN, END)}
    begins = [n for n, text in marker.items() if text == BEGIN and n not in code]
    ends = [n for n, text in marker.items() if text == END and n not in code]
    if not begins and not ends:
        if any(n in code for n in marker):
            return ['the af task report block is inside a code block (a ``` or ~~~ fence, or '
                    'indented four spaces): GitHub shows it as code, not as the report. Paste '
                    'it as plain text, outside any fence and without indenting it']
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
    found = []
    if not any(is_pipeline_line(line) for line in block[:header]):
        found.append('the af task report block has no pipeline line before its round table: '
                     'expected a line like `**name@version**: implement (...) → gate (...)`')
    if header is None:
        found.append('the af task report block has no round table: expected a header row with '
                     + ', '.join(COLUMNS))
        return found
    names = cells(block[header])
    width = len(COLUMNS)
    if names != COLUMNS:
        missing = list((Counter(COLUMNS) - Counter(names)).elements())
        extra = list((Counter(names) - Counter(COLUMNS)).elements())
        if missing:
            found.append('the round table is missing the column'
                         + ('s ' if len(missing) > 1 else ' ') + ', '.join(missing))
        if extra:
            found.append('the round table has the extra column'
                         + ('s ' if len(extra) > 1 else ' ') + ', '.join(extra)
                         + f': the v1 header has exactly {width}')
        if not missing and not extra:
            found.append('the round table columns are out of order: the v1 header is '
                         + ' | '.join(COLUMNS))
    rows = []
    below = cells(block[header + 1]) if header + 1 < len(block) else None
    separator = is_separator(below)
    if not separator:
        found.append('the round table header is not followed by its `| --- |` separator row')
    elif len(below) != width:
        found.append(f'the round table separator row has {plural(len(below), "cell", "cells")}'
                     f', but the header has {width}: one `| --- |` per column')
        separator = False
    else:
        # GitHub renders every line up to the first blank one as a row, outer `|` or not.
        for line in block[header + 2:]:
            if not line.strip():
                break
            rows.append((line.strip(), split(line)[0]))
    if not separator:
        return found
    task = COLUMNS.index('Task')
    total = rows[-1] if rows and len(rows[-1][1]) == width else None
    if total is None or not visible(total[1][task]).startswith(TOTAL):
        found.append(f'the round table has no `{TOTAL}` row: its last row must be the totals '
                     f'row, whose Task cell starts with `{TOTAL}`')
        total = None
    rounds = rows[:-1] if total else rows
    if not rounds:
        found.append('the round table has no round row')
    required = [(COLUMNS.index(column), column) for column in REQUIRED]
    for number, (line, row) in enumerate(rounds, 1):
        if len(row) == 1 and width > 1:
            found.append(f'round row {number} of the round table is a one-cell placeholder '
                         f'(`{line}`), not a round row with {width} cells')
        elif len(row) != width:
            found.append(f'round row {number} of the round table has '
                         f'{plural(len(row), "cell", "cells")}, but the header has {width}: '
                         f'`{line}`')
        else:
            empty = [column for index, column in required if not visible(row[index])]
            if empty:
                found.append(f'round row {number} of the round table has an empty '
                             + ', '.join(empty) + (' cell' if len(empty) == 1 else ' cells')
                             + ' (an HTML comment shows nothing)')
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
