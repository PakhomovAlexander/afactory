# ADR-0132: Measure and compare source candidates in the kernel

Status: accepted, 2026-09-28.

Implements package R2 of [`docs/design/research-pipelines.md`](../design/research-pipelines.md)
under that plan's §2 ("the kernel measures; a model never writes a number the kernel did not
record"), on top of [ADR-0131](0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md)
(the Warm Check Cache a warm measure builds into),
[ADR-0093](0093-derive-code-task-acceptance-from-execution-and-evidence.md) (code acceptance
from execution and evidence) and [ADR-0002](0002-event-payload-changes-bump-the-type-version.md)
(new payload shapes are new types).

## Context

A source-writing Worker with a shell can build and time a command in its sandbox (ADR-0120),
but the number it reports is prose in its own `af/ImplementationReport@1`. Nothing in the
runtime runs a declared command under the kernel's control, records its wall time or the bytes
it produced as a typed artifact, or compares two such artifacts. The only comparison machinery,
the self-optimizer's `optimization_experiment`, is fixed to `.af/` configuration and harness
files and to its own cases and recipes. A research Task — "make the release build faster" —
therefore has no evidence an independent evaluator could hold it to.

## Considered options

- **Let the implementer report its own numbers.** Rejected: the Worker that proposes the
  candidate would author the evidence that judges it. Its numbers come from its own sandbox,
  under an environment nobody recorded, and a model can round, omit or invent them. The design's
  fixed requirement is that no measured value reaches acceptance unless the kernel observed it.
- **A generic `command` operator with untyped output.** Rejected: an untyped stdout blob cannot
  be compared, a `when` cannot branch on it, and every consumer would re-parse it with its own
  rules for units, even samples and zero. The unit a value is in, the number of repetitions and
  what failure means must be part of the contract, not of each reader.
- **Reuse `optimization_experiment` for source candidates.** Rejected: its slot owns a separately
  approved child closure whose writable target is `.af/` configuration and harness files, with
  recipes and protected cases that exist for the optimizer's economics. A source candidate is an
  ordinary sealed Snapshot that the ordinary `seal` → `check` chain already produces; routing it
  through the optimizer would widen that slot and couple two unrelated acceptance rules.
- **Two installed code-domain operators over typed artifacts (chosen).**

## Decision

1. `af.code-task-policy/1` gains optional `[measures.<name>]` and `[objectives.<name>]` tables.
   A measure is a `command` (program and args with `literal` or `untrusted` provenance, resolved
   exactly as a check's), `repetitions` (1 to 16), `warm`, `wall_ms` per repetition (at most
   3,600,000) and `metrics`, a list of `{ key, unit }` with `unit` one of `ms`, `bytes`, `count`
   and `ratio`. `elapsed_ms` is the built-in metric; a declared metric may not reuse its key.
   `warm = true` requires `[warm] build_cache` to declare `cargo_target`. An objective names one
   `measure`, one of its metrics, a `direction` (`lower` or `higher`), `min_improvement_ratio`
   (0 to 1 inclusive, written as canonical decimal text such as `"0.1"` or as the integer 0 or 1;
   a TOML float is refused, because the parser rounds it before the kernel can capture it) and
   `min_repetitions` (1 to 16, default 3). A policy without the tables is captured byte-identical
   to before.
2. The installed `measure { measures }` operator takes one `source: af/SourceTree@1` and emits
   one `af/Measurement@1` per named measure, on an output port named by the measure. Each is an
   outcome receipt (`passed` or `failed`), so `when` and `select` can branch on it. It holds one
   zero-token Attempt whose wall is the captured `check_wall_ms`; the plan compiler refuses a
   measure node whose measures' summed `repetitions × wall_ms` exceeds it (a measure Attempt owns
   no checks), and refuses a measure the captured policy does not declare. The compiler composes
   the node's contract from per-measure installed signatures, and the code domain recomposes it
   from the captured policy before it runs anything.
3. Each repetition runs against a fresh read-only materialization of the exact Snapshot, as its
   working directory, with a private runtime directory holding `HOME`, `TMPDIR`,
   `XDG_CACHE_HOME` and, for `warm = false`, `CARGO_TARGET_DIR`. `warm = true` binds the Warm
   Check Cache's `cargo_target` directory under its key lock, byte bound and monitor, exactly as
   a check does; any other declared warm kind or Cache Snapshot binds as it does for a check, and
   under `[warm]` the kernel's rustup home is passed on. After every repetition the Snapshot's
   Manifest is re-verified; a changed or added entry fails the measurement with the message a
   mutated check produces. The runtime directory is discarded: a command that cares about bytes
   it produced reports them itself.
4. The command's last stdout line is its report when it is an `af.measure-report/1` JSON object,
   `{"schema":"af.measure-report/1","metrics":{"<key>":{"value":"<decimal>","unit":"<unit>"}}}`.
   It must name exactly the declared keys, each value a canonical non-negative decimal of at most
   38 significant digits. A measure that declares metrics requires the line.
5. A repetition that exits non-zero, is ended by a signal or cannot start (`exit`), exceeds its
   `wall_ms` (`timeout`), is cut by or cannot start before the Attempt deadline (`deadline`),
   reports a malformed line (`malformed_report`), reports another unit (`unit_mismatch`) or
   mutates its source (`source_mutated`) fails the measurement. No later repetition runs, the
   failed repetition's receipt is kept, and the measurement carries no summary. A passed
   measurement records every repetition and, per metric including `elapsed_ms`, the exact
   median, minimum, maximum and `n`. A measurement is never partial.
6. `af/Measurement@1` records the plan, policy and Snapshot, the measure, the content identity of
   the resolved command, the Warm Check Cache toolchain key when one was resolved, `warm`, the
   declared repetitions, wall and metrics, and one run per repetition: start time, elapsed
   milliseconds, exit code, stdout and stderr digests, the cargo target it actually ran against
   when the measure asked for the Warm Check Cache — warm or cold, its bytes, and the reason when
   it ran cold — and the parsed metrics. Every value is the kernel's observation or the command's own
   report admitted under the declared keys and units.
7. The installed `compare { objective }` operator takes `baseline` and `candidate`, each one
   `af/Measurement@1` straight from a measure node, and emits one
   `af/MeasurementComparison@1` outcome receipt. It invokes no process and holds no Attempt, so
   the same two Measurements always fold to the same artifact. The compiler refuses an objective
   the policy lacks, two inputs of different measures, and inputs of a measure the objective
   does not compare.
8. The fold uses exact decimal arithmetic; nothing is saturated, clamped or rounded. The median
   of an even sample is the exact mean of its two middle values. Per metric the improvement is
   `baseline − candidate` for `lower` and `candidate − baseline` for `higher`; the ratio is
   `improvement / baseline`, kept as a fraction in lowest terms (`1/19`) because it rarely
   terminates as a decimal; it is absent with a zero baseline. The conclusion is `improved` for a
   strictly positive improvement whose ratio is at least `min_improvement_ratio`,
   `below_threshold` for a strictly positive one whose ratio is not, `regressed` for a strictly
   negative one, `unchanged` for exactly zero (including zero against zero), and `inconclusive`
   when either side has fewer than `min_repetitions`, either measurement failed, or the baseline
   is zero and the candidate is not. The design names four conclusions; `below_threshold` is the
   fifth it describes without naming, so that an insufficient improvement is visible as such.
   The comparison's outcome is `passed` only for `improved` on the objective's metric, `failed`
   for `regressed`, `unchanged` or `below_threshold`, and `inconclusive` otherwise. A zero
   threshold therefore still requires a strictly positive improvement.
9. An experiment Pipeline measures the source as its baseline, lets the implementer write the
   candidate, seals and checks it, and — only after passed checks, through a called child
   Pipeline — measures the candidate, compares, and dispatches an independent evaluator whose
   contract declares a `comparison` input, gated with `when` on the comparison passing. A
   `failed` or `inconclusive` comparison therefore never reaches an evaluator, and the Task ends
   `incomplete` with the comparison as its public explanation. The call is how a node is gated
   on two receipts: `when` takes one, and the child inherits the call's condition. The public
   outputs are `snapshot`, `verification`, `baseline`, and — produced only after passed checks
   — `candidate` and `comparison`. The Pipeline runs one node at a time (`max_parallel = 1`) so
   that no measurement shares the machine with other work of the same Task.
10. `af task output --port comparison --format markdown` renders a comparison as one table;
    `--format json` is unchanged. `af task show` prints each Measurement's median elapsed time,
    or the repetition and reason it failed at, and each comparison's conclusion. `af catalog
    init --profile experiment` emits the `builtin/experiment` starter.

## After the first verification

Verification of the package (Task `research-r2-verify-2`) changed three rules above, and the text
now reads as amended:

- **A time bound is classified from the supervisor's typed ending, never from its message.** The
  cancellable supervisor a Task runs under keeps a timed-out command's output and reports the
  timeout typed; the runner now carries that ending beside the recorded result, and a repetition
  the kernel ended records `timeout` (its own wall) or `deadline` (the Attempt's), with what it
  printed kept, instead of `exit`.
- **A run records the cache condition it had, not the one the policy asked for.** A `warm = true`
  measure whose key is busy or whose toolchain cannot be resolved runs cold against a private
  target, and one whose directory the kernel discarded before binding runs against the emptied
  directory; either run says `cache.warm = false`, `cache.bytes = 0` and why, and `af task show`
  prints `cold (busy)` or `warm 2 of 3` after the median. The measurement's `warm` stays the
  request.
- **`min_improvement_ratio` is text or the integer 0 or 1; a float is refused.** A TOML or JSON
  float has been rounded to a binary fraction by the parser before the kernel sees it, so a
  threshold that is compared exactly cannot be captured from it: `0.10000000000000001` would
  have been captured as `0.1` and passed an improvement of exactly one tenth.

## After the second verification

The package was verified (Task `research-r2-verify-3`); the two findings its reviewers still
reported were fixed after the verdict, with tests, and are not re-verified:

- **A signal leaves no exit code.** The check runner records `-1` for a command a signal ended;
  a run's `exit_code` is the command's own and absent otherwise, and the failure says a signal
  ended it.
- **A discarded directory is recorded as the cold run it was.** The warm layer keeps why it
  discarded a directory before binding it, and the repetition records `cache.warm = false` with
  `discarded: <why>`.

## Consequences

- A research Task's number is evidence: an evaluator reads a comparison it cannot change, and a
  failed or inconclusive comparison cannot be argued into acceptance.
- `schemas/measurement-v1.json`, `schemas/measurement-comparison-v1.json` and the two policy
  tables in `schemas/code-task-policy-v1.json` are pinned against the Rust types in both
  directions; the two operators are closed members of the Pipeline operator schema.
- A policy that declares measures installs `operator/measure`, `operator/measure/<name>`,
  `operator/compare`, `operator/compare/<objective>` and
  `operator/compare/<objective>/<measure>`; a policy without them installs nothing new, so
  existing plans, fixtures and `--json` documents are byte-identical.
- Adding measures to a committed code policy changes its identity. A Worker package whose
  `[signature.evidence]` pins the old policy digest must be re-pinned with it.
- This repository's `release_build` measure and `release_build_time` objective, the
  `kernel/experiment` packages and `scripts/measure-release.sh` are staged under
  `fixtures/kernel-experiment/` for installation into `.af/` by a human, because a Task
  Worker may not edit `.af/`; a test installs them into a copy of `.af/`, passes `af catalog
  test` and plans the Pipeline with zero Attempts.
- The self-optimizer's `optimization_experiment` is unchanged; its `latency` recipe is the
  natural later consumer of `af/Measurement@1`.
