# Share process supervision through a leaf crate

**Status:** accepted (2026-09-23); acceptance recorded in
[ADR-0113](0113-ga-reads-only-what-ga-writes.md)

Git source capture, model reviewers, command reviewers, gate checks, and container-runtime probes
all need the same bounded subprocess lifecycle: their own process group, an exact deadline,
bounded streaming stdin delivery, concurrent bounded pipe draining, group killing, and explicit
post-exit policy. Local copies
had already diverged on whether held pipes fabricated empty output, whether stdin could block past
the deadline, and whether successful background work survived.

We will keep one policy-parameterized supervisor in the leaf crate `review-process`. Source,
reviewer, check, and sandbox crates depend on that leaf directly. `review-runner` may re-export it
for source compatibility, but no lower infrastructure crate depends on reviewer adapters to obtain
process supervision. Consumer-specific result and charging semantics remain at each caller.

## Considered options

- **Keep one copy per consumer.** Preserves current dependency directions. Rejected because the
  copies had already produced contradictory timeout, stdin, and held-pipe behavior at the same
  security boundary.
- **Keep the shared primitive inside `review-runner`.** Requires the least file movement. Rejected
  because gate and sandbox crates would depend on reviewer adapters, foreclosing a future typed
  runner-to-sandbox dependency and making unrelated reviewer contracts part of their build graph.
- **Put the primitive in `review-core`.** Avoids a new crate. Rejected because subprocess lifecycle
  is executable infrastructure, not a persisted Review Kernel contract.
- **Use a dedicated leaf crate (chosen).** Matches the existing `review-parallel` shape: one small
  dependency-neutral infrastructure boundary with policies supplied explicitly by consumers.

## Consequences

- `review-process` is the only crate that owns process-group creation and killing, exact child
  waits, bounded stdin delivery, and bounded output collection.
- `review-source-git`, `review-runner`, `review-check`, and `review-sandbox` depend on
  `review-process`; the leaf depends only on platform process support.
- Exit policy is explicit. Reviewers preserve the group after leader exit and refuse held pipes;
  successful checks kill background descendants because their work ends with the check leader.
- Spawn failure remains distinct from post-spawn failure so budget accounting can release only
  work that truly never executed.
- A new workspace crate and dependency edge are accepted in exchange for preventing lifecycle and
  security semantics from drifting independently.
