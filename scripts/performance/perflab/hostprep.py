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
import subprocess

from . import limits
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
