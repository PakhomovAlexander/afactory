"""Drive a two-worker durability trial from the controller loopback API."""
import argparse
import json
import os
from pathlib import Path
import re
import time

from control import Control, save


def event(logs, phase):
    prefix = "AF_DURABILITY "
    matches = []
    for line in logs["output"].splitlines():
        _, separator, payload = line.partition(prefix)
        if separator:
            value = json.loads(payload)
            if value["phase"] == phase:
                matches.append(value)
    if len(matches) > 1:
        raise RuntimeError(f"duplicate {phase} evidence")
    return matches[0] if matches else None


def run(args):
    control = Control(None)
    directory = args.evidence
    directory.mkdir(parents=True, exist_ok=False)
    save(directory / "guide.json", control.request("/"))
    resources = {row["id"]: row for row in control.request("/v1/resources")}
    save(directory / "resources.json", list(resources.values()))
    if args.engine == args.fixture:
        raise ValueError("engine and fixture require independent allocations")
    for handle in (args.engine, args.fixture):
        row = resources[handle]
        if (row["profile"], row["state"], row["power"]) != ("af", "allocated", "running"):
            raise RuntimeError(f"{handle} is unavailable for AF jobs")
        jobs = control.request(f"/v1/resources/{handle}/jobs")
        save(directory / (handle + "-jobs.json"), jobs)
        if any(j["state"] in {"active", "activating"} and j["substate"] != "exited" for j in jobs):
            raise RuntimeError(f"{handle} already has a live broker job")

    def submit(label, worker, entrypoint, phase, argument, evidence, timeout=35):
        output = directory / label
        argv = ["/usr/bin/timeout", str(timeout), "/usr/bin/python3", entrypoint, phase,
                str(argument), json.dumps(evidence, separators=(",", ":")), "--binary", args.binary,
                "--work-base", args.engine_scratch if worker == args.engine else args.fixture_scratch]
        return control.submit(worker, f"{args.trial}-{label}", argv, output), output

    def wait(status, output, phase, *, allow_failure=False):
        ok = control.wait_job(status, output, 85)
        if not ok and not allow_failure:
            raise RuntimeError(f"job failed at {status}")
        value = event(json.loads((output / "logs.json").read_text()), phase)
        if not value:
            raise RuntimeError(f"job {status} did not produce {phase} evidence")
        return value, ok

    def fixture(phase, data):
        status, output = submit(phase, args.fixture, args.fixture_script, phase, args.trial, data)
        value, _ = wait(status, output, phase)
        return value

    initialized = fixture("initialize", {})
    stage_path, stage = submit("stage", args.engine, args.engine_script, "stage",
                               args.source, {**initialized, "clean": args.clean}, timeout=255)
    deadline = time.monotonic() + 70
    outbox = None
    while time.monotonic() < deadline:
        logs = control.request(stage_path + "/logs")
        outbox = event(logs, "outbox")
        if outbox:
            break
        status = control.request(stage_path)["properties"]
        if status["SubState"] in {"exited", "failed", "dead"}:
            save(stage / "logs.json", logs)
            raise RuntimeError("scheduler stage exited before producing a Checks intent")
        time.sleep(2)
    if outbox is None:
        raise TimeoutError("stage did not export the native Checks request")
    save(stage / "outbox.json", outbox)

    if outbox["host"] == initialized["host"]:
        raise RuntimeError("fixture and engine reported the same guest identity")
    accepted = fixture("accept", outbox["request"])
    if accepted["host"] != initialized["host"] or accepted["logical_checks"] != 1:
        raise RuntimeError("fixture commit moved or duplicated the check")
    down = None
    if not args.clean:
        marker, output = submit("mark", args.engine, args.engine_script, "mark",
                                outbox["root"], accepted)
        wait(marker, output, "mark")
        down, _ = wait(stage_path, stage, "down")
        if down["killed"] != -9 or down["host"] != outbox["host"]:
            raise RuntimeError("no witnessed scheduler process death on the engine worker")

    finished = fixture("finish", {**outbox["request"], "result": args.result})
    if (finished["committed_events"] != ["initialize", "accept", "finish"]
            or finished["host"] != accepted["host"] or finished["logical_checks"] != 1
            or finished["id"] != accepted["id"]):
        raise RuntimeError("fixture did not preserve one committed check across the crash")
    if args.clean:
        marker, output = submit("mark", args.engine, args.engine_script, "mark",
                                outbox["root"], finished)
        wait(marker, output, "mark")
        terminal, ok = wait(stage_path, stage, "terminal", allow_failure=True)
        correct = ok and terminal["execution"] == outbox["execution"] and terminal["pid"] == outbox["pid"]
    else:
        status, output = submit("restart", args.engine, args.engine_script,
                                "restart", outbox["root"], finished)
        terminal, ok = wait(status, output, "terminal", allow_failure=True)
        correct = ok and terminal["execution"] == down["execution"] and terminal["pid"] != down["pid"]
    report = {
        "schema": "af-durability/broker-recovery@1", "trial": args.trial,
        "transport": "control-api job relay", "engine": args.engine, "fixture": args.fixture,
        "engine_host": outbox["host"], "fixture_host": initialized["host"],
        "fault": None if args.clean else "committed_check_reply_withheld_and_scheduler_killed",
        "checks_result": args.result, "down": down, "terminal": terminal,
        "external_ledger": finished, "verdict": "pass" if correct else "fail",
    }
    save(directory / "distributed-report.json", report)
    print(json.dumps(report), flush=True)
    return 0 if correct else 1


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trial")
    parser.add_argument("engine")
    parser.add_argument("fixture")
    parser.add_argument("engine_script")
    parser.add_argument("fixture_script")
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--source", required=True, help="absolute Afactory checkout on the engine")
    parser.add_argument("--binary", required=True, help="absolute AF executable on the engine")
    parser.add_argument("--engine-scratch", required=True, help="private scratch directory on the engine")
    parser.add_argument("--fixture-scratch", required=True, help="private scratch directory on the fixture")
    parser.add_argument("--result", choices=("green", "red"), default="green")
    parser.add_argument("--clean", action="store_true", help="deliver the green receipt without a crash")
    args = parser.parse_args()
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,36}", args.trial):
        parser.error("use a unique lowercase trial name up to 37 characters")
    for value in (args.source, args.binary, args.engine_script, args.fixture_script,
                  args.engine_scratch, args.fixture_scratch):
        if not Path(value).is_absolute():
            parser.error(f"worker paths must be absolute: {value}")
    if args.clean and args.result != "green":
        parser.error("clean control requires green Checks")
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
