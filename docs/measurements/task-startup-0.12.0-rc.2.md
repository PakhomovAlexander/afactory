# Task startup profile, af 0.12.0-rc.2

Measured on 2026-10-13 and first published as
[issue #255](https://github.com/PakhomovAlexander/afactory/issues/255), which this page replaces:
the run's raw artifact has expired and its diagnostic branch is deleted, so the numbers below
exist only here.

Release-target profiling of Task startup on AF `0.12.0-rc.2` found substantial CPU time in repeated
typed-artifact decoding/validation and canonical serialization. In one synthetic 5,605,230-byte
package case, **399 canonical serialization calls accounted for 4.641 CPU seconds out of 11.044 CPU
seconds of startup (~42%)**.

This page records measurements and their limits, not a decision or a proposed change. The
measurements were collected while diagnosing the startup failure encountered during
[#230](https://github.com/PakhomovAlexander/afactory/issues/230); they do **not** establish the
exact cause of that historical failure.

## Evidence and build provenance

- [Release profiling run
  38347191591](https://github.com/PakhomovAlexander/afactory/actions/runs/38347191591), 2026-10-13.
- Raw profiling artifact `release-profile-1857` of that run: **expired**, so this page is the
  remaining copy of the data. It included test logs, Cargo build JSON, verbose build output,
  host/toolchain metadata, instrumentation patch, checksums, and the failed perf attempt.
- AF source base: `6ca82d7d5e3ab94b9f426b2c99b8f44b95f87d42`; diagnostic head:
  `fd6bc75b79b628114ddcb25a2ddc166c381b83c0` (its branch `diagnostic/release-profile-1857` is
  deleted; GitHub still serves the commit by SHA).
- Rust 1.88.0, `x86_64-unknown-linux-musl`, release build using cargo-zigbuild. Cargo artifact JSON
  and verbose rustc output confirm optimization level 3, debug assertions off, overflow checks off,
  and debuginfo 0. No symbol/frame-pointer/optimization overrides were applied.
- The installed AF binary's SHA-256 matched the published rc.2 musl binary. The [official build
  log](https://github.com/PakhomovAlexander/afactory/actions/runs/37968059255/job/113947393325)
  confirms an optimized release build. The measured diagnostic binary is a **separate instrumented
  integration-test binary**, not bit-identical to the shipping CLI.
- Diagnostic binary SHA-256: `9ff0441089eb723376fd60daa4344810cad8869d10b395a70beb94c7a7c9c03c`.
- Instrumentation patch SHA-256: `54ae7359751ed1265b021ce5cf3197a070672d4e712f5431d29f68dcff23cb22`.

## What was measured

The fixture uses the real TaskPlanCompiler, CapturedTaskAuthority, CapturedTaskPlanValidator, and
TaskRuntime::with_store. Its graph contains exactly RootInputs + Check, with one dependency. It
stops at runtime construction: no graph execution, Worker, Provider, model, or check command is
dispatched. The writer lease remains 15,000 ms, and normal validation/fencing checks remain enabled.

A package here is a captured component manifest plus file contents, stored in a typed artifact
envelope. **Package sizes below are serialized envelope bytes, not prompt tokens, raw file bytes, or
repository size.**

- S: one source file; small package.
- F: 1,362 source files; the same small package.
- P: one source file; large package containing 1,362 inert 1-KiB data files plus its manifest.
- FP: the large source set and the large package.
- H: one source file; intermediate package containing 460 synthetic data files plus its manifest.

Source files are 1 KiB each. Every case has a 2,137-byte compiled graph, 1,553-byte revision,
1,581-byte plan, and the same two-node topology. S/F/P/FP each ran in two independent processes; H
ran once. Fresh process/validator does not mean a cold filesystem cache: setup has already touched
CAS, and no global cache eviction was performed.

Startup is measured **from before lease acquisition through completion of the first runtime
constructor**. It includes acquisition and observer overhead; it is not identical to elapsed time
since the lease was published. Separate cold/hot authority-validation measurements precede this
startup interval.

## Startup results

| Case | Package envelope bytes | Source files | Wall seconds, individual runs | Process CPU seconds, individual runs |
|---|---:|---:|---|---|
| S | 3,324 | 1 | 0.023931 / 0.025051 | 0.023262 / 0.024356 |
| F | 3,324 | 1,362 | 0.022626 / 0.023894 | 0.022103 / 0.023316 |
| P | 5,605,230 | 1 | 11.079041 / 11.405332 | 11.043796 / 11.369734 |
| FP | 5,605,230 | 1,362 | 11.368013 / 11.190710 | 11.330629 / 11.155149 |
| H | 1,895,304 | 1 | 4.033713 | 4.019022 |

For P/FP, process CPU was 99.67–99.69% of wall time; H was 99.64%. Thread CPU was essentially equal
to process CPU. Increasing source-file count alone did not increase the measured startup time in
this fixture.

### Phase breakdown

Representative P1 and H1, in milliseconds:

| Phase | P1 wall / CPU | H1 wall / CPU |
|---|---:|---:|
| Cold authority validation, before startup | 358.191 / 357.622 | 130.537 / 130.174 |
| Hot authority validation, before startup | 367.624 / 366.649 | 137.591 / 137.203 |
| Acquire lease | 2148.835 / 2141.878 | 797.975 / 794.793 |
| Confirm plan | 1048.245 / 1045.175 | 396.411 / 394.905 |
| Recover attempts | 1053.877 / 1050.678 | 386.054 / 384.429 |
| Admit plan | 3583.743 / 3571.589 | 1296.291 / 1291.795 |
| First runtime constructor | 3241.495 / 3231.632 | 1154.120 / 1150.237 |

## Serialization and validation CPU measurements

P1 totals across the five disjoint startup phases above:

| Instrumented category | Calls | CPU seconds | Approx. share of startup CPU |
|---|---:|---:|---:|
| Task projection | 8 | 9.200 | 83% |
| Typed-envelope decode + validation | 120 | 7.580 | 69% |
| Canonical serialization | 399 | 4.641 | 42% |
| In-memory SHA-256 | 340 | 0.228 | 2.1% |
| Streaming hash + read | 79 | 0.033 | 0.3% |
| Allocated CAS read | 69 | 0.015 | 0.1% |

**These are inclusive, overlapping categories. They must not be added together.** Typed decode
includes parsing/deserialization, canonical validation, envelope checks, and allocation; its
remainder does not isolate a parser or allocator function.

Cold structural graph compilation consumed approximately **150 microseconds CPU**. The inner
runtime_from_captured span of the first constructor consumed approximately **222 microseconds CPU**.
Structural validation caching already exists; no structural recompilation occurred during the
measured startup phases. Cached authority paths still verified package envelopes.

Admission artifact preparation accounted for 0.370 CPU seconds of authority verification. The two
observed transaction-lock spans totalled approximately 35 microseconds wall time; these do not
constitute a complete SQLite/lock profile.

## I/O and memory counters

For P1 startup:

- Logical CAS work: 69 reads covering 56,127,849 bytes and 79 verification operations covering
  61,722,895 bytes, approximately 117.85 MB in total. This is repeated logical processing, **not
  physical disk traffic**.
- /proc physical read_bytes: **0**; getrusage block reads: **0**; major page faults: **0**. H showed
  the same pattern.
- /proc write_bytes: 77,824 bytes; getrusage block-write units: 152.
- /proc rchar: 179,594,272 bytes; read syscalls: 1,473; write syscalls: 2,825. These include
  diagnostic logging and observer reads.
- Minor page faults: P1 2,566,354; H 886,122. These counters do not establish a particular allocator
  or libc cause.

CPU time accounts for almost all measured wall time on this runner. The approximately 35-ms P1
wall-minus-CPU remainder is not exclusively disk wait. No separate SQLite fsync timing was obtained.

## Test outcomes

All **nine initial startups and first constructors succeeded within 15 seconds**. S/F/H completed
their entire fixtures. Each P/FP case also completed its first two additional warm constructors, but
expired during its **third additional warm constructor**, with observed expiry overruns of 1,151 /
1,391 / 1,674 / 1,240 ms.

The Actions workflow therefore concluded **FAILURE**. That result is retained: it was neither a
compilation failure nor an initial-startup failure. The post-warm assertion was not reached in those
four cases; no-dispatch behavior follows from the constructor-only fixture and refusing execution
domain, not from a nonexistent success marker.

## Measurement limitations

- CPU hotspots were measured with function/phase instrumentation, not sampled stacks. The perf
  attempt failed with kernel permission denial (perf_event_paranoid=4, exit 255); no usable stack
  profile or flamegraph was produced.
- There was no uninstrumented baseline. Observers and logging perturb execution, particularly the
  small cases.
- These are synthetic packages and a simple graph on a hosted runner, not a replay of the original
  VM, its task history, or its full dependency graph. H approximates the byte size of one large
  captured package, not the entire historical task.
- The earlier [test-profile
  run](https://github.com/PakhomovAlexander/afactory/actions/runs/38265084580) measured
  approximately 9.01–9.08 seconds for the large fixture. It is not a controlled debug-versus-release
  comparison: runner/time, libc target, and observers differ, and that test profile already
  optimized review-store, serde_json, and SHA2 through per-package settings. Neither a
  release-optimization regression nor a musl regression is established.
- The historical 15.262-second startup failure has not been attributed to individual functions by
  these measurements.

No product changes, installed-binary replacement, production task restart, or lease-duration changes
were made for this profiling run.
