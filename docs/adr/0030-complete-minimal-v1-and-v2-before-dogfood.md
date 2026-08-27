# Complete minimal v1 and v2 before candidate dogfood

**Status:** accepted (2026-08-26)

The proposed A0 checkpoint put a candidate `af review` compatibility layer in front of the proven
v0.2.0 Review Kernel before the target Task/Pipeline/Worker runtime existed. Its first implementation
needed substantial temporary adapters and would make the repository pay implementation, review,
and context costs twice before reaching the `implement` capability needed for real Afactory
development.

Complete the smallest useful v1 and v2 before candidate dogfood. **v1** provides the final local
`af review` surface on one machine. **v2** adds one sequential `implement` Task: one implementer
Worker produces a candidate, read-only acceptance Gates check it, and a separate evaluator Worker
decides the result. v2 ends at a sealed, verified internal derived Snapshot with a typed
verified/unverified outcome and token accounting. It performs no automatic working-tree, branch,
or PR delivery.

The first candidate dogfood after v2 uses `af` to implement a real change in this repository.
Existing frozen `.review/` and `review.kernel/*` contracts may remain internal through v2 rather
than being copied into a temporary compatibility architecture. The final user-facing surface is
`af` and `.af/`; physical internal renaming is not required to prove v1 or v2 behavior.

## Considered options

- **Dogfood the compatibility-backed A0 skeleton first.** Rejected because it creates a temporary
  path that must immediately be removed and increases the distance to useful implementation.
- **Complete every planned v1 and v2 subsystem before dogfood.** Rejected because integrations,
  scale, and delivery do not contribute to the first useful local loop.
- **Complete minimal v1, then minimal v2, then dogfood (chosen).** Builds required behavior once
  on the intended boundary and minimizes code, Worker context, and review spend.

## Consequences

- ADR-0029's A0 checkpoint and candidate ratchet are superseded. A0 compatibility work is not
  part of the release path.
- v1 is local review only. v2 is sequential implementation ending at a verified internal
  Snapshot; acceptance Gates are read-only and the evaluator is independent of the implementer.
- v3 owns delivery/write-back, dynamic fan-out and parallel graphs, shared/distributed storage,
  TUI, MCP/hooks/connections, direct API Providers, richer Envs, hosted integrations, and any
  physical crate or persisted-contract rename approved by a later migration ADR.
- Candidate feedback starts later. Deterministic `make check` remains mandatory throughout, and
  released `v0.2.0` remains available as the last-green reviewer before candidate dogfood.
