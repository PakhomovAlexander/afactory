# Release safeguards: repair-1 focused execution evidence

## Identity and scope

This record closes the missing execution evidence identified by the repair-1 evaluator.
The Linux execution and the earlier focused results below were supplied by the owner in
this Task's requirements. The implementing Worker reran both focused suites in this Task
and independently read and SHA256-checked the supplied smoke stdout file. The Worker did
not rerun the Linux containment experiment on this Mac.

The owner tested the materialized repair-1 candidate Snapshot
`sha256:7c7167e1789c801b287df54157e5d0810f283ba548b90ab1bccf4c8cfd46ac46`.
Its disposable Linux checkout was committed locally as
`948c0e8d84a8c78ee9aa30c94dd9efaa2e729c10`. The current Task reads Snapshot
`sha256:364c034968695a3109017ccf8814e9e97136d8885f7755a2a3dd9b539b2bdd14`.
This Task retains evidence and repairs the separate pin-publication credential finding;
it does not change performance scripts, fixtures, schemas, budgets or runtime code.
The Linux evidence is bound to repair-1, not a claim that the new pin script ran on Linux.

## Focused contract suites

Run from the repository root. Each command selects just the named discovery pattern;
`-I -B` isolates Python and prevents bytecode writes. Both stdout streams were empty;
unittest writes its summary to stderr. These tests use stand-in binaries and spend no
model tokens.

```sh
python3 -I -B -c 'import sys, unittest; sys.path.insert(0, "scripts/performance"); result = unittest.TextTestRunner().run(unittest.defaultTestLoader.discover("scripts/performance/tests", pattern="test_contracts.py", top_level_dir="scripts/performance")); sys.exit(not result.wasSuccessful())'
python3 -I -B -c 'import sys, unittest; sys.path.insert(0, "scripts/performance"); result = unittest.TextTestRunner().run(unittest.defaultTestLoader.discover("scripts/performance/tests", pattern="test_telemetry_bench_contracts.py", top_level_dir="scripts/performance")); sys.exit(not result.wasSuccessful())'
```

The owner reports these same `python3 -I -B -c` unittest discovery selections on the
materialized repair-1 Snapshot on 2026-11-09: `test_contracts.py`: 9 tests, `OK`, exit 0;
`test_telemetry_bench_contracts.py`: 12 tests, `OK`, exit 0. Exact elapsed times and full
stderr for those earlier runs were not supplied.

Current Worker rerun on 2026-11-09, after starting this Attempt at 21:41:38 MSK:
first command exit `0`, stderr verbatim:

```text
.........
----------------------------------------------------------------------
Ran 9 tests in 0.682s

OK
```

Second command exit `0`, stderr verbatim:

```text
............
----------------------------------------------------------------------
Ran 12 tests in 7.117s

OK
```

Schema parity checks generated lab schemas and golden fixtures. The telemetry suite proves
explicit release-contract selection, unknown-version refusal and current fixture admission.
The existing `scripts/test-release-contract.py` test
`test_missing_smoke_artifact_names_captured_stderr` separately verifies the missing-artifact
diagnostic, and `test_read_only_source_copies_allow_all_contract_mutations` verifies writable
disposable fixtures without modifying sealed source bytes or modes. All 8 tests in that
script passed here (exit 0).

## Supplied Linux contained real-AF smoke

Execution on 2026-11-09: idle Linux claw VM, disposable checkout of the repair-1 Snapshot
identified above. The pinned GNU build image was:

```text
ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2
```

It built actual AF `0.15.1-rc.1`. Binary SHA256:
`a930eaa3d83bfd3a6d015e2c80a972a914a398b42c941fbe46806fc2215949b6`.
The owner's command (angle-bracket paths denote the disposable paths, not literal argv):

```sh
python3 -I -B scripts/performance/tests/telemetry_runtime_smoke.py --af <built-af> --work-parent <disposable-dir>
```

Exit status: `0`. Full stdout is retained verbatim below. Its original owner-host file is
`/private/tmp/release-guard-smoke-20261109.log` (490 bytes), SHA256
`eb2710179fd6cbf293540de8f2c90a8cad82525fa6916dad8c099ecd5ae2fff6`.
The Worker verified that file's bytes and hash when writing this record. No separate smoke
stderr capture was supplied, so this record makes no claim that stderr was empty.

```text
bounded scratch: gross 4294967296 available 4125827072
real AF setup: overhead exit 1
4 GiB reproduces the public check resource failure before any reviewer
bounded scratch: gross 17179869184 available 16710995968
real AF setup: overhead exit 0
real AF setup: invocation exit 0
real AF setup: corpus exit 0
real AF setup: reads exit 0
actual AF admitted/completed all three experiment paths; smoke scale only
real contained AF setup regression passed; hosted rollout proof remains separate
```

At the former 4 GiB gross scratch capacity, setup deliberately exits 1 and the smoke
asserts the public check resource failure before any reviewer: finished/exhausted,
inconclusive, one Attempt, failed check class `resources`, diagnostic
`Task runtime evidence cannot be empty`, and `finished/exhausted/budget` on captured stderr.
This expected refusal is why the overall smoke can pass while the first setup exits 1.
At the current 16 GiB gross capacity, overhead, invocation, corpus and reads each exit 0.
Actual AF admits and completes all three experiment paths; fixture/receipt assertions pass.

These are focused contract and contained setup results at smoke scale. They establish no
benchmark speedup, hosted rollout success, production-scale capacity guarantee or remote
AF gate result. The Linux binary/checkout and image identities above are owner-supplied;
only the retained stdout hash and the current focused reruns were independently checked by
this Worker. No release, push, PR creation or AF remote gate was performed by this Task.
The mandatory native local gate and independent reviews remain separate kernel receipts.
