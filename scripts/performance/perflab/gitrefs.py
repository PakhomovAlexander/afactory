"""Read-only Git access: resolve selectors once to full SHAs and apply the baseline rules.

Only plumbing that reads objects and refs runs here; nothing checks out, builds or executes
a revision. A missing parent, an ambiguous or absent release tag and an unresolved selector
are explicit unavailable results, never a more convenient baseline.
"""
import hashlib
import os
import re
import subprocess

from .values import ContractError, canonical_digest, require

SEMVER = re.compile(r"^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
                    r"(?:-((?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
                    r"(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*))?$")
SELECTOR = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/@^~{}-]{0,254}$")
MAX_BLOB_BYTES = 16 * 1024 * 1024


class Unavailable(ContractError):
    """A selector or baseline cannot be resolved; the result is unavailable, not substituted."""

    def __init__(self, reason, detail):
        ContractError.__init__(self, detail)
        self.reason = reason


class Git:
    """`budget`, when given (`perflab.budget.Budget`), bounds every Git command by the run's
    remaining deadline and refuses one that has no time left (ADR-0171)."""

    def __init__(self, repo, budget=None):
        self.repo = os.path.abspath(repo)
        self.budget = budget

    def _run(self, args, check=True, stdin=None):
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LC_ALL": "C",
               "GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_NOSYSTEM": "1",
               "GIT_CONFIG_GLOBAL": os.devnull, "HOME": os.devnull,
               "GIT_OPTIONAL_LOCKS": "0"}
        step = "git " + " ".join(args[:1])
        timeout = self.budget.seconds(step) if self.budget is not None else None
        try:
            result = subprocess.run(["git", "-C", self.repo] + args, input=stdin,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env,
                                    timeout=timeout)
        except subprocess.TimeoutExpired:
            raise self.budget.expired(step, timeout)
        if check and result.returncode != 0:
            raise ContractError("git %s failed: %s" % (" ".join(args[:2]),
                                                       result.stderr.decode("utf-8", "replace")
                                                       .strip()))
        return result

    def resolve(self, selector, what):
        """Resolve a selector to one full commit SHA, or report it unavailable."""
        require(isinstance(selector, str) and SELECTOR.match(selector) is not None
                and ".." not in selector,
                "%s: selector %r is not an allowed revision spelling" % (what, selector))
        result = self._run(["rev-parse", "--verify", "--quiet", "--end-of-options",
                            selector + "^{commit}"], check=False)
        if result.returncode != 0:
            raise Unavailable("unresolved_ref", "%s: %s does not resolve to a commit"
                              % (what, selector))
        sha = result.stdout.decode("ascii").strip()
        require(re.match(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$", sha) is not None,
                "%s: git returned an unexpected object ID" % what)
        return sha

    def exact_ref(self, refname, what):
        """The commit the full ref `refname` names, read exactly as spelled (`show-ref
        --verify`, no search through refs/, refs/tags/ or refs/heads/), or None when no ref of
        that name exists."""
        require(isinstance(refname, str) and refname.startswith("refs/")
                and SELECTOR.match(refname) is not None and ".." not in refname,
                "%s: %r is not a full ref name" % (what, refname))
        result = self._run(["show-ref", "--verify", "--hash", "--", refname], check=False)
        if result.returncode != 0:
            return None
        return self.resolve(result.stdout.decode("ascii").strip(), what)

    def parents(self, sha):
        out = self._run(["rev-list", "--parents", "-n", "1", sha, "--"]).stdout.decode("ascii")
        fields = out.split()
        require(fields and fields[0] == sha, "git rev-list returned another commit")
        return fields[1:]

    def is_ancestor(self, ancestor, descendant):
        result = self._run(["merge-base", "--is-ancestor", ancestor, descendant], check=False)
        if result.returncode not in (0, 1):
            raise ContractError("git merge-base --is-ancestor failed")
        return result.returncode == 0

    def merge_base(self, left, right):
        result = self._run(["merge-base", left, right], check=False)
        if result.returncode != 0:
            raise Unavailable("no_merge_base", "%s and %s share no merge base" % (left, right))
        return result.stdout.decode("ascii").strip()

    def tags(self, merged_into=None):
        """Every tag that peels to a commit, with that commit: [(name, sha)]."""
        args = ["for-each-ref", "--format=%(refname:strip=2)%00%(objectname)%00%(objecttype)"
                "%00%(*objectname)%00%(*objecttype)"]
        if merged_into:
            args.append("--merged=" + merged_into)
        args.append("refs/tags")
        out = []
        for line in self._run(args).stdout.decode("utf-8", "replace").splitlines():
            name, obj, kind, peeled, peeled_kind = line.split("\x00")
            if peeled:
                obj, kind = peeled, peeled_kind
            if kind == "commit":
                out.append((name, obj))
        return out

    def tree_entries(self, sha, prefixes):
        """(mode, type, object, path) for every entry under `prefixes` at commit `sha`."""
        out = self._run(["ls-tree", "-r", "-z", "--full-tree", sha, "--"] + list(prefixes))
        entries = []
        for item in out.stdout.split(b"\x00"):
            if not item:
                continue
            meta, path = item.split(b"\t", 1)
            mode, kind, obj = meta.decode("ascii").split()
            entries.append((mode, kind, obj, path.decode("utf-8")))
        return entries

    def blob(self, obj):
        return self.blobs([obj])[obj]

    def blobs(self, objects, bytes_max=None):
        """Read many blobs through one `git cat-file --batch`."""
        wanted = sorted(set(objects))
        if not wanted:
            return {}
        if bytes_max is not None:
            sizes = self._run(["cat-file", "--batch-check"],
                              stdin=("\n".join(wanted) + "\n").encode("ascii")).stdout
            total = 0
            lines = sizes.decode("ascii").splitlines()
            require(len(lines) == len(wanted), "git cat-file returned incomplete sizes")
            for obj, line in zip(wanted, lines):
                fields = line.split()
                require(len(fields) == 3 and fields[:2] == [obj, "blob"],
                        "git cat-file returned an unexpected size object")
                size = int(fields[2])
                require(0 <= size <= MAX_BLOB_BYTES, "a protected file exceeds 16 MiB")
                total += size
                require(total <= bytes_max, "the authority exceeds its byte bound")
        out = self._run(["cat-file", "--batch"],
                        stdin=("\n".join(wanted) + "\n").encode("ascii")).stdout
        found = {}
        offset = 0
        for obj in wanted:
            end = out.index(b"\n", offset)
            header = out[offset:end].decode("ascii").split()
            require(len(header) == 3 and header[0] == obj and header[1] == "blob",
                    "git cat-file returned an unexpected object for " + obj)
            size = int(header[2])
            require(size <= MAX_BLOB_BYTES, "a protected file exceeds 16 MiB")
            found[obj] = out[end + 1:end + 1 + size]
            offset = end + 1 + size + 1
        return found

    def read_file(self, sha, path):
        """Bytes of `path` at commit `sha`, or None when absent; symlinks are refused."""
        entries = [entry for entry in self.tree_entries(sha, [path]) if entry[3] == path]
        if not entries:
            return None
        mode, kind, obj, _ = entries[0]
        require(kind == "blob" and mode in ("100644", "100755"),
                "%s at %s is not a regular file" % (path, sha[:12]))
        return self.blob(obj)

    def manifest_digest(self, sha, prefixes, exclude=()):
        """Digest of a sorted path/size/SHA256 manifest of the files under `prefixes`."""
        selected = []
        for mode, kind, obj, path in self.tree_entries(sha, prefixes):
            if any(path == item or path.startswith(item.rstrip("/") + "/") for item in exclude):
                continue
            require(kind == "blob" and mode in ("100644", "100755"),
                    "protected path %s at %s is not a regular file" % (path, sha[:12]))
            selected.append((path, mode, obj))
        contents = self.blobs([obj for _, _, obj in selected])
        entries = [{"path": path, "size": len(contents[obj]), "mode": mode,
                    "sha256": "sha256:" + hashlib.sha256(contents[obj]).hexdigest()}
                   for path, mode, obj in sorted(selected)]
        return canonical_digest({"schema": "af.perf-tree-manifest/1", "entries": entries})

    def changed_paths(self, base, head, prefixes):
        out = self._run(["diff", "--name-only", "-z", "--no-renames", base, head, "--"]
                        + list(prefixes))
        return sorted(set(item.decode("utf-8") for item in out.stdout.split(b"\x00") if item))


# Semantic versions --------------------------------------------------------------------------

def parse_version(tag):
    match = SEMVER.match(tag)
    if match is None:
        return None
    major, minor, patch, pre = match.groups()
    pre_key = ()
    if pre:
        pre_key = tuple((0, int(part), "") if part.isdigit() else (1, 0, part)
                        for part in pre.split("."))
    return (int(major), int(minor), int(patch)), pre_key


def version_key(version):
    """Total semver precedence: a prerelease sorts before its release."""
    core, pre = version
    return (core, 0 if pre else 1, pre)


def _unique_by_version(tags):
    by_version = {}
    for name, sha in tags:
        version = parse_version(name)
        if version is None:
            continue
        by_version.setdefault(version_key(version), []).append((name, sha, version))
    return by_version


def _choose(entries, what):
    shas = sorted(set(sha for _, sha, _ in entries))
    if len(shas) != 1:
        raise Unavailable("ambiguous_release_tag", "%s: tags %s name the same version on "
                          "different commits" % (what, ", ".join(sorted(n for n, _, _ in entries))))
    return sorted(entries)[0]


def nightly_baseline(git, main_sha):
    """The highest stable semver tag reachable from frozen main."""
    by_version = _unique_by_version(git.tags(merged_into=main_sha))
    stable = [key for key, entries in by_version.items() if not entries[0][2][1]]
    if not stable:
        raise Unavailable("no_release_baseline", "no stable release tag is reachable from "
                          + main_sha)
    name, sha, _ = _choose(by_version[max(stable)], "nightly baseline")
    return name, sha


def release_baseline(git, tag):
    """The highest smaller stable semver; prerelease predecessors only for a prerelease."""
    version = parse_version(tag)
    if version is None:
        raise Unavailable("unsupported_baseline", "release candidate %s is not a semver tag"
                          % tag)
    by_version = _unique_by_version(git.tags())
    own = by_version.get(version_key(version), [])
    if not any(name == tag for name, _, _ in own):
        raise Unavailable("unresolved_ref", "release tag %s does not exist" % tag)
    tag_sha = _choose(own, "release candidate")[1]
    prerelease = bool(version[1])
    eligible = [key for key, entries in by_version.items()
                if key < version_key(version) and (prerelease or not entries[0][2][1])]
    if not eligible:
        raise Unavailable("no_release_baseline", "no eligible release precedes " + tag)
    name, sha, _ = _choose(by_version[max(eligible)], "release baseline")
    return tag_sha, prerelease, name, sha


def first_parent(git, sha, what):
    parents = git.parents(sha)
    if not parents:
        raise Unavailable("missing_parent", "%s %s has no parent" % (what, sha))
    return parents[0]
