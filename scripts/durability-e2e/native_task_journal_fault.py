"""Witness native Task journal exhaustion on a private Linux volume."""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess

from native_artifact_fault import full_volume, namespace, object_set
from native_recovery import process, until
from native_smoke import prepare, require_linux, write_json


def journal_rows(db):
    return db.execute(
        "SELECT run_id, sequence, event_id, type, artifact_refs, payload "
        "FROM events ORDER BY run_id, sequence"
    ).fetchall()


def captured_plan(cas, task_id):
    plans = []
    for path in (cas / "objects").glob("*/*"):
        try:
            obj = json.loads(path.read_bytes())
        except (UnicodeError, ValueError):
            continue
        if not isinstance(obj, dict) or obj.get("type") != "af/ExecutionPlan@5":
            continue
        revision_id = obj["payload"]["task_revision_id"]
        digest = revision_id.removeprefix("sha256:")
        revision = json.loads((cas / "objects" / digest[:2] / digest[2:]).read_bytes())
        if revision["payload"]["task_id"] == task_id:
            plans.append((obj["artifact_id"], revision_id))
    assert len(plans) == 1, plans
    return plans[0]


def run(binary, source, boundary):
    root, af, env, policy, bundle = prepare(binary, source, "green")
    repo, state, cas = root / "repo", root / "task-state", root / "task-state" / "cas"
    scenario = "scenario-state"
    # Build and record an accepted predecessor before pressuring the independent Task journal.
    initial = bundle["definitions"]["clean"]["steps"][0]["operation"]["target"]
    af("prior-run", "task-state", "task", "run", initial["task_id"],
       "--confirm-plan", initial["plan_id"])
    prior = af("prior-before", "task-state", "task", "show", initial["task_id"])
    assert prior["result"]["acceptance"] == "satisfied"
    assert prior["chargeable_tokens"] == "0"

    commit = subprocess.check_output(["git", "-C", repo, "rev-parse", "HEAD"], text=True).strip()
    document = json.loads((repo / "ticket.json").read_bytes())
    document["goal"] = "Implement pagination independently on the pinned source."
    document["verification"] = "evaluation"
    ticket = root / "second.json"
    write_json(ticket, document)
    template = af("template", scenario, "scenario", "template-pin",
                  "--file", str(ticket), "--repo", str(repo),
                  "--task-state", str(state), "--authority-commit", commit)
    definition = bundle["definitions"]["clean"]
    definition["steps"].append({
        "id": "second", "operation": {"kind": "fresh_task",
                                      "template": template["template"], "bindings": {}},
        "inputs": {}, "outputs": {"result": "task"}, "when": None,
        "max_attempts": 1, "timeout_ms": 60_000, "on_timeout": "fail",
    })
    definition["outputs"]["second"] = "task"
    definition["result"]["second"] = {"source": "step", "step": "second", "port": "result"}
    definition["limits"]["max_tokens"] = 2000
    definition["authority"]["task_templates"] = policy["task_templates"] = [template["template_id"]]
    inputs, bundle_file, policy_file = (root / name for name in ("inputs.json", "bundle.json", "policy.json"))
    write_json(inputs, {})
    write_json(bundle_file, bundle)
    write_json(policy_file, policy)
    spec = ("--file", str(bundle_file), "--authority", str(policy_file))
    pin = af("pin", scenario, "scenario", "validate", *spec)

    volume = root / "journal-volume"
    volume.mkdir(mode=0o700)
    subprocess.run(["mount", "-t", "tmpfs", "-o", "size=4M,nosuid,nodev,noexec",
                    "tmpfs", volume], check=True, timeout=5)
    try:
        return trial(binary, root, af, repo, state, cas, scenario, env, spec,
                     inputs, pin, prior, volume, boundary)
    finally:
        subprocess.run(["umount", volume], check=True, timeout=5)


def trial(binary, root, af, repo, state, cas, scenario, env, spec, inputs, pin, prior, volume, boundary):
    path = state / "events.sqlite"
    database = volume / path.name
    # SQLite resolves the symlink before opening its WAL and SHM. Move only a checked,
    # standalone copy of the database onto the private volume.
    with closing(sqlite3.connect(path)) as original, closing(sqlite3.connect(database)) as db:
        original.backup(db)
    for suffix in ("-wal", "-shm"):
        sidecar = path.with_name(path.name + suffix)
        assert not sidecar.exists(), sidecar
    path.unlink()
    path.symlink_to(database)
    assert path.stat().st_dev == volume.stat().st_dev
    assert path.stat().st_dev != cas.stat().st_dev
    original_cas = object_set(cas)
    barriers = root / "barriers"
    point = ("scenario-before-task-opening-1" if boundary == "open" else "scenario-task-opened-1")
    barrier = barriers / point
    barrier.mkdir(parents=True)
    cmd = [binary, "scenario"]
    args = ["--json", "--state", str(root / scenario)]
    start = cmd + ["start", "clean", *spec, "--inputs", str(inputs), "--confirm-pin",
                   pin["revision"], "--execute", *args]
    resume = cmd + ["resume", "clean", *args]
    env = env | {"AF_TEST_TASK_LEASE_MS": "6000"}

    def show(label):
        return af(label, scenario, "scenario", "show", "clean")

    def waiting(label):
        observed = show(label)
        diagnostic = (observed.get("waiting") or {}).get("diagnostic", "")
        assert observed["status"] == "waiting", observed.get("failure")
        assert observed["waiting"]["kind"] == "reconcile"
        assert "Native Task event journal capacity exhausted" in diagnostic, diagnostic
        assert "database or disk is full" in diagnostic or "os error 28" in diagnostic, diagnostic
        return observed, diagnostic

    with closing(sqlite3.connect(path)) as db:
        assert Path(db.execute("PRAGMA database_list").fetchone()[2]) == database
        db.execute("PRAGMA journal_mode=WAL").fetchone()
        history = journal_rows(db)
        with process(start, root / "start.log", cwd=repo,
                     env=env | {"AF_TEST_WAIT_OBSERVATION_BARRIER": str(barriers)}) as child:
            until(15, lambda: (barrier / "ready").exists(), child, point)
            assert (root / scenario).stat().st_dev == cas.stat().st_dev
            before = show("before")
            step = before["frames"]["root"]["steps"][1]
            intent = step["intents"][0]
            task_id = "scenario-" + intent["id"].removeprefix("sha256:")
            plan, revision = captured_plan(cas, task_id)
            at_barrier = journal_rows(db)
            fresh = [row for row in at_barrier if row not in history]
            assert len(fresh) == (boundary == "proposal"), fresh
            if fresh:
                assert json.loads(fresh[0][-1])["change"]["prepared_plan"]["plan_id"] == plan
            write_json(root / "history-before.json", history)
            write_json(root / "history-at-barrier.json", at_barrier)
            with full_volume(volume) as pressure_errno:
                (barrier / "proceed").touch()
                child.wait(timeout=15)
                blocked, diagnostic = waiting("waiting")
                write_json(root / "journal-fault.json", {
                    "pressure_errno": pressure_errno, "diagnostic": diagnostic,
                    "boundary": boundary, "execution_id": before["execution_id"],
                    "intent_id": intent["id"], "task_id": task_id, "plan_id": plan,
                    "revision_id": revision, "deadline_unix_ms": step["deadline_unix_ms"],
                })
                assert journal_rows(db) == at_barrier
                retry = subprocess.run(resume, cwd=repo, env=env, stdout=subprocess.PIPE,
                                       stderr=subprocess.STDOUT, timeout=15)
                (root / "retry-full.log").write_bytes(retry.stdout)
                blocked_again, retry_diagnostic = waiting("retry-waiting")
                assert journal_rows(db) == at_barrier
                write_json(root / "history-at-full.json", journal_rows(db))
                for view in (blocked, blocked_again):
                    active = view["frames"]["root"]["steps"][1]
                    assert view["execution_id"] == before["execution_id"]
                    assert view["dispatches"] == before["dispatches"]
                    assert view["task_charges"] == before["task_charges"]
                    assert not view["fresh_tasks"] and not active["outputs"]
                    assert len(active["intents"]) == 1
                    assert active["intents"][0]["phase"] == "prepared"
                    assert active["intents"][0]["id"] == intent["id"]
                    assert active["deadline_unix_ms"] == step["deadline_unix_ms"]
        # Lose the response to a committed proposal before the Scenario binds the Task.
        committed = barriers / "scenario-task-proposed-1"
        committed.mkdir()
        with process(resume, root / "reconcile-commit.log", cwd=repo,
                     env=env | {"AF_TEST_WAIT_OBSERVATION_BARRIER": str(barriers)}) as driver:
            until(15, lambda: (committed / "ready").exists(), driver, "committed proposal")
            uncertain = show("after-proposal-before-binding")
            assert not uncertain["fresh_tasks"]
            assert uncertain["frames"]["root"]["steps"][1]["intents"][0]["phase"] == "prepared"
            at_commit = journal_rows(db)
            changes = [json.loads(row[-1]).get("change", {}) for row in at_commit if row not in history]
            assert [change["plan_id"] for change in changes
                    if change.get("kind") == "plan_proposed"] == [plan]
            write_json(root / "history-at-uncertain-commit.json", at_commit)
        assert driver.returncode == -9
        restored = subprocess.run(resume, cwd=repo, env=env, stdout=subprocess.PIPE,
                                  stderr=subprocess.STDOUT, timeout=30)
        (root / "restored.log").write_bytes(restored.stdout)
        assert restored.returncode == 0, restored.stdout.decode()
        after = show("after")
        final_history = journal_rows(db)
    write_json(root / "history-after.json", final_history)
    after_prior = af("prior-after", "task-state", "task", "show", prior["task_id"])
    completed = af("second-after", "task-state", "task", "show", task_id)
    after_step = after["frames"]["root"]["steps"][1]
    final_intent, = after_step["intents"]
    target = after["fresh_tasks"][intent["id"]]
    assert len(after["fresh_tasks"]) == 1
    assert after["status"] == "succeeded"
    assert after["execution_id"] == before["execution_id"]
    assert after["dispatches"] == before["dispatches"]
    assert after["frames"]["root"]["steps"][0] == before["frames"]["root"]["steps"][0]
    assert after_step["deadline_unix_ms"] == step["deadline_unix_ms"]
    assert final_intent["id"] == intent["id"] and final_intent["input_digest"] == intent["input_digest"]
    assert final_intent["phase"] == "done"
    assert diagnostic in final_intent["evidence"] and retry_diagnostic in final_intent["evidence"]
    assert target["task_id"] == completed["task_id"] == task_id
    assert target["plan_id"] == completed["plan_id"] == plan
    assert target["revision_id"] == completed["revision_id"] == revision
    assert completed["result"]["acceptance"] == "satisfied"
    assert completed["chargeable_tokens"] == after_prior["chargeable_tokens"] == "0"
    for key in ("history", "result", "attempts", "plan_id", "revision_id"):
        assert prior[key] == after_prior[key], key
    final_cas = object_set(cas)
    assert all(final_cas.get(name) == digest for name, digest in original_cas.items())
    assert all(row in final_history for row in at_commit)
    new = [row for row in final_history if row not in history]
    assert len({row[0] for row in new}) == 1
    changes = [json.loads(row[-1]).get("change", {}).get("kind") for row in new]
    assert changes.count("opened") == changes.count("plan_proposed") == 1
    assert sum(row[3] == "TaskExecutionStarted@1" for row in new) == 1
    with Path(binary).open("rb") as executable:
        binary_digest = hashlib.file_digest(executable, "sha256").hexdigest()
    summary = {"boundary": boundary, "status": after["status"], "execution_id": after["execution_id"],
               "task_id": task_id, "plan_id": plan, "revision_id": revision,
               "chargeable_tokens": completed["chargeable_tokens"],
               "binary_sha256": binary_digest}
    write_json(root / "journal-summary.json", summary)
    print(json.dumps(summary), flush=True)


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("source", type=Path)
    parser.add_argument("boundary", choices=("open", "proposal"))
    args = parser.parse_args()
    require_linux()
    namespace()
    run(str(args.binary.resolve()), args.source.resolve(), args.boundary)
