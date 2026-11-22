"""Witness a Checks outage and cold scheduler restart against a pinned Scenario.

This profile uses separate processes on one VM. The scheduler owns discovery
and recovery. The harness only observes, kills it, restores the dependency,
and starts the same scheduler command again.
"""
import argparse
from collections import Counter
from contextlib import closing, contextmanager
import errno
from hashlib import sha256
import json
import os
from pathlib import Path
import shutil
import signal
import sqlite3
import subprocess
import sys
import time

from native_smoke import literal, prepare, require_linux, write_json


@contextmanager
def process(argv, log, **options):
    with log.open("w+") as stream:
        proc = subprocess.Popen(argv, stdout=stream, stderr=subprocess.STDOUT,
                                start_new_session=True, **options)
        try:
            yield proc
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)


def until(seconds, observe, live, description):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if live.poll() is not None:
            raise RuntimeError(f"process exited ({live.returncode}) while awaiting {description}")
        value = observe()
        if value:
            return value
        time.sleep(0.12)
    raise TimeoutError(description)


def ledger(root):
    with closing(sqlite3.connect(root / "ledger.sqlite")) as db:
        db.row_factory = sqlite3.Row
        db.execute("BEGIN")
        checks = [dict(r) for r in db.execute("SELECT * FROM checks ORDER BY id")]
        calls = [dict(r) for r in db.execute("SELECT * FROM calls ORDER BY sequence")]
    return {"checks": checks, "calls": calls}


def settings(root, **value):
    pending = root / ".settings.tmp"
    write_json(pending, value)
    pending.replace(root / "settings.json")


def witness_full_store(root, scheduler, repo, env):
    pressure = root / "journal-volume" / "fixture-pressure"
    try:
        with pressure.open("xb", buffering=0) as output:
            try:
                for _ in range(1024):
                    output.write(bytes(4096))
            except OSError as error:
                if error.errno != errno.ENOSPC:
                    raise
            else:
                raise RuntimeError("private journal volume did not reach its configured bound")
        trial = subprocess.run(
            scheduler, cwd=repo, env=env, capture_output=True, text=True, timeout=6
        )
        (root / "scheduler-full.log").write_text(trial.stdout + trial.stderr)
        diagnostic = trial.stderr.lower()
        full = "disk is full" in diagnostic or "no space left" in diagnostic
        evidence = {"pressure_errno": errno.ENOSPC, "scheduler_exit": trial.returncode,
                    "af_reported_full_store": full}
        write_json(root / "disk-fault.json", evidence)
        if not trial.returncode or not full:
            raise RuntimeError("AF did not report a full journal write")
        return evidence
    finally:
        pressure.unlink(missing_ok=True)


def configure(root, policy, bundle, endpoint, token, *, broker=False, period_seconds=None,
              distinct_checks=False):
    adapter_dir = root / "adapter"
    adapter_dir.mkdir(mode=0o700)
    adapter = adapter_dir / "adapter"
    adapter_name = "broker_check_adapter.py" if broker else "check_adapter.py"
    shutil.copyfile(Path(__file__).with_name(adapter_name), adapter)
    adapter.chmod(0o700)
    write_json(adapter_dir / "config.json", {"endpoint": endpoint})
    (adapter_dir / "token").write_text(token)
    operation = "experiment_submit_v1"
    destination = "fixture:checks"
    grant = {"adapter": "checks", "operation": operation, "destination": destination}
    policy["adapters"]["checks"] = {
        "executable": str(adapter), "digest": "sha256:" + sha256(adapter.read_bytes()).hexdigest(),
        "operations": [operation], "destinations": [destination], "max_wall_ms": 5000,
        "max_output_bytes": 4096,
    }
    definition = bundle["definitions"]["clean"]
    candidate = definition["result"]["candidate"]
    definition["authority"]["grants"].append(grant)
    definition["steps"].append({
        "id": "checks",
        "operation": {"kind": "action", "grant": grant, "requires_decision": False,
                      "required_condition": None},
        "inputs": {
            "subject": ({"source": "project", "value": candidate,
                         "projection": {"kind": "task_result_id"}}
                        if distinct_checks else literal("text", "pagination")),
            "revision": {"source": "project", "value": candidate,
                         "projection": {"kind": "task_output_snapshot", "port": "snapshot"}},
            "body": literal("text", "Fixture CI"),
            "candidate": candidate,
        },
        "outputs": {"result": "action"}, "when": None,
        "max_attempts": 4, "timeout_ms": 300_000 if broker else 60_000, "on_timeout": "fail",
    })
    definition["limits"].update(max_wall_ms=360_000 if broker else 150_000,
                                max_dispatches=400 if broker else 40)
    definition["outputs"]["checks"] = "action"
    definition["result"]["checks"] = {"source": "step", "step": "checks", "port": "result"}
    ant = {
        "schema": "af/ant@12", "id": "durability", "scenario": bundle, "inputs": {},
        "schedule": {"kind": "interval", "seconds": period_seconds or (900 if broker else 300),
                     "start_unix_ms": time.time_ns() // 1_000_000 + 2000},
        "owner": "fixture", "notification": "local:fixture",
        "execution": {"overlap": "skip", "missed": "skip", "lateness_ms": 30_000,
                      "per_run": definition["limits"], "rolling_tokens": 1000,
                      "rolling_window_ms": 300_000},
    }
    policy_file = root / "policy.json"
    write_json(policy_file, policy)
    policy_file.chmod(0o600)
    ant_file = root / "ant.json"
    write_json(ant_file, ant)
    return "--file", str(ant_file), "--authority", str(policy_file)


def run(binary, source, result, fault):
    root, af, env, policy, bundle = prepare(binary, source, "green")
    scenario_state = "scenario-state"
    if fault == "disk-full":
        volume = root / "journal-volume"
        volume.mkdir(mode=0o700)
        subprocess.run(["mount", "-t", "tmpfs", "-o", "size=4M,nosuid,nodev,noexec",
                        "tmpfs", volume], check=True, timeout=5)
        scenario_state = "journal-volume/scenario-state"
    fixtures = root / "checks-fixture"
    fixtures.mkdir(mode=0o700)
    token = os.urandom(24).hex()
    (fixtures / "token").write_text(token)
    stale_revision = "sha256:" + "0" * 64
    expected = {"lost-reply": (0, 503), "expired-auth": (201, 401),
                "disk-full": (201,), "wrong-revision": (201, 200)}[fault]
    settings(fixtures, result="pending", lose_create_reply=fault == "lost-reply",
             expire_after_create=fault == "expired-auth",
             **({"stale_revision": stale_revision} if fault == "wrong-revision" else {}))
    fixture_log = root / "fixture.log"
    scheduler = [binary, "ant", "scheduler", "run", "--for-seconds", "45",
                 "--state", str(root / scenario_state)]

    def view(label):
        return af(label, scenario_state, "scenario", "list")["runs"]

    def interrupted():
        history = ledger(fixtures)
        statuses = [call["status"] for call in history["calls"]]
        wrong_revision_seen = any(call.get("served_revision") == stale_revision
                                  for call in history["calls"])
        if all(code in statuses for code in expected) and (
            fault != "wrong-revision" or wrong_revision_seen
        ):
            runs = view("before")
            if len(runs) == 1:
                run = next(iter(runs.values()))
                if run["status"] == "waiting" and run["waiting"]["kind"] == "reconcile":
                    return run
        return None

    def settled():
        runs = view("after")
        if len(runs) != 1 or before["id"] not in runs:
            raise RuntimeError("recovery produced a second Scenario occurrence")
        run = runs[before["id"]]
        return run if run["status"] in {"succeeded", "failed", "cancelled"} else None

    with process([sys.executable, str(Path(__file__).with_name("check_service.py")),
                  str(fixtures)], fixture_log) as fixture:
        ready = until(4, lambda: fixture_log.read_text().splitlines(), fixture, "Checks listener")
        _, port = json.loads(ready[0])["listening"]
        spec = configure(root, policy, bundle, f"http://127.0.0.1:{port}", token)
        pin = af("ant-pin", scenario_state, "ant", "validate", *spec)
        af("ant-add", scenario_state, "ant", "add", *spec, "--confirm-pin", pin["revision"])
        af("ant-enabled", scenario_state, "ant", "enable", "durability")
        with process(scheduler, root / "scheduler-before.log", cwd=root / "repo", env=env) as first:
            before = until(22, interrupted, first, f"{fault} fixture response")
            previous = ledger(fixtures)
            write_json(root / "ledger-before.json", previous)
        if first.returncode != -signal.SIGKILL or fixture.poll() is not None:
            raise RuntimeError("scheduler death and fixture survival must both be observed")
        write_json(root / "crash.json", {"scheduler_pid": first.pid, "exit": first.returncode,
                                        "fixture_pid": fixture.pid, "fixture_alive": True})
        task_before = af("task-before", "task-state", "task", "show", "pagination-cli")
        if fault == "expired-auth":
            credential = os.urandom(24).hex()
            for directory in (fixtures, root / "adapter"):
                (directory / "token").write_text(credential)
        settings(fixtures, result=result, lose_create_reply=fault == "lost-reply", restored=True)
        disk = witness_full_store(root, scheduler, root / "repo", env) if fault == "disk-full" else None
        restored_at = time.monotonic()
        with process(scheduler, root / "scheduler-after.log", cwd=root / "repo", env=env) as second:
            after = until(18, settled, second, "terminal Scenario after scheduler restart")
            recovered_at = time.monotonic()
        history = ledger(fixtures)

    task_after = af("task-after", "task-state", "task", "show", "pagination-cli")
    write_json(root / "ledger-after.json", history)
    steps_before = before["frames"]["root"]["steps"]
    steps_after = after["frames"]["root"]["steps"]
    intent_before = steps_before[1]["intents"][0]
    intent_after = steps_after[1]["intents"][0]
    candidate = steps_after[0]["outputs"]["result"]["value"]["result"]["outputs"]["snapshot"]
    identity = sha256(json.dumps(["pagination", candidate["snapshot_id"]]).encode()).hexdigest()
    statuses = Counter(f"{call['action']}:{call['status']}" for call in history["calls"])
    requirements = {
        "scheduler_replaced": first.pid != second.pid,
        "same_execution": before["execution_id"] == after["execution_id"],
        "same_check_intent": intent_before["id"] == intent_after["id"],
        "same_candidate_receipt": steps_before[0]["outputs"] == steps_after[0]["outputs"],
        "exact_candidate_check": history["checks"] == [{"id": identity,
                                                      "revision": candidate["snapshot_id"]}],
        "no_duplicate_create_request": sum(call["action"] == "create" for call in history["calls"]) == 1,
        "reconciled_after_restore": any(
            call["action"] == "read" and call["status"] == 200
            and call["sequence"] > previous["calls"][-1]["sequence"] for call in history["calls"]
        ),
        "unchanged_attempts_and_charge": all(task_before[k] == task_after[k]
                                            for k in ("attempts", "chargeable_tokens", "history")),
        "correct_outcome": after["status"] == {"green": "succeeded", "red": "failed"}[result],
    }
    if fault == "wrong-revision":
        requirements["wrong_revision_refused"] = any(
            call.get("served_revision") == stale_revision for call in previous["calls"]
        ) and before["status"] == "waiting" and candidate["snapshot_id"] != stale_revision
    with Path(binary).open("rb") as executable:
        from hashlib import file_digest
        runtime_digest = file_digest(executable, "sha256").hexdigest()
    report = {
        "profile": "single-vm-process-restart",
        "fault": fault,
        "checks_result": result,
        "binary_sha256": runtime_digest,
        "scenario": after["status"],
        "scenario_id": after["execution_id"],
        "child_task": task_after["state"]["outcome"],
        "step_states": {step["step"]: step["status"] for step in steps_after},
        "terminal_steps_settled": all(step["status"] in {"succeeded", "failed", "cancelled"}
                                      for step in steps_after),
        "restored_to_terminal_s": round(recovered_at - restored_at, 3),
        "attempts": task_after["attempts"], "chargeable_tokens": task_after["chargeable_tokens"],
        "http_calls": dict(statuses), "requirements": requirements,
    }
    if disk:
        report["disk_fault"] = disk
    write_json(root / "recovery-summary.json", report)
    print(json.dumps(report), flush=True)
    return all(requirements.values()) and report["terminal_steps_settled"]


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("source", type=Path)
    parser.add_argument("result", choices=("green", "red"))
    parser.add_argument("--fault", choices=("lost-reply", "expired-auth", "disk-full", "wrong-revision"),
                        default="lost-reply")
    args = parser.parse_args()
    require_linux()
    if args.fault == "disk-full":
        # Start a dedicated user/mount namespace so the bounded Store disappears with the job.
        current = os.readlink("/proc/self/ns/mnt")
        parent = os.environ.get("AF_DURABILITY_PARENT_MOUNT_NS")
        if parent is None:
            os.environ["AF_DURABILITY_PARENT_MOUNT_NS"] = current
            os.execv("/usr/bin/unshare", ["/usr/bin/unshare", "--user", "--map-root-user",
                                         "--mount", "--fork", "--kill-child", sys.executable,
                                         *sys.argv])
        if current == parent:
            parser.error("disk-full requires a private mount namespace")
    raise SystemExit(0 if run(str(Path(args.binary).resolve()), args.source, args.result, args.fault) else 1)
