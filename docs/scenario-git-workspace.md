# Accepted candidates and owned Git workspaces

The optional Git workspace adapter prepares ordinary accepted Task candidates and cleans their
owned local/remote artifacts after a real human merge. It composes native Scenario Task, Action,
Decision and Wait steps. It selects no Task kind, Pipeline, Scenario name, Provider or model.
The inert [documentation-update example](../fixtures/scenarios/workspace/README.md) uses the
`fixture/implementation` Pipeline and the production GitHub publication/check/merge contracts.
[The decision](adr/2026-11-06-compose-owned-git-workspaces-with-scenario-publication.md) records
its authority and concurrency boundary. No automatic merge or arbitrary command is available.

## Install machine authority

Installation is offline and explicit:

```text
python3 -I scripts/install-scenario-git-workspace.py \
  --config /private/machine/workspace.json --output /private/tools/workspace-adapter
```

The config is `af/git-workspace-config@1`; its inert shape is in
[`config.example.json`](../fixtures/scenarios/workspace/config.example.json). All paths are
canonical, administrator-selected machine paths. Config, token and SSH configuration are private
files outside Git checkouts. State is a new private directory; the worktree root already exists
and is private. Repository, Task Store, workspace root and state cannot overlap. The config pins
native af and Git executable bytes, SSH configuration bytes, initial source commit/content,
repository numeric identity, actor, base, branch prefix, author, permitted operations and required
check identities.
Pinned executables must remain regular, non-symlink files without group/world write permission
and at most 256 MiB.
The fixed SSH destination is GitHub.com for that repository, with batch mode and strict host-key
checking. Git hooks, global/system config, filters, executable local config and URL rewriting
cannot add authority. The installer creates no token, remote ref, service object or schedule.

Review the complete installed executable digest printed by installation and use it in the
ordinary `af/scenario-policy@4` adapter entry. Allow only the operations/destination needed by
the consuming Scenario, with the existing 30-second wall and 65536-byte output ceilings. The
executable contains both this capability and all production GitHub code; runtime imports cannot
load application code from a checkout. Config/tool/SSH pin changes refuse further dispatch.
Retain the same executable, config and journals while a Run may resume.

## Explicit exclusive writer publication

This installation requires `publication_mode: exclusive_owned_writer`. It is a different,
explicit machine authority boundary from the ordinary GitHub adapter's administrator-protected
locked-head mode, which remains unchanged. An administrator must ensure this installation is
the **only writer** of its owned remote branches, across people, automation, machines and other
installations. A stable private lock serializes local invocations through push, publication and
observation. Pushes compare against absence initially, then the exact prior owned commit, and
read back the head. Every continuation is a direct child of the previous owned commit.

GitHub PR creation has no expected-head-SHA precondition. This mode therefore trusts the sole
writer restriction during the POST; pre/post GETs alone cannot enforce it against another
writer. Do not select this mode where that restriction cannot be guaranteed. The original
`install-scenario-github-adapter.py` continues to require server-locked heads and never falls
back to this mode. Publication still requires the kernel-verified exact signed Decision.

## Typed composition

`prepare_candidate_v1` takes `candidate: task` plus text `subject` (its result ID), `revision`
(its accepted Snapshot) and `body` (`af/git-workspace-content@1` with title/body). Optional
`previous: action` names a completed preparation. The adapter reopens the native Task Store and
checks the exact accepted result, plan, revision, source/candidate SourceTree artifacts, Snapshot
ancestry, repository and committed source bytes. `af scenario candidate-source` exports these
checked objects into a private isolated capture using the existing materialization plumbing.
A continuation additionally checks that its admitted source is the exact predecessor Snapshot,
including native re-root provenance. New remote base bytes are never fetched/admitted as Task
source; unresolved conflicts require a separately captured source and explicit operator handling.

The adapter creates a deterministic owned commit, an exclusively reserved detached worktree and
an owned `target/` build directory. A new worktree is registered with `--no-checkout` and populated
by the existing native raw materializer, which bypasses attributes/filters and reads back exact
bytes. Its fixed `--force` invocation is permitted only immediately after registration with
exactly the owned `.git` file present; reconciliation never resets a partial checkout. It returns an independently versioned
`af/git-workspace-prepared@1` Action evidence document. Native admission binds it to the exact
Task receipt. These projections accept only that preparation Action:

- `workspace_subject`: the existing `github-pr:` hash of the complete publication payload.
- `workspace_revision`: the exact owned commit SHA.
- `workspace_publication`: the existing `af/github-publish-pr@1` body, for `publish_pr`.
- `workspace_receipt`: the complete preparation document, for cleanup.

A standard Decision signs the subject/SHA. Existing `publish_pr` uses the matching local accepted
preparation and pushes only under that signed authority. The production adapter owns the POST,
marker, object readback, durable journal and uncertain-response reconciliation. An ambiguous POST
never becomes absent from an empty listing and is never repeated. The same original publication
subject/SHA feeds the existing `checks_passed` and `merged` Waits. Checks retain exact app/context,
latest-suite/run and complete-listing rules. Merge needs the exact PR/head, closed merged state,
timestamp and merge SHA; green checks and closed-unmerged PRs cannot satisfy it.

A continuation retains the branch, publication content and PR marker/object identity. Another
exact signed `publish_pr` advances only that PR's open head using a lease, with no second POST.
Previous-head CI and merge observations fail closed. A consuming Scenario owns its finite repair
steps and Task budgets; this capability does not choose a repair strategy or reviewer policy.
A typed `select` binding lazily resolves exactly one of two equal-type arms under a boolean
condition. It lets a consuming Scenario choose an original preparation or a conditional repair
without reading skipped outputs. Both arms are statically checked; Decision selection is refused
to retain static signed-Decision ownership. It is ordinary data selection, not a workflow engine.

For bounded CI repair, use the production `checks_completed` Wait on the preparation's exact
subject/revision. It completes only after **all** configured required identities have completed
with success or an explicitly recognized failure (`failure`, `cancelled`, `timed_out`,
`action_required`, `startup_failure`; status contexts use `failure`/`error`). Queued/in-progress,
missing, neutral, skipped or unknown conclusions stay pending. Incomplete listings refuse;
stale heads, untrusted producers and closed-unmerged PRs never route repair. Timeout supplies
no failure evidence and must not start a repair.

The observation's evidence is the independently reusable
[`af/github-check-outcome@1`](../schemas/github-check-outcome-v1.json) document. It binds the
publication subject and exact head to up to 32 configured check/status identities, their latest
run/status IDs, suite IDs (null for status contexts), and `pending`, `ready` or `repairable`
outcomes. The typed `checks_state` projection requires this condition, parses the bounded
contract, checks its outer subject/revision and terminal consistency, and returns its state.
`observation_satisfied` means completed here; it does **not** mean checks passed. Existing
`checks_passed` retains its successful-only meaning and stronger locked-head installations
still perform their protection checks.

An ordinary Scenario compares `checks_state` to `repairable`, conditionally invokes a bounded
`fresh_task`, and supplies `source` (the accepted Task), `scenario_evidence` (the completed Wait)
and `publication` (the **preparation Action**, not a PR Action). The fresh Task template declares
`bindings.source` with the candidate Snapshot port/type. Native admission verifies the pinned
accepted predecessor and the preparation's Task/result/Snapshot identities, subject and exact
commit against the observation. Only completed failing evidence is inserted into the fresh
Task's `requirements.scenario_evidence`; source bytes still come solely from the admitted Task
Snapshot. A repair is another ordinary accepted Task, prepared with `previous` set to the
original preparation. It needs a fresh exact signed Decision before same-PR `publish_pr`.
Use lazy `select` to choose the original or repaired preparation; subsequent checks, actual
merge and cleanup consume that selected head. A finite sequence of conditional repair steps
and its Task/dispatch/token bounds belong to the consuming Scenario.

This interface covers CI outcomes. It does not admit review-comment bodies, change requests,
remote base trees or conflict resolution as Task source authority; those remain unavailable
without a separate bounded source contract. `ready` describes required CI only, not human
approval, mergeability or actual merge.

`cleanup_workspace_v1` takes the preparation Action as `prepared`, its document as `body`, the
publication subject/SHA, and an exact `merged` observation as `evidence`. Its Action must declare
`required_condition: merged`. The adapter independently re-observes the production publication
receipt and actual merge before removal. It preflights the complete explicit predecessor chain:
worktree device/inode, detached exact commit, common Git directory, clean tracked state,
untracked/ignored paths restricted to the exact leased build root, build inode and current
local/remote ref leases. Raw blob hashes, types, modes and link bytes must also equal the owned
commit: attributes, index flags and disabled Git filemode detection cannot conceal modifications.
Projects do not need an ignore rule for the owned build directory.
Dirty, foreign, replaced, attached or changed-head objects remain. Cleanup retains progress before
effects. Before each local/remote ref deletion it persists a deletion intent. A fresh dispatch
may delete under its exact SHA lease; reconciliation accepts absence but **retains every present
ref after that intent**, including an unchanged SHA. The process may have deleted the original
and another actor may have recreated the same SHA. A crash before dispatch is deliberately
indistinguishable from a lost successful response: an unresolved retained ref requires operator
inspection, never another automatic deletion. Commit equality alone is not renewed ownership. Build
contents inside the exact owned build directory belong to that reservation. Task Stores are kept.

## Recovery and bounds

The workspace journal is `af/git-workspace-state@1`. Its separate `github.json` is the ordinary
`af/github-adapter-state@3`; both share the stable workspace lock. Both are owner-only,
checksummed, no-follow and fsynced. Missing/corrupt/foreign journals refuse dispatch. The local
journal admits 32 intents and at most 32 explicit lineage members; the production journal keeps
its existing bounds. Native exports admit 32768 entries, 4096-byte paths and 64 MiB of content.
Native capture has a separate retained private lock and admits at most four uncertain scratch
directories per capture parent; a killed exporter cannot grow an unbounded staging sequence.
Those directories are never adopted or removed to recover capacity. All subprocess argv and
environment are fixed by installed code/config; no Scenario command,
credential path or unvalidated JSON operation escape exists.

Worktree and build reservations each have four staging slots. Creation is fsynced, device/inode
ownership is journaled **before** exclusive no-replace directory publication, then final publication
is fsynced and journaled. A crash after publication resumes only the recorded inode. A directory
created before its ownership record is uncertain: it is retained, never adopted from emptiness.
Only an empty private uncertain slot may be bypassed, up to the fixed slot bound; changed content,
symlinks and exhaustion refuse. An interrupted checkout is preserved rather than reset. Local
idempotent operations recover during ordinary reconciliation; there is no new protocol method.

The Scenario vocabulary is generation 4, embedding wrappers/Ants are generation 4, the Scenario
Store is generation 5, adapter requests and the GitHub journal are generation 3. Added operation
and projection discriminators change typed persisted shapes; old generations fail closed without
migration. Task delivery, reviewer evidence, cache authority and GitHub operation payloads retain
their existing shapes. Finish old Runs with their original release before upgrading.

Run `python3 -I scripts/test-scenario-git-workspace.py` for real local Git plus inert-service tests,
including both reservation crash matrices, signed input checks, same-PR continuations, uncertain
POSTs and merge-gated cleanup. The native `task_delivery::workspace` integration test executes an
ordinary accepted command-backed Task and checks object bytes and forged-receipt refusals. No
live GitHub account, model, remote gate or scheduler is used or claimed by these tests.

The native `ordinary_scenario_routes_completed_checks_into_one_fresh_repair_and_signed_same_pr`
regression composes ordinary accepted command-backed Tasks, completed outcomes, conditional
fresh repair, exact signed Decisions and lazy receipt selection in `documentation-maintenance`.
Its inert adapter models PR identity; the separate production-adapter real-Git tests exercise
actual publication/recovery code and stale-head refusal. Neither uses live services or models.

The native `scenario::workspace_lifecycle::production_workspace_scenario_repairs_same_pr_and_cleans_only_after_merge`
regression closes the production composition boundary. It privately installs the complete production
workspace/GitHub adapter and native executable, runs accepted command-backed Tasks and native signed
Decisions, and uses real Git commits, worktrees and a bare remote. Only HTTPS transport and the fixed
GitHub Git destination are inert. It covers ready, pending, stale-head refusal and completed failing CI
with fresh repair admission, predecessor/source provenance, a second signature and a fast-forward of
the same PR with exactly one creation POST. This exposed and fixed the installed entry point's
missing GitHub transport during continuation preparation; initial preparation remains offline.
A real no-ff merge supplies the merge commit reported by the HTTP fixture; cleanup removes actual
owned worktrees, build roots and local/remote refs while
retaining the merged base. The focused `outcome-service.py` test above remains a composition model,
not production publication proof. Run the new regression with:

```sh
cargo test -p af --test it scenario::workspace_lifecycle
```

The native integration fixtures use a shared helper to copy the held Cargo executable into a
new private file, strip only that copy's debug symbols with `strip -S`, then hash/pin the resulting
bytes. The helper checks that the shared executable's identity, permissions and digest remain
unchanged. A local Rust 1.88 Linux container reproduced both installation refusals at the size
check: its 288,451,120-byte debug executable exceeded the 268,435,456-byte bound. This is local
reproduction evidence; hosted Linux CI must independently confirm the fix.
