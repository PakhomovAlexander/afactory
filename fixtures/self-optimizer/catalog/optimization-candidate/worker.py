import json
import sys

json.load(sys.stdin)
print(json.dumps({"schema": "af.worker-reply/1", "outputs": {"receipt": [{"schema": "af.optimization-arm-receipt/1", "arm": "candidate"}]}}))
