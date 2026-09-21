# ADR-0051 — Compile fixed implementation Tasks into the common runtime

**Status:** accepted for the unreleased Task increment, 2026-09-11. Superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): readers for the original implementation history
and delivery schemas, and the v0.8.0 compatibility fixture.

## Decision

New `af task start --goal ...` executions compile the pinned fixed implementation v1 format
into the same public Pipeline, captured Worker contracts, common Store and runtime used by
Task files. The CLI no longer dispatches Workers or writes new implementation executions to
`tasks.sqlite`. Captured compatibility packages retain the original pipeline, lock, project
and Worker identities. Their generated pins use the existing package digest implementation.
This installed version adapter is deterministic and does not call a Planner.

The fixed Pipeline becomes implement → seal → check → independent evaluation → acceptance.
Each Worker and the aggregate check operator use the common Attempt path. Command Workers
consume zero model tokens; the check and evaluator Attempts and their wall allowance remain
reserved before implementation. The old format bounded individual processes, without an
overall deadline; the adapter adds those bounds and a 60-second capture/admission allowance.
`check_process_wall_ms` preserves each old Check's own deadline within the aggregate Attempt;
one slow check cannot borrow another check's per-process allowance.

An explicit `legacy_task_command` transport preserves the original implementation text and
evaluation verdict protocols. Translation occurs once at the transport boundary. The resulting
typed ports undergo the same schema, provenance and Snapshot validation as native Task replies.
The transport version is part of the captured Worker contract identity. Bounded retrieval of
current/source Manifest metadata and check receipts reconstructs evaluator input and is recorded
in its context manifest. No implementer transcript is added.

Fixed v1 native model packages must migrate to explicit Task catalog Model bindings. New-format
Model execution already admits canonical Provider identities and charged capability probes;
the old ambient credential and caller-owned security flags cannot bypass that boundary.

New executions return `af/task-inspection@2`, with the Task result, history, Attempt records,
usage and diagnostics. The new Snapshot format is `af.task-snapshot/1`. Existing history and
delivery readers continue to recognize their original persisted schemas. New results retain
the same explicit confirmation and absent-target rules for local delivery.

Delivery recovery closes a rolled-back prepared operation with a failed receipt before
preparing its retry. This makes the recovery path valid under the common journal's transition
checks and preserves the original local rollback and operator-content protections.

## Compatibility evidence

All eleven original implementation/delivery behavior cases execute against the common path.
Their assertions now read the new result and journal; recovery fault fixtures preserve dense
event sequences and writer release. They include ignored files, an empty index, partial
materialization, operator edits/staging, failed creation, exact repeat and dirty-source refusal.

The original 0.8.0 regression source is preserved byte for byte at
`fixtures/compatibility/task-implement-v0.8.0.rs.fixture`, SHA-256
`21da1453f236d8f09d1ab4b15ce41274e8b785d5b260491d26d407a2ccee5cb7`.
It remains a compatibility reference; it is not presented as unchanged current test source.
Historical Store artifacts and synthetic fixtures are not rewritten.

Native Task transport regressions separately prove that timeout and CAS failure refuse the
business result while retaining reported usage. Arithmetic overflow saturates the reported
counter rather than discarding an overrun or panicking. Raw stdout/stderr remain available
when CAS storage succeeds. Legacy review capture retains its existing interface and framing.
