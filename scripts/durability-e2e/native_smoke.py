"""Exercise a real child Task and local gate in a private native Scenario."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def literal(kind, value):
    return {"source": "literal", "value": {"type": kind, "value": value}}


def write_json(path, value):
    path.write_text(json.dumps(value) + "\n")


def require_linux():
    if sys.platform != "linux":
        raise SystemExit("native durability trials require Linux")


def private_scratch(path=None):
    require_linux()
    path = path or os.environ.get("AF_DURABILITY_SCRATCH")
    if not path:
        raise SystemExit("set AF_DURABILITY_SCRATCH to an existing private scratch directory")
    root = Path(path).resolve()
    if not root.is_dir() or root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077:
        raise SystemExit(f"scratch directory must be owned by the trial user and private: {root}")
    return root


def new_trial(prefix="af-durability-", *, work_base=None):
    root = Path(tempfile.mkdtemp(prefix=prefix, dir=private_scratch(work_base)))
    print(f"evidence_dir={root}", flush=True)
    return root


def prepare(binary, source, gate, *, edit=None, work_base=None):
    root = new_trial(work_base=work_base)
    repo = root / "repo"
    shutil.copytree(source / "fixtures/task-runtime/pagination", repo)
    if gate == "red":
        policy_path = repo / ".af" / "code-policy.toml"
        check = policy_path.read_text()
        if check.count("== [2,3,4]") != 1:
            raise ValueError("pagination gate fixture changed")
        policy_path.write_text(check.replace("== [2,3,4]", "== [2,3,5]"))
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "HOME": str(root / "home"),
        "TMPDIR": str(root / "scratch"),
        "AF_SELF_OFFLINE": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "LANG": "C.UTF-8",
    }
    (root / "home").mkdir(mode=0o700)
    (root / "scratch").mkdir(mode=0o700)

    def command(argv):
        return subprocess.check_output(argv, cwd=repo, env=env, text=True, timeout=90)

    def af(label, state, *argv):
        data = command([binary, *argv, "--json", "--state", str(root / state)])
        (root / f"{label}.json").write_text(data)
        return json.loads(data)

    if edit:
        edit(root, repo, command)
    for args in (["init", "-q", "-b", "main"], ["config", "user.name", "Fixture"],
                 ["config", "user.email", "fixture@example.invalid"],
                 ["add", "-A"], ["commit", "-qm", "Durability fixture"]):
        command(["git", *args])
    plan = af("task-plan", "task-state", "task", "plan", "--file", "ticket.json")
    target = {
        "repo": str(repo), "state": str(root / "task-state"),
        "task_id": "pagination-cli", "plan_id": plan["plan_id"],
        "revision_id": plan["revision_id"], "max_tokens": 1000,
    }
    policy = {"schema": "af/scenario-policy@8", "adapters": {},
              "task_plans": [plan["plan_id"]], "task_templates": [],
              "file_roots": [], "decision_keys": {}}
    output = {"source": "step", "step": "implement", "port": "result"}
    definition = {
        "schema": "af/scenario@10", "admission": None, "inputs": {},
        "outputs": {"candidate": "task"},
        "steps": [{
            "id": "implement", "operation": {"kind": "task", "target": target},
            "inputs": {}, "outputs": {"result": "task"}, "when": None,
            "max_attempts": 1, "timeout_ms": 90_000, "on_timeout": "fail",
        }],
        "result": {"candidate": output}, "completion": [literal("boolean", True)],
        "authority": {"task_plans": [plan["plan_id"]], "task_templates": [],
                      "grants": [], "decision_owners": []},
        "limits": {"max_tokens": 1000, "max_wall_ms": 95_000, "max_dispatches": 4},
    }
    bundle = {"schema": "af/scenario-bundle@10", "root": "clean",
              "definitions": {"clean": definition}}
    return root, af, env, policy, bundle


def run(binary, source, gate, *, work_base=None):
    started = time.monotonic()
    root, af, _, policy, bundle = prepare(binary, source, gate, work_base=work_base)
    policy_file = root / "policy.json"
    write_json(policy_file, policy)
    policy_file.chmod(0o600)
    bundle_file = root / "scenario.json"
    inputs = root / "inputs.json"
    write_json(bundle_file, bundle)
    write_json(inputs, {})
    spec = ("--file", str(bundle_file), "--authority", str(policy_file))
    pin = af("pin", "scenario-state", "scenario", "validate", *spec)
    af("start", "scenario-state", "scenario", "start", "clean", *spec,
       "--inputs", str(inputs), "--confirm-pin", pin["revision"], "--execute")
    observed = af("observed", "scenario-state", "scenario", "show", "clean")
    task = af("task", "task-state", "task", "show", "pagination-cli")
    if gate == "red":
        af("task-checks", "task-state", "task", "show", "pagination-cli", "--checks")
    summary = {
        "scenario_status": observed["status"],
        "fixture_gate": gate,
        "child_status": observed["frames"]["root"]["steps"][0]["status"],
        "attempts": task["attempts"],
        "chargeable_tokens": task["chargeable_tokens"],
        "task_state": task["state"],
        "task_failures": task.get("failures", []),
        "scenario_wait": observed["waiting"],
        "duration_s": round(time.monotonic() - started, 3),
        "fixture_checkout": subprocess.check_output(
            ["git", "-C", source, "rev-parse", "HEAD"], text=True).strip(),
    }
    with Path(binary).open("rb") as executable:
        summary["binary_sha256"] = hashlib.file_digest(executable, "sha256").hexdigest()
    outputs = observed["frames"]["root"]["outputs"] or {}
    candidate = outputs.get("candidate")
    if candidate:
        summary["acceptance"] = candidate["value"]["result"]["acceptance"]
    write_json(root / "summary.json", summary)
    print(json.dumps(summary), flush=True)
    if gate == "red":
        return (summary["scenario_status"] == "failed" and
                summary["task_state"].get("outcome") == "gate_failed")
    return (summary["scenario_status"] == "succeeded" and
            summary.get("acceptance") == "satisfied" and summary["attempts"] > 0)


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("source", type=Path)
    parser.add_argument("gate", choices=("green", "red"))
    parser.add_argument("--work-base", type=Path)
    args = parser.parse_args()
    raise SystemExit(0 if run(str(Path(args.binary).resolve()), args.source, args.gate,
                             work_base=args.work_base) else 1)
