# ADR-2026-11-06-compose-owned-git-workspaces-with-scenario-publication: Compose owned Git workspaces with Scenario publication

**Status:** proposed (2026-11-06).

## Context

Native Scenarios already own bounded Tasks, signed publication Decisions, durable Actions and
exact-head Waits. The production GitHub adapter already owns publication transport, checks,
actual merge observation and at-most-once uncertain POST recovery. Missing reusable capabilities
are accepted-candidate preparation and explicitly owned Git/workspace cleanup. Ordinary Tasks
and differently named Scenarios need these without implementation recipes or model policy.

## Considered options

- Copy the implementation lifecycle's publication/observation subsystem: rejected because it
  duplicates the production safety contract and embeds a consuming recipe in runtime code.
- Publish an unlocked dynamic branch using only pre/post SHA reads: rejected as an implicit
  weakening of ADR-0275; GitHub has no expected-head precondition on PR creation.
- Require administrator-locked immutable heads for dynamic fast-forward continuations: rejected
  for this capability because the authorized writer must advance the branch. The original
  locked-head adapter remains available unchanged.
- Add generic command or arbitrary JSON execution: rejected because workflow data would acquire
  executable or credential authority. Extend only typed operation/projection vocabulary.

## Decision

1. Supply a separately installed Git workspace capability with `prepare_candidate_v1` and
   `cleanup_workspace_v1`, independent receipt/config schemas and four typed preparation
   projections. Existing Task, Action, Decision and Wait kinds compose it. No Task kind,
   Scenario identity, reviewer role, Provider account or model name selects admission.
2. Native candidate export reuses Task selection, checked SourceTree/Snapshot provenance and
   deterministic raw Git object materialization. An accepted ordinary Task may prepare an exact
   owned commit/detached worktree/build directory. Continuations bind an explicitly owned
   predecessor and prove unchanged source bytes/provenance. Unadmitted remote base bytes remain
   unavailable; no automatic conflict resolution or merge is introduced.
3. Reuse production GitHub payloads, journal, transport, PR validation, checks and merge Waits.
   The installed extension adds owned pushes and same-PR fast-forward continuation under a
   newly signed exact publication. Uncertain initial creation never repeats POST. Completion
   and cleanup require actual exact merge evidence, independently read back from production
   publication receipts.
4. Require an explicit `exclusive_owned_writer` machine publication mode. Administrators ensure
   no other person, machine, installation or automation writes these branches. One stable
   private lock spans local pushes and POST; all ref mutations additionally use exact leases.
   This mode trusts that sole writer boundary because GitHub cannot atomically enforce SHA at
   PR creation. It is never selected as fallback from the original server-locked mode and
   changes none of its protection checks. A deployment unable to guarantee exclusivity must
   refuse this mode.
5. Both directory protocols fsync creation, journal inode/device before exclusive no-replace
   publication, then fsync and journal publication. Never infer ownership from emptiness/null
   state. Four fixed slots bound uncertain staging; unknown directories remain. Cleanup checks
   exact owned identities, clean state and local/remote commit leases, journals progress before
   effects and preserves changed/replaced/foreign objects after interruption. A persisted ref
   deletion intent makes every subsequently present ref uncertain, even at the original SHA;
   reconciliation retains it and accepts only absence. Fresh dispatch alone may use the lease.
6. Added enum discriminators change the Scenario typed shape: version Scenario/bundle/policy/
   pin/run/list and embedding Ant documents to 4, Store to 5, adapter request and embedding
   GitHub state to 3. Update current readers/writers/schemas/fixtures together. Refuse earlier
   generations without migration under ADR-0145. GitHub operation payloads and Task delivery
   shapes remain unchanged. One typed lazy `select` binding is necessary to choose conditional repair receipts without
   consuming skipped outputs: both arms have equal non-Decision types and the condition is
   boolean. Signed Decision ownership remains static. No new recovery method, reviewer evidence feature
   or build-cache policy is introduced.

7. Reuse one production exact-head check enumerator for successful-only gates and the
   `checks_completed` Wait. Its independent `af/github-check-outcome@1` evidence records bounded
   required identities, latest IDs and ready/repairable/pending state. `checks_state` parses and
   binds it to the outer Observation. Pending/missing/incomplete/unknown results never become
   repair evidence. A fresh repair Task requires a matching accepted source Task and preparation
   Action (`publication` input); current-head evidence is captured in requirements, never source
   authority. Review requests, remote base/conflict sources remain unavailable. Consuming
   Scenarios own finite conditional repairs, new signatures, lazy selection and actual merge.
   These additions complete the unreleased generation-4 capability vocabulary; no established
   Observation, Task result, operation payload or reviewer evidence shape changes.

## Consequences

The consuming package still owns Task budgets, finite repair strategy, review policy and human
signatures. Explicit exclusive writer authority has a material trust cost and cannot be claimed
as a server lock. A private installation pins both complete application sources and all machine
scope; local real-Git and inert service tests cover interruptions without real publication.
The [operator guide](../scenario-git-workspace.md) defines installation, contracts and recovery.
