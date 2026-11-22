"""Kill the private Task writer during an Attempt and observe normal recovery."""
import argparse
import json
import os
from pathlib import Path
import signal
import time

from native_recovery import process, until
from native_smoke import prepare, write_json


def edit_fixture(binary):
    def edit(root, repo, command):
        catalog = repo / ".af/task-catalog.toml"
        work = repo / ".af/task-packages/fixture/implementer"
        original = (work / "worker.py").read_text()
        marker = "request=json.load(sys.stdin)"
        if original.count(marker) != 1:
            raise ValueError("shipped pagination worker changed")
        injected = f"""
import os, time
from pathlib import Path
private = Path({str(root)!r})
if not (private / 'replacement-allowed').exists():
    pending = private / 'worker-ready.tmp'
    pending.write_text(str(os.getpid()))
    pending.replace(private / 'worker-ready')
    deadline = time.monotonic() + 90
    while not (private / 'release-late-reply').exists():
        if time.monotonic() >= deadline:
            raise SystemExit(1)
        time.sleep(0.05)
    Path('pagination.py').write_text('def paginate(*args): return []' + chr(10))
    (private / 'late-reply-attempted').write_text(str(os.getpid()))
    print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'report':[{{'summary':'Stale result'}}]}}}}), flush=True)
    raise SystemExit(0)
"""
        (work / "worker.py").write_text(original.replace(marker, marker + "\n" + injected))
        manifest = work / "worker.toml"
        manifest.write_text(manifest.read_text().replace("wall_ms = 5000", "wall_ms = 100000"))
        digest = command([binary, "catalog", "digest", str(work)]).strip()
        old = next(row.split('"')[1] for row in
                   catalog.read_text().split('[packages."fixture/implementer"]')[1].splitlines()
                   if row.startswith("digest ="))
        catalog.write_text(catalog.read_text().replace(old, digest))
        ticket = repo / "ticket.json"
        document = json.loads(ticket.read_text())
        # One production lease may elapse before replacement. Keep enough
        # captured wall time for a full second Attempt and the original gate.
        document["limits"]["wall_ms"] = 240_000
        write_json(ticket, document)

    return edit


def ancestor_writer(worker, wrapper, root):
    """Identify the current private writer through the fixture Worker's process tree."""
    pid = worker
    while pid > 1 and pid != wrapper:
        status = Path(f"/proc/{pid}/status").read_text()
        parent = int(next(line.split()[1] for line in status.splitlines() if line.startswith("PPid:")))
        args = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        if (b"run" in args and b"task" in args and b"pagination-cli" in args
                and os.fsencode(root / "task-state") in args and pid != worker):
            return pid
        pid = parent
    raise RuntimeError("fixture worker has no private AF Task writer ancestor")


def run(binary, source):
    root, af, env, policy, bundle = prepare(binary, source, "green", edit=edit_fixture(binary))
    repo = root / "repo"
    state = root / "scenario-state"
    definition = bundle["definitions"]["clean"]
    definition["steps"][0]["timeout_ms"] = 135_000
    definition["limits"].update(max_wall_ms=150_000, max_dispatches=150)
    ant = {
        "schema": "af/ant@12", "id": "writer-loss", "scenario": bundle, "inputs": {},
        "schedule": {"kind": "interval", "seconds": 300,
                     "start_unix_ms": time.time_ns() // 1_000_000 + 1500},
        "owner": "fixture", "notification": "local:fixture",
        "execution": {"overlap": "skip", "missed": "skip", "lateness_ms": 30_000,
                      "per_run": definition["limits"], "rolling_tokens": 1000,
                      "rolling_window_ms": 300_000},
    }
    policy_path = root / "policy.json"
    ant_path = root / "ant.json"
    write_json(policy_path, policy)
    write_json(ant_path, ant)
    spec = ("--file", str(ant_path), "--authority", str(policy_path))
    pin = af("ant-pin", "scenario-state", "ant", "validate", *spec)
    af("ant-add", "scenario-state", "ant", "add", *spec, "--confirm-pin", pin["revision"])
    af("ant-enable", "scenario-state", "ant", "enable", "writer-loss")

    def inspect(label):
        runs = af(label, "scenario-state", "scenario", "list")["runs"]
        return next(iter(runs.values())) if len(runs) == 1 else None

    def done():
        run = inspect("terminal")
        return run if run and run["status"] in {"succeeded", "failed", "cancelled"} else None

    worker = None
    ready = root / "worker-ready"
    argv = [binary, "ant", "scheduler", "run", "--for-seconds", "125", "--state", str(state)]
    try:
        with process(argv, root / "scheduler.log", cwd=repo, env=env) as scheduler:
            worker = int(until(12, lambda: ready.read_text() if ready.is_file() else None,
                               scheduler, "started command Worker"))
            writer = ancestor_writer(worker, scheduler.pid, root)
            before = inspect("before")
            opened = af("task-before", "task-state", "task", "show", "pagination-cli")
            if opened["state"]["phase"] not in {"running", "waiting"}:
                raise RuntimeError("writer lost before a real Task attempt")
            writer_fd = os.pidfd_open(writer)
            try:
                signal.pidfd_send_signal(writer_fd, signal.SIGKILL)
            finally:
                os.close(writer_fd)
            (root / "replacement-allowed").touch()
            fault_at = time.monotonic()
            write_json(root / "writer-loss.json", {"writer": writer, "worker": worker, "signal": 9})
            completed = until(92, done, scheduler, "native outcome after writer loss")
            (root / "release-late-reply").touch()
            limit = time.monotonic() + 1
            late = root / "late-reply-attempted"
            while time.monotonic() < limit and not late.is_file():
                time.sleep(0.05)
            final = inspect("after-late")
            task = af("task-after", "task-state", "task", "show", "pagination-cli")
    finally:
        # Release the bounded, private fixture Worker if another assertion failed.
        (root / "release-late-reply").touch()

    abandoned = [entry["record"] for entry in task["execution_records"]
                 if entry["record"].get("result", {}).get("kind") == "abandoned"]
    writer_epochs = [row["transition"]["epoch"] for row in task["history"]
                     if row["transition"]["change"]["kind"] == "lease_taken"]
    checks = {
        "same_scenario": before["execution_id"] == final["execution_id"],
        "same_task_authority": task["plan_id"] == opened["plan_id"]
        and task["revision_id"] == opened["revision_id"],
        "writer_replaced": len(set(writer_epochs)) >= 2,
        "abandoned_attempt_retained": len(abandoned) == 1,
        "same_scenario_recovers": completed["status"] == "succeeded"
        and task["state"]["outcome"] == "verified",
        "terminal_unchanged_after_late_release": completed == final,
    }
    summary = {
        "scope": "same-VM Task writer death with production lease",
        "checks": checks, "scenario": completed["status"], "task": task["state"],
        "elapsed_after_loss_s": round(time.monotonic() - fault_at, 3),
        "attempts": task["attempts"], "chargeable_tokens": task["chargeable_tokens"],
        "late_worker_attempted_reply": late.is_file(), "writer_epochs": writer_epochs,
        "abandoned": abandoned,
    }
    write_json(root / "writer-summary.json", summary)
    print(json.dumps(summary), flush=True)
    return all(checks.values())


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    raise SystemExit(0 if run(str(Path(args.binary).resolve()), args.source) else 1)
