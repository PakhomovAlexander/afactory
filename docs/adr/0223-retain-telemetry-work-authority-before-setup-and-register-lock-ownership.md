# ADR-0223: Retain telemetry work authority before setup and register lock ownership

**Status:** proposed (2026-10-26). Extends ADR-0222's lifecycle decisions.

## Context

A stopped container is insufficient cleanup authority while controller filesystem work is
pending. A failed uncertainty transition can leave the previous stopped record intact.
Retrying ownership persistence after a deadline signal has unwound its timer is unbounded.
Machine lock acquisition can also return through an exit-time deadline check before its
caller owns the descriptor. Diagnostic decoders and renderers have writable staging without
a Namespace, so namespace-only uncertainty cannot protect them.

## Options considered

- Fence only immediately before deletion: rejected because fence publication itself can fail.
- Retry ownership writes after interruption: rejected because it extends the absolute deadline.
- Defer alarms around acquisition or validation: rejected because these operations can block.
- Retain only reader namespaces: rejected because ordinary tools also own writable staging.

## Decision

The owner becomes `af.perf-telemetry-owner/2`, adding required `filesystem_pending` authority.
The controller durably sets it before executor setup and clears it only after confirmed final
work-root removal. Container phases and exact launch accounting retain their meanings. A
stopped container with pending filesystem work cannot authorize workflow cleanup. The live
controller may continue settled executions while holding its owner lock; reopening pending
work refuses admission. Readers refuse version 1; no migration or compatibility reader exists.
Every original golden case remains indexed, updated with the current shape, beside new fence
and old-version refusal cases.

Ownership admission, transitions, deletion and settlement use the original absolute deadline.
Failure handlers latch only memory and preserve the earlier durable fence. Interrupted writes
leave their private temporary file for recovery instead of attempting compensating unlink or
buffered flush. Rollback remains within the original terminal allowance and cannot replace the
original exception. The profile timeout, 90-second reserve and publication reserve are unchanged.

MachineLock registers its object in the caller's cleanup scope before opening a descriptor.
Only descriptor transfer and close defer alarms; flock, validation and waits remain interruptible.
Constructor failures and caller return-boundary expiry close the independent lock description.

Profile's shared tool boundary retains cumulative uncertainty for results and exceptions after
possible launch. This covers decoders, renderers, ordinary tools and telemetry readers. It
retains writable paths, blocks later launches and admission, and preserves both work roots.
The administrative refusal remains readable; retained scratch prevents bundle-manifest creation,
which would otherwise read possibly live local tool output. Safe pre-launch refusals and normal
successful captures and rendering retain their behavior.

## Consequences

Persisted-owner tests use the real workflow cleanup admission and short actual timers across
reads, writes, replacement, file/directory sync, deletion and settlement. Public-run tests cover
stalled fences and exceptions after confirmed container removal. Real independent flock tests
cover constructor failures, deadline return handoff and successful subsequent runs. Diagnostic
caller tests cover perf decoding and Renderer.run in local and stand-in container modes, including
post-launch exceptions without a Namespace. Existing lifecycle and reader assertions remain.

These are unit reproductions, not live Docker evidence. The unchanged full native gates,
independent Terra/Astra reviews and goal evaluation, committed whole-source verification and
fresh hosted CI remain separate obligations. No rollout, cohort or deployment is authorized.
