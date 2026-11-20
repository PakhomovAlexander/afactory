"""Verify retained cross-VM evidence and emit an engine-comparison report."""
import argparse
from contextlib import closing
from hashlib import sha256
import json
from pathlib import Path
import sqlite3

from compare import SCHEMA
from check_trace import check
from distributed_trial import event


def read(root, name):
    return json.loads((root / name).read_bytes())


def digest(value):
    return sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def export(root, manifest, trace_path=None):
    if manifest["schema"] != "af-durability/trial-inputs@1":
        raise ValueError("unsupported pairing manifest")
    report = read(root, "control/distributed-report.json")
    if report["schema"] != "af-durability/broker-recovery@1":
        raise ValueError("unsupported native distributed evidence")
    ledger = report["external_ledger"]
    initial = event(read(root, "control/initialize/logs.json"), "initialize")
    accepted = event(read(root, "control/accept/logs.json"), "accept")
    marked = event(read(root, "control/mark/logs.json"), "mark")
    finished = event(read(root, "control/finish/logs.json"), "finish")
    outbox = read(root, "control/stage/outbox.json")
    request = outbox["request"]
    down, terminal = report["down"], report["terminal"]
    if not all((initial, accepted, marked, finished)):
        raise ValueError("incomplete broker evidence")
    with closing(sqlite3.connect((root / "fixture/checks.sqlite").resolve().as_uri() + "?mode=ro",
                                 uri=True)) as database:
        checks = database.execute("SELECT id, revision, result FROM checks ORDER BY id").fetchall()
        history = database.execute("SELECT phase,id FROM history ORDER BY sequence").fetchall()
    before_runs = read(root, "engine/before.json")["runs"]
    after_runs = read(root, "engine/after.json")["runs"]
    before, = before_runs.values()
    after, = after_runs.values()
    task_before = read(root, "engine/task-before.json")
    task_after = read(root, "engine/task-after.json")
    ant = read(root, "engine/ant.json")
    limits = ant["execution"]["per_run"]
    if any(limits[key] != manifest["workload"][key] for key in limits):
        raise ValueError("captured Scenario limits differ from the pairing manifest")
    if (report["engine"] != manifest["placement"]["engine"]
            or report["fixture"] != manifest["placement"]["fixture"]):
        raise ValueError("observed placement differs from the pairing manifest")
    fixture_tree = outbox.get("fixture_git_tree")
    if fixture_tree is None:
        provenance = read(root, "engine/provenance.json")
        if provenance["host"] != outbox["host"] or provenance["repository"] != outbox["root"] + "/repo":
            raise ValueError("retained fixture provenance points to a different trial")
        fixture_tree = provenance["fixture_git_tree"]
    if fixture_tree != manifest["workload"]["fixture_git_tree"]:
        raise ValueError("captured fixture Git tree differs from the pairing manifest")
    steps_before = before["frames"]["root"]["steps"]
    steps_after = after["frames"]["root"]["steps"]
    expected_outcome = {"green": "succeeded", "red": "failed"}[manifest["fault"]["check_result"]]

    valid_chain = (
        initial["state"] == "empty" and accepted["state"] == "pending" and finished == ledger
        and initial["host"] == accepted["host"] == finished["host"] != outbox["host"]
        and before["status"] == "waiting" and before["waiting"]["kind"] == "reconcile"
        and marked["root"] == outbox["root"] and marked["id"] == request["id"]
    )
    marker = json.loads(read(root, "control/mark/request.json")["argv"][6])
    control = manifest["expectation"] == "control"
    if control:
        valid_chain = (
            valid_chain and down is None and report["fault"] is None and marker == finished
            and terminal["pid"] == outbox["pid"]
            and event(read(root, "control/stage/logs.json"), "terminal") == terminal
        )
        execution = outbox["execution"]
    else:
        valid_chain = (
            valid_chain and down["killed"] == -9 and down["pid"] != terminal["pid"]
            and event(read(root, "control/stage/logs.json"), "down") == down
            and marker == accepted
        )
        execution = down["execution"]
    candidate = steps_before[0]["outputs"]["result"]["value"]["result"]["outputs"]["snapshot"]["snapshot_id"]
    assertions = {
        "Scenario was resubmitted or replaced": (
            before["execution_id"] == after["execution_id"] == execution
            == terminal["execution"]
        ),
        "wrong business outcome": after["status"] == expected_outcome == terminal["scenario"],
        "terminal Scenario has an unsettled step": all(
            step["status"] in {"succeeded", "failed", "cancelled"} for step in steps_after
        ),
        "first child has no accepted local gate result": (
            task_before["state"]["outcome"] == "verified"
            and task_before["result"]["acceptance"] == "satisfied"
        ),
        "local candidate changed": steps_before[0]["outputs"] == steps_after[0]["outputs"],
        "exact check changed or was duplicated": request["revision"] == candidate and checks == [
            (request["id"], request["revision"], manifest["fault"]["check_result"])
        ] and history == [("initialize", None), ("accept", request["id"]), ("finish", request["id"])],
        "Task history or charge changed": all(task_before[key] == task_after[key]
                                             for key in ("history", "chargeable_tokens")),
    }
    violations = [message for message, valid in assertions.items() if not valid]
    if valid_chain:
        phases = [("fixture.empty", "control/initialize/logs.json", initial["host"])]
        if assertions["first child has no accepted local gate result"]:
            phases.append(("child.verified", "engine/task-before.json", outbox["host"]))
        phases.extend([("check.requested", "control/stage/outbox.json", outbox["host"]),
                       ("check.committed", "control/accept/logs.json", accepted["host"])])
        if not control:
            phases.append(("coordinator.down", "control/stage/logs.json", down["host"]))
        phases.append(("check.completed", "control/finish/logs.json", finished["host"]))
        phases.append(("receipt.delivered", "control/mark/logs.json", marked["host"]) if control else
                      ("coordinator.restarted", "control/restart/logs.json", terminal["host"]))
        phases.append(("parent." + after["status"], "engine/after.json", terminal["host"]))
        trace = [
            {"id": str(index), "fact": fact, "parents": [str(index - 1)] if index else [],
             "scenario": execution, "candidate": candidate, "producer": producer,
             "evidence": {"path": path, "sha256": sha256((root / path).read_bytes()).hexdigest()}}
            for index, (fact, path, producer) in enumerate(phases)
        ]
        contracts = json.loads(Path(__file__).with_name("cases.json").read_text())
        violations.extend(check(contracts[manifest["case"]], trace, root.resolve()))
        if trace_path is not None:
            trace_path.write_text(json.dumps(trace, indent=2) + "\n")
    verdict = ("invalid" if not valid_chain else "fail" if violations else "pass")
    return {
        "schema": SCHEMA, "case": manifest["case"], "profile": manifest["profile"],
        "engine": {"name": "af-native", "build": terminal["binary_sha256"]},
        "inputs": {"workload_sha256": digest(manifest["workload"]),
                   "placement_sha256": digest(manifest["placement"]),
                   "fault_sha256": digest(manifest["fault"]),
                   "clock": manifest["clock"], "seed": manifest["seed"]},
        "expectation": manifest["expectation"], "fault_witnessed": valid_chain and not control,
        "verdict": verdict, "violations": violations,
        "metrics": {"terminal_s": terminal["terminal_s"],
                    "provider_tokens": int(task_after["chargeable_tokens"]),
                    "repeated_steps": task_after["attempts"] - task_before["attempts"]},
        "evidence": [str(root / filename) for filename in (
            "control/distributed-report.json", "engine/before.json", "engine/after.json",
            "engine/task-before.json", "engine/task-after.json", "fixture/checks.sqlite"
        )],
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--trace", type=Path)
    args = parser.parse_args()
    print(json.dumps(export(args.evidence, json.loads(args.manifest.read_bytes()), args.trace),
                     indent=2))
