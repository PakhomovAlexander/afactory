# ADR-0063: Require goal acceptance alongside embedded Review

Status: accepted for the Task increment implementation; unreleased.

## Context

The accepted product design requires implementation to satisfy the Task requirements and pass
its configured Review or repair guarantee. The first composition in ADR-0052 replaced the
independent evaluator when `verification = "review"` was selected. A clean Review could therefore
mask an unmet ticket criterion. Review Workers also lacked the captured requirements input.

## Decision

Every newly submitted reviewed implementation has two named acceptance obligations. `verified`
requires the configured `af/ReviewedImplementation@1` or `af/RepairAllowedImplementation@1`
receipt. `goal` requires `af/VerificationResult@1` under the captured code policy, exposed through
the public `evaluation` output. Both must pass for delivery. The default implementation profile
continues to require independent evaluation; standalone Review retains its existing verdicts.

After any repair, select the final Snapshot and its exact current checks. Run one protected
independent evaluator with that Snapshot, the exact Task Requirements and those check receipts.
Skip evaluation when the current checks did not pass. `accept` assembles goal evidence while the
original Review or repair operator supplies the public Snapshot. Neither step rewrites earlier
receipts, reruns the checks or creates another allowance.

The shared Review contract declares an optional Requirements input so it works standalone with
or without supplied requirements. When a Task supplies requirements, every Review verifier must
receive that exact root input and retain its identity in its result. Evaluators always require
that input. The compiler proves wiring and retention before Provider admission; runtime
acceptance rejects missing, replaced or stale requirement provenance. Optional retained inputs
may be absent only when the trusted Worker contract declares them optional.

The software starters reserve one additional evaluator Attempt: reviewed implementation has
five total/four protected Attempts, targeted repair eight/six, and heavy repair eleven/nine.
Planning keeps its separate two-Attempt ceiling inside the same Task allowance. Its sample Task
allows seven total Attempts, including all business verification. Existing negative fixtures
retain their failure conditions and account for the newly mandatory verifier.

Already compiled planning graphs use a separate remaining-capacity check that retains their
Provider admission nodes. Structural candidates still receive those nodes during compilation.
This prevents duplicate admission insertion for native Planners and protects remaining wall time
when a refreshed source needs a new preparation plan.

## Validation and compatibility

Regressions cover a passing Review with a failed or missing evaluator, refusal of delivery,
missing requirements retention before dispatch, stale S1 evaluation after S2 repair, exact
requirements in minimal Review context, and forged positive evaluator envelopes. The same
starter, generation, refresh, repair and replay fixtures exercise the stronger contract.

This supersedes ADR-0052's choice between Review and evaluator acceptance for new implementation
Tasks. Persisted legacy contracts and frozen v0.8.0 fixtures remain unchanged. The full
compatibility gate and the requested external PR review remain required before release.
