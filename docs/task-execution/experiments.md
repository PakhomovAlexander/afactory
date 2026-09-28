# Experiments: measure and compare

An experiment Task asks whether a candidate the implementer wrote improves a number the
repository cares about — release build time, bytes on disk — and lets only the kernel answer.
The kernel runs a declared command on the Task's source (the baseline) and on the sealed
candidate, records each run, and compares the two under a declared objective. An independent
evaluator reads the comparison and is dispatched only when it passed. A model may propose the
candidate and may read the comparison; it cannot author either
([ADR-0124](../adr/0124-measure-and-compare-source-candidates-in-the-kernel.md)).

## Declaring a measure and an objective

Both live in the committed code policy, `.af/code-policy.toml`:

```toml
[measures.write]
repetitions = 3          # 1 to 16
warm = false             # true: build into the Warm Check Cache's cargo_target (ADR-0123)
wall_ms = 20000          # per repetition, at most 3600000

[measures.write.command]
program = "python3"

[[measures.write.command.args]]
value = "measure.py"
provenance = "literal"

[[measures.write.metrics]]
key = "bytes_written"
unit = "bytes"           # ms, bytes, count or ratio

[objectives.smaller]
measure = "write"
metric = "bytes_written" # or the built-in elapsed_ms
direction = "lower"      # or higher
min_improvement_ratio = 0.10
min_repetitions = 3      # default 3
```

`elapsed_ms` is always recorded and cannot be declared. `warm = true` needs `[warm]
build_cache` to declare `cargo_target`. The plan compiler refuses a `measure` node whose
measures' summed `repetitions × wall_ms` exceeds the captured `check_wall_ms` — one measure
node is one Attempt with that wall — and refuses a measure or an objective the policy does not
declare, a `compare` of two different measures, and a `compare` whose objective names another
measure. The refusal names the node and the numbers, before any Attempt.

## What one repetition gets

- The working directory is a fresh read-only materialization of the exact Snapshot.
- `HOME`, `TMPDIR` and `XDG_CACHE_HOME` point into a private runtime directory that is discarded
  after the repetition; with `warm = false`, so does `CARGO_TARGET_DIR`. The command reports the
  bytes it cares about itself before the directory goes.
- With `warm = true`, `CARGO_TARGET_DIR` is the Warm Check Cache directory for the resolved
  toolchain key, under the same lock, byte bound and monitor as a check. Any other declared warm
  kind or Cache Snapshot binds exactly as it does for a check, and the kernel's rustup home is
  passed on under `[warm]`.
- After the command the Snapshot's Manifest is verified again. A changed or added entry fails
  the measurement with `source_mutated` and the message a mutated check produces.

## The report line

The command's last stdout line, when it is an `af.measure-report/1` JSON object, is its report:

```json
{"schema":"af.measure-report/1","metrics":{"bytes_written":{"value":"4096","unit":"bytes"}}}
```

It must name exactly the declared keys. Each value is a canonical non-negative decimal string
— no sign, exponent, leading zero or trailing fractional zero — of at most 38 significant
digits, and each unit must equal the declared unit. Everything else the command prints is kept
by digest and never read. A measure that declares metrics requires the line.

## Failure is never partial

The first repetition that fails ends the measurement; no later one runs, its receipt is kept,
and the `af/Measurement@1` is `failed` with one reason and no summary:

| Reason | When |
|---|---|
| `exit` | the command exited non-zero, was ended by a signal or by the warm cache's bound, or could not start |
| `timeout` | the repetition exceeded its `wall_ms` |
| `deadline` | the measure Attempt's deadline cut the repetition or left no time to start it |
| `malformed_report` | declared metrics without a well-formed report line, extra or missing keys, or a malformed value |
| `unit_mismatch` | a reported unit differs from the declared one |
| `source_mutated` | the command changed or added a source entry |

A passed Measurement records every repetition and, per metric including `elapsed_ms`, the exact
`median`, `min`, `max` and `n`. The median of an even sample is the exact mean of its two
middle values: `100` and `101` give `100.5`.

## How a comparison concludes

`compare { objective }` folds a `baseline` and a `candidate` Measurement of one measure into an
`af/MeasurementComparison@1`, for every metric, in exact decimal arithmetic:

- the improvement is `baseline − candidate` for `lower`, `candidate − baseline` for `higher`;
- the ratio is `improvement / baseline` as a fraction in lowest terms (`1/5`, `-2/69`), absent
  with a zero baseline;
- `improved`: strictly positive improvement and ratio at least `min_improvement_ratio`;
- `below_threshold`: strictly positive improvement and a smaller ratio;
- `regressed`: strictly negative improvement;
- `unchanged`: an improvement of exactly zero, including zero against zero;
- `inconclusive`: either side has fewer than `min_repetitions`, either measurement failed, or
  the baseline is zero and the candidate is not.

The comparison's outcome is `passed` only for `improved` on the objective's metric; `failed`
for `regressed`, `unchanged` and `below_threshold`; `inconclusive` otherwise. A zero threshold
still needs a strictly positive improvement. Nothing is saturated, clamped or rounded, and
`compare` holds no Attempt: the same two Measurements always give the same artifact.

## The experiment Pipeline

```text
source ──> measure (baseline)
source ──> implementer ──> seal ──> check ──[passed]──> trial:
                                                   measure (candidate)
                                                   compare(baseline, candidate)
                                                   evaluator ──[comparison passed]
           accept(snapshot, checks, evaluation) <──┘
```

The Pipeline runs one node at a time (`max_parallel = 1`), so no measurement shares the machine
with the implementer or a check of the same Task. `when` takes one receipt, so the trial is a
called child Pipeline gated on the checks, and the
evaluator inside it is gated on the comparison: it runs only when both passed. A `failed` or
`inconclusive` comparison leaves the evaluation absent, and the Task ends `incomplete` with the
comparison as its public explanation. The public outputs are `snapshot`, `verification`,
`baseline`, and — after passed checks — `candidate` and `comparison`. The evaluator's contract
declares `comparison` beside `checks`, `requirements` and `source`, all on the sealed Snapshot.

`fixtures/task-runtime/experiment/` is the credential-free fixture repository
(`crates/af/tests/task_experiment.rs` pins every outcome above), and `af catalog init --profile
experiment` emits the same shape as the `builtin/experiment` starter
([starters](starters.md)).

## Reading the result

```sh
af task show TASK_ID
af task output TASK_ID --port comparison --format markdown --output comparison.md
af task output TASK_ID --port candidate --output candidate.json
```

`af task show` prints one line per Measurement — its median elapsed time, or the repetition and
reason it failed at — and one per comparison with its conclusion on the objective's metric.
The Markdown export of a comparison is one table, a row per metric; `--format json` writes the
exact artifact envelope.

## This repository's release build

This repository's experiment measures `cargo build --release -p af --locked` through
`scripts/measure-release.sh`, which reports its target directory's byte total and the `af`
binary's size. The `release_build` measure, the `release_build_time` objective and the
`kernel/experiment` packages are staged in `fixtures/kernel-experiment/` with installation
steps; a test installs them into a copy of `.af/`, passes `af catalog test` and plans the
Pipeline with zero Attempts.
