# ADR-0214: Bound guarded experiment consumers to build and instrument inputs

**Status:** proposed (2026-10-25).

## Context

The macOS release check for `0.13.0-rc.2` failed the guarded kernel experiment's `improve`
Task: its evaluator reached process dispatch with 742 ms left of its five-second Attempt
wall. The isolated retry then exceeded the gate's existing fifteen-minute retry window.
Suite exclusivity does not remove filesystem or process startup latency on the runner.

The fixture copied the whole repository into each consumer. A source Worker validates and
materializes that tree inside its Attempt; a judging Worker also validates and materializes
its base as `../base` (ADR-0212). Thus unrelated ADRs, release assets, schemas and other CLI
fixtures became filesystem preparation for a Python evaluator that reads the declared check
and comparison receipts. Temporary local instrumentation measured 1.911 seconds in evaluator
environment preparation, including 0.906 seconds between base validation and source readiness;
its process received 2,955 ms. Those measurements locate setup work, not a product speedup or
an exact reproduction of hosted load.

## Decision

The disposable guarded consumer copies every file under `crates/`, `scripts/performance/` and
`fixtures/performance/`, plus every path named by the checked-in protected inventory. Keeping
all crate files preserves the real native probe and its workspace dependencies. Inventory
membership retains protected Cargo, toolchain and configuration inputs anywhere, without
assuming today's directory names. The fixture still substitutes its own experiment authority
and installs the staged policy verbatim.

The original literal bootstrap and staged digest check validate the resulting consumer; a
stale or incomplete inventory still fails. No guard or inventory entry is removed. The source
and base remain complete materializations of their consumer Snapshots, with the same validation,
read-only permissions and sealing checks. No production code, deadline, lease, retry policy,
comparison threshold, repetition count, golden contract or protected instrument changes.

A focused regression compares every protected file and every crate file with the workspace,
checks the exact staged policy and five-second read-source evaluator, and proves that unrelated
repository growth adds no consumer inputs. The existing integration retains all missing-input,
controlled-improvement, tampered-harness, malformed-report and no-execution-marker assertions.
Repeat it through the existing repeat facility, which stops at its first failure; the gate's
isolated retry remains a reporting policy, not proof that this repair passes repeatedly.

## Consequences

Repository documentation and unrelated test corpora no longer add work to short Worker
Attempts. A future probe build that needs an unprotected input outside these trees must add
that input explicitly; its offline build must fail if an input is missing. Hosted reliability
still requires the enclosing native gate. This change declares no cohort, deployment or
performance gain.
