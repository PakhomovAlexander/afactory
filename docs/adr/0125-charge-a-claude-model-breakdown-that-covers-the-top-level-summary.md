# ADR-0125: Charge a Claude model breakdown that covers the top-level summary

**Status:** accepted (2026-10-01). Supersedes in part
[ADR-0102](0102-account-for-every-reported-claude-task-model.md): the rule that the top-level
bill must equal the aggregate of the model breakdown or the selected model's entry.

## Context

ADR-0102 reconciles the two usage summaries in a Claude native result: the top-level `usage` and
the per-model `modelUsage` breakdown. It requires the top-level input, output and cache-creation
tokens to equal either the breakdown's aggregate or the selected model's entry. Any other pair is
an unreconciled bill: the charge is incomplete, the Attempt keeps its whole reservation, and its
output is refused.

Claude Code 2.1.285 breaks that equality at the end of a long session. The same Worker, Task and
repository produced these final results (input / output / cache creation / cache read):

| Native client | top-level `usage` | sum of `modelUsage` |
|---|---|---|
| 2.1.284 | 88 / 70,430 / 171,379 / 5,737,166 | the same |
| 2.1.284 | 82 / 69,694 / 167,022 / 5,030,037 | the same |
| 2.1.285 | 88 / 60,087 / 165,142 / 5,375,264 | 90 / 60,095 / 166,844 / 5,524,885 |

The 2.1.285 result names one model, the selected one, and no subagents. Its breakdown is larger
than its top-level summary in every component, by what looks like one small request that the
client counts in the breakdown only. Short sessions on 2.1.285 still report equal summaries.

The adapter refused that result as unreconciled. A Worker that had finished 46 minutes of work
failed, and the run report said only `Native billing usage is incomplete`. The native client
updates itself, so a machine reaches 2.1.285 without notice.

The equality rule exists to keep an under-reported charge from passing as complete. Here the
breakdown reports more than the top-level summary, and ADR-0102 already states that the
top-level counters can understate the invocation.

## Considered options

- **Keep exact equality and require an older native client.** Rejected: the client updates
  itself, and the rule then refuses a bill larger than the one it would have accepted.
- **Charge the larger of each component whenever the summaries differ.** Rejected: a top-level
  component above the breakdown is spend that no reported model entry explains. ADR-0102 refuses
  unknown model activity, and calling that bill complete would hide it.
- **Accept a difference within a tolerance.** Rejected: no native field bounds the size of one
  request, so any threshold is a guess that a later client can exceed.
- **Charge a breakdown that no top-level component exceeds as the complete bill (chosen).**

## Decision

The exact matches of ADR-0102 are checked first and keep their meaning. When neither holds, a
complete top-level summary and a complete aggregate breakdown still reconcile if the top-level
input, output and cache-creation tokens are each at most the aggregate's. The Attempt is charged
the aggregate, and that charge is complete. No usage observation records the difference: the
retained raw native result holds both summaries.

Cache reads stay informational, and are compared only when both summaries report them. Equal
bills must then report equal cache reads. A larger breakdown may report more cache reads than
the top-level summary, never fewer: a top-level cache-read count above the breakdown's refuses
the output and leaves the charge complete.

A top-level billed component above the aggregate remains an unreconciled bill, with the larger
known charge, the reservation floor and refused output of ADR-0102. Model identity, duplicate,
malformed and oversized-map rules are unchanged, and apply before a larger breakdown is accepted.

When the Task runtime refuses output because the bill is incomplete, or because a usage
observation differs from the returned counters, the Attempt's diagnostic keeps the reason the
Worker had already failed with:
`Native billing usage is incomplete: Claude top-level and per-model usage cannot be reconciled`.

## Consequences

Claude Task Workers complete on Claude Code 2.1.285. Their Attempts are charged the breakdown,
which is the larger of the two reported bills. That charge can exceed the reservation; recording
the overrun still never grants another invocation.

The adapter cannot tell which requests the top-level summary left out. It relies on the
breakdown listing every model the client used, as ADR-0102 already does for model identity.

The diagnostic is prose in the existing `af.task-diagnostic/1` shape. No artifact type, event or
schema changes, and settled Attempts keep the result they recorded.
