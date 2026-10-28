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
import threading

from .budget import defer_alarm

MACHINE_LOCK_DIRECTORY = "/tmp"
MACHINE_LOCK_NAME = "af-perf-machine.lock"
LOCK_MODE = 0o644
MACHINE_LOCK_ANCHOR = "/etc/af-perf-machine-lock.json"


class Refused(Exception):
    """The machine lock is not ours: `reason` is `concurrent_shard` when another process holds
    it and `coordination_unavailable` when it cannot be taken safely at all."""

    def __init__(self, reason, detail):
        Exception.__init__(self, detail)
        self.reason = reason


def default_path():
    """The machine lock's path; the same for every process on this machine."""
    return os.path.join(os.path.realpath(MACHINE_LOCK_DIRECTORY), MACHINE_LOCK_NAME)


def read_anchor(required=False):
    """Root-owned inode authority shared even with a service's private temporary namespace.

    Independent VM installation requires it. Once installed it constrains every legacy
    entry point too. It is never inferred from an environment variable or rewritten by us.
    """
    from . import contracts
    fd = None
    try:
        with defer_alarm():
            fd = os.open(MACHINE_LOCK_ANCHOR, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_nlink != 1
                or info.st_mode & 0o022 or info.st_size > 4096):
            raise Refused("coordination_unavailable", "unsafe root-owned machine lock anchor")
        data = os.read(fd, 4097)
        doc = contracts.read_json(data, "machine-lock-anchor")
        return [doc["device"], doc["inode"]]
    except FileNotFoundError:
        if required:
            raise Refused("coordination_unavailable", "independent compute requires root-owned "
                          + MACHINE_LOCK_ANCHOR)
        return None
    except (OSError, contracts.ContractError) as error:
        raise Refused("coordination_unavailable", "machine lock anchor refused: %s" % error) from error
    finally:
        with defer_alarm():
            if fd is not None:
                os.close(fd)


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

    def __init__(self, directory=None, register=None, expected_identity=None):
        self.fd = None
        self.guarded = False
        self.uncertain = False
        self.pid = os.getpid()
        self.thread = threading.get_ident()
        # Register even an interrupted constructor before acquisition can start.
        if register is not None:
            register(self)
        system_owned = directory is None
        self.expected_identity = (read_anchor() if system_owned else expected_identity)
        directory = os.path.realpath(directory or MACHINE_LOCK_DIRECTORY)
        _check_directory(directory, system_owned)
        self.path = os.path.join(directory, MACHINE_LOCK_NAME)
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
        if self.expected_identity is None:
            flags |= os.O_CREAT
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
            self.fence = self.path + ".fence"
            if os.path.lexists(self.fence):
                raise Refused("coordination_unavailable", "unfinished experiment fence %s; "
                              "operator cleanup is required before any new experiment" % self.fence)
        except BaseException:
            self.release()
            raise

    def _check_file(self):
        info = os.fstat(self.fd)
        if self.expected_identity is not None and [info.st_dev, info.st_ino] != self.expected_identity:
            raise Refused("coordination_unavailable", "machine lock inode differs from the root-owned "
                          "anchor; private temporary namespaces cannot create another slot")
        if not stat.S_ISREG(info.st_mode):
            raise Refused("coordination_unavailable", "%s is not a regular file" % self.path)
        if info.st_nlink != 1:
            raise Refused("coordination_unavailable", "%s has %d links, not one"
                          % (self.path, info.st_nlink))
        mode = stat.S_IMODE(info.st_mode)
        if mode & (stat.S_IWGRP | stat.S_IWOTH):
            raise Refused("coordination_unavailable", "%s is writable by others (mode %o)"
                          % (self.path, mode))
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

    def check(self):
        """An in-process capability, never an environment variable or inherited FD number."""
        if (self.fd is None or self.pid != os.getpid()
                or self.thread != threading.get_ident()):
            raise Refused("coordination_unavailable", "machine lease is not held by this caller")
        self._check_file()
        named, held = os.lstat(self.path), os.fstat(self.fd)
        if (named.st_dev, named.st_ino) != (held.st_dev, held.st_ino):
            raise Refused("coordination_unavailable", "machine lease inode changed")

    def guard(self):
        """Fence shared work durably before launch. A crash leaves exclusion behind.

        The sticky system directory protects this directory across users. Only its creator
        can settle it; no acquisition, release, timeout or recovery automatically removes it.
        """
        self.check()
        if not self.guarded:
            os.mkdir(self.fence, 0o700)
            self.guarded = True
            with defer_alarm():
                fd = os.open(os.path.dirname(self.path), os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                with defer_alarm():
                    os.close(fd)

    def confirm_clean(self):
        """Called only after owned descendants, shared resources and terminal writes settle."""
        self.check()
        if self.uncertain:
            raise Refused("coordination_unavailable", "owned cleanup is uncertain; retain machine fence")
        if self.guarded:
            os.rmdir(self.fence)
            self.guarded = False
            with defer_alarm():
                fd = os.open(os.path.dirname(self.path), os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                with defer_alarm():
                    os.close(fd)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.release()
        return False


class Borrowed:
    """A checked explicit borrow; closing it cannot release the owner's exclusion."""

    def __init__(self, owner):
        if type(owner) is not MachineLock:
            raise Refused("coordination_unavailable", "invalid machine lease capability")
        owner.check()
        self.owner, self.path = owner, owner.path

    def release(self):
        self.owner.check()

    def mark_uncertain(self):
        self.owner.uncertain = True


def acquire(directory=None, register=None, lease=None):
    if lease is None:
        return MachineLock(directory, register)
    borrowed = Borrowed(lease)
    if register is not None:
        register(borrowed)
    return borrowed
