# ADR-0047 — Preserve Task wire identity and review completeness

**Status:** accepted, 2026-09-11. Refines the unreleased P01 contracts from
[ADR-0046](0046-add-versioned-task-contracts-with-exact-plan-approval.md).

## Context

The P01 review found that valid set permutations became different bytes after typed
serialization, that empty result-output bindings could accompany satisfied acceptance, and
that Task execution/acceptance alone cannot prove whether all required review nodes completed.
It also exposed a default-policy conflict between command-only tutorials and model-principal
independence. None of these new Task contracts has been enabled for execution or released.

## Decision

Canonical Task wire sets are strictly ascending and unique. Deserialization rejects reordered
sets rather than silently normalizing their content identity. Their elements are validated ASCII
names or digests, so Rust ordering and canonical name order agree. Maps remain order-independent
under the existing canonical JSON implementation; ordered artifact vectors retain their order.
Default-valued `facts` and `covers` are explicit wire fields. Optional properties admit omission
or a valid value, never null. A config/root adapter may provide convenience defaults before
capturing a canonical artifact; a loaded artifact cannot be rewritten under normalization rules.

Typed deserialize/serialize/content-ID checks cover every positive Task fixture. Multi-element
cases cover each set family and reject every nontrivial reversed order. The negative corpus uses
explicit JSON Pointer mutations plus a fingerprint of the intended invalid payload. Fingerprints
of invalid examples are test data, not proof of I-JSON or Store admission. Schema and semantic
negative classifications are checked in both directions. Test validators and shared schema
resources are cached independently of the product runtime.

A present Task result output contains at least one artifact, including many-valued outputs;
satisfied acceptance also requires a nonempty output map. Missing output is represented by
absence and unsatisfied/inconclusive acceptance. Empty many-valued *inputs* remain permitted.
Store admission still checks every Task-required output's name, type, cardinality and evidence;
local result validation does not replace that cross-artifact proof.

Review outcome validation receives explicit required-node completeness, derived by the adapter
from recorded review receipts. Missing required nodes permit only the Incomplete conclusion
and exit 4, regardless of exhaustion or other known failures. Complete review output may retain
exit 3 while satisfying a completed Task whose goal was to produce a review. Task acceptance
therefore cannot be used as a proxy for review completeness or change approval. The completeness
argument is not authorization; the Store/runtime must verify its receipt-derived source.

Trusted independence policy explicitly controls `command_workers_by_package`, enabled by
default. Distinct-package command slots may participate without invented Provider identities.
Model/Model pairs still require distinct principals by default. Explicit model/Provider diversity
requirements cannot be satisfied by Command bindings; a project can also disable Command package
independence. Fresh sessions and role-scoped contexts remain mandatory runtime requirements.

## Alternatives rejected

- Sorting after loading a recorded plan would silently change its identity and invalidate approval.
- Making every exhausted result unsatisfied would conflate producing a review with approving code.
- Treating Command workers as satisfying every diversity rule would weaken stronger project policy.
- Ignoring identity drift in empty default fields would leave a second round-trip failure after
  fixing set ordering.

## Consequences

No frozen review artifact or historical event is changed. P02/P03 must consume these canonical
contracts and their strict validation; P04/P06 must derive completeness and authority from exact
receipts. The [P01 review record](../task-execution/p01-review.md) distinguishes corrections in
the branch from the original reviewed Subject's still-open Finding and Demand ledger.
