"""The laboratory's content-addressed filesystem archive (package p4, ADR-0172).

CI artifacts are transport; an archive root on a persistent filesystem keeps evidence beyond
their expiry. It is written only by a trusted collector, separately from benchmark execution,
and nothing in it is ever executed. Layout below the root:

    archive.json                 af.perf-archive/1: capacity (default 100 GiB, at most 1 TiB)
    objects/sha256/<2>/<62>      every admitted member's bytes, by SHA-256, read-only
    index/<12 digits>.json       af.perf-archive-event/1, append-only: runs, retention, pins,
                                 supersessions and reference-traced garbage collections
    collections/<12 digits>.json af.perf-archive-collection/1, append-only: every attempt
    tmp/                         private staging, cleared under the lock
    lock                         one advisory flock held by every command

An entry is written whole to `tmp/`, synced, then linked to its next sequence name: `link`
never replaces an existing name, so an interrupted command leaves only staging, which the next
command clears, and no entry ever names an object that was not durably written first. Readers
refuse a gap, an unknown version or field, or a name out of sequence, so the index fails closed.

A run is admitted only whole: `compare.evaluate` rehashes its run bundle and every shard bundle
present, a partial or tampered bundle refuses the import, and each member is rehashed again while
it is copied into its object; an existing object of the same name is rehashed before it is
trusted, so a collision or a corrupted object refuses. Importing the same run again is
idempotent; another run under the same ID refuses. Capacity is checked before anything is
written: an import that would exceed it, or the filesystem's free space, refuses with a recorded
`capacity_exhausted` collection, which the archive's status reports as a health failure; the
source evidence is never touched.

Retention (`retain`) writes an explicit `retention` entry with the run's original manifest, what
expired and what remains: numeric samples, plans, build manifests, verdicts and summaries stay
at least 365 days after admission; logs, transported views, profiles and their symbols 90 days.
A pinned run (an active calibration cohort or a release-budget demonstration) stays whole until
it is superseded and then 365 days more. Garbage collection only removes objects no available
member of any run references, so a shared object stays while any run needs it, and space is
never a reason to evict anything.
"""
import errno
import fcntl
import os
import re
import secrets
import shutil
import stat
import time

from . import admission, bundle, compare, contracts, limits, report
from .values import ContractError, require, sha256_bytes

ARCHIVE_FILE = "archive.json"
OBJECTS = "objects/sha256"
INDEX = "index"
COLLECTIONS = "collections"
TMP = "tmp"
LOCK = "lock"
DAY_MS = 86400000
RETENTION_DAYS = {"plan": 365, "build_manifest": 365, "manifest": 365, "shard": 365,
                  "samples": 365, "verdict": 365, "summary": 365, "log": 90,
                  "transported_view": 90, "profile": 90, "symbol": 90}
NUMERIC = tuple(name for name, days in sorted(RETENTION_DAYS.items()) if days == 365)
PROFILE_CLASSES = ("profile", "symbol")
PINNED_AFTER_SUPERSEDED_DAYS = 365
ENTRY_NAME = re.compile(r"^([0-9]{12})\.json$")
# A run's verdict and generated summary, beside its bundles' members.
VERDICT_PATH = "verdict.json"
SUMMARY_PATH = "report.json"
VIEWS = "views/"
# The objects below the root an archive enumerates; its capacity bounds their bytes.
OBJECT_BOUNDS = admission.Bounds(entries=50000000, depth=3, file_bytes=limits.ARCHIVE_BYTES_MAX,
                                 total_bytes=2 * limits.ARCHIVE_BYTES_MAX)
# Kept free on the filesystem beyond an import's own bytes.
FILESYSTEM_MARGIN_BYTES = 64 * limits.MIB
# How close to its expiry an uncollected transport artifact is a risk.
EXPIRY_RISK_MS = 7 * DAY_MS
CAPACITY_LOW_RATIO = 10


class Refused(ContractError):
    """The archive refused the command; `status` is the collection outcome it recorded."""

    def __init__(self, status, detail):
        ContractError.__init__(self, detail)
        self.status = status


def _class(path):
    if path.startswith(VIEWS):
        return "transported_view"
    if "/logs/" in path:
        return "log"
    if path == VERDICT_PATH:
        return "verdict"
    if path == SUMMARY_PATH:
        return "summary"
    return {"run.json": "plan", "builds.json": "build_manifest", "manifest.json": "manifest",
            "shard.json": "shard", "samples.jsonl": "samples"}[path.rsplit("/", 1)[-1]]


class RunState(object):
    """One archived run: its admission entry, what expired, its pins and supersession."""

    def __init__(self, event):
        self.event = event
        self.expired = {}
        self.pinned = []
        self.superseded = None

    @property
    def members(self):
        return self.event["event"]["members"]

    def available(self):
        return [member for member in self.members if member["path"] not in self.expired]

    def numeric(self):
        return "complete" if all(member["path"] not in self.expired for member in self.members
                                 if member["class"] in NUMERIC) else "expired"

    def profile_reproduction(self):
        profiles = [member for member in self.members if member["class"] in PROFILE_CLASSES]
        if not profiles:
            return "not_applicable"
        return "expired" if any(member["path"] in self.expired for member in profiles) \
            else "available"


class Archive(object):
    """An opened archive root, its lock held until `close`."""

    def __init__(self, root, wall=time.time, free_bytes=None):
        self.root = os.path.abspath(root)
        self.wall = wall
        self.free_bytes = free_bytes or (lambda path: shutil.disk_usage(path).free)
        self.config = None
        self.lock_fd = None

    # Creation, opening and the lock -------------------------------------------------------

    @classmethod
    def create(cls, root, capacity_bytes=limits.ARCHIVE_BYTES_DEFAULT, wall=time.time):
        """A new archive at `root`, which must not exist or be an empty directory."""
        require(1 <= capacity_bytes <= limits.ARCHIVE_BYTES_MAX,
                "--capacity-bytes: 1 to %d (1 TiB) without a reviewed policy change"
                % limits.ARCHIVE_BYTES_MAX)
        root = os.path.abspath(root)
        if os.path.lexists(root):
            require(os.path.isdir(root) and not os.path.islink(root) and not os.listdir(root),
                    "%s exists and is not an empty directory" % root)
        else:
            os.mkdir(root, 0o700)
        for directory in (OBJECTS, INDEX, COLLECTIONS, TMP):
            os.makedirs(os.path.join(root, directory), 0o700)
        doc = {"schema": contracts.ARCHIVE_SCHEMA, "capacity_bytes": capacity_bytes,
               "created_unix_ms": int(wall() * 1000)}
        _write_new(os.path.join(root, ARCHIVE_FILE), contracts.write_bytes(doc, "archive"))
        return cls(root, wall)

    def open(self):
        info = os.lstat(self.root)
        require(stat.S_ISDIR(info.st_mode), "%s is not an archive directory (a symlink is "
                "never followed)" % self.root)
        for directory in (OBJECTS, INDEX, COLLECTIONS, TMP):
            self._check_directory(directory)
        self.config = contracts.read_json(self._read(ARCHIVE_FILE), "archive", ARCHIVE_FILE)
        fd = os.open(os.path.join(self.root, LOCK), os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW,
                     0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            os.close(fd)
            if error.errno in (errno.EWOULDBLOCK, errno.EAGAIN):
                raise Refused("refused", "another archive command holds %s" % self.root)
            raise
        self.lock_fd = fd
        # Staging an interrupted command left; nothing in it was ever linked into the index.
        staging = os.path.join(self.root, TMP)
        for name in os.listdir(staging):
            admission.remove_tree(os.path.join(staging, name))
        return self

    def close(self):
        if self.lock_fd is not None:
            os.close(self.lock_fd)
            self.lock_fd = None

    def __enter__(self):
        return self.open()

    def __exit__(self, *exc):
        self.close()

    def _check_directory(self, relative):
        path = self.root
        for part in relative.split("/"):
            path = os.path.join(path, part)
            info = os.lstat(path)
            require(stat.S_ISDIR(info.st_mode), "%s is not a directory of this archive (a "
                    "symlink is never followed)" % path)

    def _read(self, relative, limit=limits.MAX_JSON_BYTES):
        fd, info = admission.open_confined(self.root, relative, "the archive")
        try:
            require(info.st_size <= limit, "%s exceeds %d bytes" % (relative, limit))
            chunks = []
            admission.read_bounded(fd, info.st_size, None, relative, "reading " + relative,
                                   chunks.append)
            return b"".join(chunks)
        finally:
            os.close(fd)

    # The append-only logs ------------------------------------------------------------------

    def _entries(self, directory, name):
        entries = []
        for index, entry_name in enumerate(sorted(os.listdir(os.path.join(self.root,
                                                                          directory)))):
            match = ENTRY_NAME.match(entry_name)
            require(match is not None and int(match.group(1)) == index + 1,
                    "%s/%s is not the next entry of the archive's %s" % (directory, entry_name,
                                                                         directory))
            relative = "%s/%s" % (directory, entry_name)
            doc = contracts.read_json(self._read(relative), name, relative)
            require(doc["sequence"] == index + 1, "%s names sequence %d" % (relative,
                                                                           doc["sequence"]))
            entries.append(doc)
        return entries

    def events(self):
        return self._entries(INDEX, "archive-event")

    def collections(self):
        return self._entries(COLLECTIONS, "archive-collection")

    def _staged(self, data):
        path = os.path.join(self.root, TMP, "stage-" + secrets.token_hex(12))
        _write_new(path, data, sync=True)
        return path

    def _append(self, directory, name, body):
        require(self.lock_fd is not None, "the archive is not open")
        sequence = len(self._entries(directory, name)) + 1
        doc = dict(body, schema=contracts.CONTRACTS[name][0], sequence=sequence)
        data = contracts.write_bytes(doc, name)
        staged = self._staged(data)
        target = os.path.join(self.root, directory, "%012d.json" % sequence)
        try:
            os.link(staged, target)
        finally:
            os.unlink(staged)
        os.chmod(target, 0o444)
        _sync_directory(os.path.join(self.root, directory))
        return doc

    def _event(self, event, unix_ms=None):
        return self._append(INDEX, "archive-event", {
            "unix_ms": int(self.wall() * 1000) if unix_ms is None else unix_ms,
            "event": event})

    def _collection(self, started, source, status, run_id, detail, run_ended=None,
                    needed=None, available=None):
        return self._append(COLLECTIONS, "archive-collection", {
            "started_unix_ms": started, "ended_unix_ms": max(started, int(self.wall() * 1000)),
            "source": source, "status": status, "run_id": run_id,
            "run_ended_unix_ms": run_ended, "needed_bytes": needed,
            "available_bytes": available, "detail": detail[:4096]})

    # State --------------------------------------------------------------------------------

    def state(self):
        """{run_id: RunState} from the index, in admission order."""
        runs = {}
        for doc in self.events():
            event = doc["event"]
            kind = event["kind"]
            if kind == "run":
                require(event["run_id"] not in runs, "index entry %d admits run %s twice"
                        % (doc["sequence"], event["run_id"]))
                runs[event["run_id"]] = RunState(doc)
                continue
            if kind == "gc":
                continue
            run = runs.get(event["run_id"])
            require(run is not None, "index entry %d names run %s, which was never admitted"
                    % (doc["sequence"], event["run_id"]))
            if kind == "retention":
                run.expired = dict((item["path"], item) for item in event["expired"])
            elif kind == "pin":
                run.pinned.append(doc)
            else:
                run.superseded = doc
        return runs

    def object_relative(self, digest):
        hexdigest = digest[len("sha256:"):]
        return "%s/%s/%s" % (OBJECTS, hexdigest[:2], hexdigest[2:])

    def objects(self):
        """[(sha256, size)] of every object, enumerated without following anything."""
        entries = admission.scan(os.path.join(self.root, OBJECTS), OBJECT_BOUNDS,
                                 label="the archive's objects")
        found = []
        for entry in entries:
            parts = entry.path.split("/")
            if entry.kind == "directory":
                require(len(parts) == 1 and re.match(r"^[0-9a-f]{2}$", parts[0]),
                        "objects/sha256/%s is not an object directory" % entry.path)
                continue
            require(len(parts) == 2 and re.match(r"^[0-9a-f]{62}$", parts[1]),
                    "objects/sha256/%s is not an object" % entry.path)
            found.append(("sha256:" + parts[0] + parts[1], entry.size))
        return found

    def usage(self):
        """Bytes the archive holds: its objects and its logs."""
        used = sum(size for _, size in self.objects())
        for directory in (INDEX, COLLECTIONS):
            base = os.path.join(self.root, directory)
            used += sum(os.lstat(os.path.join(base, name)).st_size for name in os.listdir(base))
        return used

    def _object_present(self, digest, size):
        """Whether the object exists, after rehashing it: anything but exactly these bytes
        under this name is a collision or a corruption, and refuses."""
        relative = self.object_relative(digest)
        try:
            fd, info = admission.open_confined(self.root, relative, "the archive")
        except FileNotFoundError:
            return False
        except ContractError:
            raise
        try:
            require(info.st_size == size and admission.read_bounded(
                fd, info.st_size, None, relative, "verifying " + relative) == digest,
                "the archive's object %s is not the %d bytes its name says; nothing was "
                "admitted over it" % (digest, size))
        finally:
            os.close(fd)
        return True

    def _put(self, digest, size, write):
        """Write a new object: `write(stream)` streams its bytes and returns their digest,
        which must be `digest`; linked under its name only once complete and synced."""
        relative = self.object_relative(digest)
        directory = os.path.join(self.root, os.path.dirname(relative))
        if not os.path.lexists(directory):
            os.mkdir(directory, 0o700)
        self._check_directory(os.path.dirname(relative))
        staged = os.path.join(self.root, TMP, "object-" + secrets.token_hex(12))
        fd = os.open(staged, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            with os.fdopen(fd, "wb") as stream:
                written = write(stream)
                stream.flush()
                os.fsync(stream.fileno())
            require(written == digest, "a member changed while it was archived: %s, not %s"
                    % (written, digest))
            require(os.lstat(staged).st_size == size, "a member changed size while it was "
                    "archived")
            os.chmod(staged, 0o444)
            try:
                os.link(staged, os.path.join(self.root, relative))
            except FileExistsError:
                self._object_present(digest, size)
        finally:
            if os.path.lexists(staged):
                os.unlink(staged)
        _sync_directory(directory)

    # Import -------------------------------------------------------------------------------

    def _members(self, run_dir, evaluation, views):
        """[(member, source)] of a whole run: every verified bundle member (source a
        (root, relative path) to stream), its verdict and summary, and transported views
        (source bytes)."""
        plan = evaluation.plan
        reasons = "; ".join("%s: %s" % (reason["code"], reason["detail"])
                            for reason in evaluation.verdict["aggregate"]["reasons"])[:2048]
        require(evaluation.hashes is not None, "the run bundle did not verify, so nothing was "
                "archived: " + reasons)
        found = []

        def add(root, prefix, hashes, identity):
            with open(os.path.join(root, bundle.MANIFEST), "rb") as stream:
                manifest = stream.read(limits.MAX_JSON_BYTES + 1)
            require(sha256_bytes(manifest) == identity, "%s%s changed after verification"
                    % (prefix, bundle.MANIFEST))
            found.append(({"path": prefix + bundle.MANIFEST, "sha256": identity,
                           "size": len(manifest), "class": "manifest"}, manifest))
            for path, digest in sorted(hashes.items()):
                size = os.lstat(os.path.join(root, path)).st_size
                found.append(({"path": prefix + path, "sha256": digest, "size": size,
                               "class": _class(prefix + path)}, (root, path)))

        add(run_dir, "", evaluation.hashes, evaluation.verdict["inputs"]["run_bundle"])
        for evidence in evaluation.evidence:
            root = os.path.join(run_dir, compare.SHARD_DIR, str(evidence.index))
            if evidence.hashes is None:
                require(not os.path.lexists(root), "shard %d's bundle is partial, tampered or "
                        "of another run, so nothing was archived: %s" % (evidence.index,
                                                                         reasons))
                continue
            add(root, "%s/%d/" % (compare.SHARD_DIR, evidence.index), evidence.hashes,
                evidence.bundle)
        generated = [(VERDICT_PATH, contracts.write_bytes(evaluation.verdict, "verdict")),
                     (SUMMARY_PATH, contracts.write_bytes(report.build(evaluation, []),
                                                          "report"))]
        for name, data in views:
            require(not name.startswith(".") and "/" not in name, "a transported view is "
                    "named %r" % name)
            generated.append((VIEWS + name, data))
        for path, data in generated:
            found.append(({"path": path, "sha256": sha256_bytes(data), "size": len(data),
                           "class": _class(path)}, data))
        found.sort(key=lambda item: item[0]["path"])
        paths = [member["path"] for member, _ in found]
        require(len(paths) == len(set(paths)), "the run names one member twice")
        return found

    def import_run(self, run_dir, source=None, views=()):
        """Admit the whole run in `run_dir`; (status, run_id): `imported` or
        `already_archived`. Anything else refuses (`Refused`) with its recorded collection."""
        started = int(self.wall() * 1000)
        source = source or {"kind": "local"}
        try:
            evaluation = compare.evaluate(run_dir)
            members = self._members(run_dir, evaluation, views)
        except (ContractError, OSError) as error:
            detail = "the run was not admitted: %s" % error
            self._collection(started, source, "refused", None, detail)
            raise Refused("refused", detail)
        plan, verdict = evaluation.plan, evaluation.verdict
        run_id = plan["run_id"]
        ended = [evidence.doc["ended_unix_ms"] for evidence in evaluation.evidence
                 if evidence.doc is not None]
        run_ended = max(ended) if ended else None
        event = {"kind": "run", "run_id": run_id, "plan_digest": plan["plan_digest"],
                 "source": source,
                 "verdict": {"result": verdict["aggregate"]["result"],
                             "exit_code": verdict["aggregate"]["exit_code"],
                             "scope": verdict["scope"], "provenance": verdict["provenance"],
                             "satisfies_required_check":
                                 verdict["aggregate"]["satisfies_required_check"]},
                 "bundles": {"run": verdict["inputs"]["run_bundle"],
                             "shards": verdict["inputs"]["shard_bundles"]},
                 "run_ended_unix_ms": run_ended,
                 "members": [member for member, _ in members]}
        existing = self.state().get(run_id)
        if existing is not None:
            known = existing.event["event"]
            same = (known["plan_digest"], known["bundles"], known["members"]) == (
                event["plan_digest"], event["bundles"], event["members"])
            if same:
                self._collection(started, source, "already_archived", run_id,
                                 "the identical run is already archived", run_ended)
                return "already_archived", run_id
            detail = ("another run is already archived under %s; an archived run is never "
                      "replaced" % run_id)
            self._collection(started, source, "refused", run_id, detail, run_ended)
            raise Refused("refused", detail)
        needed = {}
        try:
            for member, _ in members:
                if member["sha256"] not in needed and not self._object_present(
                        member["sha256"], member["size"]):
                    needed[member["sha256"]] = member["size"]
        except (ContractError, OSError) as error:
            detail = "the run was not admitted: %s" % error
            self._collection(started, source, "refused", run_id, detail, run_ended)
            raise Refused("refused", detail)
        needed_bytes = sum(needed.values()) + limits.MAX_JSON_BYTES // 64
        used = self.usage()
        available = max(0, self.config["capacity_bytes"] - used)
        free = self.free_bytes(self.root)
        if needed_bytes > available or needed_bytes + FILESYSTEM_MARGIN_BYTES > free:
            what = ("its capacity" if needed_bytes > available
                    else "the filesystem's free space")
            detail = ("the archive's %s is exhausted: run %s needs %d bytes, %d of the %d-byte "
                      "capacity and %d filesystem bytes are free; nothing was imported and "
                      "the source evidence is untouched" % (
                          what, run_id, needed_bytes, available, self.config["capacity_bytes"],
                          free))
            self._collection(started, source, "capacity_exhausted", run_id, detail, run_ended,
                             needed_bytes, min(available, free))
            raise Refused("capacity_exhausted", detail)
        for member, origin in members:
            if member["sha256"] not in needed:
                continue
            needed.pop(member["sha256"])
            if isinstance(origin, bytes):
                self._put(member["sha256"], member["size"], _bytes_writer(origin))
            else:
                self._put(member["sha256"], member["size"],
                          _file_writer(origin[0], origin[1], member["size"]))
        self._event(event)
        self._collection(started, source, "imported", run_id, "run %s admitted with %d members"
                         % (run_id, len(members)), run_ended)
        return "imported", run_id

    def record_failure(self, started, source, status, detail):
        """Record a collection that admitted nothing, such as a transport failure."""
        return self._collection(started, source, status, None, detail)

    # Pins and retention ---------------------------------------------------------------------

    def pin(self, run_id, reason, note=""):
        run = self.state().get(run_id)
        require(run is not None, "no archived run %s" % run_id)
        require(not run.expired, "run %s already lost members to retention; a pin keeps only "
                "what is still archived, so pin it before it expires" % run_id)
        if run.pinned and run.superseded is None:
            return run.pinned[-1]
        require(run.superseded is None, "run %s was superseded; a superseded pin is not "
                "renewed" % run_id)
        return self._event({"kind": "pin", "run_id": run_id, "reason": reason,
                            "note": note[:4096]})

    def supersede(self, run_id, by=None):
        runs = self.state()
        run = runs.get(run_id)
        require(run is not None and run.pinned, "run %s is not a pinned archived run" % run_id)
        require(run.superseded is None, "run %s is already superseded" % run_id)
        require(by is None or by in runs, "no archived run %s supersedes it" % by)
        return self._event({"kind": "supersede", "run_id": run_id, "superseded_by": by})

    def due(self, run, member):
        """When a member's retention ends, or None while it is pinned."""
        due = run.event["unix_ms"] + RETENTION_DAYS[member["class"]] * DAY_MS
        if run.pinned:
            if run.superseded is None:
                return None
            due = max(due, run.superseded["unix_ms"] + PINNED_AFTER_SUPERSEDED_DAYS * DAY_MS)
        return due

    def retain(self, now_unix_ms=None):
        """Apply retention at `now_unix_ms`: an explicit retention entry for every run that
        loses members, then a reference-traced collection of unreferenced objects."""
        now = int(self.wall() * 1000) if now_unix_ms is None else now_unix_ms
        written = []
        for run_id, run in self.state().items():
            expired = dict(run.expired)
            for member in run.available():
                due = self.due(run, member)
                if due is not None and now >= due:
                    expired[member["path"]] = {"path": member["path"],
                                               "sha256": member["sha256"],
                                               "class": member["class"], "due_unix_ms": due}
            if len(expired) == len(run.expired):
                continue
            run.expired = expired
            written.append(self._event({
                "kind": "retention", "run_id": run_id,
                "run_sequence": run.event["sequence"], "manifest": run.members,
                "expired": [expired[path] for path in sorted(expired)],
                "available": sorted(member["path"] for member in run.available()),
                "numeric": run.numeric(), "profile_reproduction": run.profile_reproduction()},
                now))
        referenced = set()
        for run in self.state().values():
            referenced.update(member["sha256"] for member in run.available())
        removed = []
        for digest, size in self.objects():
            if digest not in referenced:
                os.unlink(os.path.join(self.root, self.object_relative(digest)))
                removed.append({"sha256": digest, "size": size})
        if removed:
            written.append(self._event({"kind": "gc", "removed": removed,
                                        "freed_bytes": sum(item["size"] for item in removed)},
                                       now))
        return written

    # Reading back -----------------------------------------------------------------------------

    def read_object(self, member, limit):
        """A member's bytes from its object, rehashed."""
        require(member["size"] <= limit, "%s exceeds %d bytes" % (member["path"], limit))
        relative = self.object_relative(member["sha256"])
        fd, info = admission.open_confined(self.root, relative, "the archive")
        try:
            require(info.st_size == member["size"], "the object of %s has %d bytes, not %d"
                    % (member["path"], info.st_size, member["size"]))
            chunks = []
            digest = admission.read_bounded(fd, info.st_size, None, relative,
                                            "reading " + relative, chunks.append)
        finally:
            os.close(fd)
        require(digest == member["sha256"], "the object of %s does not match its name"
                % member["path"])
        return b"".join(chunks)

    def materialize(self, run_id, target):
        """Rebuild the run's directory at `target` from its objects, every byte rehashed; only
        a run whose bundle members all remain can be rebuilt."""
        run = self.state().get(run_id)
        require(run is not None, "no archived run %s" % run_id)
        bundled = [member for member in run.available()
                   if member["path"] not in (VERDICT_PATH, SUMMARY_PATH)
                   and not member["path"].startswith(VIEWS)]
        require(len(bundled) == len([member for member in run.members
                                     if member["path"] not in (VERDICT_PATH, SUMMARY_PATH)
                                     and not member["path"].startswith(VIEWS)]),
                "run %s lost bundle members to retention; its bundles cannot be rebuilt whole"
                % run_id)
        os.mkdir(target, 0o700)
        for member in bundled:
            data = self.read_object(member, limits.SHARD_BUNDLE_BYTES_MAX)
            _write_new(os.path.join(target, *member["path"].split("/")), data)
        return target

    # Status -------------------------------------------------------------------------------

    def status(self, now_unix_ms=None):
        """The archive's operational status at `now_unix_ms`."""
        now = int(self.wall() * 1000) if now_unix_ms is None else now_unix_ms
        runs = self.state()
        collections = self.collections()
        used = self.usage()
        capacity = self.config["capacity_bytes"]
        available = max(0, capacity - used)
        problems = []
        last = collections[-1] if collections else None
        successes = [item for item in collections
                     if item["status"] in ("imported", "already_archived")]
        lags = [item["ended_unix_ms"] - item["run_ended_unix_ms"] for item in collections
                if item["status"] == "imported" and item["run_ended_unix_ms"] is not None]
        if last is None:
            problems.append({"code": "no_collection", "detail": "no collection was ever "
                             "recorded; continuous collection is not demonstrated"})
        elif last["status"] == "capacity_exhausted":
            problems.append({"code": "capacity_exhausted", "detail": last["detail"]})
        elif last["status"] not in ("imported", "already_archived"):
            problems.append({"code": "collection_failed", "detail": last["detail"]})
        if available * CAPACITY_LOW_RATIO < capacity:
            problems.append({"code": "capacity_low", "detail": "%d of %d bytes remain"
                             % (available, capacity)})
        collected = set()
        for item in collections:
            if item["source"]["kind"] == "actions" and item["status"] in ("imported",
                                                                          "already_archived"):
                for artifact in item["source"]["artifacts"]:
                    if artifact["state"] == "downloaded":
                        collected.add(artifact["id"])
        transport = {}
        for item in collections:
            if item["source"]["kind"] != "actions":
                continue
            for artifact in item["source"]["artifacts"]:
                # Downloaded by a collection that admitted nothing is still not archived.
                if artifact["id"] in collected:
                    continue
                expires = artifact["expires_unix_ms"]
                risk = ("expired" if artifact["state"] == "expired"
                        or (expires is not None and expires <= now)
                        else "unknown" if expires is None
                        else "expiring" if expires - now <= EXPIRY_RISK_MS else None)
                if risk is None:
                    continue
                transport[artifact["id"]] = {
                    "actions_run_id": item["source"]["actions_run_id"],
                    "artifact_id": artifact["id"], "name": artifact["name"],
                    "expires_unix_ms": expires, "state": artifact["state"], "risk": risk}
        for entry in transport.values():
            code = "transport_expired" if entry["risk"] == "expired" else "transport_expiring"
            problems.append({"code": code, "detail": "artifact %d (%s) of Actions run %d was "
                             "not collected; %s" % (entry["artifact_id"], entry["name"],
                                                    entry["actions_run_id"],
                                                    "its evidence is incomplete" if code
                                                    == "transport_expired" else
                                                    "it expires soon")})
        doc = {"schema": contracts.ARCHIVE_STATUS_SCHEMA, "now_unix_ms": now,
               "capacity_bytes": capacity, "used_bytes": used, "available_bytes": available,
               "filesystem_free_bytes": self.free_bytes(self.root), "runs": len(runs),
               "pinned": sum(1 for run in runs.values()
                             if run.pinned and run.superseded is None),
               "objects": len(self.objects()),
               "last_collection": None if last is None else {
                   "sequence": last["sequence"], "status": last["status"],
                   "ended_unix_ms": last["ended_unix_ms"], "detail": last["detail"]},
               "last_successful_collection_unix_ms": (successes[-1]["ended_unix_ms"]
                                                      if successes else None),
               "collection_lag_ms": {"last": lags[-1] if lags else None,
                                     "max": max(lags) if lags else None},
               "transport": sorted(transport.values(), key=lambda entry: entry["artifact_id"]),
               "health": {"status": "failed" if problems else "ok", "problems": problems[:64]}}
        return contracts.validate_archive_status(doc)


def _bytes_writer(data):
    def write(stream):
        stream.write(data)
        return sha256_bytes(data)
    return write


def _file_writer(root, relative, size):
    def write(stream):
        fd, info = admission.open_confined(root, relative, "the run")
        try:
            require(info.st_size == size, "%s changed size after verification" % relative)
            return admission.read_bounded(fd, size, None, relative, "archiving " + relative,
                                          stream.write)
        finally:
            os.close(fd)
    return write


def _write_new(path, data, sync=False):
    os.makedirs(os.path.dirname(path), 0o700, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        if sync:
            stream.flush()
            os.fsync(stream.fileno())


def _sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def is_archive(path):
    return os.path.isfile(os.path.join(path, ARCHIVE_FILE)) and not os.path.islink(path)
