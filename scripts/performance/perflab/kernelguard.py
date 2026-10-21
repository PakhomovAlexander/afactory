"""The protected harness of guarded kernel experiments (p7, ADR-0185; design section 13).

A kernel experiment measures a candidate Snapshot with a command its captured code policy
declares (ADR-0132). The command here is the literal isolated Python bootstrap `BOOTSTRAP`,
captured in that policy with the exact protected-harness digest it expects as its first
argument. Before anything of the candidate is imported or built, it walks the measured
Snapshot, refuses every symbolic link, special file and undecodable name anywhere in it, and
hashes the exact protected closure:

- the laboratory driver `scripts/performance`, the registry, golden entries and every fixture
  of `fixtures/performance`, the native probe and its adapters, the supervisor example, and the
  default-off trace and calibration helpers of `review-core`;
- the complete build closure wherever it appears, additions included: every `Cargo.toml`,
  `Cargo.lock`, `build.rs`, `rust-toolchain` or `rust-toolchain.toml`, and every file below any
  `.cargo` directory.

A missing required file, an extra protected file (a Python bytecode file in the driver, a new
manifest or Cargo configuration), a changed byte or a digest other than the captured one
refuses the measurement with exit 2 and no report. Only then does it run the verified driver's
`measure` command. The only exclusion is the staged policy subtree
`fixtures/performance/kernel-measure`, which holds the expected digest itself; it may hold only
nonexecuted `.md`, `.toml` and `.json` data, never a build input.

`BOOTSTRAP` is the one definition of these rules: the staged policy embeds it verbatim, and the
digest and re-pin tooling (`digest`, `check_staged`, `render_staged`) execute the same text. The
expected digest is never read from the measured tree: a candidate cannot change the bootstrap or
the digest the captured authority holds. A protected harness does not make arbitrary product
code trustworthy; golden oracles and code review remain necessary.
"""
import json
import os
import subprocess

from .values import ContractError, require

STAGED_DIR = "fixtures/performance/kernel-measure"
STAGED_POLICY = STAGED_DIR + "/code-policy-measures.toml"
STAGED_INVENTORY = STAGED_DIR + "/inventory.json"
INVENTORY_SCHEMA = "af.perf-kernel-inventory/1"

# The literal bootstrap. It is executed by `python3 -I -S -B -c BOOTSTRAP DIGEST measure ...`
# from the measured Snapshot's root; its own imports are standard-library modules only, and the
# current directory is removed from the path before any of them.
BOOTSTRAP = r'''import sys
sys.path[:] = [entry for entry in sys.path if entry not in ("", ".")]
sys.dont_write_bytecode = True
import hashlib
import os
import stat
import unicodedata

SCHEMA = "af.perf-kernel-harness/1"
EXCLUDED = "fixtures/performance/kernel-measure"
STAGED_SUFFIXES = (".md", ".toml", ".json")
TREES = ("scripts/performance", "fixtures/performance",
         "crates/review-pipeline/examples/perf-probe",
         "crates/review-process/examples/perf-supervisor")
FILES = ("crates/review-pipeline/examples/perf-probe.rs",
         "crates/review-process/examples/perf-supervisor.rs",
         "crates/review-core/src/perf_calibration.rs",
         "crates/review-core/src/perf_trace.rs")
NAMES = ("Cargo.toml", "Cargo.lock", "build.rs", "rust-toolchain", "rust-toolchain.toml")
REQUIRED = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "scripts/performance/cli.py",
            "fixtures/performance/suite.toml", "fixtures/performance/golden/native.json",
            "crates/review-pipeline/Cargo.toml") + FILES
FILES_MAX = 8192
ENTRIES_MAX = 200000
BYTES_MAX = 64 * 1024 * 1024


class Refused(Exception):
    pass


def fold(path):
    """`path` caselessly: a case-insensitive filesystem opens BUILD.RS for build.rs and
    .CaRgO/config.toml for .cargo/config.toml, so classification never depends on case or on
    canonically equivalent spellings."""
    return unicodedata.normalize("NFD", unicodedata.normalize("NFD", path).casefold())


FOLDED_FILES = tuple(fold(path) for path in FILES)
FOLDED_NAMES = tuple(fold(name) for name in NAMES)
FOLDED_TREES = tuple(fold(tree) + "/" for tree in TREES)


def protected(path):
    folded = fold(path)
    parts = folded.split("/")
    return (folded in FOLDED_FILES or parts[-1] in FOLDED_NAMES or ".cargo" in parts[:-1]
            or folded.startswith(FOLDED_TREES))


def staged(path):
    return path == EXCLUDED or path.startswith(EXCLUDED + "/")


def name_of(raw, where):
    try:
        name = os.fsencode(raw).decode("utf-8")
    except UnicodeError:
        raise Refused("%s holds a name that is not UTF-8" % (where or "the root"))
    if not name or "\n" in name or "\\" in name or name in (".", ".."):
        raise Refused("%s holds the name %r" % (where or "the root", name))
    return name


def walk(root):
    """Every regular file below `root`, relative; a link or special file anywhere refuses."""
    out, pending, seen = [], [""], 0
    while pending:
        relative = pending.pop()
        with os.scandir(os.path.join(root, relative) if relative else root) as listing:
            for entry in listing:
                seen += 1
                if seen > ENTRIES_MAX:
                    raise Refused("the measured tree holds more than %d entries" % ENTRIES_MAX)
                name = name_of(entry.name, relative)
                path = relative + "/" + name if relative else name
                mode = entry.stat(follow_symlinks=False).st_mode
                if stat.S_ISLNK(mode):
                    raise Refused("%s is a symbolic link; a measured tree holds none" % path)
                if stat.S_ISDIR(mode):
                    pending.append(path)
                elif stat.S_ISREG(mode):
                    out.append(path)
                else:
                    raise Refused("%s is not a regular file or directory" % path)
    return out


def listed_files(root, paths):
    """The given relative files, each a regular file reached through no link."""
    out = []
    for path in paths:
        parts = path.split("/")
        for index in range(1, len(parts) + 1):
            current = os.path.join(root, *parts[:index])
            mode = os.lstat(current).st_mode
            if stat.S_ISLNK(mode):
                raise Refused("%s is a symbolic link; a measured tree holds none"
                              % "/".join(parts[:index]))
            if index < len(parts) and not stat.S_ISDIR(mode):
                raise Refused("%s is not a directory" % "/".join(parts[:index]))
            if index == len(parts) and not stat.S_ISREG(mode):
                raise Refused("%s is not a regular file" % path)
        out.append(path)
    return out


def file_digest(root, path):
    descriptor = os.open(os.path.join(root, *path.split("/")),
                         os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            raise Refused("%s is not a regular file" % path)
        if info.st_size > BYTES_MAX:
            raise Refused("%s holds %d bytes, over %d" % (path, info.st_size, BYTES_MAX))
        digest, size = hashlib.sha256(), 0
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            if size > BYTES_MAX:
                raise Refused("%s grew past %d bytes while it was read" % (path, BYTES_MAX))
            digest.update(chunk)
        return size, "sha256:" + digest.hexdigest()
    finally:
        os.close(descriptor)


def inventory(root, paths=None):
    """(digest, entries): the protected closure of `root`, each entry (path, size, sha256)."""
    files = walk(root) if paths is None else listed_files(root, paths)
    entries = []
    for path in sorted(files, key=lambda item: item.encode("utf-8")):
        if staged(path):
            parts = fold(path).split("/")
            if parts[-1] in FOLDED_NAMES or ".cargo" in parts[:-1] or "__pycache__" in parts \
                    or not path.endswith(STAGED_SUFFIXES):
                raise Refused("%s: the staged policy subtree holds only nonexecuted .md, .toml "
                              "and .json data" % path)
            continue
        if protected(path):
            entries.append((path,) + file_digest(root, path))
            if len(entries) > FILES_MAX:
                raise Refused("more than %d protected files" % FILES_MAX)
    present = set(entry[0] for entry in entries)
    missing = [path for path in REQUIRED if path not in present]
    if missing:
        raise Refused("required protected files are missing: %s" % ", ".join(missing[:8]))
    digest = hashlib.sha256((SCHEMA + "\n").encode("utf-8"))
    for path, size, sha in entries:
        digest.update(("%s\0%d\0%s\n" % (path, size, sha)).encode("utf-8"))
    return "sha256:" + digest.hexdigest(), entries


if __name__ == "__main__":
    root = os.getcwd()
    if len(sys.argv) < 3 or not sys.argv[1].startswith("sha256:"):
        sys.stderr.write("kernel-measure guard: expected DIGEST COMMAND...\n")
        sys.exit(2)
    try:
        found, entries = inventory(root)
    except (Refused, OSError) as error:
        sys.stderr.write("kernel-measure guard: refused before any candidate import or build: "
                         "%s\n" % error)
        sys.exit(2)
    if found != sys.argv[1]:
        sys.stderr.write("kernel-measure guard: refused before any candidate import or build: "
                         "the protected harness of %d files has digest %s, not the captured "
                         "%s\n" % (len(entries), found, sys.argv[1]))
        sys.exit(2)
    sys.stderr.write("kernel-measure guard: %d protected files match %s\n"
                     % (len(entries), found))
    import runpy
    driver = os.path.join(root, "scripts", "performance", "cli.py")
    sys.argv = [driver] + sys.argv[2:]
    runpy.run_path(driver, run_name="__main__")
'''

# The arguments every staged measure command starts with: isolated mode (no environment
# variables, no user site, no script directory on the path), no site module, no bytecode.
PYTHON = "python3"
PYTHON_FLAGS = ("-I", "-S", "-B", "-c")


def _rules():
    namespace = {"__name__": "perflab.kernelguard.bootstrap"}
    exec(compile(BOOTSTRAP, "<kernel-measure bootstrap>", "exec"), namespace)
    return namespace


RULES = _rules()


def tracked_paths(root):
    """The files of a Git checkout that a Snapshot of it would capture: tracked files and
    untracked ones that are not ignored, so an incidental ignored cache (a local `__pycache__`)
    is not part of the inventory while committed bytecode is. None outside a work tree."""
    if not os.path.exists(os.path.join(root, ".git")):
        return None
    result = subprocess.run(["git", "-C", root, "ls-files", "-z", "--cached", "--others",
                             "--exclude-standard"], stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=60,
                            env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                                 "GIT_CONFIG_NOSYSTEM": "1", "LC_ALL": "C"})
    require(result.returncode == 0, "git ls-files failed in %s: %s"
            % (root, result.stderr.decode("utf-8", "replace")[-1024:]))
    paths = set()
    for raw in result.stdout.split(b"\0"):
        if not raw:
            continue
        path = raw.decode("utf-8")
        # A deleted but still-indexed file is not in the tree a Snapshot would capture.
        if os.path.lexists(os.path.join(root, *path.split("/"))):
            paths.add(path)
    return sorted(paths)


def digest(root, paths=None):
    """(digest, entries) of `root` under exactly the bootstrap's rules; `paths`, when given, is
    the file list to judge (a checkout's `tracked_paths`), else the whole tree is walked."""
    try:
        return RULES["inventory"](root, paths)
    except (RULES["Refused"], OSError) as error:
        raise ContractError("the protected harness of %s is refused: %s" % (root, error))


def checkout_digest(root):
    return digest(root, tracked_paths(root))


def inventory_document(found, entries):
    return {"schema": INVENTORY_SCHEMA, "harness_digest": found,
            "files": [{"path": path, "size": size, "sha256": sha}
                      for path, size, sha in entries]}


def toml_basic(text):
    """`text` as one TOML basic string."""
    out = ['"']
    for char in text:
        if char == "\\":
            out.append("\\\\")
        elif char == '"':
            out.append('\\"')
        elif char == "\n":
            out.append("\\n")
        elif char == "\t":
            out.append("\\t")
        elif ord(char) < 0x20 or ord(char) == 0x7f:
            out.append("\\u%04x" % ord(char))
        else:
            out.append(char)
    out.append('"')
    return "".join(out)


def command_args(found, instance, metric):
    """The literal argument list of one staged measure."""
    return list(PYTHON_FLAGS) + [BOOTSTRAP, found, "measure", "--case", instance,
                                 "--metric", metric]


# The staged policy ---------------------------------------------------------------------------

# (measure, instance, metric): each staged measure runs one nonspawning native instance and
# reports its probe-region wall time. Three repetitions of at most 20 minutes, a cold first
# build included, fit the repository's 60-minute check_wall_ms.
STAGED_MEASURES = (
    ("perf_canonical_serialize_small", "canonical_serialize_small__package_small_envelope",
     "wall_ms"),
    ("perf_canonical_validate_small", "canonical_validate_small__package_small_envelope",
     "wall_ms"),
    ("perf_cas_raw_get_small", "cas_raw_get_small__package_small_envelope", "wall_ms"),
    ("perf_cas_typed_get_small", "cas_typed_get_small__package_small_envelope", "wall_ms"),
)
STAGED_REPETITIONS = 3
STAGED_WALL_MS = 1200000
STAGED_THRESHOLD = "0.1"
# Until the guard's kernel integration and tampering tests are accepted on a sealed source and
# a human installs this policy, objectives compare only the kernel's own elapsed time.
OBJECTIVE_METRIC = "elapsed_ms"
INNER_METRIC_STATUS = "pending"
INNER_METRIC_REASON = ("the inner wall_ms metric is recorded in every Measurement but no staged "
                       "objective selects it until the guard's real kernel integration and "
                       "tampered-candidate tests are accepted on the sealed source and a reviewed "
                       "human change installs and selects it")

HEADER = """\
# Generated by `python3 scripts/performance/cli.py harness-digest --write`; never edit it by hand.
# Guarded kernel experiments (ADR-0185): append these tables to `.af/code-policy.toml` in a
# reviewed human change, as README.md describes. Each measure runs the literal isolated bootstrap
# of scripts/performance/perflab/kernelguard.py, which verifies the protected harness of the
# measured Snapshot against the digest below before any candidate import or build, then supplies
# one judged probe-region sample per kernel repetition. `[warm]` must bind `cargo_target` (and a
# Cargo home or Cache Snapshot holding the locked dependencies): the build is offline.
# Protected harness digest: %s (%d files).
# The objectives compare the kernel's elapsed_ms. Inner metric: %s, because %s.
"""


def _arg(name, value):
    return "[[measures.%s.command.args]]\nvalue = %s\nprovenance = \"literal\"\n" % (
        name, toml_basic(value))


def render_staged(found, count):
    """The staged policy's exact bytes for protected-harness digest `found`."""
    parts = [HEADER % (found, count, INNER_METRIC_STATUS, INNER_METRIC_REASON)]
    for name, instance, metric in STAGED_MEASURES:
        parts.append("\n[measures.%s]\nrepetitions = %d\nwarm = true\nwall_ms = %d\n"
                     % (name, STAGED_REPETITIONS, STAGED_WALL_MS))
        parts.append("\n[measures.%s.command]\nprogram = %s\n" % (name, toml_basic(PYTHON)))
        for value in command_args(found, instance, metric):
            parts.append("\n" + _arg(name, value))
        parts.append("\n[[measures.%s.metrics]]\nkey = %s\nunit = \"ms\"\n"
                     % (name, toml_basic(metric)))
    for name, _, _ in STAGED_MEASURES:
        parts.append("\n[objectives.%s_elapsed]\nmeasure = %s\nmetric = %s\ndirection = "
                     "\"lower\"\nmin_improvement_ratio = %s\nmin_repetitions = %d\n"
                     % (name, toml_basic(name), toml_basic(OBJECTIVE_METRIC),
                        toml_basic(STAGED_THRESHOLD), STAGED_REPETITIONS))
    return "".join(parts).encode("utf-8")


def staged_documents(root):
    """{relative path: exact bytes} the staged subtree must hold for `root`'s harness."""
    found, entries = checkout_digest(root)
    inventory = inventory_document(found, entries)
    inventory.update(objective_metric=OBJECTIVE_METRIC, inner_metric=INNER_METRIC_STATUS,
                     inner_metric_reason=INNER_METRIC_REASON)
    from . import contracts
    contracts.validate_kernel_inventory(inventory, STAGED_INVENTORY)
    # One file per line, so a re-pin's review diff names exactly what changed.
    listing = json.dumps(inventory, indent=1, sort_keys=True, ensure_ascii=False) + "\n"
    return found, {STAGED_POLICY: render_staged(found, len(entries)),
                   STAGED_INVENTORY: listing.encode("utf-8")}


def _read(path):
    try:
        with open(path, "rb") as stream:
            return stream.read()
    except FileNotFoundError:
        return None


def installed_measures(root):
    """{measure: argument list} of every guarded kernel measure installed in `.af/`."""
    from . import tomlfile
    raw = _read(os.path.join(root, ".af", "code-policy.toml"))
    if raw is None:
        return {}
    policy = tomlfile.loads(raw, ".af/code-policy.toml")
    out = {}
    for name, measure in sorted(policy.get("measures", {}).items()):
        args = [arg.get("value") for arg in measure.get("command", {}).get("args", [])]
        if len(args) > len(PYTHON_FLAGS) and isinstance(args[len(PYTHON_FLAGS)], str) \
                and "af.perf-kernel-harness/" in args[len(PYTHON_FLAGS)]:
            out[name] = (measure["command"].get("program"), args)
    return out


def check_staged(root):
    """(digest, problems): the staged policy and inventory must be exactly what the harness of
    `root` renders, and any installed guarded measure exactly the staged one."""
    found, expected = staged_documents(root)
    problems = []
    for path, data in sorted(expected.items()):
        if _read(os.path.join(root, *path.split("/"))) != data:
            problems.append("%s is not what the protected harness %s renders" % (path, found))
    staged = dict((name, (PYTHON, command_args(found, instance, metric)))
                  for name, instance, metric in STAGED_MEASURES)
    for name, installed in sorted(installed_measures(root).items()):
        if staged.get(name) != installed:
            problems.append(".af/code-policy.toml measure %s does not run the staged bootstrap "
                            "with digest %s; a reviewed human re-pin installs it again" % (
                                name, found))
    return found, problems


def write_staged(root):
    found, expected = staged_documents(root)
    for path, data in sorted(expected.items()):
        target = os.path.join(root, *path.split("/"))
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as stream:
            stream.write(data)
    return found
