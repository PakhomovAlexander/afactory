#!/usr/bin/env python3
"""Exercise `prereqs.py` (issue #161) against fake tools on a private PATH, then on this host:
nothing installs, nothing reaches the network."""
import importlib.util
import io
import os
from pathlib import Path
import contextlib
import subprocess
import sys
import tempfile
import unittest
import unittest.mock

SCRIPTS = Path(__file__).resolve().parent
TOOL = SCRIPTS / "prereqs.py"
ROOT = SCRIPTS.parent


def load():
    spec = importlib.util.spec_from_file_location("prereqs", TOOL)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prereqs = load()

FAKES = {
    "git": "echo 'git version 2.39.5'",
    "rustup": "case \"$1\" in\n"
              "  toolchain) echo '1.88.0-aarch64-apple-darwin (default)';"
              " echo 'stable-aarch64-apple-darwin' ;;\n"
              "  component) echo 'cargo-aarch64-apple-darwin'; echo 'clippy-aarch64-apple-darwin';"
              " echo 'rustfmt-aarch64-apple-darwin' ;;\n"
              "esac",
    "cargo-nextest": "echo 'cargo-nextest 0.9.132'",
    "cc": "echo 'cc (fixture) 13.2.0'",
    "npx": "echo '10.8.2'",
    "minisign": "echo 'minisign 0.11'",
}


class Prereqs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.toolchain = self.root / "rust-toolchain.toml"
        self.toolchain.write_text(
            '[toolchain]\nchannel = "1.88.0"\nprofile = "minimal"\n'
            'components = ["clippy", "rustfmt"]\n', encoding="utf-8")

    def tearDown(self):
        self.temp.cleanup()

    def fake(self, name, body):
        path = self.bin / name
        path.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
        path.chmod(0o755)

    def run_checks(self, min_free_gib=0):
        env = {"PATH": str(self.bin)}
        return {name: (passed, found, fix) for name, passed, found, fix in
                prereqs.checks(env, self.toolchain, self.root / "target", min_free_gib)}

    def main(self, *args):
        out = io.StringIO()
        env = dict(os.environ, PATH=str(self.bin))
        with contextlib.redirect_stdout(out), unittest.mock.patch.dict(os.environ, env, clear=True):
            code = prereqs.main(["--toolchain-file", str(self.toolchain),
                                 "--target", str(self.root / "target"), *args])
        return code, out.getvalue()

    def test_every_tool_present_passes_with_one_line_each(self):
        for name, body in FAKES.items():
            self.fake(name, body)
        code, out = self.main("--min-free-gib", "0")
        self.assertEqual(code, 0, out)
        lines = out.splitlines()
        self.assertEqual(len(lines), 9, out)
        for line, name in zip(lines, ["python3", "git", "rust", "cargo-nextest", "cc", "npx",
                                      "minisign", "disk"]):
            self.assertTrue(line.startswith("ok ") and f" {name} " in line, line)
        self.assertIn("rustup holds the pinned 1.88.0", out)
        self.assertEqual(lines[-1], "all 8 prerequisites present; next: make check")

    def test_a_missing_or_old_tool_names_its_install_command(self):
        for name, body in FAKES.items():
            if name != "minisign":
                self.fake(name, body)
        self.fake("git", "echo 'git version 2.20.1'")
        self.fake("rustup", "echo 'stable-x86_64-unknown-linux-gnu (default)'")
        results = self.run_checks()
        self.assertFalse(results["git"][0])
        self.assertIn("git version 2.20.1", results["git"][1])
        self.assertIn("git 2.30 or later", results["git"][2])
        self.assertFalse(results["rust"][0])
        self.assertEqual(results["rust"][1], "rustup holds no 1.88.0 (rust-toolchain.toml)")
        self.assertEqual(results["rust"][2],
                         "rustup toolchain install 1.88.0 --component clippy --component rustfmt")
        self.assertFalse(results["minisign"][0])
        self.assertEqual(results["minisign"][1], "not on PATH")
        self.assertTrue(results["cc"][0] and results["npx"][0] and results["cargo-nextest"][0])
        code, out = self.main("--min-free-gib", "0")
        self.assertEqual(code, 1, out)
        self.assertIn("missing  minisign", out)
        self.assertIn("— fix: ", out)
        self.assertTrue(out.splitlines()[-1].startswith("3 of 8 prerequisites missing: git, rust"),
                        out)

    def test_too_little_free_space_names_the_volume_and_the_way_out(self):
        for name, body in FAKES.items():
            self.fake(name, body)
        passed, found, fix = self.run_checks(min_free_gib=1 << 40)["disk"]
        self.assertFalse(passed)
        self.assertIn("GiB free on the volume of", found)
        self.assertIn(str(self.root.resolve()), found)
        self.assertIn("free 1099511627776 GiB there", fix)

    def test_nothing_on_path_is_every_tool_missing_and_nothing_runs(self):
        results = self.run_checks()
        for name in ["git", "rust", "cargo-nextest", "cc", "npx", "minisign"]:
            self.assertFalse(results[name][0], name)
        self.assertIn("install rustup (https://rustup.rs)", results["rust"][2])

    def test_make_prereqs_on_this_host(self):
        """`make prereqs` on this host: one line per check and a last line, its exit status 0
        exactly when every check passed (on a contributor's machine with the tools installed,
        0)."""
        done = subprocess.run(["make", "-s", "prereqs"], cwd=ROOT, capture_output=True,
                              text=True, timeout=120, stdin=subprocess.DEVNULL)
        said = done.stdout + done.stderr
        lines = done.stdout.splitlines()
        # One line per check, a wrapped one continued on indented lines, and a last line.
        entries = [line for line in lines if not line.startswith(" ")]
        self.assertEqual(len(entries), 9, said)
        passed = all(line.startswith("ok ") for line in entries[:-1])
        self.assertEqual(done.returncode == 0, passed, said)
        self.assertTrue(all(len(line) <= prereqs.WIDTH for line in lines), said)

    def test_a_toolchain_without_a_pinned_component_is_missing_with_the_command(self):
        for name, body in FAKES.items():
            self.fake(name, body)
        self.fake("rustup", "case \"$1\" in\n"
                            "  toolchain) echo '1.88.0-aarch64-apple-darwin (default)' ;;\n"
                            "  component) echo 'cargo-aarch64-apple-darwin';"
                            " echo 'rustfmt-aarch64-apple-darwin' ;;\n"
                            "esac")
        passed, found, fix = self.run_checks()["rust"]
        self.assertFalse(passed)
        self.assertEqual(found, "rustup holds 1.88.0 without clippy")
        self.assertEqual(fix, "rustup component add --toolchain 1.88.0 clippy")
        code, out = self.main("--min-free-gib", "0")
        self.assertEqual(code, 1, out)
        self.assertIn("missing  rust", out)
        self.assertTrue(out.splitlines()[-1].startswith("1 of 8 prerequisites missing: rust"),
                        out)

    def test_long_lines_wrap_at_100_columns_and_home_is_abbreviated(self):
        for name, body in FAKES.items():
            self.fake(name, body)
        deep = self.root / ("a-rather-long-directory-name/" * 6) / "target"
        deep.parent.mkdir(parents=True)
        out = io.StringIO()
        env = dict(os.environ, PATH=str(self.bin), HOME=str(self.root))
        with contextlib.redirect_stdout(out), \
                unittest.mock.patch.dict(os.environ, env, clear=True):
            code = prereqs.main(["--toolchain-file", str(self.toolchain), "--target", str(deep),
                                 "--min-free-gib", str(1 << 40)])
        said = out.getvalue()
        self.assertEqual(code, 1, said)
        lines = said.splitlines()
        self.assertTrue(all(len(line) <= 100 for line in lines), said)
        self.assertIn("GiB free on the volume of\n" + " " * 24 + "~/a-rather-long", said)
        disk = next(index for index, line in enumerate(lines) if line.startswith("missing  disk"))
        self.assertTrue(lines[disk + 1].startswith(" " * 24), said)


if __name__ == "__main__":
    unittest.main()
