#!/usr/bin/env python3
"""This repository's own `.af/af.lock` pins the newest af release (ADR-0138).

    scripts/af-pin.py --check                   fail when the pin is not the newest release in
                                                CHANGELOG.md (or the one before it, while that
                                                release is in flight), or lacks the digest of a
                                                released target (run by `make check`)
    scripts/af-pin.py --only-pin-moved REV VER  fail unless .af/af.lock differs from REV only in
                                                its [af] tables and now pins VER, with every
                                                target's digest (run by scripts/pin-release.sh)

The release pull request cannot pin its own version: the lock binds archive digests copied from
the signed SHA256SUMS, which exist only once the release is published. The release workflow pins
it right after publishing (scripts/pin-release.sh --push); until then the previous release is the
newest one that can be pinned, so the check accepts it.
"""
import re
import subprocess
import sys
from pathlib import Path

LOCK = Path(".af/af.lock")
CHANGELOG = Path("CHANGELOG.md")
WORKFLOW = Path(".github/workflows/release.yml")
PIN_TABLES = {"af", "af.digests"}
FIX = "fix: scripts/pin-release.sh VERSION (pins a published release with `af onboard --refresh-lock`)"


def tables(text):
    """The lock's lines by table header ("" before the first), so tables compare by content."""
    found = {}
    name = ""
    for line in text.splitlines():
        header = re.fullmatch(r"\[\[?([^\[\]]+)\]\]?\s*", line)
        if header:
            name = header.group(1).strip()
        found.setdefault(name, []).append(line)
    return found


def pin(text):
    """The pinned af version and its digest per target."""
    by_table = tables(text)
    version = None
    for line in by_table.get("af", []):
        match = re.fullmatch(r'version\s*=\s*"([^"]+)"\s*', line)
        if match:
            version = match.group(1)
    digests = {}
    for line in by_table.get("af.digests", []):
        match = re.fullmatch(r'"?([A-Za-z0-9_.-]+)"?\s*=\s*"sha256:([0-9a-f]{64})"\s*', line)
        if match:
            digests[match.group(1)] = match.group(2)
    return version, digests


def releases():
    """The released versions CHANGELOG.md has sections for, newest first."""
    return re.findall(r"^## \[(\d+\.\d+\.\d+(?:-rc\.\d+)?)\]", CHANGELOG.read_text(), re.M)


def targets():
    """Every target the release workflow builds, and so every digest a pin must carry."""
    return re.findall(r"^\s*- target: (\S+)\s*$", WORKFLOW.read_text(), re.M)


def missing_digests(digests):
    missing = [target for target in targets() if target not in digests]
    if missing:
        return [f"{LOCK} has no digest for {', '.join(missing)}"]
    return []


def check():
    version, digests = pin(LOCK.read_text())
    recent = releases()[:2]
    if not recent:
        print(f"{CHANGELOG} has no release section", file=sys.stderr)
        return 1
    problems = []
    if version not in recent:
        allowed = recent[0] + (f" (or {recent[1]} while {recent[0]} is released)" if len(recent) > 1 else "")
        problems.append(f"{LOCK} pins af {version}; it must pin the newest release, {allowed}")
    problems += missing_digests(digests)
    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        print(FIX, file=sys.stderr)
        return 1
    note = "" if version == recent[0] else f"; the release workflow pins {recent[0]} once it is published"
    print(f"{LOCK} pins af {version}{note}")
    return 0


def only_pin_moved(rev, version):
    before = subprocess.run(
        ["git", "show", f"{rev}:{LOCK}"], capture_output=True, text=True, check=True
    ).stdout
    after = LOCK.read_text()

    def rest(text):
        return {name: lines for name, lines in tables(text).items() if name not in PIN_TABLES}

    problems = []
    if rest(before) != rest(after):
        problems.append(f"the refresh changed {LOCK} beyond its [af] tables; review that in a pull request")
    pinned, digests = pin(after)
    if pinned != version:
        problems.append(f"{LOCK} pins af {pinned} after the refresh, not {version}")
    problems += missing_digests(digests)
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


def main(argv):
    if argv == ["--check"]:
        return check()
    if len(argv) == 3 and argv[0] == "--only-pin-moved":
        return only_pin_moved(argv[1], argv[2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
