"""The content-addressed filesystem archive (package p4, ADR-0172).

A real archive root in each test's own directory, filled from current-format synthetic runs
labeled synthetic. Retention and status run at controlled instants; capacity exhaustion uses
a tiny configured capacity and a stand-in filesystem probe, never a full disk.
"""
import json
import os
import subprocess
import sys
from unittest import mock

from perflab import archive, compare, contracts, limits, synthetic
from perflab.values import ContractError, sha256_bytes
from tests.support import CLI, TempDir, read, run_dir, synthetic_suite

DAY = archive.DAY_MS
T0 = 1800000000000


class Clock(object):
    def __init__(self, unix_ms=T0):
        self.unix_ms = unix_ms

    def __call__(self):
        return self.unix_ms / 1000.0


def tree(root):
    """{relative path: bytes} of every file below `root`."""
    found = {}
    for current, _, names in os.walk(root):
        for name in names:
            path = os.path.join(current, name)
            found[os.path.relpath(path, root)] = read(path)
    return found


class ArchiveHarness(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.suite = synthetic_suite()
        self.clock = Clock()
        self.root = self.path("archive")
        archive.Archive.create(self.root, wall=self.clock)

    def make_run(self, run_id="archived_run", candidate="candidate", value="10.5", with_log=False,
            **options):
        plan = synthetic.plan(self.suite, run_id=run_id, candidate=candidate)
        path = run_dir(self, plan, synthetic.constant(value), **options)
        if with_log:
            # A retained failed-sample log, as a measured shard keeps one.
            root = os.path.join(path, "shards", "0")
            os.mkdir(os.path.join(root, "logs"))
            with open(os.path.join(root, "logs", "000004.log"), "wb") as stream:
                stream.write(b"shared log bytes\n")
            synthetic.reseal_shard(root)
        return path

    def open(self, **options):
        return archive.Archive(self.root, wall=self.clock, **options)


class Import(ArchiveHarness):
    def test_a_verified_run_is_admitted_whole_and_its_import_is_idempotent(self):
        source = self.make_run()
        before = tree(source)
        with self.open() as store:
            self.assertEqual(store.import_run(source), ("imported", "archived_run"))
            events = store.events()
            objects = sorted(store.objects())
            self.assertEqual(store.import_run(source), ("already_archived", "archived_run"))
            self.assertEqual(store.events(), events)
            self.assertEqual(sorted(store.objects()), objects)
            run = store.state()["archived_run"]
            members = dict((member["path"], member) for member in run.members)
            # Every bundle member, the recomputed verdict and the generated summary, each its
            # object, rehashed.
            for path in ("run.json", "builds.json", "manifest.json", "shards/0/samples.jsonl",
                         "shards/0/shard.json", "shards/0/manifest.json", "verdict.json",
                         "report.json"):
                self.assertIn(path, members)
                data = store.read_object(members[path], limits.SHARD_BUNDLE_BYTES_MAX)
                self.assertEqual(sha256_bytes(data), members[path]["sha256"])
            self.assertEqual(json.loads(store.read_object(members["verdict.json"], 1 << 24)),
                             compare.compare(source))
            self.assertEqual(members["shards/0/samples.jsonl"]["class"], "samples")
            self.assertEqual(members["run.json"]["class"], "plan")
            # Collections record both attempts; the run index holds the run once.
            self.assertEqual([item["status"] for item in store.collections()],
                             ["imported", "already_archived"])
            materialized = store.materialize("archived_run", self.path("rebuilt"))
        self.assertEqual(tree(materialized), before)
        self.assertEqual(tree(source), before, "the source evidence is never changed")
        for current, _, names in os.walk(os.path.join(self.root, "objects")):
            for name in names:
                self.assertEqual(os.stat(os.path.join(current, name)).st_mode & 0o777, 0o444)

    def test_another_run_under_an_archived_id_is_refused(self):
        with self.open() as store:
            store.import_run(self.make_run())
            with self.assertRaises(archive.Refused) as caught:
                store.import_run(self.make_run(value="11"))
            self.assertIn("an archived run is never replaced", str(caught.exception))
            self.assertEqual(len(store.state()), 1)
            self.assertEqual(store.collections()[-1]["status"], "refused")

    def test_partial_tampered_and_symlinked_bundles_are_refused_before_anything_is_written(self):
        cases = []
        tampered = self.make_run("tampered_run")
        with open(os.path.join(tampered, "shards", "0", "samples.jsonl"), "ab") as stream:
            stream.write(b"\n")
        cases.append((tampered, "shard 0's bundle is partial, tampered or of another run"))
        partial = self.make_run("partial_run")
        os.unlink(os.path.join(partial, "shards", "1", "shard.json"))
        cases.append((partial, "shard 1's bundle is partial"))
        broken = self.make_run("broken_run")
        os.unlink(os.path.join(broken, "builds.json"))
        cases.append((broken, "the run bundle did not verify"))
        linked = self.make_run("linked_run")
        outside = self.path("outside-samples")
        os.rename(os.path.join(linked, "shards", "1", "samples.jsonl"), outside)
        os.symlink(outside, os.path.join(linked, "shards", "1", "samples.jsonl"))
        cases.append((linked, "shard 1's bundle is partial"))
        with self.open() as store:
            for source, why in cases:
                with self.subTest(source=os.path.basename(source)):
                    with self.assertRaises(archive.Refused) as caught:
                        store.import_run(source)
                    self.assertIn(why, str(caught.exception))
            self.assertEqual(store.events(), [])
            self.assertEqual(store.objects(), [])
            self.assertEqual([item["status"] for item in store.collections()],
                             ["refused"] * len(cases))
        # A missing shard is incomplete evidence, admitted and kept as it is.
        missing = self.make_run("missing_shard_run")
        archive.admission.remove_tree(os.path.join(missing, "shards", "1"))
        with self.open() as store:
            self.assertEqual(store.import_run(missing), ("imported", "missing_shard_run"))
            event = store.state()["missing_shard_run"].event["event"]
            self.assertEqual(event["verdict"]["result"], "incomplete")
            self.assertEqual(event["bundles"]["shards"][1]["bundle"], None)

    def test_a_corrupted_or_colliding_object_is_never_trusted(self):
        with self.open() as store:
            run = self.make_run()
            store.import_run(run)
            # The plan's object, which every materialization reads; the first object by digest
            # may be one it recomputes instead (a verdict).
            digest = sha256_bytes(read(os.path.join(run, "run.json")))
            self.assertIn(digest, [item[0] for item in store.objects()])
            path = os.path.join(self.root, store.object_relative(digest))
            os.chmod(path, 0o644)
            with open(path, "r+b") as stream:
                stream.write(b"X")
            with self.assertRaises(ContractError) as caught:
                store.materialize("archived_run", self.path("rebuilt"))
            self.assertIn("does not match its name", str(caught.exception))
        # Bytes already at a member's object name that are not that member refuse its import,
        # recorded, rather than being trusted as the member.
        source = self.make_run("second_run")
        member = read(os.path.join(source, "run.json"))
        fresh = self.path("fresh-archive")
        archive.Archive.create(fresh, wall=self.clock)
        with archive.Archive(fresh, wall=self.clock) as store:
            relative = store.object_relative(sha256_bytes(member))
            os.makedirs(os.path.dirname(os.path.join(fresh, relative)))
            with open(os.path.join(fresh, relative), "wb") as stream:
                stream.write(b"X" * len(member))
            with self.assertRaises(archive.Refused) as caught:
                store.import_run(source)
            self.assertIn("is not the %d bytes its name says" % len(member),
                          str(caught.exception))
            self.assertEqual(store.events(), [])
            self.assertEqual(store.collections()[-1]["status"], "refused")

    def test_an_interrupted_import_leaves_only_staging_and_the_next_command_completes(self):
        source = self.make_run()
        with self.open() as store:
            with mock.patch.object(store, "_event", side_effect=KeyboardInterrupt):
                with self.assertRaises(KeyboardInterrupt):
                    store.import_run(source)
            # Objects were written durably, but no index entry names the run.
            self.assertEqual(store.events(), [])
            self.assertTrue(store.objects())
            with open(os.path.join(self.root, archive.TMP, "stage-leftover"), "wb") as stream:
                stream.write(b"half an entry")
        with self.open() as store:
            # Opening cleared the staging; the import completes over the existing objects.
            self.assertEqual(os.listdir(os.path.join(self.root, archive.TMP)), [])
            self.assertEqual(store.import_run(source), ("imported", "archived_run"))
            self.assertEqual(len(store.events()), 1)

    def test_the_index_fails_closed_on_gaps_unknown_versions_and_fields(self):
        with self.open() as store:
            store.import_run(self.make_run())
            store.pin("archived_run", "release_budget")
        index = os.path.join(self.root, archive.INDEX)
        first = os.path.join(index, "000000000001.json")
        original = read(first)
        for change, why in (
                (lambda doc: doc.update(schema="af.perf-archive-event/2"),
                 "expected af.perf-archive-event/1"),
                (lambda doc: doc.update(extra=True), "extra"),
                (lambda doc: doc["event"].update(kind="unknown"), "expected one of gc, pin"),
                (lambda doc: doc.update(sequence=2), "names sequence 2")):
            doc = json.loads(original)
            change(doc)
            os.chmod(first, 0o644)
            with open(first, "w") as stream:
                stream.write(json.dumps(doc))
            with self.subTest(why=why):
                with self.open() as store:
                    with self.assertRaises(ContractError) as caught:
                        store.state()
                    self.assertIn(why, str(caught.exception))
        with open(first, "wb") as stream:
            stream.write(original)
        os.rename(os.path.join(index, "000000000002.json"),
                  os.path.join(index, "000000000003.json"))
        with self.open() as store:
            with self.assertRaises(ContractError) as caught:
                store.events()
            self.assertIn("is not the next entry", str(caught.exception))

    def test_a_symlinked_root_or_directory_and_a_held_lock_refuse(self):
        link = self.path("archive-link")
        os.symlink(self.root, link)
        with self.assertRaises(ContractError):
            archive.Archive(link).open()
        objects = os.path.join(self.root, "objects", "sha256")
        os.rename(objects, self.path("elsewhere"))
        os.symlink(self.path("elsewhere"), objects)
        with self.assertRaises(ContractError) as caught:
            self.open().open()
        self.assertIn("symlink is never followed", str(caught.exception))
        os.unlink(objects)
        os.rename(self.path("elsewhere"), objects)
        with self.open():
            with self.assertRaises(archive.Refused) as caught:
                self.open().open()
            self.assertIn("another archive command holds", str(caught.exception))
        with self.assertRaises(ContractError):
            archive.Archive.create(self.root)
        with self.assertRaises(ContractError):
            archive.Archive.create(self.path("big"), limits.ARCHIVE_BYTES_MAX + 1)


class Capacity(ArchiveHarness):
    def test_exhausted_capacity_refuses_visibly_and_leaves_the_source_intact(self):
        small = self.path("small")
        archive.Archive.create(small, capacity_bytes=4096, wall=self.clock)
        source = self.make_run()
        before = tree(source)
        with archive.Archive(small, wall=self.clock) as store:
            with self.assertRaises(archive.Refused) as caught:
                store.import_run(source)
            self.assertEqual(caught.exception.status, "capacity_exhausted")
            self.assertEqual(store.objects(), [])
            collection = store.collections()[-1]
            self.assertEqual(collection["status"], "capacity_exhausted")
            self.assertGreater(collection["needed_bytes"], collection["available_bytes"])
            status = store.status(T0)
            self.assertEqual(status["health"]["status"], "failed")
            self.assertEqual([item["code"] for item in status["health"]["problems"]][0],
                             "capacity_exhausted")
        self.assertEqual(tree(source), before)
        # The filesystem's free space bounds an import too, whatever the capacity.
        with self.open(free_bytes=lambda path: 1024) as store:
            with self.assertRaises(archive.Refused) as caught:
                store.import_run(source)
            self.assertIn("the filesystem's free space is exhausted", str(caught.exception))
        result = subprocess.run([sys.executable, CLI, "archive", "--root", small, "--status"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 4)
        self.assertEqual(json.loads(result.stdout)["health"]["status"], "failed")

    def test_status_reports_capacity_lag_and_the_last_successful_collection(self):
        with self.open() as store:
            status = store.status(T0)
            self.assertEqual([item["code"] for item in status["health"]["problems"]],
                             ["no_collection"])
            store.import_run(self.make_run())
            status = store.status(T0)
        self.assertEqual(status["health"], {"status": "ok", "problems": []})
        self.assertEqual(status["runs"], 1)
        self.assertEqual(status["capacity_bytes"], limits.ARCHIVE_BYTES_DEFAULT)
        self.assertEqual(status["available_bytes"],
                         status["capacity_bytes"] - status["used_bytes"])
        self.assertEqual(status["last_successful_collection_unix_ms"], T0)
        # Synthetic shards ended at their fixed instants; the lag is collection minus that.
        self.assertGreater(status["collection_lag_ms"]["last"], 0)
        contracts.validate_archive_status(status)


class Retention(ArchiveHarness):
    def test_logs_expire_after_90_days_and_numeric_evidence_after_365(self):
        source = self.make_run(with_log=True)
        with self.open() as store:
            store.import_run(source)
            self.assertEqual(store.retain(T0 + 89 * DAY), [])
            written = store.retain(T0 + 90 * DAY)
            kinds = [doc["event"]["kind"] for doc in written]
            self.assertEqual(kinds, ["retention", "gc"])
            retention = written[0]["event"]
            self.assertEqual([item["path"] for item in retention["expired"]],
                             ["shards/0/logs/000004.log"])
            self.assertEqual(retention["numeric"], "complete")
            self.assertEqual(retention["profile_reproduction"], "not_applicable")
            # The retention record keeps the original manifest and names what remains.
            self.assertEqual(retention["manifest"], store.state()["archived_run"].members)
            self.assertIn("shards/0/samples.jsonl", retention["available"])
            self.assertEqual(written[1]["event"]["removed"][0]["sha256"],
                             sha256_bytes(b"shared log bytes\n"))
            # Without its log the shard bundle cannot be rebuilt whole, and nothing claims so.
            with self.assertRaises(ContractError) as caught:
                store.materialize("archived_run", self.path("rebuilt"))
            self.assertIn("lost bundle members to retention", str(caught.exception))
            self.assertEqual(store.retain(T0 + 364 * DAY), [])
            written = store.retain(T0 + 365 * DAY)
            retention = written[0]["event"]
            self.assertEqual(retention["numeric"], "expired")
            self.assertEqual(retention["available"], [])
            self.assertEqual(store.objects(), [])
            # The run's record and its manifest stay in the index.
            self.assertEqual(len(store.state()["archived_run"].members),
                             len(retention["manifest"]))

    def test_pinned_runs_stay_whole_until_superseded_and_365_days_more(self):
        with self.open() as store:
            store.import_run(self.make_run(with_log=True))
            store.pin("archived_run", "active_cohort", "cohort c1")
            self.assertEqual(store.retain(T0 + 3000 * DAY), [])
            self.assertEqual(store.state()["archived_run"].expired, {})
            self.clock.unix_ms = T0 + 3000 * DAY
            store.import_run(self.make_run("successor_run", candidate="next"))
            store.supersede("archived_run", "successor_run")
            superseded = T0 + 3000 * DAY
            self.assertEqual(store.retain(superseded + 364 * DAY), [])
            self.assertNotIn("shards/0/samples.jsonl", store.state()["archived_run"].expired)
            written = store.retain(superseded + 365 * DAY)
            expired = [doc["event"] for doc in written if doc["event"]["kind"] == "retention"
                       and doc["event"]["run_id"] == "archived_run"]
            self.assertEqual(expired[0]["numeric"], "expired")
            with self.assertRaises(ContractError):
                store.pin("archived_run", "release_budget")

    def test_garbage_collection_traces_references_so_shared_objects_stay(self):
        # Two runs share a log's bytes and, as a diagnostic child run's symbols would be, a
        # symbol object (synthetic members; package p5 supplies real profiles and symbols).
        with self.open() as store:
            store.import_run(self.make_run("first_run", with_log=True))
            self.clock.unix_ms = T0 + 60 * DAY
            store.import_run(self.make_run("second_run", candidate="other", with_log=True))
            symbol = b"synthetic symbol table\n"
            digest = sha256_bytes(symbol)
            store._put(digest, len(symbol), archive._bytes_writer(symbol))
            for run_id in ("first_run", "second_run"):
                run = store.state()[run_id]
                event = json.loads(json.dumps(run.event["event"]))
                event["run_id"] = run_id + "_profiles"
                event["members"] = sorted(event["members"] + [
                    {"path": "profiles/af.sym", "sha256": digest, "size": len(symbol),
                     "class": "symbol"}], key=lambda member: member["path"])
                store._event(event, run.event["unix_ms"])
            log = sha256_bytes(b"shared log bytes\n")
            store.retain(T0 + 90 * DAY)
            runs = store.state()
            self.assertIn("shards/0/logs/000004.log", runs["first_run"].expired)
            self.assertNotIn("shards/0/logs/000004.log", runs["second_run"].expired)
            self.assertEqual(runs["first_run_profiles"].profile_reproduction(), "expired")
            self.assertEqual(runs["second_run_profiles"].profile_reproduction(), "available")
            present = dict(store.objects())
            self.assertIn(log, present, "the second run still references the shared log")
            self.assertIn(digest, present, "the second run still references the symbol")
            store.retain(T0 + 150 * DAY)
            present = dict(store.objects())
            self.assertNotIn(log, present)
            self.assertNotIn(digest, present)
            self.assertEqual(store.state()["second_run_profiles"].profile_reproduction(),
                             "expired")
            # Numeric evidence of both runs remains for its 365 days.
            self.assertEqual(store.state()["second_run"].numeric(), "complete")

    def test_the_cli_imports_retains_pins_and_reports(self):
        source = self.make_run()
        root = self.path("cli-archive")
        commands = [["--init", "--capacity-bytes", str(1 << 30)], ["--import", source],
                    ["--import", source], ["--pin", "archived_run", "--reason", "release_budget"],
                    ["--retain", "--now-unix-ms", str(T0 + 400 * DAY)], ["--status"]]
        outputs = []
        for arguments in commands:
            result = subprocess.run([sys.executable, CLI, "archive", "--root", root]
                                    + arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertEqual(result.returncode, 0, result.stderr)
            outputs.append(result)
        self.assertIn(b"already archived", outputs[2].stderr)
        status = json.loads(outputs[-1].stdout)
        self.assertEqual((status["runs"], status["pinned"]), (1, 1))
        result = subprocess.run([sys.executable, CLI, "archive", "--root", root, "--retain",
                                 "--status"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 2)
