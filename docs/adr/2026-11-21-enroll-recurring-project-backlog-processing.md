# ADR-2026-11-21-enroll-recurring-project-backlog-processing: Enroll recurring Project backlog processing

**Status:** proposed (2026-11-21).

## Context

The [Project batch journal](2026-11-20-freeze-project-backlog-at-authority-cutoff.md)
already owns the intake high-water mark, timezone, cursor, overlap policy and rolling
reservations. The [portable backlog Scenario](2026-11-20-prepare-private-daily-backlog-assessments.md)
requires an exact batch reference and independent review. An Ant bound to one prepared
Scenario cannot process subsequent cutoffs with that same pin.

## Considered options

- Register a fresh Ant per day. This requires a person or external script to recreate daily
  authority and leaves scheduling and settlement recovery split across several commands.
- Admit a generic Ant first and then copy its receipt to the Project Store. A crash could
  advance the Ant without recording membership in the authority's Project Store.
- Let the native scheduler drive recurring Project enrollment. Keep occurrence admission
  and intake freeze in the same transaction and reuse the existing Scenario and Task contracts
  after admission.

## Decision

`af ant backlog validate` captures the static portable Task closure and prints the
receiver-local `af/backlog-schedule@3` scope. `enroll` confirms its exact hash and installs
one owner-only record per Project under the Project Store lock. It binds the Scenario Store,
checkout, source anchor and optional reviewed remote-tracking ref, reviewed portable artifact
and native Task closure, local Provider registry and optional bindings, intake authority and
policy, batch policy and timezone, destination and limits. Machine paths belong to the receiver's record.

The foreground scheduler and `af ant tick` find enrollments for the selected Store. They
validate live authority before using the Project journal's shared Ant calendar and admission
rules. A new cutoff can follow a source descendant of the enrollment anchor when HEAD matches
the confirmed remote-tracking ref and the captured Task closure is unchanged. The journal
atomically freezes membership, the committed intake cutoff, the exact source commit and the
enrollment revision. Pause suppresses admission while leaving existing work recoverable. Resuming consumes due paused
slots before enabling admission. The journal uses `BacklogBatchStored@6` with its recorded
pause state and admission mode, replacing the prior journal version.

The scheduler prepares the existing backlog Scenario from the recorded cutoff and source and
admits one Run under a deterministic batch-derived ID. The enrolled scope permits substituting
the frozen source and batch input into the reviewed template, with an exact fresh template grant
and no other machine grants. The native template records the enrollment revision. The Run and
native Task retain separate pins and receipts. On restart the scheduler resumes an existing
Run and its uncertain intents or prepares from the original frozen source. A terminal
failure with a batch still open requires explicit recovery. Native independent verification,
Project settlement, publication authority and adapter reconciliation remain mandatory.
A receiver can explicitly admit a manual batch while enrollment is paused. That batch
settles through a nonreserved manual Run with its own optional provider observation pin,
including after the enrollment resumes. A recurring batch keeps its enrolled authority.

## Consequences

A receiver can run successive daily batches after one enrollment. Changed authority blocks
dispatch, and intentional scope replacement requires a new confirmation after pausing and
settling outstanding work. Old batch journals fail closed under the current writer. The Project
audit and Scenario Run views keep their existing formats.
