# ADR-0076: Decode and verify typed CAS reads once

Date: 2026-09-12
Status: Accepted

## Context

The Linux implementation-PR gate recovered an interrupted fix-verifier but exhausted the
original Task deadline before goal evaluation. A local sample of the same CLI regression
showed repeated JSON decoding and canonical identity validation during Task projection.
`Cas::get_json` first verifies an envelope through the generic byte reader; typed callers then
decode and verify that envelope again. The same complete integrity check need not run twice
within one read.

## Decision

Add `Cas::get_artifact` for callers that require an artifact-addressed envelope. It opens one
object, enforces the existing envelope size limit before allocation, checks exact length,
decodes canonical stored JSON and validates both the content and provenance identities. The
requested digest must equal the envelope's artifact identity. Return that validated envelope.
The generic byte reader uses the same envelope decoder after its ordinary blob check.

Task execution-record decoding, common Task envelope reads, captured catalog validation and
the Task runtime use the typed method. They retain their own expected-type, version, graph,
input, lease, context and domain checks. No identity result survives the read as a cache;
removal, replacement or corruption is checked again on the next call, including after reopen.

Raw content-addressed JSON remains readable through `get_json`. It cannot impersonate an
artifact-addressed record through `get_artifact`. No canonical encoding, hash domain,
persisted artifact ID, Task deadline, Worker allowance or verification reserve changes.

## Alternatives

Increasing the fixture's deadline would not address the repeated work. A cache keyed only by
CAS path or modification metadata could conceal replacement and is not used. Removing the
caller's version/domain checks would widen admission and is not used.

## Verification

Typed-reader regressions cover original/reopened reads, raw-versus-artifact identity,
noncanonical and duplicate-field JSON, truncation, trailing bytes, false content/provenance
digests, replacement, restoration, removal and invalid requested digests. Existing Task replay,
wide-accounting and catalog-authority regressions remain applicable. The repair interruption
fixture retains its original deadline, real lease expiry and lost-Attempt charge assertions.
