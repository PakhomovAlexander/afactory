# ADR-0163: Measure native workloads through a public-API probe and independent golden oracles

**Status:** accepted (2026-10-19); decisions 1, 3 and 6 tightened by
[ADR-0164](0164-judge-only-declared-probe-requests-against-a-reference-checked-golden.md);
scratch cleanup in decisions 1 and 6 superseded by
[ADR-0165](0165-own-probe-scratch-in-the-driver-and-take-lease-expiry-only-from-the-store.md);
the driver's execution in decision 6 superseded by
[ADR-0166](0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md).
Package p2 of the accepted
[continuous performance laboratory](../design/continuous-performance.md) design; builds on the
contracts of [ADR-0162](0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md).
Build and overlay orchestration (p3a), paired execution, CLI fixtures and health (p3b) are later
packages and are not decided here.

## Context

The registry of ADR-0162 declares the library cases of the design (canonical data, CAS, source
capture, state projection, the authority constructors of issue #255 and an advisory persistence
diagnostic) and a baseline-versus-baseline sentinel, but nothing executed them and every oracle
was pending. A library sample must time a real production operation of the side it measures,
not a proxy, and its result must be judged by something that side cannot influence: a candidate
that computes its own expected answer would pass any regression in correctness. The probe also
has to compile, unchanged, in measured worktrees of other revisions (p3a overlays it), so it may
use only public APIs and the crate dependencies `review-pipeline` already has.

The fixtures are large on purpose. Issue #255's packages are an approximately 1.9 MB and an
exactly 5,605,230-byte typed envelope, below the production 8,388,608-byte cap; the state case
needs exactly 10,000 valid relevant Task events. They must come from the product's own writers
under the production 15-second lease, not from SQL, relaxed validation or a raised cap, and they
must not be committed as megabytes of opaque bytes.

## Considered options

1. **Commit the fixture bytes and their digests.** Rejected: tens of megabytes of opaque data,
   and a digest of committed bytes says nothing about whether the product generates them.
2. **Record golden digests from a first trusted probe run.** Rejected: the expected answer would
   be learned from an execution of the same product code it later judges.
3. **A general RFC 8785 oracle in Python.** Rejected: Python's `json` is not one, and a second
   general canonicalizer is a second thing to get wrong. The fixtures are restricted instead to
   printable-ASCII keys and strings and admitted integers, whose canonical form has exactly one
   independent spelling, and the reference encoder refuses everything else.
4. **Let the probe report pass or fail.** Rejected as the oracle: the probe runs on the measured
   side. It still checks what it can (every sentinel repetition, every captured object), but only
   the trusted driver's comparison with golden entries passes a sample.
5. **One process per case batch, or retry a lapsed lease.** Rejected: unreported batching hides
   per-operation cost, and a retry hides a functional failure the design attributes to its side.

## Decision

1. **Probe.** `crates/review-pipeline/examples/perf-probe.rs` with its helper modules under
   `examples/perf-probe/` is the native probe; the modules are its complete closure and use only
   existing public APIs. One process reads one `af.perf-probe-request/1` (case, contract,
   parameters, repetitions, an empty canonical absolute scratch directory; at most 64 KiB) and
   writes one `af.perf-probe/1` line. It refuses, with exit 2 and no message, an undeclared case
   or contract, an unknown or duplicate field or key, a parameter outside its declared domain or
   of the wrong type, a wrong instance ID or repetition count, and a scratch that is relative,
   missing, non-empty or reached through a symlink. It writes only below
   `<scratch>/probe-work/` and removes only that.
2. **Workloads.** Each case performs its production operation once per declared repetition:
   `canonicalize(to_value(envelope))`; `from_slice` and `validate_envelope`; `Cas::get` of the
   envelope's exact bytes filed raw; `Cas::get_artifact`; `Capture::committed` of a committed
   repository of 100 or 10,000 deterministic 128-byte files; `EventStore::task_projection` of
   exactly 10,000 events (open, propose, admit, then production lease renewals) on a newly opened
   Store, application-cold or after one prewarming projection;
   `check_current_task_plan_for_recording` and the first `TaskRuntime::new` of a compiled
   two-node graph whose author package envelope has each issue #255 size; and, advisory, a CAS put, flush and get of 1 MiB and 100
   renewals read back, with their flush semantics and logical bytes. The sentinel canonicalizes
   its fixed value 64 times and checks every repetition. Fixture setup, CAS population,
   prewarming, lease renewal and every check stay outside the region; the region result is kept
   alive with `black_box`. Wall time is a monotonic nanosecond count written as an exact decimal
   of milliseconds; `setup_ms` reports setup beside it.
3. **Leases and failures.** Authority samples renew the production lease and open a fresh Store
   connection immediately before the region. A lapsed lease is the side's `lease_expired`
   failure, never retried; an operation or fixture error is `operation_failed` or
   `fixture_failed`. A failed message carries count 0 (it checked nothing), exits 1, and the
   driver maps it to the sample contract's failure kinds.
4. **Fixtures.** Typed envelopes are generated by the product's writers: the small one an
   `af/Requirements@1` envelope through `Cas::put_artifact`, the issue #255 sizes the
   `perf/author` Worker package captured by `TaskPlanCompiler::capture_package`, padded with
   deterministic digit bytes to the exact declared full envelope size, so the canonical, CAS and
   authority cases share one package identity. A Rust test also proves the real cap: an
   8,388,608-byte envelope is admitted and one byte more is refused.
5. **Independent golden entries.** `scripts/performance/perflab/reference.py` constructs from the
   same fixture inputs, with its own restricted encoder, the reference envelope bytes, package
   digests, source manifest digests and the semantic projections the cases report, and derives
   every golden digest and count from them or from explicit semantic facts (final Task phase,
   event count, graph order and slots). `fixtures/performance/golden/native.json`
   (`af.perf-golden/1`) holds them, with lease, clock and revision metadata outside every stable
   projection; `cli.py golden` regenerates it byte for byte and a test requires that.
6. **Trusted driver.** `perflab/oracle.py` builds each request from the authority registry, runs
   the probe with a private scratch it removes afterwards (`cli.py probe`) or judges a recorded
   execution (`cli.py judge`), and passes a sample only when identity, fixture digest, digest and
   count equal the golden entry and the metrics are declared; anything else fails that side.
7. **Registry.** The probe declaration, each library case's probe contract and the sentinel
   contract changed the registry shape, so it is `af.perf-suite/2` and `/1` is refused
   (ADR-0145). Library cases and the sentinel have available oracles and the `setup_ms` optional
   metric; CLI oracles, every recipe, environment and case execution stay not executable until
   p3a and p3b. The state projection case's sample timeout is 120 s, because its fixture writes
   10,000 FULL-synchronous transactions before the region.

## Consequences

- Library cases can be sampled on a developer host with `cli.py probe`; such a sample is a
  diagnostic, not a Performance Sample, and local Darwin timings establish no Linux budget.
- The probe and its golden entries are instrument paths: a candidate that changes them is
  `harness_changed`, and an intentional fixture change regenerates the golden file in review.
- Product constants are unchanged: the 15-second lease, the envelope cap, validation and CAS
  limits. Issue #255 is measured, not optimized.
