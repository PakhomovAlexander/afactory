# ADR-0246: Carry the Reviewer Result schema natively on the Campaign path

**Status:** accepted (2026-10-28). Extends
[ADR-0103](0103-constrain-native-claude-task-replies.md) and
[ADR-0208](0208-recover-an-attempt-from-a-transient-provider-failure-and-a-reply-that-does-not-parse.md)
from typed Task replies to review-Campaign reviewers, and changes the printed `af review render
--json` document to `af/review-render@2`
([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

## Context

Typed Task Workers already reply under a native schema: Claude Code receives the reply envelope
inline as `--json-schema` (ADR-0103) and Codex its strict projection as the `--output-schema` file
(ADR-0208), so the CLI refuses a structurally wrong reply before it is charged as a reply failure.

A review-Campaign reviewer did not. Its prompt is Markdown, not an `af.worker-request/1`, so both
adapters ran it unconstrained: the package instructions end with a prose output contract (`Your
FINAL message must be exactly one JSON object and nothing else …`), and the kernel read the reply
only after the paid call. A reply with a missing field, a severity outside the enum or no object at
all failed the Attempt as `invalid_output_contract`, and its whole charge was spent on an answer the
kernel could not read (issue
[#372](https://github.com/PakhomovAlexander/afactory/issues/372)). `af review render` showed the
prose contract and nothing about a schema.

## Decision

1. **One schema for the Campaign reviewer's reply.** `review_runner::task::reply_schema` gains
   `ReplyContract`, the contract a native call is constrained to: `Request`, a typed request's own
   output schemas exactly as before, or `ReviewerResult`. Its schema, `reviewer_result()`, is the
   structure of `schemas/reviewer-result-v2.json` — every field with its type, the required ones,
   the enums and closed objects — beside the two transport fields a reply may carry next to the
   flat result: the optional `proposal` (ADR-0038) and the optional `notes` of warm layers. A unit
   test holds it equal to the published schema's structure and the transport fields equal to their
   Rust declarations. The value rules of the result schema (string bounds and patterns, the
   canonical path grammar, the line and confidence ranges, the change-wide line rule) and the
   Campaign's own rules (one disposition per assigned prior Finding) are not in it: they stay the
   kernel's.
2. **Both runners carry it on every Campaign reviewer dispatch.** `WorkerModelAdapter` gains
   `invoke_constrained(…, contract)`; `invoke` and `invoke_started` are it with
   `ReplyContract::Request`. The Campaign Review host dispatches every model reviewer with
   `ReplyContract::ReviewerResult`, and af's current-Provider adapter forwards the contract to the
   native client after its identity recheck. Claude Code receives `reviewer_result()` inline after
   `--json-schema`, bounded to 64 KiB as in ADR-0103, and the reply is the native
   `structured_output`; its textual `result` is never a fallback. Codex receives the strict
   projection of ADR-0208 — every property required, an optional one nullable — written to the
   Attempt's staging directory and named by `--output-schema`; the `null`s it made the model write
   for an absent `proposal`, `notes`, `line`, `confidence`, `rule_id` or `occurrence_key` are
   removed before the kernel reads the reply, and the `-o` file is kept as written. The prompt,
   its prose contract and the context manifest are unchanged, byte for byte. Command Workers are
   unchanged. The Provider admission probe stays unconstrained.
3. **Kernel validation is unchanged.** The reply still goes through `parse_reviewer_result`, the
   proposal and notes parsers, `ReviewerResult@2` admission and disposition coverage exactly as
   before; the native schema is a structural constraint before the reply exists, never a
   replacement for any of them.
4. **How the charge and the failure class change.** A structurally wrong reply is refused inside
   the native call: Claude Code validates the reply against the schema and asks the model again
   within the same call, and Codex constrains the final message to it. So:
   - A call that ends with a constrained reply is charged, as before, its complete reported usage,
     any native re-ask included, under the Attempt's existing reservation and deadline. af adds no
     retry loop and refunds nothing.
   - A call that ends without one — a Claude Code envelope without a `structured_output` object,
     or an error result; a Codex process without a final message — fails the Attempt with feedback
     code `provider_failure`, charged exactly the usage that call reported. Before, the same
     structurally wrong reply failed it as `invalid_output_contract`. `af task report` therefore
     counts such an Attempt and its tokens under `provider_failure`, and the retry Attempt's typed
     feedback names that class. A Task whose reviewer ends this way on every Attempt still reports
     the cause `malformed_reply`.
   - `invalid_output_contract` and `output_admission_rejected` remain for a reply that passes the
     native schema and still breaks a kernel rule: an empty `title`, a non-canonical path, a
     missing disposition.
5. **The capability check covers both options.** `--json-schema` and `--output-schema` were
   already on `CLAUDE_INVOCATION` and `CODEX_INVOCATION` (ADR-0103, ADR-0208), so the token-free
   check of ADR-0146 refuses a CLI whose help page lacks either before any paid dispatch, at Task
   Provider admission as on `af provider status` and `af provider setup`. No weaker invocation
   without the schema exists.
6. **`af review render` shows the schema.** `RenderedInput` carries the `NativeReplySchema` (the
   option and the exact schema) the adapter derives through the same `ReplyContract`. The text
   header prints it on its own `reply` line; `--json` prints `af/review-render@2`
   (`schemas/review-render-v2.json`), whose new `reply_schema` field is that pair, or `null` for a
   command Worker. `af/review-render@1` is no longer printed; nothing reads either version.

## Considered options

- **Put the schema in the prompt only.** Rejected: that is the prose contract the reviewers already
  had, and a prompt is not a transport contract (ADR-0103).
- **Wrap the Campaign reviewer in an `af.worker-request/1`.** Rejected: it changes the dispatched
  bytes, the context manifest and every recorded Review context, and the flat result contract would
  become a port of a wrapper. The contract travels beside the input instead.
- **Constrain to the full published schema, patterns and bounds included.** Rejected for both
  runners alike: a strict facility does not take those keywords (ADR-0208), and one structural
  schema keeps the two runners judged by the same native rule while the kernel keeps every value
  rule.
- **Allow `notes` only when the node's warm policy asks for them.** Rejected: the schema would then
  depend on Campaign state `af review render` cannot know. The kernel already ignores notes it did
  not ask for.
- **Change `invoke`'s signature.** Rejected: every Worker double in the suites would change; one
  method with a default for doubles that spawn nothing keeps the contract explicit where a native
  client runs.

## Consequences

- A Campaign reviewer's structurally wrong answer costs a native re-ask inside the same call, not
  a failed Attempt and a second paid Attempt. The model's tokens for that re-ask are the call's.
- A reviewer Attempt that fails because its CLI delivered no constrained reply is recorded as a
  Provider failure, not a contract failure.
- A Codex reviewer now always writes `proposal` and `notes`, `null` when absent; af removes them
  before parsing, so the recorded Reviewer Result is unchanged.
- Consumers of `af review render --json` read `af/review-render@2`.

Tests: `review-runner` `reply_schema` unit tests (published-structure parity, strict round trip);
`review-runner-claude` `task_structured::a_campaign_reviewer_passes_the_reviewer_result_schema_and_reads_structured_output`;
`review-runner-codex` `task_worker::a_campaign_reviewer_passes_the_strict_reviewer_result_schema_file`;
`review-pipeline` `task_campaign_review::host::native` (every reviewer dispatch constrained, and
`a_reviewer_reply_the_cli_did_not_deliver_fails_as_a_provider_failure_charged_its_usage`); af's
`providers::compatibility` `a_cli_without_the_native_reply_schema_option_is_refused`,
`render::a_model_worker_input_is_the_package_contract_and_change_set_with_no_effects` and
`campaign_loop::provider_admission::common_review_captures_admission_cost_and_retains_exact_overruns`
(a fake `codex` records the schema file a Campaign reviewer's dispatch names).
