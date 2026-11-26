#!/usr/bin/python3
"""Private Checks Action fixture for the CAS pressure trial without loopback access."""
import json
from pathlib import Path
import sys


def main():
    call = json.load(sys.stdin)
    if call["destination"] != "fixture:checks" or call["method"] not in {"execute", "reconcile"}:
        raise SystemExit("unexpected Checks request")
    fixture = Path(__file__).resolve().parent.parent / "checks-fixture"
    receipt = fixture / "check.json"
    identity = {"intent_id": call["intent_id"], "input_digest": call["input_digest"]}
    check = {**identity, "revision": call["inputs"]["revision"]["value"]}
    if not receipt.exists() and call["method"] == "reconcile":
        outcome = "absent"
    else:
        if call["method"] == "execute":
            with receipt.open("x") as stream:
                json.dump(check, stream)
        if json.loads(receipt.read_bytes()) != check:
            raise SystemExit("Checks receipt identity changed")
        result = json.loads((fixture / "settings.json").read_bytes())["result"]
        outcome = {"pending": "pending", "green": "done"}[result]
    print(json.dumps({"schema": "af/scenario-adapter-response@3", **identity,
                      "outcome": outcome, "evidence": f"File-backed fixture check is {outcome}",
                      "observation": None}))


if __name__ == "__main__":
    main()
