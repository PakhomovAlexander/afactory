# ADR-0124: Run tests in parallel processes and link them once

**Status:** accepted (2026-09-28). Supersedes in part
[ADR-0107](0107-share-release-validation-and-overlap-builds.md): the clause that kept Cargo's
sequential test runner as the gate and nextest as an opt-in experiment, and the four-thread
bound it fixed for every machine.

## Context

`make check` is the gate for every pull request, every Task check and every release, and its
time is dominated by executing tests, not by compiling them. Measured on 2026-09-28 before this
record, on the same tree:

| Where | test step | test-build | lint | whole gate |
|---|---|---|---|---|
| CI, ubuntu, 4 vCPU | 755 s | 236 s | 48 s | 1123 s |
| this repository's Task check (cold) | 728 s | 50 s | 45 s | 13.8 min |
| developer Mac, 14 cores, warm | 661 s | 11 s | 8 s | ~690 s |

Four things made the test step slow, none of them the tests' own work:

1. `cargo test` runs the 105 integration-test binaries one after another, each bounded by
   `--test-threads=4`. A binary holding two ninety-second tests kept two threads and every other
   core idle for ninety seconds. Five binaries were half of the step.
2. The longest tests were loops over cases inside one `#[test]`, so a scheduler could never run
   the cases side by side: six repair cases in one 73 s test, four heavy-review cases in one
   89 s test, three starter configurations in one 55 s test.
3. Every subject file under `crates/<crate>/tests/` was its own executable that linked the whole
   review stack with debug information: 122 executables, 5.7 GB of `target/debug`, 58 s to link
   on a fast machine and about four minutes on a CI runner.
4. The CI compile cache stored the whole `target/` directory per commit (1.9 GB a save, the
   repository at its 10 GB quota), missed on every `Cargo.toml` change including each release,
   and even a hit saved less time than its restore and save cost, because the workspace crates
   are rebuilt from a fresh checkout regardless.

ADR-0107 kept nextest out of the gate because "native-provider probe timeouts under
cross-binary scheduling" flaked: a handful of tests assert against a fixed real-time budget (the
5 s provider probe, a 3 s observer window, a 5 s worker deadline, one real elapsed-time
comparison) and a loaded machine breaks them. Throttling the whole suite to four sequential
threads protected five tests at the cost of every other one.

## Considered options

- **Raise the fixed budgets those tests assert.** Rejected: the budgets are product behaviour
  (how long a hung provider takes to report); a test that needs 20 s to pass under load is
  hiding a scheduling problem behind a slower product.
- **Make the budgets injectable from tests.** Rejected as ADR-0114 rejected a test clock in the
  CLI: a new production surface no requirement asked for, in exactly the code whose fixed value
  the tests exist to pin.
- **Keep Cargo and merely raise `--test-threads`.** Rejected: threads only help inside one
  binary; a two-test binary still idles the machine, and the loops stay serial.
- **Cache compiled artifacts by hashing `target/` harder.** Rejected: the artifacts rebuilt on
  every commit are the workspace's own crates and the linked test executables; no directory cache
  reuses them across commits. A cache keyed by the compiler's inputs does.
- **One test per process, scheduled across binaries, with the timing-sensitive tests running
  alone; one integration-test binary per crate; per-case tests instead of loops; a compile cache
  keyed by rustc inputs (chosen).**

## Decision

- **nextest is the gate.** `make test` runs `cargo nextest run --profile ci` over every test
  binary at once, one process per test. `TEST_THREADS` (default four on a four-core runner, half
  the cores elsewhere; the Makefile computes it) bounds concurrent tests, not compiler jobs,
  because these tests spawn real process trees. `TEST_RUNNER=cargo` keeps the sequential path
  for comparison and is not a gate. CI installs the pinned, checksum-verified runner through
  `scripts/install-nextest.sh`. Doctests are off (`doctest = false` in every library crate):
  there are none, and the fifteen empty rustdoc runs cost 22 s a gate; the first doctest turns
  the flag back on for its crate.
- **Timing-sensitive tests run alone, listed in one place.** `.config/nextest.toml` names the
  tests whose assertions depend on wall-clock; each holds every test thread while it runs
  (`threads-required = "num-test-threads"`) and runs first. Adding a test there is the only
  sanctioned way to protect a real-time assertion; widening its budget is not.
- **A case is a test.** A test that iterates over fixture cases becomes one helper and one
  `#[test]` per case, so the scheduler sees the cases. The helper keeps the body byte for byte.
- **One integration-test binary per crate.** `crates/<crate>/tests/it/main.rs` declares one
  module per subject file; subject files and their support modules live under `tests/it/`. The
  crate links its test dependencies once. Crates whose ignored probes are selected by binary name
  (`review-sandbox`) and the small leaf crates keep separate files. Test names gain their module
  prefix: nextest filters in this repository match by suffix, and a test that re-runs its own
  executable selects itself through `module_path!()` and requires the child to report a passed
  test, because a child that selected nothing also exits 0.
- **The CI compile cache is per rustc invocation.** `sccache` with the GitHub Actions backend
  caches every compiled crate by its inputs, so an unchanged crate is a hit on any later commit;
  the cache action keeps only the crate registry, keyed by `Cargo.lock`. Linux CI links with
  `lld`; the flag is scoped to the `x86_64-unknown-linux-gnu` target and never reaches a release
  build. Debug information is `line-tables-only` for the dev and test profiles in `Cargo.toml`
  rather than through CI environment, so the Task check and a developer build link the same
  amount as CI. Formatting and clippy run in their own CI job beside the test job.

## Consequences

Measured on the developer Mac above, same tree family, one change at a time:

| Measurement | before | after |
|---|---|---|
| test step, warm, 8 threads available | 661 s (`cargo test`, 4 threads) | 277 s (nextest, same 1457 tests); 289 s on the final tree (1482 tests) |
| longest test that shares the machine | 92.9 s | 43.8 s |
| cold `cargo test --no-run` | 58 s, 122 executables, 5.7 GB | 46 s, 48 executables, 3.5 GB |
| doctest step | 22 s, 15 empty groups | none |
| whole `make check`, warm, default threads | ~690 s | 323 s |
| flakes in four full runs | – | 0 of 5,903 test executions |

On the four-core CI runner, first run of the new gate with a mostly cold compile cache
(57% of rustc invocations hit, from the parallel jobs' own compiles):

| CI measurement | before | after |
|---|---|---|
| check job wall | 1123 s | 745 s |
| test step | 755 s | 528 s (4 threads) |
| test-build | 236 s | 194 s |
| fmt and clippy | 52 s, in series before the tests | 38 s, in a parallel job |
| container probes | 92 to 165 s | 49 s |

The warm-cache run and later ones are recorded on the pull request that introduced this
record; the four-core runner gains most from the linked-once binaries and the compile cache,
the fourteen-core machine from the scheduler.

- A flake under the new scheduling is a scheduling defect to fix in the test or the kernel, or
  a test to list in `.config/nextest.toml`; it is never a reason to lower `TEST_THREADS` for
  everyone.
- `make review-kernel-container-probes` selects the review-pipeline probe through the crate's
  single binary and its module path.
- The first doctest in a crate sets `doctest = true` for that crate and pays for it.
- A CI runner with more cores raises `TEST_THREADS` on its own; the timing-sensitive list does
  not change with it.
