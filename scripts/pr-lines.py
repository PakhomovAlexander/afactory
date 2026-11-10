#!/usr/bin/env python3
"""Count BASE...HEAD additions and deletions in one hierarchical PR table (ADR-0195).

Root rows partition both counts by actual first path component. crates/ includes crate
main/, tests/, other/ leaves and direct files. Rust test paths take priority over src;
inline cfg(test) and common/shared source remain main. Binary and 0/0 files are absent.
Siblings sort by descending additions, descending deletions, then raw identity. Shares use
immediate parent additions; zero child is 0%, zero denominator is undefined (—).
Figures round half up: exact below 1000, 0.1k below 10k, 1k above. Percentages round half
up, positive values below 0.5% show <1%. Subtotals never add to the PR totals again.
Separate +/− bars use up to twelve cells, sharing the largest sibling addition or deletion
count; children have their own sibling scale. Eighths round half up, positives get a sliver,
zeros are blank. One table uses bold roots/parents and four &nbsp; entities per depth,
with safe literal names.
--json emits only af/PrLines@4 (schemas/pr-lines-v4.json), with exact added/deleted counts.
The unchanged af-pr-lines:v1 markers delimit canonical refresh from exact PR commits.
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
SCHEMA = 'af/PrLines@4'
INDENT = '&nbsp;' * 4
KINDS = ('main', 'tests', 'other')
HEADER = ['Area', 'Changes', '+', '−', 'Share of additions']
SEPARATOR_ROW = '|:--|--:|:--|:--|:--|'
BAR = re.compile(r'^(?:█{1,12}|█{0,11}[▏▎▍▌▋▊▉])?$')
FIGURE = re.compile(r'^(?:[1-9][0-9]{0,2}|0|[1-9][0-9]*(?:\.[0-9])?k)$')
TITLE = re.compile(r'^\*\*Lines added: (none|0|[1-9][0-9]{0,2}|[1-9][0-9]*(?:\.[0-9])?k)\*\*$')
SEPARATOR = re.compile(r'^:?-+:?$')
SHARE = re.compile(r'^(?:—|<1%|0%|[1-9][0-9]?%|100%)$')
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
            return 'tests/'
        if relative[0] == 'src':
            return 'main/'
    return 'other/'


def report(entries):
    """One exact tree. Every addition and deletion belongs to one top-level row and one terminal row."""
    roots = {}
    for added, deleted, path in entries:
        if added is None or deleted is None or not (added or deleted):
            continue
        parts = path.split('/')
        names = [parts[0] + ('/' if len(parts) > 1 else '')]
        if parts[0] == 'crates' and len(parts) > 1:
            names.append(parts[1] + ('/' if len(parts) > 2 else ''))
            if len(parts) > 2:
                names.append(code_kind(parts[2:]))
        children = roots
        for name in names:
            node = children.setdefault(name, {'name': name, 'added': 0, 'deleted': 0, 'children': {}})
            node['added'] += added
            node['deleted'] += deleted
            children = node['children']

    def ordered(nodes):
        return [{'name': node['name'], 'added': node['added'], 'deleted': node['deleted'],
                 'children': ordered(node['children'])}
                for node in sorted(nodes.values(), key=lambda node: (
                    -node['added'], -node['deleted'], node['name']))]

    rows = ordered(roots)
    return {'schema': SCHEMA, 'added': sum(node['added'] for node in rows),
            'deleted': sum(node['deleted'] for node in rows), 'rows': rows}


def render_entries(entries):
    return render(report(entries))


def figure(n):
    """A count as the block shows it: exact below 1000, to a tenth of a thousand below 10k, to
    a thousand from there; half rounds up."""
    if n < 1000:
        return str(n)
    if n < 10_000:
        tenths = (n + 50) // 100
        if tenths >= 100:
            return '10k'
        return f'{tenths // 10}k' if tenths % 10 == 0 else f'{tenths // 10}.{tenths % 10}k'
    return f'{(n + 500) // 1000}k'


def bar(n, maximum):
    """At most twelve cells on the common sibling scale; integer half-up eighths."""
    if not n:
        return ''
    eighths = max(1, (n * 96 * 2 + maximum) // (maximum * 2))
    full, fraction = divmod(eighths, 8)
    return '█' * full + ('▏▎▍▌▋▊▉'[fraction - 1] if fraction else '')


def share(n, total):
    """Parent-relative addition share, undefined for a zero denominator."""
    if not total:
        return '—'
    if not n:
        return '0%'
    percent = 100 * n / total
    return f'{int(percent + 0.5)}%' if percent >= 0.5 else '<1%'


def escape(name):
    # Quotes make literal leading NBSP/space visible as part of a name, never hierarchy depth.
    body = ''.join(f'&#{ord(char)};' if char in '\\|&<>`*_[]~↳"' or char.isspace()
                   else char for char in name)
    return f'&#34;{body}&#34;' if any(char.isspace() for char in name) else body


def safe_name(name):
    decoded = re.sub(r'&#([0-9]+);', lambda m: chr(int(m[1]))
                     if len(m[1]) <= 7 and int(m[1]) <= 0x10ffff else '', name)
    if any(char.isspace() for char in decoded):
        if not (decoded.startswith('"') and decoded.endswith('"')):
            return False
        decoded = decoded[1:-1]
    return bool(name) and '/' not in name and escape(decoded) == name


def display_names(rows):
    """Project sibling identities without slashes; alias only colliding files.

    Reserve every literal bare name before allocating (file), (file2), ... in raw identity
    order, so neither row ordering nor literal suffixes can change or impersonate an alias.
    Escaping is applied afterwards, equally to literals and aliases.
    """
    names = {row['name']: row['name'].removesuffix('/') for row in rows}
    reserved = set(names.values())
    directories = {row['name'] for row in rows if row['name'].endswith('/')}
    for name in sorted(names):
        if name + '/' not in directories:
            continue
        suffix = 1
        while True:
            label = f'{name} (file{suffix if suffix > 1 else ""})'
            if label not in reserved:
                break
            suffix += 1
        names[name] = label
        reserved.add(label)
    return names


def render(data):
    """One table with bold parents, visible indentation and parent-relative shares."""
    if not data['rows']:
        return f'**Lines added: {NONE}**'
    lines = [f"**Lines added: {figure(data['added'])}**", '',
             '| ' + ' | '.join(HEADER) + ' |', SEPARATOR_ROW]

    def append(rows, parent, depth):
        names = display_names(rows)
        maximum = max(max(row['added'], row['deleted']) for row in rows)
        for row in rows:
            n = row['added']
            cells = [escape(names[row['name']]),
                     f"+{figure(n)}/−{figure(row['deleted'])}",
                     bar(n, maximum), bar(row['deleted'], maximum), share(n, parent)]
            if depth == 0 or row['children']:
                cells = [f'**{cell}**' if cell else '' for cell in cells]
            cells[0] = INDENT * depth + cells[0]
            lines.append('| ' + ' | '.join(cells) + ' |')
            if row['children']:
                append(row['children'], n, depth + 1)

    append(data['rows'], data['added'], 0)
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
    if separator is None or len(separator) != 5 or not all(SEPARATOR.match(c) for c in separator):
        return None, f'the header is not followed by its separator `{SEPARATOR_ROW}`: `{lines[2]}`'
    rows, stack, branches, parents = {}, [], set(), set()
    for line in lines[3:]:
        cells = row_cells(line)
        if cells is None or len(cells) != 5:
            return None, f'not a row of five cells: `{line}`'
        name, changes, added_bar, deleted_bar, rest = cells
        depth = 0
        while name.startswith(INDENT):
            depth += 1
            name = name[len(INDENT):]
        bold = name.startswith('**') and name.endswith('**')
        if bold:
            name = name[2:-2]
            if not all(cell.startswith('**') and cell.endswith('**') for cell in (changes, rest)):
                return None, 'inconsistent bold row'
            for cell in (added_bar, deleted_bar):
                if cell and (not cell[2:-2] or not (cell.startswith('**') and cell.endswith('**'))):
                    return None, 'inconsistent bold bar'
            changes, rest = changes[2:-2], rest[2:-2]
            added_bar = added_bar[2:-2] if added_bar else ''
            deleted_bar = deleted_bar[2:-2] if deleted_bar else ''
        if depth > 2 or depth > len(stack):
            return None, 'missing hierarchy parent'
        stack = stack[:depth]
        # Only canonical numeric entities may encode literal syntax/whitespace. In particular
        # a literal &nbsp; in a filename is escaped as &#38;nbsp;, never indentation.
        if not safe_name(name):
            return None, 'invalid hierarchy name'
        counts = re.fullmatch(r'\+(.+)/−(.+)', changes)
        if not counts or not all(FIGURE.fullmatch(n) for n in counts.groups()) \
                or counts.groups() == ('0', '0') or not SHARE.fullmatch(rest):
            return None, 'invalid changes or share'
        if not all(BAR.fullmatch(cell) and (count == '0') == (cell == '')
                   for cell, count in zip((added_bar, deleted_bar), counts.groups())):
            return None, 'invalid addition or deletion bar'
        parent = stack[-1] if stack else None
        if (depth == 0 and not bold) or (depth == 2 and bold) \
                or (depth and (stack[0][0] != 'crates' or
                               (depth == 2 and (not parent[1] or name not in KINDS)))):
            return None, 'invalid hierarchy or bold row'
        denominator = parent[2] if parent else title.group(1)
        if (denominator == '0' and rest != '—') or (denominator != '0' and
                (rest == '—' or (counts[1] == '0') != (rest == '0%'))):
            return None, 'invalid addition share denominator'
        path = tuple(row[0] for row in stack) + (name,)
        if path in rows:
            return None, f'`{name}` has two rows in the same hierarchy parent'
        if depth:
            parents.add(path[:-1])
        if depth == 1 and bold:
            branches.add(path)
        rows[path] = (changes, added_bar, deleted_bar, rest, bold)
        stack.append((name, bold, counts[1]))
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
