# Demands are independent blocking obligations

**Status:** accepted (2026-08-20)

A performance claim may live outside any Finding, so a Benchmark Demand cannot borrow Finding
identity or resolution state. Selected Demands are immutable, snapshot-scoped artifacts with a
separate projected lifecycle. Pipeline policy classifies them as required or advisory; every
required Demand remains blocking until trusted policy admits linked, snapshot-current Evidence
or an authenticated operator explicitly waives it with a reason.

## Consequences

- `wontfix`, `rejected`, and `fixed` Finding resolutions never satisfy or waive a Demand
  implicitly.
- Evidence names the exact Demand and Subject snapshot it supports. A new head Snapshot makes the
  satisfaction stale unless trusted policy explicitly permits reuse.
- Reviewers request measurements but do not choose executable recipes, grant themselves
  authority, or declare their own Demand satisfied.
- Campaign reports show open, satisfied, stale, and waived Demands independently of Findings.
