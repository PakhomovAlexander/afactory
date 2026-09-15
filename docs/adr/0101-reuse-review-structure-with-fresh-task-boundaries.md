# ADR-0101: Reuse Review structure with fresh Task boundaries

Date: 2026-09-14
Status: Proposed

## Context

Captured Review recompiles identical plans at every execution boundary. Task projection also
replays the same Campaign for each handoff and integration phase, and some Store methods repeat
an already completed post-callback integrity check. Native usage classification unnecessarily
parses complete raw transcripts. Repeated authenticated approval revocation appends another event.

## Decision

The immutable captured Review compiler retains at most one exact Task/Plan pair and the complete
set of authority artifacts read by compilation. A hit rehashes every artifact; a changed Task or
Plan recompiles. The authority Snapshot manifest remains a metadata dependency, without reading
unrelated source blobs. No mutable resource, writer or approval authority enters this memo.

Within one Task projection, historical handoff and integration checks share canonical event
reads. An appended prefix forces another read. The memo ends with that projection; cached CAS
integrity never survives a public operation. Integration phases are validated once after the
Task suffix is applied, including the previously cached phases.

A private append helper can consume a projection checked earlier in the same public operation.
It preserves the expected-sequence transaction fence, transition validation and publication of
new references. Arbitrary output, context and retry callbacks still require a fresh projection
after returning. First reservation, invocation, start and failed settlement have no such callback
between their checked state and append. Successful settlement/publication retain both reads;
owned completion retains a read after each of its two domain callbacks. They are not redundant.

Mixed native evidence is classified by its verified CAS identity. Raw content-addressed bytes
stream through a fixed buffer and cannot become typed usage authority through a JSON `type` key.
Artifact-addressed envelopes retain the existing 8 MiB bound and canonical validation, followed
by exact usage type, Attempt producer, context, uniqueness and charge-floor checks.

A new revocation requires an Approved decision. An exact authenticated repeat returns the recorded
revocation event after checking current writer, expiry and authorization. Changed repeats refuse.
Historical events remain readable, including duplicate or non-approved revocations accepted by
older writers. Unauthenticated historical revocations lack an exact proof to match and cannot
acquire new replay authority. No old artifact or event identity is rewritten.

## Alternatives and verification

An operation-wide hash cache spanning callbacks could miss evidence changed by a domain callback;
it is not used. Disabling fresh integrity checks or remembering authorizations would change the
Task contract. Cross-operation event or CAS freshness caching is also excluded.

Mutation tests replace and remove captured authority classes, retain current approval/lease and
usage-floor refusals, and check callback corruption on publication. Focused fixtures record
compiler full-versus-memo latency, canonical replay counts and exact projection counts. These
are deterministic local measurements, not a claim about live-model Round latency or contention.
