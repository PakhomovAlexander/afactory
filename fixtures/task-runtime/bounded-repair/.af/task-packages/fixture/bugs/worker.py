import json,sys
json.load(sys.stdin)
print('{"schema": "af.worker-reply/1", "outputs": {"result": [{"verdict": "request-changes", "summary": "Negative offset is not rejected", "reports": [{"severity": "major", "file": "pagination.py", "line": 1, "title": "Reject negative offset", "body": "A negative offset silently slices from the end.", "fix": "Raise ValueError for negative offset.", "confidence": 0.99}], "benchmark_demands": [], "disputes": []}]}}')
