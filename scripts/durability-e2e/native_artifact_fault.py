"""Exercise an actual CAS write failure after a committed Task and Checks wait."""
import argparse
from contextlib import contextmanager
import errno
import hashlib
import json
import os
from pathlib import Path
import shutil
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
                if error.errno != errno.ENOSPC:
                    raise
            else:
                raise RuntimeError("private artifact volume did not reach ENOSPC")
        yield
    finally:
        pressure.unlink(missing_ok=True)


def object_set(cas):
    return {
        str(path.relative_to(cas)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in (cas / "objects").glob("*/*") if path.is_file()
    }


def run(binary, source):
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

    def waiting():
        observed = view("before")
        if observed and observed["status"] == "waiting" and ledger(fixture)["checks"]:
            return observed
        return None

    def terminal(label):
        observed = view(label)
        return observed if observed and observed["status"] in {"succeeded", "failed", "cancelled"} else None

    with process([sys.executable, str(Path(__file__).with_name("check_service.py")), str(fixture)],
                 fixture_log) as service:
        _, port = json.loads(until(5, lambda: fixture_log.read_text().splitlines(),
                                   service, "Checks fixture")[0])["listening"]
        configure(root, policy, bundle, f"http://127.0.0.1:{port}", token)
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
            before = until(15, waiting, first, "verified Task and pending Checks")
        if first.returncode != -9:
            raise RuntimeError("first scheduler did not stop at the committed wait")
        known_objects = object_set(cas)
        first_task = af("task-before", "task-state", "task", "show", "pagination-cli")

        with full_volume(volume):
            settings(fixture, result="green")
            with process(scheduler, root / "scheduler-full.log", cwd=repo, env=env) as blocked:
                on_disk_full = until(12, lambda: terminal("at-full"), blocked,
                                     "terminal or visible CAS write failure")
            diagnostic = on_disk_full.get("failure") or ""
            if "No space left on device" not in diagnostic:
                raise RuntimeError(f"AF did not witness publication ENOSPC: {diagnostic}")
            write_json(root / "cas-fault.json", {"errno": errno.ENOSPC, "native_diagnostic": diagnostic})

        restored = time.monotonic()
        probe = scheduler.copy()
        probe[probe.index("--for-seconds") + 1] = "3"
        restarted = subprocess.run(probe, cwd=repo, env=env, capture_output=True,
                                   text=True, timeout=10)
        (root / "scheduler-restored.log").write_text(restarted.stdout + restarted.stderr)
        after = view("after")
        elapsed = time.monotonic() - restored

    latest_objects = object_set(cas)
    after_task = af("task-after", "task-state", "task", "show", "pagination-cli")
    evidence = {
        "same_scenario": after["execution_id"] == before["execution_id"],
        "prior_task_unchanged": first_task["history"] == after_task["history"],
        "prior_cas_objects_unchanged": bool(known_objects) and all(
            latest_objects.get(name) == digest for name, digest in known_objects.items()
        ),
        "no_false_acceptance_on_full_volume": on_disk_full["status"] == "failed",
        "normal_scheduler_completed": restarted.returncode == 0,
        "same_scenario_recovers": after["status"] == "succeeded",
    }
    summary = {"case": "artifact-cas-enospc", "evidence": evidence, "scenario": after["status"],
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
    args = parser.parse_args()
    require_linux()
    namespace()
    raise SystemExit(0 if run(str(Path(args.binary).resolve()), args.source) else 1)
