#!/usr/bin/env python3
"""Count BASE...HEAD additions and print one hierarchical PR additions table (ADR-0195).

Top-level rows partition additions by actual first path component; root files keep their names.
crates/ contains crate directories with nonzero main/, test/, other/ leaves, and direct files.
Rust test paths take priority: any tests directory within the crate, tests.rs, test_*.rs or
*_test.rs. Remaining Rust under src/ is main; everything else is other. Inline cfg(test) in
ordinary source remains main. There is no shared classification.

Git numstat uses the merge base, rename destinations, and zero additions for binary files.
Siblings sort by descending count then raw name. Shares use the immediate parent (PR total at
the top); bars use the largest sibling, at most twelve cells with eighth blocks and a sliver
at least. Figures are exact below 1000, rounded to 0.1k below 10k and 1k above it, half up.
Only nonzero nodes appear. Parent rows repeat subtotals, never increase the PR total.

The first cell prefixes a child with ↳ and a grandchild with ↳ ↳. Directory names end in /;
direct files do not. Names escape Markdown/HTML, whitespace and the structural arrow as numeric
HTML entities. --json writes af/PrLines@3 (schemas/pr-lines-v3.json): exact total and nested rows.
The unchanged af-pr-lines:v1 markers delimit the refresh container. The checker regenerates the
whole block from the PR's exact commits; shape parsing never adds rounded figures.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

BEGIN = '<!-- af-pr-lines:v1 -->'
END = '<!-- /af-pr-lines -->'
NONE = 'none'
SCHEMA = 'af/PrLines@3'
TREE = '↳ '
KINDS = ('main/', 'test/', 'other/')
HEADER = ['Directory', 'Added', 'Share']
SEPARATOR_ROW = '|:--|--:|:--|'
BAR_CELLS = 12
BLOCKS = '▏▎▍▌▋▊▉█'
FIGURE = re.compile(r'^(?:[1-9][0-9]{0,2}|0|[1-9][0-9]{0,2}(?:\.[0-9])?k)$')
TITLE = re.compile(r'^\*\*Lines added: (none|0|[1-9][0-9]{0,2}|[1-9][0-9]{0,2}(?:\.[0-9])?k)\*\*$')
SEPARATOR = re.compile(r'^:?-+:?$')
SHARE = re.compile(r'^([▏▎▍▌▋▊▉█]+) (<1%|[0-9]{1,3}%)$')
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


def code_kind(relative):
    """Disjoint path metric within a crate; test Rust paths take priority over src."""
    name = relative[-1]
    if name.endswith('.rs'):
        if 'tests' in relative[:-1] or name == 'tests.rs' \
                or name.startswith('test_') or name.endswith('_test.rs'):
            return 'test/'
        if relative[0] == 'src':
            return 'main/'
    return 'other/'


def report(entries):
    """One exact tree. Every addition belongs to one top-level row and one terminal row."""
    roots = {}
    for added, _, path in entries:
        if not added:
            continue
        parts = path.split('/')
        names = [parts[0] + ('/' if len(parts) > 1 else '')]
        if parts[0] == 'crates' and len(parts) > 1:
            names.append(parts[1] + ('/' if len(parts) > 2 else ''))
            if len(parts) > 2:
                names.append(code_kind(parts[2:]))
        children = roots
        for name in names:
            node = children.setdefault(name, {'name': name, 'added': 0, 'children': {}})
            node['added'] += added
            children = node['children']

    def ordered(nodes):
        return [{'name': node['name'], 'added': node['added'],
                 'children': ordered(node['children'])}
                for node in sorted(nodes.values(), key=lambda node: (-node['added'], node['name']))]

    rows = ordered(roots)
    return {'schema': SCHEMA, 'total': sum(node['added'] for node in rows), 'rows': rows}


def render_entries(entries):
    return render(report(entries))


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
    return ''.join(f'&#{ord(char)};' if char in '\\|&<>`*_[]~↳' or char.isspace()
                   else char for char in name)


def render(data):
    """One table, with parent-relative shares and bars scaled against siblings."""
    if not data['rows']:
        return f'**Lines added: {NONE}**'
    lines = [f"**Lines added: {figure(data['total'])}**", '',
             '| ' + ' | '.join(HEADER) + ' |', SEPARATOR_ROW]

    def append(rows, parent, depth):
        largest = max(row['added'] for row in rows)
        for row in rows:
            n = row['added']
            lines.append(f"| {TREE * depth}{escape(row['name'])} | {figure(n)} | "
                         f"{bar(n, largest)} {share(n, parent)} |")
            if row['children']:
                append(row['children'], n, depth + 1)

    append(data['rows'], data['total'], 0)
    return '\n'.join(lines)


def block(data):
    return f'{BEGIN}\n{render(data)}\n{END}\n'


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
    `({'total': figure, 'rows': hierarchy rows}, None)`, or
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
    rows, stack, branches, parents = {}, [], set(), set()
    for line in lines[3:]:
        cells = row_cells(line)
        if cells is None or len(cells) != 3:
            return None, f'not a row of three cells `| directory | figure | bar share |`: `{line}`'
        name, shown, rest = cells
        shared = SHARE.fullmatch(rest)
        if not name or not FIGURE.fullmatch(shown) or shown == '0' or not shared:
            return None, f'not a row `| directory | figure | bar share |`: `{line}`'
        depth = 0
        while name.startswith(TREE):
            depth += 1
            name = name[len(TREE):]
        if not name.rstrip('/') or '↳' in name or '/' in name.rstrip('/') or name.endswith('//'):
            return None, f'invalid hierarchy name: `{line}`'
        if depth > 2 or depth > len(stack):
            return None, f'missing hierarchy parent: `{line}`'
        stack = stack[:depth]
        if depth and (stack[0] != 'crates/' or
                      (depth == 2 and (not stack[1].endswith('/') or name not in KINDS))):
            return None, f'invalid hierarchy: `{line}`'
        path = tuple(stack + [name])
        if path in rows:
            return None, f'`{name}` has two rows in the same hierarchy parent'
        if depth:
            parents.add(tuple(stack))
        if (name == 'crates/' and depth == 0) or (depth == 1 and name.endswith('/')):
            branches.add(path)
        rows[path] = (shown, shared.group(1), shared.group(2))
        stack.append(name)
    if branches - parents:
        return None, 'hierarchy branch has no children'
    return {'total': title.group(1),
            'rows': frozenset((path, *values) for path, values in rows.items())}, None


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
        print(block(data), end='')
    return 0


if __name__ == '__main__':
    sys.exit(main())
