#!/usr/bin/env python3
"""Count the lines a pull request adds, per directory and crate, and print the block its description
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

A second view covers additions under crates/<crate>/. Rust files (.rs, case-sensitive) under
any tests directory within the crate, or named tests.rs, test_*.rs or *_test.rs, count as Tests.
Remaining Rust files under the crate's src/ count as Main code; everything else is Other,
including non-Rust fixtures in tests/, Cargo.toml, build.rs and examples. Inline #[cfg(test)]
modules in an ordinary src file remain Main code: this metric uses paths, not a Rust parser.
The directory overview keeps its separate tests-directory rule, including non-Rust files.
Each crate addition appears once in its split; this second view is not added to the total.
The crate section is omitted when no crate has additions. Crates sort by descending
total then name; shares use the whole PR total, with the same figures as the overview.
Names are HTML numeric-escaped for Markdown metacharacters, whitespace and HTML syntax.
--json prints af/PrLines@2 (schemas/pr-lines-v2.json): exact directories and crate splits;
the former unversioned directory-only JSON map is no longer written.

The block is a title with the total, `**Lines added: 18.9k**`, and a table with one row per
directory, the largest first: the directory, its figure, and its share of the total as a bar
and a percentage (`| crates | 7.5k | ████████████ 40% |`). A figure is exact below 1000, to a
tenth of a thousand up to 10k (`5k`, `7.9k`) and to a thousand from there (`23k`); the bar is
the row's count against the largest row's, in up to twelve cells of eighth blocks, a sliver at
least; a pull request that adds no line says `**Lines added: none**`. The `PR report` check
recomputes the block from the pull request's own commits and, when the description's block is
missing or differs, writes the block into the description itself; it refuses the description,
printing the block to paste, only when it cannot: two blocks, a marker without its pair, or a
refused write (ADR-0270).
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
CRATE_HEADER = ['Crate', 'Main code', 'Tests', 'Other', 'Total', 'Share']
CRATE_SEPARATOR_ROW = '|:--|--:|--:|--:|--:|--:|'
SCHEMA = 'af/PrLines@2'
CRATE_TITLE = '**Crate additions**'
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


def crate_counts(entries):
    """Disjoint path-based main/test/other additions under crates/<crate>/.

    Test Rust paths take priority over src; inline test modules are main code.
    Non-Rust files are always other, including fixtures inside tests directories.
    """
    crates = {}
    for added, _, path in entries:
        parts = path.split('/')
        if not added or len(parts) < 3 or parts[0] != 'crates':
            continue
        relative, name = parts[2:], parts[-1]
        kind = 'other'
        if name.endswith('.rs'):
            if 'tests' in relative[:-1] or name == 'tests.rs' \
                    or name.startswith('test_') or name.endswith('_test.rs'):
                kind = 'tests'
            elif relative[0] == 'src':
                kind = 'main'
        split = crates.setdefault(parts[1], {'main': 0, 'tests': 0, 'other': 0})
        split[kind] += added
    return crates


def report(entries):
    """Versioned exact counts; the crate table is a second view, not more additions."""
    return {'schema': SCHEMA, 'directories': counts(entries), 'crates': crate_counts(entries)}


def render_entries(entries):
    data = report(entries)
    return render(data['directories'], data['crates'])


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
    return ''.join(f'&#{ord(char)};' if char in '\\|&<>`*_[]~' or char.isspace()
                   else char for char in name)


def render(totals, crates=None):
    """The overview and optional crate table; both views share one PR total."""
    if not totals:
        return f'**Lines added: {NONE}**'
    ordered = sorted(totals.items(), key=lambda item: (-item[1], item[0]))
    total, largest = sum(totals.values()), ordered[0][1]
    lines = [f'**Lines added: {figure(total)}**', '',
             '| ' + ' | '.join(HEADER) + ' |', SEPARATOR_ROW]
    lines.extend(f'| {escape(name)} | {figure(n)} | {bar(n, largest)} {share(n, total)} |'
                 for name, n in ordered)
    if crates:
        lines.extend(['', CRATE_TITLE, '', '| ' + ' | '.join(CRATE_HEADER) + ' |',
                      CRATE_SEPARATOR_ROW])
        ordered_crates = sorted(crates.items(), key=lambda item: (-sum(item[1].values()), item[0]))
        for name, split in ordered_crates:
            n = sum(split.values())
            cells = [escape(name), *(figure(split[kind]) for kind in ('main', 'tests', 'other')),
                     figure(n), share(n, total)]
            lines.append('| ' + ' | '.join(cells) + ' |')
    return '\n'.join(lines)


def block(totals, crates=None):
    return f'{BEGIN}\n{render(totals, crates)}\n{END}\n'


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
    `({'total': figure, 'rows': directory rows, 'crates': crate rows}, None)`, or
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
        return {'total': NONE, 'rows': frozenset(), 'crates': frozenset()}, None
    header = '| ' + ' | '.join(HEADER) + ' |'
    if len(lines) < 4:
        return None, (f'the title is not followed by the table: the header `{header}`, its '
                      f'separator `{SEPARATOR_ROW}` and one row per directory')
    if row_cells(lines[1]) != HEADER:
        return None, f'the line after the title is not the table header `{header}`: `{lines[1]}`'
    separator = row_cells(lines[2])
    if separator is None or len(separator) != 3 or not all(SEPARATOR.match(c) for c in separator):
        return None, f'the header is not followed by its separator `{SEPARATOR_ROW}`: `{lines[2]}`'
    crate_at = next((n for n in range(3, len(lines))
                     if lines[n] == CRATE_TITLE), len(lines))
    crate_rows = {}
    if crate_at < len(lines):
        if crate_at + 1 >= len(lines) or row_cells(lines[crate_at + 1]) != CRATE_HEADER:
            return None, 'the crate title is not followed by its table header'
        tail = lines[crate_at + 2:]
        separator = row_cells(tail[0]) if tail else None
        if separator is None or len(separator) != 6 \
                or not all(SEPARATOR.match(c) for c in separator):
            return None, 'the crate header is not followed by its separator'
        if len(tail) < 2:
            return None, 'the crate table has no rows'
        for line in tail[1:]:
            cells = row_cells(line)
            if cells is None or len(cells) != 6 or not cells[0] \
                    or not all(FIGURE.match(c) for c in cells[1:5]) \
                    or not re.fullmatch(r'<1%|\d{1,3}%', cells[5]):
                return None, f'not a crate row `| crate | main | tests | other | total | share |`: `{line}`'
            if cells[0] in crate_rows:
                return None, f'`{cells[0]}` has two crate rows'
            crate_rows[cells[0]] = tuple(cells[1:])
    if crate_at == 3:
        return None, 'the directory table has no rows'
    rows = {}
    for line in lines[3:crate_at]:
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
             'rows': frozenset((name, *values) for name, values in rows.items()),
             'crates': frozenset((name, *values) for name, values in crate_rows.items())}, None)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--repo', type=Path, default=Path('.'), help='the repository (default: .)')
    parser.add_argument('--base', default='origin/main', help='the base ref (default: origin/main)')
    parser.add_argument('--head', default='HEAD', help='the head ref (default: HEAD)')
    parser.add_argument('--json', action='store_true', help='print the counts as JSON')
    args = parser.parse_args(argv)
    try:
        data = report(numstat(args.repo, args.base, args.head))
    except subprocess.CalledProcessError as error:
        print(f'pr-lines: git diff {args.base}...{args.head} failed: '
              f'{error.stderr.decode("utf-8", "replace").strip()}', file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(data, sort_keys=True))
    else:
        print(block(data['directories'], data['crates']), end='')
    return 0


if __name__ == '__main__':
    sys.exit(main())
