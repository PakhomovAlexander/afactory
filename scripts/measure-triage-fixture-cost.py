#!/usr/bin/env python3
"""Manually measure the full hermetic triage suite; never part of a gate or runner.

Use the same interpreter for each source root. Count successful Git Popen launches,
including fixture setup and production adapters, without replacing any Git results.
"""
import argparse
import importlib.util
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path, help='source tree containing the full runner suite')
    args = parser.parse_args()
    suite_path = args.root.resolve() / 'scripts/test-backlog-triage.py'
    with tempfile.TemporaryDirectory(prefix='triage-cost-') as scratch:
        # The old fixture inherited Git config. Give both sides identical private
        # homes and disable hooks/signing even when measuring that unmodified source.
        os.environ.clear()
        os.environ.update(PATH='/usr/bin:/bin:/usr/sbin:/sbin', HOME=scratch, TMPDIR=scratch,
                          XDG_CONFIG_HOME=scratch, XDG_CACHE_HOME=scratch, XDG_STATE_HOME=scratch,
                          LC_ALL='C', TZ='UTC', GIT_CONFIG_NOSYSTEM='1',
                          GIT_CONFIG_SYSTEM=os.devnull, GIT_CONFIG_GLOBAL=os.devnull,
                          GIT_CONFIG_COUNT='3', GIT_CONFIG_KEY_0='core.hooksPath',
                          GIT_CONFIG_VALUE_0=os.devnull, GIT_CONFIG_KEY_1='commit.gpgsign',
                          GIT_CONFIG_VALUE_1='false', GIT_CONFIG_KEY_2='tag.gpgsign',
                          GIT_CONFIG_VALUE_2='false')
        # Audit notifications occur before launch. Count only after Popen succeeds.
        git_launches = []
        original_popen = subprocess.Popen

        class CountedPopen(original_popen):
            def __init__(self, argv, *args, **kwargs):
                super().__init__(argv, *args, **kwargs)
                if isinstance(argv, (list, tuple)) and Path(argv[0]).name == 'git':
                    git_launches.append(1)

        subprocess.Popen = CountedPopen
        started = time.perf_counter()
        try:
            spec = importlib.util.spec_from_file_location('measured_triage_suite', suite_path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromModule(module))
        finally:
            elapsed = time.perf_counter() - started
            subprocess.Popen = original_popen
        print('interpreter: ' + sys.executable + ' (' + platform.python_version() + ')')
        print('host: ' + platform.platform() + ' ' + platform.machine())
        print('tests: %d; failures: %d; errors: %d; skipped: %d' %
              (result.testsRun, len(result.failures), len(result.errors), len(result.skipped)))
        print('git_processes: %d; wall_seconds: %.6f' % (len(git_launches), elapsed))
        return 0 if result.wasSuccessful() else 1


if __name__ == '__main__':
    sys.exit(main())
