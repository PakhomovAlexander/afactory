"""The machine's one performance coordination lock (ADR-0169).

Shards, scratch enforcement fills and every other use of a machine's shared measurement
resources take one advisory `flock` on one fixed file, `/tmp/af-perf-machine.lock`, resolved
once through the system's own `/tmp` (a root-owned symlink to `/private/tmp` on macOS). The path
never depends on TMPDIR, HOME, XDG, the checkout or the run directory, so two processes with
different private roots still meet at the same lock. It is held from before anything shared is
touched (a scratch preflight, the run directory, a container) until the terminal evidence is
written, and released on every exit; the kernel releases it when the process dies.

The policy is explicit: one lock per machine, for every user. The directory must be root-owned
and, when anyone may write it, sticky, so only the lock file's owner can remove or rename it.
The file is created `0644` by whoever runs first and opened read-only by everyone, which is all
`flock` needs; it is never written, truncated, removed or replaced, so no process ever deletes a
live lock's inode to take another. Anything that does not hold (a symlink, a non-regular file, a
second link, a group- or world-writable file, a directory that is not the system's, a path that
no longer names the locked inode) refuses as `coordination_unavailable`; another holder refuses
as `concurrent_shard`. Neither ever runs concurrently. A different machine has its own lock:
independent concurrent shards need separate hosts.
"""
import errno
import fcntl
import os
import stat

from .budget import defer_alarm

MACHINE_LOCK_DIRECTORY = "/tmp"
MACHINE_LOCK_NAME = "af-perf-machine.lock"
LOCK_MODE = 0o644


class Refused(Exception):
    """The machine lock is not ours: `reason` is `concurrent_shard` when another process holds
    it and `coordination_unavailable` when it cannot be taken safely at all."""

    def __init__(self, reason, detail):
        Exception.__init__(self, detail)
        self.reason = reason


def default_path():
    """The machine lock's path; the same for every process on this machine."""
    return os.path.join(os.path.realpath(MACHINE_LOCK_DIRECTORY), MACHINE_LOCK_NAME)


def _check_directory(directory, system_owned):
    try:
        info = os.lstat(directory)
    except OSError as error:
        raise Refused("coordination_unavailable", "the lock directory %s: %s"
                      % (directory, error.strerror or error))
    if not stat.S_ISDIR(info.st_mode):
        raise Refused("coordination_unavailable", "%s is not a directory" % directory)
    mode = stat.S_IMODE(info.st_mode)
    if system_owned and info.st_uid != 0:
        raise Refused("coordination_unavailable", "the lock directory %s is owned by uid %d, "
                      "not the system" % (directory, info.st_uid))
    if mode & stat.S_IWOTH and not mode & stat.S_ISVTX:
        raise Refused("coordination_unavailable", "the lock directory %s is world-writable "
                      "without the sticky bit, so anyone could replace the lock" % directory)


class MachineLock(object):
    """The held machine lock. `directory` is a seam for the laboratory's own tests; production
    always uses the system's `/tmp`, which must be root-owned."""

    def __init__(self, directory=None, register=None):
        self.fd = None
        # Register even an interrupted constructor before acquisition can start.
        if register is not None:
            register(self)
        system_owned = directory is None
        directory = os.path.realpath(directory or MACHINE_LOCK_DIRECTORY)
        _check_directory(directory, system_owned)
        self.path = os.path.join(directory, MACHINE_LOCK_NAME)
        flags = os.O_RDONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK
        try:
            try:
                with defer_alarm():
                    self.fd = os.open(self.path, flags, LOCK_MODE)
            except OSError as error:
                raise Refused("coordination_unavailable", "cannot open the machine lock %s: %s"
                              % (self.path, error.strerror or error))
            self._check_file()
            try:
                fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as error:
                if error.errno in (errno.EWOULDBLOCK, errno.EAGAIN):
                    raise Refused("concurrent_shard", "another process holds the machine's "
                                  "performance lock %s; independent concurrent shards require "
                                  "separate hosts" % self.path)
                raise Refused("coordination_unavailable", "cannot lock %s: %s"
                              % (self.path, error.strerror or error))
            # The name must still be the inode we hold: a lock taken on a replaced or removed
            # file would let another process take the new one at the same time.
            try:
                named = os.lstat(self.path)
            except OSError as error:
                raise Refused("coordination_unavailable", "the machine lock %s vanished while "
                              "it was taken: %s" % (self.path, error.strerror or error))
            held = os.fstat(self.fd)
            if (named.st_dev, named.st_ino) != (held.st_dev, held.st_ino):
                raise Refused("coordination_unavailable", "%s no longer names the locked file"
                              % self.path)
            self.owner_uid = held.st_uid
        except BaseException:
            self.release()
            raise

    def _check_file(self):
        info = os.fstat(self.fd)
        if not stat.S_ISREG(info.st_mode):
            raise Refused("coordination_unavailable", "%s is not a regular file" % self.path)
        if info.st_nlink != 1:
            raise Refused("coordination_unavailable", "%s has %d links, not one"
                          % (self.path, info.st_nlink))
        mode = stat.S_IMODE(info.st_mode)
        if info.st_uid == os.geteuid() and mode != LOCK_MODE:
            # A creator's umask may have narrowed it; every user must be able to open it.
            os.fchmod(self.fd, LOCK_MODE)
            mode = LOCK_MODE
        if mode & (stat.S_IWGRP | stat.S_IWOTH):
            raise Refused("coordination_unavailable", "%s is writable by others (mode %o)"
                          % (self.path, mode))

    def release(self):
        """Release the lock; the file stays for the next holder."""
        with defer_alarm():
            if self.fd is not None:
                # Closing our independent description releases its flock too.
                fd, self.fd = self.fd, None
                os.close(fd)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.release()
        return False
