# ADR-0236: Scope class builds to their execution

**Status:** accepted (2026-10-27).

## Context

ADR-0231's selected native build fix compiles every selected variant before the first
successful build receipt starts the native test allowance. Applying the whole-plan closure
to independent hosted class jobs also compiles perf-only features in functional, e2e and uix
jobs whenever their shared plan selects perf.

## Decision

Refine only the build scope of ADR-0231's selected-class amendment. Pass an explicit
`--build-scope plan` from native `execute` to `run` and its recorded `build-inventory` command.
Direct `run` defaults to `--build-scope class`. The build command validates the original plan
and class membership, then compiles the default workspace and both perf variants when perf
belongs to that execution scope. No environment variable or plan provenance infers the scope.

Native execution still completes every selected variant before its first successful build
receipt. Hosted perf completes both feature variants before its build receipt; other hosted
classes compile only the default inventory. Keep the complete original plan in every receipt,
audit the full default inventory, and retain exact execution membership checks. Failed required
compilation stops before suites and cannot record a successful build or class receipt.

Keep JSON inventory stdout separate from receipt output, compiler diagnostics on stderr,
process supervision, original deadlines and `.af/` authority unchanged.

## Considered options

- Infer hosted execution from environment variables: rejected because explicit command wiring
  defines the execution boundary independently of runner environment.
- Narrow the saved plan to a single class: rejected because receipts must retain their exact
  selected plan and aggregation membership.
- Compile only the current class in native execution: rejected because a later perf compile
  would run after the first successful build receipt started the test allowance.

## Consequences

The focused regression runs the real wrapper, Make graph, runner and receipt writer with
compilation and suite stand-ins. It checks a full native plan, every hosted class under that
same full plan, required-variant failures and unrelated perf failures in hosted non-perf jobs.
These fixtures do not establish a full native gate pass or hosted CI success. Publication and
hosted CI remain operator actions; this Task runs focused checks only.
