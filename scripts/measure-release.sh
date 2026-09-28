#!/usr/bin/env bash
set -euo pipefail

# The `release_build` measure (ADR-0124, docs/design/research-pipelines.md package R2). The
# kernel runs this against a read-only Snapshot with a private runtime directory and times it;
# this script builds the release `af` into the CARGO_TARGET_DIR the kernel bound and reports,
# on its last stdout line, that directory's byte total and the `af` binary's size. Cargo's own
# output goes to stderr, so nothing follows the report line.
target_dir="${CARGO_TARGET_DIR:?the kernel binds CARGO_TARGET_DIR for every measure repetition}"
cargo build --release -p af --locked 1>&2
python3 - "$target_dir" <<'PY'
import json
import os
import sys

root = sys.argv[1]
total = 0
for directory, _, files in os.walk(root):
    for name in files:
        path = os.path.join(directory, name)
        if not os.path.islink(path):
            total += os.lstat(path).st_size
binary = os.lstat(os.path.join(root, 'release', 'af')).st_size
print(json.dumps({'schema': 'af.measure-report/1', 'metrics': {
    'target_bytes': {'value': str(total), 'unit': 'bytes'},
    'binary_bytes': {'value': str(binary), 'unit': 'bytes'},
}}, sort_keys=True))
PY
