#!/usr/bin/env python3
"""The change notes of the next release: every fragment under changelog.d/ (one per pull request,
oldest first by the commit that added it) and anything still written under [Unreleased] in
CHANGELOG.md.

    scripts/changelog-notes.py                 print the notes
    scripts/changelog-notes.py --write SECTION put SECTION and the notes under [Unreleased] in
                                               CHANGELOG.md and remove the fragments
    scripts/changelog-notes.py --check         fail when [Unreleased] holds anything but the
                                               pointer to changelog.d/, or a fragment holds a
                                               Markdown heading (run by `make check`)

A fragment holds bullets: a heading copied into the release section would break its structure
(a second top-level heading fails markdownlint's MD025), so --check and --write refuse any
fragment line that starts a heading outside a fenced code block, before writing anything.

Pull requests never edit CHANGELOG.md: each adds its own changelog.d/<name>.md, so two open pull
requests never conflict on the changelog. Only the release pull request writes it.
"""
import re
import subprocess
import sys
from pathlib import Path

CHANGELOG = Path("CHANGELOG.md")
FRAGMENTS = Path("changelog.d")
MARKER = "## [Unreleased]\n"
POINTER = (
    "Changes since the last release are notes under [`changelog.d/`](changelog.d/), one file per\n"
    "pull request; the release pull request collects them here."
)
# An ATX heading (up to three spaces of indent, one to six '#', then a blank or the line's end)
# and a code fence's opening run of backticks or tildes, also at most three spaces in: a line
# indented further is indented code, so it neither opens nor closes a fence. A '#' line inside a
# list item's deeper fence is indented four or more spaces and is no heading either way.
HEADING = re.compile(r" {0,3}#{1,6}(?:[ \t]|$)")
FENCE = re.compile(r" {0,3}(`{3,}|~{3,})")


def fragments():
    """The fragment files, oldest first by the commit that added each, then by name."""
    found = []
    for path in sorted(FRAGMENTS.glob("*.md")):
        if path.name == "README.md":
            continue
        added = subprocess.run(
            ["git", "log", "--diff-filter=A", "--format=%ct", "--", str(path)],
            capture_output=True,
            text=True,
            check=False,
        ).stdout.split()
        # Not committed yet: after everything that is.
        found.append((int(added[-1]) if added else 1 << 62, path.name, path))
    return [path for _, _, path in sorted(found)]


def headings():
    """Every fragment line that starts a Markdown heading outside a fenced code block."""
    found = []
    for path in sorted(FRAGMENTS.glob("*.md")):
        if path.name == "README.md":
            continue
        fence = None
        for number, line in enumerate(path.read_text().splitlines(), 1):
            opened = FENCE.match(line)
            if fence is None:
                if opened:
                    fence = opened.group(1)
                elif HEADING.match(line):
                    found.append(f"{path}:{number}: a heading in a change note: {line.strip()!r}")
            elif (
                opened
                and opened.group(1)[0] == fence[0]
                and len(opened.group(1)) >= len(fence)
                and not line[opened.end() :].strip()
            ):
                fence = None
    return found


def refuse_headings():
    found = headings()
    if found:
        sys.exit(
            "\n".join(found)
            + "\nA fragment holds bullets; see changelog.d/README.md. Fold the heading's text into "
            "a bullet."
        )


def split(text):
    """CHANGELOG.md as (before [Unreleased], what [Unreleased] holds, the released sections)."""
    if MARKER not in text:
        sys.exit("CHANGELOG.md has no [Unreleased] section")
    head, rest = text.split(MARKER, 1)
    at = rest.find("\n## [")
    if at < 0:
        return head, rest, ""
    return head, rest[: at + 1], rest[at + 1 :]


def notes(body):
    """The notes: what [Unreleased] still holds (without the pointer), then every fragment."""
    parts = []
    legacy = body.replace(POINTER, "").strip()
    if legacy:
        parts.append(legacy)
    for path in fragments():
        text = path.read_text().strip()
        if text:
            parts.append(text)
    return "\n\n".join(parts)


def main():
    head, body, released = split(CHANGELOG.read_text())
    if len(sys.argv) == 1:
        print(notes(body))
        return
    if sys.argv[1:] == ["--check"]:
        if body.replace(POINTER, "").strip():
            sys.exit(
                "CHANGELOG.md: [Unreleased] must hold only the pointer to changelog.d/. Put the "
                "entry in a new changelog.d/<topic>.md instead (see changelog.d/README.md)."
            )
        refuse_headings()
        return
    if len(sys.argv) != 3 or sys.argv[1] != "--write":
        sys.exit(__doc__)
    refuse_headings()
    section = sys.argv[2].rstrip("\n")
    collected = notes(body)
    if collected:
        section += "\n\n" + collected
    CHANGELOG.write_text(
        head + MARKER + "\n" + POINTER + "\n\n" + section + "\n\n" + released.lstrip("\n")
    )
    for path in fragments():
        subprocess.run(["git", "rm", "-q", "--", str(path)], check=True)


if __name__ == "__main__":
    main()
