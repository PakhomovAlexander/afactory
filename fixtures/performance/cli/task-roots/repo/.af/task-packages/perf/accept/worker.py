"""Acceptance of the performance laboratory's CLI Task fixture (ADR-0168): passes exactly when
the check receipt passed. It judges nothing about requirements and never reads the source tree;
the laboratory's own oracle independently checks the Task outcome and that the check ran.
"""
import json
import sys

request = json.load(sys.stdin)
receipt = request["inputs"]["checks"][0]["payload"]
outcome = receipt.get("outcome")
if outcome == "passed":
    result = {"outcome": "passed", "reason": "Every required check passed on the sealed Snapshot"}
else:
    result = {"outcome": "failed", "reason": "The check receipt is %s, not passed" % outcome}
print(json.dumps({"schema": "af.worker-reply/1", "outputs": {"result": [result]}}))
