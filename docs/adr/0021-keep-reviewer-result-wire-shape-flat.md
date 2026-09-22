# Keep the ReviewerResult wire shape flat

**Status:** accepted (2026-08-25); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the permanence of `ReviewerResult@1`, which is
refused, and the new-version rule for pre-GA reviewer-result shapes. The flat report shape, the one
`review-core` validator and the conformance corpus carry over to `ReviewerResult@2`, the only
reviewer result.

Every live reviewer adapter emits one flat `ReviewerResult@1` value whose `reports` entries carry
`severity`, `file`, `line`, `title`, `body`, `fix`, and `confidence`. The kernel admits that value,
then converts each report to the typed `FindingReport@1` artifact that is authoritative for the
Ledger under ADR-0005. An earlier schema description incorrectly placed the typed report shape on
the reviewer wire, even though no producer emitted it. Separate hand-written validators in the
store and pipeline then allowed their interpretations to drift.

We decided that `ReviewerResult@1` permanently denotes the produced flat wire shape. Its complete
semantic validator lives in `review-core`, beside the Rust report types it validates. Persistence
and pipeline admission both call that validator; `review-store` retains only a compatibility
wrapper for callers that need `StoreError`. The schema and conformance corpus describe the same
flat value, including optional `line` and `confidence`, canonical paths, and non-whitespace claim
content. Typed `FindingReport@1` remains the admitted artifact shape, not a second reviewer-result
arm.

## Considered options

- **Change adapters to emit typed Finding Reports directly.** This would align the wire with the
  mistaken schema, but it would make reviewers choose typed relation and location semantics before
  the kernel admits and assigns claim identity. Rejected because M3 owns that transition and
  existing reviewer packages already emit the flat contract.
- **Accept both flat and typed reports in ReviewerResult@1.** This is tolerant, but makes one
  version name denote two incompatible shapes and leaves every consumer to normalize both forever.
  Rejected because the live producer set proves only one shape is required.
- **Keep a validator in each consuming crate.** Locally convenient, but changes to demands,
  disputes, paths, or numeric bounds can again reach persistence and execution at different times.
  Rejected because a durable contract needs one semantic owner.
- **Keep the flat contract and one core validator (chosen).** It records the wire that exists,
  preserves package compatibility, and makes all admission paths enforce the same rules.

## Consequences

- `ReviewerResult@1` readers accept only the flat `reports` shape; typed reports at that boundary
  are refused rather than guessed.
- The schema, conformance corpus, Rust validator, store admission, and pipeline admission must
  advance together. Corpus cases cover every semantic rule that JSON Schema and Rust can share.
- The kernel conversion remains the only bridge from a reviewer result to authoritative typed
  Finding Report artifacts.
- A future incompatible reviewer-result shape requires a new artifact type version; it may not be
  added as another arm under `ReviewerResult@1`.
