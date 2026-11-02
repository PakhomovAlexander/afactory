# Issue-triage fixture cost: retained local observations

On 2026-11-02, the full runner suite was measured on one arm64 host running
macOS 27.0.1. Every invocation used `python3`, resolving to
`/Library/Developer/CommandLineTools/usr/bin/python3` 3.9.6. The nine runs below were
sequential, in the recorded order, after this Task's Rust build finished.

## Sources and method

The continuation already contains the in-memory coordinator split. Measuring only that
source against this candidate would not establish the split's savings, so the comparison
also includes its retained parent, before the split:

- **Pre-split:** the source Snapshot of `triage-3b`,
  `sha256:04f20d7c6ed82ef1175c884c8f7e834b4df3e0c373c86a47cdc479f3a65022ea`.
- **Continuation:** this Task's input Snapshot,
  `sha256:7858c4eea9f8a488c533c59b54d1d89599f26b7d18ba01fccac2eee31af93702`.
  Its Manifest is identical to the reviewed `triage-3b` candidate,
  `sha256:4c8258cfbd1f23a29f72681aa7d638306f4861e15b1520f68cce691cc61a5d8e`,
  whose `parent_snapshot_id` names the pre-split Snapshot above.
- **Candidate:** the continuation with this close-out's parser, sweep, regression tests
  and documentation changes. Its measured files are identified by raw SHA-256 below;
  the kernel assigns the candidate Snapshot ID after the implementer returns.

Pre-split and continuation source bytes came directly from those Snapshots' retained CAS
Manifests, without editing their suites. Between those revisions, only
`scripts/backlog-triage.py`, `scripts/test-backlog-triage.py`,
`docs/design/issue-triage.md` and `changelog.d/issue-triage-closeout.md` differ.
All 94 original test methods remain in both later suites, including their assertions;
the continuation adds five methods and this candidate adds two. No test is skipped.

The manual stdlib helper
[`scripts/measure-triage-fixture-cost.py`](../../scripts/measure-triage-fixture-cost.py)
imports and discovers the entire selected suite, runs it through unittest, and counts
successful Git launches from Python's `Popen`, including fixture setup and production
adapters. It forwards the real subprocess calls and results. This count excludes any
internal descendants Git itself might launch. The wall starts before suite import and
ends after unittest returns, including discovery, setup, tests and cleanup; interpreter
startup and source materialization are outside it.

Every run clears inherited environment injection and uses a new private HOME, TMPDIR and
XDG directories, the same fixed PATH/locale/timezone, disabled system/global Git config,
and disabled hooks/signing. This also isolates the unmodified older fixture that inherited
Git configuration. The suite's fake API, Task, lease, clock and network guards still apply.
The helper is manual developer tooling, outside the production runner and future gates.

## Observed full-suite results

Every row exited 0 with no failures, errors or skipped tests. Times are seconds.

| Round | Source | UTC start (2026-11-02) | Tests | Git launches | Wall |
| --- | --- | --- | ---: | ---: | ---: |
| 1 | Pre-split | 02:37:55.258788 | 94 | 711 | 10.339194 |
| 1 | Continuation | 02:38:05.744732 | 99 | 126 | 4.534429 |
| 1 | Candidate | 02:38:10.430765 | 101 | 145 | 4.776310 |
| 2 | Pre-split | 02:38:15.349910 | 94 | 711 | 10.207885 |
| 2 | Continuation | 02:38:25.699117 | 99 | 126 | 4.563829 |
| 2 | Candidate | 02:38:30.407783 | 101 | 145 | 4.763168 |
| 3 | Pre-split | 02:38:35.314488 | 94 | 711 | 10.196120 |
| 3 | Continuation | 02:38:45.652699 | 99 | 126 | 4.699292 |
| 3 | Candidate | 02:38:50.495462 | 101 | 145 | 4.816422 |

The candidate retains 566 fewer Git launches per full suite than pre-split (79.61% fewer),
while running seven more tests. Median wall falls from 10.207885 to 4.776310 seconds
(53.21% lower). The already optimized continuation's median is 4.563829 seconds with
126 launches. This candidate adds 19 launches and about 0.21 seconds to that continuation
for the extra real-source regressions. The material reduction is relative to pre-split;
the continuation-to-candidate comparison records the close-out's additional cost.
The host was shared; no exclusive CPU access was assumed. These observations describe
this host and run series, with no timing gate or tolerance.

## Reproduction

From the candidate repository root, populate the two scratch source roots from a retained
Task Store containing these Snapshots. Set `TRIAGE_COST_CAS` to its `cas/objects` directory;
this reads only the named Snapshots and their files, without invoking `af` or a live service:

```sh
python3 - <<'PY'
import json
import os
from pathlib import Path

cas = Path(os.environ['TRIAGE_COST_CAS'])

def read(identity):
    value = identity.removeprefix('sha256:')
    return (cas / value[:2] / value[2:]).read_bytes()

snapshots = {
    'pre-split': '04f20d7c6ed82ef1175c884c8f7e834b4df3e0c373c86a47cdc479f3a65022ea',
    'continuation': '7858c4eea9f8a488c533c59b54d1d89599f26b7d18ba01fccac2eee31af93702',
}
for label, identity in snapshots.items():
    snapshot = json.loads(read(identity))
    manifest = json.loads(read(snapshot['manifest_id']))
    root = Path('/tmp/triage-fixture-cost') / label
    for entry in manifest['entries']:
        path = entry['path']
        if entry['kind'] != 'file':
            continue
        if path.startswith(('scripts/', 'docs/', 'fixtures/', 'schemas/', '.config/')) or path == 'Makefile':
            destination = root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(read(entry['content']))
PY
```

The exact measurement commands below ran three times, in this order each round:

```sh
python3 -I scripts/measure-triage-fixture-cost.py /tmp/triage-fixture-cost/pre-split
python3 -I scripts/measure-triage-fixture-cost.py /tmp/triage-fixture-cost/continuation
python3 -I scripts/measure-triage-fixture-cost.py .
```

Each command reports interpreter, host, test outcomes, successful Git launches and wall.
Keep the same interpreter for every root. The helper's isolated environment is identical
across runs; actual wall times will vary with host load.

## Measured file identities

These are SHA-256 of raw file bytes, distinct from the kernel's domain-separated CAS IDs.
Within each block, entries are the runner, suite and design document, in that order.
Supporting scripts, schemas and fixtures are unchanged between the captured revisions.

Pre-split:

```text
cb848f17e8cb822fb117cad6515e4ba027c613d4a320601c8580ae1ebb26bb8c
4c2c6c70a2274f3f8b851e66fb0cd194958efb8572cf6b39053d00b37fd525f3
6a5ec66ebed6df1ecca83be91a54d2d21392eacadb4a053f75275f4b7afab594
```

Continuation:

```text
8328660c1a320119c52ed674aa9c307115e558650e3aed520c08f283a13c2d49
45865f73b66729016f1d4fc5874c64c494106af95897fcef0d1fee84d6a28bd6
2bfb576e96d21d7cd1ed8c2355302933e16368a5ffdcfdd77cb4773eb4401b5a
```

Candidate:

```text
7a00570dbd1dd05672be679a4e39cf983cbbeb7804aa1490778a1b85ed5b3a61
07efc79e79b1258713d959282d091e75960a8d1c8f59fcdb8cc001cf516d9d70
3bc4d2bfe307d69798c17a55919cb21a65d5560d62a67260b46151770f60bac2
```

Measurement helper (the same candidate file used for every root):

```text
62bdcc7f02e836aa3ef7464e3f1523561d0d0933a0d0d143db4ae0d357ef7e57
```
