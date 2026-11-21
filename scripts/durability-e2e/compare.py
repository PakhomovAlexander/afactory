"""Compare paired engine qualification reports with identical trial inputs."""
import argparse
from pathlib import Path
import json
import math
import re

SCHEMA = "af-durability/trial-report@1"
IDENTITY = {"workload_sha256", "fault_sha256", "placement_sha256", "clock", "seed"}
VERDICTS = {"pass", "fail", "invalid", "unsupported"}
METRICS = {"first_progress_s", "terminal_s", "outage_s", "repeated_steps",
           "additional_calls", "simulated_tokens", "provider_tokens", "cpu_s",
           "unknown_settlements"}


def load(directory):
    records = {}
    for path in sorted(directory.glob("*.json")):
        entry = json.loads(path.read_text())
        if entry.get("schema") != SCHEMA:
            raise ValueError(f"{path}: expected {SCHEMA}")
        inputs = entry["inputs"]
        if set(inputs) != IDENTITY or any(value is None for value in inputs.values()):
            raise ValueError(f"{path}: incomplete paired-trial identity")
        for field in ("workload_sha256", "fault_sha256", "placement_sha256"):
            if not re.fullmatch(r"[0-9a-f]{64}", inputs[field]):
                raise ValueError(f"{path}: invalid {field}")
        if type(inputs["seed"]) is not int or not isinstance(inputs["clock"], str):
            raise ValueError(f"{path}: invalid seed or clock")
        if entry["verdict"] not in VERDICTS or type(entry["fault_witnessed"]) is not bool:
            raise ValueError(f"{path}: invalid verdict or fault status")
        if entry["expectation"] not in {"control", "recover", "policy"}:
            raise ValueError(f"{path}: unknown expectation")
        if entry["expectation"] != "control" and entry["verdict"] in {"pass", "fail"}:
            if not entry["fault_witnessed"]:
                raise ValueError(f"{path}: a missed fault must be an invalid trial")
        if entry["verdict"] == "pass" and entry["violations"]:
            raise ValueError(f"{path}: a safety violation cannot pass")
        if entry["metrics"].keys() - METRICS or any(
            type(value) not in {int, float} or not math.isfinite(value) or value < 0
            for value in entry["metrics"].values()
        ):
            raise ValueError(f"{path}: invalid metric")
        if not entry["evidence"] or not entry["engine"]["build"]:
            raise ValueError(f"{path}: missing evidence references or build")
        key = (entry["case"], entry["profile"], inputs["seed"])
        if key in records:
            raise ValueError(f"{path}: duplicate trial for {key}")
        records[key] = entry
    if not records:
        raise ValueError(f"{directory}: no reports")
    return records


def summary(records):
    by_expectation = {}
    for expectation in ("control", "recover", "policy"):
        selected = [e for e in records.values() if e["expectation"] == expectation]
        by_expectation[expectation] = {
            verdict: sum(e["verdict"] == verdict for e in selected)
            for verdict in sorted(VERDICTS)
        }
    return {"outcomes": by_expectation,
            "violations": sum(len(e["violations"]) for e in records.values())}


def compare(before, after):
    pairs = []
    for key in sorted(before.keys() | after.keys()):
        baseline, candidate = before.get(key), after.get(key)
        row = {"case": key[0], "profile": key[1], "seed": key[2],
               "before": baseline, "after": candidate, "delta": {}}
        if baseline is None or candidate is None:
            row["comparison"] = "unpaired"
        elif baseline["inputs"] != candidate["inputs"] or baseline["expectation"] != candidate["expectation"]:
            row["comparison"] = "different_inputs"
        else:
            row["comparison"] = "paired"
            if baseline["verdict"] == candidate["verdict"] == "pass":
                row["delta"] = {
                    name: candidate["metrics"][name] - baseline["metrics"][name]
                    for name in sorted(baseline["metrics"].keys() & candidate["metrics"].keys())
                }
        pairs.append(row)
    return {"schema": "af-durability/comparison@1", "before": summary(before),
            "after": summary(after), "pairs": pairs}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(compare(load(args.baseline), load(args.candidate)), indent=2))
    except (ValueError, KeyError, TypeError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
