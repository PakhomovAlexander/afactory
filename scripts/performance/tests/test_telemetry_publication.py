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
