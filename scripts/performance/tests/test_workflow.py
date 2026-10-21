"""The laboratory's Python compatibility legs gate a required check, and fail it when they do not
succeed.

The repository's required checks name `validation / lint` and `validation / check
(ubuntu-latest)` (`.af/code-policy.toml`); the `performance-python` matrix is not one of them.
These tests read `.github/workflows/validate.yml` as text (the standard library has no YAML
reader) and require that the required `lint` job depends on the matrix, runs whatever its
outcome, and first executes a guard that fails unless the matrix succeeded. The guard's own
shell is extracted and run for every result GitHub reports for a dependency.
"""
import os
import re
import shutil
import subprocess
import unittest

from tests.support import ROOT, read

WORKFLOW = os.path.join(ROOT, ".github/workflows/validate.yml")
POLICY = os.path.join(ROOT, ".af/code-policy.toml")
DEPENDENCY_RESULTS = ("success", "failure", "cancelled", "skipped")


def jobs(text):
    """Each top-level job's lines, keyed by job ID."""
    lines = text.splitlines()
    start = lines.index("jobs:")
    out = {}
    current = None
    for line in lines[start + 1:]:
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if match:
            current = match.group(1)
            out[current] = []
        elif current is not None:
            out[current].append(line)
    return out


def key(job, name):
    """The scalar value of a job-level key, or None."""
    for line in job:
        match = re.match(r"^    %s:\s*(.*?)\s*$" % re.escape(name), line)
        if match:
            return match.group(1)
    return None


def steps(job):
    """Each step's lines, in order."""
    out = []
    inside = False
    for line in job:
        if re.match(r"^    steps:\s*$", line):
            inside = True
            continue
        if not inside:
            continue
        if re.match(r"^    \S", line):
            break
        if line.startswith("      - "):
            out.append([line])
        elif out:
            out[-1].append(line)
    return out


def block(step, name):
    """The literal block scalar (`name: |`) of a step, dedented."""
    for index, line in enumerate(step):
        match = re.match(r"^(\s*)(?:- )?%s: \|\s*$" % re.escape(name), line)
        if match:
            indent = None
            body = []
            for item in step[index + 1:]:
                if item.strip() and indent is None:
                    indent = len(item) - len(item.lstrip())
                if item.strip() and len(item) - len(item.lstrip()) < indent:
                    break
                body.append(item[indent:] if item.strip() else "")
            return "\n".join(body) + "\n"
    return None


def env(step):
    out = {}
    inside = False
    for line in step:
        if re.match(r"^        env:\s*$", line):
            inside = True
            continue
        if inside:
            match = re.match(r"^          ([A-Z_][A-Z0-9_]*): (.*?)\s*$", line)
            if not match:
                break
            out[match.group(1)] = match.group(2)
    return out


class RequiredPythonCompatibility(unittest.TestCase):
    def setUp(self):
        self.text = read(WORKFLOW).decode("utf-8")
        self.jobs = jobs(self.text)

    def test_the_matrix_runs_python_3_9_and_3_11_plus_with_tomllib_parity(self):
        matrix = "\n".join(self.jobs["performance-python"])
        legs = re.findall(r'- python: "([0-9.]+)"\n\s+tomllib: "([0-9]*)"', matrix)
        versions = dict(legs)
        self.assertEqual(versions.get("3.9"), "", legs)
        later = [v for v in versions if tuple(int(p) for p in v.split(".")) >= (3, 11)]
        self.assertTrue(later, legs)
        self.assertTrue(all(versions[v] == "1" for v in later), legs)
        self.assertIn("fail-fast: false", matrix)
        self.assertIn("python scripts/test-performance.py", matrix)
        self.assertIn("python scripts/performance/schemas.py", matrix)

    def test_a_required_check_depends_on_the_matrix_and_always_runs(self):
        policy = read(POLICY).decode("utf-8")
        required = re.search(r'^required = (\[[^\]]*"validation / [^\]]*\])$', policy, re.M)
        self.assertIsNotNone(required)
        self.assertIn('"validation / lint"', required.group(1))
        lint = self.jobs["lint"]
        self.assertEqual(key(lint, "name"), "lint")
        self.assertEqual(key(lint, "needs"), "performance-python")
        # always() runs the job after a failed, cancelled or skipped dependency; without it
        # GitHub skips a dependent job, and a skipped required check reports success.
        self.assertEqual(key(lint, "if"), "always()")
        self.assertIsNone(key(self.jobs["performance-python"], "if"))

    def test_the_guard_runs_first_and_names_the_dependency_result(self):
        first = steps(self.jobs["lint"])[0]
        self.assertIn("Require the performance lab", first[0])
        self.assertEqual(env(first), {
            "PERFORMANCE_PYTHON": "${{ needs.performance-python.result }}"})
        self.assertFalse(any(re.match(r"^        (if|continue-on-error):", line)
                             for line in first), first)

    @unittest.skipUnless(shutil.which("bash"), "bash unavailable")
    def test_the_guard_fails_unless_every_leg_succeeded(self):
        script = block(steps(self.jobs["lint"])[0], "run")
        self.assertIsNotNone(script)
        for result in DEPENDENCY_RESULTS + ("",):
            with self.subTest(result=result):
                # GitHub runs `run:` steps with `bash -e {0}` on Linux runners.
                outcome = subprocess.run(["bash", "-e", "-c", script],
                                         env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                                              "PERFORMANCE_PYTHON": result},
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(outcome.returncode == 0, result == "success",
                                 outcome.stderr.decode())
                if result != "success":
                    self.assertIn("'%s'" % result, outcome.stderr.decode())

    def test_every_action_stays_pinned_to_a_commit(self):
        for line in self.text.splitlines():
            match = re.match(r"^\s*(?:- )?uses: (\S+)", line)
            if match and not match.group(1).startswith("./"):
                self.assertRegex(match.group(1), r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$")


if __name__ == "__main__":
    unittest.main()
