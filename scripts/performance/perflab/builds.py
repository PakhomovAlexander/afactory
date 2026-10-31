"""Build a frozen plan's products and its one trusted supervisor (package p3a, ADR-0166).

`Builder` resolves no selector again: it builds exactly the plan's resolved SHAs. It creates a
private bare repository below the owning checkout's `target/performance/<run>/git`, fetches the
three commits into it from the checkout, and adds detached worktrees `worktrees/authority`,
`worktrees/base` and `worktrees/head`, each building into its own `target/`. The private
repository's configuration is checked to carry no HTTP or credential setting, so no Git
authentication header the checkout persisted reaches a build script; HOME, TMPDIR and
CARGO_HOME are run-private, PATH holds the verified toolchain and the system directories, and
every other variable is dropped. Dependencies are fetched once by `cargo fetch --locked` into
the run's Cargo home (or an operator's pre-fetched one) before any build script runs, and every
build is `--locked --offline` with an explicit `--target-dir`.

The supervisor is built once, from the authority worktree. Each product is built once from its
own worktree; when the baseline and candidate are one commit (every frozen input identical),
the candidate references the baseline's artifacts. After the product, the registry's adapters
are tried in priority order: the authority's closure files are copied over the measured source,
read-only, the probe example is compiled, and a failure restores the source before the next
adapter. Only a compile decides; no probe is run here. Before and after every compile attempt
each overlaid file must still be exactly the authority's bytes, type and mode, or the build
refuses (`overlay_integrity`, ADR-0168): a measured build script cannot change the instrument.
The musl shipping recipe builds through exactly its pinned cargo-zigbuild and Zig. The AF binary must embed the 12-character prefix of
its own source commit (crates/af/build.rs reads it through the worktree's `.git` file), and the
source and Cargo.lock must be exactly as checked out apart from the overlay.

A Linux recipe builds in a pinned build image (ADR-0169): the image the plan froze (ADR-0170),
the recipe's own `container_image` or one configured when the plan was frozen, pinned by digest
in a canonical form and already present locally (`--pull never`). A build never chooses
another: `--build-image` only restates the frozen reference, and anything else is refused
before anything is created. Every compile then runs in its own named container of that image as this
user, without network, with a read-only root and no capability, and with only the run's own
directories mounted at their host paths: the step's worktree, the private repository
(read-only), the run's HOME, TMPDIR and Cargo home, and each overlaid authority closure file
bound read-only over its overlay. The toolchain, cargo-zigbuild and Zig are the image's,
verified there by version. Dependencies are prepared once by `cargo fetch --locked` in a
container of the same image with network and nothing but the run's directories, or come from
an operator's pre-fetched Cargo home; no credential, Git configuration or provider state of the
host is mounted or passed. Each container's removal is confirmed. A Linux recipe without a
pinned image builds on the host only when its plan froze an explicitly trusted host build,
recorded as a trusted host process that claims no filesystem isolation; nothing falls back from
one to the other.

The build is the run's first execution (ADR-0170): it records the run's one execution start and
its absolute total deadline, the start plus the plan's frozen `run_total_minutes`, in
`builds.json`, where every shard reads it. A caller's deadline only tightens what the build
itself may use, and no step runs past the earlier of the two.

Deadlines, the worktree byte budget, the free-disk floor, the artifact budget and each step's
log cap are enforced while building. A built output is admitted as a confined regular file below
its own worktree, never through a symlinked file or directory, within what remains of the
artifact budget and the run's deadline, and an AF binary without its own commit is never
published (ADR-0172). Artifacts are copied, read-only, into
`target/performance/<run>/build/artifacts` with a manifest, beside `builds.json` and the logs,
so shards consume them without rebuilding; `verify_artifacts` rehashes them before any launch.
Everything else of the run (repository, worktrees, private homes) is removed on every exit,
and the cleanup outcome is recorded; a failed build keeps its logs and its `builds.json`.

Each build records the compiled cache its measured pass built against (`af.perf-builds/6`,
ADR-0185), observed rather than requested: `--build-cache cold` (the default) requires every
worktree target to be observed empty; `--build-cache warm` first runs a bounded priming build of
the same sources, recipe, toolchain and protected inputs into the run's own worktree targets
(the supervisor in the authority, AF in each product source), under the same step, deadline and
disk budgets, records its steps and time apart, and requires every target primed before the
measured pass begins. No target is shared between runs or kept outside the run's directory, and
dependency preparation (`dependency_fetch`) is reported separately: it is never a build cache.
"""
import hashlib
import json
import os
import re
import secrets
import shutil
import signal
import stat
import subprocess
import threading
import time

from . import adapters as adapters_module
from . import admission, budget as budget_module, bundle, contracts, images, limits, tomlfile
from .values import ContractError, check_id, require, sha256_bytes

SYSTEM_PATH = "/usr/bin:/bin"
EMBEDDED_PREFIX = 12
POLL_SECONDS = 5
READ_CHUNK = 1 << 20
ARTIFACTS = "artifacts"
LOGS = "logs"
BUILDS_FILE = "builds.json"
PLAN_FILE = "run.json"
# The deepest nesting any well-formed identity field has: a list of strings.
IDENTITY_DEPTH_MAX = 1
JSON_STRING = re.compile(r'"(?:[^"\\]|\\.)*"')
EXECUTABLE = stat.S_IRUSR | stat.S_IXUSR | stat.S_IRGRP | stat.S_IXGRP


class Unavailable(ContractError):
    """The build cannot run as the plan requires on this host; nothing is substituted."""

    def __init__(self, reason, detail):
        ContractError.__init__(self, detail)
        self.reason = reason


def git_environment(home):
    """Git with no system or global configuration, no prompt and a private HOME."""
    return {"PATH": SYSTEM_PATH, "HOME": home, "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C",
            "GIT_OPTIONAL_LOCKS": "0"}


def _run(argv, env, cwd=None, check=True, timeout=None):
    result = subprocess.run(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    if check and result.returncode != 0:
        raise ContractError("%s failed: %s" % (" ".join(argv[:3]), result.stderr.decode(
            "utf-8", "replace").strip()[:2048]))
    return result


def _first_line(data):
    lines = data.decode("utf-8", "replace").strip().splitlines()
    return lines[0].strip()[:256] if lines else ""


# Toolchain ------------------------------------------------------------------------------------

class Toolchain:
    """The exact compiler of a recipe, by absolute path, with its verified identities."""

    def __init__(self, bin_dir, version):
        self.bin_dir = os.path.realpath(bin_dir)
        self.rustc = os.path.join(self.bin_dir, "rustc")
        self.cargo = os.path.join(self.bin_dir, "cargo")
        for tool in (self.rustc, self.cargo):
            if not os.access(tool, os.X_OK):
                raise Unavailable("toolchain_unavailable", "%s is not an executable" % tool)
        env = {"PATH": self.bin_dir + ":" + SYSTEM_PATH, "LC_ALL": "C"}
        verbose = _run([self.rustc, "-vV"], env, check=False)
        if verbose.returncode != 0:
            raise Unavailable("toolchain_unavailable", "%s -vV failed" % self.rustc)
        fields = dict(line.split(": ", 1) for line in verbose.stdout.decode(
            "utf-8", "replace").splitlines() if ": " in line)
        self.release = fields.get("release", "")
        self.host = fields.get("host", "")
        self.rustc_identity = _first_line(verbose.stdout)
        if self.release != version:
            raise Unavailable("toolchain_mismatch", "the recipe pins Rust %s, but %s is %s"
                              % (version, self.rustc, self.release or "unknown"))
        cargo = _run([self.cargo, "-V"], env, check=False)
        self.cargo_identity = _first_line(cargo.stdout)
        if cargo.returncode != 0 or not self.cargo_identity.startswith("cargo " + version):
            raise Unavailable("toolchain_mismatch", "%s is %s, not cargo %s"
                              % (self.cargo, self.cargo_identity or "unknown", version))
        require(self.host and self.rustc_identity, "%s -vV named no host" % self.rustc)

    @classmethod
    def discover(cls, version, bin_dir=None):
        """The recipe's toolchain: an explicit bin directory, or the sysroot rustup selects
        for exactly `version` (a rustc that ignores the selection is then checked too)."""
        if bin_dir is None:
            env = dict((key, value) for key, value in os.environ.items()
                       if key in ("PATH", "HOME", "RUSTUP_HOME"))
            env["RUSTUP_TOOLCHAIN"] = version
            try:
                sysroot = _run(["rustc", "--print", "sysroot"], env, check=False)
            except OSError as error:
                raise Unavailable("toolchain_unavailable", "no rustc on PATH: %s" % error)
            if sysroot.returncode != 0:
                raise Unavailable("toolchain_unavailable", "Rust %s is not installed: %s"
                                  % (version, _first_line(sysroot.stderr)))
            bin_dir = os.path.join(sysroot.stdout.decode("utf-8").strip(), "bin")
        return cls(bin_dir, version)

    def host_os(self):
        return "macos" if "apple-darwin" in self.host else (
            "linux" if "-linux-" in self.host else self.host)

    def supports_target(self, target):
        env = {"PATH": self.bin_dir + ":" + SYSTEM_PATH, "LC_ALL": "C"}
        result = _run([self.rustc, "--print", "target-libdir", "--target", target], env,
                      check=False)
        return result.returncode == 0 and os.path.isdir(result.stdout.decode("utf-8").strip())


class Zigbuild:
    """The shipping recipe's cargo-zigbuild and Zig, by absolute path, with verified versions.

    `bin_dir` holds executables `cargo-zigbuild` and `zig` (a wrapper of `python3 -m ziglang`
    qualifies when it reports the version). Each must report exactly the recipe's version;
    nothing is installed or substituted (ADR-0168)."""

    def __init__(self, bin_dir, cargo_zigbuild_version, zig_version):
        self.bin_dir = os.path.realpath(bin_dir)
        self.cargo_zigbuild = os.path.join(self.bin_dir, "cargo-zigbuild")
        self.zig = os.path.join(self.bin_dir, "zig")
        for tool in (self.cargo_zigbuild, self.zig):
            if not os.access(tool, os.X_OK):
                raise Unavailable("toolchain_unavailable", "%s is not an executable" % tool)
        env = {"PATH": self.bin_dir + ":" + SYSTEM_PATH, "LC_ALL": "C"}
        found = _first_line(_run([self.cargo_zigbuild, "--version"], env, check=False).stdout)
        if found != "cargo-zigbuild " + cargo_zigbuild_version:
            raise Unavailable("toolchain_mismatch", "the recipe pins cargo-zigbuild %s, but %s "
                              "reports %r" % (cargo_zigbuild_version, self.cargo_zigbuild,
                                              found))
        zig = _first_line(_run([self.zig, "version"], env, check=False).stdout)
        if zig != zig_version:
            raise Unavailable("toolchain_mismatch", "the recipe pins Zig %s, but %s reports %r"
                              % (zig_version, self.zig, zig))
        self.identity = "%s; zig %s" % (found, zig)


def linker_identity(kind, path):
    """The recipe linker's actual identity, or why it has none."""
    env = {"PATH": path, "LC_ALL": "C"}
    argv = {"system_cc": ["cc", "--version"], "apple_ld": ["ld", "-v"],
            "zig_cc": ["zig", "version"]}[kind]
    try:
        result = _run(argv, env, check=False)
    except OSError as error:
        return {"kind": kind, "identity": None, "unavailable": "%s: %s" % (argv[0], error)}
    line = _first_line(result.stdout) or _first_line(result.stderr)
    if result.returncode != 0 or not line:
        return {"kind": kind, "identity": None,
                "unavailable": "%s exited %d" % (" ".join(argv), result.returncode)}
    return {"kind": kind, "identity": line, "unavailable": None}


def ambient_cargo_configs(path):
    """Cargo configuration files Cargo would read from the ancestors of `path`.

    Returns (neutralized, refused): a file setting only `build.target-dir` is neutralized by
    the explicit `--target-dir` of every build; any other setting would change the build and
    refuses it."""
    neutralized, refused = [], []
    current = os.path.dirname(os.path.realpath(path))
    while True:
        for name in ("config.toml", "config"):
            candidate = os.path.join(current, ".cargo", name)
            if os.path.isfile(candidate):
                try:
                    with open(candidate, "rb") as stream:
                        doc = tomlfile.loads(stream.read(tomlfile.MAX_TOML_BYTES + 1),
                                             candidate)
                except ContractError:
                    refused.append(candidate)
                    continue
                only_target = set(doc) <= {"build"} and set(doc.get("build", {})) <= {
                    "target-dir"}
                (neutralized if only_target else refused).append(candidate)
        parent = os.path.dirname(current)
        if parent == current:
            return neutralized, refused
        current = parent


# Build images ---------------------------------------------------------------------------------

# The docker client's own settings; none of them reaches a build container.
DOCKER_CLIENT_KEYS = ("HOME", "DOCKER_HOST", "DOCKER_CONFIG", "DOCKER_CONTEXT",
                      "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY")
CONTAINER_PROBE_SECONDS = 120


def docker_client_environment():
    env = {"PATH": "/usr/local/bin:/opt/homebrew/bin:" + SYSTEM_PATH, "LC_ALL": "C"}
    for key in DOCKER_CLIENT_KEYS:
        if key in os.environ:
            env[key] = os.environ[key]
    return env


def pinned_image(reference):
    """Whether `reference` names an image by digest: `sha256:<id>` or `name@sha256:<digest>`."""
    name, at, digest = reference.rpartition("@")
    if not at:
        digest = reference
    elif not name:
        return False
    value = digest[len("sha256:"):]
    return (digest.startswith("sha256:") and len(value) == 64
            and all(c in "0123456789abcdef" for c in value))


def refuse_implicit_volumes(docker, reference, budget=None):
    """One pre-launch image-volume policy for builds, samples, seeds and readers."""
    seconds = (CONTAINER_PROBE_SECONDS if budget is None else
               budget.seconds("image volume inspection", CONTAINER_PROBE_SECONDS))
    try:
        result = _run([docker, "image", "inspect", "--format", "{{json .Config.Volumes}}",
                       reference], docker_client_environment(), timeout=seconds)
    except subprocess.TimeoutExpired:
        if budget is not None and budget.left() <= 0:
            raise budget.expired("image volume inspection", seconds)
        raise ContractError("image volume inspection exceeded its allowance")
    if budget is not None:
        budget.seconds("completing image volume inspection")
    require(len(result.stdout) <= limits.RECORD_BYTES_MAX,
            "image volume declaration exceeds its byte bound")
    try:
        volumes = json.loads(result.stdout)
    except ValueError:
        raise ContractError("image volume declaration is not JSON")
    require(volumes in (None, {}), "bounded build image declares implicit writable volumes")


def image_identity(stdout, reference, role):
    """The five fields of a build image's `docker image inspect` identity, each of its shape:
    the ID, OS and architecture as strings and the repository digests and environment as lists
    of strings or null. Anything else is a typed `build_image_malformed` refusal, raised before
    the caller creates any resource."""
    def malformed(why):
        return Unavailable("build_image_malformed", "docker image inspect gave a malformed "
                           "identity for the %s %s: %s" % (role, reference, why))
    if len(stdout) > limits.RECORD_BYTES_MAX:
        raise malformed("it exceeds its byte bound")
    try:
        fields = stdout.decode("utf-8").strip().split("\t")
    except UnicodeDecodeError:
        raise malformed("it is not UTF-8")
    if len(fields) != 5:
        raise malformed("%d fields, not 5" % len(fields))
    values = []
    for name, field in zip(("ID", "OS", "architecture", "repository digests", "environment"),
                           fields):
        # Bound the nesting before decoding, so no field below the byte bound can make the
        # decoder recurse past the interpreter's limit (a RecursionError, not a refusal).
        if _nesting(field) > IDENTITY_DEPTH_MAX:
            raise malformed("its %s nests deeper than %d" % (name, IDENTITY_DEPTH_MAX))
        try:
            values.append(json.loads(field))
        except (ValueError, RecursionError):
            raise malformed("its %s is not JSON" % name)
    for name, value in zip(("ID", "OS", "architecture"), values[:3]):
        if not isinstance(value, str) or not value:
            raise malformed("its %s is not a non-empty string" % name)
    for name, value in zip(("repository digests", "environment"), values[3:]):
        if value is not None and not (isinstance(value, list)
                                      and all(isinstance(item, str) for item in value)):
            raise malformed("its %s is not a list of strings" % name)
    return values


def _nesting(field):
    """The deepest array or object nesting of a JSON text, its strings' contents not counted."""
    depth = deepest = 0
    for bracket in re.sub(r"[^\[\]{}]", "", JSON_STRING.sub("", field)):
        depth += 1 if bracket in "[{" else -1
        deepest = max(deepest, depth)
    return deepest


class BuildImage:
    """A build image pinned by digest and present locally, run as this user."""

    def __init__(self, docker, reference, uid=None, gid=None, budget=None, role="build image"):
        self.budget = budget
        if not pinned_image(reference):
            raise Unavailable("build_image_unpinned", "the %s %s is not pinned by "
                              "digest" % (role, reference))
        try:
            images.canonical(reference)
        except ContractError as error:
            raise Unavailable("build_image_unpinned", str(error))
        self.docker = docker
        self.reference = reference
        self.uid = os.getuid() if uid is None else uid
        self.gid = os.getgid() if gid is None else gid
        try:
            result = _run([docker, "image", "inspect", "--format",
                           "{{json .Id}}\t{{json .Os}}\t{{json .Architecture}}\t"
                           "{{json .RepoDigests}}\t{{json .Config.Env}}", reference],
                          docker_client_environment(), check=False,
                          timeout=self.seconds("image inspection", CONTAINER_PROBE_SECONDS))
        except OSError as error:
            raise Unavailable("no_container_runtime", "%s: %s" % (docker, error))
        if result.returncode != 0:
            raise Unavailable("build_image_unavailable", "the %s %s is not present "
                              "on this host (builds never pull): %s" % (
                                  role, reference, _first_line(result.stderr)))
        self.id, image_os, arch, repo_digests, image_env = image_identity(
            result.stdout, reference, role)
        if reference.startswith("sha256:"):
            require(self.id == reference, "the %s ID is %s, not %s" % (role, self.id,
                                                                       reference))
        else:
            # A manifest digest names the image only under exactly its repository name; it is
            # never compared with the local image ID (ADR-0170).
            if reference not in (repo_digests or []):
                raise Unavailable("build_image_mismatch", "the local image %s is not %s: its "
                                  "repository digests are %s" % (
                                      self.id, reference, ", ".join(repo_digests or [])
                                      or "none"))
        if image_os != "linux":
            raise Unavailable("build_image_unsupported", "the %s %s is %s, not Linux"
                              % (role, reference, image_os))
        self.platform = "linux/%s" % arch
        self.env = dict(item.split("=", 1) for item in image_env or [] if "=" in item)

    def record(self):
        return {"reference": self.reference, "id": self.id, "platform": self.platform}

    def seconds(self, step, maximum):
        return maximum if self.budget is None else self.budget.seconds(step, maximum)

    def refuse_implicit_volumes(self):
        """A bounded build must not acquire writable anonymous image volumes."""
        refuse_implicit_volumes(self.docker, self.reference, self.budget)


    def command(self, name, argv, cwd, env, mounts, network=False):
        """`docker run` of `argv` in a new named container of this image. The program starts
        through `env -i` with exactly `env`: whatever the image itself declares is dropped."""
        command = [self.docker, "run", "--name", name, "--pull", "never",
                   "--network", "bridge" if network else "none", "--read-only",
                   "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                   "--user", "%d:%d" % (self.uid, self.gid), "--workdir", cwd]
        for source, target, writable in mounts:
            command += ["--mount", "type=bind,src=%s,dst=%s%s" % (
                source, target, "" if writable else ",readonly")]
        return command + ["--entrypoint", "/usr/bin/env", self.reference, "-i"] \
            + ["%s=%s" % item for item in sorted(env.items())] + list(argv)

    def remove(self, name):
        """Remove a named container and confirm it is gone; None, or why it is not."""
        env = docker_client_environment()
        try:
            subprocess.run([self.docker, "rm", "--force", name], stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL,
                           timeout=self.seconds("container removal", 60), env=env)
            listed = subprocess.run([self.docker, "ps", "--all", "--quiet", "--filter",
                                     "name=^/%s$" % name], stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL,
                                    timeout=self.seconds("confirming container removal", 60), env=env)
        except (OSError, subprocess.TimeoutExpired) as error:
            return "build container %s removal unconfirmed: %s" % (name, error)
        if listed.returncode != 0 or listed.stdout.strip():
            return "build container %s removal unconfirmed" % name
        return None

    def run_once(self, argv, env=None):
        """Run one probe of the image's own tools (no mount, no network); the result."""
        name = "af-perf-probe-%s" % secrets.token_hex(6)
        command = self.command(name, argv, "/", env or {}, [])
        try:
            return subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, env=docker_client_environment(),
                                  timeout=self.seconds("toolchain probe", CONTAINER_PROBE_SECONDS))
        except (OSError, subprocess.TimeoutExpired) as error:
            raise Unavailable("build_image_unavailable", "%s in %s: %s"
                              % (argv[0], self.reference, error))
        finally:
            problem = self.remove(name)
            if problem is not None:
                raise Unavailable("build_container_unconfirmed", problem)


class ContainerToolchain:
    """The recipe's exact compiler inside a pinned build image, by its absolute path there,
    with identities verified by running it there."""

    def __init__(self, image, version):
        self.image = image
        # The image's own rustup proxy selects exactly the recipe's toolchain; every build
        # step then runs that toolchain's binaries directly.
        env = {"PATH": image.env.get("PATH", SYSTEM_PATH), "LC_ALL": "C",
               "RUSTUP_TOOLCHAIN": version}
        for key in ("RUSTUP_HOME", "CARGO_HOME"):
            # The image's own rustup installation; no build step sees either.
            if key in image.env:
                env[key] = image.env[key]
        sysroot = image.run_once(["rustc", "--print", "sysroot"], env)
        if sysroot.returncode != 0:
            raise Unavailable("toolchain_unavailable", "Rust %s is not in the build image %s: "
                              "%s" % (version, image.reference, _first_line(sysroot.stderr)))
        self.bin_dir = os.path.join(sysroot.stdout.decode("utf-8").strip(), "bin")
        self.rustc = os.path.join(self.bin_dir, "rustc")
        self.cargo = os.path.join(self.bin_dir, "cargo")
        # Cargo needs a home even to name its version; every build step gets the run's own.
        env = {"PATH": self.bin_dir + ":" + SYSTEM_PATH, "LC_ALL": "C",
               "CARGO_HOME": image.env.get("CARGO_HOME", "/nonexistent-cargo-home")}
        verbose = image.run_once([self.rustc, "-vV"], env)
        fields = dict(line.split(": ", 1) for line in verbose.stdout.decode(
            "utf-8", "replace").splitlines() if ": " in line)
        self.release = fields.get("release", "")
        self.host = fields.get("host", "")
        self.rustc_identity = _first_line(verbose.stdout)
        if verbose.returncode != 0 or self.release != version:
            raise Unavailable("toolchain_mismatch", "the recipe pins Rust %s, but %s in %s is %s"
                              % (version, self.rustc, image.reference,
                                 self.release or "unknown"))
        cargo = image.run_once([self.cargo, "-V"], env)
        self.cargo_identity = _first_line(cargo.stdout)
        if cargo.returncode != 0 or not self.cargo_identity.startswith("cargo " + version):
            raise Unavailable("toolchain_mismatch", "%s in %s is %s, not cargo %s"
                              % (self.cargo, image.reference, self.cargo_identity or "unknown",
                                 version))
        require(self.host and self.rustc_identity, "%s -vV named no host" % self.rustc)

    def host_os(self):
        return "linux" if "-linux-" in self.host else self.host

    def supports_target(self, target):
        env = {"PATH": self.bin_dir + ":" + SYSTEM_PATH, "LC_ALL": "C"}
        result = self.image.run_once(["/bin/sh", "-c", 'test -d "$(%s --print target-libdir '
                                      '--target %s)"' % (self.rustc, target)], env)
        return result.returncode == 0


class ContainerZigbuild:
    """The shipping recipe's cargo-zigbuild and Zig inside the build image, by the directory
    holding them there (`--zigbuild-bin`): its `cargo-zigbuild`, and its `zig`, or, as
    cargo-zigbuild itself falls back to, its `python3 -m ziglang`. Each must report exactly the
    recipe's version."""

    def __init__(self, image, bin_dir, cargo_zigbuild_version, zig_version):
        require(os.path.isabs(bin_dir), "--zigbuild-bin: a directory inside the build image")
        self.bin_dir = bin_dir
        self.cargo_zigbuild = os.path.join(bin_dir, "cargo-zigbuild")
        env = {"PATH": bin_dir + ":" + SYSTEM_PATH, "LC_ALL": "C"}
        found = _first_line(image.run_once([self.cargo_zigbuild, "--version"], env).stdout)
        if found != "cargo-zigbuild " + cargo_zigbuild_version:
            raise Unavailable("toolchain_mismatch", "the recipe pins cargo-zigbuild %s, but %s "
                              "in %s reports %r" % (cargo_zigbuild_version, self.cargo_zigbuild,
                                                    image.reference, found))
        zig = image.run_once(["/bin/sh", "-c", 'if [ -x "$1/zig" ]; then echo zig; "$1/zig" '
                              'version; else echo "python3 -m ziglang"; "$1/python3" -m ziglang '
                              'version; fi', "sh", bin_dir], env)
        lines = zig.stdout.decode("utf-8", "replace").strip().splitlines()
        if zig.returncode != 0 or len(lines) != 2 or lines[1].strip() != zig_version:
            raise Unavailable("toolchain_mismatch", "the recipe pins Zig %s, but %s in %s "
                              "reports %r" % (zig_version, bin_dir, image.reference,
                                              lines[1:] or _first_line(zig.stderr)))
        self.zig_command = lines[0].strip()
        self.identity = "%s; zig %s (%s)" % (found, zig_version, self.zig_command)


class ContainerSteps:
    """Runs each build step in its own container of the pinned build image."""

    def __init__(self, image, layout, run_id, bounded=False):
        self.image = image
        self.layout = layout
        self.bounded = bounded
        self.prefix = "af-perf-build-%s" % run_id[-24:].lower().replace("_", "-")

    def wrap(self, argv, cwd, env, network, readonly_files):
        """(docker argv, client environment, cleanup) of one step."""
        name = "%s-%s" % (self.prefix, secrets.token_hex(4))
        layout = self.layout
        mounts = [(cwd, cwd, True), (layout.git, layout.git, False),
                  (layout.home, layout.home, True), (layout.tmp, layout.tmp, True),
                  (layout.cargo_home, layout.cargo_home, True)]
        mounts += [(source, target, False) for source, target in readonly_files]
        command = self.image.command(name, argv, cwd, env, mounts, network)
        if self.bounded:
            bound_container(command)
        return command, docker_client_environment(), lambda: self.image.remove(name)


def bound_container(command):
    """Shared build-container resource ceiling; candidate code owns no daemon log or IPC."""
    command[2:2] = ["--memory", "8g", "--memory-swap", "8g", "--pids-limit", "512",
                    "--ipc", "none", "--log-driver", "none",
                    "--ulimit", "fsize=8589934592:8589934592"]


# Layout ---------------------------------------------------------------------------------------

class Layout:
    """`<checkout>/target/performance/<run>`: everything one run builds, never reused."""

    def __init__(self, owner, run_id, cargo_home=None):
        check_id(run_id, "run_id")
        self.owner = os.path.realpath(owner)
        require(os.path.isdir(self.owner), "%s is not a checkout directory" % owner)
        self.run = os.path.join(self.owner, "target", "performance", run_id)
        self.git = os.path.join(self.run, "git")
        self.worktrees = os.path.join(self.run, "worktrees")
        self.home = os.path.join(self.run, "home")
        self.tmp = os.path.join(self.run, "tmp")
        self.private_cargo_home = cargo_home is None
        self.cargo_home = (os.path.join(self.run, "cargo-home") if cargo_home is None
                           else os.path.realpath(cargo_home))
        self.build = os.path.join(self.run, "build")
        self.overlays = os.path.join(self.run, "overlays")
        self.artifacts = os.path.join(self.build, ARTIFACTS)
        self.logs = os.path.join(self.build, LOGS)

    def worktree(self, role):
        return os.path.join(self.worktrees, role)

    def create(self):
        require(not os.path.lexists(self.run), "%s already exists; a run directory is never "
                "reused" % self.run)
        os.makedirs(os.path.dirname(self.run), exist_ok=True)
        os.mkdir(self.run, 0o700)
        for path in (self.worktrees, self.home, self.tmp, self.build, self.artifacts,
                     self.logs, self.overlays):
            os.mkdir(path, 0o700)
        if self.private_cargo_home:
            os.mkdir(self.cargo_home, 0o700)

    def scratch(self):
        """Every run-owned path removed at the end; the build directory is evidence."""
        paths = [self.git, self.worktrees, self.home, self.tmp, self.overlays]
        if self.private_cargo_home:
            paths.append(self.cargo_home)
        return paths

    def recorded(self, value):
        """A machine-local path spelled relative to the run, the checkout or the toolchain."""
        for prefix, name in ((self.run, "{run}"), (self.owner, "{checkout}"),
                             (self.cargo_home, "{cargo_home}")):
            value = value.replace(prefix, name)
        return value


def tree_bytes(paths):
    """Bytes of every file below `paths`, by lstat; nothing is followed."""
    total = 0
    for root in paths:
        for current, _, files in os.walk(root):
            for name in files:
                try:
                    total += os.lstat(os.path.join(current, name)).st_size
                except OSError:
                    continue
    return total


def file_sha256(path):
    digest = hashlib.sha256()
    size = 0
    with open(path, "rb") as stream:
        while True:
            chunk = stream.read(READ_CHUNK)
            if not chunk:
                return "sha256:" + digest.hexdigest(), size
            digest.update(chunk)
            size += len(chunk)


BUILD_CACHE_TREATMENTS = ("cold", "warm")


# Steps ----------------------------------------------------------------------------------------

class Budget:
    """The plan's build limits, checked while every step runs."""

    def __init__(self, layout, plan_limits, log_bytes, free_bytes=None):
        self.layout = layout
        self.free_bytes = free_bytes or (lambda path: shutil.disk_usage(path).free)
        self.worktree_limit = plan_limits["worktree_bytes"]
        self.artifact_limit = plan_limits["build_artifact_bytes"]
        self.free_floor = plan_limits["free_disk_floor_bytes"]
        self.deadline_seconds = plan_limits["build_minutes"] * 60
        # The earlier of the run's total deadline and a caller's, as a time.monotonic() instant;
        # no step runs past it (ADR-0170).
        self.run_deadline = None
        self.log_bytes = log_bytes
        self.peak = 0
        self.whole_run = False

    def exceeded(self):
        """Why the run is over budget now, or None."""
        used = tree_bytes([self.layout.run] if self.whole_run else
                          [self.layout.worktrees, self.layout.git])
        self.peak = max(self.peak, used)
        if used > self.worktree_limit:
            return "worktrees and targets hold %d bytes, over the %d-byte budget" % (
                used, self.worktree_limit)
        free = self.free_bytes(self.layout.run)
        if free < self.free_floor:
            return "%d bytes are free, below the %d-byte floor" % (free, self.free_floor)
        return None


class Steps:
    """Run build commands in their own session with a deadline, a budget and a capped log; in
    a build container of the pinned image when `container` is given (ADR-0169)."""

    def __init__(self, layout, budget, container=None):
        self.layout = layout
        self.budget = budget
        self.container = container

    def run(self, name, argv, cwd, env, network=False, readonly_files=()):
        """Run one step; `network` lets only a container dependency fetch reach the network,
        and `readonly_files` are (authority copy, path) pairs bound read-only over the path."""
        log_path = os.path.join(self.layout.logs, name + ".log")
        record = {"argv": [self.layout.recorded(arg) for arg in argv],
                  "environment": dict((key, self.layout.recorded(value))
                                      for key, value in sorted(env.items())),
                  "deadline_seconds": self.budget.deadline_seconds}
        launch, launch_env, cleanup = argv, env, None
        if self.container is not None:
            launch, launch_env, cleanup = self.container.wrap(argv, cwd, env, network,
                                                              readonly_files)
        try:
            return self._run(record, launch, cwd, launch_env, log_path)
        finally:
            problem = cleanup() if cleanup is not None else None
            if problem is not None:
                raise Unavailable("build_container_unconfirmed", problem)

    def _run(self, record, argv, cwd, env, log_path):
        started = time.monotonic()
        run_deadline = self.budget.run_deadline
        if run_deadline is not None and started >= run_deadline:
            with open(log_path, "wb") as stream:
                stream.write(b"not started: the run's total deadline has passed\n")
            return self._finish(record, "timeout", None, started, log_path, 0)
        preallocated = getattr(self.budget, "preallocate_logs", False)
        if preallocated:
            with open(log_path, "xb") as stream:
                os.posix_fallocate(stream.fileno(), 0, self.budget.log_bytes)
        try:
            child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                     start_new_session=True)
        except OSError as error:
            with open(log_path, "wb") as stream:
                stream.write(("cannot start %s: %s\n" % (argv[0], error)).encode("utf-8"))
            return self._finish(record, "failed", None, started, log_path, 0)
        counted, log_errors = [0], []
        def drain():
            try:
                _drain_log(child.stdout, log_path, self.budget.log_bytes, counted, preallocated)
            except BaseException as error:
                log_errors.append(str(error)[:1024])
            finally:
                child.stdout.close()
        reader = threading.Thread(target=drain, daemon=True)
        reader.start()
        status = None
        deadline = started + self.budget.deadline_seconds
        if run_deadline is not None:
            deadline = min(deadline, run_deadline)
        try:
            while True:
                try:
                    child.wait(timeout=max(0.0, min(POLL_SECONDS, deadline - time.monotonic())))
                    break
                except subprocess.TimeoutExpired:
                    pass
                reason = self.budget.exceeded()
                if reason is not None or time.monotonic() >= deadline:
                    status = "budget_exceeded" if reason is not None else "timeout"
                    _kill_session(child)
                    child.wait()
                    break
        except BaseException:
            # A deadline timer can interrupt any wait or budget observation. The container
            # wrapper still confirms removal in run()'s finally; also settle its host client.
            _kill_session(child)
            child.wait(timeout=2)
            reader.join(2)
            raise
        reader.join(10)
        if reader.is_alive():
            # A process the build left behind still holds the log pipe; it is a member of the
            # build's session group, which therefore still exists and is ended now.
            _kill_session(child)
            reader.join(10)
            status = status or "failed"
        if log_errors:
            status = "failed"
        if status is None:
            status = "succeeded" if child.returncode == 0 else "failed"
            reason = self.budget.exceeded()
            if reason is not None:
                status = "budget_exceeded"
        code = child.returncode if child.returncode is not None and child.returncode >= 0 \
            else None
        return self._finish(record, status, code, started, log_path, counted[0])

    def _finish(self, record, status, code, started, log_path, total):
        digest, retained = file_sha256(log_path)
        record.update(status=status, exit_code=code,
                      elapsed_ms=int((time.monotonic() - started) * 1000),
                      log={"sha256": digest, "bytes": retained,
                           "truncated_bytes": max(0, total - retained)})
        return record


def _drain_log(pipe, path, cap, counted, preallocated=False):
    with open(path, "r+b" if preallocated else "wb") as stream:
        while True:
            chunk = pipe.read(64 * 1024)
            if not chunk:
                break
            room = cap - counted[0]
            if room > 0:
                stream.write(chunk[:room])
            counted[0] += len(chunk)
        if preallocated:
            stream.truncate(min(counted[0], cap))
    pipe.close()


def _kill_session(child):
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


# Repository and worktrees ---------------------------------------------------------------------

CREDENTIAL_KEYS = ("http.", "https.", "credential.", "url.", "core.askpass", "core.sshcommand",
                   "include.", "includeif.")


def private_repository(layout, shas):
    """A bare repository holding exactly the plan's commits, with a credential-free config."""
    env = git_environment(layout.home)
    _run(["git", "init", "--bare", "--quiet", layout.git], env)
    for sha in sorted(set(shas)):
        _run(["git", "-c", "protocol.version=2", "--git-dir", layout.git, "fetch", "--no-tags",
              "--quiet", "--no-write-fetch-head", layout.owner, sha], env)
        _run(["git", "--git-dir", layout.git, "update-ref", "refs/perf/" + sha, sha], env)
    listing = _run(["git", "--git-dir", layout.git, "config", "--list", "--show-origin"], env)
    keys = [line.split("\t", 1)[-1].split("=", 1)[0].lower()
            for line in listing.stdout.decode("utf-8", "replace").splitlines()]
    leaked = sorted(key for key in keys if key.startswith(CREDENTIAL_KEYS))
    require(not leaked, "the private repository's configuration carries %s"
            % ", ".join(leaked))
    return True


def add_worktree(layout, role, sha):
    env = git_environment(layout.home)
    path = layout.worktree(role)
    _run(["git", "--git-dir", layout.git, "worktree", "add", "--detach", "--quiet", path, sha],
         env)
    head = _run(["git", "-C", path, "rev-parse", "--verify", "HEAD"], env)
    require(head.stdout.decode("ascii").strip() == sha,
            "worktree %s is not at the planned commit %s" % (role, sha))
    require(os.path.isfile(os.path.join(path, ".git")),
            "worktree %s has no .git indirection file" % role)
    return path


def changed_paths(layout, path):
    """Tracked and untracked changes in a worktree, outside its target directory."""
    env = git_environment(layout.home)
    out = _run(["git", "-C", path, "status", "--porcelain=v1", "-z", "--untracked-files=all",
                "--", ".", ":(exclude)target"], env).stdout
    changed = set()
    for item in out.split(b"\x00"):
        if len(item) > 3:
            changed.add(item[3:].decode("utf-8", "replace"))
    return changed


def lock_digest(path):
    lock = os.path.join(path, "Cargo.lock")
    if not os.path.isfile(lock) or os.path.islink(lock):
        return None
    with open(lock, "rb") as stream:
        return sha256_bytes(stream.read())


# An operator's Cargo home is reached by every build: it may hold fetched dependencies, never
# credentials or a configuration that changes the build (ADR-0169).
CARGO_HOME_AUTHORITY = ("credentials", "credentials.toml", "config", "config.toml")


# Overlay --------------------------------------------------------------------------------------

# The mode of every overlaid closure file while it is compiled: read-only, so a build script
# that rewrites it must first change its mode, which the integrity check sees too.
OVERLAY_MODE = 0o444


class OverlayIntegrity(Unavailable):
    """An overlaid authority closure file was not exactly the authority's bytes, type and mode
    before or after a compile attempt. This is never a compile outcome: no other adapter is
    tried and nothing built from that source is published."""

    def __init__(self, detail):
        Unavailable.__init__(self, "overlay_integrity", detail)


class Overlay:
    """Authority closure files written over a measured source, restorable exactly.

    `verify` compares every overlaid file with the authority bytes the overlay record hashes:
    a regular file, not a symlink, of exactly mode `OVERLAY_MODE`, holding exactly those bytes.
    It runs before and after every compile attempt (ADR-0168)."""

    def __init__(self, root, files):
        self.root = root
        self.files = files
        self.saved = []
        self.created = []

    def apply(self):
        for path, data in self.files:
            full = os.path.join(self.root, path)
            parent = os.path.dirname(full)
            missing = []
            while not os.path.lexists(parent):
                missing.append(parent)
                parent = os.path.dirname(parent)
            for directory in reversed(missing):
                os.mkdir(directory)
                self.created.append(directory)
            if os.path.lexists(full):
                info = os.lstat(full)
                require(stat.S_ISREG(info.st_mode),
                        "%s in the measured source is not a regular file" % path)
                with open(full, "rb") as stream:
                    self.saved.append((full, stream.read(), stat.S_IMODE(info.st_mode)))
                os.unlink(full)
            else:
                self.saved.append((full, None, None))
            descriptor = os.open(full, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                 0o600)
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(data)
            os.chmod(full, OVERLAY_MODE)

    def problems(self):
        """Every way the overlaid files differ from the authority closure now; empty when each
        is exactly the authority's bytes as a read-only regular file."""
        found = []
        for path, data in self.files:
            full = os.path.join(self.root, path)
            try:
                info = os.lstat(full)
            except OSError as error:
                found.append("%s: %s" % (path, error.strerror or error))
                continue
            if not stat.S_ISREG(info.st_mode):
                found.append("%s is no longer a regular file" % path)
                continue
            if stat.S_IMODE(info.st_mode) != OVERLAY_MODE:
                found.append("%s has mode %o, not %o" % (path, stat.S_IMODE(info.st_mode),
                                                         OVERLAY_MODE))
            try:
                descriptor = os.open(full, os.O_RDONLY | os.O_NOFOLLOW)
            except OSError as error:
                found.append("%s: %s" % (path, error.strerror or error))
                continue
            with os.fdopen(descriptor, "rb") as stream:
                actual = stream.read(len(data) + 1)
            if actual != data:
                found.append("%s holds %s, not the authority's %s" % (
                    path, sha256_bytes(actual), sha256_bytes(data)))
        return found

    def verify(self, when):
        found = self.problems()
        if found:
            raise OverlayIntegrity("the authority probe closure overlaid on the measured source "
                                   "changed %s: %s" % (when, "; ".join(found)[:3072]))

    def restore(self):
        for full, data, mode in reversed(self.saved):
            if os.path.lexists(full):
                os.unlink(full)
            if data is not None:
                descriptor = os.open(full, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                     0o600)
                with os.fdopen(descriptor, "wb") as stream:
                    stream.write(data)
                os.chmod(full, mode)
        for directory in reversed(self.created):
            os.rmdir(directory)
        self.saved, self.created = [], []


# Builder --------------------------------------------------------------------------------------

class Builder:
    """Build one frozen plan; `build()` returns the validated builds.json document."""

    def __init__(self, owner, plan, toolchain, adapters_doc=None, cargo_home=None,
                 keep_worktrees=False, log_bytes=limits.BUILD_LOG_BYTES_DEFAULT,
                 free_bytes=None, zigbuild_bin=None, build_image=None, docker="docker",
                 deadline_unix_ms=None, wall=time.time, build_cache="cold",
                 bounded_container=False):
        self.plan = contracts.validate_run(plan)
        # By default the adapters are the authority commit's own registry, read from its
        # worktree; an explicit registry is for fixtures.
        self.adapters = (None if adapters_doc is None
                         else contracts.validate_adapters(adapters_doc))
        self.toolchain = toolchain
        self.layout = Layout(owner, plan["run_id"], cargo_home)
        self.keep = keep_worktrees
        self.recipe = plan["profile"]["recipe"]
        self.settings = contracts.recipe_settings(self.recipe)
        self.budget = Budget(self.layout, plan["limits"], log_bytes, free_bytes)
        self.bounded_container = bounded_container
        if bounded_container:
            require(cargo_home is None, "bounded builds require a private Cargo home")
            self.budget.whole_run = True
        self.steps = Steps(self.layout, self.budget)
        self.ambient = ([], [])
        self.zigbuild_bin = zigbuild_bin
        self.zigbuild = None
        # The build the plan froze (ADR-0170); `build_image` may only restate its image.
        self.frozen = plan["profile"]["build"]
        self.build_image = build_image
        self.docker = docker
        self.image = None
        self.deadline_unix_ms = deadline_unix_ms
        self.wall = wall
        self.execution = None
        require(build_cache in BUILD_CACHE_TREATMENTS, "--build-cache: cold or warm")
        # The compiled cache the measured pass builds against (ADR-0185): observed empty, or
        # primed by this run's own bounded build of the same sources.
        self.build_cache = build_cache
        # Logical bytes already published into the artifacts, within their budget.
        self.published = 0

    # Preconditions -----------------------------------------------------------------------

    def check_host(self):
        """Refuse, before anything is created, what this host cannot build as frozen."""
        recipe = self.recipe
        reference = self.image_reference()
        require(not self.bounded_container or reference is not None,
                "bounded builds require a pinned container")
        if reference is not None:
            self.image = BuildImage(self.docker, reference)
            if self.bounded_container:
                self.image.refuse_implicit_volumes()
            self.toolchain = ContainerToolchain(self.image, recipe["toolchain"])
            self.steps.container = ContainerSteps(self.image, self.layout,
                                                  self.plan["run_id"], self.bounded_container)
        require(self.toolchain is not None, "a host build verifies the recipe's toolchain first")
        if recipe["builder"] == "cargo_zigbuild":
            if self.zigbuild_bin is None:
                raise Unavailable("builder_unavailable", "the recipe builds with cargo-zigbuild "
                                  "%s and Zig %s; name their directory with --zigbuild-bin"
                                  % (recipe["cargo_zigbuild"], recipe["zig"]))
            if self.image is not None:
                self.zigbuild = ContainerZigbuild(self.image, self.zigbuild_bin,
                                                  recipe["cargo_zigbuild"], recipe["zig"])
            else:
                self.zigbuild = Zigbuild(self.zigbuild_bin, recipe["cargo_zigbuild"],
                                         recipe["zig"])
        elif recipe["builder"] != "cargo":
            raise Unavailable("builder_unavailable", "recipe builder %s is not available"
                              % recipe["builder"])
        if recipe["instrumented"] and self.plan["scope"] != "diagnostic":
            # A diagnostic recipe (ADR-0174) builds only a diagnostic child run, never a verdict
            # sample's binary.
            raise Unavailable("instrumented_recipe", "an instrumented recipe builds only a "
                              "diagnostic plan")
        if self.toolchain.host_os() != recipe["os"]:
            raise Unavailable("host_mismatch", "the recipe builds for %s, but the toolchain "
                              "host is %s" % (recipe["os"], self.toolchain.host))
        if not self.toolchain.supports_target(recipe["target"]):
            raise Unavailable("target_unavailable", "target %s is not installed for %s"
                              % (recipe["target"], self.toolchain.rustc_identity))
        if self.layout.private_cargo_home is False:
            require(os.path.isdir(self.layout.cargo_home),
                    "--cargo-home %s is not a directory" % self.layout.cargo_home)
            found = [name for name in CARGO_HOME_AUTHORITY
                     if os.path.lexists(os.path.join(self.layout.cargo_home, name))]
            if found:
                raise Unavailable("cargo_home_credentials", "--cargo-home %s holds %s; a build "
                                  "reaches the Cargo home, so it holds only fetched "
                                  "dependencies" % (self.layout.cargo_home, ", ".join(found)))
        if self.image is not None:
            # Nothing above the run's own directories is mounted into a build container.
            self.ambient = ([], [])
        else:
            self.ambient = ambient_cargo_configs(self.layout.run)
        if self.ambient[1]:
            raise Unavailable("ambient_cargo_config", "Cargo would read %s, which changes "
                              "builds beyond the explicit target directory"
                              % ", ".join(self.ambient[1]))
        free = self.budget.free_bytes(self.layout.owner)
        if free < self.plan["limits"]["free_disk_floor_bytes"]:
            raise Unavailable("disk_floor", "%d bytes are free, below the %d-byte floor"
                              % (free, self.plan["limits"]["free_disk_floor_bytes"]))

    def image_reference(self):
        """The pinned build image the plan froze, or None for its native or explicitly trusted
        host build; a configured image that is not exactly the frozen one refuses before
        anything is created (ADR-0170)."""
        frozen = self.frozen
        if self.build_image is not None and self.build_image != frozen["image"]:
            raise Unavailable("build_image_mismatch", "the plan froze %s; --build-image %s is "
                              "not it, and a build never chooses another image"
                              % ("build image " + frozen["image"] if frozen["image"]
                                 else "a %s build" % frozen["isolation"].replace("_", " "),
                                 self.build_image))
        return frozen["image"]

    def environment(self, network=False):
        path = self.toolchain.bin_dir + ":" + SYSTEM_PATH
        if self.zigbuild is not None:
            # cargo-zigbuild runs the recipe's cargo and Zig from PATH; only these two come first.
            path = self.zigbuild.bin_dir + ":" + path
        env = {"PATH": path, "HOME": self.layout.home,
               "TMPDIR": self.layout.tmp, "CARGO_HOME": self.layout.cargo_home,
               "RUSTC": self.toolchain.rustc, "LC_ALL": "C", "TZ": "UTC",
               "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
               "GIT_TERMINAL_PROMPT": "0", "CARGO_TERM_COLOR": "never"}
        if not network:
            env["CARGO_NET_OFFLINE"] = "true"
        flags = list(self.recipe["rustflags"])
        if self.recipe["frame_pointers"]:
            flags.append("-Cforce-frame-pointers=yes")
        if flags:
            env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
        if self.recipe["debug_symbols"]:
            env["CARGO_PROFILE_RELEASE_DEBUG"] = "true"
        return env

    def cargo(self, worktree, package, target_kind, name, features=True):
        command = ([self.zigbuild.cargo_zigbuild, "zigbuild"] if self.zigbuild is not None
                   else [self.toolchain.cargo, "build"])
        argv = command + ["--release", "--locked", "--offline",
                "--target-dir", os.path.join(worktree, "target"),
                "--target", self.recipe["target"], "-p", package, target_kind, name]
        wanted = list(self.recipe["features"]) if features else []
        if self.calibration_build() and package != "review-process":
            # Both sides of a preparation or slowdown member are one calibration build of the
            # same source; only the candidate's environment arms the loop (ADR-0181).
            wanted.append(contracts.CALIBRATION_FEATURE)
        if wanted:
            argv += ["--features", ",".join(wanted)]
        return argv

    def calibration_build(self):
        cohort = self.plan["cohort"]
        return cohort is not None and cohort["injection"] is not None

    def output_relative(self, kind, name):
        """Where a build leaves an output, relative to its worktree."""
        base = "target/%s/release" % self.recipe["target"]
        return "%s/examples/%s" % (base, name) if kind == "--example" else "%s/%s" % (base, name)

    # The build ----------------------------------------------------------------------------

    def start(self):
        """Establish the run's one execution start and total deadline (ADR-0170)."""
        started = int(self.wall() * 1000)
        total = self.plan["limits"]["run_total_minutes"]
        run = {"started_unix_ms": started, "deadline_unix_ms": started + total * 60000,
               "total_minutes": total}
        caller = self.deadline_unix_ms
        if caller is not None and caller <= started:
            raise Unavailable("run_deadline", "the caller's deadline %d has already passed"
                              % caller)
        deadline = contracts.effective_deadline(run["deadline_unix_ms"], caller)
        self.budget.run_deadline = time.monotonic() + (deadline - started) / 1000.0
        self.execution = {"run": run, "caller_deadline_unix_ms": caller,
                          "deadline_unix_ms": deadline, "ended_unix_ms": started}

    def build(self):
        self.start()
        self.check_host()
        layout = self.layout
        layout.create()
        try:
            doc = self._build()
        except BaseException:
            self._cleanup()
            raise
        doc["cleanup"] = self._cleanup()
        self.execution["ended_unix_ms"] = max(self.execution["run"]["started_unix_ms"],
                                              int(self.wall() * 1000))
        doc["execution"] = self.execution
        data = contracts.write_bytes(doc, "builds")
        with open(os.path.join(layout.build, BUILDS_FILE), "wb") as stream:
            stream.write(data)
        return doc

    def _build(self):
        layout, plan = self.layout, self.plan
        resolved = plan["resolved"]
        shared = resolved["baseline_sha"] == resolved["candidate_sha"]
        with open(os.path.join(layout.build, PLAN_FILE), "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        roles = [("authority", resolved["authority_sha"]), ("base", resolved["baseline_sha"])]
        if not shared:
            roles.append(("head", resolved["candidate_sha"]))
        credential_free = private_repository(layout, [sha for _, sha in roles])
        worktrees = dict((role, add_worktree(layout, role, sha)) for role, sha in roles)
        if self.adapters is None:
            path = os.path.join(worktrees["authority"], adapters_module.ADAPTERS_PATH)
            if not os.path.isfile(path):
                raise Unavailable("no_adapter_registry", "the authority %s has no %s"
                                  % (resolved["authority_sha"], adapters_module.ADAPTERS_PATH))
            self.adapters = adapters_module.load(path)
        adapters_module.check_closure(self.adapters, worktrees["authority"])
        input_roles = {"authority": "authority", "base": "baseline", "head": "candidate"}
        for role, path in worktrees.items():
            expected = plan["build_inputs"][input_roles[role]]["cargo_lock_sha256"]
            require(lock_digest(path) == expected, "the %s worktree's Cargo.lock is not the "
                    "planned %s" % (role, expected))
        if layout.private_cargo_home:
            for role, path in sorted(worktrees.items()):
                fetched = self.steps.run("fetch-" + role, [
                    self.toolchain.cargo, "fetch", "--locked", "--target",
                    self.recipe["target"]], path, self.environment(network=True), network=True)
                if fetched["status"] != "succeeded":
                    raise Unavailable("dependencies_unavailable", "cargo fetch --locked failed "
                                      "for the %s source; see build/logs/fetch-%s.log"
                                      % (role, role))

        cache = self._prime(worktrees)
        supervisor = self._supervisor(worktrees["authority"])
        baseline = self._side("baseline", "base", worktrees["base"], resolved["baseline_sha"],
                              worktrees["authority"])
        if shared:
            candidate = dict(baseline, worktree="head", shared_with="baseline")
        else:
            candidate = self._side("candidate", "head", worktrees["head"],
                                   resolved["candidate_sha"], worktrees["authority"])
        manifest = bundle.write_manifest(layout.artifacts, self.budget.artifact_limit) \
            if os.listdir(layout.artifacts) else None
        artifact_bytes = tree_bytes([layout.artifacts])
        require(artifact_bytes <= self.budget.artifact_limit,
                "build artifacts hold %d bytes, over the %d-byte budget"
                % (artifact_bytes, self.budget.artifact_limit))
        neutralized = self.ambient[0]
        if self.image is not None:
            detail = ("pinned build image %s (%s): each step a named container of this user "
                      "with a read-only root, no capability, no new privileges and no network "
                      "but the dependency fetch's; only the step's worktree, the private "
                      "repository (read-only), the run's HOME, TMPDIR and Cargo home and the "
                      "overlaid authority files (read-only) are mounted; allowlisted "
                      "environment, credential-free private repository, locked offline builds; "
                      "each container's removal confirmed"
                      % (self.image.reference, self.image.platform))
        else:
            detail = ("explicitly trusted host build, no filesystem isolation: allowlisted "
                      "environment, private HOME, TMPDIR and CARGO_HOME, credential-free "
                      "private repository, locked offline builds")
        if neutralized:
            detail += "; ambient Cargo configuration setting only build.target-dir, overridden " \
                      "by --target-dir: " + ", ".join(neutralized)
        if self.zigbuild is not None:
            detail += "; recipe builder " + self.zigbuild.identity
        linker_path = (self.zigbuild.bin_dir + ":" if self.zigbuild is not None else "") \
            + self.toolchain.bin_dir + ":" + SYSTEM_PATH
        fetch = ("prefetched" if not layout.private_cargo_home
                 else "container_network" if self.image is not None else "host_network")
        groups = dict((name, (side["chosen_adapter"], side["equivalence_group"]))
                      for name, side in (("baseline", baseline), ("candidate", candidate)))
        return {
            "schema": contracts.BUILDS_SCHEMA, "run_id": plan["run_id"],
            "plan_digest": plan["plan_digest"], "recipe_id": plan["profile"]["recipe_id"],
            "recipe_digest": plan["profile"]["recipe_digest"], "provenance": "measured",
            "execution": self.execution, "build_cache": cache,
            "isolation": {"kind": "container" if self.image is not None else "host_process",
                          "environment": "allowlist",
                          "credential_free_git": credential_free, "cargo_offline": True,
                          "image": self.image.record() if self.image is not None else None,
                          "compile_network": "none" if self.image is not None else "host",
                          "dependency_fetch": fetch,
                          "filesystem": ("run_mounts_only" if self.image is not None
                                         else "trusted_host"),
                          "detail": detail[:4096]},
            "toolchain": {"rustc": self.toolchain.rustc_identity,
                          "cargo": self.toolchain.cargo_identity, "host": self.toolchain.host},
            "linker": (self.container_linker() if self.image is not None
                       else linker_identity(self.recipe["linker"], linker_path)),
            "supervisor": supervisor,
            "sides": {"baseline": baseline, "candidate": candidate},
            "adapter_binding": {
                "baseline": groups["baseline"][0], "candidate": groups["candidate"][0],
                "baseline_group": groups["baseline"][1],
                "candidate_group": groups["candidate"][1],
                "equivalent": groups["baseline"][1] is not None
                and groups["baseline"][1] == groups["candidate"][1]},
            "artifacts": {"manifest_sha256": manifest, "bytes": artifact_bytes,
                          "limit_bytes": self.budget.artifact_limit},
            "budgets": {"worktree_limit_bytes": self.budget.worktree_limit,
                        "worktree_peak_bytes": self.budget.peak,
                        "build_log_limit_bytes": self.budget.log_bytes},
            "cleanup": {"status": "retained", "detail": "pending"},
        }

    def _prime(self, worktrees):
        """The run's compiled build cache, observed before the measured pass (ADR-0185). Warm,
        a bounded priming build of each worktree's own products (the supervisor in the
        authority, AF in each product source) fills that worktree's target first, under the
        run's step, time and disk budgets; cold, every target must be observed empty. The
        priming's time is recorded apart from the measured pass, which starts afterwards."""
        roles = [role for role in contracts.BUILD_CACHE_ROLES if role in worktrees]
        identity = contracts.canonical_digest({
            "plan_digest": self.plan["plan_digest"],
            "recipe_digest": self.plan["profile"]["recipe_digest"],
            "rustc": self.toolchain.rustc_identity, "cargo": self.toolchain.cargo_identity,
            "locks": dict((role, lock_digest(worktrees[role])) for role in roles)})
        priming = None
        if self.build_cache == "warm":
            started = int(self.wall() * 1000)
            steps, env = [], self.environment()
            for role in roles:
                argv = (self.cargo(worktrees[role], "review-process", "--example",
                                   "perf-supervisor", features=False) if role == "authority"
                        else self.cargo(worktrees[role], "af", "--bin", "af"))
                name = "prime_" + role
                step = self.steps.run(name, argv, worktrees[role], env)
                steps.append({"name": name, "step": step})
                if step["status"] != "succeeded":
                    raise Unavailable("priming_failed", "the warm cache's priming build of the "
                                      "%s worktree %s; see build/logs/%s.log"
                                      % (role, step["status"], name))
            priming = {"started_unix_ms": started, "ended_unix_ms": int(self.wall() * 1000),
                       "steps": steps}
        observed = []
        for role in roles:
            compiled = tree_bytes([os.path.join(worktrees[role], "target")])
            observed.append({"worktree": role, "state": "primed" if compiled else "empty",
                             "compiled_bytes": compiled})
        expected = "primed" if self.build_cache == "warm" else "empty"
        wrong = [item["worktree"] for item in observed if item["state"] != expected]
        if wrong:
            raise Unavailable("build_cache_unobserved", "the %s target of %s was not observed "
                              "%s before the measured pass" % (
                                  ", ".join(wrong), self.build_cache, expected))
        return {"treatment": self.build_cache, "scope": "run_worktree_targets",
                "identity": identity, "priming": priming, "observed": observed,
                "measured_started_unix_ms": int(self.wall() * 1000)}

    def container_linker(self):
        """The recipe linker's identity inside the build image."""
        kind = self.recipe["linker"]
        if kind == "zig_cc":
            return {"kind": kind, "identity": self.zigbuild.identity.split("; ", 1)[1],
                    "unavailable": None}
        argv = {"system_cc": ["cc", "--version"], "apple_ld": ["ld", "-v"]}[kind]
        result = self.image.run_once(argv, {"PATH": SYSTEM_PATH, "LC_ALL": "C"})
        line = _first_line(result.stdout) or _first_line(result.stderr)
        if result.returncode != 0 or not line:
            return {"kind": kind, "identity": None, "unavailable": "%s exited %d in %s" % (
                " ".join(argv), result.returncode, self.image.reference)}
        return {"kind": kind, "identity": line, "unavailable": None}

    def overlay_mounts(self, adapter_id, worktree, files):
        """Read-only binds of the authority's closure bytes over their overlay in a build
        container, from copies of this run's own; none for a host build."""
        if self.image is None:
            return ()
        mounts = []
        for path, data in files:
            source = os.path.join(self.layout.overlays, adapter_id, path)
            if not os.path.lexists(source):
                os.makedirs(os.path.dirname(source), 0o700, exist_ok=True)
                descriptor = os.open(source, os.O_WRONLY | os.O_CREAT | os.O_EXCL
                                     | os.O_NOFOLLOW, 0o600)
                with os.fdopen(descriptor, "wb") as stream:
                    stream.write(data)
                os.chmod(source, OVERLAY_MODE)
            mounts.append((source, os.path.join(worktree, path)))
        return mounts

    def _publish(self, worktree, relative, role, name, needle=None):
        """Admit a built output and copy it, read-only, into the artifacts (ADR-0172).

        A measured build writes its own target directory, so the output is opened as a
        confined regular file below the run's own worktree: no symlinked file or ancestor is
        followed, its logical size must fit what is left of the artifact budget before any
        byte is read, and one bounded read within the run's deadline hashes it, copies it and,
        for an AF binary, finds its embedded commit (`needle`). An output that is refused, or
        lacks its commit, is never published. (record, whether `needle` was found, the refusal)
        """
        label = "the %s output %s" % (role, relative)
        try:
            source, info = admission.open_confined(worktree, relative, label)
        except (ContractError, OSError) as error:
            return None, False, "%s was refused before anything was read: %s" % (
                label, getattr(error, "strerror", None) or error)
        directory = os.path.join(self.layout.artifacts, role)
        target = os.path.join(directory, name)
        deadline = None
        if self.budget.run_deadline is not None:
            deadline = budget_module.Budget(self.budget.run_deadline, time.monotonic, 0)
        try:
            room = self.budget.artifact_limit - self.published
            if info.st_size > room:
                return None, False, ("%s holds %d logical bytes; %d remain of the %d-byte "
                                     "artifact budget, so nothing was read" % (
                                         label, info.st_size, max(0, room),
                                         self.budget.artifact_limit))
            os.makedirs(directory, exist_ok=True)
            found = [needle is None]
            tail = [b""]
            out = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)

            def sink(chunk):
                if not found[0]:
                    window = tail[0] + chunk
                    found[0] = needle in window
                    tail[0] = window[len(window) - len(needle) + 1:] if len(needle) > 1 else b""
                view = memoryview(chunk)
                while view:
                    view = view[os.write(out, view):]

            try:
                digest = admission.read_bounded(source, info.st_size, deadline, label,
                                                "publishing " + label, sink)
                os.fchmod(out, EXECUTABLE)
            except BaseException as error:
                os.close(out)
                os.unlink(target)
                if isinstance(error, budget_module.Expired):
                    raise Unavailable("run_deadline", str(error))
                if isinstance(error, (ContractError, OSError)):
                    return None, False, "%s was refused while it was read: %s" % (label, error)
                raise
            os.close(out)
        finally:
            os.close(source)
        if not found[0]:
            os.unlink(target)
            return None, False, ("%s does not embed its source commit %s, so it was not "
                                 "published" % (label, needle.decode("ascii")))
        self.published += info.st_size
        return ({"path": "%s/%s/%s" % (ARTIFACTS, role, name), "sha256": digest,
                 "size": info.st_size}, needle is not None, None)

    def _note(self, step, name, text):
        """Append the laboratory's own note to a step's retained log and refresh its record:
        why an output the step built was not published."""
        path = os.path.join(self.layout.logs, name + ".log")
        with open(path, "ab") as stream:
            stream.write(b"\nperflab: " + text.encode("utf-8", "replace")[:4096] + b"\n")
        digest, retained = file_sha256(path)
        step["log"].update(sha256=digest, bytes=retained)

    def _supervisor(self, worktree):
        env = self.environment()
        argv = self.cargo(worktree, "review-process", "--example", "perf-supervisor",
                          features=False)
        step = self.steps.run("supervisor", argv, worktree, env)
        unchanged = not changed_paths(self.layout, worktree) and lock_digest(worktree) \
            == self.plan["build_inputs"]["authority"]["cargo_lock_sha256"]
        artifact = None
        if step["status"] == "succeeded" and unchanged:
            artifact, _, refusal = self._publish(
                worktree, self.output_relative("--example", "perf-supervisor"), "supervisor",
                "perf-supervisor")
            if refusal is not None:
                self._note(step, "supervisor", refusal)
        return {"source_sha": self.plan["resolved"]["authority_sha"],
                "status": "succeeded" if artifact else "failed",
                "binary_sha256": artifact["sha256"] if artifact else None,
                "artifact": artifact, "source_unchanged": unchanged,
                "cargo_lock_sha256": lock_digest(worktree), "settings": dict(self.settings),
                "step": step, "log": step["log"]}

    def _side(self, side, role, worktree, sha, authority):
        env = self.environment()
        argv = self.cargo(worktree, "af", "--bin", "af")
        product = self.steps.run(side + "-af", argv, worktree, env)
        expected = sha[:EMBEDDED_PREFIX]
        af_artifact = embedded = None
        if product["status"] == "succeeded":
            # Published only when admitted and embedding its own commit (ADR-0172).
            af_artifact, verified, refusal = self._publish(
                worktree, self.output_relative("--bin", "af"), side, "af",
                expected.encode("ascii"))
            embedded = {"expected": expected, "method": "binary_bytes", "verified": verified}
            if refusal is not None:
                self._note(product, side + "-af", refusal)
        pristine = not changed_paths(self.layout, worktree)

        outcomes, chosen, overlay, probe_artifact = [], None, None, None
        probe_refusal = None
        for adapter_id, entry in adapters_module.ordered(self.adapters):
            record = {"adapter_id": adapter_id, "priority": entry["priority"],
                      "equivalence_group": entry["equivalence_group"]}
            if chosen is not None:
                outcomes.append(dict(record, outcome="not_attempted", step=None))
                continue
            files = adapters_module.read_closure(authority, entry)
            applied = Overlay(worktree, files)
            applied.apply()
            applied.verify("before %s's %s compile" % (side, adapter_id))
            step_name = "%s-probe-%s" % (side, adapter_id)
            # A diagnostic recipe's features (perf-control, perf-trace) are the probe's too.
            step = self.steps.run(step_name, self.cargo(
                worktree, entry["package"], "--example", entry["example"],
                features=self.plan["scope"] == "diagnostic"),
                worktree, env, readonly_files=self.overlay_mounts(adapter_id, worktree, files))
            # A build script of the measured source runs as this user and could rewrite the
            # closure it compiles; whatever the compile's status, only exact authority bytes
            # may stand behind a published probe or the next adapter's attempt.
            applied.verify("during %s's %s compile (%s; see build/logs/%s-probe-%s.log)"
                           % (side, adapter_id, step["status"], side, adapter_id))
            if step["status"] == "succeeded":
                chosen = entry
                overlay = adapters_module.overlay_record(adapter_id, files)
                # The compile decided the adapter; an output that is not admitted is the
                # side's failure, never a reason to try the next adapter.
                probe_artifact, _, probe_refusal = self._publish(
                    worktree, self.output_relative("--example", entry["example"]), side,
                    entry["example"])
                if probe_refusal is not None:
                    self._note(step, step_name, probe_refusal)
                outcomes.append(dict(record, outcome="compiled", step=step))
            else:
                applied.restore()
                outcomes.append(dict(record, outcome="failed", step=step))
        allowed = set(item["path"] for item in overlay["files"]) if overlay else set()
        unchanged = pristine and changed_paths(self.layout, worktree) <= allowed \
            and lock_digest(worktree) == self.plan["build_inputs"][side]["cargo_lock_sha256"]
        succeeded = (product["status"] == "succeeded" and af_artifact is not None
                     and embedded["verified"] and unchanged and probe_refusal is None)
        reason = None
        if chosen is None:
            reason = ("no declared adapter compiles against this source; its native coverage "
                      "is unavailable" if outcomes else "no adapter is declared")
        elif probe_artifact is None:
            reason = probe_refusal[:4096]
        return {
            "source_sha": sha, "status": "succeeded" if succeeded else "failed",
            "worktree": role, "shared_with": None, "adapter_outcomes": outcomes,
            "chosen_adapter": overlay["adapter_id"] if overlay else None,
            "equivalence_group": chosen["equivalence_group"] if chosen else None,
            "probe_coverage": "available" if probe_artifact else "unavailable",
            "coverage_reason": reason, "overlay": overlay,
            "binaries": {"af": af_artifact["sha256"] if af_artifact else None,
                         "probe": probe_artifact["sha256"] if probe_artifact else None},
            "artifacts": {"af": af_artifact, "probe": probe_artifact},
            "embedded_commit": embedded, "source_unchanged": unchanged,
            "cargo_lock_sha256": lock_digest(worktree), "settings": dict(self.settings),
            "argv": product["argv"], "environment": product["environment"],
            "product": product, "log": product["log"],
        }

    def _cleanup(self):
        """Remove the run's repository, worktrees and private homes; keep the build evidence."""
        if self.keep:
            return {"status": "retained", "detail": "worktrees kept on request below "
                    + self.layout.recorded(self.layout.worktrees)}
        problems = []
        for path in self.layout.scratch():
            if not os.path.lexists(path):
                continue
            _make_removable(path)
            shutil.rmtree(path, onerror=lambda function, failed, info: problems.append(
                "%s: %s" % (self.layout.recorded(failed), info[1])))
        if problems:
            return {"status": "failed", "detail": "; ".join(problems)[:4096]}
        return {"status": "removed", "detail": "repository, worktrees, targets and private "
                "homes removed; build evidence kept"}


def _make_removable(root):
    """Cargo's registry sources are read-only; let their owner remove them."""
    for current, dirs, _ in os.walk(root):
        for name in dirs:
            path = os.path.join(current, name)
            if not os.path.islink(path):
                try:
                    os.chmod(path, stat.S_IRWXU)
                except OSError:
                    pass


# Consumption ----------------------------------------------------------------------------------

def read_manifest(build_dir):
    """(builds.json, its bytes), read within the JSON bound and validated; no artifact is read,
    so a shard can establish its run's deadline before any artifact admission (ADR-0172)."""
    path = os.path.join(build_dir, BUILDS_FILE)
    require(not os.path.islink(path), "%s is a symlink" % BUILDS_FILE)
    with open(path, "rb") as stream:
        data = stream.read(limits.MAX_JSON_BYTES + 1)
    return contracts.read_json(data, "builds", BUILDS_FILE), data


def verify_artifacts(build_dir, budget=None, manifest=None):
    """The verified builds.json and the absolute path of every artifact, after rehashing the
    manifest and each binary against the hash builds.json recorded; nothing is rebuilt. Every
    enumeration and read leaves time before `budget`'s deadline when one is given."""
    doc = (manifest or read_manifest(build_dir))[0]
    artifacts_dir = os.path.join(build_dir, ARTIFACTS)
    identity, hashes = bundle.verify(artifacts_dir, doc["artifacts"]["limit_bytes"],
                                     label="build/artifacts", budget=budget)
    require(doc["artifacts"]["manifest_sha256"] == identity,
            "build/artifacts: the manifest is not the one builds.json recorded")
    records = [("supervisor", doc["supervisor"]["artifact"])]
    for side in contracts.SIDES:
        for key in ("af", "probe"):
            records.append(("%s/%s" % (side, key), doc["sides"][side]["artifacts"][key]))
    paths = {}
    for name, record in records:
        if record is None:
            continue
        relative = record["path"][len(ARTIFACTS) + 1:]
        require(hashes.get(relative) == record["sha256"],
                "build/%s: %s does not match builds.json" % (record["path"], name))
        paths[name] = os.path.join(build_dir, record["path"])
    return doc, paths


def verified_binary(path, expected, budget=None, size=None):
    """Rehash a binary immediately before launching it: a regular file, never through a
    symlink, of its recorded size when one is given, read within the dominant deadline."""
    fd = os.open(path, admission.FILE)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode), "%s is not a regular file" % path)
        require(size is None or info.st_size == size, "%s changed after the build: %d bytes, "
                "not %s" % (path, info.st_size, size))
        digest = admission.read_bounded(fd, info.st_size, budget, path,
                                        "verifying " + os.path.basename(path))
    finally:
        os.close(fd)
    require(digest == expected, "%s changed after the build: %s, not %s"
            % (path, digest, expected))
    return path
