# Guarded kernel experiments for this repository

The staged authority of package p7 of
[`docs/design/continuous-performance.md`](../../../docs/design/continuous-performance.md)
([ADR-0185](../../../docs/adr/0185-measure-guarded-kernel-experiments-through-a-literal-bootstrap-over-a-protected-harness.md)),
kept here because a Task Worker may not edit `.af/`. Nothing in this directory is executed: it
is the only subtree the protected-harness digest excludes, because it holds that digest, and it
may hold only `.md`, `.toml` and `.json` data.

- `code-policy-measures.toml` — four `[measures.perf_*]` tables and their `*_elapsed`
  objectives. Each measure runs `python3 -I -S -B -c BOOTSTRAP DIGEST measure --case ID --metric
  wall_ms`, where `BOOTSTRAP` is the literal text of `BOOTSTRAP` in
  `scripts/performance/perflab/kernelguard.py` and `DIGEST` the protected-harness digest of this
  revision. Before the measured Snapshot's driver is imported or anything is built, the
  bootstrap refuses every symbolic link and special file, a missing or extra protected file and
  any changed byte. Protected names match whatever their case or canonical spelling, because a
  case-insensitive filesystem hands Cargo `BUILD.RS` and `.CaRgO/config.toml` too
  ([ADR-0186](../../../docs/adr/0186-fold-protected-paths-key-warm-targets-by-the-whole-snapshot-bound-measure-output-and-replan-bridge-runs.md)). Only then does `cli.py measure` build `perf-probe` offline into the kernel's
  `CARGO_TARGET_DIR`, run one judged sample and print one `af.measure-report/1` line.
- `receipts.json` — kept as recorded: the compact `af.perf-kernel-receipts/1` export of p7's
  integration run on its source before sealing, for that source's harness digest. Its stream IDs
  name a Store that no longer exists. It is evidence only, read by no code, and the current
  reader refuses its version. The integration now exports `af.perf-kernel-receipts/3` to
  `$AF_PERF_KERNEL_RECEIPTS` as three homogeneous v3 documents named
  `receipts-<pid>-missing-inputs.json` (0 ms), `receipts-<pid>-improve.json` (7,500 ms),
  and `receipts-<pid>-refusal.json` (0 ms). The last retains six tampered baseline
  refusals and the build-free policy refusal; the manual test also exports
  `receipts-<pid>-candidate-refusal.json` (1,500 ms) with six tampered-candidate refusals
  and two report-policy cases. The fixture asserts the exact phase partition;
  every document passes both validators. Together these exports hold every Task's typed result, each output's
  complete envelope and every repetition's public stdout and stderr, whole up to 64 KiB, else
  the first and last 32 KiB. It is checked against `schemas/performance-kernel-receipts-v3.json`
  and the laboratory's reader, which recomputes each envelope's content and artifact IDs with
  the kernel's canonical JSON and domains, each whole stream's content ID, and requires the Task
  result to name exactly the retained outputs. A cut stream says so (`retained =
  "head_and_tail"`): its ID and hash are the exporter's, and nothing rehashes the omitted middle
  ([ADR-0187](../../../docs/adr/0187-bind-kernel-receipts-to-their-task-results-word-reports-by-what-was-admitted-and-retain-a-real-local-run.md)).
  The reader also applies `ArtifactEnvelope::validate` and `TaskResultV1::validate`. It refuses
  every retained part longer than 32 KiB once decoded
  ([ADR-0188](../../../docs/adr/0188-validate-kernel-receipts-as-the-kernel-does-and-restore-the-retained-history.md)),
  and a result's `evidence` or `missing_obligations` that is not strictly ascending, as the
  kernel's set deserialization refuses it
  ([ADR-0189](../../../docs/adr/0189-read-a-task-result-s-sets-in-the-kernel-s-order-and-reconcile-a-second-repair-through-the-first.md)).
- `inventory.json` — every protected file's path, size and SHA-256 (`af.perf-kernel-inventory/1`),
  so a re-pin's review shows exactly which instrument, fixture or build input changed.

The objectives compare the kernel's own `elapsed_ms`. Every Measurement also records the judged
probe-region `wall_ms`, but no staged objective selects it: the guarded inner metric stays
`pending` until the guard's real kernel integration and tampered-candidate tests
(`crates/af/tests/it/perf_kernel_measure.rs`) are accepted on a sealed source and a reviewed
human change selects it. A protected harness does not make arbitrary product code trustworthy;
the golden oracles and code review remain necessary.

## Installing

A reviewed human change, never a Worker:

1. Append `code-policy-measures.toml` to `.af/code-policy.toml`. The measures need `[warm]`
   with `build_cache = ["cargo_target", "cargo_home"]` (this repository's policy has it), and a
   Cargo home that already holds the locked dependencies: the build is `--offline --locked`, so
   a missing input fails the repetition instead of being fetched. Appending the tables changes
   the code policy's identity: re-pin any Worker package whose `[signature.evidence]` names the
   old policy digest.
2. Add a `kernel/experiment` Pipeline whose measure node names one of the staged measures and
   whose compare node names its `*_elapsed` objective, as `fixtures/kernel-experiment/`
   describes for `release_build`.

## Re-pinning

Any change to a protected file changes the digest: the laboratory driver, a fixture or golden
entry, the probe or its adapters, the trace or calibration helper, a Cargo manifest, the lock, a
build script, the toolchain pin or Cargo configuration, including a dependency update and every
version bump. Then:

```sh
python3 scripts/performance/cli.py harness-digest --write
python3 scripts/performance/cli.py harness-digest --check
```

`--write` regenerates both files here deterministically; review the inventory's diff. `make
check` (`performance-kernel-harness`) and `scripts/release.sh` run `--check`, which fails while
the staged files are stale or while a guarded measure installed in `.af/code-policy.toml` runs a
bootstrap or digest other than the staged one. The release script re-pins the staged files in
the release commit itself; an installed measure is re-installed by hand, as above.
