"""Live flow evidence probe, provisioned only by explicit container validation.

The driver reads the real supervisor frame outside each disposable inner PID namespace.
Root and SYS_ADMIN belong only to this fixture container: no host user namespace is needed.
The ordinary Rust/perf Python suites never launch it; the selected live perf smoke does.
"""
import argparse
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
import secrets
import subprocess

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from perflab import builds, supervise
from tests.telemetry_measurement_smoke import IMAGE

LAB = Path(__file__).resolve().parents[1]
EXPECTED = {"complete": 2, "missing_refused": True, "unconfirmed_refused": True}

# The four original cases share the actual supervisor, framed reader and strict flow oracle.
PROBE = r"""
import json, os, subprocess, sys, tempfile
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from perflab import dev_flow, supervise
from perflab.values import canonical_bytes

child = r'''
import ctypes, json, os, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from perflab import dev_flow
from tests.dev_flow_fixture import document

scratch, treatment, mode = Path(sys.argv[2]), sys.argv[3], sys.argv[4]
flow = document(scratch / "work", treatment)
evidence = Path(flow["evidence_root"])
evidence.rename(scratch / "work/evidence")
evidence.parent.rmdir()
flow["evidence_root"] = dev_flow.CONTAINED_EVIDENCE_ROOT
(scratch / "flow.json").write_text(json.dumps(flow))
if mode == "missing":
    (scratch / "work/evidence/round-0-checks.json").unlink()
if mode == "unconfirmed":
    # Make enumeration fail immediately, without a timed-out or surviving descendant.
    # This mount is private to this disposable mount/PID namespace. It also prevents
    # the supervisor from accidentally observing the host's procfs in the negative case.
    libc = ctypes.CDLL(None, use_errno=True)
    assert libc.mount(b"none", b"/proc", b"tmpfs", 0, b"size=4096") == 0, ctypes.get_errno()
    Path("/proc/2").mkdir()
    Path("/proc/2/stat").write_text("invalid process status")
os.write(1, b"flow child completed\n")
os.write(2, b"flow diagnostic\n")
'''

with tempfile.TemporaryDirectory(prefix="af-contained-flow-") as temporary:
    for treatment, mode in (("cold", "complete"), ("primed", "complete"),
                            ("cold", "missing"), ("cold", "unconfirmed")):
        scratch = Path(temporary) / (treatment + "-" + mode)
        scratch.mkdir()
        order = supervise.request(
            [sys.executable, "-I", "-B", "-c", child, sys.argv[1], str(scratch), treatment, mode],
            {"PATH": "/usr/bin:/bin"}, str(scratch), "", 120000, 1024, 2048,
            grace_ms=1000, containment="pid_namespace", scratch=str(scratch),
            evidence_paths=dev_flow.transfer_paths())
        completed = subprocess.run(
            ["unshare", "--mount", "--pid", "--fork",
             "--mount-proc", sys.argv[2]], input=canonical_bytes(order), capture_output=True,
            check=True)
        result, out, err, files = supervise.read_reply(completed.stdout, order)
        assert result["termination"]["kind"] == "exited", result
        assert result["termination"]["code"] == 0, (result, err)
        assert result["supervisor"]["containment"] == "pid_namespace", result
        assert result["timing"]["exit_observation"] == "unreaped", result
        assert out == b"flow child completed\n" and err == b"flow diagnostic\n", (out, err)
        if mode == "unconfirmed":
            assert result["lifecycle"]["status"] == "cleanup_uncertain", result
            assert "/proc/2/stat is not a process status" in result["lifecycle"]["detail"], result
            assert result["scratch"]["status"] == "retained", result
            assert (scratch / "flow.json").is_file()
            assert "not verified empty" in result["evidence"]["detail"], result
        else:
            assert result["lifecycle"]["status"] == "clean", result
            assert result["lifecycle"]["remaining_after_cleanup"] == 0, result
            assert result["scratch"]["status"] == "cleared", result
            assert list(scratch.iterdir()) == [], "the supervisor must have cleared all side files"
        if mode == "complete":
            assert result["evidence"]["status"] == "complete", result
            assert dev_flow.validate_transferred(files, treatment=treatment) == treatment
        else:
            assert result["evidence"]["status"] == "refused", result
            assert result["evidence"]["files"] == [] and files is None, result
            if mode == "missing":
                assert "round-0-checks.json" in result["evidence"]["detail"], result
            try:
                dev_flow.validate_transferred(files, treatment=treatment)
            except ValueError as error:
                assert "missing or unexpected" in str(error), str(error)
            else:
                raise AssertionError("the flow oracle accepted refused evidence")
print(json.dumps({"complete": 2, "missing_refused": True, "unconfirmed_refused": True}))
"""


def command(name, supervisor, docker="docker", lab=LAB):
    """No acquisition, writable host bind, host PID/proc mount or inherited credentials.

    The mount capability and relaxed syscall/LSM profiles are fixture-only: the negative
    case mounts malformed procfs in its own inner mount namespace to refuse enumeration.
    """
    return [docker, "run", "--name", name, "--pull", "never", "--interactive",
            "--network", "none", "--ipc", "none", "--log-driver", "none",
            "--read-only", "--user", "0:0", "--cap-drop", "ALL", "--cap-add", "SYS_ADMIN",
            "--security-opt", "no-new-privileges", "--security-opt", "seccomp=unconfined",
            "--security-opt", "apparmor=unconfined",
            "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=64m",
            "--mount", "type=bind,src=%s,dst=/af-probe/lab,readonly" % lab,
            "--mount", "type=bind,src=%s,dst=/af-probe/supervisor,readonly"
            % Path(supervisor).resolve(),
            "--entrypoint", "/usr/bin/python3", IMAGE, "-I", "-B", "-c", PROBE,
            "/af-probe/lab", "/af-probe/supervisor"]


def run(supervisor, owner=None):
    """Invoked means required: provisioning, assertion and removal failures all fail."""
    name = "af-contained-flow-probe-" + secrets.token_hex(8)
    if owner is not None:
        owner.begin(name)
    try:
        with tempfile.TemporaryDirectory(prefix="af-flow-probe-inputs-") as temporary:
            # A fixture-only read-only bind. Container UID 0 has no DAC override after
            # --cap-drop ALL, so provision every traversed directory and file explicitly.
            root = Path(temporary)
            lab = root / "lab"
            shutil.copytree(LAB / "perflab", lab / "perflab",
                            ignore=shutil.ignore_patterns("__pycache__"))
            (lab / "tests").mkdir()
            for filename in ("__init__.py", "dev_flow_fixture.py"):
                shutil.copyfile(LAB / "tests" / filename, lab / "tests" / filename)
            binary = root / "perf-supervisor"
            shutil.copyfile(supervisor, binary)
            for path in [root, *root.rglob("*")]:
                path.chmod(0o755 if path.is_dir() or path == binary else 0o644)
            completed = subprocess.run(command(name, binary, lab=lab), capture_output=True,
                                       env=builds.docker_client_environment(), timeout=600)
            assert completed.returncode == 0, completed.stderr.decode("utf-8", "replace")
            result = json.loads(completed.stdout)
            assert result == EXPECTED, result
            return result
    finally:
        removed = supervise.remove_container("docker", name)
        if owner is not None:
            owner.finish(name, removed.confirmed)
        assert removed.confirmed, removed.detail


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--supervisor", required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(run(args.supervisor)))


if __name__ == "__main__":
    main()
