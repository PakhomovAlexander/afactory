#!/usr/bin/env python3
"""This repository's own `.af/af.lock` pins the newest af release (ADR-0138).

    scripts/af-pin.py --check                   fail when the pin is not the newest release in
                                                CHANGELOG.md (or the one before it, while that
                                                release is in flight), or lacks the digest of a
                                                released target (run by `make check`)
    scripts/af-pin.py --check-next VERSION [--from RC]
                                                check the prospective release before planning
                                                and require the RC pin when promoting
    scripts/af-pin.py --only-pin-moved REV VER  fail unless .af/af.lock differs from REV only in
                                                its [af] tables and now pins VER, with every
                                                target's digest (run by scripts/pin-release.sh)

The release pull request cannot pin its own version: the lock binds archive digests copied from
the signed SHA256SUMS, which exist only once the release is published. The release workflow pins
it through a CI-reviewed PR after publishing (scripts/pin-release.sh --pr); until a human
merges that PR the previous release is accepted. Publication alone does not move the pin.

A release that was tagged but never published (its workflow failed after the tag) keeps its
section, marked `[YANKED]` after the date as Keep a Changelog does; it has no signed digests to
pin, so the check skips it and the next release is judged against the one before it.
"""
import re
import subprocess
import sys
from pathlib import Path

LOCK = Path(".af/af.lock")
CHANGELOG = Path("CHANGELOG.md")
WORKFLOW = Path(".github/workflows/release.yml")
# Use the same vendored grammar for value skipping and full-document validation.
# No host TOML package (or installed af binary) participates in pin authority.
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / "performance"))
from perflab._vendor import tomli
from perflab._vendor.tomli import _parser as grammar

PIN_TABLES = {("af",), ("af", "digests")}
FIX = "fix: prepare a separate reviewed published-release pin PR with scripts/pin-release.sh VERSION; merge it after CI before planning the next release"


def table_spans(text):
    """Validated table spans in original text offsets, including header indentation.

    Skip entire TOML values, never interpreting header-like text inside strings or
    arrays. The vendored parser normalizes CRLF; map only statement line starts back
    to the original text so the caller can retain every non-pin byte.
    """
    tomli.loads(text)
    src = text.replace("\r\n", "\n")
    line_offsets = {}
    original = normalized = 0
    for line in text.split("\n"):
        line_offsets[normalized] = original
        original += len(line) + 1
        normalized += len(line.removesuffix("\r")) + 1
    headers = [((), 0)]
    pos = 0
    while pos < len(src):
        line_start = pos
        pos = grammar.skip_chars(src, pos, grammar.TOML_WS)
        if pos == len(src):
            break
        char = src[pos]
        if char == "\n":
            pos += 1
            continue
        if char == "[":
            array = src.startswith("[[", pos)
            pos = grammar.skip_chars(src, pos + (2 if array else 1), grammar.TOML_WS)
            pos, name = grammar.parse_key(src, pos)
            if array and name in PIN_TABLES:
                raise ValueError("array pin table refused")
            headers.append((name, line_offsets[line_start]))
            pos += 2 if array else 1
        elif char in grammar.KEY_INITIAL_CHARS:
            pos, _, _ = grammar.parse_key_value_pair(src, pos, float, nest_lvl=0)
        pos = grammar.skip_chars(src, pos, grammar.TOML_WS)
        pos = grammar.skip_comment(src, pos)
        if pos < len(src) and src[pos] != "\n":
            raise ValueError("unsafe TOML statement boundary")
        pos += 1
    return [(name, start, headers[index + 1][1] if index + 1 < len(headers) else len(text))
            for index, (name, start) in enumerate(headers)]


def non_pin_content(text):
    """Keep section order, exact non-pin text, and markers for the two editable spans."""
    return [(name, "" if name in PIN_TABLES else text[start:end])
            for name, start, end in table_spans(text)]


def validate_pin_edit(before, after, version, digests):
    """Shared semantic and byte invariants for preparation and the standalone guard."""
    old = tomli.loads(before)
    new = tomli.loads(after)
    if new.get("af") != {"version": version, "digests": digests}:
        raise ValueError("prepared pin differs from signed release")
    if ({key: value for key, value in old.items() if key != "af"}
            != {key: value for key, value in new.items() if key != "af"}
            or non_pin_content(before) != non_pin_content(after)):
        raise ValueError("prepared lock changed non-pin content")
    # Unknown af fields cannot be silently discarded even within a pin table.
    if set(old.get("af", {})) != {"version", "digests"}:
        raise ValueError("unsupported af pin shape")


def pin(text):
    """The pinned af version and its digest per target, using actual TOML values."""
    af = tomli.loads(text).get("af", {})
    digests = af.get("digests", {})
    return af.get("version"), {
        target: value.removeprefix("sha256:") for target, value in digests.items()
        if isinstance(value, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", value)
    }


def releases():
    """The released versions CHANGELOG.md has sections for, newest first; a `[YANKED]` section
    was never published and is skipped."""
    sections = re.findall(r"^## \[(\d+\.\d+\.\d+(?:-rc\.\d+)?)\](.*)$", CHANGELOG.read_text(), re.M)
    return [version for version, rest in sections if "[YANKED]" not in rest]


def targets():
    """Every target the release workflow builds, and so every digest a pin must carry."""
    return re.findall(r"^\s*- target: (\S+)\s*$", WORKFLOW.read_text(), re.M)


def missing_digests(digests):
    missing = [target for target in targets() if target not in digests]
    if missing:
        return [f"{LOCK} has no digest for {', '.join(missing)}"]
    return []


def check(next_version=None, previous=None):
    version, digests = pin(LOCK.read_text())
    recent = (([next_version] if next_version else []) + releases())[:2]
    if not recent:
        print(f"{CHANGELOG} has no release section", file=sys.stderr)
        return 1
    problems = []
    if previous is not None:
        if recent[1:2] != [previous]:
            problems.append(f"{CHANGELOG} must list {previous} as the newest non-yanked release")
        if version != previous:
            problems.append(f"{LOCK} must pin {previous} before promoting it")
    elif version not in recent:
        allowed = recent[0] + (f" (or {recent[1]} while {recent[0]} awaits its pin PR merge)" if len(recent) > 1 else "")
        problems.append(f"{LOCK} pins af {version}; it must pin the newest release, {allowed}")
    problems += missing_digests(digests)
    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        print(FIX, file=sys.stderr)
        return 1
    note = "" if version == recent[0] else f"; {recent[0]} awaits its pin PR merge after publication"
    print(f"{LOCK} pins af {version}{note}")
    return 0


def only_pin_moved(rev, version):
    before = subprocess.run(
        ["git", "show", f"{rev}:{LOCK}"], capture_output=True, check=True
    ).stdout.decode("utf-8")
    after = LOCK.read_bytes().decode("utf-8")

    problems = []
    pinned, digests = pin(after)
    try:
        validate_pin_edit(before, after, version,
                          {target: 'sha256:' + digest for target, digest in digests.items()})
    except ValueError:
        problems.append(f"the refresh changed {LOCK} beyond its [af] tables or has an invalid pin; review that in a pull request")
    if pinned != version:
        problems.append(f"{LOCK} pins af {pinned} after the refresh, not {version}")
    problems += missing_digests(digests)
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


def main(argv):
    if argv == ["--check"]:
        return check()
    base = r"[0-9]+\.[0-9]+\.[0-9]+"
    rc = r"-rc\.[1-9][0-9]*"
    if argv[:1] == ["--check-next"]:
        if len(argv) == 2 and re.fullmatch(base + "(?:" + rc + ")?", argv[1]):
            return check(argv[1])
        if (len(argv) == 4 and argv[2] == "--from" and re.fullmatch(base, argv[1])
                and re.fullmatch(re.escape(argv[1]) + rc, argv[3])):
            return check(argv[1], argv[3])
    if len(argv) == 3 and argv[0] == "--only-pin-moved":
        return only_pin_moved(argv[1], argv[2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (ValueError, KeyError, TypeError, AttributeError):
        print("af-pin: invalid or unsafe TOML lock; " + FIX, file=sys.stderr)
        sys.exit(1)
