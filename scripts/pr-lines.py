#!/usr/bin/env python3
"""Count the lines a pull request adds, per directory, and print the block its description
carries (ADR-0195).

    pr-lines.py [--repo DIR] [--base REF] [--head REF]   the block for BASE...HEAD
    pr-lines.py ... --json                                 the counts as JSON, for a tool

The range is the one GitHub shows for a pull request, `BASE...HEAD`: what HEAD added since it
forked from BASE (`origin/main` and `HEAD` by default; a stacked pull request names its base
branch). Each added line counts under the first component of its path — `crates`, `docs`,
`scripts` — except a line of a file under a directory named `tests`, at any depth, which counts
under `tests` (`crates/af/tests/it/x.rs`), and a file at the repository root, which counts under
its own name (`Cargo.lock`). Renames are detected, so a moved file adds only what changed in it,
and a binary file adds nothing.

The block is a title with the total, `**Lines added: 18.9k**`, and a table with one row per
directory, the largest first: the directory, its figure, and its share of the total as a bar
and a percentage (`| crates | 7.5k | ████████████ 40% |`). A figure is exact below 1000, to a
tenth of a thousand up to 10k (`5k`, `7.9k`) and to a thousand from there (`23k`); the bar is
the row's count against the largest row's, in up to twelve cells of eighth blocks, a sliver at
least; a pull request that adds no line says `**Lines added: none**`. The `PR report` check
recomputes the block from the pull request's own commits and refuses a description whose block
is missing or differs, printing the block to paste.
"""
import argparse
from collections import Counter
import json
from pathlib import Path
import re
import subprocess
import sys

BEGIN = '<!-- af-pr-lines:v1 -->'
END = '<!-- /af-pr-lines -->'
NONE = 'none'
TESTS = 'tests'
HEADER = ['Directory', 'Added', 'Share']
SEPARATOR_ROW = '|:--|--:|:--|'
BAR_CELLS = 12
BLOCKS = '▏▎▍▌▋▊▉█'
FIGURE = re.compile(r'^(?:[1-9]\d{0,2}|0|[1-9]\d{0,2}(?:\.\d)?k)$')
TITLE = re.compile(r'^\*\*Lines added: (none|0|[1-9]\d{0,2}|[1-9]\d{0,2}(?:\.\d)?k)\*\*$')
SEPARATOR = re.compile(r'^:?-+:?$')
SHARE = re.compile(r'^([▏▎▍▌▋▊▉█]+) (<1%|\d{1,3}%)$')
HOW = ('Run `python3 scripts/pr-lines.py` from this repository on the pull request\'s branch '
       '(with its base fetched; `--base REF` for a base other than origin/main) and paste its '
       'output, both markers included, into the template\'s `Lines added` section; after a '
       'push, run it again and replace the block when it changed.')


def numstat(repo, base, head):
    """(added, deleted, path) for each file `BASE...HEAD` changes, a rename under its new path;
    `added` and `deleted` are None for a binary file."""
    out = subprocess.run(
        ['git', '-C', str(repo), 'diff', '--numstat', '-z', '-M', '--no-color', '--no-ext-diff',
         f'{base}...{head}', '--'],
        capture_output=True, check=True).stdout
    fields = out.split(b'\0')
    entries, index = [], 0
    while index < len(fields):
        record = fields[index]
        index += 1
        if not record:
            continue
        added, deleted, path = record.split(b'\t', 2)
        if not path:
            # A rename: the old path, then the new one, each in a field of its own.
            path = fields[index + 1]
            index += 2
        entries.append((None if added == b'-' else int(added),
                        None if deleted == b'-' else int(deleted),
                        path.decode('utf-8', 'replace')))
    return entries


def group(path):
    """The directory a path's lines count under."""
    parts = path.split('/')
    if TESTS in parts[:-1]:
        return TESTS
    return parts[0]


def counts(entries):
    """Lines added per directory, the directories with none left out."""
    totals = Counter()
    for added, _, path in entries:
        if added:
            totals[group(path)] += added
    return dict(totals)


def figure(n):
    """A count as the block shows it: exact below 1000, to a tenth of a thousand below 10k, to
    a thousand from there; half rounds up."""
    if n < 1000:
        return str(n)
    if n < 10_000:
        tenths = int(n / 100 + 0.5)
        if tenths >= 100:
            return '10k'
        return f'{tenths // 10}k' if tenths % 10 == 0 else f'{tenths // 10}.{tenths % 10}k'
    return f'{int(n / 1000 + 0.5)}k'


def bar(n, largest):
    """`n` against `largest` as a bar of up to BAR_CELLS cells in eighth blocks, a sliver at
    least."""
    eighths = int(8 * BAR_CELLS * n / largest + 0.5)
    full, rest = divmod(eighths, 8)
    return BLOCKS[7] * full + (BLOCKS[rest - 1] if rest else '') or BLOCKS[0]


def share(n, total):
    """`n` as a percentage of `total`, half rounding up; `<1%` below a half."""
    percent = 100 * n / total
    return f'{int(percent + 0.5)}%' if percent >= 0.5 else '<1%'


def escape(name):
    return name.replace('|', '\\|')


def render(totals):
    """The block's body for `totals` (directory → lines added): the title, and the table when
    anything was added."""
    if not totals:
        return f'**Lines added: {NONE}**'
    ordered = sorted(totals.items(), key=lambda item: (-item[1], item[0]))
    total, largest = sum(totals.values()), ordered[0][1]
    lines = [f'**Lines added: {figure(total)}**', '',
             '| ' + ' | '.join(HEADER) + ' |', SEPARATOR_ROW]
    lines.extend(f'| {escape(name)} | {figure(n)} | {bar(n, largest)} {share(n, total)} |'
                 for name, n in ordered)
    return '\n'.join(lines)


def block(totals):
    return f'{BEGIN}\n{render(totals)}\n{END}\n'


def row_cells(line):
    """The cells of a table row written with both outer `|`, a `\\|` read as a `|`; None for
    any other line."""
    text = line.strip()
    if len(text) < 2 or not text.startswith('|') or not text.endswith('|') \
            or text.endswith('\\|'):
        return None
    cells, cell, escaped = [], '', False
    for char in text[1:-1]:
        if escaped:
            cell += char if char == '|' else '\\' + char
            escaped = False
        elif char == '\\':
            escaped = True
        elif char == '|':
            cells.append(cell.strip())
            cell = ''
        else:
            cell += char
    cells.append(cell.strip() + ('\\' if escaped else ''))
    return cells


def parse(lines):
    """The block's body `lines` (what it shows, blank lines or not) read back:
    `({'total': figure, 'rows': frozenset of (directory, figure, bar, share)}, None)`, or
    `(None, what is wrong)`."""
    lines = [line.strip() for line in lines if line.strip()]
    if not lines:
        return None, 'the block is empty'
    title = TITLE.match(lines[0])
    if not title:
        return None, (f'the first line is not the title `**Lines added: <figure>**` or '
                      f'`**Lines added: {NONE}**`: `{lines[0]}`')
    if title.group(1) == NONE:
        if len(lines) > 1:
            return None, f'`**Lines added: {NONE}**` is the whole block, but more follows: `{lines[1]}`'
        return {'total': NONE, 'rows': frozenset()}, None
    header = '| ' + ' | '.join(HEADER) + ' |'
    if len(lines) < 4:
        return None, (f'the title is not followed by the table: the header `{header}`, its '
                      f'separator `{SEPARATOR_ROW}` and one row per directory')
    if row_cells(lines[1]) != HEADER:
        return None, f'the line after the title is not the table header `{header}`: `{lines[1]}`'
    separator = row_cells(lines[2])
    if separator is None or len(separator) != 3 or not all(SEPARATOR.match(c) for c in separator):
        return None, f'the header is not followed by its separator `{SEPARATOR_ROW}`: `{lines[2]}`'
    rows = {}
    for line in lines[3:]:
        cells = row_cells(line)
        if cells is None or len(cells) != 3:
            return None, f'not a row of three cells `| directory | figure | bar share |`: `{line}`'
        name, shown, rest = cells
        shared = SHARE.match(rest)
        if not name or not FIGURE.match(shown) or not shared:
            return None, ('not a row `| directory | figure | bar share |` (a figure as `412`, '
                          '`1.5k` or `23k`; the share as `40%` or `<1%` after its bar): '
                          f'`{line}`')
        if name in rows:
            return None, f'`{name}` has two rows'
        rows[name] = (shown, shared.group(1), shared.group(2))
    return ({'total': title.group(1),
             'rows': frozenset((name, *values) for name, values in rows.items())}, None)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--repo', type=Path, default=Path('.'), help='the repository (default: .)')
    parser.add_argument('--base', default='origin/main', help='the base ref (default: origin/main)')
    parser.add_argument('--head', default='HEAD', help='the head ref (default: HEAD)')
    parser.add_argument('--json', action='store_true', help='print the counts as JSON')
    args = parser.parse_args(argv)
    try:
        totals = counts(numstat(args.repo, args.base, args.head))
    except subprocess.CalledProcessError as error:
        print(f'pr-lines: git diff {args.base}...{args.head} failed: '
              f'{error.stderr.decode("utf-8", "replace").strip()}', file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(totals, sort_keys=True))
    else:
        print(block(totals), end='')
    return 0


if __name__ == '__main__':
    sys.exit(main())
