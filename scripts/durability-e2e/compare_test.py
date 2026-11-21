"""Manual contract checks for the engine-independent comparison report."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from compare import SCHEMA, compare, load
from worker_failover import digest


def report(build):
    return {
        "schema": SCHEMA, "case": "ci-lost-green", "profile": "private",
        "inputs": {"workload_sha256": "a" * 64, "placement_sha256": "b" * 64,
                   "fault_sha256": "c" * 64, "clock": "wall", "seed": 1},
        "expectation": "recover", "fault_witnessed": True, "verdict": "pass",
        "violations": [], "metrics": {"terminal_s": 1.0},
        "evidence": ["trace.json"], "engine": {"build": build},
    }


class Comparison(unittest.TestCase):
    def read(self, entry):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "trial.json").write_text(json.dumps(entry))
            return load(root)

    def test_pair_checks_semantic_inputs_and_omits_unearned_delta(self):
        original = self.read(report("native"))
        entry = report("alternate")
        entry["metrics"]["terminal_s"] = 0.75
        result = compare(original, self.read(entry))
        self.assertEqual(result["pairs"][0]["delta"], {"terminal_s": -0.25})

        different = copy.deepcopy(entry)
        different["inputs"]["placement_sha256"] = "0" * 64
        self.assertEqual(compare(original, self.read(different))["pairs"][0]["comparison"],
                         "different_inputs")

        entry["verdict"] = "fail"
        entry["violations"] = ["progress after terminal"]
        result = compare(original, self.read(entry))
        self.assertEqual(result["after"]["violations"], 1)
        self.assertEqual(result["after"]["outcomes"]["recover"]["fail"], 1)
        self.assertEqual(result["pairs"][0]["delta"], {})

    def test_invalid_trial_cannot_enter_comparison_as_pass(self):
        entry = report("native")
        entry["fault_witnessed"] = False
        with self.assertRaisesRegex(ValueError, "missed fault"):
            self.read(entry)
        entry["fault_witnessed"] = True
        entry["violations"] = ["duplicated external check"]
        with self.assertRaisesRegex(ValueError, "safety violation"):
            self.read(entry)

    def test_checked_in_baseline_matches_pinned_manifests_and_verdicts(self):
        suite = Path(__file__).resolve().parent
        baseline = load(suite / "baselines/2026-11-20")
        expected = {"broker-clean": "pass", "broker-green": "pass",
                    "broker-red": "fail", "worker-failover": "unsupported"}
        self.assertEqual(len(baseline), len(expected))
        for name, verdict in expected.items():
            with self.subTest(manifest=name):
                manifest = json.loads((suite / "manifests" / (name + ".json")).read_bytes())
                case = manifest.get("case", "worker-loss-and-late-completion")
                entry = baseline[(case, manifest["profile"], manifest["seed"])]
                self.assertEqual(entry["verdict"], verdict)
                self.assertEqual(entry["expectation"], manifest.get("expectation", "recover"))
                self.assertEqual(entry["inputs"]["clock"], manifest["clock"])
                for section in ("workload", "fault", "placement"):
                    self.assertEqual(entry["inputs"][section + "_sha256"],
                                     digest(manifest[section]))
                for evidence in entry["evidence"]:
                    self.assertTrue((suite.parents[1] / evidence).is_file())


if __name__ == "__main__":
    unittest.main()
