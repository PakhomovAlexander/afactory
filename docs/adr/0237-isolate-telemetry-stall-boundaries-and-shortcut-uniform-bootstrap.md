# ADR-0237: Isolate telemetry stall boundaries and shortcut uniform bootstrap

**Status:** proposed (2026-10-27).

## Context

Issue 347's public-run ownership-fence fixture measures controller preparation inside its
one-second assertion. Final work settlement gives fencing and filesystem deletion part of
its 80 ms allowance, so slow deletion can prevent either selected ownership operation from
starting. A failed read subcase also leaves state for the following write subcase.

The collection rotation fixture repeatedly generates, admits and independently verifies
bootstrap documents. The hosted test baseline is 34.529 seconds; the Python suite baseline
is 1080.465 seconds. Neither is a local profiled measurement.

## Decision

Extend ADR-0232's test-owned boundary clock pattern to these two cleanup fixtures. Defer the
selected cleanup timer through real preparation and deletion, then invoke the saved production
`Budget.enforce` at the blocked ownership operation. Keep the 80 ms work window, actual
30-second blocking operation, real SIGALRM interruption and one-second measured assertion.
Assert entry, exactly one interruption and no retry, and preserve durable pending ownership,
launch accounting, publication refusal and the actual retained mount recovery path. Give
read and write independent test fixtures. Entry and active-context guards require production
`_sync` to enter enforcement before either injected operation can arm its timer. Three mutation
controls bypass only that production context and require each fixture to fail at the guard,
before the injected timer or blocking I/O starts. Delayed preparation/deletion tests exceed the former
one-second setup bound but still reach and interrupt each selected operation.

For nonempty, identically represented equal bootstrap observations, every possible resample
has the same median. Compute that median once with the original even-count arithmetic and
rounding. Mixed representations, including opposite signed zeros, retain the original seeded
10,000-resample implementation. The RNG is local to each interval, so skipping uniform draws
cannot affect another interval. Keep the statistical policy, document shapes, sample counts,
verifier, production budgets, harness controls and filesystem accounting unchanged.

## Bounded diagnostic evidence

The operator's supplied profile under load 230 took 372.527 seconds. It recorded 1775
`used_bytes` calls (211.567 seconds cumulative), 843 `publish_record` calls (203.313), 823
`write_position` calls (199.293), 2653 `allocated` calls (131.171), and 560
`bootstrap_interval` calls (67.241). These overlapping cumulative times cannot be added or
compared as elapsed savings against the unprofiled hosted baseline. No full profile is repeated.
A local bounded profile of one original uniform interval confirms 10,000 median computations.

Separate local Python 3.14 timings compare the saved original function and candidate in one
process, using identical inputs, labels and seeds, three paired batches and alternating
before/after order. All serialized outputs agree. Median batch seconds are:

| Input | Calls per batch | Original | Candidate |
| --- | ---: | ---: | ---: |
| Uniform singleton `1.0` | 40 | 0.948247 | 0.000035 |
| Uniform 20 observations `-1.23456` | 8 | 1.751400 | 0.000061 |
| Nonuniform 20 observations `-10.0` through `9.0` | 8 | 1.397952 | 1.653616 |

These bounded function timings show the uniform shortcut removes avoidable computation.
They establish no whole-test, whole-suite or hosted speedup, nor a general-input improvement;
local scheduling/load remains uncontrolled. Filesystem traversal remains the larger diagnostic
bottleneck. An exploratory rotation run on legacy Python 3.9, while source authority was being
edited, failed history admission in 235.571 seconds and is excluded from timing comparisons.

## Considered options

- Increase deadlines or retry stalls: rejected because that loses the selected-boundary proof.
- Cache filesystem allocation or mutable retained authority: rejected because admission and
  budgets must observe the actual current files.
- Lower resamples or trust generated summaries without verification: rejected because that
  changes the statistical protocol or removes independent admission.
- Optimize general resampling: deferred; the smaller uniform identity has direct exact proof.

## Consequences

Regression tests compare serialized intervals to an independent literal 10,000-resample
implementation over positive, negative, zero, signed-zero, mixed and nonuniform observations,
odd/even counts, multiple seeds and labels. General-input tests count every original RNG draw.
Existing document admission and protected-harness guards remain required. Regenerate staged
harness hashes with the generator, leaving `.af/` unchanged. Task-specific requirements limit
this Worker to focused checks; complete Rust/Python suites and hosted validation remain separate
kernel/operator obligations.
