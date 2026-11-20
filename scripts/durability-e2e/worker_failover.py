"""Drive the engine-neutral Worker replacement and stale completion contract."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

from check_trace import check
from compare import SCHEMA

CASE = "worker-loss-and-late-completion"
REQUIRED = {"remote_worker_replacement", "late_completion_delivery"}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def report(manifest, engine, evidence, verdict, faults, violations=(), metrics=None):
    return {
        "schema": SCHEMA, "case": CASE, "profile": manifest["profile"], "expectation": "recover",
        "engine": engine,
        "inputs": {**{f"{name}_sha256": digest(manifest[name])
                      for name in ("workload", "fault", "placement")},
                   "clock": manifest["clock"], "seed": manifest["seed"]},
        "verdict": verdict, "fault_witnessed": faults, "violations": list(violations),
        "metrics": metrics or {}, "evidence": evidence,
    }


def run(manifest, directory, call):
    """Call the supplied test adapter, then validate independent durable observations."""
    directory = directory.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    write(directory / "inputs.json", manifest)
    refs = [str(directory / "inputs.json")]

    def invoke(action, **args):
        return call(action, {"input": manifest, "evidence_dir": str(directory), **args})

    profile = invoke("capabilities")
    write(directory / "capabilities.json", profile)
    refs.append(str(directory / "capabilities.json"))
    missing = REQUIRED - set(profile["capabilities"])
    if missing:
        result = report(manifest, profile["engine"], refs, "unsupported", False)
        result["missing_capabilities"] = sorted(missing)
        return result

    contract = json.loads(Path(__file__).with_name("cases.json").read_text())[CASE]
    required_facts = {fact for chain in contract["chains"] for fact in chain}
    execution = invoke("start")["scenario"]
    if not execution:
        raise ValueError("engine adapter must return an admitted Scenario identity")
    events = []
    deadline = time.monotonic() + manifest["fault"]["deadline_s"]
    killed_at = None
    violations = []

    def bind(fact, subject):
        if detected[fact]["subject"] != subject:
            violations.append(f"{fact} refers to a different Worker or Attempt")

    def wait(facts):
        nonlocal events
        while time.monotonic() < deadline:
            current = invoke("observe", scenario=execution)["trace"]
            if events != current[:len(events)]:
                raise ValueError("adapter changed previously observed history")
            if any(record["scenario"] != execution for record in current):
                raise ValueError("trace crosses Scenario identities")
            events = current
            write(directory / "trace.json", events)
            check(contract, events, directory)
            found = {record["fact"]: record for record in events}
            if set(facts) <= found.keys():
                return found
            time.sleep(0.2)
        raise TimeoutError("worker failover deadline for " + ", ".join(facts))

    try:
        detected = wait(["first_attempt.started"])
        original = detected["first_attempt.started"]["subject"]
        if "worker.lost" in detected:
            raise ValueError("Worker loss was reported before fault injection")
        invoke("isolate_worker", scenario=execution, worker=original)
        lost = wait(["worker.lost"])["worker.lost"]
        if lost["subject"] != original:
            raise ValueError("injected failure targeted a different Worker or Attempt")
        killed_at = time.monotonic()
        detected = wait(["first_attempt.fenced", "replacement.verified"])
        replacement = detected["replacement.verified"]["subject"]
        bind("first_attempt.fenced", original)
        if (original["allocation"] == replacement["allocation"]
                or original["attempt"] == replacement["attempt"]):
            violations.append("replacement reused the original allocation or Attempt")
        if "replacement.started" not in detected:
            violations.append("replacement verified before its start was observed")
        else:
            bind("replacement.started", replacement)
        if "stale_completion.delivered" in detected:
            violations.append("stale completion was reported before its release")
        if not violations:
            invoke("deliver_stale", scenario=execution, worker=original, replacement=replacement)
            detected = wait(required_facts)
            for fact in ("stale_completion.delivered", "stale_completion.rejected",
                         "late_usage.reconciled"):
                bind(fact, original)
            violations.extend(check(contract, events, directory))
    except TimeoutError as error:
        violations.append(str(error))
    if (directory / "trace.json").exists():
        refs.append(str(directory / "trace.json"))
    witnessed = killed_at is not None
    verdict = "invalid" if not witnessed else "fail" if violations else "pass"
    metrics = {"terminal_s": round(time.monotonic() - killed_at, 3)} if verdict == "pass" else {}
    return report(manifest, profile["engine"], refs, verdict, witnessed, violations, metrics)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("adapter", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_bytes())
    if manifest.get("schema") != "af-durability/trial-inputs@1":
        parser.error("unknown trial inputs schema")
    adapter = args.adapter.resolve()

    def invoke(action, data):
        result = subprocess.run([adapter, action], input=json.dumps(data),
                                capture_output=True, text=True, check=True, timeout=30)
        return json.loads(result.stdout)

    result = run(manifest, args.evidence, invoke)
    write(args.evidence / "report.json", result)
    print(json.dumps(result, indent=2))
    return {"pass": 0, "fail": 1, "invalid": 2, "unsupported": 3}[result["verdict"]]


if __name__ == "__main__":
    raise SystemExit(main())
