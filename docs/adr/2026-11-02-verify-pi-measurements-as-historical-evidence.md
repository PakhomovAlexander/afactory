# ADR-2026-11-02-verify-pi-measurements-as-historical-evidence: Verify historical Pi evidence explicitly

**Status:** accepted (2026-11-02).

## Context

The #534 Pi validation helper compared a retained whole-source measurement projection to every
future checkout in normal functional CI. Unrelated release generation and merged product code
then required another expensive measurement just to pass functional validation. Issue #551
corrects that contract. A past measurement cannot prove a future source was measured.

## Considered options

- Require a fresh Pi measurement for each checkout or fast release: rejected because historical
  evidence is not a current performance obligation and this couples functional CI to measurement.
- Accept a mismatching projection or silently update its digest: rejected because it invents
  evidence and breaks exact-source integrity.
- Drop source or log checks: rejected because a retained outcome must stay bound to its measured
  source, command, filter and actual log bytes.

## Decision

Historical Pi measurements are immutable, source-bound evidence. Fast release does not mandate
fresh Pi measurement. Normal functional CI tests the helper with controlled source/log fixtures
and parses retained records, without requiring any past projection to equal today's checkout.

Historical verification requires explicit source directory and claim selection. The helper
compares the exact digest and entry/file counts under the existing fixed projection exclusions.
A supplied real log must match its SHA256, source binding, command, filter, exit and sole summary.
A source-only check explicitly says the log was not supplied or read. It makes no verified log
or fresh performance claim. Wrong sources, tampering, malformed or duplicate claims, unknown
versions and extra fields fail closed. No implicit current-checkout verification remains.

The closed `af/pi-validation-claim@1` JSON shape and `af.pi.validation-source/1` projection stay
unchanged. Neutral versioned claim delimiters replace the single-current designation; explicit
`--claim-source` selects one recorded source projection. Catalog, adapter, RC4 and premerge RC5
measurements remain separate historical records with their original values and log hashes.

This corrects the evidence contract documented in the Pi validation design record. It changes
no release architecture, Ubuntu build pin, QEMU execution, archive smoke, signature, exact-SHA
selection, functional/e2e/uix requirement, test budget or sandbox/security boundary.

## Consequences

Unrelated future edits do not force performance work in normal validation. Operators can still
verify a historical measurement with its matching source and real log. Missing historical
inputs cannot be represented as verified. No measurement of the combined RC5 tree is claimed,
and cancelled Tasks remain unverified. Release publication still requires its actual gates.
