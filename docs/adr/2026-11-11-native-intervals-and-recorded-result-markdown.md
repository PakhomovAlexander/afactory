# ADR-2026-11-11-native-intervals-and-recorded-result-markdown: Native intervals and recorded result Markdown

**Status:** proposed. **Date:** 2026-11-11.
Extends ADR-0274's daily admission and inspection; ADR-0145 remains binding.

## Context

Interval scheduling and presentation are separate capabilities. Neither grants execution authority,
collects new evidence on a refresh, installs services or changes a running recovery monitor.

## Considered options

- External sleep/tick wrappers: rejected as the required scheduler lifecycle would remain external.
- Completion-relative recurrence: rejected because work duration would move the intended cadence.
- Backlog replay or overlapping waiting Runs: rejected because either can multiply authorized work.
- A fixed fleet/Task table: rejected because output meanings and columns belong to Scenario authors.
- Model-written render summaries: rejected because rendering must faithfully project recorded data.
- Silent journal truncation or schema migrations: rejected because they erase accounting or preserve
  unsupported generations, contrary to the current native contracts.

## Decision

Ant definition, pin, state, list and tick move to generation 6; the Scenario Store moves to 7.
Unsupported earlier generations fail closed without migrations. Unchanged Scenario contracts remain
5. Daily schedules remain current functionality with an explicit `kind: daily`, pinned timezone,
first fold instant and skipped DST gaps. Intervals use `kind: interval`, positive integral `seconds`
through the JSON safe integer maximum and explicit `start_unix_ms`. Index zero is the anchor;
index `n` is `anchor + n * seconds * 1000`. Intervals never consult timezone/DST. The supported calendar
horizon remains MAX_TIME; intervals whose next instant exceeds it have no representable next due.

An immediate native transaction admits each `(Ant ID, exact pin, index)` once. The existing
`next_day` field is the daily local-day cursor or interval ordinal. Interval downtime records one
inclusive skipped range for all older unprocessed indexes and considers only the latest index.
Every range retains its first due time, through index and actual admission time. No missed queue
exists. Overlap is explicitly `skip`: waiting and uncertain Runs remain active, with one active Run
per Ant. Pause/disable preserve historical due modes and future suppression without cancelling work.
Clock rollback refuses admission until the clock reaches the retained high-water time; forward
jumps compact missed interval indexes. Restart reuses native intents and receipts, never guesses
whether an external effect occurred. Existing token, deadline and recovery authority stay unchanged.

`af ant scheduler run` explicitly owns one foreground OS lease per Store. `status` probes that
lease and reports heartbeat, stop request and last tick error. A dead process cannot leave an active
lease. `stop` requests graceful exit after current bounded dispatch, without signalling a guessed
PID or cancelling Runs. SIGINT/SIGTERM use AF's existing interrupt/supervision path. No installation,
detachment, autostart or machine configuration occurs. `--for-seconds` provides a finite lifecycle.
The driver polls at most once per second when idle; dispatch may delay subsequent polls. Timing is
best effort, including OS sleep, Store contention and native dispatch duration, never real time.
Inspection exposes scheduler availability, next due instant, active Run, admission receipts and
lateness; a heartbeat describes a last poll, not a promise that work is currently runnable.

A native named `text` result, or the recorded evidence of an `observation`/`action` result,
may explicitly contain `af/result-document@1`: an observation time,
freshness duration and ordered Markdown/table blocks. Table columns and rows are ordered arrays;
row width and duplicate columns are validated. Nested values render as canonical JSON; native numeric admission rejects unsafe integers, negative
zero and unsupported canonical number magnitudes (authors use strings for those). Structured
text is escaped, line breaks are explicit, and values are never silently truncated. Bounds are
32 blocks, 32 columns, 512 rows per table and the native 64 KiB text bound for the entire document.
Intentional Markdown is a separate block kind; terminal escape/control bytes are rejected there.

`scenario output` and `ant output` observe selected named results. Operational `show --json` retains
complete underlying native results; Ant `show` uses `af/ant-inspection@1`, retaining its state fields
and adding live inspection data. `af/result-snapshot@1` describes the rendered metadata: Ant,
occurrence, Run, port, content revision, observed/rendered times, producer status, partial/final,
empty, unavailable/error, and independent fresh/stale state. Refresh errors emit unavailable with
an error, never the old document under a fresh timestamp. A future observation time is an error.
An Ant selects its latest admitted Run, even if its output is unavailable; it does not silently
substitute an earlier successful Run.

`--watch --interval-seconds N` refreshes independently of scheduling and production. It never ticks,
retries, clears a hold or collects observations. Every streamed Markdown snapshot has explicit
begin/end timestamps; no terminal ANSI operations are used, including terminals. `--output PATH`
atomically replaces a complete synced document on the same filesystem, using a private temporary
file. Interruption leaves Ants and Runs intact. A transactionally derived keyed result index avoids
journal-history scans during refresh; it grants no authority and does not replace native receipts.

## Consequences

The existing private Store registration, ownership, machine Storage Budget and 16 MiB native journal
bound remain unchanged. Receipts, Runs and accounting are not silently removed to keep scheduling
alive. Exhaustion refuses new journal transitions. Global journal/dispatch errors stop the scheduler
with a nonzero exit and retained diagnostic, while Ant-specific refusals leave healthy bindings
progressing; operators must
retain/finish/discard Stores under native policy. The result index is counted as Store remainder;
its size is bounded by the same finite recorded Runs/results. These limits constrain long-running
high-frequency use; measurements must report duration, growth and limits without extrapolating a
production performance claim. Independent executable correctness and the unchanged local gate
remain necessary for acceptance. No Telegram integration, publication or production activation is
part of this decision.
