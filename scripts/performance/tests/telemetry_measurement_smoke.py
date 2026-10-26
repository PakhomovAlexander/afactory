"""Mandatory live Docker measurement boundary regressions, beside the eight build cases.

No mock runtime, credentials, candidate repository build, or published measurement. All
generated files live in an owned temporary tree; each filesystem is a 128-MiB loop image.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/performance"))
from perflab import budget, builds, hostprep, runner, telemetryexec  # noqa: E402

IMAGE = ("ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:"
         "26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2")
SMALL_BYTES = 128 << 20
FILE_BYTES = 2 << 20


def docker(*args):
    return subprocess.check_output(["docker", *args], timeout=20,
                                   env=builds.docker_client_environment())


def main():
    assert shutil.which("docker"), "Docker unavailable: live measurement smoke was not run"
    assert os.getuid() != 0, "smoke requires a real non-root runner-owned checkout"
    image = builds.BuildImage("docker", IMAGE)
    root = Path(tempfile.mkdtemp(prefix="af-telemetry-measurement-smoke-"))
    sentinel = root / "operator-sentinel"
    sentinel.write_bytes(b"external sentinel")
    backing, mount, control = root / "scratch.img", root / "scratch", root / "owner"
    hostprep.prepare(str(backing), str(mount), SMALL_BYTES)
    # A crash before the owner is installed retains the image; it never authorizes a
    # measurement. All actual launches below are fenced by the trusted owner.
    with hostprep.TelemetryOwner(str(control), str(mount), create=True,
                                image=str(backing)) as owner:
        environment = {"logical_cpus": 1, "memory_limit_bytes": 256 << 20,
                       "pid_limit": 64, "scratch_limit_bytes": SMALL_BYTES}
        timing = budget.Budget(time.monotonic() + 90, time.monotonic, 0)
        scratch, cpus, runtime = runner.contained_host("docker", IMAGE, str(mount),
                                                       environment, timing, time.monotonic)
        executor = telemetryexec.Contained("docker", image, environment, scratch,
            os.getuid(), os.getgid(), cpus, "smoke", runtime, owner=owner)

        def execute(label, script, inspect=False, selected=executor, readable=()):
            own = Path(selected.directory)
            own.mkdir(exist_ok=True)
            work, capture = own / "run", own / "capture"
            work.mkdir()
            capture.mkdir()
            env = {"PATH": "/usr/bin:/bin", "HOME": str(work), "TMPDIR": str(work),
                   "PYTHONDONTWRITEBYTECODE": "1"}
            if inspect:
                script = '''from pathlib import Path
import time
Path('ready').write_text('ready')
until = time.monotonic() + 10
while not Path('release').exists():
    assert time.monotonic() < until, 'controller did not release fixture'
    time.sleep(.01)
''' + script
            execution = telemetryexec.Execution([selected.python, "-I", "-B", "-c", script],
                env, str(work), [str(work)], list(readable), str(capture), label, SMALL_BYTES,
                time.monotonic() + 30, FILE_BYTES)
            outcomes, errors = [], []

            def run():
                try:
                    outcomes.append(selected.run(execution))
                except BaseException as error:
                    errors.append(error)

            worker = threading.Thread(target=run)
            worker.start()
            try:
                if inspect:
                    until = time.monotonic() + 10
                    while not (work / "ready").exists():
                        assert worker.is_alive() and time.monotonic() < until, errors
                        time.sleep(.01)
                    # Observe real daemon policy while the real candidate is running.
                    observed = json.loads(docker("inspect", "--format", "{{json .}}",
                                                 selected.current))
                    assert observed["HostConfig"]["LogConfig"]["Type"] == "none"
                    assert observed["HostConfig"]["IpcMode"] == "none"
                    assert observed["LogPath"] == ""
                    assert all(item["Type"] == "bind" for item in observed["Mounts"])
            finally:
                if inspect: (work / "release").touch()
                worker.join(55)
            assert not worker.is_alive(), "execution owner did not finish within its bound"
            assert not errors, errors
            outcome = outcomes[0]
            assert outcome.cleanup == "confirmed", outcome.cleanup_detail
            if selected.current:
                assert not docker("ps", "--all", "--quiet", "--filter",
                                  "name=^/" + selected.current + "$").strip()
            assert backing.stat().st_size == SMALL_BYTES
            info = os.statvfs(mount)
            assert info.f_blocks * info.f_frsize <= SMALL_BYTES
            assert telemetryexec.allocated([str(capture)]) <= 2 * FILE_BYTES + 16384
            shutil.rmtree(own)  # Only after confirmed namespace removal.
            print("real Docker measurement:", label, "passed", flush=True)
            return outcome

        try:
            legitimate = execute("legitimate", "from pathlib import Path\n"
                "Path('owned').write_bytes(b'ok')\nprint('{}')\n", inspect=True)
            assert legitimate.code == 0 and legitimate.stdout == b"{}\n"
            denied = execute("outside-scratch", '''from pathlib import Path
for name in ('/controller-escaped', '/tmp/escaped', '/dev/shm/escaped', SENTINEL):
    try:
        Path(name).write_bytes(b'bad')
    except OSError:
        continue
    raise AssertionError('write escaped owned scratch: ' + name)
assert not Path('/var/run/docker.sock').exists()
print('{}')
'''.replace("SENTINEL", repr(str(sentinel))), readable=[str(sentinel)])
            assert denied.code == 0, denied.stderr
            assert sentinel.read_bytes() == b"external sentinel"
            flood = execute("output-flood", '''import os
for _ in range(1024):
    os.write(1, b'x' * 65536)
    os.write(2, b'y' * 65536)
''', inspect=True)
            assert flood.code != 0 or flood.failure() is not None, "flood was not bounded"
            assert 0 < flood.stdout_bytes <= FILE_BYTES and flood.stderr_bytes <= FILE_BYTES
            for unlinked in (False, True):
                result = execute("unlinked-allocation" if unlinked else "aggregate-allocation",
                    '''import errno, os
held = []
try:
    for i in range(4096):
        fd = os.open('small-' + str(i), os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        if UNLINK:
            os.unlink('small-' + str(i))
            held.append(fd)
        os.write(fd, b'x' * (262144 if UNLINK else 65536))
        if not UNLINK: os.close(fd)
except OSError as error:
    assert error.errno == errno.ENOSPC, error
    if UNLINK:
        for fd in held: os.close(fd)
    else:
        os.unlink('small-0')
    print('aggregate ENOSPC')
else:
    raise AssertionError('filesystem did not enforce aggregate allocation')
'''.replace("UNLINK", repr(unlinked)))
                assert result.code == 0 and result.stdout == b"aggregate ENOSPC\n", result.stderr
            from tests import telemetry_diagnostic_smoke
            telemetry_diagnostic_smoke.run(root, mount, image, environment, cpus, ROOT, docker, owner)
            with tempfile.TemporaryDirectory(dir=root) as context:
                Path(context, "Dockerfile").write_text("FROM " + IMAGE + "\nVOLUME /implicit\n")
                volume_image = docker("build", "--quiet", "--network", "none", "--pull=false",
                                      context).decode().strip()
                try:
                    volumes = docker("volume", "ls", "--quiet")
                    hostile = telemetryexec.Contained("docker", builds.BuildImage("docker", volume_image),
                        environment, scratch, os.getuid(), os.getgid(), cpus, "volume", runtime,
                        owner=owner)
                    refused = execute("implicit-volume", "raise AssertionError('must not run')",
                                      selected=hostile)
                    assert "implicit writable volumes" in refused.error
                    assert hostile.names == 0 and hostile.current is None
                    assert volumes == docker("volume", "ls", "--quiet")
                finally:
                    docker("image", "rm", volume_image)
        finally:
            # An exception never settles a pending launch. This is the same guard the
            # workflow CLI uses; uncertainty leaves the exact filesystem recoverable.
            problems = owner.cleanup(str(backing))
            assert not problems, problems
    assert not backing.exists() and not mount.exists()
    shutil.rmtree(root)
    print("live Docker measurement boundary smoke passed; no hosted performance claim")


if __name__ == "__main__":
    main()
