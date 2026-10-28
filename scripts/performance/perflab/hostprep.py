"""Prepare a hosted Linux runner's disk-bounded sample scratch (ADR-0177).

A shard's samples need a dedicated scratch filesystem whose 2-GiB bound the runner verifies by
an enforcement fill before anything runs (`capability.scratch_capability`). A standard hosted
Linux VM offers no such filesystem, but it has passwordless sudo. This trusted preparation step,
which runs before any candidate byte is executed, creates a task-owned ext4 image file of exactly
the environment's scratch bound, mounts it through a loop device with `nodev,nosuid` and gives it
to the runner's user. It never touches a sample container: no capability, privilege or kernel
setting is granted to a benchmark, and nothing escalates automatically.

It refuses, creating nothing, on a host that is not Linux, on a sudo that asks for a password,
or when the free disk would fall below the 10-GiB floor plus the image; the floor is never
lowered. `cleanup` unmounts and removes exactly what `prepare` created, and reports what it
could not. That this works on a given hosted image is operational evidence of a real run; the
deterministic tests drive these steps with a recorded stand-in runner.
"""
import os
import fcntl
import secrets
import stat
import subprocess

from . import contracts, limits
from .budget import defer_alarm
from .values import ContractError, require

STEP_SECONDS = 120


class Refused(ContractError):
    def __init__(self, reason, detail):
        ContractError.__init__(self, detail)
        self.reason = reason


def _run(argv, timeout=STEP_SECONDS):
    result = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=timeout,
                            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C"})
    return result.returncode, result.stderr.decode("utf-8", "replace")[-1024:]


class Host(object):
    """The host operations, replaced by a recorded stand-in in tests."""

    def __init__(self, run=_run, statvfs=os.statvfs, system=None, uid=None, gid=None):
        self.run = run
        self.statvfs = statvfs
        self.system = system or os.uname().sysname.lower()
        self.uid = os.getuid() if uid is None else uid
        self.gid = os.getgid() if gid is None else gid


def free_bytes(host, path):
    info = host.statvfs(path)
    return info.f_bavail * info.f_frsize


def prepare(image, mount, size_bytes, host=None, floor_bytes=limits.FREE_DISK_FLOOR_BYTES):
    """Create, mount and hand over the scratch filesystem; its record. Refused with a reason,
    creating nothing, when this host cannot."""
    host = host or Host()
    require(isinstance(size_bytes, int) and 0 < size_bytes <= 64 * limits.GIB,
            "the scratch size is a positive byte count")
    require(floor_bytes >= limits.FREE_DISK_FLOOR_BYTES,
            "the free-disk floor is never lowered below %d bytes" % limits.FREE_DISK_FLOOR_BYTES)
    image, mount = os.path.abspath(image), os.path.abspath(mount)
    if host.system != "linux":
        raise Refused("not_linux", "a loop-mounted scratch filesystem is prepared only on a "
                      "Linux runner; this host is %s" % host.system)
    require(not os.path.lexists(image), "%s already exists; the scratch image is created new"
            % image)
    require(not os.path.lexists(mount), "%s already exists; the mount point is created new"
            % mount)
    free = free_bytes(host, os.path.dirname(image))
    if free < floor_bytes + size_bytes:
        raise Refused("insufficient_disk", "%d bytes are free beside %s; the scratch needs %d "
                      "above the %d-byte floor, which is never lowered"
                      % (free, image, size_bytes, floor_bytes))
    code, err = host.run(["sudo", "-n", "true"])
    if code != 0:
        raise Refused("no_passwordless_sudo", "this runner's sudo needs a password or is "
                      "absent: %s" % err.strip())
    record = {"image": image, "mount": mount, "size_bytes": size_bytes, "free_bytes": free,
              "steps": []}
    created = []
    try:
        steps = [["truncate", "-s", str(size_bytes), image],
                 ["mkfs.ext4", "-q", "-F", "-m", "0", image],
                 ["mkdir", "-m", "0700", mount],
                 ["sudo", "-n", "mount", "-o", "loop,nodev,nosuid", image, mount],
                 ["sudo", "-n", "chown", "%d:%d" % (host.uid, host.gid), mount],
                 ["sudo", "-n", "chmod", "0700", mount]]
        for argv in steps:
            code, err = host.run(argv)
            record["steps"].append({"argv": argv, "code": code})
            if code != 0:
                raise Refused("preparation_failed", "%s exited %d: %s"
                              % (" ".join(argv), code, err.strip()))
            created.append(argv[0] if argv[0] != "sudo" else argv[2])
    except BaseException:
        cleanup(image, mount, host, mounted="mount" in created)
        raise
    return record


def cleanup(image, mount, host=None, mounted=True):
    """Unmount and remove exactly what `prepare` created; the problems found, empty only when
    everything is gone."""
    host = host or Host()
    problems = []
    if mounted and os.path.lexists(mount):
        code, err = host.run(["sudo", "-n", "umount", mount])
        if code != 0:
            problems.append("umount %s exited %d: %s" % (mount, code, err.strip()))
    if not problems:
        for argv in (["rmdir", mount], ["rm", "-f", image]):
            target = argv[-1]
            if os.path.lexists(target):
                code, err = host.run(argv)
                if code != 0:
                    problems.append("%s exited %d: %s" % (" ".join(argv), code, err.strip()))
    return problems


class TelemetryOwner:
    """Trusted, bounded work and launch fence for one telemetry filesystem, outside candidate
    mounts. The workflow creates it after preparation, holds its nonblocking lock throughout
    measurement, and reacquires it for cleanup. A pending launch survives loss of the controller
    or result publication. Only the executor's confirmed removal can settle that launch.

    This is cleanup authority, never a candidate result or a claim that unmount killed work.
    """

    def __init__(self, path, mount, create=False, image=None):
        self.path = os.path.abspath(path)
        self.mount = os.path.abspath(mount)
        self.create = create
        self.image = os.path.abspath(image) if image else None
        self.directory = self.lock = None
        self.work_pending = False

    @staticmethod
    def identity(info):
        return [info.st_dev, info.st_ino]

    def __enter__(self):
        require(os.path.realpath(self.path) == self.path,
                "scratch ownership directory must contain no symlinks")
        require(not self.path.startswith(self.mount + os.sep) and self.path != self.mount,
                "scratch ownership must be outside candidate-writable scratch")
        if self.create:
            os.mkdir(self.path, 0o700)
        try:
            with defer_alarm():
                self.directory = os.open(self.path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            info = os.fstat(self.directory)
            require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700,
                    "scratch ownership directory must be private and owned by this user")
            with defer_alarm():
                self.lock = os.open("lock", os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK |
                                    (os.O_CREAT | os.O_EXCL if self.create else 0),
                                    0o600, dir_fd=self.directory)
            self._private(os.fstat(self.lock))
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            if self.create:
                image = os.lstat(self.image)
                mount = os.lstat(self.mount)
                require(stat.S_ISREG(image.st_mode) and stat.S_ISDIR(mount.st_mode),
                        "scratch ownership needs the actual image and mount")
                self.doc = {"schema": contracts.TELEMETRY_OWNER_SCHEMA,
                            "owner_uid": os.getuid(),
                            "control": self.identity(info),
                            "lock": self.identity(os.fstat(self.lock)),
                            "image": self.image, "mount": self.mount,
                            "image_identity": self.identity(image), "image_bytes": image.st_size,
                            "mount_identity": self.identity(mount), "phase": "ready",
                            "container": None, "launches": 0, "filesystem_pending": False}
                self._write()
            self._read()
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    @staticmethod
    def _private(info):
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
                and stat.S_IMODE(info.st_mode) == 0o600 and info.st_nlink == 1,
                "scratch ownership file must be private, regular and singly linked")

    def _read(self):
        fd = None
        try:
            with defer_alarm():
                fd = os.open("state.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=self.directory)
            self._private(os.fstat(fd))
            data = os.read(fd, 8193)
            require(len(data) <= 8192, "scratch ownership exceeds its byte bound")
            self.doc = contracts.read_json(data, "telemetry-owner", self.path)
        finally:
            with defer_alarm():
                if fd is not None:
                    os.close(fd)
        doc = self.doc
        require(doc["control"] == self.identity(os.fstat(self.directory))
                and doc["control"] == self.identity(os.lstat(self.path))
                and doc["lock"] == self.identity(os.fstat(self.lock))
                and doc["lock"] == self.identity(os.stat("lock", dir_fd=self.directory,
                                                       follow_symlinks=False))
                and doc["owner_uid"] == os.getuid() and doc["mount"] == self.mount,
                "foreign scratch ownership evidence")
        require(doc["phase"] != "removed", "scratch ownership is already retired")
        image, mount = os.lstat(doc["image"]), os.lstat(doc["mount"])
        require(os.path.realpath(doc["image"]) == doc["image"]
                and os.path.realpath(doc["mount"]) == doc["mount"]
                and stat.S_ISREG(image.st_mode) and stat.S_ISDIR(mount.st_mode)
                and doc["image_identity"] == self.identity(image)
                and doc["image_bytes"] == image.st_size
                and doc["mount_identity"] == self.identity(mount),
                "scratch image or filesystem identity changed; retain recovery paths")
        return doc

    def _write(self):
        data = contracts.write_bytes(self.doc, "telemetry-owner")
        require(len(data) <= 8192, "scratch ownership exceeds its byte bound")
        name = ".state-" + secrets.token_hex(8)
        # A failed/interrupted publication leaves its private temporary file for recovery.
        # In particular, do not start unlink I/O after a deadline signal has fired.
        fd = None
        try:
            with defer_alarm():
                fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             0o600, dir_fd=self.directory)
            require(os.write(fd, data) == len(data), "short scratch ownership write")
            os.fsync(fd)
            os.replace(name, "state.json", src_dir_fd=self.directory,
                       dst_dir_fd=self.directory)
            # A failure from here leaves the new record in place, only its durability unproven:
            # every transition is written once it holds, and a lost one returns the earlier,
            # fenced record (issue #388, docs/performance-telemetry.md).
            os.fsync(self.directory)
        finally:
            with defer_alarm():
                if fd is not None:
                    os.close(fd)

    def begin(self, container):
        doc = self.available()
        doc.update(phase="active", container=container, launches=doc["launches"] + 1)
        self._write()  # Durable before Popen can contact the daemon.

    def available(self):
        doc = self._read()
        require(doc["phase"] in ("ready", "stopped")
                and (not doc["filesystem_pending"] or self.work_pending),
                "scratch has an unsettled launch; retain it and run nothing more")
        return doc

    def prepare_work(self):
        """Fence the controller's entire work lifetime before setup or candidate work.

        Later stopped containers or failed uncertainty writes cannot authorize workflow
        cleanup. Only this live controller admits more executions while the fence is set.
        """
        doc = self.available()
        require(not doc["filesystem_pending"], "scratch work is already pending")
        doc["filesystem_pending"] = True
        self._write()
        self.work_pending = True

    def work_cleaned(self):
        doc = self.available()
        require(self.work_pending and doc["filesystem_pending"], "no pending scratch work")
        doc["filesystem_pending"] = False
        self._write()
        self.work_pending = False

    def finish(self, container, confirmed):
        doc = self._read()
        require(doc["phase"] == "active" and doc["container"] == container,
                "scratch launch settlement names another execution")
        doc["phase"] = "stopped" if confirmed else "uncertain"
        self._write()

    def uncertain(self):
        """A stopped container does not prove deletion of its owned filesystem."""
        doc = self._read()
        if doc["launches"]:
            if doc["phase"] != "uncertain":
                doc["phase"] = "uncertain"
                self._write()
        else:
            # Pre-launch state is safe for ci-scratch cleanup; no hostile writer ran.
            require(doc["phase"] == "ready", "unexpected pre-launch ownership")

    def filesystem_cleaned(self):
        """Settle only the controller's completed filesystem-deletion fence."""
        doc = self._read()
        require(doc["phase"] == "uncertain" and doc["launches"] > 0,
                "filesystem cleanup settlement has no fence")
        doc["phase"] = "stopped"
        self._write()

    def cleanup(self, image, host=None):
        doc = self._read()
        require(os.path.abspath(image) == doc["image"]
                and doc["phase"] in ("ready", "stopped") and not doc["filesystem_pending"],
                "scratch cleanup unconfirmed; retain image, mount and ownership evidence")
        problems = cleanup(doc["image"], doc["mount"], host)
        if not problems:
            doc.update(phase="removed", container=None)
            self._write()
        return problems

    def __exit__(self, *unused):
        with defer_alarm():
            for name in ("lock", "directory"):
                fd = getattr(self, name)
                setattr(self, name, None)
                if fd is not None:
                    os.close(fd)
