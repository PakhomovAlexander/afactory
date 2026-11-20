#!/usr/bin/python3
"""Use durable files for the broker-delivered Checks transport."""
import hashlib
import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parent
call = json.load(sys.stdin)
if call["destination"] != "fixture:checks" or call["method"] not in {"execute", "reconcile"}:
    raise SystemExit("unexpected Checks operation")
subject = call["inputs"]["subject"]["value"]
revision = call["inputs"]["revision"]["value"]
check = hashlib.sha256(json.dumps([subject, revision]).encode()).hexdigest()
payload = {"id": check, "revision": revision, "intent": call["intent_id"]}
outbox = root / "request.json"
receipt = root / "receipt.json"
if outbox.exists():
    if json.loads(outbox.read_text()) != payload:
        raise SystemExit("outstanding check belongs to a different intent")
    if receipt.exists():
        data = json.loads(receipt.read_text())
        if (any(data[key] != value for key, value in payload.items())
                or data["result"] not in {"green", "red"}):
            raise SystemExit("fixture returned an unrelated check")
        outcome = "done" if data["result"] == "green" else "failed"
        evidence = f"Broker-delivered exact candidate check {check} is {data['result']}"
    else:
        outcome, evidence = "pending", "Check request is outstanding in the broker transport"
elif receipt.exists():
    raise SystemExit("fixture receipt has no durable check request")
elif call["method"] == "reconcile":
    initial = json.loads((root / "empty.json").read_text())
    if initial["state"] != "empty":
        raise SystemExit("missing fixture empty receipt")
    outcome, evidence = "absent", "Fixture provided an empty ledger at admission"
else:
    pending = root / "request.pending"
    with pending.open("x") as stream:
        json.dump(payload, stream)
        stream.flush()
    pending.replace(outbox)
    outcome, evidence = "pending", "Check request persisted without a response"

print(json.dumps({"schema": "af/scenario-adapter-response@3", "outcome": outcome,
                  "intent_id": call["intent_id"], "input_digest": call["input_digest"],
                  "evidence": evidence, "observation": None}))
