# ADR-0143: Charge zero and record unknown usage when no usage is reported

**Status:** accepted (2026-10-06). Supersedes in part
[ADR-0049](0049-run-task-workers-through-shared-durable-attempts.md): the rule that started work
of an unsettled old-writer Attempt keeps its full reservation, and
[ADR-0078](0078-bind-review-conclusions-to-exact-task-accounting.md): the rule that unknown
Review usage charges the original reservation. Amends the round table and details of
[ADR-0142](0142-carry-the-af-task-report-in-every-pull-request.md) in place, before their first
release.

## Context

Every Attempt runs under a bounded token reservation (ADR-0028). When its Provider reports usage,
the Attempt is charged exactly that. When nothing is reported, af charged the whole reservation:
the Task runner settled a native Attempt without counters at its reservation, a Task-hosted
review Attempt recorded the reservation as its provenance charge, and recovery of an Attempt
whose writer lease expired settled it as abandoned at the reservation.

Issue #165 shows what that costs. A Codex Attempt that used tools and then failed with `Selected
model is at capacity` reported no usage and was charged 400,000 tokens. So was an Attempt lost
when the machine slept and its lease expired. Two such failures exhaust a verification reserve,
so the next reviewer cannot start, although the Provider billed little or nothing. The report
then shows that figure as spend. The failure's diagnostic did not help either: the Codex runner
recognized the `turn.failed` event, but `capacity` was no class it knew, so the Attempt failed
with `Codex Worker failed with Ok(ExitStatus(unix_wait_status(256)))`.

The owner decided (2026-10-06): when a Provider reported no usage for an Attempt, af charges 0
tokens and records the usage as unknown, never an estimate and never the reservation. Attempts
whose usage was reported are charged exactly as before.

## Decision

1. **Settlement.** An Attempt that settles with no usage reported is charged `0` and its
   `Settled` execution record carries `unknown_usage: { cause }`. This covers the Codex and
   Claude Task runners (a Claude result envelope without a `usage` object reports nothing),
   Provider admission probes, Task-hosted review Attempts, recovery of an Attempt whose writer
   lease expired, and interrupted Attempts. An Attempt with any reported counter, an observed charge or a retained
   wall usage is "reported": it is charged as before, including the reservation floor of a
   reported but incomplete bill (ADR-0079, ADR-0125). An Attempt with no Provider, such as a
   command Worker, keeps its known zero and is not unknown.
2. **The cause** is what af knows, a closed `TaskUnknownUsageCauseV1`: the classified native
   failure (`capacity`, `rate_limit`, `authentication`, `model_unavailable`, `network`),
   `lease_expired` for an Attempt its writer's successor recovered, `interrupted` when af
   stopped it (an operator interruption or a lost heartbeat), and `unreported` when nothing
   better is known, an unclassified native failure included. A writer is fenced only by a
   successor that took its expired lease, so a fenced Attempt is recorded as `lease_expired`.
3. **The record.** `af/TaskExecutionRecord@5` changes in place under clause 3 of
   [ADR-0113](0113-ga-reads-only-what-ga-writes.md): `Settled` gains the optional
   `unknown_usage` object. A record that carries it has `charged_tokens` `"0"` and no
   `usage_id`; the type and the schema both refuse anything else. Every record written before
   has no such field, keeps its bytes, and is read with the charge it recorded: nothing is
   rewritten. Replay refuses an unknown settlement for an Attempt with an observed charge, and
   admits an abandoned settlement below its reservation only with the marker. A Task-hosted
   review provenance without a usage report records the zero charge its settlement carries.
4. **Budgets.** The unknown Attempt's reservation is released and nothing is added to the Task's,
   node's or verification reserve's charged tokens, so a capacity failure no longer exhausts a
   verification reserve. The Attempt still counts against the Attempt limits.
5. **Diagnostics.** The native failure classes gain `capacity` (`at capacity`, `over capacity`,
   `overloaded`). A Codex Attempt that ends with an `error` or `turn.failed` event reports the
   closed class diagnostic, such as `Codex Worker failed: Provider model at capacity
   (capacity); transport: …`, and `Provider returned an unclassified failure (unknown)` for an
   unclassified one, instead of the bare exit status. The raw Provider message is still never
   copied into an ordinary diagnostic.
6. **Reporting shows unknown as unknown, never as 0 spend.** `af task show` and `af task list`
   add `unknown_usage_attempts` to `af/task-inspection@11` and `af/task-list-entry@2` (absent
   when none). `af task list` prints `5712 tokens (+1 unknown)`, and `af task show` adds the
   line `tokens 5712 (+1 unknown: capacity)` naming the causes. The browser's Tasks and Workers
   panes print `(+N unknown)` after their charged totals. In `af task report` the
   Tokens cell of a round, the `Total:` row and a node is the charged total followed by
   `(+N unknown)` when N of its Attempts' usage is unknown; the round's `<summary>` names the
   causes (`1 Attempt's usage unknown (capacity)`); and the `af/task-report@1` document carries
   `unknown_usage` counts on the Task's attempts, each node and the totals, with the causes by
   count. `scripts/check-pr-report.py` needs no change: it checks the table's shape, not a
   Tokens cell's text.

## Considered options

- **Charge the reservation (the status quo).** Safe against an under-reporting Provider, but it
  reports a fiction as spend: 400,000 tokens for a call the Provider refused at once. It also
  turns transient capacity failures into an exhausted budget. Rejected by the owner.
- **Charge a declared placeholder,** a per-Provider or per-Worker estimate of a failed call.
  Rejected: it is still an estimate presented as a charge, it needs a figure nobody can check,
  and the report would mix measured and invented numbers in one column.
- **Recover the usage from Codex session files.** Codex writes per-session logs that may hold
  the counters of a failed turn. Rejected: af runs Codex with `--ephemeral`, which keeps those
  files off the host, and reading a Provider's private state would widen what af touches for a
  figure the Provider chose not to report.

## Consequences

- A budget no longer counts these Attempts. What bounds repeated failures without a usage report
  is the Attempt limits — the node's `max_attempts`, the Task's and the verification reserve's
  Attempt counts — and the wall-clock deadline. A Provider that bills without reporting is
  under-counted; the report says so by showing `(+N unknown)` beside the total.
- The reservation still bounds an Attempt while it runs. Only the settlement changes.
- A Task that ran under earlier releases keeps the charges it recorded, reservation-priced ones
  included; only new settlements carry the marker.
- A late usage observation for an Attempt that settled as unknown raises its charge as before,
  and reporting then counts that Attempt as known.
