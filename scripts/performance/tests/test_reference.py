"""The independent reference generator: its encoder, exact sizes and the committed golden file."""
import hashlib
import json
import os
import re
import subprocess
import sys
import unittest

from perflab import contracts, oracle, reference, suite
from perflab.values import ContractError
from tests.support import CLI, ROOT, SUITE, read

PROBE = os.path.join(ROOT, "crates/review-pipeline/examples/perf-probe")
GOLDEN = os.path.join(ROOT, "fixtures/performance/golden/native.json")


class Encoder(unittest.TestCase):
    def test_known_values_have_their_hand_written_canonical_spelling(self):
        self.assertEqual(reference.canonical({"b": [1, -2, True, None], "a": {"z": "x", "y": 0}}),
                         b'{"a":{"y":0,"z":"x"},"b":[1,-2,true,null]}')
        self.assertEqual(reference.canonical(reference.POLICY_DOCUMENT),
                         b'{"perf_probe":"trusted fixture policy"}')
        self.assertEqual(reference.content_id(b""), "sha256:" + hashlib.sha256(
            b"review.kernel/content-id/v1\0").hexdigest())

    def test_everything_outside_the_restricted_domain_is_refused(self):
        for value in ({"k": 1.5}, {"k": "café"}, {"k": 'quote"'}, {"k": "back\\slash"},
                      {"k": "line\n"}, {"é": 1}, {"k": 2 ** 53}, {"k": b"bytes"},
                      {"k": (1, {2})}):
            with self.subTest(value=value):
                with self.assertRaises(ContractError):
                    reference.canonical(value)

    def test_the_encoder_never_delegates_to_json(self):
        source = read(os.path.join(ROOT, "scripts/performance/perflab/reference.py")).decode()
        self.assertNotIn("import json", source)
        self.assertNotIn("json.dumps", source)


class Fixtures(unittest.TestCase):
    def setUp(self):
        self.doc = suite.load(SUITE).doc

    def test_every_typed_envelope_has_exactly_its_declared_full_size(self):
        golden = reference.golden_document(self.doc)
        for fixture_id, fixture in self.doc["fixtures"].items():
            if fixture["kind"] != "typed_envelope":
                continue
            envelope, _ = reference._envelope_fixture(fixture_id, fixture)
            # Full typed envelope, not the payload: the payload is strictly smaller.
            self.assertEqual(len(envelope.bytes), fixture["serialized_envelope_bytes"])
            self.assertEqual(golden["fixtures"][fixture_id]["serialized_envelope_bytes"],
                             fixture["serialized_envelope_bytes"])
        self.assertEqual(golden["fixtures"]["issue_255_large"]["serialized_envelope_bytes"],
                         5605230)

    def test_the_cap_boundary_is_the_production_cap_and_one_byte_more(self):
        cap = reference.golden_document(self.doc)["cap_boundary"]
        self.assertEqual(cap["admitted"]["serialized_envelope_bytes"], 8388608)
        self.assertEqual(cap["refused"]["serialized_envelope_bytes"], 8388609)
        self.assertEqual(len(reference.requirements_envelope(
            cap["refused"]["text_bytes"]).bytes), 8388609)

    def test_source_files_are_deterministic_128_byte_files(self):
        for index in (0, 99, 100, 9999):
            path, data = reference.source_file(index)
            self.assertEqual(len(data), 128)
            self.assertTrue(data.endswith(b"\n"))
            self.assertRegex(path, r"^src/d[0-9]{3}/f[0-9]{5}\.txt$")
        self.assertNotEqual(reference.source_file(1)[1], reference.source_file(2)[1])

    def test_the_probe_constants_are_the_reference_solutions(self):
        source = read(os.path.join(PROBE, "fixtures.rs")).decode()
        golden = reference.golden_document(self.doc)
        self.assertIn('"%s"' % reference.policy_id(), source)
        small = golden["fixtures"]["small_envelope"]["generation"]
        self.assertIn("Envelope::Requirements { text_bytes: %d }" % small["text_bytes"], source)
        for fixture_id in ("issue_255_medium", "issue_255_large"):
            generation = golden["fixtures"][fixture_id]["generation"]
            block = re.search(r'id: "%s",.*?digits: ([0-9_]+),\s*tens: ([0-9]+)' % fixture_id,
                              source, re.S)
            self.assertEqual((int(block.group(1).replace("_", "")), int(block.group(2))),
                             (generation["padding_digits"], generation["padding_tens"]))

    def test_the_committed_golden_file_is_the_reference_output(self):
        golden = oracle.load_golden(GOLDEN)
        oracle.check_reference(self.doc, golden)
        oracle.check_coverage(self.doc, golden)
        self.assertEqual(read(GOLDEN), contracts.write_bytes(golden, "golden"))
        result = subprocess.run([sys.executable, "-I", CLI, "golden"], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_tampered_golden_entry_is_refused(self):
        golden = json.loads(read(GOLDEN))
        entry = [item for item in golden["entries"]
                 if item["contract_id"] == "canonical_serialize_v1"][0]
        entry["digest"] = "sha256:" + "0" * 64
        contracts.validate_golden(golden)
        with self.assertRaises(ContractError) as caught:
            oracle.check_reference(self.doc, golden)
        self.assertIn("is not what perflab.reference derives", str(caught.exception))
        golden = json.loads(read(GOLDEN))
        golden["entries"].pop()
        with self.assertRaises(ContractError) as caught:
            oracle.check_coverage(self.doc, golden)
        self.assertIn("no golden entry for", str(caught.exception))

    def test_golden_entries_cover_every_native_instance_and_variant(self):
        golden = oracle.load_golden(GOLDEN)
        ids = [entry["instance_id"] for entry in golden["entries"]]
        for expected in ("source_capture__files_10000_tree_source_files_128b",
                         "source_capture__files_100_tree_source_files_128b",
                         "state_projection__log_events_10000_projection_application_cold",
                         "state_projection__log_events_10000_projection_prewarmed",
                         "authority_runtime_new__package_issue_255_large",
                         "authority_plan_check__package_issue_255_medium",
                         "persistence_fs__operation_state_append_read", "sentinel_canonical"):
            self.assertIn(expected, ids)
        self.assertFalse(any(entry["case_id"].startswith("cli_") for entry in golden["entries"]))
        authority = dict((entry["instance_id"], entry) for entry in golden["entries"])[
            "authority_runtime_new__package_issue_255_large"]
        self.assertEqual(authority["semantic"]["graph_order"],
                         ["root.inputs", "root.nodes.draft", "root.nodes.check"])
        self.assertEqual(authority["count"], 5605230)
        self.assertEqual(authority["fixture_digest"],
                         authority["semantic"]["packages"]["perf/author"]["artifact_id"])
        state = dict((entry["instance_id"], entry) for entry in golden["entries"])[
            "state_projection__log_events_10000_projection_prewarmed"]
        self.assertEqual((state["count"], state["semantic"]["events"]), (10000, 10000))


if __name__ == "__main__":
    unittest.main()
