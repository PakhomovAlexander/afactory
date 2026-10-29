# ADR-0240: Execute performance experiments independently on a locked VM

**Status:** proposed (2026-10-27).

## Context

Claw's existing services dispatch and collect Actions work. Changing a runner label would
still depend on Actions jobs, artifacts and run IDs. The operator chose independent VM
compute with Actions retained as a separate backend and one experiment per machine.

## Considered options

- Register a self-hosted Actions runner: rejected; local computation must work independently.
- Duplicate the measurement harness in shell: rejected; containment, oracles and statistics
  must continue to have their existing owners.
- Release exclusion when a controller disappears: rejected; an orphaned container or uncertain
  scratch cleanup can outlive it.

## Decision

Add `cli.py executor` and one closed operator configuration selecting `local` or
`github_actions`, with no fallback. Local core, telemetry and diagnostic work call the existing
planner, pinned builders, supervisor, runners, comparator, renderer and archives. Installed
control is checked against its exact committed producer; candidate sources are only built or
executed inside the existing containers. Retain exact source objects, frozen plans and raw
samples locally. Local identities are opaque request IDs, never numeric Actions IDs.

Reserve an original directory atomically before launch; sync its request, configuration and
state receipts and an index that refuses deleted/unindexed originals. A machine busy refusal precedes the claim and may be retried. A durable claim
is never automatically executed again. Lost claims become uncertain; torn publications refuse
recovery. Bound the journal and storage by refusal, preserving every original. Drain admitted work
before polling; a full busy queue defers new discovery, never execution of admitted work.
Recheck a calibration original's frozen member day after capacity scanning and at the actual
builder launch decision after source preparation. An expired original retires durably as
non-executed, keeps its identity/denominator and appears off schedule in local readiness. The controller
polls newly observed reviewed main tips, comparing each with its first parent. Exact PR merges
and release tags require explicit trusted submissions. This is advisory polling, not a required
CI check or automatic coverage of every intermediate revision.

Extend `perflab.hostlock` with an explicit checked in-process lease and a durable sibling fence.
Hold the same fixed machine lock across source retention, builds, scratch preparation, every
sequential shard, cleanup and terminal evidence. Nested runners borrow the actual lease;
neither environment variables nor descriptor numbers convey it. Controller loss or uncertain
owned cleanup leaves the fence. Unconfirmed diagnostic build-container cleanup is terminal
ownership uncertainty independently of diagnostic scratch; it stops subsequent launches and
retains named recovery evidence for owning and borrowed leases. Never unlink or replace the lock. All compute services must
see its same inode: a required root-owned inode anchor refuses private temporary namespaces,
and compute uses no `PrivateTmp` or private scratch mount namespace. Exclude the inode and
fence from temporary-file age sweeps. Operator recovery must
confirm all owned resources stopped before settling a fence, and never retries an original.
This extends ADR-0169's process-death behavior; its inode and admission requirements stand.

Introduce separately named executor request/state/result, local cohort and local readiness
contracts, with schemas and golden parity examples. The repaired executor configuration is
v2 and freezes an explicit image-local Zig/cargo-zigbuild directory for musl profiles; both
parent and diagnostic builders retain exact pinned version checks. State/result v2 add the
explicit non-executed `expired` outcome. Their obsolete v1 shapes fail closed (ADR-0145);
request, cohort and readiness shapes stay unchanged. Leaf measurement shapes stay unchanged.
Local core environments are separate `dedicated` profiles; telemetry has a separate local
history root and its executor receipt binds backend, environment and native bundle hashes.
Keep hosted declarations, original Actions receipts and hosted readiness readers unchanged.
A local declaration binds the journal, machine, environment and exact producer, spans at least
seven days, and reuses the unchanged 20 A/A, preparation, 30 descriptive and ten whole power
trials, complete critical family, dose rules, thresholds and readiness computation. Commit and
review the declaration before dispatch and the computed parameter seal before trials.

## Consequences

The [operator guide](../performance-local-executor.md) supplies configuration, systemd examples,
manual execution, source polling, report/admission commands and backend switching. A controller
holds its journal lock for one experiment, so another command against that journal can return
busy; pre-submitted requests are durable. No calibration retry, backfill or invented result is
provided. Full serial runs may exhaust the existing total deadline and remain incomplete.
Existing hosted PR/nightly/release workflows remain separate and unchanged; switching the
operator controller does not disable GitHub event triggers.

Focused tests use real processes, locks, Git retention, journal transitions and native
admission with synthetic measurement seams. They establish no Docker/Claw execution, rollout,
calibration qualification, enforcement or speedup. The enclosing native gate and independent
reviews remain required; the coordinator owns deployment and genuine VM proofs.

**Amended 2026-10-29.** The local backend now plans its serial schedule before reservation: a
core plan whose build, sequential sampling windows and finish reserve cannot fit its run
deadline is refused before any original, claim or build, and `preflight` shows the same
checks without reserving. Executor configuration v3 names the diagnostic perf and Inferno
tools; request v2 binds a profile to a verified parent core original whose verdict selects
its cases; `overview` combines retained originals into `af.perf-comparison-overview/1` with
state, verification, coverage and verdict apart. A run that fits may still stop incomplete.
The [operator guide](../performance-local-executor.md) gives the commands.
