"""Live Docker regressions through Profile.telemetry_documents and the native supervisor.

Called by the mandatory measurement smoke in its existing job/time/filesystem bounds.
The tiny reader is hostile fixture code; the supervisor is the repository's native source.
"""
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import threading
import time
from types import SimpleNamespace

from perflab import budget, builds, diagnostics, supervise, telemetrydiag, toolrun, tomlfile


def native_supervisor(root, mount, image, source, owner):
    """Prepare only dependencies with network; compile the unchanged supervisor offline."""
    crate = root / "supervisor-source"
    shutil.copytree(source / "crates/review-process/src", crate / "src")
    shutil.copytree(source / "crates/review-process/examples", crate / "examples")
    (crate / "Cargo.toml").write_text('[package]\nname="review-process"\nversion="0.0.0"'
        '\nedition="2024"\n[dependencies]\nnix={version="0.31.3",default-features=false,'
        'features=["fs","signal","process","resource","user"]}\n[dev-dependencies]\n'
        'serde={version="1.0.228",features=["derive"]}\nserde_json="1.0.149"\n')
    # Cargo-generated minimal lock, with every external package admitted against the
    # authority's workspace lock before the network-only, hook-free fetch. Compile is locked
    # and offline. No candidate source is used to prepare this native supervision tool.
    lock_bytes = (source / "scripts/performance/tests/diagnostic-supervisor.lock").read_bytes()
    trusted = tomlfile.loads((source / "Cargo.lock").read_bytes(), "workspace lock")["package"]
    locked = tomlfile.loads(lock_bytes, "native smoke lock")["package"]
    def identity(package):
        return tuple(package.get(key) for key in ("name", "version", "source", "checksum"))
    admitted = {identity(package) for package in trusted if "source" in package}
    assert all(identity(package) in admitted for package in locked if "source" in package)
    (crate / "Cargo.lock").write_bytes(lock_bytes)
    state = mount / "native-build"
    state.mkdir()
    for name in ("home", "cargo", "tmp", "target"):
        (state / name).mkdir()
    timing = budget.Budget(time.monotonic() + 100, time.monotonic, 10)
    image.budget = timing
    tool = builds.ContainerToolchain(image, "1.88.0")
    env = {"PATH": tool.bin_dir + ":/usr/bin:/bin", "RUSTUP_HOME": "/usr/local/rustup",
           "HOME": "/state/home", "CARGO_HOME": "/state/cargo", "TMPDIR": "/state/tmp",
           "CARGO_TARGET_DIR": "/state/target", "RUSTC": tool.rustc,
           "CARGO_BUILD_JOBS": "1", "CARGO_INCREMENTAL": "0",
           "CARGO_PROFILE_RELEASE_OPT_LEVEL": "0", "CARGO_PROFILE_RELEASE_DEBUG": "0"}
    for phase, args in (("fetch", ["fetch", "--locked"]), ("compile", ["build", "--locked",
                         "--offline", "--release", "--example", "perf-supervisor"])):
        name = "af-perf-telemetry-diagnostic-supervisor-" + secrets.token_hex(5)
        command = image.command(name, [tool.cargo] + args, "/src", env,
                                [(str(crate), "/src", False), (str(state), "/state", True)],
                                network=phase == "fetch")
        command[2:2] = ["--ipc", "none", "--log-driver", "none", "--memory", "256m",
                        "--memory-swap", "256m", "--pids-limit", "64"]
        owner.begin(name)
        try:
            result = subprocess.run(command, env=builds.docker_client_environment(),
                                    capture_output=True, timeout=timing.seconds(phase, 60))
            assert result.returncode == 0, result.stderr[-8192:]
        finally:
            image.budget = budget.Budget(timing.deadline, time.monotonic, 0)
            removed = image.remove(name)
            owner.finish(name, removed is None)
            assert removed is None, "native build cleanup unconfirmed"
            image.budget = timing
    binary = root / "perf-supervisor"
    shutil.copyfile(state / "target/release/examples/perf-supervisor", binary)
    binary.chmod(0o755)
    shutil.rmtree(state)
    return binary


def run(root, mount, image, environment, cpus, source, docker, owner):
    environment = dict(environment, swap="disabled", network="disabled", cpu_quota="none")
    binary = native_supervisor(root, mount, image, source, owner)
    sentinel = root / "diagnostic-sentinel"
    sentinel.write_bytes(b"operator state")
    docs = source / "fixtures/performance/telemetry-diagnostic"
    reader = root / "diagnostic-reader"
    # Actual executable reader requests go through the Profile method, toolrun.Contained,
    # the native PID-1 supervisor and the shared sample argv builder.
    reader.write_text('''#!/usr/bin/python3
import json, os, sys, time
from pathlib import Path
kind = sys.argv[2]
root = Path(os.environ['HOME'])
for path in ('/dev/shm/escaped', '/tmp/escaped', '/controller-escaped', SENTINEL):
    try: Path(path).write_bytes(b'escape')
    except OSError: pass
    else: raise AssertionError('write escaped verified scratch: ' + path)
(root / 'owned').write_text('allowed')
if kind == 'status':
    (root / 'ready').touch()
    until = time.monotonic() + 10
    while not (root / 'release').exists():
        assert time.monotonic() < until
        time.sleep(.01)
sys.stdout.buffer.write((Path(DOCS) / (kind + '.json')).read_bytes())
'''.replace("SENTINEL", repr(str(sentinel))).replace("DOCS", repr(str(docs))))
    reader.chmod(0o755)
    work = mount / supervise.DIAGNOSTIC_DIRECTORY
    work.mkdir()
    namespace = work / "namespace"
    namespace.mkdir()
    stage = work / "stage"
    stage.mkdir()
    output = root / "diagnostic-evidence"
    output.mkdir()
    profile = diagnostics.Profile.__new__(diagnostics.Profile)
    profile.execution = {"mode": "linux_container"}
    profile.output, profile.stage = str(output), str(stage)
    profile.retained = []
    profile.work = str(work)
    profile.unsafe_staging = []
    profile.clock = time.monotonic
    profile.budget = diagnostics.Budget({"total_minutes": 2, "profile_bytes": 8 << 20,
        "builds": 1, "tool_seconds": 30, "tool_output_bytes": 1 << 20}, str(output))
    profile.budget.stage = str(stage)
    profile.supervisor = toolrun.Contained(str(binary), "docker", image.reference, environment,
        str(mount), os.getuid(), os.getgid(), cpus, "diagnostic-smoke")
    env = {"PATH": "/usr/bin:/bin", "HOME": str(namespace), "TMPDIR": str(namespace)}
    documents, errors = {}, []
    # Include exact reader data and a read-only external sentinel, using the real binds method.
    binds = profile.binds
    profile.binds = lambda reads=(), writes=(): binds(tuple(reads) + (str(docs), str(sentinel)), writes)
    def read():
        try:
            profile.telemetry_documents(str(reader), SimpleNamespace(path=str(namespace)), env,
                                        "telemetry/reader", documents)
        except BaseException as error:
            errors.append(error)
    worker = threading.Thread(target=read)
    owner.begin("af-perf-telemetry-diagnostic-reader-smoke")
    worker.start()
    try:
        until = time.monotonic() + 10
        while not (namespace / "ready").exists():
            assert worker.is_alive() and time.monotonic() < until, errors
            time.sleep(.01)
        names = docker("ps", "--format", "{{.Names}}").decode().splitlines()
        names = [name for name in names if name.startswith("af-perf-diag-" + profile.supervisor.label[-12:] + "-")]
        assert len(names) == 1, names
        observed = json.loads(docker("inspect", "--format", "{{json .}}", names[0]))
        assert observed["HostConfig"]["IpcMode"] == "none"
        assert observed["HostConfig"]["LogConfig"]["Type"] == "none"
        assert observed["LogPath"] == ""
        assert all(item["Type"] == "bind" for item in observed["Mounts"])
    finally:
        (namespace / "release").touch()
        worker.join(85)
    owner.finish("af-perf-telemetry-diagnostic-reader-smoke", not worker.is_alive() and not errors)
    assert not worker.is_alive() and not errors, errors
    assert set(documents) == set(telemetrydiag.DOCUMENT_KINDS)
    assert sentinel.read_bytes() == b"operator state"
    assert profile.supervisor.requests == 4
    assert not profile.retained
    print("real Docker diagnostic: legitimate reader, denied writes and daemon policy passed", flush=True)
    shutil.rmtree(work)
    # A declared volume must be rejected on this actual reader route before docker run.
    context = root / "diagnostic-volume-image"
    context.mkdir()
    (context / "Dockerfile").write_text("FROM " + image.reference +
        "\nRUN mkdir -p /implicit && chmod 0777 /implicit\nVOLUME /implicit\n")
    volume_image = docker("build", "--quiet", "--network", "none", "--pull=false", str(context)).decode().strip()
    try:
        work.mkdir(); namespace.mkdir(); stage.mkdir()
        before = docker("volume", "ls", "--quiet")
        profile.supervisor.image = volume_image
        count = profile.supervisor.requests
        try:
            profile.telemetry_documents(str(reader), SimpleNamespace(path=str(namespace)), env,
                                        "telemetry/refused", {})
        except Exception as error:
            assert "implicit writable volumes" in str(error), error
        else:
            raise AssertionError("diagnostic image volume was admitted")
        assert profile.supervisor.requests == count
        assert before == docker("volume", "ls", "--quiet")
        assert not (namespace / "owned").exists()
        print("real Docker diagnostic: implicit image VOLUME refused before launch", flush=True)
    finally:
        docker("image", "rm", volume_image)
        shutil.rmtree(work)
