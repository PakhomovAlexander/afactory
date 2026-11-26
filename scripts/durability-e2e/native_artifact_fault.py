"""Exercise an actual CAS write failure after a committed Task and Checks wait."""
import argparse
from contextlib import closing, contextmanager, nullcontext
import errno
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import time

from native_recovery import configure, ledger, process, settings, until
from native_smoke import prepare, require_linux, write_json


def namespace():
    here = os.readlink("/proc/self/ns/mnt")
    parent = os.environ.get("AF_DURABILITY_PARENT_MOUNT_NS")
    if parent is None:
        os.environ["AF_DURABILITY_PARENT_MOUNT_NS"] = here
        os.execv("/usr/bin/unshare", ["/usr/bin/unshare", "--user", "--map-root-user",
                                     "--mount", "--fork", "--kill-child", sys.executable, *sys.argv])
    if parent == here:
        raise RuntimeError("artifact pressure requires a private mount namespace")


@contextmanager
def full_volume(volume):
    pressure = volume / "fixture-pressure"
    try:
        with pressure.open("xb", buffering=0) as stream:
            try:
                for _ in range(1024):
                    stream.write(bytes(4096))
            except OSError as error:
                pressure_errno = error.errno
                if pressure_errno != errno.ENOSPC:
                    raise
            else:
                raise RuntimeError("private artifact volume did not reach ENOSPC")
        yield pressure_errno
    finally:
        pressure.unlink(missing_ok=True)


def object_set(cas):
    return {
        str(path.relative_to(cas)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in (cas / "objects").glob("*/*") if path.is_file()
    }


def task_logs(state):
    with closing(sqlite3.connect((state / "events.sqlite").as_uri() + "?mode=ro", uri=True)) as db:
        logs = {}
        for run_id, event in db.execute("SELECT run_id, type FROM events ORDER BY run_id, sequence"):
            logs.setdefault(run_id, []).append(event)
        return logs


def second_step(run):
    steps = run.get("frames", {}).get("root", {}).get("steps", [])
    return next((step for step in steps if step.get("step") == "second"), None)


def native_fault(run):
    step = second_step(run)
    if not step or len(step.get("intents", [])) != 1:
        return None
    diagnostic = run.get("failure") or (run.get("waiting") or {}).get("diagnostic", "")
    if "cas io: No space left on device (os error 28)" in diagnostic:
        return step, diagnostic
    return None


def run(binary, source, file_checks=False):
    root, af, env, policy, bundle = prepare(binary, source, "green")
    repo = root / "repo"
    task_state = root / "task-state"
    scenario_state = "scenario-state"
    volume = root / "artifact-volume"
    volume.mkdir(mode=0o700)
    subprocess.run(["mount", "-t", "tmpfs", "-o", "size=4M,nosuid,nodev,noexec",
                    "tmpfs", volume], check=True, timeout=5)
    cas = task_state / "cas"
    shutil.copytree(cas, volume / "cas")
    subprocess.run(["mount", "--bind", volume / "cas", cas], check=True, timeout=5)
    commit = subprocess.check_output(["git", "-C", repo, "rev-parse", "HEAD"], text=True).strip()
    document = json.loads((repo / "ticket.json").read_bytes())
    document["goal"] = "Independently implement the second authorized pagination objective."
    document["verification"] = "evaluation"
    ticket = root / "second-task.json"
    write_json(ticket, document)
    template = af("template", scenario_state, "scenario", "template-pin",
                  "--file", str(ticket), "--repo", str(repo),
                  "--task-state", str(task_state), "--authority-commit", commit)

    fixture = root / "checks-fixture"
    fixture.mkdir(mode=0o700)
    token = os.urandom(24).hex()
    (fixture / "token").write_text(token)
    settings(fixture, result="pending")
    fixture_log = root / "fixture.log"
    scheduler = [binary, "ant", "scheduler", "run", "--for-seconds", "25",
                 "--state", str(root / scenario_state)]

    def view(label):
        runs = af(label, scenario_state, "scenario", "list")["runs"]
        return next(iter(runs.values())) if len(runs) == 1 else None

    def checks_ledger():
        if not file_checks:
            return ledger(fixture)
        receipt = fixture / "check.json"
        return {"checks": [json.loads(receipt.read_bytes())] if receipt.exists() else []}

    def waiting():
        observed = view("before")
        if (observed and observed["status"] == "waiting" and not second_step(observed)
                and observed["frames"]["root"]["steps"][0]["status"] == "succeeded"
                and checks_ledger()["checks"]):
            return observed
        return None

    checks = nullcontext() if file_checks else process(
        [sys.executable, str(Path(__file__).with_name("check_service.py")), str(fixture)],
        fixture_log,
    )
    with checks as service:
        port = None
        if service:
            _, port = json.loads(until(5, lambda: fixture_log.read_text().splitlines(),
                                       service, "Checks fixture")[0])["listening"]
        configure(root, policy, bundle, f"http://127.0.0.1:{port}", token)
        if file_checks:
            adapter = Path(policy["adapters"]["checks"]["executable"])
            shutil.copyfile(Path(__file__).with_name("artifact_file_checks.py"), adapter)
            policy["adapters"]["checks"]["digest"] = "sha256:" + hashlib.sha256(adapter.read_bytes()).hexdigest()
        ant_file = root / "ant.json"
        ant = json.loads(ant_file.read_text())
        definition = ant["scenario"]["definitions"]["clean"]
        definition["steps"].append({
            "id": "second", "operation": {"kind": "fresh_task",
                                          "template": template["template"], "bindings": {}},
            "inputs": {}, "outputs": {"result": "task"}, "when": None,
            "max_attempts": 1, "timeout_ms": 60_000, "on_timeout": "fail",
        })
        definition["outputs"]["second"] = "task"
        definition["result"]["second"] = {"source": "step", "step": "second", "port": "result"}
        definition["limits"]["max_tokens"] = 2000
        ant["execution"].update(per_run=definition["limits"], rolling_tokens=2000)
        policy["task_templates"] = [template["template_id"]]
        definition["authority"]["task_templates"] = [template["template_id"]]
        write_json(ant_file, ant)
        policy_file = root / "policy.json"
        write_json(policy_file, policy)
        spec = ("--file", str(ant_file), "--authority", str(policy_file))
        pin = af("ant-pin", scenario_state, "ant", "validate", *spec)
        af("ant-add", scenario_state, "ant", "add", *spec, "--confirm-pin", pin["revision"])
        af("ant-enabled", scenario_state, "ant", "enable", "durability")

        with process(scheduler, root / "scheduler-before.log", cwd=repo, env=env) as first:
            try:
                before = until(15, waiting, first, "verified Task and pending Checks")
            except (RuntimeError, TimeoutError):
                af("task-before-failure", "task-state", "task", "show", "pagination-cli")
                raise
        if first.returncode != -9:
            raise RuntimeError("first scheduler did not stop at the committed wait")
        known_objects = object_set(cas)
        write_json(root / "cas-before.json", known_objects)
        logs_before = task_logs(task_state)
        write_json(root / "task-logs-before.json", logs_before)
        first_task = af("task-before", "task-state", "task", "show", "pagination-cli")

        with full_volume(volume) as pressure_errno:
            settings(fixture, result="green")

            def observe_fault():
                run = view("poll-full")
                return run if run and native_fault(run) else None

            with process(scheduler, root / "scheduler-full.log", cwd=repo, env=env) as blocked:
                on_disk_full = until(12, observe_fault, blocked, "native CAS ENOSPC on second Task")
                full_step, diagnostic = native_fault(on_disk_full)
                intent = full_step["intents"][0]
                task_id = "scenario-" + intent["id"].removeprefix("sha256:")
                write_json(root / "at-full.json", on_disk_full)
                write_json(root / "cas-fault.json", {
                    "errno": errno.ENOSPC, "pressure_errno": pressure_errno,
                    "native_diagnostic": diagnostic, "step": "root/second",
                    "execution_id": on_disk_full["execution_id"], "intent_id": intent["id"],
                    "task_id": task_id, "deadline_unix_ms": full_step["deadline_unix_ms"],
                })
                if on_disk_full["status"] == "waiting":
                    until(8, lambda: (observed := observe_fault()) and
                          observed["updated_unix_ms"] > on_disk_full["updated_unix_ms"],
                          blocked, "repeat poll with CAS still full")
            reopened = view("reopened-full")
            full_objects = object_set(cas)
            write_json(root / "cas-at-full.json", full_objects)
            logs_full = task_logs(task_state)
            write_json(root / "task-logs-at-full.json", logs_full)
            first_full = af("task-at-full", "task-state", "task", "show", "pagination-cli")

        restored = time.monotonic()
        probe = scheduler.copy()
        probe[probe.index("--for-seconds") + 1] = "8"
        restarted = subprocess.run(probe, cwd=repo, env=env, capture_output=True,
                                   text=True, timeout=18)
        (root / "scheduler-restored.log").write_text(restarted.stdout + restarted.stderr)
        after = view("after")
        elapsed = time.monotonic() - restored
        write_json(root / "checks-after.json", checks_ledger())

    latest_objects = object_set(cas)
    write_json(root / "cas-after.json", latest_objects)
    logs_after = task_logs(task_state)
    write_json(root / "task-logs-after.json", logs_after)
    after_task = af("task-after", "task-state", "task", "show", "pagination-cli")
    after_step = second_step(after)
    bound = after["fresh_tasks"].get(intent["id"])
    second_task = af("second-task-after", "task-state", "task", "show", task_id) if bound else {}
    history = [row.get("transition", {}).get("change", {}).get("kind")
               for row in second_task.get("history", [])]
    added_logs = [events for log_id, events in logs_after.items() if log_id not in logs_before]
    executions = sum(events.count("TaskExecutionStarted@1") for events in added_logs)
    waiting = on_disk_full["waiting"] or {}
    evidence = {
        "same_scenario": all(before[key] == on_disk_full[key] == reopened[key] == after[key]
                             for key in ("execution_id", "created_unix_ms", "limits", "pin")),
        "prior_task_unchanged": (first_task["result"]["acceptance"] == "satisfied" and all(
            first_task[key] == first_full[key] == after_task[key]
            for key in ("history", "result", "chargeable_tokens", "attempts", "plan_id", "revision_id")
        )),
        "checks_completed_before_fault": all(
            step["status"] == "succeeded"
            for step in on_disk_full["frames"]["root"]["steps"] if step["step"] != "second"
        ),
        "prior_cas_objects_unchanged": bool(known_objects) and all(
            full_objects.get(name) == latest_objects.get(name) == digest
            for name, digest in known_objects.items()
        ),
        "active_wait_durable": (
            on_disk_full["status"] == reopened["status"] == full_step["status"] == "waiting"
            and waiting.get("kind") == "reconcile" and waiting.get("step") == "root/second"
            and waiting.get("intent_id") == intent["id"] and intent["phase"] == "prepared"
            and reopened["waiting"] == waiting and second_step(reopened) == full_step
            and reopened["execution_id"] == on_disk_full["execution_id"]
            and reopened["dispatches"] == on_disk_full["dispatches"]
        ),
        "no_task_bound_or_dispatched_at_capacity": (
            logs_full == logs_before and not on_disk_full["fresh_tasks"] and not reopened["fresh_tasks"]
            and not full_step["outputs"]
        ),
        "normal_scheduler_completed": restarted.returncode == 0,
        "same_scenario_recovers": after["status"] == "succeeded",
        "same_intent_and_deadline": bool(after_step and len(after_step["intents"]) == 1 and
            after_step["intents"][0]["id"] == intent["id"] and after_step["intents"][0]["phase"] == "done"
            and after_step["intents"][0]["input_digest"] == intent["input_digest"]
            and after_step["deadline_unix_ms"] == full_step["deadline_unix_ms"]
            and diagnostic in after_step["intents"][0]["evidence"]),
        "second_task_completed_once": bool(bound and len(after["fresh_tasks"]) == 1
            and set(logs_before) < set(logs_after) and len(logs_after) == len(logs_before) + 1
            and bound["task_id"] == task_id == second_task["task_id"]
            and bound["plan_id"] == second_task["plan_id"]
            and bound["revision_id"] == second_task["revision_id"]
            and history.count("opened") == history.count("plan_proposed") == executions == 1
            and (second_task.get("result") or {}).get("acceptance") == "satisfied"),
    }
    with Path(binary).open("rb") as executable:
        binary_digest = hashlib.file_digest(executable, "sha256").hexdigest()
    summary = {"case": "artifact-cas-enospc", "evidence": evidence, "scenario": after["status"],
               "status_at_full": on_disk_full["status"], "binary_sha256": binary_digest,
               "checks_transport": "file" if file_checks else "http",
               "known_cas_objects": len(known_objects), "after_restore_s": round(elapsed, 3),
               "failure": after["failure"]}
    write_json(root / "artifact-summary.json", summary)
    print(json.dumps(summary), flush=True)
    return all(evidence.values())


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("source", type=Path)
    parser.add_argument("--file-checks", action="store_true",
                        help="use the private file-backed Checks fixture when loopback is unavailable")
    args = parser.parse_args()
    require_linux()
    namespace()
    raise SystemExit(0 if run(str(Path(args.binary).resolve()), args.source, args.file_checks) else 1)
