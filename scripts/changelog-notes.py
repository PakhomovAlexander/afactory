#!/usr/bin/env python3
"""The change notes of the next release: every fragment under changelog.d/ (one per pull request,
oldest first by the commit that added it) and anything still written under [Unreleased] in
CHANGELOG.md.

    scripts/changelog-notes.py                 print the notes
    scripts/changelog-notes.py --write SECTION put SECTION and the notes under [Unreleased] in
                                               CHANGELOG.md and remove the fragments
    scripts/changelog-notes.py --check         fail when [Unreleased] holds anything but the
                                               pointer to changelog.d/ (run by `make check`)

Pull requests never edit CHANGELOG.md: each adds its own changelog.d/<name>.md, so two open pull
requests never conflict on the changelog. Only the release pull request writes it.
"""
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
        return
    if len(sys.argv) != 3 or sys.argv[1] != "--write":
        sys.exit(__doc__)
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
