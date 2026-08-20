# Fixed requires current-Subject verification

**Status:** accepted (2026-08-20)

Neither an operator assertion, a reviewer Drop, a patch application, nor a Change Attestation may
set a Finding to `fixed`. They may move exact Report claims to `pending-verification`. A trusted
verifier must then evaluate the current Subject, explicit required-reviewer dispositions, checks,
lineage, and Evidence and emit `FixVerification@1`; only trusted resolution policy may convert a
positive verification covering every active claim into `Resolution(fixed)`.

## Considered options

- **Keep `reviewctl resolve ... fixed`.** Rejected because a note can become terminal state with
  no machine-checkable relationship to changed content or evidence.
- **Treat one clean reviewer Round as proof.** Rejected because absence is not a disposition and
  one reviewer's Drop may not cover corroborating claims or required checks.
- **Attestation followed by trusted verification (chosen).** It supports human-applied and future
  kernel-integrated changes through one resolution path without granting either source authority.

## Consequences

- `reviewctl resolve` directly admits only policy-authorized non-fixed dispositions such as
  `rejected` and `wontfix`.
- External fixes use `reviewctl attest-change`; stale lineage or stale Finding views fail closed.
- Every active attached Report claim needs coverage. Verification of one claim cannot terminally
  resolve a distinct corroborating claim.
