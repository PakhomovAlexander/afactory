# ADR-2026-11-03-bound-pi-native-inferences-by-remaining-allowance: Bound Pi native inferences by remaining allowance

Date: 2026-11-03. Status: proposed; accepted on merge.

## Context

The first real Kimi Pi-light trial spent input 8,277,986 and output 39,385 tokens with
8,088,576 cache-read tokens, against a 1,000,000-token reservation. Its conservative AF
charge was 8,317,371. The initial rendered context was 16,737 bytes; that does not establish
the cost of subsequent prompts. The adapter retained aggregate usage only, so no retrospective
per-inference breakdown can be established. The final reply used an object for `outputs.report`
instead of an array; typed admission refused it, no candidate was sealed, and its removed edits
are not accepted. Issue537 remains for a later actual Pi implementation.

## Decision

Extend the common model adapter interface with a typed kernel-owned remaining token allowance.
The initial Attempt dispatch, Provider admission and existing format re-ask supply their actual
remaining reservation. Alternate admission wording likewise subtracts the first call's complete
reported charge. Worker JSON and environment variables cannot supply this authority. Pi's
unbudgeted entrypoints refuse before starting its host; token-free discovery remains token-free.
Other adapters retain their existing native invocation and reply-constraining behavior.

Version the closed host protocol to `af.pi-host/4`. Rust sends the positive safe-integer allowance
and the actual reply schema in `prepare`, separately from model-visible input. Old protocols and
missing, zero, malformed or unsafe allowances refuse. The host wraps every native SDK
`streamSimple` inference, checking cumulative observed AF charge first. A previous inference
must have finished successfully and supplied all counters required by its backend convention.
Malformed, conflicting, decreasing, missing or incomplete required usage prevents continuation.
No paid inference may start after the observed allowance is exhausted. Each inference receives
SDK `maxTokens` bounded by remaining allowance and the pinned model's native output limit.

Native prompt usage is known only after the response. Therefore a **single inference can
overshoot** the remaining AF allowance, even with an output cap. AF does not assert tokenizer-exact
preflight enforcement from byte or character estimates. Preserve the entire observed charge,
including overshoot, and refuse the next inference. Unknown usage remains unknown, with available
observed floors retained. Malformed or conflicting optional counters do not erase independently
valid aggregate counters from the same native observation, including the first one. Exclude
inconsistent optional components before emitting incomplete safe evidence; retain preceding safe
counters on decreases and leave conflicting aggregate counters unknown. Such accounting fails
the Attempt and prevents further native inference, even when the aggregate floor is known.
Cancellation, original deadline, settlement requirements, disabled retries/compaction and common
process supervision remain unchanged.

Kimi still charges full input plus output once per inference, without a cache discount.
OpenAI still charges input minus cache read plus output and requires observed cache reads.
Neither convention is a provider invoice. Reasoning and cache writes are already included.
Domain `af.pi-adapter-policy@3` binds both conventions and the host's changed runtime semantics.
All identity, OAuth/auth-context, endpoint, model, sandbox and tool protections remain in force.

Retain `af/pi-inference-evidence@1` as bounded safe JSON in the Attempt's raw CAS evidence,
including on failure or cancellation when available. It holds sequential per-inference cumulative
native counters, native usage-observation counts and completion flags; aggregate started/completed
counts; counts for the six declared tool names; the fresh non-secret delivery nonce identifying
this request; allowance and policy identity; settlement/success
flags; and the separately labeled aggregate AF usage observation. It contains no prompt, answer,
reasoning text, raw SDK body, credential, account claim, tool argument/result or arbitrary error.
Repeated cumulative and final observations replace one inference; the trace is diagnostic and
never a second source of charge. No retrospective trace is fabricated for the failed trial.

Bound a call to 256 native inferences, 128 native usage observations per inference and 4,096
attempted file-tool executions. Stop at diagnostic capacity instead of allowing further inference
without coverage. Reaching the usage-observation limit retains that last observation and fails;
a tool beyond capacity is refused before execution. Unsettled/unsuccessful evidence is explicitly
non-authoritative for complete accounting, even when earlier counters are available. The existing
bounded protocol/output supervisor also fails closed and retains previously admitted observations.

At the actual system boundary, supply file-only execution guidance, the exact array-envelope
example and the actual declared reply schema. Pi-light instructions omit shell/test-running
boilerplate while retaining scope, architecture, mandatory check nodes, budgets and review
contracts. A file-only Worker reports tests as not run. The example grants no output port;
the request's actual schemas and existing typed admission remain authoritative. There is no
additional reply salvage or object-to-array conversion beyond common ADR-0208 extraction/re-ask.

## Considered options

- Increase the reservation or discount Kimi cache: obscures repeated-prompt spend and changes
  the chosen budget convention; rejected.
- Trust a prompt field or environment allowance: model-visible data could widen authority;
  rejected.
- Estimate input using characters divided by four: cannot prove exact native token usage;
  rejected as an enforcement claim.
- Keep only aggregates or log SDK bodies: the former cannot distinguish repeated prompts from
  aggregation defects; the latter exposes sensitive data and grows without bound; rejected.
- Salvage the failed trial's wrong reply shape: weakens typed admission and cannot recover a
  removed sandbox; rejected.

## Consequences

Hermetic multi-inference SDK tests exercise output caps, exhaustion before the next call,
overshoot retention, unsafe usage and diagnostic capacity. Common dispatch tests prove remaining
format-reask allowance and refusal of object payloads. Closed schema/parity tests cover the host
and safe evidence. These tests spend no model tokens and read no private authentication.
The coordinator can consider a bounded retry only after inspection and independent verification;
this implementation does not claim a successful live Kimi trial, implement issue537, publish,
run remote gates or measure performance.
