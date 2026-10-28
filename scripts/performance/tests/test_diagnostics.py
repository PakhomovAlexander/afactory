"""Separate bounded diagnostic child runs (package p5, ADR-0174, ADR-0175).

The perf, inferno, valgrind, samply and probe executables here are stand-ins
(`tests/diagnostic_fixtures.py`) that speak the real tools' protocols; every child run they make
is synthetic and no test makes a performance claim. What is exercised is the driver: selection,
budgets, the acknowledged region bracket (a slow setup never reaches a capture), the bounded
tool runs, malformed output, SVG sanitization, trace nesting and overhead, and the honest
unavailable states. Every execution goes through a stand-in of the authority supervisor here;
the real supervisor's bounds over diagnostic tools are exercised by
`crates/review-process/tests/diagnostic_tools.rs`.
"""
import struct
import lzma
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
from fractions import Fraction

from perflab import (clicases, contracts, counters, diagcontracts, diagnostics, flamegraph,
                     gitrefs, oracle, perfcapture, phasetrace, planning, runner, stats, supervise,
                     svgsafe, toolrun)
from perflab import suite as suite_module
from perflab.values import ContractError, sha256_bytes
from tests import build_fixtures
from tests import diagnostic_fixtures as fakes
from tests.support import ROOT, SUITE, TempDir, fake_supervisor, lab_repo, read

REGISTRY = os.path.join(ROOT, diagnostics.REGISTRY_PATH)
SMALL = "canonical_serialize_small__package_small_envelope"
VALIDATE = "canonical_validate_small__package_small_envelope"
RAW = "cas_raw_get_small__package_small_envelope"


def supervisor(directory):
    """The stand-in supervisor in its own directory, with a short cancellation reserve."""
    os.makedirs(directory, exist_ok=True)
    return toolrun.Supervisor(fake_supervisor(os.path.join(directory, "perf-supervisor")),
                              cancel_margin=0, grace_ms=100)


def perf_data(records):
    """A little-endian perf.data of `records`, each (type, payload bytes)."""
    body = b"".join(struct.pack("<IHH", kind, 0, 8 + len(payload)) + payload
                    for kind, payload in records)
    header = (b"PERFILE2" + struct.pack("<QQQQQQQQ", 104, 136, 104, 0, 104, len(body), 0, 0)
              + bytes(32))
    return header + body


def golden():
    return oracle.load_golden(os.path.join(ROOT, "fixtures/performance/golden/native.json"))


class Registry(unittest.TestCase):
    def test_the_registry_derives_every_recipe_from_a_verdict_recipe_of_the_suite(self):
        loaded = suite_module.load(SUITE)
        registry = diagnostics.load(REGISTRY, loaded.doc)
        self.assertEqual(registry["capture"]["frequency_hz"], 997)
        self.assertEqual(registry["capture"]["duration_seconds"], 10)
        self.assertEqual(registry["capture"]["event"], "task-clock:u")
        recipe = diagcontracts.diagnostic_recipe(loaded.doc, registry, "linux_gnu_cpu_profile")
        base = loaded.doc["recipes"]["linux_gnu_native_release"]
        for key in ("os", "target", "toolchain", "builder", "linker", "profile"):
            self.assertEqual(recipe[key], base[key])
        self.assertTrue(recipe["instrumented"] and recipe["frame_pointers"]
                        and recipe["debug_symbols"])
        self.assertEqual(recipe["features"], ["perf-control"])
        self.assertEqual(recipe["rustflags"], base["rustflags"] + [
            "-Cdebuginfo=line-tables-only"])
        self.assertEqual(base["rustflags"], [])
        # Derivation must not mutate the base's list, including on a second planning pass.
        self.assertEqual(diagcontracts.diagnostic_recipe(
            loaded.doc, registry, "linux_gnu_cpu_profile"), recipe)
        for recipe_id in registry["recipes"]:
            if recipe_id == "linux_gnu_cpu_profile":
                continue
            other = diagcontracts.diagnostic_recipe(loaded.doc, registry, recipe_id)
            self.assertEqual(other["rustflags"], loaded.doc["recipes"][
                registry["recipes"][recipe_id]["base_recipe"]]["rustflags"])
        # The verdict recipe itself is untouched: uninstrumented, no symbols, no frame pointers.
        self.assertFalse(base["instrumented"] or base["frame_pointers"] or base["debug_symbols"])
        inferno = registry["tools"]["inferno_flamegraph"]
        self.assertEqual((inferno["version"], inferno["source_sha256"]),
                         ("0.12.8", "0c460d4fa06223667240720ab69a8045133755ae6dfbe100cf481b95e3a"
                          "014f1"))
        self.assertNotIn("version_argv", inferno, "inferno's binaries refuse --version")

    def test_the_musl_shipping_recipes_keep_their_whole_shipping_build_identity(self):
        # Built like the shipping recipe, in its pinned image with cargo-zigbuild and Zig;
        # only what diagnosis needs is added (ADR-0176).
        loaded = suite_module.load(SUITE)
        registry = diagnostics.load(REGISTRY, loaded.doc)
        base = loaded.doc["recipes"]["linux_musl_shipping"]
        for recipe_id, features in (("linux_musl_cpu_profile", ["perf-control"]),
                                    ("linux_musl_trace", ["perf-trace"])):
            with self.subTest(recipe=recipe_id):
                recipe = diagcontracts.diagnostic_recipe(loaded.doc, registry, recipe_id)
                for key in ("os", "target", "toolchain", "builder", "cargo_zigbuild", "zig",
                            "linker", "profile", "rustflags"):
                    self.assertEqual(recipe[key], base[key])
                self.assertEqual(recipe["target"], "x86_64-unknown-linux-musl")
                self.assertEqual((recipe["features"], recipe["instrumented"]), (features, True))
        self.assertTrue(registry["recipes"]["linux_musl_cpu_profile"]["frame_pointers"])

    def test_a_recipe_on_a_tool_of_another_system_or_an_unknown_base_is_refused(self):
        loaded = suite_module.load(SUITE)
        text = read(REGISTRY).decode("utf-8")
        block = ('base_recipe = "macos_arm64_native_release"\nfeatures = ["perf-trace"]\n'
                 'debug_symbols = false\nframe_pointers = false\ntools = []')
        self.assertIn(block, text)
        wrong = text.replace(block, block.replace("tools = []", 'tools = ["perf"]'), 1)
        with self.assertRaises(ContractError) as caught:
            diagnostics.load_bytes(wrong.encode("utf-8"), "registry", loaded.doc)
        self.assertIn("perf does not run on macos", str(caught.exception))
        missing = text.replace('base_recipe = "linux_gnu_native_release"',
                               'base_recipe = "no_such_recipe"', 1)
        with self.assertRaises(ContractError):
            diagnostics.load_bytes(missing.encode("utf-8"), "registry", loaded.doc)


def verdict_for(plan, classes):
    """A verdict-shaped document naming each case's classification; only the fields selection
    reads matter here, and selection checks the run and plan identity."""
    return {"run_id": plan["run_id"], "plan_digest": plan["plan_digest"],
            "cases": [{"instance_id": name, "statistical_classification": value}
                      for name, value in classes]}


class Selection(unittest.TestCase):
    PLAN = {"run_id": "parent", "plan_digest": "sha256:" + "a" * 64,
            "cases": [{"instance_id": name, "case_id": name.split("__")[0]}
                      for name in ("a__x", "b__x", "c__x", "d__x", "e__x", "f__x")]}

    def test_automatic_selection_takes_regressed_then_inconclusive_by_stable_id(self):
        verdict = verdict_for(self.PLAN, [
            ("f__x", stats.INCONCLUSIVE), ("e__x", stats.REGRESSION),
            ("d__x", stats.WITHIN_TOLERANCE), ("c__x", stats.REGRESSION),
            ("b__x", stats.INCONCLUSIVE), ("a__x", stats.IMPROVEMENT)])
        record, cases = diagnostics.select(self.PLAN, verdict, verdict_sha256="sha256:" + "b" * 64)
        self.assertEqual([(item["instance_id"], item["reason"]) for item in record["cases"]],
                         [("c__x", "regressed"), ("e__x", "regressed"), ("b__x", "inconclusive")])
        self.assertEqual(record["limit"], 3)
        self.assertEqual(record["not_selected"],
                         [{"instance_id": "f__x", "reason": "over_case_limit"}])
        self.assertEqual([case["instance_id"] for case in cases], ["c__x", "e__x", "b__x"])
        wide, _ = diagnostics.select(self.PLAN, verdict, limit=8,
                                     verdict_sha256="sha256:" + "b" * 64)
        self.assertEqual(len(wide["cases"]), 4)
        with self.assertRaises(ContractError):
            diagnostics.select(self.PLAN, verdict, limit=9, verdict_sha256="sha256:" + "b" * 64)

    def test_explicit_selection_is_exactly_what_was_named_within_the_hard_cap(self):
        record, cases = diagnostics.select(self.PLAN, explicit=["d__x", "a__x"])
        self.assertEqual([item["reason"] for item in record["cases"]], ["explicit"] * 2)
        self.assertEqual([case["instance_id"] for case in cases], ["d__x", "a__x"])
        with self.assertRaises(ContractError):
            diagnostics.select(self.PLAN, explicit=["nope"])
        with self.assertRaises(ContractError):
            diagnostics.select(self.PLAN, explicit=["a__x", "a__x"])
        many = dict(self.PLAN, cases=[{"instance_id": "c%d__x" % index, "case_id": "c"}
                                      for index in range(9)])
        with self.assertRaises(ContractError):
            diagnostics.select(many, explicit=["c%d__x" % index for index in range(9)])

    def test_a_verdict_of_another_run_is_refused(self):
        verdict = dict(verdict_for(self.PLAN, []), plan_digest="sha256:" + "c" * 64)
        with self.assertRaises(ContractError) as caught:
            diagnostics.select(self.PLAN, verdict, verdict_sha256="sha256:" + "b" * 64)
        self.assertIn("another run or plan", str(caught.exception))


class Budgets(TempDir):
    LIMITS = {"total_minutes": 15, "profile_bytes": 1000, "builds": 2, "tool_output_bytes": 64,
              "tool_seconds": 5}

    def test_the_deadline_and_byte_cap_stop_new_work_and_the_build_cap_refuses_a_build(self):
        now = [100.0]
        budget = diagnostics.Budget(self.LIMITS, self.tmp, clock=lambda: now[0],
                                    wall=lambda: 1760000000.0)
        budget.check("the first step")
        with open(self.path("big"), "wb") as stream:
            stream.write(b"x" * 1000)
        with self.assertRaises(diagnostics.Exhausted) as caught:
            budget.check("a capture")
        self.assertEqual(caught.exception.reason, "profile_bytes_exhausted")
        os.remove(self.path("big"))
        now[0] += 15 * 60
        with self.assertRaises(diagnostics.Exhausted) as caught:
            budget.check("a capture")
        self.assertEqual(caught.exception.reason, "diagnostic_deadline")
        budget.build(2)
        with self.assertRaises(diagnostics.Exhausted) as caught:
            budget.build(1)
        self.assertEqual(caught.exception.reason, "build_cap")
        with self.assertRaises(ContractError):
            diagnostics.Budget(self.LIMITS, self.tmp, total_minutes=31)


class Evidence(TempDir):
    """What a tool or candidate wrote is admitted as a confined, bounded regular file, and the
    byte budget records the evidence's actual size."""

    LIMITS = {"total_minutes": 15, "profile_bytes": 4096, "builds": 2,
              "tool_output_bytes": 1024, "tool_seconds": 5}

    def profile(self):
        profile = diagnostics.Profile.__new__(diagnostics.Profile)
        profile.output = self.path("run")
        os.makedirs(profile.output)
        profile.budget = diagnostics.Budget(self.LIMITS, profile.output)
        profile.clock = time.monotonic
        return profile

    def test_files_are_admitted_only_as_bounded_regular_files_in_their_directory(self):
        profile = self.profile()
        os.makedirs(self.path("run", "captures"))
        with open(self.path("run", "captures", "ok"), "wb") as stream:
            stream.write(b"abc")
        self.assertEqual(profile.admitted(profile.output, "captures/ok", 10, "ok"), b"abc")
        with open(self.path("secret"), "wb") as stream:
            stream.write(b"secret")
        os.symlink(self.path("secret"), self.path("run", "captures", "link"))
        os.symlink(self.path("."), self.path("run", "ancestor"))
        os.mkfifo(self.path("run", "captures", "fifo"))
        for relative, needle in (("captures/link", "symlink"),
                                 ("ancestor/secret", "symlink is never followed"),
                                 ("captures/fifo", "FIFO")):
            with self.subTest(relative=relative):
                with self.assertRaises(ContractError) as caught:
                    profile.admitted(profile.output, relative, 1 << 20, relative)
                self.assertIn(needle, str(caught.exception))
        if os.path.exists("/dev/null"):
            with self.assertRaises(ContractError) as caught:
                profile.admitted("/dev", "null", 10, "a device")
            self.assertIn("device", str(caught.exception))
        # A sparse file is bounded by its logical size, never by its allocated blocks.
        with open(self.path("run", "captures", "sparse"), "wb") as stream:
            stream.truncate(1 << 30)
        with self.assertRaises(diagnostics.Exhausted) as caught:
            profile.admitted(profile.output, "captures/sparse", 1 << 20, "sparse")
        self.assertIn("1073741824 bytes", caught.exception.detail)
        # The evidence holding anything but directories and regular files is refused.
        with self.assertRaises(ContractError):
            profile.budget.used_bytes()

    def test_the_byte_budget_records_actual_usage_and_its_overage(self):
        profile = self.profile()
        budget = profile.budget
        self.assertEqual((budget.reserve, budget.evidence_limit), (2048, 2048))
        profile.write("a", b"x" * 2000)
        with self.assertRaises(diagnostics.Exhausted) as caught:
            profile.write("b", b"y" * 100)
        self.assertEqual(caught.exception.reason, "profile_bytes_exhausted")
        self.assertFalse(os.path.exists(self.path("run", "b")), "charged before it is written")
        # A tool's file that outgrew the room (perf's --max-size is not exact) is counted as it
        # is: never clamped, never deleted to fit, and its overage stated.
        with open(self.path("run", "c"), "wb") as stream:
            stream.write(b"z" * 1000)
        record = budget.record(1)["bytes"]
        self.assertEqual(record, {"limit": 4096, "administrative_reserve": 2048,
                                  "evidence_used": 3000, "evidence_overage": 952})
        sparse = self.path("run", "d")
        with open(sparse, "wb") as stream:
            stream.truncate(1 << 20)
        self.assertEqual(budget.record(1)["bytes"]["evidence_used"], 3000 + (1 << 20))


class Symbols(TempDir):
    def profile(self):
        profile = Evidence.profile(self)
        profile.retained = []
        profile.symbol_archives = {}
        return profile

    LIMITS = Evidence.LIMITS

    def noise(self, size, seed):
        source = random.Random(seed)
        return bytes(source.getrandbits(8) for _ in range(size))

    def executable(self, name, data):
        path = self.path(name)
        with open(path, "wb") as stream:
            stream.write(data)
        return path, sha256_bytes(data)

    def test_shared_symbols_pay_once_even_when_another_copy_would_not_fit(self):
        profile = self.profile()
        binary = self.executable("probe", self.noise(1400, 1))
        symbols = profile.symbols(binary, ["abcd"])
        self.assertEqual(symbols["state"], "retained")
        self.assertEqual(symbols["build_ids"], ["abcd"])
        self.assertEqual(symbols["archive"], profile.artifacts("symbols")[0])
        self.assertLess(profile.budget.room(), os.stat(binary[0]).st_size)
        before = profile.budget.used_bytes()
        self.assertEqual(profile.symbols(binary, ["abcd"])["state"], "retained")
        self.assertEqual(profile.budget.used_bytes(), before)
        archive = read(self.path("run", "symbols", binary[1][7:] + ".xz"))
        self.assertEqual(lzma.decompress(archive), read(binary[0]))
        self.assertEqual(profile.artifacts("symbols"), [{
            "path": "symbols/" + binary[1][7:] + ".xz", "bytes": len(archive),
            "sha256": sha256_bytes(archive)}])
        self.assertEqual(before, len(archive))

    def test_distinct_symbols_pay_separately_and_an_oversize_binary_stays_referenced(self):
        profile = self.profile()
        sizes = []
        for name, data in (("baseline", self.noise(700, 1)),
                           ("candidate", self.noise(800, 2))):
            binary = self.executable(name, data)
            self.assertEqual(profile.symbols(binary, [])["state"], "retained")
            archive = read(self.path("run", "symbols", binary[1][7:] + ".xz"))
            self.assertEqual(sha256_bytes(lzma.decompress(archive)), binary[1])
            sizes.append(len(archive))
        self.assertEqual(profile.budget.used_bytes(), sum(sizes))
        large = self.executable("large", self.noise(1000, 3))
        self.assertEqual(profile.symbols(large, [])["state"], "referenced")
        self.assertEqual(profile.budget.used_bytes(), sum(sizes))
        self.assertFalse(os.path.exists(self.path("run", "symbols", large[1][7:] + ".xz")))
        usage = profile.budget.record(0)["bytes"]
        self.assertEqual((usage["evidence_used"], usage["evidence_overage"]), (sum(sizes), 0))

    def test_lossless_archives_fit_by_their_logical_size_and_have_no_timestamp(self):
        profile = self.profile()
        binary = self.executable("probe", b"symbols" * 10000)
        self.assertGreater(os.stat(binary[0]).st_size, profile.budget.evidence_limit)
        self.assertEqual(profile.symbols(binary, [])["state"], "retained")
        archive = read(self.path("run", "symbols", binary[1][7:] + ".xz"))
        self.assertTrue(archive.startswith(b"\xfd7zXZ\x00"))  # XZ has no timestamp or filename
        self.assertEqual(lzma.decompress(archive), read(binary[0]))
        self.assertEqual(profile.budget.used_bytes(), len(archive))
        self.assertEqual(profile.symbol_archive(read(binary[0]), len(archive)), archive)
        self.assertIsNone(profile.symbol_archive(read(binary[0]), len(archive) - 1))

    def test_compression_keeps_the_absolute_deadline(self):
        profile = self.profile()
        now = [0]
        def clock():
            now[0] += 1
            return now[0]
        profile.clock = clock
        profile.budget.deadline = 2
        with self.assertRaisesRegex(ContractError, "deadline"):
            profile.symbol_archive(b"symbols" * 10000, 2048)
        self.assertEqual(profile.budget.used_bytes(), 0)

    def test_symbol_bytes_must_match_the_profiled_hash_and_retained_files_are_admitted(self):
        profile = self.profile()
        binary = self.executable("probe", b"original symbols")
        wrong = (binary[0], sha256_bytes(b"other bytes"))
        with self.assertRaisesRegex(ContractError, "profiled executable hash changed"):
            profile.symbols(wrong, [])
        self.assertEqual(profile.budget.used_bytes(), 0)
        profile.symbols(binary, [])
        target = self.path("run", "symbols", binary[1][7:] + ".xz")
        with open(target, "wb") as stream:
            stream.write(b"changed symbols!")
        with self.assertRaisesRegex(ContractError, "retained symbols changed"):
            profile.symbols(binary, [])
        os.unlink(target)
        os.symlink(binary[0], target)
        with self.assertRaisesRegex(ContractError, "symlink"):
            profile.symbols(binary, [])


class AggregateRoom(TempDir):
    """A step's files are bounded all together while they are written, not only each by
    `RLIMIT_FSIZE`, and a step over its room is refused with its bytes kept and counted
    (root-4, ADR-0176)."""

    LIMITS = {"total_minutes": 15, "profile_bytes": 32, "builds": 2,
              "tool_output_bytes": 1024, "tool_seconds": 30}

    def profile(self):
        profile = diagnostics.Profile.__new__(diagnostics.Profile)
        profile.output = self.path("run")
        os.makedirs(os.path.join(profile.output, "out"))
        profile.budget = diagnostics.Budget(self.LIMITS, profile.output)
        profile.clock = time.monotonic
        profile.supervisor = supervisor(self.path("supervisor"))
        profile.execution = {"mode": diagnostics.TRUSTED_LOCAL}
        profile.retained = []
        return profile

    def test_two_files_each_within_the_file_limit_but_over_the_room_are_refused(self):
        profile = self.profile()
        self.assertEqual((profile.budget.bytes_limit, profile.budget.evidence_limit), (32, 16))
        # The positive control: three bytes of stdout fit the 16-byte room.
        self.assertEqual(profile.tool_output("out/plain", ["/bin/sh", "-c", "printf abc"],
                                             "the control"), b"abc")
        os.remove(self.path("run", "out", "plain"))
        writer = ("head -c 16 /dev/zero > one; head -c 16 /dev/zero > two; printf abc")
        with self.assertRaises(diagnostics.Exhausted) as caught:
            profile.tool_output("out/stdout", ["/bin/sh", "-c", writer], "the writer")
        self.assertEqual(caught.exception.reason, diagnostics.OVER_ROOM)
        # Every byte is kept and counted as it is: 16 + 16 + 3, 19 over the room.
        self.assertEqual(sorted(os.listdir(self.path("run", "out"))), ["one", "stdout", "two"])
        self.assertEqual(profile.budget.record(1)["bytes"], {
            "limit": 32, "administrative_reserve": 16, "evidence_used": 35,
            "evidence_overage": 19})

    def test_a_writer_that_keeps_writing_small_files_is_cancelled_while_it_runs(self):
        profile = self.profile()
        # Each file is 4 bytes, far below RLIMIT_FSIZE; together they pass the room at once,
        # and the writer would otherwise run for 20 s.
        writer = ('i=0; while [ $i -lt 2000 ]; do printf abcd > f$i; i=$((i+1)); sleep 0.01; '
                  'done')
        started = time.monotonic()
        result = profile.tool_run(["/bin/sh", "-c", writer], 30, 64,
                                  self.path("run", "out"), file_bytes=16)
        self.assertLess(time.monotonic() - started, 10, "cancelled while it ran")
        self.assertIsNotNone(result.exceeded)
        self.assertIn("over its 16-byte room", result.supervision_failure())
        self.assertFalse(result.ok)
        with self.assertRaises(diagnostics.Exhausted) as caught:
            profile.settle("the writer")
        self.assertEqual(caught.exception.reason, diagnostics.OVER_ROOM)


class Sanitizer(unittest.TestCase):
    INFERNO = (b'<?xml version="1.0" standalone="no"?>\n<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG '
               b'1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">\n'
               b'<svg version="1.1" width="1200" height="70" onload="init(evt)" '
               b'viewBox="0 0 1200 70" xmlns="http://www.w3.org/2000/svg" '
               b'xmlns:xlink="http://www.w3.org/1999/xlink" '
               b'xmlns:fg="http://github.com/jonhoo/inferno"><!--comment-->'
               b'<defs><linearGradient id="background" y1="0" y2="1" x1="0" x2="0">'
               b'<stop stop-color="#eeeeee" offset="5%"/></linearGradient></defs>'
               b'<style type="text/css">text { font-family:monospace; font-size:12px }</style>'
               b'<script type="text/ecmascript"><![CDATA[var x = 1; init();]]></script>'
               b'<rect x="0" y="0" width="100%" height="70" fill="url(#background)"/>'
               b'<svg id="frames" x="10" width="1180" total_samples="9"><g><title>main '
               b'(9 samples, 100%)</title><rect x="0.0000%" y="37" width="100.0000%" '
               b'height="15" fill="rgb(227,0,7)" fg:x="0" fg:w="9"/><text x="0.25%" y="47.50">'
               b'main</text></g></svg></svg>\n')

    def test_inferno_output_keeps_its_drawing_and_loses_its_script_and_handlers(self):
        result = svgsafe.sanitize(self.INFERNO)
        text = result.data.decode("utf-8")
        for kept in ('<rect x="0.0000%" y="37" width="100.0000%" height="15" '
                     'fill="rgb(227,0,7)" fg:x="0" fg:w="9"/>', "<title>main (9 samples, 100%)",
                     'fill="url(#background)"', "<style type=\"text/css\">"):
            self.assertIn(kept, text)
        for gone in ("<script", "onload", "init(", "<!DOCTYPE", "<!--", "xmlns:xlink",
                     "<?xml"):
            self.assertNotIn(gone, text)
        self.assertEqual(result.removed_elements, 1)
        # onload, and the xlink namespace no kept attribute uses.
        self.assertEqual(result.removed_attributes, 2)
        self.assertEqual(svgsafe.sanitize(self.INFERNO).data, result.data, "deterministic")
        # The sanitized document is itself sanitized to the same bytes.
        self.assertEqual(svgsafe.sanitize(result.data).data, result.data)

    def wrap(self, body, head=b""):
        return (head + b'<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">' + body
                + b"</svg>")

    def test_active_content_external_references_and_unsafe_css_are_removed(self):
        cases = {
            b'<foreignObject><body xmlns="http://www.w3.org/1999/xhtml">x</body></foreignObject>':
                "foreignObject",
            b'<a xlink:href="https://evil.invalid/" href="javascript:alert(1)" '
            b'xmlns:xlink="http://www.w3.org/1999/xlink"><rect x="1"/></a>': "evil",
            b'<rect x="1" onclick="steal()" onmouseover="steal()"/>': "steal",
            b'<rect x="1" fill="url(https://evil.invalid/x.svg#a)"/>': "evil.invalid",
            b'<rect x="1" style="fill:url(https://evil.invalid/)"/>': "evil.invalid",
            b'<image href="https://evil.invalid/pixel.png" width="1" height="1"/>': "pixel",
            b'<use href="#x"/><set attributeName="x" to="1"/><animate attributeName="x"/>':
                "attributeName",
            b'<style>@import url(https://evil.invalid/x.css);</style>': "@import",
            b'<style>rect { fill: url(https://evil.invalid/) }</style>': "evil.invalid",
            b'<style>rect { behavior: url(x.htc) }</style>': "behavior",
            b'<?xml-stylesheet href="https://evil.invalid/x.css"?><rect x="1"/>': "stylesheet",
            b'<text x="1">&lt;script&gt;alert(1)&lt;/script&gt;</text>': "<script>",
            b'<script>alert(1)</script>': "alert",
        }
        for body, needle in cases.items():
            with self.subTest(body=body):
                text = svgsafe.sanitize(self.wrap(body)).data.decode("utf-8")
                self.assertNotIn(needle, text)
                self.assertTrue(text.startswith('<svg xmlns="http://www.w3.org/2000/svg"'))
        # Escaped text stays text: re-escaped, never markup.
        text = svgsafe.sanitize(self.wrap(b'<text x="1">&lt;b&gt;</text>')).data.decode()
        self.assertIn("<text x=\"1\">&lt;b&gt;</text>", text)

    def test_namespaces_and_css_are_held_to_the_safe_subset_not_a_url_blocklist(self):
        removed = {
            # CSS string URL forms and functions other than rgb(): the whole block goes.
            b'<style>rect { fill: image-set("https://evil.invalid/x.png" 1x) }</style>':
                "evil.invalid",
            b"<style>rect { cursor: image-set('x.png' 1x), auto }</style>": "image-set",
            b'<style>@font-face { font-family: x; src: "x.woff" }</style>': "font-face",
            b"<style>rect { fill: \\75 rl(x) }</style>": "rl(x)",
            b"<style>/* */ rect { fill: red }</style>": "fill: red",
            b'<style>rect { background: red }</style>': "background",
            # Another namespace, even under SVG's own element names, is never drawn.
            b'<g xmlns="http://www.w3.org/1999/xhtml"><rect x="1" id="html"/></g>': "html",
            b'<h:script xmlns:h="http://www.w3.org/1999/xhtml">alert(1)</h:script>': "alert",
            b'<s:script xmlns:s="http://www.w3.org/2000/svg">alert(2)</s:script>': "alert",
            b'<rect x="1" xlink:href="https://evil.invalid/" '
            b'xmlns:xlink="http://www.w3.org/1999/xlink"/>': "evil.invalid",
            b'<rect x="1" e:onclick="steal()" xmlns:e="http://example.invalid/e"/>': "steal",
            b'<rect x="javascript:alert(1)" fill="url(#a) url(https://evil.invalid/)"/>':
                "javascript",
        }
        for body, needle in removed.items():
            with self.subTest(body=body):
                text = svgsafe.sanitize(self.wrap(body)).data.decode("utf-8")
                self.assertNotIn(needle, text)
                self.assertNotIn("xhtml", text)
                self.assertNotIn("xlink", text)
                self.assertTrue(text.startswith('<svg xmlns="http://www.w3.org/2000/svg"'))
        # A prefixed SVG element that is drawn is kept, written in the default namespace.
        text = svgsafe.sanitize(self.wrap(b'<s:rect xmlns:s="http://www.w3.org/2000/svg" '
                                          b'x="1"/>')).data.decode()
        self.assertIn('<rect x="1"/>', text)
        self.assertTrue(svgsafe.safe_css("text { font-family:monospace; font-size:12px }\n"
                                         "#frames > *:hover { stroke:black; stroke-width:0.5; "
                                         "cursor:pointer; }\n#subtitle { text-anchor:middle; "
                                         "font-color:rgb(160,160,160); }\n"
                                         'text { font-family:"Verdana" }'))
        # The prefixed SVG root and inferno's own namespace under another prefix both resolve.
        text = svgsafe.sanitize(
            b'<s:svg xmlns:s="http://www.w3.org/2000/svg" xmlns:i="http://github.com/jonhoo/'
            b'inferno"><s:rect x="1" i:w="3"/></s:svg>').data.decode()
        self.assertIn('xmlns:fg="http://github.com/jonhoo/inferno"', text)
        self.assertIn('<rect x="1" fg:w="3"/>', text)

    def test_dtds_entities_and_malformed_documents_are_refused(self):
        refused = {
            b'<!DOCTYPE svg [<!ENTITY x "boom">]>' + self.wrap(b"<text>&x;</text>"):
                "internal subset",
            b'<!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/passwd">]>'
            + self.wrap(b"<text>&x;</text>"): "internal subset",
            b'<!DOCTYPE svg SYSTEM "https://evil.invalid/x.dtd">' + self.wrap(b"<text>&x;</text>"):
                "undeclared entity",
            self.wrap(b"<text>&undefined;</text>"): "not well-formed",
            self.wrap(b"<text>unclosed"): "not well-formed",
            self.wrap(b'<rect xlink:href="x"/>'): "unbound prefix",
            b'<html xmlns="http://www.w3.org/2000/svg"/>': "not one <svg>",
            b'<svg xmlns="http://example.invalid/"/>': "SVG namespace",
            b'<svg/>': "SVG namespace",
            b'<x:svg xmlns:x="http://www.w3.org/1999/xhtml"/>': "SVG namespace",
            b"\xff\xfe<svg/>": "not UTF-8",
        }
        for data, needle in refused.items():
            with self.subTest(data=data[:60]):
                with self.assertRaises(ContractError) as caught:
                    svgsafe.sanitize(data)
                self.assertIn(needle, str(caught.exception))
        with self.assertRaises(ContractError):
            svgsafe.sanitize(self.wrap(b"<g>" * 300 + b"</g>" * 300))


class PerfText(unittest.TestCase):
    SCRIPT = ("perf-probe 10/10 1.000000: task-clock:u:\n"
              "\t    55aa00000010 alloc::vec::Vec<u8>::push (/w/perf-probe)\n"
              "\t    7f0000000030 [unknown] ([unknown])\n"
              "\t    7f0000000031 [unknown] (/usr/lib/libc.so.6)\n"
              "\t    55aa00000040 main (/w/perf-probe)\n\n"
              "perf-probe 10/10 1.001003: task-clock:u:\n"
              "\t    55aa00000040 main (/w/perf-probe)\n\n")

    def test_samples_frames_and_unknown_frames_come_from_the_script_itself(self):
        parsed = perfcapture.parse_script(self.SCRIPT)
        self.assertEqual((parsed.samples, parsed.frames, parsed.unknown), (2, 5, 2))
        self.assertEqual(parsed.events, {"task-clock:u"})
        perfcapture.check_event(parsed, "task-clock:u")
        with self.assertRaises(perfcapture.CaptureError) as caught:
            perfcapture.check_event(parsed, "cycles:u")
        self.assertEqual(caught.exception.reason, "event_substituted")
        self.assertEqual(parsed.ratio(), Fraction(2, 5))
        with self.assertRaises(perfcapture.CaptureError) as caught:
            perfcapture.parse_script("\tdangling frame without a sample\n")
        self.assertEqual(caught.exception.reason, "malformed_script")
        with self.assertRaises(perfcapture.CaptureError):
            perfcapture.parse_script("perf 1/1 1.0: x:\n\tnot-an-address frame\n")

    def losses(self, data):
        return perfcapture.decode_losses(lambda offset, count: data[offset:offset + count],
                                         len(data))

    def test_loss_records_are_decoded_from_their_payloads_and_never_summed(self):
        # The retained injected fixture's shape: one LOST record of 777 lost events and one
        # LOST_SAMPLES record of 333 potentially lost samples among ordinary records. Two
        # records are not two losses, and 1110 is no known count of lost samples.
        data = perf_data([(9, b"\0" * 32), (2, struct.pack("<QQ", 7, 777)), (9, b"\0" * 8),
                          (13, struct.pack("<Q", 333) + b"\0" * 8), (68, b"")])
        self.assertEqual(self.losses(data), {
            "state": "decoded", "lost_event_records": 1, "lost_events": 777,
            "lost_sample_records": 1, "potentially_lost_samples": 333, "detail": ""})
        many = perf_data([(2, struct.pack("<QQ", 1, 5)), (2, struct.pack("<QQ", 2, 6))])
        self.assertEqual((self.losses(many)["lost_event_records"],
                          self.losses(many)["lost_events"]), (2, 11))
        none = self.losses(perf_data([(9, b"\0" * 16)]))
        self.assertEqual([none[key] for key in perfcapture.LOSS_KEYS], [0, 0, 0, 0])
        # Anything undecodable is unknown, never a zero.
        unfinished = bytearray(perf_data([(2, struct.pack("<QQ", 7, 777))]))
        unfinished[48:56] = bytes(8)
        for bad, needle in ((b"not a recording", "PERFILE2"),
                            (bytes(unfinished), "did not finish"),
                            (perf_data([(2, struct.pack("<QQ", 7, 777))])[:-4], "outside"),
                            (perf_data([(81, b"\0" * 16)]), "compressed"),
                            (perf_data([(2, b"\0" * 4)]), "LOST record"),
                            (perf_data([(13, b"")]), "LOST_SAMPLES record")):
            with self.subTest(needle=needle):
                found = self.losses(bad)
                self.assertEqual(found["state"], "unknown")
                self.assertEqual([found[key] for key in perfcapture.LOSS_KEYS], [None] * 4)
                self.assertIn(needle, found["detail"])
        malformed = bytearray(perf_data([(9, b"\0" * 8)]))
        malformed[104 + 6:104 + 8] = struct.pack("<H", 4)
        self.assertIn("does not fit", self.losses(bytes(malformed))["detail"])

    def test_build_ids_and_elf_notes_are_parsed_or_refused(self):
        self.assertEqual(perfcapture.parse_build_ids(
            "0123456789abcdef0123456789abcdef01234567 /w/perf-probe\n"),
            [("0123456789abcdef0123456789abcdef01234567", "/w/perf-probe")])
        # perf 6.1 pads a DSO without a build ID with blanks, as a real capture listed it.
        self.assertEqual(perfcapture.parse_build_ids(
            "                                         /run/host/perf-probe\n"),
            [(None, "/run/host/perf-probe")])
        with self.assertRaises(perfcapture.CaptureError):
            perfcapture.parse_build_ids("notabuildid-line\n")
        self.assertIsNone(perfcapture.elf_build_id(b"#!/bin/sh\n"))
        self.assertIsNone(perfcapture.elf_build_id(b"\x7fELF" + b"\x00" * 10))


class Acknowledgements(unittest.TestCase):
    def ack(self, data, close=False, timeout=1.0):
        read, write = os.pipe()
        try:
            if data:
                os.write(write, data)
            if close:
                os.close(write)
                write = None
            return perfcapture.read_ack(read, time.monotonic() + timeout)
        finally:
            os.close(read)
            if write is not None:
                os.close(write)

    def test_perfs_acknowledgement_is_exactly_ack_newline_with_at_most_its_nul(self):
        self.assertEqual(self.ack(b"ack\n\x00"), b"ack\n\x00")
        self.assertEqual(self.ack(b"ack\n"), b"ack\n")
        for data, reason in ((b"nak\n\x00", "control_protocol"), (b"ack\n\x00\x00junk",
                                                                   "control_protocol"),
                             (b"", "control_closed")):
            with self.subTest(data=data):
                with self.assertRaises(perfcapture.CaptureError) as caught:
                    self.ack(data, close=True)
                self.assertEqual(caught.exception.reason, reason)
        started = time.monotonic()
        with self.assertRaises(perfcapture.CaptureError) as caught:
            self.ack(b"", timeout=0.2)
        self.assertEqual(caught.exception.reason, "control_timeout")
        self.assertLess(time.monotonic() - started, 2)


class BoundedTools(TempDir):
    def test_a_tool_runs_only_as_a_supervisor_request_bounded_by_the_deadline(self):
        boss = supervisor(self.path("supervisor"))
        env = {"PATH": "/usr/bin:/bin"}
        started = time.monotonic()
        slow = toolrun.run(boss, ["/bin/sh", "-c", "sleep 30"], env, 0.3, 64, cwd=self.tmp)
        self.assertTrue(slow.timed_out)
        self.assertLess(time.monotonic() - started, 10)
        self.assertIn("killed", slow.failure())
        loud = toolrun.run(boss, ["/bin/sh", "-c", "yes | head -c 5000"], env, 10, 64,
                           cwd=self.tmp)
        self.assertEqual((len(loud.stdout), loud.stdout_bytes), (64, 5000))
        self.assertFalse(loud.ok)
        self.assertIn("over its 64-byte output cap", loud.failure())
        missing = toolrun.run(boss, [self.path("absent")], {}, 5, 64, cwd=self.tmp)
        self.assertIn("cannot run", missing.failure())
        # No time beyond the supervisor's reserve: nothing launches, and it is no success.
        past = toolrun.run(boss, [sys.executable, "-I", "-c", "pass"], {}, 5, 64, cwd=self.tmp,
                           deadline=time.monotonic() + 0.5)
        self.assertFalse(past.ok)
        self.assertTrue(past.late)
        with open(os.path.join(self.path("supervisor"), "supervisor-calls")) as stream:
            self.assertEqual(stream.read(), "3", "the refused run reached no supervisor")
        # A reply after the deadline is a failure whatever it says: this clock passes the
        # deadline while the tool runs and exits 0.
        offset = [0.0]
        clock = lambda: time.monotonic() + offset[0]  # noqa: E731
        running = toolrun.start(boss, ["/bin/sh", "-c", "sleep 1"], env, 5, 64, cwd=self.tmp,
                                deadline=clock() + 30, clock=clock)
        time.sleep(0.3)
        offset[0] = 100.0
        late = running.wait()
        self.assertEqual(late.code, 0)
        self.assertTrue(late.late)
        self.assertIn("after the diagnostic deadline", late.failure())
        # Output beyond a stream cap goes to a new file the tool writes within its room.
        written = toolrun.run(boss, ["/bin/sh", "-c", "yes | head -c 5000"], env, 10, 64,
                              cwd=self.tmp, stdout_path=self.path("out"), file_bytes=6000)
        self.assertTrue(written.ok, written.failure())
        self.assertEqual(os.path.getsize(self.path("out")), 5000)
        again = toolrun.run(boss, [sys.executable, "-I", "-c", "pass"], env, 10, 64, cwd=self.tmp,
                            stdout_path=self.path("out"))
        self.assertIn("already exists", again.failure())
        bounded = toolrun.run(boss, ["/bin/sh", "-c", "yes | head -c 50000"], env, 10, 64,
                              cwd=self.tmp, stdout_path=self.path("big"), file_bytes=4096)
        self.assertFalse(bounded.ok, "the tool could not outgrow its byte room")
        self.assertLessEqual(os.path.getsize(self.path("big")), 4096)

    def test_a_running_tool_is_cancelled_through_its_supervisor(self):
        boss = supervisor(self.path("supervisor"))
        running = toolrun.start(boss, ["/bin/sh", "-c", "sleep 30"], {"PATH": "/usr/bin:/bin"},
                                60, 64, cwd=self.tmp)
        time.sleep(0.5)
        self.assertFalse(running.done())
        started = time.monotonic()
        running.cancel()
        result = running.wait()
        self.assertLess(time.monotonic() - started, 10)
        self.assertTrue(result.cancelled)
        self.assertIn("cancelled", result.failure())

    def test_inferno_output_that_is_not_collapsed_stacks_is_refused(self):
        self.assertEqual(flamegraph.check_folded(b"main;work 3\nmain 1\n"), 4)
        self.assertEqual(flamegraph.check_folded(b"main;work 3 5\n", differential=True), 5)
        for bad in (b"main;work three\n", b" 3\n", b"\xff 1\n"):
            with self.assertRaises(flamegraph.RenderError):
                flamegraph.check_folded(bad)
        merged = flamegraph.merge_folded([b"b 1\na 2\n", b"a 3\n"])
        self.assertEqual(merged, b"a 5\nb 1\n")


class Flamegraphs(TempDir):
    def test_both_directions_of_the_difference_keep_disappearing_stacks_visible(self):
        tools = fakes.inferno(self.path("inferno"))
        os.makedirs(self.path("run", "flamegraphs", "case"))
        os.makedirs(self.path("run", "in"))
        folded = {"baseline": b"main;old_work 40\nmain;shared 60\n",
                  "candidate": b"main;new_work 30\nmain;shared 70\n"}
        for side, data in folded.items():
            with open(self.path("run", "in", side), "wb") as stream:
                stream.write(data)
        budget = diagnostics.Budget({"total_minutes": 15, "profile_bytes": 1 << 24,
                                     "builds": 2, "tool_output_bytes": 1 << 20,
                                     "tool_seconds": 30}, self.path("run"))
        profile = diagnostics.Profile.__new__(diagnostics.Profile)
        profile.output, profile.budget, profile.clock = self.path("run"), budget, time.monotonic
        profile.supervisor = supervisor(self.path("supervisor"))
        profile.execution = {"mode": diagnostics.TRUSTED_LOCAL}
        profile.retained = []
        renderer = diagnostics.Renderer(profile, dict(
            (name, os.path.join(tools, name.replace("_", "-"))) for name in flamegraph.TOOLS))
        renders = flamegraph.render_case(renderer, "flamegraphs/case", "case",
                                         {"baseline": "in/baseline", "candidate": "in/candidate"})
        os.rename(self.path("run", "flamegraphs", "case"), self.path("out"))
        self.assertEqual([item["kind"] for item in renders],
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        forward = read(self.path("out", "difference_forward.folded")).decode()
        reverse = read(self.path("out", "difference_reverse.folded")).decode()
        self.assertNotIn("old_work", forward)
        self.assertIn("main;old_work 0 40", reverse, "the reverse direction keeps the stack the "
                      "candidate lost")
        for item in renders:
            sanitized = read(self.path("out", os.path.basename(item["sanitized"]["path"])))
            original = read(self.path("out", os.path.basename(item["original"]["path"])))
            self.assertIn(b"<script", original)
            self.assertNotIn(b"<script", sanitized)
            self.assertNotIn(b"onload", sanitized)
            self.assertEqual(item["sanitized"]["sha256"], sha256_bytes(sanitized))
            self.assertEqual(item["original"]["sha256"], sha256_bytes(original))
            self.assertEqual(item["color"], "sample_share_change"
                             if item["kind"].startswith("difference") else "sample_count")
        self.assertIn(b"(negated)", read(self.path("out", "difference_reverse.original.svg")))
        self.assertEqual(renders[3]["inputs"], renders[2]["inputs"][::-1])


REAL_TOOL = os.path.join(ROOT, "fixtures/performance/diagnostic/real-tool-synthetic")


class RealToolFixture(TempDir):
    """Retained output of a real perf 6.1 over an owned synthetic C workload (`fixture.c`), with
    only its machine-specific DSO prefix normalized (see the fixture's README): the actual
    sample header format and the libc frame perf left unresolved. It is not a capture of the AF
    probe, and no performance is claimed from it."""

    def script(self, side):
        return read(os.path.join(REAL_TOOL, "%s.perf-script.txt" % side)).decode("utf-8")

    def test_the_parser_reads_real_perf_script_samples_frames_and_unknown_frames(self):
        registry = diagnostics.load(REGISTRY, suite_module.load(SUITE).doc)
        threshold = Fraction(registry["capture"]["dwarf_when_unknown_ratio_above"])
        for side, chosen in (("baseline", "old_work"), ("candidate", "new_work")):
            with self.subTest(side=side):
                text = self.script(side)
                header = text.splitlines()[0]
                self.assertRegex(header, r"^stack-fixture +[0-9]+/[0-9]+ +[0-9]+\.[0-9]{6}: "
                                 r"task-clock:u: $")
                parsed = perfcapture.parse_script(text)
                # The two chosen records: the side's own work, then shared_work; each six
                # frames, one of them the libc frame perf could not resolve.
                self.assertEqual((parsed.samples, parsed.frames, parsed.unknown), (2, 12, 2))
                self.assertEqual(parsed.events, {"task-clock:u"})
                perfcapture.check_event(parsed, "task-clock:u")
                self.assertEqual(parsed.ratio(), Fraction(1, 6))
                self.assertLessEqual(parsed.ratio(), threshold,
                                     "frame pointers unwound it: no DWARF alternative is due")
                self.assertIn(chosen, text)
                self.assertEqual(text.count("[unknown] (/usr/lib/aarch64-linux-gnu/libc.so.6)"),
                                 2)
                listed = perfcapture.parse_build_ids(read(os.path.join(
                    REAL_TOOL, "%s.buildid-list.txt" % side)).decode("utf-8"))
                self.assertEqual(listed, [
                    ("611870d34ca7b0f7f70efb81ab546d1b3826218d", "[vdso]"),
                    ("16e7fc798f754633a38d91ba09be84f8aa839464",
                     "/usr/lib/aarch64-linux-gnu/libc.so.6")])

    def test_real_script_text_collapses_and_differs_in_both_directions(self):
        # The collapse and difference are the stand-in inferno tools here; real inferno 0.12.8
        # over these records is retained outside the repository (ADR-0175, ADR-0176).
        tools = fakes.inferno(self.path("inferno"))
        os.makedirs(self.path("run", "in"))
        budget = diagnostics.Budget({"total_minutes": 15, "profile_bytes": 1 << 24,
                                     "builds": 2, "tool_output_bytes": 1 << 20,
                                     "tool_seconds": 30}, self.path("run"))
        profile = diagnostics.Profile.__new__(diagnostics.Profile)
        profile.output, profile.budget, profile.clock = self.path("run"), budget, time.monotonic
        profile.supervisor = supervisor(self.path("supervisor"))
        profile.execution = {"mode": diagnostics.TRUSTED_LOCAL}
        profile.retained = []
        folded = {}
        for side in diagcontracts.SIDES:
            with open(self.path("run", "in", side + ".script"), "w") as stream:
                stream.write(self.script(side))
            folded[side] = "in/%s.folded" % side
            profile.tool_output(folded[side], [os.path.join(tools, "inferno-collapse-perf"),
                                               side + ".script"], "collapse " + side)
        # The stand-in keeps perf's `[unknown]` frame and no process name; real inferno writes
        # `stack-fixture;...;[libc.so.6];main`, as its retained output over these runs shows.
        libc = "_start;__libc_start_main;[unknown];main"
        self.assertEqual(read(self.path("run", "in", "baseline.folded")).decode(),
                         "%s;old_work;leaf_work 1\n%s;shared_work;leaf_work 1\n" % (libc, libc))
        os.makedirs(self.path("run", "flamegraphs", "case"))
        renderer = diagnostics.Renderer(profile, dict(
            (name, os.path.join(tools, name.replace("_", "-"))) for name in flamegraph.TOOLS))
        renders = flamegraph.render_case(renderer, "flamegraphs/case", "case", folded)
        self.assertEqual([item["kind"] for item in renders],
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        forward = read(self.path("run", "flamegraphs", "case",
                                 "difference_forward.folded")).decode()
        reverse = read(self.path("run", "flamegraphs", "case",
                                 "difference_reverse.folded")).decode()
        self.assertEqual(forward, "%s;new_work;leaf_work 0 1\n%s;shared_work;leaf_work 1 1\n"
                         % (libc, libc))
        self.assertEqual(reverse, "%s;old_work;leaf_work 0 1\n%s;shared_work;leaf_work 1 1\n"
                         % (libc, libc), "the stack the candidate lost stays visible")


class Traces(unittest.TestCase):
    def trace(self, spans, dropped=0, open_spans=0):
        return contracts.validate_trace({
            "schema": "af.perf-trace/1", "pid": 7, "clock": "monotonic_ns",
            "epoch_unix_ns": "1", "capacity": 64, "dropped": dropped, "open": open_spans,
            "spans": [dict(zip(("id", "parent", "phase", "tid", "start_ns", "end_ns"), span))
                      for span in spans]})

    def test_nested_spans_share_a_track_and_a_non_nesting_one_gets_an_async_track(self):
        doc = self.trace([(1, None, "probe_region", 1, 0, 1000),
                          (4, None, "task_projection", 2, 50, 60),
                          (2, 1, "cas_get", 1, 100, 400),
                          (3, 1, "canonicalize", 1, 300, 1200)])
        placed = phasetrace.tracks(doc)
        self.assertEqual(placed[1], ("nested", 0))
        self.assertEqual(placed[2], ("nested", 1))
        self.assertEqual(placed[3], ("async", None))
        self.assertEqual(placed[4], ("nested", 0))
        exported = json.loads(phasetrace.perfetto(doc, "case baseline").decode("utf-8"))
        events = exported["traceEvents"]
        complete = [event for event in events if event["ph"] == "X"]
        self.assertEqual(sorted(event["args"]["id"] for event in complete), [1, 2, 4])
        self.assertEqual([event["ph"] for event in events if event.get("id") == 3], ["b", "e"])
        self.assertEqual(complete[0]["ts"], 0.0)
        self.assertEqual([event["dur"] for event in complete if event["args"]["id"] == 2],
                         [0.3])
        self.assertEqual(exported["metadata"]["dropped"], 0)

    def test_phase_totals_are_unions_and_never_a_false_sum(self):
        doc = self.trace([(1, None, "cas_get_artifact", 1, 0, 1000),
                          (2, 1, "canonicalize", 1, 100, 300),
                          (3, 2, "canonicalize", 1, 150, 250),
                          (4, 1, "canonicalize", 1, 600, 700)])
        rows = dict((row["phase"], row) for row in phasetrace.phase_summary(doc))
        self.assertEqual(rows["canonicalize"]["covered_ns"], 300, "200 + 100, the nested 100 "
                         "inside the first counted once")
        self.assertEqual(rows["canonicalize"]["spans"], 3)
        self.assertEqual(rows["cas_get_artifact"]["covered_ns"], 1000)
        self.assertEqual(phasetrace.union_ns([(0, 10), (5, 20), (30, 31)]), 21)
        svg = phasetrace.timeline_svg(doc).text
        self.assertTrue(svg.startswith("<svg") and "cas_get_artifact #1" in svg)

    def test_overhead_is_the_exact_median_of_paired_relative_overheads(self):
        self.assertEqual(diagcontracts.trace_overhead(["10", "20", "10"], ["10.1", "20.2", "11"]),
                         Fraction(1, 100))
        self.assertEqual(diagcontracts.trace_overhead(["10", "10"], ["10.2", "10.6"]),
                         Fraction(4, 100))
        with self.assertRaises(ContractError):
            diagcontracts.trace_overhead(["0"], ["1"])


class Region(TempDir):
    """The acknowledged bracket over a real FIFO handshake with the stand-ins."""

    def setUp(self):
        TempDir.setUp(self)
        self.golden = golden()
        self.doc = suite_module.load(SUITE).doc

    def capture(self, probe_config, perf_config=None, capture_seconds=0.25, on_next=None):
        probe = fakes.probe(self.path("probe"), self.golden, **probe_config)
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe", "phase"),
                          **(perf_config or {}))
        os.mkdir(self.path("work"))
        request = oracle.request_for(self.doc, SMALL, self.path("work"))
        with open(self.path("request.json"), "wb") as stream:
            stream.write(json.dumps(request).encode("utf-8"))
        os.mkdir(self.path("fifo"))
        control = perfcapture.PerfControl(self.path("fifo"))
        data = self.path("capture.data")
        argv = perfcapture.record_argv(perf, "task-clock:u", 997, "frame_pointer", data,
                                       control, [probe])
        messages = []
        try:
            run = perfcapture.bracketed_run(
                supervisor(self.path("supervisor")), argv, {"PATH": "/usr/bin:/bin"}, self.tmp,
                self.path("request.json"), self.path("fifo"),
                lambda deadline: control.send(b"enable\n", deadline),
                lambda deadline: control.send(b"disable\n", deadline),
                lambda index, message: messages.append(message), capture_seconds,
                time.monotonic() + 30, 2, 25, on_next=on_next)
        finally:
            control.close()
        run.messages = messages
        return run, control, data, request

    def samples(self, data):
        """The stand-in's samples, from the records of its perf.data that hold them."""
        raw = read(data)
        offset, size = struct.unpack_from("<QQ", raw, 40)
        position, body = offset, b""
        while position < offset + size:
            kind, _, length = struct.unpack_from("<IHH", raw, position)
            if kind == 99:
                body += raw[position + 8:position + length]
            position += length
        return json.loads(body)["samples"]

    def executions(self):
        return len(read(self.path("probe", "executions")).splitlines())

    def reset_work(self, run):
        shutil.rmtree(self.path("work"))
        os.mkdir(self.path("work"))
        return True

    def test_a_slow_setup_never_reaches_a_capture_of_a_short_region(self):
        run, control, data, request = self.capture(
            {"setup_seconds": 1.0, "region_seconds": 0.15})
        self.assertEqual(run.code, 0)
        [message] = run.messages
        judged = oracle.judge(self.doc, self.golden, request, run.code, message)
        self.assertEqual(judged["status"], "passed")
        self.assertEqual(run.result.stdout_bytes, 0, "a driven probe writes nothing to stdout")
        [region] = run.regions
        self.assertGreaterEqual(region.begun - run.launched, 1.0, "setup preceded the region")
        self.assertLess(run.recorded(), 0.6, "only the region was enabled")
        self.assertEqual(control.acknowledgements, [b"ack\n\x00", b"ack\n\x00"])
        samples = self.samples(data)
        self.assertTrue(samples)
        self.assertNotIn("setup", samples, "a sample of the setup was recorded")
        self.assertNotIn("after", samples, "a sample after the region was recorded")
        self.assertIn("region", samples)

    def counted_reset(self, repetitions):
        """A driver that replaces the state and asks for exactly `repetitions` executions in
        all, then declines: the repetition count is the driver's, never the scheduler's."""
        asked = []

        def on_next(run):
            if len(run.regions) >= repetitions:
                return False
            asked.append(len(run.regions))
            return self.reset_work(run)
        return on_next, asked

    def test_short_regions_repeat_as_fresh_executions_in_one_recording_on_fresh_state(self):
        # One perf event for every repetition (a new one per process never reaches its
        # sampling period over a sub-period region), each repetition a fresh execution of the
        # probe on a fresh scratch: the stand-in exits 4 if it finds the last repetition's file.
        # The driver asks for exactly four repetitions under a cap they cannot reach, so how
        # many complete never depends on how fast this host schedules the handshake.
        on_next, asked = self.counted_reset(4)
        run, control, data, request = self.capture(
            {"setup_seconds": 0.2, "region_seconds": 0.05}, capture_seconds=20,
            on_next=on_next)
        self.assertEqual(run.code, 0, run.stderr)
        self.assertEqual(len(run.regions), 4)
        self.assertEqual(asked, [1, 2, 3], "the state was replaced before every repetition")
        self.assertEqual(run.stopped, "declined")
        for region in run.regions:
            # Each region ran its whole 0.05 s recorded, after its own 0.2 s setup.
            self.assertGreaterEqual(region.disabled - region.enabled, 0.05)
            self.assertGreaterEqual(region.begun - region.started, 0.2)
            self.assertGreaterEqual(region.enabled, region.begun,
                                    "every repetition's setup stayed out")
        self.assertEqual(len(run.messages), len(run.regions))
        self.assertEqual(self.executions(), len(run.regions), "one execution per repetition")
        for line in run.messages:
            self.assertEqual(oracle.judge(self.doc, self.golden, request, 0, line)["status"],
                             "passed")
        self.assertEqual(len(control.acknowledgements), 2 * len(run.regions))
        samples = self.samples(data)
        self.assertEqual(set(samples) - {"handshake"}, {"region"})

    def test_repetitions_stop_once_the_recorded_interval_reaches_its_cap(self):
        # The real capture bound: a driver that would always repeat is stopped by the cap, and
        # every repetition that did run was a fresh execution judged on its own.
        run, control, _, request = self.capture(
            {"setup_seconds": 0, "region_seconds": 0.05}, capture_seconds=0.2,
            on_next=self.reset_work)
        self.assertEqual(run.code, 0, run.stderr)
        self.assertEqual(run.stopped, "capped")
        self.assertGreaterEqual(run.recorded(), 0.2)
        self.assertGreaterEqual(len(run.regions), 1)
        self.assertEqual(self.executions(), len(run.regions))
        self.assertEqual(len(run.messages), len(run.regions))
        for line in run.messages:
            self.assertEqual(oracle.judge(self.doc, self.golden, request, 0, line)["status"],
                             "passed")
        self.assertEqual(len(control.acknowledgements), 2 * len(run.regions))

    def test_a_probe_that_repeats_in_its_warm_process_is_refused(self):
        with self.assertRaises(perfcapture.CaptureError) as caught:
            self.capture({"setup_seconds": 0, "region_seconds": 0.02, "stay_warm": True},
                         capture_seconds=0.5, on_next=self.reset_work)
        self.assertEqual(caught.exception.reason, "probe_exited")
        self.assertIn("warm repetition", caught.exception.detail)

    def test_a_stale_scratch_is_refused_by_the_probe_and_not_presented_as_a_capture(self):
        # A driver that answers `again` without replacing the state: the second repetition
        # finds the first one's file and the capture ends, never recorded as fresh.
        with self.assertRaises(perfcapture.CaptureError) as caught:
            self.capture({"setup_seconds": 0, "region_seconds": 0.02}, capture_seconds=0.5,
                         on_next=lambda run: True)
        self.assertEqual(caught.exception.reason, "probe_exited")
        self.assertIn("stale scratch", caught.exception.detail)

    def test_a_probe_that_fails_before_its_region_has_its_message_judged(self):
        with self.assertRaises(perfcapture.CaptureError) as caught:
            self.capture({"setup_seconds": 0, "fail_before_region": True})
        self.assertEqual(caught.exception.reason, "probe_failed")
        self.assertIn("without reaching its region", caught.exception.detail)

    def test_the_capture_cap_disables_a_region_that_is_still_running(self):
        run, _, _, _ = self.capture({"setup_seconds": 0, "region_seconds": 0.8},
                                    capture_seconds=0.2)
        self.assertEqual(run.code, 0)
        [region] = run.regions
        self.assertTrue(region.truncated)
        self.assertLess(region.disabled - region.enabled, 0.6)
        self.assertGreater(region.ended - region.enabled, 0.6)

    def test_an_early_exit_or_a_silent_profiler_ends_the_capture_with_its_reason(self):
        with self.assertRaises(perfcapture.CaptureError) as caught:
            self.capture({"setup_seconds": 0.1, "exit_before_region": True})
        self.assertEqual(caught.exception.reason, "probe_exited")
        shutil.rmtree(self.path("probe"))
        shutil.rmtree(self.path("perf"))
        shutil.rmtree(self.path("work"))
        shutil.rmtree(self.path("fifo"))
        os.remove(self.path("request.json"))
        with self.assertRaises(perfcapture.CaptureError) as caught:
            self.capture({"setup_seconds": 0}, {"never_ack": True})
        self.assertEqual(caught.exception.reason, "control_timeout")


class Capability(TempDir):
    def test_a_real_acknowledged_capture_is_the_capability_and_no_samples_is_none(self):
        boss = supervisor(self.path("supervisor"))
        perf = fakes.perf(self.path("perf"))
        os.mkdir(self.path("work"))
        available, reason, detail, version = perfcapture.probe_capability(
            boss, perf, "task-clock:u", 997, self.path("work"), {"PATH": "/usr/bin:/bin"}, 20)
        self.assertTrue(available, detail)
        self.assertEqual(version, "perf version 6.1.fake")
        self.assertIn("samples decoded under acknowledged control", detail)
        silent = fakes.perf(self.path("silent"), no_samples=True)
        os.mkdir(self.path("work2"))
        available, reason, detail, _ = perfcapture.probe_capability(
            boss, silent, "cycles:u", 997, self.path("work2"), {"PATH": "/usr/bin:/bin"}, 20)
        self.assertFalse(available)
        self.assertEqual(reason, "event_unsupported")
        self.assertIn("cycles:u", detail)
        # perf's silent fallback (`cycles:u` recorded as `cpu-clock:uH`, as the real perf 6.1 of
        # the configured image did) is refused, never relabelled.
        substituting = fakes.perf(self.path("substituting"), recorded_event="cpu-clock:uH")
        os.mkdir(self.path("work4"))
        available, reason, detail, _ = perfcapture.probe_capability(
            boss, substituting, "cycles:u", 997, self.path("work4"), {"PATH": "/usr/bin:/bin"}, 20)
        self.assertEqual((available, reason), (False, "event_substituted"))
        self.assertIn("cpu-clock:uH instead of the declared cycles:u", detail)
        os.mkdir(self.path("work3"))
        available, reason, _, _ = perfcapture.probe_capability(
            boss, self.path("absent-perf"), "task-clock:u", 997, self.path("work3"), {}, 5)
        self.assertEqual((available, reason), (False, "perf_unavailable"))


class Counters(TempDir):
    def test_callgrind_dhat_and_samply_outputs_are_parsed_or_refused(self):
        self.assertEqual(counters.parse_callgrind("events: Ir\nsummary: 12\ntotals: 12\n"), 12)
        for bad in ("events: Dr\ntotals: 1\n", "events: Ir\n", "events: Ir\ntotals: 1\ntotals: 2\n",
                    "events: Ir\nsummary: 2\ntotals: 1\n"):
            with self.assertRaises(counters.CounterError):
                counters.parse_callgrind(bad)
        self.assertEqual(counters.parse_dhat("==9== Total:     1,234 bytes in 56 blocks\n"),
                         (56, 1234))
        with self.assertRaises(counters.CounterError):
            counters.parse_dhat("no summary")
        self.assertEqual(counters.samply_samples(json.dumps(
            {"threads": [{"samples": {"length": 5}}, {"samples": {"data": [[1], [2]]}}]}
        ).encode()), 7)
        import gzip
        self.assertEqual(counters.samply_samples(gzip.compress(
            b'{"threads": [{"samples": {"length": 3}}]}')), 3)
        self.assertIsNone(counters.samply_samples(b"not json"))
        self.assertIsNone(counters.samply_samples(b'{"threads": [{"samples": 3}]}'))
        # A profile that would inflate beyond its bound is never inflated whole.
        bomb = gzip.compress(b" " * (counters.SAMPLY_PROFILE_BYTES_MAX + 1))
        self.assertIsNone(counters.samply_samples(bomb))

    def test_samply_needs_its_flags_and_refuses_protected_or_hardened_targets(self):
        def samply(path, help_text):
            with open(path, "w") as stream:
                stream.write("#!/bin/sh\nif [ \"$1\" = --version ]; then echo samply 0.0.fake; "
                             "exit 0; fi\ncat <<'EOF'\n%s\nEOF\n" % help_text)
            os.chmod(path, 0o755)
            return path
        boss = supervisor(self.path("supervisor"))
        full = samply(self.path("samply"), "--save-only --rate --output")
        target = self.path("target")
        with open(target, "w") as stream:
            stream.write("x")
        env = {"PATH": "/usr/bin:/bin"}
        available, _, detail, version = counters.samply_capability(boss, full, target, env, 10,
                                                                   codesign=self.path("none"))
        self.assertTrue(available, detail)
        self.assertEqual(version, "samply 0.0.fake")
        partial = samply(self.path("old-samply"), "--rate --output")
        available, reason, detail, _ = counters.samply_capability(boss, partial, target, env, 10,
                                                                  codesign=self.path("none"))
        self.assertEqual((available, reason), (False, "samply_flags_missing"))
        self.assertIn("--save-only", detail)
        # Model macOS canonical paths, independent of the verifier's system symlinks
        # (on Linux /bin/ls can resolve outside every macOS protected prefix).
        protected_paths = ("/System/Library/af-fixture", "/usr/bin/af-fixture",
                           "/usr/sbin/af-fixture", "/usr/libexec/af-fixture",
                           "/bin/af-fixture", "/sbin/af-fixture")
        link = self.path("protected-link")
        os.symlink(protected_paths[0], link)
        for requested, resolved in ([(path, path) for path in protected_paths]
                                    + [(link, protected_paths[0])]):
            with self.subTest(requested=requested, resolved=resolved):
                with mock.patch.object(counters.os.path, "realpath",
                                       return_value=resolved) as realpath:
                    available, reason, detail, version = counters.samply_capability(
                        boss, full, requested, env, 10, codesign=self.path("none"))
                realpath.assert_called_once_with(requested)
                self.assertEqual((available, reason), (False, "protected_executable"))
                self.assertIn(resolved, detail)
                self.assertEqual(version, "samply 0.0.fake")
        resolve = os.path.realpath
        for real in ("/usr/lib/cargo/bin/coreutils", "/usr/lib/cargo/bin/coreutils/ls"):
            # Only the target resolves to the multicall binary; every other path is itself.
            with mock.patch.object(counters.os.path, "realpath", side_effect=lambda path, real=real:
                                   real if path == target else resolve(path)):
                available, reason, _, _ = counters.samply_capability(
                    boss, full, target, env, 10, codesign=self.path("none"))
            self.assertEqual((available, reason), (False, "protected_executable"), real)
        hardened = self.path("codesign")
        with open(hardened, "w") as stream:
            stream.write("#!/bin/sh\necho 'CodeDirectory v=20500 size=1 flags=0x10000(runtime) "
                         "hashes=1' >&2\n")
        os.chmod(hardened, 0o755)
        available, reason, _, _ = counters.samply_capability(boss, full, target, env, 10,
                                                             codesign=hardened)
        self.assertEqual((available, reason), (False, "hardened_runtime"))


class ChildRunBase(TempDir):
    PARENT_CASES = ["canonical_serialize_small", "canonical_validate_small", "cas_raw_get_small",
                    "state_projection", "cli_discovery", "cli_state_read"]

    """A whole child run over stand-ins: the parent plan, the frozen child plan, captures,
    flamegraphs, traces and counters, profiles.json, the page and the manifest."""

    def setUp(self):
        TempDir.setUp(self)
        text = read(REGISTRY)
        self.repo, self.root = lab_repo(self.path("repo"))
        self.root = self.repo.commit("diagnostics", {diagnostics.REGISTRY_PATH: text})
        self.git = gitrefs.Git(self.repo.path)
        self.parent = planning.plan(self.git, planning.Request(
            trigger="manual", tier="quick", scope="partial_scope", base=self.root,
            head=self.root, run_id="parent_run", seed=5, trusted_host_build=True,
            cases=self.PARENT_CASES))
        with open(self.path("parent.json"), "wb") as stream:
            stream.write(contracts.write_bytes(self.parent, "run"))
        self.suite = suite_module.load(SUITE)
        self.registry = diagnostics.load(REGISTRY, self.suite.doc)
        self.golden = golden()

    def child(self, recipe, cases, duration_ms=300, run_id="child_run"):
        request = planning.Request(scope="diagnostic", parent_plan=self.parent, cases=cases,
                                   run_id=run_id, diagnostic_duration_ms=duration_ms)
        return planning._diagnostic(self.git, request, recipe=(
            recipe, diagcontracts.diagnostic_recipe(self.suite.doc, self.registry, recipe),
            diagcontracts.frozen_identity(self.registry, recipe)))

    def profile(self, recipe, cases, tools, probe_config, parent_probes=None, duration_ms=300,
                registry=None, af_config=None, parent_af_config=None, contained=None,
                prior_bytes=0, work_removed=True, run_id="child_run", telemetry=None, distinct_candidate=False,
                cli_version="0.12.0-rc.3"):
        plan = self.child(recipe, cases, duration_ms, run_id)
        output = self.path("child")
        os.makedirs(output)
        # The frozen child plan, written first as `run_profile` writes it.
        plan_bytes = contracts.write_bytes(plan, "run")
        with open(os.path.join(output, diagnostics.PLAN_FILE), "xb") as stream:
            stream.write(plan_bytes)
        if prior_bytes:
            # Evidence the run already holds, its plan included, counted against its room.
            with open(os.path.join(output, "prior.bin"), "wb") as stream:
                stream.write(b"p" * (prior_bytes - len(plan_bytes)))
        if registry is None:
            # The stand-in perf samples a little slower than 1 kHz; 20 is its minimum here.
            registry = json.loads(json.dumps(self.registry))
            registry["capture"]["minimum_samples"] = 20
        budget = diagnostics.Budget(registry["limits"], output)
        selection, chosen = diagnostics.select(self.parent, explicit=cases)
        probe = fakes.probe(self.path("probe-bin"), self.golden, **probe_config)
        probes = dict((side, (probe, sha256_bytes(read(probe)))) for side in diagcontracts.SIDES)
        if distinct_candidate:
            candidate = fakes.probe(self.path("candidate-probe"), self.golden, **probe_config)
            with open(candidate, "ab") as stream:
                stream.write(b"\n# distinct candidate executable\n")
            probes["candidate"] = (candidate, sha256_bytes(read(candidate)))
        boss = supervisor(self.path("supervisor"))
        found = diagnostics.probe_tools(registry, tools, "linux",
                                        registry["recipes"][recipe]["tools"], boss,
                                        time.monotonic() + 60, cwd=self.tmp)
        execution = dict(mode="trusted_local", containment="process_group",
                         declared_containment="linux_container", matches_environment=False,
                         supervisor_sha256="sha256:" + "5" * 64, machine_lock=None,
                         scratch_bound="not_enforced_in_trusted_local_mode", container=None)
        work = None
        if contained is not None:
            # The contained mode over the stand-in container runtime: one container per
            # request, every writable bind in the run's own directory of the scratch root.
            work = os.path.join(contained["scratch_root"], "diagnostic")
            os.mkdir(work, 0o700)
            boss = toolrun.Contained(boss.path, contained["docker"], "sha256:" + "0" * 64,
                                     plan["profile"]["environment"], contained["scratch_root"],
                                     os.getuid(), os.getgid(), [0, 1], "child_run",
                                     grace_ms=100)
            execution.update(mode="linux_container", containment="pid_namespace",
                             matches_environment=True, scratch_bound="verified_disk_bounded",
                             container={"image": "sha256:" + "0" * 64,
                                        "runtime": "stand-in docker", "cpus": [0, 1],
                                        "scratch_limit_bytes": plan["profile"]["environment"][
                                            "scratch_limit_bytes"]})
        afs, parent_afs, cli = {}, {}, None
        if af_config is not None:
            binary = fakes.af(self.path("af-bin"), real_home=os.environ.get("HOME"),
                              **af_config)
            afs = dict((side, (binary, sha256_bytes(read(binary))))
                       for side in diagcontracts.SIDES)
            cli_doc = clicases.load(os.path.join(ROOT, clicases.CLI_PATH))
            versions = dict((side, cli_version) for side in diagcontracts.SIDES)
            cli = diagnostics.CliAuthority(
                cli_doc, os.path.join(ROOT, "fixtures/performance/cli"), versions,
                dict((side, clicases.choose_adapter(cli_doc, versions[side]))
                     for side in diagcontracts.SIDES))
        if parent_af_config is not None:
            binary = fakes.af(self.path("parent-af-bin"), real_home=os.environ.get("HOME"),
                              **parent_af_config)
            parent_afs = dict((side, (binary, sha256_bytes(read(binary))))
                              for side in diagcontracts.SIDES)
        profile = diagnostics.Profile(
            output, self.parent, plan, registry, sha256_bytes(read(REGISTRY)), recipe,
            selection, chosen, self.suite.doc, self.golden, probes, parent_probes or {}, found,
            budget, lambda instance: ["python3", "scripts/performance/cli.py", "profile",
                                      "--case", instance or "-"], "sha256:" + "d" * 64, "linux",
            supervisor=boss, execution=execution, cli=cli, afs=afs, parent_afs=parent_afs,
            work=work, telemetry=telemetry)
        doc = profile.run()
        diagnostics.finish(output, doc, profile.trace_docs, budget)
        reread = contracts.read_file(os.path.join(output, diagnostics.PROFILES_FILE), "profiles")
        self.assertEqual(reread, doc)
        self.assertEqual(os.path.exists(os.path.join(output, diagnostics.WORK_DIR)),
                         not work_removed, "the run's own work directory is removed only when "
                         "nothing of it was kept for recovery")
        self.profile_object = profile
        return doc, output


class ChildRun(ChildRunBase):

    def test_a_cpu_profile_child_run_captures_both_sides_and_renders_four_graphs(self):
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"),
                          unknown_with_frame_pointers=2)
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        registry = json.loads(json.dumps(self.registry))
        registry["capture"].update(max_repetitions=3, minimum_samples=20)
        doc, output = self.profile("linux_gnu_cpu_profile", [SMALL], tools,
                                   {"setup_seconds": 0.4, "region_seconds": 0.12},
                                   duration_ms=20000, registry=registry)
        self.assertEqual(doc["verdict_effect"], "none")
        self.assertEqual(doc["parent_run_id"], "parent_run")
        self.assertEqual(doc["recipe"]["features"], ["perf-control"])
        # Frame pointers left 2 of 5 frames unknown, above the declared 0.25: one DWARF capture
        # per side, both kept.
        attempts = sorted((item["side"], item["attempt"], item["unwind"])
                          for item in doc["captures"])
        self.assertEqual(attempts, [("baseline", 1, "frame_pointer"), ("baseline", 2, "dwarf"),
                                    ("candidate", 1, "frame_pointer"), ("candidate", 2, "dwarf")])
        for item in doc["captures"]:
            self.assertIsNone(item["omission"], item["omission"])
            region = item["region"]
            self.assertTrue(region["observed"])
            # Ask for three repetitions under a cap they cannot reach. Scheduler delays in
            # the real FIFO handshake must not decide how many fresh executions this tests.
            self.assertEqual(region["repetitions"], 3)
            self.assertEqual(region["stopped"], "repetition_cap")
            self.assertGreaterEqual(Fraction(region["region_ms"]), 300)
            if not region["truncated_at_cap"]:
                # A region the cap disabled kept running unrecorded; otherwise recording
                # spanned every region and its handshakes.
                self.assertGreaterEqual(Fraction(region["enabled_ms"]),
                                        Fraction(region["region_ms"]))
            self.assertGreater(Fraction(region["setup_ms"]), Fraction(region["region_ms"]),
                               "the slow setup was outside the capture")
            self.assertGreater(item["samples"], 0)
            self.assertEqual(item["losses"], {
                "state": "decoded", "lost_event_records": 0, "lost_events": 0,
                "lost_sample_records": 0, "potentially_lost_samples": 0, "detail": ""})
            self.assertEqual(item["region"]["process"], "fresh_exec")
            self.assertEqual(item["symbols"]["state"], "retained")
            self.assertEqual(item["unknown_frame_ratio"],
                             "2/5" if item["unwind"] == "frame_pointer" else "0")
        self.assertEqual(sorted(item["kind"] for item in doc["renders"]),
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        folded = read(os.path.join(output, "flamegraphs", SMALL, "baseline.folded")).decode()
        self.assertIn("region_work", folded)
        self.assertNotIn("setup_work", folded, "a setup-only stack is never attributed")
        self.assertNotIn("[unknown]", folded, "the DWARF capture was rendered")
        page = read(os.path.join(output, diagnostics.PAGE_FILE)).decode()
        self.assertIn('<img src="flamegraphs/%s/difference_reverse.svg"' % SMALL, page)
        self.assertNotIn("<script>alert", page)
        tools_by_name = dict((tool["name"], tool) for tool in doc["tools"])
        self.assertEqual(tools_by_name["perf"]["reported_version"], "perf version 6.1.fake")
        self.assertEqual(tools_by_name["inferno_flamegraph"]["version_provenance"],
                         "declared_pin_only")
        manifest = contracts.read_file(os.path.join(output, "manifest.json"), "manifest")
        paths = [entry["path"] for entry in manifest["entries"]]
        self.assertIn(diagnostics.PROFILES_FILE, paths)
        self.assertTrue(any(path.endswith(".data") for path in paths), "raw captures kept")

    def test_a_seeded_case_copies_its_sides_written_seed_fresh_for_every_repetition(self):
        # The side's own writer writes the seed once (the stand-in writes probe-work/), and each
        # repetition reads a new private copy: the stand-in exits 5 on a copy it already used.
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"))
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        cold = "state_projection__log_events_10000_projection_application_cold"
        registry = json.loads(json.dumps(self.registry))
        registry["capture"].update(max_repetitions=3, minimum_samples=20)
        doc, _ = self.profile("linux_gnu_cpu_profile", [cold], tools,
                              {"setup_seconds": 0.05, "region_seconds": 0.1},
                              duration_ms=20000, registry=registry)
        for item in doc["captures"]:
            self.assertIsNone(item["omission"], item["omission"])
            self.assertEqual(item["region"]["repetitions"], 3)
            self.assertEqual(item["region"]["stopped"], "repetition_cap")

    def test_perf_ends_a_capture_at_the_byte_cap_and_the_partial_capture_is_kept(self):
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"),
                          size_limit_samples=30)
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        doc, _ = self.profile("linux_gnu_cpu_profile", [SMALL], tools,
                              {"setup_seconds": 0.05, "region_seconds": 0.2})
        self.assertEqual(doc["renders"], [])
        for item in doc["captures"]:
            self.assertEqual(item["omission"]["reason"], "profile_bytes_exhausted")
            self.assertGreaterEqual(item["samples"], 30)
            self.assertTrue(any(artifact["path"].endswith("capture.data")
                                for artifact in item["artifacts"]))

    def test_a_failed_seed_fails_every_case_reading_it_and_is_never_rewritten(self):
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"))
        tools = diagnostics.configured_tools(perf=perf)
        cold = "state_projection__log_events_10000_projection_application_cold"
        warm = "state_projection__log_events_10000_projection_prewarmed"
        doc, _ = self.profile("linux_gnu_cpu_profile", [cold, warm], tools,
                              {"refuse": ["state_projection_seed_v1"]})
        reasons = sorted((item["instance_id"], item["side"], item["omission"]["reason"])
                         for item in doc["captures"])
        self.assertEqual(reasons, [(cold, "baseline", "seed_failed"),
                                   (cold, "candidate", "seed_failed"),
                                   (warm, "baseline", "seed_failed"),
                                   (warm, "candidate", "seed_failed")])

    def test_too_few_samples_keep_the_capture_and_render_nothing_from_it(self):
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"))
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        registry = json.loads(json.dumps(self.registry))
        registry["capture"]["minimum_samples"] = 100000
        doc, output = self.profile("linux_gnu_cpu_profile", [SMALL], tools,
                                   {"setup_seconds": 0.05, "region_seconds": 0.05},
                                   registry=registry)
        self.assertEqual(doc["renders"], [])
        for item in doc["captures"]:
            self.assertEqual(item["omission"]["reason"], "too_few_samples")
            self.assertGreater(item["samples"], 0)
            self.assertTrue(any(artifact["path"].endswith("capture.data")
                                for artifact in item["artifacts"]), "the raw capture is kept")
        self.assertEqual(sorted(item["reason"] for item in doc["omissions"]),
                         ["no_capture", "no_capture"])

    def test_without_configured_tools_every_capture_is_an_omission_with_its_reproduction(self):
        doc, output = self.profile("linux_gnu_cpu_profile", [SMALL, RAW], {},
                                   {"setup_seconds": 0})
        self.assertEqual(doc["captures"], [])
        self.assertEqual(doc["renders"], [])
        reasons = [(item["kind"], item["instance_id"], item["reason"])
                   for item in doc["omissions"]]
        self.assertEqual(reasons, [("cpu_profile", SMALL, "perf_unavailable"),
                                   ("cpu_profile", RAW, "perf_unavailable")])
        for item in doc["omissions"]:
            self.assertEqual(item["reproduction"][:3], ["python3", "scripts/performance/cli.py",
                                                         "profile"])
        states = dict((tool["name"], (tool["state"], tool["detail"])) for tool in doc["tools"])
        self.assertEqual(states["perf"], ("unavailable", "not configured on this executor"))

    def test_a_trace_child_run_measures_overhead_against_the_verdict_build(self):
        trace = {"schema": "af.perf-trace/1", "pid": 9, "clock": "monotonic_ns",
                 "epoch_unix_ns": "1", "capacity": 65536, "dropped": 0, "open": 0,
                 "spans": [{"id": 1, "parent": None, "phase": "probe_region", "tid": 1,
                            "start_ns": 10, "end_ns": 5000},
                           {"id": 2, "parent": 1, "phase": "canonicalize", "tid": 1,
                            "start_ns": 20, "end_ns": 900}]}
        plain = fakes.probe(self.path("plain"), self.golden, wall_ms={"plain": "10"})
        parent_probes = dict((side, (plain, sha256_bytes(read(plain))))
                             for side in diagcontracts.SIDES)
        registry = json.loads(json.dumps(self.registry))
        registry["trace"]["overhead_pairs"] = 12
        doc, output = self.profile("linux_gnu_trace", [SMALL], {},
                                   {"trace": trace, "wall_ms": {"trace": "10.5"}},
                                   parent_probes=parent_probes, registry=registry)
        self.assertEqual(len(doc["traces"]), 2)
        for entry in doc["traces"]:
            self.assertIsNone(entry["omission"], entry["omission"])
            overhead = entry["overhead"]
            self.assertEqual(overhead["pairs"], 12)
            self.assertEqual(overhead["median_overhead"], "1/20")
            self.assertEqual(overhead["label"], "perturbing", "5% is above the 2% target")
            self.assertNotEqual(overhead["off_recipe_digest"], overhead["on_recipe_digest"])
            self.assertEqual(entry["spans"], 2)
        exported = json.loads(read(os.path.join(output, "traces", SMALL, "baseline",
                                                "perfetto.json")).decode("utf-8"))
        self.assertEqual(len([event for event in exported["traceEvents"]
                              if event["ph"] == "X"]), 2)
        page = read(os.path.join(output, diagnostics.PAGE_FILE)).decode()
        self.assertIn("covered ns (union)", page)

    def test_a_trace_the_candidate_made_a_symlink_or_a_fifo_is_never_read(self):
        # The trace file is the candidate's: a symlink to a file outside its namespace, or a
        # FIFO, is refused before the host reads, hashes or copies anything; an oracle that
        # passed does not make the file safe.
        secret = self.path("outside-secret")
        with open(secret, "wb") as stream:
            stream.write(b"not the candidate's to export")
        for config, needle in (({"trace_symlink": secret}, "symlink"),
                               ({"trace_fifo": True}, "FIFO")):
            with self.subTest(config=sorted(config)):
                doc, output = self.profile("linux_gnu_trace", [SMALL], {}, config)
                for entry in doc["traces"]:
                    self.assertEqual(entry["omission"]["reason"], "trace_unadmitted")
                    self.assertIn(needle, entry["omission"]["detail"])
                    self.assertEqual(entry["artifacts"], [])
                for directory, _, files in os.walk(output):
                    for name in files:
                        self.assertNotIn(b"not the candidate's", read(os.path.join(directory,
                                                                                   name)))
                self.assertEqual(read(secret), b"not the candidate's to export")
                shutil.rmtree(output)
                shutil.rmtree(self.path("probe-bin"))
                shutil.rmtree(self.path("supervisor"))

    TRACE = {"schema": "af.perf-trace/1", "pid": 9, "clock": "monotonic_ns",
             "epoch_unix_ns": "1", "capacity": 65536, "dropped": 0, "open": 0,
             "spans": [{"id": 1, "parent": None, "phase": "probe_region", "tid": 1,
                        "start_ns": 10, "end_ns": 5000}]}

    def test_a_truncated_probe_stdout_never_passes_on_the_prefix_that_parses(self):
        # The probe writes one valid JSON line padded to the 64-KiB record, then 10 bytes
        # more: the retained prefix alone would pass the golden oracle, but the supervisor
        # counted a longer stream, so the probe fails before any oracle (root-3, ADR-0176).
        for total, reason in ((64 * 1024, None), (64 * 1024 + 10, "probe_failed")):
            with self.subTest(total=total):
                doc, output = self.profile("linux_gnu_trace", [SMALL], {},
                                           {"trace": self.TRACE, "stdout_total": total})
                for entry in doc["traces"]:
                    if reason is None:
                        self.assertIsNone(entry["omission"], "the positive control passes")
                    else:
                        self.assertEqual(entry["omission"]["reason"], reason)
                        self.assertIn("over its 65536-byte output cap",
                                      entry["omission"]["detail"])
                        self.assertEqual(entry["artifacts"], [])
                shutil.rmtree(output)
                shutil.rmtree(self.path("probe-bin"))
                shutil.rmtree(self.path("supervisor"))

    def test_a_seed_is_admitted_before_it_is_copied_and_a_symlink_in_it_refuses(self):
        # The writer leaves a symlink to an outside file beside a message that passes its
        # oracle: the seed is refused before any copy, read or hash, the outside file is
        # never copied, and the run's own scratch is still removed (root-5, ADR-0176).
        secret = self.path("outside-secret")
        with open(secret, "wb") as stream:
            stream.write(b"never the seed's to copy")
        cold = "state_projection__log_events_10000_projection_application_cold"
        doc, output = self.profile("linux_gnu_trace", [cold], {},
                                   {"trace": self.TRACE, "seed_symlink": secret})
        for entry in doc["traces"]:
            self.assertEqual(entry["omission"]["reason"], "seed_unadmitted")
            self.assertIn("symlink", entry["omission"]["detail"])
        for directory, _, files in os.walk(output):
            for name in files:
                self.assertNotIn(b"never the seed's", read(os.path.join(directory, name)))
        self.assertEqual(read(secret), b"never the seed's to copy")

    def test_counters_bracket_callgrind_and_label_dhat_whole_process(self):
        tools = diagnostics.configured_tools(valgrind_bin=fakes.valgrind(self.path("valgrind")))
        doc, _ = self.profile("linux_gnu_counters", [SMALL, VALIDATE], tools,
                              {"setup_seconds": 0.05, "region_seconds": 0.05})
        by_key = dict(((item["instance_id"], item["side"], item["metric"]), item)
                      for item in doc["counters"])
        callgrind = by_key[(SMALL, "baseline", "instructions")]
        self.assertIsNone(callgrind["omission"], callgrind["omission"])
        self.assertEqual(callgrind["scope"], "region")
        self.assertGreater(callgrind["value"], 0, "instrumentation was switched on")
        dhat = by_key[(SMALL, "baseline", "allocated_bytes")]
        self.assertEqual((dhat["scope"], dhat["value"]), ("whole_process", 12345))
        self.assertEqual(by_key[(SMALL, "baseline", "allocations")]["value"], 67)
        for item in doc["counters"]:
            self.assertEqual(item["interpretation"], "explanatory_proxy_not_wall_time")


DISCOVERY = "cli_discovery__command_version"
TASK_SHOW = "cli_state_read__command_task_show_state_cli_state_10000"
TUI_EMPTY = "tui_open__registry_empty"


class CliCases(ChildRunBase):
    """CLI cases diagnosed through each side's own AF process (ADR-0176): a private HOME, XDG
    and TMPDIR per execution, the command's declared setup, and its oracle over the whole
    output the supervisor observed. The AF binaries are stand-ins."""

    def registry_with(self, **trace):
        registry = json.loads(json.dumps(self.registry))
        registry["capture"]["minimum_samples"] = 20
        registry["trace"].update(trace)
        return registry

    def test_a_discovery_trace_runs_each_sides_af_privately_and_judges_its_oracle(self):
        doc, output = self.profile("linux_gnu_trace", [DISCOVERY], {}, {},
                                   registry=self.registry_with(overhead_pairs=12), af_config={},
                                   parent_af_config={})
        self.assertEqual(len(doc["traces"]), 2)
        for entry in doc["traces"]:
            self.assertIsNone(entry["omission"], entry["omission"])
            self.assertEqual((entry["subject"], entry["region_phase"], entry["region_spans"],
                              entry["coverage"]), ("cli_process", "cli_startup", 1, "complete"))
            self.assertEqual(entry["overhead"]["pairs"], 12)
            self.assertEqual(entry["overhead"]["off_binary_sha256"],
                             sha256_bytes(read(self.path("parent-af-bin", "af"))))
        runs = read(self.path("af-bin", "af-executions")).decode().splitlines()
        self.assertEqual(runs, ['["--version"]'] * 24, "12 traced executions per side")
        page = read(os.path.join(output, diagnostics.PAGE_FILE)).decode()
        self.assertIn("cli_process, region cli_startup (1 spans): complete", page)

    def test_a_cli_command_that_fails_its_oracle_is_an_omission_never_a_trace(self):
        doc, _ = self.profile("linux_gnu_trace", [DISCOVERY], {}, {}, af_config={
            "version": "9.9.9"})
        for entry in doc["traces"]:
            self.assertEqual(entry["omission"]["reason"], "oracle_failed")
            self.assertIn("af 0.12.0-rc.3", entry["omission"]["detail"])
            self.assertEqual((entry["artifacts"], entry["coverage"]), ([], None))

    def test_a_state_read_needs_its_contained_environment_here(self):
        doc, _ = self.profile("linux_gnu_trace", [TASK_SHOW], {}, {}, af_config={})
        self.assertEqual(doc["traces"], [])
        self.assertEqual([(item["kind"], item["instance_id"], item["reason"])
                          for item in doc["omissions"]],
                         [("trace", TASK_SHOW, "requires_linux_containment")])
        self.assertIsNotNone(doc["omissions"][0]["reproduction"])

    def test_a_cli_cpu_profile_records_whole_fresh_af_processes(self):
        perf = fakes.perf(self.path("perf"))
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        doc, output = self.profile("linux_gnu_cpu_profile", [DISCOVERY], tools, {},
                                   registry=self.registry_with(), af_config={
                                       "busy_seconds": 0.08}, duration_ms=1500)
        self.assertEqual(len(doc["captures"]), 2)
        for item in doc["captures"]:
            self.assertIsNone(item["omission"], item["omission"])
            self.assertEqual((item["subject"], item["region"]["mechanism"],
                              item["region"]["process"]),
                             ("cli_process", "whole_process", "fresh_process"))
            self.assertGreaterEqual(item["region"]["repetitions"], 2)
            self.assertEqual(item["region"]["stopped"], "duration_reached")
            self.assertGreater(item["samples"], 0)
            names = [artifact["path"].rsplit("/", 1)[1] for artifact in item["artifacts"]]
            self.assertIn("rep-0001.data", names)
            self.assertIn("capture.script", names)
        self.assertEqual(sorted(item["kind"] for item in doc["renders"]),
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        folded = read(os.path.join(output, "flamegraphs", DISCOVERY, "baseline.folded"))
        self.assertIn(b"af_work", folded)

    def test_a_trace_whose_setup_filled_its_recorder_is_never_labelled_complete(self):
        # The probe's setup took every span: its region was dropped, and the trace says so.
        trace = {"schema": "af.perf-trace/1", "pid": 9, "clock": "monotonic_ns",
                 "epoch_unix_ns": "1", "capacity": 2, "dropped": 5, "open": 0,
                 "spans": [{"id": 1, "parent": None, "phase": "probe_setup", "tid": 1,
                            "start_ns": 10, "end_ns": 50},
                           {"id": 2, "parent": None, "phase": "canonicalize", "tid": 1,
                            "start_ns": 60, "end_ns": 90}]}
        doc, output = self.profile("linux_gnu_trace", [SMALL], {}, {"trace": trace})
        for entry in doc["traces"]:
            self.assertIsNone(entry["omission"], entry["omission"])
            self.assertEqual((entry["region_spans"], entry["coverage"]), (0, "region_missing"))
        page = read(os.path.join(output, diagnostics.PAGE_FILE)).decode()
        self.assertIn("region missing: no probe_region span was kept", page)


class TraceRoom(ChildRunBase):
    """A raw trace is evidence while it is written (root p5-root-cli-trace-budget): staged below
    the evidence, counted by the watch and settled before it is admitted. 1 MiB in all, 512 KiB
    of evidence room, 522,240 bytes of it already held (the frozen plan included): 2,048 bytes
    are left."""

    PRIOR = 512 * 1024 - 2048

    def registry_room(self):
        registry = json.loads(json.dumps(self.registry))
        registry["capture"]["minimum_samples"] = 20
        registry["limits"]["profile_bytes"] = 1024 * 1024
        return registry

    def staged(self, output):
        stage = os.path.join(output, diagnostics.TRACE_STAGE)
        found = []
        for directory, _, files in os.walk(stage):
            found += [os.path.join(directory, name) for name in files]
        return found

    def test_a_cli_trace_within_the_room_left_is_admitted_and_unstaged(self):
        doc, output = self.profile("linux_gnu_trace", [DISCOVERY], {}, {},
                                   registry=self.registry_room(), af_config={},
                                   prior_bytes=self.PRIOR)
        baseline = doc["traces"][0]
        self.assertIsNone(baseline["omission"], baseline["omission"])
        kept = read(os.path.join(output, "traces", DISCOVERY, "baseline", "trace.json"))
        self.assertLess(len(kept), 2048)
        self.assertEqual(self.staged(output), [], "an admitted trace is unstaged")
        self.assertEqual(doc["budgets"]["bytes"]["evidence_overage"], 0)

    def over_room(self, af_config):
        """A profile of DISCOVERY whose AF exports an 8,192-byte trace into 2,048 bytes of room,
        and every supervised Result of it, kept for what its supervisor said."""
        actual, results = toolrun.run, []

        def recording(*args, **kwargs):
            result = actual(*args, **kwargs)
            results.append(result)
            return result

        with mock.patch.object(diagnostics.toolrun, "run", recording):
            doc, output = self.profile("linux_gnu_trace", [DISCOVERY], {}, {},
                                       registry=self.registry_room(),
                                       af_config=dict(af_config, trace_bytes=8192),
                                       prior_bytes=self.PRIOR)
        return doc, output, results

    def test_a_cli_trace_over_the_room_left_is_refused_kept_and_counted(self):
        doc, output, _ = self.over_room({})
        self.kept_and_counted(doc, output)

    def kept_and_counted(self, doc, output):
        """The trace over the room: trace_over_cap with the bytes the evidence holds, the trace
        kept, counted and in the manifest, and the other side never launched."""
        baseline, candidate = doc["traces"]
        self.assertEqual(baseline["omission"]["reason"], "trace_over_cap")
        self.assertIn("the exported trace left the evidence holding %d bytes"
                      % (self.PRIOR + 8192), baseline["omission"]["detail"])
        self.assertEqual(baseline["artifacts"], [])
        # Never deleted to report nothing: the 8,192 bytes stay in the evidence, counted.
        [staged] = self.staged(output)
        self.assertEqual(os.path.getsize(staged), 8192)
        used = doc["budgets"]["bytes"]
        self.assertGreaterEqual(used["evidence_used"], self.PRIOR + 8192)
        self.assertEqual(used["evidence_overage"],
                         used["evidence_used"] - (used["limit"] - used["administrative_reserve"]))
        self.assertGreater(used["evidence_overage"], 0)
        manifest = json.loads(read(os.path.join(output, "manifest.json")).decode("utf-8"))
        self.assertIn(os.path.relpath(staged, output),
                      [entry["path"] for entry in manifest["entries"]])
        # Nothing more starts once the room is spent.
        self.assertEqual(candidate["omission"]["reason"], "profile_bytes_exhausted")
        runs = read(self.path("af-bin", "af-executions")).decode().splitlines()
        self.assertEqual(len(runs), 1)

    def test_a_cli_trace_over_the_room_left_while_its_command_is_live_is_trace_over_cap(self):
        # Root p6a-integrity-root-trace-cap-reason: the writer exports its 8,192 bytes and stays
        # live for a bounded 3 s before its finished marker. The watch cancels it through its
        # supervisor; the reason is the room's, never the cancellation's cli_failed.
        doc, output, results = self.over_room({"linger_seconds": 3})
        # The cancellation actually happened, its end confirmed: the marker was never written.
        [cancelled] = results
        self.assertIsNotNone(cancelled.exceeded)
        self.assertTrue(cancelled.cancelled)
        self.assertFalse(cancelled.uncertain)
        self.assertIsNotNone(cancelled.supervision_failure(observed_stream=True))
        self.assertFalse(os.path.lexists(self.path("af-bin", "af-lingered")))
        self.kept_and_counted(doc, output)

    def test_a_completed_oversized_trace_gives_the_same_reason_and_account(self):
        # The control: the watch first looks only after the writer exited, so the same 8,192
        # bytes reach the completed path, neither cancelled nor exceeded, and give the same
        # reason and account as the live cancellation.
        with mock.patch.object(toolrun, "WATCH_SECONDS", 120):
            doc, output, results = self.over_room({})
        [completed] = results
        self.assertIsNone(completed.exceeded)
        self.assertFalse(completed.cancelled)
        self.assertIsNone(completed.failure())
        self.kept_and_counted(doc, output)

    def test_a_cli_failure_within_the_room_is_still_cli_failed(self):
        # A supervision failure whose trace fits the room keeps its own reason: only bytes over
        # the room become trace_over_cap.
        actual = toolrun.run

        def timed_out(*args, **kwargs):
            result = actual(*args, **kwargs)
            result.timed_out = True
            return result

        with mock.patch.object(diagnostics.toolrun, "run", timed_out):
            doc, output = self.profile("linux_gnu_trace", [DISCOVERY], {}, {},
                                       registry=self.registry_room(), af_config={},
                                       prior_bytes=self.PRIOR)
        baseline = doc["traces"][0]
        self.assertEqual(baseline["omission"]["reason"], "cli_failed")
        self.assertIn("did not finish within its bound", baseline["omission"]["detail"])
        self.assertEqual(doc["budgets"]["bytes"]["evidence_overage"], 0)
        self.assertEqual(self.staged(output), [], "a trace within the room is unstaged")

    def test_a_native_trace_is_bounded_by_the_same_room_and_never_counted_as_zero(self):
        doc, output = self.profile("linux_gnu_trace", [SMALL], {},
                                   {"trace": ChildRun.TRACE, "trace_bytes": 8192},
                                   registry=self.registry_room(), prior_bytes=self.PRIOR)
        baseline = doc["traces"][0]
        self.assertIsNotNone(baseline["omission"])
        self.assertEqual(baseline["artifacts"], [])
        # The file limit stopped the native writer at the room: no overage, nothing staged.
        self.assertEqual(doc["budgets"]["bytes"]["evidence_overage"], 0)
        self.assertEqual(self.staged(output), [])
        doc, output = None, None
        shutil.rmtree(self.path("child"))
        shutil.rmtree(self.path("probe-bin"))
        shutil.rmtree(self.path("supervisor"))
        doc, output = self.profile("linux_gnu_trace", [SMALL], {}, {"trace": ChildRun.TRACE},
                                   registry=self.registry_room(), prior_bytes=self.PRIOR)
        for entry in doc["traces"]:
            self.assertIsNone(entry["omission"], entry["omission"])
        self.assertEqual(self.staged(output), [])


class UncertainCleanup(ChildRunBase):
    """An execution whose end is not confirmed keeps its scratch (root p5-root-uncertain-cleanup):
    uncertainty is checked before any removal, the data a live process could reach stays, the
    reason names the path, and the run launches nothing more."""

    def bare(self):
        profile = diagnostics.Profile.__new__(diagnostics.Profile)
        profile.work = self.path("work")
        profile.retained = []
        return profile

    def test_internal_supervisor_exceptions_retain_decoder_and_renderer_work(self):
        from tests.supervision_faults import fault
        for mode in ("trusted_local", "linux_container"):
            for target in ("decoder", "renderer"):
                for failure, callback in ((OSError, False), (ContractError, False),
                                          (RuntimeError, True), (KeyboardInterrupt, True)):
                    with self.subTest(mode=mode, target=target, failure=failure, callback=callback):
                        perf = fakes.perf(self.path("perf"),
                                          phase_file=self.path("probe-bin", "phase"))
                        tools = diagnostics.configured_tools(perf=perf,
                            inferno_bin=fakes.inferno(self.path("inferno")))
                        place = None
                        if mode == "linux_container":
                            root = self.path("scratch-fs")
                            os.mkdir(root, 0o700)
                            place = dict(scratch_root=root, docker=fakes.docker(self.path("docker")))
                        def match(request):
                            argv = request["argv"]
                            return ((target == "decoder" and "script" in argv)
                                    or (target == "renderer" and any(
                                        os.path.basename(arg) == "inferno-diff-folded" for arg in argv)
                                        and "--version" not in argv))
                        actual_run, actual_remove = diagnostics.Profile.run, diagnostics.admission.remove_tree
                        removals = []
                        def run(profile):
                            os.makedirs(os.path.join(profile.output, diagnostics.WORK_DIR), exist_ok=True)
                            return actual_run(profile)
                        def remove(path, *args, **kwargs):
                            if state["faults"]: removals.append(path)
                            return actual_remove(path, *args, **kwargs)
                        with fault(match, failure, mode == "linux_container", callback=callback) as state, \
                                mock.patch.object(diagnostics.Profile, "run", run), \
                                mock.patch.object(diagnostics.admission, "remove_tree", remove):
                            doc, output = self.profile("linux_gnu_cpu_profile", [SMALL, VALIDATE],
                                tools, {}, contained=place, work_removed=False)
                        profile = self.profile_object
                        self.assertEqual(len(state["faults"]), 1)
                        self.assertEqual(len(state["popen"]), 1)
                        self.assertTrue(match(state["requests"][-1]), "no later launch")
                        self.assertEqual(removals, [])
                        self.assertTrue(os.path.isdir(profile.work))
                        self.assertTrue(os.path.isdir(os.path.join(output, diagnostics.WORK_DIR)))
                        self.assertTrue(profile.retained)
                        self.assertIn(profile.work, profile.unsafe_staging)
                        if profile.stage: self.assertTrue(os.path.isdir(profile.stage))
                        if target == "decoder":
                            self.assertTrue(all(item["omission"]["reason"] == "cleanup_uncertain"
                                                for item in doc["captures"]))
                            self.assertTrue(all(item["artifacts"] == [] for item in doc["captures"]))
                        else:
                            self.assertTrue(any(item["reason"] == "cleanup_uncertain"
                                                for item in doc["omissions"]))
                        self.assertFalse(os.path.exists(os.path.join(output, "manifest.json")))
                        with self.assertRaises(perfcapture.CaptureError): profile.collect("unsafe")
                        with self.assertRaises(perfcapture.CaptureError):
                            profile.tool_run([sys.executable, "-I", "-c", "pass"], 1, 1, self.tmp)
                        if mode == "linux_container": self.assertTrue(state["removals"])
                        for directory in ("child", "probe-bin", "supervisor", "perf", "inferno",
                                          "scratch-fs", "docker"):
                            path = self.path(directory)
                            if os.path.isdir(path): shutil.rmtree(path)

    def test_only_a_typed_confirmed_removal_releases_a_contained_namespace(self):
        # Issue #388: a failed launch's removal was confirmed by its text, so an OSError whose
        # message ends in "removed" cleared uncertainty and the namespace was removed. Now the
        # removal's typed outcome decides; its wording never does.
        for raised in ("container was not removed", "removal failed"):
            with self.subTest(raised=raised):
                for directory in ("child", "probe-bin", "supervisor", "perf", "scratch-fs",
                                  "docker"):
                    path = self.path(directory)
                    if os.path.isdir(path): shutil.rmtree(path)
                perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"))
                tools = diagnostics.configured_tools(perf=perf)
                root = self.path("scratch-fs")
                os.mkdir(root, 0o700)
                place = dict(scratch_root=root, docker=fakes.docker(self.path("docker")))
                actual_launch, actual_remove = supervise.launch, oracle.Namespace.remove
                lost, launched, removed = [], [], []

                def launch(argv, request, **kwargs):
                    launched.append(request)
                    if not lost and request["cwd"].startswith(os.path.realpath(root)):
                        lost.append(request)
                        raise supervise.SupervisorFailure("lost supervisor reply", uncertain=True)
                    return actual_launch(argv, request, **kwargs)

                def remove_container(docker, name, deadline=None, clock=time.monotonic):
                    raise OSError(raised)

                def remove_namespace(namespace):
                    removed.append(namespace.path)
                    return actual_remove(namespace)

                actual_run = diagnostics.Profile.run

                def run(profile):
                    os.makedirs(os.path.join(profile.output, diagnostics.WORK_DIR), exist_ok=True)
                    return actual_run(profile)

                with mock.patch.object(supervise, "launch", launch), \
                        mock.patch.object(supervise, "remove_container", remove_container), \
                        mock.patch.object(oracle.Namespace, "remove", remove_namespace), \
                        mock.patch.object(diagnostics.Profile, "run", run):
                    doc, output = self.profile("linux_gnu_cpu_profile", [SMALL], tools, {},
                                               contained=place, work_removed=False)
                profile = self.profile_object
                self.assertEqual(len(lost), 1)
                self.assertIs(launched[-1], lost[0], "nothing launches after the lost reply")
                self.assertEqual(removed, [], "the namespace is kept, never removed")
                self.assertEqual(profile.retained, [lost[0]["cwd"]])
                self.assertTrue(os.path.isdir(lost[0]["cwd"]))
                self.assertTrue(any(item["omission"]["reason"] == "cleanup_uncertain"
                                    for item in doc["captures"]))
                self.assertFalse(os.path.exists(os.path.join(output, "manifest.json")))

    def test_proven_decoder_prelaunch_refusal_allows_next_side_and_final_cleanup(self):
        from tests.supervision_faults import fault
        for mode in ("trusted_local", "linux_container"):
            with self.subTest(mode=mode):
                perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"))
                tools = diagnostics.configured_tools(perf=perf)
                place = None
                if mode == "linux_container":
                    root = self.path("scratch-fs")
                    os.mkdir(root, 0o700)
                    place = dict(scratch_root=root, docker=fakes.docker(self.path("docker")))
                with fault(lambda request: "script" in request["argv"], prelaunch=True) as state:
                    doc, output = self.profile("linux_gnu_cpu_profile", [SMALL], tools, {},
                                               contained=place)
                self.assertEqual(len(state["faults"]), 2, "the candidate side remains usable")
                self.assertEqual(state["popen"], [])
                self.assertEqual(state["removals"], [])
                self.assertEqual(self.profile_object.retained, [])
                self.assertTrue(os.path.exists(os.path.join(output, "manifest.json")))
                self.assertTrue(all(item["omission"]["reason"] != "cleanup_uncertain"
                                    for item in doc["captures"]))
                for directory in ("child", "probe-bin", "supervisor", "perf", "scratch-fs", "docker"):
                    path = self.path(directory)
                    if os.path.isdir(path): shutil.rmtree(path)

    def test_decoder_and_renderer_uncertainty_and_exceptions_retain_all_staging(self):
        # Profile.run -> perf_capture -> analyse -> tool_output -> tool_run, and
        # Profile.run -> render -> Renderer.run -> tool_output -> tool_run.
        # These calls have no Namespace; the recording's Namespace is already released.
        from perflab import budget as deadlines
        for mode in ("trusted_local", "linux_container"):
            for target in ("decoder", "renderer"):
                for failure in (None, OSError, RuntimeError, KeyboardInterrupt,
                                deadlines.Interrupted):
                    with self.subTest(mode=mode, target=target, failure=failure):
                        perf = fakes.perf(self.path("perf"),
                                          phase_file=self.path("probe-bin", "phase"))
                        tools = diagnostics.configured_tools(perf=perf,
                            inferno_bin=fakes.inferno(self.path("inferno")))
                        place = None
                        if mode == "linux_container":
                            root = self.path("scratch-fs")
                            os.mkdir(root, 0o700)
                            place = dict(scratch_root=root, docker=fakes.docker(self.path("docker")))
                        actual = toolrun.run
                        actual_run = diagnostics.Profile.run
                        launched, unsafe, removals = [], [], []
                        real_remove = diagnostics.admission.remove_tree
                        def run_profile(profile):
                            # Materialized authority work is separate from contained work.
                            path = os.path.join(profile.output, diagnostics.WORK_DIR)
                            os.makedirs(path, exist_ok=True)
                            with open(os.path.join(path, "authority-work"), "wb") as stream:
                                stream.write(b"retain all work roots")
                            return actual_run(profile)
                        def remove(path, *args, **kwargs):
                            if unsafe: removals.append(path)
                            return real_remove(path, *args, **kwargs)
                        def tool(*args, **kwargs):
                            result = actual(*args, **kwargs)
                            argv = args[1]
                            launched.append(list(argv))
                            match = ((target == "decoder" and len(argv) > 1 and argv[1] == "script")
                                     or (target == "renderer" and os.path.basename(argv[0])
                                         == "inferno-diff-folded" and "--version" not in argv))
                            if match:
                                self.assertFalse(unsafe, "no second tool may launch")
                                path = kwargs["stdout_path"]
                                self.assertTrue(read(path), "actual tool wrote its output")
                                unsafe.append(path)
                                if failure is not None:
                                    raise failure("after possible tool launch")
                                result.uncertain = True
                            return result
                        with mock.patch.object(toolrun, "run", tool), \
                                mock.patch.object(diagnostics.Profile, "run", run_profile), \
                                mock.patch.object(diagnostics.admission, "remove_tree", remove):
                            doc, output = self.profile("linux_gnu_cpu_profile", [SMALL, VALIDATE],
                                tools, {}, contained=place, work_removed=False)
                        profile = self.profile_object
                        self.assertEqual(len(unsafe), 1)
                        self.assertTrue(read(unsafe[0]), "unsafe output must remain untouched")
                        self.assertEqual(removals, [], "nothing is deleted after uncertainty")
                        self.assertTrue(os.path.isdir(profile.work))
                        if profile.stage:
                            self.assertTrue(os.path.isdir(profile.stage))
                            relative = os.path.relpath(unsafe[0], profile.stage)
                            self.assertFalse(os.path.exists(os.path.join(output, relative)),
                                             "unsafe output was collected into evidence")
                        self.assertIn(os.path.dirname(unsafe[0]), profile.unsafe_staging)
                        self.assertEqual(profile.retained, [os.path.dirname(unsafe[0])])
                        if target == "decoder":
                            self.assertEqual(launched[-1][1], "script")
                            self.assertTrue(all(item["omission"]["reason"] == "cleanup_uncertain"
                                                for item in doc["captures"]))
                            self.assertTrue(all(item["artifacts"] == [] for item in doc["captures"]))
                        else:
                            self.assertEqual(os.path.basename(launched[-1][0]), "inferno-diff-folded")
                            self.assertTrue(any(item["reason"] == "cleanup_uncertain"
                                                for item in doc["omissions"]))
                        self.assertEqual(doc["execution"]["retained_scratch"], 1)
                        self.assertFalse(os.path.exists(os.path.join(output, "manifest.json")),
                                         "uncertain staging cannot become an admitted bundle")
                        with self.assertRaises(perfcapture.CaptureError):
                            profile.tool_run([sys.executable, "-I", "-c", "pass"], 1, 1, self.tmp)
                        with self.assertRaises(perfcapture.CaptureError):
                            profile.collect("unsafe")
                        # Only test-owned recovery after all product retention assertions.
                        for directory in ("child", "probe-bin", "supervisor", "perf", "inferno",
                                          "scratch-fs", "docker"):
                            path = self.path(directory)
                            if os.path.isdir(path): shutil.rmtree(path)

    def test_an_uncertain_release_keeps_the_namespace_and_its_data(self):
        profile = self.bare()
        namespace = profile.namespace()
        with open(os.path.join(namespace.path, "live-data"), "w") as stream:
            stream.write("still written by a live process")
        with self.assertRaises(perfcapture.CaptureError) as caught:
            profile.release(namespace, uncertain=True)
        self.assertEqual(caught.exception.reason, "cleanup_uncertain")
        self.assertIn(namespace.path, caught.exception.detail)
        self.assertEqual(profile.retained, [namespace.path])
        self.assertEqual(read(os.path.join(namespace.path, "live-data")),
                         b"still written by a live process")
        # Nothing more of the run is launched or given a namespace.
        with self.assertRaises(perfcapture.CaptureError) as caught:
            profile.namespace()
        self.assertEqual(caught.exception.reason, "cleanup_uncertain")
        self.assertIn("kept for recovery", caught.exception.detail)
        self.assertEqual(os.listdir(profile.work), [namespace.name])

    def test_a_confirmed_release_removes_the_namespace(self):
        profile = self.bare()
        namespace = profile.namespace()
        with open(os.path.join(namespace.path, "data"), "w") as stream:
            stream.write("done")
        profile.release(namespace, uncertain=False)
        self.assertFalse(os.path.lexists(namespace.path))
        self.assertEqual(profile.retained, [])
        profile.release(profile.namespace())
        self.assertEqual(os.listdir(profile.work), [])

    def test_an_unconfirmed_execution_end_is_propagated_and_its_scratch_kept(self):
        # The supervisor's reply leaves the probe's end unconfirmed after it wrote below its
        # namespace: the trace is an omission for that reason, the namespace and its data are
        # kept, the other side never launches, and the run's work is not removed.
        actual = toolrun.run
        launched = []

        def unconfirmed(*args, **kwargs):
            result = actual(*args, **kwargs)
            with open(os.path.join(kwargs["cwd"], "live-data"), "w") as stream:
                stream.write("reachable")
            launched.append(kwargs["cwd"])
            result.uncertain = True
            return result

        with mock.patch.object(diagnostics.toolrun, "run", unconfirmed):
            doc, output = self.profile("linux_gnu_trace", [SMALL], {},
                                       {"trace": ChildRun.TRACE}, work_removed=False)
        baseline, candidate = doc["traces"]
        self.assertEqual(baseline["omission"]["reason"], "cleanup_uncertain")
        self.assertEqual(candidate["omission"]["reason"], "cleanup_uncertain")
        self.assertIn("was not started", candidate["omission"]["detail"])
        self.assertEqual(len(launched), 1, "nothing launched after the unconfirmed end")
        [kept] = launched
        self.assertIn(kept, baseline["omission"]["detail"])
        self.assertEqual(read(os.path.join(kept, "live-data")), b"reachable")
        self.assertEqual(doc["execution"]["retained_scratch"], 1)
        self.assertEqual(self.profile_object.retained, [kept])


class BrowserDiagnostic(ChildRunBase):
    """The browser case under a diagnostic (ADR-0260): its PTY driver owns the browser's
    session outside the supervisor's group, so a cleanup the driver could not confirm is the
    execution's, whatever the supervisor confirmed of its own group."""

    PARENT_CASES = ChildRunBase.PARENT_CASES + ["tui_open"]

    def driver_ended(self, termination, held, **fields):
        """A diagnostic of the browser case whose driver execution the supervisor reports as
        `termination` with `held` on stderr, in place of running it; every other execution runs
        for real. (profiles document, the one driver's cwd)."""
        actual = toolrun.run
        launched = []

        def ended(boss, argv, *args, **kwargs):
            if not any(arg.endswith("/tui-open.py") for arg in argv):
                return actual(boss, argv, *args, **kwargs)
            with open(os.path.join(kwargs["cwd"], "live-data"), "w") as stream:
                stream.write("reachable")
            launched.append(kwargs["cwd"])
            reply = {"termination": termination, "timing": {"child_wall_ns": 5000000},
                     "stdout": {"retained": 0, "total": 0, "truncated": 0},
                     "stdout_pattern_counts": {}, "stdout_json": None}
            return toolrun.Result(argv, stderr=held, stderr_bytes=len(held), elapsed_ms=5,
                                  supervised=supervise.Supervised(reply, b"", held, 5000000,
                                                                  None), **fields)

        with mock.patch.object(diagnostics.toolrun, "run", ended):
            # The browser case is a command of the 0.13 adapter.
            doc, _ = self.profile("linux_gnu_trace", [TUI_EMPTY], {}, {}, af_config={},
                                  work_removed=False, cli_version="0.13.0-rc.1")
        self.assertEqual(len(launched), 1, "nothing launched after the unconfirmed end")
        return doc, launched[0]

    def test_a_driver_that_cannot_confirm_the_sessions_end_keeps_the_scratch(self):
        # The supervisor's reply is clean and the driver exited `DRIVER_UNCONFIRMED_EXIT`
        # after it wrote below its namespace: the execution's cleanup is unconfirmed, the
        # namespace and its data are kept, the other side never launches, and the run's work
        # is not removed.
        held = b"tui-open: cleanup unconfirmed: held\n"
        doc, kept = self.driver_ended(
            {"kind": "exited", "code": clicases.DRIVER_UNCONFIRMED_EXIT, "signal": None,
             "detail": ""}, held, code=clicases.DRIVER_UNCONFIRMED_EXIT)
        baseline, candidate = doc["traces"]
        self.assertEqual(baseline["omission"]["reason"], "cleanup_uncertain")
        self.assertIn("could not confirm that the browser's session ended: tui-open: cleanup "
                      "unconfirmed: held", baseline["omission"]["detail"])
        self.assertEqual(candidate["omission"]["reason"], "cleanup_uncertain")
        self.assertIn("was not started", candidate["omission"]["detail"])
        self.assertIn(kept, baseline["omission"]["detail"])
        self.assertEqual(read(os.path.join(kept, "live-data")), b"reachable")
        self.assertEqual(doc["execution"]["retained_scratch"], 1)
        self.assertEqual(self.profile_object.retained, [kept])

    def test_a_driver_that_did_not_exit_of_its_own_accord_keeps_the_scratch(self):
        # The supervisor ended the driver at its bound, its own group clean: whether the
        # driver settled the browser's session, outside that group, is unknown, so the
        # execution's cleanup is unconfirmed the same way, before anything is judged.
        held = b"tui-open: still settling\n"
        doc, kept = self.driver_ended(
            {"kind": "timeout", "code": None, "signal": None, "detail": ""}, held,
            timed_out=True)
        baseline, candidate = doc["traces"]
        self.assertEqual(baseline["omission"]["reason"], "cleanup_uncertain")
        self.assertIn("did not exit of its own accord (timeout), so its settlement of the "
                      "browser's session is unconfirmed: tui-open: still settling",
                      baseline["omission"]["detail"])
        self.assertEqual(candidate["omission"]["reason"], "cleanup_uncertain")
        self.assertIn("was not started", candidate["omission"]["detail"])
        self.assertIn(kept, baseline["omission"]["detail"])
        self.assertEqual(read(os.path.join(kept, "live-data")), b"reachable")
        self.assertEqual(doc["execution"]["retained_scratch"], 1)
        self.assertEqual(self.profile_object.retained, [kept])


class ContainedRuns(ChildRunBase):
    """The contained Linux mode's driver over a stand-in container runtime (ADR-0176): one
    container per request with the declared limits, writable binds only in the run's own
    directory of the scratch filesystem, outputs staged there and admitted into the evidence,
    and every container's removal confirmed. The stand-in isolates nothing; no test here
    establishes qualified Linux execution."""

    def place(self):
        root = self.path("scratch-fs")
        os.mkdir(root, 0o700)
        return {"scratch_root": os.path.realpath(root),
                "docker": fakes.docker(self.path("docker"))}

    def check_containers(self, place, output):
        calls = fakes.docker_calls(self.path("docker"))
        runs = [call for call in calls if call[0] == "run"]
        self.assertTrue(runs)
        self.assertEqual(len([call for call in calls if call[0] == "rm"]), len(runs),
                         "every container is removed")
        diagnostic = os.path.join(place["scratch_root"], "diagnostic") + "/"
        for call in runs:
            pairs = list(zip(call, call[1:]))
            for flag in (("--network", "none"), ("--cap-drop", "ALL"),
                         ("--security-opt", "no-new-privileges"), ("--cpuset-cpus", "0,1"),
                         ("--cgroupns", "private")):
                self.assertIn(flag, pairs)
            self.assertIn("--read-only", call)
            memory = call[call.index("--memory") + 1]
            self.assertEqual(call[call.index("--memory-swap") + 1], memory, "no swap")
            for index, item in enumerate(call):
                if item != "--mount":
                    continue
                fields = dict(part.split("=", 1) for part in call[index + 1].split(",")
                              if "=" in part)
                if call[index + 1].endswith(",readonly"):
                    continue
                if fields["dst"] == "/perf/scratch":
                    self.assertEqual(fields["src"],
                                     os.path.join(place["scratch_root"], "sample"))
                    continue
                self.assertEqual(fields["src"], fields["dst"])
                self.assertTrue(fields["src"].startswith(diagnostic), fields["src"])
                self.assertFalse(fields["src"].startswith(output))
        self.assertFalse(os.path.lexists(diagnostic), "the run's own directory is removed")
        self.assertEqual(os.listdir(os.path.join(place["scratch_root"], "sample")), [])
        return runs

    def test_a_contained_state_read_writes_its_own_seed_and_judges_its_whole_output(self):
        place = self.place()
        doc, output = self.profile("linux_gnu_trace", [TASK_SHOW], {}, {}, af_config={},
                                   contained=place)
        self.assertEqual((doc["execution"]["mode"], doc["execution"]["containment"],
                          doc["execution"]["scratch_bound"]),
                         ("linux_container", "pid_namespace", "verified_disk_bounded"))
        for entry in doc["traces"]:
            self.assertIsNone(entry["omission"], entry["omission"])
            self.assertEqual((entry["region_phase"], entry["coverage"]),
                             ("cli_command", "complete"))
        runs = self.check_containers(place, output)
        # Each side's seed writer, then one traced command each.
        self.assertEqual(len(runs), 4)
        # The document was far larger than the retained log, judged by its stream rules.
        log = self.parent["limits"]["sample_log_bytes"]
        self.assertGreater(10000 * 150, log)

    def test_a_contained_state_read_whose_stream_breaks_fails_past_the_retained_log(self):
        place = self.place()
        doc, output = self.profile("linux_gnu_trace", [TASK_SHOW], {}, {}, af_config={
            "skip_sequence": 9000}, contained=place)
        for entry in doc["traces"]:
            self.assertEqual(entry["omission"]["reason"], "oracle_failed")
            self.assertIn("9001", entry["omission"]["detail"])
        self.check_containers(place, output)

    def test_a_contained_capture_stages_its_outputs_and_admits_them_into_the_evidence(self):
        place = self.place()
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"))
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        doc, output = self.profile("linux_gnu_cpu_profile", [SMALL], tools,
                                   {"setup_seconds": 0.05, "region_seconds": 0.1},
                                   contained=place)
        for item in doc["captures"]:
            self.assertIsNone(item["omission"], item["omission"])
            self.assertTrue(os.path.isfile(os.path.join(output, item["artifacts"][0]["path"])))
        self.assertEqual(len(doc["renders"]), 4)
        runs = self.check_containers(place, output)
        self.assertTrue(all("--ulimit" in call for call in runs
                            if any("record" == item for item in call)))


class Command(TempDir):
    def test_profile_without_a_build_records_every_capture_unavailable_and_exits_4(self):
        repo, root = lab_repo(self.path("repo"))
        root = repo.commit("diagnostics", {diagnostics.REGISTRY_PATH: read(REGISTRY)})
        git = gitrefs.Git(repo.path)
        parent = planning.plan(git, planning.Request(
            trigger="manual", tier="quick", scope="partial_scope", base=root, head=root,
            run_id="parent_run", seed=5, trusted_host_build=True,
            cases=["canonical_serialize_small"]))
        with open(self.path("parent.json"), "wb") as stream:
            stream.write(contracts.write_bytes(parent, "run"))
        os.mkdir(self.path("lock"), 0o755)
        options = diagnostics.Options(plan=self.path("parent.json"), cases=[SMALL],
                                      recipe="linux_gnu_cpu_profile",
                                      output=self.path("child"), run_id="child_run",
                                      host_os="linux", mode="trusted_local",
                                      lock_directory=self.path("lock"))
        doc_golden = (suite_module.load(SUITE).doc, golden())
        code, path, message = diagnostics.run_profile(git, options,
                                                      authority_factory=lambda plan: doc_golden)
        self.assertEqual(code, 4)
        doc = contracts.read_file(path, "profiles")
        reasons = sorted(item["reason"] for item in doc["omissions"])
        self.assertEqual(reasons, ["no_diagnostic_build", "no_diagnostic_build"])
        child = contracts.read_file(self.path("child", diagnostics.PLAN_FILE), "run")
        self.assertEqual(child["scope"], "diagnostic")
        self.assertEqual(child["diagnostic"]["parent_plan_digest"], parent["plan_digest"])
        self.assertEqual(child["profile"]["recipe_id"], "linux_gnu_cpu_profile")
        self.assertTrue(child["profile"]["recipe"]["frame_pointers"])
        self.assertEqual(doc["plan_digest"], child["plan_digest"])
        # Explicitly trusted local mode, recorded against what the environment declares.
        self.assertEqual((doc["execution"]["mode"], doc["execution"]["declared_containment"],
                          doc["execution"]["matches_environment"]),
                         ("trusted_local", "linux_container", False))
        self.assertEqual(doc["execution"]["machine_lock"],
                         os.path.join(os.path.realpath(self.path("lock")),
                                      "af-perf-machine.lock"))
        bytes_used = doc["budgets"]["bytes"]
        self.assertGreater(bytes_used["evidence_used"], 0)
        self.assertEqual(bytes_used["evidence_overage"], 0)
        # The child never writes beside its parent, and a second run needs a new directory.
        with self.assertRaises(ContractError):
            diagnostics.run_profile(git, options, authority_factory=lambda plan: doc_golden)
        result = subprocess.run([sys.executable, "-I", os.path.join(
            ROOT, "scripts/performance/cli.py"), "profile", "--plan", self.path("parent.json"),
            "--recipe", "linux_gnu_cpu_profile", "--mode", "trusted_local",
            "--output", self.path("other")],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=repo.path)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(b"explicit --case selections or automatic selection", result.stderr)

    def parent(self):
        repo, root = lab_repo(self.path("repo"))
        root = repo.commit("diagnostics", {diagnostics.REGISTRY_PATH: read(REGISTRY)})
        git = gitrefs.Git(repo.path)
        parent = planning.plan(git, planning.Request(
            trigger="manual", tier="quick", scope="partial_scope", base=root, head=root,
            run_id="parent_run", seed=5, trusted_host_build=True,
            cases=["canonical_serialize_small"]))
        with open(self.path("parent.json"), "wb") as stream:
            stream.write(contracts.write_bytes(parent, "run"))
        return git

    def test_unconfirmed_build_retains_standalone_fence_and_recovery_before_any_launch(self):
        git = self.parent()
        os.mkdir(self.path("lock"))
        options = diagnostics.Options(plan=self.path("parent.json"), cases=[SMALL],
            recipe="linux_gnu_cpu_profile", output=self.path("child"),
            host_os="linux", mode="trusted_local", lock_directory=self.path("lock"))
        def build(plan, deadline):
            with self.assertRaises(diagnostics.hostlock.Refused) as busy:
                diagnostics.hostlock.MachineLock(self.path("lock"))
            self.assertEqual(busy.exception.reason, "concurrent_shard")
            image = "sha256:" + "a" * 64
            docker = build_fixtures.fake_docker(self.path("docker"), {image: (image, [])},
                                               lingering="af-perf-probe-")
            diagnostics.builds.BuildImage(docker, image).run_once(["/bin/true"])
        options.builder = build
        with mock.patch.object(diagnostics, "_probes") as probes, \
                mock.patch.object(diagnostics, "probe_tools") as tools, \
                self.assertRaises(diagnostics.Unavailable) as caught:
            diagnostics.run_profile(git, options)
        self.assertFalse(caught.exception.cleanup_confirmed)
        self.assertEqual(caught.exception.reason, "build_container_unconfirmed")
        probes.assert_not_called()
        tools.assert_not_called()
        self.assertTrue(os.path.isfile(self.path("child", diagnostics.PLAN_FILE)))
        self.assertIn(b"af-perf-probe-", read(self.path("child", "cleanup-uncertain.txt")))
        # No diagnostic scratch existed; the owned build alone retains exclusion.
        self.assertFalse(os.path.exists(self.path("child", diagnostics.WORK_DIR)))
        with self.assertRaisesRegex(diagnostics.hostlock.Refused, "unfinished experiment"):
            diagnostics.hostlock.MachineLock(self.path("lock"))

    def test_confirmed_clean_build_failure_remains_a_reportable_omission(self):
        git = self.parent()
        os.mkdir(self.path("lock"))
        def build(plan, deadline):
            raise diagnostics.builds.Unavailable("toolchain_mismatch", "confirmed-clean tool mismatch")
        options = diagnostics.Options(plan=self.path("parent.json"), cases=[SMALL],
            recipe="linux_gnu_cpu_profile", output=self.path("child"), builder=build,
            host_os="linux", mode="trusted_local", lock_directory=self.path("lock"))
        code, path, _ = diagnostics.run_profile(git, options,
            authority_factory=lambda plan: (suite_module.load(SUITE).doc, golden()))
        self.assertEqual(code, 4)
        doc = contracts.read_file(path, "profiles")
        self.assertEqual(doc["execution"]["retained_scratch"], 0)
        self.assertIn("toolchain_mismatch", [o["reason"] for o in doc["omissions"]])
        with diagnostics.hostlock.MachineLock(self.path("lock")):
            pass

    def test_explicit_zero_negative_and_over_limit_bounds_are_refused_before_anything(self):
        git = self.parent()
        base = dict(plan=self.path("parent.json"), cases=[SMALL],
                    recipe="linux_gnu_cpu_profile", output=self.path("child"),
                    run_id="child_run", host_os="linux", mode="trusted_local")
        for key, value in (("frequency_hz", 0), ("frequency_hz", -1), ("frequency_hz", 2000),
                           ("duration_seconds", 0), ("duration_seconds", 31),
                           ("total_minutes", 0), ("total_minutes", 31), ("max_cases", 0)):
            with self.subTest(key=key, value=value):
                options = diagnostics.Options(**dict(base, **{key: value}))
                with self.assertRaises(ContractError) as caught:
                    diagnostics.run_profile(git, options)
                self.assertIn("is not between", str(caught.exception))
                self.assertFalse(os.path.lexists(self.path("child")), "nothing was created")
        # The execution mode is explicit. The contained mode needs its pinned image and a
        # Linux host, and runs only for an environment that declares it; it is never replaced
        # by host execution, and a refusal creates nothing.
        os.mkdir(self.path("lock"), 0o755)
        for changes, reason, needle in (
                ({"mode": None}, None, "--mode"),
                ({"mode": "linux_container", "lock_directory": self.path("lock")}, None,
                 "--image"),
                ({"mode": "linux_container", "host_os": "macos"}, "host_mismatch", "macos")):
            with self.subTest(changes=sorted(changes)):
                with self.assertRaises(ContractError) as caught:
                    diagnostics.run_profile(git, diagnostics.Options(**dict(base, **changes)))
                self.assertEqual(getattr(caught.exception, "reason", None), reason)
                self.assertIn(needle, str(caught.exception))
                self.assertFalse(os.path.lexists(self.path("child")))

    def test_the_contained_mode_takes_the_shards_preflight_and_records_its_container(self):
        git = self.parent()
        os.mkdir(self.path("lock"), 0o755)
        root = os.path.realpath(self.path("scratch-fs"))
        os.mkdir(root, 0o700)
        image = "registry.invalid/af-perf-sample@sha256:" + "0" * 64
        checked = []

        def verified(docker, pinned, scratch_fs, environment, budget, clock):
            # Stands in for runner.contained_host: the runtime query and the enforcement fill.
            checked.append((pinned, scratch_fs, environment["scratch_limit_bytes"]))
            return root, [0, 1], "linux stand-in cgroup2; image %s; cpus 0,1" % pinned

        def unverified(*args):
            raise runner.Unavailable(
                "scratch_unverified", "no verified disk-bounded scratch: stand-in")

        base = dict(plan=self.path("parent.json"), cases=[SMALL],
                    recipe="linux_gnu_cpu_profile", output=self.path("child"),
                    run_id="child_run", host_os="linux", mode="linux_container",
                    lock_directory=self.path("lock"), image=image, scratch_fs=root)
        with self.assertRaises(diagnostics.Unavailable) as caught:
            diagnostics.run_profile(git, diagnostics.Options(contained_host=unverified, **base))
        self.assertEqual(caught.exception.reason, "scratch_unverified")
        self.assertFalse(os.path.lexists(self.path("child")), "nothing was created")
        # A directory an earlier run kept is the operator's, never reused or removed.
        os.mkdir(os.path.join(root, "diagnostic"))
        with self.assertRaises(diagnostics.Unavailable) as caught:
            diagnostics.run_profile(git, diagnostics.Options(contained_host=verified, **base))
        self.assertEqual(caught.exception.reason, "scratch_in_use")
        self.assertFalse(os.path.lexists(self.path("child")))
        os.rmdir(os.path.join(root, "diagnostic"))
        doc_golden = (suite_module.load(SUITE).doc, golden())
        code, path, _ = diagnostics.run_profile(git, diagnostics.Options(
            contained_host=verified, **base), authority_factory=lambda plan: doc_golden)
        self.assertEqual(code, 4)
        doc = contracts.read_file(path, "profiles")
        self.assertEqual(checked[-1], (image, root, 2147483648))
        execution = doc["execution"]
        self.assertEqual((execution["mode"], execution["containment"],
                          execution["matches_environment"], execution["scratch_bound"]),
                         ("linux_container", "pid_namespace", True, "verified_disk_bounded"))
        self.assertEqual(execution["container"]["image"], image)
        self.assertEqual(execution["container"]["cpus"], [0, 1])
        self.assertFalse(os.path.lexists(os.path.join(root, "diagnostic")))
        self.assertIn("--image", doc["omissions"][0]["reproduction"])

    def test_a_child_run_waits_for_no_one_it_refuses_while_another_holds_the_machine(self):
        git = self.parent()
        os.mkdir(self.path("lock"), 0o755)
        held = diagnostics.hostlock.MachineLock(self.path("lock"))
        try:
            options = diagnostics.Options(
                plan=self.path("parent.json"), cases=[SMALL], recipe="linux_gnu_cpu_profile",
                output=self.path("child"), run_id="child_run", host_os="linux",
                mode="trusted_local", lock_directory=self.path("lock"))
            with self.assertRaises(diagnostics.Unavailable) as caught:
                diagnostics.run_profile(git, options)
            self.assertEqual(caught.exception.reason, "concurrent_shard")
            self.assertFalse(os.path.lexists(self.path("child")))
        finally:
            held.release()

    def test_an_automatic_selection_with_nothing_regressed_diagnoses_nothing(self):
        repo, root = lab_repo(self.path("repo"))
        root = repo.commit("diagnostics", {diagnostics.REGISTRY_PATH: read(REGISTRY)})
        git = gitrefs.Git(repo.path)
        parent = planning.plan(git, planning.Request(
            trigger="manual", tier="quick", scope="partial_scope", base=root, head=root,
            run_id="parent_run", seed=5, trusted_host_build=True,
            cases=["canonical_serialize_small"]))
        with open(self.path("parent.json"), "wb") as stream:
            stream.write(contracts.write_bytes(parent, "run"))
        verdict = verdict_for(parent, [(SMALL, stats.WITHIN_TOLERANCE)])
        record, cases = diagnostics.select(parent, verdict, verdict_sha256="sha256:" + "e" * 64)
        self.assertEqual((record["cases"], cases), ([], []))


if __name__ == "__main__":
    unittest.main()


class PerfCache(ChildRunBase):
    """The recorder fixture models perf's build-ID cache through real namespace cleanup."""

    def capture(self, whole=False, legacy=False, hostile=False):
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"),
                          cache_links=True, hostile_cache_link=hostile)
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        name = "whole_argv" if whole else "record_argv"
        actual = getattr(perfcapture, name)
        def argv(*args, **kwargs):
            values = actual(*args, **kwargs)
            return [value for value in values if value != "--no-buildid-cache"]
        self.cleanup_details = []
        original_release = diagnostics.Profile.release
        def release(profile, *args, **kwargs):
            try:
                return original_release(profile, *args, **kwargs)
            except perfcapture.CaptureError as error:
                self.cleanup_details.append(error.detail)
                raise
        with mock.patch.object(perfcapture, name, argv if legacy else actual), \
                mock.patch.object(diagnostics.Profile, "release", release):
            doc, output = self.profile("linux_gnu_cpu_profile",
                [DISCOVERY] if whole else [SMALL], tools, {},
                af_config={} if whole else None, work_removed=not (legacy or hostile))
        return doc, output

    def test_probe_recording_keeps_ids_and_cleans_without_a_cache(self):
        self.check_clean(False)

    def test_whole_command_recording_keeps_ids_and_cleans_without_a_cache(self):
        self.check_clean(True)

    def check_clean(self, whole):
        doc, output = self.capture(whole=whole)
        self.assertEqual(doc["execution"]["retained_scratch"], 0)
        self.assertTrue(os.path.isfile(os.path.join(output, "manifest.json")))
        for capture in doc["captures"]:
            self.assertIsNone(capture["omission"], capture)
            self.assertTrue(capture["symbols"]["build_ids"])
        for line in read(self.path("perf", "record-argv.jsonl")).splitlines():
            argv = json.loads(line)
            self.assertIn("--no-buildid-cache", argv)
            self.assertNotIn("--no-buildid", argv)
            self.assertEqual(argv[argv.index("-e") + 1], self.registry["capture"]["event"])
        self.assertEqual(len(doc["renders"]), 4)

    def test_probe_without_flag_reproduces_hard_link_cleanup_uncertainty(self):
        self.check_refused(False, True, False)

    def test_whole_command_without_flag_reproduces_hard_link_cleanup_uncertainty(self):
        self.check_refused(True, True, False)

    def test_a_hostile_recorder_hard_link_is_still_refused_with_the_flag(self):
        self.check_refused(False, False, True)

    def check_refused(self, whole, legacy, hostile):
        doc, output = self.capture(whole, legacy, hostile)
        self.assertEqual(doc["execution"]["retained_scratch"], 1)
        self.assertFalse(os.path.exists(os.path.join(output, "manifest.json")))
        self.assertTrue(all(item["omission"]["reason"] == "cleanup_uncertain"
                            for item in doc["captures"]))
        self.assertTrue(any("still has a link" in detail for detail in self.cleanup_details),
                        self.cleanup_details)
        self.assertIn("was not started", doc["captures"][1]["omission"]["detail"])
        self.assertEqual(doc["renders"], [])
        self.assertEqual(len(read(self.path("perf", "record-argv.jsonl")).splitlines()), 1)
        # The external admitted executable remains intact. Only this test's scratch exists.
        profiled = self.profile_object.afs if whole else self.profile_object.probes
        path, digest = profiled["baseline"]
        self.assertEqual(sha256_bytes(read(path)), digest)
        self.assertTrue(os.path.isdir(self.profile_object.retained[0]))

    def test_capability_probe_uses_the_same_cache_free_recording(self):
        boss = supervisor(self.path("supervisor"))
        perf = fakes.perf(self.path("perf"), cache_links=True)
        os.mkdir(self.path("work"))
        home = self.path("home")
        os.mkdir(home)
        result = perfcapture.probe_capability(boss, perf, "task-clock:u", 997,
            self.path("work"), {"PATH": "/usr/bin:/bin", "HOME": home}, 20)
        self.assertTrue(result[0], result)
        self.assertFalse(os.path.exists(os.path.join(home, ".debug")))
        argv = json.loads(read(self.path("perf", "record-argv.jsonl")))
        self.assertIn("--no-buildid-cache", argv)
        self.assertNotIn("--no-buildid", argv)
