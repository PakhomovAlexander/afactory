"""Evidence bundles: a directory plus a sorted path/size/SHA256 manifest naming every file.

The manifest's own SHA256 identifies the bundle. Verification rehashes every entry and
refuses missing or extra files, symlinks, non-regular files and unsafe paths. Messages name
a bundle by its `label` (a path relative to the run), never by a machine-local path.
"""
import hashlib
import os
import stat

from . import admission, contracts
from .values import check_relative_path, require, sha256_bytes

MANIFEST = "manifest.json"
READ_CHUNK = 1 << 20


def _walk(root, skip=(), label=None, budget=None):
    """Every regular file below `root` as a relative path; anything else is refused.

    Top-level names in `skip` are other bundles nested in this directory and are not walked.
    """
    label = label or root
    files = []
    pending = [""]
    while pending:
        admission.check(budget, "enumerating " + label)
        relative = pending.pop()
        absolute = os.path.join(root, relative) if relative else root
        for name in sorted(os.listdir(absolute)):
            if not relative and name in skip:
                continue
            path = relative + "/" + name if relative else name
            info = os.lstat(os.path.join(root, path))
            require(not stat.S_ISLNK(info.st_mode), "%s: bundle entry %s is a symlink"
                    % (label, path))
            if stat.S_ISDIR(info.st_mode):
                pending.append(path)
            else:
                require(stat.S_ISREG(info.st_mode), "%s: bundle entry %s is not a regular file"
                        % (label, path))
                check_relative_path(path, "%s: bundle entry" % label)
                files.append(path)
    return sorted(files)


def _hash_file(path, limit, label, budget=None):
    digest = hashlib.sha256()
    size = 0
    with open(path, "rb") as stream:
        while True:
            admission.check(budget, "hashing " + label)
            chunk = stream.read(READ_CHUNK)
            if not chunk:
                break
            size += len(chunk)
            require(size <= limit, "%s: exceeds the bundle byte bound" % label)
            digest.update(chunk)
    return size, "sha256:" + digest.hexdigest()


def write_manifest(root, limit, skip=()):
    """Hash every file of `root` into `manifest.json`; return the bundle identity."""
    entries = []
    total = 0
    for path in _walk(root, skip):
        require(path != MANIFEST, "%s: already holds a manifest" % root)
        size, digest = _hash_file(os.path.join(root, path), limit, path)
        total += size
        require(total <= limit, "%s: exceeds the bundle byte bound" % root)
        entries.append({"path": path, "size": size, "sha256": digest})
    doc = {"schema": contracts.MANIFEST_SCHEMA, "entries": entries}
    data = contracts.write_bytes(doc, "manifest")
    with open(os.path.join(root, MANIFEST), "wb") as stream:
        stream.write(data)
    return sha256_bytes(data)


def verify(root, limit, allowed=None, skip=(), label=None, budget=None):
    """Verify a bundle; return (bundle identity, {path: sha256}).

    `allowed` optionally restricts which paths a bundle of this kind may hold; `skip` names
    top-level entries that are separate bundles. `budget`, when given, is the dominant deadline
    every enumeration and hash read must leave time before (ADR-0172).
    """
    label = label or root
    require(os.path.isdir(root) and not os.path.islink(root),
            "%s: not a bundle directory" % label)
    manifest_path = os.path.join(root, MANIFEST)
    require(os.path.isfile(manifest_path) and not os.path.islink(manifest_path),
            "%s: missing manifest.json" % label)
    with open(manifest_path, "rb") as stream:
        data = stream.read(contracts.limits.MAX_JSON_BYTES + 1)
    doc = contracts.read_json(data, "manifest", label + "/" + MANIFEST)
    require(data == contracts.write_bytes(doc, "manifest"),
            "%s: manifest.json is not canonical" % label)
    listed = dict((entry["path"], entry) for entry in doc["entries"])
    present = [path for path in _walk(root, skip, label, budget) if path != MANIFEST]
    missing = sorted(set(listed) - set(present))
    extra = sorted(set(present) - set(listed))
    require(not missing, "%s: missing bundle files %s" % (label, ", ".join(missing)))
    require(not extra, "%s: unlisted bundle files %s" % (label, ", ".join(extra)))
    if allowed is not None:
        for path in listed:
            require(allowed(path), "%s: %s does not belong in this bundle" % (label, path))
    # Logical sizes are admitted before any byte is read: a sparse or grown file is refused by
    # its size, never by reading it (ADR-0172).
    require(sum(entry["size"] for entry in listed.values()) <= limit,
            "%s: exceeds the bundle byte bound" % label)
    for path, entry in sorted(listed.items()):
        size = os.lstat(os.path.join(root, path)).st_size
        require(size == entry["size"], "%s: %s has %d bytes, the manifest says %d"
                % (label, path, size, entry["size"]))
    total = 0
    hashes = {}
    for path, entry in sorted(listed.items()):
        size, digest = _hash_file(os.path.join(root, path), limit, label + "/" + path, budget)
        total += size
        require(total <= limit, "%s: exceeds the bundle byte bound" % label)
        require(size == entry["size"], "%s: %s has %d bytes, the manifest says %d"
                % (label, path, size, entry["size"]))
        require(digest == entry["sha256"], "%s: %s does not match its manifest hash"
                % (label, path))
        hashes[path] = digest
    return sha256_bytes(data), hashes


def read_member(root, path, hashes, limit, label=None):
    """Read a member of a verified bundle and check it still matches its manifest hash."""
    label = label or root
    require(path in hashes, "%s: %s is not in the bundle" % (label, path))
    full = os.path.join(root, path)
    require(not os.path.islink(full), "%s: %s is a symlink" % (label, path))
    with open(full, "rb") as stream:
        data = stream.read(limit + 1)
    require(len(data) <= limit, "%s: %s exceeds its bound" % (label, path))
    require(sha256_bytes(data) == hashes[path], "%s: %s changed after verification"
            % (label, path))
    return data
