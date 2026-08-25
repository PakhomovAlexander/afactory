# Separate retry feedback from terminal diagnostics

**Status:** accepted (2026-08-25)

ADR-0022 made the exact refusal history consumed by a retry durable as `AttemptInput@1`. Its first
implementation still reconstructed that input after process restart from every preceding
`AttemptFailed@1` and `AttemptFenced@1` diagnostic. That made generic failures—adapter panics,
sandbox sealing errors, CAS errors, and failures without a reviewer answer—silently become prompt
instructions. It also made stable prompt bytes depend on human-readable diagnostic wording.

We decided to publish the accumulated feedback artifact when a retryable attempt produces it, in
an additive `AttemptFeedback@1` event appended atomically with that attempt's frozen failed or
fenced terminal event. Replay obtains feedback only from `AttemptFeedback@1` and exact retry input
only from `AttemptInput@1`; it never parses terminal diagnostics into prompt text. A later retry
copies the feedback artifact's value into its own durable Attempt input before dispatch, preserving
the input/output boundary on both sides of a process restart.

## Considered options

- **Continue parsing terminal diagnostic strings.** Requires no new event. Rejected because
  diagnostics and invocation input are different contracts, as ADR-0022 already records, and
  generic infrastructure failures do not imply that the reviewer must correct its answer.
- **Change `AttemptFailed@1` or `AttemptFenced@1`.** Could add an optional feedback ID to each
  payload. Rejected because both payload versions are frozen by ADR-0002.
- **Infer feedback from artifact-reference position.** Avoids a payload change, but artifact
  references are a set of durability dependencies rather than a positional protocol. Rejected as
  ambiguous and invisible to schema validation.
- **Add `AttemptFeedback@1` (chosen).** Keeps terminal evidence immutable, names exact prompt data,
  and lets the event store enforce atomic publication with its originating terminal attempt.

## Consequences

- `AttemptFeedback@1` is a permanent event type. It names one non-empty refusal-history artifact,
  and must be in the same append transaction as the matching `AttemptFailed@1` or
  `AttemptFenced@1`.
- Contract refusals and retryable timeouts may produce feedback. Panics, transport failures,
  sandbox/CAS failures, and other terminal diagnostics produce none unless a future contract
  explicitly defines exact reviewer-facing feedback for them.
- Replay remains independent of diagnostic prose. Changing an error message cannot change a later
  reviewer prompt.
- `AttemptFeedback@1` records produced feedback; `AttemptInput@1` records feedback actually
  consumed by a dispatched retry. Neither substitutes for the other.
