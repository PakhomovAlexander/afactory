"""Finite job phases for the broker-mediated, two-worker recovery trial."""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import sqlite3
import subprocess
import time

from native_smoke import prepare, private_scratch, write_json
from native_recovery import configure, process, until


def emit(phase, **evidence):
    print("AF_DURABILITY " + json.dumps(
        {"phase": phase, "host": socket.gethostname(), **evidence}, separators=(",", ":")
    ), flush=True)


def scheduler(binary, root, seconds):
    return [binary, "ant", "scheduler", "run", "--for-seconds", str(seconds),
            "--state", str(root / "scenario-state")]


def final_run(view):
    if len(view) == 1:
        return next(iter(view.values()))
    return None


def stage(binary, source, empty, work_base):
    root, af, env, policy, bundle = prepare(binary, source, "green", work_base=work_base)
    write_json(root / "env.json", env)
    spec = configure(root, policy, bundle, "", "", broker=True)
    write_json(root / "adapter" / "empty.json", empty)
    pin = af("ant-pin", "scenario-state", "ant", "validate", *spec)
    af("ant-add", "scenario-state", "ant", "add", *spec, "--confirm-pin", pin["revision"])
    af("ant-enabled", "scenario-state", "ant", "enable", "durability")

    def waiting():
        request = root / "adapter" / "request.json"
        if request.is_file():
            run = final_run(af("staged", "scenario-state", "scenario", "list")["runs"])
            if run and run["status"] == "waiting" and run["waiting"]["kind"] == "reconcile":
                return json.loads(request.read_text())
        return None

    with process(scheduler(binary, root, 260), root / "scheduler-before.log",
                 cwd=root / "repo", env=env) as first:
        request = until(18, waiting, first, "real native Checks dispatch")
        before = final_run(af("before", "scenario-state", "scenario", "list")["runs"])
        task_before = af("task-before", "task-state", "task", "show", "pagination-cli")
        fixture_tree = subprocess.check_output(
            ["git", "-C", root / "repo", "rev-parse", "HEAD^{tree}"], text=True
        ).strip()
        emit("outbox", root=str(root), request=request, pid=first.pid,
             execution=before["execution_id"], fixture_git_tree=fixture_tree)
        if empty.get("clean"):
            receipt_path = root / "adapter" / "receipt.json"
            until(210, receipt_path.is_file, first, "completed check delivery")
            receipt = json.loads(receipt_path.read_text())
            after = observe_terminal(af, first, before["execution_id"])
            duration = round(time.monotonic() - receipt["received_monotonic"], 3)
            task = af("task-after", "task-state", "task", "show", "pagination-cli")
            return summarize(binary, root, before, after, task_before, task,
                             receipt["result"], first.pid, duration)
        committed = until(210, lambda: (root / "committed.json").is_file(),
                          first, "remote fixture commit acknowledgement to controller")
        if not committed:
            raise RuntimeError("fixture never acknowledged")
        evidence = json.loads((root / "committed.json").read_text())
        if any(evidence[key] != request[key] for key in request) or evidence["state"] != "pending":
            raise ValueError("external fixture commit differs from original request")
    if first.returncode != -signal.SIGKILL:
        raise RuntimeError("scheduler was not killed while undelivered")
    before = final_run(af("before", "scenario-state", "scenario", "list")["runs"])
    task = af("task-before", "task-state", "task", "show", "pagination-cli")
    emit("down", root=str(root), execution=before["execution_id"], pid=first.pid,
         killed=first.returncode, attempts=task["attempts"], charge=task["chargeable_tokens"])
    return True


def observe_terminal(af, proc, execution):
    def done():
        observed = af("after", "scenario-state", "scenario", "list")["runs"]
        run = final_run(observed)
        if not run or run["execution_id"] != execution:
            raise RuntimeError("scheduler rediscovered a different Scenario execution")
        return run if run["status"] in {"succeeded", "failed", "cancelled"} else None

    return until(15, done, proc, "terminal outcome of the original Scenario")


def restart(binary, root, receipt):
    env = json.loads((root / "env.json").read_text())
    before = final_run(json.loads((root / "before.json").read_text())["runs"])
    before_task = json.loads((root / "task-before.json").read_text())
    adapter = root / "adapter"
    request = json.loads((adapter / "request.json").read_text())
    if any(receipt[key] != request[key] for key in request):
        raise ValueError("receipt failed candidate identity")
    write_json(adapter / "receipt.json", receipt)
    started = time.monotonic()

    def af(name, state, *args):
        raw = subprocess.check_output(
            [binary, *args, "--state", str(root / state), "--json"],
            cwd=root / "repo", env=env, timeout=10,
        )
        (root / (name + ".json")).write_bytes(raw)
        return json.loads(raw)

    with process(scheduler(binary, root, 25), root / "scheduler-after.log",
                 cwd=root / "repo", env=env) as second:
        after = observe_terminal(af, second, before["execution_id"])
        duration = round(time.monotonic() - started, 3)
    task = af("task-after", "task-state", "task", "show", "pagination-cli")
    return summarize(binary, root, before, after, before_task, task, receipt["result"],
                     second.pid, duration)


def summarize(binary, root, before, after, before_task, task, expected, pid, duration):
    before_steps = before["frames"]["root"]["steps"]
    after_steps = after["frames"]["root"]["steps"]
    checks = {
        "same_attempts": before_task["history"] == task["history"],
        "same_charge": before_task["chargeable_tokens"] == task["chargeable_tokens"],
        "same_candidate": before_steps[0]["outputs"] == after_steps[0]["outputs"],
        "same_intent": before_steps[1]["intents"][0]["id"] == after_steps[1]["intents"][0]["id"],
        "terminal_steps_settled": all(s["status"] in {"succeeded", "failed", "cancelled"}
                                      for s in after_steps),
        "expected_outcome": after["status"] == {"green": "succeeded", "red": "failed"}[expected],
    }
    with Path(binary).open("rb") as file:
        digest = hashlib.file_digest(file, "sha256").hexdigest()
    result = {"root": str(root), "execution": after["execution_id"], "scenario": after["status"],
              "pid": pid, "terminal_s": duration, "binary_sha256": digest,
              "charge": task["chargeable_tokens"], "attempts": task["attempts"], "checks": checks}
    write_json(root / "distributed-summary.json", result)
    emit("terminal", **result)
    return all(checks.values())


def remote_ledger(trial, phase, data, work_base):
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,39}", trial):
        raise ValueError("invalid trial ID")
    base = work_base / f"af-durability-fixture-{trial}"
    if phase == "initialize":
        base.mkdir(mode=0o700)
    database = base / "checks.sqlite"
    if phase != "initialize" and not database.is_file():
        raise RuntimeError("missing remote fixture ledger")
    with closing(sqlite3.connect(database)) as db, db:
        if phase == "initialize":
            db.execute("CREATE TABLE checks (id TEXT PRIMARY KEY, revision TEXT, result TEXT)")
            db.execute("CREATE TABLE history (sequence INTEGER PRIMARY KEY, phase TEXT, id TEXT)")
            evidence = {"state": "empty", "fixture": str(base)}
        elif phase == "accept":
            db.execute("INSERT INTO checks VALUES (?, ?, 'pending')", (data["id"], data["revision"]))
            evidence = {"state": "pending", "fixture": str(base), **data}
        elif phase == "finish":
            if data["result"] not in {"green", "red"}:
                raise ValueError("invalid check result")
            cursor = db.execute("UPDATE checks SET result=? WHERE id=? AND revision=? AND result='pending'",
                                (data["result"], data["id"], data["revision"]))
            if cursor.rowcount != 1:
                raise ValueError("missing or already terminal check")
            evidence = {"state": "done", "fixture": str(base), **data}
        else:
            raise ValueError("unknown fixture phase")
        db.execute("INSERT INTO history(phase,id) VALUES (?,?)", (phase, data.get("id")))
        evidence["committed_events"] = [row[0] for row in db.execute("SELECT phase FROM history ORDER BY sequence")]
        evidence["logical_checks"] = db.execute("SELECT count(*) FROM checks").fetchone()[0]
    emit(phase, **evidence)


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("stage", "mark", "restart", "initialize", "accept", "finish"))
    parser.add_argument("path")
    parser.add_argument("data", type=json.loads)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--work-base", type=Path, required=True)
    args = parser.parse_args()
    work_base = private_scratch(args.work_base)
    if args.phase == "stage":
        return 0 if stage(args.binary, Path(args.path), args.data, work_base) else 1
    if args.phase in {"mark", "restart"}:
        root = Path(args.path).resolve()
        if root.parent != work_base or not root.name.startswith("af-durability-"):
            parser.error("expected a trial directory inside the private scratch directory")
    if args.phase == "mark":
        request = json.loads((root / "adapter" / "request.json").read_text())
        if any(args.data[key] != request[key] for key in request):
            raise ValueError("wrong check acknowledgement")
        if args.data["state"] == "done":
            target = root / "adapter" / "receipt.json"
            payload = {**args.data, "received_monotonic": time.monotonic()}
        elif args.data["state"] == "pending":
            target = root / "committed.json"
            payload = args.data
        else:
            raise ValueError("unexpected fixture state")
        pending = target.with_suffix(".tmp")
        write_json(pending, payload)
        pending.replace(target)
        emit("mark", root=str(root), id=request["id"])
    elif args.phase == "restart":
        return 0 if restart(args.binary, root, args.data) else 1
    else:
        remote_ledger(args.path, args.phase, args.data, work_base)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
