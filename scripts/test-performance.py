#!/usr/bin/env python3
"""Run the performance laboratory's deterministic tests; they never run a benchmark."""
import os
import sys
import unittest

sys.dont_write_bytecode = True
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LAB = os.path.join(ROOT, "scripts", "performance")
sys.path.insert(0, LAB)


def main():
    tests = unittest.defaultTestLoader.discover(os.path.join(LAB, "tests"), top_level_dir=LAB)
    result = unittest.TextTestRunner(verbosity=1).run(tests)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
