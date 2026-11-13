# ADR-2026-11-13-seal-local-worker-authority-in-scenario-templates: Seal local Worker authority in Scenario templates

**Status:** proposed (2026-11-13).

## Context

Scenario templates previously rejected local packages because reconstructing a bindings file in
a temporary directory lost its original package base. Customer Worker configuration belongs to
explicit local bindings, while reusable Pipelines and constraining policy remain committed authority.
The accepted issue 804 design requires immutable reuse of the existing native capture and compiler.

## Considered options

- Remove the package refusal and reopen paths on each plan: rejected because mutable paths cannot
  supply pinned authority, and the reconstructed file has the wrong base directory.
- Copy customer Workers into the operator catalog: rejected because customers remain independent.
- Add a template-specific compiler: rejected because it duplicates contract and independence rules.

## Decision

Version 2 Task templates retain the existing content-addressed native RunAuthority reference and
its diagnostic bindings document. Explicit pinning passes the original bindings path into native
bounded capture once. The native package artifacts retain exact pins, bytes, Provider aliases,
slot mappings, constraining defaults and policies. Only permitted local Worker packages are admitted.

Fresh planning first requires the exact durable native template-pin event and verifies its
CAS template bytes, binding source, repository, commit, authority and diagnostic bindings as one
identity. Machine policy admission alone cannot create that association. Reconciliation passes
through the same check. Planning then restores authority through the ordinary compiler. It compares diagnostic
bindings with their captured document and verifies committed repository/source identity. Planning
and restart never reopen original local paths. Missing or corrupt artifacts fail closed. Existing
Task reconciliation requires both exact file bytes and the same captured authority; recorded plan
and predecessor equality checks remain mandatory. Canonical Provider admission remains native Task
work, and local aliases never establish independence.

A `ScenarioTemplatePinned@1` event references the template in a non-Task stream in its Task Store.
Capture through durable publication holds a native process-owned shared `flock` reservation
on the existing CAS directory inode. Collection takes the exclusive reservation nonblocking before
its SQLite transaction and keeps it through tombstones and sweep. Capture takes its reservation
before reading or reusing any objects. Thus collection either finishes first or refuses before
capture, including for reused objects older than collection's timestamp cutoff. Neither path waits
for this reservation while holding SQLite. Shared captures can proceed concurrently, and unrelated
Task heartbeats retain their ordinary SQLite renewal authority and bounded busy handling.

No fake Task, persistent lease row or timestamp refresh establishes capture liveness. Descriptor
close, unwind and process death release the reservation; the next native collection reclaims
unpublished scratch without recovery authority or a stale reservation to expire. No persisted shape
changes for this exclusion protocol. Extending busy timeouts or protecting only newly written
objects would leave the heartbeat defect or the reused-object race intact.

Before publishing, pinning restores and validates the complete captured closure under that
reservation, including deserializing and comparing the exact template artifact before taking
SQLite's writer lock. Final insertion uses a private verified-reference guard tied to the live
reservation, avoiding redundant CAS filesystem checks. Ordinary append retains its dangling-reference
recheck. Only SQLite operations and bounded in-memory encoding and exact durable-record comparisons
hold the final writer lock; Git, CAS reads, filesystem capture and compilation do not. Existing rows of any sequence prevent
insertion; negative, gapped or otherwise corrupt pin histories fail closed without mutation.
The sole event must retain the native writer's fixed epoch timestamp and absent node, attempt,
causation and correlation metadata, as well as its exact type, payload and artifact references.
Existing conservative CAS reachability retains the complete closure before the first Task and
through Task collection. Pins live for the Store lifetime, count against the machine storage
budget, and are discarded with that Store. Native Tasks also retain their own authority references.
There is no mutable recapture fallback. Version 1 templates and pin envelopes are refused.

The changed embedded template contract versions Scenario, Bundle, Pin, Run and Run-list to 6;
Ant and Ant-pin to 7; Ant-state and Ant-list to 8; Ant-inspection to 3; Ant-tick to 7; and
Scenario-store (including its ownership marker) to 10. Writers, validators, schemas, shipped
schema tables and fixtures move together; previous versions fail closed. Machine policy remains
version 5, and adapter, roadmap and decision contracts retain their existing versions: they do
not embed templates or the changed enclosing records. Updating shared schema references does
not itself change those contracts.

## Consequences

Customers can delete or move local bindings and Worker directories after pinning. New configuration
requires a new template identity and exact Scenario/policy confirmation, without per-run overrides.
Pinning remains token-free capture, with no Task execution, publication, remote gate or paid experiment
permission. Public typed contracts, evidence, effects, budgets, verification reserves, sandbox bounds
and writer-reviewer independence remain enforced by native planning and Provider admission.
