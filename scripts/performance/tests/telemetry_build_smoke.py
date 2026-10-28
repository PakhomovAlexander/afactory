#!/usr/bin/env python3
"""Real Docker build/publication attacks; opt-in validate.yml hook, never a full AF build.

Requires a non-root Linux runner, the pinned image and passwordless sudo for the existing
ci-scratch backend. Each disposable filesystem is only 128 MiB. No service is installed.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/performance"))
CLI = ROOT / "scripts/performance/cli.py"
# The child must run the real CLI's source-only admission before this harness imports
# any library. Loading without __main__ defers only argument dispatch until observation
# is installed; the production admission and imports execute in their original order.
CHILD_CLI = (runpy.run_path(str(CLI), run_name="af_telemetry_build_smoke_cli")
             if __name__ == "__main__" and sys.argv[1:2] == ["--fixture-child"] else None)
from perflab import admission, budget, builds, hostlock, hostprep, telemetrybuild, tomlfile  # noqa: E402
from perflab.values import ContractError  # noqa: E402

IMAGE = ("ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:"
         "26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2")
SMALL_BYTES = 128 << 20


def identity(info):
    return [info.st_dev, info.st_ino]


def record_guard(lease, receipt):
    """Test-only observation, under the real CLI lease, before any candidate launch.

    The receipt lives outside every candidate mount. It grants maintenance over only this
    exact fence; a later experiment's fence cannot be mistaken for the fixture's.
    """
    lease.check()
    info = os.lstat(lease.fence)
    assert stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid()
    assert stat.S_IMODE(info.st_mode) == 0o700 and not os.listdir(lease.fence)
    with open(receipt, "x") as stream:
        json.dump({"lock": identity(os.fstat(lease.fd)),
                   "lock_uid": os.fstat(lease.fd).st_uid,
                   "fence": identity(info), "fence_ctime_ns": info.st_ctime_ns,
                   "fence_uid": info.st_uid}, stream)


def fixture_child(receipt, argv):
    # Observe the normal guard; do not replace acquisition, borrow a descriptor, change
    # the common path, or catch the build's rejection. runpy executes the actual CLI.
    cli = CHILD_CLI or runpy.run_path(str(CLI), run_name="af_telemetry_build_smoke_cli")
    original = hostlock.MachineLock.guard
    def guard(lease):
        original(lease)
        record_guard(lease, receipt)
    hostlock.MachineLock.guard = guard
    raise SystemExit(cli["main"](argv))


def maintain_fixture(receipt, fenced, cleanup, directory=None):
    """Explicit smoke-fixture maintenance, never product recovery.

    `directory` is only the unit-test seam, like MachineLock's. The live smoke always
    opens the system path without creating it, and honors its installed inode anchor.
    """
    path = (os.path.join(directory, hostlock.MACHINE_LOCK_NAME) if directory is not None
            else hostlock.default_path())
    hostlock._check_directory(os.path.dirname(path), directory is None)
    anchor = hostlock.read_anchor() if directory is None else None
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    fence_fd = None
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        def check_lock():
            held, named = os.fstat(fd), os.lstat(path)
            assert stat.S_ISREG(held.st_mode) and held.st_nlink == 1
            assert not held.st_mode & 0o022
            assert identity(held) == identity(named) == receipt["lock"], "machine lock changed"
            assert held.st_uid == receipt["lock_uid"], "machine lock owner changed"
            assert anchor is None or identity(held) == anchor, "machine anchor changed"
        check_lock()
        fence = path + ".fence"
        if fenced:
            fence_fd = os.open(fence, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            def check_fence():
                held, named = os.fstat(fence_fd), os.lstat(fence)
                assert stat.S_ISDIR(named.st_mode)
                assert identity(held) == identity(named) == receipt["fence"], "foreign fence"
                assert held.st_ctime_ns == receipt["fence_ctime_ns"], "changed fence"
                assert held.st_uid == receipt["fence_uid"] == os.geteuid(), "foreign fence owner"
                assert stat.S_IMODE(held.st_mode) == 0o700 and not os.listdir(fence_fd), "unsafe fence"
            check_fence()
        else:
            assert not os.path.lexists(fence), "unexpected experiment fence"
        cleanup()  # Reaped child, no Docker descendants, exact unmounted/removed scratch.
        check_lock()
        if fenced:
            check_fence()
            os.rmdir(fence)
            parent = os.open(os.path.dirname(path), os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(parent)
            finally:
                os.close(parent)
    finally:
        if fence_fd is not None:
            os.close(fence_fd)
        os.close(fd)


def invoke(source, output, mount, sha, success, fixture):
    result = subprocess.run([sys.executable, "-I", "-B", str(Path(__file__).resolve()),
        "--fixture-child", str(fixture.receipt), "telemetry-contained-build",
        "--source", str(source), "--source-sha", sha, "--output", str(output), "--image", IMAGE,
        "--scratch-fs", str(mount), "--seconds", "120"], capture_output=True, timeout=150,
        env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1"))
    fixture.child = result  # subprocess.run has synchronously reaped the CLI child.
    check_build_result(result, success)
    assert fixture.receipt.is_file(), "CLI never acquired/guarded the machine lease"
    assert output.exists() == success, "refused build published an artifact"
    if not success:
        if (source / "build.rs").read_text().find("many-small") >= 0:
            check_compile_rejection(result, mount, "aggregate bound")
        assert b"cleanup unconfirmed" not in result.stdout + result.stderr
    return result


def check_build_result(result, success):
    assert (result.returncode == 0) == success, result.stdout + result.stderr
    if not success:
        assert result.returncode == 2, result.stdout + result.stderr
        refusal = json.loads(result.stdout)
        assert refusal["reason"] == "telemetry_build_failed", refusal


def check_compile_rejection(result, mount, message):
    assert b"compile failed; no binary admitted" in result.stdout + result.stderr
    assert (mount / "logs/compile.log").is_file(), "compilation never started"
    assert message in (mount / "logs/compile.log").read_text()


@contextmanager
def owned_root(target):
    root = Path(tempfile.mkdtemp(dir=target))
    try:
        yield root
    except BaseException:
        # TemporaryDirectory would recursively delete a possibly still-mounted scratch.
        sys.stderr.write("smoke fixture retained for inspection: %s\n" % root)
        raise
    else:
        shutil.rmtree(root)


def fixture(root, script):
    source = root / "source"
    source.mkdir(mode=0o755)
    (source / "src").mkdir()
    (source / "src/main.rs").write_text('fn main() { let mut b = itoa::Buffer::new(); '
        'println!("{} {}", b.format(7), env!("FIXTURE_COMMIT")); }\n')
    # A real registry dependency: --locked fetch must populate an initially empty home,
    # then the exact production --offline compilation must use it successfully.
    lock = tomlfile.loads((ROOT / "Cargo.lock").read_bytes(), "trusted lock")
    dep = next(item for item in lock["package"] if item["name"] == "itoa")
    (source / "Cargo.toml").write_text('[package]\nname="af"\nversion="0.0.0"\nedition="2024"\n'
        '[dependencies]\nitoa="=' + dep["version"] + '"\n')
    (source / "Cargo.lock").write_text('version=4\n[[package]]\nname="af"\nversion="0.0.0"\n'
        'dependencies=["itoa"]\n[[package]]\nname="itoa"\nversion="' + dep["version"] + '"\n'
        'source="' + dep["source"] + '"\nchecksum="' + dep["checksum"] + '"\n')
    (source / "rust-toolchain.toml").write_bytes((ROOT / "rust-toolchain.toml").read_bytes())
    (source / "build.rs").write_text('''fn main() {
    let git = std::process::Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap();
    assert!(git.status.success());
    println!("cargo:rustc-env=FIXTURE_COMMIT={}", String::from_utf8(git.stdout).unwrap().trim());
    assert!(std::fs::write("/src/escaped", b"bad").is_err());
    assert!(std::fs::write("/controller-escaped", b"bad").is_err());
    assert!(!std::path::Path::new("/var/run/docker.sock").exists());
    assert!(std::fs::write("/dev/shm/escaped", b"bad").is_err());
    for key in ["GH_TOKEN", "GITHUB_TOKEN", "ACTIONS_RUNTIME_TOKEN"] {
        assert!(std::env::var_os(key).is_none());
    }
    for key in ["CARGO_HOME", "RUSTUP_HOME", "HOME", "TMPDIR", "CARGO_TARGET_DIR"] {
        std::fs::write(std::path::Path::new(&std::env::var(key).unwrap()).join("owned"), b"ok").unwrap();
    }
''' + script + '\n}\n')
    # Git mutations are confined to this disposable fixture; the checkout is never changed.
    env = builds.git_environment(str(root))
    for args in (("init", "-q", "--initial-branch=main"), ("add", "."),
                 ("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                  "commit", "-q", "--no-gpg-sign", "-m", "fixture")):
        subprocess.run(["git", "-C", str(source), *args], env=env, check=True, timeout=10)
    sha = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], env=env,
                                  timeout=10).decode().strip()
    return source, sha


class BuildFixture:
    def __init__(self, root):
        self.receipt = root / "machine-guard.json"
        self.child = None


def scratch(root, action, cli=True):
    image, mount = root / "scratch.img", root / "scratch"
    fixture = BuildFixture(root)
    hostprep.prepare(str(image), str(mount), SMALL_BYTES)
    image_before, mount_before = os.lstat(image), os.lstat(mount)
    def cleanup():
        if cli:
            assert fixture.child is not None and fixture.child.returncode >= 0, "CLI cleanup uncertain"
        image_now, mount_now = os.lstat(image), os.lstat(mount)
        assert stat.S_ISREG(image_now.st_mode) and stat.S_ISDIR(mount_now.st_mode)
        assert image_now.st_uid == mount_now.st_uid == os.geteuid(), "foreign scratch owner"
        assert identity(image_now) == identity(image_before), "scratch image changed"
        assert identity(mount_now) == identity(mount_before), "scratch mount changed"
        assert image_now.st_size == SMALL_BYTES
        live = subprocess.check_output(["docker", "ps", "--all", "--quiet", "--filter",
            "name=^/af-telemetry-build-"], env=builds.docker_client_environment(), timeout=10)
        assert not live.strip(), "container cleanup uncertain; bounded scratch retained: " + str(mount)
        problems = hostprep.cleanup(str(image), str(mount))
        assert not problems, problems
        assert not os.path.ismount(mount) and not os.path.lexists(mount)
        assert not os.path.lexists(image), "owned scratch cleanup incomplete"
    # Failed assertions/timeouts retain the fence and fixture for inspection. Settlement
    # is an explicit successful step after the intended attack and all cleanup proofs.
    action(mount, fixture)
    if cli:
        assert fixture.child is not None and fixture.child.returncode >= 0, "CLI cleanup uncertain"
        with open(fixture.receipt) as stream:
            receipt = json.load(stream)
        maintain_fixture(receipt, fixture.child.returncode != 0, cleanup)
    else:
        with hostlock.MachineLock():
            cleanup()


def main():
    assert os.getuid() != 0, "smoke requires a real non-root runner-owned checkout"
    image = builds.BuildImage("docker", IMAGE)
    gateway = subprocess.check_output(["docker", "network", "inspect", "bridge", "--format",
        "{{(index .IPAM.Config 0).Gateway}}"], env=builds.docker_client_environment(), timeout=10).decode().strip()
    listener = socket.socket()
    listener.bind(("0.0.0.0", 0))
    listener.listen(4)
    listener.settimeout(1)
    address = gateway + ":" + str(listener.getsockname()[1])
    received, stop = [], threading.Event()
    def accept():
        while not stop.is_set():
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(1)
                    received.append(connection.recv(32))
            except socket.timeout:
                continue
    server = threading.Thread(target=accept)
    server.start()
    target = ROOT / "target/telemetry-build-smoke"
    target.mkdir(parents=True, exist_ok=True)
    try:
        # Establish that the operator-controlled sentinel is reachable with bridge network.
        name = "af-build-sentinel-" + str(os.getpid())
        cmd = image.command(name, ["/bin/bash", "-c", "printf reachable > /dev/tcp/" +
            address.replace(":", "/")], "/", {}, [], network=True)
        cmd[2:2] = ["--log-driver", "none", "--ipc", "none"]
        try:
            subprocess.run(cmd, check=True, timeout=15, env=builds.docker_client_environment())
        finally:
            assert image.remove(name) is None
        for _ in range(100):
            if received: break
            time.sleep(.01)
        assert received == [b"reachable"], "sentinel positive control failed"
        received.clear()
        scripts = {
            "legitimate-offline-network-denial": '''
    let addr: std::net::SocketAddr = "''' + address + '''".parse().unwrap();
    assert!(std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)).is_err(),
            "candidate compilation reached operator sentinel");
''',
            "metadata-symlink": "std::os::unix::fs::symlink(SENTINEL, \"/state/af-rustc\").unwrap();",
            "many-small-files": '''
    let areas = ["HOME", "TMPDIR", "CARGO_HOME", "RUSTUP_HOME", "CARGO_TARGET_DIR"];
    eprintln!("many-small aggregate fixture started");
    for i in 0..10000 {
        let dir = std::env::var(areas[i % areas.len()]).unwrap();
        std::fs::write(format!("{dir}/small-{i}"), vec![1u8; 65536]).expect("aggregate bound");
    }
    panic!("aggregate bound did not stop writes");
''',
            "unlinked-open-file": '''
    use std::io::Write;
    let path = std::path::Path::new("/state/tmp/unlinked");
    let mut file = std::fs::File::create(path).unwrap();
    std::fs::remove_file(path).unwrap();
    for _ in 0..10000 { file.write_all(&[1u8; 65536]).expect("unlinked aggregate bound"); }
    panic!("aggregate bound did not stop open-file writes");
'''}
        for label, script in scripts.items():
            with owned_root(target) as temporary:
                root = Path(temporary)
                sentinel = root / "operator-sentinel"
                sentinel.write_bytes(b"external sentinel")
                # Rust literal generated from an owned path, never shell interpolation.
                script = script.replace("SENTINEL", json.dumps(str(sentinel)))
                source, sha = fixture(root, script)
                output = root / "build"
                success = label.startswith("legitimate")
                def exercise(mount, owned):
                    result = invoke(source, output, mount, sha, success, owned)
                    if label == "many-small-files":
                        check_compile_rejection(result, mount, "aggregate bound")
                        files = list((mount / "sample").rglob("small-*"))
                        assert len(files) > 64, "did not exercise many individually small writes"
                        assert all(path.stat().st_size <= 65536 for path in files)
                        assert "aggregate bound" in (mount / "logs/compile.log").read_text()
                    if label == "unlinked-open-file":
                        check_compile_rejection(result, mount, "unlinked aggregate bound")
                        assert "unlinked aggregate bound" in (mount / "logs/compile.log").read_text()
                        assert not (mount / "sample/tmp/unlinked").exists()
                    if label == "metadata-symlink":
                        assert b"foreign state entry" in result.stdout + result.stderr
                    stats = os.statvfs(mount)
                    assert stats.f_blocks * stats.f_frsize <= SMALL_BYTES
                scratch(root, exercise)
                assert sentinel.read_bytes() == b"external sentinel"
                assert not (source / "target").exists() and not (source / "escaped").exists()
                if success:
                    binary = (output / "af").read_bytes()
                    assert sha.encode() in binary, "fixture lost its real Git commit"
                    assert (output / "af.sha256").read_text().strip() == hashlib.sha256(binary).hexdigest()
                    for key in ("af-rustc", "af-cargo"):
                        assert "1.88.0" in (output / key).read_text()
                print("real Docker build:", label, "passed", flush=True)
        # Produce hostile outputs in real containers, then use production admission. No
        # linker behavior is assumed to preserve a symlink a build script planted early.
        with owned_root(target) as temporary:
            root = Path(temporary)
            sentinel = root / "operator-sentinel"
            sentinel.write_bytes(b"external sentinel")
            def attacks(mount, owned):
                state = mount / "sample"
                for label, command in (
                    ("binary-link", 'ln -s "$SENTINEL" /state/target/release/af'),
                    ("ancestor-link", 'rmdir /state/target/release; ln -s "$SENTINEL" /state/target/release'),
                    ("oversized", 'truncate -s ' + str(telemetrybuild.BINARY_BYTES + 1) + ' /state/target/release/af'),
                    ("fifo", 'mkfifo /state/target/release/af')):
                    state.mkdir()
                    for entry in telemetrybuild.STATE_NAMES: (state / entry).mkdir()
                    (state / "target/release").mkdir()
                    name = "af-build-output-" + label + "-" + str(os.getpid())
                    argv = image.command(name, ["/bin/sh", "-c", command], "/", {"SENTINEL": str(sentinel),
                        "PATH": "/usr/bin:/bin"}, [(str(state), "/state", True)])
                    argv[2:2] = ["--log-driver", "none", "--ipc", "none", "--memory", "128m", "--pids-limit", "32"]
                    try:
                        subprocess.run(argv, check=True, timeout=15, env=builds.docker_client_environment())
                    finally:
                        assert image.remove(name) is None
                    output = root / ("admit-" + label)
                    output.mkdir()
                    try:
                        telemetrybuild.admit_binary(str(state), str(output),
                            budget.Budget(time.monotonic() + 10, time.monotonic, 0))
                    except (ContractError, OSError):
                        pass
                    else:
                        raise AssertionError("hostile Docker output admitted: " + label)
                    assert list(output.iterdir()) == []
                    assert sentinel.read_bytes() == b"external sentinel"
                    assert not admission.remove_tree(str(state))
                    print("real Docker admission:", label, "passed", flush=True)
            scratch(root, attacks, cli=False)
        assert received == [], "candidate reached the operator sentinel"
    finally:
        stop.set()
        server.join(2)
        listener.close()
        assert not server.is_alive()
    print("live Docker build boundary smoke passed; minimal fixtures only")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--fixture-child":
        fixture_child(sys.argv[2], sys.argv[3:])
    else:
        main()
