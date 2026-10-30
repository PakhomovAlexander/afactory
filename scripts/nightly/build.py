#!/usr/bin/env python3
"""Build a signed nightly af from the newest green main on the owner's Linux VM (ADR-0265).

    python3 -I scripts/nightly/build.py plan  --config PATH --date YYYY-MM-DD [--previous PATH]
    python3 -I scripts/nightly/build.py build --config PATH --date YYYY-MM-DD [--previous PATH]
                                              [--only-plan]

`plan` walks the first parents of the mirror's refs/heads/main from its tip and prints, as an
af.nightly-plan/1 document, the newest commit whose `check /` check runs are all green, every
red commit it walked over, the previous night and the stable baseline. `build` builds that
plan in the pinned musl image, packages and smokes both Linux targets as the release does,
signs SHA256SUMS with the nightly key and writes builds_root/<date>/nightly.json
(af.nightly-build/1). It never pushes, publishes, posts or deletes anything outside
builds_root/<date>/.

Exit codes: 0 built (or planned), 3 skipped because main has not moved since the previous
night, 1 refused with the reason on stderr. docs/nightly.md is the operator page.
"""
import argparse
import hashlib
import http.client
import io
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import time

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(ROOT, "scripts", "performance"))

from perflab import contracts, gitrefs, tomlfile  # noqa: E402
from perflab.nightlycontracts import (CHECK_PREFIX, DATE_PATTERN, RELEASE_JOB_PREFIX,  # noqa: E402
                                      RELEASE_JOBS, SIGNATURE, SUMS, TARGETS, asset_name,
                                      night_date, nightly_tag, nightly_version)
from perflab.values import ContractError, loads_strict, require  # noqa: E402

EXIT_BUILT, EXIT_REFUSED, EXIT_SKIPPED = 0, 1, 3
MAX_WALK_DEFAULT = 20
API_HOST = "api.github.com"
CONNECT_SECONDS, READ_SECONDS = 5, 30
RESPONSE_BYTES_MAX = 16 * 1024 * 1024
PER_PAGE, PAGES_MAX = 100, 20
GREEN = ("success", "skipped")
PASSWORD_ENV = "AF_NIGHTLY_SIGN_PASSWORD"
SHELLS = ("bash", "zsh", "fish", "elvish", "powershell")
# Bounds of each external step, in seconds.
FETCH_SECONDS, BUILD_SECONDS, PROBE_SECONDS = 1800, 5400, 120
SMOKE_SECONDS, AF_SECONDS, SIGN_SECONDS = 900, 120, 120
CONTAINER_HOME = "/home/nightly"


class Refused(Exception):
    """The night cannot be planned or built; the message says why."""


# Configuration ------------------------------------------------------------------------------

def load_config(path):
    """The af.nightly-config/1 TOML at `path`, validated, with `max_walk` defaulted."""
    try:
        with open(path, "rb") as stream:
            data = stream.read(tomlfile.MAX_TOML_BYTES + 1)
    except OSError as error:
        raise Refused("--config %s: %s" % (path, error))
    doc = contracts.validate_nightly_config(tomlfile.loads(data, path), path)
    return dict(doc, max_walk=doc.get("max_walk", MAX_WALK_DEFAULT))


def read_token(config):
    """The GitHub token from `github_token_file`, or None to ask anonymously."""
    path = config.get("github_token_file")
    if path is None:
        return None
    try:
        with open(path, "r", encoding="utf-8") as stream:
            token = stream.read(4096).strip()
    except OSError as error:
        raise Refused("github_token_file %s: %s" % (path, error))
    require(token and re.match(r"^[A-Za-z0-9_.-]+$", token) is not None,
            "github_token_file %s holds no token" % path)
    return token


# GitHub check runs --------------------------------------------------------------------------

def https_get(path, headers):
    """One bounded GET of api.github.com: 5 s to connect, 30 s for each read."""
    connection = http.client.HTTPSConnection(API_HOST, timeout=CONNECT_SECONDS)
    try:
        connection.connect()
        connection.sock.settimeout(READ_SECONDS)
        connection.request("GET", path, headers=headers)
        response = connection.getresponse()
        return response.status, response.read(RESPONSE_BYTES_MAX + 1)
    finally:
        connection.close()


class GitHub:
    """The check runs of a commit, every page, through `get` (`https_get` by default), each
    request retried once when it fails or the server answers 5xx."""

    def __init__(self, repository, token=None, get=https_get):
        self.repository = repository
        self.token = token
        self.get = get

    def request(self, path):
        headers = {"Accept": "application/vnd.github+json", "User-Agent": "af-nightly-build",
                   "X-GitHub-Api-Version": "2022-11-28"}
        if self.token:
            headers["Authorization"] = "Bearer " + self.token
        failure = None
        for _ in range(2):
            try:
                status, body = self.get(path, headers)
            except (OSError, http.client.HTTPException) as error:
                failure = str(error) or type(error).__name__
                continue
            if status >= 500:
                failure = "HTTP %d" % status
                continue
            if status != 200:
                raise Refused("GitHub answered HTTP %d for %s" % (status, path))
            try:
                return loads_strict(body, "GitHub " + path, RESPONSE_BYTES_MAX)
            except ContractError as error:
                raise Refused(str(error))
        raise Refused("GitHub %s failed twice: %s" % (path, failure))

    def check_runs(self, sha):
        """[{name, status, conclusion}] of every check run of `sha`, all pages."""
        runs = []
        for page in range(1, PAGES_MAX + 1):
            doc = self.request("/repos/%s/commits/%s/check-runs?filter=latest&per_page=%d"
                               "&page=%d" % (self.repository, sha, PER_PAGE, page))
            batch = doc.get("check_runs") if isinstance(doc, dict) else None
            total = doc.get("total_count") if isinstance(doc, dict) else None
            if not isinstance(batch, list) or type(total) is not int:
                raise Refused("GitHub check runs of %s: not a check-runs page" % sha)
            for item in batch:
                if not (isinstance(item, dict) and isinstance(item.get("name"), str)
                        and isinstance(item.get("status"), str)
                        and (item.get("conclusion") is None
                             or isinstance(item.get("conclusion"), str))):
                    raise Refused("GitHub check runs of %s: a malformed check run" % sha)
                runs.append({"name": item["name"], "status": item["status"],
                             "conclusion": item["conclusion"]})
            if not batch or len(runs) >= total:
                return runs
        raise Refused("%s has more than %d check runs" % (sha, PER_PAGE * PAGES_MAX))


def is_release_job(name):
    return name in RELEASE_JOBS or name.startswith(RELEASE_JOB_PREFIX)


def judge(runs, required_checks):
    """None when the check runs make a commit green, else why it is red.

    Green: at least one `check /` run, every one of them completed with success or skipped,
    and every required check present and green. The release workflow's own jobs are
    ignored."""
    runs = [run for run in runs if not is_release_job(run["name"])]
    names = set(run["name"] for run in runs)
    problems = []
    if not any(run["name"].startswith(CHECK_PREFIX) for run in runs):
        problems.append("no `check /` check runs")
    for name in required_checks:
        if name not in names:
            problems.append("required check %r is missing" % name)
    for run in sorted(runs, key=lambda item: item["name"]):
        if not (run["name"].startswith(CHECK_PREFIX) or run["name"] in required_checks):
            continue
        if run["status"] != "completed":
            problems.append("%s is %s" % (run["name"], run["status"]))
        elif run["conclusion"] not in GREEN:
            problems.append("%s concluded %s" % (run["name"], run["conclusion"]))
    if not problems:
        return None
    reason = "; ".join(problems)
    return reason if len(reason) <= 2048 else reason[:2045] + "..."


# The mirror ---------------------------------------------------------------------------------

class Mirror(gitrefs.Git):
    """Read-only access to the host's bare mirror, which the operator trusts by configuring
    it (as the executor trusts its `repository_path`)."""

    def __init__(self, path):
        gitrefs.Git.__init__(self, path, trusted_repository=True)

    def first_parents(self, ref, count):
        tip = self.exact_ref(ref, "mirror")
        if tip is None:
            raise Refused("the mirror %s has no %s" % (self.repo, ref))
        out = self._run(["rev-list", "--first-parent", "--max-count=%d" % count, tip, "--"])
        return out.stdout.decode("ascii").split()

    def committed_unix(self, sha):
        return int(self._run(["log", "-1", "--format=%ct", sha, "--"]).stdout.strip())

    def export(self, sha, destination):
        """Write the tree of `sha` into the empty directory `destination`."""
        archive = self._run(["archive", "--format=tar", sha]).stdout
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as tar:
            for member in tar.getmembers():
                require(not os.path.isabs(member.name) and ".." not in member.name.split("/"),
                        "git archive of %s holds the path %r" % (sha, member.name))
            if hasattr(tarfile, "data_filter"):
                tar.extractall(destination, filter="data")
            else:
                tar.extractall(destination)


def choose(git, github, required_checks, max_walk, previous_head):
    """(chosen sha, rejected) of the walk along main's first parents from its tip.

    The newest commit whose checks are green is chosen, and every commit's checks are judged,
    the previous night's head included. The walk never goes behind the previous night's head:
    green, it is chosen (the night is skipped); red, the night is refused, because a nightly
    never moves backwards."""
    rejected = []
    for sha in git.first_parents("refs/heads/main", max_walk):
        reason = judge(github.check_runs(sha), required_checks)
        if reason is None:
            return sha, rejected
        if sha == previous_head:
            raise Refused("the previous night's head %s is red now (%s), and a nightly never "
                          "moves behind it" % (sha, reason))
        rejected.append({"sha": sha, "reason": reason})
    raise Refused("no green commit among the newest %d first-parent commits of main: %s"
                  % (max_walk, "; ".join("%s: %s" % (entry["sha"][:12], entry["reason"])
                                         for entry in rejected[:3]) or "main is empty"))


# Versions and Cargo files -------------------------------------------------------------------

_HEADER = re.compile(r"^\s*\[\[?\s*([^\]]+?)\s*\]\]?\s*(?:#.*)?$")
_KEY_STRING = r'^\s*%s\s*=\s*"([^"\\]*)"\s*(?:#.*)?$'


def _workspace_version_line(text):
    lines = text.splitlines(True)
    section = None
    found = []
    for index, line in enumerate(lines):
        header = _HEADER.match(line.rstrip("\r\n"))
        if header is not None:
            section = header.group(1)
            continue
        value = re.match(_KEY_STRING % "version", line.rstrip("\r\n"))
        if section == "workspace.package" and value is not None:
            found.append((index, value.group(1)))
    require(len(found) == 1, "Cargo.toml: expected one version in [workspace.package], "
            "found %d" % len(found))
    return lines, found[0][0], found[0][1]


def workspace_version(manifest):
    """The `[workspace.package] version` of the Cargo.toml text `manifest`."""
    return _workspace_version_line(manifest)[2]


def rewrite_manifest(manifest, version):
    """(old version, Cargo.toml text with `[workspace.package] version` set to `version`)."""
    lines, index, old = _workspace_version_line(manifest)
    ending = lines[index][len(lines[index].rstrip("\r\n")):]
    lines[index] = 'version = "%s"%s' % (version, ending)
    return old, "".join(lines)


def rewrite_lock(lock, members, old, version):
    """Cargo.lock text with every workspace member's `version = "<old>"` set to `version`.

    A member is a `[[package]]` named in `members` with no `source` (a path package). Each
    member must appear exactly once at `old`; nothing else changes."""
    lines = lock.splitlines(True)
    starts = [index for index, line in enumerate(lines) if line.rstrip("\r\n") == "[[package]]"]
    rewritten = []
    for number, start in enumerate(starts):
        end = starts[number + 1] if number + 1 < len(starts) else len(lines)
        fields = {}
        for index in range(start + 1, end):
            if _HEADER.match(lines[index].rstrip("\r\n")):
                break
            for key in ("name", "version", "source"):
                value = re.match(_KEY_STRING % key, lines[index].rstrip("\r\n"))
                if value is not None:
                    fields[key] = (index, value.group(1))
        name = fields.get("name", (None, None))[1]
        if name not in members or "source" in fields:
            continue
        require("version" in fields and fields["version"][1] == old,
                "Cargo.lock: workspace member %s is not at version %s" % (name, old))
        index = fields["version"][0]
        ending = lines[index][len(lines[index].rstrip("\r\n")):]
        lines[index] = 'version = "%s"%s' % (version, ending)
        rewritten.append(name)
    require(sorted(rewritten) == sorted(members),
            "Cargo.lock: expected each workspace member once, rewrote %s of %s"
            % (", ".join(sorted(rewritten)) or "none", ", ".join(sorted(members))))
    return "".join(lines)


def workspace_members(source):
    """The package names of the workspace at `source`; each must inherit the workspace
    version, so rewriting [workspace.package] moves all of them."""
    def load(path):
        with open(path, "rb") as stream:
            return tomlfile.loads(stream.read(tomlfile.MAX_TOML_BYTES + 1), path)
    manifest = load(os.path.join(source, "Cargo.toml"))
    paths = manifest.get("workspace", {}).get("members", [])
    require(isinstance(paths, list) and paths, "Cargo.toml: no workspace members")
    names = []
    for member in paths:
        require(isinstance(member, str) and "*" not in member and "?" not in member,
                "Cargo.toml: workspace member %r is not a plain path" % (member,))
        package = load(os.path.join(source, member, "Cargo.toml")).get("package", {})
        require(package.get("version") == {"workspace": True},
                "%s/Cargo.toml: the package does not inherit the workspace version" % member)
        names.append(package["name"])
    return names


def rewrite_source(source, version):
    """Set the workspace version of the exported tree at `source` to `version` in Cargo.toml
    and in every workspace member of Cargo.lock; the old version."""
    paths = [os.path.join(source, name) for name in ("Cargo.toml", "Cargo.lock")]
    texts = []
    for path in paths:
        with open(path, "r", encoding="utf-8", newline="") as stream:
            texts.append(stream.read())
    old, manifest = rewrite_manifest(texts[0], version)
    lock = rewrite_lock(texts[1], workspace_members(source), old, version)
    for path, text in zip(paths, (manifest, lock)):
        with open(path, "w", encoding="utf-8", newline="") as stream:
            stream.write(text)
    return old


def sha256sums_text(digests):
    """SHA256SUMS exactly as the release writes it (`sha256sum ./*.tar.gz | sed 's| \\./| |'`):
    `<hex>  <asset>` per archive, in name order."""
    return "".join("%s  %s\n" % (digests[name], name) for name in sorted(digests))


# The plan -----------------------------------------------------------------------------------

def read_record(path, where):
    try:
        return contracts.read_file(path, "nightly-build")
    except OSError as error:
        raise Refused("%s %s: %s" % (where, path, error))


def previous_night(builds_root, date, path=None):
    """The record of the newest night before `date`, built or skipped: `path` when given,
    else the newest `builds_root/<YYYY-MM-DD>/nightly.json`; None on the first night."""
    if path is not None:
        record = read_record(path, "--previous")
        require(record["date"] < date, "--previous %s: not an earlier night" % path)
        return record
    if not os.path.isdir(builds_root):
        return None
    for name in sorted(os.listdir(builds_root), reverse=True):
        candidate = os.path.join(builds_root, name, "nightly.json")
        if re.match(DATE_PATTERN, name) is None or name >= date or not os.path.isfile(candidate):
            continue
        record = read_record(candidate, "previous night")
        require(record["date"] == name, "%s records the night %s" % (candidate, record["date"]))
        return record
    return None


def last_built_tag(previous):
    """The tag of the last built night: the previous record's own when it is built, else the
    one its skipped record carries forward; None on the first night."""
    if previous is None:
        return None
    return previous["tag"] if previous["status"] == "built" else previous["previous_tag"]


def make_plan(config, date, git, github, previous):
    head, rejected = choose(git, github, config["required_checks"], config["max_walk"],
                            previous["head_sha"] if previous else None)
    built_tag = last_built_tag(previous)
    manifest = git.read_file(head, "Cargo.toml")
    require(manifest is not None, "%s has no Cargo.toml" % head)
    cargo_version = workspace_version(manifest.decode("utf-8"))
    baseline_tag, baseline_sha = gitrefs.nightly_baseline(git, head)
    tag = nightly_tag(date)
    skipped = previous is not None and previous["head_sha"] == head
    plan = {
        "schema": contracts.CONTRACTS["nightly-plan"][0],
        "status": "skipped" if skipped else "build",
        "date": date, "tag": tag, "version": nightly_version(cargo_version, date),
        "head_sha": head, "head_committed_unix": git.committed_unix(head),
        "rejected": rejected,
        "previous_head_sha": previous["head_sha"] if previous else None,
        "previous_tag": built_tag,
        "baseline_tag": baseline_tag, "baseline_sha": baseline_sha,
        "cargo_version_on_main": cargo_version,
        "skip_reason": ("main has not moved since %s: its newest green commit is %s, the "
                        "commit %s built" % (built_tag, head, built_tag)
                        if skipped else None),
    }
    return contracts.validate_nightly_plan(plan)


# Running things -----------------------------------------------------------------------------

class Host:
    """The processes a build runs, each bounded; tests substitute a fake."""

    def __init__(self):
        self.system = platform.system()
        self.machine = platform.machine()

    def run(self, argv, cwd=None, env=None, stdin=None, timeout=None):
        try:
            return subprocess.run(argv, cwd=cwd, env=env, input=stdin, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=timeout)
        except subprocess.TimeoutExpired:
            raise Refused("%s did not finish within %d s" % (argv[0], timeout))
        except OSError as error:
            raise Refused("%s: %s" % (argv[0], error))

    def executable(self, path):
        return os.path.isfile(path) and os.access(path, os.X_OK)


def tail(data, lines=12):
    text = data.decode("utf-8", "replace").rstrip()
    return "\n".join(text.splitlines()[-lines:])


def now_ms():
    return int(time.time() * 1000)


class Night:
    """One night's build in builds_root/<date>/."""

    def __init__(self, config, plan, host, clock, environ):
        self.config = config
        self.plan = plan
        self.host = host
        self.clock = clock
        self.environ = environ
        self.root = os.path.join(config["builds_root"], plan["date"])
        self.source = os.path.join(self.root, "source")
        self.dist = os.path.join(self.root, "dist")
        self.work = os.path.join(self.root, "work")
        self.logs = os.path.join(self.root, "logs")
        self.cache = os.path.join(config["builds_root"], "cache", "cargo")
        self.timings = {}
        self.image_env = None

    def phase(self, name, action, *args):
        started = self.clock()
        sys.stderr.write("nightly: %s\n" % name)
        result = action(*args)
        self.timings[name] = {"started_unix_ms": started, "ended_unix_ms": self.clock()}
        return result

    def path_env(self):
        return self.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin")

    def check(self, result, what, log=None):
        if log is not None:
            with open(os.path.join(self.logs, log), "wb") as stream:
                stream.write(result.stdout + result.stderr)
        if result.returncode != 0:
            raise Refused("%s failed (exit %d)%s" % (what, result.returncode,
                                                     ": " + tail(result.stderr)
                                                     if result.stderr.strip() else ""))
        return result

    # The container -------------------------------------------------------------------------

    def inspect_image(self):
        image = self.config["build_image"]
        result = self.host.run(["docker", "image", "inspect", "--format",
                                "{{json .Config.Env}}", image],
                               env={"PATH": self.path_env(), "LC_ALL": "C"},
                               timeout=PROBE_SECONDS)
        if result.returncode != 0:
            raise Refused("the build image %s is not on this host (a nightly never pulls): %s"
                          % (image, tail(result.stderr, 2)))
        try:
            declared = json.loads(result.stdout.decode("utf-8")) or []
        except ValueError:
            raise Refused("docker image inspect gave no environment for %s" % image)
        self.image_env = dict(item.split("=", 1) for item in declared
                              if isinstance(item, str) and "=" in item)

    def container(self, name, argv, network=False, offline=True):
        env = {"PATH": self.image_env.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
               "CARGO_HOME": "/cargo", "HOME": CONTAINER_HOME, "TMPDIR": "/tmp",
               "LC_ALL": "C"}
        if "RUSTUP_HOME" in self.image_env:
            env["RUSTUP_HOME"] = self.image_env["RUSTUP_HOME"]
        if offline:
            env["CARGO_NET_OFFLINE"] = "true"
        mounts = ((self.source, "/src"), (self.cache, "/cargo"),
                  (os.path.join(self.work, "home"), CONTAINER_HOME),
                  (os.path.join(self.work, "tmp"), "/tmp"))
        command = ["docker", "run", "--rm", "--name", name, "--pull", "never",
                   "--network", "bridge" if network else "none", "--read-only",
                   "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                   "--user", "%d:%d" % (os.getuid(), os.getgid()), "--workdir", "/src"]
        for source, target in mounts:
            command += ["--mount", "type=bind,src=%s,dst=%s" % (source, target)]
        return (command + ["--entrypoint", "/usr/bin/env", self.config["build_image"], "-i"]
                + ["%s=%s" % item for item in sorted(env.items())] + list(argv))

    def in_container(self, phase, argv, timeout, network=False, offline=True):
        name = "af-nightly-%s-%s" % (self.plan["date"], phase)
        client = {"PATH": self.path_env(), "LC_ALL": "C"}
        try:
            result = self.host.run(self.container(name, argv, network, offline), env=client,
                                   timeout=timeout)
        except Refused:
            self.host.run(["docker", "rm", "--force", name], env=client, timeout=PROBE_SECONDS)
            raise
        return self.check(result, " ".join(argv[:3]) + " in " + self.config["build_image"],
                          phase + ".log")

    # The phases ----------------------------------------------------------------------------

    def clear(self):
        """Remove the partial night directory a refused attempt left, if any."""
        if os.path.lexists(self.root):
            require(os.path.isdir(self.root) and not os.path.islink(self.root),
                    "%s is not a directory" % self.root)
            shutil.rmtree(self.root)

    def prepare(self):
        """An empty night directory; a partial one an earlier attempt left is replaced."""
        self.clear()
        for path in (self.source, self.dist, self.logs, os.path.join(self.work, "home"),
                     os.path.join(self.work, "tmp"), self.cache):
            os.makedirs(path, exist_ok=True)

    def export(self, git):
        git.export(self.plan["head_sha"], self.source)
        old = rewrite_source(self.source, self.plan["version"])
        require(old == self.plan["cargo_version_on_main"],
                "the exported Cargo.toml is at %s, not %s" % (old,
                                                               self.plan["cargo_version_on_main"]))

    def fetch(self):
        self.inspect_image()
        installed = self.in_container("targets", ["rustup", "target", "list", "--installed"],
                                      PROBE_SECONDS)
        missing = [target for target in TARGETS
                   if target not in installed.stdout.decode("utf-8", "replace").split()]
        if missing:
            raise Refused("the build image %s has no Rust target %s"
                          % (self.config["build_image"], ", ".join(missing)))
        self.in_container("fetch", ["cargo", "fetch", "--locked"], FETCH_SECONDS,
                          network=True, offline=False)

    def build(self, target):
        # Exactly the release's command; CARGO_NET_OFFLINE and --network none keep it offline.
        self.in_container("build-" + target, ["cargo", "zigbuild", "--release", "--locked",
                                              "--bin", "af", "--target", target],
                          BUILD_SECONDS)

    def af_env(self):
        home = os.path.join(self.work, "host-home")
        os.makedirs(home, exist_ok=True)
        return {"PATH": self.path_env(), "LC_ALL": "C", "HOME": home,
                "XDG_CONFIG_HOME": os.path.join(home, ".config"),
                "XDG_STATE_HOME": os.path.join(home, ".local", "state"),
                "XDG_DATA_HOME": os.path.join(home, ".local", "share"),
                "XDG_CACHE_HOME": os.path.join(home, ".cache"), "AF_SELF_OFFLINE": "1"}

    def package(self, target):
        """Prove, smoke and package one target as release.yml's `Prove, plan, and package`
        step does; its targets entry."""
        binary = os.path.join(self.source, "target", target, "release", "af")
        require(os.path.isfile(binary), "the build left no %s" % binary)
        run = os.path.join(self.work, "run-af-" + target)
        prefix = ([shlex.quote(self.config["qemu_aarch64"])]
                  if target.startswith("aarch64-") else [])
        with open(run, "w", encoding="utf-8") as stream:
            stream.write("#!/bin/sh\nexec %s \"$@\"\n" % " ".join(prefix + [shlex.quote(binary)]))
        os.chmod(run, 0o755)
        env = self.af_env()

        def af(*args):
            return self.check(self.host.run([run] + list(args), cwd=self.work, env=env,
                                            timeout=AF_SECONDS), "af " + " ".join(args))

        reported = af("--version").stdout.decode("utf-8", "replace").strip()
        require(reported == "af " + self.plan["version"],
                "the %s binary reports %r, expected 'af %s'" % (target, reported,
                                                                self.plan["version"]))
        report = af("--version", "--json").stdout
        version_json = "version-%s.json" % target
        with open(os.path.join(self.root, version_json), "wb") as stream:
            stream.write(report)
        try:
            embedded = loads_strict(report, version_json, RESPONSE_BYTES_MAX).get("release_key")
        except (ContractError, AttributeError):
            embedded = None
        require(embedded == "embedded", "the %s binary embeds no release key "
                "(crates/af/keys/release.pub)" % target)
        smoke = os.path.join(self.source, "scripts", "release-smoke.sh")
        self.check(self.host.run(["bash", smoke, run, os.path.join(self.work, "smoke-" + target)],
                                 cwd=self.work, env=env, timeout=SMOKE_SECONDS),
                   "scripts/release-smoke.sh on " + target, "smoke-%s.log" % target)
        stage = os.path.join(self.work, "dist-" + target)
        os.makedirs(os.path.join(stage, "completions"))
        shutil.copyfile(binary, os.path.join(stage, "af"))
        os.chmod(os.path.join(stage, "af"), 0o755)
        for shell in SHELLS:
            with open(os.path.join(stage, "completions", "af." + shell), "wb") as stream:
                stream.write(af("completions", shell).stdout)
        af("self", "man", os.path.join(stage, "man"))
        name = asset_name(self.plan["tag"], target)
        archive = os.path.join(self.dist, name)
        with tarfile.open(archive, "w:gz") as tar:
            for member in ("af", "completions", "man"):
                tar.add(os.path.join(stage, member), arcname=member, filter=_owned_by_root)
        with open(archive, "rb") as stream:
            data = stream.read()
        return {"target": target, "asset": "dist/" + name,
                "sha256": "sha256:" + hashlib.sha256(data).hexdigest(), "bytes": len(data),
                "smoke": "passed", "version_json": version_json}

    def sign(self, entries):
        digests = dict((entry["asset"][len("dist/"):], entry["sha256"][len("sha256:"):])
                       for entry in entries)
        with open(os.path.join(self.root, SUMS), "w", encoding="ascii", newline="") as stream:
            stream.write(sha256sums_text(digests))
        env = {"PATH": self.path_env(), "LC_ALL": "C"}
        password = self.environ.get(PASSWORD_ENV, "")
        comment = "af " + self.plan["tag"]
        # -W signs with a key that has no passphrase; with one, minisign reads it from stdin.
        argv = ["minisign", "-S"] + ([] if password else ["-W"]) + [
            "-s", self.config["sign_key"], "-m", "SHA256SUMS", "-t", comment]
        self.check(self.host.run(argv, cwd=self.dist, env=env,
                                 stdin=(password + "\n").encode("utf-8") if password else None,
                                 timeout=SIGN_SECONDS), "minisign -S")
        verified = self.host.run(["minisign", "-V", "-q", "-m", "SHA256SUMS", "-x",
                                  "SHA256SUMS.minisig", "-p", self.config["public_key"]],
                                 cwd=self.dist, env=env, timeout=SIGN_SECONDS)
        if verified.returncode != 0:
            raise Refused("SHA256SUMS.minisig does not verify against public_key %s: %s"
                          % (self.config["public_key"], tail(verified.stderr, 3)))
        with open(os.path.join(self.root, SIGNATURE), "r", encoding="utf-8") as stream:
            lines = stream.read().splitlines()
        require(len(lines) >= 3 and lines[2] == "trusted comment: " + comment,
                "SHA256SUMS.minisig does not carry the trusted comment %r" % comment)

    def record(self, status, targets):
        record = dict(self.plan, schema=contracts.CONTRACTS["nightly-build"][0], status=status,
                      build_image=self.config["build_image"], targets=targets,
                      sha256sums=({"path": SUMS, "signature_path": SIGNATURE}
                                  if status == "built" else None),
                      timings=self.timings)
        data = contracts.write_bytes(record, "nightly-build")
        os.makedirs(self.root, exist_ok=True)
        path = os.path.join(self.root, "nightly.json")
        with open(path + ".tmp", "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(path + ".tmp", path)
        return record

    def refuse_recorded(self):
        path = os.path.join(self.root, "nightly.json")
        if os.path.lexists(path):
            raise Refused("%s already records this night" % path)

    def skip(self):
        """A skipped night's directory holds only its record."""
        self.refuse_recorded()
        self.clear()
        return self.record("skipped", [])

    def run_build(self, git):
        self.refuse_recorded()
        require(self.host.system == "Linux" and self.host.machine == "x86_64",
                "a nightly builds on an x86_64 Linux host, not %s %s" % (self.host.system,
                                                                          self.host.machine))
        if not self.host.executable(self.config["qemu_aarch64"]):
            raise Refused("qemu_aarch64 %s is not an executable: the aarch64 smoke cannot run, "
                          "and a nightly never skips it" % self.config["qemu_aarch64"])
        for key in ("sign_key", "public_key"):
            require(os.path.isfile(self.config[key]), "%s %s is not a file" % (
                key, self.config[key]))
        self.prepare()
        self.phase("export", self.export, git)
        self.phase("fetch", self.fetch)
        for target in TARGETS:
            self.phase("build-" + target, self.build, target)
        targets = [self.phase("package-" + target, self.package, target) for target in TARGETS]
        self.phase("sign", self.sign, targets)
        return self.record("built", targets)


def _owned_by_root(info):
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    return info


# Command line -------------------------------------------------------------------------------

class Parser(argparse.ArgumentParser):
    def error(self, message):
        self.print_usage(sys.stderr)
        sys.stderr.write("nightly: refused: %s\n" % message)
        sys.exit(EXIT_REFUSED)


def parse(argv):
    parser = Parser(prog="build.py", description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", parser_class=Parser)
    commands.required = True
    for name, text in (("plan", "choose the commit and print the plan; build nothing"),
                       ("build", "build, package, smoke and sign the plan's commit")):
        command = commands.add_parser(name, help=text)
        command.add_argument("--config", required=True, help="the af.nightly-config/1 TOML")
        command.add_argument("--date", required=True, help="the night, YYYY-MM-DD")
        command.add_argument("--previous", help="the previous night's nightly.json, built or "
                             "skipped, instead of the newest one under builds_root")
        if name == "build":
            command.add_argument("--only-plan", action="store_true",
                                 help="plan and exit as build would (0, or 3 when skipped), "
                                 "writing nothing")
    return parser.parse_args(argv)


def emit(doc):
    sys.stdout.write(json.dumps(doc, indent=2, sort_keys=True) + "\n")
    sys.stdout.flush()


def main(argv=None, github=None, host=None, clock=now_ms, environ=None):
    args = parse(argv)
    environ = os.environ if environ is None else environ
    try:
        night_date(args.date, "--date")
        config = load_config(args.config)
        git = Mirror(config["mirror"])
        if github is None:
            github = GitHub(config["repository"], read_token(config))
        started = clock()
        previous = previous_night(config["builds_root"], args.date, args.previous)
        plan = make_plan(config, args.date, git, github, previous)
        skipped = plan["status"] == "skipped"
        if args.command == "plan":
            emit(plan)
            return EXIT_BUILT
        if args.only_plan:
            emit(plan)
            return EXIT_SKIPPED if skipped else EXIT_BUILT
        night = Night(config, dict((key, value) for key, value in plan.items()
                                   if key not in ("schema", "status")),
                      host or Host(), clock, environ)
        night.timings["plan"] = {"started_unix_ms": started, "ended_unix_ms": clock()}
        if skipped:
            emit(night.skip())
            sys.stderr.write("nightly: skipped: %s\n" % plan["skip_reason"])
            return EXIT_SKIPPED
        emit(night.run_build(git))
        return EXIT_BUILT
    except (Refused, ContractError) as error:
        sys.stderr.write("nightly: refused: %s\n" % error)
        return EXIT_REFUSED


if __name__ == "__main__":
    sys.exit(main())
