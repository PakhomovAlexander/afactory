#!/usr/bin/env python3
"""Exercise the stress-proof entry point without running a test: its load generator leaves no
worker behind on any exit path (a normal end, SIGTERM, SIGKILL of the driver), and its table
reads nextest's JUnit and the sampled load the way the proof prints them."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

# The entry point is loaded as a module below; no bytecode is left beside it.
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts' / 'stress-proof.py'
SPEC = importlib.util.spec_from_file_location('stress_proof', SCRIPT)
stress = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stress)

# A driver that starts the load, prints its workers' pids and waits to be ended.
DRIVER = '''
import importlib.util, json, signal, sys, time
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('stress_proof', sys.argv[1])
stress = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stress)
for signum in (signal.SIGTERM, signal.SIGHUP):
    signal.signal(signum, stress.interrupt_as_exit)
with stress.Load(2, 1) as load:
    print(json.dumps([worker.pid for worker in load.workers]), flush=True)
    time.sleep(120)
'''


def gone(pid):
    """Whether `pid` runs no more: no such process, or a zombie its new parent (a Linux gate's
    subreaper, say) has not reaped yet."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    try:
        with open(f'/proc/{pid}/stat') as stream:
            return stream.read().rpartition(')')[2].split()[0] in ('Z', 'X')
    except OSError:
        return False


def wait_gone(pids, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if all(gone(pid) for pid in pids):
            return True
        time.sleep(0.02)
    return all(gone(pid) for pid in pids)


class LoadCleanup(unittest.TestCase):
    def driver(self):
        process = subprocess.Popen([sys.executable, '-I', '-c', DRIVER, str(SCRIPT)],
                                   stdout=subprocess.PIPE, text=True, start_new_session=True)
        self.addCleanup(process.stdout.close)
        self.addCleanup(process.wait)
        self.addCleanup(lambda: process.poll() is None and process.kill())
        pids = json.loads(process.stdout.readline())
        self.assertEqual(len(pids), 3)
        for pid in pids:
            self.assertFalse(gone(pid), f'worker {pid} runs')
        return process, pids

    def test_a_normal_end_kills_and_reaps_every_worker(self):
        with stress.Load(2, 1) as load:
            pids = [worker.pid for worker in load.workers]
            self.assertEqual(len(pids), 3)
        self.assertEqual(load.workers, [])
        self.assertTrue(all(gone(pid) for pid in pids), 'stop reaps before it returns')

    def test_sigterm_of_the_driver_ends_every_worker_before_it_exits(self):
        process, pids = self.driver()
        process.send_signal(signal.SIGTERM)
        self.assertEqual(process.wait(timeout=30), 128 + signal.SIGTERM)
        self.assertTrue(all(gone(pid) for pid in pids), 'the driver reaped its workers')

    def test_a_killed_driver_leaves_no_worker_behind(self):
        process, pids = self.driver()
        process.kill()
        process.wait(timeout=30)
        self.assertTrue(wait_gone(pids), f'orphaned workers {pids} outlived the driver')


class Table(unittest.TestCase):
    JUNIT = '''<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="2">
  <testsuite name="af::it" tests="2">
    <testcase name="a::passes" classname="af::it" timestamp="2026-10-27T03:00:00.000+00:00"
              time="10.0"></testcase>
    <testcase name="b::fails" classname="af::it" timestamp="2026-10-27T03:00:05.000+00:00"
              time="2.5"><failure message="test failure">panicked</failure>
      <system-err>left: None</system-err></testcase>
    <testcase name="c::other" classname="review-process::x" time="1.0"></testcase>
  </testsuite>
</testsuites>
'''

    def test_outcomes_read_each_af_case_with_its_start_wall_and_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            junit = Path(directory) / 'junit.xml'
            junit.write_text(self.JUNIT)
            found = stress.outcomes(junit)
        self.assertEqual(sorted(found), ['a::passes', 'b::fails'])
        passed, started, seconds, text = found['a::passes']
        self.assertEqual((passed, seconds, text), (True, 10.0, ''))
        self.assertEqual(started, 1793070000.0)
        passed, _, seconds, text = found['b::fails']
        self.assertEqual((passed, seconds), (False, 2.5))
        self.assertIn('panicked', text)
        self.assertIn('left: None', text)
        self.assertEqual(stress.outcomes(Path('/nonexistent/junit.xml')), {})

    def test_the_table_counts_passes_and_ranges_load_and_wall_per_test(self):
        sampler = stress.Sampler(clock=lambda: 0, load=lambda: (0, 0, 0))
        sampler.samples = [(100.0, 4.0), (105.0, 9.5), (111.0, 30.0), (200.0, 2.0)]
        results = [{'t': (True, 100.0, 10.0, '')}, {'t': (False, 199.0, 4.0, 'x')},
                   {'t': (False, None, 0.0, 'not run')}]
        rows = stress.table(['t'], results, 3, sampler)
        self.assertEqual(rows, [('t', '1/3', '2.0-9.5', '4.0/7.0/10.0')])
        rendered = stress.render(rows).splitlines()
        self.assertTrue(rendered[0].startswith('test'))
        self.assertEqual(rendered[2].split(), ['t', '1/3', '2.0-9.5', '4.0/7.0/10.0'])

    def test_a_skipped_case_and_a_nonzero_nextest_exit_pass_nothing(self):
        junit = '''<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1">
  <testsuite name="af::it" tests="1">
    <testcase name="a::skips" classname="af::it" time="0.0"><skipped/></testcase>
    <testcase name="b::passes" classname="af::it" time="1.0"></testcase>
  </testsuite>
</testsuites>
'''
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'junit.xml'
            path.write_text(junit)
            found = stress.outcomes(path)
        self.assertFalse(found['a::skips'][0], 'a skipped case did not pass')
        self.assertIn('skipped', found['a::skips'][3])
        self.assertTrue(found['b::passes'][0])
        judged = stress.judged(found, ['a::skips', 'b::passes', 'c::absent'], 0)
        self.assertEqual([judged[t][0] for t in ('a::skips', 'b::passes', 'c::absent')],
                         [False, True, False])
        # A run nextest ended nonzero passed nothing, whatever its JUnit says.
        judged = stress.judged(found, ['b::passes'], 101)
        self.assertFalse(judged['b::passes'][0])
        self.assertIn('nextest exited 101', judged['b::passes'][3])
        with tempfile.TemporaryDirectory() as directory:
            def run(command, cwd):
                store = Path(command[command.index('--tool-config-file') + 1].split(':', 1)[1])
                junit_path = store.parent / 'store' / 'ci' / 'junit.xml'
                junit_path.parent.mkdir(parents=True)
                junit_path.write_text(junit)
                return 4
            found = stress.one_run(1, ['b::passes'], 1, Path(directory), run=run)
        self.assertFalse(found['b::passes'][0])

    def test_a_load_worker_that_ends_on_its_own_fails_the_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            load = stress.Load(0, 0, logs=directory, programs=[
                "import sys\nsys.stderr.write('posix_spawn: resource unavailable')\n"
                "sys.exit(7)\n", stress.CPU_WORKER])
            with load:
                deadline = time.monotonic() + 20
                while not load.ended() and time.monotonic() < deadline:
                    time.sleep(0.02)
                ended = load.ended()
            self.assertEqual(len(ended), 1)
            pid, status, tail = ended[0]
            self.assertEqual(status, 7)
            self.assertIn('resource unavailable', tail)
        sampler = stress.Sampler(clock=lambda: 0, load=lambda: (0, 0, 0))
        sampler.samples = [(100.0, 4.0)]
        passing = [{'t': (True, 100.0, 1.0, '')}]
        # One passing run, but its load worker died: not a proof.
        self.assertEqual(stress.summarize(['t'], passing, 1, sampler, time.monotonic(), 'w',
                                          ended, None), 1)
        # Fewer runs than asked for is not a proof either; all of them passing is.
        self.assertEqual(stress.summarize(['t'], passing, 2, sampler, time.monotonic(), 'w',
                                          [], None), 1)
        self.assertEqual(stress.summarize(['t'], passing, 1, sampler, time.monotonic(), 'w',
                                          [], None), 0)

    def test_the_selection_names_each_test_exactly(self):
        self.assertEqual(stress.filter_expression(['a::b', 'c::d']),
                         'package(af) & binary(it) & (test(=a::b) | test(=c::d))')
        self.assertIn('review_plan_refusals::first_run_refusals_name_the_fix', stress.FAMILIES)
        self.assertEqual(len(stress.FAMILIES), 7)


if __name__ == '__main__':
    unittest.main()
