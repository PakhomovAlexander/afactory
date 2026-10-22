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
and a binary file adds nothing. A count is shown as it is below 1000, to a tenth of a thousand
up to 10k (`5k`, `7.9k`) and to a thousand from there (`23k`), each after its directory
(`docs/10k`), the largest first; a pull request that adds no line says `none`. The `PR report`
check recomputes the figures from the pull request's own commits and refuses a description
whose block is missing or differs.
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
LEAD = 'Lines added: '
NONE = 'none'
TESTS = 'tests'
FIGURE = re.compile(r'^(?:[1-9]\d{0,2}|0|[1-9]\d{0,2}(?:\.\d)?k)$')
HOW = ('Run `python3 scripts/pr-lines.py` from this repository on the pull request\'s branch '
       '(with its base fetched; `--base REF` for a base other than origin/main) and paste its '
       'output, both markers included, into the template\'s `Lines added` section; run it again '
       'after a push that changes the figures.')


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


def render(totals):
    """The block's one line for `totals` (directory → lines added)."""
    if not totals:
        return LEAD + NONE
    ordered = sorted(totals.items(), key=lambda item: (-item[1], item[0]))
    return LEAD + ' '.join(f'{name}/{figure(n)}' for name, n in ordered)


def block(totals):
    return f'{BEGIN}\n{render(totals)}\n{END}\n'


def parse(line):
    """The line's figures as directory → figure, `{}` for `none`; None when the line is not
    of the block's form."""
    text = line.strip()
    if not text.startswith(LEAD):
        return None
    rest = text[len(LEAD):].strip()
    if rest == NONE:
        return {}
    figures = {}
    for token in rest.split():
        name, slash, shown = token.rpartition('/')
        if not slash or not name or not FIGURE.match(shown) or name in figures:
            return None
        figures[name] = shown
    return figures or None


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
