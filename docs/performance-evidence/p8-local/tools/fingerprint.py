"""Fingerprints of a source tree, to reconcile the measured commit with a later sealed tree.

  fingerprint.py ROOT                          the full fingerprint of ROOT, with every line
  fingerprint.py reconcile ROOT FULL_JSON      what differs between FULL_JSON's tree and ROOT
  fingerprint.py verify ROOT RETAINED_JSON     whether ROOT is the tree RETAINED_JSON reconciled
  fingerprint.py repair ROOT FINALIZED RETAINED_JSON
                                               what differs between FINALIZED, the tree
                                               RETAINED_JSON reconciled, and ROOT
  fingerprint.py verify-repair ROOT RETAINED_JSON REPAIR_JSON
                                               whether ROOT is the tree REPAIR_JSON reconciled
                                               with the tree RETAINED_JSON reconciled
  fingerprint.py rerepair ROOT PRIOR_ROOT RETAINED_JSON PRIOR_REPAIR_JSON
                                               what differs between FINALIZED, the tree
                                               RETAINED_JSON reconciled, and ROOT, where
                                               PRIOR_ROOT is the tree PRIOR_REPAIR_JSON
                                               reconciled; verify-repair checks the result

The file list is every regular file below ROOT except the top-level `target/` and `.git/`, and,
for reconciliation, the evidence directory `docs/performance-evidence/p8-local/` itself. A file's
line is "path\\0size\\0sha256-hex\\n" (UTF-8); a digest is the SHA-256 of the lines of the files it
covers, sorted by path bytes.
"""
import hashlib
import json
import os
import subprocess
import sys

EVIDENCE = "docs/performance-evidence/p8-local/"
REGISTRY = ("fixtures/performance/suite.toml", "fixtures/performance/cli.toml",
            "fixtures/performance/adapters.toml", "fixtures/performance/diagnostics.toml",
            "fixtures/performance/golden/native.json", "Cargo.lock", "rust-toolchain.toml",
            "crates/review-pipeline/examples/perf-probe.rs",
            "crates/review-process/examples/perf-supervisor.rs")


def files(root, evidence=True):
    out = {}
    for base, dirs, names in os.walk(root):
        if os.path.relpath(base, root) == ".":
            dirs[:] = [name for name in dirs if name not in ("target", ".git")]
        for name in names:
            path = os.path.join(base, name)
            relative = os.path.relpath(path, root).replace(os.sep, "/")
            if not evidence and relative.startswith(EVIDENCE):
                continue
            with open(path, "rb") as stream:
                data = stream.read()
            out[relative] = (len(data), hashlib.sha256(data).hexdigest())
    return out


def digest(entries, paths):
    hasher = hashlib.sha256()
    for path in sorted(paths, key=lambda item: item.encode("utf-8")):
        size, sha = entries[path]
        hasher.update(("%s\0%d\0%s\n" % (path, size, sha)).encode("utf-8"))
    return "sha256:" + hasher.hexdigest()


def harness_digest(root):
    """ROOT's protected-harness digest, as ROOT's own laboratory computes it."""
    return subprocess.run(
        ["python3", "-B", os.path.join(root, "scripts/performance/cli.py"), "harness-digest",
         "--root", root], stdout=subprocess.PIPE, check=True).stdout.decode().split()[0]


def fingerprint(root):
    entries = files(root)
    tops = sorted(set(path.split("/")[0] for path in entries))
    harness = harness_digest(root)
    return {
        "files": len(entries),
        "all_files_digest": digest(entries, entries),
        "top_level_digests": dict((top, digest(entries, [path for path in entries
                                                         if path.split("/")[0] == top]))
                                  for top in tops),
        "harness_digest": harness,
        "registry_and_probe_sha256": dict((path, "sha256:" + entries[path][1])
                                          for path in REGISTRY),
        "lines": dict((path, "%d %s" % entries[path]) for path in sorted(entries)),
    }


def reconcile(root, full_path):
    with open(full_path) as stream:
        before = json.load(stream)["lines"]
    entries = files(root, evidence=False)
    now = dict((path, "%d %s" % entries[path]) for path in entries)
    changed = sorted(path for path in set(now) | set(before) if now.get(path) != before.get(path))
    unchanged = [path for path in now if path not in changed]
    return {"changed_paths": [{"path": path, "measured": before.get(path),
                               "finalized": now.get(path)} for path in changed],
            "unchanged_files": len(unchanged),
            "unchanged_files_digest": digest(entries, unchanged)}


def verify_entries(entries, retained_path):
    with open(retained_path) as stream:
        recorded = json.load(stream)["reconciliation"]
    changed = dict((item["path"], item["finalized"]) for item in recorded["changed_paths"])
    unchanged = [path for path in entries if path not in changed]
    mismatched = sorted(path for path, line in changed.items()
                        if ("%d %s" % entries[path] if path in entries else None) != line)
    result = {"unchanged_files": len(unchanged),
              "unchanged_files_digest": digest(entries, unchanged),
              "changed_paths_not_as_finalized": mismatched}
    result["matches"] = (result["unchanged_files_digest"] == recorded["unchanged_files_digest"]
                         and result["unchanged_files"] == recorded["unchanged_files"]
                         and not mismatched)
    return result


def verify(root, retained_path):
    return verify_entries(files(root, evidence=False), retained_path)


def repair(root, base, retained_path):
    """A later repair's reconciliation with the finalized tree `base`, which must be the tree
    RETAINED_JSON reconciled; the measured record is never rewritten."""
    if not verify(base, retained_path)["matches"]:
        sys.exit("%s is not the tree %s reconciled" % (base, retained_path))
    before = dict((path, "%d %s" % item) for path, item in files(base, evidence=False).items())
    return repair_record(root, before,
                         "ADR-0173: each path the repair changed relative to the tree that "
                         "measured-source.json reconciled, and the digest of every other file")


def rerepair(root, prior_root, retained_path, prior_repair_path):
    """A further repair's reconciliation with the same finalized tree, which no longer exists
    as a directory: `prior_root` must be the tree PRIOR_REPAIR_JSON reconciled, and replacing
    each path that record lists by its finalized line (removing a path it added) gives the
    finalized tree's lines. Neither earlier record is rewritten."""
    if not verify_repair(prior_root, retained_path, prior_repair_path)["matches"]:
        sys.exit("%s is not the tree %s reconciled" % (prior_root, prior_repair_path))
    with open(prior_repair_path) as stream:
        prior = json.load(stream)
    before = dict((path, "%d %s" % item)
                  for path, item in files(prior_root, evidence=False).items())
    for item in prior["changed_paths"]:
        if item["finalized"] is None:
            before.pop(item["path"], None)
        else:
            before[item["path"]] = item["finalized"]
    return repair_record(root, before,
                         "ADR-0174: each path the repairs changed relative to the tree that "
                         "measured-source.json reconciled, and the digest of every other file")


def repair_record(root, before, what):
    """Each path whose line in ROOT differs from `before`, the finalized tree's lines, with
    both lines, ROOT's harness digest and the digest of every other file."""
    entries = files(root, evidence=False)
    now = dict((path, "%d %s" % entries[path]) for path in entries)
    changed = sorted(path for path in set(now) | set(before) if now.get(path) != before.get(path))
    unchanged = [path for path in now if path not in changed]
    harness = harness_digest(root)
    return {"what": what,
            "changed_paths": [{"path": path, "finalized": before.get(path),
                               "repaired": now.get(path)} for path in changed],
            "repaired_harness_digest": harness,
            "unchanged_files": len(unchanged),
            "unchanged_files_digest": digest(entries, unchanged)}


def verify_repair(root, retained_path, repair_path):
    """ROOT holds each listed path's repaired bytes and every other file as recorded, and
    replacing the listed paths by their finalized bytes gives the tree RETAINED_JSON
    reconciled, so every file neither record lists is the measured commit's own."""
    with open(repair_path) as stream:
        recorded = json.load(stream)
    entries = files(root, evidence=False)
    changed = dict((item["path"], item) for item in recorded["changed_paths"])
    unchanged = [path for path in entries if path not in changed]
    mismatched = sorted(path for path, item in changed.items()
                        if ("%d %s" % entries[path] if path in entries else None)
                        != item["repaired"])
    base = dict((path, entries[path]) for path in unchanged)
    for path, item in changed.items():
        if item["finalized"] is not None:
            size, sha = item["finalized"].split(" ")
            base[path] = (int(size), sha)
    chained = verify_entries(base, retained_path)
    result = {"unchanged_files": len(unchanged),
              "unchanged_files_digest": digest(entries, unchanged),
              "changed_paths_not_as_repaired": mismatched,
              "finalized_tree_reconstructed": chained["matches"]}
    result["matches"] = (result["unchanged_files_digest"] == recorded["unchanged_files_digest"]
                         and result["unchanged_files"] == recorded["unchanged_files"]
                         and not mismatched and chained["matches"])
    return result


if __name__ == "__main__":
    if sys.argv[1] == "reconcile":
        print(json.dumps(reconcile(sys.argv[2], sys.argv[3]), indent=1, sort_keys=True))
    elif sys.argv[1] == "verify":
        outcome = verify(sys.argv[2], sys.argv[3])
        print(json.dumps(outcome, indent=1, sort_keys=True))
        sys.exit(0 if outcome["matches"] else 1)
    elif sys.argv[1] == "repair":
        print(json.dumps(repair(sys.argv[2], sys.argv[3], sys.argv[4]), indent=1,
                         sort_keys=True))
    elif sys.argv[1] == "rerepair":
        print(json.dumps(rerepair(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]),
                         indent=1, sort_keys=True))
    elif sys.argv[1] == "verify-repair":
        outcome = verify_repair(sys.argv[2], sys.argv[3], sys.argv[4])
        print(json.dumps(outcome, indent=1, sort_keys=True))
        sys.exit(0 if outcome["matches"] else 1)
    else:
        print(json.dumps(fingerprint(sys.argv[1]), indent=1, sort_keys=True))
