# ADR-0104: Preview captured Task plans before first execution

Date: 2026-09-15
Status: Accepted (2026-09-17); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the preview for the legacy goal entry point.

## Context

A user working through Claude or Codex needs to see the resolved Pipeline, embedded calls,
actual Worker bindings and limits before authorizing work. Raw inspection JSON is useful for
machines but obscures those decisions. A plan name alone does not bind the reviewed execution.

## Decision

Render human planning output from the captured ExecutionPlan, compiled graph and package
artifacts. The default ASCII flow folds internal bookkeeping with an explicit omission count;
`task explain --tree` expands calls and conditions. Both include exact plan identity, package
version, effective model/effort/account alias, public ports, effects, destinations and limits.
Do not resolve live configuration, claim isolation from a display label, or expose account
principal identifiers. Sanitize terminal controls and bound line width. JSON contracts remain
unchanged; historical inspection remains read-only and advertises no historical execution.

`task start` captures and previews by default for both Task-file and legacy goal entry points.
The default is independent of JSON mode or terminal detection. Initial `task run` requires an
exact `--confirm-plan` identity; mismatch refuses before dispatch. Recheck the captured plan
under the writer lease before recovery/admission, including the Planner bootstrap path.
`--execute` explicitly opts into automation on start or run. Existing admitted Tasks resume
and finished Tasks replay without requiring another confirmation. No cross-Task trust cache
or persisted approval authority is introduced.

This local CLI confirmation is not cryptographic developer approval. A generated plan still
requires the captured signed decision; neither flag weakens that boundary. The initial no-fit
preview describes only the fixed Planner preparation. All approval waiting remains inside the
original deadline. The existing `af review run` execution contract is unchanged.

Claude/Codex integrations display the CLI preview and obtain user approval before issuing the
exact confirmation command. The executable does not impersonate a host chat UI or prove who
entered a local confirmation flag. Plan admission and the existing Task history remain the
record of actual execution; no second scheduler, ledger or approval Store is added.

## Consequences

Existing scripts that intentionally start new work must supply `--execute`, or separately
capture, inspect and confirm its full plan ID. Existing execution fixtures use the explicit
opt-in; dedicated CLI tests prove zero-usage preview, confirmation refusal, successful exact
confirmation, finished replay, captured rendering after live configuration removal, both entry
points and preservation of the generated-plan signature barrier. The plan's full JSON remains
the detailed inspection format when a compact label or folded subtree is insufficient.
