# ADR-0196: Recover an Attempt from a transient Provider failure and a reply that does not parse

**Status:** accepted (2026-10-22).

## Context

Across 414 recorded af runs on afactory itself, 22 Tasks (9.3M tokens) died on one malformed
reviewer reply (``Invalid typed Worker reply: expected `,` or `]` at line 1 column 1042``) with
no second chance, and 18 more (4.4M) on Provider failures that pass by themselves: a Codex
Worker that exited 1 with no output, a network request that timed out, a model at capacity, an
admission probe that timed out. Three ended on the identity recheck before a send, because a
15-second status probe on a busy host — or a Claude CLI that updated itself between the plan
and the review — was taken for an account change. A safety-classifier refusal of the 196-byte
admission probe ended three more, once after a 29-minute gate, and af redacted the whole native
capture because the refusal linked `platform.claude.com`. Report Tasks lost whole author
Attempts to shape checks that ran only after sealing and fed the author nothing (issues #283,
#295, #289, #33, #167).

Every one of these is a failure of one call inside an Attempt that still had wall, tokens and a
plan. Failing the Attempt spends its reservation and, for a slot with one Attempt, the Task.

## Decision

1. **A typed reply that does not parse is read tolerantly, then re-asked once.** The strict
   reply validator stays the authority. When it refuses, the first balanced JSON object of a
   reply that carries prose around it is tried; failing that, the same Worker is asked once
   more within the same Attempt: the exact captured request plus one feedback value,
   `af/WorkerReplyReask@1`, quoting the parser's message, its line and column and at most 512
   characters of the reply around them. The re-ask spends the same Attempt — its remaining wall,
   its reservation, its cancellation — and is not sent when the first call's usage is not
   completely reported or already reaches the reservation. Both replies and the re-ask request
   are kept as CAS blobs among the Attempt's raw evidence and named in the diagnostic
   (`raw reply kept: <digest>`). A second failure fails the Attempt as before.
2. **The reply contract is the Codex output schema.** One derivation in
   `review_runner::task::reply_schema` builds a typed request's native schema from the request's
   own captured output schemas: the ADR-0103 envelope for Claude's inline `--json-schema`, and
   for Codex its strict projection, written to the Attempt's staging directory and passed as
   `--output-schema`. Codex enforces structured outputs strictly, so the projection names every
   property required (an optional one becomes nullable), every port required (an unused one is
   an empty array), and leaves the keywords a strict facility does not take — `allOf`, `if`,
   `not`, string and number bounds — to the kernel; the nulls and empty ports it made the model
   write are removed before the kernel judges the reply, and the `-o` file is kept as written.
   A contract with no strict projection (an open map, a reference, a boolean schema) runs
   without a schema, as before. `--output-schema` joins the options `codex exec --help` must
   declare (ADR-0146).
3. **A transient Provider failure is called again inside the Attempt.** An adapter classifies a
   call that ended without a reply as transient when it failed for the network, a model at
   capacity, a timeout, or an exit 1 that wrote nothing and nothing af recognizes. The runtime
   then waits and calls the host again within the same Attempt: the wait the Provider named
   (`Retry-After`, `try again in`), else 60 s doubling, at most three retries and ten minutes of
   waiting, whichever comes first, and only while the Attempt's wall outlasts the wait and its
   reservation is not spent. Each wait is recorded before it begins; SIGINT and SIGTERM end it.
   The calls' usage adds; a failed call that reported none adds nothing (ADR-0143); the later
   call's capture leads the raw evidence. A failure that outlasts the retries fails the Attempt
   with its class and the retry history in its diagnostic.
4. **What an Attempt did about its Provider is a typed event.** `af/TaskProviderEvent@1` is its
   own event kind of the Task's log, beside `TaskTransition@5` and `TaskExecutionStarted@1`,
   whose shapes it leaves unchanged (ADR-0145): a `retry_wait`, a `reply_reasked`, an
   `identity_reprobed`, an `identity_refreshed`, an `admission_reprobed`. It names the lease
   that wrote it, the Attempt and its node, references no artifact, and the Store refuses it
   for an Attempt that is not started and unsettled. `af task show --json` lists them as
   `provider_events`; `af task show` prints `NODE: waiting 120 s for capacity (retry 2 of 3)`
   while a wait lasts.
5. **The identity recheck never takes silence for an account change.** A probe that times out
   or answers a line af cannot read is sent again, three probes in all within the Attempt's
   wall; then the call fails with the closed class `environment` and the cause (`identity
   recheck: probe timed out 3× … the account was not verified as changed`). Only another
   account, authentication method or organization fails it as a change (``identity recheck: the
   Claude account differs from the one captured at plan: run `af task plan` again``); Claude
   Code's `claude.ai` and `oauth` are one method. When the executable file changed since
   capture, its `--version` is read again: a new version with the same account passes, the
   capture follows it, and the update is one `identity_refreshed` event.
6. **Every Provider admission is decided before any other node starts.** The scheduler runs a
   Task's admissions first and starts nothing else until all have completed; when one fails,
   every other node is suppressed, so no implementer or gate runs for a reviewer or evaluator
   that will be refused. A safety-classifier refusal of the admission probe itself is answered
   once, in the same Attempt, with an alternate wording; the refusal category the stream's
   `stop_details` names is the event's and the hint's. Provider documentation hosts
   (`platform.claude.com`, `docs.claude.com`, `docs.anthropic.com`, `platform.openai.com`) are
   no reason to redact a capture; their login, OAuth, device and key paths, and a code in any
   URL, still are.
7. **A report draft is validated for shape before it is sealed.** The report domain judges an
   author's reply — exactly one draft, a draft its version admits (citation sets sorted and
   unique), every required section present by its exact heading — into
   `af/ReportDraftValidation@1`. A failed validation refuses the reply; the Attempt's retry
   feedback references the receipt, and the retried author's request carries it beside the
   feedback. The author's instructions end with the policy, verbatim: its required sections,
   `max_document_bytes` and citation rules. The report's size and citations stay the report
   check's after sealing (ADR-0133), so a bad citation is still a failed check that names it.
8. **Reviewer and evaluator slots of the shipped pipelines get two Attempts.** A reply that does
   not parse twice in one Attempt still has a second Attempt with typed retry feedback (issue
   #283); the packages are re-pinned with `af catalog digest`.
9. **`af task show` names each failed node's class and first line.** The class is the closed one
   af wrote into the diagnostic (`capacity`, `invalid_reply`, `environment`, …), else the run
   report's; a node that failed only because the one it follows produced no receipt is a
   `consequence`, and its line names that node.

## Consequences

The 9.3M tokens of the reviewer replies and the 4.4M of the transient failures above would have
cost a re-ask or a wait, inside the same reservations. An Attempt may now take longer than one
call: up to ten minutes of waiting, all inside its wall. Its charge is the sum of its calls;
nothing is reset or refunded. A refused admission no longer lets the implementer and the gate
run first, so a Task whose reviewer cannot run ends before its first paid business Worker.

Not done here: a sealed and checked report whose verifier missed the plan deadline still cannot
be verified in a later revision without a new author Attempt (issue #166). That needs evidence
of one plan admitted under another, which neither the Store nor the report domain grants; it
remains open.

Tests: `review-runner` re-ask and transport tests, the Codex output-schema and transient-ending
tests, `review-pipeline` `task_runtime::resilience`, the scheduler's first-node test, the
identity recheck unit tests, the report draft validation tests, the schema parity tests of the
three new shapes, and `af`'s `task_file::attempt_resilience`.
