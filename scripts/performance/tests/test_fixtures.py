"""Integration fixtures: SYNTHETIC run bundles, generated at test time, reproduce their
committed golden verdicts (ADR-0179)."""
import hashlib
import json
import os
import subprocess
import sys
import unittest

from perflab import compare
from perflab.values import document_bytes
from tests import compare_fixtures
from tests.support import CLI, TempDir, read


def tree(root):
    out = {}
    for directory, _, files in os.walk(root):
        for name in files:
            path = os.path.join(directory, name)
            out[os.path.relpath(path, root)] = read(path)
    return out


class CommittedComparisons(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.index = json.loads(read(os.path.join(compare_fixtures.TARGET, "index.json")))
        # The run bundles each golden verdict judges, generated here, never committed.
        self.generated = self.path("generated")
        compare_fixtures.write_all(self.generated)

    def test_every_fixture_is_labeled_synthetic(self):
        self.assertIn("SYNTHETIC", self.index["note"])
        self.assertEqual(sorted(entry["name"] for entry in self.index["fixtures"]),
                         sorted(compare_fixtures.FIXTURES))
        for entry in self.index["fixtures"]:
            self.assertTrue(entry["synthetic"])
            verdict = json.loads(read(os.path.join(compare_fixtures.TARGET, entry["name"],
                                                   "verdict.json")))
            self.assertIn(verdict["provenance"], ("synthetic", "none"))
            self.assertFalse(verdict["aggregate"]["satisfies_required_check"])

    def test_compare_reproduces_each_recorded_verdict_and_exit_code(self):
        for entry in self.index["fixtures"]:
            with self.subTest(fixture=entry["name"]):
                run = os.path.join(self.generated, entry["name"], "run")
                expected = read(os.path.join(compare_fixtures.TARGET, entry["name"],
                                             "verdict.json"))
                self.assertEqual(document_bytes(compare.compare(run)), expected)
                result = subprocess.run([sys.executable, "-I", CLI, "compare", "--run", run],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, entry["exit_code"])
                self.assertEqual(result.stdout, expected)

    def test_fixtures_regenerate_byte_for_byte(self):
        # Only the goldens are committed, and the generated tree reproduces them exactly; each
        # verdict's bundle identities pin the generated runs' bytes.
        generated = tree(self.generated)
        self.assertEqual(tree(compare_fixtures.TARGET),
                         dict((relative, generated[relative])
                              for relative in compare_fixtures.goldens()))
        for entry in self.index["fixtures"]:
            with self.subTest(fixture=entry["name"]):
                verdict = json.loads(generated["%s/verdict.json" % entry["name"]])
                run = os.path.join(self.generated, entry["name"], "run")
                with open(os.path.join(run, "manifest.json"), "rb") as stream:
                    self.assertEqual(verdict["inputs"]["run_bundle"],
                                     "sha256:" + hashlib.sha256(stream.read()).hexdigest())


if __name__ == "__main__":
    unittest.main()
