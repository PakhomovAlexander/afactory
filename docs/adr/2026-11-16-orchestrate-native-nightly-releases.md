# ADR-2026-11-16-orchestrate-native-nightly-releases: Orchestrate native nightly releases

Status: accepted on reviewed merge (owner authorized 2026-11-16; binding only when merged)
Date: 2026-11-16

## Context

The existing nightly chain owns four services and a UTC timer. Its two measurement originals
cannot establish the full native comparison family required for successful release completion.
The current Scenario v6, Ant v7 and Store v10 already support interval scheduling and retention;
nightly work must extend those contracts without reverting their behavior. The former nightly
private key is absent and the owner explicitly authorized a new public signing identity.

## Considered options

- Keep the four-service clock: rejected because it duplicates native occurrence admission and
  lacks full-family verified completion.
- Build and measure inside an adapter: rejected because heavy work cannot meet the 30-second
  effect boundary or recover safely after its process tree is terminated.
- Select another baseline automatically after preflight fails: rejected because unrelated source
  pairs and missing families cannot establish the requested comparison.
- Reuse old prototype wire versions: rejected under ADR-0145; current main already changed them.

## Decision

Add a validated nightly tag scalar and projection, four typed nightly Actions with exact
predecessor observation contracts, and a separately pinned machine scope binding repository and
configuration digest. Generic signed publication remains unchanged. A deterministic Scenario runs
build, full native comparison, report and publication through durable admission Actions and Waits.
Only the external supervised worker performs heavy work or publication. All token budgets are zero.

A daily 03:00 Europe/Moscow Ant derives its immutable tag from the pinned scheduled local date
before creating a Run. Interval schedules cannot request a daily date projection. Overlap/missed
skip, retention, settlement capacity, limits and exact accounting remain unchanged.

The native comparison retains all existing profiles, oracles, member keys and offline verifier.
A complete verified measured regression remains reportable; absent, failed, incomplete, synthetic
or unverified evidence blocks publication and successful completion. An explicit optional
`baseline_ref` release tag selects an operator-reviewed ancestor; omission/null retains highest
reachable stable behavior. The chosen tag and commit are sealed in the build and bound through
comparison, report and publication certificates. This VM selects `v0.15.1-rc.3`; incompatible
`v0.15.0` is never silently skipped or replaced.

Installation is offline preparation only. It refuses missing tools, local pinned images,
external signing prerequisites, storage, private same-worker config/state ownership or immutable
root-owned control. Source, schemas and native producer must match the exact reviewed committed
`control_sha`. Registration, manual release proof and activation remain operator steps. Retire
both the old timer and all chain handovers before enabling the native Ant drivers.

Replace only the nightly public identity with authorized key `0596484DC93A8A62`. Keep the stable
release key unchanged; existing clients can explicitly override their nightly public-key file.
No private key is stored, generated or read by deployment tooling.

## Consequences

Changed current-release shapes become Scenario/Bundle/Pin/Run/List v7, Policy v6, adapter request
v5, Ant/Pin/Tick v8, Ant State/List v9, inspection v4, Store v11 and nightly config v2, including
writers, readers, schemas, examples and parity tests. Old versions fail closed without migration.
The asynchronous service adds closed v1 service/job/completion contracts with exact digests.

Restart reconciles the same durable job and native originals. A missing sealed build after
interruption and an absent remote after uncertain publication fail closed. Interrupted pending
reports/exports require inspection. Jobs and their evidence remain bounded and retained; reaching
capacity refuses new admission. A successful queue response never claims a successful release.
This change neither deploys on a VM nor establishes measured performance improvement.

Generic adapter receipt containers also advance to GitHub state v5 and Git workspace state v3
because they persist the new request version. Their signed publication authority is unchanged.

The release capability digest is regenerated from the exact schema/telemetry fixture bytes,
with the captured input Snapshot recorded as source provenance. This is reviewable candidate
metadata, not release authority; normal release preparation still binds its exact Git first parent.
