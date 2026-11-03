"""Bounded invocation of the native accepted-Snapshot capture; no source format is reimplemented."""
import os
import subprocess
import tempfile

from . import executor
from .values import ContractError, canonical_digest, document_bytes, loads_strict, require, sha256_bytes


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


def capture(config, path, expected_digest):
    request_path = os.path.join(path, "source-request.json")
    request = loads_strict(executor.read(request_path), "native source request", 131072)
    require(canonical_digest(request) == expected_digest, "native source request changed")
    require(sha256_bytes(executor.read(config["af"], 256 * 1024 * 1024)) == config["af_digest"],
            "installed native af bytes changed")
    # The native af command reopens the configured Store, verifies the exact accepted result,
    # materializes through its existing path/Manifest/Git helpers, and verifies replay. It
    # spawns no model or benchmark. The outer oneshot service owns the process group.
    into = os.path.join(path, "source")
    with tempfile.TemporaryDirectory(dir=path) as home:
        env = {"PATH": os.defpath, "HOME": home, "XDG_CONFIG_HOME": home,
               "XDG_STATE_HOME": home, "XDG_CACHE_HOME": home}
        with tempfile.TemporaryFile(dir=path) as output, tempfile.TemporaryFile(dir=path) as error:
            try:
                result = subprocess.run([config["af"], "scenario", "experiment-source",
                    "--file", request_path, "--repo", config["task_repo"], "--task-state", config["task_state"],
                    "--into", into, "--json"], env=env, stdout=output, stderr=error,
                    stdin=subprocess.DEVNULL, timeout=300, check=False)
            except subprocess.TimeoutExpired as expired:
                raise ContractError("native source capture uncertain; retain private capture for inspection") from expired
            require(result.returncode == 0, "native source handoff refused; inspect the accepted Task and capture")
            output.seek(0)
            data = output.read(65537)
            require(len(data) <= 65536, "native source receipt exceeds its bound")
    receipt = loads_strict(data, "native source receipt", 65536)
    require(receipt["schema"] == "af/scenario-experiment-source@1"
            and receipt["request_digest"] == expected_digest
            and (request["baseline_snapshot_id"] is None or receipt["baseline_snapshot_id"] == request["baseline_snapshot_id"])
            and receipt["candidate_snapshot_id"] == request["candidate_snapshot_id"]
            and receipt["repository"] == os.path.join(into, "repository.git"),
            "native source receipt differs from exact accepted Snapshots")
    return receipt
