"""Bounded, confined admission of files a candidate could have written (ADR-0172).

A host reads what a measured side wrote: a seed its writer published, a binary its build left
in its target directory. None of their paths is trusted. Everything below a trusted root is
reached one name at a time through held directory descriptors, never following a symlink
(`O_NOFOLLOW` at every component), and every opened entry must be the very one enumerated.

`scan` enumerates a whole tree before anything is read or any mode changes: a symlink, a
device, a FIFO or a socket anywhere refuses the tree, and the entry count, the depth, each
file's logical size and their sum must stay within the declared `Bounds`. A directory is read
one entry at a time, and its listing is refused as soon as it names more entries than the
bound still allows and stopped by the deadline between entries, before any listing is kept
whole or sorted (ADR-0173). A sparse file's
logical size is what is bounded, never only its allocated blocks. Every read, hash and copy is
bounded by the declared bytes and by the dominant deadline while it runs (`budget.Budget`):
nothing starts once no time is left, and a deadline that ends a read stops it with the
deadline's refusal (`budget.Expired`). A file that grows or shrinks while it is read is
refused.
"""
import errno
import hashlib
import os
import stat

from .budget import Expired
from .values import ContractError, require

READ_CHUNK = 1 << 20
DIRECTORY = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
FILE = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK


class Bounds(object):
    """The most a tree may hold: entries, nesting depth, one file's and all files' bytes."""

    def __init__(self, entries, depth, file_bytes, total_bytes):
        self.entries = entries
        self.depth = depth
        self.file_bytes = file_bytes
        self.total_bytes = total_bytes


class Entry(object):
    """One enumerated entry: its relative path, kind, mode, logical size and identity."""

    def __init__(self, path, info):
        self.path = path
        self.kind = "directory" if stat.S_ISDIR(info.st_mode) else "file"
        self.mode = stat.S_IMODE(info.st_mode)
        self.size = info.st_size if self.kind == "file" else 0
        self.identity = (info.st_dev, info.st_ino)


def check(budget, step):
    """Refuse continuing `step` once the dominant deadline leaves no time; nothing when no
    deadline binds it."""
    if budget is not None and budget.left() <= 0:
        raise Expired("the run's total deadline ended %s, %d s before the deadline, which is "
                      "kept for cleanup and the terminal record; nothing more of it was done"
                      % (step, budget.reserve))


def _kind(info):
    if stat.S_ISLNK(info.st_mode):
        return "a symlink"
    if stat.S_ISFIFO(info.st_mode):
        return "a FIFO"
    if stat.S_ISSOCK(info.st_mode):
        return "a socket"
    if stat.S_ISCHR(info.st_mode) or stat.S_ISBLK(info.st_mode):
        return "a device"
    return "neither a directory nor a regular file"


def open_root(root, label):
    """A held descriptor of the directory `root` itself, which must not be a symlink."""
    try:
        fd = os.open(root, DIRECTORY)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.ENOTDIR):
            raise ContractError("%s is not a directory (a symlink is never followed)" % label)
        raise
    return fd


def _listing(fd, room=None, refusal=None, budget=None, step=None):
    """The sorted names of the directory held by `fd`, through a new open description of it:
    a held descriptor already enumerated does not reliably see later entries (perflab.oracle).

    The names are read one at a time. With `room`, the listing is refused (`refusal`) at the
    first name beyond that many, so no more than `room` names are ever held or sorted, and the
    deadline (`budget`) is checked before each name is read (ADR-0173)."""
    fresh = os.open(".", DIRECTORY, dir_fd=fd)
    names = []
    try:
        with os.scandir(fresh) as listed:
            while True:
                if room is not None:
                    check(budget, step)
                entry = next(listed, None)
                if entry is None:
                    break
                require(room is None or len(names) < room, refusal)
                names.append(entry.name)
    finally:
        os.close(fresh)
    return sorted(names)


def _opened(fd, info, wanted, what):
    held = os.fstat(fd)
    if (held.st_dev, held.st_ino) != (info.st_dev, info.st_ino) \
            or stat.S_IFMT(held.st_mode) != stat.S_IFMT(wanted):
        raise ContractError("%s changed while it was being admitted" % what)
    return held


def scan(root, bounds, budget=None, label=None, step=None):
    """Every entry below `root`, in sorted path order, after refusing anything unsupported or
    beyond `bounds`; nothing is read and nothing changes."""
    label = label or root
    step = step or "admitting " + label
    entries = []
    totals = {"bytes": 0}
    check(budget, step)
    fd = open_root(root, label)
    try:
        _scan(fd, "", 1, entries, totals, bounds, budget, label, step)
    finally:
        os.close(fd)
    return sorted(entries, key=lambda entry: entry.path)


def _scan(fd, prefix, depth, entries, totals, bounds, budget, label, step):
    too_many = "%s holds more than %d entries" % (label, bounds.entries)
    for name in _listing(fd, bounds.entries - len(entries), too_many, budget, step):
        check(budget, step)
        path = prefix + name
        require(len(entries) < bounds.entries, too_many)
        info = os.stat(name, dir_fd=fd, follow_symlinks=False)
        if stat.S_ISDIR(info.st_mode):
            require(depth < bounds.depth, "%s nests %s deeper than %d levels"
                    % (label, path, bounds.depth))
            entries.append(Entry(path, info))
            child = os.open(name, DIRECTORY, dir_fd=fd)
            try:
                _opened(child, info, stat.S_IFDIR, "%s/%s" % (label, path))
                _scan(child, path + "/", depth + 1, entries, totals, bounds, budget, label,
                      step)
            finally:
                os.close(child)
        elif stat.S_ISREG(info.st_mode):
            require(info.st_size <= bounds.file_bytes, "%s/%s holds %d logical bytes, more "
                    "than %d" % (label, path, info.st_size, bounds.file_bytes))
            totals["bytes"] += info.st_size
            require(totals["bytes"] <= bounds.total_bytes, "%s holds more than %d logical "
                    "bytes" % (label, bounds.total_bytes))
            entries.append(Entry(path, info))
        else:
            raise ContractError("%s holds %s, which is %s" % (label, path, _kind(info)))


def open_entry(root_fd, entry, label):
    """A descriptor of an enumerated entry, reached from `root_fd` one component at a time
    without following a symlink, verified to be that very entry."""
    parts = entry.path.split("/")
    fd = root_fd
    opened = []
    try:
        for part in parts[:-1]:
            fd = os.open(part, DIRECTORY, dir_fd=fd)
            opened.append(fd)
        flags = DIRECTORY if entry.kind == "directory" else FILE
        target = os.open(parts[-1], flags, dir_fd=fd)
    finally:
        for descriptor in opened:
            os.close(descriptor)
    try:
        held = os.fstat(target)
        if (held.st_dev, held.st_ino) != entry.identity:
            raise ContractError("%s/%s changed while it was being admitted"
                                % (label, entry.path))
    except BaseException:
        os.close(target)
        raise
    return target


def read_bounded(fd, size, budget, what, step, sink=None):
    """Read exactly `size` bytes from `fd` in bounded chunks, each started only while the
    deadline leaves time; the SHA256 of what was read. `sink` receives every chunk."""
    digest = hashlib.sha256()
    read = 0
    while True:
        check(budget, step)
        chunk = os.read(fd, min(READ_CHUNK, size - read + 1))
        if not chunk:
            break
        read += len(chunk)
        require(read <= size, "%s grew while it was read" % what)
        digest.update(chunk)
        if sink is not None:
            sink(chunk)
    require(read == size, "%s shrank while it was read" % what)
    return "sha256:" + digest.hexdigest()


def digests(root, entries, budget=None, label=None, step=None):
    """{path: sha256} of every enumerated file below `root`, each opened as its enumerated
    entry and read within its enumerated size."""
    label = label or root
    step = step or "hashing " + label
    found = {}
    root_fd = open_root(root, label)
    try:
        for entry in entries:
            if entry.kind != "file":
                continue
            fd = open_entry(root_fd, entry, label)
            try:
                found[entry.path] = read_bounded(fd, entry.size, budget,
                                                 "%s/%s" % (label, entry.path), step)
            finally:
                os.close(fd)
    finally:
        os.close(root_fd)
    return found


def chmod_entries(root, entries, modes, budget=None, label=None, root_mode=None):
    """Set each enumerated entry's mode (`modes(entry)`) through its own verified descriptor,
    children before their directories, then the root's when `root_mode` is given; nothing is
    followed and nothing outside the tree can change."""
    label = label or root
    step = "sealing " + label
    root_fd = open_root(root, label)
    try:
        for entry in sorted(entries, key=lambda item: item.path, reverse=True):
            check(budget, step)
            fd = open_entry(root_fd, entry, label)
            try:
                os.fchmod(fd, modes(entry))
            finally:
                os.close(fd)
        if root_mode is not None:
            os.fchmod(root_fd, root_mode)
    finally:
        os.close(root_fd)


def open_confined(root, relative, label):
    """(descriptor, fstat) of the regular file at `relative` below the trusted directory
    `root`: every component is opened without following a symlink, so a symlinked file or
    ancestor refuses before anything is read."""
    parts = relative.split("/")
    require(all(part and part not in (".", "..") for part in parts),
            "%s: %s is not a plain relative path" % (label, relative))
    fd = open_root(root, label)
    try:
        for index, part in enumerate(parts[:-1]):
            try:
                child = os.open(part, DIRECTORY, dir_fd=fd)
            except OSError as error:
                if error.errno in (errno.ELOOP, errno.ENOTDIR):
                    raise ContractError("%s: %s is not a directory (a symlink is never "
                                        "followed)" % (label, "/".join(parts[:index + 1])))
                raise
            os.close(fd)
            fd = child
        info = os.stat(parts[-1], dir_fd=fd, follow_symlinks=False)
        if not stat.S_ISREG(info.st_mode):
            raise ContractError("%s: %s is %s" % (label, relative, _kind(info)))
        target = os.open(parts[-1], FILE, dir_fd=fd)
    finally:
        os.close(fd)
    try:
        held = _opened(target, info, stat.S_IFREG, "%s: %s" % (label, relative))
    except BaseException:
        os.close(target)
        raise
    return target, held


COPY_FILE = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW


def copy_tree(source, target, bounds, budget=None, label=None, step=None):
    """Copy the tree below `source` to the new directory `target`. The whole source is enumerated within `bounds` before anything is created
    (`scan`): a symlink, a FIFO, a device or a socket anywhere, more entries, deeper nesting or
    more logical bytes refuse it with nothing copied. Every file is then opened as its
    enumerated entry and read within its enumerated size and the dominant deadline, so a file
    that changed, grew or shrank refuses the copy, and nothing outside `source` is read. The
    copy is private and writable: directories 0700, files 0600 (0700 when executable); every
    entry is created new below a held descriptor of `target` and never follows a symlink.
    Returns (entries, {path: sha256}): what was enumerated and the bytes actually copied."""
    label = label or source
    step = step or "copying " + label
    entries = scan(source, bounds, budget, label, step)
    os.mkdir(target, 0o700)
    found = {}
    source_fd = open_root(source, label)
    try:
        target_fd = open_root(target, target)
        try:
            for entry in entries:
                check(budget, step)
                parent, name = os.path.split(entry.path)
                parent_fd = _walk(target_fd, parent)
                try:
                    if entry.kind == "directory":
                        os.mkdir(name, 0o700, dir_fd=parent_fd)
                        continue
                    found[entry.path] = _copy_file(source_fd, entry, parent_fd, name, budget,
                                                   label, step)
                finally:
                    if parent_fd != target_fd:
                        os.close(parent_fd)
        finally:
            os.close(target_fd)
    finally:
        os.close(source_fd)
    return entries, found


def copy_into(source, target, bounds, budget=None, label=None, step=None):
    """Copy the tree below `source` into the existing directory `target`, as `copy_tree` does:
    the whole source enumerated within `bounds` first, each file read as its enumerated entry.
    A directory already in `target` is entered without following a symlink; a file never
    replaces one. Returns (entries, {path: sha256})."""
    label = label or source
    step = step or "copying " + label
    entries = scan(source, bounds, budget, label, step)
    found = {}
    source_fd = open_root(source, label)
    try:
        target_fd = open_root(target, target)
        try:
            for entry in entries:
                check(budget, step)
                parent, name = os.path.split(entry.path)
                parent_fd = _walk(target_fd, parent)
                try:
                    if entry.kind == "directory":
                        try:
                            os.mkdir(name, 0o700, dir_fd=parent_fd)
                        except FileExistsError:
                            os.close(os.open(name, DIRECTORY, dir_fd=parent_fd))
                        continue
                    found[entry.path] = _copy_file(source_fd, entry, parent_fd, name, budget,
                                                   label, step)
                finally:
                    if parent_fd != target_fd:
                        os.close(parent_fd)
        finally:
            os.close(target_fd)
    finally:
        os.close(source_fd)
    return entries, found


def copy_fd(fd, size, target, budget=None, what=None):
    """Copy exactly `size` bytes of the held descriptor `fd` to the new file `target`, within
    the deadline; its SHA-256."""
    writing = os.open(target, COPY_FILE, 0o600)
    try:
        def sink(chunk):
            view = memoryview(chunk)
            while view:
                view = view[os.write(writing, view):]

        return read_bounded(fd, size, budget, what or target, "copying " + (what or target),
                            sink=sink)
    finally:
        os.close(writing)


def _walk(root_fd, relative):
    """A descriptor of the directory `relative` below `root_fd`, opened without following a
    symlink; `root_fd` itself for the empty path."""
    fd = root_fd
    for part in (relative.split("/") if relative else ()):
        child = os.open(part, DIRECTORY, dir_fd=fd)
        if fd != root_fd:
            os.close(fd)
        fd = child
    return fd


def _copy_file(source_fd, entry, parent_fd, name, budget, label, step):
    reading = open_entry(source_fd, entry, label)
    try:
        mode = 0o700 if entry.mode & 0o100 else 0o600
        writing = os.open(name, COPY_FILE, mode, dir_fd=parent_fd)
        try:
            def sink(chunk):
                view = memoryview(chunk)
                while view:
                    view = view[os.write(writing, view):]

            return read_bounded(reading, entry.size, budget, "%s/%s" % (label, entry.path),
                                step, sink=sink)
        finally:
            os.close(writing)
    finally:
        os.close(reading)


def remove_tree(root):
    """Remove the directory `root` and everything below it through held descriptors; the
    problems, empty when it is gone. A symlink is unlinked, never followed, so nothing outside
    the tree is read, changed or removed."""
    if not os.path.lexists(root):
        return []
    problems = []
    info = os.lstat(root)
    if not stat.S_ISDIR(info.st_mode):
        try:
            os.unlink(root)
        except OSError as error:
            problems.append("%s: %s" % (root, error.strerror or error))
        return problems
    parent = os.path.dirname(os.path.abspath(root)) or "/"
    name = os.path.basename(os.path.abspath(root))
    try:
        parent_fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY)
    except OSError as error:
        return ["%s: %s" % (parent, error.strerror or error)]
    try:
        _remove_child(parent_fd, name, root, problems)
    finally:
        os.close(parent_fd)
    if os.path.lexists(root) and not problems:
        problems.append("%s still exists" % root)
    return problems


def _open_owned_directory(parent_fd, name, info):
    """A held descriptor of the directory `name`, made traversable by its owner first when a
    sealed or hostile mode refuses opening it; it must still be the directory enumerated."""
    try:
        fd = os.open(name, DIRECTORY, dir_fd=parent_fd)
    except PermissionError:
        again = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
        require(stat.S_ISDIR(again.st_mode)
                and (again.st_dev, again.st_ino) == (info.st_dev, info.st_ino),
                "%s changed while it was removed" % name)
        os.chmod(name, stat.S_IRWXU, dir_fd=parent_fd)
        fd = os.open(name, DIRECTORY, dir_fd=parent_fd)
    _opened(fd, info, stat.S_IFDIR, name)
    os.fchmod(fd, stat.S_IRWXU)
    return fd


def _remove_child(parent_fd, name, shown, problems):
    try:
        info = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(info.st_mode):
        try:
            os.unlink(name, dir_fd=parent_fd)
        except OSError as error:
            problems.append("%s: %s" % (shown, error.strerror or error))
        return
    try:
        fd = _open_owned_directory(parent_fd, name, info)
    except (OSError, ContractError) as error:
        problems.append("%s: %s" % (shown, getattr(error, "strerror", None) or error))
        return
    try:
        for child in _listing(fd):
            _remove_child(fd, child, shown + "/" + child, problems)
    finally:
        os.close(fd)
    try:
        os.rmdir(name, dir_fd=parent_fd)
    except OSError as error:
        problems.append("%s: %s" % (shown, error.strerror or error))
