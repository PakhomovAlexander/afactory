"""Side-written fixture seeds of a shard (ADR-0169, ADR-0172).

A seed is large state, such as a 10,000-event Task log or Campaign, that one side writes once
per shard through its own binaries, under the authority supervisor and in the shard's
containment. Once every process of its writer has exited, it is published to a directory of the
shard's own and sealed: every directory becomes 0555 and every file 0444 (0555 when its owner
could execute it), and its identity is one digest over every entry's relative path, kind, mode
and bytes. The shard verifies that digest before and after every sample that copies the seed,
and each such sample works on its own private copy, so no sample shares a mutable Store, another
side's state or a cached projection.

Its writer is a measured side, so nothing it left is trusted (ADR-0172). The whole published
tree is enumerated through held descriptors before any mode changes or any byte is read
(`perflab.admission`): a symlink, a symlinked directory or any entry but a directory or a
regular file refuses the seed, and so do more entries, deeper nesting or more logical bytes
than its `Bounds`. Sealing changes each entry's mode through its own verified descriptor, so
nothing outside the seed can change, and every hash read is bounded by the entry's enumerated
size and the dominant deadline. Removal unlinks a symlink and never follows it.
"""
from . import admission, limits, reference
from .values import canonical_bytes, require

DIRECTORY_MODE = 0o555
FILE_MODE = 0o444
EXECUTABLE_MODE = 0o555


def bounds(scratch_limit_bytes):
    """A seed's bounds: the declared entry count and depth, and no more logical bytes than the
    scratch every sample copies it into (the environment's `scratch_limit_bytes`)."""
    return admission.Bounds(limits.SEED_ENTRIES_MAX, limits.SEED_DEPTH_MAX,
                            scratch_limit_bytes, scratch_limit_bytes)


def _sealed_mode(entry):
    if entry.kind == "directory":
        return DIRECTORY_MODE
    return EXECUTABLE_MODE if entry.mode & 0o100 else FILE_MODE


def seal(root, seed_bounds, budget=None):
    """Make every entry below `root`, and `root`, read-only, after the whole tree is admitted:
    nothing changes when anything in it is unsupported or beyond `seed_bounds`."""
    entries = admission.scan(root, seed_bounds, budget, label="the seed",
                             step="admitting the seed")
    admission.chmod_entries(root, entries, _sealed_mode, budget, label="the seed",
                            root_mode=DIRECTORY_MODE)


def tree(root, seed_bounds, budget=None):
    """(digest, files, bytes) of the seed below `root`, enumerated and read within
    `seed_bounds` and the dominant deadline."""
    entries = admission.scan(root, seed_bounds, budget, label="the seed",
                             step="verifying the seed")
    hashes = admission.digests(root, entries, budget, label="the seed",
                               step="hashing the seed")
    return identity(entries, hashes)


def identity(entries, hashes):
    """(digest, files, bytes) of enumerated seed entries and their files' SHA-256s: the one
    identity `tree` gives a seed, also of a confined copy's bytes (`admission.copy_tree`)."""
    records = []
    files = size = 0
    for entry in entries:
        if entry.kind == "directory":
            records.append({"path": entry.path, "kind": "directory", "mode": entry.mode})
        else:
            files += 1
            size += entry.size
            records.append({"path": entry.path, "kind": "file", "mode": entry.mode,
                            "size": entry.size, "sha256": hashes[entry.path]})
    require(files > 0, "the seed holds no file")
    return reference.content_id(canonical_bytes(records)), files, size


def remove(root):
    """Remove a sealed seed; the problems, empty when it is gone."""
    return admission.remove_tree(root)
