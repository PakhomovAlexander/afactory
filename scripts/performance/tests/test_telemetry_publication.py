"""Interrupted report publication: real partial writes, replace failures and locked restart."""
import builtins
import os
from unittest import mock

from perflab import budget, contracts, telemetrylab
from perflab.values import ContractError
from tests.support import TempDir, read


class Publication(TempDir):
    def setUp(self):
        super().setUp()
        self.root = self.path("history")
        with telemetrylab.HistoryRoot(self.root) as kept:
            doc = kept.write()
            self.assertEqual(doc["used_bytes"], kept.used_bytes())
        self.before = {name: read(os.path.join(self.root, name)) for name in (
            telemetrylab.HISTORY_JSON, telemetrylab.HISTORY_HTML)}

    def stage_files(self):
        return sorted(os.listdir(os.path.join(self.root, telemetrylab.REPORT_STAGE)))

    def test_repeated_write_and_replace_interruptions_both_outputs_restart_bounded(self):
        real_open, real_replace = builtins.open, os.replace
        for kind in ("write", "replace"):
            for name in (telemetrylab.HISTORY_JSON, telemetrylab.HISTORY_HTML):
                with self.subTest(kind=kind, name=name):
                    for iteration in range(3):
                        with telemetrylab.HistoryRoot(self.root) as kept:
                            self.assertEqual(self.stage_files(), ["owner"])
                            before = {key: read(os.path.join(self.root, key)) for key in self.before}
                            baseline = kept.used_bytes()

                            class InterruptedWriter:
                                def __init__(self, stream):
                                    self.stream = stream
                                def __enter__(self):
                                    self.stream.__enter__()
                                    return self
                                def __exit__(self, *args):
                                    return self.stream.__exit__(*args)
                                def write(self, data):
                                    self.stream.write(data[:7])
                                    self.stream.flush()
                                    raise budget.Interrupted("synthetic interrupted report write")

                            def opened(path, *args, **kwargs):
                                stream = real_open(path, *args, **kwargs)
                                if str(path) == os.path.join(self.root, telemetrylab.REPORT_STAGE, name):
                                    return InterruptedWriter(stream)
                                return stream

                            def replaced(source, target):
                                if str(target) == os.path.join(self.root, name):
                                    raise budget.Interrupted("synthetic interrupted report replace")
                                return real_replace(source, target)

                            with mock.patch("builtins.open", opened if kind == "write" else real_open), \
                                    mock.patch.object(os, "replace", replaced if kind == "replace" else real_replace):
                                with self.assertRaises(budget.Interrupted):
                                    kept.write([{"input": "test", "reason": "cycle %d" % iteration}])
                            self.assertLessEqual(len(self.stage_files()), 3)  # owner + two fixed outputs
                            self.assertGreater(kept.used_bytes(), baseline)
                            self.assertLessEqual(kept.used_bytes(), kept.bytes_max)
                            self.assertEqual(read(os.path.join(self.root, name)), before[name])
                            # Every published JSON is still a complete valid document.
                            contracts.read_file(os.path.join(self.root, telemetrylab.HISTORY_JSON),
                                                "telemetry-history")
                            self.assertTrue(read(os.path.join(self.root, telemetrylab.HISTORY_HTML)).endswith(
                                b"</html>\n"))
                    with telemetrylab.HistoryRoot(self.root) as kept:
                        self.assertEqual(self.stage_files(), ["owner"])
                        doc = kept.write()
                        self.assertEqual(doc["used_bytes"], kept.used_bytes())
        self.assertFalse(any(".tmp-" in name for name in os.listdir(self.root)))

    def test_capacity_includes_complete_reports_and_both_staged_outputs_exactly(self):
        # Keep the exact-capacity fixture within the unchanged schema minimum.
        with open(os.path.join(self.root, "retained-foreign"), "wb") as stream:
            stream.write(b"x" * (1 << 20))
        # Measure the real peak while both new outputs coexist with both old reports.
        with telemetrylab.HistoryRoot(self.root) as kept:
            before = kept.used_bytes()
            with mock.patch.object(os, "replace", side_effect=OSError("replace failed")):
                with self.assertRaises(OSError):
                    kept.write()
            peak = kept.used_bytes()
            self.assertGreater(peak, before)
        for capacity, succeeds in ((peak - 1, False), (peak, True)):
            with self.subTest(capacity=capacity), telemetrylab.HistoryRoot(
                    self.root, bytes_max=capacity) as kept:
                if succeeds:
                    doc = kept.write()
                    self.assertEqual(doc["used_bytes"], kept.used_bytes())
                    self.assertLessEqual(kept.used_bytes(), capacity)
                else:
                    with self.assertRaisesRegex(ContractError, "history byte bound"):
                        kept.write()
                    self.assertEqual(self.stage_files(), ["owner"])
                    for name, data in self.before.items():
                        self.assertEqual(read(os.path.join(self.root, name)), data)

    def test_foreign_paths_are_counted_never_swept_and_links_are_refused(self):
        foreign = self.path("history", "history.json.tmp-foreign")
        with open(foreign, "wb") as stream:
            stream.write(b"retained foreign bytes" * 4096)
        with telemetrylab.HistoryRoot(self.root) as kept:
            self.assertTrue(os.path.isfile(foreign))
            self.assertGreaterEqual(kept.used_bytes(), os.stat(foreign).st_size)
            kept.write()
        stage = os.path.join(self.root, telemetrylab.REPORT_STAGE)
        outsider = self.path("outsider")
        with open(outsider, "wb") as stream:
            stream.write(b"do not touch")
        os.symlink(outsider, os.path.join(stage, telemetrylab.HISTORY_JSON))
        with self.assertRaisesRegex(ContractError, "never links"):
            with telemetrylab.HistoryRoot(self.root):
                pass
        self.assertEqual(read(outsider), b"do not touch")
        os.unlink(os.path.join(stage, telemetrylab.HISTORY_JSON))
        with open(os.path.join(stage, "foreign"), "wb") as stream:
            stream.write(b"foreign")
        with self.assertRaisesRegex(ContractError, "foreign report staging entry"):
            with telemetrylab.HistoryRoot(self.root):
                pass
        self.assertTrue(os.path.isfile(os.path.join(stage, "foreign")))


class CollectionCapacity(TempDir):
    """Drive the real Cycle, including initial/position/report/terminal publication."""
    def setUp(self):
        super().setUp()
        self.root = self.path("history")
        os.mkdir(self.root)
        with open(os.path.join(self.root, "foreign-padding"), "wb") as stream:
            stream.write(b"x" * (1 << 20))
        self.capacity = 1 << 28

    def cycle(self, count=0, refused=False):
        import time
        from types import SimpleNamespace
        from perflab import telemetrycollect, transport
        from tests.test_telemetry_collect import FakeApi, REPOSITORY, API, TOKEN, NOW, run_entry
        api = transport.Collector(FakeApi([run_entry(i) for i in range(1, count + 1)], {}),
                                  REPOSITORY, TOKEN, api_url=API)
        config = {"root": self.root, "bytes_max": self.capacity, "results_max": 100,
                  "retention_days": None, "repository": REPOSITORY,
                  "lookback_days": 13, "runs_max": 200}
        cycle = telemetrycollect.Cycle(config, api, SimpleNamespace(), lambda: 1700000000, NOW)
        if refused:
            def collect(kept, run):
                cycle.record(run, 1, "refused", reason="x" * 4096)
            cycle.collect = collect
        return cycle.run()

    def usage(self):
        return telemetrylab.HistoryRoot(self.root, sweep=False).used_bytes()

    def status(self):
        return contracts.read_file(os.path.join(self.root, "collection.json"), "telemetry-collection")

    def test_first_publication_and_growth_exact_peak_and_one_byte_short(self):
        import shutil
        for initial, count in ((True, 1), (False, 300)):
            with self.subTest(initial=initial):
                if not initial:
                    self.cycle(1)
                saved = self.path("saved-%s" % initial)
                shutil.copytree(self.root, saved)
                observed = []
                original = telemetrylab.HistoryRoot.used_bytes
                def usage(kept):
                    size = original(kept)
                    observed.append(size)
                    return size
                with mock.patch.object(telemetrylab.HistoryRoot, "used_bytes", usage):
                    doc = self.cycle(count, refused=not initial)
                self.assertEqual(doc["used_bytes"], self.usage())
                peak = max(observed)
                for capacity, succeeds in ((peak, True), (peak - 1, False)):
                    shutil.rmtree(self.root)
                    shutil.copytree(saved, self.root)
                    self.capacity = capacity
                    result = self.cycle(count, refused=not initial)
                    self.assertLessEqual(self.usage(), capacity)
                    if succeeds:
                        self.assertEqual(result["used_bytes"], self.usage())
                    else:
                        self.assertEqual(result["status"], "failed")
                        self.assertIn("history byte bound", result["detail"])
                        self.assertEqual(self.status()["status"], "failed")
                        # Reports are complete even if terminal status could not be staged.
                        if os.path.exists(os.path.join(self.root, telemetrylab.HISTORY_JSON)):
                            contracts.read_file(os.path.join(self.root, telemetrylab.HISTORY_JSON),
                                                "telemetry-history")
                self.capacity = 1 << 28

    def test_full_root_invalidates_previous_success_without_allocating(self):
        self.cycle(1)
        self.assertEqual(self.status()["status"], "ok")
        self.capacity = self.usage()
        doc = self.cycle(300, refused=True)
        self.assertEqual(doc["status"], "failed")
        self.assertEqual(self.status()["status"], "failed")
        self.assertLessEqual(self.usage(), self.capacity)

    def test_status_and_position_write_replace_interruptions_and_restart(self):
        from perflab import telemetrycollect
        self.cycle(1)
        real_open, real_replace = builtins.open, os.replace
        for name in ("collection.json", "collection-position.json"):
            for kind in ("write", "replace"):
                with self.subTest(name=name, kind=kind):
                    stage = os.path.join(self.root, telemetrylab.REPORT_STAGE, name)
                    class InterruptedWriter:
                        def __init__(self, stream): self.stream = stream
                        def __enter__(self): return self
                        def __exit__(self, *args): return self.stream.__exit__(*args)
                        def write(self, data):
                            # Let the compact initial failure record finish.
                            if name == "collection.json" and len(data) < 4096:
                                return self.stream.write(data)
                            self.stream.write(data[:7])
                            self.stream.flush()
                            raise budget.Interrupted("partial collection write")
                        def __getattr__(self, key): return getattr(self.stream, key)
                    def opened(path, *args, **kwargs):
                        stream = real_open(path, *args, **kwargs)
                        return InterruptedWriter(stream) if path == stage else stream
                    def replaced(source, target):
                        if source == stage and (name != "collection.json" or os.stat(source).st_size > 4096):
                            raise budget.Interrupted("collection replacement interrupted")
                        return real_replace(source, target)
                    with mock.patch("builtins.open", opened if kind == "write" else real_open), \
                            mock.patch.object(os, "replace", replaced if kind == "replace" else real_replace):
                        doc = self.cycle(10, refused=True)
                    self.assertEqual(doc["status"], "failed")
                    self.assertEqual(self.status()["status"], "failed")
                    self.assertLessEqual(self.usage(), self.capacity)
                    result = self.cycle(1)
                    self.assertEqual(result["status"], "ok")
                    self.assertEqual(result["used_bytes"], self.usage())
                    self.assertFalse(os.path.lexists(stage))

    def test_foreign_temporaries_and_symlink_paths_are_never_followed_or_swept(self):
        self.cycle(1)
        sentinel = self.path("sentinel")
        with open(sentinel, "wb") as stream:
            stream.write(b"external sentinel")
        foreign = os.path.join(self.root, "collection-position.json.tmp")
        with open(foreign, "wb") as stream:
            stream.write(b"foreign bytes")
        for relative in ("collection.json", "collection-position.json",
                         telemetrylab.REPORT_STAGE + "/collection.json"):
            path = os.path.join(self.root, relative)
            if os.path.lexists(path): os.unlink(path)
            os.symlink(sentinel, path)
            try:
                try:
                    doc = self.cycle(1)
                    self.assertEqual(doc["status"], "failed")
                except (ContractError, OSError):
                    pass
                self.assertEqual(read(sentinel), b"external sentinel")
                self.assertTrue(os.path.isfile(foreign))
            finally:
                os.unlink(path)

    def test_first_publication_full_root_refuses_before_ownership_allocation(self):
        self.capacity = 1 << 20
        before = self.usage()
        with self.assertRaisesRegex(ContractError, "history byte bound"):
            self.cycle()
        self.assertEqual(self.usage(), before)
        self.assertFalse(os.path.exists(os.path.join(self.root, "collection.json")))

    def test_interrupted_initial_reserve_write_restarts_from_failed_record(self):
        real_open = builtins.open
        reserve = os.path.join(self.root, telemetrylab.REPORT_STAGE, "collection-incomplete.json")
        class InterruptedWriter:
            def __init__(self, stream): self.stream = stream
            def __enter__(self): return self
            def __exit__(self, *args): return self.stream.__exit__(*args)
            def write(self, data):
                self.stream.write(data[:7])
                self.stream.flush()
                raise budget.Interrupted("partial initial reserve")
        def opened(path, *args, **kwargs):
            stream = real_open(path, *args, **kwargs)
            return InterruptedWriter(stream) if path == reserve and "x" in args[0] else stream
        with mock.patch("builtins.open", opened):
            result = self.cycle()
        self.assertEqual(result["status"], "failed")
        self.assertEqual(self.status()["status"], "failed")
        result = self.cycle(1)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["used_bytes"], self.usage())
