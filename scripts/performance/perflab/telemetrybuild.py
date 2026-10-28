"""Hosted builds: verified tools, hook-free fetch, offline compile and confined admission.

Only sample/ of an enforcement-verified block filesystem reaches candidate code. The
controller's logs share that filesystem's aggregate bound but are never mounted into it.
Trusted output and minimal credential-free Git provenance never reach a writable mount.
"""
import hashlib
import os
import secrets
import shutil
import subprocess
import time
import zlib
from types import SimpleNamespace

from . import admission, budget, builds, capability, hostprep, limits, supervise, tomlfile
from .values import ContractError, require

# Authority-owned ceilings. A caller may supply a smaller filesystem, never raise these.
BUILD_BYTES = 8 * limits.GIB
BINARY_BYTES = limits.BUILD_ARTIFACT_BYTES_MAX
CLEANUP_SECONDS = 30
STATE_NAMES = ("home", "tmp", "cargo-home", "rustup-home", "target")


def read_source(source, relative, maximum=tomlfile.MAX_TOML_BYTES):
    fd, info = admission.open_confined(source, relative, "build source")
    try:
        require(info.st_size <= maximum, "build source file exceeds its byte bound")
        chunks = []
        admission.read_bounded(fd, info.st_size, None, relative, "reading source", chunks.append)
        return b"".join(chunks)
    finally:
        os.close(fd)


def check_configs(source):
    # Cargo fetch never executes hooks, but Cargo configuration can select executable
    # wrappers/credential providers. Reuse Builder's refusal rule, including member configs.
    for directory, subdirs, _ in os.walk(source, followlinks=False):
        subdirs[:] = [name for name in subdirs if name != ".git"]
        cargo = os.path.join(directory, ".cargo")
        if not os.path.lexists(cargo):
            continue
        require(not os.path.islink(cargo), "candidate Cargo configuration must not be a link")
        for name in ("config", "config.toml"):
            path = os.path.join(cargo, name)
            if os.path.lexists(path):
                relative = os.path.relpath(path, source)
                doc = tomlfile.loads(read_source(source, relative), relative)
                require(set(doc) <= {"build"} and set(doc.get("build", {})) <= {"target-dir"},
                        "candidate Cargo configuration changes build/fetch authority: " + relative)


def provenance(source, target, source_sha, timing):
    """Real commit bytes in a minimal read-only Git repository, without host config/refs.

    A worktree's .git file otherwise points outside /src and AF embeds 'unknown'. No host
    repository (or its credentials, hooks or configuration) is ever mounted.
    """
    env = builds.git_environment(target)
    def git(*args):
        return subprocess.run(["git", "-C", source, *args], env=env, check=True,
                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, timeout=timing.seconds("source identity", 30)).stdout
    actual = git("rev-parse", "HEAD").decode().strip()
    require(len(source_sha) == 40 and all(c in "0123456789abcdef" for c in source_sha)
            and actual == source_sha, "source commit differs from the declared full SHA")
    commit = git("cat-file", "commit", actual)
    require(len(commit) <= limits.MIB, "source commit exceeds provenance bound")
    raw = b"commit " + str(len(commit)).encode() + b"\0" + commit
    require(hashlib.sha1(raw).hexdigest() == actual, "source commit bytes changed")
    os.mkdir(target)
    os.makedirs(os.path.join(target, "objects", actual[:2]))
    os.mkdir(os.path.join(target, "refs"))
    with open(os.path.join(target, "HEAD"), "xb") as stream:
        stream.write((actual + "\n").encode())
    with open(os.path.join(target, "objects", actual[:2], actual[2:]), "xb") as stream:
        stream.write(zlib.compress(raw))
    with open(os.path.join(target, "config"), "xb") as stream:
        stream.write(b"[core]\nrepositoryformatversion = 0\nbare = true\n")


def stage(image, tool, source, state, logs, git, phase, timing):
    env = {"PATH": tool.bin_dir + ":/usr/bin:/bin", "LC_ALL": "C",
           "HOME": "/state/home", "TMPDIR": "/state/tmp", "CARGO_HOME": "/state/cargo-home",
           "RUSTUP_HOME": "/state/rustup-home", "CARGO_TARGET_DIR": "/state/target",
           "RUSTC": tool.rustc, "RUSTDOC": os.path.join(tool.bin_dir, "rustdoc"),
           "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
           "GIT_TERMINAL_PROMPT": "0", "GIT_DIR": "/provenance", "GIT_WORK_TREE": "/src",
           "PYTHONDONTWRITEBYTECODE": "1"}
    argv = ([tool.cargo, "fetch", "--locked"] if phase == "fetch" else
            [tool.cargo, "build", "--release", "--locked", "--offline", "--bin", "af",
             "--target-dir", "/state/target"])
    name = "af-telemetry-build-" + secrets.token_hex(6)
    command = image.command(name, argv, "/src", env,
                            [(source, "/src", False), (state, "/state", True),
                             (git, "/provenance", False)], network=phase == "fetch")
    # No daemon log can escape the filesystem budget. No writable /dev/shm mount.
    builds.bound_container(command)
    def exceeded():
        if shutil.disk_usage(os.path.dirname(git)).free < limits.FREE_DISK_FLOOR_BYTES:
            return "build free-disk floor crossed"
        return None
    require(exceeded() is None, "build free-disk floor unavailable")
    require(shutil.disk_usage(logs).free > limits.BUILD_LOG_BYTES_DEFAULT,
            "bounded build filesystem has no space for its log")
    # Reuse the lab's bounded step/log/process lifecycle. The verified filesystem supplies
    # the aggregate hard limit, including open files, instead of a racy path-only counter.
    layout = SimpleNamespace(logs=logs, recorded=lambda value: value)
    step_budget = SimpleNamespace(run_deadline=timing.deadline - timing.reserve,
        deadline_seconds=timing.seconds(phase), log_bytes=limits.BUILD_LOG_BYTES_DEFAULT,
        preallocate_logs=True, exceeded=exceeded)
    container = SimpleNamespace(wrap=lambda *args: (
        command, builds.docker_client_environment(), lambda: image.remove(name)))
    result = builds.Steps(layout, step_budget, container).run(
        phase, argv, source, env, network=phase == "fetch")
    require(result["status"] == "succeeded", "%s %s; no binary admitted" % (
        phase, result["status"]))


def admit_binary(state, output, timing):
    require(set(os.listdir(state)) == set(STATE_NAMES), "candidate planted a foreign state entry")
    fd, info = admission.open_confined(state, "target/release/af", "compiled af")
    try:
        require(0 < info.st_size <= BINARY_BYTES, "compiled af exceeds binary byte bound")
        require(shutil.disk_usage(output).free >= limits.FREE_DISK_FLOOR_BYTES + info.st_size
                + limits.RECORD_BYTES_MAX,
                "binary admission would cross the free-disk floor")
        digest = admission.copy_fd(fd, info.st_size, os.path.join(output, "af"), timing, "compiled af")
        after = os.fstat(fd)
        require((info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns) ==
                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
                "compiled af mutated during admission")
        os.chmod(os.path.join(output, "af"), 0o500)
        return digest
    finally:
        os.close(fd)


def run(source, output, reference, scratch_fs, source_sha, seconds=4800):
    require(CLEANUP_SECONDS < seconds <= 4800, "build deadline must be within 31..4800 seconds")
    require(os.getuid() != 0, "hosted build requires a non-root runner")
    source, output, scratch_fs = map(os.path.abspath, (source, output, scratch_fs))
    require(not os.path.lexists(output), "build output must be a new owned directory")
    deadline = time.monotonic() + seconds
    timing = budget.Budget(deadline, time.monotonic, CLEANUP_SECONDS)
    terminal = budget.Budget(deadline, time.monotonic, 0)
    host = hostprep.Host(run=lambda argv: hostprep._run(
        argv, timeout=terminal.seconds("build filesystem preparation/cleanup", hostprep.STEP_SECONDS)))
    prepared = False
    pending = output + ".pending-" + secrets.token_hex(6)
    state, logs, inventory = None, None, None
    try:
        with terminal.enforce("contained build including cleanup"):
            root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(__file__))))
            trusted = tomlfile.loads(read_source(root, "rust-toolchain.toml"), "trusted toolchain")["toolchain"]["channel"]
            channel = tomlfile.loads(read_source(source, "rust-toolchain.toml"), "source toolchain")["toolchain"]["channel"]
            require(channel == trusted, "source toolchain differs from trusted control")
            check_configs(source)
            require(shutil.disk_usage(os.path.dirname(output)).free >= limits.FREE_DISK_FLOOR_BYTES,
                    "build free-disk floor unavailable")
            if not os.path.lexists(scratch_fs):
                hostprep.prepare(scratch_fs + ".img", scratch_fs, BUILD_BYTES, host=host)
                prepared = True
            checked = capability.scratch_capability(scratch_fs, BUILD_BYTES, True,
                                                    deadline=timing.until("scratch verification", 120))
            require(checked["capability"]["status"] == "available", "; ".join(checked["capability"]["reasons"]))
            inventory = supervise.prepare_scratch(scratch_fs, os.getuid(), os.getgid())
            state = os.path.join(scratch_fs, "sample")
            logs = os.path.join(scratch_fs, "logs")
            os.mkdir(logs, 0o700)
            for name in STATE_NAMES:
                os.mkdir(os.path.join(state, name), 0o700)
            os.mkdir(pending, 0o700)
            git = os.path.join(pending, "provenance")
            provenance(source, git, source_sha, timing)
            image = builds.BuildImage("docker", reference, budget=terminal)
            image.refuse_implicit_volumes()
            tool = builds.ContainerToolchain(image, trusted)
            for phase in ("fetch", "compile"):
                stage(image, tool, source, state, logs, git, phase, timing)
            digest = admit_binary(state, pending, timing)
            for filename, identity in (("af-rustc", tool.rustc_identity), ("af-cargo", tool.cargo_identity),
                                       ("af.sha256", digest.split(":")[1])):
                with open(os.path.join(pending, filename), "x") as stream:
                    stream.write(identity + "\n")
            require(not admission.remove_tree(git), "provenance cleanup failed")
            for path in [os.path.join(state, name) for name in os.listdir(state)] + [logs]:
                problems = admission.remove_tree(path)
                require(not problems, "; ".join(problems))
            # Restore the exact scratch inventory before publishing any controller bytes.
            os.chmod(state, 0o700)
            require(not supervise.scratch_problems(scratch_fs, inventory), "build scratch cleanup unconfirmed")
            if prepared:
                problems = hostprep.cleanup(scratch_fs + ".img", scratch_fs, host=host)
                require(not problems, "; ".join(problems))
            timing.seconds("publishing admitted build")
            os.replace(pending, output)
    except BaseException as error:
        # Never claim successful cleanup on deadline/uncertainty. Keep bounded work for the
        # operator; the next CI cleanup must report an unmount failure, never delete a live mount.
        raise ContractError("contained build refused; inspect any retained owned state at %s "
                            "and %s; %s" % (scratch_fs, pending, error)) from error
