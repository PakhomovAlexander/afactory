# ADR-0069 — Compile captured Review ports with explicit artifact codecs

**Status:** accepted, 2026-09-12; unreleased compatibility frontend checkpoint.

## Context

Legacy Review graphs carry both flat JSON and typed envelopes. Their many-valued inputs merge
edges and sort original artifact IDs. Task compilation instead assigns one source address to
each input port. Treating every legacy value as an envelope, wrapping an existing Finding Set
again, or sorting new wrapper IDs would change the evidence delivered to canonical reducers.
Legacy Review also requires every predecessor to complete, even when its output is optional.

## Decision

An installed frontend compiles the validated Review topology into the common Task graph. Its
public input/output contract is explicit. Original canonical node and port names remain in a
mapping; generated qualified Task names do not rename Review evidence. Every original edge has
one typed input lane. The domain restores original IDs, combines lanes under the original port
names and sorts those IDs. Undeclared or missing lanes fail before domain execution.

The compiler selects a flat or enveloped codec from the original operation and contract, never
from JSON keys. A flat wrapper retains the unchanged object payload and exactly its raw CAS ID;
restoration rechecks both objects. Existing envelopes retain their producer, inputs, Snapshot
and identity. This includes historical Finding Sets; they are never stamped with the current
Snapshot. Generation preserves optional genesis absence and the exact Round assignment.
V1 shorthand receives explicit compatibility types; V2 and later retain the existing prohibition
on opaque Generation outputs.

`af/LegacyReviewRound@1` is an input binding to an exact Campaign Manifest, Round event, epoch,
Subject and head. It is distinct from the existing `af/TaskReviewRound@1` completed reduction.
The capture adapter loads current Round authority from the Store and retains the original raw
SourceSnapshot beside its Task wrapper. These artifacts provide data, not execution authority.

Installed `ReviewDomain` operations are outside the authorable Pipeline operator vocabulary.
The common scheduler applies the original successful-predecessor barrier to them while still
evaluating Task branch conditions. Every inherited Gate supplies a passed-outcome condition
through `af/LegacyReviewGateOutcome@1`, beside the unchanged legacy decision. Setup failure must
not produce an outcome. Reviewer results and Proposal metadata occupy separate typed ports.
Reviewer and Scatter slots participate in common Provider admission and resource accounting.

Serialized mappings and compiled graphs cannot authorize themselves. Execution admission must
recompile from captured configuration, packages, Round inputs and trusted resource/binding
settings. No second scheduler, retry loop or Attempt ledger belongs in the domain adapter.

## Verification and remaining integration

Compiler tests cover V1/V2 compatibility, fan-in, original names, inherited Gates, V5 history
and shared Provider guards. Scheduler tests preserve the optional-predecessor distinction and
Task branch suppression. Codec tests cover replay, malformed references, missing/corrupt CAS
objects, JSON keys resembling an envelope and unchanged historical Snapshot identity. A real
Store fixture captures and reopens Round inputs and Generation without adding execution events.
The new wire contracts have matching positive and negative schema/Rust checks.

This checkpoint does not move the legacy CLI. Captured plan admission, operation execution,
canonical publication/replay, broker callbacks, bounded Scatter sub-invocations and heavy Round
continuation still require their connection to the common runtime. Historical Campaign
execution and frozen evidence remain unchanged until that cutover is complete.
