# ADR-0162: Freeze performance plans and judge them by exact paired verdicts

**Status:** accepted (2026-10-19); amended 2026-10-19
([p1 repair](#amendment-2026-10-19-p1-repair)), which makes `compare` enforce what decision 5
already required and makes the Python compatibility legs gate a required check. Package p1 of
the accepted [continuous performance laboratory](../design/continuous-performance.md) design;
later packages add execution, reports, archive, profiles, CI scheduling, calibration and kernel
experiments under their own decisions.

## Context

The owner accepted a design for a performance laboratory that answers, with retained
evidence, whether a pull request or release made a critical AF operation materially slower,
whether shipping speed targets are met on named environments, and why. Its correctness rests
on contracts fixed before any sample is taken: which cases exist and how they are expanded,
which revision measures which, how many pairs run in which order, and how a verdict follows
from the records. A verdict computed from loosely shaped evidence would turn a missing record,
a swapped binary or a float rounding into a false pass, and a plan that could be revised after
seeing samples would turn a fixed-N interval into a data-dependent one.

Nothing in AF ran performance comparisons. The Review Kernel already has Measurement and
Comparison (ADR-0132), which observe a declared command per repetition; the laboratory is a
different instrument with paired same-host samples, a corrected family of gates and its own
evidence. It must work on the Python the repository's scripts already require (3.9 through
current), download nothing at run time, and leave every product constant unchanged.

## Considered options

1. **Build the laboratory into the Rust workspace or as an `af perf` command.** Rejected: the
   design keeps v1 outside the product's public surface and its kernel contracts, so the
   instrument can measure releases it is not part of. A new CLI surface is explicitly deferred.
2. **Validate evidence with a JSON Schema library and hand-written schemas.** Rejected: a
   runtime dependency would be downloaded or vendored at size, and hand-written schemas drift
   from the readers. The readers and the committed schemas come from one definition instead.
3. **Read TOML with `tomllib` only.** Rejected: it exists from Python 3.11, and two parsers by
   version would read the registry differently if they ever disagreed. Every version uses the
   vendored Tomli; 3.11+ proves the standard library agrees.
4. **Floats, `decimal.Decimal` or rounded strings for metrics and bounds.** Rejected: a bound
   at exactly the tolerance must classify the same everywhere. Metrics are canonical decimal
   strings and every derived value an exact `fractions.Fraction`, written as a lowest-terms
   rational.
5. **Python's `random` for the AB/BA schedule.** Rejected: it binds the schedule to one
   interpreter's generator. Each block's order is one bit of a SHA-256 over seed, instance,
   phase and block, which any implementation recomputes.
6. **Treat an unavailable baseline or authority as invalid input.** Rejected: an absent release
   tag or a missing activation is not an operator error, and must not look like one. It is an
   explicit unavailable refusal, exit 4, and never a substitute baseline.
7. **Let an unstable shard, a baseline failure or an inconclusive case outrank a proven
   regression elsewhere.** Rejected: those make their own cases incomplete, but a regression on
   a healthy shard is still established. Only integrity and coverage failure, which leave the
   evidence itself in doubt, take precedence over fail.
8. **Commit an activation now.** Rejected: nothing is calibrated, so a full plan reports
   `no_active_authority` until a reviewed activation binds a passing cohort.

## Decision

1. **Boundary.** The laboratory is `scripts/performance/` (`cli.py`, the `perflab` package and
   its tests): Python 3.9+ standard library plus vendored, unmodified Tomli 2.2.1, whose source
   archive digest, license and every file digest `perflab/_vendor/VENDOR.json` records and a test
   rehashes. Its Performance Case, Performance Sample, Performance Run and Performance Verdict
   are distinct from Measurement, Comparison, Evidence and Demand (`CONTEXT.md`). p1 offers
   `list`, `plan` and `compare`; no command it ships builds, executes or reports a run.
2. **Registry.** `fixtures/performance/suite.toml` (`af.perf-suite/1`) declares workload
   (cases, typed finite parameter domains, timing boundary, cache treatment, oracle), policy
   (criticality, tolerances, pairs, budgets), recipe and environment separately, with the
   initial design coverage: 28 quick, 31 nightly and 9 release instances, 40 pairs for
   discovery and small canonical/CAS cases and 20 otherwise in quick, 40 in nightly and
   release, `r = 0.05`, `a = 1 ms`. Every recipe, environment and case is `not_yet_executable`
   with a note naming the package that supplies what is missing, every oracle and fixture
   golden is pending, and every budget is `pending_calibration`; a `supported` state requires
   what it would need (a container image digest, an available oracle). Floats and dates are
   refused; a tolerance other than the initial policy must be calibrated and name its cohort.
   Tests pin the recipes to `rust-toolchain.toml` and the release workflow's cargo-zigbuild and
   Zig, and the recorded production lease and envelope cap to their Rust constants.
3. **Contracts.** Fourteen closed, versioned documents: suite, run, builds, sample, shard,
   manifest, verdict, profiles, cohort, calibration parameters, readiness, activation, probe
   message and plan refusal (`af.perf-*/1`). Each is one definition that both validates and
   generates `schemas/performance-*-v1.json` (`scripts/performance/schemas.py` checks them),
   plus semantic rules a schema cannot state, shared through `perflab/values.py`: IDs
   `[a-z][a-z0-9_]{0,63}`, canonical decimals of at most 38 significant digits, lowest-terms
   rationals, canonical JSON without floats, relative paths, 64-KiB records. Golden valid and
   invalid examples under `fixtures/performance/schema/` are checked by the readers and, through
   an independent evaluator, by the committed schemas: shape-invalid examples fail the schema,
   semantic-invalid ones pass it and fail the reader. An unknown version or field fails closed;
   nothing migrates an earlier shape (ADR-0145).
4. **Frozen plans.** `plan` resolves every selector once to a full SHA through read-only Git
   plumbing and applies the trigger's baseline rule: a PR's merge commit against its first
   parent, which must equal the event's base SHA and is also the control revision; a main push
   against its first parent; nightly frozen main against the highest reachable stable tag;
   a release tag against the highest smaller stable version, prereleases only for a prerelease;
   manual refs as given. A missing parent, an ambiguous or absent tag and an unresolved selector
   are unavailable. A `full` plan reads `fixtures/performance/activation.toml` at the control
   revision, requires its authority to be reachable from it and its policy digest to equal the
   authority registry's, and is the only scope whose plan is `required_check_eligible`.
   `calibration` takes an explicit proposed authority reachable from the trusted control
   revision and a declared cohort member, without any activation, so the first activation is
   possible. `bootstrap` may measure with any proposed authority; `partial_scope` selects cases;
   `diagnostic` copies its parent's identities for at most eight cases. Candidate changes to
   the instrument paths are recorded as `harness`. The plan freezes expansion, K, each case's
   rank and alpha, the sorted round-robin shards, the seeded AB/BA orders, warmups, sentinels
   and limits within the hard maxima, and its digest; the reader recomputes every one of them.
   A refusal is an `af.perf-plan-refusal/1`: invalid input exits 2, unavailable exits 4.
5. **Exact verdicts.** `compare` reads a run directory (`run.json`, `builds.json` and their
   manifest; one bundle per shard) and writes nothing into it. It matches every planned slot,
   sentinels included, to exactly one record and refuses unexpected, duplicate, reordered,
   wrong-side, wrong-source, wrong-binary, wrong-host and undeclared-metric records, tampered,
   symlinked or extra files and missing shards. `paired_median_tolerance_v2` runs on exact
   rationals: K counts critical instances only, advisory intervals use 0.05 and are exploratory,
   and a case passes only with `U <= 1`. Functional failures, warmups included, are attributed to
   the side that produced them: candidate only fails, any baseline failure is incomplete with
   candidate failures preserved; a missing required metric in a present record is a functional
   failure. Sentinels feed the health rule, never K; a flagged shard keeps its numbers and makes
   its cases `environment_unstable`. An enabled applicable budget fails unless every candidate
   sample is strictly below it. The aggregate takes integrity or coverage failure (incomplete,
   exit 4) over any critical fail (exit 3) over any other critical incomplete (exit 4) over
   pass (exit 0); only a measured, passing, active full run satisfies the required check.
6. **Synthetic evidence is labeled.** `perflab.synthetic` writes bundles whose shards say
   `provenance = "synthetic"`; such a verdict never satisfies the required check. The committed
   comparison fixtures under `fixtures/performance/compare/` and the synthetic registry are
   labeled so, and the fixture test regenerates them byte for byte.
7. **Gates.** `make preflight-check` runs the laboratory's deterministic tests and the schema
   check; neither runs a benchmark. The `performance lab` CI job runs them on Python 3.9, 3.11
   and 3.13, and on 3.11+ requires the Tomli/`tomllib` parity check instead of skipping it.

## Consequences

- Until a reviewed activation exists, `plan --scope full` is unavailable and nothing gates on
  performance; bootstrap, partial and calibration plans can be frozen today.
- Later packages implement `run`, `report`, `profile`, `history`, `archive`, `measure`,
  `harness-digest` and `activate` against these contracts. A changed shape takes a new version
  with its reader, schema and fixtures in the same change, and the old version fails closed.
- A registry change to a case, oracle, tolerance, pair count, recipe, environment or instrument
  changes the policy digest, so it needs a new calibration cohort before it can be activated.
- Issue #255's optimizations, the 15-second Task lease, the 8 MiB envelope cap and all
  validation and release authority are unchanged.

## Amendment 2026-10-19: p1 repair

Review of the first p1 tree found that `compare` could pass evidence decision 5 refuses, and that
the Python legs of decision 7 could fail without failing any required check. The repair changes
no threshold, scope, fixture rule or product constant; it enforces the decisions above.

1. **Coverage.** A missing planned record is a coverage failure whatever its case's
   criticality, and so is a missing sentinel record: the aggregate is incomplete (exit 4) with a
   `missing_records` reason naming the case or the sentinel workload. An advisory case's present
   negative evidence (a candidate failure, a regression) still never gates. The verdict counts
   expected, matched and missing records per shard and in total.
2. **Shard terminal status.** A shard whose `shard.json` says `failed` or `interrupted` is a
   coverage failure (`shard_not_complete`) even when every record is present; the verdict keeps
   each shard's status and failure, and its cases' numbers under the new case status
   `shard_not_complete`.
3. **Order.** Records of a shard appear in strictly increasing planned sequence; a record at or
   below an earlier sequence is `reordered_records`, an integrity failure. A partial shard may
   skip slots, which are missing, not reordered.
4. **Builds.** Planning reads `Cargo.lock` at the authority, baseline and candidate commits from
   Git objects and freezes each digest in the plan; a source without one is unavailable
   (`missing_build_input`). Each side's and the supervisor's build records every build-relevant
   recipe setting (OS, target, toolchain, profile, builder, cargo-zigbuild and Zig versions,
   linker, features, rustflags, instrumentation, debug symbols, frame pointers, container image)
   and its Cargo.lock digest. `compare` requires them to equal the frozen recipe and the planned
   digests, instead of trusting a copied recipe digest.
5. **Budgets.** On a complete, healthy case without functional failures, a missed enabled budget
   fails the case (exit 3) even when the relative interval is inconclusive; the interval and its
   classification are kept. Coverage, functional and health outcomes still take precedence.
6. **Versions.** The plan, build and verdict shapes changed, so they are `af.perf-run/2`,
   `af.perf-builds/2` and `af.perf-verdict/2` with schemas `schemas/performance-{run,builds,
   verdict}-v2.json`; their `/1` documents are refused (ADR-0145). The other eleven contracts
   stay `/1`.
7. **Required check.** The required `validation / lint` job depends on the Python 3.9/3.11/3.13
   matrix, runs whatever its result (`always()`), and fails first unless that result is
   `success`, so a failed, cancelled or skipped leg cannot leave a required check green or
   skipped. A test runs that guard's own shell for each dependency result.
