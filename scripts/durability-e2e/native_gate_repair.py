"""Exercise a real failing gate and a captured Scenario repair."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

from native_smoke import literal, new_trial, write_json


def ref(step):
    return {"source": "step", "step": step, "port": "result"}


def project(value, kind):
    return {"source": "project", "value": value, "projection": {"kind": kind}}


def fresh(task, label, inputs, when):
    return {
        "id": label, "operation": {"kind": "fresh_task", "template": task, "bindings": {}},
        "inputs": inputs, "outputs": {"result": "task_outcome"}, "when": when,
        "max_attempts": 1, "timeout_ms": 90_000, "on_timeout": "fail",
    }


def run(binary, source, fixture_scripts):
    root = new_trial("af-durability-repair-")
    fixture_scripts = fixture_scripts.resolve()
    repo = root / "repo"
    shutil.copytree(source / "fixtures/task-runtime/continue", repo)
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(root / "home"),
        "TMPDIR": str(root / "scratch"), "AF_SELF_OFFLINE": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
        "LANG": "C.UTF-8",
    }
    for name in ("home", "scratch"):
        (root / name).mkdir(mode=0o700)

    def command(argv):
        return subprocess.check_output(argv, cwd=repo, env=env, text=True, timeout=100)

    def af(name, state, *argv):
        raw = command([binary, *argv, "--state", str(root / state), "--json"])
        (root / (name + ".json")).write_text(raw)
        return json.loads(raw)

    generator = (fixture_scripts / "fixtures/scenarios/impl/generate.py").read_text()
    expression = "MODE == 'native-gates' and stage < 2"
    if generator.count(expression) != 1:
        raise ValueError("unexpected upstream native gate fixture")
    # The only variation from the upstream command fixture is one failing stage.
    fixture = root / "generate.py"
    fixture.write_text(generator.replace(expression, "MODE == 'native-gates' and stage < 1"))
    command(["/usr/bin/python3", "-B", "-I", fixture, fixture_scripts,
             repo, binary, "durability", "native-gates"])
    (repo / "NOTES.md").write_text("Fixture pagination command.\n")
    for args in (["init", "-q", "-b", "main"], ["config", "user.name", "Fixture"],
                 ["config", "user.email", "fixture@example.invalid"], ["add", "-A"],
                 ["commit", "-qm", "One sealed candidate and one repair"]):
        command(["git", *args])
    authority_commit = command(["git", "rev-parse", "HEAD"]).strip()
    document = json.loads((repo / "first.json").read_text())
    document["pipeline"]["name"] = "durability/implementation"
    document["facts"]["standard"] = True
    document["limits"].update(max_attempts=16, wall_ms=90_000)
    document["limits"]["verification"].update(attempts=10, wall_ms=35_000)
    ticket = root / "template.json"
    write_json(ticket, document)
    state = root / "task-state"
    captured = af("template-pin", "scenario-state", "scenario", "template-pin", "--file", str(ticket),
                  "--repo", str(repo), "--task-state", str(state), "--authority-commit", authority_commit)
    template_id = captured["template_id"]
    template = captured["template"]
    accepted_initial = project(ref("first"), "task_accepted")
    candidate = {
        "source": "select", "condition": accepted_initial,
        "when_true": project(ref("first"), "accepted_task"),
        "when_false": project(ref("repair"), "accepted_task"),
    }
    definition = {
        "schema": "af/scenario@11", "admission": None, "inputs": {},
        "outputs": {"candidate": "task"},
        "steps": [fresh(template, "first", {}, None),
                  fresh(template, "repair", {"repair": ref("first")},
                        project(ref("first"), "task_repairable"))],
        "result": {"candidate": candidate},
        "completion": [{"source": "select", "condition": accepted_initial,
                        "when_true": literal("boolean", True),
                        "when_false": project(ref("repair"), "task_accepted")}],
        "authority": {"task_plans": [], "task_templates": [template_id],
                      "grants": [], "decision_owners": []},
        "limits": {"max_tokens": 2000, "max_wall_ms": 180_000, "max_dispatches": 8},
    }
    policy = root / "policy.json"
    write_json(policy, {"schema": "af/scenario-policy@8", "adapters": {}, "task_plans": [],
                        "task_templates": [template_id], "file_roots": [], "decision_keys": {}})
    policy.chmod(0o600)
    bundle = root / "scenario.json"
    write_json(bundle, {"schema": "af/scenario-bundle@11", "root": "gate-repair",
                        "definitions": {"gate-repair": definition}})
    inputs = root / "inputs.json"
    write_json(inputs, {})
    spec = ("--file", str(bundle), "--authority", str(policy))
    pin = af("scenario-pin", "scenario-state", "scenario", "validate", *spec)
    started = af("start", "scenario-state", "scenario", "start", "gate-repair", *spec,
                 "--confirm-pin", pin["revision"], "--inputs", str(inputs), "--execute")
    observed = af("observed", "scenario-state", "scenario", "show", "gate-repair")

    records = {}
    for step in observed["frames"]["root"]["steps"]:
        outcome = step["outputs"].get("result", {}).get("value")
        if outcome:
            task_id = outcome["receipt"]["task_id"]
            label = step["step"]
            task = af(label + "-task", "task-state", "task", "show", task_id)
            gate = af(label + "-checks", "task-state", "task", "show", task_id, "--checks")
            records[label] = (outcome, task, gate)

    first, first_task, first_gate = records.get("first", ({}, {}, {}))
    repair, repair_task, repair_gate = records.get("repair", ({}, {}, {}))
    receipt_a = first_gate.get("receipt") or {}
    receipt_b = repair_gate.get("receipt") or {}
    snapshot_a = first.get("candidate_snapshot_id")
    snapshot_b = repair.get("candidate_snapshot_id")
    accepted = ((observed["frames"]["root"]["outputs"] or {}).get("candidate") or {}).get("value", {})
    exact_snapshot = accepted.get("result", {}).get("outputs", {}).get("snapshot", {}).get("snapshot_id")
    checks = {
        "first_real_gate_failed": first.get("outcome") == "gate_failed"
        and first.get("cause") == "check_failed" and receipt_a.get("outcome") == "failed"
        and first.get("checks_id") == receipt_a.get("receipt_id"),
        "one_distinct_repair_task": len(observed["fresh_tasks"]) == 2
        and first_task.get("task_id") != repair_task.get("task_id"),
        "original_failure_receipt_retained": first_task.get("state", {}).get("outcome") == "gate_failed"
        and any(check["exit_code"] == 1 for check in receipt_a.get("checks", [])),
        "new_candidate_verified": bool(snapshot_a) and bool(snapshot_b)
        and snapshot_a != snapshot_b and snapshot_b == exact_snapshot
        and repair.get("outcome") == "verified" and receipt_b.get("outcome") == "passed"
        and repair.get("checks_id") == receipt_b.get("receipt_id")
        and first.get("checks_id") != repair.get("checks_id"),
        "parent_succeeded_on_same_run": observed["status"] == "succeeded"
        and started["execution_id"] == observed["execution_id"],
    }
    with Path(binary).open("rb") as executable:
        digest = hashlib.file_digest(executable, "sha256").hexdigest()
    summary = {"scenario": observed["status"], "execution_id": observed["execution_id"],
               "binary_sha256": digest, "authority_commit": authority_commit, "snapshot_a": snapshot_a,
               "snapshot_b": snapshot_b, "first_check": first.get("checks_id"),
               "repair_check": repair.get("checks_id"), "checks": checks,
               "attempts": [task.get("attempts") for task in (first_task, repair_task)],
               "source_fixture_commit": subprocess.check_output(
                   ["git", "-C", source, "rev-parse", "HEAD"], text=True).strip(),
               "chargeable_tokens": sum(int(task.get("chargeable_tokens", 0))
                                        for task in (first_task, repair_task))}
    write_json(root / "repair-summary.json", summary)
    print(json.dumps(summary), flush=True)
    return all(checks.values())


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("source", type=Path)
    parser.add_argument("fixture_scripts", type=Path, nargs="?")
    args = parser.parse_args()
    ok = run(str(Path(args.binary).resolve()), args.source, args.fixture_scripts or args.source)
    raise SystemExit(0 if ok else 1)
