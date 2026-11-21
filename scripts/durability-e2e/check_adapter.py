#!/usr/bin/python3
"""Translate one pinned AF action into a private HTTP fixture call."""
import hashlib
import json
from pathlib import Path
import sys
from urllib import error, request

root = Path(__file__).resolve().parent
config = json.loads((root / "config.json").read_text())
call = json.load(sys.stdin)
if call["destination"] != "fixture:checks" or call["method"] not in {"execute", "reconcile"}:
    raise SystemExit("fixture adapter only accepts checks execute and reconcile")
revision = call["inputs"]["revision"]["value"]
subject = call["inputs"]["subject"]["value"]
check = hashlib.sha256(json.dumps([subject, revision]).encode()).hexdigest()
method = {"execute": "POST", "reconcile": "GET"}[call["method"]]
token = (root / "token").read_text().strip()
transport = request.build_opener(request.ProxyHandler({}))
try:
    with transport.open(request.Request(
        config["endpoint"] + "/checks/" + check, method=method,
        headers={"Authorization": "Bearer " + token, "X-Fixture-Revision": revision,
                 "X-Fixture-Intent": call["intent_id"]},
    ), timeout=3) as response:
        body = json.load(response)
except error.HTTPError as failure:
    if failure.code == 404 and method == "GET":
        outcome, evidence = "absent", "Fixture has no check for this intent"
    else:
        raise SystemExit(f"Check dependency returned HTTP {failure.code}") from None
else:
    if body["id"] != check or body["revision"] != revision:
        raise SystemExit("Check receipt identity differs from the pinned request")
    outcome = {"pending": "pending", "green": "done", "red": "failed"}[body["result"]]
    evidence = f"Fixture check {check} for {revision} is {body['result']}"
print(json.dumps({
    "schema": "af/scenario-adapter-response@3", "intent_id": call["intent_id"],
    "input_digest": call["input_digest"], "outcome": outcome,
    "evidence": evidence, "observation": None,
}))
