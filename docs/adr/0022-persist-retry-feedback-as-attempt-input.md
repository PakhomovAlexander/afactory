# Persist retry feedback as an Attempt input

**Status:** accepted (2026-08-25)

When a syntactically valid reviewer answer failed contract admission, the pipeline retried the
same node. Feeding the refusal reason back to the reviewer made the retry useful, but the first
implementation held that reason only in the running process and appended it directly to the next
prompt. A crash and replay could therefore dispatch a different invocation from the one implied by
the durable event log. Adding the field to `AttemptDispatched@1` would instead mutate a frozen
event payload in violation of ADR-0002.

We decided to store the canonical refusal history as a content-addressed JSON artifact and publish
an additive `AttemptInput@1` event for the retry attempt. The event names the node and attempt,
references the history artifact in both its payload and artifact references, and is appended in
one transaction with `AttemptDispatched@1` before external execution. Prompt construction loads
the history from that durable artifact. Replay rebuilds the same input and refuses an unreadable or
empty history rather than silently issuing a different prompt.

## Considered options

- **Keep refusal history in memory.** Smallest implementation and enough for uninterrupted runs.
  Rejected because exact invocation inputs are part of M0's replay authority, including retries.
- **Add refusal history to `AttemptDispatched@1`.** Keeps one event per dispatch, but changes an
  established `@1` payload and makes old and new rows with the same type mean different things.
  Rejected under ADR-0002.
- **Reconstruct prompt text from prior failure events.** Avoids another event, but couples prompt
  rendering to diagnostic strings and requires every future failure shape to remain a prompt-input
  parser. Rejected because failure evidence and exact invocation input are different contracts.
- **Publish an additive Attempt input artifact and event (chosen).** It preserves frozen events,
  makes the prompt source explicit, and gives replay one exact artifact to validate.

## Consequences

- `AttemptInput@1` is a permanent member of the event vocabulary and its reader cannot later be
  removed while logs containing it remain supported.
- A first attempt has no refusal-history input, so its prompt remains byte-identical. A retry with
  refused attempts must publish its input before dispatch.
- The input event and dispatch event are atomic with respect to the event store; external execution
  starts only after both are durable.
- Retry feedback is labelled as data, JSON encoded, and subject to the existing prompt-input byte
  bound. Model-controlled output cannot become an instruction channel through a diagnostic.
- Any future retry-only prompt input follows the same rule: persist exact input authority instead
  of relying on process memory or changing a frozen dispatch event.
