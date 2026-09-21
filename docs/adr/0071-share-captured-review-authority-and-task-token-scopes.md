# ADR-0071 — Share captured Review authority and Task token scopes

**Status:** accepted, 2026-09-12; unreleased execution compatibility checkpoint. Superseded in part
by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): readability of historical `.review/` captures.

## Context

The legacy Review CLI already validates captured packages and their reachability from the
Authority Snapshot. A separate Task loader would duplicate that trust boundary. Moving Review
to common Attempts must also retain aggregate reviewer and Scatter caps: a per-Attempt
reservation alone would let retries or parallel children exceed those captured limits.

## Decision

The CLI and Task compatibility compiler use `review_config::captured_review` to load the exact
Campaign Manifest's pipeline, lock and packages. It verifies package bytes, bindings, execution
policy closure, genesis identities, Subject kind, budgets, check timeout, selected light/heavy
convergence and source reachability. Historical `.review/` captures remain readable; the CLI
still refuses that layout for new Campaigns. No live project configuration supplies resume
authority. The extracted loader preserves existing validation and error semantics.

`CompiledTask` may carry named aggregate token scopes. Each scope has an exact token ceiling
and a set of qualified node addresses or child prefixes. Prefix matching respects address
segments. Scope identity is separate from membership: Review must derive a scope identity from
the Campaign and numeric Round, retaining it across that Round's input epochs. Later Rounds
may reuse node addresses with different scope identities, while the lifetime Task Run limit
continues to include all spend. The installed captured compiler supplies these scopes.

The common Task budget reserves against the Run and every matching scope in one token ledger.
Overlapping members do not charge a scope twice. Still-required verification is protected inside
each scope, and compilation checks minimum mandatory work. Releasing an unstarted reservation
returns all its scoped credit; failures, in-flight observations and settlement share the same
charge. Late observations increase every original scope, including inactive scopes retained
after graph replacement. They never refund spend or charge the new Round's scope instead.

Graph replacement validates new membership before committing changes. Reusing a captured scope
identity with different members or a different ceiling is refused. Old accounts and reservation
identities survive invalidation. An omitted empty scope map preserves earlier compiled wire
bytes. A scope ceiling may exceed the Task's remaining capacity; the Run and scope limits are
enforced together, without confusing an original ceiling with remaining credit.

## Verification and remaining integration

Captured-loader tests exercise both historical layouts, light/heavy mode, valid but unreachable
replacement bytes, changed check policy and forged genesis. Existing Campaign lifecycle tests
exercise the CLI after extraction. Budget tests cover aggregate retries, parallel reservations,
segment matching, overlapping membership, verifier protection, graph replacement and late
overrun. A real Store reopen retains scoped usage and refuses another retry without appending
an event. Compilation refuses a scope that cannot fit mandatory work.

This supplies the shared accounting mechanism. The Review execution-policy capture, executable
plan compiler, domain host, retry eligibility, native/broker wiring, owned Scatter children and
same-Task Round-advancement barrier remain required before the legacy CLI cutover is complete.
Scatter's inert parent must not receive a second paid reservation alongside its actual children.
