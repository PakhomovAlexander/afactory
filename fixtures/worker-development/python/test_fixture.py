import unittest

from fixture import twice


class Fixture(unittest.TestCase):
    def test_twice_seven_is_fourteen(self):
        self.assertEqual(twice(7), 14)
