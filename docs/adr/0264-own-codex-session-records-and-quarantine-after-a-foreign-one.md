# ADR-0264: Own Codex session records and quarantine after a foreign one

**Status:** proposed (2026-10-29).

## Context

`af self optimize` captures declared provider history into `OptimizationHistory@1`. The `codex`
adapter read every record through one shared identity step: any `payload.id` or `session_id` it
found replaced the session, reset project membership, model and effort, and any `cwd` or
`project_id` reset membership. Current Codex rollout JSONL breaks that step in three ways:

- `response_item.payload.id` is an item ID, not a session ID, so each message or tool item
  reset the declared session and dropped the usage that followed it as `project_identity_unknown`;
- `token_usage_record` names its session explicitly (`payload.session_id`, `payload.turn_id`) and
  carries the whole-session cumulative `payload.thread_token_usage` beside the overlapping
  per-turn `usage` and `turn_token_usage`;
- `token_count`, `turn_context` and the `event_msg` lifecycle (`task_started`, `task_complete`,
  `turn_aborted`) name no session at all.

A source that interleaves another session's records, or whose `turn_context` leaves the project,
could therefore redeclare the session, relabel its model, add a foreign cumulative total under the
declared session's key, or move the start of an open task span.

## Considered options

1. Keep the shared identity step and skip `response_item`. Rejected: a foreign `session_meta` or
   `token_usage_record` still redeclares the session, and the unscoped records after it are
   counted as the declared session's.
2. Drop every unscoped record and count only `token_usage_record`. Rejected: lifecycle spans and
   the `token_count` snapshots of a session that writes no usage record would be lost.
3. One declared identity per source, ownership checked before project membership, and a
   quarantine of unscoped records after a refused foreign record. Chosen.

## Decision

- The first `session_meta` whose `payload.cwd` and `payload.project_id`, where present, match
  the project declares the source's one session; model and effort come from it or from a project
  `turn_context`. A foreign `session_meta` before any declaration is excluded as before
  (`excluded_foreign_project`). `response_item` records are never identity or economics input.
- After the declaration, a `session_meta` with another ID, a `token_usage_record` naming another
  session or none, and a `turn_context` whose `cwd` leaves the project are refused foreign
  records: they change no declared identity, model, effort, project membership, counter or open
  task span, they record `foreign_session_record`, and they quarantine the source.
- A `token_usage_record` is checked against the declared session before project membership is
  consulted. While the source is quarantined, every record that names no session is refused with
  `quarantined_unscoped_record`, including a `turn_context` back in the project. A repeated
  declared `session_meta` confirms the identity and clears nothing; only a `token_usage_record`
  of the declared session whose `thread_token_usage` is exact restores scope.
- `thread_token_usage` and `token_count.info.total_token_usage` are snapshots of one total: both
  are observed under the session's cumulative key, so the economics projection keeps each
  counter's maximum. `usage`, `turn_token_usage` and `last_token_usage` are not read.
- A task span opens at the first `task_started` of a turn ID and closes once, at that turn's
  `task_complete` (incomplete outcome) or `turn_aborted` (abandoned outcome); a repeated start
  never moves it, and no outcome is ever verified acceptance.
- A range continued from a cursor starts after the retained `session_meta`; it adopts the first
  session a `token_usage_record` names and records `session_identity_unverified`.
- Each of these gaps makes the source receipt `partial`. The economics allowlist is unchanged:
  no message, tool, reasoning, header, environment or credential content is copied.

Amended (2026-10-29): a `session_meta` with the declared ID and a foreign `cwd` or `project_id`
is a refused foreign record that quarantines the source too. Adoption by a `token_usage_record`
is limited to a range continued from a cursor whose retained prefix could not be replayed: a range
read from byte 0 declares its session only through `session_meta`. A continued Codex range
replays the retained prefix below its cursor through the adapter while
`verify_retained_ranges` rechecks its hashes, so the declared identity, project membership, open
task spans and quarantine carry over the cursor within the existing raw-read allowance and no
checkpoint is persisted. This supersedes the bullet on continued ranges above and the
consequence that an incremental capture of a source writing `token_usage_record` is partial.

Amended again (2026-10-29): on a range read from byte 0, a project `turn_context` before the
declaring `session_meta` supplies project evidence and labels only: a `token_count` or task
lifecycle record before that declaration emits no counter, outcome or span and records
`session_identity_unknown`. A `token_usage_record` is refused as a foreign record when any
present `payload.project_id` or `payload.cwd` is foreign, even when it names the declared
session: it changes no counter, label or membership, cannot restore scope, and quarantines the
source, as it also does in a range continued from a cursor, where it is never adopted.
A present `payload.project_id` or `payload.cwd` that is not a string (`null`, a number, an
object, an array, a boolean) is foreign evidence too, on every Codex record that is checked
against the project: a malformed field cannot restore scope or let a following unscoped count
in.

No persisted or printed shape changes: the gaps are names in the existing
`OptimizationHistory@1` gap set and the receipt's `adapter_version` stays `native-v1`.

## Consequences

- A Codex session with interleaved foreign records reports exact declared totals and stays
  visibly partial instead of silently mixing sessions.
- Unscoped records between a foreign record and the next declared `token_usage_record` are lost
  from the report; a session that writes no `token_usage_record` stays quarantined to its end.
- An incremental capture of a source that writes `token_usage_record` is partial, because its
  continued range cannot see the `session_meta` that declared the session.
- The regressions are the `codex_*` unit tests in
  `crates/af/src/self_optimizer/optimization_adapters.rs`, classified functional by exact entries
  in `.config/test-classes.json` while the rest of `self_optimizer::` stays perf.
