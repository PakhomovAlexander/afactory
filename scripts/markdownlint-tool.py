#!/usr/bin/env python3
"""The markdownlint gate's tool, installed once and run offline (ADR-0145).

`install` is the one step that may use the network: it fetches exactly the tarballs that
scripts/markdownlint/package-lock.json pins from registry.npmjs.org (or reads them from a local
directory with `--from`), checks each against the lock's sha512, extracts regular files only,
checks the installed tree against scripts/markdownlint/tree.sha256 and places it, read-only, at
`<prefix>/markdownlint-cli2-<version>-<tree>`. It never runs npm, a lifecycle script or the
operator's npm configuration or cache.

`run ARGS...` is the gate's entry point. It never touches the network or HOME: it finds the
installed tree through a `<root>/bin` entry on PATH, refuses it unless every directory and file
is the one the lock and the tree digest name, owned by this user, read-only and free of links,
and then replaces itself with `node <root>/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs
ARGS...` in the same working directory, so the configuration, the glob and the exit status are
markdownlint-cli2's own. A missing or refused tool exits 2 before markdownlint-cli2 starts; there
is no fallback to npx.
"""
import base64
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import sys
import tempfile

HERE = Path(__file__).resolve().parent / "markdownlint"
PACKAGE = "markdownlint-cli2"
VERSION = "0.22.1"
BIN = f"node_modules/{PACKAGE}/markdownlint-cli2-bin.mjs"
MANIFEST = "af-tool.json"
SCHEMA = "af.markdownlint-tool/1"
TREE_DOMAIN = b"af.markdownlint-tool.tree/1\n"
REGISTRY = "https://registry.npmjs.org/"
DIR_MODE, FILE_MODE, EXEC_MODE = 0o555, 0o444, 0o555
LAUNCHER_PATH = "bin/markdownlint-cli2"
LAUNCHER = (
    "#!/bin/sh\n"
    "# markdownlint-cli2 as scripts/markdownlint-tool.py installed it. The gate runs it through\n"
    "# `python3 scripts/markdownlint-tool.py run`, which verifies this tree first.\n"
    'here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P) || exit 2\n'
    f'exec node "$here/../{BIN}" "$@"\n'
).encode()
# Bounds on the one networked step and on what a tarball may unpack to.
FETCH_TIMEOUT_S = 30
MAX_TARBALL_BYTES = 16 * 1024 * 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_ENTRIES = 20000
# Lock entry fields the closure may carry; anything else (links, install scripts, optional or
# platform-specific packages, bundles) is refused rather than interpreted.
ENTRY_FIELDS = {"version", "resolved", "integrity", "license", "dependencies", "engines", "bin",
                "funding", "peerDependencies", "peerDependenciesMeta"}
NAME = r"(?:@[a-z0-9][a-z0-9._~-]*/)?[a-z0-9][a-z0-9._~-]*"
KEY = re.compile(rf"node_modules/{NAME}(?:/node_modules/{NAME})*")
EXACT_VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?")


class Refused(Exception):
    """The tool or its pin is not exactly what the repository names."""


def require(condition, message):
    if not condition:
        raise Refused(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def read_pin(here=HERE):
    """The committed package.json, lock and tree digest, checked against each other."""
    try:
        package_bytes = (here / "package.json").read_bytes()
        lock_bytes = (here / "package-lock.json").read_bytes()
        tree_text = (here / "tree.sha256").read_text()
    except OSError as error:
        raise Refused(f"the committed pin is unreadable: {error}") from error
    package = json.loads(package_bytes, object_pairs_hook=unique_object)
    lock = json.loads(lock_bytes, object_pairs_hook=unique_object)
    tree = tree_text.strip()
    require(isinstance(package, dict) and isinstance(lock, dict), "the pin is not two JSON objects")
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", tree), "tree.sha256 is not one sha256 digest")
    require(package.get("dependencies") == {PACKAGE: VERSION},
            f"package.json must depend on exactly {PACKAGE} {VERSION}")
    packages = closure(lock)
    require(packages[""].get("dependencies") == package["dependencies"],
            "the lock's root dependencies differ from package.json")
    require(packages.get(f"node_modules/{PACKAGE}", {}).get("version") == VERSION,
            f"the lock does not pin {PACKAGE} {VERSION}")
    return {"lock": packages, "lock_sha256": "sha256:" + sha256(lock_bytes), "tree": tree}


def package_name(key):
    return key.rsplit("node_modules/", 1)[1]


def closure(lock):
    """The lock's packages, refused unless they are exactly the transitive closure of the root,
    each an exact registry tarball with a sha512 integrity."""
    require(lock.get("lockfileVersion") == 3, "the lock must be lockfileVersion 3")
    packages = lock.get("packages")
    require(isinstance(packages, dict) and "" in packages, "the lock has no root package")
    for key, entry in packages.items():
        if key == "":
            continue
        require(KEY.fullmatch(key), f"lock key {key!r} is not a node_modules path")
        require(isinstance(entry, dict) and set(entry) <= ENTRY_FIELDS,
                f"lock entry {key} carries unsupported fields {sorted(set(entry) - ENTRY_FIELDS)}")
        name, version = package_name(key), entry.get("version")
        require(isinstance(version, str) and EXACT_VERSION.fullmatch(version),
                f"lock entry {key} has no exact version")
        tarball = f"{name.rsplit('/', 1)[-1]}-{version}.tgz"
        require(entry.get("resolved") == f"{REGISTRY}{name}/-/{tarball}",
                f"lock entry {key} does not resolve to its registry tarball")
        integrity = entry.get("integrity")
        require(isinstance(integrity, str) and re.fullmatch(r"sha512-[A-Za-z0-9+/]{86}==",
                                                            integrity),
                f"lock entry {key} has no single sha512 integrity")
    reached, pending = {""}, [""]
    while pending:
        key = pending.pop()
        entry = packages[key]
        optional_peers = {name for name, meta in entry.get("peerDependenciesMeta", {}).items()
                          if meta.get("optional")}
        wanted = list(entry.get("dependencies", {}))
        wanted += [name for name in entry.get("peerDependencies", {}) if name not in optional_peers]
        for name in wanted:
            found = resolve(packages, key, name)
            require(found is not None, f"{key or 'the root'} needs {name}, which the lock lacks")
            if found not in reached:
                reached.add(found)
                pending.append(found)
    extra = sorted(set(packages) - reached)
    require(not extra, f"the lock holds packages nothing depends on: {extra}")
    return packages


def resolve(packages, key, name):
    """Node's lookup from the package at `key`: its own node_modules, then each enclosing one."""
    base = key
    while True:
        candidate = f"{base}/node_modules/{name}" if base else f"node_modules/{name}"
        if candidate in packages:
            return candidate
        if not base:
            return None
        base = base.rsplit("/node_modules/", 1)[0] if "/node_modules/" in base else ""


# -- the installed tree -----------------------------------------------------------------------


def root_name(tree):
    return f"{PACKAGE}-{VERSION}-{tree.split(':', 1)[1][:16]}"


def tree_digest(entries):
    """One digest over every directory and file below the root except the manifest."""
    lines = []
    for path, kind, digest in sorted(entries):
        lines.append(f"d {path}\n" if kind == "dir" else f"f {path} {digest}\n")
    return "sha256:" + sha256(TREE_DOMAIN + "".join(lines).encode())


def expected_mode(path, kind):
    if kind == "dir":
        return DIR_MODE
    return EXEC_MODE if path == LAUNCHER_PATH else FILE_MODE


def scan(root, uid):
    """Every entry below `root`, refused unless it is a directory or regular file owned by `uid`
    with exactly the mode the installer gives it. Never follows a link."""
    entries, pending = [], [("", root)]
    while pending:
        relative, directory = pending.pop()
        with os.scandir(directory) as listing:
            children = sorted(listing, key=lambda child: child.name)
        for child in children:
            path = f"{relative}/{child.name}" if relative else child.name
            if path == MANIFEST:
                continue
            info = child.stat(follow_symlinks=False)
            require(info.st_uid == uid, f"{path} is not owned by this user")
            if stat.S_ISDIR(info.st_mode):
                kind = "dir"
            elif stat.S_ISREG(info.st_mode):
                kind = "file"
            else:
                raise Refused(f"{path} is neither a directory nor a regular file")
            require(stat.S_IMODE(info.st_mode) == expected_mode(path, kind),
                    f"{path} has mode {stat.S_IMODE(info.st_mode):o}, not "
                    f"{expected_mode(path, kind):o}")
            if kind == "dir":
                entries.append((path, "dir", None))
                pending.append((path, child.path))
            else:
                require(info.st_nlink == 1, f"{path} has another hard link")
                entries.append((path, "file", sha256(read_regular(child.path))))
            require(len(entries) <= MAX_ENTRIES, "the tool tree has too many entries")
    return entries


def read_regular(path):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(descriptor, "rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), f"{path} is not a regular file")
        return stream.read()


def check_ancestors(path, uid):
    """No one but this user or root may replace `path` or anything above it."""
    for directory in [path, *path.parents]:
        info = os.lstat(directory)
        require(stat.S_ISDIR(info.st_mode), f"{directory} is not a directory")
        require(info.st_uid in (uid, 0), f"{directory} is owned by another user")
        require(not info.st_mode & 0o022 or info.st_mode & stat.S_ISVTX,
                f"{directory} is writable by other users")


def verify(root, pin):
    """Refuse `root` unless it is exactly the pinned tree; return its markdownlint-cli2 entry."""
    uid = os.getuid()
    root = Path(os.path.realpath(root))
    check_ancestors(root, uid)
    info = os.lstat(root)
    require(info.st_uid == uid and stat.S_IMODE(info.st_mode) == DIR_MODE,
            f"{root} is not a read-only directory owned by this user")
    try:
        manifest_bytes = read_regular(root / MANIFEST)
    except OSError as error:
        raise Refused(f"{root} has no readable {MANIFEST}: {error}") from error
    manifest_info = os.lstat(root / MANIFEST)
    require(manifest_info.st_uid == uid and stat.S_IMODE(manifest_info.st_mode) == FILE_MODE,
            f"{MANIFEST} is not read-only and owned by this user")
    manifest = json.loads(manifest_bytes, object_pairs_hook=unique_object)
    require(isinstance(manifest, dict) and isinstance(manifest.get("files"), dict)
            and manifest == build_manifest(pin, manifest["files"]),
            f"{MANIFEST} names another pin than the repository's")
    entries = scan(root, uid)
    actual = tree_digest(entries)
    if actual != pin["tree"]:
        recorded = manifest["files"]
        found = {path: digest for path, kind, digest in entries if kind == "file"}
        changed = sorted(path for path in set(recorded) | set(found)
                         if recorded.get(path, "absent") != found.get(path, "absent"))
        raise Refused(f"the tool tree is {actual}, not {pin['tree']}; changed: {changed[:5]}")
    require(read_regular(root / LAUNCHER_PATH) == LAUNCHER, "the launcher is not the pinned one")
    return root / BIN


def build_manifest(pin, files):
    return {"schema": SCHEMA, "package": PACKAGE, "version": VERSION, "bin": BIN,
            "lock_sha256": pin["lock_sha256"], "tree_sha256": pin["tree"], "files": files}


def discover(pin, path_value):
    """The first `<root>/bin` on PATH whose root is the pinned tree. A root that names this pin
    and fails verification is refused, not skipped; roots of other pins are skipped."""
    name, seen = root_name(pin["tree"]), []
    for entry in path_value.split(os.pathsep):
        directory = Path(entry)
        if not directory.is_absolute() or directory.name != "bin":
            continue
        root = directory.parent
        if not os.path.lexists(root / MANIFEST):
            continue
        seen.append(str(root))
        if root.name == name or Path(os.path.realpath(root)).name == name:
            return verify(root, pin)
    hint = f" (other pins on PATH: {', '.join(seen)})" if seen else ""
    raise Refused(f"no {name}/bin on PATH{hint}; run `python3 scripts/markdownlint-tool.py "
                  "install` and add the bin directory it prints to PATH")


def run(arguments, environ=os.environ):
    pin = read_pin()
    path_value = environ.get("PATH", "")
    entry = discover(pin, path_value)
    node = shutil.which("node", path=path_value)
    require(node is not None, "node is not on PATH")
    # Only the inherited check environment reaches node, without preload or module-path hooks.
    environment = {key: value for key, value in environ.items()
                   if not key.startswith(("NODE_", "npm_", "NPM_"))}
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    os.execve(node, [node, str(entry), *arguments], environment)


# -- installation ------------------------------------------------------------------------------


def fetch(url, source, remaining):
    import urllib.request  # install only: `run` stays free of network modules and their startup
    require(url.startswith(REGISTRY), f"{url} is not on {REGISTRY}")
    if source is not None:
        # A local mirror keeps the registry's layout: <source>/<name>/-/<file>.tgz.
        with open(Path(source, *url[len(REGISTRY):].split("/")), "rb") as stream:
            data = stream.read(MAX_TARBALL_BYTES + 1)
    else:
        with urllib.request.urlopen(url, timeout=FETCH_TIMEOUT_S) as response:
            data = response.read(MAX_TARBALL_BYTES + 1)
    require(len(data) <= min(MAX_TARBALL_BYTES, remaining), f"{url} exceeds the download bound")
    return data


def extract(data, target, key):
    """Regular files and directories only, below `target`, with the tarball's top directory
    stripped as npm does. Duplicate, absolute, parent-relative and link entries are refused."""
    import tarfile
    try:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            written = unpack(archive, target, key)
    except tarfile.TarError as error:
        raise Refused(f"{key} is not a readable gzip tarball: {error}") from error
    require(written, f"{key} is empty")


def unpack(archive, target, key):
    written = set()
    for member in archive:
        parts = PurePosixPath(member.name).parts[1:]
        if not parts:
            continue
        relative = "/".join(parts)
        require(not PurePosixPath(member.name).is_absolute()
                and all(part not in ("", ".", "..") and "\\" not in part and "\0" not in part
                        for part in parts),
                f"{key} has an unsafe path {member.name!r}")
        destination = target.joinpath(*parts)
        if member.isdir():
            destination.mkdir(parents=True, exist_ok=True)
            continue
        require(member.isfile(), f"{key} has a link or special file {member.name!r}")
        require(relative not in written, f"{key} holds {member.name!r} twice")
        written.add(relative)
        destination.parent.mkdir(parents=True, exist_ok=True)
        stream = archive.extractfile(member)
        with open(destination, "xb") as output:
            output.write(stream.read())
    return written


def seal(root):
    """Give every entry the installer's mode, deepest first, and list what was sealed."""
    files = {}
    for directory, subdirectories, names in os.walk(root, topdown=False):
        base = Path(directory)
        for name in names:
            path = base / name
            relative = path.relative_to(root).as_posix()
            if relative != MANIFEST:
                files[relative] = sha256(read_regular(path))
            os.chmod(path, expected_mode(relative, "file"))
        for name in subdirectories:
            os.chmod(base / name, DIR_MODE)
    os.chmod(root, DIR_MODE)
    return dict(sorted(files.items()))


def remove(root):
    """Remove an installer-owned tree whose directories are read-only."""
    for directory, _, _ in os.walk(root):
        os.chmod(directory, 0o700)
    shutil.rmtree(root)


def default_prefix(environ):
    data = environ.get("XDG_DATA_HOME") or os.path.join(environ.get("HOME", ""), ".local/share")
    require(os.path.isabs(data), "set HOME or XDG_DATA_HOME, or pass --prefix")
    return Path(data) / "af-tools"


def install(prefix, source=None, replace=False):
    pin = read_pin()
    prefix.mkdir(mode=0o755, parents=True, exist_ok=True)
    prefix = Path(os.path.realpath(prefix))
    check_ancestors(prefix, os.getuid())
    require(os.lstat(prefix).st_uid == os.getuid(), f"{prefix} is not owned by this user")
    final = prefix / root_name(pin["tree"])
    if os.path.lexists(final):
        try:
            verify(final, pin)
            return final
        except (Refused, OSError, ValueError) as error:
            require(replace, f"{final} exists and is refused ({error}); "
                             "pass --replace to reinstall it")
            remove(final)
    staging = Path(tempfile.mkdtemp(prefix=".staging-", dir=prefix))
    try:
        remaining = MAX_TOTAL_BYTES
        for key, entry in sorted(pin["lock"].items()):
            if key == "":
                continue
            data = fetch(entry["resolved"], source, remaining)
            remaining -= len(data)
            algorithm, expected = entry["integrity"].split("-", 1)
            require(base64.b64encode(hashlib.new(algorithm, data).digest()).decode() == expected,
                    f"{key}: the tarball does not match the lock's integrity")
            extract(data, staging / key, key)
            package = json.loads((staging / key / "package.json").read_bytes())
            require(package.get("name") == package_name(key)
                    and package.get("version") == entry["version"],
                    f"{key}: the tarball holds {package.get('name')} {package.get('version')}")
        require((staging / BIN).is_file(), f"{BIN} is missing from the closure")
        (staging / "bin").mkdir()
        (staging / LAUNCHER_PATH).write_bytes(LAUNCHER)
        files = seal(staging)
        os.chmod(staging, 0o700)
        manifest = build_manifest(pin, files)
        (staging / MANIFEST).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        os.chmod(staging / MANIFEST, FILE_MODE)
        os.chmod(staging, DIR_MODE)
        actual = tree_digest(scan(staging, os.getuid()))
        require(actual == pin["tree"],
                f"the installed tree is {actual}; scripts/markdownlint/tree.sha256 pins "
                f"{pin['tree']}")
        try:
            os.rename(staging, final)
        except OSError:
            # A concurrent install placed the same tree first; keep it if it verifies.
            verify(final, pin)
            remove(staging)
            return final
        verify(final, pin)
        return final
    except BaseException:
        if staging.exists():
            remove(staging)
        raise


USAGE = """usage: markdownlint-tool.py install [--prefix DIR] [--from DIR] [--replace]
       markdownlint-tool.py verify [--prefix DIR]
       markdownlint-tool.py run [--] ARGS...
"""


def options(arguments, allowed):
    values = {}
    while arguments:
        flag = arguments.pop(0)
        require(flag in allowed, f"unknown option {flag!r}\n{USAGE}")
        if flag == "--replace":
            values[flag] = True
        else:
            require(arguments, f"{flag} needs a value")
            values[flag] = arguments.pop(0)
    return values


def main(argv):
    if not argv or argv[0] not in ("install", "verify", "run"):
        sys.stderr.write(USAGE)
        return 2
    command, arguments = argv[0], list(argv[1:])
    try:
        if command == "run":
            run(arguments)
        values = options(arguments, {"--prefix", "--from", "--replace"} if command == "install"
                         else {"--prefix"})
        prefix = Path(values.get("--prefix") or default_prefix(os.environ)).absolute()
        if command == "install":
            root = install(prefix, values.get("--from"), values.get("--replace", False))
        else:
            pin = read_pin()
            root = prefix / root_name(pin["tree"])
            verify(root, pin)
        print(f"{PACKAGE} {VERSION}: {root}\nput on PATH: {root / 'bin'}")
        return 0
    except (Refused, OSError, ValueError, EOFError) as error:
        sys.stderr.write(f"markdownlint-tool: refused: {error}\n")
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
