# ADR-0016: Provider preflight is a fenced, charged operation

**Status:** Accepted

## Context

A machine-local Provider label names an authentication context, but does not prove that its
credentials remain valid, the selected model exists, quota is available, or real inference works.
Launching a reviewer directly can therefore spend budget or create an interactive login challenge
before the Campaign has durable knowledge of what happened. A process crash can also leave the
operator unable to distinguish safe continuation from duplicate external work.

Provider setup is not a reviewer Attempt. It establishes whether a Provider can satisfy the
adapter capability that a later Attempt consumes. Provider labels are machine-local and must not
become part of pinned pipeline authority.

## Decision

An explicitly configured Provider is admitted before reviewer dispatch by a durable Provider
Operation bound to the active Round, reviewer node, Provider label, and adapter capability digest.
The operation first runs a bounded structural authentication probe, then a bounded real-inference
smoke. Applicable failed, timed-out, and abandoned work is charged to the Campaign budget.

Every transition is appended as `ProviderOperationTransition@1`. Continuation requires the exact
operation ID and epoch. A stale or superseded token makes no external call. One transient failure
may retry automatically; a repeated normalized failure fingerprint opens the circuit. Interactive
authentication is performed by the operator in a persistent terminal and then resumed explicitly;
the review command never launches duplicate headless login challenges.

Persisted events contain only stable IDs, classifications, normalized fingerprints, timing,
accounting, next action, and an opaque non-secret continuation handle. OAuth codes, access or
refresh tokens, PKCE material, credential bytes, and raw provider output may exist only inside the
provider adapter process boundary and are never emitted to the event log, CAS, Ledger, fixtures,
or diagnostics.

Provider selection uses repeatable CLI bindings (`--provider NODE=PROVIDER_ID`) resolved against
the machine-local registry. It is deliberately absent from the pipeline definition and Campaign
Manifest. Existing ambient adapter authentication remains a compatibility path, not an admitted
Provider.

## Rejected alternatives

**Put Provider IDs in the pipeline.** Rejected because machine-local labels would make portable,
content-pinned authority depend on one operator's workstation.

**Treat preflight as a reviewer Attempt.** Rejected because it has different authority, recovery,
output, and secret boundaries and exists before an Attempt can safely receive a binding.

**Retry by rerunning the command.** Rejected because process death would make duplicate inference
and duplicate interactive challenges indistinguishable from continuation.

**Persist provider output for diagnosis.** Rejected because provider output is not a stable
contract and can contain credential or account material. Typed classification is the durable
boundary; adapter parsing remains private and version-specific.

## Consequences

Provider admission consumes small, visible budget before reviewer execution and requires an
explicit binding to receive these guarantees. Recovery is deterministic and stale work is fenced,
but interactive login remains an operator action outside the review process. A future broker may
replace local CLI execution without changing the Provider Operation event contract.

Admissions execute serially in canonical node order in this slice. This preserves deterministic
reservation and transition ordering; a later bounded-parallel implementation must durably prepare
all reservations first and serialize settlement rather than assigning event identity by completion
timing.
