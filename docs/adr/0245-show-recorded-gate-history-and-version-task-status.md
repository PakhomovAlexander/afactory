# ADR-0245: Show recorded Gate history and version Task status

**Status:** proposed (2026-10-27).

## Context

A Task plan states its limits but has no observed duration to help an operator choose a wait.
The status document has only charged tokens, even though the execution ledger also records held
reservations, remaining capacity and Attempts whose usage was not reported. A wait can settle
while an operator is away from the terminal.

## Decision

`af task plan` and `af task status` may show the latest check span recorded in their Store when
the check name, captured code policy ID and source repository ID all match the Task's plan. The
span supplies the duration and its start time orders history. Without a comparable recorded
span, they show no hint. Preview never probes a Provider or runs a Gate to obtain it.

`af task status --json` and `af task wait --json` print `af/task-status@2`. Its exact decimal
strings distinguish reserved, charged and remaining tokens; `unknown_usage_attempts` separately
counts settled Attempts with unreported usage. The old printed document shape is not read as the
new one. A text status prints those figures on one line. `af task wait --bell` writes a bell only
to terminal stderr when the Task settles. `--notify` invokes the platform notifier after
settlement; absence or failure warns on stderr and never changes the wait's exit code.

## Considered options

- Estimate a Gate from the plan's limits. A limit is not a measurement and would mislead.
- Treat unknown Provider usage as charged zero. The accounting charge is zero, but spend is
  unknown under ADR-0143.
- Make notifications required for a successful wait. OS notification services are optional
  and may be absent on headless hosts.

## Consequences

The hint can disappear when old Tasks are collected or their Store is unavailable. That is
preferable to a stale estimate. Status consumers must validate `af/task-status@2` and its
published schema.
