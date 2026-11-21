"""Prove that a waiting native Ant holds one slot, then admits distinct work."""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from native_recovery import configure, ledger, process, settings, until
from native_smoke import prepare, write_json


def run(binary, source, pause):
    root, af, env, policy, bundle = prepare(binary, source, "green")
    repo = root / "repo"
    commit = subprocess.check_output(["git", "-C", repo, "rev-parse", "HEAD"], text=True).strip()
    ticket = root / "fixture-task.json"
    document = json.loads((repo / "ticket.json").read_bytes())
    document["verification"] = "evaluation"
    write_json(ticket, document)
    captured = af("template", "scenario-state", "scenario", "template-pin",
                  "--file", str(ticket), "--repo", str(repo),
                  "--task-state", str(root / "task-state"), "--authority-commit", commit)
    template = captured["template_id"]
    definition = bundle["definitions"]["clean"]
    definition["steps"][0]["operation"] = {
        "kind": "fresh_task", "template": captured["template"], "bindings": {},
    }
    for authority in (policy, definition["authority"]):
        authority["task_plans"] = []
        authority["task_templates"] = [template]

    fixtures = root / "checks-fixture"
    fixtures.mkdir(mode=0o700)
    token = os.urandom(24).hex()
    (fixtures / "token").write_text(token)
    settings(fixtures, result="pending")
    log = root / "fixture.log"
    scheduler = [binary, "ant", "scheduler", "run", "--for-seconds", "30",
                 "--state", str(root / "scenario-state")]

    def inspect(label):
        ant = af(label + "-ant", "scenario-state", "ant", "show", "durability")
        runs = af(label + "-runs", "scenario-state", "scenario", "list")["runs"]
        return ant, runs

    def blocked():
        ant, runs = inspect("initial")
        dispositions = [entry["outcome"]["kind"] for entry in ant["occurrences"]]
        if "skipped_busy" in dispositions and len(runs) == 1:
            run = next(iter(runs.values()))
            if run["status"] == "waiting":
                return ant, run
        return None

    def settled():
        ant, runs = inspect("recovered")
        if len(runs) >= 2 and all(entry["status"] == "succeeded" for entry in runs.values()):
            return ant, runs
        return None

    def paused():
        ant, runs = inspect("paused-after-restart")
        if (len(runs) == 1 and next(iter(runs.values()))["status"] == "succeeded"
                and ant["state"] == "paused"
                and any(entry["outcome"]["kind"] == "skipped_paused" for entry in ant["occurrences"])):
            return ant
        return None

    with process([sys.executable, str(Path(__file__).with_name("check_service.py")),
                  str(fixtures)], log) as service:
        ready = until(5, lambda: log.read_text().splitlines(), service, "Checks fixture")
        _, port = json.loads(ready[0])["listening"]
        spec = configure(root, policy, bundle, f"http://127.0.0.1:{port}", token,
                         period_seconds=4, distinct_checks=True)
        pin = af("ant-pin", "scenario-state", "ant", "validate", *spec)
        af("ant-add", "scenario-state", "ant", "add", *spec, "--confirm-pin", pin["revision"])
        af("ant-enabled", "scenario-state", "ant", "enable", "durability")
        with process(scheduler, root / "scheduler-before.log", cwd=repo, env=env) as first:
            before_ant, before_run = until(16, blocked, first, "native skipped_busy receipt")
            if pause:
                af("ant-paused", "scenario-state", "ant", "pause", "durability")
        if first.returncode != -9:
            raise RuntimeError("first scheduler was not killed")
        settings(fixtures, result="green")
        restored = time.monotonic()
        paused_state = None
        with process(scheduler, root / "scheduler-after.log", cwd=repo, env=env) as second:
            if pause:
                paused_state = until(12, paused, second, "persisted pause after restart")
                af("ant-resumed", "scenario-state", "ant", "enable", "durability")
            final_ant, final_runs = until(15, settled, second, "two distinct admitted Scenarios")
            elapsed = time.monotonic() - restored
        truth = ledger(fixtures)

    originals = final_runs[before_run["id"]]
    other, = (value for key, value in final_runs.items() if key != before_run["id"])
    previous_creations = [item["outcome"]["run_id"] for item in before_ant["occurrences"]
                          if item["outcome"]["kind"] == "started"]
    dispositions = Counter(item["outcome"]["kind"] for item in final_ant["occurrences"])
    parent_tasks = {
        run["id"]: [t["task_id"] for t in run["fresh_tasks"].values()]
        for run in (originals, other)
    }
    for index, task_id in enumerate(sorted({task for ids in parent_tasks.values() for task in ids})):
        af(f"admitted-task-{index}", "task-state", "task", "show", task_id)

    recorded = root / "admission-ledger.json"
    write_json(recorded, truth)
    checks = {
        "one_run_while_busy": previous_creations == [before_run["id"]],
        "persisted_busy_receipt": any(item in final_ant["occurrences"]
                                     for item in before_ant["occurrences"]
                                     if item["outcome"]["kind"] == "skipped_busy"),
        "later_admission": dispositions["started"] == 2
        and other["created_unix_ms"] >= originals["updated_unix_ms"],
        "original_execution_preserved": originals["execution_id"] == before_run["execution_id"],
        "distinct_native_tasks": len({task for tasks in parent_tasks.values() for task in tasks}) == 2,
        "two_external_checks": len(truth["checks"]) == 2
        and sum(c["action"] == "create" for c in truth["calls"]) == 2,
        "normal_scheduler_replaced": first.pid != second.pid,
    }
    if pause:
        checks["paused_occurrence_persisted_without_admission"] = (
            dispositions["skipped_paused"] >= 1
            and sum(item["outcome"]["kind"] == "started" for item in paused_state["occurrences"]) == 1
        )
    summary = {"checks": checks, "parents": parent_tasks, "occurrences": dict(dispositions),
               "restored_to_second_completed_s": round(elapsed, 3),
               "scope": "single-active-Run Ant overlap slot", "tested_pause": pause}
    write_json(root / "admission-summary.json", summary)
    print(json.dumps(summary), flush=True)
    return all(checks.values())


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("source", type=Path)
    parser.add_argument("--pause", action="store_true")
    args = parser.parse_args()
    raise SystemExit(0 if run(str(Path(args.binary).resolve()), args.source, args.pause) else 1)
