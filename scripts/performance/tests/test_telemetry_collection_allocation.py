"""Full collection allocation admission on the checkout filesystem, never sparse stand-ins."""
import io
import os
import shutil
import tempfile
import zipfile
from unittest import mock

from perflab import budget, contracts, telemetrycollect, telemetrylab, transport
from perflab.values import ContractError
from tests import test_telemetry_collect as collecting
from tests.allocation_observer import observe
from tests.support import ROOT, TempDir, read


class AllocationCycles(TempDir):
    result = collecting.Collection.result
    cycle = collecting.Collection.cycle

    def setUp(self):
        collecting.Collection.setUp(self)
        owned = os.path.join(ROOT, "target", "performance-allocation-tests")
        os.makedirs(owned, exist_ok=True)
        self.disk = tempfile.mkdtemp(dir=owned)
        self.addCleanup(shutil.rmtree, self.disk, True)
        self.config["root"] = os.path.join(self.disk, "history")
        self.config["artifact_bytes_max"] = 4 << 20

    def usage(self):
        return telemetrylab.HistoryRoot(self.config["root"], sweep=False).used_bytes()

    def attempt(self, data, ident=701):
        return self.cycle([collecting.run_entry(ident)], {ident: [collecting.artifact_entry(
            ident, ident, "perf-telemetry-%d-1" % ident, data)]})[0]

    def watch(self):
        return observe(self, self.config["root"], lambda: self.config["bytes_max"])

    def pad(self, size):
        self.cycle([], {})
        with open(os.path.join(self.config["root"], "foreign-padding"), "wb") as stream:
            stream.write(b"p" * size)

    def test_near_full_valid_and_malformed_downloads_refuse_before_excess_writes(self):
        good = collecting.zipped(self.result(701))
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w", zipfile.ZIP_STORED) as container:
            container.writestr("malformed.bin", os.urandom(2 << 20))
        self.pad(3 << 20)
        self.config["bytes_max"] = 4 << 20
        actual = transport._materialize
        for data in (buffer.getvalue(), good):
            with self.subTest(valid=data is good), self.watch() as samples, \
                    mock.patch.object(transport, "_materialize", wraps=actual) as materialized:
                doc = self.attempt(data)
            self.assertEqual(doc["runs"][0]["outcome"], "refused", doc)
            self.assertIn("byte bound", doc["runs"][0]["reason"])
            self.assertLessEqual(max(samples), self.config["bytes_max"])
            self.assertFalse(os.listdir(os.path.join(self.config["root"], "results")))
            self.assertTrue(os.path.exists(os.path.join(self.config["root"], "foreign-padding")))
            # A valid download may fit, but authority expansion must not start without room
            # for itself, retained authority and the import copy together.
            self.assertLessEqual(materialized.call_count, 1)
        self.assertTrue(read(os.path.join(self.config["root"], telemetrylab.HISTORY_HTML)).endswith(
            b"</html>\n"))

    def test_small_files_and_directories_are_charged_before_materialization(self):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w", zipfile.ZIP_STORED) as container:
            for index in range(240):
                container.writestr("d%03d/x" % index, b"x")
        self.pad(3 << 20)
        self.config["bytes_max"] = 4 << 20
        with self.watch(), mock.patch.object(transport, "_materialize",
                                             wraps=transport._materialize) as materialized:
            doc = self.attempt(buffer.getvalue())
        self.assertEqual(doc["runs"][0]["outcome"], "refused")
        self.assertIn("byte bound", doc["runs"][0]["reason"])
        materialized.assert_not_called()

    def test_full_peak_reservation_exact_and_one_byte_short_then_successful_history(self):
        data = collecting.zipped(self.result(701))
        self.pad(1 << 20)
        saved = os.path.join(self.disk, "saved")
        shutil.copytree(self.config["root"], saved)
        original = telemetrylab.HistoryRoot.reserve_paths
        requirements = []

        def reserved(kept, files, directories=(), entries=()):
            requirements.append(kept.used_bytes() + kept.allocation_for(files, directories, entries))
            return original(kept, files, directories, entries)

        with self.watch() as samples, mock.patch.object(telemetrylab.HistoryRoot,
                                                       "reserve_paths", reserved):
            doc = self.attempt(data)
        self.assertEqual(doc["runs"][0]["outcome"], "collected", doc)
        capacity = max(requirements)
        self.assertGreaterEqual(capacity, max(samples))
        for limit, succeeds in ((capacity - 1, False), (capacity, True)):
            shutil.rmtree(self.config["root"])
            shutil.copytree(saved, self.config["root"])
            self.config["bytes_max"] = limit
            with self.subTest(limit=limit), self.watch():
                doc = self.attempt(data)
            self.assertEqual(doc["runs"][0]["outcome"], "collected" if succeeds else "refused", doc)
            if succeeds:
                self.assertEqual(doc["status"], "ok", doc)
                self.assertEqual(doc["used_bytes"], self.usage())
                again = self.attempt(data)
                self.assertEqual(again["runs"][0]["outcome"], "already_collected")
            else:
                self.assertFalse(os.listdir(os.path.join(self.config["root"], "results")))

    def test_interrupted_materialization_retains_bounded_owned_bytes_and_restarts(self):
        data = collecting.zipped(self.result(701))
        self.pad(1 << 20)
        self.config["bytes_max"] = 64 << 20
        saved = os.path.join(self.disk, "saved")
        shutil.copytree(self.config["root"], saved)
        materialize, copy_members = transport._materialize, telemetrylab._write_members
        for kind in ("download", "authority", "retained", "import"):
            shutil.rmtree(self.config["root"])
            shutil.copytree(saved, self.config["root"])
            left = []

            def interrupted(target, members, *modes):
                write = copy_members if modes else materialize
                name = os.path.basename(target)
                matched = ((kind == "download" and name.startswith(".staging-collect-")) or
                           (kind == "authority" and name.startswith(".staging-authority-")) or
                           (kind == "retained" and os.path.dirname(target).endswith("/authorities")) or
                           (kind == "import" and os.path.dirname(target).endswith("/results")
                            and not name.startswith((".staging-collect-", ".staging-authority-"))))
                if matched:
                    path = next(iter(members))
                    write(target, {path: members[path][:17]}, *modes)
                    left.append(target)
                    # Leave this actual partial tree for restart, as an expired terminal
                    # budget would. Preserve the initial incomplete publication too.
                    raise budget.Interrupted("materialization interrupted")
                return write(target, members, *modes)

            cleanup = telemetrylab.HistoryRoot.cleanup_staging
            def stop_cleanup(kept):
                if left:
                    raise budget.Interrupted("cleanup interrupted")
                return cleanup(kept)

            with self.subTest(kind=kind), self.watch(), \
                    mock.patch.object(transport, "_materialize", interrupted), \
                    mock.patch.object(telemetrylab, "_write_members", interrupted), \
                    mock.patch.object(telemetrylab.HistoryRoot, "cleanup_staging", stop_cleanup), \
                    mock.patch.object(shutil, "rmtree", side_effect=budget.Interrupted("cleanup interrupted")):
                # rmtree is used only after materialization (initial cleanup is empty).
                doc = self.attempt(data)
            self.assertTrue(left, kind)
            self.assertTrue(os.path.isdir(left[0]))
            self.assertEqual(doc["status"], "failed")
            status = contracts.read_file(os.path.join(self.config["root"], "collection.json"),
                                         "telemetry-collection")
            self.assertEqual(status["status"], "failed")
            self.assertIsNone(status["used_bytes"])
            with self.watch():
                recovered = self.attempt(data)
            self.assertEqual(recovered["runs"][0]["outcome"], "collected", recovered)
            self.assertEqual(recovered["used_bytes"], self.usage())
            self.assertFalse(any(name.startswith(".") for name in os.listdir(
                os.path.join(self.config["root"], "results"))))

    def test_retained_authority_and_import_rounding_are_admitted_together(self):
        source = self.result(701)
        result, _, hashes = telemetrylab.verify(source)
        self.pad(1 << 20)
        # Keep a real downloaded copy under the same root while both retained copies
        # would be written. Standalone import must enforce the same peak as Cycle.
        with telemetrylab.HistoryRoot(self.config["root"]) as kept:
            downloaded = os.path.join(kept.results, telemetrylab.STAGING_PREFIX + "download")
            members = telemetrylab.read_files(source, list(hashes) + ["manifest.json"],
                                               telemetrylab.BUNDLE_BYTES_MAX)
            kept.reserve_trees([(downloaded, members)])
            transport._materialize(downloaded, members)
            baseline = kept.used_bytes()
            registry, _ = telemetrylab.load_registry()
            logical = sum(os.stat(os.path.join(ROOT, name)).st_size for name in
                          telemetrylab.authority_files(registry))
            self.config["bytes_max"] = baseline + logical
            kept.bytes_max = self.config["bytes_max"]
            with self.watch(), mock.patch.object(telemetrylab, "_write_members",
                                                 wraps=telemetrylab._write_members) as copied:
                with self.assertRaisesRegex(ContractError, "byte bound"):
                    kept.retain_authority(result, ROOT)
                with self.assertRaisesRegex(ContractError, "byte bound"):
                    kept.import_result(downloaded)
            copied.assert_not_called()
            self.assertEqual(kept.used_bytes(), baseline)
            self.assertTrue(os.path.isdir(downloaded))

    def test_parent_directory_growth_and_small_file_rounding_have_one_reservation(self):
        self.pad(1 << 20)
        self.config["bytes_max"] = 8 << 20
        with telemetrylab.HistoryRoot(self.config["root"], bytes_max=self.config["bytes_max"]) as kept:
            # A parent whose name table needs multiple blocks: the old logical byte sum
            # missed both this growth and 600 rounded allocations for one-byte files.
            for index in range(180):
                with open(os.path.join(kept.results, ".staging-pad-%04d" % index), "xb"):
                    pass
            target = os.path.join(kept.results, ".staging-many")
            members = {"long-entry-%04d" % index: b"x" for index in range(600)}
            baseline = kept.used_bytes()
            required = kept.reserve_trees([(target, members)])
            self.assertGreater(required, len(members) * kept.report_allocation(1))
            kept.bytes_max = self.config["bytes_max"] = baseline + required - 1
            with self.watch():
                with self.assertRaisesRegex(ContractError, "byte bound"):
                    kept.reserve_trees([(target, members)])
                self.assertFalse(os.path.lexists(target))
            kept.bytes_max = self.config["bytes_max"] = baseline + required
            with self.watch() as samples:
                kept.reserve_trees([(target, members)])
                transport._materialize(target, members)
            self.assertGreater(max(samples), baseline + len(members))
            self.assertEqual(len(os.listdir(target)), 600)

    def test_git_authority_is_fully_validated_and_bounded_before_mkdir(self):
        from perflab import gitrefs
        git = gitrefs.Git(self.repo.path)
        real_entries = git.tree_entries
        entries = real_entries(self.authority, [telemetrylab.REGISTRY_PATH])
        target = os.path.join(self.disk, "authority")
        for mutation in ("special", "missing", "duplicate", "collision", "escape"):
            def altered(sha, prefixes):
                rows = real_entries(sha, prefixes)
                if prefixes == [telemetrylab.REGISTRY_PATH]: return rows
                if mutation == "special": rows[-1] = ("120000",) + rows[-1][1:]
                elif mutation == "missing": rows = [r for r in rows if r[3] != "Cargo.lock"]
                elif mutation == "duplicate": rows.append(rows[-1])
                elif mutation == "collision": rows.append(("100644", "blob", entries[0][2], "schemas"))
                else: rows.append(("100644", "blob", entries[0][2], "../escape"))
                return rows
            with self.subTest(mutation=mutation), mock.patch.object(git, "tree_entries", altered):
                with self.assertRaises(ContractError):
                    telemetrylab.materialize_authority(git, self.authority, target)
            self.assertFalse(os.path.lexists(target))
        with mock.patch.object(telemetrylab, "AUTHORITY_BYTES_MAX", 10):
            with self.assertRaises(ContractError):
                telemetrylab.materialize_authority(git, self.authority, target)
        self.assertFalse(os.path.lexists(target))
