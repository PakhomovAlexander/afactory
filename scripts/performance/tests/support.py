"""Shared helpers of the laboratory tests. Every generated bundle is labeled synthetic."""
import inspect
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

from perflab import suite, synthetic

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
SUITE = os.path.join(ROOT, "fixtures/performance/suite.toml")
SYNTHETIC_SUITE = os.path.join(ROOT, "fixtures/performance/synthetic/suite.toml")
CLI = os.path.join(ROOT, "scripts/performance/cli.py")


def read(path):
    with open(path, "rb") as stream:
        return stream.read()


def synthetic_suite(edit=None):
    text = read(SYNTHETIC_SUITE).decode("utf-8")
    if edit is not None:
        text = edit(text)
    return suite.load_bytes(text.encode("utf-8"), "synthetic-suite.toml")


def replace_once(text, old, new):
    if text.count(old) < 1:
        raise AssertionError("fixture text not found: " + old)
    return text.replace(old, new, 1)


class TempDir(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="af-perf-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def path(self, *parts):
        return os.path.join(self.tmp, *parts)


def writable_copy(source, target):
    """Copy a committed fixture tree into a test's own directory and make only that copy
    owner-writable: a sealed checkout's files and directories may be read-only, and copytree
    keeps their modes."""
    shutil.copytree(source, target, symlinks=True)
    for current, dirs, files in os.walk(target):
        os.chmod(current, os.stat(current).st_mode | 0o700)
        for name in files:
            path = os.path.join(current, name)
            if not os.path.islink(path):
                os.chmod(path, os.stat(path).st_mode | 0o600)
    return target


def run_dir(test, run_plan, value, **options):
    """A synthetic run directory in the test's temporary directory."""
    target = test.path("run-%d" % len(os.listdir(test.tmp)))
    return synthetic.write_run(run_plan, target, value, **options)


class Repo:
    """A disposable Git repository for planning tests; nothing in it is ever executed."""

    def __init__(self, path, create=True):
        self.path = path
        if create:
            os.makedirs(path)
            self.git("init", "-q", "-b", "main")

    @classmethod
    def existing(cls, path):
        return cls(path, create=False)

    def git(self, *args, **kwargs):
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": self.path,
               "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
               "GIT_AUTHOR_NAME": "lab", "GIT_AUTHOR_EMAIL": "lab@example.invalid",
               "GIT_COMMITTER_NAME": "lab", "GIT_COMMITTER_EMAIL": "lab@example.invalid",
               "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
               "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z", "LC_ALL": "C"}
        result = subprocess.run(["git", "-C", self.path] + list(args), env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)
        if result.returncode != 0:
            raise AssertionError("git %s: %s" % (args, result.stderr.decode()))
        return result.stdout.decode().strip()

    def write(self, relative, data):
        full = os.path.join(self.path, relative)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "wb") as stream:
            stream.write(data if isinstance(data, bytes) else data.encode("utf-8"))

    def commit(self, message, files=None):
        for relative, data in (files or {}).items():
            self.write(relative, data)
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def tag(self, name, sha, annotated=False):
        if annotated:
            self.git("tag", "-a", name, "-m", name, sha)
        else:
            self.git("tag", name, sha)


# The pinned actions/checkout's fetch with `fetch-depth: 0` (ref-helper.ts, ADR-0178).
CHECKOUT_REFSPECS = ("+refs/heads/*:refs/remotes/origin/*", "+refs/tags/*:refs/tags/*")


def fetched_checkout(origin, path, branch="main"):
    """A checkout laid out as the pinned actions/checkout lays it out: a new repository whose
    `origin` is the local `origin` repository, fetched with that action's refspecs, so every
    branch is only `refs/remotes/origin/*`, and only the event's `branch` checked out locally."""
    repo = Repo(path)
    repo.git("remote", "add", "origin", origin)
    repo.git("fetch", "-q", "--prune", "--no-recurse-submodules", "origin", *CHECKOUT_REFSPECS)
    repo.git("checkout", "-q", "--force", "-B", branch, "refs/remotes/origin/" + branch)
    return repo


def lab_repo(path, suite_bytes=None):
    """A repository whose first commit holds the registry and a stand-in instrument."""
    repo = Repo(path)
    files = {"fixtures/performance/suite.toml": suite_bytes or read(SUITE),
             "scripts/performance/cli.py": b"# instrument stand-in\n",
             "Cargo.lock": b"# lock stand-in\nversion = 4\n",
             "README": b"product\n"}
    root = repo.commit("lab", files)
    return repo, root


def loads(data):
    return json.loads(data.decode("utf-8"))


def observe_json(selectors, data):
    """A stand-in's observation of declared JSON fields in `data`, reported the way the real
    supervisor reports it (ADR-0172); test code only. The whole output is parsed at once: a
    plain integer token is kept apart from a float, an exponent or a constant, and every
    object's member pairs are kept so a repeated selected member is a duplicate."""
    import json as _json

    class Float(object):
        pass

    def pairs(items):
        return ("object", items)

    def refuse(token):
        raise ValueError(token)

    result = {"document": {"status": "complete", "offset": None, "detail": ""},
              "selectors": [{"holders": 0, "count": 0, "duplicates": 0, "invalid": 0,
                             "first_invalid": None, "first": None, "last": None, "breaks": 0,
                             "first_break": None, "matches": [0] * len(selector["equals"])}
                            for selector in selectors]}
    try:
        text = data.decode("utf-8")
        if text.startswith("\ufeff"):
            raise ValueError("a byte order mark")
        root = _json.loads(text, object_pairs_hook=pairs, parse_float=lambda _: Float(),
                           parse_constant=refuse)
    except ValueError as error:
        # A document that ends before its value does is incomplete; anything else malformed.
        position = getattr(error, "pos", 0)
        status = "incomplete" if position >= len(data.rstrip()) else "malformed"
        result["document"] = {"status": status, "offset": min(position, len(data)),
                              "detail": "the stand-in's parser refused it"}
        return result
    previous = [None] * len(selectors)

    def visit(value, depth, reach):
        for index in reach:
            path = selectors[index]["path"].split(".")
            if len(path) == depth + 1:
                result["selectors"][index]["holders"] += 1
            if len(path) == depth:
                record(index, value)
        deeper = [index for index in reach
                  if len(selectors[index]["path"].split(".")) > depth]
        if isinstance(value, tuple):
            seen = set()
            for key, item in value[1]:
                chosen = [index for index in deeper
                          if selectors[index]["path"].split(".")[depth] == key]
                for index in chosen:
                    if index in seen:
                        result["selectors"][index]["duplicates"] += 1
                    seen.add(index)
                visit(item, depth + 1, chosen)
        elif isinstance(value, list):
            chosen = [index for index in deeper
                      if selectors[index]["path"].split(".")[depth] == "*"]
            for item in value:
                visit(item, depth + 1, chosen)

    def record(index, value):
        observed, selector = result["selectors"][index], selectors[index]
        at = observed["count"]
        observed["count"] += 1
        if selector["kind"] == "integer":
            number = value if type(value) is int and 0 <= value < 2 ** 64 else None
            if at == 0:
                observed["first"] = number
            elif previous[index] is None or number != previous[index] + 1:
                observed["breaks"] += 1
                if observed["first_break"] is None:
                    observed["first_break"] = {"index": at, "after": previous[index],
                                               "value": number}
            observed["last"] = previous[index] = number
            valid = number is not None
        else:
            valid = isinstance(value, str)
            for position, wanted in enumerate(selector["equals"]):
                if value == wanted:
                    observed["matches"][position] += 1
        if not valid:
            observed["invalid"] += 1
            if observed["first_invalid"] is None:
                observed["first_invalid"] = at

    visit(root, 0, list(range(len(selectors))))
    return result


FAKE_SUPERVISOR = r'''
import json, os, subprocess, sys, time
request = json.loads(sys.stdin.buffer.read())
assert request["schema"] == "af.perf-supervisor-request/4"
# Test hooks: every invocation is counted beside this file, and `overrides.json` there may
# replace one invocation's outcome by its index, as the real supervisor could report it.
here = os.path.dirname(os.path.abspath(sys.argv[0]))
counter = os.path.join(here, "supervisor-calls")
call = int(open(counter).read()) if os.path.exists(counter) else 0
with open(counter, "w") as stream:
    stream.write(str(call + 1))
overrides_path = os.path.join(here, "overrides.json")
overrides = json.load(open(overrides_path)) if os.path.exists(overrides_path) else {}
action = overrides.get(str(call))
if action is None and '_seed_v1"' in request["stdin"]:
    # Every native seed writer's request (ADR-0170), whatever its index.
    action = overrides.get("native_seed_writer")
if action == "garbage":
    sys.stdout.write("not a supervisor reply\n")
    sys.exit(0)
if action == "deaf":
    # A supervisor that took SIGTERM over, launched an owned nonspawning child in its own
    # session and never replies: nothing confirms that its sample ended. The child records its
    # pid beside this file so the test that owns it can end it.
    import signal
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    marker = os.path.join(here, "deaf-child")
    subprocess.Popen(["/bin/sh", "-c", "echo $$ > '%s.tmp' && mv '%s.tmp' '%s'; exec sleep 60"
                      % (marker, marker, marker)], start_new_session=True,
                     cwd=request["cwd"])
    time.sleep(120)
    sys.exit(1)
LIFECYCLE = {"observation": "pipes_only", "descendants_at_exit": None,
             "remaining_after_cleanup": None, "pipes_held": False,
             "status": "no_observed_descendants", "detail": "fake supervisor"}
NOTHING = {"observation": "pipes_only", "descendants_at_exit": 0, "remaining_after_cleanup": 0,
           "pipes_held": False, "status": "clean", "detail": "nothing was launched"}


import signal
running, cancelled = [None], [False]


def cancel(signum, frame):
    # A cancellation is forwarded to the child's whole session, as the real supervisor does.
    cancelled[0] = True
    if running[0] is not None:
        try:
            os.killpg(running[0].pid, signal.SIGKILL)
        except OSError:
            pass


signal.signal(signal.SIGTERM, cancel)


def execute(argv, stdin, environment, cwd, timeout_ms):
    started = time.monotonic_ns()
    kind, code, out, err, detail = "exited", None, b"", b"", ""
    try:
        child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, env=environment, cwd=cwd,
                                 start_new_session=True)
        running[0] = child
        try:
            out, err = child.communicate(stdin, timeout=timeout_ms / 1000.0)
            code = child.returncode
            if code < 0:
                kind = "signalled"
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            out, err = child.communicate()
            kind = "timeout"
    except OSError as error:
        kind, detail = "launch_failed", str(error)
    if cancelled[0] and kind != "launch_failed":
        kind, code = "cancelled", None
    termination = {"kind": kind, "code": code if kind == "exited" else None,
                   "signal": (-code if kind == "signalled" else 15 if kind == "cancelled"
                              else None), "detail": detail}
    wall = str(time.monotonic_ns() - started) if kind in ("exited", "signalled") else None
    observation = "none" if kind == "launch_failed" else "unreaped"
    return termination, wall, observation, out, err


setup_record = None
setup_failed = None
setup = request["setup"]
if setup is not None:
    termination, wall, observation, out, err = execute(
        setup["argv"], b"", setup["environment"], setup["cwd"], setup["timeout_ms"])
    lifecycle = NOTHING if termination["kind"] == "launch_failed" else LIFECYCLE
    setup_record = {"termination": termination, "wall_ns": wall,
                    "exit_observation": observation,
                    "stdout": {"retained": min(len(out), 4096), "total": len(out),
                               "truncated": max(0, len(out) - 4096)},
                    "stderr": {"retained": min(len(err), 4096), "total": len(err),
                               "truncated": max(0, len(err) - 4096)},
                    "stderr_tail": err[-1024:].decode("utf-8", "replace"),
                    "lifecycle": lifecycle}
    if termination["kind"] != "exited" or termination["code"] != 0:
        setup_failed = "it ended %s %s" % (termination["kind"], termination["code"])
if setup_failed is None:
    termination, wall, observation, out, err = execute(
        request["argv"], request["stdin"].encode("utf-8"), request["environment"],
        request["cwd"], request["timeout_ms"])
    lifecycle = NOTHING if termination["kind"] == "launch_failed" else LIFECYCLE
else:
    termination = {"kind": "setup_failed", "code": None, "signal": None,
                   "detail": setup_failed}
    wall, observation, out, err = None, "none", b"", b""
    lifecycle = setup_record["lifecycle"]
stdout_cap = request["stdout_cap_bytes"]
stderr_cap = request["log_cap_bytes"] - stdout_cap
kept_out, kept_err = out[:stdout_cap], err[:stderr_cap]
patterns = [pattern.encode("utf-8") for pattern in request["stdout_patterns"]]


# OBSERVE_JSON

if action == "timeout":
    termination = {"kind": "timeout", "code": None, "signal": None, "detail": ""}
    wall = None
elif action == "leak":
    lifecycle = {"observation": "process_group_scan", "descendants_at_exit": 1,
                 "remaining_after_cleanup": 0, "pipes_held": False,
                 "status": "unexpected_descendants",
                 "detail": "1 descendant outlived the child; it was killed"}
elif action == "uncertain":
    lifecycle = {"observation": "process_group_scan", "descendants_at_exit": 1,
                 "remaining_after_cleanup": 1, "pipes_held": False,
                 "status": "cleanup_uncertain", "detail": "1 process remained after cleanup"}
elif isinstance(action, str) and action.startswith("tamper:"):
    target = action[len("tamper:"):]
    os.chmod(target, 0o755)
    with open(target, "ab") as stream:
        stream.write(b"# changed by a sample\n")
result = {
    "schema": "af.perf-supervisor-result/5",
    "supervisor": {"platform": "fake", "arch": "fake", "namespace_init": False,
                   "containment": request["containment"]},
    "termination": termination,
    "timing": {"child_wall_ns": wall, "exit_observation": observation,
               "before_launch_ns": "0", "cleanup_ns": "0", "after_exit_ns": "0"},
    "stdout": {"retained": len(kept_out), "total": len(out),
               "truncated": len(out) - len(kept_out)},
    "stderr": {"retained": len(kept_err), "total": len(err),
               "truncated": len(err) - len(kept_err)},
    "stdout_pattern_counts": [out.count(pattern) for pattern in patterns],
    "stdout_json": (observe_json(request["stdout_json"], out) if request["stdout_json"]
                    else None),
    "lifecycle": lifecycle,
    "setup": setup_record,
    "scratch": None, "rusage": None, "rusage_unavailable": "fake supervisor",
    "observations": {"before": {}, "after": {}, "unavailable": "fake supervisor"},
}
sys.stdout.buffer.write(json.dumps(result).encode() + b"\n" + kept_out + kept_err)
'''


FAKE_SUPERVISOR = FAKE_SUPERVISOR.replace("# OBSERVE_JSON\n",
                                          inspect.getsource(observe_json))


def fake_supervisor(path):
    """A stand-in for the Rust perf-supervisor speaking its framed protocol; never product
    code and no lifecycle claim beyond `no_observed_descendants`."""
    with open(path, "w") as stream:
        stream.write("#!%s -I\n%s" % (sys.executable, FAKE_SUPERVISOR))
    os.chmod(path, 0o755)
    return path
