"""Gate-only acceptance: passes exactly when the sealed Snapshot's check receipt passed.

This Worker judges nothing about requirements. It exists so a checks-only Task can run without
a model, for example to measure cold and warm check spans (docs/design/research-pipelines.md,
package R1). It never reads the source tree.
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
