# ADR-0055: Select captured Pipelines before generation

Status: accepted, 2026-09-11.

## Context

A Task's preferred Pipeline is an input preference, not proof that its public contract or
resource requirements fit. A missing account, insufficient verification allowance or unknown
fact must not silently become permission to generate a replacement definition. Local bindings
and embedded child contracts affect feasibility and must survive resume.

## Decision

Select deterministically from the captured Pipeline catalog. Check Task kind, known facts,
public input/output contracts and acceptance coverage, then compile the exact expanded
structure and check captured capabilities and resource feasibility. Keep semantic no-fit,
unknown facts, unavailable capabilities, infeasible budget and invalid definitions distinct.

The explicit Task choice has precedence when feasible. Its fallback is `refuse`, `select` or
`generate`; generation first searches all existing candidates. Without an explicit choice,
project `no_match` controls refusal or generation. Trusted per-strategy numeric priorities order
remaining candidates. Equal best feasible candidates produce visible ambiguity. Lower priority
compatible definitions need no account probe once a higher priority candidate is selected.

Only known semantic no-fit can request generation. Planning does not assert unknown facts or
run a model. Token-free Provider identity reads bind the intended account; paid capability
admission remains a bounded node in the selected Task plan. Protect verifier reservations and
mandatory work, including Provider admission, before any business dispatch.

Persist the selection assessment and original request. The selected Task revision retains its
original requested Pipeline and binds its selection through captured adapter provenance. Plan
and run select identically. Resume uses the plan's exact captured root, source and dependencies;
it never reroutes from live configuration. A selection refusal exposes artifact IDs and reasons
without creating execution state or spending an Attempt.

## Consequences

Developers can prefer cheap definitions and fall back to fitting shared heavy definitions
without invoking a Planner. Inspection explains each rejected candidate and distinguishes
unchecked lower priority capabilities from admitted bindings. Generation and authenticated
approval remain subsequent, separately enforced operations under the same Task authority.
