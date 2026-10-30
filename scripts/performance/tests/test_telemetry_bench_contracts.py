"""The telemetry bench scripts' drain boundary and per-release fixture contracts.

The drain is read from two `CLOCK_MONOTONIC` readings, never from a file timestamp; the
workload each release accepts is produced from the committed fixtures by an explicit contract
selected by the measured release's version. A stand-in `af` answers `--version` and `catalog
digest`; nothing here measures anything.
"""
import copy
import json
import os
import pathlib
import shutil
import stat
import time

from perflab import telemetrylab
from tests.support import TempDir

overhead = telemetrylab.script_module("scripts/telemetry-overhead.py")
REPOSITORY = pathlib.Path(__file__).resolve().parents[3]

# A stand-in `af`: `--version` prints its release, `catalog digest PATH` prints the digest the
# environment's table names for PATH's worker.toml contents, else the committed pin.
STAND_IN = r'''#!/usr/bin/env python3
import hashlib, json, os, sys
table = json.loads(os.environ["STAND_IN_TABLE"])
if sys.argv[1:] == ["--version"]:
    print("af " + table["release"])
elif sys.argv[1:3] == ["catalog", "digest"]:
    data = b""
    for root, _, files in sorted(os.walk(sys.argv[3])):
        for name in sorted(files):
            with open(os.path.join(root, name), "rb") as file:
                data += file.read()
    key = "@1" if b"af/TaskCheckReceipt@1" in data else "@2"
    print(table["digests"][key].get(sys.argv[3], "sha256:" + "0" * 64))
'''


class Drain(TempDir):
    def store(self, lines, reply_mtime_ns=None):
        """A Store whose CAS holds the dense harness's answer and its stderr."""
        state = pathlib.Path(self.tmp) / "store"
        objects = state / "cas" / "objects" / "ab"
        objects.mkdir(parents=True)
        reply = objects / "reply"
        reply.write_bytes(overhead.REPLY)
        if reply_mtime_ns is not None:
            os.utime(reply, ns=(reply_mtime_ns, reply_mtime_ns))
        (objects / "stderr").write_bytes(b"".join(json.dumps(line, separators=(",", ":"))
                                                  .encode() + b"\n"
                                                  for line in lines))
        return state

    def test_the_drain_is_ordered_monotonic_readings_whatever_the_file_timestamps(self):
        # A de-duplicated answer object may keep an earlier run's modification time, and the
        # wall clock may step back: neither reaches the drain, which reads only the harness's
        # and the measurement's CLOCK_MONOTONIC.
        written = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        state = self.store([{"type": "assistant"},
                            {"type": "result", "subtype": "af_bench_last_write",
                             "last_write_unix_ns": str(time.time_ns() + 10 ** 12),
                             "last_write_monotonic_ns": str(written)}],
                           reply_mtime_ns=1)
        self.assertEqual(overhead.last_write(state), written)
        exited = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        self.assertGreaterEqual(exited, written)
        self.assertGreaterEqual(overhead.drain_latency(written, exited), 0)
        self.assertEqual(overhead.drain_latency(1_000_000, 3_500_000), 2.5)

    def test_a_negative_raw_drain_is_kept_as_measured(self):
        self.assertEqual(overhead.drain_latency(3_000_000, 1_000_000), -2.0)

    def test_a_harness_without_a_monotonic_last_write_is_refused(self):
        # The superseded file-clock spelling is not read.
        state = self.store([{"type": "result", "subtype": "af_bench_last_write",
                             "last_write_unix_ns": "5", "last_write_fs_ns": "5"}])
        with self.assertRaisesRegex(overhead.BenchError, "CLOCK_MONOTONIC"):
            overhead.last_write(state)

    def test_judge_refuses_another_drain_boundary_or_clock(self):
        from tests import telemetry_fixtures as fx
        doc = fx.overhead_doc(20261021, 3)
        for key, value in (("boundary", "cas_commit"), ("clock", "filesystem")):
            changed = copy.deepcopy(doc)
            changed["drain"][key] = value
            with self.assertRaisesRegex(overhead.BenchError, "other boundaries"):
                overhead.summarize(changed)


class FixtureContracts(TempDir):
    def stand_in(self, release, digests):
        path = pathlib.Path(self.tmp) / "af"
        path.write_text(STAND_IN)
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        os.environ["STAND_IN_TABLE"] = json.dumps({"release": release, "digests": digests})
        self.addCleanup(os.environ.pop, "STAND_IN_TABLE", None)
        return str(path)

    def copy(self, fixture):
        repo = pathlib.Path(self.tmp) / fixture / "pristine"
        overhead.copy_writable(overhead.FIXTURES_DIR / fixture / "repo", repo)
        return repo

    def test_the_release_selects_its_contract_and_a_mismatched_request_is_refused(self):
        self.assertEqual(overhead.select_contract("0.13.0"), "check-receipt@1")
        self.assertEqual(overhead.select_contract("0.14.0-rc.1"), "check-receipt@2")
        self.assertEqual(overhead.select_contract("0.14.0-rc.1", "check-receipt@2"),
                         "check-receipt@2")
        for release, requested in (("0.13.0", "check-receipt@2"),
                                   ("0.14.0-rc.1", "check-receipt@1")):
            with self.assertRaisesRegex(overhead.BenchError, "measures under"):
                overhead.select_contract(release, requested)
        with self.assertRaisesRegex(overhead.BenchError, "not `af VERSION`"):
            overhead.release_version("0.13.0")

    def test_a_release_no_contract_names_is_refused_never_given_the_current_contract(self):
        # `0.11.9` (and a development `0.0.0`) declare no bench workload: refused, with or
        # without a requested contract, rather than measured under check-receipt@2.
        for release in ("0.11.9", "0.0.0", "0.14.0"):
            with self.subTest(release=release):
                with self.assertRaisesRegex(overhead.BenchError, "named by no fixture contract"):
                    overhead.select_contract(release)
                with self.assertRaisesRegex(overhead.BenchError, "named by no fixture contract"):
                    overhead.select_contract(release, overhead.CURRENT_CONTRACT)
        named = [version for contract in overhead.FIXTURE_CONTRACTS.values()
                 for version in contract["versions"]]
        self.assertEqual(len(named), len(set(named)), "a release is named by one contract")

    def test_check_receipt_1_renames_only_the_artifact_type_and_pins_the_declared_digests(self):
        fixture = "telemetry-bench-dense"
        pins = overhead.FIXTURE_CONTRACTS["check-receipt@1"]["pins"][fixture]
        # Only `bench/stream` declares the receipt: the renamed one is digested as `@1`.
        af = self.stand_in("0.13.0", {
            "@1": {".af/task-packages/bench/stream": pins["bench/stream"]},
            "@2": {".af/task-packages/bench/dense": pins["bench/dense"]}})
        repo = self.copy(fixture)
        produced = overhead.apply_contract(af, fixture, repo, "check-receipt@1")
        self.assertEqual(produced["packages"], pins)
        self.assertEqual(produced["renamed_files"],
                         [".af/task-packages/bench/stream/worker.toml"])
        source = overhead.FIXTURES_DIR / fixture / "repo"
        worker = ".af/task-packages/bench/stream/worker.toml"
        self.assertEqual((repo / worker).read_bytes(),
                         (source / worker).read_bytes().replace(b"af/TaskCheckReceipt@2",
                                                                b"af/TaskCheckReceipt@1"))
        catalog = (repo / ".af/task-catalog.toml").read_text()
        for digest in pins.values():
            self.assertIn('digest = "%s"' % digest, catalog)
        # The same business workload: nothing else in the repository changed.
        for path in source.rglob("*"):
            relative = path.relative_to(source)
            if path.is_file() and str(relative) not in (worker, ".af/task-catalog.toml"):
                self.assertEqual((repo / relative).read_bytes(), path.read_bytes(), relative)
        self.assertFalse((repo.parent / "contract").exists())

    def test_a_digest_the_contract_does_not_declare_is_refused(self):
        fixture = "telemetry-bench-dense"
        af = self.stand_in("0.13.0", {"@1": {}, "@2": {}})
        with self.assertRaisesRegex(overhead.BenchError, "not the declared"):
            overhead.apply_contract(af, fixture, self.copy(fixture), "check-receipt@1")

    def test_the_current_contract_keeps_the_committed_packages(self):
        fixture = "telemetry-bench-invocation-one"
        catalog = (overhead.FIXTURES_DIR / fixture / "repo/.af/task-catalog.toml").read_text()
        import re
        committed = dict(re.findall(r'digest = "([^"]+)"\npath = "([^"]+)"', catalog))
        af = self.stand_in("0.14.0-rc.1", {"@1": {}, "@2": {path: digest for digest, path
                                                            in committed.items()}})
        repo = self.copy(fixture)
        produced = overhead.apply_contract(af, fixture, repo, "check-receipt@2")
        self.assertEqual(produced["renamed_files"], [])
        self.assertEqual(sorted(produced["packages"].values()), sorted(committed))
        self.assertEqual((repo / ".af/task-catalog.toml").read_text(), catalog)

    def test_every_contract_pins_exactly_each_fixtures_packages(self):
        import re
        for name, contract in overhead.FIXTURE_CONTRACTS.items():
            if contract["pins"] is None:
                continue
            for fixture in sorted(os.listdir(overhead.FIXTURES_DIR)):
                catalog = overhead.FIXTURES_DIR / fixture / "repo/.af/task-catalog.toml"
                if not catalog.exists():
                    continue
                packages = re.findall(r'^\[packages\."([^"]+)"\]', catalog.read_text(), re.M)
                self.assertEqual(sorted(contract["pins"][fixture]), sorted(packages),
                                 (name, fixture))


if __name__ == "__main__":
    import unittest
    unittest.main()
