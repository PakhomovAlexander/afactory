"""Shared value rules: IDs, canonical decimals and rationals, JSON and paths."""
from fractions import Fraction
import unittest

from perflab import values
from perflab.values import ContractError


class Identifiers(unittest.TestCase):
    def test_ids_follow_the_one_rule(self):
        for good in ("a", "a1", "cli_discovery__command_task_help", "a" * 64):
            self.assertEqual(values.check_id(good, "id"), good)
        for bad in ("", "A", "1a", "_a", "a-b", "a" * 65, "a.b", None, 3):
            with self.assertRaises(ContractError):
                values.check_id(bad, "id")

    def test_git_shas_are_full_and_lowercase(self):
        values.check_git_sha("a" * 40, "sha")
        values.check_git_sha("b" * 64, "sha")
        for bad in ("a" * 39, "A" * 40, "a" * 41, "g" * 40):
            with self.assertRaises(ContractError):
                values.check_git_sha(bad, "sha")


class Decimals(unittest.TestCase):
    def test_canonical_decimals_round_trip_exactly(self):
        for text, number in (("0", 0), ("1", 1), ("12.34", Fraction(1234, 100)),
                             ("0.001", Fraction(1, 1000)), ("5605230", 5605230)):
            self.assertEqual(values.parse_decimal(text, "d"), number)
            self.assertEqual(values.format_decimal(number), text)

    def test_noncanonical_and_malformed_numbers_are_refused(self):
        for bad in ("00", "01", "1.0", "1.50", "0.0", "-1", "+1", "1e3", "1E-3", ".5", "5.",
                    " 1", "1 ", "NaN", "inf", "1_000", "", "0x10", "١"):
            with self.assertRaises(ContractError, msg=bad):
                values.check_decimal(bad, "d")

    def test_at_most_38_significant_digits(self):
        values.check_decimal("1" * 38, "d")
        values.check_decimal("0.000" + "1" * 38, "d")
        with self.assertRaises(ContractError):
            values.check_decimal("1" * 39, "d")
        with self.assertRaises(ContractError):
            values.check_decimal("1." + "1" * 38, "d")

    def test_nonterminating_or_negative_values_have_no_metric_decimal(self):
        with self.assertRaises(ContractError):
            values.format_decimal(Fraction(1, 3))
        with self.assertRaises(ContractError):
            values.format_decimal(Fraction(-1, 2))


class Rationals(unittest.TestCase):
    def test_lowest_terms_signed_rationals(self):
        for text, number in (("0", 0), ("-3/4", Fraction(-3, 4)), ("7", 7),
                             ("1/40", Fraction(1, 40))):
            self.assertEqual(values.parse_rational(text, "r"), number)
            self.assertEqual(values.format_rational(number), text)
        for bad in ("-0", "2/4", "3/1", "1/0", "1/-2", "0.5", "--1", "1/", "/2", "01"):
            with self.assertRaises(ContractError, msg=bad):
                values.check_rational(bad, "r")


class Json(unittest.TestCase):
    def test_strict_reader_refuses_floats_duplicates_constants_and_boms(self):
        for bad in (b'{"a":1.5}', b'{"a":1,"a":2}', b'{"a":NaN}', b'{"a":Infinity}',
                    b'\xef\xbb\xbf{}', b'{"a":1e3}', b'\xff', b'{'):
            with self.assertRaises(ContractError, msg=bad):
                values.loads_strict(bad, "doc", 1024)
        with self.assertRaises(ContractError):
            values.loads_strict(b'{"a":"' + b"x" * 2000 + b'"}', "doc", 1024)

    def test_canonical_bytes_sort_keys_and_refuse_floats(self):
        self.assertEqual(values.canonical_bytes({"b": [2, 1], "a": "é"}),
                         '{"a":"é","b":[2,1]}'.encode("utf-8"))
        with self.assertRaises(ContractError):
            values.canonical_bytes({"a": 1.0})


class Paths(unittest.TestCase):
    def test_relative_paths_only(self):
        values.check_relative_path("samples.jsonl", "p")
        values.check_relative_path("logs/a-1/b_2.txt", "p")
        for bad in ("", "/abs", "../x", "a/../b", "a/./b", "./a", "a//b", "a/", "a\\b",
                    "a\x00b", "a b", "~/x", "a/" * 600 + "b"):
            with self.assertRaises(ContractError, msg=bad):
                values.check_relative_path(bad, "p")


if __name__ == "__main__":
    unittest.main()
