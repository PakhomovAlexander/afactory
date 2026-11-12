#!/usr/bin/env python3
"""Credential-free bounded adapter fixture. It models a durable service, not a real publisher.

Tests install a private copy outside their repository and explicitly pin its bytes. Each effect
has an idempotency key and a durable receipt. A switch loses the first execute response after
saving the effect, exercising recovery through the real adapter process boundary.
"""
import json
import os
import pathlib
import sys

root = pathlib.Path(__file__).parent
request = json.load(sys.stdin)
intent = request["intent_id"].split(":")[1]
receipt = root / (intent + ".json")
response = {
    "schema": "af/scenario-adapter-response@1",
    "intent_id": request["intent_id"],
    "input_digest": request["input_digest"],
    "outcome": "absent",
    "evidence": "authoritative service lookup: no effect",
    "observation": None,
}
if request["destination"] != "fixture:repository":
    raise SystemExit("unauthorized destination")
if request["method"] == "observe":
    current = json.loads((root / "observation.json").read_text())
    condition = request["inputs"]["condition"]["value"]
    response.update(
        outcome="done",
        evidence="bounded exact-revision service observation",
        observation={
            "subject": request["inputs"]["subject"]["value"],
            "revision": current["revision"],
            "condition": condition,
            "satisfied": current.get(condition, False),
            "evidence": current.get("evidence", "service revision " + current["revision"]),
        },
    )
elif request["method"] == "reconcile":
    if receipt.exists():
        response = json.loads(receipt.read_text())
        if response["input_digest"] != request["input_digest"]:
            raise SystemExit("idempotency key reused with changed inputs")
elif request["method"] == "execute":
    current = json.loads((root / "observation.json").read_text())
    if current["revision"] != request["inputs"]["revision"]["value"]:
        response.update(outcome="failed", evidence="exact head precondition refused")
    elif receipt.exists():
        response = json.loads(receipt.read_text())
    else:
        response.update(outcome="done", evidence="fixture service accepted exactly one effect")
        # Exclusive creation is this fixture service's atomic idempotent publication boundary.
        with receipt.open("x") as stream:
            json.dump(response, stream)
            stream.flush()
            os.fsync(stream.fileno())
        with (root / "effects").open("a") as stream:
            stream.write(intent + "\n")
        if (root / "lose-response").exists():
            (root / "lose-response").unlink()
            raise SystemExit(1)
else:
    raise SystemExit("unknown adapter method")
json.dump(response, sys.stdout)
