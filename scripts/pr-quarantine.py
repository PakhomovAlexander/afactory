#!/usr/bin/env python3
"""Print the block a pull request description carries for the quarantined tests it adds
(ADR-0268, amended 2026-10-30).

    pr-quarantine.py [--repo DIR] [--base REF] [--head REF]   the block for BASE...HEAD

`.config/test-quarantine.json` is read with `git show` at the fork point of HEAD from BASE (the
commit `BASE...HEAD` diffs from, as `scripts/pr-lines.py` counts; `origin/main` and `HEAD` by
default) and at HEAD. A file absent at either commit counts as empty. An identity in HEAD's
`tests` that the fork point's lacks is added; an entry changed or removed is not. A pull request
that adds none needs no block: the script prints nothing to stdout, says so on stderr and exits
0. Otherwise it prints the block: the title `**Quarantined tests added: N**` and a table with
one row per added identity, sorted: the identity as code, its `state`, its `expires`, and why,
built from the entry as its state, `flaky in K Tasks' gates:` (`1 Task's` for one) with the
evidence Task IDs as recorded, then `; #<issue>` when the entry names one (`quarantined; flaky in 2 Tasks' gates:
id1, id2; #466`). A `|` in a cell is escaped. A file that cannot be read or is malformed at
either commit is an error naming the commit, with exit 1. The `PR report` check renders the
block again from the pull request's commits and refuses a description whose block is missing or
differs, printing the block to paste, and one that holds a block when nothing is added.

This script imports nothing from the other scripts: the check loads it from the base branch.
"""
import argparse
import datetime as dt
import json
from pathlib import Path
import re
import subprocess
import sys

BEGIN = '<!-- af-pr-quarantine:v1 -->'
END = '<!-- /af-pr-quarantine -->'
FILE = '.config/test-quarantine.json'
SCHEMA = 'af.test-quarantine/1'
STATES = ('quarantined', 'flaky')
# The file's static shape, as scripts/test-classes.py guards it (ADR-0268), without its
# expiry-against-today guard: a past commit's entries may have expired since.
FIELDS = {'state', 'since', 'expires', 'evidence', 'issue'}
IDENTITY = re.compile(r'[A-Za-z0-9_:./-]+ [A-Za-z0-9_:]+')
TASK_ID = re.compile(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}')
QUARANTINE_DAYS = 14
HEADER = ['Test', 'State', 'Expires', 'Why']
SEPARATOR_ROW = '|---|---|---|---|'
TITLE = re.compile(r'^\*\*Quarantined tests added: ([1-9]\d*)\*\*$')
SEPARATOR = re.compile(r'^:?-+:?$')
DATE = re.compile(r'^\d{4}-\d{2}-\d{2}$')
WHY = re.compile(r"^(quarantined|flaky); flaky in ([1-9]\d*) (?:Task's|Tasks') gates: (.+?)"
                 r"(?:; #([1-9]\d*))?$")
NONE_NEEDED = 'no quarantined tests added: no block needed'
HOW = ('Run `python3 scripts/pr-quarantine.py` from this repository on the pull request\'s branch '
       '(with its base fetched; `--base REF` for a base other than origin/main) and paste its '
       'output, both markers included, into the template\'s `Test quarantine` section, or delete '
       'that section when the script says no block is needed; after a push, run it again.')


class QuarantineError(Exception):
    """The quarantine file at a commit cannot be read, or is malformed."""


def git(repo, *args):
    return subprocess.run(['git', '-C', str(repo), *args], capture_output=True, check=True).stdout


def fork_point(repo, base, head):
    """The commit `BASE...HEAD` diffs from: the merge base of the two."""
    try:
        return git(repo, 'merge-base', base, head).decode().strip()
    except subprocess.CalledProcessError as error:
        why = error.stderr.decode('utf-8', 'replace').strip() or 'they have no common ancestor'
        raise QuarantineError(f'git merge-base {base} {head} failed in {repo}: {why}') from None


def entries(repo, commit, name):
    """The `tests` of the quarantine file at `commit` (`name` says which commit in an error),
    {} when the file is absent there."""
    where = f'{FILE} at {name}'
    try:
        if not git(repo, 'ls-tree', commit, '--', FILE).strip():
            return {}
        text = git(repo, 'show', f'{commit}:{FILE}').decode('utf-8')
    except subprocess.CalledProcessError as error:
        raise QuarantineError(f'cannot read {where}: '
                              f'{error.stderr.decode("utf-8", "replace").strip()}') from None
    except UnicodeDecodeError:
        raise QuarantineError(f'{where} is not UTF-8') from None
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        raise QuarantineError(f'{where} is not JSON: {error}') from None
    if not isinstance(document, dict) or document.get('schema') != SCHEMA \
            or not isinstance(document.get('tests'), dict):
        raise QuarantineError(f'{where} is not an `{SCHEMA}` document with a `tests` object')
    for identity, entry in document['tests'].items():
        problem = malformed(identity, entry)
        if problem:
            raise QuarantineError(f'{where}: the entry `{identity}` is malformed: {problem}')
    return document['tests']


def date(text):
    """`text` as a date when it is `YYYY-MM-DD`, else None."""
    if not isinstance(text, str) or not DATE.match(text):
        return None
    try:
        return dt.date.fromisoformat(text)
    except ValueError:
        return None


def malformed(identity, entry):
    """What breaks the quarantine file's shape in this entry, or None."""
    if not IDENTITY.fullmatch(identity):
        return 'its key is not a `<binary_id> <test name>` nextest identity'
    if not isinstance(entry, dict):
        return 'it is not an object'
    if set(entry) - FIELDS:
        return f'unknown field {sorted(set(entry) - FIELDS)}'
    if {'state', 'since', 'expires', 'evidence'} - set(entry):
        return f"missing field {sorted({'state', 'since', 'expires', 'evidence'} - set(entry))}"
    if entry['state'] not in STATES:
        return f'state {entry["state"]!r} is neither {" nor ".join(STATES)}'
    since, expires = date(entry['since']), date(entry['expires'])
    if since is None or expires is None:
        return '`since` and `expires` are not both YYYY-MM-DD dates'
    # A difference, not `since + 14 days`, which overflows near 9999-12-31.
    if not dt.timedelta(0) < expires - since <= dt.timedelta(days=QUARANTINE_DAYS):
        return f'`expires` is not after `since` and at most {QUARANTINE_DAYS} days after it'
    evidence = entry['evidence']
    if not isinstance(evidence, list) or not evidence \
            or not all(isinstance(task, str) and TASK_ID.fullmatch(task) for task in evidence):
        return '`evidence` is not a non-empty list of af Task IDs'
    if 'issue' in entry and (type(entry['issue']) is not int or entry['issue'] < 1):
        return f'issue {entry["issue"]!r} is not a positive integer'
    return None


def added(base, head, repo):
    """{identity: entry} for each identity the head's file adds since the fork point."""
    fork = fork_point(repo, base, head)
    before = entries(repo, fork, f'the fork point {fork} of {base}...{head}')
    after = entries(repo, head, f'{head}')
    return {identity: entry for identity, entry in after.items() if identity not in before}


def escape(text):
    return text.replace('|', '\\|')


def why(entry):
    """The Why cell's text for `entry`, unescaped."""
    evidence = entry['evidence']
    tasks = "Task's" if len(evidence) == 1 else "Tasks'"
    text = f"{entry['state']}; flaky in {len(evidence)} {tasks} gates: {', '.join(evidence)}"
    return text + (f"; #{entry['issue']}" if 'issue' in entry else '')


def render(base, head, repo):
    """The block's body for the quarantined tests `BASE...HEAD` adds in `repo`, or None when it
    adds none. Raises QuarantineError when the file cannot be read at either commit."""
    new = added(base, head, repo)
    if not new:
        return None
    lines = [f'**Quarantined tests added: {len(new)}**', '',
             '| ' + ' | '.join(HEADER) + ' |', SEPARATOR_ROW]
    lines.extend(f"| `{escape(identity)}` | {escape(entry['state'])} | "
                 f"{escape(entry['expires'])} | {escape(why(entry))} |"
                 for identity, entry in sorted(new.items()))
    return '\n'.join(lines)


def block(body):
    return f'{BEGIN}\n{body}\n{END}\n'


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
    `({'count': N, 'rows': tuple of (identity, state, expires, why) in order}, None)`, or
    `(None, what is wrong)`."""
    lines = [line.strip() for line in lines if line.strip()]
    if not lines:
        return None, 'the block is empty'
    title = TITLE.match(lines[0])
    if not title:
        return None, f'the first line is not the title `**Quarantined tests added: N**`: `{lines[0]}`'
    header = '| ' + ' | '.join(HEADER) + ' |'
    if len(lines) < 4:
        return None, (f'the title is not followed by the table: the header `{header}`, its '
                      f'separator `{SEPARATOR_ROW}` and one row per added test')
    if row_cells(lines[1]) != HEADER:
        return None, f'the line after the title is not the table header `{header}`: `{lines[1]}`'
    separator = row_cells(lines[2])
    if separator is None or len(separator) != 4 or not all(SEPARATOR.match(c) for c in separator):
        return None, f'the header is not followed by its separator `{SEPARATOR_ROW}`: `{lines[2]}`'
    rows = {}
    for line in lines[3:]:
        cells = row_cells(line)
        if cells is None or len(cells) != 4:
            return None, f'not a row of four cells `| test | state | expires | why |`: `{line}`'
        test, state, expires, reason = cells
        shown = WHY.match(reason)
        if not (len(test) > 2 and test[0] == test[-1] == '`' and test.strip('`')) \
                or state not in STATES or not DATE.match(expires) or not shown \
                or shown.group(1) != state \
                or int(shown.group(2)) != len(shown.group(3).split(', ')):
            return None, ('not a row `| `identity` | state | YYYY-MM-DD | why |` (why as '
                          '`<state>; flaky in K Tasks\' gates: <K Task IDs>` and `; #<issue>` '
                          f'when one is set): `{line}`')
        identity = test[1:-1]
        if identity in rows:
            return None, f'`{identity}` has two rows'
        rows[identity] = (state, expires, reason)
    if len(rows) != int(title.group(1)):
        return None, (f'the title says {title.group(1)} quarantined tests added, but the table '
                      f'has {len(rows)} rows')
    return ({'count': len(rows),
             'rows': tuple((name, *values) for name, values in rows.items())}, None)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--repo', type=Path, default=Path('.'), help='the repository (default: .)')
    parser.add_argument('--base', default='origin/main', help='the base ref (default: origin/main)')
    parser.add_argument('--head', default='HEAD', help='the head ref (default: HEAD)')
    args = parser.parse_args(argv)
    try:
        body = render(args.base, args.head, args.repo)
    except QuarantineError as error:
        print(f'pr-quarantine: {error}', file=sys.stderr)
        return 1
    if body is None:
        print(NONE_NEEDED, file=sys.stderr)
    else:
        print(block(body), end='')
    return 0


if __name__ == '__main__':
    sys.exit(main())
