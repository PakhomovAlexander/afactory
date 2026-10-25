# ADR-0216: Bind calibration dispatch to the checked main source and separate group arguments

**Status:** proposed (2026-10-24). Renumbered from unmerged ADR-0202 after
published main's ADR-0204. Repairs the calibration dispatch and group package of
[design §12 and §15](../design/continuous-performance.md), amending ADR-0182's unconditional
member run naming. ADR-0184's producer equality and ADR-0194's per-case group binding stand.
No persisted laboratory contract changes.

## Context

The combined `ID=CPU MODEL@IMAGE` parser split at the final `@`, truncating registry-pinned
images to `sha256:DIGEST` and appending the registry reference to the CPU. CPUs themselves
can contain `@`. Declarations therefore failed exact matching against real shard observations.
The deployed operator also checked main's workflow and re-read main before dispatching to
`ref: main`; main could move between that GET and POST. Its disabled dry-run returned before
any preflight, concealing whether configuration could pass the guards.

The frozen old cohort cannot be repaired by operator hash re-pinning. Its authority's workflow
and scheduler must remain the exact producer, and changing those bytes requires a new cohort
declared from the committed repair, after this Task.

## Considered options

- Split the combined group spelling at another `@`: rejected because both CPUs and images may
  contain it. Three separate CLI arguments preserve every byte without escaping conventions.
- Add another main GET or dispatch to a raw SHA: rejected as an atomicity claim; another GET
  still races and raw-SHA dispatch support is not assumed.
- Relax producer equality or rewrite the old cohort: rejected; this would change frozen
  authority or exclude failed/missing originals to manufacture readiness.
- Enable a timer temporarily for dry-run: rejected; an explicit dry-run can exercise preflight
  while configuration remains disabled, with no POST or dispatch intent.

## Decision

`--group ID 'CPU MODEL' 'NAME@sha256:DIGEST'` replaces the combined spelling, without an alias.
It validates a group ID, exact printable CPU model and complete pinned registry image. The
cohort shape and exact CPU/image tuple matching are unchanged.

The operator sends `dispatch_head`, the exact full main SHA whose producer bytes it checked.
Only a calibration workflow running at that SHA gets the cohort-member run name. Missing,
malformed or stale inputs instead yield `af-perf source-mismatch`, a non-member refusal before
checkout, planning, build or measurement. Full hex SHA case represents the same identity.
The scheduler independently refuses the mismatch. Other triggers, PR exemptions, read-only
permissions, action pins and workload semantics keep their existing rules.

The product entry point `scripts/performance/cohort-dispatch.py` retains the captured deployed
operator's `ROOT`, `LAB`, `main()` and `--dry-run` interface, installed-library verification,
bounds, locks, credentials separation, UTC cutoff, sealed-parameter gate and no retry/backfill.
A disabled explicit dry-run executes preflight, including authority workflow equality and
control scheduler-tree equality and the final main recheck, without intent or POST. This
also applies when an uncertain retained intent prevents member selection: a valid dry-run
returns that uncertainty only after all guards, and leaves the journal byte-identical. A failed
guard takes precedence over uncertainty. A normal disabled cycle stays disabled.
An enabled cycle durably fsyncs intent before one POST; an ambiguous response or a raced
non-member run leaves the intent latched for operator inspection.

## Consequences

The fake-API operator fixture exercises disabled preflight and guard refusals, enabled durable
intent, ambiguous responses, and main moving after the final GET before POST. It executes the
workflow's own refusal step and run-name expression, checks that no plan or measurement follows,
and retains the race in history with a latched intent. Group tests connect declarations to
observed shard matching, including a CPU containing `@`, exact image identity and malformed
input refusal. Installed-runtime and frozen-cohort tests protect the deployment boundary.
These tests establish orchestration behavior, not a hosted run or empirical readiness.

The [dispatcher runbook](../performance-cohort-dispatch.md) requires a separate new cohort
identity, refreshed history anchor, future start date and exact observed groups, declared only
from the committed repaired producer, with unchanged statistics and all old evidence preserved.
No replacement cohort, activation, publication, VM access or scheduler change occurs here.
Main's dispatch remains mutable: the source guard prevents membership and measurement on a
raced repaired workflow, and unchanged producer verification refuses future incompatible
workflow bytes even if those bytes change the guard. No atomic API guarantee is claimed.
