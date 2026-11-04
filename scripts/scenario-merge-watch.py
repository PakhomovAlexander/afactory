#!/usr/bin/env python3
"""Bounded local coordinator for a manually published PR. No publication or shell hooks."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time

CONFIG = "af/merge-watch-config@1"
STATUS = "af/merge-watch-status@1"
SHA = r"[0-9a-f]{40}"
REPO = r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_][A-Za-z0-9_.-]{0,99}"
BRANCH = r"[A-Za-z0-9][A-Za-z0-9_./-]{0,199}"
TERMINAL = {"DONE", "FAILED"}
LIMIT = 1048576
STATUSES = ["STARTING", "WAITING_FOR_GITHUB", "WAITING_FOR_CI", "WAITING_FOR_HUMAN_MERGE", "CLEANING", "DONE", "FAILED"]


class Refused(Exception):
    """A fixed diagnostic, without remote response or credential bytes."""


class StopRequested(BaseException):
    """Stop a driver without converting retained progress into a terminal conclusion."""


def stop_requested(_signal, _frame):
    raise StopRequested()


class Unavailable(Refused):
    """A bounded command failed; an observation may be retried within the original budget."""


def need(condition, message):
    if not condition:
        raise Refused(message)


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def decode(raw):
    need(len(raw) <= LIMIT, "document exceeds byte limit")

    def unique(pairs):
        value = {}
        for key, item in pairs:
            need(key not in value, "duplicate document key")
            value[key] = item
        return value

    try:
        return json.loads(raw, object_pairs_hook=unique,
                          parse_constant=lambda _: need(False, "nonfinite number"))
    except (ValueError, UnicodeError, RecursionError) as exc:
        raise Refused("invalid JSON") from exc


def read(path):
    with path.open("rb") as stream:
        return decode(stream.read(LIMIT + 1))


def schema_documents():
    def obj(properties):
        return {"type": "object", "additionalProperties": False,
                "required": list(properties), "properties": properties}

    def text(pattern=None, maximum=1024):
        result = {"type": "string", "minLength": 1, "maxLength": maximum}
        if pattern:
            result["pattern"] = "^" + pattern + "$"
        return result

    def integer(low, high):
        return {"type": "integer", "minimum": low, "maximum": high}

    def array(item, maximum=16, minimum=0):
        return {"type": "array", "items": item, "minItems": minimum,
                "maxItems": maximum, "uniqueItems": True}

    path, sha = text(r"/.*"), text(SHA)
    branch = obj({"branch": text(BRANCH), "head": sha})
    worktree = obj({"path": path, **branch["properties"]})
    config = obj({"schema": {"const": CONFIG}, "repository": text(REPO),
                  "repository_id": integer(1, 9007199254740991), "pr": integer(1, 2147483647),
                  "head": sha, "local_repo": path, "state_dir": path,
                  "required_checks": array(text(maximum=200), minimum=1, maximum=32),
                  "worktrees": array(worktree), "branches": array(branch),
                  "poll_seconds": integer(1, 3600), "max_seconds": integer(1, 604800),
                  "max_polls": integer(1, 20000), "max_errors": integer(1, 8)})
    progress = obj({"kind": {"enum": ["worktree", "branch"]}, "target": text(),
                    "head": sha, "identity": text(maximum=4096),
                    "phase": {"enum": ["ready", "intent", "done"]}})
    state = obj({"schema": {"const": STATUS}, "config": config, "common_dir": path,
                 "deadline_unix": integer(1, 9007199254740991), "polls": integer(0, 20000),
                 "errors": integer(0, 8), "pid": integer(1, 2147483647),
                 "status": {"enum": STATUSES}, "message": text(maximum=200),
                 "merge_sha": {"type": "string", "pattern": "^(?:" + SHA + ")?$"},
                 "merge_kind": {"enum": ["", "ancestry", "exact_tree"]},
                 "cleanup": array(progress, maximum=32),
                 "checksum": text(r"[0-9a-f]{64}")})
    return {name + "-v1.json": {"$schema": "https://json-schema.org/draft/2020-12/schema",
                               "$id": "urn:af:schema:" + name + ":1", **value}
            for name, value in (("merge-watch-config", config), ("merge-watch-status", state))}


def validate(value, shape):
    """Validate precisely the JSON Schema subset above, with semantic checks at admission."""
    if "const" in shape:
        need(value == shape["const"], "unknown document version")
    if "enum" in shape:
        need(value in shape["enum"], "unknown state")
    kind = shape.get("type")
    if kind == "object":
        need(isinstance(value, dict) and set(value) == set(shape["required"]), "unexpected document fields")
        for key, item in value.items():
            validate(item, shape["properties"][key])
    elif kind == "array":
        need(isinstance(value, list) and shape["minItems"] <= len(value) <= shape["maxItems"], "invalid list bound")
        need(len({canonical(item) for item in value}) == len(value), "duplicate list item")
        for item in value:
            validate(item, shape["items"])
    elif kind == "integer":
        need(type(value) is int and shape["minimum"] <= value <= shape["maximum"], "invalid integer bound")
    elif kind == "string":
        need(isinstance(value, str) and shape.get("minLength", 0) <= len(value) <= shape.get("maxLength", LIMIT)
             and "\0" not in value, "invalid text bound")
        if "pattern" in shape:
            need(re.fullmatch(shape["pattern"], value) is not None, "invalid identity")


def is_sha(value):
    return isinstance(value, str) and re.fullmatch(SHA, value) is not None


def absolute(value):
    path = Path(value)
    need(path.is_absolute() and str(path.resolve()) == value, "paths must be canonical absolute paths without symlinks")
    return path


def outside_checkout(path):
    for parent in (path, *path.parents):
        need(not (parent / ".git").exists()
             and not ((parent / "HEAD").exists() and (parent / "objects").is_dir()),
             "watcher state must be outside every checkout and Git directory")


def private(path, directory=False):
    info = path.lstat()
    need((stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))
         and info.st_uid == os.getuid() and info.st_mode & 0o077 == 0,
         "config, state and lock must be private and owned by this user")


def load_config(path):
    absolute(str(path))
    outside_checkout(path)
    private(path)
    config = read(path)
    validate(config, schema_documents()["merge-watch-config-v1.json"])
    for key in ("state_dir", "local_repo"):
        absolute(config[key])
    outside_checkout(Path(config["state_dir"]))
    need(config["poll_seconds"] <= config["max_seconds"], "poll interval exceeds runtime")
    need(config["worktrees"] or config["branches"], "name at least one cleanup target")
    for key, field in (("worktrees", "path"), ("branches", "branch")):
        need(len({item[field] for item in config[key]}) == len(config[key]), "duplicate cleanup target")
    for item in config["worktrees"]:
        path = absolute(item["path"])
        repo = Path(config["local_repo"])
        need(path != repo and path not in repo.parents and repo not in path.parents,
             "cleanup must be outside the coordinator checkout")
        need(path not in Path(config["state_dir"]).parents, "state lies beneath cleanup target")
    return config


def command(argv, env, deadline, limit=LIMIT, allowed=(0,)):
    """Bound bytes, wall time and the whole child process group; never invoke a shell."""
    remaining = min(15, deadline - time.monotonic())
    need(remaining > 0, "runtime exhausted")
    stop = time.monotonic() + remaining
    with subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, start_new_session=True) as process:
        output = bytearray()
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    timeout = stop - time.monotonic()
                    if timeout <= 0:
                        raise Unavailable("command deadline exceeded")
                    for key, _ in selector.select(timeout):
                        data = os.read(key.fd, 65536)
                        if not data:
                            selector.unregister(key.fd)
                        output.extend(data)
                        need(len(output) <= limit, "command output exceeds byte limit")
                try:
                    code = process.wait(timeout=max(0.001, stop - time.monotonic()))
                except subprocess.TimeoutExpired as exc:
                    raise Unavailable("command deadline exceeded") from exc
            if code not in allowed:
                raise Unavailable("command failed; inspect local Git or gh authentication separately")
            return bytes(output), code
        finally:
            # A helper may have closed stdout but left descendants behind. Only this command's
            # new process group is signalled, never processes selected by name.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


class Watcher:
    def __init__(self, config):
        self.config = config
        self.state_dir = Path(config["state_dir"])
        self.status_path = self.state_dir / "status.json"
        self.deadline_unix = int(time.time()) + config["max_seconds"]
        self.deadline = time.monotonic() + max(0, self.deadline_unix - time.time())
        # Git plumbing must not consult ambient alternate repositories or execute local hooks,
        # fsmonitor or pager commands. gh retains the owner's existing authentication only.
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null", GIT_NO_REPLACE_OBJECTS="1",
                        GIT_TERMINAL_PROMPT="0", GIT_OPTIONAL_LOCKS="0", GH_PROMPT_DISABLED="1", GH_PAGER="cat")
        self.git_options = ["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
                            "-c", "core.untrackedCache=false", "-c", "core.attributesFile=/dev/null"]
        self.common = absolute(self.git("rev-parse", "--path-format=absolute", "--git-common-dir").decode().strip())
        # Disable every configured filter before any Git cleanliness check. Unknown filters in
        # attributes have no driver and are inert. Plumbing and our raw scan run no filters.
        self.disable_filters()
        self.state = None

    def disable_filters(self, repo=None):
        # Include chains and per-worktree config can also declare a filter driver. Read the
        # effective configuration of the target before a Git status may inspect its files.
        raw, _ = self.git_result("config", "--includes", "--name-only", "--get-regexp", r"^filter\.",
                                 repo=repo, allowed=(0, 1))
        for key in raw.decode().splitlines():
            name = key.rsplit(".", 1)[0]
            for suffix, value in (("clean", ""), ("smudge", ""), ("process", ""), ("required", "false")):
                option = name + "." + suffix + "=" + value
                if option not in self.git_options:
                    self.git_options.extend(["-c", option])

    def git_result(self, *args, repo=None, allowed=(0,)):
        return command(["git", *self.git_options, "-C", repo or self.config["local_repo"], *args],
                       self.env, self.deadline, allowed=allowed)

    def git(self, *args, repo=None):
        return self.git_result(*args, repo=repo)[0]

    def gh(self, endpoint):
        raw, _ = command(["gh", "api", "--hostname", "github.com", "--method", "GET",
                          "repos/" + self.config["repository"] + endpoint], self.env, self.deadline)
        return decode(raw)

    def worktrees(self):
        raw = self.git("worktree", "list", "--porcelain", "-z")
        result = {}
        for block in raw.split(b"\0\0"):
            fields = dict(item.split(b" ", 1) if b" " in item else (item, b"")
                          for item in block.split(b"\0") if item)
            if fields:
                result[os.fsdecode(fields[b"worktree"])] = fields
        return result

    def ref(self, branch):
        need(re.fullmatch(BRANCH, branch) is not None, "invalid branch")
        self.git("check-ref-format", "refs/heads/" + branch)
        raw, code = self.git_result("rev-parse", "--verify", "--quiet", "refs/heads/" + branch, allowed=(0, 1))
        return raw.decode().strip() if code == 0 else None

    def ancestor(self, head):
        _, code = self.git_result("merge-base", "--is-ancestor", head, self.config["head"], allowed=(0, 1))
        need(code == 0, "cleanup tip is not an ancestor of the exact PR head")

    def identity(self, item):
        path = Path(item["target"])
        info = path.lstat()
        need(stat.S_ISDIR(info.st_mode), "worktree was replaced")
        marker = path / ".git"
        need(marker.is_file() and not marker.is_symlink(), "only linked worktrees may be removed")
        need(marker.stat().st_size < 4096, "linked worktree marker exceeds byte limit")
        marker_bytes = marker.read_bytes()
        need(len(marker_bytes) < 4096 and marker_bytes.startswith(b"gitdir: "), "invalid linked worktree marker")
        admin = absolute(os.fsdecode(marker_bytes[8:].strip()))
        need(admin.parent == self.common / "worktrees", "worktree belongs to another repository")
        admin_info = admin.lstat()
        need(stat.S_ISDIR(admin_info.st_mode), "invalid worktree entry")
        backlink = admin / "gitdir"
        need(backlink.stat().st_size < 4096 and not backlink.is_symlink(), "invalid worktree backlink")
        need(backlink.read_text().strip() == str(marker), "worktree backlink mismatch")
        return f"{info.st_dev}:{info.st_ino}:{admin_info.st_dev}:{admin_info.st_ino}:{digest(marker_bytes.hex())}"

    def clean(self, item):
        """Compare index and raw files/modes with the commit; include ignored/untracked files."""
        root = Path(item["target"])
        raw = self.git("ls-tree", "-rz", "--full-tree", item["head"])
        expected, directories = {}, set()
        for row in raw.split(b"\0"):
            if not row:
                continue
            metadata, name = row.split(b"\t", 1)
            mode, kind, sha = metadata.split()
            need(kind == b"blob" and mode in (b"100644", b"100755", b"120000"), "submodule or unsupported tree entry")
            path = os.fsdecode(name)
            need("events.sqlite" not in Path(path).parts, "audit Store found in cleanup tree")
            expected[path] = (mode, sha)
            directories.update(str(parent) for parent in Path(path).parents if str(parent) != ".")
        index = b"".join(mode + b" " + sha + b" 0\t" + os.fsencode(name) + b"\0"
                         for name, (mode, sha) in expected.items())
        need(self.git("ls-files", "--stage", "-z", repo=str(root)) == index, "dirty worktree index")
        seen, count, total = set(), 0, 0
        pending = [(root, "")]
        while pending:
            directory, prefix = pending.pop()
            for path in directory.iterdir():
                need(time.monotonic() < self.deadline, "runtime exhausted during cleanup scan")
                count += 1
                need(count <= 20000, "cleanup scan entry limit exceeded")
                name = prefix + path.name
                if name == ".git":
                    continue
                info = path.lstat()
                if stat.S_ISDIR(info.st_mode):
                    need(name in directories, "untracked or ignored directory in cleanup worktree")
                    pending.append((path, name + "/"))
                    continue
                need(name in expected, "untracked or ignored file in cleanup worktree")
                mode, sha = expected[name]
                if mode == b"120000":
                    need(stat.S_ISLNK(info.st_mode), "worktree file type changed")
                    data = os.fsencode(os.readlink(path))
                    actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
                else:
                    need(stat.S_ISREG(info.st_mode) and bool(info.st_mode & 0o111) == (mode == b"100755"),
                         "worktree file mode changed")
                    total += info.st_size
                    need(total <= 268435456, "cleanup scan byte limit exceeded")
                    hasher = hashlib.sha1(b"blob " + str(info.st_size).encode() + b"\0")
                    with path.open("rb") as stream:
                        while data := stream.read(65536):
                            need(time.monotonic() < self.deadline, "runtime exhausted during cleanup scan")
                            hasher.update(data)
                    actual = hasher.hexdigest()
                need(actual.encode() == sha, "dirty worktree content")
                seen.add(name)
        need(seen == set(expected), "worktree file missing")

    def check_worktree(self, item, identity=True):
        entries = self.worktrees()
        entry = entries.get(item["target"])
        need(entry and entry.get(b"HEAD", b"").decode() == item["head"]
             and b"locked" not in entry and b"prunable" not in entry, "worktree head changed, locked or missing")
        config = next(c for c in self.config["worktrees"] if c["path"] == item["target"])
        need(entry.get(b"branch") == ("refs/heads/" + config["branch"]).encode(), "worktree branch changed")
        fingerprint = self.identity(item)
        if identity:
            need(fingerprint == item["identity"], "worktree identity changed")
        self.clean(item)
        return fingerprint

    def save(self, status=None, message=None):
        if status:
            need(self.state["status"] not in TERMINAL, "terminal state is immutable")
            self.state.update(status=status, message=message)
        self.state["checksum"] = digest({k: v for k, v in self.state.items() if k != "checksum"})
        raw = canonical(self.state)
        need(len(raw) <= LIMIT, "status exceeds byte limit")
        with tempfile.NamedTemporaryFile(dir=self.state_dir, prefix=".status-", delete=False) as stream:
            temporary = Path(stream.name)
            try:
                stream.write(raw)
                stream.flush()
                os.fsync(stream.fileno())
                os.replace(temporary, self.status_path)
                fd = os.open(self.state_dir, os.O_RDONLY)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
            finally:
                temporary.unlink(missing_ok=True)

    def admit(self):
        if self.status_path.exists():
            private(self.status_path)
            state = read(self.status_path)
            validate(state, schema_documents()["merge-watch-status-v1.json"])
            need(state["checksum"] == digest({k: v for k, v in state.items() if k != "checksum"}), "corrupt watcher status")
            need(state["config"] == self.config and state["common_dir"] == str(self.common), "watcher binding changed")
            need(state["polls"] <= self.config["max_polls"] and state["errors"] <= self.config["max_errors"], "corrupt budgets")
            expected = [("worktree", c["path"], c["head"]) for c in self.config["worktrees"]]
            expected += [("branch", c["branch"], c["head"]) for c in self.config["branches"]]
            need([(c["kind"], c["target"], c["head"]) for c in state["cleanup"]] == expected, "corrupt cleanup bindings")
            need(state["status"] != "DONE" or (all(c["phase"] == "done" for c in state["cleanup"])
                 and state["merge_sha"] and state["merge_kind"]), "DONE lacks merge or cleanup evidence")
            need(bool(state["merge_sha"]) == bool(state["merge_kind"]), "incomplete retained merge evidence")
            need(state["status"] not in ("CLEANING", "DONE") or state["merge_sha"], "cleanup lacks merge evidence")
            need(not any(c["phase"] != "ready" for c in state["cleanup"]) or state["merge_sha"],
                 "cleanup progress lacks merge evidence")
            self.state = state
            self.deadline = time.monotonic() + min(self.config["max_seconds"], state["deadline_unix"] - time.time())
            if state["status"] not in TERMINAL:
                state["pid"] = os.getpid()
                self.save()
            return
        cleanup = []
        for c in self.config["worktrees"]:
            item = dict(kind="worktree", target=c["path"], head=c["head"], identity="pending", phase="ready")
            self.ancestor(c["head"])
            item["identity"] = self.check_worktree(item, identity=False)
            cleanup.append(item)
        for c in self.config["branches"]:
            need(self.ref(c["branch"]) == c["head"], "cleanup branch tip changed or missing")
            self.ancestor(c["head"])
            cleanup.append(dict(kind="branch", target=c["branch"], head=c["head"], identity=c["head"], phase="ready"))
        self.state = dict(schema=STATUS, config=self.config, common_dir=str(self.common),
                          deadline_unix=self.deadline_unix, polls=0, errors=0,
                          pid=os.getpid(), status="STARTING", message="Watcher admitted; CI and human merge still required",
                          merge_sha="", merge_kind="", cleanup=cleanup, checksum="0" * 64)
        self.save()

    def pr(self):
        pr = self.gh("/pulls/" + str(self.config["pr"]))
        need(isinstance(pr, dict) and pr.get("number") == self.config["pr"], "PR identity mismatch")
        for side in ("base", "head"):
            need(isinstance(pr.get(side), dict), "incomplete PR repository evidence")
            repo = pr[side].get("repo")
            need(isinstance(repo, dict), "incomplete PR repository evidence")
            need(repo.get("id") == self.config["repository_id"] and repo.get("full_name") == self.config["repository"],
                 "PR repository mismatch; fork heads are not supported")
        need(pr["head"].get("sha") == self.config["head"], "PR head changed")
        need(pr.get("state") in ("open", "closed") and type(pr.get("merged")) is bool, "incomplete PR lifecycle")
        need(pr["state"] != "closed" or pr["merged"], "PR closed without merge")
        if pr["merged"]:
            need(pr["state"] == "closed" and isinstance(pr.get("merged_at"), str) and pr["merged_at"]
                 and is_sha(pr.get("merge_commit_sha")), "incomplete merge evidence")
        return pr

    def green(self):
        # Commit statuses/check-runs are addressed by the exact pinned head, never a branch.
        checks = self.gh("/commits/" + self.config["head"] + "/check-runs?per_page=100")
        need(isinstance(checks, dict) and type(checks.get("total_count")) is int
             and isinstance(checks.get("check_runs"), list)
             and checks["total_count"] == len(checks["check_runs"]) <= 100, "incomplete or oversized CI evidence")
        rows = checks["check_runs"]
        need(all(isinstance(row, dict) for row in rows), "invalid CI evidence")
        # Refuse ambiguous duplicate names instead of guessing which app/attempt is authoritative.
        for name in self.config["required_checks"]:
            matches = [r for r in rows if r.get("name") == name]
            if len(matches) != 1:
                return False
            check = matches[0]
            need(check.get("head_sha") == self.config["head"], "CI head mismatch")
            if check.get("status") != "completed" or check.get("conclusion") != "success":
                return False
        return True

    def merged(self, pr):
        sha = pr["merge_commit_sha"]
        commit = self.gh("/git/commits/" + sha)
        need(isinstance(commit, dict) and commit.get("sha") == sha, "merge commit identity mismatch")
        need(isinstance(commit.get("tree"), dict), "incomplete merge tree")
        tree = commit["tree"].get("sha")
        parents = commit.get("parents")
        need(is_sha(tree) and isinstance(parents, list) and len(parents) <= 2
             and all(isinstance(p, dict) and is_sha(p.get("sha")) for p in parents),
             "incomplete merge commit")
        local_tree = self.git("rev-parse", self.config["head"] + "^{tree}").decode().strip()
        if sha == self.config["head"] or self.config["head"] in [p["sha"] for p in parents]:
            kind = "ancestry"
        else:
            need(len(parents) == 1 and tree == local_tree, "merge ancestry or squash tree mismatch")
            kind = "exact_tree"
        after = self.pr()
        need(after["merged"] and after["merge_commit_sha"] == sha, "merge evidence changed")
        if self.state["merge_sha"]:
            need(self.state["merge_sha"] == sha and self.state["merge_kind"] == kind, "retained merge evidence changed")
        self.state.update(merge_sha=sha, merge_kind=kind)
        self.save("CLEANING", "Human merge verified; DONE requires every cleanup target to be absent")

    def absent(self, item):
        if item["kind"] == "worktree":
            return not os.path.lexists(item["target"]) and item["target"] not in self.worktrees()
        return self.ref(item["target"]) is None

    def cleanup(self):
        for item in self.state["cleanup"]:
            if item["phase"] == "done":
                need(self.absent(item), "a completed cleanup target was recreated")
                continue
            if item["phase"] == "intent":
                # A crash may have completed the deletion. Positive absence reconciles it;
                # an existing target is uncertain, never permission to repeat a deletion.
                need(self.absent(item), "uncertain cleanup intent; preserve targets and inspect status")
            else:
                if item["kind"] == "worktree":
                    self.check_worktree(item)
                    self.disable_filters(repo=item["target"])
                else:
                    need(self.ref(item["target"]) == item["head"], "cleanup branch tip changed")
                    need(not any(e.get(b"branch") == ("refs/heads/" + item["target"]).encode()
                                 for e in self.worktrees().values()), "cleanup branch is checked out")
                item["phase"] = "intent"
                self.save()
                if item["kind"] == "worktree":
                    self.git("worktree", "remove", "--", item["target"])
                else:
                    self.git("update-ref", "-d", "refs/heads/" + item["target"], item["head"])
                need(self.absent(item), "cleanup target still exists")
            item["phase"] = "done"
            self.save()
        need(all(self.absent(item) for item in self.state["cleanup"]), "cleanup incomplete")
        self.save("DONE", "DONE: exact PR merged; every configured worktree and local branch cleanup verified; Task Stores retained")

    def run(self):
        if self.state["status"] in TERMINAL:
            return
        self.state["pid"] = os.getpid()
        while True:
            try:
                need(time.monotonic() < self.deadline, "watcher runtime exhausted")
                need(self.state["polls"] < self.config["max_polls"], "watcher poll budget exhausted")
                self.state["polls"] += 1
                self.save()
                pr = self.pr()
                if not pr["merged"]:
                    need(not self.state["merge_sha"], "merged PR became unmerged")
                # Merge evidence never substitutes for successful exact-head required checks,
                # including on entry and when resuming interrupted cleanup.
                green = self.green()
                after = pr if pr["merged"] else self.pr()
                if after["merged"] and green:
                    self.merged(after)
                    self.cleanup()
                    return
                self.save("WAITING_FOR_HUMAN_MERGE" if green else "WAITING_FOR_CI",
                          "CI green; watcher remains active for the human merge" if green else "Waiting for exact-head required CI checks")
            except Unavailable as exc:
                self.state["errors"] += 1
                if self.state["status"] == "CLEANING" or self.state["errors"] >= self.config["max_errors"]:
                    self.save("FAILED", str(exc))
                    return
                self.save("WAITING_FOR_GITHUB", "Observation unavailable; retrying within original budgets")
            except (Refused, OSError) as exc:
                self.save("FAILED", str(exc) if isinstance(exc, Refused) else "Local operation failed; inspect retained status and targets")
                return
            time.sleep(max(0, min(self.config["poll_seconds"], self.deadline - time.monotonic())))


def locked(path):
    # Never unlink lock files: their stable inode is the concurrency boundary.
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    stream = os.fdopen(fd, "r+")
    try:
        private(path)
        fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except (OSError, Refused) as exc:
        stream.close()
        raise Refused("concurrent watcher refused or private lock unavailable") from exc
    return stream


def background(config_path, state_dir):
    ready, notify = os.pipe()
    try:
        process = subprocess.Popen([sys.executable, "-I", str(Path(__file__).resolve()), "--config", str(config_path),
                                    "--ready-fd", str(notify)], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True, pass_fds=(notify,))
        os.close(notify)
        notify = None
        with selectors.DefaultSelector() as selector:
            selector.register(ready, selectors.EVENT_READ)
            need(selector.select(30), "background watcher startup deadline exceeded")
            need(os.read(ready, 1) == b"r", "background watcher refused startup; run foreground for diagnostic")
        return read(state_dir / "status.json")
    except BaseException:
        if 'process' in locals() and process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=5)
        raise
    finally:
        os.close(ready)
        if notify is not None:
            os.close(notify)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True, help="owner-only exact PR/cleanup JSON configuration")
    parser.add_argument("--background", action="store_true", help="detach, return after admission; status.json is caller output")
    parser.add_argument("--ready-fd", type=int, help=argparse.SUPPRESS)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, stop_requested)
    signal.signal(signal.SIGINT, stop_requested)
    try:
        config = load_config(args.config)
        directory = Path(config["state_dir"])
        if args.background:
            need(args.ready_fd is None, "invalid startup arguments")
            state = background(args.config.resolve(), directory)
        else:
            directory.mkdir(mode=0o700, parents=True, exist_ok=True)
            private(directory, directory=True)
            with locked(directory / "watch.lock"):
                watcher = Watcher(config)
                # This second lock prevents duplicate processes using different status directories
                # for the same local repository and PR, including a different pinned head.
                lock_path = watcher.common / f"af-merge-watch-{config['repository_id']}-{config['pr']}.lock"
                with locked(lock_path):
                    watcher.admit()
                    if args.ready_fd is not None:
                        os.write(args.ready_fd, b"r")
                        os.close(args.ready_fd)
                    watcher.run()
                    state = watcher.state
        sys.stdout.buffer.write(canonical(state))
        return 1 if state["status"] == "FAILED" else 0
    except StopRequested:
        print("Watcher stopped; resume with the same configuration and original budgets", file=sys.stderr)
        return 130
    except (Refused, OSError) as exc:
        print(str(exc) if isinstance(exc, Refused) else "Local watcher setup failed", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
