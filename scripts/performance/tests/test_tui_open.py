"""The PTY driver and its private registry fixture."""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest

from perflab import clicases
from perflab.values import ContractError
from tests.support import ROOT

DRIVER = os.path.join(ROOT, "fixtures/performance/cli/tui-open.py")
SETUP = os.path.join(ROOT, "fixtures/performance/cli/setup.sh")


class TuiOpen(unittest.TestCase):
    def test_first_status_line_and_missing_frame(self):
        with tempfile.TemporaryDirectory() as root:
            child = os.path.join(root, "child.py")
            with open(child, "w", encoding="utf-8") as output:
                output.write("import fcntl, os, struct, sys, termios, time, tty\n"
                             "assert os.isatty(0) and os.isatty(1)\n"
                             "size = fcntl.ioctl(0, termios.TIOCGWINSZ, b'\\x00' * 8)\n"
                             "assert struct.unpack('HHHH', size)[:2] == (30, 100)\n"
                             "os.write(1, b'splash')\n"
                             "time.sleep(0.03)\n"
                             "if sys.argv[1] == 'paint':\n"
                             "    tty.setraw(0)\n"
                             "    os.write(1, b'\\x1b[2J')\n"
                             "    time.sleep(0.03)\n"
                             "    os.write(1, b'\\x1b[30;1HNORMAL browser')\n"
                             "    os.read(0, 1)\n"
                             "if sys.argv[1] == 'hang': time.sleep(1)\n")
            good = subprocess.run([sys.executable, "-I", DRIVER, sys.executable, "-I", child,
                                   "paint"], capture_output=True, text=True, timeout=10)
            self.assertEqual(good.returncode, 0, good.stderr)
            result = json.loads(good.stdout)
            self.assertEqual(result["schema"], "af.perf-tui-open/1")
            self.assertEqual(result["exit_code"], 0)
            self.assertGreaterEqual(float(result["browser_ms"]), float(result["splash_ms"]))
            self.assertEqual(float(clicases.tui_interval(good.stdout.encode(), "browser_ms")),
                             float(result["browser_ms"]))
            missing = subprocess.run([sys.executable, "-I", DRIVER, sys.executable, "-I",
                                      child, "missing"], capture_output=True, text=True,
                                     timeout=10)
            self.assertNotEqual(missing.returncode, 0)
            self.assertEqual(missing.stdout, "")
            self.assertIn("before browser status-line frame", missing.stderr)
            spec = importlib.util.spec_from_file_location("tui_open_driver", DRIVER)
            driver = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(driver)
            driver.DEADLINE_SECONDS = 0.05
            with self.assertRaisesRegex(TimeoutError, "deadline"):
                driver.measure([sys.executable, "-I", child, "hang"])

    def test_reported_interval_rejects_untrusted_fields(self):
        valid = {"schema": "af.perf-tui-open/1", "browser_ms": "42.125",
                 "splash_ms": "5", "exit_code": 0}
        for edit in ({"extra": 1}, {"browser_ms": "-1"}, {"splash_ms": "50"},
                     {"exit_code": 1}):
            doc = dict(valid, **edit)
            with self.subTest(edit=edit), self.assertRaises(ContractError):
                clicases.tui_interval(json.dumps(doc).encode(), "browser_ms")

    def test_registry_setup_uses_af_for_twenty_stores(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = os.path.join(root, "scratch")
            seed = os.path.join(root, "seed")
            os.makedirs(os.path.join(seed, "probe-work"))
            with open(os.path.join(seed, "probe-work", "events.sqlite"), "wb") as output:
                output.write(b"seed")
            os.mkdir(scratch)
            af = os.path.join(root, "af")
            with open(af, "w", encoding="utf-8") as output:
                output.write("#!/bin/sh\n"
                             "test \"$1 $2 $3\" = \"task list --state\" || exit 2\n"
                             "echo \"$4\" >>\"$XDG_STATE_HOME/calls\"\n"
                             "mkdir -p \"$XDG_STATE_HOME/af\"\n"
                             "printf '[[store]]\\n' >>\"$XDG_STATE_HOME/af/stores.toml\"\n")
            os.chmod(af, 0o700)
            environment = dict(os.environ, **clicases.tui_home_environment(scratch))
            self.assertEqual(environment["XDG_STATE_HOME"],
                             os.path.join(scratch, "home/state"))
            result = subprocess.run(["/bin/sh", SETUP, "registry_empty", scratch, af, "-",
                                     seed], env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(os.path.exists(os.path.join(scratch,
                                                         "home/state/af/stores.toml")))
            result = subprocess.run(["/bin/sh", SETUP, "registry_stores_20x10000", scratch,
                                     af, "-", seed], env=environment, capture_output=True,
                                    text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            with open(os.path.join(scratch, "home/state/calls"), encoding="utf-8") as source:
                calls = source.read().splitlines()
            self.assertEqual(len(calls), 20)
            self.assertEqual(len(set(calls)), 20)
            for path in calls:
                with open(os.path.join(path, "events.sqlite"), "rb") as source:
                    self.assertEqual(source.read(), b"seed")


if __name__ == "__main__":
    unittest.main()
