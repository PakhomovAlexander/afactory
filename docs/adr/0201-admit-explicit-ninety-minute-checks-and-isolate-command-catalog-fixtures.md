# ADR-0201: Admit explicit ninety-minute checks and isolate Command catalog fixtures

**Status:** proposed (2026-10-23).

## Context

The operator approved a 90-minute whole check Attempt, but policy validation refused values
above one hour without an explicit per-process cap. Three catalog tests copy production
authority and bind deterministic Command Workers, which have no native model identity.
Production intentionally requires model diversity. Compile-time Provider fixture paths also
break the runtime workspace-root contract of warm check binaries.

## Decision

Admit positive `check_wall_ms` through `5400000` only in the branch without
`check_process_wall_ms`. Preserve the explicit per-process ceiling of `3600000` and its existing
aggregate rule, `max(3600000, per_process * checks)`. Keep ordinary configured defaults and all
check commands, assertions and budgets unchanged. The closed policy schema constrains the
absent-cap branch too; typed validation still checks cross-field consistency and multiplication.
No persisted field or typed shape changes.

Copied catalogs that replace Model Workers with deterministic Command fixtures explicitly
select package independence and disable principal, Provider-kind and model comparisons before
committing fixture authority. Assert that the production catalog still requires model diversity
and retains the authorized principal policy. This grants no production independence bypass.

Load Provider disable parity fixtures from `AF_WORKSPACE_ROOT`, with the established compile-time
manifest directory used only as a fallback. Preserve schema validation and long-ID coverage.
When a Project collection subprocess fails, include the raw exit status and Unix signal in its
assertion diagnostic without changing the asserted exit code or discovery limits.

The telemetry corpus failure reproduced with native macOS Python SQLite 3.54.0: after two
successful af status commands, the final af connection had checkpointed and removed WAL/SHM.
Python's `mode=ro` connection opened, but its first query returned `SQLITE_CANTOPEN` (14), with
system errno `ENOENT` (2). The database was readable through a writable connection that could
reconstruct sidecars. Reading it as immutable also worked, but would ignore a retained WAL.

Read corpus facts from a private copy of the database and any retained WAL, under the index's
existing lifetime lock while copying. Reconstruct shared memory only in private scratch and
enable `query_only` for fact queries. Never change the measured database, create its sidecars,
or discard committed WAL rows. Keep this work outside measured command timing. Report SQLite
version, error code/name when the Python API exposes them, and database/WAL/SHM file details on
failure. Regression coverage includes the sidecar-free lifecycle, committed WAL rows, lock
contention, malformed input, query-only enforcement and unchanged source bytes and file set.

## Consequences

An explicitly configured 90-minute kernel check can be planned. Larger implicit check walls,
zero walls, oversized explicit process caps and inconsistent aggregate budgets remain refused.
Fixture-only policy matches its actual Workers while production keeps model diversity.
Required kernel verification still runs every existing check.
