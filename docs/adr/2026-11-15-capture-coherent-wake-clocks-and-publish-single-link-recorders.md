# ADR-2026-11-15-capture-coherent-wake-clocks-and-publish-single-link-recorders: Capture coherent wakes

**Status:** proposed (2026-11-15); candidate repairs for #942, #943 and #944.

## Context

A checked append or competing poll can advance source or checkpoint policy time after a live
poll captures its observation clock. An explicit-clock projector correctly refuses that prefix,
but terminating the attachment loses ordinary pre-native progress. The earlier
[currentness decision](2026-11-15-revalidate-all-pre-native-wake-contention.md) also missed recorder
publication: linking a complete aside before unlinking it briefly exposes a two-link recorder,
which the strict owner-private reader must refuse.

## Considered options

- Treat future clocks, all conflicts or diagnostic strings as retryable: rejected because invalid
  clocks, checkpoints and authority would be concealed.
- Relax the recorder single-link rule or check existence before ordinary rename: rejected because
  foreign links and concurrent overwrite would become possible.
- Coherently validate live clocks and atomically rename absent-only recorders: selected.

## Decision

The live journal captures its initial clock before inspection, then samples again after capturing
checked source. It validates the entire candidate page against the latter clock. A checked event
or authenticated retained checkpoint beyond the initial clock causes a typed logical change,
never a stale page commit. Existing serialized current-evidence validation also checks current
history and checkpoint times against a fresh clock before permitting one bounded empty Pi poll.
Clock rollback, impossible future evidence, invalid provenance, corruption and authority remain
terminal. Standalone explicit-clock projection retains its strict contract.

On Linux, publish fully written and synchronized private recorder files using the existing
rustix no-replace rename primitive. There is no hard-link interval, replacement or unsafe fallback
when the primitive is unavailable. Propagate synchronization and publication errors. Preserve
ordinary run/cancel on other supported platforms with their existing absent-only link behavior;
automatic wake remains Linux-specific. No dependency, public shape or schema version changes.

The [boundary evidence map](../design/task-wake-journal.md#clock-and-publication-boundaries)
covers capture, inspection, restoration, projection and both transactional recorder checks.
Debug barriers and scoped clocks decide the regression interleavings. Existing finite attachment,
ordering and uncertainty fences remain unchanged; no native resend follows from deferral.

This retains research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`, source Snapshot
`sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`, and its common
Store/outbox recommendation with conservative ambiguous-admission handling. The reviewed comparison
base remains `1a4dd4cc82d5f01f55014283c95d3bf6ea77fac3`.

## Consequences

Legitimate writers consume bounded pre-native polling work without losing the attachment. Strict
single-link readers continue to reject genuinely foreign hard links. Independent whole-candidate
verification and Pi/Codex live certification remain coordinator work; Claude live trials remain
deferred under #876. No service, host call, login, publication or Task execution authority is added.
