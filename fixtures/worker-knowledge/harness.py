"""Deterministic retrieval protocol example; no language tool or Provider is required."""
import json
import sys

request = json.load(sys.stdin)
assert "notes" not in request["inputs"]
assert "knowledge_query" not in request["inputs"]
knowledge = request["inputs"]["knowledge"][0]["payload"]
assert "PRIVATE_HYPOTHESIS" not in json.dumps(request)
entries = knowledge["manifest"]["entries"]
excerpts = knowledge["excerpts"]
text = excerpts[0]["text"] if excerpts else "manifest only"
outputs = {
    "output": [{"outcome": "passed", "text": text}],
    "knowledge_query": [{
        "schema": "af/KnowledgeQuery@1",
        "index_id": knowledge["index_id"],
        "entries": [entries["guide"]["entry_id"]],
    }],
    "notes": [{
        "model_of_change": "PRIVATE_HYPOTHESIS",
        "inspected": [{"path": "guide.txt"}],
    }],
}
print(json.dumps({"schema": "af.worker-reply/1", "outputs": outputs}))
