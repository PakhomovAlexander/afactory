"""Observe actual allocation at every history write/mkdir/rename, including partial I/O."""
import builtins
import os
import stat
from contextlib import contextmanager, ExitStack
from unittest import mock


@contextmanager
def observe(test, root, capacity):
    # Keep lstat observations of unchanged entries, refreshing every mutated path and its
    # ancestors. This observes the actual filesystem without walking thousands of unchanged
    # authority files again for each single-file write. Reconcile the whole root on exit.
    samples, descriptors, blocks = [], {}, {}
    root = os.path.abspath(root)
    original_open, original_fdopen = builtins.open, os.fdopen
    original_osopen, original_mkdir = os.open, os.mkdir
    original_rename, original_replace = os.rename, os.replace
    original_unlink, original_rmdir, original_close = os.unlink, os.rmdir, os.close

    def absolute(path, dir_fd=None):
        if isinstance(path, int): return descriptors.get(path)
        if dir_fd is not None:
            parent = descriptors.get(dir_fd)
            if parent is None: return None
            path = os.path.join(parent, path)
        return os.path.abspath(path)

    def inside(path):
        return path is not None and os.path.commonpath([path, root]) == root

    def refresh(path):
        if not inside(path): return
        while inside(path):
            try:
                info = os.lstat(path)
                blocks[path] = (info.st_blocks * 512 if stat.S_ISDIR(info.st_mode)
                                else info.st_blocks * 512 or info.st_size)
            except FileNotFoundError:
                blocks.pop(path, None)
            if path == root: break
            path = os.path.dirname(path)

    def check(*paths):
        for path in paths: refresh(path)
        actual = sum(blocks.values())
        samples.append(actual)
        test.assertLessEqual(actual, capacity(), "allocated peak exceeded bytes_max")

    def scan():
        blocks.clear()
        if os.path.isdir(root):
            refresh(root)
            for current, dirs, files in os.walk(root):
                for name in dirs + files: refresh(os.path.join(current, name))

    class Writer:
        def __init__(self, stream, path): self.stream, self.path = stream, path
        def __enter__(self): return self
        def __exit__(self, *args):
            try: return self.stream.__exit__(*args)
            finally: check(self.path)
        def write(self, data):
            try: return self.stream.write(data)
            finally:
                self.stream.flush()
                check(self.path)
        def __getattr__(self, name): return getattr(self.stream, name)

    def opened(path, mode="r", *args, **kwargs):
        stream = original_open(path, mode, *args, **kwargs)
        full = absolute(path)
        if inside(full) and any(flag in mode for flag in "xwa+"):
            check(full)
            return Writer(stream, full)
        return stream

    def osopened(path, flags, *args, **kwargs):
        full = absolute(path, kwargs.get("dir_fd"))
        fd = original_osopen(path, flags, *args, **kwargs)
        descriptors[fd] = full
        if inside(full) and flags & (os.O_WRONLY | os.O_RDWR): check(full)
        return fd

    def closed(fd):
        descriptors.pop(fd, None)
        return original_close(fd)

    def fdopened(fd, *args, **kwargs):
        stream = original_fdopen(fd, *args, **kwargs)
        path = descriptors.pop(fd, None)
        return Writer(stream, path) if inside(path) else stream

    def made(path, *args, **kwargs):
        full = absolute(path, kwargs.get("dir_fd"))
        value = original_mkdir(path, *args, **kwargs)
        if inside(full): check(full)
        return value

    def moved(operation, source, target, *args, **kwargs):
        old, new = absolute(source, kwargs.get("src_dir_fd")), absolute(target, kwargs.get("dst_dir_fd"))
        value = operation(source, target, *args, **kwargs)
        if inside(old) and inside(new):
            for path in list(blocks):
                if path == old or path.startswith(old + os.sep):
                    blocks[new + path[len(old):]] = blocks.pop(path)
        if inside(old) or inside(new): check(old, new)
        return value

    def removed(operation, path, *args, **kwargs):
        full = absolute(path, kwargs.get("dir_fd"))
        value = operation(path, *args, **kwargs)
        if inside(full): check(full)
        return value

    scan()
    with ExitStack() as stack:
        for owner, name, function in ((builtins, "open", opened), (os, "open", osopened),
                                      (os, "fdopen", fdopened), (os, "mkdir", made),
                                      (os, "close", closed),
                                      (os, "unlink", lambda *a, **k: removed(original_unlink, *a, **k)),
                                      (os, "rmdir", lambda *a, **k: removed(original_rmdir, *a, **k)),
                                      (os, "rename", lambda *a, **k: moved(original_rename, *a, **k)),
                                      (os, "replace", lambda *a, **k: moved(original_replace, *a, **k))):
            stack.enter_context(mock.patch.object(owner, name, function))
        check()
        yield samples
        counted = sum(blocks.values())
        scan()
        test.assertEqual(counted, sum(blocks.values()), "mutation observer must reconcile")
        check()
