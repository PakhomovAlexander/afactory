"""Bounded invocation of the native accepted-Snapshot capture; no source format is reimplemented."""
from contextlib import ExitStack
import os
import signal
import subprocess
import tempfile

from . import executor
from .values import ContractError, canonical_digest, document_bytes, loads_strict, require, sha256_bytes


CAPTURE_SECONDS = 300


def create_request(receipt, revision, subject, native_config):
    require(isinstance(receipt, dict) and set(receipt) == {"type", "value"}
            and receipt["type"] == "task", "experiment candidate requires an accepted native Task receipt")
    value = receipt["value"]
    require(isinstance(value, dict) and isinstance(value.get("result"), dict),
            "unsupported candidate: native accepted Task result required")
    output = value["result"].get("outputs", {}).get("snapshot", {})
    require(output.get("snapshot_id") is not None,
            "unsupported source: candidate Task has no native snapshot output")
    require(value.get("result_id") == subject and output["snapshot_id"] == revision,
            "experiment subject/revision differ from the accepted result and candidate Snapshot")
    return dict(schema="af/scenario-experiment-source-request@1", receipt=value,
                baseline_snapshot_id=None, candidate_snapshot_id=revision,
                control_sha=native_config["control_sha"], trusted_ref=native_config["trusted_ref"])


def save_request(path, request):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        data = document_bytes(request)
        require(os.write(fd, data) == len(data), "short native source request write")
        os.fsync(fd)
        executor.sync_dir(os.path.dirname(path))
    finally:
        os.close(fd)


class UncertainCapture(ContractError):
    """Capture termination or publication has not been confirmed clean."""


class CleanCaptureRefusal(ContractError):
    """No capture exists and no capture process remains; safe to settle exclusion."""


def capture(config, path, expected_digest):
    into = os.path.join(path, "source")
    # Existing materialization (including a torn one) is never a clean refusal.
    try:
        return _capture(config, path, expected_digest, into)
    except CleanCaptureRefusal:
        require(not os.path.lexists(into), "native capture appeared during refusal; cleanup uncertain")
        raise


def _capture(config, path, expected_digest, into):
    try:
        request_path = os.path.join(path, "source-request.json")
        request = loads_strict(executor.read(request_path), "native source request", 131072)
        require(canonical_digest(request) == expected_digest, "native source request changed")
        require(sha256_bytes(executor.read(config["af"], 256 * 1024 * 1024)) == config["af_digest"],
                "installed native af bytes changed")
    except (ContractError, OSError) as error:
        if not os.path.lexists(into):
            raise CleanCaptureRefusal(str(error)) from error
        raise
    # The native af command reopens the configured Store, verifies the exact accepted result,
    # materializes through its existing path/Manifest/Git helpers, and verifies replay. It
    # spawns no model or benchmark. A private, synchronously waited group lets us
    # distinguish a reaped refusal from one with surviving descendants.
    process = None
    prelaunch_error = None
    # Only setup OSErrors and Popen's definitive launch refusal prove no process
    # started. An interrupted Popen remains uncertain even without a process handle.
    with ExitStack() as resources:
        try:
            home = resources.enter_context(tempfile.TemporaryDirectory(dir=path))
            env = {"PATH": os.defpath, "HOME": home, "XDG_CONFIG_HOME": home,
                   "XDG_STATE_HOME": home, "XDG_CACHE_HOME": home}
            output = resources.enter_context(tempfile.TemporaryFile(dir=path))
            error = resources.enter_context(tempfile.TemporaryFile(dir=path))
            process = subprocess.Popen([config["af"], "scenario", "experiment-source",
                "--file", request_path, "--repo", config["task_repo"], "--task-state", config["task_state"],
                "--into", into, "--json"], env=env, stdout=output, stderr=error,
                stdin=subprocess.DEVNULL, start_new_session=True)
        except OSError as error:
            prelaunch_error = error
        if prelaunch_error is None:
            try:
                code = process.wait(timeout=CAPTURE_SECONDS)
            except subprocess.TimeoutExpired as expired:
                # Best effort termination is not proof of cleanup. Keep the fence even
                # after reaping the leader; descendants/capture publication may be torn.
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
                raise ContractError("native source capture uncertain; retain private capture for inspection") from expired
            except BaseException:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
                raise
            # A valid receipt and zero exit prove nothing about surviving descendants.
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise ContractError("native source capture process group remains; cleanup uncertain")
            if code != 0:
                if code > 0 and not os.path.lexists(into):
                    raise CleanCaptureRefusal("native source handoff refused before capture")
                raise ContractError("native source handoff refused; capture cleanup uncertain")
            output.seek(0)
            data = output.read(65537)
            require(len(data) <= 65536, "native source receipt exceeds its bound")
    # Settle only after all temporary resources have confirmed cleanup. A cleanup
    # exception, existing capture or ambiguous launch never reaches this refusal.
    if prelaunch_error is not None:
        if not os.path.lexists(into):
            raise CleanCaptureRefusal(str(prelaunch_error)) from prelaunch_error
        raise prelaunch_error
    receipt = loads_strict(data, "native source receipt", 65536)
    require(receipt["schema"] == "af/scenario-experiment-source@1"
            and receipt["request_digest"] == expected_digest
            and (request["baseline_snapshot_id"] is None or receipt["baseline_snapshot_id"] == request["baseline_snapshot_id"])
            and receipt["candidate_snapshot_id"] == request["candidate_snapshot_id"]
            and receipt["repository"] == os.path.join(into, "repository.git"),
            "native source receipt differs from exact accepted Snapshots")
    return receipt
