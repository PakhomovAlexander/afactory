"""Bounded, confined admission of candidate-written files (ADR-0172).

Real owned trees in each test's own directory: symlinks to owned marker files and directories
outside the tree, sparse files whose logical size exceeds a bound without allocating or reading
it, FIFOs, entry and depth bounds, and controlled clocks for the dominant deadline. Nothing
reads a real private path and nothing large is read or copied.
"""
import hashlib
import json
import os
import stat
import subprocess
import sys
from unittest import mock

from perflab import admission, budget, limits, reference, seeds
from perflab.values import ContractError, canonical_bytes
from tests.support import TempDir, read

GIB = 1 << 30


def legacy_identity(root):
    """The seed identity as ADR-0169 defined it, computed independently: every entry's
    relative path, kind, mode and, for a file, its size and bytes, in sorted path order."""
    entries = []
    for current, dirs, files in os.walk(root):
        for name in dirs + files:
            path = os.path.join(current, name)
            info = os.lstat(path)
            relative = os.path.relpath(path, root).replace(os.sep, "/")
            if stat.S_ISDIR(info.st_mode):
                entries.append({"path": relative, "kind": "directory",
                                "mode": stat.S_IMODE(info.st_mode)})
            else:
                entries.append({"path": relative, "kind": "file",
                                "mode": stat.S_IMODE(info.st_mode), "size": info.st_size,
                                "sha256": "sha256:" + hashlib.sha256(read(path)).hexdigest()})
    entries.sort(key=lambda entry: entry["path"])
    return reference.content_id(canonical_bytes(entries))


class Clock(object):
    """A controlled monotonic clock; `tick` seconds pass at every read of the clock."""

    def __init__(self, tick=0.0):
        self.now = 0.0
        self.tick = tick

    def __call__(self):
        value = self.now
        self.now += self.tick
        return value


class Seeds(TempDir):
    def seed(self, name="seed", files=3):
        """A writer's seed, with the modes the tests assert set explicitly rather than taken
        from the process umask: a host whose umask is 0077 creates 0700 directories."""
        root = self.path(name)
        os.makedirs(os.path.join(root, "probe-work", "nested"))
        for directory in (root, os.path.join(root, "probe-work"),
                          os.path.join(root, "probe-work", "nested")):
            os.chmod(directory, 0o755)
        for index in range(files):
            file = os.path.join(root, "probe-work", "nested", "f%d" % index)
            with open(file, "wb") as out:
                out.write(b"entry %d\n" % index)
            os.chmod(file, 0o644)
        tool = os.path.join(root, "probe-work", "tool")
        with open(tool, "wb") as out:
            out.write(b"#!/bin/sh\n")
        os.chmod(tool, 0o755)
        return root

    def outside(self):
        marker = self.path("outside-marker")
        with open(marker, "wb") as out:
            out.write(b"owned bytes outside the seed\n")
        os.chmod(marker, 0o600)
        directory = self.path("outside-dir")
        os.mkdir(directory, 0o700)
        inner = os.path.join(directory, "inner")
        with open(inner, "wb") as out:
            out.write(b"owned bytes in an outside directory\n")
        os.chmod(inner, 0o600)
        return marker, directory, inner

    def modes(self, root):
        found = {}
        for current, dirs, files in os.walk(root):
            for name in dirs + files:
                path = os.path.join(current, name)
                found[os.path.relpath(path, root)] = os.lstat(path).st_mode & 0o777
        return found

    def test_a_sealed_seed_keeps_its_deterministic_identity_and_read_only_modes(self):
        root = self.seed()
        bounds = seeds.bounds(2 * GIB)
        seeds.seal(root, bounds)
        digest, files, size = seeds.tree(root, bounds)
        self.assertEqual(digest, legacy_identity(root))
        self.assertEqual(files, 4)
        self.assertEqual(size, sum(os.lstat(os.path.join(current, name)).st_size
                                   for current, _, names in os.walk(root) for name in names))
        self.assertEqual(os.stat(root).st_mode & 0o777, 0o555)
        modes = self.modes(root)
        self.assertEqual(modes["probe-work"], 0o555)
        self.assertEqual(modes[os.path.join("probe-work", "tool")], 0o555)
        self.assertEqual(modes[os.path.join("probe-work", "nested", "f0")], 0o444)
        # Rehashing an unchanged seed gives the same identity; one changed byte does not.
        self.assertEqual(seeds.tree(root, bounds)[0], digest)
        changed = os.path.join(root, "probe-work", "nested", "f1")
        os.chmod(changed, 0o644)
        with open(changed, "r+b") as out:
            out.write(b"E")
        os.chmod(changed, 0o444)
        self.assertNotEqual(seeds.tree(root, bounds)[0], digest)
        self.assertEqual(seeds.remove(root), [])
        self.assertFalse(os.path.lexists(root))

    def test_a_ten_thousand_file_seed_is_admitted_within_the_declared_bounds(self):
        root = self.path("large")
        for index in range(10000):
            directory = os.path.join(root, "repo", "d%02d" % (index % 100))
            if index < 100:
                os.makedirs(directory)
            with open(os.path.join(directory, "f%05d" % index), "wb") as out:
                out.write(b"%0128d" % index)
        bounds = seeds.bounds(2 * GIB)
        seeds.seal(root, bounds)
        digest, files, size = seeds.tree(root, bounds)
        self.assertEqual((files, size), (10000, 10000 * 128))
        self.assertEqual(digest, legacy_identity(root))
        self.assertGreater(limits.SEED_ENTRIES_MAX, 10101 * 2)
        self.assertEqual(seeds.remove(root), [])

    def test_symlinks_refuse_the_seed_before_any_mode_or_byte_outside_it_changes(self):
        marker, directory, inner = self.outside()
        bounds = seeds.bounds(2 * GIB)
        cases = [("absolute file", lambda here: marker, "escape"),
                 ("relative file", lambda here: os.path.relpath(marker, here), "escape"),
                 ("symlinked directory", lambda here: directory, "escape-dir"),
                 ("dangling", lambda here: self.path("missing"), "escape")]
        for index, (what, target, name) in enumerate(cases):
            with self.subTest(case=what):
                root = self.seed("s%d" % index)
                here = os.path.join(root, "probe-work")
                os.symlink(target(here), os.path.join(here, name))
                before = self.modes(root)
                with self.assertRaises(ContractError) as caught:
                    seeds.seal(root, bounds)
                self.assertIn("probe-work/%s, which is a symlink" % name, str(caught.exception))
                # Nothing was sealed: every mode inside is as the writer left it.
                self.assertEqual(self.modes(root), before)
                self.assertEqual(os.stat(root).st_mode & 0o777, 0o755)
                with self.assertRaises(ContractError):
                    seeds.tree(root, bounds)
                # Nothing outside was read, changed or removed.
                self.assertEqual(read(marker), b"owned bytes outside the seed\n")
                self.assertEqual(os.stat(marker).st_mode & 0o777, 0o600)
                self.assertEqual(os.stat(directory).st_mode & 0o777, 0o700)
                self.assertEqual(os.stat(inner).st_mode & 0o777, 0o600)
                self.assertEqual(seeds.remove(root), [])
                self.assertFalse(os.path.lexists(root))
                self.assertEqual(read(inner), b"owned bytes in an outside directory\n")
                self.assertTrue(os.path.exists(marker) and os.path.isdir(directory))

    def test_a_symlinked_seed_root_and_other_entry_kinds_are_refused(self):
        marker, directory, _ = self.outside()
        bounds = seeds.bounds(2 * GIB)
        link = self.path("seed-link")
        os.symlink(directory, link)
        with self.assertRaises(ContractError) as caught:
            seeds.seal(link, bounds)
        self.assertIn("is not a directory", str(caught.exception))
        self.assertEqual(os.stat(directory).st_mode & 0o777, 0o700)
        root = self.seed("fifo")
        os.mkfifo(os.path.join(root, "probe-work", "pipe"))
        with self.assertRaises(ContractError) as caught:
            seeds.seal(root, bounds)
        self.assertIn("probe-work/pipe, which is a FIFO", str(caught.exception))
        self.assertEqual(seeds.remove(root), [])

    def test_sparse_logical_bytes_are_refused_before_anything_is_read(self):
        root = self.seed("sparse")
        sparse = os.path.join(root, "probe-work", "sparse")
        with open(sparse, "wb") as out:
            out.truncate(2 * GIB + 1)
        # A sparse file allocates almost nothing; its logical size is what is bounded.
        self.assertLess(os.lstat(sparse).st_blocks * 512, 1 << 20)
        bounds = seeds.bounds(2 * GIB)
        with mock.patch.object(admission, "read_bounded",
                               side_effect=AssertionError("nothing may be read")):
            with self.assertRaises(ContractError) as caught:
                seeds.seal(root, bounds)
            self.assertIn("probe-work/sparse holds 2147483649 logical bytes, more than "
                          "2147483648", str(caught.exception))
            with self.assertRaises(ContractError):
                seeds.tree(root, bounds)
        self.assertEqual(os.stat(root).st_mode & 0o777, 0o755)
        # The aggregate is bounded too: two files that each fit, but not together.
        os.unlink(sparse)
        for name in ("a", "b"):
            with open(os.path.join(root, "probe-work", name), "wb") as out:
                out.truncate(GIB + 1)
        with mock.patch.object(admission, "read_bounded",
                               side_effect=AssertionError("nothing may be read")):
            with self.assertRaises(ContractError) as caught:
                seeds.tree(root, bounds)
        self.assertIn("holds more than 2147483648 logical bytes", str(caught.exception))
        self.assertEqual(seeds.remove(root), [])

    def test_entry_count_and_depth_are_bounded_while_enumerating(self):
        root = self.seed("entries", files=6)
        tight = admission.Bounds(entries=5, depth=64, file_bytes=GIB, total_bytes=GIB)
        with self.assertRaises(ContractError) as caught:
            admission.scan(root, tight, label="the seed")
        self.assertIn("the seed holds more than 5 entries", str(caught.exception))
        deep = self.path("deep")
        os.makedirs(os.path.join(deep, *["d"] * 5))
        shallow = admission.Bounds(entries=100, depth=3, file_bytes=GIB, total_bytes=GIB)
        with self.assertRaises(ContractError) as caught:
            admission.scan(deep, shallow, label="the seed")
        self.assertIn("deeper than 3 levels", str(caught.exception))

    def counting_scandir(self):
        """A stand-in for `os.scandir` that counts every name a listing reads."""
        read = []
        original = os.scandir

        class Counted(object):
            def __init__(self, fd):
                self.listed = original(fd)

            def __enter__(self):
                return self

            def __exit__(self, *failure):
                self.listed.close()

            def __iter__(self):
                return self

            def __next__(self):
                entry = next(self.listed)
                read.append(entry.name)
                return entry

        return read, Counted

    def test_a_directory_beyond_the_entry_bound_is_refused_while_it_is_read(self):
        # Root-reproduced (ADR-0173): with two entries allowed, a real eight-file directory
        # was listed and sorted whole before the bound refused it. Now no more than one name
        # past the bound is read, and nothing past the bound is kept or sorted.
        root = self.path("wide")
        os.mkdir(root)
        for index in range(8):
            with open(os.path.join(root, "f%d" % index), "wb") as out:
                out.write(b"x")
        read, counted = self.counting_scandir()
        tight = admission.Bounds(entries=2, depth=64, file_bytes=GIB, total_bytes=GIB)
        with mock.patch.object(admission.os, "scandir", counted):
            with self.assertRaises(ContractError) as caught:
                admission.scan(root, tight, label="the seed")
        self.assertIn("the seed holds more than 2 entries", str(caught.exception))
        self.assertEqual(len(read), 3)
        # The bound counts the whole tree: a directory's listing has only the room its
        # ancestors left.
        nested = self.path("nested")
        os.makedirs(os.path.join(nested, "d"))
        for index in range(8):
            with open(os.path.join(nested, "d", "f%d" % index), "wb") as out:
                out.write(b"x")
        del read[:]
        three = admission.Bounds(entries=3, depth=64, file_bytes=GIB, total_bytes=GIB)
        with mock.patch.object(admission.os, "scandir", counted):
            with self.assertRaises(ContractError):
                admission.scan(nested, three, label="the seed")
        self.assertEqual((read[0], len(read)), ("d", 1 + 3))
        # Exactly at the bound is admitted, in sorted order.
        exact = self.path("exact")
        os.mkdir(exact)
        for name in ("b", "a"):
            with open(os.path.join(exact, name), "wb") as out:
                out.write(b"x")
        self.assertEqual([entry.path for entry in admission.scan(exact, tight)], ["a", "b"])

    def test_the_deadline_stops_a_listing_between_its_entries(self):
        root = self.path("slow-listing")
        os.mkdir(root)
        for index in range(8):
            with open(os.path.join(root, "f%d" % index), "wb") as out:
                out.write(b"x")
        read, counted = self.counting_scandir()
        # Every clock read costs 1 s, and 4 s are left beyond the reserve.
        clock = Clock(tick=1.0)
        bounds = admission.Bounds(entries=100, depth=64, file_bytes=GIB, total_bytes=GIB)
        with mock.patch.object(admission.os, "scandir", counted):
            with self.assertRaises(budget.Expired) as caught:
                admission.scan(root, bounds, budget.Budget(34.0, clock, 30), label="the seed")
        self.assertIn("the run's total deadline ended admitting the seed", str(caught.exception))
        self.assertLess(len(read), 8)

    def test_the_dominant_deadline_bounds_enumeration_and_hash_reads(self):
        root = self.seed("timed", files=8)
        bounds = seeds.bounds(2 * GIB)
        seeds.seal(root, bounds)
        # Nothing left: refused before anything is enumerated.
        clock = Clock()
        spent = budget.Budget(10.0, clock, 30)
        with self.assertRaises(budget.Expired) as caught:
            seeds.tree(root, bounds, spent)
        self.assertIn("verifying the seed", str(caught.exception))
        # Every clock read costs 1 s: enumeration and the hashing of its files stop once the
        # 20 s left before the reserve are gone, with the deadline's refusal.
        clock = Clock(tick=1.0)
        reads = []
        original = os.read

        def counted(fd, size):
            reads.append(size)
            return original(fd, size)

        with mock.patch.object(admission.os, "read", counted):
            with self.assertRaises(budget.Expired) as caught:
                seeds.tree(root, bounds, budget.Budget(50.0, clock, 30))
        self.assertIn("the run's total deadline ended", str(caught.exception))
        self.assertLess(len(reads), 9)
        # With time enough, the same tree is hashed whole.
        clock = Clock(tick=0.001)
        self.assertEqual(seeds.tree(root, bounds, budget.Budget(1000.0, clock, 30))[0],
                         legacy_identity(root))
        self.assertEqual(seeds.remove(root), [])

    def test_a_file_that_changes_size_while_it_is_read_is_refused(self):
        root = self.seed("growing")
        bounds = seeds.bounds(2 * GIB)
        entries = admission.scan(root, bounds)
        target = os.path.join(root, "probe-work", "nested", "f0")
        with open(target, "ab") as out:
            out.write(b"appended after the enumeration\n")
        with self.assertRaises(ContractError) as caught:
            admission.digests(root, entries)
        self.assertIn("grew while it was read", str(caught.exception))

    def test_removal_unlinks_symlinks_and_never_follows_them(self):
        marker, directory, inner = self.outside()
        root = self.seed("remove")
        os.symlink(directory, os.path.join(root, "probe-work", "dir-link"))
        os.symlink(marker, os.path.join(root, "probe-work", "file-link"))
        hidden = os.path.join(root, "probe-work", "locked")
        os.mkdir(hidden)
        with open(os.path.join(hidden, "x"), "wb") as out:
            out.write(b"x")
        os.chmod(hidden, 0)
        self.assertEqual(seeds.remove(root), [])
        self.assertFalse(os.path.lexists(root))
        self.assertEqual(read(marker), b"owned bytes outside the seed\n")
        self.assertEqual(os.stat(marker).st_mode & 0o777, 0o600)
        self.assertEqual(os.stat(directory).st_mode & 0o777, 0o700)
        self.assertEqual(json.loads(json.dumps(sorted(os.listdir(directory)))), ["inner"])
        self.assertEqual(os.stat(inner).st_mode & 0o777, 0o600)


class PortableSeeds(TempDir):
    def test_the_seed_fixtures_pass_under_a_restrictive_umask(self):
        """A Linux host with umask 0077 failed every seed fixture that took its modes from the
        umask. The seed tests run again in a child process whose umask is 0077."""
        lab = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        child = ("import os, sys, unittest\n"
                 "os.umask(0o077)\n"
                 "sys.dont_write_bytecode = True\n"
                 "sys.path.insert(0, %r)\n"
                 "suite = unittest.defaultTestLoader.loadTestsFromName("
                 "'tests.test_admission.Seeds')\n"
                 "result = unittest.TextTestRunner(verbosity=1).run(suite)\n"
                 "sys.exit(0 if result.wasSuccessful() and result.testsRun else 1)\n" % lab)
        completed = subprocess.run([sys.executable, "-I", "-c", child], cwd=lab,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=600)
        output = completed.stdout.decode("utf-8", "replace")
        self.assertEqual(completed.returncode, 0, output)
        self.assertIn("OK", output)


class Confined(TempDir):
    def test_a_confined_open_refuses_symlinked_files_and_ancestors(self):
        root = self.path("worktree")
        os.makedirs(os.path.join(root, "target", "release"))
        marker = self.path("outside")
        with open(marker, "wb") as out:
            out.write(b"outside marker\n")
        os.symlink(marker, os.path.join(root, "target", "release", "af"))
        with self.assertRaises(ContractError) as caught:
            admission.open_confined(root, "target/release/af", "the output")
        self.assertIn("is a symlink", str(caught.exception))
        os.unlink(os.path.join(root, "target", "release", "af"))
        os.rename(os.path.join(root, "target", "release"), self.path("elsewhere"))
        with open(self.path("elsewhere", "af"), "wb") as out:
            out.write(b"outside binary\n")
        os.symlink(self.path("elsewhere"), os.path.join(root, "target", "release"))
        with self.assertRaises(ContractError) as caught:
            admission.open_confined(root, "target/release/af", "the output")
        self.assertIn("target/release is not a directory", str(caught.exception))
        for bad in ("../outside", "target/../x", "/abs", ""):
            with self.assertRaises(ContractError):
                admission.open_confined(root, bad, "the output")


class ConfinedCopy(TempDir):
    """A seed's private copy (ADR-0176): only directories and regular files within bounds,
    every file read as its enumerated entry, and the copied bytes' identity is the seed's."""

    BOUNDS = admission.Bounds(16, 4, 1 << 20, 1 << 20)

    def tree(self):
        root = self.path("seed")
        os.makedirs(os.path.join(root, "work", "state"))
        with open(os.path.join(root, "work", "state", "events"), "wb") as out:
            out.write(b"e" * 5000)
        with open(os.path.join(root, "work", "run"), "wb") as out:
            out.write(b"#!/bin/sh\n")
        os.chmod(os.path.join(root, "work", "run"), 0o755)
        return root

    def test_a_copy_keeps_the_bytes_and_identity_and_is_private_and_writable(self):
        root = self.tree()
        seeds.seal(root, self.BOUNDS)
        sealed = seeds.tree(root, self.BOUNDS)
        entries, hashes = admission.copy_tree(root, self.path("copy"), self.BOUNDS)
        self.assertEqual(seeds.identity(entries, hashes), sealed)
        self.assertEqual(read(self.path("copy", "work", "state", "events")), b"e" * 5000)
        self.assertEqual(os.stat(self.path("copy", "work", "run")).st_mode & 0o777, 0o700)
        self.assertEqual(os.stat(self.path("copy", "work", "state", "events")).st_mode
                         & 0o777, 0o600)
        self.assertEqual(os.stat(self.path("copy", "work")).st_mode & 0o777, 0o700)
        with self.assertRaises(FileExistsError):
            admission.copy_tree(root, self.path("copy"), self.BOUNDS)
        seeds.remove(root)

    def test_symlinks_fifos_and_bounds_refuse_before_anything_is_copied(self):
        marker = self.path("outside")
        with open(marker, "wb") as out:
            out.write(b"owned bytes outside the seed\n")
        for name, make, needle in (
                ("file_link", lambda path: os.symlink(marker, path), "symlink"),
                ("dir_link", lambda path: os.symlink(self.tmp, path), "symlink"),
                ("fifo", os.mkfifo, "FIFO")):
            with self.subTest(name=name):
                root = self.tree()
                make(os.path.join(root, "work", name))
                with self.assertRaises(ContractError) as caught:
                    admission.copy_tree(root, self.path("copy"), self.BOUNDS)
                self.assertIn(needle, str(caught.exception))
                self.assertFalse(os.path.lexists(self.path("copy")), "nothing was created")
                seeds.remove(root)
        root = self.tree()
        with open(os.path.join(root, "work", "sparse"), "wb") as out:
            out.truncate(GIB)
        with self.assertRaises(ContractError) as caught:
            admission.copy_tree(root, self.path("copy"), self.BOUNDS)
        self.assertIn("logical bytes", str(caught.exception))
        self.assertFalse(os.path.lexists(self.path("copy")))
        self.assertEqual(read(marker), b"owned bytes outside the seed\n")

    def test_the_deadline_bounds_the_copy_while_it_reads(self):
        root = self.tree()
        # Every read of this clock is a second later: the copy stops at its deadline, wherever
        # in the enumeration or the reads that falls.
        with self.assertRaises(budget.Expired):
            admission.copy_tree(root, self.path("copy"), self.BOUNDS,
                                budget.Budget(6.0, Clock(1.0), 0))
        seeds.remove(self.path("copy"))
        seeds.remove(root)
