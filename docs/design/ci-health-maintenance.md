# Daily CI health maintenance

**Status:** Phase 1 has an inactive production reader, hermetic acceptance fixtures and
[operator guide](../ci-health-maintenance.md). Phases 2 and 3 remain proposed; no live authority,
Ant activation or systemd installation is accepted here.

## Problem

Nightly builds detect instability, and release gates expose it, but no owned loop classifies a
failure and carries it to a bounded remedy. A single transient macOS failure can therefore leave
a release candidate blocked until the owner inspects logs, reruns a job or opens a repair pull
request by hand. Nightly green-commit selection must remain detection only; it must not lower
release acceptance.

## Goal

Add one owner-bound daily Ant whose pinned Scenario observes main, open release pull requests and
the nightly receipt, classifies failures from exact evidence, performs narrowly authorized
remedies, and alerts the owner. It must leave ordinary pull-request review, human release merges,
Task acceptance and the quarantine evidence policy unchanged. The first deployment is read-only
apart from its own durable state and notifications.

## Existing components

The design composes existing mechanisms rather than adding a second workflow engine:

- the nightly systemd chain and its bounded GitHub transport, receipts and Telegram route;
- Scenario Runs and Ant admission from ADR-0274, with `af ant tick` driven by a systemd timer;
- the durable service, scoped-principal and bounded-state patterns of the issue-triage runner;
- the existing implementation Scenario and merge watcher for repair pull requests;
- `test-quarantine.py scan`, `confirm` and `apply` under ADR-0268;
- the scoped standing publication precedent of the performance-publication decision, not its
  performance-specific operation.

No coordinator step contains a model, shell, arbitrary command or workflow-defined environment.
Models run only inside pinned Tasks.

## Scenario

The native Scenario identifier is `maintenance-ci-health` (Scenario and bundle names do not
permit `/`). Its inputs are the exact repository identity, main ref, optional release-pull-request
selector, observation window and notification destination. Its pin names every Task plan, adapter
grant, decision owner and resource bound.

### Evidence contracts and routing

Phase 1 adds two versioned artifacts. `af.ci-health-snapshot/1` contains `repository`,
`captured_at`, `source_sha`, `release_pr` (number and head SHA, or null), `nightly_receipt`
(identity and conclusion, or null), `complete`, and a bounded `failures` array. Each failure
records `occurrence_id`, `signature_id`, `workflow_id`, `run_id`, `run_attempt`, `job_id`, `job_name`,
`platform`, `head_sha`, `conclusion`, `log_artifacts` (bounded evidence references, not inline
unbounded logs), `first_seen_at`, `last_seen_at`, `occurrence_count` and `release_blocking`.
A snapshot has at most 100 failures, 20 log references per failure and 64 KiB total evidence text;
excess or unreadable evidence sets `complete = false` and names the omission. A SHA, run attempt or
job identity missing from an observation is malformed evidence, not a passing or transient check.

`af.ci-diagnosis/1` contains `snapshot_id`, `occurrence_id`, `signature_id`, `source_sha`,
`category` (`transient`, `product_bug`, `test_bug`, `ci_infra` or `unknown`), `rationale`, and
bounded `evidence` references to the snapshot and source revision. A pinned light Task accepts the
exact snapshot artifact and returns exactly one diagnosis artifact per selected occurrence. The
Selection Requirements, diagnosis and all evidence use the selected failure's `head_sha` as
`source_sha`; the snapshot's main SHA is not the source revision of a distinct release job.
The Task receives that failed revision in its captured Selection before Provider admission.
These existing identity fields retain their version 1 shapes. The Task cannot mutate GitHub or
the repository. Schema violations, mismatched identities, references
outside the snapshot or an empty evidence list refuse the result and select `unknown` only through
an explicit deterministic malformed-evidence record, never through model text.

The Scenario binds the snapshot through an existing typed Task input. Because native
`task_output_id` projects an artifact identity, not an arbitrary field inside it, phase 1 also
adds a native `task_output_field` projection for an admitted structured Task output. Its initial
allowlist is the scalar `category` and `signature_id` fields of `af.ci-diagnosis/1`; it refuses
other schemas, fields, missing values and non-scalar values. Scenario `equals` conditions compare
the projected category with literals and the signature with the selected occurrence.
The accepted-output projection remains closed to those fields. The explicit `af/scenario@6`
`ci_diagnosis` Task specialization additionally settles refused output as an
`af/scenario-ci-diagnosis@2` native observation. It retains the original refused Task receipt,
exact captured selection and bounded native diagnostic evidence beside the deterministic unknown;
it never creates accepted diagnosis output. Native `ci_diagnosis_field` category/signature
projections and boolean `ci_diagnosis_refused` read that recorded settlement. Each remedy
notification consumes the settlement as well as its category. Refusal selects alert-only, sends
diagnosis, remedy and outcome, and skips Watch. Findings are sent before diagnosis using
`af.ci-health-notification/5`: every phase binds repository identity through a canonical digest
in `repository_id` and the transport subject, restored by the reader in durable records. Bounded batches retain every
observed failure's workflow/run/job identity, job name, platform, conclusion, failed head, log
references and release-blocking flag.
Each release-blocking occurrence has an explicit immediate `release_alerts` entry even when
another failure is selected or the diagnosis reservation is exhausted. Each batch also carries
`deterministic_results`: the exact diagnoses and first matching report-only remedy rows and
intentions for all deterministically classified occurrences. Summaries and results can be batched
separately; findings omit the repeated single `deterministic_diagnosis` field and inherit their
repository from the snapshot bound by the native request's subject digest. The reader restores repository identity in
the durable notification and refuses a conflicting body value. Only the first unresolved
occurrence reserves a Task; later unresolved occurrences carry deterministic unknowns with an
exhausted-reservation rationale and alert-only intent. Batching includes these records in the
existing serialized request bound and preserves the Scenario's dispatch limit. Notification
versions 1–4 are refused. Plan authority, Task acceptance, reservations,
accounting and cancellation retain their ordinary native checks.

Notifications that consume a native diagnosis settlement use
`af/scenario-ci-notification-evidence@2` in the adapter request's text `diagnosis_settlement`
input. It carries bounded exact Task, result, selection, output and diagnostic references, the
canonical full-settlement digest and an explicit marker for details retained in the native Run.
Before encoding, the kernel checks the observation's subject against the notification repository.
The transport subject carries the canonical repository digest, verified against the adapter's
pinned snapshot; durable notifications restore the full repository. The original observation stays
in the Run and intent inputs, and the intent digest continues to bind those original inputs.
Notification version 5 references previously delivered findings details in lifecycle bodies
and retains the native evidence projection. The installed reader restores those details; prior
evidence and notification versions fail closed. Complete serialized requests retain the existing
65,536-byte bound without copying potentially large receipt results or diagnostic prefixes.

### Failure identity

`occurrence_id` is the SHA-256 of the canonical tuple `(repository, workflow_id, run_id,
run_attempt, job_id, head_sha)`. It identifies one observed job attempt. `signature_id` is the
SHA-256 of `(schema_version, repository, workflow_file, job_name, platform, normalized_fingerprint)`.
The fingerprint is the first deterministic failing test identity for a test failure, otherwise the
first normalized error line selected by a versioned adapter rule. Normalization removes timestamps,
durations, temporary paths, SHAs, run/job numbers, ANSI escapes and trailing whitespace; it never
removes test names, operating system, workflow or assertion text. An observation without a
fingerprint has category `unknown` and a unique occurrence-only signature, so unrelated unknowns
cannot share a remedy allowance.

The observer persists signature state keyed by repository and signature ID. Recurrence counts
distinct `occurrence_id`s on distinct run attempts; retries of one attempt do not increment it.
State expires after 14 UTC days without an observation. A green conclusion for the same workflow,
job, platform and head SHA closes the active signature and resets its transient and repair
counters. A different head SHA starts a new occurrence but retains cross-occurrence history for 14
days. These rules, not model judgement, control deduplication and one-open-PR limits.
Version 2 of the observer journal metadata atomically retains the latest service-evidence digest
and its exact snapshot with the recurrence folds. Unchanged evidence replays the original
pre-capture evaluation, including per-job recurrence counts; changed evidence is previewed on a
private journal copy and must satisfy the exact snapshot condition before any durable update.
The bounded `af.ci-health-state/2` record replaces version 1 without a migration reader.

### Remedy precedence

The deterministic classifier handles every occurrence, including infrastructure and exact
known-transient signatures, before selecting any diagnosis Task. Phase 1 selects the first
unresolved occurrence for its single Task reservation; only additional unresolved occurrences
are reported as reservation-exhausted. Exactly the first
matching row applies; later rows cannot add an effect:

| Order | Condition | Effect |
| --- | --- | --- |
| 1 | snapshot incomplete, malformed evidence, unknown signature or category, or exhausted bound | alert only |
| 2 | `release_blocking = true` | alert immediately; no automated rerun, repair or quarantine |
| 3 | `ci_infra` | infrastructure alert only |
| 4 | `transient` and no rerun reservation for this signature on the current UTC date | reserve and dispatch one scoped rerun |
| 5 | `test_bug` with at least two distinct eligible af Task gate occurrences in seven days | run the reviewed quarantine-evidence path |
| 6 | `product_bug` or `test_bug` with a declared small-repair path and no open PR for the signature | run one repair Task and publication path |
| 7 | anything else | report only |

Rows 2 and 4 make release-blocking take precedence over transient. Row 5 makes eligible test
evidence take precedence over a repair PR. No row both reruns and repairs one occurrence.

A Run proceeds through these steps:

1. **Observe.** The installed `ci_observe_v1` adapter performs bounded read-only GitHub requests,
   reads the nightly publish receipt and emits the exact health-snapshot artifact. Missing or
   incomplete evidence remains unknown, never healthy.
   The native Wait binds the selected snapshot content digest in
   `ci_observe_v1:sha256:<hex>`. Owner-only `af.ci-observe-occurrence/1` selectors provide that
   occurrence's window, nightly receipt and release PR under one pinned executable and scope;
   changed evidence at unchanged main refuses the Wait before diagnosis or notifications.
   Production capture bounds the complete serialized adapter response, including nested JSON
   escaping and the final newline, before committing the journal. Excess evidence produces an
   incomplete snapshot with a named omission and retained findings, within the same 64 KiB bound.
   [The production guide](../ci-health-maintenance.md#production-observation-staged-only)
   specifies selector staging, signed log-download scope and supported failing-test records.

2. **Diagnose.** Only occurrences reaching the Task from the precedence rules enter diagnosis.
   The Task emits the versioned diagnosis artifact described above.
3. **Notify findings.** A bounded notification Action sends observations, refusals and intended
   remedies before any rerun, publication or potentially long merge wait. It carries the Scenario
   Run and snapshot identities. Later Actions send separate lifecycle updates.
4. **Remedy.** The typed category projections and ordered table select at most one remedy per
   failure signature. Unknown, over-budget and release-blocking cases have no mutation step.
5. **Report outcome.** A second notification records reruns, opened pull requests and escalations.
6. **Watch.** Existing merge-watching follows only pull requests opened by this Run, until merge,
   closure or a bounded deadline. Pull-request checks and Scenario completion remain separate
   facts; a watch timeout cannot retract an earlier alert.

### Scoped rerun and recovery

Phase 2 installs a dedicated `rerun_failed_jobs_v1` Action operation and exact machine grant for
one repository destination. Its text `subject` is the repository, `revision` the observed head SHA,
and `body` a canonical `af.ci-rerun-request/1` document containing only `workflow_id`, `run_id`,
`run_attempt`, `job_ids`, `occurrence_id`, `signature_id` and `observed_failed_at`. The adapter
re-reads the run and jobs immediately before dispatch. It refuses a different head SHA, a missing
observed attempt, a job not failed on that attempt, an already-running newer attempt, or any job
outside the declared observation. It reruns only failed jobs through GitHub's failed-jobs rerun
endpoint; it never reruns successful jobs or an entire release workflow.

Before `execute`, the observer journal atomically reserves `(repository, signature_id, UTC date)`.
The Action's durable intent ID and exact input digest bind that reservation. The reservation is
spent before the POST and is never returned on timeout, truncated output, lost connection or
`pending`. Reconciliation queries by exact run and attempt: a newer attempt with the adapter's
expected actor is positive completion evidence; authoritative absence of any newer attempt and of
an accepted rerun receipt is `absent`; anything else retains `pending` and forbids another POST.
A concurrent human rerun is recorded as an externally satisfied remedy, not retried. Crossing a
UTC date while uncertain does not release the old reservation or authorize a second attempt for
the same occurrence. Hermetic fixtures cover lost response, adapter crash after dispatch,
concurrent human rerun, stale head, already-green job and the UTC boundary.

### Independent liveness observer

A separate `af-maintenance-watchdog` systemd service and timer, not the Scenario's Report step,
owns missed-occurrence alerts. It has no model and only bounded read access to the Ant journal and
Run receipts plus the notification grant. The owner configuration records the expected daily
occurrence and a two-hour grace deadline. After that deadline the watchdog requires an admitted
Run receipt or a durable skip reason; absence produces one `missed_occurrence` alert per expected
date. A Run without a terminal or still-active receipt after its pinned wall bound produces
`run_stalled`; a failed Run before its outcome notification produces `run_failed_before_report`.
Deduplication is keyed by `(Ant identity, expected occurrence, alert kind)`. Ant pause and disable
suppress future expected occurrences only after their durable state is visible; they do not erase
an already due alert. The watchdog's own missed timer is covered by the nightly chain's independent
service-health alert, rather than by itself.

Watchdog fixtures cover no tick, admission skipped with and without a durable reason, Run failure
before Report, and a PR still open across the next daily heartbeat. Scenario notification fixtures
prove that findings and remedy outcomes are sent before Watch and that a merge wait does not delay
the next daily heartbeat.

## Quarantine boundary

The Ant never hand-edits `.config/test-quarantine.json`, marks a test quarantined by model
judgement, or bypasses `test-classes.py` guards. Its diagnosis and repair Tasks run through the
normal af gate and thereby create durable `gate.flaky` evidence. Only the existing `scan --write`
may propose an entry after the required distinct-Task evidence exists. `confirm` and `apply`
remain separate owner-controlled steps. A platform-specific flake requires evidence from that
platform; a clean Linux confirmation does not settle macOS-only evidence. If the evidence bar is
not met, the Scenario reports that fact instead of manufacturing quarantine.

## Repair and publication authority

Repair Tasks produce candidates under existing Task acceptance. They never push to main, merge,
tag, release, edit branch protection or alter required checks. A new ADR must authorize any
unattended publication before phase 3: a standing owner scope limited to a dedicated GitHub
principal, a deterministic `af-maintenance/` branch prefix, exact repository destination, declared
path allowance, one open pull request per failure signature, and ordinary human merge. Generic
`publish_pr` decisions remain required until that ADR is accepted. Quarantine paths are publishable
only as the exact output of the reviewed scan path.

The maintenance token is distinct from nightly publish and owner credentials. It has checks read,
bounded Actions rerun, issues/pull-requests write, and contents write only for the scoped branch
prefix. It has no administration permission. Credentials are delivered through the service
manager's private credential mechanism and never enter Scenario inputs, logs or Task text.

## Ant and resource policy

The Ant runs daily in a pinned timezone after the nightly publish unit. Overlap skips the new
occurrence with a durable reason. After downtime, admission runs at most one missed occurrence
and skips the rest. Per-Run and rolling token, dispatch, wall and GitHub-request limits are part
of the pin; unknown charges retain their reservations. Pause and disable stop future launches,
not an active Run; cancellation stops future dispatch and never claims rollback.

A systemd timer calls `af ant tick`; it is not an always-running model or scheduler inside af.
Installation and the first live tick remain explicit operator actions.

## Rollout and acceptance

1. **Phase 1:** observe, diagnose and report only. The only external effect is the scoped
   notification. Deliverables include both artifact schemas and the native `task_output_field`
   extension. Acceptance uses hermetic GitHub and receipt fixtures for healthy, failing,
   missing-evidence and malformed-projection cases; equivalent normalized logs share a signature,
   distinct platforms and workflows never do, repeated attempts do not inflate recurrence, green
   closure resets state, and every overlapping pair of remedy predicates selects exactly the first
   table row. Independent-watchdog fixtures cover no tick, skipped admission and failure before
   Report. No paid live model evidence is claimed.
2. **Phase 2:** add the single bounded rerun and escalation. Acceptance proves rerun deduplication,
   request bounds, exact-attempt preconditions, principal checks, reservation retention across
   uncertain recovery, concurrent-human-rerun reconciliation, UTC-boundary behavior and
   transition-only alerts.
3. **Phase 3:** after the publication ADR, add repair and quarantine-evidence pull requests.
   Acceptance proves exact pins and path scope, no direct main/tag writes, ordinary gates and human
   merge, quarantine-evidence refusal below the bar, and no automatic `confirm` or `apply`.

Every phase defaults to dry-run in development. Live activation requires owner review of the exact
Scenario and machine-policy pin. This note does not activate an Ant, register a principal, widen
an adapter or schedule a timer.
