"""The independent reference generator: its encoder, exact sizes and the committed golden file."""
import copy
import hashlib
import json
import os
import re
import subprocess
import sys
import unittest
from unittest import mock

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

    def test_string_domain_matches_the_literal_ascii_rule_for_values_and_keys(self):
        self.assertEqual(reference.canonical(""), b'""')
        for number in list(range(256)) + [0x20ac, 0xd800, 0xdfff, 0xffff, 0x10000, 0x10ffff]:
            char = chr(number)
            admitted = 0x20 <= number <= 0x7e and char not in '"\\'
            for value in (char, {char: "value"}):
                with self.subTest(number=number, value=value):
                    if admitted:
                        expected = ('"' + char + '"' if isinstance(value, str)
                                    else '{"' + char + '":"value"}')
                        self.assertEqual(reference.canonical(value), expected.encode("ascii"))
                    else:
                        with self.assertRaisesRegex(ContractError, "printable ASCII without escapes"):
                            reference.canonical(value)

    def test_long_strings_still_refuse_an_invalid_character_at_either_end_or_inside(self):
        text = "abcdefghijklmnopqrstuvwxyz" * 4096
        self.assertEqual(reference.canonical(text), ('"' + text + '"').encode("ascii"))
        for bad in ('"', "\\", "\x00", "\n", "\x7f", "é", "\ud800"):
            for value in (bad + text, text + bad + text, text + bad):
                with self.assertRaisesRegex(ContractError, "printable ASCII without escapes"):
                    reference.canonical(value)
        for value in (None, 1, b"ascii"):
            with self.assertRaisesRegex(ContractError, "field: expected a string"):
                reference._string(value, "field")


class PeriodicFixtures(unittest.TestCase):
    def test_letters_and_padding_equal_the_independent_per_position_definition(self):
        for count in (-27, -1, 0, 1, 9, 10, 11, 25, 26, 27, 51, 52, 53, 4097, False, True):
            self.assertEqual(reference.letters(count),
                             "".join(chr(97 + index % 26) for index in range(count)))
            for tens in (-1, 0, 1, 2):
                self.assertEqual(reference.padding(count, tens),
                                 bytes(index % 10 for index in range(count)) + b"\n" * tens)

    def test_counts_keep_integer_index_and_unsupported_input_semantics(self):
        class Index:
            def __index__(self):
                return 27
        self.assertEqual(reference.letters(Index()), "abcdefghijklmnopqrstuvwxyz" + "a")
        self.assertEqual(reference.padding(Index(), 1), bytes(range(10)) * 2 + bytes(range(7))
                         + b"\n")
        for bad in (None, "3", 3.0, -1.0):
            with self.subTest(bad=bad):
                with self.assertRaises(TypeError):
                    reference.letters(bad)
                with self.assertRaises(TypeError):
                    reference.padding(bad, 0)
                with self.assertRaises(TypeError):
                    reference.padding(0, bad)

    def test_source_files_equal_the_independent_definition_for_both_declared_trees(self):
        for index in range(max(reference.SOURCE_TREES)):
            header = ("af performance source fixture file %05d\n" % index).encode("ascii")
            expected = header + bytes(97 + (index + fill) % 26
                                      for fill in range(127 - len(header))) + b"\n"
            self.assertEqual(reference.source_file(index),
                             ("src/d%03d/f%05d.txt" % (index // 100, index), expected))

    def test_source_padding_uses_the_current_fixture_size_including_empty_fill(self):
        for size in (0, 40, 41, 42, 67, 128, 257):
            with mock.patch.object(reference, "SOURCE_FILE_BYTES", size):
                for index in (0, 25, 26, 99, 100, 9999):
                    header = ("af performance source fixture file %05d\n" % index).encode("ascii")
                    expected = header + bytes(97 + (index + fill) % 26
                                              for fill in range(size - 1 - len(header))) + b"\n"
                    self.assertEqual(reference.source_file(index)[1], expected)


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

    def test_repeated_derivation_checks_the_exact_current_registry_and_seed_facts(self):
        original = oracle.load_golden(GOLDEN)
        first = reference.golden_document(self.doc)
        self.assertEqual(first, original)
        # Mutating one result must not establish authority for any later validation.
        first["fixtures"]["small_envelope"]["generation"]["text_bytes"] += 1
        first["seeds"][0]["digest"] = "sha256:" + "1" * 64
        with self.assertRaisesRegex(ContractError, "is not what perflab.reference derives"):
            oracle.check_reference(self.doc, first)
        self.assertEqual(reference.golden_document(self.doc), original)

        changed = copy.deepcopy(self.doc)
        changed["fixtures"]["small_envelope"]["serialized_envelope_bytes"] += 1
        changed["fixtures"]["issue_255_medium"]["serialized_envelope_bytes"] += 2
        changed["fixtures"]["events_10000"]["events"] += 1
        suite.validate(changed)
        expected = reference.golden_document(changed)
        self.assertNotEqual(expected, original)
        with self.assertRaisesRegex(ContractError, "is not what perflab.reference derives"):
            oracle.check_reference(changed, original)
        oracle.check_reference(changed, expected)
        for key in ("entries", "seeds"):
            before = dict((entry["instance_id"], entry) for entry in original[key])
            after = dict((entry["instance_id"], entry) for entry in expected[key])
            for ident, entry in before.items():
                if entry["parameters"].get("package") in ("small_envelope", "issue_255_medium"):
                    self.assertNotEqual(entry["fixture_digest"], after[ident]["fixture_digest"])
                if entry["parameters"].get("log") == "events_10000":
                    self.assertEqual(after[ident]["count"], entry["count"] + 1)
        # The very same mutable input is re-read, rather than keyed by object identity.
        changed["fixtures"]["small_envelope"]["serialized_envelope_bytes"] += 1
        with self.assertRaisesRegex(ContractError, "is not what perflab.reference derives"):
            oracle.check_reference(changed, expected)
        self.assertEqual(reference.golden_document(self.doc), original)

    def test_envelope_size_and_generator_refusals_still_fail_closed(self):
        base = len(reference.requirements_envelope(0).bytes)
        self.assertEqual(reference.solve_requirements(base), 0)
        with self.assertRaisesRegex(ContractError, "below the base"):
            reference.solve_requirements(base - 1)
        base = len(reference.package_envelope(reference.AUTHOR_PACKAGE,
                                             reference.author_files(reference.padding(1, 0))).bytes)
        self.assertEqual(reference.solve_padding(base), (1, 0))
        with self.assertRaisesRegex(ContractError, "below the package base"):
            reference.solve_padding(base - 1)
        with self.assertRaisesRegex(ContractError, "unreachable"):
            reference.solve_padding(base + 1)
        fixture = self.doc["fixtures"]["small_envelope"]
        with self.assertRaisesRegex(ContractError, "no reference generator"):
            reference._envelope_fixture("unsupported", fixture)
        with self.assertRaisesRegex(ContractError, "not a typed envelope"):
            reference._envelope_fixture("small_envelope", {"kind": "event_log"})

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
