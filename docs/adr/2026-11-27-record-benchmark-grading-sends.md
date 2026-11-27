# ADR-2026-11-27-record-benchmark-grading-sends: Record external grading sends in the Task Store

Status: proposed

Date: 2026-11-27

## Context

External grading consumes a bounded allowance. A worker can lose the response after
a grader accepts a request. An operator needs enough evidence to tell which sends
consumed the allowance and which scheduled slots still lack a result.

## Options considered

Recording only completed responses loses the send on a crash. Retrying an ambiguous
send can spend the allowance twice. Adding a ledger on first use would mutate an old
Store and violate [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).

## Decision

Persist the grading intent before sending and retain its response and usage against
the same intent. Stop further admission on unknown usage or an allowance overshoot.
Keep scheduled slots visible in the report. The subject receives the case without the
grading oracle. Record the subject family and distinguish fixture evidence from live runs.

Initialize SQLite layout 6 with the send ledger. Validate it on every Store open.
The persisted event envelope remains RunEvent@5.

## Consequences

Reopening the Store preserves evidence for offline accounting. Ambiguous sends require
reconciliation. New binaries refuse layout 5 even for inspection. Inspect a historical
Store with its recorded release.
