# ADR-0081: Register owned Review children in the common Task runtime

Date: 2026-09-12
Status: Proposed

## Context

The approved Task increment makes every business operation use one scheduler and common
Attempt accounting. Historical typed Scatter owns dynamic Review slices inside a static DAG.
Running its old executor inside a common Task operation would create a second scheduler and
budget, charge a coordinating parent, and deadlock when an embedded Pipeline allows only one
concurrent operation. Restart must preserve every registered slice, including work that never
started or whose result could not be published.

## Decision

Capture the bounded child template in `CompiledTask@1` under its static owner. Only the
installed compatibility compiler can create it. It contains the existing Reviewer operator,
public input/output contract, original Worker slot and allowance, exact inherited input
mapping, item input and Slicer fan-out bound. `LegacyReviewTaskPolicy@3` selects this capture;
earlier policy generations preserve their compiled bytes and execution behavior.

Persist the complete ordered `TaskOwnedChildSet@1` before any child can reserve. It references
the admitted parent invocation, source artifact, exact item envelopes and child invocations.
It carries data identity only. A protected Store registration checks the captured template,
complete source coverage, canonical child addresses and exact inherited inputs. The static
DAG stays immutable. Registered children share the original Task and enclosing Pipeline
Attempt, token, deadline and concurrency limits; the parent acquires no Attempt or execution
slot. Provider admission remains a parent barrier.

The existing scheduler expands the admitted owner, schedules children through ordinary
invocation/context/reservation/start/settlement, then folds all terminal children. There is no
nested scheduler, per-slice Task or separate Attempt ledger. Canonical Review node names remain
the exact `ReviewSlice@1` runtime names; Task addresses are separate qualified names. Worker,
package, model and Broker authority continue to come from the original captured Scatter slot.

Reuse protected Task execution transitions with a strict `TaskExecutionRecord@4` payload for
registration, selected child publication and immutable parent completion. Earlier payload
generations remain frozen. Read-only `af/task-inspection@5` exposes these exact records and
typed child sets; ordinary and Broker-only inspections retain their earlier generations.
Historical accounting resolves retired children against their original plan and registration.

Factual recovery requires the current Task writer, same admitted plan, current developer
approval and exact current Review Round. It can publish an already selected child or complete
an already admitted parent after the Task execution deadline or budget expires. It cannot
admit a new invocation, reserve, start or retry work. Approval expiry and revocation still
refuse recovery. Store transactions compare both Task and canonical Review history prefixes.

A completed shard requires selected common output plus canonical Review selection and output
receipt. Bare CAS content or settlement alone is insufficient. The pure fold records every
registered child: published results become Completed, started work without accepted publication
becomes Failed, and never-started work becomes Missing. Failed publication can therefore seal
an incomplete Shard Set; whole-Subject semantic closure still refuses acceptance. Sealing
prevents subsequent child dispatch and publication. Late paid usage remains in the original
Attempt and scope without rewriting the recorded Shard Set.

## Considered options

- Reusing the historical Scatter executor would preserve a second scheduler and budget and
  violate one-slot progress. It is unsuitable for the common runtime.
- Creating a Task per slice would detach child spend and acceptance from the parent Task.
- Rewriting the admitted DAG after slicing would let data change reviewed execution authority.
- Captured templates plus protected membership preserve static authority and allow bounded
  data-dependent execution through the existing scheduler.

## Consequences

Embedded Review shares the same resource owner as implementation work, and recovery does not
repeat paid calls. The Store must retain historical child registrations for accounting while
refusing their use for current dispatch after a plan or Round handoff. The additional wire and
inspection generations keep old artifacts readable without relabeling their payloads.

This implements part of the authorized common-runtime design. Its specialist review and full
integration gate remain pending; it does not by itself complete the legacy CLI cutover or the
live performance pilot.
