#!/usr/bin/python3 -IS
"""Inert native-composition fixture. No Git transport, credentials or network."""
import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parent
request = json.load(sys.stdin)
inputs = {k: v["value"] for k, v in request["inputs"].items()}
state_path = root / "effects.json"
state = json.loads(state_path.read_text()) if state_path.exists() else {"publications": [], "posts": 0}
response = dict(schema="af/scenario-adapter-response@1", intent_id=request["intent_id"],
                input_digest=request["input_digest"], outcome="done", evidence="inert", observation=None)
operation = request["operation"]
if request["method"] == "reconcile":
    response["outcome"] = "absent"
elif operation == "prepare_candidate_v1":
    candidate = inputs["candidate"]
    previous = json.loads(inputs["previous"]["evidence"]) if "previous" in inputs else None
    prepared = dict(schema="af/git-workspace-prepared@1", owner=request["intent_id"],
        previous_owner=previous["owner"] if previous else None, config_digest="sha256:" + "a" * 64,
        repository="octo/project", repository_id=17, task_id=candidate["task_id"], result_id=candidate["result_id"],
        snapshot_id=candidate["result"]["outputs"]["snapshot"]["snapshot_id"], source_snapshot="sha256:" + "d" * 64,
        source_predecessor=previous["snapshot_id"] if previous else None,
        source_commit=previous["commit"] if previous else "a" * 40, branch="owned/documentation", base="main",
        commit=(chr(ord(previous["commit"][0]) + 1) if previous else "b") * 40, tree="d" * 40, worktree=str(root / candidate["task_id"]),
        title="Documentation", body="Exact accepted candidate")
    response["evidence"] = json.dumps(prepared, sort_keys=True, separators=(",", ":"))
elif operation == "publish_pr":
    assert inputs["decision"]["request"]["approve"] is True
    assert inputs["decision"]["request"]["revision"] == inputs["revision"]
    if not state["publications"]:
        state["posts"] += 1
    state["publications"].append(dict(subject=inputs["subject"], revision=inputs["revision"]))
    state_path.write_text(json.dumps(state))
elif operation == "observe":
    assert state["publications"][-1] == dict(subject=inputs["subject"], revision=inputs["revision"])
    mode = (root / "mode").read_text()
    if mode.startswith("repairs-"):
        count = int(mode.split("-")[1])
        outcome = "repairable" if len(state["publications"]) <= count else "ready"
    else:
        outcome = "ready" if inputs["revision"] == "c" * 40 else mode
    outcome = "repairable" if mode == "stale" else outcome
    packet = dict(schema="af/github-check-outcome@1", subject=inputs["subject"], revision=inputs["revision"],
        state=outcome, checks=[dict(identity=dict(kind="check", name="CI", app_id=4), id=50, suite_id=40, outcome=outcome)])
    if mode == "stale":
        packet["revision"] = "a" * 40
    condition = inputs.get("condition", "checks_completed")
    satisfied = outcome != "pending"
    evidence = json.dumps(packet, sort_keys=True, separators=(",", ":"))
    if condition == "checks_passed":
        satisfied = outcome == "ready"
    if condition == "merged":
        satisfied = (root / "human-state").read_text() == "merged"
        evidence = "Exact inert PR head merged by human" if satisfied else "Not actually merged"
    response["observation"] = dict(subject=inputs["subject"], revision=inputs["revision"], condition=condition,
        satisfied=satisfied, evidence=evidence)
elif operation == "cleanup_workspace_v1":
    assert (root / "human-state").read_text() == "merged"
    assert inputs["evidence"]["condition"] == "merged"
    assert inputs["evidence"]["satisfied"] is True
    assert json.loads(inputs["body"]) == json.loads(inputs["prepared"]["evidence"])
    state["cleaned"] = inputs["revision"]
    state_path.write_text(json.dumps(state))
else:
    raise AssertionError(operation)
json.dump(response, sys.stdout)
