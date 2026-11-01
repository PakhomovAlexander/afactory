"""Native probe adapters (`fixtures/performance/adapters.toml`, `af.perf-adapters/2`, ADR-0166).

An adapter is a reviewed set of authority example files that a build copies onto a measured
source, at the same paths, before compiling that source's own package. Adapters are tried in
their fixed priority order and the first one that compiles is chosen; nothing at runtime ever
selects another. A contract has at most four adapters. The closure an adapter declares must be
the complete set of files its example compiles from in the authority tree, so a build never
copies an entry point without the helper modules it includes. Each adapter declares the first
AF release it covers (`min_version`): a submission admits a side's library workload only when
an adapter covering its release serves every native contract the plan runs (`coverage`),
before any claim or build; the compile still chooses among the adapters at the build.
"""
import os
import stat

from . import contracts, gitrefs, tomlfile
from .values import ContractError, canonical_digest, require, sha256_bytes

ADAPTERS_PATH = "fixtures/performance/adapters.toml"


def load_bytes(data, where=ADAPTERS_PATH):
    doc = tomlfile.loads(data, where)
    require(doc.get("schema") == contracts.ADAPTERS_SCHEMA,
            where + ".schema: expected %s, found %r" % (contracts.ADAPTERS_SCHEMA,
                                                         doc.get("schema")))
    return contracts.validate_adapters(doc, where)


def load(path):
    with open(path, "rb") as stream:
        return load_bytes(stream.read(tomlfile.MAX_TOML_BYTES + 1), path)


def native_contracts(suite_doc):
    """Every native contract the registry declares: each library case's, its seed writer's and
    the sentinel's."""
    declared = set()
    for case in suite_doc["cases"].values():
        probe = case.get("probe")
        if probe is not None:
            declared.add(probe["contract_id"])
            if "seed" in probe:
                # The seed's writer runs in the same probe (ADR-0170).
                declared.add(probe["seed"]["contract_id"])
    declared.add(suite_doc["probe"]["sentinel_contract"])
    return sorted(declared)


def check_registry(doc, suite_doc, where=ADAPTERS_PATH):
    """Every declared native contract has an adapter, and no adapter serves an undeclared one."""
    declared = set(native_contracts(suite_doc))
    served = set(contract for entry in doc["adapters"].values() for contract in entry["contracts"])
    missing = sorted(declared - served)
    extra = sorted(served - declared)
    require(not missing, where + ": no adapter serves " + ", ".join(missing))
    require(not extra, where + ": adapters serve undeclared contracts " + ", ".join(extra))
    return doc


def coverage(doc, version, needed):
    """(adapter ID, None) of the first adapter by priority whose declared releases include AF
    `version` and that serves every contract of `needed` ({contract: what runs it}), or
    (None, why not)."""
    key = gitrefs.version_key(gitrefs.parse_version(version))
    covering = [(adapter_id, entry) for adapter_id, entry in ordered(doc)
                if key >= gitrefs.version_key(gitrefs.parse_version(entry["min_version"]))]
    if not covering:
        return None, "no declared native probe adapter covers AF %s" % version
    for adapter_id, entry in covering:
        if set(needed) <= set(entry["contracts"]):
            return adapter_id, None
    missing = sorted(contract for contract in needed
                     if not any(contract in entry["contracts"] for _, entry in covering))
    if missing:
        return None, ("no native probe adapter declared for AF %s serves %s" % (
            version, ", ".join("%s (%s)" % (contract, needed[contract]) for contract in missing)))
    return None, ("no single native probe adapter declared for AF %s serves all of %s"
                  % (version, ", ".join(sorted(needed))))


def ordered(doc):
    """[(adapter_id, entry)] in fixed priority order."""
    return sorted(doc["adapters"].items(), key=lambda item: item[1]["priority"])


def closure_files(root, entry):
    """The example's actual files in a tree: its entry point and every file of its module
    directory. Symlinks and other non-regular files are refused."""
    package = "crates/%s/examples/" % entry["package"]
    found = [package + entry["example"] + ".rs"]
    directory = os.path.join(root, package + entry["example"])
    if os.path.lexists(directory):
        info = os.lstat(directory)
        require(stat.S_ISDIR(info.st_mode), "%s is not a directory" % directory)
        for current, dirs, files in os.walk(directory):
            dirs.sort()
            for name in sorted(dirs + files):
                path = os.path.join(current, name)
                require(not os.path.islink(path), "%s is a symlink" % path)
            for name in sorted(files):
                path = os.path.join(current, name)
                require(stat.S_ISREG(os.lstat(path).st_mode),
                        "%s is not a regular file" % path)
                found.append(os.path.relpath(path, root).replace(os.sep, "/"))
    return sorted(found)


def check_closure(doc, root, where=ADAPTERS_PATH):
    """Each adapter's declared closure is exactly its example's files in the tree at `root`."""
    for adapter_id, entry in ordered(doc):
        actual = closure_files(root, entry)
        require(actual == entry["closure"],
                "%s.adapters.%s.closure: declares %s, but the example's files are %s"
                % (where, adapter_id, ", ".join(entry["closure"]), ", ".join(actual)))
    return doc


def read_closure(root, entry):
    """[(path, bytes)] of an adapter's closure in the authority tree at `root`."""
    files = []
    for path in entry["closure"]:
        full = os.path.join(root, path)
        try:
            info = os.lstat(full)
        except FileNotFoundError:
            raise ContractError("authority closure file %s is missing" % path)
        require(stat.S_ISREG(info.st_mode), "authority closure file %s is not a regular file"
                % path)
        with open(full, "rb") as stream:
            files.append((path, stream.read()))
    return files


def overlay_record(adapter_id, files):
    """The builds.json record of one overlay: every file's hash and their joint digest."""
    entries = [{"path": path, "sha256": sha256_bytes(data)} for path, data in files]
    return {"adapter_id": adapter_id, "digest": canonical_digest(entries), "files": entries}
