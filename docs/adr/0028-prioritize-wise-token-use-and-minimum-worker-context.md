# Prioritize wise token use and minimum Worker context

**Status:** accepted (2026-08-25)

M2's dogfood Campaigns proved that model context is the kernel's dominant variable cost: later
Rounds approached or exceeded one million tokens while repeated reviewers rediscovered overlapping
parts of the same change. ADR-0027 bounded the standard milestone policy, but it did not establish
the project-wide trade-off order or require every Attempt to justify the information injected into
its model context.

The binding order for Afactory is now: **wise token consumption**, **as little information in
Worker context as possible**, performance, fast, simple and clear, extendable, deterministic,
Unix-native. Wise consumption means total tokens per verified outcome, not the cheapest call:
every model call has a bounded reservation and a named transition or typed artifact it can advance;
retries and fan-out require new information or a distinct hypothesis. Every Worker receives the
smallest sufficient role-scoped Input. The Snapshot remains inspectable through allowed tools, but
parent transcripts, whole Ledgers, unrelated documents, repository dumps, and other Workers'
private reasoning are absent from the prompt by default.

## Considered options

- **Keep determinism first and treat token use as a performance metric.** Rejected because a
  replayable pipeline can still waste the dominant resource and bury decisive evidence in an
  oversized context.
- **Minimize tokens absolutely.** Rejected because under-informed work and starved verification
  create retries and false confidence; they increase cost per trusted result.
- **Put wise token use and minimum Worker context first (chosen).** Makes each model call and each
  injected artifact justify its informational value while retaining deterministic replay as a
  lower-ranked, binding contract.

## Consequences

- Attempt Inputs carry a context manifest: every injected artifact and the port or rule requiring
  it, plus rendered byte and estimated-token size. Tool retrieval is bounded and recorded.
- Provider usage receipts and Task/Campaign reports expose available input, output, cache, and
  reasoning tokens. Prompt, retry, fan-out, and context changes report a representative dogfood
  baseline or state why no comparison is possible yet.
- ADR-0027 remains the default review policy; this ADR explains the higher-level priority behind
  one reviewer, bounded rounds, and explicit specialist exceptions.
- Existing deterministic schemas, replay guarantees, and fixtures remain binding. Moving the
  value lower does not silently weaken an accepted contract; doing that still requires a
  superseding ADR and migration.
