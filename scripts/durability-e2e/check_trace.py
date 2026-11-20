"""Check milestone order in a single trial export. This is one part of the E2E judge."""
import argparse
import hashlib
import json
from pathlib import Path


def check(contract, events, evidence_root=None, *, complete=True):
    ancestors = {}
    milestones = {}
    problems = []
    required = {fact for chain in contract["chains"] for fact in chain}
    for event in events:
        key, fact, parents = event["id"], event["fact"], event["parents"]
        if not key or key in ancestors or not event["evidence"]:
            raise ValueError(f"invalid evidence or duplicate observation ID: {key}")
        if set(parents) - ancestors.keys():
            raise ValueError(f"missing predecessor or unordered export at {key}")
        if evidence_root is not None:
            evidence = event["evidence"]
            path = (evidence_root / evidence["path"]).resolve()
            if not path.is_relative_to(evidence_root) or not path.is_file():
                raise ValueError(f"evidence missing or outside trial directory at {key}")
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            if digest != evidence["sha256"]:
                raise ValueError(f"evidence digest changed at {key}")
        history = set(parents)
        for parent in parents:
            history.update(ancestors[parent])
        ancestors[key] = history
        if fact in contract["forbidden"]:
            problems.append(f"forbidden observation: {fact}")
        if fact in required:
            if fact in milestones:
                problems.append(f"duplicate milestone: {fact}")
            milestones[fact] = key

    if complete:
        for fact in sorted(required - milestones.keys()):
            problems.append(f"missing milestone: {fact}")
    for chain in contract["chains"]:
        for before, after in zip(chain, chain[1:]):
            if before in milestones and after in milestones:
                if milestones[before] not in ancestors[milestones[after]]:
                    problems.append(f"missing causal edge: {before} -> {after}")
    return problems


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    contracts = json.loads(Path(__file__).with_name("cases.json").read_text())
    parser.add_argument("case", choices=contracts)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--evidence-root", type=Path)
    args = parser.parse_args()
    root = args.evidence_root.resolve() if args.evidence_root else None
    problems = check(contracts[args.case], json.loads(args.trace.read_text()), root)
    print(json.dumps({"transition_contract": "failed" if problems else "satisfied",
                      "problems": problems}, indent=2))
    raise SystemExit(bool(problems))
