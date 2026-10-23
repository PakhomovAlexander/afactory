"""The guarded kernel experiment adapter and its literal bootstrap (ADR-0185).

The bootstrap runs here exactly as a captured code policy runs it, `python3 -I -S -B -c
BOOTSTRAP DIGEST measure ...`, over small trees with the protected layout, whose driver writes
an owned marker the moment it is imported: a refused tree must leave no marker. The adapter's
own steps run against this repository's registry and golden entries with a stand-in Cargo and
probe; the real build, the real probe and the kernel's receipts are exercised by
`crates/af/tests/it/perf_kernel_measure.rs`. Nothing here measures the product.
"""
import io
import json
import os
import shutil
import stat
import subprocess
import sys

from perflab import contracts, kernelguard, kernelmeasure, limits, planning
from perflab.values import ContractError

from tests import support

RULES = kernelguard.RULES
INSTANCE = "canonical_serialize_small__package_small_envelope"

# The driver of a guarded tree: it records that it was imported, then reports.
DRIVER = b"""import os, sys
open(os.environ["KG_MARKER"], "w").write(" ".join(sys.argv[1:]))
print('{"schema":"af.measure-report/1","metrics":{"wall_ms":{"value":"1","unit":"ms"}}}')
"""


def guarded_tree(root):
    """A tree holding every required protected file, its driver the marker-writing stand-in."""
    files = dict((path, ("// %s\n" % path).encode()) for path in RULES["REQUIRED"])
    files["scripts/performance/cli.py"] = DRIVER
    files["scripts/performance/perflab/__init__.py"] = b""
    files["fixtures/performance/activation.toml"] = b"# reviewed activation\n"
    files["crates/review-store/src/canonical.rs"] = b"// product code\n"
    files["docs/notes.md"] = b"unprotected\n"
    for path, data in files.items():
        target = os.path.join(root, *path.split("/"))
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as stream:
            stream.write(data)
    return root


def write(root, path, data):
    target = os.path.join(root, *path.split("/"))
    os.makedirs(os.path.dirname(target), exist_ok=True)
    with open(target, "wb") as stream:
        stream.write(data)


def alive(pid):
    """Whether `pid` is a live process, not a zombie of a parent that has not reaped it."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL).stdout.strip()
    return bool(state) and not state.startswith(b"Z")


class Bootstrap(support.TempDir):
    def run_bootstrap(self, root, digest, *args, **extra):
        marker = self.path("marker-%d" % len(os.listdir(self.tmp)))
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "KG_MARKER": marker,
               "PYTHONPATH": self.path("never-on-the-path")}
        env.update(extra)
        completed = subprocess.run(
            [sys.executable] + list(kernelguard.PYTHON_FLAGS)
            + [kernelguard.BOOTSTRAP, digest] + list(args or ("measure", "--case", INSTANCE,
                                                              "--metric", "wall_ms")),
            cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
        return completed, os.path.exists(marker)

    def test_the_digest_covers_exactly_the_protected_closure(self):
        root = guarded_tree(self.path("tree"))
        found, entries = kernelguard.digest(root)
        paths = [entry[0] for entry in entries]
        self.assertEqual(paths, sorted(paths, key=lambda path: path.encode("utf-8")))
        for path in RULES["REQUIRED"]:
            self.assertIn(path, paths)
        # The trace and calibration helpers are instrument; product code and docs are not.
        self.assertIn("crates/review-core/src/perf_calibration.rs", paths)
        self.assertNotIn("crates/review-store/src/canonical.rs", paths)
        self.assertNotIn("docs/notes.md", paths)
        # Unlike planning, which leaves the activation out of its fixture digest, the guard's
        # only exclusion is the staged policy subtree: no configuration gains another one.
        self.assertIn(planning.ACTIVATION_PATH, paths)
        self.assertIn(planning.ACTIVATION_PATH, planning.FIXTURE_EXCLUDE)
        self.assertEqual(RULES["EXCLUDED"], kernelguard.STAGED_DIR)
        write(root, kernelguard.STAGED_DIR + "/README.md", b"staged\n")
        write(root, kernelguard.STAGED_POLICY, b"# any staged digest\n")
        self.assertEqual(kernelguard.digest(root)[0], found)
        # Product code is free; every protected byte, addition and deletion is not.
        write(root, "crates/review-store/src/canonical.rs", b"// optimized\n")
        self.assertEqual(kernelguard.digest(root)[0], found)
        for path, data in (("fixtures/performance/golden/native.json", b"{}\n"),
                           ("crates/review-pipeline/examples/perf-probe.rs", b"// region\n"),
                           ("crates/review-pipeline/.cargo/config.toml", b"[build]\n"),
                           ("crates/review-store/build.rs", b"fn main() {}\n"),
                           ("crates/extra/Cargo.toml", b"[package]\n"),
                           ("scripts/performance/perflab/__pycache__/cli.cpython-39.pyc",
                            b"\x61\x0d\x0d\x0a\x03\x00\x00\x00" + b"\x00" * 8)):
            with self.subTest(path=path):
                changed = guarded_tree(self.path("changed-%d" % len(os.listdir(self.tmp))))
                write(changed, path, data)
                self.assertNotEqual(kernelguard.digest(changed)[0], found)

    def test_case_variant_build_inputs_are_protected(self):
        # A case-insensitive filesystem opens BUILD.RS for build.rs and .CaRgO/config.toml for
        # .cargo/config.toml, and Cargo uses them: classification never depends on case or on a
        # canonically equivalent spelling. Ordinary product sources stay free.
        protected = RULES["protected"]
        for path in ("crates/review-store/BUILD.RS", "crates/review-store/Build.Rs",
                     ".CaRgO/config.toml", "crates/review-store/.CARGO/config",
                     "crates/extra/CARGO.TOML", "CARGO.LOCK", "Cargo.loc\u212a",
                     "RUST-TOOLCHAIN.toml", "Rust-Toolchain", "SCRIPTS/Performance/perflab/x.py",
                     "Fixtures/Performance/golden/native.json",
                     "crates/review-pipeline/examples/PERF-PROBE.rs",
                     "crates/review-core/src/Perf_Trace.rs"):
            with self.subTest(path=path):
                self.assertTrue(protected(path))
        for path in ("crates/review-store/src/canonical.rs", "crates/review-store/src/build_rs.rs",
                     "docs/BUILD.md", "crates/review-store/src/cargo/mod.rs",
                     "crates/review-store/src/target/lib.rs"):
            with self.subTest(path=path):
                self.assertFalse(protected(path))
        root = guarded_tree(self.path("tree"))
        found, _ = kernelguard.digest(root)
        for path in ("crates/review-store/BUILD.RS", ".CaRgO/config.toml",
                     "crates/extra/CARGO.TOML"):
            with self.subTest(added=path):
                changed = guarded_tree(self.path("case-%d" % len(os.listdir(self.tmp))))
                write(changed, path, b"fn main() {}\n")
                entries = [entry[0] for entry in kernelguard.digest(changed)[1]]
                self.assertNotEqual(kernelguard.digest(changed)[0], found)
                self.assertTrue(any(entry.lower() == path.lower() for entry in entries), entries)
        for path in (kernelguard.STAGED_DIR + "/cargo.toml", kernelguard.STAGED_DIR + "/BUILD.RS",
                     kernelguard.STAGED_DIR + "/.CARGO/config.toml"):
            with self.subTest(staged=path):
                staged = guarded_tree(self.path("staged-case-%d" % len(os.listdir(self.tmp))))
                write(staged, path, b"[build]\n")
                with self.assertRaisesRegex(ContractError, "holds only nonexecuted"):
                    kernelguard.digest(staged)

    def test_links_special_files_and_missing_inputs_are_refused(self):
        root = guarded_tree(self.path("tree"))
        os.symlink(os.path.join(root, "Cargo.lock"), os.path.join(root, "crates", "lock"))
        with self.assertRaisesRegex(ContractError, "crates/lock is a symbolic link"):
            kernelguard.digest(root)
        parent = guarded_tree(self.path("parent"))
        os.symlink(os.path.join(parent, "scripts", "performance"),
                   os.path.join(parent, "crates", "driver"))
        with self.assertRaisesRegex(ContractError, "crates/driver is a symbolic link"):
            kernelguard.digest(parent)
        fifo = guarded_tree(self.path("fifo"))
        os.mkfifo(os.path.join(fifo, "docs", "pipe"))
        with self.assertRaisesRegex(ContractError, "docs/pipe is not a regular file"):
            kernelguard.digest(fifo)
        missing = guarded_tree(self.path("missing"))
        os.remove(os.path.join(missing, "Cargo.lock"))
        with self.assertRaisesRegex(ContractError, "required protected files are missing: "
                                    "Cargo.lock"):
            kernelguard.digest(missing)
        staged = guarded_tree(self.path("staged"))
        write(staged, kernelguard.STAGED_DIR + "/helper.py", b"import os\n")
        with self.assertRaisesRegex(ContractError, "holds only nonexecuted"):
            kernelguard.digest(staged)
        cargo = guarded_tree(self.path("staged-cargo"))
        write(cargo, kernelguard.STAGED_DIR + "/Cargo.toml", b"[package]\n")
        with self.assertRaisesRegex(ContractError, "holds only nonexecuted"):
            kernelguard.digest(cargo)

    def test_the_bootstrap_refuses_before_importing_the_candidate_driver(self):
        root = guarded_tree(self.path("tree"))
        found, entries = kernelguard.digest(root)
        completed, imported = self.run_bootstrap(root, found)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertTrue(imported)
        self.assertIn(b"%d protected files match %s" % (len(entries), found.encode()),
                      completed.stderr)
        self.assertTrue(completed.stdout.endswith(b'"unit":"ms"}}}\n'))
        tampered = (
            ("fixture", "fixtures/performance/golden/native.json", b"{\"entries\": []}\n"),
            ("timing boundary", "crates/review-pipeline/examples/perf-probe/cases.rs",
             b"// the region moved\n"),
            ("cargo configuration", ".cargo/config.toml", b"[build]\nrustflags = []\n"),
            ("extra manifest", "crates/review-pipeline/examples/x/Cargo.toml", b"[package]\n"),
            # Effective on a case-insensitive filesystem: Cargo runs BUILD.RS as the build
            # script and reads .CaRgO/config.toml as Cargo configuration.
            ("case-variant build script", "crates/review-store/BUILD.RS", b"fn main() {}\n"),
            ("case-variant cargo configuration", ".CaRgO/config.toml",
             b"[build]\nrustflags = []\n"),
            # An unchecked-hash bytecode file would be loaded instead of its verified source.
            ("bytecode", "scripts/performance/perflab/__pycache__/__init__.cpython-39.pyc",
             b"\x61\x0d\x0d\x0a\x03\x00\x00\x00" + b"\x00" * 8),
            ("driver", "scripts/performance/cli.py", DRIVER + b"# tampered\n"))
        for name, path, data in tampered:
            with self.subTest(tampered=name):
                copy = guarded_tree(self.path("tampered-%s" % name.replace(" ", "-")))
                write(copy, path, data)
                completed, imported = self.run_bootstrap(copy, found)
                self.assertEqual(completed.returncode, 2)
                self.assertFalse(imported, "the candidate driver ran before the guard refused")
                self.assertEqual(completed.stdout, b"")
                self.assertIn(b"refused before any candidate import or build", completed.stderr)
        linked = guarded_tree(self.path("linked"))
        os.symlink("../Cargo.toml", os.path.join(linked, "crates", "Cargo.toml"))
        completed, imported = self.run_bootstrap(linked, found)
        self.assertEqual((completed.returncode, imported), (2, False))
        self.assertIn(b"is a symbolic link", completed.stderr)
        # A staged policy the candidate edited changes neither the digest nor the bootstrap the
        # captured authority holds.
        staged = guarded_tree(self.path("staged-edit"))
        write(staged, kernelguard.STAGED_POLICY, b"# a candidate's own digest: sha256:00\n")
        completed, imported = self.run_bootstrap(staged, found)
        self.assertEqual((completed.returncode, imported), (0, True), completed.stderr)
        completed, imported = self.run_bootstrap(root, "sha256:" + "0" * 64)
        self.assertEqual((completed.returncode, imported), (2, False))
        self.assertIn(b"not the captured sha256:" + b"0" * 64, completed.stderr)

    def test_the_bootstrap_imports_nothing_from_the_measured_tree_before_its_check(self):
        # A planted standard-library name in the measured root is never imported: isolated mode
        # and the bootstrap's own path filter keep the tree off the path.
        root = guarded_tree(self.path("tree"))
        found, _ = kernelguard.digest(root)
        planted = self.path("planted")
        for name in ("hashlib.py", "stat.py", "runpy.py", "os.py"):
            write(root, name, b"open(__import__('sys').argv[0] and '%s', 'w').write('x')\n"
                  % planted.encode())
        completed, imported = self.run_bootstrap(root, found, "probe")
        self.assertEqual((completed.returncode, imported), (0, True), completed.stderr)
        self.assertFalse(os.path.exists(planted))


class Staged(support.TempDir):
    def test_the_staged_policy_is_rendered_and_checked_deterministically(self):
        root = guarded_tree(self.path("tree"))
        found = kernelguard.write_staged(root)
        self.assertEqual(kernelguard.check_staged(root), (found, []))
        with open(os.path.join(root, *kernelguard.STAGED_POLICY.split("/")), "rb") as stream:
            policy = stream.read()
        self.assertEqual(policy, kernelguard.render_staged(found, len(
            kernelguard.digest(root)[1])))
        from perflab import tomlfile
        parsed = tomlfile.loads(policy, kernelguard.STAGED_POLICY)
        for name, instance, metric in kernelguard.STAGED_MEASURES:
            measure = parsed["measures"][name]
            args = [arg["value"] for arg in measure["command"]["args"]]
            self.assertEqual(args, kernelguard.command_args(found, instance, metric))
            self.assertTrue(all(arg["provenance"] == "literal"
                                for arg in measure["command"]["args"]))
            self.assertEqual(args[len(kernelguard.PYTHON_FLAGS)], kernelguard.BOOTSTRAP)
            self.assertEqual((measure["warm"], measure["metrics"]),
                             (True, [{"key": metric, "unit": "ms"}]))
            # Until the guard is accepted, the objectives compare only the kernel's time.
            self.assertEqual(parsed["objectives"][name + "_elapsed"]["metric"], "elapsed_ms")
        self.assertFalse(any(objective["metric"] != "elapsed_ms"
                             for objective in parsed["objectives"].values()))
        inventory = contracts.read_file(os.path.join(root, *kernelguard.STAGED_INVENTORY.split(
            "/")), "kernel-inventory")
        self.assertEqual((inventory["harness_digest"], inventory["inner_metric"]),
                         (found, "pending"))
        # A changed protected byte, or a hand-edited staged file, is stale.
        write(root, "Cargo.lock", b"# a dependency changed\n")
        _, problems = kernelguard.check_staged(root)
        self.assertEqual(len(problems), 2, problems)
        kernelguard.write_staged(root)
        self.assertEqual(kernelguard.check_staged(root)[1], [])
        write(root, kernelguard.STAGED_POLICY, policy)
        self.assertIn("code-policy-measures.toml is not what", kernelguard.check_staged(
            root)[1][0])

    def test_an_installed_guarded_measure_must_be_the_staged_one(self):
        root = guarded_tree(self.path("tree"))
        found = kernelguard.write_staged(root)
        with open(os.path.join(root, *kernelguard.STAGED_POLICY.split("/")), "rb") as stream:
            staged = stream.read()
        write(root, ".af/code-policy.toml", b'schema = "af.code-task-policy/1"\n' + staged)
        self.assertEqual(kernelguard.check_staged(root), (found, []))
        self.assertEqual(sorted(kernelguard.installed_measures(root)),
                         sorted(name for name, _, _ in kernelguard.STAGED_MEASURES))
        # After a dependency change the installed authority still pins the old digest: the
        # repository check fails until a human re-pins it.
        write(root, "Cargo.toml", b"[workspace]\nmembers = []\n")
        kernelguard.write_staged(root)
        problems = kernelguard.check_staged(root)[1]
        self.assertEqual(len(problems), len(kernelguard.STAGED_MEASURES), problems)
        self.assertIn("a reviewed human re-pin installs it again", problems[0])

    def test_a_pin_made_in_a_worker_sandbox_ignores_its_home_and_cargo_state(self):
        """A Worker sandbox's HOME may lie inside the tree, or be the tree, and Cargo writes its
        package-cache lock and registry tag there. The pin generator ignores both, so the digest
        equals the one a developer machine computes, while the tree's own Cargo configuration
        stays protected."""
        clean = guarded_tree(self.path("developer"))
        expected = kernelguard.checkout_digest(clean, environ={"HOME": self.path("elsewhere")})
        nested = guarded_tree(self.path("nested"))
        for path in ("worker-home/.cargo/.package-cache",
                     "worker-home/.cargo/registry/CACHEDIR.TAG",
                     "worker-home/.cargo/registry/src/index/dep-1.0.0/Cargo.toml",
                     "worker-home/notes/Cargo.toml"):
            write(nested, path, b"author state\n")
        os.symlink("/", os.path.join(nested, "worker-home", "link"))
        # A build in the sandbox leaves what its checkout's .gitignore names, outside Git too.
        write(nested, ".gitignore", b"/target\n__pycache__/\n*.key\n")
        write(clean, ".gitignore", b"/target\n__pycache__/\n*.key\n")
        for path in ("target/debug/build/dep/out/Cargo.toml", "target/copy/Cargo.lock",
                     "scripts/performance/perflab/__pycache__/cli.cpython-313.pyc",
                     "scripts/performance/signing.key"):
            write(nested, path, b"build output\n")
        self.assertEqual(kernelguard.checkout_digest(
            nested, environ={"HOME": os.path.join(nested, "worker-home")}), expected)
        # The same files count when no HOME claims them, and a nested Cargo.toml is protected.
        os.unlink(os.path.join(nested, "worker-home", "link"))
        self.assertNotEqual(kernelguard.checkout_digest(
            nested, environ={"HOME": self.path("elsewhere")})[0], expected[0])
        at_root = guarded_tree(self.path("at-root"))
        for path in (".cargo/.package-cache", ".cargo/registry/CACHEDIR.TAG"):
            write(at_root, path, b"author state\n")
        self.assertEqual(kernelguard.checkout_digest(at_root, environ={"HOME": at_root}),
                         expected)
        write(at_root, ".cargo/config.toml", b"[build]\njobs = 1\n")
        found, entries = kernelguard.checkout_digest(at_root, environ={"HOME": at_root})
        self.assertNotEqual(found, expected[0])
        self.assertIn(".cargo/config.toml", [entry[0] for entry in entries])
        cargo_home = guarded_tree(self.path("cargo-home"))
        write(cargo_home, "tools/cargo/.package-cache", b"author state\n")
        self.assertEqual(kernelguard.checkout_digest(
            cargo_home, environ={"HOME": self.path("elsewhere"),
                                 "CARGO_HOME": os.path.join(cargo_home, "tools/cargo")}),
            expected)

    def test_the_cli_checks_and_re_pins(self):
        root = guarded_tree(self.path("tree"))
        cli = support.CLI
        run = lambda *args: subprocess.run([sys.executable, "-B", cli, "harness-digest",
                                            "--root", root] + list(args),
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(run("--check").returncode, 1)
        written = run("--write")
        self.assertEqual(written.returncode, 0, written.stderr)
        checked = run("--check")
        self.assertEqual((checked.returncode, checked.stdout), (0, written.stdout))
        listed = run("--json")
        doc = json.loads(listed.stdout)
        self.assertEqual(doc["harness_digest"], written.stdout.decode().strip())


def measured_copy(target):
    """This repository's files as a materialized Snapshot holds them, owner-writable so the copy
    can be removed: a checkout's tracked and unignored files, else every file of the sealed tree
    but the root's build target, scratch and caches."""
    paths = kernelguard.tracked_paths(support.ROOT)
    if paths is None:
        paths = []
        for current, dirs, files in os.walk(support.ROOT):
            relative = os.path.relpath(current, support.ROOT)
            if relative == ".":
                dirs[:] = [name for name in dirs if name not in ("target", ".git", ".scratch")]
            dirs[:] = [name for name in dirs if name != "__pycache__"]
            paths += [os.path.normpath(os.path.join(relative, name)) for name in files]
    for path in paths:
        source = os.path.join(support.ROOT, path)
        if os.path.islink(source):
            continue
        into = os.path.join(target, path)
        os.makedirs(os.path.dirname(into), exist_ok=True)
        shutil.copyfile(source, into)
    return target


class Adapter(support.TempDir):
    """`measure` against a copy of this repository's registry with a stand-in Cargo and probe."""

    @classmethod
    def setUpClass(cls):
        import tempfile
        cls.shared = tempfile.mkdtemp(prefix="af-perf-measured-")
        cls.root = measured_copy(os.path.join(cls.shared, "tree"))

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.shared, True)

    def setUp(self):
        super().setUp()
        self.bin = self.path("bin")
        os.makedirs(self.bin)
        self.scratch = self.path("scratch")
        os.makedirs(self.scratch)
        self.target = self.path("target")
        with open(os.path.join(support.ROOT, "fixtures/performance/golden/native.json")) as s:
            self.golden = dict((entry["instance_id"], entry)
                               for entry in json.load(s)["entries"])

    def tools(self, probe_body, cargo_exit=0):
        """A stand-in cargo that 'builds' a probe executing `probe_body` with the request."""
        probe = self.path("probe-source")
        with open(probe, "w") as stream:
            stream.write("#!%s\nimport json, sys\nrequest = json.load(sys.stdin)\n%s\n"
                         % (sys.executable, probe_body))
        cargo = os.path.join(self.bin, "cargo")
        with open(cargo, "w") as stream:
            stream.write("#!/bin/sh\necho \"cargo $*\" >&2\n[ %d = 0 ] || exit %d\n"
                         "mkdir -p \"$CARGO_TARGET_DIR/release/examples\"\n"
                         "cp '%s' \"$CARGO_TARGET_DIR/release/examples/perf-probe\"\n"
                         "chmod +x \"$CARGO_TARGET_DIR/release/examples/perf-probe\"\n"
                         % (cargo_exit, cargo_exit, probe))
        os.chmod(cargo, stat.S_IRWXU)

    def golden_probe(self, digest=None):
        entry = self.golden[INSTANCE]
        message = {"schema": "af.perf-probe/1", "case_id": entry["case_id"],
                   "instance_id": INSTANCE, "contract_id": entry["contract_id"],
                   "fixture_digest": entry["fixture_digest"], "parameters": entry["parameters"],
                   "metrics": {"wall_ms": {"value": "0.0135", "unit": "ms"},
                               "setup_ms": {"value": "2.5", "unit": "ms"}},
                   "correctness": {"status": "passed", "digest": digest or entry["digest"],
                                   "count": entry["count"], "detail": ""}}
        return "print(json.dumps(%r, sort_keys=True, separators=(',', ':')))" % message

    def measure(self, case=INSTANCE, metric="wall_ms", root=None, **environ):
        env = {"PATH": self.bin + ":/usr/bin:/bin", "CARGO_TARGET_DIR": self.target,
               "AF_CHECK_SCRATCH": self.scratch}
        env.update(environ)
        for key in [key for key, value in env.items() if value is None]:
            del env[key]
        out, err = io.StringIO(), io.StringIO()
        code = kernelmeasure.measure(root or self.root, case, metric, env, out, err)
        return code, out.getvalue(), err.getvalue()

    def test_one_judged_sample_is_one_report_line(self):
        self.tools(self.golden_probe())
        code, out, err = self.measure()
        self.assertEqual(code, 0, err)
        self.assertEqual(out, '{"metrics":{"wall_ms":{"unit":"ms","value":"0.0135"}},'
                              '"schema":"af.measure-report/1"}\n')
        setup = json.loads([line for line in err.splitlines()
                            if line.startswith(kernelmeasure.SETUP_PREFIX)][0][
            len(kernelmeasure.SETUP_PREFIX):])
        # The cache state is what the target held, observed: empty, then populated.
        self.assertEqual((setup["state_before"], setup["probe_before"], setup["probe_rebuilt"],
                          setup["target"]), ("empty", None, True, "kernel_bound"))
        code, _, err = self.measure()
        setup = json.loads([line for line in err.splitlines()
                            if line.startswith(kernelmeasure.SETUP_PREFIX)][0][
            len(kernelmeasure.SETUP_PREFIX):])
        self.assertEqual((code, setup["state_before"], setup["probe_rebuilt"]),
                         (0, "populated", False))
        # A case ID with one instance names it; the setup and sample wrote only the scratch.
        self.assertEqual(self.measure(case="canonical_serialize_small")[0], 0)
        self.assertTrue(os.listdir(self.scratch))

    def test_other_sources_invalidate_only_the_workspace_fingerprints_of_a_warm_target(self):
        # A materialized Snapshot's files may carry old modification times: a warm target would
        # take another revision's product build for fresh, and measure it.
        self.tools(self.golden_probe())
        fingerprints = os.path.join(self.target, "release", ".fingerprint")
        for name in ("review-store-0123456789abcdef", "serde-0123456789abcdef"):
            os.makedirs(os.path.join(fingerprints, name))
        write(self.target, kernelmeasure.SOURCES_RECORD, b'{"sources": "sha256:other"}')
        code, _, err = self.measure()
        self.assertEqual(code, 0, err)
        observed = json.loads([line for line in err.splitlines()
                               if line.startswith(kernelmeasure.SETUP_PREFIX)][0][
            len(kernelmeasure.SETUP_PREFIX):])
        self.assertEqual((observed["sources_before"], observed["workspace_fingerprints_invalidated"]),
                         ("sha256:other", 1))
        self.assertEqual(sorted(os.listdir(fingerprints)), ["serde-0123456789abcdef"])
        os.makedirs(os.path.join(fingerprints, "review-store-0123456789abcdef"))
        code, _, err = self.measure()
        observed = json.loads([line for line in err.splitlines()
                               if line.startswith(kernelmeasure.SETUP_PREFIX)][0][
            len(kernelmeasure.SETUP_PREFIX):])
        self.assertEqual((code, observed["sources_before"], observed["sources"],
                          observed["workspace_fingerprints_invalidated"]),
                         (0, observed["sources"], observed["sources"], 0))
        self.assertIn("review-store-0123456789abcdef", os.listdir(fingerprints))

    def test_the_source_key_covers_every_file_whatever_its_directory_or_mtime(self):
        # Cargo compiles crates/fixture/src/target/lib.rs as a library source, and workspace
        # packages include schemas and fixtures outside their member directories: a changed
        # byte anywhere changes the key, even with the old modification time preserved.
        tree = self.path("sources")
        for path, data in (("Cargo.toml", b"[workspace]\nmembers = [\"crates/fixture\"]\n"),
                           ("crates/fixture/Cargo.toml", b"[package]\nname = \"fixture\"\n"),
                           ("crates/fixture/src/target/lib.rs", b"pub fn f() -> u8 { 1 }\n"),
                           ("crates/fixture/src/main.rs", b"fn main() {}\n"),
                           ("schemas/included.json", b"{}\n")):
            write(tree, path, data)
        found = kernelmeasure.sources_digest(tree)
        self.assertEqual(kernelmeasure.sources_digest(tree), found)
        for path, data in (("crates/fixture/src/target/lib.rs", b"pub fn f() -> u8 { 2 }\n"),
                           ("crates/fixture/src/main.rs", b"fn main() { }\n"),
                           ("schemas/included.json", b"[]\n")):
            with self.subTest(path=path):
                full = os.path.join(tree, *path.split("/"))
                info = os.stat(full)
                with open(full, "rb") as stream:
                    original = stream.read()
                write(tree, path, data)
                os.utime(full, ns=(info.st_atime_ns, info.st_mtime_ns))
                self.assertNotEqual(kernelmeasure.sources_digest(tree), found)
                write(tree, path, original)
                self.assertEqual(kernelmeasure.sources_digest(tree), found)
        # Only content decides: a fresh modification time alone is the same source.
        os.utime(os.path.join(tree, "crates", "fixture", "src", "main.rs"))
        self.assertEqual(kernelmeasure.sources_digest(tree), found)
        os.symlink("main.rs", os.path.join(tree, "crates", "fixture", "src", "link.rs"))
        with self.assertRaisesRegex(ContractError, "link.rs is not a regular file"):
            kernelmeasure.sources_digest(tree)

    def test_a_changed_source_below_a_target_directory_invalidates_a_warm_target(self):
        self.tools(self.golden_probe())
        root = measured_copy(self.path("measured"))
        code, _, err = self.measure(root=root)
        self.assertEqual(code, 0, err)
        first = self.setup_record(err)
        hidden = os.path.join(root, "crates", "review-store", "src", "target", "lib.rs")
        write(root, "crates/review-store/src/target/lib.rs", b"// compiled source\n")
        os.utime(hidden, (0, 0))
        fingerprints = os.path.join(self.target, "release", ".fingerprint")
        os.makedirs(os.path.join(fingerprints, "review-store-0123456789abcdef"))
        code, _, err = self.measure(root=root)
        observed = self.setup_record(err)
        self.assertEqual((code, observed["sources_before"]), (0, first["sources"]))
        self.assertNotEqual(observed["sources"], first["sources"])
        self.assertEqual(observed["workspace_fingerprints_invalidated"], 1)

    def setup_record(self, err):
        return json.loads([line for line in err.splitlines()
                           if line.startswith(kernelmeasure.SETUP_PREFIX)][0][
            len(kernelmeasure.SETUP_PREFIX):])

    def test_a_corrupted_source_copy_invalidates_even_with_an_unchanged_source_record(self):
        self.tools(self.golden_probe())
        code, _, err = self.measure()
        self.assertEqual(code, 0, err)
        first = self.setup_record(err)
        cached = os.path.join(self.target, kernelmeasure.SOURCES_COPY)
        write(cached, "crates/review-store/src/canonical.rs", b"// poisoned cache\n")
        fingerprints = os.path.join(self.target, "release", ".fingerprint")
        os.makedirs(os.path.join(fingerprints, "review-store-0123456789abcdef"))
        code, _, err = self.measure()
        self.assertEqual(code, 0, err)
        observed = self.setup_record(err)
        self.assertEqual(observed["sources_before"], first["sources"])
        self.assertEqual(observed["workspace_fingerprints_invalidated"], 1)
        self.assertEqual(kernelmeasure.sources_digest(cached), first["sources"])

    def test_a_probe_over_its_output_bound_is_killed_while_it_writes(self):
        # 1114112 bytes of stderr is over the hard 1-MiB sample log maximum; a probe that never
        # stops writing must be killed, not buffered.
        pid = self.path("probe.pid")
        self.tools("import os\nopen(%r, 'w').write(str(os.getpid()))\n"
                   "chunk = b'e' * 65536\nwhile True:\n    sys.stderr.buffer.write(chunk)\n"
                   "    sys.stderr.buffer.flush()\n" % pid)
        code, out, err = self.measure()
        self.assertEqual((code, out), (1, ""))
        self.assertIn("wrote more than its bound on stdout and stderr", err)
        self.assertLess(len(err), 16 * 1024, "only a bounded tail is printed")
        self.assertFalse(alive(int(support.read(pid))))
        # The bounded run itself: everything counted, little kept.
        captured = kernelmeasure.capture(
            [sys.executable, "-c", "import sys; sys.stderr.buffer.write(b'e' * 1114112); "
             "sys.stdout.buffer.write(b'o' * 200000)"], self.tmp, {"PATH": "/usr/bin:/bin"},
            60, None, head=1000, tail=4096)
        self.assertEqual((captured.code, captured.total, len(captured.head), len(captured.tail)),
                         (0, 1314112, 1000, 4096))
        self.assertIsNone(captured.stopped())
        bounded = kernelmeasure.capture(
            [sys.executable, "-c", "import sys; sys.stderr.buffer.write(b'e' * 1114112)"],
            self.tmp, {"PATH": "/usr/bin:/bin"}, 60, limits.SAMPLE_LOG_BYTES_MAX, tail=4096)
        self.assertTrue(bounded.over)
        self.assertLessEqual(len(bounded.tail), 4096)

    def test_a_descendant_holding_the_probe_streams_fails_the_sample(self):
        # The probe reports a passing message but leaves a process holding its stderr: no
        # report. The descendant stays in the measure's process group, which the kernel's check
        # runner ends when the measure exits; here the test ends it.
        pid = self.path("descendant.pid")
        self.tools("import subprocess\nchild = subprocess.Popen(['/bin/sleep', '30'])\n"
                   "open(%r, 'w').write(str(child.pid))\n%s\n" % (pid, self.golden_probe()))
        drain = kernelmeasure.DRAIN_SECONDS
        kernelmeasure.DRAIN_SECONDS = 0.5
        self.addCleanup(setattr, kernelmeasure, "DRAIN_SECONDS", drain)
        try:
            code, out, err = self.measure()
        finally:
            descendant = int(support.read(pid))
            self.assertTrue(alive(descendant))
            os.kill(descendant, 9)
        self.assertEqual((code, out), (1, ""))
        self.assertIn("still held its streams", err)

    def test_the_build_log_and_its_failure_tail_are_bounded(self):
        self.tools(self.golden_probe())
        cargo = os.path.join(self.bin, "cargo")
        with open(cargo, "w") as stream:
            stream.write("#!%s\nimport sys\nsys.stdout.buffer.write(b'w' * 300000)\n"
                         "sys.stderr.buffer.write(b'x' * 300000 + b'the end')\nsys.exit(101)\n"
                         % sys.executable)
        log_bytes = kernelmeasure.BUILD_LOG_BYTES
        kernelmeasure.BUILD_LOG_BYTES = 65536
        self.addCleanup(setattr, kernelmeasure, "BUILD_LOG_BYTES", log_bytes)
        code, out, err = self.measure()
        self.assertEqual((code, out), (1, ""))
        self.assertIn("exited 101", err)
        self.assertTrue(err.rstrip().endswith("the end"), err[-200:])
        self.assertLess(len(err), 16 * 1024)
        logs = [os.path.join(current, name) for current, _, files in os.walk(self.scratch)
                for name in files if name == "build.log"]
        self.assertEqual([os.path.getsize(path) for path in logs], [65536])
        with open(cargo, "w") as stream:
            stream.write("#!/bin/sh\nexec /bin/sleep 30\n")
        timeout = kernelmeasure.BUILD_TIMEOUT_SECONDS
        kernelmeasure.BUILD_TIMEOUT_SECONDS = 1
        self.addCleanup(setattr, kernelmeasure, "BUILD_TIMEOUT_SECONDS", timeout)
        code, out, err = self.measure()
        self.assertEqual((code, out), (1, ""))
        self.assertIn("did not finish within its bound and was killed", err)

    def test_every_failure_exits_nonzero_with_a_diagnostic_and_no_report(self):
        self.tools(self.golden_probe(digest="sha256:" + "0" * 64))
        code, out, err = self.measure()
        self.assertEqual((code, out), (1, ""))
        self.assertIn("oracle_mismatch", err)
        self.tools("sys.exit(3)")
        self.assertEqual(self.measure()[:2], (1, ""))
        self.tools(self.golden_probe(), cargo_exit=101)
        code, out, err = self.measure()
        self.assertEqual((code, out), (1, ""))
        self.assertIn("never fetched", err)
        self.tools(self.golden_probe())
        for name, (case, metric, env), why in (
                ("spawning", ("source_capture__files_100_tree_source_files_128b", "wall_ms",
                              {}), "not a nonspawning native library case"),
                ("process", ("cli_discovery__command_help", "wall_ms", {}),
                 "names no native case"),
                ("several", ("canonical_serialize", "wall_ms", {}), "2 instances"),
                ("undeclared", (INSTANCE, "page_faults", {}), "is not a metric case"),
                ("unreported", (INSTANCE, "user_cpu_ms", {}), "reported no user_cpu_ms"),
                ("target", (INSTANCE, "wall_ms", {"CARGO_TARGET_DIR": None}),
                 "CARGO_TARGET_DIR is not set"),
                ("inside", (INSTANCE, "wall_ms", {"CARGO_TARGET_DIR": os.path.join(
                    self.root, "target", "kernel-measure-inside")}), "inside the measured"),
                ("scratch", (INSTANCE, "wall_ms", {"AF_CHECK_SCRATCH": None}),
                 "AF_CHECK_SCRATCH is not set")):
            with self.subTest(failure=name):
                code, out, err = self.measure(case, metric, **env)
                self.assertNotEqual(code, 0)
                self.assertEqual(out, "")
                self.assertIn(why, err)

    def test_the_report_is_the_exact_canonical_protocol(self):
        self.assertEqual(kernelmeasure.report_line("wall_ms", "12.34", "ms"),
                         '{"metrics":{"wall_ms":{"unit":"ms","value":"12.34"}},'
                         '"schema":"af.measure-report/1"}')
        for value in ("012", "1.0", "1e3", "-1", ".5", "1." , "1" * 39):
            with self.subTest(value=value):
                with self.assertRaises(ContractError):
                    kernelmeasure.report_line("wall_ms", value, "ms")
        with self.assertRaises(ContractError):
            kernelmeasure.report_line("wall_ms", "1", "seconds")
        # Only the in-process canonical and CAS cases are admitted in v1.
        self.assertEqual(sorted(set(case for _, instance, _ in kernelguard.STAGED_MEASURES
                                    for case in [instance.split("__")[0]])
                                - set(kernelmeasure.KERNEL_CASES)), [])


class StableSources(support.TempDir):
    def test_new_materializations_keep_one_stable_copy_and_changed_content_replaces_it(self):
        first, second, target = self.path("first"), self.path("second"), self.path("target")
        relative = "crates/example/src/target/lib.rs"
        for tree in (first, second):
            write(tree, relative, b"// same Snapshot\n")
        # Even coarse/future timestamps in a fresh materialization must not dirty this copy.
        os.utime(os.path.join(second, relative), (2000000000, 2000000000))
        os.chmod(os.path.join(first, "crates/example/src/target"), 0o555)
        self.addCleanup(os.chmod, os.path.join(first, "crates/example/src/target"), 0o755)
        found = kernelmeasure.sources_digest(first)
        path, replaced = kernelmeasure.stable_sources(first, target, found)
        self.assertTrue(replaced)
        before = os.stat(os.path.join(path, relative))
        self.assertEqual(kernelmeasure.stable_sources(second, target, found), (path, False))
        after = os.stat(os.path.join(path, relative))
        self.assertEqual((after.st_ino, after.st_mtime_ns), (before.st_ino, before.st_mtime_ns))
        write(second, relative, b"// changed Snapshot\n")
        changed = kernelmeasure.sources_digest(second)
        self.assertEqual(kernelmeasure.stable_sources(second, target, changed), (path, True))
        self.assertEqual(kernelmeasure.sources_digest(path), changed)
        self.assertEqual(os.listdir(target), [kernelmeasure.SOURCES_COPY])
        # Returning to old bytes also replaces the one copy, rather than trusting an old build.
        self.assertEqual(kernelmeasure.stable_sources(first, target, found), (path, True))

        # Executable mode is part of a Snapshot too: a build script may invoke a helper.
        os.chmod(os.path.join(second, relative), 0o755)
        executable = kernelmeasure.sources_digest(second)
        self.assertNotEqual(executable, changed)
        self.assertEqual(kernelmeasure.stable_sources(second, target, executable), (path, True))
        self.assertTrue(os.stat(os.path.join(path, relative)).st_mode & 0o111)

    def test_links_in_or_in_place_of_the_cache_are_refused_without_following_them(self):
        root, target = self.path("root"), self.path("target")
        write(root, "source", b"immutable\n")
        found = kernelmeasure.sources_digest(root)
        path, _ = kernelmeasure.stable_sources(root, target, found)
        os.symlink(os.path.join(root, "source"), os.path.join(path, "link"))
        with self.assertRaisesRegex(ContractError, "not a regular file"):
            kernelmeasure.stable_sources(root, target, found)
        shutil.rmtree(path)
        os.symlink(root, path)
        with self.assertRaisesRegex(ContractError, "not a regular directory"):
            kernelmeasure.stable_sources(root, target, found)
        self.assertEqual(kernelmeasure.sources_digest(root), found)


class Receipts(support.TempDir):
    """The native integration's exported receipts (af.perf-kernel-receipts/3, ADR-0187)."""

    def receipts(self):
        return json.loads(support.read(os.path.join(
            support.ROOT, "fixtures/performance/schema/kernel-receipts/valid-fixture.json")))

    def test_the_committed_fixtures_are_their_recipes(self):
        from tests import receipts_fixtures
        target = receipts_fixtures.TARGET
        expected = dict((name, receipts_fixtures.encode(doc))
                        for name, doc in receipts_fixtures.documents())
        self.assertEqual(sorted(os.listdir(target)), sorted(expected))
        for name, data in expected.items():
            self.assertEqual(support.read(os.path.join(target, name)), data, name)
        index = json.loads(support.read(os.path.join(
            support.ROOT, "fixtures/performance/schema/index.json")))["fixtures"]
        self.assertEqual([entry for entry in index if entry["contract"] == "kernel-receipts"],
                         receipts_fixtures.index_entries())

    def test_identities_are_recomputed_with_the_kernel_s_canonical_json_and_domains(self):
        import hashlib
        # RFC 8785 as the kernel writes it: UTF-16 member order, minimal escapes, integers.
        value = {"\U0001f600": 1, "\uff61": [True, None, -3], "a": "\u00e9\n\x1f\"\\"}
        canonical = ('{"a":"\u00e9\\n\\u001f\\"\\\\","\U0001f600":1,"\uff61":[true,null,-3]}'
                     .encode("utf-8"))
        self.assertEqual(contracts.kernel_digest(contracts.KERNEL_CONTENT_DOMAIN, value, "v"),
                         "sha256:" + hashlib.sha256(b"review.kernel/content-id/v1\0"
                                                    + canonical).hexdigest())
        self.assertEqual(contracts.kernel_blob_id(b"x"), "sha256:" + hashlib.sha256(
            b"review.kernel/content-id/v1\0x").hexdigest())
        for refused in ({"n": 2 ** 53}, {"s": "\ud800"}):
            with self.assertRaises(ContractError):
                contracts.kernel_digest(contracts.KERNEL_CONTENT_DOMAIN, refused, "v")

    def test_root_counterexamples_are_refused_and_the_valid_receipts_accepted(self):
        import copy
        doc = self.receipts()
        contracts.validate_kernel_receipts(copy.deepcopy(doc))
        # A changed run with the original content and artifact IDs kept.
        mutated = copy.deepcopy(doc)
        mutated["tasks"][0]["outputs"]["baseline"]["envelope"]["payload"]["runs"][0][
            "metrics"]["wall_ms"] = "999"
        with self.assertRaisesRegex(ContractError, "not the kernel's content ID"):
            contracts.validate_kernel_receipts(mutated)
        # A Task result pointing at another artifact than the retained envelope.
        contradictory = copy.deepcopy(doc)
        contradictory["tasks"][0]["result"]["outputs"]["baseline"]["artifact_ids"] = [
            "sha256:" + "e" * 64]
        with self.assertRaisesRegex(ContractError, "the Task result's artifact"):
            contracts.validate_kernel_receipts(contradictory)
        # The refused Task's result names no candidate or comparison, and none is retained;
        # neither side may claim one without the other.
        refused = doc["tasks"][1]
        self.assertEqual(sorted(refused["outputs"]), ["baseline"])
        self.assertNotIn("candidate", refused["result"]["outputs"])
        claimed = copy.deepcopy(doc)
        claimed["tasks"][1]["outputs"]["candidate"] = copy.deepcopy(
            doc["tasks"][0]["outputs"]["candidate"])
        with self.assertRaisesRegex(ContractError, "names no output on this port"):
            contracts.validate_kernel_receipts(claimed)
        # A port retaining another port's artifact type.
        swapped = copy.deepcopy(doc)
        outputs = swapped["tasks"][0]["outputs"]
        result = swapped["tasks"][0]["result"]["outputs"]
        outputs["baseline"], outputs["comparison"] = outputs["comparison"], outputs["baseline"]
        result["baseline"], result["comparison"] = result["comparison"], result["baseline"]
        with self.assertRaisesRegex(ContractError, "the port's artifact type"):
            contracts.validate_kernel_receipts(swapped)

    def test_a_long_stream_keeps_its_first_and_last_bytes_and_is_never_rehashed(self):
        import base64
        import copy
        import hashlib
        from tests import receipts_fixtures
        doc = self.receipts()
        data = bytes(range(256)) * 300
        part = contracts.KERNEL_STREAM_PART_BYTES
        stream = {"id": contracts.kernel_blob_id(data),
                  "sha256": "sha256:" + hashlib.sha256(data).hexdigest(), "bytes": len(data),
                  "retained": "head_and_tail", "encoding": "base64",
                  "head": base64.b64encode(data[:part]).decode(),
                  "tail": base64.b64encode(data[-part:]).decode(),
                  "omitted_bytes": len(data) - 2 * part}
        task = doc["tasks"][0]
        output = task["outputs"]["baseline"]
        output["streams"][0]["stderr"] = stream
        output["envelope"]["payload"]["runs"][0]["stderr_id"] = stream["id"]
        receipts_fixtures.seal(output["envelope"])
        output["artifact_id"] = output["envelope"]["artifact_id"]
        task["result"]["outputs"]["baseline"]["artifact_ids"] = [output["artifact_id"]]
        contracts.validate_kernel_receipts(copy.deepcopy(doc))
        # The omitted middle is the exporter's: another ID is not detectable here, and the
        # document says so by what it retains.
        unrehashed = copy.deepcopy(doc)
        unrehashed["tasks"][0]["outputs"]["baseline"]["streams"][0]["stderr"]["sha256"] = (
            "sha256:" + "0" * 64)
        contracts.validate_kernel_receipts(unrehashed)
        for name, change, why in (
                ("short head", {"head": base64.b64encode(data[:part - 1]).decode(),
                                "omitted_bytes": stream["omitted_bytes"] + 1},
                 "first and last"),
                ("not base64", {"tail": "%%%"}, "not base64 text"),
                ("claimed whole", {"retained": "whole"}, "whole exactly when"),
                ("whole but cut", {"omitted_bytes": 0, "bytes": 2 * part,
                                   "retained": "whole"}, "a whole stream")):
            with self.subTest(change=name):
                changed = copy.deepcopy(doc)
                changed["tasks"][0]["outputs"]["baseline"]["streams"][0]["stderr"].update(change)
                with self.assertRaisesRegex(ContractError, why):
                    contracts.validate_kernel_receipts(changed)
        # Another run's stream is not this run's.
        swapped = copy.deepcopy(doc)
        runs = swapped["tasks"][0]["outputs"]["baseline"]["streams"]
        runs[0]["stdout"], runs[1]["stdout"] = runs[1]["stdout"], runs[0]["stdout"]
        with self.assertRaisesRegex(ContractError, "the run's own stream"):
            contracts.validate_kernel_receipts(swapped)

    def with_stream(self, data, encoding):
        """The valid fixture with its first baseline stdout replaced by `data`, kept whole and
        split after the first part, every identity recomputed."""
        import base64
        import hashlib
        from tests import receipts_fixtures
        part = contracts.KERNEL_STREAM_PART_BYTES
        text = ((lambda raw: raw.decode("utf-8")) if encoding == "utf-8"
                else (lambda raw: base64.b64encode(raw).decode("ascii")))
        doc = self.receipts()
        output = doc["tasks"][0]["outputs"]["baseline"]
        stream = {"id": contracts.kernel_blob_id(data),
                  "sha256": "sha256:" + hashlib.sha256(data).hexdigest(), "bytes": len(data),
                  "retained": "whole", "encoding": encoding, "head": text(data[:part]),
                  "tail": text(data[part:]), "omitted_bytes": 0}
        output["streams"][0]["stdout"] = stream
        output["envelope"]["payload"]["runs"][0]["stdout_id"] = stream["id"]
        receipts_fixtures._rebind(doc)
        return doc

    def test_every_retained_part_is_bounded_in_its_decoded_bytes(self):
        part = contracts.KERNEL_STREAM_PART_BYTES
        binary = bytes(range(256)) * 300
        # A two-byte character fills a part in half its characters, so the schema's character
        # bound admits a tail of one byte too many; base64 likewise.
        accent = "é".encode("utf-8")
        for encoding, head, tail in (
                ("utf-8", b"h" * part, b"t" * part),
                ("utf-8", accent * (part // 2), accent * (part // 2)),
                ("base64", binary[:part], binary[part:2 * part])):
            with self.subTest(encoding=encoding, bound=True):
                contracts.validate_kernel_receipts(self.with_stream(head + tail, encoding))
            with self.subTest(encoding=encoding, bound=False):
                over = self.with_stream(head + tail + b"x", encoding)
                with self.assertRaisesRegex(ContractError, "a retained part holds at most 32768"):
                    contracts.validate_kernel_receipts(over)
        # The reviewers' instance: a 40,000-byte tail behind a full head, kept whole.
        with self.assertRaisesRegex(ContractError, "a retained part holds at most 32768"):
            contracts.validate_kernel_receipts(self.with_stream(b"h" * part + b"t" * 40000,
                                                                "utf-8"))

    def test_envelopes_and_results_follow_the_kernel_s_own_validation(self):
        import copy
        from tests import receipts_fixtures

        def changed(change):
            doc = self.receipts()
            change(doc)
            receipts_fixtures._rebind(doc)
            return doc

        def producer(port, name, value):
            def change(doc):
                doc["tasks"][0]["outputs"][port]["envelope"]["producer"][name] = value
            return change

        # Rust's `trim` removes exactly the White_Space characters; U+001F is not one, though
        # Python's `isspace` says it is, so the kernel admits it as a run ID.
        for character in contracts._KERNEL_WHITESPACE:
            with self.subTest(character=hex(ord(character))):
                with self.assertRaisesRegex(ContractError, "producer.run_id: blank"):
                    contracts.validate_kernel_receipts(changed(producer(
                        "baseline", "run_id", character * 2)))
        self.assertTrue("\x1f".isspace())
        contracts.validate_kernel_receipts(changed(producer("baseline", "run_id", "\x1f")))
        for name in ("node_id", "attempt_id"):
            with self.subTest(name=name):
                with self.assertRaises(ContractError):
                    contracts.validate_kernel_receipts(changed(producer("candidate", name, "\t")))
        # A kernel operation's node is optional; present, it must not be blank.
        absent = changed(lambda doc: doc["tasks"][0]["outputs"]["comparison"]["envelope"][
            "producer"].pop("node_id"))
        contracts.validate_kernel_receipts(copy.deepcopy(absent))
        with self.assertRaisesRegex(ContractError, "producer.node_id: blank"):
            contracts.validate_kernel_receipts(changed(producer("comparison", "node_id", " ")))

        def result(task, **fields):
            def change(doc):
                doc["tasks"][task]["result"].update(fields)
            return change
        # Satisfied acceptance couples completed execution, evidence, outputs and no missing
        # obligation; an unsatisfied result needs none of them.
        for fields in ({"execution": "exhausted"}, {"evidence": []},
                       {"missing_obligations": ["verified"]}):
            with self.subTest(fields=fields):
                with self.assertRaisesRegex(ContractError, "satisfied acceptance requires"):
                    contracts.validate_kernel_receipts(changed(result(0, **fields)))
        contracts.validate_kernel_receipts(changed(result(1, evidence=[],
                                                          execution="incomplete")))
        with self.assertRaisesRegex(ContractError, "satisfied acceptance requires"):
            contracts.validate_kernel_receipts(self.without_outputs())
        # The closed shape's patterns match a trailing newline; the kernel's byte checks do not.
        with self.assertRaisesRegex(ContractError, "not a kernel name"):
            contracts.validate_kernel_receipts(changed(result(1, missing_obligations=[
                "verified\n"])))
        with self.assertRaisesRegex(ContractError, "not a versioned kernel artifact type"):
            contracts.validate_kernel_receipts(changed(lambda doc: doc["tasks"][0]["result"][
                "outputs"]["snapshot"].update(artifact_type="af/SourceTree@1\n")))
        # Present bindings: never empty, on a retained port or any other, whatever their
        # cardinality.
        with self.assertRaisesRegex(ContractError, "must contain an artifact"):
            contracts.validate_kernel_receipts(changed(lambda doc: doc["tasks"][1]["result"][
                "outputs"]["snapshot"].update(artifact_ids=[], cardinality="many")))
        with self.assertRaisesRegex(ContractError, "cardinality one names exactly one"):
            contracts.validate_kernel_receipts(changed(lambda doc: doc["tasks"][1]["result"][
                "outputs"]["snapshot"].update(artifact_ids=[])))
        many = changed(lambda doc: doc["tasks"][0]["result"]["outputs"]["snapshot"].update(
            cardinality="many", artifact_ids=sorted([receipts_fixtures.digest("a"),
                                                     receipts_fixtures.digest("b")])))
        contracts.validate_kernel_receipts(many)

    def test_result_sets_are_strictly_ascending_and_output_ids_keep_their_order(self):
        import copy
        from tests import receipts_fixtures
        doc = self.receipts()

        def result(task, name, items):
            changed = copy.deepcopy(doc)
            changed["tasks"][task]["result"][name] = items
            return changed
        evidence = doc["tasks"][0]["result"]["evidence"]
        self.assertEqual(len(evidence), 4)
        self.assertEqual(evidence, sorted(set(evidence)))
        # Empty, singleton and ascending sets in the kernel's order: Rust compares a `String`
        # by its UTF-8 bytes, so an upper-case letter and `-` sort before `_` and lower case.
        for name, items in (("evidence", []), ("evidence", evidence[:1]),
                            ("missing_obligations", []), ("missing_obligations", ["verified"]),
                            ("missing_obligations", ["measured", "verified"]),
                            ("missing_obligations", ["Zeta", "a-b", "a_b", "alpha"])):
            with self.subTest(name=name, items=items, accepted=True):
                contracts.validate_kernel_receipts(result(1, name, items))
        # Out of order, refused rather than normalized; a repeated entry, by the closed shape.
        for task, name, items, why in (
                (0, "evidence", evidence[::-1], "set entries must be sorted and unique"),
                (0, "evidence", evidence[1:2] + evidence[:1] + evidence[2:],
                 "set entries must be sorted and unique"),
                (0, "evidence", evidence[:1] + evidence, "items must be unique"),
                (1, "missing_obligations", ["verified", "measured"],
                 "set entries must be sorted and unique"),
                (1, "missing_obligations", ["alpha", "Zeta"],
                 "set entries must be sorted and unique"),
                (1, "missing_obligations", ["verified", "verified"], "items must be unique")):
            with self.subTest(name=name, items=items, accepted=False):
                with self.assertRaisesRegex(ContractError, "tasks\\[%d\\]\\.result\\.%s: %s"
                                            % (task, name, why)):
                    contracts.validate_kernel_receipts(result(task, name, items))
        # An output's `artifact_ids` is a `Vec`: many distinct IDs in descending order stay
        # valid, with the result's evidence still ascending.
        unsorted = copy.deepcopy(doc)
        ids = sorted([receipts_fixtures.digest("a"), receipts_fixtures.digest("b"),
                      receipts_fixtures.digest("c")], reverse=True)
        task = unsorted["tasks"][0]["result"]
        task["outputs"]["snapshot"].update(cardinality="many", artifact_ids=ids)
        task["evidence"] = sorted(set(task["evidence"]) | set(ids))
        contracts.validate_kernel_receipts(unsorted)
        self.assertNotEqual(ids, sorted(ids))

    def without_outputs(self):
        """A satisfied result with no outputs at all, and no retained output to bind."""
        doc = self.receipts()
        task = doc["tasks"][0]
        task["result"]["outputs"] = {}
        task["outputs"] = {}
        return doc

    def test_every_invalid_fixture_is_self_consistent_but_for_its_rule(self):
        # Each document's identities are the kernel's own and every binding names the retained
        # artifact, so its refusal is the rule it names and never a stale ID.
        from tests import receipts_fixtures
        rebuilt = ("invalid-blank-producer.json", "invalid-blank-operation.json",
                   "invalid-stream-part-bytes.json", "invalid-unsorted-evidence.json",
                   "invalid-unsorted-missing-obligations.json",
                   "invalid-duplicate-evidence.json",
                   "invalid-duplicate-missing-obligations.json")
        for name, doc in receipts_fixtures.documents():
            for task in doc["tasks"]:
                # Every generated evidence set ascends, but for the one counterexample to it.
                evidence = task["result"]["evidence"]
                if name != "invalid-unsorted-evidence.json":
                    self.assertEqual(evidence, sorted(evidence), name)
                ids = {}
                for port, output in sorted(task["outputs"].items()):
                    envelope = output["envelope"]
                    unsigned = dict((key, item) for key, item in envelope.items()
                                    if key != "artifact_id")
                    consistent = (
                        envelope["content_id"] == contracts.kernel_digest(
                            contracts.KERNEL_CONTENT_DOMAIN, envelope["payload"], "payload")
                        and envelope["artifact_id"] == contracts.kernel_digest(
                            contracts.KERNEL_ARTIFACT_DOMAIN, unsigned, "envelope"))
                    if name in rebuilt or name == "valid-fixture.json":
                        self.assertTrue(consistent, (name, port))
                    ids[port] = output["artifact_id"]
                if name in rebuilt:
                    bound = dict((port, binding["artifact_ids"]) for port, binding
                                 in task["result"]["outputs"].items() if port in ids)
                    self.assertEqual(bound, dict((port, [ids[port]]) for port in ids), name)
                    self.assertTrue(set(ids.values()) <= set(task["result"]["evidence"]), name)
                    if "comparison" in ids:
                        self.assertEqual(task["outputs"]["comparison"]["envelope"][
                            "input_artifacts"], [ids["baseline"], ids["candidate"]], name)


if __name__ == "__main__":
    import unittest
    unittest.main()
