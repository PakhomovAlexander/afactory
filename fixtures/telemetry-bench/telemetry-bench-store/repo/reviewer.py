"""The scripted reviewer of `telemetry-bench-store` (fixtures/telemetry-bench/README.md).

It reads its request and reports `findings_per_reviewer` minor Findings from `bench.json`, each
with its own title and occurrence key, so the Round's publication appends one
`FindingReported@1` event per Finding. Minor Findings under the `major` gate leave the Round
clean. Credential-free; never touches the network.
"""

import json
import sys

name = sys.argv[1]
sys.stdin.buffer.read()
with open("bench.json", encoding="utf-8") as file:
    count = json.load(file)["harness"]["findings_per_reviewer"]
findings = [
    {
        "severity": "minor",
        "file": "src/main.rs",
        "line": 1,
        "title": f"Bench finding {name}-{index}",
        "body": f"The bench reviewer {name} reports finding {index}",
        "fix": "Nothing to fix: a bench fixture",
        "confidence": 0.5,
        "occurrence_key": f"{name}-{index}",
    }
    for index in range(count)
]
print(json.dumps({"findings": findings, "benchmark_demands": [], "dispositions": []}))
