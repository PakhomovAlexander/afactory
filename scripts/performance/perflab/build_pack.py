"""Move one build from its build job to its shards without rebuilding (ADR-0177).

Actions artifacts are ZIP transport: a downloaded file is mode 0644, every executable bit lost,
and a member's name is whatever the uploader wrote. A build therefore travels as an uncompressed
`build.tar` beside `build-pack.json` (`af.perf-build-pack/1`), which names the run and plan,
the packed `builds.json`, the archive's own hash and every member's path, size, hash and whether
it is executable.

`pack` admits the build directory as confined, bounded directories and regular files
(`admission.scan`), never following a symlink, and writes a deterministic archive: sorted
members, owner 0, time 0, mode 0644 or 0755. `unpack` refuses before anything is written unless
the pack names this very plan and the archive is the one it hashed; then every member must be a
regular file or directory at a safe relative path, named once, listed in the manifest with its
size. Each file is written new (never through a symlink, never over an existing path), hashed
while it is written, and the set written must be exactly the manifest's. Only then are modes
restored, read-only: 0555 for an executable, 0444 for anything else. A shard then verifies the
build's own manifests and every binary's hash before any launch, as it would on the build host.
"""
import hashlib
import os
import stat
import tarfile

from . import admission, contracts, limits
from .values import check_relative_path, require, sha256_bytes

ARCHIVE = "build.tar"
MANIFEST = "build-pack.json"
CHUNK = 1 << 20
EXECUTABLE_MODE = 0o555
FILE_MODE = 0o444


def _bound(plan):
    return plan["limits"]["build_artifact_bytes"] + limits.BUILD_PACK_EXTRA_BYTES


class _Hashing(object):
    """A file object that hashes exactly what tarfile reads from it."""

    def __init__(self, stream, size, what):
        self.stream = stream
        self.left = size
        self.what = what
        self.digest = hashlib.sha256()

    def read(self, size=-1):
        size = self.left if size is None or size < 0 else min(size, self.left)
        chunk = self.stream.read(size)
        self.left -= len(chunk)
        self.digest.update(chunk)
        return chunk


def pack(build_dir, plan, output):
    """Pack `build_dir`, the verified build of `plan`, into the new directory `output`; the
    build-pack document."""
    bound = _bound(plan)
    entries = admission.scan(build_dir, admission.Bounds(limits.BUILD_PACK_ENTRIES_MAX, 16,
                                                         bound, bound), label="build")
    files = [entry for entry in entries if entry.kind == "file"]
    require(sum(entry.size for entry in files) <= bound, "the build holds more than its %d-byte "
            "bound" % bound)
    os.makedirs(output)
    archive = os.path.join(output, ARCHIVE)
    records = []
    root_fd = admission.open_root(build_dir, "build")
    try:
        with tarfile.open(archive, "w", format=tarfile.PAX_FORMAT) as tar:
            for entry in files:
                check_relative_path(entry.path, "a build member")
                fd = admission.open_entry(root_fd, entry, "build")
                with os.fdopen(fd, "rb") as stream:
                    executable = bool(entry.mode & 0o111)
                    info = tarfile.TarInfo(entry.path)
                    info.size = entry.size
                    info.mode = 0o755 if executable else 0o644
                    info.mtime = 0
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    reader = _Hashing(stream, entry.size, entry.path)
                    tar.addfile(info, reader)
                    require(reader.left == 0, "build/%s shrank while it was packed" % entry.path)
                    require(stream.read(1) == b"", "build/%s grew while it was packed"
                            % entry.path)
                records.append({"path": entry.path, "size": entry.size,
                                "sha256": "sha256:" + reader.digest.hexdigest(),
                                "executable": executable})
    finally:
        os.close(root_fd)
    builds = [item for item in records if item["path"] == "builds.json"]
    require(builds, "the build directory holds no builds.json")
    with open(archive, "rb") as stream:
        size, digest = _hash_stream(stream, bound * 2)
    doc = {"schema": contracts.BUILD_PACK_SCHEMA, "run_id": plan["run_id"],
           "plan_digest": plan["plan_digest"], "builds_sha256": builds[0]["sha256"],
           "archive": {"name": ARCHIVE, "size": size, "sha256": digest},
           "entries": records, "bytes": sum(item["size"] for item in records)}
    with open(os.path.join(output, MANIFEST), "xb") as stream:
        stream.write(contracts.write_bytes(doc, "build-pack"))
    return doc


def _hash_stream(stream, limit):
    digest = hashlib.sha256()
    size = 0
    while True:
        chunk = stream.read(CHUNK)
        if not chunk:
            break
        size += len(chunk)
        require(size <= limit, "the build archive is larger than its bound")
        digest.update(chunk)
    return size, "sha256:" + digest.hexdigest()


def read_manifest(pack_dir, plan):
    """The pack's manifest when it names exactly `plan` and its archive is the one it hashed."""
    path = os.path.join(pack_dir, MANIFEST)
    info = os.lstat(path)
    require(stat.S_ISREG(info.st_mode), "%s is not a regular file" % MANIFEST)
    with open(path, "rb") as stream:
        doc = contracts.read_json(stream.read(limits.MAX_JSON_BYTES + 1), "build-pack",
                                  MANIFEST)
    require((doc["run_id"], doc["plan_digest"]) == (plan["run_id"], plan["plan_digest"]),
            "the build pack is of run %s (%s), not of this plan's %s (%s)"
            % (doc["run_id"], doc["plan_digest"], plan["run_id"], plan["plan_digest"]))
    require(doc["bytes"] <= _bound(plan), "the build pack declares more than the plan's bound")
    archive = os.path.join(pack_dir, ARCHIVE)
    info = os.lstat(archive)
    require(stat.S_ISREG(info.st_mode) and info.st_size == doc["archive"]["size"],
            "%s is not the regular file of %d bytes the pack names" % (ARCHIVE,
                                                                       doc["archive"]["size"]))
    with open(archive, "rb") as stream:
        size, digest = _hash_stream(stream, doc["archive"]["size"])
    require((size, digest) == (doc["archive"]["size"], doc["archive"]["sha256"]),
            "%s is not the archive the pack hashed" % ARCHIVE)
    return doc


def _members(tar, doc):
    """Every archive member checked before anything is written: a regular file or directory
    at a safe path, once, listed with its size."""
    listed = dict((entry["path"], entry) for entry in doc["entries"])
    members = tar.getmembers()
    require(len(members) <= limits.BUILD_PACK_ENTRIES_MAX * 2,
            "the build archive holds too many members")
    seen = set()
    files = []
    for member in members:
        name = member.name
        check_relative_path(name, "a build archive member")
        require(name not in seen, "the build archive holds %s twice" % name)
        seen.add(name)
        if member.isdir():
            continue
        require(member.isfile() and member.type in (tarfile.REGTYPE, tarfile.AREGTYPE),
                "the build archive member %s is not a regular file" % name)
        require(name in listed, "the build archive member %s is not in the pack's manifest"
                % name)
        require(member.size == listed[name]["size"], "the build archive member %s holds %d "
                "bytes, the manifest says %d" % (name, member.size, listed[name]["size"]))
        files.append(member)
    missing = sorted(set(listed) - set(member.name for member in files))
    require(not missing, "the build archive lacks %s" % ", ".join(missing[:8]))
    return files


def unpack(pack_dir, plan, target):
    """Write the verified build of `plan` to the new directory `target` and restore its modes;
    the build-pack document. Anything unexpected refuses, and a partly written target is
    removed."""
    doc = read_manifest(pack_dir, plan)
    listed = dict((entry["path"], entry) for entry in doc["entries"])
    require(not os.path.lexists(target), "%s already exists; a build is unpacked once" % target)
    with tarfile.open(os.path.join(pack_dir, ARCHIVE), "r:") as tar:
        files = _members(tar, doc)
        os.makedirs(target, 0o700)
        try:
            for member in files:
                _write_member(tar, member, listed[member.name], target)
            for member in files:
                entry = listed[member.name]
                os.chmod(os.path.join(target, *member.name.split("/")),
                         EXECUTABLE_MODE if entry["executable"] else FILE_MODE)
        except BaseException:
            admission.remove_tree(target)
            raise
    return doc


def _write_member(tar, member, entry, target):
    parts = member.name.split("/")
    directory = target
    for part in parts[:-1]:
        directory = os.path.join(directory, part)
        if not os.path.lexists(directory):
            os.mkdir(directory, 0o755)
        require(os.path.isdir(directory) and not os.path.islink(directory),
                "%s is not a directory this unpack created" % directory)
    path = os.path.join(directory, parts[-1])
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
                 0o600)
    digest = hashlib.sha256()
    written = 0
    source = tar.extractfile(member)
    require(source is not None, "the build archive member %s has no data" % member.name)
    with os.fdopen(fd, "wb") as stream:
        while True:
            chunk = source.read(min(CHUNK, entry["size"] - written + 1))
            if not chunk:
                break
            written += len(chunk)
            require(written <= entry["size"], "%s is longer than the manifest says"
                    % member.name)
            digest.update(chunk)
            stream.write(chunk)
    require(written == entry["size"], "%s is shorter than the manifest says" % member.name)
    require("sha256:" + digest.hexdigest() == entry["sha256"],
            "%s does not match the pack's hash" % member.name)


def read_member(pack_dir, plan, name):
    """One verified member's bytes, such as the packed `builds.json`, without unpacking."""
    doc = read_manifest(pack_dir, plan)
    listed = dict((entry["path"], entry) for entry in doc["entries"])
    require(name in listed, "the build pack holds no %s" % name)
    with tarfile.open(os.path.join(pack_dir, ARCHIVE), "r:") as tar:
        files = _members(tar, doc)
        member = [item for item in files if item.name == name][0]
        source = tar.extractfile(member)
        data = source.read(listed[name]["size"] + 1)
    require(len(data) == listed[name]["size"]
            and sha256_bytes(data) == listed[name]["sha256"],
            "the packed %s does not match the pack's hash" % name)
    return data

