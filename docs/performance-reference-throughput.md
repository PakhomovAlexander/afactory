# Reference-generation throughput repair

Package: `telemetry-performance-verification-throughput`, 23 October 2026.
Decision: [ADR-0201](adr/0201-construct-reference-fixture-bytes-without-per-byte-python-loops.md).

## Scope

`perflab.reference` now builds deterministic letters, digit padding and source-file fill by
repetition and prefix slicing. Its encoder scans for characters outside exactly the original
restricted ASCII domain with a compiled regular expression. It still constructs reference
bytes and hashes independently of the native product and committed golden answers, on every
call for the exact supplied registry. No new cache or remembered admission verdict is involved.

No Rust source, Task runtime bookkeeping, schema, gate, budget, timeout, workload, workflow,
archive collection, frozen cohort or `.af/` authority changes belong to this repair. The
protected inventory and staged policy are regenerated with the existing tool.

## Unprofiled comparison

Host: `macOS-27.0.1-arm64-arm-64bit`; Python 3.9.6, Clang 21.0.0.
The baseline is an untouched copy of the input reference module, saved before editing.
Each fresh process loads the same full registry, then times **two complete** calls to
`golden_document`, including contract-checked serialization and output SHA-256 hashing.
Contract validation imports that side's reference module too; the runner asserts its identity.
Registry parsing and module loading precede the timed region for both sides. There is no
profiler, affinity control or OS-cache flush. No test suite or Cargo build from this Attempt
ran concurrently with these samples. Other host activity was not controlled.

The bounded order was AB, BA, AB (A = baseline, B = candidate), using the saved unchanged
module for each baseline process. Exact elapsed samples, in order:

| Sample | Side | Raw elapsed ns | Seconds |
| --- | --- | ---: | ---: |
| 1 | baseline | 3934396375 | 3.934396375 |
| 2 | candidate | 987163375 | 0.987163375 |
| 3 | candidate | 982597625 | 0.982597625 |
| 4 | baseline | 3907514792 | 3.907514792 |
| 5 | baseline | 3947632500 | 3.947632500 |
| 6 | candidate | 1308442333 | 1.308442333 |

The baseline median is 3.934396375 s; the candidate median is 0.987163375 s, about 74.9%
less elapsed time (3.99 times the throughput) for this focused command. The final candidate
is visibly slower than the others; no sample was dropped. Three pairs on a shared host
establish a local focused improvement, not a confidence interval, product improvement,
complete-suite speedup or assurance that the one-hour gate will finish.

All **twenty-four** generated documents (two in each of six final and six preliminary processes)
had exactly the committed
golden file's SHA-256, covering every declared native instance, seed writer, fixture identity
and cap boundary:

```text
2c4f31bcf6a2db2e01e814ab696c14576707961d59c79a923d4d684bdb38eeb7
```

The measured reference module hashes were:

```text
baseline  21842d32e4f8639ced07dfae75e381d5ba82c2625063022f0eb61c2223607552
candidate 57837c8ccb4a3f9239ed33d2861911d281cc005e1d7c673c4ab10dd7bab2cfed
```

The frozen workflow and cohort retained these input hashes:

```text
.github/workflows/performance.yml
191847a0d127ff7ff54ba4d2f242a18b4d74d40d2c19f89115f7804ff6ecdd42
fixtures/performance/cohort-quick-20261023-hosted-v6.json
62955096a58517dad3544425ead9b18ebe3b85de94b8518d5ce85deb761d5ab7
```

### Preliminary samples retained

An initial three-pair comparison loaded the measured generator as
`perflab.measured_reference`, after importing contracts. Consequently, contract validation
used the checkout's reference module rather than the saved baseline module in later baseline
processes. That validation touches only small semantic projections, but these samples are
not used for the final comparison above. The first baseline preceded any edit; later baselines
used the saved module. These are all preliminary samples, in order, without filtering:

| Sample | Side | Raw elapsed ns | Seconds |
| --- | --- | ---: | ---: |
| 1 | baseline | 11197387209 | 11.197387209 |
| 2 | candidate | 1864100833 | 1.864100833 |
| 3 | candidate | 1287687250 | 1.287687250 |
| 4 | baseline | 6047004292 | 6.047004292 |
| 5 | baseline | 5955837417 | 5.955837417 |
| 6 | candidate | 984379542 | 0.984379542 |

Their source and output hashes are the same ones listed above. The corrected runner below
binds the module for both generation and validation before importing contracts.

### Reproduction

From the repository root, before editing, save `reference.py` as
`target/reference-throughput/reference_before.py`. Save the following scratch runner as
`target/reference-throughput/measure.py` and run
`python3 -I -B target/reference-throughput/measure.py baseline` or the same command with
`candidate`, in the order above. Scratch files are not shipped. This is the exact runner
used for the final six samples:

```python
"""Scratch-only focused measurement; independent full reference documents, no profiling."""
import hashlib
import importlib.util
import json
import pathlib
import platform
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/performance"))
import perflab

side = sys.argv[1]
path = (ROOT / "target/reference-throughput/reference_before.py" if side == "baseline"
        else ROOT / "scripts/performance/perflab/reference.py")
spec = importlib.util.spec_from_file_location("perflab.reference", path)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
perflab.reference = module
spec.loader.exec_module(module)
from perflab import contracts, suite
assert contracts.reference is module
doc = suite.load(str(ROOT / "fixtures/performance/suite.toml")).doc
digests = []
started = time.perf_counter_ns()
for _ in range(2):
    data = contracts.write_bytes(module.golden_document(doc), "golden")
    digests.append(hashlib.sha256(data).hexdigest())
elapsed = time.perf_counter_ns() - started
expected = hashlib.sha256((ROOT / "fixtures/performance/golden/native.json").read_bytes()).hexdigest()
assert digests == [expected, expected], digests
print(json.dumps({"side": side, "elapsed_ns": elapsed, "digests": digests,
                  "reference_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                  "python": sys.version, "platform": platform.platform()}))
```

## Regression evidence

The changed-path tests exercise the literal per-position definitions, complete committed
golden output, exact 8-MiB boundary and one byte more, ASCII keys and values, Unicode and lone
surrogates, empty/negative/index/unsupported counts, changed fixture sizes, independent repeated
derivation, changed registry envelopes and event counts, and native seed facts. The new CLI
fixture rejects stale golden data for a changed registry, regenerates it, and then rejects a
forged seed with a recomputed semantic digest. Existing `probe` and `judge` tests still reject
forged golden data before launching or judging anything.

Completed checks:

- `cargo fmt --all` and workspace/all-target Clippy with `-D warnings`, using pinned Rust 1.88.0.
  Clippy used `--target-dir target` and a checkout-local `TMPDIR` after the inherited shared
  target and host temporary directory refused writes. No Rust crate changed, so no Rust tests
  were selected inside this implementer.
- `tests.test_reference` and `tests.test_oracle`: 60 tests passed on Python 3.9.6 and 60 on
  Python 3.12.13.
- `tests.test_contracts`, `tests.test_suite`, `tests.test_kernelguard`: 56 tests passed on
  Python 3.9.6, with the two existing `tomllib`-requires-3.11 skips. The initial sandboxed run
  could not launch `ps` for a descendant assertion; the same unchanged tests passed with
  process-inspection permission. No test or timeout changed.
- Python 3.12 `tests.test_contracts` and `tests.test_suite`: 28 tests passed with
  `AF_PERF_REQUIRE_TOMLLIB=1`, including parser parity, with no skips.
- `scripts/performance/schemas.py`, `cli.py golden`, `cli.py harness-digest --write` followed
  by `--check`, and Markdown lint 0.22.1 for all four changed Markdown files.

Tests used synthetic HOME/XDG/TMPDIR directories under the checkout's `target/`; no operator
AF state was used. Rust and npm created scratch cache directories at the sandbox's HOME;
these Attempt-created caches were removed before the re-pin. The no-Git sandbox's whole-tree
guard had correctly refused their extra protected files and npm symlinks. The final inventory
and staged policy use digest
`sha256:96632274ca4e0349bcbd967d6d49f1bfbff8e4d919fb08da1a1d7ba0d713a5f1`.

The full repository gate, Terra bug review, Astra correctness review and independent Astra
goal verification run after sealing, under the coordinator. They are not claimed by this
focused measurement or by the implementer's selected tests.

## Linux verifier follow-up: fixture portability

Package: `telemetry-linux-fixture-repair`, 23 October 2026. The supplied native check receipt
for retained candidate `0d4255301c79e8288e38b737a947952f22bb90a0`
(`telemetry-performance-claw-gate-20261023`) records **765 Python tests in 452.551 s, with
six assertion failures**, and Markdown lint passing for 348 files. Its verifier was Claw
Ubuntu 26.04, Python 3.14.4, non-root `af-task`, pinned Rust 1.88, with service umask `0077`
and a private `0700` state root. The check receipt is
`sha256:302e4e909acf7b980fda4e9b24dab59c269a8511021d451f99ba5e7c5642667f`;
its stderr is
`sha256:95c3feeac087f7d231830263b4f8839ec0f4aa7d4dfc193f5a36a46a8b705920`.
This is supplied Linux evidence, not a new Linux execution by this implementer. It establishes
neither a full Rust gate nor semantic acceptance of the telemetry integration.

Five failures were seed-fixture assertions assuming a `0755` root when the writer's umask
created `0700`. The sixth assumed `/bin/ls` resolved into a macOS protected prefix; on that
Linux host it resolved to `/usr/lib/cargo/bin/coreutils/ls`. Before editing, focused local
reproductions failed on all six assertions: the seed tests ran under `0077`, and the samply
test used that exact Linux resolution as a controlled stand-in. No system path was modified.

The corrected seed fixtures explicitly create the four hostile entry-link cases and both
sparse-size cases under `0022` and `0077`, then compare root and entry modes with their originals.
They check unchanged regular bytes, unchanged sparse-file metadata without reading its
logical contents, no admission reads, and unchanged owned markers outside the seed.
The umask is restored in `finally`, including setup failure. Symlinked-root and FIFO
refusals retain their cases and now also compare original modes and regular bytes.
The samply fixture supplies deterministic macOS canonical paths for all six protected
prefixes and an owned symlink resolving into one of them. Real stand-in tools still exercise
version, required flags, the allowed self-built target and hardened-runtime refusal.
Production Python and Rust, the committed golden oracle, the frozen cohort and workflow,
and all gates and budgets are unchanged. The full gate and independent integration review
remain kernel-owned; this fixture repair cannot replace them.

Focused checks in this macOS sandbox passed without skips: `tests.test_admission` and
`tests.test_diagnostics`, 87 tests on Python 3.9.6 with inherited umask `0022` (156.404 s),
and the same 87 on Python 3.14.7 with inherited umask `0077` (118.121 s). The loader used
`-I -B` with HOME, XDG and temporary directories under `target/fixture-repair/`; it also
asserted normal and exceptional umask restoration. The three `tests.test_kernelguard.Staged`
tests passed on Python 3.14.7. Markdown lint 0.22.1 checks both changed Markdown files.
The golden oracle and frozen workflow/cohort SHA-256s match the pre-edit bytes listed above.
The existing `harness-digest --write` tool regenerated the inventory and staged policy;
`--check` passes with
`sha256:6ec80b334642b5b058ec3d0d04e7457e03af897d786797bb08bbc4f1e91edd7e`.
Only the two corrected test modules differ in the protected inventory. No Cargo build,
Rust suite, full Python suite, benchmark or remote operation ran for this fixture repair.
