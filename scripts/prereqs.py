#!/usr/bin/env python3
"""What a contributor's machine needs before `make check` (issue #161): one line per tool,
`ok` with what was found, or `missing` with what was found (an old version, nothing on PATH)
and the command that installs it.

It changes nothing: every probe is a version query or a read, never an install. Exit 0 when
every check passes, 1 when one does not. `make prereqs` runs it.
"""
import argparse
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
GIB = 1 << 30
MIN_GIT = (2, 30)
MIN_FREE_GIB = 30


def run(argv, env):
    """The stdout and stderr of `argv`, or None when it cannot start or fails; at most 20 s."""
    try:
        done = subprocess.run(argv, capture_output=True, text=True, env=env, timeout=20,
                              stdin=subprocess.DEVNULL)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if done.returncode != 0:
        return None
    return (done.stdout + done.stderr).strip()


def first_line(text):
    return text.splitlines()[0].strip() if text else ""


def macos():
    return platform.system() == "Darwin"


def toolchain(path):
    """The channel and components `rust-toolchain.toml` pins."""
    text = path.read_text(encoding="utf-8")
    channel = re.search(r'^\s*channel\s*=\s*"([^"]+)"', text, re.M)
    components = re.search(r'^\s*components\s*=\s*\[([^\]]*)\]', text, re.M)
    names = re.findall(r'"([^"]+)"', components.group(1)) if components else []
    return (channel.group(1) if channel else None), names


def free_volume(target):
    """The directory whose volume a build fills: `target`, or its nearest existing ancestor."""
    path = Path(target).resolve()
    while not path.exists() and path != path.parent:
        path = path.parent
    return path


def checks(env, toolchain_file, target, min_free_gib):
    """Every check, in order: (name, passed, what was found or why not, how to fix it)."""
    which = lambda tool: shutil.which(tool, path=env.get("PATH", ""))
    results = []

    version = ".".join(str(part) for part in sys.version_info[:3])
    results.append(("python3", sys.version_info >= (3, 9), f"python3 {version}",
                    "install Python 3.9 or later (python.org, or your package manager)"))

    found = (tool := which("git")) and run([tool, "--version"], env)
    numbers = re.search(r"(\d+)\.(\d+)", found or "")
    git = tuple(int(part) for part in numbers.groups()) if numbers else None
    results.append((
        "git", bool(git) and git >= MIN_GIT,
        first_line(found) if found else "not on PATH",
        "install git 2.30 or later" + (" (xcode-select --install, or brew install git)"
                                       if macos() else " (apt install git, dnf install git)")))

    channel, components = toolchain(toolchain_file)
    listed = (tool := which("rustup")) and run([tool, "toolchain", "list"], env)
    pinned = bool(listed and channel and any(
        line.split()[0].startswith(channel) for line in listed.splitlines() if line.strip()))
    install = f"rustup toolchain install {channel}"
    if components:
        install += " --component " + " --component ".join(components)
    results.append((
        "rust", pinned,
        f"rustup holds the pinned {channel}" if pinned else
        (f"rustup holds no {channel} ({toolchain_file.name})" if listed else "rustup not on PATH"),
        install if listed else "install rustup (https://rustup.rs), then " + install))

    nextest = (tool := which("cargo-nextest")) and run([tool, "nextest", "--version"], env)
    results.append(("cargo-nextest", bool(nextest),
                    first_line(nextest) if nextest else "not on PATH",
                    "cargo install cargo-nextest --locked (CI: scripts/install-nextest.sh)"))

    cc = (tool := which("cc")) and run([tool, "--version"], env)
    results.append(("cc", bool(cc), first_line(cc) if cc else "not on PATH",
                    "xcode-select --install" if macos()
                    else "apt install build-essential (dnf install gcc)"))

    npx = (tool := which("npx")) and run([tool, "--version"], env)
    results.append(("npx", bool(npx), f"npx {first_line(npx)}" if npx else "not on PATH",
                    "install Node.js 18 or later, which brings npx (markdownlint-cli2 runs "
                    "through it)"))

    minisign = (tool := which("minisign")) and run([tool, "-v"], env)
    results.append(("minisign", bool(minisign),
                    first_line(minisign) if minisign else "not on PATH",
                    "brew install minisign" if macos() else "apt install minisign"))

    volume = free_volume(target)
    try:
        free = shutil.disk_usage(volume).free
        enough = free >= min_free_gib * GIB
        said = f"{free / GIB:.1f} GiB free on the volume of {volume}"
    except OSError as error:
        enough, said = False, f"{volume}: {error}"
    results.append(("disk", enough, said,
                    f"free {min_free_gib} GiB there: a built worktree takes 5-50 GB, so remove "
                    "the worktrees and target/ directories no task needs any more"))
    return results


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--toolchain-file", type=Path, default=ROOT / "rust-toolchain.toml")
    parser.add_argument("--target", type=Path,
                        default=Path(os.environ.get("CARGO_TARGET_DIR") or ROOT / "target"),
                        help="the build directory whose volume needs room (default: target/)")
    parser.add_argument("--min-free-gib", type=int, default=MIN_FREE_GIB)
    args = parser.parse_args(argv)
    results = checks(dict(os.environ), args.toolchain_file, args.target, args.min_free_gib)
    width = max(len(name) for name, *_ in results)
    for name, passed, found, fix in results:
        if passed:
            print(f"ok       {name:<{width}}  {found}")
        else:
            print(f"missing  {name:<{width}}  {found} — fix: {fix}")
    failed = [name for name, passed, *_ in results if not passed]
    if failed:
        print(f"{len(failed)} of {len(results)} prerequisites missing: {', '.join(failed)}")
        return 1
    print(f"all {len(results)} prerequisites present; next: make check")
    return 0


if __name__ == "__main__":
    sys.exit(main())
