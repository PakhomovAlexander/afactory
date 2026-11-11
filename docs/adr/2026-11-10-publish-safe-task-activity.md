# ADR-2026-11-10-publish-safe-task-activity: Publish bounded Task activity metadata

**Status:** accepted (2026-11-10). Partially supersedes
[Task inspection](2026-11-10-inspect-task-attempts.md): its absence of a live channel.

## Context

Recorded Attempt transitions cannot tell an operator what a running Worker is doing. Native
stdout can contain credentials, arguments, prompts, output and private reasoning; streaming that
content into a terminal before whole-capture credential admission would break the existing gate.
The inspector already preserves recorded Attempt identities across polling and retries.

## Decision

1. The Task host may construct an optional metadata sink for an exact Task and admitted Attempt.
   The CLI supplies the Task Store location; neither the Worker request nor the Provider supplies
   a sidecar path or attribution. The current implementation enables this for ordinary Codex
   model Workers, including their bounded reply re-ask. Provider identity rechecks still precede
   native dispatch. Admission probes, Campaign reviewers, command Workers, Claude and Pi do not
   have this channel and are explicitly unavailable in the inspector.
2. The existing `review-process` pipe drain offers independently to bounded consumers. Activity
   uses a 256 KiB queue and a background consumer, independently of optional Telemetry. A full
   queue drops chunks and counts them; it cannot block the drain or affect its sibling observer,
   process deadline, cancellation, capture, accounting or credential screening.
3. Only the incremental projection of known Codex JSON notifications leaves that consumer.
   `item.started` and `item.completed` project `command_execution`, `file_change` and
   `mcp_tool_call` into closed operation and phase enums. A nonzero command exit or failed status
   projects `failed`; turn notifications project Worker started/completed/failed. Unknown data is
   discarded. No arbitrary Provider string, item ID, command, path, tool name, argument, output,
   prompt or reasoning enters an activity record. A frame is capped at 64 KiB; an oversized frame
   or an offset gap discards through the next newline. Overflow and truncated frames have closed
   metadata notices. An incomplete trailing frame is discarded with a truncation notice.
4. `af/task-activity/1` is a disposable sidecar document, with its schema in
   `schemas/task-activity-v1.json`, not a CAS artifact or durable execution event. A Task Store's
   `activity/` holds opaque Task/Attempt names. Each admitted Attempt channel is exactly 129 KiB:
   128 fixed 1 KiB record slots plus one head slot. Its writer updates a slot before publishing
   the head and wraps, retaining recent activity. Total channels are limited by admitted
   Attempts; the sidecars count as af-owned Store bytes and Task collection removes them. Files are
   private, and descriptor-relative opens below the Store refuse links and special files.
5. A reader validates the document version, exact Task/Attempt binding and monotonic sequence,
   reads only new slots, and retains at most 128 records. It refuses mismatched, malformed,
   oversized, truncated or concurrently changed data with an explicit error; a later poll can
   recover. Missing and unreadable channels remain explicit. Overwritten history is counted.
   No sidecar can settle an Attempt, satisfy acceptance, change execution authority or supply
   accounting. Missing activity never implies a stalled Worker.
6. The inspector polls activity on its own background reader, with one bounded snapshot in
   flight, about four times a second. Store inspection remains on its existing reader, about
   once a second. Switching selection discards the old reader without waiting for IO. The view
   shows captured Worker identity, latest observed operation and its age; after 15 seconds the
   age is marked stale, without inferring a stall. All terminal text uses the existing printable
   ASCII renderer. Raw Provider output continues through the existing completion-time credential
   gate; the inspector displays metadata and recorded artifact IDs, not a live raw transcript.
7. Enter on an Attempt opens activity following the bottom. Scrolling pauses, preserving the
   viewport through polling and retries and counting new notifications; `G` resumes following.
   If the ring overwrites a paused row, the view clamps to retained history and says how much
   was truncated. The metadata header remains visible at 100x30 and 80x24. `h`/Escape return focus
   left; Tab/BackTab switch focus. Page movement, artifact opening and Ctrl-C retain their
   existing meanings. Scrolling clamps to the viewport's height.

## Considered options

- Stream raw stdout and redact it line by line: rejected because whole-capture credential
  screening and reasoning privacy cannot be replaced by speculative text filtering.
- Display only Attempt state transitions: rejected because they do not describe native work.
- Make activity authoritative execution events: rejected because loss, partial reads and bounded
  retention are expected; execution already has its own canonical records.
- Keep an unbounded transcript: rejected because both disk retention and terminal memory need
  fixed limits. The ring instead makes overwritten history explicit.

## Consequences

Codex activity becomes visible while the native process remains alive, without releasing any
unadmitted content. Unsupported runners and missing, stale or lost observations are explicit.
Activity is best-effort and disposable; it grants no new Worker authority or publication right.
A future runner needs its own closed metadata projection and tests before enabling the channel.
