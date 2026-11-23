"""Cancel the exact child Task while the private Scenario scheduler is unavailable."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import time

from native_recovery import process, until
from native_smoke import prepare, write_json
from native_writer_loss import edit_fixture


def run(binary, source):
    root, af, env, policy, bundle = prepare(binary, source, "green", edit=edit_fixture(binary))
    repo = root / "repo"
    state = root / "scenario-state"
    task_state = root / "task-state"
    ticket = root / "template-task.json"
    document = json.loads((repo / "ticket.json").read_bytes())
    document["verification"] = "evaluation"
    write_json(ticket, document)
    commit = subprocess.check_output(["git", "-C", repo, "rev-parse", "HEAD"], text=True).strip()
    pin = af("template", "scenario-state", "scenario", "template-pin", "--file", str(ticket),
             "--repo", str(repo), "--task-state", str(task_state), "--authority-commit", commit)
    defn = bundle["definitions"]["clean"]
    defn["steps"][0]["operation"] = {"kind": "fresh_task", "template": pin["template"], "bindings": {}}
    defn["limits"].update(max_wall_ms=260_000)
    defn["steps"][0]["timeout_ms"] = 250_000
    for authority in (policy, defn["authority"]):
        authority["task_plans"] = []
        authority["task_templates"] = [pin["template_id"]]

    ant_id = "child-cancel"
    ant = {
        "schema": "af/ant@12", "id": ant_id, "scenario": bundle, "inputs": {},
        "schedule": {"kind": "interval", "seconds": 10,
                     "start_unix_ms": time.time_ns() // 1_000_000 + 1500},
        "owner": "fixture", "notification": "local:fixture",
        "execution": {"overlap": "skip", "missed": "skip", "lateness_ms": 30_000,
                      "per_run": defn["limits"], "rolling_tokens": 1000,
                      "rolling_window_ms": 300_000},
    }
    policy_file = root / "policy.json"
    ant_file = root / "ant.json"
    write_json(policy_file, policy)
    write_json(ant_file, ant)
    spec = ("--file", str(ant_file), "--authority", str(policy_file))
    validated = af("ant-pin", "scenario-state", "ant", "validate", *spec)
    af("ant-add", "scenario-state", "ant", "add", *spec, "--confirm-pin", validated["revision"])
    af("ant-enabled", "scenario-state", "ant", "enable", ant_id)

    worker_ready = root / "worker-ready"
    argv = [binary, "ant", "scheduler", "run", "--for-seconds", "30", "--state", str(state)]

    def view(label):
        return af(label, "scenario-state", "scenario", "list")["runs"]

    try:
        with process(argv, root / "scheduler-before.log", cwd=repo, env=env) as first:
            until(10, worker_ready.is_file, first, "exact active child Worker")
            first.send_signal(signal.SIGSTOP)
            _, stop_status = os.waitpid(first.pid, os.WUNTRACED)
            if not os.WIFSTOPPED(stop_status):
                raise RuntimeError("coordinator did not stop")
            before, = view("before-cancel").values()
            target, = before["fresh_tasks"].values()
            task_id = target["task_id"]
            task_before = af("task-before-cancel", "task-state", "task", "show", task_id)
            cancelled = af("cancel", "task-state", "task", "cancel", task_id)
            while_down, = view("coordinator-down").values()
            child_stopped = af("task-cancelled", "task-state", "task", "show", task_id)
            write_json(root / "outage.json", {"scheduler_pid": first.pid,
                                             "stopped_signal": os.WSTOPSIG(stop_status)})
        if first.returncode != -9:
            raise RuntimeError("the unavailable scheduler was not replaced")
        restored = time.monotonic()
        (root / "replacement-allowed").touch()

        def settled():
            runs = view("final")
            return runs if len(runs) == 2 and all(
                r["status"] in {"succeeded", "failed", "cancelled"} for r in runs.values()
            ) else None

        with process(argv, root / "scheduler-after.log", cwd=repo, env=env) as second:
            after = until(25, settled, second, "terminal original and next Ant Run")
            elapsed = time.monotonic() - restored
        af("post-restart-ant", "scenario-state", "ant", "show", ant_id)
        original = after[before["id"]]
        other, = (run for id_, run in after.items() if id_ != before["id"])
        other_target, = other["fresh_tasks"].values()
        child_after = af("task-after", "task-state", "task", "show", task_id)
        later = af("next-task", "task-state", "task", "show", other_target["task_id"])
    finally:
        (root / "release-late-reply").touch()

    invariants = {
        "recorded_cancel_bound_to_original_child": cancelled["request"]["task_id"] == task_id
        and child_stopped["state"]["outcome"] == "cancelled",
        "coordinator_made_no_progress_while_stopped": before == while_down,
        "same_original_run": original["execution_id"] == before["execution_id"],
        "original_reaches_nonacceptance": original["status"] in {"failed", "cancelled"},
        "cancelled_child_not_redispatched": child_stopped["history"] == child_after["history"],
        "next_distinct_task_admitted": other_target["task_id"] != task_id
        and other["status"] == "succeeded" and later["state"]["outcome"] == "verified"
        and other["created_unix_ms"] >= original["updated_unix_ms"],
        "original_terminal_steps_settled": all(
            s["status"] in {"failed", "cancelled", "succeeded"}
            for s in original["frames"]["root"]["steps"]
        ),
    }
    report = {
        "case": "cancel-child-during-parent-outage", "invariants": invariants,
        "original": original["status"], "next": other["status"],
        "cancelled_child_before_restart": child_stopped["state"],
        "cancelled_child_after_restart": child_after["state"],
        "attempts": [task_before["attempts"], child_stopped["attempts"],
                     child_after["attempts"], later["attempts"]],
        "after_restart_s": round(elapsed, 3),
        "known_chargeable_tokens": int(child_after["chargeable_tokens"])
        + int(later["chargeable_tokens"]),
        "scheduler_pids": [first.pid, second.pid],
    }
    write_json(root / "cancel-summary.json", report)
    print(json.dumps(report), flush=True)
    return all(invariants.values())


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    raise SystemExit(0 if run(str(Path(args.binary).resolve()), args.source) else 1)
