#!/usr/bin/env python3
"""Require the `af task report` block in a pull request description (ADR-0142).

Every change to this repository is made through af Tasks, and its pull request description
carries the `af task report` of those Tasks. This check reads only the pull request event
payload GitHub hands the workflow (`$GITHUB_EVENT_PATH`): no API call, no token. It cannot
compare the report with the Store it came from, which lives on the author's machine; it
checks that a well-formed block is there.

A pull request opened by Dependabot, or from a `release/` branch of this repository itself (a
fork's branch of that name is not), is exempt. Anything else
passes only with exactly one block: the begin marker, then the end marker, and between them at
least one pipeline line (`**name@version**: ` and its steps, or `**unknown pipeline**: not
retained` for a Task whose plan is not retained), then the round table: a header with exactly
the six v1 columns in order, a separator row of that width, at least one round row, and a last
row whose Task cell starts with `Total:`. Every non-blank line after the separator, up to the
first blank line, is a row, with or without its outer `|` (GitHub renders both as rows), and
has exactly six cells; a round row's Round, Task and Outcome cells have visible content once
HTML comments are removed. A marker inside a fenced (``` or ~~~) or an indented code block is
text, not a marker: a block shown as code does not count, including one indented right after a
heading, a thematic break, a fence, an HTML block or a list item, which end a paragraph. Nor
is a marker inside a raw HTML block, opened by `<pre>`, `<script>`, `<style>` or `<textarea>` and
running to the first line that holds a closing tag of any of the four: GitHub shows its lines as
literal text, or not at all. Between the markers, a pipeline line or a table line counts only
for what GitHub renders of it as Markdown: not in a code block, not inside an HTML block, which
a fence inside it does not end, and not inside an HTML comment, which the browser keeps open
from its `<!--`, in an HTML block or inline in a paragraph, across blank lines and blocks, to
the first `-->` of raw HTML after it, nor inside the quoted attribute value of a tag an HTML
block leaves open, up to that quote in raw HTML, nor starting inside a code span opened on a
paragraph line above it, which GitHub shows as code.

The description also carries the lines the pull request adds per directory (ADR-0195): exactly
one block between `<!-- af-pr-lines:v1 -->` and `<!-- /af-pr-lines -->`, rendered as text by the
same rules, holding exactly one line `Lines added: dir/figure ...` (or `Lines added: none`) as
`scripts/pr-lines.py` prints it. Given the pull request's commits — the event's `base.sha` and
`head.sha`, which the workflow fetched, or `--diff BASE HEAD` — the figures are counted again
and a block that differs is refused, with the line to paste; without them the shape alone is
checked, and the check says so.

The workflow runs this script as the base branch holds it, on `pull_request_target`, so a pull
request can change neither the check nor the workflow that runs it (ADR-0142).

    check-pr-report.py [--event PATH]      the event payload (default: $GITHUB_EVENT_PATH)
    check-pr-report.py --body FILE         a description on its own, as a local check
    ... [--repo DIR] [--diff BASE HEAD]    where and what to count the lines added from
    ... --no-diff                          check the lines block's shape only
"""
import argparse
from collections import Counter
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
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
# The type 6 names are those GitHub's renderer holds (GFM 0.29), checked against what GitHub
# renders: `source` is one, and `search`, which only the newer CommonMark 0.31 list adds, is
# not, so a lone `<search>` is a type 7 block, which cannot interrupt a paragraph.
HTML_BLOCK = re.compile(
    r'</?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd'
    r'|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset'
    r'|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|nav|noframes|ol'
    r'|optgroup|option|p|param|section|source|summary|table|tbody|td|tfoot|th|thead|title|tr'
    r'|track|ul)(?:[ \t]|/?>|$)', re.IGNORECASE)
HTML_TAG = re.compile(
    r'(?:<[A-Za-z][A-Za-z0-9-]*(?:[ \t]+[A-Za-z_:][\w.:-]*(?:[ \t]*=[ \t]*(?:[^ \t"\'=<>`]+'
    r'|\'[^\']*\'|"[^"]*"))?)*[ \t]*/?>|</[A-Za-z][A-Za-z0-9-]*[ \t]*>)[ \t]*$')
# A closing tag of a type 1 name, which is not a type 7 block either.
RAW_CLOSE = re.compile(r'</(?:script|pre|style|textarea)(?![A-Za-z0-9-])', re.IGNORECASE)
# The end condition of an HTML block: the texts any of which ends it on the line holding one.
# A raw block (type 1) ends at the closing tag of any of the four names, not only its own, as
# GitHub renders it; a comment (type 2) at `-->`; a block that ends at a blank line at none.
RAW_ENDS = ('</script>', '</pre>', '</style>', '</textarea>')
COMMENT_ENDS = ('-->',)
BLANK = ()
# Raw HTML inside a paragraph, a heading or a table cell (CommonMark 6.6): a comment, a
# processing instruction, a declaration, CDATA, an open tag or a closing tag; it may span the
# lines of a paragraph. An autolink is not raw HTML, and is matched first.
INLINE_HTML = re.compile(
    r'<!---?>|<!--.*?-->|<\?.*?\?>|<![A-Za-z][^>]*>|<!\[CDATA\[.*?\]\]>'
    r'|<[A-Za-z][A-Za-z0-9-]*(?:\s+[A-Za-z_:][\w.:-]*(?:\s*=\s*(?:[^\s"\'=<>`]+|\'[^\']*\''
    r'|"[^"]*"))?)*\s*/?>|</[A-Za-z][A-Za-z0-9-]*\s*>', re.DOTALL)
AUTOLINK = re.compile(r'<[A-Za-z][A-Za-z0-9+.-]{1,31}:[^\s<>]*>|<[^\s<>@\\]+@[A-Za-z0-9.-]+>')
PUNCTUATION = set('!"#$%&\'()*+,-./:;<=>?@[\\]^_`{|}~')
LINES_BEGIN = '<!-- af-pr-lines:v1 -->'
LINES_END = '<!-- /af-pr-lines -->'
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
    for opener, end in (('<!--', COMMENT_ENDS), ('<?', ('?>',)), ('<![CDATA[', (']]>',))):
        if text.startswith(opener):
            return len(opener), end
    if HTML_RAW.match(text):
        return 1, RAW_ENDS
    if re.match(r'<![A-Za-z]', text):
        return 2, ('>',)
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


def closes(end, text):
    """Whether `text` holds the end condition `end` of an HTML block."""
    return any(token in text.lower() for token in end)


class Comments:
    """Where the browser hides text in an HTML comment, fed the raw HTML GitHub emits in order.

    The comment state is the browser's, not Markdown's: a `<!--` opened in an HTML block, or
    inline in a paragraph, stays open across the end of that block, blank lines and the blocks
    after it, until the first `-->` of raw HTML (an HTML block or inline raw HTML). Markdown
    text never closes it, as GitHub escapes its `>`. A `<!--` inside a tag's quoted attribute
    value opens nothing. `<?`, `<!` and `</` not starting a tag open a bogus comment, which ends
    at the first `>`. A tag an HTML block leaves inside a quoted attribute value at its blank
    line likewise hides what follows, up to that quote in raw HTML, which Markdown text never
    holds either, as GitHub escapes its `"`.
    """

    def __init__(self):
        self.close = None  # the text that ends the open comment, or None
        self.tag = None  # inside a tag: '' or '=', or the quote of an attribute value

    def hides(self):
        """Whether the browser hides the text it reads next: in a comment, or in a tag's
        quoted attribute value."""
        return bool(self.close) or self.tag in ('"', "'")

    def feed(self, text, start=0, end=None):
        """Read the raw HTML `text[start:end]`; return the (start, end) spans it hides."""
        end = len(text) if end is None else end
        hidden, n = [], start
        while n < end:
            if self.close:
                ends = (self.close, '--!>') if self.close == '-->' else (self.close,)
                found = min(((at, close) for close in ends
                             if (at := text.find(close, n, end)) != -1), default=None)
                if not found:
                    hidden.append((n, end))
                    return hidden
                at, close = found
                hidden.append((n, at + len(close)))
                n, self.close = at + len(close), None
            elif self.tag in ('"', "'"):
                at = text.find(self.tag, n, end)
                if at == -1:
                    return hidden
                n, self.tag = at + 1, ''
            elif self.tag is not None:
                char = text[n]
                if char == '>':
                    self.tag = None
                elif char in '"\'' and self.tag == '=':
                    self.tag = char
                elif not char.isspace():
                    self.tag = '=' if char == '=' else ''
                n += 1
            else:
                at = text.find('<', n, end)
                if at == -1:
                    return hidden
                rest = text[at:at + 6]
                if rest.startswith(('<!-->', '<!--->')):
                    width = 5 if rest.startswith('<!-->') else 6
                    hidden.append((at, at + width))
                    n = at + width
                elif rest.startswith('<!--'):
                    hidden.append((at, at + 4))
                    n, self.close = at + 4, '-->'
                elif re.match(r'</?[A-Za-z]', rest):
                    n, self.tag = at + 1, ''
                elif rest.startswith(('<!', '<?')) or (rest.startswith('</') and rest[2:3] != '>'
                                                       and at + 2 < end):
                    hidden.append((at, at + 2))
                    n, self.close = at + 2, '>'
                else:
                    n = at + 1
        return hidden


def inline(text):
    """The code spans and the raw HTML in the inline content `text` of a paragraph, a heading
    or a table cell, as (kind, start, end): kind `code` for a code span, its backtick strings
    included, which may span the lines of a paragraph; kind `html` for raw HTML Markdown passes
    through, not escaped by a backslash, not in a code span, and complete within `text`, or
    Markdown shows it as text."""
    n = 0
    while n < len(text):
        char = text[n]
        if char == '\\' and n + 1 < len(text) and text[n + 1] in PUNCTUATION:
            n += 2
        elif char == '`':
            ticks = len(text[n:]) - len(text[n:].lstrip('`'))
            closing = re.compile(r'(?<!`)' + '`' * ticks + r'(?!`)').search(text, n + ticks)
            if closing:
                yield 'code', n, closing.end()
            n = closing.end() if closing else n + ticks
        elif char == '<' and (link := AUTOLINK.match(text, n)):
            n = link.end()
        elif char == '<' and (raw := INLINE_HTML.match(text, n)):
            yield 'html', n, raw.end()
            n = raw.end()
        else:
            n += 1


def unrendered(lines):
    """Which lines GitHub does not render as Markdown, and what it shows of the others, as
    (code, raw, spanned, shown).

    `code` holds the indexes of the lines rendered as code: inside a fenced code block, its
    fences included, or in an indented code block (four columns of indent, not continuing a
    paragraph). `raw` holds those of a raw HTML block (`<pre>`, `<script>`, `<style>` or
    `<textarea>`), its opening and closing lines included, which GitHub shows as literal text
    or hides. `spanned` holds those of the paragraph lines that start inside a code span opened
    on a line above them. `shown` holds each line as GitHub renders it as Markdown: a code line
    or a line of any HTML block as nothing, a spanned line after its span's opening backticks,
    and any other line without what an HTML comment or an open tag's quoted attribute value
    hides of it (see Comments), a line wholly hidden reading as a blank one.

    An ATX or setext heading, a thematic break, a fenced code block, an HTML block and a list
    item each end a paragraph, so a line indented four columns right after one, with no blank
    line between, is code. A line inside an HTML block, such as the multi-line comment of the
    pull request template's guidance, is not code however far it is indented; a fence opened
    inside an HTML block other than a comment or a raw block still counts as code, the stricter
    reading, but its lines stay the HTML block's up to the block's end, as GitHub shows them,
    and the fence runs on past that end to its own close. A raw block runs to the first line
    holding a closing tag of any of its four names,
    or to the end: a fence inside it is its text and opens nothing. A tag nested inside an HTML
    block does not change where that block ends (CommonMark 4.6), so a `<pre>` inside a `<div>`
    opens no raw block. List items are not modelled as containers: a line indented under one
    counts as code, again the stricter reading. The inline content of a paragraph spans its
    lines, while each row of a table is its own: a comment opened in one row and closed in the
    next is text, and so is a backtick string, which opens no code span across rows. A table
    takes its header from the paragraph line right above its delimiter row, so a code span
    opened above that line does not reach into the table.
    """
    code, raw, spanned, shown = set(), set(), set(), list(lines)
    comments, run = Comments(), []
    fence, html, paragraph, table = None, None, False, False
    # The tag state an HTML block opened in, which its end restores (see block_line).
    carried = None

    def flush():
        """Read the inline content of the lines in `run`, and show what it does not hide."""
        if not run:
            return
        text = '\n'.join(lines[n] for n in run)
        hidden, at, spans = [False] * len(text), 0, []
        for kind, start, end in inline(text):
            if kind == 'code':
                spans.append((start, end))
                continue
            # The text between raw HTML is escaped: it neither opens nor closes a comment.
            gone = ([(at, start)] if comments.hides() else []) + comments.feed(text, start, end)
            for first, last in gone:
                hidden[first:last] = [True] * (last - first)
            at = end
        if comments.hides():
            hidden[at:] = [True] * (len(text) - at)
        offset = 0
        for n in run:
            line = lines[n]
            gone = hidden[offset:offset + len(line)]
            shown[n] = ''.join(char for char, hide in zip(line, gone) if not hide)
            # A line that starts inside a code span opened on a line above it is that span's
            # code: it reads with the span's opening backticks before it, so neither a pipeline
            # line nor a table row.
            opened = next((start for start, end in spans if start < offset < end), None)
            if opened is not None and not hidden[offset]:
                ticks = len(text[opened:]) - len(text[opened:].lstrip('`'))
                shown[n] = '`' * ticks + shown[n]
                spanned.add(n)
            offset += len(line) + 1
        run.clear()

    def block_line(n, line, end):
        """Read line `n` of the open HTML block whose end condition is `end`; return `end`, or
        None when the line ends the block."""
        if end == BLANK and not line.strip():
            # A tag left in a quoted attribute value runs on into what GitHub renders next;
            # any other tag left open ends at the `>` of the next element.
            if comments.tag not in ('"', "'"):
                comments.tag = None
            return None
        if end == RAW_ENDS:
            raw.add(n)
        shown[n] = ''
        comments.feed(line)
        if closes(end, line):
            comments.tag = carried
            return None
        return end

    for n, line in enumerate(lines):
        if fence:
            code.add(n)
            shown[n] = ''
            if html is not None:
                html = block_line(n, line, html)
            closing = FENCE.match(line)
            if (closing and closing.group(1)[0] == fence[0]
                    and len(closing.group(1)) >= len(fence) and not closing.group(2).strip()):
                fence = None
            continue
        opening = FENCE.match(line)
        opening = opening if opening and not (
            opening.group(1)[0] == '`' and '`' in opening.group(2)) else None
        if html is not None:
            if opening and html not in (COMMENT_ENDS, RAW_ENDS):
                # The fence is the HTML block's text up to the block's end, yet counts as code.
                fence = opening.group(1)
                code.add(n)
            html = block_line(n, line, html)
            paragraph = False
            continue
        if opening:
            flush()
            fence, table = opening.group(1), False
            code.add(n)
            shown[n] = ''
            paragraph = False
            continue
        if not line.strip():
            flush()
            paragraph = table = False
            continue
        indent = len(line) - len(line.lstrip(' \t'))
        if len(line[:indent].expandtabs(4)) >= 4:
            if not paragraph:
                flush()
                table = False
                code.add(n)
                shown[n] = ''
            else:
                if table:
                    flush()
                run.append(n)
            continue
        text = line.strip()
        if block := html_block(text, paragraph):
            flush()
            opener, end = block
            if end == RAW_ENDS:
                raw.add(n)
            shown[n], table, carried = '', False, comments.tag
            comments.feed(line)
            if end == BLANK or not closes(end, text[opener:]):
                html = end
            else:
                comments.tag = carried
            paragraph = False
            continue
        breaks = ends_paragraph(line, True)
        row = split(line)[0]
        if table and not breaks:
            flush()
            run.append(n)
        elif (run and run[-1] == n - 1 and '|' in line and is_separator(row)
              and len(split(lines[n - 1])[0]) == len(row)):
            # A delimiter row: the line above it is the table's header, no longer the
            # paragraph's, and each row from here on is inline content of its own.
            header = run.pop()
            flush()
            run.append(header)
            flush()
            run.append(n)
            table = True
        else:
            table = False
            if breaks:
                flush()
            run.append(n)
            if breaks and not LIST_ITEM.match(line):
                flush()
        paragraph = not ends_paragraph(line, paragraph)
    flush()
    return code, raw, spanned, shown


def is_pipeline_line(line):
    """Whether `line` is a pipeline line: `**name@version**: ` and its steps, or the line the
    renderer prints for a Task whose plan is not retained."""
    return bool(PIPELINE.match(line.strip())) or line.strip() == UNKNOWN_PIPELINE


def problems(body):
    """What is missing from `body`, as messages; an empty list passes."""
    lines = (body or '').splitlines()
    code, raw, spanned, shown = unrendered(lines)
    marker = {n: line.strip() for n, line in enumerate(lines) if line.strip() in (BEGIN, END)}
    # A marker line that closes a comment still counts: it is the marker, not the comment.
    begins = [n for n, text in marker.items() if text == BEGIN and n not in code | raw]
    ends = [n for n, text in marker.items() if text == END and n not in code | raw]
    if not begins and not ends:
        if any(n in raw for n in marker):
            return ['the af task report block is inside a raw HTML block (<pre>, <script>, '
                    '<style> or <textarea>): GitHub shows it as literal text, or hides it, not '
                    'as the report. Paste it as plain text, outside any such element']
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
    inside = range(begins[0] + 1, ends[0])
    found = block_problems([shown[n] for n in inside])
    # A pipeline line or a table line GitHub shows as code, as literal text, or hides, is said
    # apart.
    lost = [n for n in inside if is_pipeline_line(lines[n]) and not is_pipeline_line(shown[n])
            or cells(lines[n]) and not cells(shown[n])]
    if found and any(n in spanned for n in lost):
        found.insert(0, 'part of the af task report block is inside a code span (a ` or `` '
                     'string of backticks on a line above it, closed on a line below): GitHub '
                     'shows it as code, not as the report. Paste the block as plain text, with '
                     'no backticks around its lines')
    if found and any(n not in code | spanned for n in lost):
        found.insert(0, 'part of the af task report block is inside an HTML block, such as '
                     '<pre>, or an HTML comment: GitHub shows it as literal text, or hides it, '
                     'not as the report. Paste the block as plain text, outside any HTML element '
                     'or comment')
    if found and any(n in code for n in lost):
        found.insert(0, 'part of the af task report block is inside a code block (a ``` or ~~~ '
                     'fence, or indented four spaces): GitHub shows it as code, not as the '
                     'report. Paste the block as plain text, outside any fence and without '
                     'indenting it')
    return found


def block_problems(block):
    """What is missing from the lines between the markers, as GitHub shows them, as messages;
    an empty list passes."""
    found, header = [], None
    for n, line in enumerate(block):
        row = cells(line)
        if row and len(set(row) & set(COLUMNS)) >= 3:
            header = n
            break
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


def pr_lines():
    """The `pr-lines.py` module beside this script: the block's renderer and parser."""
    spec = importlib.util.spec_from_file_location('pr_lines', Path(__file__).with_name('pr-lines.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def lines_problems(body, expected=None):
    """What is wrong with the lines added block of `body`, as messages; an empty list passes.
    `expected` is the line the pull request's commits give (`pr-lines.render`), or None to
    check the block's shape alone."""
    tool = pr_lines()
    lines = (body or '').splitlines()
    code, raw, spanned, shown = unrendered(lines)
    marker = {n: line.strip() for n, line in enumerate(lines)
              if line.strip() in (LINES_BEGIN, LINES_END)}
    begins = [n for n, text in marker.items() if text == LINES_BEGIN and n not in code | raw]
    ends = [n for n, text in marker.items() if text == LINES_END and n not in code | raw]
    if not begins and not ends:
        if any(n in raw for n in marker):
            return ['the lines added block is inside a raw HTML block (<pre>, <script>, '
                    '<style> or <textarea>): GitHub shows it as literal text, or hides it. '
                    'Paste it as plain text, outside any such element']
        if any(n in code for n in marker):
            return ['the lines added block is inside a code block (a ``` or ~~~ fence, or '
                    'indented four spaces): GitHub shows it as code. Paste it as plain text, '
                    'outside any fence and without indenting it']
        return [f'no lines added block: the description has neither `{LINES_BEGIN}` nor '
                f'`{LINES_END}`']
    if len(begins) > 1 or len(ends) > 1:
        return [f'{max(len(begins), len(ends))} lines added blocks: keep exactly one']
    if not begins:
        return [f'the lines added block has no begin marker `{LINES_BEGIN}`']
    if not ends:
        return [f'the lines added block has no end marker `{LINES_END}`']
    if ends[0] < begins[0]:
        return [f'the end marker `{LINES_END}` comes before the begin marker `{LINES_BEGIN}`']
    inside = range(begins[0] + 1, ends[0])
    content = [shown[n].strip() for n in inside if shown[n].strip()]
    hidden = [n for n in inside if lines[n].strip() and not shown[n].strip()]
    if len(content) != 1:
        if not content and hidden:
            return ['the lines added block\'s line is inside a code block, an HTML block or '
                    'an HTML comment: GitHub does not show it as text. Paste the block as '
                    'plain text, outside any fence, element or comment']
        return [f'the lines added block holds {plural(len(content), "line", "lines")}: it holds '
                f'exactly one, `{tool.LEAD}dir/figure ...` or `{tool.LEAD}{tool.NONE}`']
    figures = tool.parse(content[0])
    if figures is None:
        return [f'the lines added line is not `{tool.LEAD}dir/figure ...` (figures as '
                f'`412`, `1.5k`, `23k`) or `{tool.LEAD}{tool.NONE}`: `{content[0]}`']
    if expected is not None and tool.parse(expected) != figures:
        return ['the lines added figures are not those of this pull request\'s commits: '
                f'expected `{expected}`']
    return []


def exemption(event):
    """Why `event`'s pull request needs no block, or None."""
    pull = (event or {}).get('pull_request') or {}
    author = ((pull.get('user') or {}).get('login') or '')
    head = pull.get('head') or {}
    branch = head.get('ref') or ''
    # Anyone can name a fork's branch `release/…`: only the base repository's own counts.
    head_repo = (head.get('repo') or {}).get('full_name') or ''
    base_repo = ((pull.get('base') or {}).get('repo') or {}).get('full_name') or ''
    if author in EXEMPT_AUTHORS:
        return f'opened by {author}'
    if branch.startswith(EXEMPT_BRANCH_PREFIX) and head_repo and head_repo == base_repo:
        return f'a release pull request from {branch}'
    return None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    source = parser.add_mutually_exclusive_group()
    source.add_argument('--event', type=Path, help='pull request event payload (JSON)')
    source.add_argument('--body', type=Path, help='a pull request description (Markdown)')
    parser.add_argument('--repo', type=Path, default=Path('.'),
                        help='the repository the lines added are counted in (default: .)')
    parser.add_argument('--diff', nargs=2, metavar=('BASE', 'HEAD'),
                        help='count the lines added from BASE...HEAD (with --body; an event '
                             'names its own commits)')
    parser.add_argument('--no-diff', action='store_true',
                        help='check the lines added block\'s shape only')
    args = parser.parse_args(argv)
    base = head = None
    if args.body is not None:
        body = args.body.read_text(encoding='utf-8')
        if args.diff:
            base, head = args.diff
    else:
        path = args.event or os.environ.get('GITHUB_EVENT_PATH')
        if not path:
            parser.error('give --event PATH, --body FILE or set GITHUB_EVENT_PATH')
        event = json.loads(Path(path).read_text(encoding='utf-8'))
        if why := exemption(event):
            print(f'af task report: not required, {why}')
            return 0
        pull = event.get('pull_request') or {}
        body = pull.get('body') or ''
        base = (pull.get('base') or {}).get('sha')
        head = (pull.get('head') or {}).get('sha')
    found = problems(body)
    if not found:
        print('af task report: the description carries one well-formed block')
    for problem in found:
        print(f'af task report: {problem}', file=sys.stderr)
    if found:
        print(f'af task report: {HOW}', file=sys.stderr)
    expected, lines_found = None, []
    if base and head and not args.no_diff:
        tool = pr_lines()
        try:
            expected = tool.render(tool.counts(tool.numstat(args.repo, base, head)))
        except subprocess.CalledProcessError as error:
            lines_found.append(f'could not count the lines added: git diff {base}...{head} '
                               f'failed in {args.repo}: '
                               f'{error.stderr.decode("utf-8", "replace").strip()}')
    if not lines_found:
        lines_found = lines_problems(body, expected)
    if not lines_found:
        print('lines added: the description carries one well-formed block'
              + ('; the figures match the pull request\'s commits' if expected is not None
                 else '; the figures were not compared with any commits'))
    for problem in lines_found:
        print(f'lines added: {problem}', file=sys.stderr)
    if lines_found:
        print(f'lines added: {pr_lines().HOW}', file=sys.stderr)
    return 1 if found or lines_found else 0


if __name__ == '__main__':
    sys.exit(main())
