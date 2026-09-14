# ADR-0070 — Separate Review domain operations and fence Task dispatch by Round

**Status:** accepted, 2026-09-12; unreleased compatibility execution checkpoint.

## Context

A captured Review graph must use the common Task owner while preserving canonical Review
operations. Constructing a disabled legacy Kernel would still replay and fence legacy Attempts
and create its own Attempt ledger. The original Round can also be superseded or closed while
a Task is running; a Task-only sequence comparison cannot detect that separate log change.
Exact reviewer context requires a durable canonical invocation identity before rendering.

## Decision

`ReviewDomainState` owns the original Generation, Gate, Slicer, Gather and Ledger operations,
canonical receipt publication, shared projection/cache state and selected evidence facts.
Its constructor validates immutable Round/Subject identity and installs no Attempt ledger,
retry loop, budget, Worker transport or Scatter scheduler. The legacy Kernel composes this
state and retains its existing execution and replay owner. Generation has one raw algorithm
shared with the typed Task codec adapter. Original node/port names, producer identities,
artifact bytes and operation ordering are preserved.

A Task with one `af/LegacyReviewRound@1` root input must retain the exact wrapper references,
Subject/head identity, Campaign Manifest, Round event and epoch. Common dispatch rechecks that
Round. Under the same SQLite writer lock as each new Task invocation, preparation, start or
output publication, the private write permit checks that the Round is still current and has
no terminal report. A concurrent Campaign change therefore invalidates the write even if the
Task sequence did not change. An Incomplete report leaves the Round open.

Supersession stops further effects; it does not erase paid work. Settlement, late cumulative
usage, credit release, lease recovery and diagnostics remain recordable. Common selection and
canonical publication retain their existing separate guards. A serialized Round binding alone
cannot replace trusted plan recompilation, package admission, approval, leases or budgets.

The common runtime adds an idempotent domain-invocation publication hook after its own durable
invocation and before Attempt reservation/context capture. It releases the Store lock before
the hook and rechecks authority afterward. Lost acknowledgement or panic produces a durable
domain-publication diagnostic and a recoverable waiting Task. Reopen retries publication with
the same invocation identity; it starts no Worker until publication succeeds. Context capture
remains pure. The existing output-publication hook retains the same recovery behavior.

## Verification and remaining integration

Store regressions cover forged bindings, superseded and closed Rounds, nonterminal Incomplete
reports, a concurrent Campaign change inside the write comparison, release before dispatch,
and late usage/lower settlement retained through reopen. Runtime tests cover lost invocation
acknowledgement before any Attempt, successful recovery with one paid invocation, and existing
post-output acknowledgement recovery. Legacy Gate, Proposal, Scatter, canonical reduction and
Task compatibility tests exercise the moved operations without changing their algorithms.

The legacy CLI still needs trusted captured-plan admission and the operation host connection.
Common replay must hydrate actual Task selections. Gate evidence must survive a crash between
common output and canonical publication without rerunning checks; Slicer/Ledger writes also
need durable operation reuse. Broker callbacks, common owned Scatter and one original Task
allowance across heavy Round continuation remain required. Moving these operations alone does
not complete those connections or establish live performance.
